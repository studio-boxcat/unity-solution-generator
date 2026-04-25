import Darwin

// MARK: - C-ABI exports for [DllImport] from C#

// Error state — valid until the next usg_ call from the same thread.
// Not thread-safe; Unity typically calls from the main thread.
nonisolated(unsafe) private var _lastErrorCStr: UnsafeMutablePointer<CChar>?

private func setLastError(_ message: String) {
    if let old = _lastErrorCStr { free(old) }
    _lastErrorCStr = strdup(message)
}

private func clearLastError() {
    if let old = _lastErrorCStr { free(old); _lastErrorCStr = nil }
}

/// Generate .csproj/.sln files from an existing lockfile.
/// Auto-runs lock if no lockfile exists.
///
/// - Parameters:
///   - projectRoot: Absolute path to Unity project root
///   - platform: "ios", "android", or "osx"
///   - config: "editor", "prod", or "dev"
///   - outputDir: Output directory relative to project root, "." for root, NULL for default variant dir
///   - extraRefs: Comma-separated absolute DLL paths, or NULL for none
///   - slnPathOut: Buffer to receive the output .sln path (relative to project root), or NULL to skip
///   - slnPathOutLen: Size of slnPathOut buffer in bytes (ignored when slnPathOut is NULL)
/// - Returns: 0 on success, nonzero on error (call usg_last_error for message)
@_cdecl("usg_generate")
public func usg_generate(
    _ projectRoot: UnsafePointer<CChar>,
    _ platform: UnsafePointer<CChar>,
    _ config: UnsafePointer<CChar>,
    _ outputDir: UnsafePointer<CChar>?,
    _ extraRefs: UnsafePointer<CChar>?,
    _ slnPathOut: UnsafeMutablePointer<CChar>?,
    _ slnPathOutLen: Int32
) -> Int32 {
    clearLastError()

    let platformStr = String(cString: platform)
    let configStr = String(cString: config)

    guard let buildPlatform = BuildPlatform(rawValue: platformStr) else {
        setLastError("Unknown platform '\(platformStr)'. Use 'ios', 'android', or 'osx'.")
        return 1
    }
    guard let buildConfig = BuildConfig(rawValue: configStr) else {
        setLastError("Unknown config '\(configStr)'. Use 'prod', 'dev', or 'editor'.")
        return 1
    }

    let resolvedRoot = resolveProjectRoot(String(cString: projectRoot))

    do {
        let options = GenerateOptions(
            projectRoot: resolvedRoot,
            outputDir: outputDir.map { String(cString: $0) },
            extraRefs: extraRefs.map { DllRef.parseList(String(cString: $0)) } ?? [],
            platform: buildPlatform,
            buildConfig: buildConfig
        )
        let lockfile = try LockfileIO.loadOrScan(projectRoot: resolvedRoot)
        let result = try SolutionGenerator().generateFromLockfile(options: options, lockfile: lockfile)

        let slnPath = result.variantSlnPath
        if let slnPathOut {
            guard slnPath.utf8.count < Int(slnPathOutLen) else {
                setLastError("Buffer too small (\(slnPathOutLen) bytes) for path (\(slnPath.utf8.count + 1) needed)")
                return 1
            }
            _ = strlcpy(slnPathOut, slnPath, Int(slnPathOutLen))
        }
        return 0
    } catch {
        setLastError("\(error)")
        return 1
    }
}

/// Scan Unity installation + project and write csproj.lock.
///
/// - Parameters:
///   - projectRoot: Absolute path to Unity project root
/// - Returns: 0 on success, nonzero on error
@_cdecl("usg_lock")
public func usg_lock(
    _ projectRoot: UnsafePointer<CChar>
) -> Int32 {
    clearLastError()

    do {
        _ = try LockfileIO.scanAndWrite(projectRoot: resolveProjectRoot(String(cString: projectRoot)))
        return 0
    } catch {
        setLastError("\(error)")
        return 1
    }
}

/// Returns the last error message, or NULL if no error.
/// The returned pointer is valid until the next usg_ call.
@_cdecl("usg_last_error")
public func usg_last_error() -> UnsafePointer<CChar>? {
    UnsafePointer(_lastErrorCStr)
}
