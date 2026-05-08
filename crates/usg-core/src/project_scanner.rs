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

use ignore::{WalkBuilder, WalkState};
use rayon::prelude::*;

use crate::error::{GeneratorError, Result};
use crate::io::{create_dir_all, has_matching_version, read_file, write_file_if_changed};
use crate::paths::{join_path, parent_directory};

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
    /// `generator_root` controls where the `scan-cache` file lives — pass
    /// [`crate::paths::DEFAULT_GENERATOR_ROOT`] for the standard layout, or
    /// a custom directory to keep multiple variant trees isolated (tests).
    pub fn scan(project_root: &str, generator_root: &str) -> Result<ScanResult> {
        let _span = tracing::info_span!("project_scanner.scan").entered();
        let generator_dir = join_path(project_root, generator_root);
        let cache_path = join_path(&generator_dir, "scan-cache");
        let file_scan = {
            let _s = tracing::info_span!("project_scanner.file_scan").entered();
            load_cached_scan(&cache_path, project_root)
                .unwrap_or_else(|| scan_and_cache(project_root, &cache_path))
        };

        let mut asm_def_by_name: HashMap<String, AsmDefRecord> = HashMap::new();
        for record in file_scan.asmdef_records {
            if asm_def_by_name.contains_key(&record.name) {
                return Err(GeneratorError::DuplicateAsmDefName(record.name));
            }
            asm_def_by_name.insert(record.name.clone(), record);
        }

        let mut assembly_roots: HashMap<String, String> = HashMap::new();
        for (name, record) in &asm_def_by_name {
            assembly_roots.insert(record.directory.clone(), name.clone());
        }
        for (dir, reference) in file_scan.asmref_records {
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

impl crate::walk::Bucket for ScanBucket {
    fn merge_from(&mut self, other: Self) {
        self.cs_dirs.extend(other.cs_dirs);
        self.asmdef_paths.extend(other.asmdef_paths);
        self.asmref_paths.extend(other.asmref_paths);
    }
}

#[derive(Debug, Clone)]
struct FileScan {
    cs_dirs: Vec<String>,
    /// Asmdef paths relative to project root. Kept on cache for invalidation
    /// (mtime tracking) and by-path debugging; the parsed record set below is
    /// what the rest of the pipeline consumes.
    asmdef_paths: Vec<String>,
    asmref_paths: Vec<String>,
    /// Pre-parsed `AsmDefRecord`s, populated either during a cold scan (parsed
    /// in parallel via rayon) or loaded straight from `[asmdef-records]` on a
    /// warm cache hit. Skipping the per-asmdef file read + JSON parse on the
    /// warm path is the whole point of this cache extension.
    asmdef_records: Vec<AsmDefRecord>,
    /// Pre-resolved (directory, reference) pairs from each `.asmref`. Same
    /// reasoning as `asmdef_records`.
    asmref_records: Vec<(String, String)>,
}

/// Scan project files using `ignore`'s parallel walker with all gitignore
/// behaviour disabled. We emulate the Swift `processDirent` filter (skip
/// `.foo` and `bar~` entries) via the visit closure.
fn scan_project_files(project_root: &str, roots: &[&str]) -> FileScan {
    use crate::walk::{Bucket, parallel_walk};

    // Use `Path::strip_prefix` (component-aware) rather than byte-slicing on
    // `path_str.len() + 1`. Byte-slicing would panic if the kernel ever handed
    // back a path whose prefix differed from `project_root` by even one byte
    // (trailing slash, canonicalised symlink component) AND that byte landed
    // inside a multibyte UTF-8 char. With `panic = "abort"` such a panic would
    // SIGKILL the process, including via FFI from Unity's Mono. Defense-in-depth.
    let project_root_path = std::path::Path::new(project_root);
    let mut bucket = ScanBucket::default();

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

        let from_root = parallel_walk(builder, |local: &mut ScanBucket, entry| {
            let name = entry.file_name().to_string_lossy();
            if name.starts_with('.') || name.ends_with('~') {
                return WalkState::Skip;
            }
            let Some(ft) = entry.file_type() else {
                return WalkState::Continue;
            };
            // Skip whole subtrees of blacklisted packages (HotReload etc.) —
            // their asmrefs merge sources into other assemblies, but their
            // internal DLLs come in editor-version-specific variants that
            // confuse a single-config typecheck.
            if let Ok(rel_dir) = entry.path().strip_prefix(project_root_path) {
                if let Some(rel_dir_str) = rel_dir.to_str() {
                    if crate::lockfile_scanner::is_blacklisted_path(rel_dir_str) {
                        return WalkState::Skip;
                    }
                }
            }
            if !ft.is_file() {
                return WalkState::Continue;
            }
            let Ok(rel) = entry.path().strip_prefix(project_root_path) else {
                return WalkState::Continue;
            };
            // Non-UTF-8 path component — Unity asset paths are always UTF-8 on
            // macOS APFS, so this only fires on a corrupt filesystem entry.
            // Skip rather than panic.
            let Some(rel_path) = rel.to_str() else {
                return WalkState::Continue;
            };
            let n: &str = name.as_ref();
            if n.ends_with(".cs") {
                local.cs_dirs.insert(parent_directory(rel_path).to_string());
            } else if n.ends_with(".asmdef") {
                local.asmdef_paths.push(rel_path.to_string());
            } else if n.ends_with(".asmref") {
                local.asmref_paths.push(rel_path.to_string());
            }
            WalkState::Continue
        });
        bucket.merge_from(from_root);
    }

    FileScan {
        cs_dirs: bucket.cs_dirs.into_iter().collect(),
        asmdef_paths: bucket.asmdef_paths,
        asmref_paths: bucket.asmref_paths,
        // Records are populated by `scan_and_cache` after parsing the asmdef
        // / asmref files in parallel; the walker itself only enumerates paths.
        asmdef_records: Vec::new(),
        asmref_records: Vec::new(),
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

// ── scan cache ────────────────────────────────────────────────────────────

/// Routed through the workspace-level `CACHE_VERSION` so the scan-cache,
/// lock-fingerprint, and generate-fingerprint all bump together. See
/// [[architecture.md]] (Versioning section).
const SCAN_CACHE_VERSION: u32 = crate::CACHE_VERSION;

fn load_cached_scan(cache_path: &str, root_path: &str) -> Option<FileScan> {
    let _s = tracing::info_span!("scan_cache.validate").entered();
    let content = read_file(cache_path).ok()?;
    if !has_matching_version(&content, SCAN_CACHE_VERSION) {
        return None;
    }

    enum Sec {
        Cs,
        Asmdef,
        Asmref,
        Mtimes,
        AsmdefRecords,
        AsmrefRecords,
    }
    let mut section: Option<Sec> = None;
    let mut cs_dirs = Vec::new();
    let mut asmdef_paths = Vec::new();
    let mut asmref_paths = Vec::new();
    let mut mtimes: Vec<(String, u128)> = Vec::new();
    let mut asmdef_records: Vec<AsmDefRecord> = Vec::new();
    let mut asmref_records: Vec<(String, String)> = Vec::new();

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
            "[asmdef-records]" => {
                section = Some(Sec::AsmdefRecords);
                continue;
            }
            "[asmref-records]" => {
                section = Some(Sec::AsmrefRecords);
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
                        mtimes.push((line[..pipe].to_string(), m));
                    }
                }
            }
            Some(Sec::AsmdefRecords) => {
                if let Some(rec) = decode_asmdef_record(line) {
                    asmdef_records.push(rec);
                }
            }
            Some(Sec::AsmrefRecords) => {
                if let Some(pair) = decode_asmref_record(line) {
                    asmref_records.push(pair);
                }
            }
            None => {}
        }
    }

    if mtimes.is_empty() {
        return None;
    }

    // Older caches (or caches written by an earlier version of usg) won't have
    // [asmdef-records]; refusing them forces a fresh scan that re-populates.
    if !asmdef_paths.is_empty() && asmdef_records.is_empty() {
        return None;
    }

    for (rel, cached) in &mtimes {
        let full = if rel.is_empty() {
            root_path.to_string()
        } else {
            join_path(root_path, rel)
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
        asmdef_records,
        asmref_records,
    })
}

