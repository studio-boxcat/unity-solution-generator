import Foundation

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
    let platform: BuildPlatform?
    let debugBuild: Bool

    init(projectRoot: URL, templateRoot: String = "Library/UnitySolutionGenerator", verbose: Bool = false, platform: BuildPlatform? = nil, debugBuild: Bool = false) {
        self.projectRoot = projectRoot
        self.templateRoot = templateRoot
        self.verbose = verbose
        self.platform = platform
        self.debugBuild = debugBuild
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

struct GenerateResult: Sendable {
    let updatedFiles: [String]
    let warnings: [String]
    let stats: GenerationStats
    let platformCsprojs: [String]
    let skippedCsprojs: [String]
}

struct GenerationStats: Sendable {
    let patternCountByProject: [String: Int]
    let unresolvedDirCount: Int
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

private let csharpProjectTypeGuid = "{FAE04EC0-301F-11D3-BF4B-00C04F79EFBC}"

final class SolutionGenerator {
    private let fileManager: FileManager

    init(fileManager: FileManager = .default) {
        self.fileManager = fileManager
    }

    // MARK: - Generate

    func generate(options: GenerateOptions) throws -> GenerateResult {
        let projectRoot = options.projectRoot.standardizedFileURL
        let templateRoot = options.templateRoot

        // Scan Unity project layout.
        let scan = try ProjectScanner.scan(projectRoot: projectRoot)

        // Discover projects from templates directory.
        let projects = try discoverProjects(templateRoot: templateRoot, projectRoot: projectRoot)
        let projectByName = Dictionary(uniqueKeysWithValues: projects.map { ($0.name, $0) })

        var warnings: [String] = []
        if !scan.unresolvedDirs.isEmpty {
            warnings.append("Unresolved source directories: \(scan.unresolvedDirs.count)")
        }

        var patternsByProject: [String: [String]] = [:]

        for project in projects {
            let dirs = scan.dirsByProject[project.name] ?? []
            patternsByProject[project.name] = dirs.sorted().map {
                $0.isEmpty ? "*.cs" : "\($0)/*.cs"
            }
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
                asmDefByName: scan.asmDefByName,
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
            patternCountByProject: patternsByProject.mapValues(\.count),
            unresolvedDirCount: scan.unresolvedDirs.count
        )

        if options.verbose {
            warnings += scan.unresolvedDirs.prefix(20).map { "Unresolved: \($0)/" }
        }

        // Generate platform variant csprojs when a target platform is specified.
        var platformCsprojs: [String] = []
        var skippedCsprojs: [String] = []

        if let platform = options.platform {
            let (generated, skipped) = try generatePlatformVariants(
                projectRoot: projectRoot,
                platform: platform,
                debugBuild: options.debugBuild,
                asmDefByName: scan.asmDefByName
            )
            platformCsprojs = generated
            skippedCsprojs = skipped
        }

        return GenerateResult(
            updatedFiles: updatedFiles.sorted(),
            warnings: warnings,
            stats: stats,
            platformCsprojs: platformCsprojs,
            skippedCsprojs: skippedCsprojs
        )
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

    // MARK: - Platform variant generation

    private func generatePlatformVariants(
        projectRoot: URL,
        platform: BuildPlatform,
        debugBuild: Bool,
        asmDefByName: [String: AsmDefRecord]
    ) throws -> (generated: [String], skipped: [String]) {
        let buildType = debugBuild ? "dev" : "prod"
        let suffix = ".v.\(platform.rawValue)-\(buildType)"

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

        let targetPlatformName = platform == .ios ? "iOS" : "Android"

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
            content = stripEditorDefines(content, debugBuild: debugBuild)
            content = swapPlatformDefines(content, platform: platform)
            content = stripNonRuntimeReferences(content, nonRuntimeNames: nonRuntimeNames)
            content = rewriteReferenceSuffix(content, suffix: suffix)
            try content.write(to: dstURL, atomically: true, encoding: .utf8)
            generated.append(dstName)
        }

        return (generated: generated.sorted(), skipped: skipped.sorted())
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
        let editorPattern = "UNITY_EDITOR(_64|_OSX)?;"
        if let regex = try? NSRegularExpression(pattern: editorPattern) {
            result = regex.stringByReplacingMatches(
                in: result, range: NSRange(result.startIndex..., in: result), withTemplate: ""
            )
        }
        if !debugBuild {
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

            if line.contains("<None Include=\"") {
                continue
            }

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
}
