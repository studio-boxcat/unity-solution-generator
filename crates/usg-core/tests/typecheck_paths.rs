//! Public `script_dll_dir` path contract. The stamp-sidecar, up-to-date, and
//! `csc` rsp internals are white-box unit-tested inside `src/typecheck.rs` and
//! `src/csc.rs` — they're crate-private and not part of the published API.

use std::path::PathBuf;

use unity_solution_generator::{BuildConfig, BuildPlatform, script_dll_dir};

/// Consumer-facing `script_dll_dir` wraps the internal variant-output-dir
/// layout with the default generator root + a `Path` ergonomic signature.
/// Used by reflection tools (e.g. pspec bake-types) that don't want to import
/// `DEFAULT_GENERATOR_ROOT` or juggle `&str`↔`PathBuf` conversions.
#[test]
fn script_dll_dir_uses_default_generator_root() {
    let p = script_dll_dir("/proj", BuildPlatform::Ios, BuildConfig::Editor);
    assert_eq!(
        p,
        PathBuf::from("/proj/Library/UnitySolutionGenerator/ios-editor/obj/Debug")
    );
}

#[test]
fn script_dll_dir_varies_with_variant() {
    let p = script_dll_dir("/proj", BuildPlatform::Android, BuildConfig::Prod);
    assert_eq!(
        p,
        PathBuf::from("/proj/Library/UnitySolutionGenerator/android-prod/obj/Debug")
    );
}
