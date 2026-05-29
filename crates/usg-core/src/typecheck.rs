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
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use rayon::prelude::*;

use crate::csc::{self, BuildRspInputs};
use crate::error::{GeneratorError, Result, io_err};
use crate::lockfile::{DllRef, Lockfile};
use crate::paths::{DEFAULT_GENERATOR_ROOT, mtime_nanos_for, resolve_real_path};
use crate::project_scanner::{AsmDefRecord, ProjectCategory, ProjectName};
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
/// Caller pre-computes scan + lockfile so a single CLI invocation hits the
/// project tree exactly once.
pub fn run(
    opts: &TypecheckOptions,
    lockfile: &Lockfile,
    scan: &crate::project_scanner::ScanResult,
) -> Result<TypecheckResult> {
    let _span = tracing::info_span!("typecheck.run").entered();
    let root = resolve_real_path(&opts.project_root);

    let included = compute_included_projects(&scan.asm_def_by_name, opts);
    let levels = topo_levels(&included, &scan.asm_def_by_name);

    // Shares `<variant>/obj/Debug/` with `build`'s MSBuild output so consumers
    // see fresh DLLs after a typecheck-only flow. Foreign writers are detected
    // per-DLL via `.usg-stamp` sidecars — see `is_up_to_date`.
    let out_dir = typecheck_output_dir(&root, DEFAULT_GENERATOR_ROOT, opts.platform, opts.build_config);
    fs::create_dir_all(&out_dir).map_err(|e| io_err(&out_dir, e))?;

    let csc_dll = csc::find_csc_dll_cached(&lockfile.unity_version).ok_or_else(|| {
        io_err(
            "csc.dll",
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "csc.dll not found — run `dotnet --list-sdks` to confirm a .NET SDK is installed",
            ),
        )
    })?;

    let common_defines = collect_defines(lockfile, opts.platform, opts.build_config);
    let common_refs = collect_refs(lockfile, opts.platform, opts.build_config, &opts.extra_refs);
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
            if crate::pe::is_managed_dll(Path::new(&r.path)) {
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
        .filter(|a| crate::pe::is_managed_dll(Path::new(a)))
        .collect();

    let mut recompiled = 0usize;
    let mut skipped = 0usize;
    let mut failures = BTreeMap::new();
    // Tracks which projects' compiles failed (or were cascade-skipped). When a
    // downstream project references one of these, we skip rather than invoking
    // csc on a doomed compile that produces a wall of CS0006 noise.
    let mut failed_set: BTreeSet<ProjectName> = BTreeSet::new();

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
        let outcomes: Vec<(ProjectName, ProjectOutcome)> = level
            .par_iter()
            .map(|name| {
                let outcome = compile_project(
                    name,
                    scan,
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
                        name.to_string(),
                        format!("skipped (cascade): upstream '{}' failed", dep),
                    );
                    failed_set.insert(name);
                }
                ProjectOutcome::Failed(stderr) => {
                    failures.insert(name.to_string(), stderr);
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
    asm_def_by_name: &HashMap<ProjectName, AsmDefRecord>,
    opts: &TypecheckOptions,
) -> BTreeSet<ProjectName> {
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
    included: &BTreeSet<ProjectName>,
    asm_def_by_name: &HashMap<ProjectName, AsmDefRecord>,
) -> Vec<Vec<ProjectName>> {
    let mut indeg: HashMap<ProjectName, usize> = HashMap::new();
    let mut adj: HashMap<ProjectName, Vec<ProjectName>> = HashMap::new();
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

    let mut levels: Vec<Vec<ProjectName>> = Vec::new();
    let mut current: BTreeSet<ProjectName> = indeg
        .iter()
        .filter(|(_, &d)| d == 0)
        .map(|(n, _)| n.clone())
        .collect();
    while !current.is_empty() {
        let mut next: BTreeSet<ProjectName> = BTreeSet::new();
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
    CascadeSkipped(ProjectName),
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
    name: &ProjectName,
    scan: &crate::project_scanner::ScanResult,
    included: &BTreeSet<ProjectName>,
    failed_set: &BTreeSet<ProjectName>,
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
    let rsp_body = csc::build_rsp(&BuildRspInputs {
        lang_version,
        defines: &defines,
        refs: common_refs,
        proj_refs: &proj_refs,
        analyzers,
        sources: &sources,
        out_dll: &out_dll,
        allow_unsafe: asm.allow_unsafe_code,
    });
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
    let prev_mtime = mtime_nanos_for(&out_dll);

    match csc::invoke_csc(csc_dll, &rsp_path) {
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
fn max_input_mtime(
    sources: &[PathBuf],
    refs: &[DllRef],
    proj_refs: &[PathBuf],
) -> Option<u128> {
    let mut max: Option<u128> = None;
    let mut bump = |t: u128| {
        max = Some(max.map_or(t, |m| m.max(t)));
    };
    for s in sources {
        if let Some(t) = mtime_nanos_for(s) {
            bump(t);
        }
    }
    for r in refs {
        if let Some(t) = mtime_nanos_for(&r.path) {
            bump(t);
        }
    }
    for p in proj_refs {
        if let Some(t) = mtime_nanos_for(p) {
            bump(t);
        }
    }
    max
}

// ── data collection ───────────────────────────────────────────────────────

fn collect_sources(
    root: &str,
    asm: &AsmDefRecord,
    dirs_by_project: &HashMap<ProjectName, Vec<String>>,
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
    included: &BTreeSet<ProjectName>,
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
        out.extend(crate::defines::EDITOR_DEFINES_BASE.iter().map(|s| s.to_string()));
        out.push(crate::defines::editor_host_define().to_string());
    }
    if matches!(config, BuildConfig::Editor | BuildConfig::Dev) {
        out.extend(crate::defines::DEBUG_DEFINES.iter().map(|s| s.to_string()));
    }
    out
}

fn collect_refs(
    lockfile: &Lockfile,
    platform: BuildPlatform,
    config: BuildConfig,
    extra: &[DllRef],
) -> Vec<DllRef> {
    let cats = platform.ref_categories(config == BuildConfig::Editor);

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

/// Set `path`'s mtime to a previously-recorded `mtime_nanos_for` value.
/// Used to roll back csc's freshly-emitted mtime when the bytes match the
/// pre-compile bytes — see "content-hash UTD" in `run`.
fn restore_mtime(path: &str, mtime_ns: u128) -> std::io::Result<()> {
    let secs = (mtime_ns / 1_000_000_000) as u64;
    let nanos = (mtime_ns % 1_000_000_000) as u32;
    let t = UNIX_EPOCH + Duration::new(secs, nanos);
    let f = File::options().write(true).open(path)?;
    f.set_times(FileTimes::new().set_modified(t))?;
    Ok(())
}

fn is_up_to_date(
    sources: &[PathBuf],
    refs: &[DllRef],
    proj_refs: &[PathBuf],
    out_dll: &str,
    stamp_path: &str,
) -> bool {
    let Some(out_mtime) = mtime_nanos_for(out_dll) else {
        return false;
    };
    // Foreign-writer guard: a missing or stale stamp means something other
    // than this code wrote the DLL (e.g. `dotnet build`) — recompile.
    if read_stamp(stamp_path) != Some(out_mtime) {
        return false;
    }
    for s in sources {
        if mtime_nanos_for(s).is_none_or(|t| t > out_mtime) {
            return false;
        }
    }
    for r in refs {
        // Refs are pre-resolved (`$(UnityPath)` substituted) by `run`.
        // If a path doesn't exist, treat as dirty.
        if mtime_nanos_for(&r.path).is_none_or(|t| t > out_mtime) {
            return false;
        }
    }
    for p in proj_refs {
        if mtime_nanos_for(p).is_none_or(|t| t > out_mtime) {
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
        project_root, generator_root, platform, config,
    )
}

fn stamp_path_for(out_dll: &str) -> String {
    format!("{}.usg-stamp", out_dll)
}

fn read_stamp(path: &str) -> Option<u128> {
    let s = fs::read_to_string(path).ok()?;
    s.trim().parse::<u128>().ok()
}

fn write_stamp(path: &str, mtime_ns: u128) -> std::io::Result<()> {
    fs::write(path, mtime_ns.to_string())
}

/// Stamp `out_dll` with its current on-disk mtime. Call after every emit so
/// the next UTD pass recognises us as the writer; absent or stale stamps
/// (from foreign writers like `dotnet build`) force recompile.
fn record_stamp_for(out_dll: &str, stamp_path: &str) -> std::io::Result<()> {
    let t = mtime_nanos_for(out_dll).ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::NotFound, format!("missing dll: {}", out_dll))
    })?;
    write_stamp(stamp_path, t)
}

#[cfg(test)]
mod tests {
    //! Path layout, stamp sidecar, and up-to-date predicate. These pin the
    //! consolidated `obj/Debug` layout (see architecture.md "On-disk layout"
    //! and "typecheck deeper") and the foreign-writer guard via per-DLL
    //! `.usg-stamp` sidecars: when `typecheck` shares `<variant>/obj/Debug/`
    //! with `build`'s MSBuild output, a naive mtime-only UTD would silently
    //! SKIP after `dotnet build` wrote a fresh DLL — typecheck would
    //! rubber-stamp MSBuild's compile instead of running csc.
    use super::*;
    use crate::lockfile::DllRef;
    use std::fs;

    const GR: &str = "Library/USG";

    fn temp() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn output_dir_is_under_variant_obj_debug() {
        let dir = typecheck_output_dir("/proj", GR, BuildPlatform::Ios, BuildConfig::Editor);
        assert_eq!(dir, "/proj/Library/USG/ios-editor/obj/Debug");
    }

    #[test]
    fn output_dir_varies_with_variant() {
        let a = typecheck_output_dir("/proj", GR, BuildPlatform::Android, BuildConfig::Dev);
        assert_eq!(a, "/proj/Library/USG/android-dev/obj/Debug");
    }

    #[test]
    fn stamp_path_is_dll_dot_usg_stamp() {
        assert_eq!(stamp_path_for("/x/Foo.dll"), "/x/Foo.dll.usg-stamp");
    }

    #[test]
    fn stamp_roundtrip() {
        let tmp = temp();
        let p = tmp.path().join("Foo.dll.usg-stamp");
        let path = p.to_str().unwrap();
        write_stamp(path, 1234567890u128).unwrap();
        assert_eq!(read_stamp(path), Some(1234567890u128));
    }

    #[test]
    fn stamp_read_returns_none_when_absent() {
        let tmp = temp();
        let p = tmp.path().join("nope.usg-stamp");
        assert_eq!(read_stamp(p.to_str().unwrap()), None);
    }

    #[test]
    fn stamp_read_returns_none_on_garbage() {
        let tmp = temp();
        let p = tmp.path().join("garbage.usg-stamp");
        fs::write(&p, b"not-a-number\n").unwrap();
        assert_eq!(read_stamp(p.to_str().unwrap()), None);
    }

    #[test]
    fn utd_false_when_dll_missing() {
        let tmp = temp();
        let dll = tmp.path().join("Foo.dll");
        let stamp = tmp.path().join("Foo.dll.usg-stamp");
        // No DLL on disk; stamp irrelevant.
        assert!(!is_up_to_date(&[], &[], &[], dll.to_str().unwrap(), stamp.to_str().unwrap()));
    }

    #[test]
    fn utd_false_when_stamp_missing() {
        // The DLL exists but no `.usg-stamp` next to it. This is exactly the
        // post-`dotnet build` state: MSBuild wrote the DLL, we did not.
        // Without the stamp guard, typecheck would silently rubber-stamp it.
        let tmp = temp();
        let dll = tmp.path().join("Foo.dll");
        fs::write(&dll, b"foreign\n").unwrap();
        let stamp = tmp.path().join("Foo.dll.usg-stamp");
        assert!(!is_up_to_date(&[], &[], &[], dll.to_str().unwrap(), stamp.to_str().unwrap()));
    }

    #[test]
    fn utd_false_when_stamp_mtime_disagrees_with_disk() {
        // Stamp recorded a different mtime than what's on disk → foreign writer
        // touched the DLL since we stamped it.
        let tmp = temp();
        let dll = tmp.path().join("Foo.dll");
        fs::write(&dll, b"x").unwrap();
        let stamp = tmp.path().join("Foo.dll.usg-stamp");
        write_stamp(stamp.to_str().unwrap(), 1u128).unwrap(); // bogus mtime
        assert!(!is_up_to_date(&[], &[], &[], dll.to_str().unwrap(), stamp.to_str().unwrap()));
    }

    #[test]
    fn utd_true_when_stamp_matches_and_inputs_older() {
        let tmp = temp();
        let dll = tmp.path().join("Foo.dll");
        fs::write(&dll, b"x").unwrap();
        let dll_mtime = mtime_nanos_for(dll.to_str().unwrap()).unwrap();
        let stamp = tmp.path().join("Foo.dll.usg-stamp");
        write_stamp(stamp.to_str().unwrap(), dll_mtime).unwrap();
        assert!(is_up_to_date(&[], &[], &[], dll.to_str().unwrap(), stamp.to_str().unwrap()));
    }

    #[test]
    fn record_stamp_after_write_makes_utd_true() {
        // Producer side: write a DLL, stamp it with its own mtime, then UTD
        // should be true on the next pass with no source changes.
        let tmp = temp();
        let dll = tmp.path().join("Foo.dll");
        fs::write(&dll, b"contents").unwrap();
        let stamp = tmp.path().join("Foo.dll.usg-stamp");
        record_stamp_for(dll.to_str().unwrap(), stamp.to_str().unwrap()).unwrap();
        assert!(is_up_to_date(&[], &[], &[], dll.to_str().unwrap(), stamp.to_str().unwrap()));
    }

    #[test]
    fn foreign_overwrite_after_stamp_breaks_utd() {
        // Stamp the DLL, then simulate `dotnet build` overwriting it. UTD must
        // flip to false so the next typecheck recompiles and re-stamps.
        let tmp = temp();
        let dll = tmp.path().join("Foo.dll");
        fs::write(&dll, b"v1").unwrap();
        let stamp = tmp.path().join("Foo.dll.usg-stamp");
        record_stamp_for(dll.to_str().unwrap(), stamp.to_str().unwrap()).unwrap();
        assert!(is_up_to_date(&[], &[], &[], dll.to_str().unwrap(), stamp.to_str().unwrap()));

        // Sleep a hair to guarantee mtime advances on filesystems with coarse
        // resolution, then overwrite. (apfs is nanosecond; ext4 may be too
        // coarse without this nudge.)
        std::thread::sleep(std::time::Duration::from_millis(10));
        fs::write(&dll, b"foreign-bytes").unwrap();
        assert!(!is_up_to_date(&[], &[], &[], dll.to_str().unwrap(), stamp.to_str().unwrap()));
    }

    #[test]
    fn restored_mtime_floors_at_freshest_input() {
        // After a foreign upstream write, downstream recompiles, csc emits the
        // same bytes (deterministic), and we restore the mtime. If we restored
        // to the OLD prev_t, downstream's mtime would be below its proj_ref's
        // mtime and UTD would fail forever — every subsequent typecheck would
        // recompile downstream. max_input_mtime is the floor that breaks this.
        let tmp = temp();
        let dll = tmp.path().join("Downstream.dll");
        let proj_ref = tmp.path().join("Upstream.dll");

        fs::write(&dll, b"downstream-bytes").unwrap();
        let prev_t = mtime_nanos_for(dll.to_str().unwrap()).unwrap();

        // proj_ref written AFTER downstream → fresher mtime, simulating the
        // post-foreign-write upstream recovery.
        std::thread::sleep(std::time::Duration::from_millis(10));
        fs::write(&proj_ref, b"upstream-bytes").unwrap();
        let upstream_t = mtime_nanos_for(proj_ref.to_str().unwrap()).unwrap();
        assert!(upstream_t > prev_t);

        let target = max_input_mtime(&[], &[], &[PathBuf::from(&proj_ref)])
            .map_or(prev_t, |m| prev_t.max(m));
        assert_eq!(
            target, upstream_t,
            "restore target must rise to upstream's mtime, not stay at prev_t",
        );

        // Apply the restore + stamp and verify UTD passes against the upstream input.
        restore_mtime(dll.to_str().unwrap(), target).unwrap();
        let stamp = tmp.path().join("Downstream.dll.usg-stamp");
        record_stamp_for(dll.to_str().unwrap(), stamp.to_str().unwrap()).unwrap();
        assert!(
            is_up_to_date(
                &[],
                &[],
                &[PathBuf::from(&proj_ref)],
                dll.to_str().unwrap(),
                stamp.to_str().unwrap(),
            ),
            "downstream must UTD-skip on the run after cascade recovery — no infinite loop",
        );
    }

    #[test]
    fn max_input_mtime_picks_freshest_across_categories() {
        let tmp = temp();
        let a = tmp.path().join("a.cs");
        let b = tmp.path().join("b.dll");
        let c = tmp.path().join("c.dll");
        fs::write(&a, b"a").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
        fs::write(&b, b"b").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
        fs::write(&c, b"c").unwrap();

        let max = max_input_mtime(
            &[PathBuf::from(&a)],
            &[DllRef::new("B", b.to_str().unwrap())],
            &[PathBuf::from(&c)],
        )
        .unwrap();
        assert_eq!(max, mtime_nanos_for(c.to_str().unwrap()).unwrap());
    }

    #[test]
    fn max_input_mtime_none_when_no_inputs() {
        assert!(max_input_mtime(&[], &[], &[]).is_none());
    }

    #[test]
    fn utd_false_when_source_newer_than_dll() {
        // The "source touched after last emit" branch of is_up_to_date. Stamp +
        // disk-mtime can agree, but a fresh source still forces recompile.
        let tmp = temp();
        let dll = tmp.path().join("Foo.dll");
        fs::write(&dll, b"x").unwrap();
        let stamp = tmp.path().join("Foo.dll.usg-stamp");
        record_stamp_for(dll.to_str().unwrap(), stamp.to_str().unwrap()).unwrap();

        // Source written AFTER the DLL+stamp → newer mtime.
        std::thread::sleep(std::time::Duration::from_millis(10));
        let src = tmp.path().join("Foo.cs");
        fs::write(&src, b"class Foo {}\n").unwrap();
        assert!(!is_up_to_date(
            &[PathBuf::from(&src)],
            &[],
            &[],
            dll.to_str().unwrap(),
            stamp.to_str().unwrap(),
        ));
    }

    #[test]
    fn utd_false_when_ref_newer_than_dll() {
        // The "external DllRef touched after last emit" branch — e.g. Unity
        // updated, advancing the engine DLL mtime under us.
        let tmp = temp();
        let dll = tmp.path().join("Foo.dll");
        fs::write(&dll, b"x").unwrap();
        let stamp = tmp.path().join("Foo.dll.usg-stamp");
        record_stamp_for(dll.to_str().unwrap(), stamp.to_str().unwrap()).unwrap();

        std::thread::sleep(std::time::Duration::from_millis(10));
        let r = tmp.path().join("UnityEngine.dll");
        fs::write(&r, b"engine-bytes").unwrap();
        assert!(!is_up_to_date(
            &[],
            &[DllRef::new("UnityEngine", r.to_str().unwrap())],
            &[],
            dll.to_str().unwrap(),
            stamp.to_str().unwrap(),
        ));
    }
}
