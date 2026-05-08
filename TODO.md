# TODO

## Deferred

### Direct `csc /shared` typecheck path

Closes the cold-rebuild gap (currently 6.6 s; the retired
`build-unity-sln`'s no-emit mode hit 1.47 s on the same case via
VBCSCompiler `/shared`).

Each `dotnet exec csc.dll` invocation cold-starts Roslyn (~390 ms
JIT + load). Speaking VBCSCompiler IPC over the named pipe lets a
long-lived Roslyn process amortize that cost across N compiles.

**Approach:**
- Implement the [Roslyn compiler server protocol](https://github.com/dotnet/roslyn/blob/main/docs/compilers/Compiler%20Server.md)
  client in Rust — length-prefixed binary frames over a Unix domain
  socket on macOS/Linux (named pipe on Windows). Pipe name is a
  hash of `(compiler path, user, working dir)`.
- Replace `invoke_csc` body with a `BuildClient`-equivalent: try the
  IPC path first, fall back to `dotnet exec csc.dll` on failure.
- `csc.exe` (the native wrapper) supports `/shared` directly but doesn't
  ship on macOS — `csc` here is `dotnet exec csc.dll` which is just a
  shell.

**Why not yet:** ~200 LOC of binary-protocol implementation for a 4 s
cold-rebuild improvement. The warm-path win (10×) already dominates the
common dev case. Revisit when cold rebuild becomes the felt pain.

### Architecture v1 leftovers

- **Fold `usg-cli` package into `usg-core` as `[[bin]]`** — cargo
  companion-bin idiom; mechanical, no caller impact. Skipped during the
  overhaul because the binary path is unaffected and the rename is pure
  churn.

### Other rejected/closed during no-emit wiring

- `-t:CoreCompile` (alone) — broke `ResolveProjectReferences`, downstream csprojs lose refs to upstream. Has to be `-t:Build` with property-level skips.
- `-p:CopyBuildOutputToOutputDirectory=false` — broke ProjectReference resolution (downstream csprojs need the upstream DLL in `bin/`). `ProduceReferenceAssemblyInOutDir=true` is the working substitute.

### Why DIY in Rust over Ninja/Bazel (researched, decided)

- **Ninja** would save ~100 LOC (topo-sort + parallel scheduler) but adds a binary dep; `usg-core` already has lockfile + scan-cache + nanosecond-mtime infra to do UTD itself. Reasonable plan B if a future DAG walker turns out non-trivial.
- **Bazel `rules_dotnet`** is slower on no-op for our scale (200–500 ms startup) and the Roslyn persistent-worker support is only mature in [AFASResearch's fork](https://github.com/AFASResearch/rules_dotnet). Wrong scale.
- **Strong precedent:** Unity's own [Bee](https://aras-p.info/blog/2019/06/21/Replacing-a-live-system-is-really-hard/) is exactly this pattern (custom DAG scheduler driving csc directly, cache in `Library/Bee/`). They chose not to wrap Ninja.
- **Risk to avoid:** never write anything into the typecheck output dir besides each project's `.dll`, or DIY UTD will misfire the same way MSBuild's `obj/.../CoreCompileInputs.cache` does today.
