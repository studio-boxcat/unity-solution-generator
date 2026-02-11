import Darwin

@main
struct CLI {
    static func main() {
        var args = Array(CommandLine.arguments.dropFirst())

        if args.isEmpty || args.contains("--help") || args.contains("-h") {
            printUsage()
            return
        }

        switch args.first {
        case "init":
            args.removeFirst()
            runInit(args)
        case "generate":
            args.removeFirst()
            runGenerate(args)
        default:
            die("Unknown command '\(args.first!)'. Use 'init' or 'generate'.")
        }
    }

    static func runInit(_ args: [String]) {
        guard !args.isEmpty else {
            die("init requires: <unity-root> [generator-root]")
        }

        let projectRoot = args[0]
        let generatorRoot = args.count > 1 ? args[1] : defaultGeneratorRoot

        do {
            let updated = try TemplateExtractor.extract(
                options: ExtractTemplatesOptions(projectRoot: resolveRealPath(projectRoot), generatorRoot: generatorRoot)
            )
            if updated.isEmpty {
                print("No changes.")
            } else {
                print("Extracted \(updated.count) template(s):")
                for file in updated { print("  - \(file)") }
            }
        } catch {
            die("\(error)")
        }
    }

    static func runGenerate(_ args: [String]) {
        guard args.count >= 3 else {
            die("generate requires: <unity-root> <platform> <config> [generator-root] [options]")
        }

        let projectRoot = args[0]

        let platform: BuildPlatform
        switch args[1] {
        case "ios": platform = .ios
        case "android": platform = .android
        default: die("Unknown platform '\(args[1])'. Use 'ios' or 'android'.")
        }

        let buildConfig: BuildConfig
        switch args[2] {
        case "prod": buildConfig = .prod
        case "dev": buildConfig = .dev
        case "editor": buildConfig = .editor
        default: die("Unknown config '\(args[2])'. Use 'prod', 'dev', or 'editor'.")
        }

        var generatorRoot = defaultGeneratorRoot
        var verbose = false
        var i = 3
        if i < args.count && !args[i].hasPrefix("-") {
            generatorRoot = args[i]
            i += 1
        }
        while i < args.count {
            switch args[i] {
            case "-v", "--verbose": verbose = true
            default: die("Unknown option: \(args[i])")
            }
            i += 1
        }

        do {
            let result = try SolutionGenerator().generate(options: GenerateOptions(
                projectRoot: resolveRealPath(projectRoot),
                generatorRoot: generatorRoot,
                verbose: verbose,
                platform: platform,
                buildConfig: buildConfig
            ))

            print(result.variantSlnPath)

            for warning in result.warnings {
                fputs("warning: \(warning)\n", stderr)
            }
        } catch {
            die("\(error)")
        }
    }

    static func die(_ message: String) -> Never {
        fputs("error: \(message)\n", stderr)
        exit(1)
    }

    static func printUsage() {
        print("""
        USAGE:
          unity-solution-generator init <unity-root> [generator-root]
          unity-solution-generator generate <unity-root> <platform> <config> [generator-root] [options]

        COMMANDS:
          init                  Extract .csproj templates from Unity-generated project files
          generate              Regenerate .csproj/.sln for a platform+config variant

        ARGUMENTS:
          unity-root            Unity project root
          platform              ios | android
          config                prod | dev | editor
          generator-root        Generator root (default: Library/UnitySolutionGenerator)

        OPTIONS:
          -v, --verbose         Print unresolved directory samples
          -h, --help            Show help
        """)
    }
}
