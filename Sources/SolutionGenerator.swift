import Darwin
import Dispatch

struct ProjectInfo: Sendable {
    let name: String
    let guid: String
    var csprojPath: String { "\(name).csproj" }
}

struct GenerateOptions: Sendable {
    let projectRoot: String
    let generatorRoot: String
    let verbose: Bool
    let platform: BuildPlatform
    let buildConfig: BuildConfig

    init(projectRoot: String, generatorRoot: String = defaultGeneratorRoot, verbose: Bool = false, platform: BuildPlatform, buildConfig: BuildConfig = .prod) {
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

enum BuildConfig: String, Sendable {
    case editor
    case dev
    case prod
}

struct GenerateResult: Sendable {
    let warnings: [String]
    let variantCsprojs: [String]
    let variantSlnPath: String
}

enum GeneratorError: Error, CustomStringConvertible {
    case missingTemplate(String)
    case noSolutionFound(String)
    case noProjectsInSolution(String)
    case duplicateAsmDefName(String)
    case noTemplatesFound(String)

    var description: String {
        switch self {
        case .missingTemplate(let path):
            return "Missing template file: \(path)"
        case .noSolutionFound(let path):
            return "No .sln file found in: \(path)"
        case .noProjectsInSolution(let path):
            return "No C# projects found in solution: \(path)"
        case .duplicateAsmDefName(let name):
            return "Duplicate asmdef name: '\(name)'"
        case .noTemplatesFound(let path):
            return "No templates found in: \(path)\nRun 'unity-solution-generator init <unity-root>' first."
        }
    }
}

struct SolutionGenerator {

    func generate(options: GenerateOptions) throws -> GenerateResult {
        let projectRoot = resolveRealPath(options.projectRoot)
        let generatorRoot = options.generatorRoot
        let generatorDir = joinPath(projectRoot, generatorRoot)
        let templatesDir = joinPath(generatorDir, "templates")
        let platform = options.platform

        let scan = try ProjectScanner.scan(projectRoot: projectRoot)
        let projects = discoverProjects(templatesDir: templatesDir)
        guard !projects.isEmpty else {
            throw GeneratorError.noTemplatesFound(templatesDir)
        }
        let projectByName = Dictionary(uniqueKeysWithValues: projects.map { ($0.name, $0) })

        var warnings: [String] = []
        if !scan.unresolvedDirs.isEmpty {
            warnings.append("Unresolved source directories: \(scan.unresolvedDirs.count)")
        }
        if options.verbose {
            warnings += scan.unresolvedDirs.prefix(20).map { "Unresolved: \($0)/" }
        }

        let variantPrefix = String(repeating: "../", count: generatorRoot.split(separator: "/").count + 1)

        var patternsByProject: [String: [String]] = [:]
        for project in projects {
            let dirs = scan.dirsByProject[project.name] ?? []
            patternsByProject[project.name] = dirs.sorted().map {
                $0.isEmpty ? "\(variantPrefix)*.cs" : "\(variantPrefix)\($0)/*.cs"
            }
        }

        // Determine included projects.
        let isEditor = options.buildConfig == .editor
        let targetPlatformName = platform == .ios ? "iOS" : "Android"

        var includedProjects: [ProjectInfo] = []
        var nonRuntimeNames: Set<String> = []

        if isEditor {
            includedProjects = projects
        } else {
            for project in projects {
                let category: ProjectCategory
                let matchesPlatform: Bool
                if let asmDef = scan.asmDefByName[project.name] {
                    category = asmDef.category
                    let platforms = asmDef.includePlatforms.filter { $0 != "Editor" }
                    matchesPlatform = platforms.isEmpty || platforms.contains(targetPlatformName)
                } else {
                    category = .runtime
                    matchesPlatform = true
                }
                if category == .runtime && matchesPlatform {
                    includedProjects.append(project)
                } else {
                    nonRuntimeNames.insert(project.name)
                }
            }
        }

        let config = "\(platform.rawValue)-\(options.buildConfig.rawValue)"
        let variantDir = joinPath(generatorDir, config)
        createDirectoryRecursive(variantDir)

        // Write Directory.Build.props.
        try writeFileIfChanged(
            joinPath(variantDir, "Directory.Build.props"),
            Self.renderDirectoryBuildProps(projectRoot: projectRoot, platform: platform, buildConfig: options.buildConfig)
        )

        // Read templates.
        var templates: [String] = []
        for project in includedProjects {
            let templatePath = joinPath(templatesDir, "\(project.name).csproj.template")
            guard fileExists(templatePath) else {
                throw GeneratorError.missingTemplate(templatePath)
            }
            templates.append(try readFile(templatePath))
        }

        // Render + write in parallel.
        let count = includedProjects.count
        let errorBuf = UnsafeMutablePointer<Error?>.allocate(capacity: count)
        errorBuf.initialize(repeating: nil, count: count)
        defer { errorBuf.deinitialize(count: count); errorBuf.deallocate() }
        let errors = SendablePtr(ptr: errorBuf)

        let projects_ = includedProjects
        let patterns_ = patternsByProject
        let templates_ = templates
        let excludeNames_ = nonRuntimeNames
        let asmDefByName_ = scan.asmDefByName

        DispatchQueue.concurrentPerform(iterations: count) { i in
            let project = projects_[i]
            let sourceBlock = Self.renderCompilePatterns(patterns_[project.name] ?? [])
            let referenceBlock = Self.renderProjectReferences(
                for: project,
                asmDefByName: asmDefByName_,
                projectByName: projectByName,
                excludeNames: excludeNames_
            )

            var rendered = templates_[i]
            rendered += "  <ItemGroup>\n"
            if !sourceBlock.isEmpty { rendered += sourceBlock + "\n" }
            if !referenceBlock.isEmpty { rendered += referenceBlock + "\n" }
            rendered += "  </ItemGroup>\n</Project>\n"

            do {
                try writeFileIfChanged(joinPath(variantDir, project.csprojPath), rendered)
            } catch {
                errors[i] = error
            }
        }

        for i in 0..<count {
            if let error = errorBuf[i] { throw error }
        }

        // Write .sln.
        let projectName = projectRoot.split(separator: "/").last.map(String.init) ?? "Project"
        let slnName = "\(projectName).sln"
        try writeFileIfChanged(
            joinPath(variantDir, slnName),
            renderSln(includedProjects)
        )

        return GenerateResult(
            warnings: warnings,
            variantCsprojs: includedProjects.map { "\(generatorRoot)/\(config)/\($0.csprojPath)" }.sorted(),
            variantSlnPath: "\(generatorRoot)/\(config)/\(slnName)"
        )
    }

    // MARK: - Project discovery

    private func discoverProjects(templatesDir: String) -> [ProjectInfo] {
        guard fileExists(templatesDir) else { return [] }

        return listDirectory(templatesDir)
            .filter { $0.hasSuffix(".csproj.template") }
            .map { filename in
                let name = String(filename.dropLast(".csproj.template".count))
                return ProjectInfo(name: name, guid: deterministicGuid(for: name))
            }
            .sorted { $0.name < $1.name }
    }

    // MARK: - Rendering

    private static func renderCompilePatterns(_ patterns: [String]) -> String {
        patterns
            .map { "    <Compile Include=\"\(xmlEscape($0))\" />" }
            .joined(separator: "\n")
    }

    private static func renderProjectReferences(
        for project: ProjectInfo,
        asmDefByName: [String: AsmDefRecord],
        projectByName: [String: ProjectInfo],
        excludeNames: Set<String> = []
    ) -> String {
        guard let asmDef = asmDefByName[project.name] else { return "" }

        var seen: Set<String> = []
        var blocks: [String] = []

        for reference in asmDef.references {
            guard !excludeNames.contains(reference),
                  let ref = projectByName[reference],
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

    private static func renderDirectoryBuildProps(
        projectRoot: String,
        platform: BuildPlatform,
        buildConfig: BuildConfig
    ) -> String {
        var defines: [String] = []
        switch platform {
        case .ios: defines.append(contentsOf: ["UNITY_IOS", "UNITY_IPHONE"])
        case .android: defines.append("UNITY_ANDROID")
        }
        if buildConfig == .editor {
            defines.append(contentsOf: ["UNITY_EDITOR", "UNITY_EDITOR_64", "UNITY_EDITOR_OSX"])
        }
        if buildConfig == .editor || buildConfig == .dev {
            defines.append(contentsOf: ["DEBUG", "TRACE"])
        }
        return "<Project>\n<PropertyGroup>\n<ProjectRoot>\(projectRoot)</ProjectRoot>\n<DefineConstants>\(defines.joined(separator: ";"))</DefineConstants>\n</PropertyGroup>\n</Project>\n"
    }
}

// MARK: - .sln rendering

private let csharpProjectTypeGuid = "{FAE04EC0-301F-11D3-BF4B-00C04F79EFBC}"

private func renderSln(_ projects: [ProjectInfo]) -> String {
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
