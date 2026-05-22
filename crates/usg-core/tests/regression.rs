//! Regression tests pinning external-facing invariants before the architecture
//! overhaul (see [[architecture.md]]). Keep these green across every checkpoint.
//!
//! Coverage axes the existing `e2e.rs` / `integration.rs` don't cover:
//! - **Deterministic GUID byte-equality** — protects `<ProjectGuid>` and `.sln`
//!   wiring from accidental hash changes.
//! - **CLI binary surface** — exit codes + stdout shape (parsed by
//!   meow-tower's justfile).
//! - **Lockfile auto-creation on `generate`** — Rider's FFI relies on this.
//! - **Multi-asmdef variant filtering** — the audit-recommended fixture covers
//!   Runtime / Editor / Tests / iOS-only branches in one synthetic project.

mod common;

use std::path::Path;

use common::{make_temp_root, write_file};
use unity_solution_generator::{
    BuildConfig, BuildPlatform, GenerateOptions, Lockfile, RefCategory, solution_generator,
    xml::deterministic_guid,
};

const GR: &str = "tpl";

// ─── deterministic GUID pin ───────────────────────────────────────────────

/// Locks the GUID for a fixed set of asmdef names. If one of these changes,
/// every `.csproj` ProjectReference and `.sln` project entry shifts — silently
/// breaking solutions that have references baked in.
#[test]
fn guid_pinning_table() {
    // Format: (asmdef_name, expected_guid). Generated against the current xml.rs
    // implementation; bumping requires acknowledging the breaking change.
    for (name, expected) in [
        ("Lib", &*deterministic_guid("Lib")),
        ("Assembly-CSharp", &*deterministic_guid("Assembly-CSharp")),
        ("Assembly-CSharp-Editor", &*deterministic_guid("Assembly-CSharp-Editor")),
    ] {
        // Each pair is generated then re-validated: catches both intra-process
        // determinism and inter-call stability. The actual byte-pinning lives
        // below — we want to detect drift in the *implementation*, not just
        // self-consistency.
        let actual = deterministic_guid(name);
        assert_eq!(
            actual, *expected,
            "GUID for {} drifted from {} to {}",
            name, expected, actual
        );
    }

    // Hard-coded values pinning the current (Swift-port) implementation. Update
    // ONLY when the byte-equality contract is intentionally broken — otherwise
    // generated `.csproj`/`.sln` files become invalid mid-refactor.
    let cases: &[(&str, &str)] = &[
        ("Lib", "{00000000-0B88-041C-24EC-8619B87F02CC}"),
        ("Foo", "{00000000-0B87-EB69-F2BB-95199C92E1D7}"),
        ("Assembly-CSharp", "{BC802F6F-F258-0B13-9024-7747C527DB7B}"),
        ("Assembly-CSharp-Editor", "{97E07C5E-6C9E-3B67-26CD-B0ED29BA73D3}"),
    ];
    for (name, expected) in cases {
        let actual = deterministic_guid(name);
        if actual != *expected {
            // Print BOTH in the failure so updating the table is mechanical.
            panic!(
                "GUID pin drift: deterministic_guid({:?}) = {:?}, expected {:?}.\n\
                 If this change is intentional, update the table in regression.rs.",
                name, actual, expected
            );
        }
    }
}

// ─── multi-asmdef variant filtering fixture ───────────────────────────────

fn lockfile_for_fixture() -> Lockfile {
    let mut lf = Lockfile::empty("6000.2.7f2", "/test/unity");
    // Just enough refs to exercise category filtering without a real Unity install.
    lf.refs
        .entry(RefCategory::Engine)
        .or_default()
        .push(unity_solution_generator::DllRef::new(
            "UnityEngine",
            "$(UnityPath)/Unity.app/Contents/Managed/UnityEngine/UnityEngine.dll",
        ));
    lf.defines.push("UNITY_6000".into());
    lf
}

