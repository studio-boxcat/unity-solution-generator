import Darwin
import Dispatch

// MARK: - Public types

enum ProjectCategory: String, Sendable {
    case runtime
    case editor
    case test
}

struct VersionDefine: Sendable {
    let packageName: String
    let define: String
}

struct AsmDefRecord: Sendable {
    let name: String
    let directory: String
    let references: [String]
    let category: ProjectCategory
    let includePlatforms: [String]
    let allowUnsafeCode: Bool
    let versionDefines: [VersionDefine]

    static func load(rootPath: String, relativePath: String) throws -> AsmDefRecord? {
        let json = try readFile(joinPath(rootPath, relativePath))
        guard let name = extractJsonString(json, key: "name") else { return nil }
        let includePlatforms = extractJsonStringArray(json, key: "includePlatforms")
        return AsmDefRecord(
            name: name,
            directory: parentDirectory(of: relativePath),
            references: extractJsonStringArray(json, key: "references"),
            category: inferCategory(
                includePlatforms: includePlatforms,
                defineConstraints: extractJsonStringArray(json, key: "defineConstraints")
            ),
            includePlatforms: includePlatforms,
            allowUnsafeCode: extractJsonBool(json, key: "allowUnsafeCode") ?? false,
            versionDefines: parseVersionDefines(json)
        )
    }
}

func parseVersionDefines(_ json: String) -> [VersionDefine] {
    let needle = "\"versionDefines\""
    guard let keyRange = json.firstRange(of: needle) else { return [] }
    var idx = keyRange.upperBound
    // Find opening [
    while idx < json.endIndex && json[idx] != "[" { json.formIndex(after: &idx) }
    guard idx < json.endIndex else { return [] }
    json.formIndex(after: &idx)

    var results: [VersionDefine] = []
    // Parse each object in the array
    while idx < json.endIndex {
        let ch = json[idx]
        if ch == "]" { break }
        if ch == "{" {
            // Find the closing }
            let objStart = idx
            var depth = 1
            json.formIndex(after: &idx)
            while idx < json.endIndex && depth > 0 {
                if json[idx] == "{" { depth += 1 }
                else if json[idx] == "}" { depth -= 1 }
                json.formIndex(after: &idx)
            }
            let objStr = String(json[objStart..<idx])
            if let packageName = extractJsonString(objStr, key: "name"),
               let define = extractJsonString(objStr, key: "define"),
               !packageName.isEmpty, !define.isEmpty {
                results.append(VersionDefine(packageName: packageName, define: define))
            }
        } else {
            json.formIndex(after: &idx)
        }
    }
    return results
}

// MARK: - Scanner

struct ProjectScanner {
    struct Result: Sendable {
        let asmDefByName: [String: AsmDefRecord]
        let dirsByProject: [String: [String]]
        let unresolvedDirs: [String]
    }

    static func scan(projectRoot: String) throws -> Result {
        let rootPath = projectRoot

        let scanMark = ProfilerMark("scan.total")
        let generatorDir = joinPath(rootPath, defaultGeneratorRoot)
        let cachePath = joinPath(generatorDir, "scan-cache")
        let fileScan = loadCachedScan(cachePath, rootPath: rootPath)
            ?? scanAndCache(rootPath: rootPath, cachePath: cachePath)
        scanMark.end()

        // Load .asmdef files and build name → record map.
        // Unity enforces globally unique assembly definition names.
        var asmDefByName: [String: AsmDefRecord] = [:]
        for path in fileScan.asmDefPaths {
            guard let record = try AsmDefRecord.load(rootPath: rootPath, relativePath: path) else { continue }
            guard asmDefByName[record.name] == nil else {
                throw GeneratorError.duplicateAsmDefName(record.name)
            }
            asmDefByName[record.name] = record
        }

        // Build assembly root map: directory → assembly name.
        var assemblyRoots: [String: String] = [:]
        for (name, record) in asmDefByName {
            assemblyRoots[record.directory] = name
        }

        // .asmref files extend an existing assembly's source roots.
        for path in fileScan.asmRefPaths {
            guard let (dir, reference) = try loadAsmRef(rootPath: rootPath, relativePath: path),
                  asmDefByName[reference] != nil else { continue }
            assemblyRoots[dir] = reference
        }

        // Assign each directory containing .cs files to its owning assembly.
        var dirsByProject: [String: [String]] = [:]
        var unresolvedDirs: [String] = []

        for dir in fileScan.csDirs {
            if let owner = findAssemblyOwner(directory: dir, assemblyRoots: assemblyRoots) {
                dirsByProject[owner, default: []].append(dir)
            } else if let legacy = resolveLegacyProject(forDirectory: dir) {
                dirsByProject[legacy, default: []].append(dir)
            } else {
                unresolvedDirs.append(dir)
            }
        }

        return Result(asmDefByName: asmDefByName, dirsByProject: dirsByProject, unresolvedDirs: unresolvedDirs)
    }
}

// MARK: - Parallel filesystem scan (POSIX readdir)

