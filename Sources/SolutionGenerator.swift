import Foundation

enum ProjectKind: String, Codable, Sendable {
    case asmdef
    case legacy
}

enum ProjectCategory: String, Codable, Sendable {
    case runtime
    case editor
    case test
}

struct ProjectEntry: Codable, Sendable {
    let name: String
    let csprojPath: String
    let templatePath: String
    let guid: String
    let kind: ProjectKind
    let category: ProjectCategory

    init(name: String, csprojPath: String, templatePath: String, guid: String, kind: ProjectKind, category: ProjectCategory = .runtime) {
        self.name = name
        self.csprojPath = csprojPath
        self.templatePath = templatePath
        self.guid = guid
        self.kind = kind
        self.category = category
    }

    private enum CodingKeys: String, CodingKey {
        case name, csprojPath, templatePath, guid, kind, category
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        name = try container.decode(String.self, forKey: .name)
        csprojPath = try container.decode(String.self, forKey: .csprojPath)
        templatePath = try container.decode(String.self, forKey: .templatePath)
        guid = try container.decode(String.self, forKey: .guid)
        kind = try container.decode(ProjectKind.self, forKey: .kind)
        category = try container.decodeIfPresent(ProjectCategory.self, forKey: .category) ?? .runtime
    }
}

struct GeneratorManifest: Codable, Sendable {
    let solutionPath: String
    let solutionTemplatePath: String
    let projectTypeGuid: String
    let projects: [ProjectEntry]

    init(solutionPath: String, solutionTemplatePath: String, projectTypeGuid: String, projects: [ProjectEntry]) {
        self.solutionPath = solutionPath
        self.solutionTemplatePath = solutionTemplatePath
        self.projectTypeGuid = projectTypeGuid
        self.projects = projects
    }
}

struct GenerateOptions: Sendable {
    let projectRoot: URL
    let manifestPath: String
    let verbose: Bool

    init(projectRoot: URL, manifestPath: String, verbose: Bool = false) {
        self.projectRoot = projectRoot
        self.manifestPath = manifestPath
        self.verbose = verbose
    }
}

struct InitManifestOptions: Sendable {
    let projectRoot: URL
    let manifestPath: String
    let templateRoot: String

    init(projectRoot: URL, manifestPath: String, templateRoot: String) {
        self.projectRoot = projectRoot
        self.manifestPath = manifestPath
        self.templateRoot = templateRoot
    }
}

struct RefreshTemplatesOptions: Sendable {
    let projectRoot: URL
    let manifestPath: String

    init(projectRoot: URL, manifestPath: String) {
        self.projectRoot = projectRoot
        self.manifestPath = manifestPath
    }
}

enum BuildPlatform: String, Sendable {
    case ios
    case android
}

struct PrepareBuildOptions: Sendable {
    let projectRoot: URL
    let manifestPath: String
    let platform: BuildPlatform
    let debugBuild: Bool

    init(projectRoot: URL, manifestPath: String, platform: BuildPlatform, debugBuild: Bool = false) {
        self.projectRoot = projectRoot
        self.manifestPath = manifestPath
        self.platform = platform
        self.debugBuild = debugBuild
    }
}

struct PrepareBuildResult: Sendable {
    let generatedCsprojs: [String]
    let skippedCsprojs: [String]
    let suffix: String
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
    case missingManifest(URL)
    case invalidManifest(URL)
    case missingTemplate(URL)
    case missingAsmDefForProject(String)
    case failedToResolveProjectReference(project: String, reference: String)
    case invalidProjectVersion(URL)
    case missingSolution(URL)
    case noSolutionFound(URL)
    case noProjectsInSolution(URL)
    case duplicateAsmDefName(String)

