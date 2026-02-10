import Foundation

enum ProjectCategory: String, Sendable {
    case runtime
    case editor
    case test
}

struct ProjectInfo: Sendable {
    let name: String
    let csprojPath: String
    let templatePath: String
    let guid: String
}

struct GenerateOptions: Sendable {
    let projectRoot: URL
    let templateRoot: String
    let verbose: Bool

    init(projectRoot: URL, templateRoot: String = "Library/UnitySolutionGenerator", verbose: Bool = false) {
        self.projectRoot = projectRoot
        self.templateRoot = templateRoot
        self.verbose = verbose
    }
}

struct ExtractTemplatesOptions: Sendable {
    let projectRoot: URL
    let templateRoot: String

    init(projectRoot: URL, templateRoot: String = "Library/UnitySolutionGenerator") {
        self.projectRoot = projectRoot
        self.templateRoot = templateRoot
    }
}

enum BuildPlatform: String, Sendable {
    case ios
    case android
}

struct PrepareBuildOptions: Sendable {
    let projectRoot: URL
    let platform: BuildPlatform
    let debugBuild: Bool

    init(projectRoot: URL, platform: BuildPlatform, debugBuild: Bool = false) {
        self.projectRoot = projectRoot
        self.platform = platform
        self.debugBuild = debugBuild
    }
}

struct PrepareBuildResult: Sendable {
    let generatedCsprojs: [String]
    let skippedCsprojs: [String]
}

struct GenerateResult: Sendable {
    let updatedFiles: [String]
    let warnings: [String]
    let stats: GenerationStats
}

struct GenerationStats: Sendable {
    let sourceCountByProject: [String: Int]
    let directoryPatternCountByProject: [String: Int]
    let unresolvedSourceCount: Int
}

enum GeneratorError: Error, CustomStringConvertible {
    case missingTemplate(URL)
    case noSolutionFound(URL)
    case noProjectsInSolution(URL)
    case duplicateAsmDefName(String)
    case noSlnTemplate(URL)

    var description: String {
        switch self {
        case .missingTemplate(let url):
            return "Missing template file: \(url.path)"
        case .noSolutionFound(let url):
            return "No .sln file found in: \(url.path)"
        case .noProjectsInSolution(let url):
            return "No C# projects found in solution: \(url.path)"
        case .duplicateAsmDefName(let name):
            return "Duplicate asmdef name: '\(name)'"
        case .noSlnTemplate(let url):
            return "No .sln.template file found in: \(url.path)"
        }
    }
}

struct AsmDefRecord: Sendable {
    let name: String
    let directory: String
    let references: [String]
    let category: ProjectCategory
    let includePlatforms: [String]
}

struct AsmRefRecord: Sendable {
    let directory: String
    let reference: String
}

struct RawAsmDef: Decodable {
    let name: String
    let references: [String]?
    let includePlatforms: [String]?
    let defineConstraints: [String]?
}

struct RawAsmRef: Decodable {
    let reference: String
}

struct ProjectFileScan: Sendable {
    let csFiles: [String]
    let asmDefPaths: [String]
    let asmRefPaths: [String]
}

private let csharpProjectTypeGuid = "{FAE04EC0-301F-11D3-BF4B-00C04F79EFBC}"

final class SolutionGenerator {
    private let fileManager: FileManager
    private let decoder: JSONDecoder

    init(fileManager: FileManager = .default) {
        self.fileManager = fileManager
        self.decoder = JSONDecoder()
    }

    // MARK: - Generate

