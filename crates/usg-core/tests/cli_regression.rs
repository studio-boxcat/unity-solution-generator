//! CLI binary surface regression tests. These pin invariants the architecture
//! overhaul (see [[architecture.md]]) must preserve:
//!
//! - **exit codes** — 0 on success, non-zero on failure (consumers rely on these).
//! - **`--help` exits 0** — keeps `dotnet build`-style scripting safe.
//! - **`typecheck` refreshes .csproj/.sln** — Rider/IDE consumers see fresh
//!   solution files via a single subcommand, no separate generate step.
//!
//! Standalone `lock` and `generate` subcommands were removed in v0.3.0 — every
//! subcommand auto-locks and refreshes via `lockfile::scan_and_write` +
//! `generate_sln` internally. The library API (`solution_generator::generate_from_lockfile`,
//! `unity_solution_generator::generate(...)`) covers the "render-only" use
//! case for FFI hosts like meow-tower's BoxcatBridge.

use std::path::Path;
use std::process::Command;

mod common;
use common::WatchedTempDir;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_unity-solution-generator")
}

fn write(root: &Path, rel: &str, content: &str) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, content).unwrap();
}

/// A minimal Unity-shaped fixture with a pre-baked lockfile so we don't need
/// a real Unity install for CLI smoke tests.
fn fixture() -> WatchedTempDir {
    let tmp = common::make_temp_root();
    let root = tmp.path();
    write(root, "ProjectSettings/ProjectVersion.txt", "m_EditorVersion: 6000.2.7f2\n");
    write(root, "Assets/A/Lib.asmdef", r#"{"name":"Lib"}"#);
    write(root, "Assets/A/Code.cs", "class Code {}\n");
    let lf = unity_solution_generator::Lockfile::empty("6000.2.7f2", "/test/unity");
    let lf_dir = root.join("Library/UnitySolutionGenerator");
    std::fs::create_dir_all(&lf_dir).unwrap();
    unity_solution_generator::lockfile::write(&lf, lf_dir.join("csproj.lock").to_str().unwrap()).unwrap();
    tmp
}

#[test]
fn help_exits_zero_and_lists_subcommands() {
    let out = Command::new(bin()).arg("--help").output().expect("spawn");
    assert!(out.status.success(), "--help should exit 0");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("typecheck"), "help missing 'typecheck'");
    assert!(stdout.contains("build"), "help missing 'build'");
}

#[test]
fn no_args_exits_zero_and_prints_usage() {
    // Matches current behavior: bare invocation prints help, exits 0.
    let out = Command::new(bin()).output().expect("spawn");
    assert!(out.status.success(), "no-args should match --help (exit 0)");
}

#[test]
fn unknown_subcommand_exits_nonzero() {
    let out = Command::new(bin()).arg("nope").output().expect("spawn");
    assert!(!out.status.success(), "unknown subcommand should fail");
}

#[test]
fn removed_subcommands_exit_nonzero() {
    // `lock` and `generate` were removed in v0.3.0. The CLI must reject them
    // explicitly rather than silently no-op — a stray script that called them
    // pre-removal should fail loudly so the operator updates.
    for cmd in ["lock", "generate"] {
        let out = Command::new(bin()).arg(cmd).output().expect("spawn");
        assert!(
            !out.status.success(),
            "removed subcommand '{}' must exit nonzero",
            cmd
        );
    }
}

/// Pinned: `typecheck` refreshes the .csproj/.sln alongside diagnostics so
/// Rider/IDE consumers see the current solution without a separate refresh
/// step. Pre-consolidation, only the (removed) `generate` and `build` wrote
/// the solution; a typecheck-only flow left the IDE stale.
#[test]
fn typecheck_refreshes_csproj_and_sln() {
    let tmp = fixture();
    let root = tmp.path();
    let out = Command::new(bin())
        .args(["typecheck", root.to_str().unwrap()])
        .output()
        .expect("spawn");
    // Tolerate exit-status mismatch (the fixture has no real csc available)
    // — what we're pinning is that the .csproj/.sln were written before any
    // csc work happens, so IDEs see fresh files regardless.
    let _ = out;
    let variant_dir = root.join("Library/UnitySolutionGenerator/ios-editor");
    assert!(
        variant_dir.join("Lib.csproj").exists(),
        ".csproj must exist after typecheck — did not consolidate with generate?",
    );
    let sln_name = format!("{}.sln", root.file_name().unwrap().to_string_lossy());
    assert!(
        variant_dir.join(&sln_name).exists(),
        "{} must exist after typecheck — did not consolidate with generate?",
        sln_name,
    );
}
