# Benchmark & Profiling

> **Related:** [[CLAUDE.md]]

Two layers of measurement: end-to-end wall-clock (`hyperfine`) and statistical microbenchmarks (`criterion`).

## End-to-end (meow-tower, 13 assemblies, ~5k .cs files)

`hyperfine --warmup 10 --runs 200 --shell=none`:

| Command | Mean ± σ | Range |
|---------|----------|-------|
| `generate` (warm — fingerprint hit) | **2.1 ± 0.5 ms** | 1.6–5.6 |
| `generate` (warm scan-cache, fingerprint missing) | ~5.6 ± 1.0 ms | 4.2–9.8 |
| `generate` (output to project root via Rust API) | **5.5 ± 0.5 ms** | 4.9–7.9 |
| `lock` (cold, fingerprint nuked each run) | **29.8 ± 2.4 ms** | 26.1–33.7 |
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

lock cold (47.1 ms total)
├─ lockfile_scanner.unity_install   0.90 ms     ← walkdir, sequential, small
├─ lockfile_scanner.project_walk   45.1  ms     ← ignore parallel walk of PackageCache
└─ lockfile_scanner.defines         0.71 ms

lock warm
└─ (no spans — fingerprint match short-circuits before LockfileScanner runs)
```

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
| Warm no-op | 460 ms | **42 ms** | **10.7× faster** |
| Touch + rebuild | 2.22 s | 2.99 s | 1.35× slower |
| Cold rebuild | 1.47 s | 6.6 s | ~4.5× slower |

Reproduce: `hyperfine 'unity-solution-generator typecheck . ios editor'` from a Unity project root.

The headline win is the warm-no-op path — what Hot Reload pre-flight hits constantly. Touch+rebuild and cold rebuild are slower because:

- **Touch+rebuild cascades**: csc with `/refonly /deterministic` produces identical bytes for unchanged inputs, but the `.dll` mtime advances on each compile, so a touched `.cs` in upstream X cascades into rebuilds of all downstream projects. Fix queued in [[TODO.md]] (content-hash UTD).
- **Cold rebuild**: each `dotnet exec csc.dll` cold-starts Roslyn (~390 ms JIT + load × 9 projects). Fix queued in [[TODO.md]] (VBCSCompiler `/shared` IPC).

### Why MSBuild's warm-no-op floor is 460 ms

For posterity: MSBuild's up-to-date check failed for our setup — `obj/Debug/<proj>.csproj.CoreCompileInputs.cache` was rewritten on every invocation with the same-second mtime as the `.dll`, so MSBuild re-invoked `csc` on all 9 projects every time. PerformanceSummary on a warm meow-tower run showed `Csc 9 calls 1168 ms` cumulative ≈ ~500 ms wall-clock with parallelism. The MSBuild Server, RAR optimizations, and analyzer-skip flags all contributed, but the floor was csc itself running unconditionally. Bypassing MSBuild was the only way to drop below it.

## Caching layers

| Cache | Path | Invalidates on | Hot-path skip |
|---|---|---|---|
| `generate-fingerprint` | `Library/UnitySolutionGenerator/.fingerprints/<options-hash>` | mtime of `csproj.lock` or `scan-cache`; or any expected output file missing | entire `generate_from_lockfile` body — render+write skipped, cached `GenerateResult` returned |
| `scan-cache` | `Library/UnitySolutionGenerator/scan-cache` | mtime of any contributing dir + each asmdef/asmref file (catches in-place edits — parent-dir mtime alone misses these) | full filesystem walk + per-asmdef JSON parse (records are pre-serialized into the cache) |
| `lock-fingerprint` | `Library/UnitySolutionGenerator/lock-fingerprint` | mtime of Unity install + any contributing dir + ProjectVersion / ProjectSettings / manifest.json | entire Unity-install + project-side DLL/asmdef walk |

Both caches store nanosecond mtimes via `MetadataExt::mtime_nsec`. Validation cost is `len(entries) × stat()` (~1–2 ms for hundreds of entries).

## Concurrency

- The hot project-side scan uses [`ignore::WalkBuilder::build_parallel`](https://docs.rs/ignore/) with all gitignore behaviour disabled — we want only the parallel walker scaffolding (crossbeam-deque work-stealing + per-thread accumulators flushed on `Drop`).
- The lockfile-side DLL/asmdef walk over `Assets`/`Packages`/`Library/PackageCache` also uses `ignore::WalkBuilder::build_parallel`. The Unity-install DLL scan stays sequential (`walkdir`) — small enough not to matter.
- `csproj` writes fan out across threads via `rayon`.

## Profiling instrumentation

Spans use [`tracing`](https://docs.rs/tracing/). Default off — zero runtime cost. Opt in:
- `USG_PROFILE=1 unity-solution-generator <cmd>` — info-level spans, one stderr line per span close with `time.busy`.
- `USG_PROFILE=full` — includes lower-level child spans.
- `USG_LOG=usg_core::project_scanner=debug` — drop-in `EnvFilter` directives override the default.