    func generate(options: GenerateOptions) throws -> GenerateResult {
        let projectRoot = options.projectRoot.standardizedFileURL
        let templateRoot = options.templateRoot

        // Single filesystem walk for .cs, .asmdef, and .asmref files.
        let scan = try scanProjectFiles(projectRoot: projectRoot, roots: ["Assets", "Packages"])

        let asmDefs = try loadAsmDefsFromPaths(scan.asmDefPaths, projectRoot: projectRoot)
        let asmRefs = try loadAsmRefsFromPaths(scan.asmRefPaths, projectRoot: projectRoot)
        let asmDefByName = try buildUniqueMap(asmDefs, key: \.name)

        // Build assembly root map: directory → assembly name.
        var assemblyRoots: [String: String] = [:]
        for (name, record) in asmDefByName {
            assemblyRoots[record.directory] = name
        }
        for asmRef in asmRefs {
            guard asmDefByName[asmRef.reference] != nil else { continue }
            if assemblyRoots[asmRef.directory] == nil {
                assemblyRoots[asmRef.directory] = asmRef.reference
            }
        }

        // Assign sources per-directory.
        let csFilesByDirectory = Dictionary(grouping: scan.csFiles) { parentDirectory(of: $0) }
        var filesByProject: [String: [String]] = [:]
        var unresolvedFiles: [String] = []

        for (directory, files) in csFilesByDirectory {
            if let owner = findAssemblyOwner(directory: directory, assemblyRoots: assemblyRoots) {
                filesByProject[owner, default: []].append(contentsOf: files)
            } else if let legacy = resolveLegacyProject(forDirectory: directory) {
                filesByProject[legacy, default: []].append(contentsOf: files)
            } else {
                unresolvedFiles.append(contentsOf: files)
            }
        }

        // Discover projects from templates directory.
        let projects = try discoverProjects(templateRoot: templateRoot, projectRoot: projectRoot)
        let projectByName = Dictionary(uniqueKeysWithValues: projects.map { ($0.name, $0) })

        var warnings: [String] = []
        if !unresolvedFiles.isEmpty {
            warnings.append("Unresolved source files: \(unresolvedFiles.count)")
        }

        var patternsByProject: [String: [String]] = [:]
        var sourceCountByProject: [String: Int] = [:]

        for project in projects {
            let files = filesByProject[project.name] ?? []
            sourceCountByProject[project.name] = files.count
            patternsByProject[project.name] = makeCompilePatterns(files: files)
        }

        var updatedFiles: [String] = []

        for project in projects {
            let templateURL = projectRoot.appendingPathComponent(project.templatePath)
            guard fileManager.fileExists(atPath: templateURL.path) else {
                throw GeneratorError.missingTemplate(templateURL)
            }

            let template = try String(contentsOf: templateURL, encoding: .utf8)
            let sourceBlock = renderCompilePatterns(patternsByProject[project.name] ?? [])

            let referenceBlock = renderProjectReferences(
                for: project,
                asmDefByName: asmDefByName,
                projectByName: projectByName
            )

            let rendered = renderTemplate(
                template,
                replacements: [
                    "{{PROJECT_ROOT}}": projectRoot.path,
                    "{{SOURCE_FOLDERS}}": sourceBlock,
                    "{{PROJECT_REFERENCES}}": referenceBlock,
                ]
            )

            let outputURL = projectRoot.appendingPathComponent(project.csprojPath)
            if try writeIfChanged(content: rendered, to: outputURL) {
                updatedFiles.append(project.csprojPath)
            }
        }

        // Find and render .sln template.
        let slnTemplateURL = try findSlnTemplate(templateRoot: templateRoot, projectRoot: projectRoot)
        let slnTemplate = try String(contentsOf: slnTemplateURL, encoding: .utf8)
        let slnProjectEntries = renderSolutionProjectEntries(projects: projects)
        let slnProjectConfigs = renderSolutionProjectConfigs(projects: projects)
        let slnRendered = renderTemplate(
            slnTemplate,
            replacements: [
                "{{PROJECT_ENTRIES}}": slnProjectEntries,
                "{{PROJECT_CONFIGS}}": slnProjectConfigs,
            ]
        )

        // Derive .sln output path from template name: "Foo.sln.template" → "Foo.sln"
        let slnName = String(slnTemplateURL.lastPathComponent.dropLast(".template".count))
        let slnURL = projectRoot.appendingPathComponent(slnName)
        if try writeIfChanged(content: slnRendered, to: slnURL) {
            updatedFiles.append(slnName)
        }

        let stats = GenerationStats(
            sourceCountByProject: sourceCountByProject,
            directoryPatternCountByProject: patternsByProject.mapValues(\.count),
            unresolvedSourceCount: unresolvedFiles.count
        )

        if options.verbose {
            warnings += unresolvedFiles.prefix(20).map { "Unresolved: \($0)" }
        }

        return GenerateResult(updatedFiles: updatedFiles.sorted(), warnings: warnings, stats: stats)
    }