private struct ScanBucket {
    var csDirs: [String] = []
    var asmDefPaths: [String] = []
    var asmRefPaths: [String] = []
}

private struct FileScan {
    let csDirs: [String]
    let asmDefPaths: [String]
    let asmRefPaths: [String]
}

private func processDirent(_ entry: UnsafeMutablePointer<dirent>, parentPath: String) -> (name: String, path: String, isDir: Bool)? {
    let name = direntName(entry)
    if name.first == "." || name.hasSuffix("~") { return nil }

    let childPath = "\(parentPath)/\(name)"
    let dType = entry.pointee.d_type
    var isDir = dType == DT_DIR
    var isFile = dType == DT_REG

    if dType == DT_LNK || dType == DT_UNKNOWN {
        if isDirectory(childPath) { isDir = true }
        else {
            var statBuf = stat()
            guard stat(childPath, &statBuf) == 0 else { return nil }
            isFile = (statBuf.st_mode & S_IFMT) == S_IFREG
        }
    }

    if isDir { return (name, childPath, true) }
    if isFile { return (name, childPath, false) }
    return nil
}

private func collectFile(name: String, path: String, prefixLen: Int, hasCS: inout Bool, bucket: inout ScanBucket) {
    if name.hasSuffix(".cs") {
        hasCS = true
    } else if name.hasSuffix(".asmdef") {
        bucket.asmDefPaths.append(String(path.dropFirst(prefixLen)))
    } else if name.hasSuffix(".asmref") {
        bucket.asmRefPaths.append(String(path.dropFirst(prefixLen)))
    }
}

private func posixScanDir(_ dirPath: String, prefixLen: Int, bucket: inout ScanBucket) {
    guard let dir = opendir(dirPath) else { return }
    defer { closedir(dir) }

    var hasCS = false

    while let entry = readdir(dir) {
        guard let (name, childPath, isDir) = processDirent(entry, parentPath: dirPath) else { continue }

        if isDir {
            posixScanDir(childPath, prefixLen: prefixLen, bucket: &bucket)
        } else {
            collectFile(name: name, path: childPath, prefixLen: prefixLen, hasCS: &hasCS, bucket: &bucket)
        }
    }

    if hasCS {
        let relDir = dirPath.count > prefixLen ? String(dirPath.dropFirst(prefixLen)) : ""
        bucket.csDirs.append(relDir)
    }
}

private func scanProjectFiles(rootPath: String, roots: [String]) -> FileScan {
    let prefixLen = rootPath.count + 1

    var walkTargets: [String] = []
    var rootBucket = ScanBucket()

    for root in roots {
        let rootDir = "\(rootPath)/\(root)"
        guard let dir = opendir(rootDir) else { continue }
        defer { closedir(dir) }

        var rootHasCS = false

        while let entry = readdir(dir) {
            guard let (name, childPath, isDir) = processDirent(entry, parentPath: rootDir) else { continue }

            if isDir {
                walkTargets.append(childPath)
            } else {
                collectFile(name: name, path: childPath, prefixLen: prefixLen, hasCS: &rootHasCS, bucket: &rootBucket)
            }
        }

        if rootHasCS {
            rootBucket.csDirs.append(root)
        }
    }

    let targets = walkTargets
    let count = targets.count
    let raw = UnsafeMutablePointer<ScanBucket>.allocate(capacity: count)
    raw.initialize(repeating: ScanBucket(), count: count)
    defer { raw.deinitialize(count: count); raw.deallocate() }
    let buckets = SendablePtr(ptr: raw)

    DispatchQueue.concurrentPerform(iterations: count) { i in
        var bucket = ScanBucket()
        posixScanDir(targets[i], prefixLen: prefixLen, bucket: &bucket)
        buckets[i] = bucket
    }

    var csDirs = rootBucket.csDirs
    var asmDefPaths = rootBucket.asmDefPaths
    var asmRefPaths = rootBucket.asmRefPaths
    for i in 0..<count {
        let b = raw[i]
        csDirs.append(contentsOf: b.csDirs)
        asmDefPaths.append(contentsOf: b.asmDefPaths)
        asmRefPaths.append(contentsOf: b.asmRefPaths)
    }

    return FileScan(csDirs: csDirs, asmDefPaths: asmDefPaths, asmRefPaths: asmRefPaths)
}

// MARK: - Category inference

private func inferCategory(includePlatforms: [String], defineConstraints: [String]) -> ProjectCategory {
    if defineConstraints.contains("UNITY_INCLUDE_TESTS") { return .test }
    if includePlatforms.count == 1 && includePlatforms[0] == "Editor" { return .editor }
    if defineConstraints.contains("UNITY_EDITOR") { return .editor }
    return .runtime
}

// MARK: - Source assignment

private func findAssemblyOwner(directory: String, assemblyRoots: [String: String]) -> String? {
    var current = directory
    while true {
        if let name = assemblyRoots[current] { return name }
        if current.isEmpty { break }
        current = parentDirectory(of: current)
    }
    return nil
}

