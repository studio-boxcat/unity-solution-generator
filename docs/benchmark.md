# Benchmark & Profiling

> **Related:** [[CLAUDE.md]]

Two layers of measurement: end-to-end wall-clock (`hyperfine`) and statistical microbenchmarks (`criterion`).

## End-to-end (meow-tower, 13 assemblies, ~5k .cs files)

`hyperfine --warmup 10 --runs 200 --shell=none`:

| Command | Mean ± σ | Range |
|---------|----------|-------|
| `generate` (warm — fingerprint hit) | **2.1 ± 0.5 ms** | 1.6–5.6 |
| `generate` (warm scan-cache, fingerprint missing) | ~5.6 ± 1.0 ms | 4.2–9.8 |
| `typecheck` (warm no-op, refreshes .csproj/.sln + diagnostics) | **36.6 ± 0.7 ms** | 35.3–38.2 |
| `lock` (cold, fingerprint nuked each run) | **59.5 ± 3.4 ms** | 55.9–63.6 |
| `lock` (warm — fingerprint hit) | **1.8 ± 0.2 ms** | 1.6–3.1 |
| startup (`--help`) | ~2 ms | — |

Run via `just profile` (cold lock + warm lock) or `just profile-spans` (per-section breakdown via tracing).

## Per-section (one run, USG_PROFILE=1)

`generate` (warm scan-cache, **fingerprint cleared** so we hit the full pipeline):
```
generate (5.78 ms total)
├─ project_scanner.scan         4.66 ms
│  └─ scan_cache.validate       2.82 ms
└─ generate.write_variant n=9   0.64 ms

lock cold (~97 ms total, with PackageCache-gap fallback firing on 13 packages)
├─ lockfile_scanner.unity_install   1.5 ms     ← walkdir, sequential, small
├─ lockfile_scanner.project_walk   92  ms     ← parallel walk + per-package fallback
└─ lockfile_scanner.defines         3 ms

lock warm
└─ (no spans — fingerprint match short-circuits before LockfileScanner runs)
```

The `project_walk` budget covers the original Assets/Packages/PackageCache walk plus per-missing-package walks of `BuiltInPackages/<name>` or `~/.cache/.../<name>`. The fallback runs only for entries `packages-lock.json` names but PackageCache hasn't resolved — empty in the steady state, so this is the worst-case cold path. Once PackageCache is fully populated the cold lock returns to ~30 ms.

`generate` (warm scan-cache + warm fingerprint):
```
generate.from_lockfile  83 µs
└─ generate.fingerprint_check   57 µs   ← stat scan-cache + lockfile + read fp
```
The remaining ~2 ms of wall-clock is process startup + dynamic linker.

## Microbenchmarks (criterion, synthetic projects)

```
project_scanner.scan/13asm_x_50cs       33.2 ms     (cold, full walk)
project_scanner.scan/100asm_x_200cs    946.6 ms     (cold, full walk)
project_scanner.scan_warm/13asm_x_50cs   0.22 ms    (cache hit, ~150× speedup)
project_scanner.scan_warm/100asm_x_200cs 1.59 ms

generate.from_lockfile/13asm_x_50cs      0.51 ms
generate.from_lockfile/100asm_x_200cs    2.81 ms

lockfile_io/write_initial               13.5 µs
lockfile_io/read                        14.1 µs
```

Run via `just bench` (all) or `just bench scan` (filter).

## `typecheck` vs the previous MSBuild driver

`unity-solution-generator typecheck` invokes `csc.dll` directly per asmdef, with mtime-based UTD short-circuit. No MSBuild involved. The previous driver (`build-unity-sln.sh`) wrapped `dotnet msbuild` with a stack of RAR-optimization flags + MSBuild Server + ref-only-assembly tricks; it was retired once `typecheck` cleared the Unity-quirk filtering bar.

Benchmarks on meow-tower (13 asm, ~5k .cs):

| Scenario | `build-unity-sln` (no-emit) | `usg typecheck` | Δ |
|---|---|---|---|
| Warm no-op | 460 ms | **38 ms** | **12.1× faster** |
| Touch + rebuild | 2.22 s | **519 ms** | **4.3× faster** |
| Cold rebuild | 1.47 s | **1.68 s** | ~14 % slower (effective parity) |

