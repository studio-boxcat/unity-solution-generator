# unity-solution-generator

Rust CLI and library that regenerates `.csproj` and `.sln` files for Unity
projects from `asmdef`/`asmref` layout — without launching the Unity editor.

Designed for headless CI and IDE-integration tools. Up to 12× faster than
Unity's own solution regeneration on real projects (warm-cache no-op);
see [`docs/benchmark.md`](docs/benchmark.md) for numbers.

## Install

`unity-solution-generator` requires [Watchman](https://facebook.github.io/watchman/)
at run time (filesystem scanning is delegated to it):

| Host | Watchman | usg binary |
|---|---|---|
| macOS | `brew install watchman` | `cargo install unity-solution-generator` |
| Windows | `choco install watchman` | `cargo install unity-solution-generator` (or download pre-built `.exe` from [Releases](https://github.com/studio-boxcat/unity-solution-generator/releases)) |
| Linux | per package manager, or build from source | `cargo install unity-solution-generator` |

Prebuilt binaries for macOS arm64 + Windows x64 are attached to each GitHub
Release.

## CLI

```bash
unity-solution-generator typecheck .                       # refresh .csproj/.sln + compile-check via csc.dll
unity-solution-generator build .                           # refresh + `dotnet build`
```

Positional: `<command> <unity-root> <platform> <config>`.
Platform: `ios | android | osx | windows`. Config: `prod | dev | editor`. Both subcommands default to `ios editor` and auto-lock on cache miss — no separate `lock` or `generate` subcommands.

## Library

```toml
[dependencies]
unity-solution-generator = "0.1"
```

```rust
unity_solution_generator::generate(
    project_root,
    "ios",
    "editor",
    None,         // output_dir; None → default Library/... path
    None,         // extra_refs (comma-separated DLL paths)
)?;
```

For lower-level control (render without invoking the compiler — what meow-tower's BoxcatBridge FFI uses) see `solution_generator::generate_from_lockfile`, `project_scanner::scan`, and `lockfile::scan_and_write`. The CLI is intentionally minimal — `typecheck` and `build` only.

## License

MIT
