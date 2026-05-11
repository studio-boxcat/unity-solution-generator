//! Shared fixture builder for benches. Synthesises a Unity-shaped project tree.
//!
//! Used by all three benches; we avoid `mod common` in each bench file because
//! Cargo treats `benches/<name>.rs` as separate crates and resolving sibling
//! modules requires `path = ...` declarations. Instead each bench declares this
//! file via `#[path = "common.rs"] mod common;`.

#![allow(dead_code)]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use tempfile::TempDir;
use unity_solution_generator::{DllRef, Lockfile, RefCategory};

pub struct Fixture {
    pub _tmp: TempDir,
    pub root: PathBuf,
}

impl Fixture {
    pub fn root_str(&self) -> String {
        self.root.to_string_lossy().into_owned()
    }
}

fn write(root: &Path, rel: &str, content: &str) {
    let path = root.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, content).unwrap();
}

/// Build a synthetic project with `assemblies` asmdefs, each owning `files_per_asm`
/// .cs files. Roughly mimics meow-tower (13 asmdefs, ~5k cs files) at the default sizes.
pub fn make_project(assemblies: usize, files_per_asm: usize) -> Fixture {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().to_path_buf();
    for i in 0..assemblies {
        let name = format!("Assembly{}", i);
        write(
            &root,
            &format!("Assets/Assemblies/{}/{}.asmdef", name, name),
            &format!(r#"{{"name":"{}"}}"#, name),
        );
        for j in 0..files_per_asm {
            write(
                &root,
                &format!("Assets/Assemblies/{}/File{}.cs", name, j),
                &format!("class C{}_{} {{}}\n", i, j),
            );
        }
    }
    Fixture { _tmp: tmp, root }
}

pub fn minimal_lockfile() -> Lockfile {
    let mut refs: BTreeMap<RefCategory, Vec<DllRef>> = BTreeMap::new();
    refs.insert(
        RefCategory::Engine,
        vec![DllRef::new(
            "UnityEngine",
            "$(UnityPath)/Unity.app/Contents/Managed/UnityEngine/UnityEngine.dll",
        )],
    );
    refs.insert(
        RefCategory::Editor,
        vec![DllRef::new(
            "UnityEditor",
            "$(UnityPath)/Unity.app/Contents/Managed/UnityEngine/UnityEditor.dll",
        )],
    );
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
        defines: vec!["UNITY_6000".into(), "ENABLE_AR".into()],
        defines_scripting: vec!["ODIN_INSPECTOR".into()],
    }
}
