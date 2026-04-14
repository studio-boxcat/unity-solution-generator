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

    var unityPlatformName: String {
        switch self {
        case .ios: return "iOS"
        case .android: return "Android"
        }
    }
}

enum BuildConfig: String, Sendable {
    case editor
    case dev
    case prod
}

/// Defines stripped from templates and injected per-variant via Directory.Build.props.
enum DynamicDefines {
    static let platform: [BuildPlatform: [String]] = [
        .ios: ["UNITY_IOS", "UNITY_IPHONE"],
        .android: ["UNITY_ANDROID"],
    ]
    static let editor = ["UNITY_EDITOR", "UNITY_EDITOR_64", "UNITY_EDITOR_OSX"]
    static let debug = ["DEBUG", "TRACE", "UNITY_ASSERTIONS"]

    static let all: Set<String> = {
        var s = Set<String>()
        for v in platform.values { s.formUnion(v) }
        s.formUnion(editor)
        s.formUnion(debug)
        return s
    }()
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
            return "No templates found in: \(path)\nRun 'unity-solution-generator lock <unity-root>' to generate a lockfile instead."
        }
    }
}

// MARK: - Shared scaffolding

/// Intermediate state shared between lockfile and template generation paths.
private struct GenerationContext {
    let projectRoot: String
    let generatorRoot: String
    let scan: ProjectScanner.Result
    let projectByName: [String: ProjectInfo]
    let patternsByProject: [String: [String]]
    let includedProjects: [ProjectInfo]
    let nonRuntimeNames: Set<String>
    let variantDir: String
    let config: String
    let warnings: [String]
}

private func buildContext(options: GenerateOptions, projectRoot: String, projects: [ProjectInfo], scan: ProjectScanner.Result? = nil) throws -> GenerationContext {
    let generatorRoot = options.generatorRoot
    let generatorDir = joinPath(projectRoot, generatorRoot)
    let platform = options.platform

    let scan = try scan ?? ProjectScanner.scan(projectRoot: projectRoot)

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

    let isEditor = options.buildConfig == .editor
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
                matchesPlatform = platforms.isEmpty || platforms.contains(platform.unityPlatformName)
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

    return GenerationContext(
        projectRoot: projectRoot,
        generatorRoot: generatorRoot,
        scan: scan,
        projectByName: projectByName,
        patternsByProject: patternsByProject,
        includedProjects: includedProjects,
        nonRuntimeNames: nonRuntimeNames,
        variantDir: variantDir,
        config: config,
        warnings: warnings
    )
}

/// Parallel csproj write + sln write, returns GenerateResult.
private func writeVariant(
    ctx: GenerationContext,
    renderCsproj: @Sendable (_ project: ProjectInfo, _ sourceBlock: String, _ referenceBlock: String) -> String
) throws -> GenerateResult {
    let count = ctx.includedProjects.count
    let errorBuf = UnsafeMutablePointer<Error?>.allocate(capacity: count)
    errorBuf.initialize(repeating: nil, count: count)
    defer { errorBuf.deinitialize(count: count); errorBuf.deallocate() }
    let errors = SendablePtr(ptr: errorBuf)

    let projects_ = ctx.includedProjects
    let patterns_ = ctx.patternsByProject
    let excludeNames_ = ctx.nonRuntimeNames
    let asmDefByName_ = ctx.scan.asmDefByName
    let projectByName_ = ctx.projectByName
    let variantDir_ = ctx.variantDir

    DispatchQueue.concurrentPerform(iterations: count) { i in
        let project = projects_[i]
        let sourceBlock = SolutionGenerator.renderCompilePatterns(patterns_[project.name] ?? [])
        let referenceBlock = SolutionGenerator.renderProjectReferences(
            for: project,
            asmDefByName: asmDefByName_,
            projectByName: projectByName_,
            excludeNames: excludeNames_
        )
        let rendered = renderCsproj(project, sourceBlock, referenceBlock)
        do {
            try writeFileIfChanged(joinPath(variantDir_, project.csprojPath), rendered)
        } catch {
            errors[i] = error
        }
    }

    for i in 0..<count {
        if let error = errorBuf[i] { throw error }
    }

    let projectName = ctx.projectRoot.split(separator: "/").last.map(String.init) ?? "Project"
    let slnName = "\(projectName).sln"
    try writeFileIfChanged(
        joinPath(ctx.variantDir, slnName),
        renderSln(ctx.includedProjects)
    )

    return GenerateResult(
        warnings: ctx.warnings,
        variantCsprojs: ctx.includedProjects.map { "\(ctx.generatorRoot)/\(ctx.config)/\($0.csprojPath)" }.sorted(),
        variantSlnPath: "\(ctx.generatorRoot)/\(ctx.config)/\(slnName)"
    )
}

