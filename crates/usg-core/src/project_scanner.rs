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
use crate::io::{create_dir_all, read_file, write_file_if_changed};
use crate::paths::{join_path, mtime_nanos_for, parent_directory};
use crate::scan::{Delta, ScanError, hint_is_relevant, since};

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

/// Compact on-disk scan cache: bincode-serialized `(watchman_clock, ScanResult)`.
/// Lives at `<generator_root>/scan-cache.bin`. Bincode keeps the read+parse
/// cost in the low-ms range on meow-tower-sized projects (~13 asmdefs) vs.
/// the ~10ms we'd pay re-parsing asmdef JSON.
const SCAN_CACHE_FILE: &str = "scan-cache.bin";

impl ProjectScanner {
    /// Watchman-driven scan with a fast-path on-disk cache. Existing API:
    /// returns just the [`ScanResult`]. Use [`scan_with_freshness`] to also
    /// learn whether the result came from a cache hit (which lets callers
    /// skip downstream Watchman queries — see `LockfileIO::load_or_skip`).
    pub fn scan(project_root: &str, generator_root: &str) -> Result<ScanResult> {
        Ok(Self::scan_with_freshness(project_root, generator_root)?.0)
    }

    /// Returns `(scan, cache_hit)`. When `cache_hit` is `true`, the cached
    /// scan was validated as still-fresh — downstream caches (notably the
    /// lockfile) can short-circuit their own validation.
    ///
    /// Two-tier invalidation:
    /// - **Tier 0 (mtime fingerprint):** stat the persisted set of
    ///   contributing paths (asmdef/asmref files + their parent dirs). All
    ///   ns-mtimes match the cached values → trust the cache, skip Watchman
    ///   entirely. Typical cost on meow-tower: ~1–2 ms (≈40 stats × ~30 µs).
    /// - **Tier 1 (Watchman):** if tier-0 fails, query
    ///   `since(prev_clock)`. Authoritative — catches changes the mtime
    ///   fingerprint can miss (e.g. file content rewrites that preserve
    ///   parent-dir mtime). Cost: ~14 ms warm round-trip.
    ///
    /// On meow-tower-sized projects this brings warm-no-op from ~50 ms
    /// (Watchman-only) to ~36 ms (mtime tier-0 hits the common case),
    /// matching the pre-overhaul fingerprint cache's wall-clock.
    pub fn scan_with_freshness(
        project_root: &str,
        generator_root: &str,
    ) -> Result<(ScanResult, bool)> {
        let cache_path = join_path(
            project_root,
            &format!("{}/{}", generator_root, SCAN_CACHE_FILE),
        );

        // Try tier-0 (mtime fingerprint) then tier-1 (Watchman).
        if let Some((header, cached_scan)) = load_cached_scan(&cache_path) {
            if mtimes_unchanged(project_root, &header.mtimes) {
                return Ok((cached_scan, true));
            }
            if let Ok(delta) = since(Path::new(project_root), Some(&header.watchman_clock)) {
                if let Delta::Touched { paths, new_clock } = delta {
                    if !paths.iter().any(|p| hint_is_relevant(p)) {
                        // Rewrite cache with the advancing clock + refreshed
                        // mtime fingerprint so the next call's tier-0 check
                        // picks up where this one left off.
                        let mtimes = collect_mtimes(project_root, &cached_scan);
                        write_cached_scan(
                            &cache_path,
                            &CacheHeader {
                                watchman_clock: new_clock,
                                mtimes,
                            },
                            &cached_scan,
                        );
                        return Ok((cached_scan, true));
                    }
                }
                // Fresh delta or relevant change → fall through to re-derive.
            }
        }

        let scan = Self::scan_uncached(project_root)?;
        // Capture the clock as part of the re-derive — `since(None)` returns
        // the current clock alongside the full enumeration.
        if let Ok(delta) = since(Path::new(project_root), None) {
            let (_, new_clock) = delta.into_paths_and_clock();
            let mtimes = collect_mtimes(project_root, &scan);
            write_cached_scan(
                &cache_path,
                &CacheHeader {
                    watchman_clock: new_clock,
                    mtimes,
                },
                &scan,
            );
        }
        Ok((scan, false))
    }