    // MARK: - Extract Templates

    func extractTemplates(options: ExtractTemplatesOptions) throws -> [String] {
        let projectRoot = options.projectRoot.standardizedFileURL
        let templateRoot = options.templateRoot

        // Find .sln and parse projects.
        let slnURL = try findSolutionFile(projectRoot: projectRoot)
        let slnContent = try String(contentsOf: slnURL, encoding: .utf8)
        let slnEntries = parseSolutionProjects(slnContent)
        guard let firstEntry = slnEntries.first else {
            throw GeneratorError.noProjectsInSolution(slnURL)
        }

        let projectRootPath = projectRoot.path

        var updatedFiles: [String] = []

        // Refresh csproj templates.
        for entry in slnEntries {
            let csprojURL = projectRoot.appendingPathComponent(entry.csprojPath)
            guard fileManager.fileExists(atPath: csprojURL.path) else {
                continue
            }

            let content = try String(contentsOf: csprojURL, encoding: .utf8)
            let template = templatizeCsproj(content, projectRoot: projectRootPath)

            let templatePath = "\(templateRoot)/csproj/\(entry.csprojPath).template"
            let templateURL = projectRoot.appendingPathComponent(templatePath)
            try fileManager.createDirectory(at: templateURL.deletingLastPathComponent(), withIntermediateDirectories: true)
            if try writeIfChanged(content: template, to: templateURL) {
                updatedFiles.append(templatePath)
            }
        }

        // Refresh sln template.
        let slnTemplate = templatizeSln(slnContent, projectTypeGuid: firstEntry.typeGuid)
        let slnName = slnURL.lastPathComponent
        let slnBaseName = String(slnName.dropLast(".sln".count))
        let slnTemplatePath = "\(templateRoot)/\(slnBaseName).sln.template"
        let slnTemplateURL = projectRoot.appendingPathComponent(slnTemplatePath)
        try fileManager.createDirectory(at: slnTemplateURL.deletingLastPathComponent(), withIntermediateDirectories: true)
        if try writeIfChanged(content: slnTemplate, to: slnTemplateURL) {
            updatedFiles.append(slnTemplatePath)
        }

        return updatedFiles.sorted()
    }

    // MARK: - Prepare Build

