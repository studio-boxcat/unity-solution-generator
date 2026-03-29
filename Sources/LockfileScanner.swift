import Darwin

struct DllRef: Sendable {
    let name: String
    let path: String
}

struct Lockfile: Sendable {
    let unityVersion: String
    let unityPath: String
    let langVersion: String
    let analyzers: [String]
    let refsEngine: [DllRef]
    let refsEditor: [DllRef]
    let refsNetstandard: [DllRef]
    let refsPlaybackIos: [DllRef]
    let refsPlaybackAndroid: [DllRef]
    let refsPlaybackStandalone: [DllRef]
    let refsProject: [DllRef]
    let defines: [String]
    let definesScripting: [String]
}

enum LockfileError: Error, CustomStringConvertible {
    case noProjectVersion(String)
    case unityNotFound(String)
    case invalidLockfile(String)

    var description: String {
        switch self {
        case .noProjectVersion(let path):
            return "Cannot find ProjectSettings/ProjectVersion.txt in: \(path)"
        case .unityNotFound(let path):
            return "Unity installation not found at: \(path)"
        case .invalidLockfile(let reason):
            return "Invalid lockfile: \(reason)"
        }
    }
}

struct LockfileScanner {

    static func scan(projectRoot: String) throws -> Lockfile {
        let (version, unityPath) = try resolveUnityPath(projectRoot: projectRoot)
        let appContents = joinPath(unityPath, "Unity.app/Contents")

        // Engine + editor DLLs from Managed/UnityEngine/
        let managedEngineDir = joinPath(appContents, "Managed/UnityEngine")
        var engineRefs: [DllRef] = []
        var editorRefs: [DllRef] = []
        for dll in listDirectory(managedEngineDir).filter({ $0.hasSuffix(".dll") }).sorted() {
            let name = String(dll.dropLast(4))
            guard name.hasPrefix("UnityEngine") || name.hasPrefix("UnityEditor") else { continue }
            let path = "$(UnityPath)/Unity.app/Contents/Managed/UnityEngine/\(dll)"
            if name.hasPrefix("UnityEditor") {
                editorRefs.append(DllRef(name: name, path: path))
            } else {
                engineRefs.append(DllRef(name: name, path: path))
            }
        }

        // UnityEditor.Graphs from Contents/Managed/ (check directly instead of scanning)
        let graphsDll = joinPath(appContents, "Managed/UnityEditor.Graphs.dll")
        if fileExists(graphsDll) {
            editorRefs.append(DllRef(name: "UnityEditor.Graphs", path: "$(UnityPath)/Unity.app/Contents/Managed/UnityEditor.Graphs.dll"))
        }

        // NetStandard shims
        let netstdBase = joinPath(appContents, "NetStandard")
        var netstdRefs: [DllRef] = []
        walkFiles(directory: netstdBase, basePath: netstdBase, extensions: [".dll"], skipNativePluginDirs: false) { relPath, dll in
            let name = String(dll.dropLast(4))
            netstdRefs.append(DllRef(name: name, path: "$(UnityPath)/Unity.app/Contents/NetStandard/\(relPath)"))
        }

        // Playback engines
        let playbackBase = joinPath(unityPath, "PlaybackEngines")
        let iosRefs = scanPlaybackDlls(joinPath(playbackBase, "iOSSupport"), prefix: "PlaybackEngines/iOSSupport")
        let androidRefs = scanPlaybackDlls(joinPath(playbackBase, "AndroidPlayer"), prefix: "PlaybackEngines/AndroidPlayer")
        let standaloneDir = joinPath(appContents, "PlaybackEngines/MacStandaloneSupport")
        let standaloneRefs = scanPlaybackDlls(standaloneDir, prefix: "Unity.app/Contents/PlaybackEngines/MacStandaloneSupport")

        // Unity source generators (analyzers)
        let sourceGenDir = joinPath(appContents, "Tools/Unity.SourceGenerators")
        var analyzers: [String] = []
        for dll in listDirectory(sourceGenDir).filter({ $0.hasSuffix(".dll") }).sorted() {
            analyzers.append("$(UnityPath)/Unity.app/Contents/Tools/Unity.SourceGenerators/\(dll)")
        }

        // Project DLLs + analyzers (deduplicate by assembly name, first wins)
        var projectRefs: [DllRef] = []
        var seenProjectDlls: Set<String> = []
        var seenAnalyzers: Set<String> = []
        // Also collect asmdef paths during the same walk to avoid a second traversal
        var asmdefPaths: [String] = []
        for root in ["Assets", "Packages", "Library/PackageCache"] {
            let rootDir = joinPath(projectRoot, root)
            walkFiles(directory: rootDir, basePath: projectRoot, extensions: [".dll", ".asmdef"]) { relPath, fileName in
                if fileName.hasSuffix(".dll") {
                    let name = String(fileName.dropLast(4))
                    let path = "$(ProjectRoot)/\(relPath)"
                    if isAnalyzerDll(name) {
                        if seenAnalyzers.insert(name).inserted { analyzers.append(path) }
                    } else {
                        if seenProjectDlls.insert(name).inserted {
                            projectRefs.append(DllRef(name: name, path: path))
                        }
                    }
                } else {
                    asmdefPaths.append(joinPath(projectRoot, relPath))
                }
            }
        }

        analyzers.sort()
        projectRefs.sort { $0.name < $1.name }

        // Defines
        let versionDefines = generateVersionDefines(version: version)
        let asmdefDefines = collectAsmdefVersionDefines(projectRoot: projectRoot, asmdefPaths: asmdefPaths)
        let allDefines = versionDefines + defaultFeatureDefines + asmdefDefines
        let scriptingDefines = parseScriptingDefines(projectRoot: projectRoot)

        return Lockfile(
            unityVersion: version,
            unityPath: unityPath,
            langVersion: "9.0",
            analyzers: analyzers,
            refsEngine: engineRefs,
            refsEditor: editorRefs,
            refsNetstandard: netstdRefs.sorted { $0.name < $1.name },
            refsPlaybackIos: iosRefs,
            refsPlaybackAndroid: androidRefs,
            refsPlaybackStandalone: standaloneRefs,
            refsProject: projectRefs,
            defines: allDefines,
            definesScripting: scriptingDefines
        )
    }
}

