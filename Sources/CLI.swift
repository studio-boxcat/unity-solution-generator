import ArgumentParser
import Foundation

@main
struct CLI: ParsableCommand {
    static let configuration = CommandConfiguration(
        commandName: "unity-solution-generator",
        abstract: "Regenerate .csproj/.sln from asmdef/asmref layout.",
        subcommands: [Generate.self, ExtractTemplates.self],
        defaultSubcommand: Generate.self
    )
}

struct Generate: ParsableCommand {
    static let configuration = CommandConfiguration(
        abstract: "Generate .csproj files from current layout and templates."
    )

    @Option(name: .shortAndLong, help: "Project root path.")
    var projectRoot: String?

    @Option(name: .long, help: "Generator root directory relative to project root.")
    var generatorRoot: String = "Library/UnitySolutionGenerator"

    @Flag(name: .shortAndLong, help: "Print unresolved source directory samples.")
    var verbose = false

    @Flag(name: .long, help: "Target iOS platform.")
    var ios = false

    @Flag(name: .long, help: "Target Android platform.")
    var android = false

    @Flag(name: .long, help: "Editor configuration (all projects, keeps UNITY_EDITOR, always debug).")
    var editor = false

    @Flag(name: [.customShort("d"), .long], help: "Dev configuration (keeps DEBUG/TRACE defines).")
    var debug = false

    mutating func validate() throws {
        guard !(ios && android) else {
            throw ValidationError("Specify only one of --ios or --android.")
        }
        guard !(editor && debug) else {
            throw ValidationError("--editor and --debug are mutually exclusive.")
        }
    }

    func run() throws {
        let root = resolveProjectRoot(projectRoot)
        let platform: BuildPlatform? = ios ? .ios : android ? .android : nil
        let buildConfig: BuildConfig = editor ? .editor : debug ? .dev : .prod
        let options = GenerateOptions(
            projectRoot: root,
            generatorRoot: generatorRoot,
            verbose: verbose,
            platform: platform,
            buildConfig: buildConfig
        )
        let generator = SolutionGenerator()
        let result = try generator.generate(options: options)

        if platform == nil {
            print("Source mapping summary:")
            for (project, count) in result.stats.patternCountByProject.sorted(by: { $0.key < $1.key }) {
                print("  - \(project): \(count) patterns")
            }

            if result.stats.unresolvedDirCount > 0 {
                print("Unresolved directories: \(result.stats.unresolvedDirCount)")
            }
        } else if let slnPath = result.variantSlnPath {
            print(slnPath)
        }

        for warning in result.warnings {
            FileHandle.standardError.write(Data("warning: \(warning)\n".utf8))
        }
    }
}

struct ExtractTemplates: ParsableCommand {
    static let configuration = CommandConfiguration(
        commandName: "extract-templates",
        abstract: "Extract templates from Unity-generated csproj/sln."
    )

    @Option(name: .shortAndLong, help: "Project root path.")
    var projectRoot: String?

    @Option(name: .long, help: "Generator root directory relative to project root.")
    var generatorRoot: String = "Library/UnitySolutionGenerator"

    func run() throws {
        let root = resolveProjectRoot(projectRoot)
        let options = ExtractTemplatesOptions(projectRoot: root, generatorRoot: generatorRoot)
        let updated = try TemplateExtractor.extract(options: options)

        if !updated.isEmpty {
            print("Extracted \(updated.count) template(s):")
            for file in updated {
                print("  - \(file)")
            }
        } else {
            print("No changes.")
        }
    }
}

private func resolveProjectRoot(_ path: String?) -> String {
    if let path {
        return URL(fileURLWithPath: path, relativeTo: URL(fileURLWithPath: FileManager.default.currentDirectoryPath)).standardizedFileURL.path
    }
    return FileManager.default.currentDirectoryPath
}