    func prepareBuild(options: PrepareBuildOptions) throws -> PrepareBuildResult {
        let projectRoot = options.projectRoot.standardizedFileURL
        let buildType = options.debugBuild ? "dev" : "prod"
        let suffix = ".v.\(options.platform.rawValue)-\(buildType)"

        // Discover categories from asmdef scan.
        let scan = try scanProjectFiles(projectRoot: projectRoot, roots: ["Assets", "Packages"])
        let asmDefs = try loadAsmDefsFromPaths(scan.asmDefPaths, projectRoot: projectRoot)
        let asmDefByName = try buildUniqueMap(asmDefs, key: \.name)

        // Scan .csproj files at project root.
        let csprojFiles = try fileManager.contentsOfDirectory(
            at: projectRoot,
            includingPropertiesForKeys: nil,
            options: [.skipsHiddenFiles]
        ).filter { $0.pathExtension == "csproj" }

        // Categorize each project.
        struct CsprojEntry {
            let name: String
            let csprojPath: String
            let category: ProjectCategory
            let matchesPlatform: Bool
        }

        let targetPlatformName = options.platform == .ios ? "iOS" : "Android"

        var entries: [CsprojEntry] = []
        for url in csprojFiles {
            let filename = url.lastPathComponent
            let name = String(filename.dropLast(".csproj".count))

            // Skip platform variant files (already generated).
            if name.contains(".v.") { continue }

            let category: ProjectCategory
            let matchesPlatform: Bool
            if let asmDef = asmDefByName[name] {
                category = asmDef.category
                // Empty includePlatforms = all platforms. Otherwise must contain the target.
                let platforms = asmDef.includePlatforms.filter { $0 != "Editor" }
                matchesPlatform = platforms.isEmpty || platforms.contains(targetPlatformName)
            } else {
                category = .runtime
                matchesPlatform = true
            }
            entries.append(CsprojEntry(name: name, csprojPath: filename, category: category, matchesPlatform: matchesPlatform))
        }

        let runtimeEntries = entries.filter { $0.category == .runtime && $0.matchesPlatform }
        let nonRuntimeNames = Set(entries.filter { $0.category != .runtime || !$0.matchesPlatform }.map(\.name))

        var generated: [String] = []
        var skipped: [String] = []

        for entry in runtimeEntries {
            let srcURL = projectRoot.appendingPathComponent(entry.csprojPath)
            let baseName = String(entry.csprojPath.dropLast(".csproj".count))
            let dstName = "\(baseName)\(suffix).csproj"
            let dstURL = projectRoot.appendingPathComponent(dstName)

            // Skip if suffixed file is newer than source (cached).
            if fileManager.fileExists(atPath: dstURL.path),
               let srcDate = modificationDate(of: srcURL),
               let dstDate = modificationDate(of: dstURL),
               dstDate >= srcDate
            {
                skipped.append(dstName)
                continue
            }

            var content = try String(contentsOf: srcURL, encoding: .utf8)
            content = stripEditorDefines(content, debugBuild: options.debugBuild)
            content = swapPlatformDefines(content, platform: options.platform)
            content = stripNonRuntimeReferences(content, nonRuntimeNames: nonRuntimeNames)
            content = rewriteReferenceSuffix(content, suffix: suffix)
            try content.write(to: dstURL, atomically: true, encoding: .utf8)
            generated.append(dstName)
        }

        return PrepareBuildResult(
            generatedCsprojs: generated.sorted(),
            skippedCsprojs: skipped.sorted()
        )
    }

    // MARK: - Project discovery

    private func discoverProjects(
        templateRoot: String,
        projectRoot: URL
    ) throws -> [ProjectInfo] {
        let templateDir = projectRoot
            .appendingPathComponent(templateRoot)
            .appendingPathComponent("csproj")

        guard fileManager.fileExists(atPath: templateDir.path) else {
            return []
        }

        let templateFiles = try fileManager.contentsOfDirectory(
            at: templateDir,
            includingPropertiesForKeys: nil,
            options: [.skipsHiddenFiles]
        ).filter { $0.pathExtension == "template" && $0.lastPathComponent.contains(".csproj.") }

        return templateFiles.map { url in
            let filename = url.lastPathComponent
            // "Foo.csproj.template" → "Foo"
            let name = String(filename.dropLast(".csproj.template".count))
            return ProjectInfo(
                name: name,
                csprojPath: "\(name).csproj",
                templatePath: "\(templateRoot)/csproj/\(filename)",
                guid: deterministicGuid(for: name)
            )
        }.sorted { $0.name < $1.name }
    }

    private func findSlnTemplate(templateRoot: String, projectRoot: URL) throws -> URL {
        let dir = projectRoot.appendingPathComponent(templateRoot)
        guard fileManager.fileExists(atPath: dir.path) else {
            throw GeneratorError.noSlnTemplate(dir)
        }
        let contents = try fileManager.contentsOfDirectory(
            at: dir,
            includingPropertiesForKeys: nil,
            options: [.skipsHiddenFiles]
        )
        guard let template = contents.first(where: { $0.lastPathComponent.hasSuffix(".sln.template") }) else {
            throw GeneratorError.noSlnTemplate(dir)
        }
        return template
    }

    // MARK: - Build validation helpers