// MARK: - Unity path resolution

private func resolveUnityPath(projectRoot: String) throws -> (version: String, path: String) {
    let versionFile = joinPath(projectRoot, "ProjectSettings/ProjectVersion.txt")
    guard fileExists(versionFile) else {
        throw LockfileError.noProjectVersion(projectRoot)
    }
    let content = try readFile(versionFile)
    guard let colonIdx = content.firstIndex(of: ":") else {
        throw LockfileError.noProjectVersion(projectRoot)
    }
    var idx = content.index(after: colonIdx)
    while idx < content.endIndex && content[idx] == " " { content.formIndex(after: &idx) }
    var end = idx
    while end < content.endIndex && content[end] != "\n" && content[end] != "\r" {
        content.formIndex(after: &end)
    }
    let version = String(content[idx..<end])
    guard !version.isEmpty else {
        throw LockfileError.noProjectVersion(projectRoot)
    }

    let unityPath = "/Applications/Unity/Hub/Editor/\(version)"
    guard fileExists(unityPath) else {
        throw LockfileError.unityNotFound(unityPath)
    }
    return (version, resolveRealPath(unityPath))
}

// MARK: - DLL scanning

/// Recursively walk directories, collecting files matching any of the given extensions.
private func walkFiles(
    directory: String,
    basePath: String,
    extensions: [String],
    skipNativePluginDirs: Bool = true,
    handler: (String, String) -> Void
) {
    guard let dir = opendir(directory) else { return }
    defer { closedir(dir) }

    while let entry = readdir(dir) {
        let name = direntName(entry)
        if name == "." || name == ".." || name.first == "." || name.hasSuffix("~") { continue }

        let childPath = "\(directory)/\(name)"
        let dType = entry.pointee.d_type

        if dType == DT_DIR || (dType == DT_LNK || dType == DT_UNKNOWN) && isDirectory(childPath) {
            if skipNativePluginDirs && isNativePluginDir(name) { continue }
            walkFiles(directory: childPath, basePath: basePath, extensions: extensions, skipNativePluginDirs: skipNativePluginDirs, handler: handler)
        } else {
            for ext in extensions {
                if name.hasSuffix(ext) {
                    let prefixLen = basePath.count + 1
                    if childPath.count > prefixLen {
                        handler(String(childPath.dropFirst(prefixLen)), name)
                    }
                    break
                }
            }
        }
    }
}

