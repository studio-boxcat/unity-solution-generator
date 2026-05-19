# TODO

## Deferred

### `build_rsp` argument count

`typecheck.rs:build_rsp` takes 8 args (clippy `too_many_arguments`). Pre-existing; the test-only re-export emerged but didn't introduce the smell. Pack into a `BuildRspInputs` struct if a 9th arg shows up.

### `walk_files` clippy `while_let_on_iterator`

`lockfile_scanner.rs:329`. Pre-existing; the loop walks `WalkDir` entries which is naturally a `while let`. Cosmetic, no behaviour change.

### `generate --root` hyperfine benchmark fails

`just profile` calls `generate . ios editor --root` which currently exits non-zero on first warmup (pre-existing — the `--root` flag may have been renamed/removed). Recipe needs an audit; for now hyperfine just skips that row.

### `run_build` double-parses `<root> <platform> <config>`

`main.rs:99-108` parses args once for `print_invocation_banner`, then `generate_sln` re-parses inside itself. Clean fix (push the parsed tuple down) needs a private `generate_sln_with_opts` shape or a callback — both worse than the duplicate parse at current scale. Three trivial string parses, no risk of drift while defaults stay shared via `parse_variant_args`. Revisit if a third banner-needing caller appears.

### UTD source/ref-mtime branches lack direct tests

`tests/typecheck_paths.rs` covers stamp/disk-mtime branches of `is_up_to_date` and the post-cascade `max_input_mtime` floor, but doesn't directly exercise "source newer than out_dll → recompile" or "DllRef newer than out_dll → recompile" in isolation. Two ~6-line tests would lock these in. Today they're covered transitively via the cascade-floor test.

### `Display` for `BuildPlatform`/`BuildConfig`

`main.rs:303` calls `.raw()` to format the strong types in the banner. A `Display` impl on each would let logs/formats use `{platform}` directly without the lossy `.raw()` hop. Trivial; no caller harm today.

### Other rejected/closed during no-emit wiring

- `-t:CoreCompile` (alone) — broke `ResolveProjectReferences`, downstream csprojs lose refs to upstream. Has to be `-t:Build` with property-level skips.
- `-p:CopyBuildOutputToOutputDirectory=false` — broke ProjectReference resolution (downstream csprojs need the upstream DLL in `bin/`). `ProduceReferenceAssemblyInOutDir=true` is the working substitute.

### Why DIY in Rust over Ninja/Bazel (researched, decided)

- **Ninja** would save ~100 LOC (topo-sort + parallel scheduler) but adds a binary dep; `usg-core` already has lockfile + scan-cache + nanosecond-mtime infra to do UTD itself. Reasonable plan B if a future DAG walker turns out non-trivial.
- **Bazel `rules_dotnet`** is slower on no-op for our scale (200–500 ms startup) and the Roslyn persistent-worker support is only mature in [AFASResearch's fork](https://github.com/AFASResearch/rules_dotnet). Wrong scale.
- **Strong precedent:** Unity's own [Bee](https://aras-p.info/blog/2019/06/21/Replacing-a-live-system-is-really-hard/) is exactly this pattern (custom DAG scheduler driving csc directly, cache in `Library/Bee/`). They chose not to wrap Ninja.
- **"Never write anything besides `.dll` into the typecheck output dir"** — held while typecheck owned `typecheck-<variant>/`; superseded by the obj/Debug consolidation. See `[[architecture.md#typecheck-deeper]]` (Foreign-writer guard).
