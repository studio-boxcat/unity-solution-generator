# unity-solution-generator

Rust CLI and library that regenerates `.csproj` and `.sln` files for Unity
projects from `asmdef`/`asmref` layout — without launching the Unity editor.

Designed for headless CI and IDE-integration tools. ~20× faster than Unity's
own solution regeneration on real projects.

## Install

```bash
cargo install unity-solution-generator
```

## CLI

```bash
unity-solution-generator lock .                            # scan + write lockfile
unity-solution-generator generate . ios editor             # default output → Library/UnitySolutionGenerator/<platform>-<config>/
unity-solution-generator typecheck .                       # compile-check via csc.dll (defaults: ios editor)
```

Positional: `<command> <unity-root> <platform> <config>`.
Platform: `ios | android | osx`. Config: `prod | dev | editor`.

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

For lower-level control see `ProjectScanner`, `LockfileScanner`, `SolutionGenerator`.

## License

MIT