private func isNativePluginDir(_ name: String) -> Bool {
    switch name {
    case "x86", "x86_64", "arm64-v8a", "armeabi-v7a", "ARM64", "x64":
        return true
    default:
        return name.hasSuffix(".framework") || name.hasSuffix(".bundle")
    }
}

/// Filter playback engine DLLs to Unity extension assemblies only.
private func scanPlaybackDlls(_ directory: String, prefix: String) -> [DllRef] {
    var refs: [DllRef] = []
    for dll in listDirectory(directory).filter({ $0.hasSuffix(".dll") }).sorted() {
        let name = String(dll.dropLast(4))
        if name.hasPrefix("UnityEditor.") || name.hasPrefix("Unity.Android.") {
            refs.append(DllRef(name: name, path: "$(UnityPath)/\(prefix)/\(dll)"))
        }
    }
    return refs
}

private func isAnalyzerDll(_ name: String) -> Bool {
    let lower = name.lowercased()
    return lower.contains("analyzer") || lower.contains("sourcegenerator")
}

// MARK: - Version defines

func generateVersionDefines(version: String) -> [String] {
    let parts = version.split(separator: ".")
    guard parts.count >= 3 else { return [] }
    guard let major = Int(parts[0]), let minor = Int(parts[1]) else { return [] }
    var patchStr = ""
    for ch in parts[2] {
        guard ch.isNumber else { break }
        patchStr.append(ch)
    }
    guard let patch = Int(patchStr) else { return [] }

    var defines: [String] = [
        "UNITY_\(major)_\(minor)_\(patch)",
        "UNITY_\(major)_\(minor)",
        "UNITY_\(major)",
    ]

    let versionPoints: [(Int, Int)] = [
        (5, 3), (5, 4), (5, 5), (5, 6),
        (2017, 1), (2017, 2), (2017, 3), (2017, 4),
        (2018, 1), (2018, 2), (2018, 3), (2018, 4),
        (2019, 1), (2019, 2), (2019, 3), (2019, 4),
        (2020, 1), (2020, 2), (2020, 3),
        (2021, 1), (2021, 2), (2021, 3),
        (2022, 1), (2022, 2), (2022, 3),
        (2023, 1), (2023, 2), (2023, 3),
    ]
    for (maj, min) in versionPoints {
        if major > maj || (major == maj && minor >= min) {
            defines.append("UNITY_\(maj)_\(min)_OR_NEWER")
        }
    }
    if major >= 6000 {
        for m in 0...minor {
            defines.append("UNITY_6000_\(m)_OR_NEWER")
        }
    }

    return defines
}

// MARK: - Feature defines (stable superset for Unity 6.x)

