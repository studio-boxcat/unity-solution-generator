//! Path layout, stamp sidecar, and up-to-date predicate for `typecheck`.
//!
//! These tests pin the consolidated obj/Debug layout (see `architecture.md`
//! → "On-disk layout" + "typecheck deeper") and the foreign-writer guard
//! implemented via per-DLL `.usg-stamp` sidecars. Background: when `typecheck`
//! shares `<variant>/obj/Debug/` with `build`'s MSBuild output, a naive
//! mtime-only UTD would silently SKIP after `dotnet build` wrote a fresh
//! DLL — typecheck would rubber-stamp MSBuild's compile instead of running csc.

mod common;

use std::fs;

use common::make_temp_root;
use unity_solution_generator::typecheck::__test_only as tcx;
use unity_solution_generator::{BuildConfig, BuildPlatform};

const GR: &str = "Library/USG";

#[test]
fn output_dir_is_under_variant_obj_debug() {
    let dir = tcx::typecheck_output_dir("/proj", GR, BuildPlatform::Ios, BuildConfig::Editor);
    assert_eq!(dir, "/proj/Library/USG/ios-editor/obj/Debug");
}

/// Consumer-facing `script_dll_dir` wraps `typecheck_output_dir` with the
/// default generator root + a `Path` ergonomic signature. Used by
/// reflection tools (e.g. pspec bake-types) that don't want to import
/// `DEFAULT_GENERATOR_ROOT` or juggle `&str`↔`PathBuf` conversions.
#[test]
fn script_dll_dir_uses_default_generator_root() {
    use std::path::PathBuf;
    use unity_solution_generator::script_dll_dir;
    let p = script_dll_dir("/proj", BuildPlatform::Ios, BuildConfig::Editor);
    assert_eq!(
        p,
        PathBuf::from("/proj/Library/UnitySolutionGenerator/ios-editor/obj/Debug")
    );
}

#[test]
fn script_dll_dir_varies_with_variant() {
    use std::path::PathBuf;
    use unity_solution_generator::script_dll_dir;
    let p = script_dll_dir("/proj", BuildPlatform::Android, BuildConfig::Prod);
    assert_eq!(
        p,
        PathBuf::from("/proj/Library/UnitySolutionGenerator/android-prod/obj/Debug")
    );
}

#[test]
fn output_dir_varies_with_variant() {
    let a = tcx::typecheck_output_dir("/proj", GR, BuildPlatform::Android, BuildConfig::Dev);
    assert_eq!(a, "/proj/Library/USG/android-dev/obj/Debug");
}

#[test]
fn stamp_path_is_dll_dot_usg_stamp() {
    assert_eq!(tcx::stamp_path_for("/x/Foo.dll"), "/x/Foo.dll.usg-stamp");
}

#[test]
fn stamp_roundtrip() {
    let tmp = make_temp_root();
    let p = tmp.path().join("Foo.dll.usg-stamp");
    let path = p.to_str().unwrap();
    tcx::write_stamp(path, 1234567890u128).unwrap();
    assert_eq!(tcx::read_stamp(path), Some(1234567890u128));
}

#[test]
fn stamp_read_returns_none_when_absent() {
    let tmp = make_temp_root();
    let p = tmp.path().join("nope.usg-stamp");
    assert_eq!(tcx::read_stamp(p.to_str().unwrap()), None);
}

#[test]
fn stamp_read_returns_none_on_garbage() {
    let tmp = make_temp_root();
    let p = tmp.path().join("garbage.usg-stamp");
    fs::write(&p, b"not-a-number\n").unwrap();
    assert_eq!(tcx::read_stamp(p.to_str().unwrap()), None);
}

// ── UTD predicate ────────────────────────────────────────────────────────

#[test]
fn utd_false_when_dll_missing() {
    let tmp = make_temp_root();
    let dll = tmp.path().join("Foo.dll");
    let stamp = tmp.path().join("Foo.dll.usg-stamp");
    // No DLL on disk; stamp irrelevant.
    assert!(!tcx::is_up_to_date(
        &[],
        &[],
        &[],
        dll.to_str().unwrap(),
        stamp.to_str().unwrap(),
    ));
}

