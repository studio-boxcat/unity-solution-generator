//! `typecheck` subcommand — validate compile via direct `csc.dll` invocation,
//! bypassing MSBuild entirely. See [[architecture.md]] (Typecheck subsystem).
//!
//! Roughly: scan + topo-sort + per-project mtime UTD check + `dotnet exec
//! csc.dll @rsp` per dirty project. The headline win is the UTD short-circuit
//! (warm no-op): MSBuild always re-invokes csc on every project even when
//! nothing changed (CoreCompile uses `$(NonExistentFile)` as a sentinel that
//! defeats stat-based UTD); we don't.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs::{self, File, FileTimes};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, UNIX_EPOCH};

use rayon::prelude::*;

use crate::error::{GeneratorError, Result, io_err};
use crate::lockfile::{DllRef, Lockfile, LockfileIO, RefCategory};
use crate::paths::{DEFAULT_GENERATOR_ROOT, resolve_real_path};
use crate::project_scanner::{AsmDefRecord, ProjectCategory, ProjectScanner};
use crate::solution_generator::{BuildConfig, BuildPlatform};

#[derive(Debug, Clone)]
pub struct TypecheckOptions {
    pub project_root: String,
    pub platform: BuildPlatform,
    pub build_config: BuildConfig,
    pub extra_refs: Vec<DllRef>,
}

impl TypecheckOptions {
    pub fn new(project_root: impl Into<String>, platform: BuildPlatform) -> Self {
        Self {
            project_root: project_root.into(),
            platform,
            build_config: BuildConfig::Editor,
            extra_refs: Vec::new(),
        }
    }
    pub fn with_build_config(mut self, c: BuildConfig) -> Self {
        self.build_config = c;
        self
    }
    pub fn with_extra_refs(mut self, refs: Vec<DllRef>) -> Self {
        self.extra_refs = refs;
        self
    }
}

#[derive(Debug)]
pub struct TypecheckResult {
    /// Number of projects that had to recompile (dirty UTD).
    pub recompiled: usize,
    /// Number of projects skipped via UTD.
    pub skipped: usize,
    /// Diagnostic output from failing csc invocations, keyed by project name.
    pub failures: BTreeMap<String, String>,
}

impl TypecheckResult {
    pub fn ok(&self) -> bool {
        self.failures.is_empty()
    }
}