private func loadAsmRef(rootPath: String, relativePath: String) throws -> (directory: String, reference: String)? {
    let json = try readFile(joinPath(rootPath, relativePath))
    guard let reference = extractJsonString(json, key: "reference") else { return nil }
    return (parentDirectory(of: relativePath), reference)
}

private func resolveLegacyProject(forDirectory directory: String) -> String? {
    let components = directory.split(separator: "/")
    guard components.first == "Assets" else { return nil }

    let isEditor = components.contains("Editor")
    let secondDir = components.count > 1 ? components[1] : Substring()
    let isFirstPass = secondDir == "Plugins"
        || secondDir == "Standard Assets"
        || secondDir == "Pro Standard Assets"

    if isEditor {
        return isFirstPass ? "Assembly-CSharp-Editor-firstpass" : "Assembly-CSharp-Editor"
    }
    return isFirstPass ? "Assembly-CSharp-firstpass" : "Assembly-CSharp"
}

// MARK: - Scan cache

/// Try loading cached scan results. Returns nil if cache is missing or stale.
private func loadCachedScan(_ cachePath: String, rootPath: String) -> FileScan? {
    guard let content = try? readFile(cachePath) else { return nil }

    var csDirs: [String] = []
    var asmDefPaths: [String] = []
    var asmRefPaths: [String] = []
    var dirMtimes: [(String, UInt64)] = []

    enum CacheSection { case cs, asmdef, asmref, mtimes }
    var section: CacheSection? = nil

    for rawLine in content.split(separator: "\n", omittingEmptySubsequences: false) {
        let line = String(rawLine)
        if line.isEmpty || line.hasPrefix("#") { continue }
        switch line {
        case "[cs]": section = .cs; continue
        case "[asmdef]": section = .asmdef; continue
        case "[asmref]": section = .asmref; continue
        case "[mtimes]": section = .mtimes; continue
        default: break
        }
        switch section {
        case .cs: csDirs.append(line)
        case .asmdef: asmDefPaths.append(line)
        case .asmref: asmRefPaths.append(line)
        case .mtimes:
            if let pipeIdx = line.firstIndex(of: "|"),
               let mtime = UInt64(line[line.index(after: pipeIdx)...]) {
                dirMtimes.append((String(line[..<pipeIdx]), mtime))
            }
        case nil: break
        }
    }

    guard !dirMtimes.isEmpty else { return nil }

    // Validate: stat each cached directory, check mtime matches
    let validateMark = ProfilerMark("scan.cache-validate")
    for (relDir, cachedMtime) in dirMtimes {
        let fullPath = relDir.isEmpty ? rootPath : joinPath(rootPath, relDir)
        var st = stat()
        guard stat(fullPath, &st) == 0 else { return nil }
        let currentMtime = UInt64(st.st_mtimespec.tv_sec) &* 1_000_000_000 &+ UInt64(st.st_mtimespec.tv_nsec)
        if currentMtime != cachedMtime { return nil }
    }
    validateMark.end()

    return FileScan(csDirs: csDirs, asmDefPaths: asmDefPaths, asmRefPaths: asmRefPaths)
}

/// Full scan + write cache for next time.
private func scanAndCache(rootPath: String, cachePath: String) -> FileScan {
    let fileScan = scanProjectFiles(rootPath: rootPath, roots: ["Assets", "Packages"])

    // Collect mtimes: scan roots + all directories that contributed to the scan result.
    // Track scan roots + all directories that contributed to results + all ancestors.
    // This ensures new files/dirs at any depth trigger cache invalidation.
    var allDirs: Set<String> = ["Assets", "Packages"]
    var addWithAncestors = { (dir: String) in
        allDirs.insert(dir)
        var parent = parentDirectory(of: dir)
        while !parent.isEmpty && allDirs.insert(parent).inserted {
            parent = parentDirectory(of: parent)
        }
    }
    for dir in fileScan.csDirs { addWithAncestors(dir) }
    for path in fileScan.asmDefPaths { addWithAncestors(parentDirectory(of: path)) }
    for path in fileScan.asmRefPaths { addWithAncestors(parentDirectory(of: path)) }

    var s = "# scan-cache — auto-generated, do not edit\n"
    s += "[cs]\n"
    for dir in fileScan.csDirs { s += "\(dir)\n" }
    s += "[asmdef]\n"
    for path in fileScan.asmDefPaths { s += "\(path)\n" }
    s += "[asmref]\n"
    for path in fileScan.asmRefPaths { s += "\(path)\n" }
    s += "[mtimes]\n"
    for dir in allDirs.sorted() {
        let fullPath = dir.isEmpty ? rootPath : joinPath(rootPath, dir)
        var st = stat()
        if stat(fullPath, &st) == 0 {
            let ns = UInt64(st.st_mtimespec.tv_sec) &* 1_000_000_000 &+ UInt64(st.st_mtimespec.tv_nsec)
            s += "\(dir)|\(ns)\n"
        }
    }

    createDirectoryRecursive(parentDirectory(of: cachePath))
    try? writeFileIfChanged(cachePath, s)

    return fileScan
}