private let defaultFeatureDefines: [String] = [
    "UNITY_INCLUDE_TESTS",
    "ENABLE_AR", "ENABLE_AUDIO", "ENABLE_CACHING", "ENABLE_CLOTH",
    "ENABLE_EVENT_QUEUE", "ENABLE_MICROPHONE", "ENABLE_MULTIPLE_DISPLAYS",
    "ENABLE_PHYSICS", "ENABLE_TEXTURE_STREAMING", "ENABLE_LZMA",
    "ENABLE_UNITYEVENTS", "ENABLE_VR", "ENABLE_WEBCAM",
    "ENABLE_UNITYWEBREQUEST", "ENABLE_WWW",
    "ENABLE_CLOUD_SERVICES", "ENABLE_CLOUD_SERVICES_ADS",
    "ENABLE_CLOUD_SERVICES_USE_WEBREQUEST", "ENABLE_UNITY_CONSENT",
    "ENABLE_UNITY_CLOUD_IDENTIFIERS",
    "ENABLE_CLOUD_SERVICES_CRASH_REPORTING",
    "ENABLE_CLOUD_SERVICES_NATIVE_CRASH_REPORTING",
    "ENABLE_CLOUD_SERVICES_PURCHASING",
    "ENABLE_CLOUD_SERVICES_ANALYTICS", "ENABLE_CLOUD_SERVICES_BUILD",
    "ENABLE_EDITOR_GAME_SERVICES",
    "ENABLE_UNITY_GAME_SERVICES_ANALYTICS_SUPPORT",
    "ENABLE_CLOUD_LICENSE", "ENABLE_EDITOR_HUB_LICENSE",
    "ENABLE_CLOUD_SERVICES_ENGINE_DIAGNOSTICS",
    "ENABLE_WEBSOCKET_CLIENT",
    "ENABLE_GENERATE_NATIVE_PLUGINS_FOR_ASSEMBLIES_API",
    "ENABLE_DIRECTOR_AUDIO", "ENABLE_DIRECTOR_TEXTURE",
    "ENABLE_MANAGED_JOBS", "ENABLE_MANAGED_TRANSFORM_JOBS",
    "ENABLE_MANAGED_ANIMATION_JOBS", "ENABLE_MANAGED_AUDIO_JOBS",
    "ENABLE_ENGINE_CODE_STRIPPING", "ENABLE_ONSCREEN_KEYBOARD",
    "ENABLE_MANAGED_UNITYTLS", "INCLUDE_DYNAMIC_GI",
    "ENABLE_SCRIPTING_GC_WBARRIERS", "PLATFORM_SUPPORTS_MONO",
    "ENABLE_MARSHALLING_TESTS", "ENABLE_VIDEO",
    "ENABLE_NAVIGATION_OFFMESHLINK_TO_NAVMESHLINK",
    "ENABLE_ACCELERATOR_CLIENT_DEBUGGING", "ENABLE_ACCESSIBILITY",
    "TEXTCORE_1_0_OR_NEWER",
    "TEXTCORE_FONT_ENGINE_1_5_OR_NEWER", "TEXTCORE_TEXT_ENGINE_1_5_OR_NEWER",
    "EDITOR_ONLY_NAVMESH_BUILDER_DEPRECATED",
    "ENABLE_EGL", "ENABLE_NETWORK", "ENABLE_RUNTIME_GI",
    "ENABLE_CRUNCH_TEXTURE_COMPRESSION", "ENABLE_FIREBASE_IDENTIFIERS",
    "UNITY_CAN_SHOW_SPLASH_SCREEN",
    "UNITY_HAS_GOOGLEVR", "UNITY_HAS_TANGO",
    "ENABLE_SPATIALTRACKING", "ENABLE_ETC_COMPRESSION",
    "PLATFORM_EXTENDS_VULKAN_DEVICE", "PLATFORM_HAS_MULTIPLE_SWAPCHAINS",
    "PLATFORM_UPDATES_TIME_OUTSIDE_OF_PLAYER_LOOP",
    "PLATFORM_EXTENDS_VULKAN_PIPELINE_CACHE",
    "PLATFORM_SUPPORTS_SPLIT_GRAPHICS_JOBS",
    "PLATFORM_HAS_ADDITIONAL_API_CHECKS",
    "PLATFORM_HAS_GRAPHICS_JOBS_SUPPORT_CHECK_OVERRIDE",
    "PLATFORM_IMPLEMENTS_INSIGHTS_ANR",
    "PLATFORM_SUPPORTS_INSIGHTS_DEVICE_INFO",
    "ENABLE_ANDROID_ADVERTISING_IDS",
    "PLATFORM_HAS_BUGGY_MSAA_RESOLVE",
    "ENABLE_ANDROID_APP_SET_ID",
    "ENABLE_INSIGHTS_PLATFORM_SPECIFIC_RESOURCES",
    "ENABLE_UNITYADS_RUNTIME", "UNITY_UNITYADS_API",
    "ENABLE_MONO", "NET_STANDARD_2_0", "NET_STANDARD", "NET_STANDARD_2_1",
    "NETSTANDARD", "NETSTANDARD2_1",
    "ENABLE_PROFILER", "ENABLE_UNITY_COLLECTIONS_CHECKS", "ENABLE_BURST_AOT",
    "UNITY_TEAM_LICENSE", "UNITY_PRO_LICENSE",
    "ENABLE_CUSTOM_RENDER_TEXTURE", "ENABLE_DIRECTOR",
    "ENABLE_LOCALIZATION", "ENABLE_SPRITES", "ENABLE_TERRAIN",
    "ENABLE_TILEMAP", "ENABLE_TIMELINE",
    "ENABLE_LEGACY_INPUT_MANAGER",
    "CSHARP_7_OR_LATER", "CSHARP_7_3_OR_NEWER",
]

