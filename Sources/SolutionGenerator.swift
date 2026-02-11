import Foundation

struct ProjectInfo: Sendable {
    let name: String
    let csprojPath: String
    let templatePath: String
    let guid: String
}

struct GenerateOptions: Sendable {
    let projectRoot: URL
    let generatorRoot: String
    let verbose: Bool
    let platform: BuildPlatform?
    let buildConfig: BuildConfig

    init(projectRoot: URL, generatorRoot: String = "Library/UnitySolutionGenerator", verbose: Bool = false, platform: BuildPlatform? = nil, buildConfig: BuildConfig = .prod) {
        self.projectRoot = projectRoot
        self.generatorRoot = generatorRoot
        self.verbose = verbose
        self.platform = platform
        self.buildConfig = buildConfig
    }
}

enum BuildPlatform: String, Sendable {
    case ios
    case android
}

/// Build configuration axis — orthogonal to platform.
///   - editor: all projects, keeps UNITY_EDITOR + DEBUG/TRACE
///   - dev:    runtime only, strips UNITY_EDITOR, keeps DEBUG/TRACE
///   - prod:   runtime only, strips UNITY_EDITOR + DEBUG/TRACE
enum BuildConfig: String, Sendable {
    case editor
    case dev
    case prod
}

struct GenerateResult: Sendable {
    let warnings: [String]
    let stats: GenerationStats
    let variantCsprojs: [String]
    /// .sln path in variant directory (for `dotnet build <sln> -m`).
    let variantSlnPath: String?
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

        }
    }
}

struct SlnRenderer {
    private static let csharpProjectTypeGuid = "{FAE04EC0-301F-11D3-BF4B-00C04F79EFBC}"

    static func render(projects: [ProjectInfo]) -> String {
        var lines: [String] = [
            "Microsoft Visual Studio Solution File, Format Version 11.00",
            "# Visual Studio 2010",
        ]

        for project in projects {
            lines.append("Project(\"\(csharpProjectTypeGuid)\") = \"\(project.name)\", \"\(project.csprojPath)\", \"\(project.guid)\"")
            lines.append("EndProject")
        }

        lines.append("Global")
        lines.append("\tGlobalSection(SolutionConfigurationPlatforms) = preSolution")
        lines.append("\t\tDebug|Any CPU = Debug|Any CPU")
        lines.append("\tEndGlobalSection")
        lines.append("\tGlobalSection(ProjectConfigurationPlatforms) = postSolution")

        for project in projects {
            lines.append("\t\t\(project.guid).Debug|Any CPU.ActiveCfg = Debug|Any CPU")
            lines.append("\t\t\(project.guid).Debug|Any CPU.Build.0 = Debug|Any CPU")
        }

        lines.append("\tEndGlobalSection")
        lines.append("EndGlobal")
        lines.append("")

        return lines.joined(separator: "\n")
    }
}

final class SolutionGenerator {
    private let fileManager: FileManager

    init(fileManager: FileManager = .default) {
        self.fileManager = fileManager
    }

    // MARK: - Generate

    func generate(options: GenerateOptions) throws -> GenerateResult {
        let projectRoot = options.projectRoot.standardizedFileURL
        let generatorRoot = options.generatorRoot
        let generatorDir = projectRoot.appendingPathComponent(generatorRoot)
        let templatesDir = generatorDir.appendingPathComponent("templates")

        // Scan Unity project layout.
        let scan = try ProjectScanner.scan(projectRoot: projectRoot)

        // Discover projects from templates directory.
        let projects = try discoverProjects(generatorRoot: generatorRoot, templatesDir: templatesDir)
        let projectByName = Dictionary(uniqueKeysWithValues: projects.map { ($0.name, $0) })

        var warnings: [String] = []
        if !scan.unresolvedDirs.isEmpty {
            warnings.append("Unresolved source directories: \(scan.unresolvedDirs.count)")
        }

        // Relative prefix to reach project root from variant subdirectory.
        // Variants sit one level below generatorDir, so depth + 1.
        // e.g. "Library/UnitySolutionGenerator" (depth 2) → "../../../"
        let variantPrefix = String(repeating: "../", count: generatorRoot.split(separator: "/").count + 1)

        var patternsByProject: [String: [String]] = [:]
        for project in projects {
            let dirs = scan.dirsByProject[project.name] ?? []
            patternsByProject[project.name] = dirs.sorted().map {
                $0.isEmpty ? "\(variantPrefix)*.cs" : "\(variantPrefix)\($0)/*.cs"
            }
        }

        // Render all csprojs in memory from templates.
        var renderedCsprojs: [(info: ProjectInfo, content: String)] = []

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

            renderedCsprojs.append((info: project, content: rendered))
        }

