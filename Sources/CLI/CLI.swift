import Darwin
import SolutionGeneratorCore

@main
struct CLI {
    static func main() {
        var args = Array(CommandLine.arguments.dropFirst())

        if args.isEmpty || args.contains("--help") || args.contains("-h") {
            printUsage()
            return
        }

        switch args.first {
        case "lock":
            args.removeFirst()
            runLock(args)
        case "init":
            args.removeFirst()
            fputs("warning: 'init' is deprecated, use 'lock' instead.\n", stderr)
            runLock(args)
        case "generate":
            args.removeFirst()
            runGenerate(args)
        default:
            die("Unknown command '\(args.first!)'. Use 'lock', 'generate', or 'init'.")
        }
    }

    static func runLock(_ args: [String]) {
        guard !args.isEmpty else { die("lock requires: <unity-root>") }

        do {
            let lockfile = try LockfileIO.scanAndWrite(projectRoot: resolveProjectRoot(args[0]))
            print("Locked csproj.lock:")
            print("  Unity \(lockfile.unityVersion) (\(lockfile.unityPath))")
            print("  \(lockfile.totalRefCount) DLL references, \(lockfile.analyzers.count) analyzers")
            print("  \(lockfile.defines.count) defines, \(lockfile.definesScripting.count) scripting defines")
        } catch {
            die("\(error)")
        }
    }

    static func runGenerate(_ args: [String]) {
        guard args.count >= 3 else {
            die("generate requires: <unity-root> <platform> <config> [options]")
        }

        let projectRoot = args[0]

        guard let platform = BuildPlatform(rawValue: args[1]) else {
            die("Unknown platform '\(args[1])'. Use 'ios' or 'android'.")
        }

        guard let buildConfig = BuildConfig(rawValue: args[2]) else {
            die("Unknown config '\(args[2])'. Use 'prod', 'dev', or 'editor'.")
        }

        var verbose = false
        var outputDir: String? = nil
        var extraRefsRaw: String? = nil
        var i = 3
        while i < args.count {
            switch args[i] {
            case "-v", "--verbose":
                verbose = true
            case "--root":
                outputDir = "."
            case "-o", "--output":
                i += 1
                guard i < args.count else { die("--output requires a directory argument") }
                outputDir = args[i]
            case "--extra-refs":
                i += 1
                guard i < args.count else { die("--extra-refs requires a comma-separated list of DLL paths") }
                extraRefsRaw = args[i]
            default: die("Unknown option: \(args[i])")
            }
            i += 1
        }

        let resolvedRoot = resolveProjectRoot(projectRoot)
        let templatesDir = joinPath(resolvedRoot, "\(defaultGeneratorRoot)/templates")

        do {
            let options = GenerateOptions(
                projectRoot: resolvedRoot,
                verbose: verbose,
                outputDir: outputDir,
                extraRefs: extraRefsRaw.map(DllRef.parseList) ?? [],
                platform: platform,
                buildConfig: buildConfig
            )

            let result: GenerateResult
            if fileExists(lockfilePath(for: resolvedRoot)) {
                let lockfile = try LockfileIO.read(from: lockfilePath(for: resolvedRoot))
                result = try SolutionGenerator().generateFromLockfile(options: options, lockfile: lockfile)
            } else if fileExists(templatesDir) && !listDirectory(templatesDir).isEmpty {
                fputs("warning: Using legacy templates. Run 'unity-solution-generator lock' to migrate.\n", stderr)
                result = try SolutionGenerator().generate(options: options)
            } else {
                fputs("No lockfile found, running lock...\n", stderr)
                let lockfile = try LockfileIO.scanAndWrite(projectRoot: resolvedRoot)
                fputs("Locked: \(lockfile.unityVersion)\n", stderr)
                result = try SolutionGenerator().generateFromLockfile(options: options, lockfile: lockfile)
            }

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
          unity-solution-generator lock <unity-root>
          unity-solution-generator generate <unity-root> <platform> <config> [options]

        COMMANDS:
          lock                  Scan Unity installation and project to generate csproj.lock
          generate              Regenerate .csproj/.sln for a platform+config variant
          init                  (deprecated) Alias for lock

        ARGUMENTS:
          unity-root            Unity project root
          platform              ios | android
          config                prod | dev | editor

        OPTIONS:
          -o, --output <dir>    Output to <dir> (relative to project root) instead of variant dir
          --root                Alias for --output . (output to project root)
          --extra-refs <paths>  Comma-separated absolute paths to additional DLLs
          -v, --verbose         Print unresolved directory samples
          -h, --help            Show help
        """)
    }
}