// MARK: - Asmdef versionDefines

/// Evaluate versionDefines from pre-collected asmdef paths (avoids a second filesystem walk).
private func collectAsmdefVersionDefines(projectRoot: String, asmdefPaths: [String]) -> [String] {
    var installedPackages: Set<String> = ["Unity"]
    let manifestPath = joinPath(projectRoot, "Packages/manifest.json")
    if let manifest = try? readFile(manifestPath) {
        for pkg in extractJsonObjectKeys(manifest, key: "dependencies") {
            installedPackages.insert(pkg)
        }
    }
    for entry in listDirectory(joinPath(projectRoot, "Packages")) {
        if entry.hasSuffix(".json") || entry.first == "." { continue }
        installedPackages.insert(entry)
    }

    var allDefines: Set<String> = []
    for path in asmdefPaths {
        guard let content = try? readFile(path) else { continue }
        // Reuse the same parser as ProjectScanner
        for vd in parseVersionDefines(content) {
            if installedPackages.contains(vd.packageName) {
                allDefines.insert(vd.define)
            }
        }
    }

    return allDefines.sorted()
}

// MARK: - Scripting defines

func parseScriptingDefines(projectRoot: String) -> [String] {
    let settingsPath = joinPath(projectRoot, "ProjectSettings/ProjectSettings.asset")
    guard let content = try? readFile(settingsPath) else { return [] }

    var inSection = false
    var allDefines: Set<String> = []

    for rawLine in content.split(separator: "\n", omittingEmptySubsequences: false) {
        let line = String(rawLine)
        if line.hasPrefix("  scriptingDefineSymbols:") {
            inSection = true
            continue
        }
        if inSection {
            guard line.hasPrefix("    ") else { break }
            if let colonIdx = line.firstIndex(of: ":") {
                let valueStart = line.index(after: colonIdx)
                let value = trimWhitespace(String(line[valueStart...]))
                if !value.isEmpty {
                    for define in value.split(separator: ";") {
                        let d = trimWhitespace(String(define))
                        if !d.isEmpty { allDefines.insert(d) }
                    }
                }
            }
        }
    }

    return allDefines.sorted()
}
