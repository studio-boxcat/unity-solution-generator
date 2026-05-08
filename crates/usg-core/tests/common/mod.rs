//! Test fixture helpers — port of `SolutionGeneratorTests` setup helpers.
//! `#![allow(dead_code)]` because each `tests/*.rs` is a separate compile unit
//! and any unused helper produces a per-binary warning. Tests as a whole use
//! all of these.
#![allow(dead_code)]

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use tempfile::TempDir;

pub fn make_temp_root() -> TempDir {
    tempfile::tempdir().expect("tempdir")
}

pub fn write_file(root: &Path, rel: &str, content: &str) {
    let path = root.join(rel);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(&path, content).unwrap();
}

pub fn read_file(root: &Path, rel: &str) -> String {
    fs::read_to_string(root.join(rel)).unwrap()
}

fn xml_unescape(value: &str) -> String {
    value
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

/// Reads `<Compile Include="..."/>` patterns from a csproj and expands them
/// against the given root, mirroring `SolutionGeneratorTests.readCompileSet`.
pub fn read_compile_set(root: &Path, csproj_rel: &str) -> BTreeSet<String> {
    let content = read_file(root, csproj_rel);
    let mut out = BTreeSet::new();
    let mut i = 0;
    let needle = "<Compile Include=\"";
    let bytes = content.as_bytes();
    while i + needle.len() < bytes.len() {
        let Some(start) = content[i..].find(needle) else {
            break;
        };
        let pat_start = i + start + needle.len();
        let Some(end_off) = content[pat_start..].find('"') else {
            break;
        };
        let pattern = xml_unescape(&content[pat_start..pat_start + end_off]);
        out.extend(expand_pattern(&pattern, root));
        i = pat_start + end_off + 1;
    }
    out
}

fn expand_pattern(pattern: &str, root: &Path) -> Vec<String> {
    let mut stripped = pattern.to_string();
    while stripped.starts_with("../") {
        stripped = stripped[3..].to_string();
    }
    if let Some(dir) = stripped.strip_suffix("/*.cs") {
        return list_cs_files(root, dir);
    }
    if stripped.ends_with(".cs") {
        let path = stripped.replace('\\', "/");
        if root.join(&path).exists() {
            return vec![path];
        }
        return Vec::new();
    }
    Vec::new()
}

fn list_cs_files(root: &Path, rel: &str) -> Vec<String> {
    let dir = if rel.is_empty() {
        root.to_path_buf()
    } else {
        root.join(rel)
    };
    if !dir.exists() {
        return Vec::new();
    }
    let Ok(rd) = fs::read_dir(&dir) else {
        return Vec::new();
    };
    rd.filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().ends_with(".cs"))
        .map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            if rel.is_empty() {
                name
            } else {
                format!("{}/{}", rel, name)
            }
        })
        .collect()
}

pub fn assert_compile_set(root: &Path, csproj_rel: &str, expected: &[&str]) {
    let actual = read_compile_set(root, csproj_rel);
    let exp: BTreeSet<String> = expected.iter().map(|s| (*s).to_string()).collect();
    assert_eq!(
        actual, exp,
        "compile set mismatch in {}\n  actual:   {:?}\n  expected: {:?}",
        csproj_rel, actual, exp
    );
}