/// Run typecheck. Returns the per-project status; caller decides exit code.
pub fn run(opts: &TypecheckOptions) -> Result<TypecheckResult> {
    let _span = tracing::info_span!("typecheck.run").entered();
    let root = resolve_real_path(&opts.project_root);
    let lockfile = LockfileIO::load_or_scan(&root, DEFAULT_GENERATOR_ROOT)?;
    let scan = ProjectScanner::scan(&root, DEFAULT_GENERATOR_ROOT)?;

    let included = compute_included_projects(&scan.asm_def_by_name, opts);
    let levels = topo_levels(&included, &scan.asm_def_by_name);

    purge_legacy_typecheck_dir(&root, DEFAULT_GENERATOR_ROOT, opts.platform, opts.build_config);

    // Shares `<variant>/obj/Debug/` with `build`'s MSBuild output so consumers
    // see fresh DLLs after a typecheck-only flow. Foreign writers are detected
    // per-DLL via `.usg-stamp` sidecars — see `is_up_to_date`.
    let out_dir = typecheck_output_dir(&root, DEFAULT_GENERATOR_ROOT, opts.platform, opts.build_config);
    fs::create_dir_all(&out_dir).map_err(|e| io_err(&out_dir, e))?;

    let csc_dll = find_csc_dll().ok_or_else(|| {
        io_err(
            "csc.dll",
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "csc.dll not found — run `dotnet --list-sdks` to confirm a .NET SDK is installed",
            ),
        )
    })?;

    let common_defines = collect_defines(&lockfile, opts.platform, opts.build_config);
    let common_refs = collect_refs(&lockfile, opts.platform, opts.build_config, &opts.extra_refs);
    // Resolve MSBuild-style properties (`$(UnityPath)`, `$(ProjectRoot)`,
    // `$(UsgCache)`) in lockfile paths now — MSBuild does this at eval time
    // but `csc.dll` doesn't recognize MSBuild property syntax. Applies to
    // refs AND analyzers.
    let usg_cache = crate::paths::usg_cache_dir(&lockfile.unity_version);
    let resolve = |s: &str| -> String {
        s.replace("$(UnityPath)", &lockfile.unity_path)
            .replace("$(ProjectRoot)", &root)
            .replace("$(UsgCache)", &usg_cache)
    };
    let common_refs: Vec<DllRef> = common_refs
        .into_iter()
        .map(|r| DllRef::new(r.name, resolve(&r.path)))
        // Filter out native DLLs (CS0009: PE image doesn't contain managed metadata).
        // Unity's lockfile references some native plugins (e.g.
        // `unity_sprite_author.dll`) that MSBuild's RAR silently filters but raw
        // csc doesn't. Match RAR's behaviour by inspecting the PE header.
        .filter(|r| {
            if is_managed_dll(Path::new(&r.path)) {
                true
            } else {
                tracing::debug!(target: "unity_solution_generator::typecheck", path = %r.path, "filtered: not a managed DLL");
                false
            }
        })
        .collect();
    let analyzers: Vec<String> = lockfile
        .analyzers
        .iter()
        .map(|a| resolve(a))
        .filter(|a| is_managed_dll(Path::new(a)))
        .collect();

    let mut recompiled = 0usize;
    let mut skipped = 0usize;
    let mut failures = BTreeMap::new();
    // Tracks which projects' compiles failed (or were cascade-skipped). When a
    // downstream project references one of these, we skip rather than invoking
    // csc on a doomed compile that produces a wall of CS0006 noise.
    let mut failed_set: BTreeSet<String> = BTreeSet::new();

    // Process the DAG level-by-level. Within a level all projects are
    // independent (no cross-edges), so we fan out via rayon — each worker
    // spawns a `dotnet exec csc.dll /shared` which connects to the shared
    // VBCSCompiler over its named pipe. The server already accepts
    // concurrent requests, and our per-project filesystem writes target
    // disjoint paths (`<name>.rsp`, `<name>.dll`).
    //
    // Sequential processing across levels is required because level N+1
    // reads level N's `.dll` outputs as `/reference:` inputs and depends
    // on the failed-set decision (cascade-skip).
    for level in &levels {
        let outcomes: Vec<(String, ProjectOutcome)> = level
            .par_iter()
            .map(|name| {
                let outcome = compile_project(
                    name,
                    &scan,
                    &included,
                    &failed_set,
                    &root,
                    &out_dir,
                    &csc_dll,
                    &common_defines,
                    &common_refs,
                    &analyzers,
                    &lockfile.lang_version,
                );
                (name.clone(), outcome)
            })
            .collect();

        for (name, outcome) in outcomes {
            match outcome {
                ProjectOutcome::Recompiled => recompiled += 1,
                ProjectOutcome::Skipped => skipped += 1,
                ProjectOutcome::Empty => {}
                ProjectOutcome::CascadeSkipped(dep) => {
                    failures.insert(
                        name.clone(),
                        format!("skipped (cascade): upstream '{}' failed", dep),
                    );
                    failed_set.insert(name);
                }
                ProjectOutcome::Failed(stderr) => {
                    failures.insert(name.clone(), stderr);
                    failed_set.insert(name);
                }
                ProjectOutcome::Io(e) => return Err(e),
            }
        }
    }

    Ok(TypecheckResult {
        recompiled,
        skipped,
        failures,
    })
}

// ── inclusion + topo ──────────────────────────────────────────────────────