// MARK: - SolutionGenerator

struct SolutionGenerator {

    // MARK: - Lockfile-based generation

    func generateFromLockfile(options: GenerateOptions, lockfile: Lockfile) throws -> GenerateResult {
        let projectRoot = resolveRealPath(options.projectRoot)
        let scan = try ProjectScanner.scan(projectRoot: projectRoot)

        // Discover projects from asmdef scan
        var projects: [ProjectInfo] = []
        var allNames: Set<String> = []
        for (name, _) in scan.asmDefByName {
            guard scan.dirsByProject[name] != nil else { continue }
            projects.append(ProjectInfo(name: name, guid: deterministicGuid(for: name)))
            allNames.insert(name)
        }
        for (name, _) in scan.dirsByProject where !allNames.contains(name) {
            projects.append(ProjectInfo(name: name, guid: deterministicGuid(for: name)))
        }
        projects.sort { $0.name < $1.name }

        let ctx = try buildContext(options: options, projectRoot: projectRoot, projects: projects, scan: scan)

        let staticDefines = lockfile.defines + lockfile.definesScripting
        try writeFileIfChanged(
            joinPath(ctx.variantDir, "Directory.Build.props"),
            Self.renderDirectoryBuildProps(
                projectRoot: ctx.projectRoot,
                unityPath: lockfile.unityPath,
                platform: options.platform,
                buildConfig: options.buildConfig,
                staticDefines: staticDefines
            )
        )

        let refs = Self.collectReferences(lockfile: lockfile, platform: options.platform, isEditor: options.buildConfig == .editor)
        let analyzerBlock = Self.renderAnalyzers(lockfile.analyzers)
        let langVersion = lockfile.langVersion
        let asmDefByName = ctx.scan.asmDefByName

        return try writeVariant(ctx: ctx) { project, sourceBlock, referenceBlock in
            let allowUnsafe = asmDefByName[project.name]?.allowUnsafeCode ?? false
            var rendered = Self.renderCsprojHeader(
                projectName: project.name,
                projectGuid: project.guid,
                langVersion: langVersion,
                allowUnsafeBlocks: allowUnsafe
            )
            rendered += analyzerBlock
            rendered += refs
            rendered += "  <ItemGroup>\n"
            if !sourceBlock.isEmpty { rendered += sourceBlock + "\n" }
            if !referenceBlock.isEmpty { rendered += referenceBlock + "\n" }
            rendered += "  </ItemGroup>\n"
            rendered += "  <Import Project=\"$(MSBuildToolsPath)\\Microsoft.CSharp.targets\" />\n"
            rendered += "</Project>\n"
            return rendered
        }
    }

    // MARK: - Template-based generation (legacy)

