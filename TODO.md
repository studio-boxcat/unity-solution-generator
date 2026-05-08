# TODO

## Deferred

### Other rejected/closed during no-emit wiring

- `-t:CoreCompile` (alone) — broke `ResolveProjectReferences`, downstream csprojs lose refs to upstream. Has to be `-t:Build` with property-level skips.
- `-p:CopyBuildOutputToOutputDirectory=false` — broke ProjectReference resolution (downstream csprojs need the upstream DLL in `bin/`). `ProduceReferenceAssemblyInOutDir=true` is the working substitute.

### Why DIY in Rust over Ninja/Bazel (researched, decided)

- **Ninja** would save ~100 LOC (topo-sort + parallel scheduler) but adds a binary dep; `usg-core` already has lockfile + scan-cache + nanosecond-mtime infra to do UTD itself. Reasonable plan B if a future DAG walker turns out non-trivial.
- **Bazel `rules_dotnet`** is slower on no-op for our scale (200–500 ms startup) and the Roslyn persistent-worker support is only mature in [AFASResearch's fork](https://github.com/AFASResearch/rules_dotnet). Wrong scale.
- **Strong precedent:** Unity's own [Bee](https://aras-p.info/blog/2019/06/21/Replacing-a-live-system-is-really-hard/) is exactly this pattern (custom DAG scheduler driving csc directly, cache in `Library/Bee/`). They chose not to wrap Ninja.
- **Risk to avoid:** never write anything into the typecheck output dir besides each project's `.dll`, or DIY UTD will misfire the same way MSBuild's `obj/.../CoreCompileInputs.cache` does today.