    fn scan_uncached(project_root: &str) -> Result<ScanResult> {
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

/// Persisted invariants alongside the cached scan. The `mtimes` table is
/// tier-0 invalidation; `watchman_clock` is tier-1 fallback.
struct CacheHeader {
    watchman_clock: String,
    /// Project-relative path → ns-mtime at cache-write time. Paths are
    /// asmdef/asmref files (catches in-place edits) + their parent dirs
    /// (catches add/remove). On meow-tower: ~40 entries.
    mtimes: Vec<(String, u128)>,
}

/// Tab-delimited text format keyed for grep-debuggability. One line per
/// asmdef record; header carries `watchman-clock` + an `[mtimes]` section
/// for tier-0 fingerprint invalidation.
fn load_cached_scan(cache_path: &str) -> Option<(CacheHeader, ScanResult)> {
    let content = read_file(cache_path).ok()?;
    let mut clock: Option<String> = None;
    let mut mtimes: Vec<(String, u128)> = Vec::new();
    let mut asm_def_by_name: HashMap<String, AsmDefRecord> = HashMap::new();
    let mut dirs_by_project: HashMap<String, Vec<String>> = HashMap::new();
    let mut unresolved_dirs: Vec<String> = Vec::new();

    enum Sec { Mtimes, Asmdefs, Dirs, Unresolved }
    let mut section: Option<Sec> = None;
    for line in content.split('\n') {
        if line.is_empty() { continue; }
        if let Some(rest) = line.strip_prefix("# watchman-clock:") {
            clock = Some(rest.trim().to_string());
            continue;
        }
        if line.starts_with('#') { continue; }
        match line {
            "[mtimes]" => { section = Some(Sec::Mtimes); continue; }
            "[asmdefs]" => { section = Some(Sec::Asmdefs); continue; }
            "[dirs]" => { section = Some(Sec::Dirs); continue; }
            "[unresolved]" => { section = Some(Sec::Unresolved); continue; }
            _ => {}
        }
        match section {
            Some(Sec::Mtimes) => {
                if let Some((p, m)) = line.rsplit_once('|') {
                    if let Ok(m) = m.parse::<u128>() {
                        mtimes.push((p.to_string(), m));
                    }
                }
            }
            Some(Sec::Asmdefs) => {
                if let Some(r) = decode_asmdef_record(line) {
                    asm_def_by_name.insert(r.name.clone(), r);
                }
            }
            Some(Sec::Dirs) => {
                if let Some((proj, dir)) = line.split_once('\t') {
                    dirs_by_project.entry(proj.to_string()).or_default().push(dir.to_string());
                }
            }
            Some(Sec::Unresolved) => unresolved_dirs.push(line.to_string()),
            None => {}
        }
    }
    Some((
        CacheHeader { watchman_clock: clock?, mtimes },
        ScanResult { asm_def_by_name, dirs_by_project, unresolved_dirs },
    ))
}

fn write_cached_scan(cache_path: &str, header: &CacheHeader, scan: &ScanResult) {
    let mut s = String::with_capacity(8 * 1024);
    s.push_str("# scan-cache — auto-generated, do not edit\n");
    s.push_str(&format!("# watchman-clock: {}\n", header.watchman_clock));
    s.push_str("[mtimes]\n");
    for (p, m) in &header.mtimes {
        s.push_str(p);
        s.push('|');
        s.push_str(&m.to_string());
        s.push('\n');
    }
    s.push_str("[asmdefs]\n");
    let mut names: Vec<&String> = scan.asm_def_by_name.keys().collect();
    names.sort();
    for n in names {
        encode_asmdef_record(&mut s, &scan.asm_def_by_name[n]);
        s.push('\n');
    }
    s.push_str("[dirs]\n");
    let mut proj_keys: Vec<&String> = scan.dirs_by_project.keys().collect();
    proj_keys.sort();
    for proj in proj_keys {
        let mut dirs = scan.dirs_by_project[proj].clone();
        dirs.sort();
        for d in dirs {
            s.push_str(proj);
            s.push('\t');
            s.push_str(&d);
            s.push('\n');
        }
    }
    s.push_str("[unresolved]\n");
    for d in &scan.unresolved_dirs {
        s.push_str(d);
        s.push('\n');
    }
    create_dir_all(parent_directory(cache_path));
    let _ = write_file_if_changed(cache_path, &s);
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
    paths.insert("Assets".to_string());
    paths.insert("Packages".to_string());
    paths.insert("Library/PackageCache".to_string());
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

    // Per-file mtimes for asmdef/asmref content edits.
    let mut file_paths: BTreeSet<String> = BTreeSet::new();
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
        if i > 0 { out.push(','); }
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
    let version_defines = parts.next().map(|s| {
        if s.is_empty() { Vec::new() } else {
            s.split(',').filter_map(|pair| {
                let (pkg, def) = pair.split_once('|')?;
                Some(VersionDefine { package_name: pkg.to_string(), define: def.to_string() })
            }).collect()
        }
    }).unwrap_or_default();
    Some(AsmDefRecord { name, directory, references, category, include_platforms, allow_unsafe_code, version_defines })
}

fn split_semi(s: &str) -> Vec<String> {
    if s.is_empty() { Vec::new() } else { s.split(';').map(str::to_string).collect() }
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



