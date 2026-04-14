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
        guard !args.isEmpty else {
            die("lock requires: <unity-root>")
        }

        let projectRoot = resolveRealPath(args[0])
        let generatorRoot = defaultGeneratorRoot
        let generatorDir = joinPath(projectRoot, generatorRoot)
        createDirectoryRecursive(generatorDir)
        let lockfilePath = joinPath(generatorDir, "csproj.lock")

        do {
            let lockfile = try LockfileScanner.scan(projectRoot: projectRoot)
            try LockfileIO.write(lockfile, to: lockfilePath)

            let totalRefs = lockfile.totalRefCount

            print("Locked csproj.lock:")
            print("  Unity \(lockfile.unityVersion) (\(lockfile.unityPath))")
            print("  \(totalRefs) DLL references, \(lockfile.analyzers.count) analyzers")
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
        var extraRefPaths: [String] = []
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
                extraRefPaths = args[i].split(separator: ",").map(String.init)
            default: die("Unknown option: \(args[i])")
            }
            i += 1
        }

        let extraRefs = extraRefPaths.map { path in
            let filename = path.split(separator: "/").last.map(String.init) ?? path
            let name = filename.hasSuffix(".dll") ? String(filename.dropLast(4)) : filename
            return DllRef(name: name, path: path)
        }

        let resolvedRoot = resolveRealPath(projectRoot)
        let generatorRoot = defaultGeneratorRoot
        let lockfilePath = joinPath(resolvedRoot, joinPath(generatorRoot, "csproj.lock"))

        do {
            let options = GenerateOptions(
                projectRoot: resolvedRoot,
                verbose: verbose,
                outputDir: outputDir,
                extraRefs: extraRefs,
                platform: platform,
                buildConfig: buildConfig
            )

            let result: GenerateResult

            if fileExists(lockfilePath) {
                // Lockfile-based generation
                let lockfile = try LockfileIO.read(from: lockfilePath)
                result = try SolutionGenerator().generateFromLockfile(options: options, lockfile: lockfile)
            } else {
                // Check for templates (legacy fallback)
                let templatesDir = joinPath(resolvedRoot, joinPath(generatorRoot, "templates"))
                if fileExists(templatesDir) && !listDirectory(templatesDir).isEmpty {
                    fputs("warning: Using legacy templates. Run 'unity-solution-generator lock' to migrate.\n", stderr)
                    result = try SolutionGenerator().generate(options: options)
                } else {
                    // Auto-run lock
                    fputs("No lockfile found, running lock...\n", stderr)
                    let generatorDir = joinPath(resolvedRoot, generatorRoot)
                    createDirectoryRecursive(generatorDir)
                    let lockfile = try LockfileScanner.scan(projectRoot: resolvedRoot)
                    try LockfileIO.write(lockfile, to: lockfilePath)
                    fputs("Locked: \(lockfile.unityVersion)\n", stderr)
                    result = try SolutionGenerator().generateFromLockfile(options: options, lockfile: lockfile)
                }
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
