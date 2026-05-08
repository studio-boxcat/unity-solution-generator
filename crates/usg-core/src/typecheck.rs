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

use crate::error::{Result, io_err};
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
    let order = topo_sort(&included, &scan.asm_def_by_name);

    // Output dir: per-variant under generator root, gitignored alongside other caches.
    let variant = format!("{}-{}", opts.platform.raw(), opts.build_config.raw());
    let out_dir = format!("{}/{}/typecheck-{}", root, DEFAULT_GENERATOR_ROOT, variant);
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
    // Resolve MSBuild-style properties (`$(UnityPath)`, `$(ProjectRoot)`)
    // in lockfile paths now — MSBuild does this at eval time but `csc.dll`
    // doesn't recognize MSBuild property syntax. Applies to refs AND analyzers.
    let resolve = |s: &str| -> String {
        s.replace("$(UnityPath)", &lockfile.unity_path)
            .replace("$(ProjectRoot)", &root)
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
                tracing::debug!(target: "usg_core::typecheck", path = %r.path, "filtered: not a managed DLL");
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

    for name in &order {
        let asm = &scan.asm_def_by_name[name];

        // Cascade skip: if any upstream failed, this one would just spew
        // CS0006 ("metadata file ... could not be found") for the missing
        // upstream `.dll`. Surface as a clear "skipped" rather than a real
        // compile error.
        if let Some(dep) = asm.references.iter().find(|r| failed_set.contains(*r)) {
            failures.insert(
                name.clone(),
                format!("skipped (cascade): upstream '{}' failed", dep),
            );
            failed_set.insert(name.clone());
            continue;
        }

        let sources = collect_sources(&root, asm, &scan.dirs_by_project);
        if sources.is_empty() {
            // No source files — nothing to compile (matches generate's behaviour
            // of skipping empty projects from variant inclusion).
            continue;
        }

        let proj_refs = collect_project_refs(asm, &included, &out_dir);
        let out_dll = format!("{}/{}.dll", out_dir, name);

        if is_up_to_date(&sources, &common_refs, &proj_refs, &out_dll) {
            skipped += 1;
            continue;
        }

        let mut defines = common_defines.clone();
        for vd in &asm.version_defines {
            defines.push(vd.define.clone());
        }
        defines.extend(asm.include_platforms.iter().cloned());
        let lang_version = lockfile.lang_version.clone();

        let rsp_path = format!("{}/{}.rsp", out_dir, name);
        let rsp_body = build_rsp(
            &lang_version,
            &defines,
            &common_refs,
            &proj_refs,
            &analyzers,
            &sources,
            &out_dll,
            asm.allow_unsafe_code,
        );
        fs::write(&rsp_path, rsp_body).map_err(|e| io_err(&rsp_path, e))?;

        // Snapshot pre-compile bytes + mtime. csc with `/refonly /deterministic`
        // produces byte-identical output for unchanged inputs, but the .dll's
        // mtime advances on every emit — that cascades into spurious downstream
        // rebuilds (downstream's UTD sees `upstream.dll` newer than its own
        // output). Compare bytes after compile; if identical, restore the old
        // mtime so the cascade never starts.
        let prev_bytes = fs::read(&out_dll).ok();
        let prev_mtime = mtime_nsec(&out_dll);

        match invoke_csc(&csc_dll, &rsp_path) {
            Ok(()) => {
                if let (Some(prev), Some(prev_t)) = (&prev_bytes, prev_mtime) {
                    if let Ok(new) = fs::read(&out_dll) {
                        if prev == &new {
                            // Bytes identical — restore old mtime. Errors here
                            // are non-fatal (worst case: spurious rebuild next run).
                            let _ = restore_mtime(&out_dll, prev_t);
                        }
                    }
                }
                recompiled += 1;
            }
            Err(stderr) => {
                failures.insert(name.clone(), stderr);
                failed_set.insert(name.clone());
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

fn topo_sort(
    included: &BTreeSet<String>,
    asm_def_by_name: &HashMap<String, AsmDefRecord>,
) -> Vec<String> {
    // Iterative Kahn's. Stable ordering: within a level, sort lexicographically
    // so the output is deterministic across runs (helps caching + debugging).
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
    let mut ready: BTreeSet<String> = indeg
        .iter()
        .filter(|(_, &d)| d == 0)
        .map(|(n, _)| n.clone())
        .collect();
    let mut out = Vec::with_capacity(included.len());
    while let Some(n) = ready.iter().next().cloned() {
        ready.remove(&n);
        if let Some(succs) = adj.get(&n) {
            for s in succs {
                if let Some(d) = indeg.get_mut(s) {
                    *d -= 1;
                    if *d == 0 {
                        ready.insert(s.clone());
                    }
                }
            }
        }
        out.push(n);
    }
    out
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
) -> bool {
    let Some(out_mtime) = mtime_nsec(out_dll) else {
        return false;
    };
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

// ── csc invocation ────────────────────────────────────────────────────────

fn find_csc_dll() -> Option<String> {
    // Reads `dotnet --list-sdks` output: "8.0.303 [/usr/local/share/dotnet/sdk]"
    // → /usr/local/share/dotnet/sdk/8.0.303/Roslyn/bincore/csc.dll. Picks the
    // last (newest) line. Skips bracket parsing if format changes.
    let out = Command::new("dotnet").arg("--list-sdks").output().ok()?;
    if !out.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    let last = stdout.lines().filter(|l| !l.trim().is_empty()).next_back()?;
    let (version, rest) = last.split_once(' ')?;
    let base = rest.trim().trim_start_matches('[').trim_end_matches(']');
    let path = format!("{}/{}/Roslyn/bincore/csc.dll", base, version);
    if Path::new(&path).exists() {
        Some(path)
    } else {
        None
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
    s.push_str("/refonly\n"); // metadata-only assembly — same as generate's no-emit mode
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
        // `/noconfig` belongs on the command line, not in the rsp (csc warns
        // CS2023 if it appears in a response file and silently reads its
        // default csc.rsp anyway).
        .arg("/noconfig")
        .arg(format!("@{}", rsp_path))
        .output()
        .map_err(|e| format!("failed to spawn dotnet: {}", e))?;
    if out.status.success() {
        Ok(())
    } else {
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        Err(format!("{}{}", stdout, stderr))
    }
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
    let Ok(mut f) = std::fs::File::open(path) else {
        return false;
    };
    let mut buf = [0u8; 1024];
    let Ok(n) = f.read(&mut buf) else { return false };
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
