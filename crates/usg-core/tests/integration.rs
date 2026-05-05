//! Integration tests ported from `Tests/SolutionGeneratorTests.swift`.

mod common;

use std::collections::BTreeMap;
use std::path::Path;

use common::{assert_compile_set, make_temp_root, read_compile_set, read_file, write_file};
use usg_core::{
    BuildConfig, BuildPlatform, DllRef, GenerateOptions, Lockfile, LockfileIO, ProjectScanner,
    RefCategory, SolutionGenerator,
    defines::{generate_version_defines, parse_scripting_defines},
    json::extract_json_object_keys,
    solution_generator::render_directory_build_props,
};

const GR: &str = "tpl";

fn opts(root: &Path, platform: BuildPlatform, cfg: BuildConfig) -> GenerateOptions {
    GenerateOptions::new(root.to_string_lossy().into_owned(), platform)
        .with_generator_root(GR)
        .with_build_config(cfg)
}

/// Bare lockfile shell used by tests that previously exercised the (now-deleted)
/// template path. They only care about scanner/owner/render behaviour, not lockfile
/// content — `Lockfile::empty` gives them a valid input with no refs and no defines.
fn bare_lockfile() -> Lockfile {
    Lockfile::empty("test", "/test")
}

#[test]
fn nested_assembly_root_mapping_and_legacy_fallback() {
    let tmp = make_temp_root();
    let root = tmp.path();
    write_file(
        root,
        "Assets/SystemAssets/Assemblies/Main/Main.asmdef",
        r#"{"name":"Main","references":["Core"]}"#,
    );
    write_file(
        root,
        "Assets/SystemAssets/Assemblies/Core/Core.asmdef",
        r#"{"name":"Core"}"#,
    );
    write_file(
        root,
        "Assets/SystemAssets/Assemblies/Tests/Tests.asmdef",
        r#"{"name":"Tests","references":["Main"]}"#,
    );

    write_file(root, "Assets/Game/Assembly.asmref", r#"{"reference":"Main"}"#);
    write_file(
        root,
        "Assets/Game/Core/Assembly.asmref",
        r#"{"reference":"Core"}"#,
    );
    write_file(
        root,
        "Assets/Game/Tests/Assembly.asmref",
        r#"{"reference":"Tests"}"#,
    );

    write_file(root, "Assets/Game/Foo.cs", "class Foo {}\n");
    write_file(
        root,
        "Assets/Game/Feature/SubFeature/Fizz.cs",
        "class Fizz {}\n",
    );
    write_file(root, "Assets/Game/Core/Bar.cs", "class Bar {}\n");
    write_file(root, "Assets/Game/Tests/Baz.cs", "class Baz {}\n");
    write_file(root, "Assets/Plugins/Legacy.cs", "class Legacy {}\n");

    SolutionGenerator::new()
        .generate_from_lockfile(&opts(root, BuildPlatform::Ios, BuildConfig::Editor), &bare_lockfile())
        .unwrap();

    let v = "tpl/ios-editor";
    assert_compile_set(
        root,
        &format!("{}/Main.csproj", v),
        &["Assets/Game/Foo.cs", "Assets/Game/Feature/SubFeature/Fizz.cs"],
    );
    assert_compile_set(root, &format!("{}/Core.csproj", v), &["Assets/Game/Core/Bar.cs"]);
    assert_compile_set(root, &format!("{}/Tests.csproj", v), &["Assets/Game/Tests/Baz.cs"]);
    assert_compile_set(
        root,
        &format!("{}/Assembly-CSharp-firstpass.csproj", v),
        &["Assets/Plugins/Legacy.cs"],
    );

    let main = read_file(root, &format!("{}/Main.csproj", v));
    assert!(main.contains("<ProjectReference Include=\"Core.csproj\">"));
    let tests = read_file(root, &format!("{}/Tests.csproj", v));
    assert!(tests.contains("<ProjectReference Include=\"Main.csproj\">"));
}

#[test]
fn asm_ref_name_resolution_and_tilde_skip() {
    let tmp = make_temp_root();
    let root = tmp.path();
    write_file(
        root,
        "Assets/SystemAssets/Assemblies/Core/Core.asmdef",
        r#"{"name":"Core"}"#,
    );
    write_file(root, "Assets/Game/Assembly.asmref", r#"{"reference":"Core"}"#);
    write_file(root, "Assets/Game/Good.cs", "class Good {}\n");
    write_file(
        root,
        "Packages/com.example/src~/Hidden.cs",
        "class Hidden {}\n",
    );

    SolutionGenerator::new()
        .generate_from_lockfile(&opts(root, BuildPlatform::Ios, BuildConfig::Editor), &bare_lockfile())
        .unwrap();
    assert_compile_set(
        root,
        "tpl/ios-editor/Core.csproj",
        &["Assets/Game/Good.cs"],
    );
}

#[test]
fn tilde_directory_excluded_from_scan() {
    let tmp = make_temp_root();
    let root = tmp.path();
    write_file(
        root,
        "Assets/SystemAssets/Assemblies/Main/Main.asmdef",
        r#"{"name":"Main"}"#,
    );
    write_file(root, "Assets/Game/Assembly.asmref", r#"{"reference":"Main"}"#);
    write_file(root, "Assets/Game/Good.cs", "class Good {}\n");
    write_file(root, "Assets/Game/src~/Hidden.cs", "class Hidden {}\n");
    write_file(root, "Assets/Game/backup~/Old.cs", "class Old {}\n");

    SolutionGenerator::new()
        .generate_from_lockfile(&opts(root, BuildPlatform::Ios, BuildConfig::Editor), &bare_lockfile())
        .unwrap();
    assert_compile_set(
        root,
        "tpl/ios-editor/Main.csproj",
        &["Assets/Game/Good.cs"],
    );
}

#[test]
fn dot_directory_excluded_from_scan() {
    let tmp = make_temp_root();
    let root = tmp.path();
    write_file(
        root,
        "Assets/SystemAssets/Assemblies/Main/Main.asmdef",
        r#"{"name":"Main"}"#,
    );
    write_file(root, "Assets/Game/Assembly.asmref", r#"{"reference":"Main"}"#);
    write_file(root, "Assets/Game/Visible.cs", "class Visible {}\n");
    write_file(root, "Assets/Game/.hidden/Secret.cs", "class Secret {}\n");

    SolutionGenerator::new()
        .generate_from_lockfile(&opts(root, BuildPlatform::Ios, BuildConfig::Editor), &bare_lockfile())
        .unwrap();
    assert_compile_set(
        root,
        "tpl/ios-editor/Main.csproj",
        &["Assets/Game/Visible.cs"],
    );
}

#[test]
fn prod_variant_category_filtering() {
    let tmp = make_temp_root();
    let root = tmp.path();
    write_file(
        root,
        "Assets/Assemblies/Runtime/Runtime.asmdef",
        r#"{"name":"Runtime","references":["MyEditor"]}"#,
    );
    write_file(
        root,
        "Assets/Assemblies/MyEditor/MyEditor.asmdef",
        r#"{"name":"MyEditor","includePlatforms":["Editor"]}"#,
    );
    write_file(
        root,
        "Assets/Assemblies/MyTests/MyTests.asmdef",
        r#"{"name":"MyTests","defineConstraints":["UNITY_INCLUDE_TESTS"]}"#,
    );

    write_file(root, "Assets/Assemblies/Runtime/Foo.cs", "class Foo {}\n");
    write_file(root, "Assets/Assemblies/MyEditor/Bar.cs", "class Bar {}\n");
    write_file(root, "Assets/Assemblies/MyTests/Baz.cs", "class Baz {}\n");

    let g = SolutionGenerator::new();
    let prod = g
        .generate_from_lockfile(&opts(root, BuildPlatform::Ios, BuildConfig::Prod), &bare_lockfile())
        .unwrap();
    assert_eq!(prod.variant_csprojs, vec!["tpl/ios-prod/Runtime.csproj"]);
    let prod_props = read_file(root, "tpl/ios-prod/Directory.Build.props");
    assert!(!prod_props.contains("UNITY_EDITOR"));
    assert!(prod_props.contains("UNITY_IOS"));
    let runtime = read_file(root, "tpl/ios-prod/Runtime.csproj");
    assert!(!runtime.contains("MyEditor.csproj\">"));
    let prod_sln = read_file(root, &prod.variant_sln_path);
    assert!(prod_sln.contains("\"Runtime\""));
    assert!(!prod_sln.contains("\"MyEditor\""));

    let editor = g
        .generate_from_lockfile(&opts(root, BuildPlatform::Ios, BuildConfig::Editor), &bare_lockfile())
        .unwrap();
    assert_eq!(editor.variant_csprojs.len(), 3);
    let editor_props = read_file(root, "tpl/ios-editor/Directory.Build.props");
    assert!(editor_props.contains("UNITY_EDITOR"));
    let editor_sln = read_file(root, &editor.variant_sln_path);
    assert!(editor_sln.contains("\"MyEditor\""));
    assert!(editor_sln.contains("\"MyTests\""));
}

fn make_minimal_lockfile() -> Lockfile {
    let mut refs: BTreeMap<RefCategory, Vec<DllRef>> = BTreeMap::new();
    refs.insert(
        RefCategory::Engine,
        vec![
            DllRef::new(
                "UnityEngine",
                "$(UnityPath)/Unity.app/Contents/Managed/UnityEngine/UnityEngine.dll",
            ),
            DllRef::new(
                "UnityEngine.CoreModule",
                "$(UnityPath)/Unity.app/Contents/Managed/UnityEngine/UnityEngine.CoreModule.dll",
            ),
        ],
    );
    refs.insert(
        RefCategory::Editor,
        vec![DllRef::new(
            "UnityEditor",
            "$(UnityPath)/Unity.app/Contents/Managed/UnityEngine/UnityEditor.dll",
        )],
    );
    refs.insert(
        RefCategory::Netstandard,
        vec![DllRef::new(
            "netstandard",
            "$(UnityPath)/Unity.app/Contents/NetStandard/ref/2.1.0/netstandard.dll",
        )],
    );
    refs.insert(
        RefCategory::PlaybackIos,
        vec![DllRef::new(
            "UnityEditor.iOS.Extensions",
            "$(UnityPath)/PlaybackEngines/iOSSupport/UnityEditor.iOS.Extensions.dll",
        )],
    );
    refs.insert(
        RefCategory::PlaybackAndroid,
        vec![DllRef::new(
            "UnityEditor.Android.Extensions",
            "$(UnityPath)/PlaybackEngines/AndroidPlayer/UnityEditor.Android.Extensions.dll",
        )],
    );
    refs.insert(
        RefCategory::Project,
        vec![DllRef::new(
            "Firebase.App",
            "$(ProjectRoot)/Packages/com.google.firebase.app-pkg/Firebase/Plugins/Firebase.App.dll",
        )],
    );
    Lockfile {
        unity_version: "6000.2.7f2".into(),
        unity_path: "/test/unity".into(),
        lang_version: "9.0".into(),
        analyzers: vec![
            "$(UnityPath)/Unity.app/Contents/Tools/Unity.SourceGenerators/Unity.SourceGenerators.dll"
                .into(),
        ],
        refs,
        defines: vec!["UNITY_6000".into(), "ENABLE_AR".into()],
        defines_scripting: vec!["ODIN_INSPECTOR".into()],
    }
}

#[test]
fn lockfile_round_trip() {
    let tmp = make_temp_root();
    let root = tmp.path();
    let lockfile_path = root.join("csproj.lock");
    let lf = make_minimal_lockfile();
    LockfileIO::write(&lf, lockfile_path.to_str().unwrap()).unwrap();
    let reloaded = LockfileIO::read(lockfile_path.to_str().unwrap()).unwrap();

    assert_eq!(reloaded.unity_version, lf.unity_version);
    assert_eq!(reloaded.unity_path, lf.unity_path);
    assert_eq!(reloaded.lang_version, lf.lang_version);
    assert_eq!(reloaded.analyzers, lf.analyzers);
    for cat in RefCategory::ALL {
        let a: Vec<_> = reloaded.refs_for(cat).iter().map(|r| &r.name).collect();
        let b: Vec<_> = lf.refs_for(cat).iter().map(|r| &r.name).collect();
        assert_eq!(a, b, "{:?}", cat);
    }
    assert_eq!(reloaded.total_ref_count(), lf.total_ref_count());
    assert_eq!(reloaded.defines, lf.defines);
    assert_eq!(reloaded.defines_scripting, lf.defines_scripting);

    // Idempotent serialization
    let first = std::fs::read_to_string(&lockfile_path).unwrap();
    let second_path = root.join("csproj2.lock");
    LockfileIO::write(&reloaded, second_path.to_str().unwrap()).unwrap();
    let second = std::fs::read_to_string(&second_path).unwrap();
    assert_eq!(first, second);
}

#[test]
fn version_defines_generation() {
    let d = generate_version_defines("6000.2.7f2");
    assert!(d.contains(&"UNITY_6000_2_7".to_string()));
    assert!(d.contains(&"UNITY_6000_2".to_string()));
    assert!(d.contains(&"UNITY_6000".to_string()));
    assert!(d.contains(&"UNITY_5_3_OR_NEWER".to_string()));
    assert!(d.contains(&"UNITY_5_6_OR_NEWER".to_string()));
    assert!(d.contains(&"UNITY_2017_1_OR_NEWER".to_string()));
    assert!(d.contains(&"UNITY_2022_3_OR_NEWER".to_string()));
    assert!(d.contains(&"UNITY_2023_3_OR_NEWER".to_string()));
    assert!(d.contains(&"UNITY_6000_0_OR_NEWER".to_string()));
    assert!(d.contains(&"UNITY_6000_2_OR_NEWER".to_string()));
    assert!(!d.contains(&"UNITY_6000_3_OR_NEWER".to_string()));
    let unique: std::collections::HashSet<_> = d.iter().collect();
    assert_eq!(unique.len(), d.len());
}

#[test]
fn version_defines_for_older_version() {
    let d = generate_version_defines("2022.3.10f1");
    assert!(d.contains(&"UNITY_2022_3_10".to_string()));
    assert!(d.contains(&"UNITY_2022_3".to_string()));
    assert!(d.contains(&"UNITY_2022".to_string()));
    assert!(d.contains(&"UNITY_2022_3_OR_NEWER".to_string()));
    assert!(d.contains(&"UNITY_2022_1_OR_NEWER".to_string()));
    assert!(d.contains(&"UNITY_5_3_OR_NEWER".to_string()));
    assert!(!d.contains(&"UNITY_6000_0_OR_NEWER".to_string()));
    assert!(!d.contains(&"UNITY_2023_1_OR_NEWER".to_string()));
}

#[test]
fn scripting_defines_parsing() {
    let tmp = make_temp_root();
    let root = tmp.path();
    write_file(
        root,
        "ProjectSettings/ProjectSettings.asset",
        "%YAML 1.1\n%TAG !u! tag:unity3d.com,2011:\n--- !u!129 &1\nPlayerSettings:\n  scriptingDefineSymbols:\n    Android: ENABLE_SPAN_T;ODIN_INSPECTOR;CUSTOM_DEFINE\n    iPhone: ENABLE_SPAN_T;ODIN_INSPECTOR\n    Standalone: ENABLE_SPAN_T\n  someOtherSetting: true\n",
    );
    let d = parse_scripting_defines(root.to_str().unwrap());
    assert!(d.contains(&"ENABLE_SPAN_T".to_string()));
    assert!(d.contains(&"ODIN_INSPECTOR".to_string()));
    assert!(d.contains(&"CUSTOM_DEFINE".to_string()));
    assert_eq!(d.len(), 3);
}

#[test]
fn lockfile_generate_references_in_csproj() {
    let tmp = make_temp_root();
    let root = tmp.path();
    let lf = make_minimal_lockfile();

    write_file(
        root,
        "Assets/Assemblies/Main/Main.asmdef",
        r#"{"name":"Main","references":["Lib"]}"#,
    );
    write_file(
        root,
        "Assets/Assemblies/Lib/Lib.asmdef",
        r#"{"name":"Lib"}"#,
    );
    write_file(root, "Assets/Assemblies/Main/Foo.cs", "class Foo {}\n");
    write_file(root, "Assets/Assemblies/Lib/Bar.cs", "class Bar {}\n");

    let r = SolutionGenerator::new()
        .generate_from_lockfile(&opts(root, BuildPlatform::Ios, BuildConfig::Editor), &lf)
        .unwrap();
    assert_eq!(r.variant_csprojs.len(), 2);

    let main = read_file(root, "tpl/ios-editor/Main.csproj");
    assert!(main.contains("<Reference Include=\"UnityEngine\">"));
    assert!(main.contains("<Reference Include=\"UnityEngine.CoreModule\">"));
    assert!(main.contains("<Reference Include=\"UnityEditor\">"));
    assert!(main.contains("<Reference Include=\"netstandard\">"));
    assert!(main.contains("<Analyzer Include="));
    assert!(main.contains("<ProjectReference Include=\"Lib.csproj\">"));
    assert!(main.contains("<Compile Include="));
    assert!(main.contains("UnityEditor.iOS.Extensions"));
    assert!(!main.contains("UnityEditor.Android.Extensions"));
}

#[test]
fn lockfile_generate_editor_refs_not_in_prod() {
    let tmp = make_temp_root();
    let root = tmp.path();
    let lf = make_minimal_lockfile();
    write_file(
        root,
        "Assets/Assemblies/Runtime/Runtime.asmdef",
        r#"{"name":"Runtime"}"#,
    );
    write_file(root, "Assets/Assemblies/Runtime/Code.cs", "class Code {}\n");

    let r = SolutionGenerator::new()
        .generate_from_lockfile(&opts(root, BuildPlatform::Android, BuildConfig::Prod), &lf)
        .unwrap();
    assert_eq!(r.variant_csprojs.len(), 1);
    let csproj = read_file(root, "tpl/android-prod/Runtime.csproj");
    assert!(csproj.contains("<Reference Include=\"UnityEngine\">"));
    assert!(!csproj.contains("<Reference Include=\"UnityEditor\">"));
    assert!(csproj.contains("UnityEditor.Android.Extensions"));
    assert!(!csproj.contains("UnityEditor.iOS.Extensions"));
}

#[test]
fn lockfile_generate_allow_unsafe_blocks() {
    let tmp = make_temp_root();
    let root = tmp.path();
    let lf = make_minimal_lockfile();
    write_file(
        root,
        "Assets/Assemblies/SafeLib/SafeLib.asmdef",
        r#"{"name":"SafeLib"}"#,
    );
    write_file(
        root,
        "Assets/Assemblies/UnsafeLib/UnsafeLib.asmdef",
        r#"{"name":"UnsafeLib","allowUnsafeCode":true}"#,
    );
    write_file(root, "Assets/Assemblies/SafeLib/S.cs", "class S {}\n");
    write_file(root, "Assets/Assemblies/UnsafeLib/U.cs", "class U {}\n");

    SolutionGenerator::new()
        .generate_from_lockfile(&opts(root, BuildPlatform::Ios, BuildConfig::Editor), &lf)
        .unwrap();

    let safe = read_file(root, "tpl/ios-editor/SafeLib.csproj");
    assert!(safe.contains("<AllowUnsafeBlocks>False</AllowUnsafeBlocks>"));
    let unsafe_csproj = read_file(root, "tpl/ios-editor/UnsafeLib.csproj");
    assert!(unsafe_csproj.contains("<AllowUnsafeBlocks>True</AllowUnsafeBlocks>"));
}

#[test]
fn lockfile_generate_defines_in_props() {
    let tmp = make_temp_root();
    let root = tmp.path();
    let lf = Lockfile {
        unity_version: "6000.2.7f2".into(),
        unity_path: "/test/unity".into(),
        lang_version: "9.0".into(),
        analyzers: Vec::new(),
        refs: BTreeMap::new(),
        defines: vec!["UNITY_6000".into(), "ENABLE_AR".into()],
        defines_scripting: vec!["ODIN_INSPECTOR".into()],
    };
    write_file(root, "Assets/Assemblies/Lib/Lib.asmdef", r#"{"name":"Lib"}"#);
    write_file(root, "Assets/Assemblies/Lib/Code.cs", "class Code {}\n");

    SolutionGenerator::new()
        .generate_from_lockfile(&opts(root, BuildPlatform::Ios, BuildConfig::Editor), &lf)
        .unwrap();
    let props = read_file(root, "tpl/ios-editor/Directory.Build.props");
    assert!(props.contains("UNITY_6000"));
    assert!(props.contains("ENABLE_AR"));
    assert!(props.contains("ODIN_INSPECTOR"));
    assert!(props.contains("UNITY_IOS"));
    assert!(props.contains("UNITY_EDITOR"));
    assert!(props.contains("DEBUG"));
}

#[test]
fn lockfile_generate_source_patterns_preserved() {
    let tmp = make_temp_root();
    let root = tmp.path();
    let lf = make_minimal_lockfile();
    write_file(root, "Assets/Assemblies/Main/Main.asmdef", r#"{"name":"Main"}"#);
    write_file(root, "Assets/Game/Assembly.asmref", r#"{"reference":"Main"}"#);
    write_file(root, "Assets/Assemblies/Main/A.cs", "class A {}\n");
    write_file(root, "Assets/Game/B.cs", "class B {}\n");
    write_file(root, "Assets/Game/Sub/C.cs", "class C {}\n");

    SolutionGenerator::new()
        .generate_from_lockfile(&opts(root, BuildPlatform::Ios, BuildConfig::Editor), &lf)
        .unwrap();
    assert_compile_set(
        root,
        "tpl/ios-editor/Main.csproj",
        &[
            "Assets/Assemblies/Main/A.cs",
            "Assets/Game/B.cs",
            "Assets/Game/Sub/C.cs",
        ],
    );
}

#[test]
fn lockfile_generate_platform_filtering() {
    let tmp = make_temp_root();
    let root = tmp.path();
    let lf = make_minimal_lockfile();
    write_file(
        root,
        "Assets/Assemblies/IOSOnly/IOSOnly.asmdef",
        r#"{"name":"IOSOnly","includePlatforms":["iOS"]}"#,
    );
    write_file(
        root,
        "Assets/Assemblies/AllPlatforms/AllPlatforms.asmdef",
        r#"{"name":"AllPlatforms"}"#,
    );
    write_file(root, "Assets/Assemblies/IOSOnly/Code.cs", "class IOSCode {}\n");
    write_file(
        root,
        "Assets/Assemblies/AllPlatforms/Code.cs",
        "class AllCode {}\n",
    );
    let g = SolutionGenerator::new();
    let ios = g
        .generate_from_lockfile(&opts(root, BuildPlatform::Ios, BuildConfig::Prod), &lf)
        .unwrap();
    let names: std::collections::HashSet<String> = ios
        .variant_csprojs
        .iter()
        .map(|p| {
            p.rsplit('/')
                .next()
                .unwrap()
                .strip_suffix(".csproj")
                .unwrap()
                .to_string()
        })
        .collect();
    assert!(names.contains("IOSOnly"));
    assert!(names.contains("AllPlatforms"));

    let android = g
        .generate_from_lockfile(&opts(root, BuildPlatform::Android, BuildConfig::Prod), &lf)
        .unwrap();
    let names: std::collections::HashSet<String> = android
        .variant_csprojs
        .iter()
        .map(|p| {
            p.rsplit('/')
                .next()
                .unwrap()
                .strip_suffix(".csproj")
                .unwrap()
                .to_string()
        })
        .collect();
    assert!(!names.contains("IOSOnly"));
    assert!(names.contains("AllPlatforms"));
}

#[test]
fn lockfile_generate_asmdef_version_defines() {
    let tmp = make_temp_root();
    let root = tmp.path();
    write_file(
        root,
        "Assets/Assemblies/Lib/Lib.asmdef",
        r#"{"name":"Lib","versionDefines":[{"name":"com.unity.modules.physics2d","expression":"","define":"PACKAGE_PHYSICS2D"},{"name":"Unity","expression":"","define":"MY_FEATURE"}]}"#,
    );
    write_file(root, "Assets/Assemblies/Lib/Code.cs", "class Code {}\n");
    let scan = ProjectScanner::scan(root.to_str().unwrap(), GR).unwrap();
    let asm = scan.asm_def_by_name.get("Lib").unwrap();
    assert_eq!(asm.version_defines.len(), 2);
    assert_eq!(asm.version_defines[0].package_name, "com.unity.modules.physics2d");
    assert_eq!(asm.version_defines[0].define, "PACKAGE_PHYSICS2D");
    assert_eq!(asm.version_defines[1].define, "MY_FEATURE");
}

#[test]
fn lockfile_generate_csproj_xml_well_formed() {
    let tmp = make_temp_root();
    let root = tmp.path();
    let lf = make_minimal_lockfile();
    write_file(root, "Assets/Assemblies/Main/Main.asmdef", r#"{"name":"Main"}"#);
    write_file(root, "Assets/Assemblies/Main/Code.cs", "class Code {}\n");
    SolutionGenerator::new()
        .generate_from_lockfile(&opts(root, BuildPlatform::Ios, BuildConfig::Editor), &lf)
        .unwrap();
    let csproj = read_file(root, "tpl/ios-editor/Main.csproj");
    assert!(csproj.starts_with("<?xml version=\"1.0\""));
    assert!(csproj.contains("<Project ToolsVersion=\"4.0\""));
    assert!(csproj.contains("</Project>"));
    assert!(csproj.contains("<AssemblyName>Main</AssemblyName>"));
    assert!(csproj.contains("<LangVersion>9.0</LangVersion>"));
    assert!(csproj.contains("<TargetFrameworkVersion>v4.7.1</TargetFrameworkVersion>"));
    assert!(csproj.contains("<NoStdLib>true</NoStdLib>"));
    assert!(csproj.contains("<Import Project=\"$(MSBuildToolsPath)"));
    let opens = csproj.matches("<ItemGroup>").count();
    let closes = csproj.matches("</ItemGroup>").count();
    assert_eq!(opens, closes);
}

#[test]
fn extract_json_object_keys_test() {
    let json = r#"{
      "dependencies": {
        "com.unity.modules.audio": "1.0.0",
        "com.unity.modules.physics2d": "2.0.0",
        "singular-unity-package": "3.1.0"
      }
    }"#;
    let keys: std::collections::HashSet<String> =
        extract_json_object_keys(json, "dependencies").into_iter().collect();
    let expected: std::collections::HashSet<String> = [
        "com.unity.modules.audio",
        "com.unity.modules.physics2d",
        "singular-unity-package",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    assert_eq!(keys, expected);
}

#[test]
fn scan_cache_invalidates_on_new_cs_file() {
    let tmp = make_temp_root();
    let root = tmp.path();
    let lf = make_minimal_lockfile();
    write_file(root, "Assets/Assemblies/Main/Main.asmdef", r#"{"name":"Main"}"#);
    write_file(root, "Assets/Assemblies/Main/A.cs", "class A {}\n");

    SolutionGenerator::new()
        .generate_from_lockfile(&opts(root, BuildPlatform::Ios, BuildConfig::Editor), &lf)
        .unwrap();
    assert_compile_set(
        root,
        "tpl/ios-editor/Main.csproj",
        &["Assets/Assemblies/Main/A.cs"],
    );

    // Bump the parent dir's mtime by sleeping, then add a file.
    std::thread::sleep(std::time::Duration::from_millis(10));
    write_file(
        root,
        "Assets/Assemblies/Main/Sub/B.cs",
        "class B {}\n",
    );

    SolutionGenerator::new()
        .generate_from_lockfile(&opts(root, BuildPlatform::Ios, BuildConfig::Editor), &lf)
        .unwrap();
    assert_compile_set(
        root,
        "tpl/ios-editor/Main.csproj",
        &[
            "Assets/Assemblies/Main/A.cs",
            "Assets/Assemblies/Main/Sub/B.cs",
        ],
    );
}

#[test]
fn project_root_is_absolute_in_props() {
    let tmp = make_temp_root();
    let root = tmp.path();
    let lf = make_minimal_lockfile();
    write_file(root, "Assets/Assemblies/Lib/Lib.asmdef", r#"{"name":"Lib"}"#);
    write_file(root, "Assets/Assemblies/Lib/Code.cs", "class Code {}\n");

    SolutionGenerator::new()
        .generate_from_lockfile(&opts(root, BuildPlatform::Ios, BuildConfig::Editor), &lf)
        .unwrap();
    let props = read_file(root, "tpl/ios-editor/Directory.Build.props");
    assert!(
        props.contains("<ProjectRoot>/"),
        "ProjectRoot must be absolute, got: {}",
        props
    );
}

#[test]
fn build_platform_unity_name() {
    assert_eq!(BuildPlatform::Ios.unity_platform_name(), "iOS");
    assert_eq!(BuildPlatform::Android.unity_platform_name(), "Android");
    assert_eq!(BuildPlatform::Osx.unity_platform_name(), "macOSStandalone");
}

#[test]
fn render_directory_build_props_unified() {
    let with_unity = render_directory_build_props(
        "/project",
        Some("/unity"),
        BuildPlatform::Ios,
        BuildConfig::Editor,
        &["CUSTOM".to_string()],
    );
    assert!(with_unity.contains("<UnityPath>/unity</UnityPath>"));
    assert!(with_unity.contains("CUSTOM"));
    assert!(with_unity.contains("UNITY_IOS"));
    assert!(with_unity.contains("UNITY_EDITOR"));

    let without = render_directory_build_props(
        "/project",
        None,
        BuildPlatform::Android,
        BuildConfig::Prod,
        &[],
    );
    assert!(!without.contains("<UnityPath>"));
    assert!(without.contains("UNITY_ANDROID"));
    assert!(!without.contains("UNITY_EDITOR"));
}

#[test]
fn output_dot_produces_root_output() {
    let tmp = make_temp_root();
    let root = tmp.path();
    let lf = make_minimal_lockfile();
    write_file(root, "Assets/Assemblies/Main/Main.asmdef", r#"{"name":"Main"}"#);
    write_file(root, "Assets/Game/Assembly.asmref", r#"{"reference":"Main"}"#);
    write_file(root, "Assets/Assemblies/Main/A.cs", "class A {}\n");
    write_file(root, "Assets/Game/B.cs", "class B {}\n");

    let r = SolutionGenerator::new()
        .generate_from_lockfile(
            &opts(root, BuildPlatform::Ios, BuildConfig::Editor).with_output_dir(Some(".")),
            &lf,
        )
        .unwrap();
    assert_eq!(r.variant_csprojs, vec!["Main.csproj"]);
    assert!(r.variant_sln_path.ends_with(".sln"));
    assert!(!r.variant_sln_path.contains('/'));
    let csproj = read_file(root, "Main.csproj");
    assert!(csproj.contains("Assets/Assemblies/Main/*.cs"));
    assert!(csproj.contains("Assets/Game/*.cs"));
    assert!(!csproj.contains("../"));
}

#[test]
fn output_deep_path_produces_correct_prefix() {
    let tmp = make_temp_root();
    let root = tmp.path();
    let lf = make_minimal_lockfile();
    write_file(root, "Assets/Assemblies/Main/Main.asmdef", r#"{"name":"Main"}"#);
    write_file(root, "Assets/Assemblies/Main/A.cs", "class A {}\n");

    let out = "Library/com.example/deep/output";
    let r = SolutionGenerator::new()
        .generate_from_lockfile(
            &opts(root, BuildPlatform::Ios, BuildConfig::Editor).with_output_dir(Some(out)),
            &lf,
        )
        .unwrap();
    assert_eq!(r.variant_csprojs, vec![format!("{}/Main.csproj", out)]);
    assert!(r.variant_sln_path.starts_with(&format!("{}/", out)));
    let csproj = read_file(root, &format!("{}/Main.csproj", out));
    assert!(csproj.contains("../../../../Assets/Assemblies/Main/*.cs"));
}

#[test]
fn output_single_dir_depth_one() {
    let tmp = make_temp_root();
    let root = tmp.path();
    let lf = make_minimal_lockfile();
    write_file(root, "Assets/Assemblies/Lib/Lib.asmdef", r#"{"name":"Lib"}"#);
    write_file(root, "Assets/Assemblies/Lib/Code.cs", "class Code {}\n");

    let r = SolutionGenerator::new()
        .generate_from_lockfile(
            &opts(root, BuildPlatform::Ios, BuildConfig::Editor).with_output_dir(Some("output")),
            &lf,
        )
        .unwrap();
    assert_eq!(r.variant_csprojs, vec!["output/Lib.csproj"]);
    let csproj = read_file(root, "output/Lib.csproj");
    assert!(csproj.contains("../Assets/Assemblies/Lib/*.cs"));
}

#[test]
fn default_output_unchanged() {
    let tmp = make_temp_root();
    let root = tmp.path();
    let lf = make_minimal_lockfile();
    write_file(root, "Assets/Assemblies/Main/Main.asmdef", r#"{"name":"Main"}"#);
    write_file(root, "Assets/Assemblies/Main/A.cs", "class A {}\n");

    let r = SolutionGenerator::new()
        .generate_from_lockfile(&opts(root, BuildPlatform::Ios, BuildConfig::Editor), &lf)
        .unwrap();
    assert_eq!(r.variant_csprojs, vec!["tpl/ios-editor/Main.csproj"]);
    let csproj = read_file(root, "tpl/ios-editor/Main.csproj");
    // generator_root="tpl" → depth = 1+1 = 2 → "../../"
    assert!(csproj.contains("../../Assets/Assemblies/Main/*.cs"));
}

#[test]
fn extra_refs_appear_in_csproj() {
    let tmp = make_temp_root();
    let root = tmp.path();
    let lf = make_minimal_lockfile();
    write_file(root, "Assets/Assemblies/Main/Main.asmdef", r#"{"name":"Main"}"#);
    write_file(root, "Assets/Assemblies/Main/Code.cs", "class Code {}\n");

    let extra = vec![
        DllRef::new(
            "SingularityGroup.HotReload.RuntimeDependencies",
            "/path/to/SingularityGroup.HotReload.RuntimeDependencies.dll",
        ),
        DllRef::new(
            "SingularityGroup.HotReload.RuntimeDependencies2022",
            "/path/to/SingularityGroup.HotReload.RuntimeDependencies2022.dll",
        ),
    ];

    SolutionGenerator::new()
        .generate_from_lockfile(
            &opts(root, BuildPlatform::Ios, BuildConfig::Editor).with_extra_refs(extra),
            &lf,
        )
        .unwrap();
    let csproj = read_file(root, "tpl/ios-editor/Main.csproj");
    assert!(csproj.contains("<Reference Include=\"SingularityGroup.HotReload.RuntimeDependencies\">"));
    assert!(csproj.contains("<HintPath>/path/to/SingularityGroup.HotReload.RuntimeDependencies.dll</HintPath>"));
    assert!(csproj.contains("<Reference Include=\"SingularityGroup.HotReload.RuntimeDependencies2022\">"));
}

#[test]
fn extra_refs_deduplicated_with_lockfile_refs() {
    let tmp = make_temp_root();
    let root = tmp.path();
    let lf = make_minimal_lockfile();
    write_file(root, "Assets/Assemblies/Main/Main.asmdef", r#"{"name":"Main"}"#);
    write_file(root, "Assets/Assemblies/Main/Code.cs", "class Code {}\n");

    let extra = vec![DllRef::new("UnityEngine", "/duplicate/UnityEngine.dll")];
    SolutionGenerator::new()
        .generate_from_lockfile(
            &opts(root, BuildPlatform::Ios, BuildConfig::Editor).with_extra_refs(extra),
            &lf,
        )
        .unwrap();
    let csproj = read_file(root, "tpl/ios-editor/Main.csproj");
    let count = csproj.matches("Include=\"UnityEngine\"").count();
    assert_eq!(count, 1);
}

#[test]
fn extra_refs_with_output_dir() {
    let tmp = make_temp_root();
    let root = tmp.path();
    let lf = make_minimal_lockfile();
    write_file(root, "Assets/Assemblies/Main/Main.asmdef", r#"{"name":"Main"}"#);
    write_file(root, "Assets/Assemblies/Main/Code.cs", "class Code {}\n");
    let extra = vec![DllRef::new("TestLib", "/path/to/TestLib.dll")];
    let out = "Library/hotreload/Solution";

    SolutionGenerator::new()
        .generate_from_lockfile(
            &opts(root, BuildPlatform::Ios, BuildConfig::Editor)
                .with_output_dir(Some(out))
                .with_extra_refs(extra),
            &lf,
        )
        .unwrap();
    let csproj = read_file(root, &format!("{}/Main.csproj", out));
    assert!(csproj.contains("<Reference Include=\"TestLib\">"));
    assert!(csproj.contains("../../../Assets/Assemblies/Main/*.cs"));
}

#[test]
fn category_inference_from_asmdef_fields() {
    let tmp = make_temp_root();
    let root = tmp.path();
    write_file(root, "Assets/A/Runtime.asmdef", r#"{"name":"Runtime"}"#);
    write_file(
        root,
        "Assets/B/PlatformLib.asmdef",
        r#"{"name":"PlatformLib","includePlatforms":["iOS","Editor"]}"#,
    );
    write_file(
        root,
        "Assets/C/EditorOnly.asmdef",
        r#"{"name":"EditorOnly","includePlatforms":["Editor"]}"#,
    );
    write_file(
        root,
        "Assets/D/EditorConstrained.asmdef",
        r#"{"name":"EditorConstrained","defineConstraints":["UNITY_EDITOR"]}"#,
    );
    write_file(
        root,
        "Assets/E/PlayTests.asmdef",
        r#"{"name":"PlayTests","defineConstraints":["UNITY_INCLUDE_TESTS"]}"#,
    );
    for c in ["A", "B", "C", "D", "E"] {
        write_file(root, &format!("Assets/{}/Code.cs", c), "class Code {}\n");
    }

    let g = SolutionGenerator::new();
    let prod = g
        .generate_from_lockfile(&opts(root, BuildPlatform::Ios, BuildConfig::Prod), &bare_lockfile())
        .unwrap();
    let prod_names: std::collections::HashSet<String> = prod
        .variant_csprojs
        .iter()
        .map(|p| {
            p.rsplit('/')
                .next()
                .unwrap()
                .strip_suffix(".csproj")
                .unwrap()
                .to_string()
        })
        .collect();
    assert_eq!(
        prod_names,
        ["Runtime", "PlatformLib"]
            .iter()
            .map(|s| s.to_string())
            .collect()
    );

    let editor = g
        .generate_from_lockfile(&opts(root, BuildPlatform::Ios, BuildConfig::Editor), &bare_lockfile())
        .unwrap();
    let editor_names: std::collections::HashSet<String> = editor
        .variant_csprojs
        .iter()
        .map(|p| {
            p.rsplit('/')
                .next()
                .unwrap()
                .strip_suffix(".csproj")
                .unwrap()
                .to_string()
        })
        .collect();
    assert_eq!(
        editor_names,
        [
            "Runtime",
            "PlatformLib",
            "EditorOnly",
            "EditorConstrained",
            "PlayTests",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect()
    );
}

#[test]
fn e2e_generated_compile_set_matches_original_csproj() {
    let tmp = make_temp_root();
    let root = tmp.path();
    write_file(
        root,
        "Assets/SystemAssets/Assemblies/Main/Main.asmdef",
        r#"{"name":"Main"}"#,
    );
    write_file(
        root,
        "Assets/SystemAssets/Assemblies/Sandbox/Sandbox.asmdef",
        r#"{"name":"Sandbox"}"#,
    );
    write_file(root, "Assets/Game/Assembly.asmref", r#"{"reference":"Main"}"#);
    write_file(
        root,
        "Assets/Game/Sandbox/Assembly.asmref",
        r#"{"reference":"Sandbox"}"#,
    );

    write_file(root, "Assets/Game/A.cs", "class A {}\n");
    write_file(root, "Assets/Game/Sub/B.cs", "class B {}\n");
    write_file(root, "Assets/Game/Tests/CTest.cs", "class CTest {}\n");
    write_file(root, "Assets/Game/Sandbox/S.cs", "class S {}\n");

    write_file(
        root,
        "Main.original.csproj",
        "<Project>\n  <ItemGroup>\n    <Compile Include=\"Assets/Game/A.cs\" />\n    <Compile Include=\"Assets/Game/Sub/B.cs\" />\n    <Compile Include=\"Assets/Game/Tests/CTest.cs\" />\n  </ItemGroup>\n</Project>",
    );
    write_file(
        root,
        "Sandbox.original.csproj",
        "<Project>\n  <ItemGroup>\n    <Compile Include=\"Assets/Game/Sandbox/S.cs\" />\n  </ItemGroup>\n</Project>",
    );

    SolutionGenerator::new()
        .generate_from_lockfile(&opts(root, BuildPlatform::Ios, BuildConfig::Editor), &bare_lockfile())
        .unwrap();
    let v = "tpl/ios-editor";
    let orig_main = read_compile_set(root, "Main.original.csproj");
    let gen_main = read_compile_set(root, &format!("{}/Main.csproj", v));
    assert_eq!(gen_main, orig_main);
    let orig_sb = read_compile_set(root, "Sandbox.original.csproj");
    let gen_sb = read_compile_set(root, &format!("{}/Sandbox.csproj", v));
    assert_eq!(gen_sb, orig_sb);
}