        let stats = GenerationStats(
            patternCountByProject: patternsByProject.mapValues(\.count),
            unresolvedDirCount: scan.unresolvedDirs.count
        )

        if options.verbose {
            warnings += scan.unresolvedDirs.prefix(20).map { "Unresolved: \($0)/" }
        }

        // Without a platform, just scan and report stats.
        guard let platform = options.platform else {
            return GenerateResult(warnings: warnings, stats: stats, variantCsprojs: [], variantSlnPath: nil)
        }

        // Write variant csprojs into {platform}-{config}/ subdirectory.
        let config = "\(platform.rawValue)-\(options.buildConfig.rawValue)"
        let variantDir = generatorDir.appendingPathComponent(config)
        try fileManager.createDirectory(at: variantDir, withIntermediateDirectories: true)

        // Filter and transform csprojs based on build configuration.
        let isEditor = options.buildConfig == .editor
        let targetPlatformName = platform == .ios ? "iOS" : "Android"

        var entriesToWrite: [(info: ProjectInfo, content: String)] = []
        var nonRuntimeNames: Set<String> = []

        if isEditor {
            // Editor: all projects, no filtering.
            entriesToWrite = renderedCsprojs
        } else {
            // Dev/Prod: runtime projects only, matching target platform.
            for entry in renderedCsprojs {
                let category: ProjectCategory
                let matchesPlatform: Bool
                if let asmDef = scan.asmDefByName[entry.info.name] {
                    category = asmDef.category
                    let platforms = asmDef.includePlatforms.filter { $0 != "Editor" }
                    matchesPlatform = platforms.isEmpty || platforms.contains(targetPlatformName)
                } else {
                    category = .runtime
                    matchesPlatform = true
                }

                if category == .runtime && matchesPlatform {
                    entriesToWrite.append(entry)
                } else {
                    nonRuntimeNames.insert(entry.info.name)
                }
            }
        }

        var variantCsprojs: [String] = []

        for entry in entriesToWrite {
            var content = entry.content

            if !isEditor {
                content = stripEditorDefines(content, debugBuild: options.buildConfig == .dev)
                content = stripNonRuntimeReferences(content, nonRuntimeNames: nonRuntimeNames)
            }
            content = swapPlatformDefines(content, platform: platform)

            let outputPath = "\(generatorRoot)/\(config)/\(entry.info.csprojPath)"
            let outputURL = variantDir.appendingPathComponent(entry.info.csprojPath)
            try writeIfChanged(content: content, to: outputURL)
            variantCsprojs.append(outputPath)
        }

        // Write .sln alongside variant csprojs.
        let slnName = "\(projectRoot.lastPathComponent).sln"
        let slnContent = SlnRenderer.render(projects: entriesToWrite.map(\.info))
        let slnURL = variantDir.appendingPathComponent(slnName)
        try writeIfChanged(content: slnContent, to: slnURL)
        let slnPath = "\(generatorRoot)/\(config)/\(slnName)"

        return GenerateResult(
            warnings: warnings,
            stats: stats,
            variantCsprojs: variantCsprojs.sorted(),
            variantSlnPath: slnPath
        )
    }

    // MARK: - Project discovery

    private func discoverProjects(
        generatorRoot: String,
        templatesDir: URL
    ) throws -> [ProjectInfo] {
        guard fileManager.fileExists(atPath: templatesDir.path) else {
            return []
        }

        let templateFiles = try fileManager.contentsOfDirectory(
            at: templatesDir,
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
                templatePath: "\(generatorRoot)/templates/\(filename)",
                guid: deterministicGuid(for: name)
            )
        }.sorted { $0.name < $1.name }
    }

    // MARK: - Build validation helpers

    func stripEditorDefines(_ content: String, debugBuild: Bool) -> String {
        var result = content
        // Order matters: strip suffixed variants before the base.
        result = result.replacingOccurrences(of: "UNITY_EDITOR_64;", with: "")
        result = result.replacingOccurrences(of: "UNITY_EDITOR_OSX;", with: "")
        result = result.replacingOccurrences(of: "UNITY_EDITOR;", with: "")
        if !debugBuild {
            result = result.replacingOccurrences(of: "DEBUG;", with: "")
            result = result.replacingOccurrences(of: "TRACE;", with: "")
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

}
