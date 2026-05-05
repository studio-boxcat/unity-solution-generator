//! Project-side filesystem scanner.
//!
//! Walks `Assets/` and `Packages/` to enumerate every `.cs` directory and every
//! `.asmdef`/`.asmref` marker, then resolves directory ownership to assemblies.
//!
//! Hot path: uses `ignore::WalkBuilder::build_parallel()` (with all gitignore
//! filters disabled) so traversal fans out across worker threads and shares
//! work via crossbeam_deque. See [[CLAUDE.md]] §"Performance" for budget.

use std::collections::{BTreeSet, HashMap};
use std::path::Path;
use std::sync::Mutex;

use ignore::{WalkBuilder, WalkState};

use crate::error::{GeneratorError, Result};
use crate::io::{create_dir_all, read_file, write_file_if_changed};
use crate::json::{extract_json_bool, extract_json_string, extract_json_string_array};
use crate::paths::{DEFAULT_GENERATOR_ROOT, join_path, parent_directory};

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
        let Some(name) = extract_json_string(&json, "name") else {
            return Ok(None);
        };
        let include_platforms = extract_json_string_array(&json, "includePlatforms");
        let define_constraints = extract_json_string_array(&json, "defineConstraints");
        Ok(Some(AsmDefRecord {
            name,
            directory: parent_directory(relative_path).to_string(),
            references: extract_json_string_array(&json, "references"),
            category: infer_category(&include_platforms, &define_constraints),
            include_platforms,
            allow_unsafe_code: extract_json_bool(&json, "allowUnsafeCode").unwrap_or(false),
            version_defines: parse_version_defines(&json),
        }))
    }
}

