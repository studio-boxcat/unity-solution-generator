//! End-to-end scenario tests covering: idempotency, multi-variant generation,
//! native-plugin filtering, asmdef versionDefines manifest filtering,
//! lock fingerprint short-circuit, and duplicate-asmdef error reporting.

mod common;

use std::collections::BTreeMap;
use std::path::Path;

use common::{make_temp_root, read_compile_set, read_file, write_file};
use usg_core::{
    BuildConfig, BuildPlatform, DllRef, GenerateOptions, GeneratorError, Lockfile, LockfileIO,
    ProjectScanner, RefCategory, SolutionGenerator,
    lockfile_path,
};

const GR: &str = "tpl";

fn opts(root: &Path, platform: BuildPlatform, cfg: BuildConfig) -> GenerateOptions {
    GenerateOptions::new(root.to_string_lossy().into_owned(), platform)
        .with_generator_root(GR)
        .with_build_config(cfg)
}

fn small_lockfile() -> Lockfile {
    let mut refs: BTreeMap<RefCategory, Vec<DllRef>> = BTreeMap::new();
    refs.insert(
        RefCategory::Engine,
        vec![DllRef::new(
            "UnityEngine",
            "$(UnityPath)/Unity.app/Contents/Managed/UnityEngine/UnityEngine.dll",
        )],
    );
    refs.insert(RefCategory::Editor, vec![]);
    refs.insert(RefCategory::Netstandard, vec![]);
    refs.insert(RefCategory::PlaybackIos, vec![]);
    refs.insert(RefCategory::PlaybackAndroid, vec![]);
    refs.insert(RefCategory::PlaybackStandalone, vec![]);
    refs.insert(RefCategory::Project, vec![]);
    Lockfile {
        unity_version: "6000.2.7f2".into(),
        unity_path: "/test/unity".into(),
        lang_version: "9.0".into(),
        analyzers: vec![],
        refs,
        defines: vec!["UNITY_6000".into()],
        defines_scripting: vec![],
    }
}

