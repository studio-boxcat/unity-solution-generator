# Benchmark & Profiling

> **Related:** [[CLAUDE.md]], [[architecture.md]], [[library-api.md]], [[TODO.md]]

Two layers of measurement: end-to-end wall-clock (`hyperfine`) and statistical microbenchmarks (`criterion`).

## End-to-end (meow-tower, 9 assemblies, ~5k .cs files)

Hyperfine, `--warmup 5 --runs 50`:

| Version | Architecture | Warm `typecheck` |
|---|---|---|
| Pre-overhaul baseline | 3 mtime-fingerprint caches | 36.6 ± 0.7 ms |
| v0.3.0 (post-Watchman strip) | Watchman-only, no caches | 875 ms |
| v0.4.0 (perf passes) | 5 caches, 2-tier invalidation, sidecars | 31.9 ± 0.7 ms |
| v0.5.0 | 1 scan-cache, mtime-only fingerprint | 35.8 ± 0.9 ms |
| **v0.6.0 (current)** | **bincode scan-cache + pruned fingerprint + free-fn API** | **~35 ms** |

v0.6.0 keeps the v0.5.0 single-cache model but drops the text codec (~150 LOC) and the redundant `manifest.json` / `packages-lock.json` / `ProjectVersion.txt` mtime entries. The csc-dll-path sidecar moved to the per-user cache, removing it from the project tree entirely.

Run via `just profile` for warm/cold breakdown, `just profile-spans` for per-section tracing.

## Per-section (one run, USG_PROFILE=1)

Cache-miss (`scan-cache.bin` deleted, full re-derive):
```
typecheck (~100 ms total)
├─ lockfile_scanner.scan                ~15 ms   Unity install walkdir + Watchman enumerate
├─ project_scanner.scan_uncached        ~10 ms   asmdef JSON parse via rayon
└─ typecheck.run                        ~70 ms   csc UTD checks
```

Warm cache-hit:
```
typecheck (~36 ms wall-clock)
├─ scan_cache_fingerprint_matches       ~1 ms    ~30 stat calls
├─ lockfile::scan_and_write             <1 ms    read csproj.lock + unity-version stringy check
└─ typecheck.run                        ~30 ms   csc UTD checks (stat sources + refs per asmdef)
```

Process startup overhead (`--help`) baseline is ~2 ms. The remaining time on warm path is the typecheck UTD checks themselves (per-project stat fan-out), which is irreducible without changing the typecheck contract.

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

## Caching layers (v0.6.0)

Two on-disk artifacts. One invalidation invariant.

| Cache | Path | Invalidates on | Hot-path skip |
|---|---|---|---|
| `scan-cache.bin` | `Library/UnitySolutionGenerator/scan-cache.bin` | Any of ~30 fingerprinted mtimes (asmdef/asmref files + parent dirs + top-level project dirs) | Full Watchman enumeration + per-asmdef JSON parse + Unity install rescan + lockfile rewrite |
| `csc-dll-path` | `<host-cache>/unity-solution-generator/<unity-version>/csc-dll-path` | Cached path no longer exists (SDK uninstall) | `dotnet --list-sdks` subprocess (~60 ms) |

Lockfile (`csproj.lock`) reuses the `scan-cache.bin` fingerprint as its invalidation signal — `lockfile::scan_and_write` validates by `unity-version` equality AND `scan_cache_fingerprint_matches`. When both hold, the existing lockfile is reused without a rescan. No separate Watchman query for the lockfile.

Hot-path cache validation: ~30 `stat()` calls (~1–2 ms total).

## Concurrency

- Project-side enumeration on cache miss: one Watchman `enumerate()` query returns the full project-relative path list in one round trip.
- The Unity-install scan (`Managed/`, `NetStandard/`, per-`PlaybackEngines/<P>`) runs sequentially via `walkdir` on cache miss only — small enough not to matter and runs once per editor version.
- Per-asmdef JSON parsing fans out across cores via `rayon::par_iter`.
- `csproj` writes fan out across threads via `rayon`.

## Profiling instrumentation

Spans use [`tracing`](https://docs.rs/tracing/). Default off — zero runtime cost. Opt in:
- `USG_PROFILE=1 unity-solution-generator <cmd>` — info-level spans, one stderr line per span close with `time.busy`.
- `USG_PROFILE=full` — includes lower-level child spans.
- `USG_LOG=unity_solution_generator::project_scanner=debug` — drop-in `EnvFilter` directives override the default.