fn compute_included_projects(
    asm_def_by_name: &HashMap<String, AsmDefRecord>,
    opts: &TypecheckOptions,
) -> BTreeSet<String> {
    let is_editor = opts.build_config == BuildConfig::Editor;
    let target_platform = opts.platform.unity_platform_name();
    asm_def_by_name
        .iter()
        .filter(|(_, asm)| {
            if is_editor {
                return true;
            }
            if asm.category != ProjectCategory::Runtime {
                return false;
            }
            let platforms: Vec<&str> = asm
                .include_platforms
                .iter()
                .filter(|p| p.as_str() != "Editor")
                .map(String::as_str)
                .collect();
            platforms.is_empty() || platforms.contains(&target_platform)
        })
        .map(|(n, _)| n.clone())
        .collect()
}

/// Group `included` into levels: `levels[0]` has no upstream deps in
/// `included`; `levels[i+1]` has all its deps in `levels[0..=i]`. Within a
/// level all projects are independent and can be compiled in parallel.
/// Lex-sorted within levels for deterministic output.
fn topo_levels(
    included: &BTreeSet<String>,
    asm_def_by_name: &HashMap<String, AsmDefRecord>,
) -> Vec<Vec<String>> {
    let mut indeg: HashMap<String, usize> = HashMap::new();
    let mut adj: HashMap<String, Vec<String>> = HashMap::new();
    for name in included {
        indeg.entry(name.clone()).or_insert(0);
        if let Some(asm) = asm_def_by_name.get(name) {
            for r in &asm.references {
                if included.contains(r) {
                    adj.entry(r.clone()).or_default().push(name.clone());
                    *indeg.entry(name.clone()).or_insert(0) += 1;
                }
            }
        }
    }

    let mut levels: Vec<Vec<String>> = Vec::new();
    let mut current: BTreeSet<String> = indeg
        .iter()
        .filter(|(_, &d)| d == 0)
        .map(|(n, _)| n.clone())
        .collect();
    while !current.is_empty() {
        let mut next: BTreeSet<String> = BTreeSet::new();
        for n in &current {
            if let Some(succs) = adj.get(n) {
                for s in succs {
                    if let Some(d) = indeg.get_mut(s) {
                        *d -= 1;
                        if *d == 0 {
                            next.insert(s.clone());
                        }
                    }
                }
            }
        }
        levels.push(current.into_iter().collect());
        current = next;
    }
    levels
}

/// Per-project compile outcome. Counters + failure-set updates happen on the
/// caller side under the level-sequential lock; the parallel work (csc spawn)
/// is contained inside `compile_project`.
enum ProjectOutcome {
    /// csc ran and exited 0.
    Recompiled,
    /// mtime UTD said inputs weren't newer than the cached output.
    Skipped,
    /// asmdef has no `.cs` files — nothing to compile.
    Empty,
    /// At least one upstream is in `failed_set`; compile would just spew CS0006.
    CascadeSkipped(String),
    /// csc exited non-zero. Stderr captured for reporting.
    Failed(String),
    /// Local I/O error (couldn't write the rsp, etc.) — fatal, surfaced to caller.
    Io(GeneratorError),
}