    var description: String {
        switch self {
        case .missingManifest(let url):
            return "Missing manifest: \(url.path)"
        case .invalidManifest(let url):
            return "Invalid manifest JSON: \(url.path)"
        case .missingTemplate(let url):
            return "Missing template file: \(url.path)"
        case .missingAsmDefForProject(let project):
            return "Project '\(project)' is asmdef-based but no matching .asmdef was found"
        case .failedToResolveProjectReference(let project, let reference):
            return "Project '\(project)' references unknown assembly '\(reference)'"
        case .invalidProjectVersion(let url):
            return "Could not parse Unity editor version from: \(url.path)"
        case .missingSolution(let url):
            return "Missing solution file: \(url.path)"
        case .noSolutionFound(let url):
            return "No .sln file found in: \(url.path)"
        case .noProjectsInSolution(let url):
            return "No C# projects found in solution: \(url.path)"
        case .duplicateAsmDefName(let name):
            return "Duplicate asmdef name: '\(name)'"
        }
    }
}

struct AsmDefRecord: Sendable {
    let name: String
    let directory: String
    let guid: String?
    let references: [String]
}

struct AsmRefRecord: Sendable {
    let directory: String
    let reference: String
}

struct SourceAssignments: Sendable {
    let filesByProject: [String: [String]]
    let unresolvedFiles: [String]
}

struct CompilePattern: Sendable {
    let include: String
    let exclude: [String]
}

struct RawAsmDef: Decodable {
    let name: String
    let references: [String]?
}

struct RawAsmRef: Decodable {
    let reference: String
}

struct ProjectFileScan: Sendable {
    let csFiles: [String]
    let asmDefPaths: [String]
    let asmRefPaths: [String]
    let ignoredDirectories: [String]
}

private enum DirectoryResolution {
    case assembly(String)
    case none
}

final class SolutionGenerator {
    private let fileManager: FileManager
    private let decoder: JSONDecoder

    init(fileManager: FileManager = .default) {
        self.fileManager = fileManager
        self.decoder = JSONDecoder()
    }

