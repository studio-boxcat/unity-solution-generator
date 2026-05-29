# TODO

> **Related:** [[CLAUDE.md]], [[architecture.md]], [[library-api.md]], [[benchmark.md]]

## Deferred

(empty)

## Historical notes

These aren't TODOs — kept as durable research breadcrumbs so a future
overhauler doesn't re-research them.

### Rejected declutter changes (decided against)

Flagged by the declutter audit, investigated, kept as-is — collapsing them
degrades the code:
- **`GenerateOptions`/`TypecheckOptions` builders** — multiple real consumers
  (`generate()` + CLI + tests), and `with_output_dir` encapsulates trailing-slash
  normalization. Collapsing to struct literals duplicates that and uglifies call
  sites. The builder is the intended published lower-level API ([[library-api.md]]).
- **`DllRef::new(impl Into<String>, …)`** — the generic lets the String-producing
  scanner sites *move* owned strings in without re-allocation, while `&str`
  literals (tests) still work. Pinning to `(&str, &str)` adds `&` noise *and*
  forces a re-allocation of strings the caller already owns.

### Rejected MSBuild knobs (no-emit wiring)

- `-t:CoreCompile` (alone) — broke `ResolveProjectReferences`, downstream csprojs lose refs to upstream. Has to be `-t:Build` with property-level skips.
- `-p:CopyBuildOutputToOutputDirectory=false` — broke ProjectReference resolution (downstream csprojs need the upstream DLL in `bin/`). `ProduceReferenceAssemblyInOutDir=true` is the working substitute.

### Why DIY in Rust over Ninja/Bazel (researched, decided)

- **Ninja** would save ~100 LOC (topo-sort + parallel scheduler) but adds a binary dep; `usg-core` already has lockfile + scan-cache + nanosecond-mtime infra to do UTD itself. Reasonable plan B if a future DAG walker turns out non-trivial.
- **Bazel `rules_dotnet`** is slower on no-op for our scale (200–500 ms startup) and the Roslyn persistent-worker support is only mature in [AFASResearch's fork](https://github.com/AFASResearch/rules_dotnet). Wrong scale.
- **Strong precedent:** Unity's own [Bee](https://aras-p.info/blog/2019/06/21/Replacing-a-live-system-is-really-hard/) is exactly this pattern (custom DAG scheduler driving csc directly, cache in `Library/Bee/`). They chose not to wrap Ninja.
- **"Never write anything besides `.dll` into the typecheck output dir"** — held while typecheck owned `typecheck-<variant>/`; superseded by the obj/Debug consolidation. See [[architecture.md#typecheck-deeper]] (Foreign-writer guard).