fn scan_and_cache(root_path: &str, cache_path: &str) -> FileScan {
    let _s = tracing::info_span!("scan_cache.full_walk").entered();
    let walk = scan_project_files(root_path, &["Assets", "Packages"]);

    // Parse asmdefs + asmrefs in parallel. Most projects have <100 of each;
    // the parse itself is microseconds, but parallelising amortises the
    // per-file `read` syscall across cores.
    let asmdef_records: Vec<AsmDefRecord> = walk
        .asmdef_paths
        .par_iter()
        .filter_map(|p| AsmDefRecord::load(root_path, p).ok().flatten())
        .collect();
    let asmref_records: Vec<(String, String)> = walk
        .asmref_paths
        .par_iter()
        .filter_map(|p| load_asm_ref(root_path, p).ok().flatten())
        .collect();

    // Track every directory that contributed work plus the asmdef/asmref files
    // themselves. Tracking the files (not just their parent dirs) catches
    // in-place edits — a parent dir's mtime doesn't bump when a file inside it
    // is rewritten in place, so without these we'd miss e.g. an asmdef name
    // change. The price is `len(asmdefs + asmrefs) × stat()` on warm validation.
    let mut all_paths: BTreeSet<String> = BTreeSet::new();
    all_paths.insert("Assets".to_string());
    all_paths.insert("Packages".to_string());
    let mut add_with_ancestors = |dir: &str| {
        let mut cur = dir.to_string();
        while !cur.is_empty() && all_paths.insert(cur.clone()) {
            cur = parent_directory(&cur).to_string();
        }
    };
    for d in &walk.cs_dirs {
        add_with_ancestors(d);
    }
    for p in &walk.asmdef_paths {
        add_with_ancestors(parent_directory(p));
    }
    for p in &walk.asmref_paths {
        add_with_ancestors(parent_directory(p));
    }
    for p in &walk.asmdef_paths {
        all_paths.insert(p.clone());
    }
    for p in &walk.asmref_paths {
        all_paths.insert(p.clone());
    }

    let mut s = String::from("# scan-cache — auto-generated, do not edit\n");
    s.push_str(&format!("# version: {}\n", SCAN_CACHE_VERSION));
    s.push_str("[cs]\n");
    for d in &walk.cs_dirs {
        s.push_str(d);
        s.push('\n');
    }
    s.push_str("[asmdef]\n");
    for p in &walk.asmdef_paths {
        s.push_str(p);
        s.push('\n');
    }
    s.push_str("[asmref]\n");
    for p in &walk.asmref_paths {
        s.push_str(p);
        s.push('\n');
    }
    s.push_str("[asmdef-records]\n");
    for r in &asmdef_records {
        encode_asmdef_record(&mut s, r);
        s.push('\n');
    }
    s.push_str("[asmref-records]\n");
    for (dir, reference) in &asmref_records {
        s.push_str(dir);
        s.push('\t');
        s.push_str(reference);
        s.push('\n');
    }
    s.push_str("[mtimes]\n");
    for p in &all_paths {
        let full = if p.is_empty() {
            root_path.to_string()
        } else {
            join_path(root_path, p)
        };
        if let Ok(m) = std::fs::metadata(&full) {
            if let Some(ns) = mtime_nanos(&m) {
                s.push_str(p);
                s.push('|');
                s.push_str(&ns.to_string());
                s.push('\n');
            }
        }
    }

    create_dir_all(parent_directory(cache_path));
    let _ = write_file_if_changed(cache_path, &s);

    FileScan {
        cs_dirs: walk.cs_dirs,
        asmdef_paths: walk.asmdef_paths,
        asmref_paths: walk.asmref_paths,
        asmdef_records,
        asmref_records,
    }
}