    func stripEditorDefines(_ content: String, debugBuild: Bool) -> String {
        var result = content
        // Remove UNITY_EDITOR, UNITY_EDITOR_64, UNITY_EDITOR_OSX defines
        let editorPattern = "UNITY_EDITOR(_64|_OSX)?;"
        if let regex = try? NSRegularExpression(pattern: editorPattern) {
            result = regex.stringByReplacingMatches(
                in: result, range: NSRange(result.startIndex..., in: result), withTemplate: ""
            )
        }
        if !debugBuild {
            // Strip DEBUG and TRACE defines for release builds
            let debugPattern = "(DEBUG|TRACE);"
            if let regex = try? NSRegularExpression(pattern: debugPattern) {
                result = regex.stringByReplacingMatches(
                    in: result, range: NSRange(result.startIndex..., in: result), withTemplate: ""
                )
            }
        }
        return result
    }

    func swapPlatformDefines(_ content: String, platform: BuildPlatform) -> String {
        var result = content
        switch platform {
        case .ios:
            result = result.replacingOccurrences(of: "UNITY_ANDROID;", with: "UNITY_IOS;")
        case .android:
            // Remove legacy alias first, then swap
            result = result.replacingOccurrences(of: "UNITY_IPHONE;", with: "")
            result = result.replacingOccurrences(of: "UNITY_IOS;", with: "UNITY_ANDROID;")
        }
        return result
    }

    func stripNonRuntimeReferences(_ content: String, nonRuntimeNames: Set<String>) -> String {
        guard !nonRuntimeNames.isEmpty else { return content }
        let escaped = nonRuntimeNames.map { NSRegularExpression.escapedPattern(for: $0) }
        let alternation = escaped.sorted().joined(separator: "|")
        let pattern = #"<ProjectReference Include="(\#(alternation))\.csproj">.*?</ProjectReference>"#
        guard let regex = try? NSRegularExpression(pattern: pattern, options: .dotMatchesLineSeparators) else {
            return content
        }
        return regex.stringByReplacingMatches(
            in: content, range: NSRange(content.startIndex..., in: content), withTemplate: ""
        )
    }