/// Pure-ish: takes everything it needs by reference, mutates only its own
/// per-project filesystem outputs (`<name>.rsp`, `<name>.dll`). Safe to call
/// in parallel across projects within a topo level.
#[allow(clippy::too_many_arguments)]
fn compile_project(
    name: &str,
    scan: &crate::project_scanner::ScanResult,
    included: &BTreeSet<String>,
    failed_set: &BTreeSet<String>,
    root: &str,
    out_dir: &str,
    csc_dll: &str,
    common_defines: &[String],
    common_refs: &[DllRef],
    analyzers: &[String],
    lang_version: &str,
) -> ProjectOutcome {
    let asm = &scan.asm_def_by_name[name];

    if let Some(dep) = asm.references.iter().find(|r| failed_set.contains(*r)) {
        return ProjectOutcome::CascadeSkipped(dep.clone());
    }

    let sources = collect_sources(root, asm, &scan.dirs_by_project);
    if sources.is_empty() {
        return ProjectOutcome::Empty;
    }

    let proj_refs = collect_project_refs(asm, included, out_dir);
    let out_dll = format!("{}/{}.dll", out_dir, name);
    let stamp_path = stamp_path_for(&out_dll);

    if is_up_to_date(&sources, common_refs, &proj_refs, &out_dll, &stamp_path) {
        return ProjectOutcome::Skipped;
    }

    let mut defines: Vec<String> = common_defines.to_vec();
    for vd in &asm.version_defines {
        defines.push(vd.define.clone());
    }
    defines.extend(asm.include_platforms.iter().cloned());

    let rsp_path = format!("{}/{}.rsp", out_dir, name);
    let rsp_body = build_rsp(
        lang_version,
        &defines,
        common_refs,
        &proj_refs,
        analyzers,
        &sources,
        &out_dll,
        asm.allow_unsafe_code,
    );
    if let Err(e) = fs::write(&rsp_path, rsp_body) {
        return ProjectOutcome::Io(io_err(&rsp_path, e));
    }

    // Snapshot pre-compile bytes + mtime. csc with `/refonly /deterministic`
    // produces byte-identical output for unchanged inputs, but the .dll's
    // mtime advances on every emit — that cascades into spurious downstream
    // rebuilds (downstream's UTD sees `upstream.dll` newer than its own
    // output). Compare bytes after compile; if identical, restore the old
    // mtime so the cascade never starts.
    let prev_bytes = fs::read(&out_dll).ok();
    let prev_mtime = mtime_nsec(&out_dll);

    match invoke_csc(csc_dll, &rsp_path) {
        Ok(()) => {
            // If csc's emit is byte-identical to the pre-compile DLL, roll
            // mtime back so downstream UTD doesn't see a spurious cascade —
            // but never below the freshest input. If we did, our own UTD
            // would fail next run (source/ref mtime > our restored mtime) and
            // we'd loop forever. The post-foreign-write recovery scenario
            // hits this: upstream got a fresh mtime, our content is unchanged
            // vs. the previous successful build, so prev_t < upstream.mtime.
            if let (Some(prev), Some(prev_t)) = (&prev_bytes, prev_mtime) {
                if let Ok(new) = fs::read(&out_dll) {
                    if prev == &new {
                        let target = max_input_mtime(&sources, common_refs, &proj_refs)
                            .map_or(prev_t, |m| prev_t.max(m));
                        let _ = restore_mtime(&out_dll, target);
                    }
                }
            }
            if let Err(e) = record_stamp_for(&out_dll, &stamp_path) {
                return ProjectOutcome::Io(io_err(&stamp_path, e));
            }
            ProjectOutcome::Recompiled
        }
        Err(stderr) => ProjectOutcome::Failed(stderr),
    }
}

/// Max mtime across every input the UTD predicate stats. Returned value is
/// the floor below which `restore_mtime` must not go — otherwise the next
/// UTD pass would see an input newer than out_dll and recompile forever.
#[doc(hidden)] pub fn max_input_mtime(
    sources: &[PathBuf],
    refs: &[DllRef],
    proj_refs: &[PathBuf],
) -> Option<u128> {
    let mut max: Option<u128> = None;
    let mut bump = |t: u128| {
        max = Some(max.map_or(t, |m| m.max(t)));
    };
    for s in sources {
        if let Some(t) = mtime_nsec(s) {
            bump(t);
        }
    }
    for r in refs {
        if let Some(t) = mtime_nsec(&r.path) {
            bump(t);
        }
    }
    for p in proj_refs {
        if let Some(t) = mtime_nsec(p) {
            bump(t);
        }
    }
    max
}

// ── data collection ───────────────────────────────────────────────────────

