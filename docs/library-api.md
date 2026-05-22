# Library API

> **Related:** [[CLAUDE.md]], [[architecture.md]], [[benchmark.md]], [[TODO.md]]

Published to crates.io as `unity-solution-generator`. Cdylib lives downstream
(meow-tower's `BoxcatBridge`); this crate exposes only a Rust API.

**Required runtime dep:** [Watchman](https://facebook.github.io/watchman/) — install per the [README](../README.md#install). Project scanning hard-fails on `ScanUnavailable` if the daemon is unreachable.

## High-level (`unity_solution_generator::generate`)

```rust
unity_solution_generator::generate(
    project_root,
    "ios",       // platform: ios | android | osx | windows
    "editor",    // config:   prod | dev | editor
    None,        // output_dir; None → Library/UnitySolutionGenerator/<platform>-<config>/
    None,        // extra_refs: comma-separated DLL paths (or None)
)?;
```

Single-threaded contract — caller serializes calls (cache files aren't
reentrant-safe). Auto-runs `lockfile::scan_and_write` on cache miss (mtime
fingerprint over the project's asmdef/asmref layout).

## Lower-level building blocks (`use unity_solution_generator::*`)

The crate exposes module-level free functions; there are no wrapper structs
to instantiate.

| Symbol | Description |
|--------|-------------|
| `solution_generator::generate(&opts, &lockfile, scan)` | Render `.csproj`/`.sln`/`Directory.Build.props` for one variant from a pre-loaded scan. |
| `solution_generator::generate_from_lockfile(&opts, &lockfile)` | Convenience: scans internally, then renders. |
| `GenerateOptions` | Builder: `new(root, platform).with_build_config(...).with_output_dir(...).with_extra_refs(...)` |
| `GenerateResult` | `variant_sln_path`, `variant_csprojs`, `warnings` |
| `BuildPlatform` | `Ios`, `Android`, `Osx`, `Windows` (also `BuildPlatform::ALL` for iteration) |
| `BuildConfig` | `Prod`, `Dev`, `Editor` |
| `lockfile::scan_and_write(root, generator_root)` | Load or rescan + persist `csproj.lock`. |
| `lockfile::read(path)` / `lockfile::write(&lf, path)` | Raw lockfile I/O. |
| `Lockfile` | Unity version, DLL refs, defines, analyzers. |
| `DllRef` | `name`, `path` (and `DllRef::parse_list` for the comma-separated CLI form) |
| `RefCategory` | `Engine`, `Editor`, `Netstandard`, `PlaybackIos`, `PlaybackAndroid`, `PlaybackStandalone`, `PlaybackWindows`, `Project` |
| `project_scanner::scan(root, generator_root)` | Watchman-backed scan with mtime-fingerprinted cache. |
| `scan::enumerate(root)` | Direct Watchman query — returns the full project-relative path list. |
| `script_dll_dir(root, platform, config)` | Per-variant `obj/Debug` path used by external reflection tools. |

```rust
use unity_solution_generator::{
    BuildConfig, BuildPlatform, DllRef, GenerateOptions, lockfile, solution_generator,
};

let lockfile = lockfile::scan_and_write(project_root, "Library/UnitySolutionGenerator")?;
let options = GenerateOptions::new(project_root, BuildPlatform::Ios)
    .with_build_config(BuildConfig::Editor)
    .with_output_dir(Some("Library/com.example/Solution"))
    .with_extra_refs(vec![DllRef::new("MyLib", "/abs/path/to/MyLib.dll")]);
let result = solution_generator::generate_from_lockfile(&options, &lockfile)?;
// result.variant_sln_path → path to generated .sln
```
