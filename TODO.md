# TODO

## Deferred

### Pipeline csc invocations across VBCSCompiler

Cold rebuild is now 4.2 s (down from 6.6 s after `/shared` landed) but
still slower than the retired `build-unity-sln` (1.47 s). The remaining
gap is sequential dispatch — we walk the asmdef DAG and run one
`dotnet exec csc.dll /shared` at a time. MSBuild Server pipelines its
csc calls in parallel through VBCSCompiler.

**Approach:**
- Walk the DAG into "levels" (independent projects per level).
- Per level, spawn N `dotnet exec csc.dll /shared` processes concurrently
  (`rayon::par_iter` or `tokio` task per project).
- Each connects to VBCSCompiler independently — the server already handles
  concurrent requests over its named pipe.

**Why not yet:** the warm + touch+rebuild paths (the dev iteration loop)
are already faster than the retired driver. Cold rebuild is rare (Unity
upgrade, fresh checkout). Revisit if it becomes the felt pain.

### Other rejected/closed during no-emit wiring

- `-t:CoreCompile` (alone) — broke `ResolveProjectReferences`, downstream csprojs lose refs to upstream. Has to be `-t:Build` with property-level skips.
- `-p:CopyBuildOutputToOutputDirectory=false` — broke ProjectReference resolution (downstream csprojs need the upstream DLL in `bin/`). `ProduceReferenceAssemblyInOutDir=true` is the working substitute.

### Why DIY in Rust over Ninja/Bazel (researched, decided)

- **Ninja** would save ~100 LOC (topo-sort + parallel scheduler) but adds a binary dep; `usg-core` already has lockfile + scan-cache + nanosecond-mtime infra to do UTD itself. Reasonable plan B if a future DAG walker turns out non-trivial.
- **Bazel `rules_dotnet`** is slower on no-op for our scale (200–500 ms startup) and the Roslyn persistent-worker support is only mature in [AFASResearch's fork](https://github.com/AFASResearch/rules_dotnet). Wrong scale.
- **Strong precedent:** Unity's own [Bee](https://aras-p.info/blog/2019/06/21/Replacing-a-live-system-is-really-hard/) is exactly this pattern (custom DAG scheduler driving csc directly, cache in `Library/Bee/`). They chose not to wrap Ninja.
- **Risk to avoid:** never write anything into the typecheck output dir besides each project's `.dll`, or DIY UTD will misfire the same way MSBuild's `obj/.../CoreCompileInputs.cache` does today.
