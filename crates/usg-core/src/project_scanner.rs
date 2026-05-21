//! Project-side scanner.
//!
//! Enumerates `.cs` directories and `.asmdef`/`.asmref` markers under the
//! project tree, then resolves directory ownership to assemblies.
//!
//! Backend: [`crate::scan`] (Watchman). One `since(None)` query per invocation
//! returns the full file enumeration; per-asmdef JSON parse is parallelised
//! via `rayon`. No on-disk scan-cache — Watchman is the source of truth for
//! "what files exist" and the cost of re-parsing a few asmdef JSONs on a
//! warm-watch query (~ms) is below the noise floor of process startup.

use std::collections::HashMap;
use std::path::Path;

use rayon::prelude::*;

use crate::error::{GeneratorError, Result};
use crate::io::read_file;
use crate::paths::{join_path, parent_directory};
use crate::scan::{ScanError, since};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectCategory {
    Runtime,
    Editor,
    Test,
}

#[derive(Debug, Clone)]
pub struct VersionDefine {
    pub package_name: String,
    pub define: String,
}

#[derive(Debug, Clone)]
pub struct AsmDefRecord {
    pub name: String,
    pub directory: String,
    pub references: Vec<String>,
    pub category: ProjectCategory,
    pub include_platforms: Vec<String>,
    pub allow_unsafe_code: bool,
    pub version_defines: Vec<VersionDefine>,
}

impl AsmDefRecord {
    pub fn load(root_path: &str, relative_path: &str) -> Result<Option<AsmDefRecord>> {
        let full = join_path(root_path, relative_path);
        let json = read_file(&full)?;
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&json) else {
            // asmdef present but malformed — treat same as missing-name (skip).
            return Ok(None);
        };
        let Some(name) = v.get("name").and_then(|x| x.as_str()).map(String::from) else {
            return Ok(None);
        };
        let include_platforms = json_string_array(&v, "includePlatforms");
        let define_constraints = json_string_array(&v, "defineConstraints");
        Ok(Some(AsmDefRecord {
            name,
            directory: parent_directory(relative_path).to_string(),
            references: json_string_array(&v, "references"),
            category: infer_category(&include_platforms, &define_constraints),
            include_platforms,
            allow_unsafe_code: v
                .get("allowUnsafeCode")
                .and_then(|x| x.as_bool())
                .unwrap_or(false),
            version_defines: parse_version_defines(&v),
        }))
    }
}