    func rewriteReferenceSuffix(_ content: String, suffix: String) -> String {
        let pattern = #"(<ProjectReference Include=")([^"]+)(\.csproj">)"#
        guard let regex = try? NSRegularExpression(pattern: pattern) else { return content }
        let escapedSuffix = NSRegularExpression.escapedTemplate(for: suffix)
        return regex.stringByReplacingMatches(
            in: content,
            range: NSRange(content.startIndex..., in: content),
            withTemplate: "$1$2\(escapedSuffix)$3"
        )
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

    // MARK: - Template extraction

    private func templatizeCsproj(_ content: String, projectRoot: String) -> String {
        var lines: [String] = []
        var sourcePlaceholderEmitted = false
        var refsPlaceholderEmitted = false
        var inProjectReference = false
        var inComment = false

        for rawLine in content.split(separator: "\n", omittingEmptySubsequences: false) {
            let line = String(rawLine)
                .replacingOccurrences(of: projectRoot, with: "{{PROJECT_ROOT}}")

            // Multi-line comment tracking.
            if inComment {
                if line.contains("-->") {
                    inComment = false
                }
                continue
            }
            if line.contains("<!--") {
                if !line.contains("-->") {
                    inComment = true
                }
                continue
            }

            // Skip <None Include="..."> entries.
            if line.contains("<None Include=\"") {
                continue
            }

            // Collapse <ProjectReference> blocks into placeholder.
            if inProjectReference {
                if line.contains("</ProjectReference>") {
                    inProjectReference = false
                }
                continue
            }
            if line.contains("<ProjectReference Include=\"") {
                if !refsPlaceholderEmitted {
                    lines.append("    {{PROJECT_REFERENCES}}")
                    refsPlaceholderEmitted = true
                }
                inProjectReference = true
                continue
            }

            // Collapse <Compile Include="..."> lines into placeholder.
            if line.contains("<Compile Include=\"") {
                if !sourcePlaceholderEmitted {
                    lines.append("    {{SOURCE_FOLDERS}}")
                    sourcePlaceholderEmitted = true
                }
                continue
            }

            lines.append(line)
        }

        return lines.joined(separator: "\n")
    }

    private func templatizeSln(_ content: String, projectTypeGuid: String) -> String {
        var lines: [String] = []
        var inProjectBlock = false
        var projectPlaceholderEmitted = false
        var configPlaceholderEmitted = false

        let projectPrefix = "Project(\"\(projectTypeGuid)\") = "

        for rawLine in content.split(separator: "\n", omittingEmptySubsequences: false) {
            let line = String(rawLine)

            // Collapse Project blocks.
            if line.hasPrefix(projectPrefix) {
                if !projectPlaceholderEmitted {
                    lines.append("{{PROJECT_ENTRIES}}")
                    projectPlaceholderEmitted = true
                }
                inProjectBlock = true
                continue
            }

            if inProjectBlock {
                if line == "Global" {
                    inProjectBlock = false
                    lines.append(line)
                }
                continue
            }

            // Collapse per-project config lines (GUID.Debug|Any CPU...).
            if line.contains(".Debug|Any CPU.") {
                let trimmed = line.trimmingCharacters(in: .whitespaces)
                if trimmed.hasPrefix("{") && (trimmed.contains("ActiveCfg") || trimmed.contains("Build.0")) {
                    if !configPlaceholderEmitted {
                        lines.append("{{PROJECT_CONFIGS}}")
                        configPlaceholderEmitted = true
                    }
                    continue
                }
            }

            lines.append(line)
        }

        return lines.joined(separator: "\n")
    }

    // MARK: - Solution parsing

    private func findSolutionFile(projectRoot: URL) throws -> URL {
        let contents = try fileManager.contentsOfDirectory(
            at: projectRoot,
            includingPropertiesForKeys: [.isRegularFileKey],
            options: [.skipsHiddenFiles]
        )
        guard let slnURL = contents.first(where: { $0.pathExtension == "sln" }) else {
            throw GeneratorError.noSolutionFound(projectRoot)
        }
        return slnURL
    }

    private struct SlnProjectEntry {
        let typeGuid: String
        let csprojPath: String
    }

    private func parseSolutionProjects(_ content: String) -> [SlnProjectEntry] {
        var results: [SlnProjectEntry] = []
        for line in content.split(separator: "\n") {
            guard line.hasPrefix("Project(\"") else { continue }

            var quoted: [String] = []
            var inQuote = false
            var current = ""
            for ch in line {
                if ch == "\"" {
                    if inQuote {
                        quoted.append(current)
                        current = ""
                    }
                    inQuote = !inQuote
                } else if inQuote {
                    current.append(ch)
                }
            }

            guard quoted.count >= 3 else { continue }

            let typeGuid = quoted[0]
            let csprojPath = quoted[2]

            guard csprojPath.hasSuffix(".csproj"), !csprojPath.contains("/") else {
                continue
            }

            results.append(SlnProjectEntry(typeGuid: typeGuid, csprojPath: csprojPath))
        }
        return results
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

    // MARK: - Compile patterns

    private func makeCompilePatterns(files: [String]) -> [String] {
        Set(files.map(parentDirectory(of:))).sorted().map { directory in
            directory.isEmpty ? "*.cs" : "\(directory)/*.cs"
        }
    }

    // MARK: - Rendering

    private func renderCompilePatterns(_ patterns: [String]) -> String {
        patterns
            .map { "    <Compile Include=\"\(xmlEscape($0))\" />" }
            .joined(separator: "\n")
    }

    private func renderProjectReferences(
        for project: ProjectInfo,
        asmDefByName: [String: AsmDefRecord],
        projectByName: [String: ProjectInfo]
    ) -> String {
        guard let asmDef = asmDefByName[project.name] else { return "" }

        var seen: Set<String> = []
        var blocks: [String] = []

        for reference in asmDef.references {
            guard let ref = projectByName[reference],
                  seen.insert(reference).inserted else {
                continue
            }

            blocks.append([
                "    <ProjectReference Include=\"\(xmlEscape(ref.csprojPath))\">",
                "      <Project>\(ref.guid)</Project>",
                "      <Name>\(xmlEscape(ref.name))</Name>",
                "    </ProjectReference>",
            ].joined(separator: "\n"))
        }

        return blocks.joined(separator: "\n")
    }

    private func renderSolutionProjectEntries(projects: [ProjectInfo]) -> String {
        projects
            .map { project in
                [
                    "Project(\"\(csharpProjectTypeGuid)\") = \"\(project.name)\", \"\(project.csprojPath)\", \"\(project.guid)\"",
                    "EndProject",
                ].joined(separator: "\n")
            }
            .joined(separator: "\n")
    }

    private func renderSolutionProjectConfigs(projects: [ProjectInfo]) -> String {
        projects
            .map { project in
                [
                    "\t\t\(project.guid).Debug|Any CPU.ActiveCfg = Debug|Any CPU",
                    "\t\t\(project.guid).Debug|Any CPU.Build.0 = Debug|Any CPU",
                ].joined(separator: "\n")
            }
            .joined(separator: "\n")
    }

    // MARK: - Filesystem scanning

    private func scanProjectFiles(projectRoot: URL, roots: [String]) throws -> ProjectFileScan {
        var csFiles: [String] = []
        var asmDefPaths: [String] = []
        var asmRefPaths: [String] = []

        // Use realpath to match the enumerator's resolved paths
        // (macOS: /var → /private/var firmlink).
        let rootPath = resolveRealPath(projectRoot.path)
        let prefixLen = rootPath.count + 1

        for root in roots {
            let rootURL = URL(fileURLWithPath: rootPath).appendingPathComponent(root)
            guard fileManager.fileExists(atPath: rootURL.path) else { continue }

            guard let enumerator = fileManager.enumerator(
                at: rootURL,
                includingPropertiesForKeys: nil,
                options: [.skipsHiddenFiles]
            ) else { continue }

            while let next = enumerator.nextObject() as? URL {
                let path = next.path
                guard path.count > prefixLen else { continue }

                // Skip directories ending with ~ (Unity ignored folders).
                // Note: .skipsHiddenFiles handles dot-prefixed dirs/files.
                if path.hasSuffix("~") {
                    enumerator.skipDescendants()
                    continue
                }

                if path.hasSuffix(".cs") {
                    csFiles.append(String(path.dropFirst(prefixLen)))
                } else if path.hasSuffix(".asmdef") {
                    asmDefPaths.append(String(path.dropFirst(prefixLen)))
                } else if path.hasSuffix(".asmref") {
                    asmRefPaths.append(String(path.dropFirst(prefixLen)))
                }
            }
        }

        csFiles.sort()
        asmDefPaths.sort()
        asmRefPaths.sort()
        return ProjectFileScan(csFiles: csFiles, asmDefPaths: asmDefPaths, asmRefPaths: asmRefPaths)
    }

    private func loadAsmDefsFromPaths(_ paths: [String], projectRoot: URL) throws -> [AsmDefRecord] {
        try paths.compactMap { path in
            let url = projectRoot.appendingPathComponent(path)
            let data = try Data(contentsOf: url)
            let raw = try decoder.decode(RawAsmDef.self, from: data)
            return AsmDefRecord(
                name: raw.name,
                directory: parentDirectory(of: path),
                references: raw.references ?? [],
                category: inferCategory(from: raw),
                includePlatforms: raw.includePlatforms ?? []
            )
        }
    }

    private func loadAsmRefsFromPaths(_ paths: [String], projectRoot: URL) throws -> [AsmRefRecord] {
        try paths.compactMap { path in
            let url = projectRoot.appendingPathComponent(path)
            let data = try Data(contentsOf: url)
            let raw = try decoder.decode(RawAsmRef.self, from: data)
            return AsmRefRecord(directory: parentDirectory(of: path), reference: raw.reference)
        }
    }

    private func buildUniqueMap(_ records: [AsmDefRecord], key: KeyPath<AsmDefRecord, String>) throws -> [String: AsmDefRecord] {
        var map: [String: AsmDefRecord] = [:]
        for record in records {
            let k = record[keyPath: key]
            if map[k] != nil {
                throw GeneratorError.duplicateAsmDefName(k)
            }
            map[k] = record
        }
        return map
    }
}