Reproduce: `hyperfine 'unity-solution-generator typecheck'` from anywhere inside a Unity project.

Four layers contribute:

- **Warm no-op (12.1× win)**: mtime-based UTD short-circuit skips csc entirely.
- **Content-hash UTD**: csc with `/deterministic` produces byte-identical output for unchanged inputs, so when a touched `.cs` upstream produces an identical `.dll`, the pre-compile mtime is restored and downstream UTD skips. Only the project whose source actually changed recompiles. (`/refonly` was dropped — it suppresses csc body diagnostics on .NET 8 SDK csc 4.10. See [[architecture.md#typecheck-deeper]].)
- **`/shared`**: each csc invocation connects to VBCSCompiler over a long-lived pipe — no Roslyn JIT cold-start per call.
- **Parallel level dispatch**: the asmdef DAG is grouped into levels; within a level all projects are independent and run concurrently via `rayon::par_iter`, each spawning its own `dotnet exec csc.dll /shared`. VBCSCompiler accepts concurrent requests over its named pipe.

Cold rebuild now lands at the same order of magnitude as the retired MSBuild driver (which had its own parallel csc dispatch via MSBuild Server's task graph).

### Why MSBuild's warm-no-op floor is 460 ms

For posterity: MSBuild's up-to-date check failed for our setup — `obj/Debug/<proj>.csproj.CoreCompileInputs.cache` was rewritten on every invocation with the same-second mtime as the `.dll`, so MSBuild re-invoked `csc` on all 9 projects every time. PerformanceSummary on a warm meow-tower run showed `Csc 9 calls 1168 ms` cumulative ≈ ~500 ms wall-clock with parallelism. The MSBuild Server, RAR optimizations, and analyzer-skip flags all contributed, but the floor was csc itself running unconditionally. Bypassing MSBuild was the only way to drop below it.

## Caching layers

| Cache | Path | Invalidates on | Hot-path skip |
|---|---|---|---|
| `generate-fingerprint` | `Library/UnitySolutionGenerator/.fingerprints/<options-hash>` | mtime of `csproj.lock` or `scan-cache`; or any expected output file missing | entire `generate_from_lockfile` body — render+write skipped, cached `GenerateResult` returned |
| `scan-cache` | `Library/UnitySolutionGenerator/scan-cache` | mtime of any contributing dir + each asmdef/asmref file (catches in-place edits — parent-dir mtime alone misses these) | full filesystem walk + per-asmdef JSON parse (records are pre-serialized into the cache) |
| `lock-fingerprint` | `Library/UnitySolutionGenerator/lock-fingerprint` | mtime of Unity install + any contributing dir + ProjectVersion / ProjectSettings / manifest.json + extracted-tarball cache root; missing paths recorded as `(p, 0)` so first appearance also invalidates | entire Unity-install + project-side DLL/asmdef walk |

Both caches store nanosecond mtimes via `MetadataExt::mtime_nsec`. Validation cost is `len(entries) × stat()` (~1–2 ms for hundreds of entries).

## Concurrency

- The hot project-side scan uses [`ignore::WalkBuilder::build_parallel`](https://docs.rs/ignore/) with all gitignore behaviour disabled — we want only the parallel walker scaffolding (crossbeam-deque work-stealing + per-thread accumulators flushed on `Drop`).
- The lockfile-side DLL/asmdef walk over `Assets`/`Packages`/`Library/PackageCache` also uses `ignore::WalkBuilder::build_parallel`. The Unity-install DLL scan stays sequential (`walkdir`) — small enough not to matter.
- `csproj` writes fan out across threads via `rayon`.

## Profiling instrumentation

Spans use [`tracing`](https://docs.rs/tracing/). Default off — zero runtime cost. Opt in:
- `USG_PROFILE=1 unity-solution-generator <cmd>` — info-level spans, one stderr line per span close with `time.busy`.
- `USG_PROFILE=full` — includes lower-level child spans.
- `USG_LOG=unity_solution_generator::project_scanner=debug` — drop-in `EnvFilter` directives override the default.
