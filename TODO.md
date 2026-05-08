# TODO

## Deferred / future work

### Direct `csc /shared` typecheck path

Bypass MSBuild entirely on the warm path for an even lower floor (~50–150 ms vs current default no-emit mode at ~460 ms warm no-op on meow-tower).

**Why the floor exists:** MSBuild's up-to-date check fails for our setup — `obj/Debug/<proj>.csproj.CoreCompileInputs.cache` is rewritten on every invocation with the same-second mtime as the `.dll`, so MSBuild re-invokes `csc` on all 9 projects every time. PerformanceSummary on a warm meow-tower run shows `Csc 9 calls 1168 ms` cumulative ≈ ~500 ms wall-clock with parallelism. Reproducing fix-the-cache approaches inside MSBuild is fragile; bypassing MSBuild altogether is the cleaner path.

**Approach:**
1. Have `usg-cli` emit a `.rsp` per asmdef alongside each `.csproj` — contains all refs, defines, source globs in csc command-line form.
2. New `usg-cli typecheck` subcommand walks the asmdef dependency graph in topological order, invalidating per-project on `.cs` mtime change, and runs `csc @<proj>.rsp /shared /noconfig`. `/shared` connects to VBCSCompiler (already warmed by `UseSharedCompilation=true` in regular builds).
3. Wire as a new mode in `build-unity-sln.sh` once stable (sibling to `--emit`).

**Refs:**
- [Roslyn Compiler Server](https://github.com/dotnet/roslyn/blob/main/docs/compilers/Compiler%20Server.md)
- Capture a working `.rsp` once via `dotnet msbuild -t:CoreCompile -p:SkipCompilerExecution=true -p:ProvideCommandLineArgs=true -bl` to see the exact csc args MSBuild would generate.

**Why not now:** bigger change (new subcommand + dep-graph walking in Rust); the current default (no-emit) already wins 2× over `--emit` and unblocks the meow-tower Hot-Reload pre-flight workflow.

### Other rejected/closed during no-emit wiring

- `-t:CoreCompile` (alone) — broke `ResolveProjectReferences`, downstream csprojs lose refs to upstream. Has to be `-t:Build` with property-level skips.
- `-p:CopyBuildOutputToOutputDirectory=false` — broke ProjectReference resolution (downstream csprojs need the upstream DLL in `bin/`). `ProduceReferenceAssemblyInOutDir=true` is the working substitute.