#[test]
fn utd_false_when_stamp_missing() {
    // The DLL exists but no `.usg-stamp` next to it. This is exactly the
    // post-`dotnet build` state: MSBuild wrote the DLL, we did not.
    // Without the stamp guard, typecheck would silently rubber-stamp it.
    let tmp = make_temp_root();
    let dll = tmp.path().join("Foo.dll");
    fs::write(&dll, b"foreign\n").unwrap();
    let stamp = tmp.path().join("Foo.dll.usg-stamp");
    assert!(!tcx::is_up_to_date(
        &[],
        &[],
        &[],
        dll.to_str().unwrap(),
        stamp.to_str().unwrap(),
    ));
}

#[test]
fn utd_false_when_stamp_mtime_disagrees_with_disk() {
    // Stamp recorded a different mtime than what's on disk → foreign writer
    // touched the DLL since we stamped it.
    let tmp = make_temp_root();
    let dll = tmp.path().join("Foo.dll");
    fs::write(&dll, b"x").unwrap();
    let stamp = tmp.path().join("Foo.dll.usg-stamp");
    tcx::write_stamp(stamp.to_str().unwrap(), 1u128).unwrap(); // bogus mtime
    assert!(!tcx::is_up_to_date(
        &[],
        &[],
        &[],
        dll.to_str().unwrap(),
        stamp.to_str().unwrap(),
    ));
}

#[test]
fn utd_true_when_stamp_matches_and_inputs_older() {
    let tmp = make_temp_root();
    let dll = tmp.path().join("Foo.dll");
    fs::write(&dll, b"x").unwrap();
    let dll_mtime = tcx::mtime_nsec(dll.to_str().unwrap()).unwrap();
    let stamp = tmp.path().join("Foo.dll.usg-stamp");
    tcx::write_stamp(stamp.to_str().unwrap(), dll_mtime).unwrap();
    assert!(tcx::is_up_to_date(
        &[],
        &[],
        &[],
        dll.to_str().unwrap(),
        stamp.to_str().unwrap(),
    ));
}

// ── post-emit stamping behaviour (pure, no csc) ──────────────────────────

#[test]
fn record_stamp_after_write_makes_utd_true() {
    // End-to-end of the producer side: write a DLL, stamp it with its own
    // mtime, then UTD should be true on the next pass with no source changes.
    let tmp = make_temp_root();
    let dll = tmp.path().join("Foo.dll");
    fs::write(&dll, b"contents").unwrap();
    let stamp = tmp.path().join("Foo.dll.usg-stamp");

    tcx::record_stamp_for(dll.to_str().unwrap(), stamp.to_str().unwrap()).unwrap();

    assert!(tcx::is_up_to_date(
        &[],
        &[],
        &[],
        dll.to_str().unwrap(),
        stamp.to_str().unwrap(),
    ));
}

#[test]
fn foreign_overwrite_after_stamp_breaks_utd() {
    // Stamp the DLL, then simulate `dotnet build` overwriting it. UTD must flip
    // to false so the next typecheck recompiles and re-stamps.
    let tmp = make_temp_root();
    let dll = tmp.path().join("Foo.dll");
    fs::write(&dll, b"v1").unwrap();
    let stamp = tmp.path().join("Foo.dll.usg-stamp");
    tcx::record_stamp_for(dll.to_str().unwrap(), stamp.to_str().unwrap()).unwrap();
    assert!(tcx::is_up_to_date(
        &[],
        &[],
        &[],
        dll.to_str().unwrap(),
        stamp.to_str().unwrap(),
    ));

    // Sleep a hair to guarantee mtime advances on filesystems with coarse
    // resolution, then overwrite. (apfs is nanosecond; ext4 may be too coarse
    // without this nudge.)
    std::thread::sleep(std::time::Duration::from_millis(10));
    fs::write(&dll, b"foreign-bytes").unwrap();

    assert!(!tcx::is_up_to_date(
        &[],
        &[],
        &[],
        dll.to_str().unwrap(),
        stamp.to_str().unwrap(),
    ));
}


// ── cascade-loop guard ───────────────────────────────────────────────────