// ── asmdef record (de)serialization ──────────────────────────────────────
//
// Single line per record, tab-delimited. Columns:
//   0: name              (Unity asmdef names — alphanumeric + dots)
//   1: directory         (forward-slash relative path)
//   2: category          ("R" = Runtime, "E" = Editor, "T" = Test)
//   3: allow_unsafe      ("0" or "1")
//   4: references        (semicolon-separated assembly names; may be empty)
//   5: include_platforms (semicolon-separated; may be empty)
//   6: version_defines   (`pkg|def` pairs, comma-separated; may be empty)
//
// Tab and newline can't appear in any of those fields (Unity rejects them in
// asmdef names; paths are forward-slash; defines are uppercase identifiers),
// so no escaping is needed.

fn encode_asmdef_record(out: &mut String, r: &AsmDefRecord) {
    out.push_str(&r.name);
    out.push('\t');
    out.push_str(&r.directory);
    out.push('\t');
    out.push(match r.category {
        ProjectCategory::Runtime => 'R',
        ProjectCategory::Editor => 'E',
        ProjectCategory::Test => 'T',
    });
    out.push('\t');
    out.push(if r.allow_unsafe_code { '1' } else { '0' });
    out.push('\t');
    out.push_str(&r.references.join(";"));
    out.push('\t');
    out.push_str(&r.include_platforms.join(";"));
    out.push('\t');
    for (i, vd) in r.version_defines.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&vd.package_name);
        out.push('|');
        out.push_str(&vd.define);
    }
}

fn decode_asmdef_record(line: &str) -> Option<AsmDefRecord> {
    let mut parts = line.split('\t');
    let name = parts.next()?.to_string();
    let directory = parts.next()?.to_string();
    let category = match parts.next()? {
        "R" => ProjectCategory::Runtime,
        "E" => ProjectCategory::Editor,
        "T" => ProjectCategory::Test,
        _ => return None,
    };
    let allow_unsafe_code = matches!(parts.next()?, "1");
    let references = split_semi(parts.next()?);
    let include_platforms = split_semi(parts.next()?);
    let version_defines = parts
        .next()
        .map(|s| {
            if s.is_empty() {
                Vec::new()
            } else {
                s.split(',')
                    .filter_map(|pair| {
                        let (pkg, def) = pair.split_once('|')?;
                        Some(VersionDefine {
                            package_name: pkg.to_string(),
                            define: def.to_string(),
                        })
                    })
                    .collect()
            }
        })
        .unwrap_or_default();
    Some(AsmDefRecord {
        name,
        directory,
        references,
        category,
        include_platforms,
        allow_unsafe_code,
        version_defines,
    })
}

fn decode_asmref_record(line: &str) -> Option<(String, String)> {
    let (dir, reference) = line.split_once('\t')?;
    Some((dir.to_string(), reference.to_string()))
}

fn split_semi(s: &str) -> Vec<String> {
    if s.is_empty() {
        Vec::new()
    } else {
        s.split(';').map(str::to_string).collect()
    }
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