fn collect_sources(
    root: &str,
    asm: &AsmDefRecord,
    dirs_by_project: &HashMap<String, Vec<String>>,
) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Some(dirs) = dirs_by_project.get(&asm.name) else {
        return out;
    };
    for d in dirs {
        let dir = if d.is_empty() {
            PathBuf::from(root)
        } else {
            Path::new(root).join(d)
        };
        let Ok(rd) = fs::read_dir(&dir) else { continue };
        for entry in rd.filter_map(|e| e.ok()) {
            let p = entry.path();
            if p.extension().and_then(|s| s.to_str()) == Some("cs") {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

fn collect_project_refs(
    asm: &AsmDefRecord,
    included: &BTreeSet<String>,
    out_dir: &str,
) -> Vec<PathBuf> {
    asm.references
        .iter()
        .filter(|r| included.contains(*r))
        .map(|r| PathBuf::from(format!("{}/{}.dll", out_dir, r)))
        .collect()
}

fn collect_defines(lockfile: &Lockfile, platform: BuildPlatform, config: BuildConfig) -> Vec<String> {
    let mut out: Vec<String> = lockfile.defines.clone();
    out.extend(lockfile.defines_scripting.iter().cloned());
    out.extend(platform.platform_defines().iter().map(|s| s.to_string()));
    if config == BuildConfig::Editor {
        for d in ["UNITY_EDITOR", "UNITY_EDITOR_64", "UNITY_EDITOR_OSX"] {
            out.push(d.to_string());
        }
    }
    if matches!(config, BuildConfig::Editor | BuildConfig::Dev) {
        for d in ["DEBUG", "TRACE", "UNITY_ASSERTIONS"] {
            out.push(d.to_string());
        }
    }
    out
}

fn collect_refs(
    lockfile: &Lockfile,
    platform: BuildPlatform,
    config: BuildConfig,
    extra: &[DllRef],
) -> Vec<DllRef> {
    let is_editor = config == BuildConfig::Editor;
    let mut cats = vec![RefCategory::Engine];
    if is_editor {
        cats.push(RefCategory::Editor);
    }
    cats.push(RefCategory::PlaybackStandalone);
    match platform {
        BuildPlatform::Ios => cats.push(RefCategory::PlaybackIos),
        BuildPlatform::Android => cats.push(RefCategory::PlaybackAndroid),
        BuildPlatform::Osx => {}
    }
    cats.push(RefCategory::Project);
    cats.push(RefCategory::Netstandard);

    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut out: Vec<DllRef> = Vec::new();
    for c in cats {
        for r in lockfile.refs_for(c) {
            if seen.insert(r.name.clone()) {
                out.push(r.clone());
            }
        }
    }
    for r in extra {
        if seen.insert(r.name.clone()) {
            out.push(r.clone());
        }
    }
    out
}

// ── up-to-date check ──────────────────────────────────────────────────────

fn mtime_nsec(p: impl AsRef<Path>) -> Option<u128> {
    let m = fs::metadata(p.as_ref()).ok()?;
    let secs = m.mtime() as u128;
    let nsecs = m.mtime_nsec() as u128;
    Some(secs * 1_000_000_000 + nsecs)
}

/// Set `path`'s mtime to a previously-recorded `mtime_nsec` value.
/// Used to roll back csc's freshly-emitted mtime when the bytes match the
/// pre-compile bytes — see "content-hash UTD" in `run`.
#[doc(hidden)] pub fn restore_mtime(path: &str, mtime_ns: u128) -> std::io::Result<()> {
    let secs = (mtime_ns / 1_000_000_000) as u64;
    let nanos = (mtime_ns % 1_000_000_000) as u32;
    let t = UNIX_EPOCH + Duration::new(secs, nanos);
    let f = File::options().write(true).open(path)?;
    f.set_times(FileTimes::new().set_modified(t))?;
    Ok(())
}

#[doc(hidden)] pub fn is_up_to_date(
    sources: &[PathBuf],
    refs: &[DllRef],
    proj_refs: &[PathBuf],
    out_dll: &str,
    stamp_path: &str,
) -> bool {
    let Some(out_mtime) = mtime_nsec(out_dll) else {
        return false;
    };
    // Foreign-writer guard: a missing or stale stamp means something other
    // than this code wrote the DLL (e.g. `dotnet build`) — recompile.
    if read_stamp(stamp_path) != Some(out_mtime) {
        return false;
    }
    for s in sources {
        if mtime_nsec(s).map_or(true, |t| t > out_mtime) {
            return false;
        }
    }
    for r in refs {
        // Refs are pre-resolved (`$(UnityPath)` substituted) by `run`.
        // If a path doesn't exist, treat as dirty.
        if mtime_nsec(&r.path).map_or(true, |t| t > out_mtime) {
            return false;
        }
    }
    for p in proj_refs {
        if mtime_nsec(p).map_or(true, |t| t > out_mtime) {
            return false;
        }
    }
    true
}

// ── path layout + stamp sidecar ───────────────────────────────────────────

/// Directory where `typecheck` writes per-asmdef `<name>.dll`, `<name>.rsp`,
/// and `<name>.dll.usg-stamp`. Shares `<variant>/obj/Debug/` with `build`'s
/// MSBuild output so external consumers see fresh DLLs after a typecheck-only
/// flow; the stamp sidecar distinguishes our emits from foreign writers.
pub fn typecheck_output_dir(
    project_root: &str,
    generator_root: &str,
    platform: BuildPlatform,
    config: BuildConfig,
) -> String {
    format!(
        "{}/{}/{}-{}/obj/Debug",
        project_root, generator_root, platform.raw(), config.raw(),
    )
}

#[doc(hidden)] pub fn legacy_typecheck_dir(
    project_root: &str,
    generator_root: &str,
    platform: BuildPlatform,
    config: BuildConfig,
) -> String {
    format!(
        "{}/{}/typecheck-{}-{}",
        project_root, generator_root, platform.raw(), config.raw(),
    )
}