pub fn parse_version_defines(json: &str) -> Vec<VersionDefine> {
    let needle = "\"versionDefines\"";
    let Some(idx) = json.find(needle) else {
        return Vec::new();
    };
    let bytes = json.as_bytes();
    let mut i = idx + needle.len();
    while i < bytes.len() && bytes[i] != b'[' {
        i += 1;
    }
    if i >= bytes.len() {
        return Vec::new();
    }
    i += 1;
    let mut out = Vec::new();
    while i < bytes.len() {
        match bytes[i] {
            b']' => break,
            b'{' => {
                let obj_start = i;
                let mut depth = 1;
                i += 1;
                while i < bytes.len() && depth > 0 {
                    match bytes[i] {
                        b'{' => depth += 1,
                        b'}' => depth -= 1,
                        _ => {}
                    }
                    i += 1;
                }
                let obj = &json[obj_start..i];
                if let (Some(p), Some(d)) = (
                    extract_json_string(obj, "name"),
                    extract_json_string(obj, "define"),
                ) {
                    if !p.is_empty() && !d.is_empty() {
                        out.push(VersionDefine {
                            package_name: p,
                            define: d,
                        });
                    }
                }
            }
            _ => i += 1,
        }
    }
    out
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
    pub fn scan(project_root: &str) -> Result<ScanResult> {
        let _span = tracing::info_span!("project_scanner.scan").entered();
        let generator_dir = join_path(project_root, DEFAULT_GENERATOR_ROOT);
        let cache_path = join_path(&generator_dir, "scan-cache");
        let file_scan = {
            let _s = tracing::info_span!("project_scanner.file_scan").entered();
            load_cached_scan(&cache_path, project_root)
                .unwrap_or_else(|| scan_and_cache(project_root, &cache_path))
        };

        let mut asm_def_by_name: HashMap<String, AsmDefRecord> = HashMap::new();
        for path in &file_scan.asmdef_paths {
            let Some(record) = AsmDefRecord::load(project_root, path)? else {
                continue;
            };
            if asm_def_by_name.contains_key(&record.name) {
                return Err(GeneratorError::DuplicateAsmDefName(record.name));
            }
            asm_def_by_name.insert(record.name.clone(), record);
        }

        let mut assembly_roots: HashMap<String, String> = HashMap::new();
        for (name, record) in &asm_def_by_name {
            assembly_roots.insert(record.directory.clone(), name.clone());
        }
        for path in &file_scan.asmref_paths {
            let Some((dir, reference)) = load_asm_ref(project_root, path)? else {
                continue;
            };
            if asm_def_by_name.contains_key(&reference) {
                assembly_roots.insert(dir, reference);
            }
        }

        let mut dirs_by_project: HashMap<String, Vec<String>> = HashMap::new();
        let mut unresolved_dirs: Vec<String> = Vec::new();
        for dir in &file_scan.cs_dirs {
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

#[derive(Default)]
struct ScanBucket {
    cs_dirs: BTreeSet<String>,
    asmdef_paths: Vec<String>,
    asmref_paths: Vec<String>,
}

#[derive(Debug, Clone)]
struct FileScan {
    cs_dirs: Vec<String>,
    asmdef_paths: Vec<String>,
    asmref_paths: Vec<String>,
}

/// Scan project files using `ignore`'s parallel walker with all gitignore
/// behaviour disabled. We emulate the Swift `processDirent` filter (skip
/// `.foo` and `bar~` entries) via `filter_entry`.
fn scan_project_files(project_root: &str, roots: &[&str]) -> FileScan {
    let prefix_len = project_root.len() + 1;
    let aggregate: Mutex<ScanBucket> = Mutex::new(ScanBucket::default());

    for root in roots {
        let root_dir = format!("{}/{}", project_root, root);
        if !Path::new(&root_dir).exists() {
            continue;
        }

        let mut builder = WalkBuilder::new(&root_dir);
        builder
            .standard_filters(false)
            .hidden(false)
            .ignore(false)
            .git_ignore(false)
            .git_global(false)
            .git_exclude(false)
            .parents(false)
            .follow_links(false);

        // Per-thread bucket flushed into the shared aggregate on drop —
        // matches the pattern recommended by `ignore`'s `ParallelVisitorBuilder` docs.
        struct Flusher<'a> {
            local: ScanBucket,
            aggregate: &'a Mutex<ScanBucket>,
        }
        impl Drop for Flusher<'_> {
            fn drop(&mut self) {
                let mut g = self.aggregate.lock().unwrap();
                g.cs_dirs.extend(std::mem::take(&mut self.local.cs_dirs));
                g.asmdef_paths
                    .extend(std::mem::take(&mut self.local.asmdef_paths));
                g.asmref_paths
                    .extend(std::mem::take(&mut self.local.asmref_paths));
            }
        }

        let agg_ref: &Mutex<ScanBucket> = &aggregate;
        builder.build_parallel().run(|| {
            let mut flusher = Flusher {
                local: ScanBucket::default(),
                aggregate: agg_ref,
            };
            Box::new(move |result| {
                let entry = match result {
                    Ok(e) => e,
                    Err(_) => return WalkState::Continue,
                };

                let name = entry.file_name().to_string_lossy();
                if name.starts_with('.') || name.ends_with('~') {
                    return WalkState::Skip;
                }

                let Some(ft) = entry.file_type() else {
                    return WalkState::Continue;
                };
                if !ft.is_file() {
                    return WalkState::Continue;
                }

                let path_str = entry.path().to_string_lossy();
                if path_str.len() <= prefix_len {
                    return WalkState::Continue;
                }
                let rel_path: &str = &path_str[prefix_len..];
                let n: &str = name.as_ref();
                if n.ends_with(".cs") {
                    flusher
                        .local
                        .cs_dirs
                        .insert(parent_directory(rel_path).to_string());
                } else if n.ends_with(".asmdef") {
                    flusher.local.asmdef_paths.push(rel_path.to_string());
                } else if n.ends_with(".asmref") {
                    flusher.local.asmref_paths.push(rel_path.to_string());
                }
                WalkState::Continue
            })
        });
    }

    let bucket = aggregate.into_inner().unwrap();
    FileScan {
        cs_dirs: bucket.cs_dirs.into_iter().collect(),
        asmdef_paths: bucket.asmdef_paths,
        asmref_paths: bucket.asmref_paths,
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
    let Some(reference) = extract_json_string(&json, "reference") else {
        return Ok(None);
    };
    Ok(Some((parent_directory(relative_path).to_string(), reference)))
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

// ── scan cache ────────────────────────────────────────────────────────────

fn load_cached_scan(cache_path: &str, root_path: &str) -> Option<FileScan> {
    let _s = tracing::info_span!("scan_cache.validate").entered();
    let content = read_file(cache_path).ok()?;

    enum Sec {
        Cs,
        Asmdef,
        Asmref,
        Mtimes,
    }
    let mut section: Option<Sec> = None;
    let mut cs_dirs = Vec::new();
    let mut asmdef_paths = Vec::new();
    let mut asmref_paths = Vec::new();
    let mut dir_mtimes: Vec<(String, u128)> = Vec::new();

    for line in content.split('\n') {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        match line {
            "[cs]" => {
                section = Some(Sec::Cs);
                continue;
            }
            "[asmdef]" => {
                section = Some(Sec::Asmdef);
                continue;
            }
            "[asmref]" => {
                section = Some(Sec::Asmref);
                continue;
            }
            "[mtimes]" => {
                section = Some(Sec::Mtimes);
                continue;
            }
            _ => {}
        }
        match section {
            Some(Sec::Cs) => cs_dirs.push(line.to_string()),
            Some(Sec::Asmdef) => asmdef_paths.push(line.to_string()),
            Some(Sec::Asmref) => asmref_paths.push(line.to_string()),
            Some(Sec::Mtimes) => {
                if let Some(pipe) = line.find('|') {
                    if let Ok(m) = line[pipe + 1..].parse::<u128>() {
                        dir_mtimes.push((line[..pipe].to_string(), m));
                    }
                }
            }
            None => {}
        }
    }

    if dir_mtimes.is_empty() {
        return None;
    }

    for (rel_dir, cached) in &dir_mtimes {
        let full = if rel_dir.is_empty() {
            root_path.to_string()
        } else {
            join_path(root_path, rel_dir)
        };
        let m = std::fs::metadata(&full).ok()?;
        let mtime_ns = mtime_nanos(&m)?;
        if mtime_ns != *cached {
            return None;
        }
    }

    Some(FileScan {
        cs_dirs,
        asmdef_paths,
        asmref_paths,
    })
}

fn scan_and_cache(root_path: &str, cache_path: &str) -> FileScan {
    let _s = tracing::info_span!("scan_cache.full_walk").entered();
    let file_scan = scan_project_files(root_path, &["Assets", "Packages"]);

    let mut all_dirs: BTreeSet<String> = BTreeSet::new();
    all_dirs.insert("Assets".to_string());
    all_dirs.insert("Packages".to_string());
    let mut add_with_ancestors = |dir: &str| {
        let mut cur = dir.to_string();
        while !cur.is_empty() && all_dirs.insert(cur.clone()) {
            cur = parent_directory(&cur).to_string();
        }
    };
    for d in &file_scan.cs_dirs {
        add_with_ancestors(d);
    }
    for p in &file_scan.asmdef_paths {
        add_with_ancestors(parent_directory(p));
    }
    for p in &file_scan.asmref_paths {
        add_with_ancestors(parent_directory(p));
    }

    let mut s = String::from("# scan-cache — auto-generated, do not edit\n");
    s.push_str("[cs]\n");
    for d in &file_scan.cs_dirs {
        s.push_str(d);
        s.push('\n');
    }
    s.push_str("[asmdef]\n");
    for p in &file_scan.asmdef_paths {
        s.push_str(p);
        s.push('\n');
    }
    s.push_str("[asmref]\n");
    for p in &file_scan.asmref_paths {
        s.push_str(p);
        s.push('\n');
    }
    s.push_str("[mtimes]\n");
    for d in &all_dirs {
        let full = if d.is_empty() {
            root_path.to_string()
        } else {
            join_path(root_path, d)
        };
        if let Ok(m) = std::fs::metadata(&full) {
            if let Some(ns) = mtime_nanos(&m) {
                s.push_str(d);
                s.push('|');
                s.push_str(&ns.to_string());
                s.push('\n');
            }
        }
    }

    create_dir_all(parent_directory(cache_path));
    let _ = write_file_if_changed(cache_path, &s);
    file_scan
}

#[cfg(unix)]
fn mtime_nanos(m: &std::fs::Metadata) -> Option<u128> {
    use std::os::unix::fs::MetadataExt;
    let secs: i64 = m.mtime();
    let nanos: i64 = m.mtime_nsec();
    if secs < 0 {
        return None;
    }
    Some((secs as u128) * 1_000_000_000 + (nanos as u128))
}

#[cfg(not(unix))]
fn mtime_nanos(m: &std::fs::Metadata) -> Option<u128> {
    let mt = m.modified().ok()?;
    let d = mt.duration_since(std::time::SystemTime::UNIX_EPOCH).ok()?;
    Some(d.as_nanos())
}

