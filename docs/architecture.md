# Architecture

> **Related:** [[CLAUDE.md]], [[benchmark.md]], [[library.md]], [[TODO.md]]

What the codebase looks like after the Phase-3 overhaul. Borrows ideas from the literature, scaled to a 3.6 k-LOC tool with four caller sites — none of those four are CI, none are external repos beyond `meow-tower` + `meow-tower-porting`.

## Background

Ported from Swift, the codebase had accreted a few localized smells: hand-rolled JSON parser that silently truncated on edge cases, three uncoordinated cache version constants, parallel-walk Flusher boilerplate copy-pasted between `project_scanner` and `lockfile_scanner`. The public surface also exported flags and FFI functions no consumer used (`init`, `--output`, `--verbose`, `usg_lock`, FFI buffer args).

The overhaul trimmed the public surface to the four real consumers, deduped the internal accretion, and added a `typecheck` subcommand that bypasses MSBuild for compile-check workflows. The shell driver (`build-unity-sln.sh`) was retired once `typecheck` cleared the Unity-quirk filtering bar (native-DLL filter, cascading-failure handling). See [[TODO.md]] for follow-ups (content-hash UTD, `/shared` IPC).

## Prior art (what we borrowed)

- **`com.unity.ide.rider`** ([needle-mirror](https://github.com/needle-mirror/com.unity.ide.rider)) — flat library shape, single `SyncSolution` entry point. Confirms our shape.
- **Bee** ([Unity blog](https://blog.unity.com/engine-platform/accelerating-player-builds-with-incremental-build-pipeline)) — separates *describe graph* (pure data) from *execute graph* (workers/scheduling). At 13 asmdefs we kept this discipline as just `TypecheckOptions` + `run`, not two struct types.
- **Cargo** ([Cargo Targets](https://doc.rust-lang.org/cargo/reference/cargo-targets.html), [RFC 3477](https://rust-lang.github.io/rfcs/3477-cargo-check-lang-policy.html)) — single binary with subcommands; `cargo check` was a subcommand from day one. Justified retiring `build-unity-sln.sh` once typecheck shipped.
- **Roslyn Compiler Server** ([Compiler Server.md](https://github.com/dotnet/roslyn/blob/main/docs/compilers/Compiler%20Server.md)) — VBCSCompiler IPC. We use `dotnet exec csc.dll` per project (no `/shared`); direct IPC deferred to [[TODO.md]].
- **Cargo + Bazel** ([many caches of Bazel](https://blog.engflow.com/2024/05/13/the-many-caches-of-bazel/)) — wholesale cache invalidation via single version constant. No migrations.

## Use cases

Audit found exactly **4 caller sites** total. The redesign is sized for these and nothing else.

| Consumer | Channel | Surface needed |
|---|---|---|
| `meow-tower` Hot Reload pre-flight (`justfile:105`) | CLI | `unity-solution-generator typecheck . ios editor` exit code |
| `meow-tower-porting` (same recipe) | CLI | same |
| Rider in-Editor regen (`ProjectGeneration.cs:91`) | FFI | `usg_generate(root, "ios", "editor", ".", extraRefs)` + `usg_last_error()` |
| Rider in-Editor regen (porting) | FFI | same |

No CI, no other repos, no other tools.

## Layout

```
crates/
  usg-core/                 lib (separate `usg-cli` package owns the binary)
    Cargo.toml              [lib]
    src/
      lib.rs                pub API + LOCKFILE_VERSION + CACHE_VERSION constants
      lockfile.rs           Lockfile, DllRef, RefCategory, LockfileIO
      project_scanner.rs    project-side scan; AsmDefRecord, ProjectCategory
      lockfile_scanner.rs   Unity-install + project DLL/asmdef scan
      solution_generator.rs render + write csproj/sln/Directory.Build.props
      typecheck.rs          DAG walk + csc invocations  (NEW, partial)
      walk.rs               ONE shared parallel-walk helper (NEW)
      lock_cache.rs         lock-fingerprint cache; reads CACHE_VERSION
      generate_cache.rs     generate-fingerprint cache; reads CACHE_VERSION
      defines.rs            version + scripting defines
      paths.rs              path utilities
      io.rs                 read/write helpers + version-header validator
      profile.rs            tracing macros
      xml.rs                escape + deterministic GUID (pinned invariant)
      error.rs              GeneratorError + LockfileError + io_err helper
  usg-cli/
    Cargo.toml              [[bin]] unity-solution-generator
    src/main.rs             arg parse + subcommand dispatch
    tests/cli_regression.rs CLI surface pinning
  usg-ffi/
    Cargo.toml              [lib] cdylib + rlib (`UnitySolutionGenerator`)
    build.rs                installs @rpath/<dylib> macOS install_name
    src/lib.rs              C ABI: usg_generate + usg_last_error
    tests/abi_smoke.rs      FFI signature pinning (post-trim)
```

`json.rs` deleted — replaced by `serde_json::Value` extraction at each call site.

`usg-cli` was NOT folded into `usg-core` as a `[[bin]]` target (architecture v1 proposed it; left for a future cleanup since the binary path is unaffected and the rename is mechanical churn that could break shell-script callers if anything goes wrong).

## Public API

### CLI (binary `unity-solution-generator`)

| Subcommand | Args | Status |
|---|---|---|
| `lock` | `<root>` | unchanged |
| `generate` | `<root> <platform> <config> [--extra-refs <paths>]` | trimmed (no `--output`/`--root`/`-v`) |
| `typecheck` | `<root> <platform> <config> [--extra-refs <paths>]` | NEW, partial — see [[TODO.md]] |

Dropped: `init` (deprecated alias), `--output`/`--root`, `-v`/`--verbose`.

### FFI (cdylib)

```c
int32_t usg_generate(const char *projectRoot, const char *platform,
                     const char *config, const char *outputDir,
                     const char *extraRefs,
                     char *slnPathOut, int32_t slnPathOutLen);
const char *usg_last_error(void);
```

Dropped: `usg_lock` (no caller). The `slnPathOut` / `slnPathOutLen` args were *kept* — although Rider passes `IntPtr.Zero, 0` and never reads the path, the buffer machinery is non-trivial to retire safely (signature changes ripple through the dylib + C# DllImport in lockstep) and the negative-buffer-len sign-extend hazard is structurally guarded.

**Single-threaded contract.** Cache files aren't reentrant-safe. Rider naturally serializes via Unity's main asset-import thread.

## Versioning: two constants

```rust
// lib.rs
pub const LOCKFILE_VERSION: u32 = 1;     // user-visible csproj.lock — bump rarely
pub const CACHE_VERSION: u32 = 1;        // dev-local caches — bump freely
```

| Artifact | Constant | Notes |
|---|---|---|
| `csproj.lock` | `LOCKFILE_VERSION` | may be checked in — format change is a real migration concern |
| `scan-cache` | `CACHE_VERSION` | dev-local, gitignored under `Library/` |
| `lock-fingerprint` | `CACHE_VERSION` | dev-local |
| `.fingerprints/<hash>` | `CACHE_VERSION` | dev-local |
| `typecheck-<variant>/<proj>.dll` | (output) | dev-local; mtime-based UTD |

Cache reload reads the version header; mismatch → cold rebuild. No migration code path (Cargo / Bazel idiom).

## Typecheck subsystem (partial)

Lives in `crates/usg-core/src/typecheck.rs`. Single `run(opts) -> Result<TypecheckResult>` function. Steps:

1. Load lockfile (auto-running `lock` if missing).
2. Project scan (reuses existing scan-cache).
3. Compute included projects via the same rules as `solution_generator` (config + platform filter).
4. Topo-sort by asmdef references.
5. Per project: build csc args, mtime-check inputs vs cached `.dll`, skip if up-to-date, otherwise `dotnet exec csc.dll @rsp.txt`.
6. Resolve `$(UnityPath)` and `$(ProjectRoot)` in lockfile paths (MSBuild does this at eval time; csc doesn't).

**MVP intentionally omits**:
- Native-DLL filtering (CS0009 errors when lockfile has e.g. `unity_sprite_author.dll`).
- VBCSCompiler IPC (`/shared` flag) — `dotnet exec csc.dll` is ~390 ms per cold call but the headline win is the warm-no-op fingerprint short-circuit, not per-call speed.
- Analyzer config dedup (Roslyn USG0001 info-message).

All three landed; `build-unity-sln.sh` retired. Remaining typecheck follow-ups (content-hash UTD, `/shared` IPC) tracked in [[TODO.md]].

## Pitfalls (avoided by design)

- **Plugin/registry** — one consumer; no abstraction.
- **Option-bag struct creep** — `GenerateOptions` is 5 fields. Splits before adding a 6th.
- **Persistent worker / daemon** — at 13 asmdefs, JIT amortization doesn't justify the protocol burden.
- **Cache version coordination drift** — single `CACHE_VERSION` invalidates all 3 caches together.
- **Hand-rolled JSON silently mistruncating** — replaced with `serde_json`.
- **Shell driver state accretion** — `build-unity-sln.sh`'s retry-on-failure logic was a smell-on-its-way-to-bug; typecheck inherits the simpler "lock auto-runs on demand" model with no retry needed.

## Non-goals

- General "build any C# solution" tool — Unity-specific assumptions are load-bearing.
- Persistent worker / daemon process.
- Multi-platform build matrix in one CLI invocation (caller's loop).
- Replacement for `dotnet build` in `--emit` mode.
- Content-hash UTD for `.cs` files (mtime is sufficient at our scale).

## Phase 3 history

Six commits across six checkpoints (plus a Phase-5 rollback for FFI):

| Commit | Checkpoint | What |
|---|---|---|
| `d4719c1` | 0 | Regression tests + architecture draft (12 new tests across `regression.rs` + `cli_regression.rs`) |
| `b771649` | 1a | Three cache-version constants → one `CACHE_VERSION` |
| `7fbf2b6` | 1b | `json.rs` (156 LOC) → `serde_json` (-180 LOC net) |
| `7e1df09` | 1c | `walk.rs` extraction (kills the duplicated Flusher) |
| `161dbbc` | 2 | Trim CLI surface (`init`/`--output`/`--root`/`-v`/`usg_lock` FFI). FFI `slnPathOut` args originally trimmed too but later restored. |
| `8f71487` | 3 | `typecheck` subcommand (partial) |
| _(post)_ | 5 | Phase 5 review fixes: MSRV (`is_none_or` → `map_or`), doc cross-links, FFI signature restore, redeploy. |

## References

See inline links above.