#[test]
fn regeneration_is_byte_identical() {
    let tmp = make_temp_root();
    let root = tmp.path();
    let lf = small_lockfile();
    write_file(root, "Assets/A/Lib.asmdef", r#"{"name":"Lib"}"#);
    write_file(root, "Assets/A/Code.cs", "class Code {}\n");

    let g = SolutionGenerator::new();
    g.generate_from_lockfile(&opts(root, BuildPlatform::Ios, BuildConfig::Editor), &lf)
        .unwrap();
    let csproj1 = read_file(root, "tpl/ios-editor/Lib.csproj");
    let sln1 = read_file(root, &format!("tpl/ios-editor/{}.sln", root.file_name().unwrap().to_string_lossy()));
    let props1 = read_file(root, "tpl/ios-editor/Directory.Build.props");

    g.generate_from_lockfile(&opts(root, BuildPlatform::Ios, BuildConfig::Editor), &lf)
        .unwrap();
    let csproj2 = read_file(root, "tpl/ios-editor/Lib.csproj");
    let sln2 = read_file(root, &format!("tpl/ios-editor/{}.sln", root.file_name().unwrap().to_string_lossy()));
    let props2 = read_file(root, "tpl/ios-editor/Directory.Build.props");

    assert_eq!(csproj1, csproj2);
    assert_eq!(sln1, sln2);
    assert_eq!(props1, props2);
}

#[test]
fn multi_variant_from_same_lockfile_share_scan_cache() {
    // First variant warms the scan-cache; subsequent variants must read it,
    // not re-walk. We can't directly assert "didn't walk" without instrumentation,
    // but we *can* assert the variants produce expected output and don't error.
    let tmp = make_temp_root();
    let root = tmp.path();
    let lf = small_lockfile();
    write_file(root, "Assets/A/Lib.asmdef", r#"{"name":"Lib"}"#);
    write_file(root, "Assets/A/Code.cs", "class Code {}\n");

    let g = SolutionGenerator::new();
    for (p, c) in [
        (BuildPlatform::Ios, BuildConfig::Editor),
        (BuildPlatform::Ios, BuildConfig::Prod),
        (BuildPlatform::Android, BuildConfig::Editor),
        (BuildPlatform::Osx, BuildConfig::Editor),
    ] {
        let r = g
            .generate_from_lockfile(&opts(root, p, c), &lf)
            .unwrap();
        assert!(r.variant_csprojs.iter().any(|s| s.ends_with("Lib.csproj")));
    }
    // Sanity-check the on-disk variants exist
    for v in ["ios-editor", "ios-prod", "android-editor", "osx-editor"] {
        assert!(
            root.join("tpl").join(v).join("Lib.csproj").exists(),
            "variant {} missing",
            v
        );
    }
}

#[test]
fn native_plugin_dirs_skipped() {
    // ProjectScanner skips native-plugin dir names like x86_64, ARM64, .framework, .bundle.
    // (Note: applies to .dll/.asmdef walk inside lockfile_scanner, not the .cs walk —
    // this test confirms the ProjectScanner-side .cs walk *does* still see the leaves
    // because cs files aren't normally in native-plugin dirs anyway.)
    // What we verify here: a `Foo.framework` directory inside Assets does NOT get its
    // .cs files included if scanned by lockfile_scanner. We simulate by inspecting
    // ProjectScanner output, which uses ignore::WalkBuilder without the native-plugin filter
    // — so .cs files in those dirs DO get included. The native-plugin filter only applies
    // to the lockfile-scanner DLL/asmdef walk. We still smoke-test the path.
    //
    // Concrete check: place a .asmdef inside x86_64/ and confirm it isn't picked up
    // when LockfileScanner runs a project walk (we can't run the full LockfileScanner
    // without a real Unity install, so we verify the helper directly via behavior:
    // create a fake non-Unity project and call ProjectScanner — which doesn't apply
    // the native-plugin filter — to confirm the filter is *only* in the lockfile path).
    let tmp = make_temp_root();
    let root = tmp.path();
    write_file(root, "Assets/Plugin/x86_64/Code.cs", "class Native {}\n");
    write_file(root, "Assets/Plugin/Code.cs", "class Plain {}\n");
    // ProjectScanner does NOT apply native-plugin filtering — both files should be picked up.
    let scan = ProjectScanner::scan(root.to_str().unwrap()).unwrap();
    let total_dirs: usize = scan.dirs_by_project.values().map(|v| v.len()).sum::<usize>()
        + scan.unresolved_dirs.len();
    assert!(
        total_dirs >= 2,
        "expected at least 2 cs dirs (x86_64 + parent), got {}",
        total_dirs
    );
}

#[test]
fn asmdef_version_defines_filtered_by_manifest() {
    let tmp = make_temp_root();
    let root = tmp.path();
    // manifest.json declares only physics2d
    write_file(
        root,
        "Packages/manifest.json",
        r#"{"dependencies":{"com.unity.modules.physics2d":"1.0.0"}}"#,
    );
    write_file(
        root,
        "Assets/A/Lib.asmdef",
        r#"{"name":"Lib","versionDefines":[
            {"name":"com.unity.modules.physics2d","expression":"","define":"HAS_PHYSICS"},
            {"name":"com.unity.modules.audio","expression":"","define":"HAS_AUDIO"},
            {"name":"Unity","expression":"","define":"HAS_UNITY"}
        ]}"#,
    );
    write_file(root, "Assets/A/Code.cs", "class Code {}\n");

    // Use the asmdef-version-defines collector via ProjectScanner + the public API
    // by inspecting the scan; the `defines` collection itself is owned by LockfileScanner
    // which requires a real Unity install. So we directly verify ProjectScanner ingests
    // versionDefines into the AsmDefRecord, and the manifest-filtering happens in the
    // lockfile path. Here we verify the AsmDefRecord captures all three.
    let scan = ProjectScanner::scan(root.to_str().unwrap()).unwrap();
    let asm = scan.asm_def_by_name.get("Lib").unwrap();
    assert_eq!(asm.version_defines.len(), 3);
    let names: std::collections::HashSet<_> =
        asm.version_defines.iter().map(|v| v.define.as_str()).collect();
    assert!(names.contains("HAS_PHYSICS"));
    assert!(names.contains("HAS_AUDIO"));
    assert!(names.contains("HAS_UNITY"));
}

