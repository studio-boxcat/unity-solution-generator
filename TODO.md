# TODO

## Deferred

### Configurable package blacklist (`lockfile_scanner.rs:BLACKLISTED_PACKAGE_DIRS`)

Currently a `&'static [&str]` hardcoded to `["com.singularitygroup.hotreload"]`. Accepted as-is for the immediate need. Future: per-project override via CLI flag (`--ignore-package <name>` repeatable) or a `.usg-config.json` at project root, since other Unity projects will hit different problem packages (asmref-merging plus version-specific DLL variants is not a HotReload-only pattern).

Relatedly, the const is a primitive `&[&str]` reused across files — strong-types policy (CLAUDE.md) suggests `PackageName(&'static str)`. Low priority while the list is one entry.

### `build_rsp` argument count

`typecheck.rs:build_rsp` takes 8 args (clippy `too_many_arguments`). Pre-existing; the test-only re-export emerged but didn't introduce the smell. Pack into a `BuildRspInputs` struct if a 9th arg shows up.

### `walk_files` clippy `while_let_on_iterator`

`lockfile_scanner.rs:329`. Pre-existing; the loop walks `WalkDir` entries which is naturally a `while let`. Cosmetic, no behaviour change.

### `generate --root` hyperfine benchmark fails

`just profile` calls `generate . ios editor --root` which currently exits non-zero on first warmup (pre-existing — the `--root` flag may have been renamed/removed). Recipe needs an audit; for now hyperfine just skips that row.

### Other rejected/closed during no-emit wiring

- `-t:CoreCompile` (alone) — broke `ResolveProjectReferences`, downstream csprojs lose refs to upstream. Has to be `-t:Build` with property-level skips.
- `-p:CopyBuildOutputToOutputDirectory=false` — broke ProjectReference resolution (downstream csprojs need the upstream DLL in `bin/`). `ProduceReferenceAssemblyInOutDir=true` is the working substitute.

### Why DIY in Rust over Ninja/Bazel (researched, decided)

- **Ninja** would save ~100 LOC (topo-sort + parallel scheduler) but adds a binary dep; `usg-core` already has lockfile + scan-cache + nanosecond-mtime infra to do UTD itself. Reasonable plan B if a future DAG walker turns out non-trivial.
- **Bazel `rules_dotnet`** is slower on no-op for our scale (200–500 ms startup) and the Roslyn persistent-worker support is only mature in [AFASResearch's fork](https://github.com/AFASResearch/rules_dotnet). Wrong scale.
- **Strong precedent:** Unity's own [Bee](https://aras-p.info/blog/2019/06/21/Replacing-a-live-system-is-really-hard/) is exactly this pattern (custom DAG scheduler driving csc directly, cache in `Library/Bee/`). They chose not to wrap Ninja.
- **Risk to avoid:** never write anything into the typecheck output dir besides each project's `.dll`, or DIY UTD will misfire the same way MSBuild's `obj/.../CoreCompileInputs.cache` does today.