#[test]
fn restored_mtime_floors_at_freshest_input() {
    // After a foreign upstream write, downstream recompiles, csc emits the
    // same bytes (deterministic), and we restore the mtime. If we restored to
    // the OLD prev_t, downstream's mtime would be below its proj_ref's mtime
    // and UTD would fail forever — every subsequent typecheck would recompile
    // downstream. max_input_mtime is the floor that breaks this loop.
    use std::path::PathBuf;
    let tmp = make_temp_root();
    let dll = tmp.path().join("Downstream.dll");
    let proj_ref = tmp.path().join("Upstream.dll");

    fs::write(&dll, b"downstream-bytes").unwrap();
    let prev_t = tcx::mtime_nsec(dll.to_str().unwrap()).unwrap();

    // proj_ref written AFTER downstream → fresher mtime, simulating the
    // post-foreign-write upstream recovery.
    std::thread::sleep(std::time::Duration::from_millis(10));
    fs::write(&proj_ref, b"upstream-bytes").unwrap();
    let upstream_t = tcx::mtime_nsec(proj_ref.to_str().unwrap()).unwrap();
    assert!(upstream_t > prev_t);

    let target = tcx::max_input_mtime(&[], &[], &[PathBuf::from(&proj_ref)])
        .map_or(prev_t, |m| prev_t.max(m));

    assert_eq!(
        target, upstream_t,
        "restore target must rise to upstream's mtime, not stay at prev_t",
    );

    // Apply the restore + stamp and verify UTD passes against the upstream input.
    tcx::restore_mtime(dll.to_str().unwrap(), target).unwrap();
    let stamp = tmp.path().join("Downstream.dll.usg-stamp");
    tcx::record_stamp_for(dll.to_str().unwrap(), stamp.to_str().unwrap()).unwrap();

    assert!(
        tcx::is_up_to_date(
            &[],
            &[],
            &[PathBuf::from(&proj_ref)],
            dll.to_str().unwrap(),
            stamp.to_str().unwrap(),
        ),
        "downstream must UTD-skip on the run after cascade recovery — no infinite loop",
    );
}

#[test]
fn max_input_mtime_picks_freshest_across_categories() {
    use unity_solution_generator::DllRef;
    use std::path::PathBuf;
    let tmp = make_temp_root();
    let a = tmp.path().join("a.cs");
    let b = tmp.path().join("b.dll");
    let c = tmp.path().join("c.dll");
    fs::write(&a, b"a").unwrap();
    std::thread::sleep(std::time::Duration::from_millis(5));
    fs::write(&b, b"b").unwrap();
    std::thread::sleep(std::time::Duration::from_millis(5));
    fs::write(&c, b"c").unwrap();

    let max = tcx::max_input_mtime(
        &[PathBuf::from(&a)],
        &[DllRef::new("B", b.to_str().unwrap())],
        &[PathBuf::from(&c)],
    )
    .unwrap();
    assert_eq!(max, tcx::mtime_nsec(c.to_str().unwrap()).unwrap());
}

#[test]
fn max_input_mtime_none_when_no_inputs() {
    assert!(tcx::max_input_mtime(&[], &[], &[]).is_none());
}

#[test]
fn utd_false_when_source_newer_than_dll() {
    // The "source touched after last emit" branch of is_up_to_date. Stamp +
    // disk-mtime can be in agreement, but a fresh source still forces recompile.
    use std::path::PathBuf;
    let tmp = make_temp_root();
    let dll = tmp.path().join("Foo.dll");
    fs::write(&dll, b"x").unwrap();
    let stamp = tmp.path().join("Foo.dll.usg-stamp");
    tcx::record_stamp_for(dll.to_str().unwrap(), stamp.to_str().unwrap()).unwrap();

    // Source written AFTER the DLL+stamp → newer mtime.
    std::thread::sleep(std::time::Duration::from_millis(10));
    let src = tmp.path().join("Foo.cs");
    fs::write(&src, b"class Foo {}\n").unwrap();

    assert!(!tcx::is_up_to_date(
        &[PathBuf::from(&src)],
        &[],
        &[],
        dll.to_str().unwrap(),
        stamp.to_str().unwrap(),
    ));
}

#[test]
fn utd_false_when_ref_newer_than_dll() {
    // The "external DllRef touched after last emit" branch — e.g. Unity
    // updated, advancing the engine DLL mtime under us.
    use unity_solution_generator::DllRef;
    let tmp = make_temp_root();
    let dll = tmp.path().join("Foo.dll");
    fs::write(&dll, b"x").unwrap();
    let stamp = tmp.path().join("Foo.dll.usg-stamp");
    tcx::record_stamp_for(dll.to_str().unwrap(), stamp.to_str().unwrap()).unwrap();

    std::thread::sleep(std::time::Duration::from_millis(10));
    let r = tmp.path().join("UnityEngine.dll");
    fs::write(&r, b"engine-bytes").unwrap();

    assert!(!tcx::is_up_to_date(
        &[],
        &[DllRef::new("UnityEngine", r.to_str().unwrap())],
        &[],
        dll.to_str().unwrap(),
        stamp.to_str().unwrap(),
    ));
}