    func generate(options: GenerateOptions) throws -> GenerateResult {
        let projectRoot = resolveRealPath(options.projectRoot)
        let generatorRoot = options.generatorRoot
        let generatorDir = joinPath(projectRoot, generatorRoot)
        let templatesDir = joinPath(generatorDir, "templates")

        var projects = discoverProjects(templatesDir: templatesDir)
        if projects.isEmpty {
            fputs("No templates found, running init...\n", stderr)
            let updated = try TemplateExtractor.extract(
                options: ExtractTemplatesOptions(projectRoot: projectRoot, generatorRoot: generatorRoot)
            )
            for file in updated { fputs("  \(file)\n", stderr) }
            projects = discoverProjects(templatesDir: templatesDir)
            guard !projects.isEmpty else {
                throw GeneratorError.noTemplatesFound(templatesDir)
            }
        }

        let ctx = try buildContext(options: options, projectRoot: projectRoot, projects: projects)

        try writeFileIfChanged(
            joinPath(ctx.variantDir, "Directory.Build.props"),
            Self.renderDirectoryBuildProps(
                projectRoot: ctx.projectRoot,
                platform: options.platform,
                buildConfig: options.buildConfig
            )
        )

        var tmpTemplates: [String: String] = [:]
        for project in ctx.includedProjects {
            let templatePath = joinPath(templatesDir, "\(project.name).csproj.template")
            guard fileExists(templatePath) else {
                throw GeneratorError.missingTemplate(templatePath)
            }
            tmpTemplates[project.name] = try readFile(templatePath)
        }
        let templatesByName = tmpTemplates

        return try writeVariant(ctx: ctx) { project, sourceBlock, referenceBlock in
            var rendered = templatesByName[project.name] ?? ""
            rendered += "  <ItemGroup>\n"
            if !sourceBlock.isEmpty { rendered += sourceBlock + "\n" }
            if !referenceBlock.isEmpty { rendered += referenceBlock + "\n" }
            rendered += "  </ItemGroup>\n</Project>\n"
            return rendered
        }
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

    static func renderCompilePatterns(_ patterns: [String]) -> String {
        patterns
            .map { "    <Compile Include=\"\(xmlEscape($0))\" />" }
            .joined(separator: "\n")
    }

    static func renderProjectReferences(
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

    // MARK: - Lockfile csproj rendering

    private static func renderCsprojHeader(
        projectName: String,
        projectGuid: String,
        langVersion: String,
        allowUnsafeBlocks: Bool
    ) -> String {
        """
        <?xml version="1.0" encoding="utf-8"?>
        <Project ToolsVersion="4.0" DefaultTargets="Build" xmlns="http://schemas.microsoft.com/developer/msbuild/2003">
          <PropertyGroup>
            <LangVersion>\(langVersion)</LangVersion>
            <_TargetFrameworkDirectories>non_empty_path_generated_by_unity.rider.package</_TargetFrameworkDirectories>
            <_FullFrameworkReferenceAssemblyPaths>non_empty_path_generated_by_unity.rider.package</_FullFrameworkReferenceAssemblyPaths>
            <DisableHandlePackageFileConflicts>true</DisableHandlePackageFileConflicts>
          </PropertyGroup>
          <PropertyGroup>
            <Configuration Condition=" '$(Configuration)' == '' ">Debug</Configuration>
            <Platform Condition=" '$(Platform)' == '' ">AnyCPU</Platform>
            <ProductVersion>10.0.20506</ProductVersion>
            <SchemaVersion>2.0</SchemaVersion>
            <RootNamespace></RootNamespace>
            <ProjectGuid>\(projectGuid)</ProjectGuid>
            <ProjectTypeGuids>{E097FAD1-6243-4DAD-9C02-E9B9EFC3FFC1};{FAE04EC0-301F-11D3-BF4B-00C04F79EFBC}</ProjectTypeGuids>
            <OutputType>Library</OutputType>
            <AppDesignerFolder>Properties</AppDesignerFolder>
            <AssemblyName>\(projectName)</AssemblyName>
            <TargetFrameworkVersion>v4.7.1</TargetFrameworkVersion>
            <FileAlignment>512</FileAlignment>
            <BaseDirectory>.</BaseDirectory>
          </PropertyGroup>
          <PropertyGroup Condition=" '$(Configuration)|$(Platform)' == 'Debug|AnyCPU' ">
            <DebugSymbols>true</DebugSymbols>
            <DebugType>full</DebugType>
            <Optimize>false</Optimize>
            <OutputPath>Temp\\Bin\\Debug\\\(projectName)\\</OutputPath>
            <DefineConstants>$(DefineConstants)</DefineConstants>
            <ErrorReport>prompt</ErrorReport>
            <WarningLevel>4</WarningLevel>
            <NoWarn>0169,0649</NoWarn>
            <AllowUnsafeBlocks>\(allowUnsafeBlocks ? "True" : "False")</AllowUnsafeBlocks>
            <TreatWarningsAsErrors>False</TreatWarningsAsErrors>
          </PropertyGroup>
          <PropertyGroup>
            <NoConfig>true</NoConfig>
            <NoStdLib>true</NoStdLib>
            <AddAdditionalExplicitAssemblyReferences>false</AddAdditionalExplicitAssemblyReferences>
            <ImplicitlyExpandNETStandardFacades>false</ImplicitlyExpandNETStandardFacades>
            <ImplicitlyExpandDesignTimeFacades>false</ImplicitlyExpandDesignTimeFacades>
          </PropertyGroup>\n
        """
    }

    private static func renderAnalyzers(_ analyzers: [String]) -> String {
        guard !analyzers.isEmpty else { return "" }
        var s = "  <ItemGroup>\n"
        for path in analyzers {
            s += "    <Analyzer Include=\"\(xmlEscape(path))\" />\n"
        }
        s += "  </ItemGroup>\n"
        return s
    }

    private static func collectReferences(lockfile: Lockfile, platform: BuildPlatform, isEditor: Bool) -> String {
        var refs: [DllRef] = []
        var seen: Set<String> = []

        func add(_ ref: DllRef) {
            if seen.insert(ref.name).inserted { refs.append(ref) }
        }

        for ref in lockfile.refsEngine { add(ref) }
        if isEditor {
            for ref in lockfile.refsEditor { add(ref) }
        }
        for ref in lockfile.refsPlaybackStandalone { add(ref) }
        switch platform {
        case .ios: for ref in lockfile.refsPlaybackIos { add(ref) }
        case .android: for ref in lockfile.refsPlaybackAndroid { add(ref) }
        }
        for ref in lockfile.refsProject { add(ref) }
        for ref in lockfile.refsNetstandard { add(ref) }

        guard !refs.isEmpty else { return "" }
        var s = "  <ItemGroup>\n"
        for ref in refs {
            s += "    <Reference Include=\"\(xmlEscape(ref.name))\">\n"
            s += "      <HintPath>\(xmlEscape(ref.path))</HintPath>\n"
            s += "    </Reference>\n"
        }
        s += "  </ItemGroup>\n"
        return s
    }

    // MARK: - Directory.Build.props (unified)

    static func renderDirectoryBuildProps(
        projectRoot: String,
        unityPath: String? = nil,
        platform: BuildPlatform,
        buildConfig: BuildConfig,
        staticDefines: [String] = []
    ) -> String {
        var dynamicDefines = DynamicDefines.platform[platform] ?? []
        if buildConfig == .editor {
            dynamicDefines.append(contentsOf: DynamicDefines.editor)
        }
        if buildConfig == .editor || buildConfig == .dev {
            dynamicDefines.append(contentsOf: DynamicDefines.debug)
        }
        let allDefines = staticDefines + dynamicDefines
        var props = "<Project>\n<PropertyGroup>\n<ProjectRoot>\(projectRoot)</ProjectRoot>\n"
        if let unityPath {
            props += "<UnityPath>\(unityPath)</UnityPath>\n"
        }
        props += "<DefineConstants>$(DefineConstants);\(allDefines.joined(separator: ";"))</DefineConstants>\n</PropertyGroup>\n</Project>\n"
        return props
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
