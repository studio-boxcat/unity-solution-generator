//! Project-side scanner.
//!
//! Enumerates `.cs` directories and `.asmdef`/`.asmref` markers under the
//! project tree, then resolves directory ownership to assemblies.
//!
//! Backend: [`crate::scan`] (Watchman). One `since(None)` query per invocation
//! returns the full file enumeration; per-asmdef JSON parse is parallelised
//! via `rayon`. Results are persisted to a bincode scan-cache (`scan-cache.bin`)
//! guarded by an mtime fingerprint — see [`scan`].

use std::borrow::Borrow;
use std::collections::HashMap;
use std::path::Path;

use rayon::prelude::*;

use crate::error::{GeneratorError, Result};
use crate::io::{create_dir_all, read_file};
use crate::paths::{join_path, mtime_nanos_for, parent_directory};
use crate::scan::{enumerate, to_generator_error};

/// An assembly (asmdef) name — the project identity used as the map key across
/// scan → generate → typecheck. A newtype so it can't be confused with the
/// many other strings in flight (directories, platforms, defines, GUIDs).
/// `Borrow<str>` lets `HashMap`/`BTreeSet` be queried with a bare `&str`.
#[derive(
    Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, bincode::Encode, bincode::Decode,
)]
pub struct ProjectName(pub String);

