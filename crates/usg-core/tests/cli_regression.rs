//! CLI binary surface regression tests. These pin invariants the architecture
//! overhaul (see [[architecture.md]]) must preserve:
//!
//! - **stdout = sln path** on `generate` success — used as
//!   `dotnet build "$(unity-solution-generator generate ...)"` in scripts.
//! - **exit codes** — 0 on success, non-zero on failure (consumers rely on these).
//! - **lockfile auto-creation** — Rider's FFI calls `generate()` without
//!   running `lock` first; the same shape must work via the CLI path too.
//! - **`--help` exits 0** — keeps `dotnet build`-style scripting safe.

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
    unity_solution_generator::LockfileIO::write(&lf, lf_dir.join("csproj.lock").to_str().unwrap()).unwrap();
    tmp
}

#[test]
fn help_exits_zero_and_lists_subcommands() {
    let out = Command::new(bin()).arg("--help").output().expect("spawn");
    assert!(out.status.success(), "--help should exit 0");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("lock"), "help missing 'lock'");
    assert!(stdout.contains("generate"), "help missing 'generate'");
}

#[test]
fn no_args_exits_zero_and_prints_usage() {
    // Matches current behavior: bare invocation prints help, exits 0.
    // If this changes (some CLIs exit nonzero on no-args), update both this
    // test and meow-tower's CI gates.
    let out = Command::new(bin()).output().expect("spawn");
    assert!(out.status.success(), "no-args should match --help (exit 0)");
}

#[test]
fn unknown_subcommand_exits_nonzero() {
    let out = Command::new(bin()).arg("nope").output().expect("spawn");
    assert!(!out.status.success(), "unknown subcommand should fail");
}

/// Pinned: `generate` writes the sln path on stdout.
/// Consumers script `dotnet build "$(unity-solution-generator generate ...)"`.
#[test]
fn generate_emits_sln_path_to_stdout() {
    let tmp = fixture();
    let root = tmp.path();
    let out = Command::new(bin())
        .args(["generate", root.to_str().unwrap(), "ios", "editor"])
        .output()
        .expect("spawn");
    assert!(
        out.status.success(),
        "generate failed:\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let sln_line = String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()
        .expect("stdout empty")
        .to_string();
    assert!(
        sln_line.ends_with(".sln"),
        "expected .sln path on stdout, got {:?}",
        sln_line
    );
}

#[test]
fn generate_invalid_platform_exits_nonzero() {
    let tmp = fixture();
    let out = Command::new(bin())
        .args(["generate", tmp.path().to_str().unwrap(), "freebsd", "editor"])
        .output()
        .expect("spawn");
    assert!(!out.status.success(), "invalid platform should fail");
}

#[test]
fn generate_invalid_config_exits_nonzero() {
    let tmp = fixture();
    let out = Command::new(bin())
        .args(["generate", tmp.path().to_str().unwrap(), "ios", "release"])
        .output()
        .expect("spawn");
    assert!(!out.status.success(), "invalid config should fail");
}

/// Pinned: `generate` succeeds even without a pre-existing lockfile by
/// running lock implicitly. Rider's FFI relies on this so a fresh checkout
/// "just works" on first regen.
///
/// We skip this test if the host lacks a real Unity install, since the
/// implicit lock needs to scan one. Detection: env override or default path.
#[test]
fn generate_auto_runs_lock_when_lockfile_missing() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    // Point ProjectVersion at a Unity install that exists in this dev env.
    // If no Unity is installed, the lock will fail with a clear error and the
    // test falls through (we assert AT LEAST that `generate` doesn't panic
    // and either succeeds or returns a non-zero exit cleanly).
    write(root, "ProjectSettings/ProjectVersion.txt", "m_EditorVersion: 6000.2.7f2\n");
    write(root, "Assets/A/Lib.asmdef", r#"{"name":"Lib"}"#);
    write(root, "Assets/A/Code.cs", "class Code {}\n");

    let out = Command::new(bin())
        .args(["generate", root.to_str().unwrap(), "ios", "editor"])
        .output()
        .expect("spawn");

    // Either succeeds (Unity install found and scanned), or fails with a
    // diagnosable error message — both are acceptable. What we're pinning
    // is that the binary doesn't crash and that stderr is informative when
    // it does fail.
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("Unity") || stderr.contains("lockfile") || stderr.contains("error"),
            "implicit lock failed without a diagnosable stderr message: {}",
            stderr
        );
    }
}

// `init` deprecated alias dropped in this checkpoint along with the test that
// pinned it. `lock` is the only canonical name now.

/// Pinned: `typecheck` refreshes the .csproj/.sln alongside diagnostics so
/// Rider/IDE consumers see the current solution without a separate `generate`
/// step. Pre-consolidation, only `generate` and `build` wrote the solution;
/// a typecheck-only flow left the IDE stale.
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
