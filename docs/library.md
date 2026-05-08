# Library API

> **Related:** [[CLAUDE.md]]

`libUnitySolutionGenerator.dylib` exposes a C ABI (for Unity `[DllImport]`) plus a Rust API via the `usg-core` crate.

## C ABI (`dist/UnitySolutionGenerator.h`)

```c
int32_t usg_generate(const char *projectRoot, const char *platform, const char *config,
                     const char *outputDir, const char *extraRefs,
                     char *slnPathOut, int32_t slnPathOutLen);
const char *usg_last_error(void);  // valid until next usg_ call
```

C# usage:

```csharp
[DllImport("UnitySolutionGenerator")]
static extern int usg_generate(string projectRoot, string platform, string config,
                               string outputDir, string extraRefs,
                               IntPtr slnPathOut, int slnPathOutLen);

[DllImport("UnitySolutionGenerator")]
static extern IntPtr usg_last_error();

// Usage (ignoring the path, like Rider does):
if (usg_generate(root, "ios", "editor", ".", "/path/to/Extra.dll", IntPtr.Zero, 0) != 0)
    throw new Exception(Marshal.PtrToStringAnsi(usg_last_error()));
```

`outputDir`: relative path, `"."` for project root, `null` for default variant dir. `extraRefs`: comma-separated absolute DLL paths, `null` for none. `slnPathOut` / `slnPathOutLen`: optional output buffer for the generated `.sln` path; pass `IntPtr.Zero, 0` to skip. `usg_generate` auto-runs the equivalent of `lock` if no lockfile exists.

**Single-threaded contract.** Cache files aren't reentrant-safe; callers must serialize. Unity callers naturally serialize via the main asset-import thread.

## Rust API (`use usg_core::*`)

| Type | Description |
|------|-------------|
| `SolutionGenerator` | `.generate_from_lockfile(&options, &lockfile)`, `.generate(&options)` |
| `GenerateOptions` | builder: `new(root, platform).with_build_config(...).with_output_dir(...).with_extra_refs(...)` |
| `GenerateResult` | `variant_sln_path`, `variant_csprojs`, `warnings` |
| `BuildPlatform` | `Ios`, `Android`, `Osx` |
| `BuildConfig` | `Prod`, `Dev`, `Editor` |
| `LockfileIO` | `::read(path)`, `::write(&lf, path)`, `::scan_and_write(root)`, `::load_or_scan(root)` |
| `Lockfile` | Unity version, DLL refs, defines, analyzers (struct literal or `LockfileIO::read`) |
| `DllRef` | `name`, `path` (and `DllRef::parse_list` for the comma-separated CLI form) |
| `RefCategory` | `Engine`, `Editor`, `Netstandard`, `PlaybackIos`, `PlaybackAndroid`, `PlaybackStandalone`, `Project` |

```rust
use usg_core::{BuildConfig, BuildPlatform, DllRef, GenerateOptions, LockfileIO, SolutionGenerator};

let lockfile = LockfileIO::read("Library/UnitySolutionGenerator/csproj.lock")?;
let options = GenerateOptions::new(project_root, BuildPlatform::Ios)
    .with_build_config(BuildConfig::Editor)
    .with_output_dir(Some("Library/com.example/Solution"))
    .with_extra_refs(vec![DllRef::new("MyLib", "/abs/path/to/MyLib.dll")]);
let result = SolutionGenerator::new().generate_from_lockfile(&options, &lockfile)?;
// result.variant_sln_path → path to generated .sln
```