impl ProjectName {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<String> for ProjectName {
    fn from(s: String) -> Self {
        ProjectName(s)
    }
}

impl From<&str> for ProjectName {
    fn from(s: &str) -> Self {
        ProjectName(s.to_string())
    }
}

impl std::fmt::Display for ProjectName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl Borrow<str> for ProjectName {
    fn borrow(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, bincode::Encode, bincode::Decode)]
pub enum ProjectCategory {
    Runtime,
    Editor,
    Test,
}

#[derive(Debug, Clone, bincode::Encode, bincode::Decode)]
pub struct VersionDefine {
    pub package_name: String,
    pub define: String,
}

#[derive(Debug, Clone, bincode::Encode, bincode::Decode)]
pub struct AsmDefRecord {
    pub name: ProjectName,
    pub directory: String,
    pub references: Vec<ProjectName>,
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
        let Some(name) = v.get("name").and_then(|x| x.as_str()).map(ProjectName::from) else {
            return Ok(None);
        };
        let include_platforms = json_string_array(&v, "includePlatforms");
        let define_constraints = json_string_array(&v, "defineConstraints");
        Ok(Some(AsmDefRecord {
            name,
            directory: parent_directory(relative_path).to_string(),
            references: json_string_array(&v, "references")
                .into_iter()
                .map(ProjectName)
                .collect(),
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

#[derive(Debug, Clone, bincode::Encode, bincode::Decode)]
pub struct ScanResult {
    pub asm_def_by_name: HashMap<ProjectName, AsmDefRecord>,
    pub dirs_by_project: HashMap<ProjectName, Vec<String>>,
    pub unresolved_dirs: Vec<String>,
}

/// On-disk scan cache. Compact bincode binary format. Lives at
/// `<generator_root>/scan-cache.bin`. Single-tier invalidation via the
/// embedded mtime fingerprint — if any tracked path's ns-mtime has changed
/// since the cache was written, full re-derive.
const SCAN_CACHE_FILE: &str = "scan-cache.bin";

/// Schema version embedded in every `scan-cache.bin`. Bumping this
/// invalidates all caches wholesale — no migrations. Cold rebuild on bump
/// is harmless (file is gitignored under `Library/`).
const SCAN_CACHE_SCHEMA: u32 = 2;

/// Full on-disk payload: schema version + mtime fingerprint + scan content.
#[derive(bincode::Encode, bincode::Decode)]
struct CacheFile {
    schema: u32,
    mtimes: Vec<(String, u128)>,
    scan: ScanResult,
}

/// Watchman-driven scan with a fast-path on-disk cache. Single-tier
/// invalidation: stat the persisted mtime fingerprint (~40 paths) and
/// reuse the cached scan if all match. Cache miss → one Watchman
/// enumeration query + per-asmdef JSON parse.
///
/// The fingerprint covers top-level dirs (add/remove), every asmdef
/// directory + ancestors, every `.asmdef`/`.asmref` file (in-place edits).
/// On meow-tower this hits ~1–2 ms warm.
pub fn scan(project_root: &str, generator_root: &str) -> Result<ScanResult> {
    let cache_path = join_path(
        project_root,
        &format!("{}/{}", generator_root, SCAN_CACHE_FILE),
    );

    if let Some((mtimes, cached_scan)) = load_cached_scan(&cache_path) {
        if mtimes_unchanged(project_root, &mtimes) {
            return Ok(cached_scan);
        }
    }

    let scan = scan_uncached(project_root)?;
    let mtimes = collect_mtimes(project_root, &scan);
    write_cached_scan(&cache_path, mtimes, &scan);
    Ok(scan)
}

fn scan_uncached(project_root: &str) -> Result<ScanResult> {
    let _span = tracing::info_span!("project_scanner.scan").entered();

    // One Watchman query — returns the full file enumeration.
    let paths = enumerate(Path::new(project_root)).map_err(to_generator_error)?;
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

    let mut asm_def_by_name: HashMap<ProjectName, AsmDefRecord> = HashMap::new();
    for record in asmdef_records {
        if asm_def_by_name.contains_key(&record.name) {
            return Err(GeneratorError::DuplicateAsmDefName(record.name.0));
        }
        asm_def_by_name.insert(record.name.clone(), record);
    }

    let mut assembly_roots: HashMap<String, ProjectName> = HashMap::new();
    for (name, record) in &asm_def_by_name {
        assembly_roots.insert(record.directory.clone(), name.clone());
    }
    for (dir, reference) in asmref_records {
        if asm_def_by_name.contains_key(reference.as_str()) {
            assembly_roots.insert(dir, ProjectName(reference));
        }
    }

    let mut dirs_by_project: HashMap<ProjectName, Vec<String>> = HashMap::new();
    let mut unresolved_dirs: Vec<String> = Vec::new();
    for dir in &cs_dirs {
        if let Some(owner) = find_assembly_owner(dir, &assembly_roots) {
            dirs_by_project.entry(owner).or_default().push(dir.clone());
        } else if let Some(legacy) = resolve_legacy_project(dir) {
            dirs_by_project
                .entry(legacy.into())
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

fn bincode_config() -> bincode::config::Configuration {
    bincode::config::standard()
}

fn load_cached_scan(cache_path: &str) -> Option<(Vec<(String, u128)>, ScanResult)> {
    let bytes = std::fs::read(cache_path).ok()?;
    let (file, _): (CacheFile, _) =
        bincode::decode_from_slice(&bytes, bincode_config()).ok()?;
    if file.schema != SCAN_CACHE_SCHEMA {
        return None;
    }
    Some((file.mtimes, file.scan))
}

fn write_cached_scan(cache_path: &str, mtimes: Vec<(String, u128)>, scan: &ScanResult) {
    let file = CacheFile {
        schema: SCAN_CACHE_SCHEMA,
        mtimes,
        scan: scan.clone(),
    };
    let Ok(bytes) = bincode::encode_to_vec(&file, bincode_config()) else {
        return;
    };
    create_dir_all(parent_directory(cache_path));
    // Best-effort: a failed cache write just means the next scan pays the full
    // rescan cost. Surface it so perf debugging isn't mystified by a silent miss.
    if let Err(e) = crate::io::atomic_write_bytes(cache_path, &bytes) {
        tracing::warn!(target: "unity_solution_generator::scan", path = %cache_path, error = %e, "scan-cache write failed; next scan will rescan");
    }
}

/// Collect the mtime-fingerprint set for `scan`. Two layers of coverage:
///
/// 1. **Directory mtimes** — catches add/remove of any file in the dir.
///    Top-level (`Assets`, `Packages`, `Library/PackageCache`), every
///    asmdef-owning directory, and ancestors thereof.
/// 2. **asmdef + asmref FILE mtimes** — catches *in-place edits* (file
///    rewrite preserves parent-dir mtime on POSIX, so dir-only tracking
///    misses content changes). We `read_dir` each asmdef directory once
///    to find the `.asmdef`/`.asmref` files; this is ~50 µs per dir,
///    ~10 dirs on meow-tower → ~0.5 ms total.
///
/// `.cs` file content edits *don't* affect the scan result — the scanner
/// only cares which `.cs` directories exist, not their bytes. So we skip
/// per-`.cs` stats and rely on parent-dir mtime to catch add/remove.
fn collect_mtimes(project_root: &str, scan: &ScanResult) -> Vec<(String, u128)> {
    use std::collections::BTreeSet;
    let mut paths: BTreeSet<String> = BTreeSet::new();
    // Top-level dir mtimes: bump on subdir add/remove. Library/PackageCache
    // catches the "Unity refreshed packages" case (subdir added/removed on
    // each package install/uninstall). Hand-edits to `manifest.json` or
    // `packages-lock.json` don't bump anything on the FS until Unity actually
    // resolves them — caching the old state is correct until that happens.
    paths.insert("Assets".to_string());
    paths.insert("Packages".to_string());
    paths.insert("Library/PackageCache".to_string());
    paths.insert("ProjectSettings".to_string());
    // File-level mtimes capture in-place rewrites of `.asmdef`/`.asmref`
    // files that preserve parent-dir mtime on POSIX. Populated below from the
    // per-asmdef-directory `read_dir` walk.
    let mut file_paths: BTreeSet<String> = BTreeSet::new();
    let mut asmdef_dirs: BTreeSet<String> = BTreeSet::new();
    for record in scan.asm_def_by_name.values() {
        asmdef_dirs.insert(record.directory.clone());
        let mut cur = record.directory.clone();
        while !cur.is_empty() && paths.insert(cur.clone()) {
            cur = parent_directory(&cur).to_string();
        }
    }
    for dirs in scan.dirs_by_project.values() {
        for d in dirs {
            asmdef_dirs.insert(d.clone());
            let mut cur = d.clone();
            while !cur.is_empty() && paths.insert(cur.clone()) {
                cur = parent_directory(&cur).to_string();
            }
        }
    }

    let mut out: Vec<(String, u128)> = paths
        .into_iter()
        .map(|p| {
            let full = if p.is_empty() {
                project_root.to_string()
            } else {
                join_path(project_root, &p)
            };
            (p, mtime_nanos_for(&full).unwrap_or(0))
        })
        .collect();

    // Per-file mtimes for asmdef/asmref content edits (in-place file rewrites
    // bump file mtime but not parent-dir mtime). Append to the file_paths set
    // seeded above with lockfile-input files.
    for dir in &asmdef_dirs {
        let full_dir = if dir.is_empty() {
            project_root.to_string()
        } else {
            join_path(project_root, dir)
        };
        let Ok(rd) = std::fs::read_dir(&full_dir) else { continue };
        for entry in rd.flatten() {
            let Some(name) = entry.file_name().to_str().map(str::to_string) else { continue };
            if name.ends_with(".asmdef") || name.ends_with(".asmref") {
                let rel = if dir.is_empty() {
                    name
                } else {
                    format!("{}/{}", dir, name)
                };
                file_paths.insert(rel);
            }
        }
    }
    for p in file_paths {
        let full = join_path(project_root, &p);
        out.push((p, mtime_nanos_for(&full).unwrap_or(0)));
    }
    out
}

/// Check whether the persisted `scan-cache` fingerprint matches the current
/// filesystem state. Returns `false` if the cache is missing, malformed, or
/// any tracked path's mtime has changed. Used by `lockfile::scan_and_write`
/// as its own invalidation signal — when the scan cache is fresh, the lockfile
/// derived from the same scan state is also fresh.
pub(crate) fn scan_cache_fingerprint_matches(
    project_root: &str,
    generator_root: &str,
) -> bool {
    let cache_path = join_path(
        project_root,
        &format!("{}/{}", generator_root, SCAN_CACHE_FILE),
    );
    let Some((mtimes, _)) = load_cached_scan(&cache_path) else {
        return false;
    };
    mtimes_unchanged(project_root, &mtimes)
}

/// Tier-0 invalidation check: all recorded paths still have their cached
/// mtimes. Missing paths are recorded as `0`; a missing path appearing later
/// (mtime > 0) invalidates.
fn mtimes_unchanged(project_root: &str, mtimes: &[(String, u128)]) -> bool {
    for (p, cached) in mtimes {
        let full = if p.is_empty() {
            project_root.to_string()
        } else {
            join_path(project_root, p)
        };
        let current = mtime_nanos_for(&full).unwrap_or(0);
        if current != *cached {
            return false;
        }
    }
    true
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

fn find_assembly_owner(
    directory: &str,
    assembly_roots: &HashMap<String, ProjectName>,
) -> Option<ProjectName> {
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