/// One-time cleanup of pre-consolidation `typecheck-<variant>/` output.
/// Idempotent: no-op when the dir doesn't exist.
#[doc(hidden)] pub fn purge_legacy_typecheck_dir(
    project_root: &str,
    generator_root: &str,
    platform: BuildPlatform,
    config: BuildConfig,
) {
    let path = legacy_typecheck_dir(project_root, generator_root, platform, config);
    if !Path::new(&path).is_dir() {
        return;
    }
    // Non-fatal: leftover legacy dir is disk waste, not a correctness issue.
    // `eprintln!` (not `tracing::warn!`) so it's visible without USG_PROFILE.
    if let Err(e) = fs::remove_dir_all(&path) {
        eprintln!("warning: failed to remove legacy typecheck dir {path}: {e}");
    }
}

#[doc(hidden)] pub fn stamp_path_for(out_dll: &str) -> String {
    format!("{}.usg-stamp", out_dll)
}

#[doc(hidden)] pub fn read_stamp(path: &str) -> Option<u128> {
    let s = fs::read_to_string(path).ok()?;
    s.trim().parse::<u128>().ok()
}

#[doc(hidden)] pub fn write_stamp(path: &str, mtime_ns: u128) -> std::io::Result<()> {
    fs::write(path, mtime_ns.to_string())
}

/// Stamp `out_dll` with its current on-disk mtime. Call after every emit so
/// the next UTD pass recognises us as the writer; absent or stale stamps
/// (from foreign writers like `dotnet build`) force recompile.
#[doc(hidden)] pub fn record_stamp_for(out_dll: &str, stamp_path: &str) -> std::io::Result<()> {
    let t = mtime_nsec(out_dll).ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::NotFound, format!("missing dll: {}", out_dll))
    })?;
    write_stamp(stamp_path, t)
}

// ── csc invocation ────────────────────────────────────────────────────────