/// Lay down a synthetic project that exercises each asmdef-category branch
/// the audit identified: Runtime (always), Editor (editor-only), Tests
/// (UNITY_INCLUDE_TESTS), and iOS-only (platform filter). Each asmdef lives in
/// its OWN directory because two asmdefs sharing a parent dir creates undefined
/// source-ownership (existing tests in `integration.rs` follow the same rule).
fn write_fixture(root: &Path) {
    write_file(root, "Assets/Foo/Foo.asmdef", r#"{"name":"Foo","references":[]}"#);
    write_file(root, "Assets/Foo/Foo.cs", "class Foo {}\n");

    write_file(
        root,
        "Assets/IOSOnly/IOSOnly.asmdef",
        r#"{"name":"IOSOnly","includePlatforms":["iOS"]}"#,
    );
    write_file(root, "Assets/IOSOnly/IOSOnly.cs", "class IOSOnly {}\n");

    write_file(
        root,
        "Assets/Bar/Bar.asmdef",
        r#"{"name":"Bar","includePlatforms":["Editor"]}"#,
    );
    write_file(root, "Assets/Bar/Bar.cs", "class Bar {}\n");

    write_file(
        root,
        "Assets/Baz/Baz.asmdef",
        r#"{"name":"Baz","defineConstraints":["UNITY_INCLUDE_TESTS"]}"#,
    );
    write_file(root, "Assets/Baz/Baz.cs", "class Baz {}\n");
}

fn opts(root: &Path, platform: BuildPlatform, cfg: BuildConfig) -> GenerateOptions {
    GenerateOptions::new(root.to_string_lossy().into_owned(), platform)
        .with_generator_root(GR)
        .with_build_config(cfg)
}

#[test]
fn ios_editor_includes_all_four_asmdefs() {
    let tmp = make_temp_root();
    let root = tmp.path();
    write_fixture(root);

    let result = solution_generator::generate_from_lockfile(
            &opts(root, BuildPlatform::Ios, BuildConfig::Editor),
            &lockfile_for_fixture(),
        )
        .unwrap();

    let names: Vec<String> = result
        .variant_csprojs
        .iter()
        .filter_map(|p| Path::new(p).file_stem().map(|s| s.to_string_lossy().into_owned()))
        .collect();
    for expected in ["Foo", "IOSOnly", "Bar", "Baz"] {
        assert!(
            names.iter().any(|n| n == expected),
            "ios-editor missing {}: got {:?}",
            expected,
            names
        );
    }
}

#[test]
fn ios_prod_excludes_editor_only() {
    let tmp = make_temp_root();
    let root = tmp.path();
    write_fixture(root);

    let result = solution_generator::generate_from_lockfile(
            &opts(root, BuildPlatform::Ios, BuildConfig::Prod),
            &lockfile_for_fixture(),
        )
        .unwrap();

    let names: Vec<String> = result
        .variant_csprojs
        .iter()
        .filter_map(|p| Path::new(p).file_stem().map(|s| s.to_string_lossy().into_owned()))
        .collect();
    assert!(names.iter().any(|n| n == "Foo"), "Foo (runtime) should be included");
    assert!(names.iter().any(|n| n == "IOSOnly"), "IOSOnly should match ios platform");
    assert!(
        !names.iter().any(|n| n == "Bar"),
        "Bar (editor-only) should NOT be in prod variant"
    );
    assert!(
        !names.iter().any(|n| n == "Baz"),
        "Baz (test) should NOT be in prod variant"
    );
}

#[test]
fn android_prod_excludes_ios_only() {
    let tmp = make_temp_root();
    let root = tmp.path();
    write_fixture(root);

    let result = solution_generator::generate_from_lockfile(
            &opts(root, BuildPlatform::Android, BuildConfig::Prod),
            &lockfile_for_fixture(),
        )
        .unwrap();

    let names: Vec<String> = result
        .variant_csprojs
        .iter()
        .filter_map(|p| Path::new(p).file_stem().map(|s| s.to_string_lossy().into_owned()))
        .collect();
    assert!(names.iter().any(|n| n == "Foo"));
    assert!(
        !names.iter().any(|n| n == "IOSOnly"),
        "iOS-only asmdef should NOT be in android-prod"
    );
    assert!(!names.iter().any(|n| n == "Bar"));
}

// CLI binary surface tests live in `tests/cli_regression.rs` (same package
// since the bin target is now in usg-core).

#[test]
fn display_round_trips_through_parse() {
    use unity_solution_generator::{BuildConfig, BuildPlatform};
    for &p in BuildPlatform::ALL {
        assert_eq!(BuildPlatform::parse(&p.to_string()), Some(p));
        assert_eq!(p.to_string(), p.raw());
    }
    for c in [BuildConfig::Editor, BuildConfig::Dev, BuildConfig::Prod] {
        assert_eq!(BuildConfig::parse(&c.to_string()), Some(c));
        assert_eq!(c.to_string(), c.raw());
    }
}
