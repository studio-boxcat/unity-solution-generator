import ArgumentParser
import Foundation

@main
struct CLI: ParsableCommand {
    static let configuration = CommandConfiguration(
        commandName: "unity-solution-generator",
        abstract: "Regenerate .csproj and .sln files from asmdef/asmref layout.",
        subcommands: [Generate.self, InitManifest.self, RefreshTemplates.self, PrepareBuild.self],
        defaultSubcommand: Generate.self
    )
}

struct Generate: ParsableCommand {
    static let configuration = CommandConfiguration(
        abstract: "Generate csproj/sln from current layout and manifest."
    )

    @Option(name: .long, help: "Project root path.")
    var projectRoot: String?

    @Option(name: .long, help: "Manifest file path relative to project root.")
    var manifest: String = "Library/UnitySolutionGenerator/projects.json"

    @Flag(name: .shortAndLong, help: "Print unresolved source file samples.")
    var verbose = false

    func run() throws {
        let root = resolveProjectRoot(projectRoot)
        let options = GenerateOptions(projectRoot: root, manifestPath: manifest, verbose: verbose)
        let generator = SolutionGenerator()
        let result = try generator.generate(options: options)

        if !result.updatedFiles.isEmpty {
            print("Updated \(result.updatedFiles.count) file(s):")
            for file in result.updatedFiles {
                print("  - \(file)")
            }
        } else {
            print("No changes.")
        }

        print("Source mapping summary:")
        for (project, count) in result.stats.sourceCountByProject.sorted(by: { $0.key < $1.key }) {
            let patternCount = result.stats.directoryPatternCountByProject[project] ?? 0
            print("  - \(project): \(count) files, \(patternCount) patterns")
        }

        if result.stats.unresolvedSourceCount > 0 {
            print("Unresolved files: \(result.stats.unresolvedSourceCount)")
        }

        for warning in result.warnings {
            FileHandle.standardError.write(Data("warning: \(warning)\n".utf8))
        }
    }
}

struct InitManifest: ParsableCommand {
    static let configuration = CommandConfiguration(
        commandName: "init-manifest",
        abstract: "Generate projects.json from .sln and asmdef scan."
    )

    @Option(name: .long, help: "Project root path.")
    var projectRoot: String?

    @Option(name: .long, help: "Output manifest path relative to project root.")
    var manifest: String = "Library/UnitySolutionGenerator/projects.json"

    @Option(name: .long, help: "Template root directory relative to project root.")
    var templateRoot: String = "Library/UnitySolutionGenerator"

    func run() throws {
        let root = resolveProjectRoot(projectRoot)
        let options = InitManifestOptions(projectRoot: root, manifestPath: manifest, templateRoot: templateRoot)
        let generator = SolutionGenerator()
        let manifestURL = try generator.initManifest(options: options)
        print("Generated: \(manifestURL.path)")
    }
}

struct RefreshTemplates: ParsableCommand {
    static let configuration = CommandConfiguration(
        commandName: "refresh-templates",
        abstract: "Extract templates from current Unity-generated csproj/sln."
    )

    @Option(name: .long, help: "Project root path.")
    var projectRoot: String?

    @Option(name: .long, help: "Manifest file path relative to project root.")
    var manifest: String = "Library/UnitySolutionGenerator/projects.json"

    func run() throws {
        let root = resolveProjectRoot(projectRoot)
        let options = RefreshTemplatesOptions(projectRoot: root, manifestPath: manifest)
        let generator = SolutionGenerator()
        let updated = try generator.refreshTemplates(options: options)

        if !updated.isEmpty {
            print("Refreshed \(updated.count) template(s):")
            for file in updated {
                print("  - \(file)")
            }
        } else {
            print("No changes.")
        }
    }
}

struct PrepareBuild: ParsableCommand {
    static let configuration = CommandConfiguration(
        commandName: "prepare-build",
        abstract: "Generate platform-variant csproj copies for device build validation."
    )

    @Option(name: .long, help: "Project root path.")
    var projectRoot: String?

    @Option(name: .long, help: "Manifest file path relative to project root.")
    var manifest: String

    @Flag(name: .long, help: "Target iOS platform.")
    var ios = false

    @Flag(name: .long, help: "Target Android platform.")
    var android = false

    @Flag(name: [.customShort("d"), .long], help: "Keep DEBUG/TRACE defines.")
    var debug = false

    mutating func validate() throws {
        guard ios || android else {
            throw ValidationError("Specify --ios or --android.")
        }
        guard !(ios && android) else {
            throw ValidationError("Specify only one of --ios or --android.")
        }
    }

    func run() throws {
        let root = resolveProjectRoot(projectRoot)
        let platform: BuildPlatform = ios ? .ios : .android
        let options = PrepareBuildOptions(
            projectRoot: root,
            manifestPath: manifest,
            platform: platform,
            debugBuild: debug
        )

        let generator = SolutionGenerator()
        let result = try generator.prepareBuild(options: options)

        if !result.skippedCsprojs.isEmpty {
            FileHandle.standardError.write(
                Data("Skipped \(result.skippedCsprojs.count) up-to-date csproj(s)\n".utf8)
            )
        }

        // Output csproj paths to stdout for piping to parallel/xargs
        for path in result.generatedCsprojs + result.skippedCsprojs {
            print(path)
        }
    }
}

private func resolveProjectRoot(_ path: String?) -> URL {
    if let path {
        return URL(fileURLWithPath: path, relativeTo: URL(fileURLWithPath: FileManager.default.currentDirectoryPath)).standardizedFileURL
    }
    return URL(fileURLWithPath: FileManager.default.currentDirectoryPath)
}