fn find_csc_dll() -> Option<String> {
    // Parse `dotnet --list-sdks` output: "8.0.303 [/usr/local/share/dotnet/sdk]"
    // → /usr/local/share/dotnet/sdk/8.0.303/Roslyn/bincore/csc.dll. Pick the
    // highest semver — `dotnet`'s own sort order isn't contractually
    // ascending, and a future 9.0/10.0 should win even if listed first.
    let out = Command::new("dotnet").arg("--list-sdks").output().ok()?;
    if !out.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    let parse_semver = |s: &str| -> (u32, u32, u32) {
        let mut parts = s.split('.').map(|p| p.parse::<u32>().unwrap_or(0));
        (
            parts.next().unwrap_or(0),
            parts.next().unwrap_or(0),
            parts.next().unwrap_or(0),
        )
    };
    let best = stdout
        .lines()
        .filter_map(|l| {
            let l = l.trim();
            if l.is_empty() {
                return None;
            }
            let (version, rest) = l.split_once(' ')?;
            let base = rest.trim().trim_start_matches('[').trim_end_matches(']');
            Some((parse_semver(version), version.to_string(), base.to_string()))
        })
        .max_by_key(|t| t.0)?;
    let path = format!("{}/{}/Roslyn/bincore/csc.dll", best.2, best.1);
    if Path::new(&path).exists() {
        Some(path)
    } else {
        None
    }
}

#[doc(hidden)]
pub fn __test_only_build_rsp(
    lang_version: &str,
    defines: &[String],
    refs: &[DllRef],
    proj_refs: &[PathBuf],
    analyzers: &[String],
    sources: &[PathBuf],
    out_dll: &str,
    allow_unsafe: bool,
) -> String {
    build_rsp(lang_version, defines, refs, proj_refs, analyzers, sources, out_dll, allow_unsafe)
}

/// Test-only re-exports of internal path/stamp/UTD helpers. Stable surface
/// for `tests/typecheck_paths.rs`; not part of the library API.
#[doc(hidden)]
pub mod __test_only {
    pub use super::{
        is_up_to_date, legacy_typecheck_dir, max_input_mtime, purge_legacy_typecheck_dir,
        read_stamp, record_stamp_for, restore_mtime, stamp_path_for, typecheck_output_dir,
        write_stamp,
    };

    pub fn mtime_nsec(path: &str) -> Option<u128> {
        super::mtime_nsec(path)
    }
}

fn build_rsp(
    lang_version: &str,
    defines: &[String],
    refs: &[DllRef],
    proj_refs: &[PathBuf],
    analyzers: &[String],
    sources: &[PathBuf],
    out_dll: &str,
    allow_unsafe: bool,
) -> String {
    // `/noconfig` MUST go on the command line (not in the rsp) — otherwise csc
    // emits CS2023 and reads its default csc.rsp anyway. See `invoke_csc`.
    let mut s = String::new();
    s.push_str("/nostdlib+\n");
    s.push_str("/target:library\n");
    // Intentionally NOT `/refonly` — under .NET 8 SDK csc (4.10.x) it silently
    // skips body-binding diagnostics that don't affect the reference-assembly
    // surface, e.g. CS1503 at call sites. Hit on meow-tower `orgel-fix`:
    // USG reported `ok` while Unity Editor flagged a real type mismatch. We
    // emit a full library; `/deterministic` keeps output byte-identical for
    // unchanged inputs so the mtime-restore cascade-skip trick still works.
    s.push_str("/deterministic\n");
    s.push_str(&format!("/langversion:{}\n", lang_version));
    s.push_str(&format!("/out:{}\n", out_dll));
    if allow_unsafe {
        s.push_str("/unsafe+\n");
    }
    if !defines.is_empty() {
        s.push_str(&format!("/define:{}\n", defines.join(";")));
    }
    for r in refs {
        s.push_str(&format!("/reference:{}\n", r.path));
    }
    for p in proj_refs {
        s.push_str(&format!("/reference:{}\n", p.display()));
    }
    for a in analyzers {
        s.push_str(&format!("/analyzer:{}\n", a));
    }
    for src in sources {
        s.push_str(&format!("{}\n", src.display()));
    }
    s
}

