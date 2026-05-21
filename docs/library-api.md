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
reentrant-safe). Auto-runs `lock` if the lockfile is missing or invalidated by
either the editor-version check or the Watchman clock delta.

## Lower-level building blocks (`use unity_solution_generator::*`)

| Type | Description |
|------|-------------|
| `SolutionGenerator` | `.generate_from_lockfile(&options, &lockfile)` |
| `GenerateOptions` | builder: `new(root, platform).with_build_config(...).with_output_dir(...).with_extra_refs(...)` |
| `GenerateResult` | `variant_sln_path`, `variant_csprojs`, `warnings` |
| `BuildPlatform` | `Ios`, `Android`, `Osx`, `Windows` (also `BuildPlatform::ALL` for iteration) |
| `BuildConfig` | `Prod`, `Dev`, `Editor` |
| `LockfileIO` | `::read(path)`, `::write(&lf, path)`, `::scan_and_write(root, generator_root)`, `::load_or_scan(root, generator_root)` |
| `Lockfile` | Unity version, DLL refs, defines, analyzers (struct literal or `LockfileIO::read`) |
| `DllRef` | `name`, `path` (and `DllRef::parse_list` for the comma-separated CLI form) |
| `RefCategory` | `Engine`, `Editor`, `Netstandard`, `PlaybackIos`, `PlaybackAndroid`, `PlaybackStandalone`, `PlaybackWindows`, `Project` |
| `scan::since(root, prev_clock)` | Direct Watchman query — returns `Delta::Fresh` (full enumeration) or `Delta::Touched { paths, new_clock }` |

```rust
use unity_solution_generator::{
    BuildConfig, BuildPlatform, DllRef, GenerateOptions, LockfileIO, SolutionGenerator,
};

let lockfile = LockfileIO::read("Library/UnitySolutionGenerator/csproj.lock")?;
let options = GenerateOptions::new(project_root, BuildPlatform::Ios)
    .with_build_config(BuildConfig::Editor)
    .with_output_dir(Some("Library/com.example/Solution"))
    .with_extra_refs(vec![DllRef::new("MyLib", "/abs/path/to/MyLib.dll")]);
let result = SolutionGenerator::new().generate_from_lockfile(&options, &lockfile)?;
// result.variant_sln_path → path to generated .sln
```