fn json_string_array(v: &serde_json::Value, key: &str) -> Vec<String> {
    v.get(key)
        .and_then(|x| x.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

pub fn parse_version_defines(v: &serde_json::Value) -> Vec<VersionDefine> {
    let Some(arr) = v.get("versionDefines").and_then(|x| x.as_array()) else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|obj| {
            let p = obj.get("name").and_then(|x| x.as_str())?;
            let d = obj.get("define").and_then(|x| x.as_str())?;
            if p.is_empty() || d.is_empty() {
                return None;
            }
            Some(VersionDefine {
                package_name: p.to_string(),
                define: d.to_string(),
            })
        })
        .collect()
}

fn infer_category(include_platforms: &[String], define_constraints: &[String]) -> ProjectCategory {
    if define_constraints.iter().any(|s| s == "UNITY_INCLUDE_TESTS") {
        return ProjectCategory::Test;
    }
    if include_platforms.len() == 1 && include_platforms[0] == "Editor" {
        return ProjectCategory::Editor;
    }
    if define_constraints.iter().any(|s| s == "UNITY_EDITOR") {
        return ProjectCategory::Editor;
    }
    ProjectCategory::Runtime
}

#[derive(Debug, Clone)]
pub struct ScanResult {
    pub asm_def_by_name: HashMap<String, AsmDefRecord>,
    pub dirs_by_project: HashMap<String, Vec<String>>,
    pub unresolved_dirs: Vec<String>,
}

pub struct ProjectScanner;

impl ProjectScanner {
    /// `generator_root` is accepted for API stability; no on-disk cache file
    /// lives under it anymore. Watchman is the source of truth for the project
    /// file set; the per-asmdef JSON parse is fast enough that caching it
    /// across invocations isn't worth the maintenance cost.
    pub fn scan(project_root: &str, _generator_root: &str) -> Result<ScanResult> {
        let _span = tracing::info_span!("project_scanner.scan").entered();

        // One Watchman query. `since(None)` returns the full file enumeration
        // — same response shape as a `Fresh` instance. Result paths are
        // project-relative with forward-slash separators.
        let delta = since(Path::new(project_root), None).map_err(scan_err_to_generator)?;
        let (paths, _clock) = delta.into_paths_and_clock();
        let (cs_dirs, asmdef_paths, asmref_paths) = partition_paths(&paths);

        // Parse asmdefs + asmrefs in parallel. Most projects have <100 of
        // each; per-file `read` is the dominant cost, so amortising across
        // cores is the win.
        let asmdef_records: Vec<AsmDefRecord> = asmdef_paths
            .par_iter()
            .filter_map(|p| AsmDefRecord::load(project_root, p).ok().flatten())
            .collect();
        let asmref_records: Vec<(String, String)> = asmref_paths
            .par_iter()
            .filter_map(|p| load_asm_ref(project_root, p).ok().flatten())
            .collect();

        let mut asm_def_by_name: HashMap<String, AsmDefRecord> = HashMap::new();
        for record in asmdef_records {
            if asm_def_by_name.contains_key(&record.name) {
                return Err(GeneratorError::DuplicateAsmDefName(record.name));
            }
            asm_def_by_name.insert(record.name.clone(), record);
        }

        let mut assembly_roots: HashMap<String, String> = HashMap::new();
        for (name, record) in &asm_def_by_name {
            assembly_roots.insert(record.directory.clone(), name.clone());
        }
        for (dir, reference) in asmref_records {
            if asm_def_by_name.contains_key(&reference) {
                assembly_roots.insert(dir, reference);
            }
        }

        let mut dirs_by_project: HashMap<String, Vec<String>> = HashMap::new();
        let mut unresolved_dirs: Vec<String> = Vec::new();
        for dir in &cs_dirs {
            if let Some(owner) = find_assembly_owner(dir, &assembly_roots) {
                dirs_by_project.entry(owner).or_default().push(dir.clone());
            } else if let Some(legacy) = resolve_legacy_project(dir) {
                dirs_by_project
                    .entry(legacy.to_string())
                    .or_default()
                    .push(dir.clone());
            } else {
                unresolved_dirs.push(dir.clone());
            }
        }

        Ok(ScanResult {
            asm_def_by_name,
            dirs_by_project,
            unresolved_dirs,
        })
    }
}

/// Path component filter: skip any path with a `.foo` (dotfile) or `bar~`
/// (Unity backup) component. Unity convention is for these to be editor-only
/// metadata that doesn't ship into compiled output.
fn is_skipped_path(p: &str) -> bool {
    p.split('/').any(|c| c.starts_with('.') || c.ends_with('~'))
}

/// Partition the Watchman path list into (cs_dirs, asmdef_paths, asmref_paths).
/// Other suffixes (`.dll`, `manifest.json`, etc.) are emitted by the Watchman
/// query but irrelevant here — the lockfile scanner consumes those.
fn partition_paths(paths: &[String]) -> (Vec<String>, Vec<String>, Vec<String>) {
    let mut cs_dirs: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut asmdef_paths: Vec<String> = Vec::new();
    let mut asmref_paths: Vec<String> = Vec::new();
    for p in paths {
        if is_skipped_path(p) {
            continue;
        }
        if p.ends_with(".cs") {
            cs_dirs.insert(parent_directory(p).to_string());
        } else if p.ends_with(".asmdef") {
            asmdef_paths.push(p.clone());
        } else if p.ends_with(".asmref") {
            asmref_paths.push(p.clone());
        }
    }
    (cs_dirs.into_iter().collect(), asmdef_paths, asmref_paths)
}

/// Map a `ScanError` into the crate-level error type, preserving the
/// `Unavailable` discriminant so callers (BoxcatBridge, CLI) can surface the
/// "install watchman" message without string-matching.
fn scan_err_to_generator(e: ScanError) -> GeneratorError {
    match e {
        ScanError::Unavailable => GeneratorError::ScanUnavailable(e.to_string()),
        ScanError::Query(_) => GeneratorError::Other(e.to_string()),
    }
}

fn find_assembly_owner(directory: &str, assembly_roots: &HashMap<String, String>) -> Option<String> {
    let mut current = directory.to_string();
    loop {
        if let Some(name) = assembly_roots.get(&current) {
            return Some(name.clone());
        }
        if current.is_empty() {
            return None;
        }
        current = parent_directory(&current).to_string();
    }
}

fn load_asm_ref(root_path: &str, relative_path: &str) -> Result<Option<(String, String)>> {
    let json = read_file(&join_path(root_path, relative_path))?;
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&json) else {
        return Ok(None);
    };
    let Some(reference) = v.get("reference").and_then(|x| x.as_str()) else {
        return Ok(None);
    };
    Ok(Some((parent_directory(relative_path).to_string(), reference.to_string())))
}

fn resolve_legacy_project(directory: &str) -> Option<&'static str> {
    let mut iter = directory.split('/');
    let first = iter.next()?;
    if first != "Assets" {
        return None;
    }
    let mut second: Option<&str> = None;
    let mut has_editor = false;
    if let Some(s) = iter.next() {
        second = Some(s);
        if s == "Editor" {
            has_editor = true;
        }
    }
    for c in iter {
        if c == "Editor" {
            has_editor = true;
        }
    }
    let is_first_pass = matches!(
        second,
        Some("Plugins") | Some("Standard Assets") | Some("Pro Standard Assets")
    );
    Some(match (has_editor, is_first_pass) {
        (true, true) => "Assembly-CSharp-Editor-firstpass",
        (true, false) => "Assembly-CSharp-Editor",
        (false, true) => "Assembly-CSharp-firstpass",
        (false, false) => "Assembly-CSharp",
    })
}