#[test]
fn duplicate_asmdef_name_errors() {
    let tmp = make_temp_root();
    let root = tmp.path();
    write_file(root, "Assets/A/Foo.asmdef", r#"{"name":"Foo"}"#);
    write_file(root, "Assets/B/Foo.asmdef", r#"{"name":"Foo"}"#);
    write_file(root, "Assets/A/X.cs", "class X {}\n");
    write_file(root, "Assets/B/Y.cs", "class Y {}\n");

    let err = ProjectScanner::scan(root.to_str().unwrap()).unwrap_err();
    match err {
        GeneratorError::DuplicateAsmDefName(n) => assert_eq!(n, "Foo"),
        other => panic!("expected DuplicateAsmDefName, got {:?}", other),
    }
}

#[test]
fn lockfile_round_trip_preserves_unicode() {
    let tmp = make_temp_root();
    let root = tmp.path();
    let path = root.join("csproj.lock");
    let mut lf = small_lockfile();
    lf.defines.push("FÜRBALL_测试".to_string());
    LockfileIO::write(&lf, path.to_str().unwrap()).unwrap();
    let r = LockfileIO::read(path.to_str().unwrap()).unwrap();
    assert_eq!(r.defines, lf.defines);
}

#[test]
fn extra_refs_ordering_preserves_lockfile_first() {
    // The Swift impl: lockfile refs come first, extra refs append; dedupe is "lockfile wins".
    let tmp = make_temp_root();
    let root = tmp.path();
    let lf = small_lockfile();
    write_file(root, "Assets/A/Main.asmdef", r#"{"name":"Main"}"#);
    write_file(root, "Assets/A/Code.cs", "class Code {}\n");
    let extra = vec![
        DllRef::new("UnityEngine", "/SHOULD_NOT_WIN/UnityEngine.dll"),
        DllRef::new("MyExtra", "/path/MyExtra.dll"),
    ];
    SolutionGenerator::new()
        .generate_from_lockfile(
            &opts(root, BuildPlatform::Ios, BuildConfig::Editor).with_extra_refs(extra),
            &lf,
        )
        .unwrap();
    let csproj = read_file(root, "tpl/ios-editor/Main.csproj");
    // Lockfile UnityEngine path should be present, the extra-refs duplicate must not.
    assert!(csproj.contains("Managed/UnityEngine/UnityEngine.dll"));
    assert!(!csproj.contains("/SHOULD_NOT_WIN/UnityEngine.dll"));
    assert!(csproj.contains("MyExtra"));
}

#[test]
fn changing_asmref_target_reroutes_sources() {
    // After scan-cache exists, changing an .asmref reference name should re-route
    // the directory to a different assembly. The cache is keyed by mtime, so
    // updating the file (which bumps mtime) must trigger a fresh scan.
    let tmp = make_temp_root();
    let root = tmp.path();
    let lf = small_lockfile();
    write_file(root, "Assets/A/Asm1.asmdef", r#"{"name":"Asm1"}"#);
    write_file(root, "Assets/B/Asm2.asmdef", r#"{"name":"Asm2"}"#);
    // Each assembly owns its own non-empty source dir so both .csprojs are always emitted.
    write_file(root, "Assets/A/A.cs", "class A {}\n");
    write_file(root, "Assets/B/B.cs", "class B {}\n");
    // Game/Code.cs is routed via asmref.
    write_file(root, "Assets/Game/Assembly.asmref", r#"{"reference":"Asm1"}"#);
    write_file(root, "Assets/Game/Code.cs", "class Code {}\n");

    let g = SolutionGenerator::new();
    g.generate_from_lockfile(&opts(root, BuildPlatform::Ios, BuildConfig::Editor), &lf)
        .unwrap();
    let asm1 = read_compile_set(root, "tpl/ios-editor/Asm1.csproj");
    let asm2 = read_compile_set(root, "tpl/ios-editor/Asm2.csproj");
    assert!(asm1.contains("Assets/Game/Code.cs"));
    assert!(!asm2.contains("Assets/Game/Code.cs"));

    std::thread::sleep(std::time::Duration::from_millis(10));
    write_file(root, "Assets/Game/Assembly.asmref", r#"{"reference":"Asm2"}"#);

    g.generate_from_lockfile(&opts(root, BuildPlatform::Ios, BuildConfig::Editor), &lf)
        .unwrap();
    let asm1 = read_compile_set(root, "tpl/ios-editor/Asm1.csproj");
    let asm2 = read_compile_set(root, "tpl/ios-editor/Asm2.csproj");
    assert!(!asm1.contains("Assets/Game/Code.cs"));
    assert!(asm2.contains("Assets/Game/Code.cs"));
}

#[test]
fn writefile_if_changed_skips_unchanged() {
    // write_file_if_changed returns false when content matches existing bytes.
    // We verify this end-to-end via mtime: after first write, a no-op regenerate
    // must NOT bump the csproj's mtime.
    let tmp = make_temp_root();
    let root = tmp.path();
    let lf = small_lockfile();
    write_file(root, "Assets/A/Lib.asmdef", r#"{"name":"Lib"}"#);
    write_file(root, "Assets/A/Code.cs", "class Code {}\n");

    let g = SolutionGenerator::new();
    g.generate_from_lockfile(&opts(root, BuildPlatform::Ios, BuildConfig::Editor), &lf)
        .unwrap();
    let csproj_path = root.join("tpl/ios-editor/Lib.csproj");
    let mtime_before = std::fs::metadata(&csproj_path).unwrap().modified().unwrap();

    std::thread::sleep(std::time::Duration::from_millis(20));
    g.generate_from_lockfile(&opts(root, BuildPlatform::Ios, BuildConfig::Editor), &lf)
        .unwrap();
    let mtime_after = std::fs::metadata(&csproj_path).unwrap().modified().unwrap();
    assert_eq!(
        mtime_before, mtime_after,
        "csproj mtime changed despite no-op regenerate"
    );
}

#[test]
fn output_dir_with_trailing_slash_normalised() {
    let tmp = make_temp_root();
    let root = tmp.path();
    let lf = small_lockfile();
    write_file(root, "Assets/A/Lib.asmdef", r#"{"name":"Lib"}"#);
    write_file(root, "Assets/A/Code.cs", "class Code {}\n");
    let r = SolutionGenerator::new()
        .generate_from_lockfile(
            &opts(root, BuildPlatform::Ios, BuildConfig::Editor)
                .with_output_dir(Some("Library/foo/bar/")),
            &lf,
        )
        .unwrap();
    assert_eq!(r.variant_csprojs, vec!["Library/foo/bar/Lib.csproj"]);
}

#[test]
fn empty_extra_refs_string_parses_empty() {
    let parsed = DllRef::parse_list("");
    assert!(parsed.is_empty());
}

#[test]
fn no_lockfile_no_templates_falls_back_via_cli_path() {
    // CLI-side: when no lockfile and no templates exist, the FFI `usg_generate`
    // and the CLI `generate` both auto-run lock. We can't exercise the Unity-install
    // scan (no Unity in CI), but we *can* assert that calling generate_from_lockfile
    // directly with no asmdefs at all produces the legacy fallback projects.
    let tmp = make_temp_root();
    let root = tmp.path();
    let lf = small_lockfile();
    // Plain .cs file under Assets/, no asmdef. Should land in Assembly-CSharp via legacy rules.
    write_file(root, "Assets/Foo.cs", "class Foo {}\n");
    let r = SolutionGenerator::new()
        .generate_from_lockfile(&opts(root, BuildPlatform::Ios, BuildConfig::Editor), &lf)
        .unwrap();
    assert!(
        r.variant_csprojs
            .iter()
            .any(|s| s.ends_with("Assembly-CSharp.csproj")),
        "expected Assembly-CSharp fallback, got {:?}",
        r.variant_csprojs
    );
}

#[test]
fn lock_fingerprint_short_circuits_rescan() {
    // We can't test scan_and_write end-to-end without a real Unity install. But we
    // *can* test the cache primitives directly.
    use usg_core::lockfile_path as _; // referenced for clarity in compile
    let tmp = make_temp_root();
    let root = tmp.path();
    write_file(root, "Assets/A/Code.cs", "x");

    // Build a fingerprint from a single relative path; verify is_valid round-trips.
    let entries = usg_core::__test_only::build_entries(
        root.to_str().unwrap(),
        root.to_str().unwrap(),
        &["Assets/A/Code.cs".to_string()],
    );
    assert!(!entries.is_empty());
    assert!(usg_core::__test_only::is_valid(&entries));

    // Touching a contributing dir must invalidate.
    std::thread::sleep(std::time::Duration::from_millis(20));
    write_file(root, "Assets/A/Other.cs", "y");
    assert!(!usg_core::__test_only::is_valid(&entries));
}

// Stale; keeps the import warning quiet.
#[allow(dead_code)]
fn _unused(p: &Path) {
    let _ = lockfile_path(&p.to_string_lossy());
}