    func generate(options: GenerateOptions) throws -> GenerateResult {
        let projectRoot = options.projectRoot.standardizedFileURL
        let manifestURL = projectRoot.appendingPathComponent(options.manifestPath)

        guard fileManager.fileExists(atPath: manifestURL.path) else {
            throw GeneratorError.missingManifest(manifestURL)
        }

        let manifest = try loadManifest(manifestURL)
        let unityVersion = try loadUnityVersion(projectRoot: projectRoot)

        let knownProjects = Set(manifest.projects.map(\.name))
        let projectByName = Dictionary(uniqueKeysWithValues: manifest.projects.map { ($0.name, $0) })

        // Single filesystem walk for .cs, .asmdef, and .asmref files.
        let scan = try scanProjectFiles(projectRoot: projectRoot, roots: ["Assets", "Packages"])

        let asmDefs = try loadAsmDefsFromPaths(scan.asmDefPaths, projectRoot: projectRoot)
        let asmRefs = try loadAsmRefsFromPaths(scan.asmRefPaths, projectRoot: projectRoot)
        let asmDefByName = try buildUniqueMap(asmDefs, key: \.name)
        let guidToAssembly = buildGuidToAssemblyMap(asmDefByName)

        let assemblyRoots = resolveAssemblyRoots(
            asmDefByName: asmDefByName,
            asmRefs: asmRefs,
            projectNames: knownProjects,
            guidToAssembly: guidToAssembly
        )

        let assignments = assignSources(
            sourceFiles: scan.csFiles,
            assemblyRoots: assemblyRoots,
            knownProjects: knownProjects
        )

        var warnings: [String] = []
        if !assignments.unresolvedFiles.isEmpty {
            warnings.append("Unresolved source files: \(assignments.unresolvedFiles.count)")
        }

        var compilePatternsByProject: [String: [CompilePattern]] = [:]
        var sourceCountByProject: [String: Int] = [:]

        for project in manifest.projects {
            let files = assignments.filesByProject[project.name] ?? []
            sourceCountByProject[project.name] = files.count
            compilePatternsByProject[project.name] = makeCompilePatterns(
                for: project,
                files: files,
                assemblyRoots: assemblyRoots,
                ignoredDirectories: scan.ignoredDirectories
            )
        }

        var updatedFiles: [String] = []

        for project in manifest.projects {
            let templateURL = projectRoot.appendingPathComponent(project.templatePath)
            guard fileManager.fileExists(atPath: templateURL.path) else {
                throw GeneratorError.missingTemplate(templateURL)
            }

            let template = try String(contentsOf: templateURL, encoding: .utf8)
            let compilePatterns = compilePatternsByProject[project.name] ?? []
            let sourceBlock = renderCompilePatterns(compilePatterns)

            let referenceBlock = try renderProjectReferences(
                for: project,
                asmDefByName: asmDefByName,
                guidToAssembly: guidToAssembly,
                projectByName: projectByName
            )

            let rendered = renderTemplate(
                template,
                replacements: [
                    "{{UNITY_VER}}": unityVersion,
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

        let slnTemplateURL = projectRoot.appendingPathComponent(manifest.solutionTemplatePath)
        guard fileManager.fileExists(atPath: slnTemplateURL.path) else {
            throw GeneratorError.missingTemplate(slnTemplateURL)
        }

        let slnTemplate = try String(contentsOf: slnTemplateURL, encoding: .utf8)
        let slnProjectEntries = renderSolutionProjectEntries(manifest: manifest)
        let slnProjectConfigs = renderSolutionProjectConfigs(manifest: manifest)
        let slnRendered = renderTemplate(
            slnTemplate,
            replacements: [
                "{{PROJECT_ENTRIES}}": slnProjectEntries,
                "{{PROJECT_CONFIGS}}": slnProjectConfigs,
            ]
        )

        let slnURL = projectRoot.appendingPathComponent(manifest.solutionPath)
        if try writeIfChanged(content: slnRendered, to: slnURL) {
            updatedFiles.append(manifest.solutionPath)
        }

        let stats = GenerationStats(
            sourceCountByProject: sourceCountByProject,
            directoryPatternCountByProject: compilePatternsByProject.mapValues(\.count),
            unresolvedSourceCount: assignments.unresolvedFiles.count
        )

        if options.verbose {
            warnings += assignments.unresolvedFiles.prefix(20).map { "Unresolved: \($0)" }
        }

        return GenerateResult(updatedFiles: updatedFiles.sorted(), warnings: warnings, stats: stats)
    }

    func initManifest(options: InitManifestOptions) throws -> URL {
        let projectRoot = options.projectRoot.standardizedFileURL
        let templateRoot = options.templateRoot
        let manifestURL = projectRoot.appendingPathComponent(options.manifestPath)

        // Find the .sln file (first .sln at project root).
        let slnURL = try findSolutionFile(projectRoot: projectRoot)
        let slnName = slnURL.lastPathComponent

        let slnContent = try String(contentsOf: slnURL, encoding: .utf8)
        let slnEntries = parseSolutionProjects(slnContent)
        guard let firstEntry = slnEntries.first else {
            throw GeneratorError.noProjectsInSolution(slnURL)
        }

        // Scan asmdefs to determine project kind.
        let scan = try scanProjectFiles(projectRoot: projectRoot, roots: ["Assets", "Packages"])
        let asmDefs = try loadAsmDefsFromPaths(scan.asmDefPaths, projectRoot: projectRoot)
        let asmDefByName = try buildUniqueMap(asmDefs, key: \.name)

        let slnBaseName = String(slnName.dropLast(".sln".count))

        let projects = slnEntries.map { entry -> ProjectEntry in
            let kind: ProjectKind = asmDefByName[entry.name] != nil ? .asmdef : .legacy
            return ProjectEntry(
                name: entry.name,
                csprojPath: entry.csprojPath,
                templatePath: "\(templateRoot)/csproj/\(entry.csprojPath).template",
                guid: entry.projectGuid,
                kind: kind,
                category: inferCategory(name: entry.name)
            )
        }

        let manifest = GeneratorManifest(
            solutionPath: slnName,
            solutionTemplatePath: "\(templateRoot)/\(slnBaseName).sln.template",
            projectTypeGuid: firstEntry.typeGuid,
            projects: projects
        )

        let encoder = JSONEncoder()
        encoder.outputFormatting = [.prettyPrinted, .sortedKeys, .withoutEscapingSlashes]
        let data = try encoder.encode(manifest)
        let json = String(decoding: data, as: UTF8.self) + "\n"

        try fileManager.createDirectory(
            at: manifestURL.deletingLastPathComponent(),
            withIntermediateDirectories: true
        )
        try writeIfChanged(content: json, to: manifestURL)

        return manifestURL
    }

    func refreshTemplates(options: RefreshTemplatesOptions) throws -> [String] {
        let projectRoot = options.projectRoot.standardizedFileURL
        let manifestURL = projectRoot.appendingPathComponent(options.manifestPath)

        guard fileManager.fileExists(atPath: manifestURL.path) else {
            throw GeneratorError.missingManifest(manifestURL)
        }

        let manifest = try loadManifest(manifestURL)
        let unityVersion = try loadUnityVersion(projectRoot: projectRoot)
        let projectRootPath = projectRoot.path

        var updatedFiles: [String] = []

        // Refresh csproj templates.
        for project in manifest.projects {
            let csprojURL = projectRoot.appendingPathComponent(project.csprojPath)
            guard fileManager.fileExists(atPath: csprojURL.path) else {
                continue
            }

            let content = try String(contentsOf: csprojURL, encoding: .utf8)
            let template = templatizeCsproj(content, projectRoot: projectRootPath, unityVersion: unityVersion)

            let templateURL = projectRoot.appendingPathComponent(project.templatePath)
            try fileManager.createDirectory(at: templateURL.deletingLastPathComponent(), withIntermediateDirectories: true)
            if try writeIfChanged(content: template, to: templateURL) {
                updatedFiles.append(project.templatePath)
            }
        }

        // Refresh sln template.
        let slnURL = projectRoot.appendingPathComponent(manifest.solutionPath)
        guard fileManager.fileExists(atPath: slnURL.path) else {
            throw GeneratorError.missingSolution(slnURL)
        }

        let slnContent = try String(contentsOf: slnURL, encoding: .utf8)
        let slnTemplate = templatizeSln(slnContent, projectTypeGuid: manifest.projectTypeGuid)

        let slnTemplateURL = projectRoot.appendingPathComponent(manifest.solutionTemplatePath)
        try fileManager.createDirectory(at: slnTemplateURL.deletingLastPathComponent(), withIntermediateDirectories: true)
        if try writeIfChanged(content: slnTemplate, to: slnTemplateURL) {
            updatedFiles.append(manifest.solutionTemplatePath)
        }

        return updatedFiles.sorted()
    }

    func prepareBuild(options: PrepareBuildOptions) throws -> PrepareBuildResult {
        let projectRoot = options.projectRoot.standardizedFileURL
        let manifestURL = projectRoot.appendingPathComponent(options.manifestPath)

        guard fileManager.fileExists(atPath: manifestURL.path) else {
            throw GeneratorError.missingManifest(manifestURL)
        }

        let manifest = try loadManifest(manifestURL)
        let buildType = options.debugBuild ? "dev" : "prod"
        let suffix = ".v.\(options.platform.rawValue)-\(buildType)"

        let runtimeProjects = manifest.projects.filter { $0.category == .runtime }
        let nonRuntimeNames = Set(
            manifest.projects.filter { $0.category != .runtime }.map(\.name)
        )

        var generated: [String] = []
        var skipped: [String] = []

        for project in runtimeProjects {
            let srcURL = projectRoot.appendingPathComponent(project.csprojPath)
            guard fileManager.fileExists(atPath: srcURL.path) else { continue }

            let baseName = String(project.csprojPath.dropLast(".csproj".count))
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
            skippedCsprojs: skipped.sorted(),
            suffix: suffix
        )
    }

    // MARK: - Build validation helpers

    private func modificationDate(of url: URL) -> Date? {
        try? fileManager.attributesOfItem(atPath: url.path)[.modificationDate] as? Date
    }

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

    private func inferCategory(name: String) -> ProjectCategory {
        let lower = name.lowercased()
        if lower.contains("editor") { return .editor }
        if lower.contains(".tests.") || lower.contains("testrunner") { return .test }
        return .runtime
    }

    private func templatizeCsproj(_ content: String, projectRoot: String, unityVersion: String) -> String {
        var lines: [String] = []
        var sourcePlaceholderEmitted = false
        var refsPlaceholderEmitted = false
        var inProjectReference = false
        var inComment = false

        for rawLine in content.split(separator: "\n", omittingEmptySubsequences: false) {
            let line = String(rawLine)
                .replacingOccurrences(of: projectRoot, with: "{{PROJECT_ROOT}}")
                .replacingOccurrences(of: unityVersion, with: "{{UNITY_VER}}")

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
        let name: String
        let csprojPath: String
        let projectGuid: String
    }

    private func parseSolutionProjects(_ content: String) -> [SlnProjectEntry] {
        // Match: Project("{TYPE-GUID}") = "Name", "Path.csproj", "{PROJ-GUID}"
        var results: [SlnProjectEntry] = []
        for line in content.split(separator: "\n") {
            guard line.hasPrefix("Project(\"") else { continue }

            // Extract the 4 quoted strings.
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

            guard quoted.count >= 4 else { continue }

            let typeGuid = quoted[0]
            let name = quoted[1]
            let csprojPath = quoted[2]
            let projectGuid = quoted[3]

            // Only root-level .csproj files (no path separators).
            guard csprojPath.hasSuffix(".csproj"), !csprojPath.contains("/") else {
                continue
            }

            results.append(SlnProjectEntry(
                typeGuid: typeGuid,
                name: name,
                csprojPath: csprojPath,
                projectGuid: projectGuid
            ))
        }
        return results
    }

    private func loadManifest(_ url: URL) throws -> GeneratorManifest {
        do {
            let data = try Data(contentsOf: url)
            return try decoder.decode(GeneratorManifest.self, from: data)
        } catch {
            throw GeneratorError.invalidManifest(url)
        }
    }

    private func loadUnityVersion(projectRoot: URL) throws -> String {
        let versionURL = projectRoot
            .appendingPathComponent("ProjectSettings")
            .appendingPathComponent("ProjectVersion.txt")

        let content = try String(contentsOf: versionURL, encoding: .utf8)
        guard
            let line = content.split(separator: "\n").first(where: { $0.hasPrefix("m_EditorVersion:") }),
            let version = line.split(separator: ":", maxSplits: 1, omittingEmptySubsequences: true).last?.trimmingCharacters(in: .whitespaces)
        else {
            throw GeneratorError.invalidProjectVersion(versionURL)
        }

        return version
    }

    private func loadMetaGuid(forAssetAt assetURL: URL) throws -> String? {
        let metaURL = URL(fileURLWithPath: assetURL.path + ".meta")
        guard fileManager.fileExists(atPath: metaURL.path) else {
            return nil
        }

        let content = try String(contentsOf: metaURL, encoding: .utf8)
        guard
            let line = content.split(separator: "\n").first(where: { $0.hasPrefix("guid:") }),
            let guid = line.split(separator: ":", maxSplits: 1, omittingEmptySubsequences: true).last?.trimmingCharacters(in: .whitespaces)
        else {
            return nil
        }

        return guid
    }

    private func resolveAssemblyRoots(
        asmDefByName: [String: AsmDefRecord],
        asmRefs: [AsmRefRecord],
        projectNames: Set<String>,
        guidToAssembly: [String: String]
    ) -> [String: String] {
        var assemblyRoots: [String: String] = [:]

        for (name, record) in asmDefByName where projectNames.contains(name) {
            assemblyRoots[record.directory] = name
        }

        for asmRef in asmRefs {
            guard let assemblyName = resolveAssemblyReference(
                asmRef.reference,
                asmDefByName: asmDefByName,
                guidToAssembly: guidToAssembly
            ) else {
                continue
            }

            guard projectNames.contains(assemblyName) else {
                continue
            }

            // Prefer asmdef ownership if both exist in the same directory.
            if assemblyRoots[asmRef.directory] == nil {
                assemblyRoots[asmRef.directory] = assemblyName
            }
        }

        return assemblyRoots
    }

    private func resolveAssemblyReference(
        _ rawReference: String,
        asmDefByName: [String: AsmDefRecord],
        guidToAssembly: [String: String]
    ) -> String? {
        if asmDefByName[rawReference] != nil {
            return rawReference
        }

        if rawReference.starts(with: "GUID:") {
            let guid = String(rawReference.dropFirst("GUID:".count)).lowercased()
            return guidToAssembly[guid]
        }

        if rawReference.count == 32 {
            return guidToAssembly[rawReference.lowercased()]
        }

        return nil
    }

    private func assignSources(
        sourceFiles: [String],
        assemblyRoots: [String: String],
        knownProjects: Set<String>
    ) -> SourceAssignments {
        var filesByProject: [String: [String]] = Dictionary(
            uniqueKeysWithValues: knownProjects.map { ($0, []) }
        )
        var unresolvedFiles: [String] = []

        var directoryCache: [String: DirectoryResolution] = [:]

        for file in sourceFiles {
            let directory = parentDirectory(of: file)
            if let owner = nearestAssemblyOwner(
                directory: directory,
                assemblyRoots: assemblyRoots,
                cache: &directoryCache
            ) {
                filesByProject[owner, default: []].append(file)
                continue
            }

            if let fallbackProject = resolveLegacyProject(for: file), knownProjects.contains(fallbackProject) {
                filesByProject[fallbackProject, default: []].append(file)
                continue
            }

            unresolvedFiles.append(file)
        }

        for key in filesByProject.keys {
            filesByProject[key]?.sort()
        }

        unresolvedFiles.sort()

        return SourceAssignments(filesByProject: filesByProject, unresolvedFiles: unresolvedFiles)
    }

    private func nearestAssemblyOwner(
        directory: String,
        assemblyRoots: [String: String],
        cache: inout [String: DirectoryResolution]
    ) -> String? {
        var current = directory
        var walked: [String] = []

        while true {
            if let cached = cache[current] {
                switch cached {
                case .assembly(let assembly):
                    for path in walked {
                        cache[path] = .assembly(assembly)
                    }
                    return assembly
                case .none:
                    for path in walked {
                        cache[path] = DirectoryResolution.none
                    }
                    return nil
                }
            }

            if let assembly = assemblyRoots[current] {
                cache[current] = .assembly(assembly)
                for path in walked {
                    cache[path] = .assembly(assembly)
                }
                return assembly
            }

            walked.append(current)

            if current.isEmpty {
                break
            }
            current = parentDirectory(of: current)
        }

        for path in walked {
            cache[path] = DirectoryResolution.none
        }
        return nil
    }

    private func resolveLegacyProject(for sourcePath: String) -> String? {
        guard sourcePath.hasPrefix("Assets/") else {
            return nil
        }

        let directory = parentDirectory(of: sourcePath)
        let directoryComponents = directory.split(separator: "/")
        let isEditor = directoryComponents.contains("Editor")

        let isFirstPass = sourcePath.hasPrefix("Assets/Plugins/")
            || sourcePath == "Assets/Plugins"
            || sourcePath.hasPrefix("Assets/Standard Assets/")
            || sourcePath == "Assets/Standard Assets"
            || sourcePath.hasPrefix("Assets/Pro Standard Assets/")
            || sourcePath == "Assets/Pro Standard Assets"

        if isEditor {
            return isFirstPass ? "Assembly-CSharp-Editor-firstpass" : "Assembly-CSharp-Editor"
        }

        return isFirstPass ? "Assembly-CSharp-firstpass" : "Assembly-CSharp"
    }

    private func makeCompilePatterns(
        for project: ProjectEntry,
        files: [String],
        assemblyRoots: [String: String],
        ignoredDirectories: [String]
    ) -> [CompilePattern] {
        switch project.kind {
        case .legacy:
            return makeLegacyCompilePatterns(files: files)
        case .asmdef:
            let ownRoots = assemblyRoots
                .compactMap { $0.value == project.name ? $0.key : nil }
                .sorted()

            guard !ownRoots.isEmpty else {
                // Fall back to directory-based legacy-style includes if no assemblyRoots exist.
                return makeLegacyCompilePatterns(files: files)
            }

            let topOwnRoots = topLevelRoots(from: ownRoots)
            let foreignRoots = assemblyRoots
                .compactMap { $0.value == project.name ? nil : $0.key }
                .sorted()

            let patterns = topOwnRoots.map { root -> CompilePattern in
                let includePattern = recursiveCsPattern(forDirectory: root)

                var excludes = foreignRoots
                    .filter { isDescendantOrSame($0, of: root) }
                    .map(recursiveCsPattern(forDirectory:))

                // Exclude directories ending with ~ or starting with .
                for dir in ignoredDirectories where isDescendantOrSame(dir, of: root) {
                    excludes.append(recursiveCsPattern(forDirectory: dir))
                }

                return CompilePattern(
                    include: includePattern,
                    exclude: deduplicatePreservingOrder(excludes)
                )
            }

            return patterns.sorted { $0.include < $1.include }
        }
    }

    private func makeLegacyCompilePatterns(files: [String]) -> [CompilePattern] {
        let directories = Set(files.map(parentDirectory(of:)))
        let patterns = directories.map { directory -> CompilePattern in
            if directory.isEmpty {
                return CompilePattern(include: "*.cs", exclude: [])
            }
            return CompilePattern(include: "\(directory)/*.cs", exclude: [])
        }
        return patterns.sorted { $0.include < $1.include }
    }

    private func topLevelRoots(from assemblyRoots: [String]) -> [String] {
        let sortedByDepth = assemblyRoots.sorted {
            let lhsDepth = pathDepth($0)
            let rhsDepth = pathDepth($1)
            if lhsDepth != rhsDepth {
                return lhsDepth < rhsDepth
            }
            return $0 < $1
        }

        var roots: [String] = []
        roots.reserveCapacity(sortedByDepth.count)

        for root in sortedByDepth {
            if roots.contains(where: { isDescendantOrSame(root, of: $0) }) {
                continue
            }
            roots.append(root)
        }

        return roots.sorted()
    }

    private func recursiveCsPattern(forDirectory directory: String) -> String {
        if directory.isEmpty {
            return "**/*.cs"
        }
        return "\(directory)/**/*.cs"
    }

    private func renderCompilePatterns(_ patterns: [CompilePattern]) -> String {
        patterns
            .map { pattern in
                let include = xmlEscape(pattern.include)
                if pattern.exclude.isEmpty {
                    return "    <Compile Include=\"\(include)\" />"
                }

                let exclude = xmlEscape(pattern.exclude.joined(separator: ";"))
                return "    <Compile Include=\"\(include)\" Exclude=\"\(exclude)\" />"
            }
            .joined(separator: "\n")
    }

    private func renderProjectReferences(
        for project: ProjectEntry,
        asmDefByName: [String: AsmDefRecord],
        guidToAssembly: [String: String],
        projectByName: [String: ProjectEntry]
    ) throws -> String {
        let orderedReferences: [String]

        switch project.kind {
        case .legacy:
            orderedReferences = []
        case .asmdef:
            guard let asmDef = asmDefByName[project.name] else {
                throw GeneratorError.missingAsmDefForProject(project.name)
            }

            var refs: [String] = []
            refs.reserveCapacity(asmDef.references.count)
            for rawReference in asmDef.references {
                guard let resolved = resolveAssemblyReference(
                    rawReference,
                    asmDefByName: asmDefByName,
                    guidToAssembly: guidToAssembly
                ) else {
                    continue
                }
                if projectByName[resolved] == nil {
                    continue
                }
                refs.append(resolved)
            }
            orderedReferences = deduplicatePreservingOrder(refs)
        }

        var blocks: [String] = []
        blocks.reserveCapacity(orderedReferences.count)

        for referenceName in orderedReferences {
            guard let referenceProject = projectByName[referenceName] else {
                throw GeneratorError.failedToResolveProjectReference(project: project.name, reference: referenceName)
            }

            let block = [
                "    <ProjectReference Include=\"\(xmlEscape(referenceProject.csprojPath))\">",
                "      <Project>\(referenceProject.guid)</Project>",
                "      <Name>\(xmlEscape(referenceProject.name))</Name>",
                "    </ProjectReference>",
            ].joined(separator: "\n")

            blocks.append(block)
        }

        return blocks.joined(separator: "\n")
    }

    private func renderSolutionProjectEntries(manifest: GeneratorManifest) -> String {
        manifest.projects
            .map { project in
                [
                    "Project(\"\(manifest.projectTypeGuid)\") = \"\(project.name)\", \"\(project.csprojPath)\", \"\(project.guid)\"",
                    "EndProject",
                ].joined(separator: "\n")
            }
            .joined(separator: "\n")
    }

    private func renderSolutionProjectConfigs(manifest: GeneratorManifest) -> String {
        manifest.projects
            .map { project in
                [
                    "\t\t\(project.guid).Debug|Any CPU.ActiveCfg = Debug|Any CPU",
                    "\t\t\(project.guid).Debug|Any CPU.Build.0 = Debug|Any CPU",
                ].joined(separator: "\n")
            }
            .joined(separator: "\n")
    }

    private func renderTemplate(_ template: String, replacements: [String: String]) -> String {
        replacements.reduce(template) { partial, item in
            partial.replacingOccurrences(of: item.key, with: item.value)
        }
    }

    @discardableResult
    private func writeIfChanged(content: String, to url: URL) throws -> Bool {
        if let current = try? String(contentsOf: url, encoding: .utf8), current == content {
            return false
        }

        try content.write(to: url, atomically: true, encoding: .utf8)
        return true
    }

    private func scanProjectFiles(projectRoot: URL, roots: [String]) throws -> ProjectFileScan {
        var csFiles: [String] = []
        var asmDefPaths: [String] = []
        var asmRefPaths: [String] = []
        var ignoredDirectories: [String] = []

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
                    ignoredDirectories.append(String(path.dropFirst(prefixLen)))
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
        ignoredDirectories.sort()
        return ProjectFileScan(csFiles: csFiles, asmDefPaths: asmDefPaths, asmRefPaths: asmRefPaths, ignoredDirectories: ignoredDirectories)
    }

    private func loadAsmDefsFromPaths(_ paths: [String], projectRoot: URL) throws -> [AsmDefRecord] {
        try paths.compactMap { path in
            let url = projectRoot.appendingPathComponent(path)
            let data = try Data(contentsOf: url)
            let raw = try decoder.decode(RawAsmDef.self, from: data)
            let metaGuid = try loadMetaGuid(forAssetAt: url)
            return AsmDefRecord(
                name: raw.name,
                directory: parentDirectory(of: path),
                guid: metaGuid,
                references: raw.references ?? []
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

    private func buildGuidToAssemblyMap(_ asmDefByName: [String: AsmDefRecord]) -> [String: String] {
        var map: [String: String] = [:]
        for (name, record) in asmDefByName {
            guard let guid = record.guid else { continue }
            map[guid.lowercased()] = name
        }
        return map
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
