# TODO

> **Related:** [[CLAUDE.md]], [[architecture.md]], [[library-api.md]], [[benchmark.md]]

## Deferred

Follow-up candidates surfaced during the v0.5.0 cache-simplification overhaul (adversarial review). All non-blocking; pick up if the codebase touches the area.

- **Collapse unit structs to module-level free functions.** `ProjectScanner`, `LockfileIO`, `LockfileScanner`, `SolutionGenerator` are zero-field unit structs with a single `impl` block of free associated fns. Convert to `project_scanner::scan(...)`, `lockfile::scan_and_write(...)`, etc. — matches `typecheck::run` and `scan::enumerate` which already are free fns. Removes meaningless `new()` calls. Breaking API change for any downstream Rust consumer; FFI surface unaffected.
- **Move `csc-dll-path` sidecar out of project tree.** It's a per-host (`dotnet --list-sdks`) artifact polluting `Library/UnitySolutionGenerator/`. Belongs under `usg_cache_dir(<unity-version>)/csc-dll-path` or `$XDG_CACHE_HOME/usg/csc-dll-path` — same place `package_cache.rs` already writes.
- **Drop the duplicate `scan_err_to_generator` helpers.** Two near-identical impls in `lockfile_scanner.rs` and `project_scanner.rs`. Merge into one `pub(crate) fn` in `scan.rs`.
- **Bincode-ify `scan-cache`.** ~150 LOC of text codec (`encode_asmdef_record`/`decode_asmdef_record`/`split_semi` etc.) for a format no human edits. `bincode = "2"` already in the workspace deps; replacing the codec drops ~120 LOC.
- **Persist `.asmdef`/`.asmref` paths in `scan-cache`.** `collect_mtimes` currently `read_dir`s each asmdef directory to find them. Storing the path list in the cache header skips that walk on warm path (~0.5 ms saved).
- **Drop `ProjectVersion.txt` from `scan-cache` fingerprint.** Lockfile owns that invariant via its `unity-version` check; the duplicate stat is redundant. (Small win, breaks the "scan-cache fingerprint is the single invalidation signal" mental model — only worth it if we're sure.)

## Historical notes

These aren't TODOs — kept as durable research breadcrumbs so a future
overhauler doesn't re-research them.

### Rejected MSBuild knobs (no-emit wiring)

- `-t:CoreCompile` (alone) — broke `ResolveProjectReferences`, downstream csprojs lose refs to upstream. Has to be `-t:Build` with property-level skips.
- `-p:CopyBuildOutputToOutputDirectory=false` — broke ProjectReference resolution (downstream csprojs need the upstream DLL in `bin/`). `ProduceReferenceAssemblyInOutDir=true` is the working substitute.

### Why DIY in Rust over Ninja/Bazel (researched, decided)

- **Ninja** would save ~100 LOC (topo-sort + parallel scheduler) but adds a binary dep; `usg-core` already has lockfile + scan-cache + nanosecond-mtime infra to do UTD itself. Reasonable plan B if a future DAG walker turns out non-trivial.
- **Bazel `rules_dotnet`** is slower on no-op for our scale (200–500 ms startup) and the Roslyn persistent-worker support is only mature in [AFASResearch's fork](https://github.com/AFASResearch/rules_dotnet). Wrong scale.
- **Strong precedent:** Unity's own [Bee](https://aras-p.info/blog/2019/06/21/Replacing-a-live-system-is-really-hard/) is exactly this pattern (custom DAG scheduler driving csc directly, cache in `Library/Bee/`). They chose not to wrap Ninja.
- **"Never write anything besides `.dll` into the typecheck output dir"** — held while typecheck owned `typecheck-<variant>/`; superseded by the obj/Debug consolidation. See [[architecture.md#typecheck-deeper]] (Foreign-writer guard).