fn invoke_csc(csc_dll: &str, rsp_path: &str) -> std::result::Result<(), String> {
    let out = Command::new("dotnet")
        .arg("exec")
        .arg(csc_dll)
        // `/noconfig` and `/shared` are client-only flags and MUST go on the
        // command line (not in the rsp — csc otherwise rejects them with
        // CS2007 / CS2023). `/shared` connects to a long-lived VBCSCompiler
        // over the named-pipe protocol; the server amortizes Roslyn JIT +
        // metadata-loading across calls (~390 ms saved per call after the
        // first). VBCSCompiler self-spawns on first connect, idles 10 min.
        .arg("/shared")
        .arg("/noconfig")
        .arg(format!("@{}", rsp_path))
        .output()
        .map_err(|e| format!("failed to spawn dotnet: {}", e))?;
    if out.status.success() {
        Ok(())
    } else {
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        Err(filter_diagnostics(&format!("{}{}", stdout, stderr)))
    }
}

/// Strip `warning CS####` and `info CS####` / `info USG####` lines from csc
/// output. Typecheck is for errors only; warnings repeat across assemblies
/// that share sources via asmref (e.g. `com.boxcat.libs` pulled into half a
/// dozen projects), and they're not actionable from the typecheck path.
/// Errors and the csc banner pass through untouched.
fn filter_diagnostics(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for line in s.lines() {
        // csc diagnostics: `<path>(L,C): <severity> <CODE>: <text>` or
        // `<severity> <CODE>: <text>` for tool-level info. Drop everything
        // except errors. Includes `info SP####` from DiagnosticSuppressor,
        // `info USG####` (Unity), `warning CS####`, etc.
        if line.contains(": warning ") || line.contains(": info ") {
            continue;
        }
        let t = line.trim_start();
        if t.starts_with("warning ") || t.starts_with("info ") {
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// True if `path` is a managed-code (CLI) PE binary. Reads enough of the PE
/// header to find the CLR Runtime Header data-directory entry (index 14 of 16);
/// if its RVA is non-zero, the file is a managed assembly.
///
/// Native DLLs (e.g. Unity's `unity_sprite_author.dll`) have an empty CLR
/// header entry; passing them via `/reference:` to csc fires CS0009. MSBuild's
/// `ResolveAssemblyReferences` task does this filter — we replicate it.
///
/// PE format reference:
/// <https://learn.microsoft.com/en-us/windows/win32/debug/pe-format>
fn is_managed_dll(path: &Path) -> bool {
    use std::io::Read;
    let mut f = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) => {
            tracing::warn!(
                "is_managed_dll: cannot open {} — dropping ref ({})",
                path.display(),
                e
            );
            return false;
        }
    };
    let mut buf = [0u8; 1024];
    let n = match f.read(&mut buf) {
        Ok(n) => n,
        Err(e) => {
            tracing::warn!(
                "is_managed_dll: read failed for {} — dropping ref ({})",
                path.display(),
                e
            );
            return false;
        }
    };
    if n < 0x40 {
        return false;
    }
    let e_lfanew = u32::from_le_bytes([buf[0x3c], buf[0x3d], buf[0x3e], buf[0x3f]]) as usize;
    // PE signature + COFF header (20) + Optional Header magic (2)
    if e_lfanew + 24 + 2 > n {
        return false;
    }
    if &buf[e_lfanew..e_lfanew + 4] != b"PE\0\0" {
        return false;
    }
    let opt_off = e_lfanew + 24;
    let magic = u16::from_le_bytes([buf[opt_off], buf[opt_off + 1]]);
    let dd_off = match magic {
        0x10b => opt_off + 96,  // PE32
        0x20b => opt_off + 112, // PE32+
        _ => return false,
    };
    // Data directory entry 14 = CLR Runtime Header. Each entry is 8 bytes
    // ({RVA: u32, Size: u32}). Non-zero RVA → managed.
    let clr_off = dd_off + 14 * 8;
    if clr_off + 4 > n {
        return false;
    }
    let clr_va = u32::from_le_bytes([buf[clr_off], buf[clr_off + 1], buf[clr_off + 2], buf[clr_off + 3]]);
    clr_va != 0
}
