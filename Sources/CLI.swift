import ArgumentParser
import Foundation

@main
struct CLI: ParsableCommand {
    static let configuration = CommandConfiguration(
        commandName: "unity-solution-generator",
        abstract: "Regenerate .csproj and .sln files from asmdef/asmref layout.",
        subcommands: [Generate.self, ExtractTemplates.self],
        defaultSubcommand: Generate.self
    )
}

struct Generate: ParsableCommand {
    static let configuration = CommandConfiguration(
        abstract: "Generate csproj/sln from current layout and templates."
    )

    @Option(name: .shortAndLong, help: "Project root path.")
    var projectRoot: String?

    @Option(name: .long, help: "Template root directory relative to project root.")
    var templateRoot: String = "Library/UnitySolutionGenerator"

    @Flag(name: .shortAndLong, help: "Print unresolved source directory samples.")
    var verbose = false

    @Flag(name: .long, help: "Generate iOS platform variant csprojs.")
    var ios = false

    @Flag(name: .long, help: "Generate Android platform variant csprojs.")
    var android = false

    @Flag(name: [.customShort("d"), .long], help: "Keep DEBUG/TRACE defines in platform variants.")
    var debug = false

    mutating func validate() throws {
        guard !(ios && android) else {
            throw ValidationError("Specify only one of --ios or --android.")
        }
    }

    func run() throws {
        let root = resolveProjectRoot(projectRoot)
        let platform: BuildPlatform? = ios ? .ios : android ? .android : nil
        let options = GenerateOptions(
            projectRoot: root,
            templateRoot: templateRoot,
            verbose: verbose,
            platform: platform,
            debugBuild: debug
        )
        let generator = SolutionGenerator()
        let result = try generator.generate(options: options)

        let hasPlatform = platform != nil

        if !hasPlatform {
            if !result.updatedFiles.isEmpty {
                print("Updated \(result.updatedFiles.count) file(s):")
                for file in result.updatedFiles {
                    print("  - \(file)")
                }
            } else {
                print("No changes.")
            }

            print("Source mapping summary:")
            for (project, count) in result.stats.patternCountByProject.sorted(by: { $0.key < $1.key }) {
                print("  - \(project): \(count) patterns")
            }

            if result.stats.unresolvedDirCount > 0 {
                print("Unresolved directories: \(result.stats.unresolvedDirCount)")
            }
        }

        if hasPlatform {
            if !result.skippedCsprojs.isEmpty {
                FileHandle.standardError.write(
                    Data("Skipped \(result.skippedCsprojs.count) up-to-date csproj(s)\n".utf8)
                )
            }

            // Output csproj paths to stdout for piping to parallel/xargs.
            for path in result.platformCsprojs + result.skippedCsprojs {
                print(path)
            }
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

    @Option(name: .long, help: "Template root directory relative to project root.")
    var templateRoot: String = "Library/UnitySolutionGenerator"

    func run() throws {
        let root = resolveProjectRoot(projectRoot)
        let options = ExtractTemplatesOptions(projectRoot: root, templateRoot: templateRoot)
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

private func resolveProjectRoot(_ path: String?) -> URL {
    if let path {
        return URL(fileURLWithPath: path, relativeTo: URL(fileURLWithPath: FileManager.default.currentDirectoryPath)).standardizedFileURL
    }
    return URL(fileURLWithPath: FileManager.default.currentDirectoryPath)
}
