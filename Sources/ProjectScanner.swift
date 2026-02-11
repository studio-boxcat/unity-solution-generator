import Foundation

// MARK: - Public types

enum ProjectCategory: String, Sendable {
    case runtime
    case editor
    case test
}

struct AsmDefRecord: Sendable {
    let name: String
    let directory: String
    let references: [String]
    let category: ProjectCategory
    let includePlatforms: [String]
}

// MARK: - Scanner

struct ProjectScanner {
    struct Result: Sendable {
        let asmDefByName: [String: AsmDefRecord]
        let dirsByProject: [String: [String]]
        let unresolvedDirs: [String]
    }

    static func scan(projectRoot: URL) throws -> Result {
        let rootPath = resolveRealPath(projectRoot.path)
        let fileScan = scanProjectFiles(rootPath: rootPath, roots: ["Assets", "Packages"])

        // Single decoder shared across all JSON parsing (asmdef + asmref files).
        let decoder = JSONDecoder()

        // Load .asmdef files and build name → record map.
        // Unity enforces globally unique assembly definition names.
        var asmDefByName: [String: AsmDefRecord] = [:]
        for path in fileScan.asmDefPaths {
            let data = try Data(contentsOf: projectRoot.appendingPathComponent(path))
            let raw = try decoder.decode(RawAsmDef.self, from: data)
            let record = AsmDefRecord(
                name: raw.name,
                directory: parentDirectory(of: path),
                references: raw.references ?? [],
                category: inferCategory(from: raw),
                includePlatforms: raw.includePlatforms ?? []
            )
            guard asmDefByName[record.name] == nil else {
                throw GeneratorError.duplicateAsmDefName(record.name)
            }
            asmDefByName[record.name] = record
        }

        // Build assembly root map: directory → assembly name.
        // Unity allows at most one .asmdef or .asmref per directory (never both,
        // never multiples), so asmdef directories and asmref directories are disjoint.
        var assemblyRoots: [String: String] = [:]
        for (name, record) in asmDefByName {
            assemblyRoots[record.directory] = name
        }

        // .asmref files extend an existing assembly's source roots into another
        // directory tree. Skip orphaned .asmref files whose target doesn't exist.
        for path in fileScan.asmRefPaths {
            let data = try Data(contentsOf: projectRoot.appendingPathComponent(path))
            let raw = try decoder.decode(RawAsmRef.self, from: data)
            guard asmDefByName[raw.reference] != nil else { continue }
            assemblyRoots[parentDirectory(of: path)] = raw.reference
        }

        // Assign each directory containing .cs files to its owning assembly.
        // Walk upward from the directory until hitting an assembly root (asmdef/asmref).
        // Directories outside any assembly root fall back to Unity's legacy assembly
        // rules (Assembly-CSharp, Assembly-CSharp-Editor, etc.).
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

// MARK: - JSON types

private struct RawAsmDef: Decodable {
    let name: String
    let references: [String]?
    let includePlatforms: [String]?
    let defineConstraints: [String]?
}

private struct RawAsmRef: Decodable {
    let reference: String
}

private struct FileScan {
    let csDirs: [String]
    let asmDefPaths: [String]
    let asmRefPaths: [String]
}

// MARK: - Parallel filesystem scan (POSIX readdir)

private struct SendablePtr<T>: @unchecked Sendable {
    let ptr: UnsafeMutablePointer<T>
    subscript(index: Int) -> T {
        get { ptr[index] }
        nonmutating set { ptr[index] = newValue }
    }
}

// Unity allows at most one .asmdef or .asmref per directory (never both).
private struct ScanBucket {
    var csDirs: [String] = []
    var asmDefPaths: [String] = []
    var asmRefPaths: [String] = []
}

/// Resolve a dirent entry into (name, fullPath, isDirectory), skipping dot/tilde entries and
/// following symlinks. Returns nil for entries that should be skipped.
private func processDirent(_ entry: UnsafeMutablePointer<dirent>, parentPath: String) -> (name: String, path: String, isDir: Bool)? {
    var d_name = entry.pointee.d_name
    let name = withUnsafePointer(to: &d_name) {
        String(cString: UnsafeRawPointer($0).assumingMemoryBound(to: CChar.self))
    }
    if name.first == "." || name.hasSuffix("~") { return nil }

    let childPath = "\(parentPath)/\(name)"
    let dType = entry.pointee.d_type
    var isDir = dType == DT_DIR
    var isFile = dType == DT_REG

    if dType == DT_LNK || dType == DT_UNKNOWN {
        var statBuf = stat()
        guard stat(childPath, &statBuf) == 0 else { return nil }
        isDir = (statBuf.st_mode & S_IFMT) == S_IFDIR
        isFile = (statBuf.st_mode & S_IFMT) == S_IFREG
    }

    if isDir { return (name, childPath, true) }
    if isFile { return (name, childPath, false) }
    return nil
}

/// Classify a file by extension and append to the appropriate bucket field.
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

    // List immediate children of each root; directories become parallel walk targets.
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

    // Walk each subdirectory in parallel using POSIX readdir.
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

    // Merge results.
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

private func inferCategory(from rawAsmDef: RawAsmDef) -> ProjectCategory {
    let constraints = rawAsmDef.defineConstraints ?? []
    if constraints.contains("UNITY_INCLUDE_TESTS") { return .test }

    let platforms = rawAsmDef.includePlatforms ?? []
    if platforms.count == 1 && platforms[0] == "Editor" { return .editor }

    if constraints.contains("UNITY_EDITOR") { return .editor }

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
