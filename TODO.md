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

**Why DIY in Rust over Ninja/Bazel** (researched separately):
- **Ninja** would save ~100 LOC (topo-sort + parallel scheduler) but adds a binary dep; `usg-core` already has lockfile + scan-cache + nanosecond-mtime infra to do UTD itself. Reasonable plan B if the DAG walker turns out non-trivial.
- **Bazel `rules_dotnet`** is slower on no-op for our scale (200–500ms startup) and the Roslyn persistent-worker support is only mature in [AFASResearch's fork](https://github.com/AFASResearch/rules_dotnet). Wrong scale.
- **Strong precedent:** Unity's own [Bee](https://aras-p.info/blog/2019/06/21/Replacing-a-live-system-is-really-hard/) is exactly this pattern (custom DAG scheduler driving csc directly, cache in `Library/Bee/`). They chose not to wrap Ninja.
- **Risk to avoid up front:** never write anything into the output `.dll`'s directory besides the `.dll` itself, or DIY UTD will misfire the same way MSBuild's `obj/.../CoreCompileInputs.cache` does today.

### `usg-cli typecheck` — landed but not yet replacing `build-unity-sln`

Phase 3 Ckpt 3 of the overhaul shipped the subcommand + module skeleton
(`crates/usg-core/src/typecheck.rs`, ~280 LOC; CLI dispatch wired). It
correctly: reads the lockfile, walks the asmdef DAG, builds csc args,
resolves `$(UnityPath)` / `$(ProjectRoot)`, runs `dotnet exec csc.dll @rsp`
per dirty project, aggregates exit codes, mtime-based UTD short-circuit.

**Doesn't yet replace `build-unity-sln.sh`** because the lockfile contains
several Unity-specific reference shapes that csc rejects:

- **Native DLLs** in `Project` category (e.g.
  `Assets/50_Modules/Tools/TexturePacker/Editor/unity_sprite_author.dll`)
  → csc errors with `CS0009: PE image doesn't contain managed metadata`.
  MSBuild's `ResolveAssemblyReferences` task filters these; we need
  equivalent filtering — either pattern-based (e.g. directories named
  `Editor` next to platform-native binaries) or PE-header inspection
  (read the first ~80 bytes, check for COR header).
- **Cascading failures**: when one project fails to compile (above), all
  downstream projects fail with `CS0006: Metadata file ... could not be
  found` because their `/reference:` to that project's output dll is now
  missing. Need to either skip-downstream-on-upstream-failure or accumulate
  diagnostics differently.
- The `info USG0001: Only allowed to have one file passed in with
  extension '.AdditionalFile.txt'` warning suggests Unity's Roslyn analyzer
  config is being pulled in twice. Investigate whether we should pass
  `/additionalfile:` flags or filter the analyzer set.

Until these land, `build-unity-sln.sh` stays the canonical compile-check
driver. The Hot Reload pre-flight in `meow-tower/justfile:105` continues
calling `build-unity-sln editor`.

### Other rejected/closed during no-emit wiring

- `-t:CoreCompile` (alone) — broke `ResolveProjectReferences`, downstream csprojs lose refs to upstream. Has to be `-t:Build` with property-level skips.
- `-p:CopyBuildOutputToOutputDirectory=false` — broke ProjectReference resolution (downstream csprojs need the upstream DLL in `bin/`). `ProduceReferenceAssemblyInOutDir=true` is the working substitute.
