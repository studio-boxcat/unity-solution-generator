use std::collections::BTreeMap;
use std::path::Path;

use crate::error::{GeneratorError, LockfileError, Result};
use crate::io::{create_dir_all, read_file, write_file_if_changed};
use crate::lockfile_scanner::LockfileScanner;
use crate::paths::{join_path, lockfile_path, parent_directory, read_unity_version};
use crate::scan::{Delta, ScanError, hint_is_relevant, since};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DllRef {
    pub name: String,
    pub path: String,
}

impl DllRef {
    pub fn new(name: impl Into<String>, path: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            path: path.into(),
        }
    }

    /// Parse a comma-separated list of absolute DLL paths, inferring each
    /// `name` from the filename (with the `.dll` suffix stripped).
    pub fn parse_list(comma_separated: &str) -> Vec<DllRef> {
        comma_separated
            .split(',')
            .filter(|s| !s.is_empty())
            .map(|part| {
                let path = part.to_string();
                let filename = path.rsplit('/').next().unwrap_or(&path);
                let name = filename
                    .strip_suffix(".dll")
                    .map(str::to_string)
                    .unwrap_or_else(|| filename.to_string());
                DllRef { name, path }
            })
            .collect()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum RefCategory {
    Engine,
    Editor,
    Netstandard,
    PlaybackIos,
    PlaybackAndroid,
    PlaybackStandalone,
    PlaybackWindows,
    Project,
}

impl RefCategory {
    /// All variants in the canonical iteration order used by `LockfileIO::write`.
    ///
    /// Adding a new variant has to update **both** the array literal AND the
    /// length below; the const assertion ties them together so a forgotten array
    /// entry fails compilation. The exhaustive match in `as_section`/`from_section`
    /// catches the variant on the first build attempt; this assertion catches it
    /// on the second (when the developer updates the match but forgets the array).
    pub const ALL: [RefCategory; Self::COUNT] = [
        RefCategory::Engine,
        RefCategory::Editor,
        RefCategory::Netstandard,
        RefCategory::PlaybackIos,
        RefCategory::PlaybackAndroid,
        RefCategory::PlaybackStandalone,
        RefCategory::PlaybackWindows,
        RefCategory::Project,
    ];

    /// Number of variants. Bumping this is enforced by the array literal above —
    /// the array length must match this constant or compilation fails.
    pub const COUNT: usize = {
        // One arm per variant. Adding a new enum variant fails the exhaustive
        // match here, forcing the developer to also bump COUNT and add to ALL.
        let count_per_variant = |v: RefCategory| -> usize {
            match v {
                RefCategory::Engine => 1,
                RefCategory::Editor => 1,
                RefCategory::Netstandard => 1,
                RefCategory::PlaybackIos => 1,
                RefCategory::PlaybackAndroid => 1,
                RefCategory::PlaybackStandalone => 1,
                RefCategory::PlaybackWindows => 1,
                RefCategory::Project => 1,
            }
        };
        let _ = count_per_variant;
        8
    };

    pub fn as_section(self) -> &'static str {
        match self {
            RefCategory::Engine => "refs.engine",
            RefCategory::Editor => "refs.editor",
            RefCategory::Netstandard => "refs.netstandard",
            RefCategory::PlaybackIos => "refs.playback.ios",
            RefCategory::PlaybackAndroid => "refs.playback.android",
            RefCategory::PlaybackStandalone => "refs.playback.standalone",
            RefCategory::PlaybackWindows => "refs.playback.windows",
            RefCategory::Project => "refs.project",
        }
    }

    pub fn from_section(name: &str) -> Option<Self> {
        Some(match name {
            "refs.engine" => RefCategory::Engine,
            "refs.editor" => RefCategory::Editor,
            "refs.netstandard" => RefCategory::Netstandard,
            "refs.playback.ios" => RefCategory::PlaybackIos,
            "refs.playback.android" => RefCategory::PlaybackAndroid,
            "refs.playback.standalone" => RefCategory::PlaybackStandalone,
            "refs.playback.windows" => RefCategory::PlaybackWindows,
            "refs.project" => RefCategory::Project,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone)]
pub struct Lockfile {
    pub unity_version: String,
    pub unity_path: String,
    pub lang_version: String,
    pub analyzers: Vec<String>,
    pub refs: BTreeMap<RefCategory, Vec<DllRef>>,
    pub defines: Vec<String>,
    pub defines_scripting: Vec<String>,
}

impl Lockfile {
    /// Build an empty lockfile shell with all `RefCategory` keys populated to
    /// empty Vecs. Useful for tests and for downstream crates that want to
    /// programmatically construct a Lockfile without spelling out every category.
    pub fn empty(unity_version: impl Into<String>, unity_path: impl Into<String>) -> Self {
        let mut refs: BTreeMap<RefCategory, Vec<DllRef>> = BTreeMap::new();
        for cat in RefCategory::ALL {
            refs.insert(cat, Vec::new());
        }
        Lockfile {
            unity_version: unity_version.into(),
            unity_path: unity_path.into(),
            lang_version: "9.0".to_string(),
            analyzers: Vec::new(),
            refs,
            defines: Vec::new(),
            defines_scripting: Vec::new(),
        }
    }

    pub fn total_ref_count(&self) -> usize {
        self.refs.values().map(|v| v.len()).sum()
    }

    pub fn refs_for(&self, cat: RefCategory) -> &[DllRef] {
        self.refs.get(&cat).map(Vec::as_slice).unwrap_or(&[])
    }
}

pub struct LockfileIO;

/// Sidecar file storing the Watchman clock at the time the lockfile was last
/// written. Lives next to `csproj.lock` in the generator root. Single line,
/// opaque token — never parsed.
const CLOCK_SIDECAR: &str = ".lock-watchman-clock";

impl LockfileIO {
    /// Scan + write the lockfile (creating the generator dir if needed).
    /// `generator_root` controls where `csproj.lock` and the `.lock-watchman-clock`
    /// sidecar live; pass [`DEFAULT_GENERATOR_ROOT`] for the standard layout.
    ///
    /// Short-circuits in two steps:
    ///   1. Read the existing lockfile + sidecar clock.
    ///   2. Query Watchman with the sidecar clock — if no project-relevant
    ///      path was touched AND the lockfile's `unity-version` still matches
    ///      `ProjectSettings/ProjectVersion.txt`, return the cached lockfile.
    /// Otherwise full rescan via [`LockfileScanner`] + rewrite both files.
    pub fn scan_and_write(project_root: &str, generator_root: &str) -> Result<Lockfile> {
        let path = lockfile_path(project_root, generator_root);
        let generator_dir = join_path(project_root, generator_root);
        let clock_path = join_path(&generator_dir, CLOCK_SIDECAR);
        create_dir_all(parent_directory(&path));

        // Fast path: lockfile and sidecar clock both exist, Watchman delta is
        // benign, and the editor version hasn't changed.
        if std::path::Path::new(&path).exists() {
            if let Ok(existing) = Self::read(&path) {
                if let Some(new_clock) =
                    check_invalidation(project_root, &clock_path, &existing)?
                {
                    // Cache hit. Update sidecar with the latest clock so the
                    // next call's `since(prev)` is incremental rather than
                    // re-touching everything since the last write.
                    let _ = write_file_if_changed(&clock_path, &new_clock);
                    return Ok(existing);
                }
            }
        }

        let scanned = LockfileScanner::scan_with_artifacts(project_root)?;
        Self::write(&scanned.lockfile, &path)?;

        // Persist the clock the scanner already captured. Best-effort: a write
        // failure forces a rescan on the next call but doesn't fail the
        // user-facing operation.
        let _ = write_file_if_changed(&clock_path, &scanned.watchman_clock);
        Ok(scanned.lockfile)
    }

    /// Ensure the lockfile is fresh and return it. Thin alias for
    /// [`scan_and_write`](Self::scan_and_write), which already short-circuits
    /// via the Watchman clock + Unity-version check when nothing relevant
    /// changed. A bare `read` would silently use a stale lockfile (e.g.
    /// references to files that no longer exist on disk) and surface as
    /// `MSB3245` from `dotnet build`.
    pub fn load_or_scan(project_root: &str, generator_root: &str) -> Result<Lockfile> {
        Self::scan_and_write(project_root, generator_root)
    }

    pub fn write(lockfile: &Lockfile, path: &str) -> Result<()> {
        let mut s = String::new();
        s.push_str("# csproj.lock — auto-generated by unity-solution-generator lock\n");
        s.push_str("# Re-run when: Unity version changes, packages added/removed\n\n");
        s.push_str(&format!("unity-version: {}\n", lockfile.unity_version));
        s.push_str(&format!("unity-path: {}\n", lockfile.unity_path));
        s.push_str(&format!("lang-version: {}\n", lockfile.lang_version));

        write_section(&mut s, "analyzers", &lockfile.analyzers);
        for cat in RefCategory::ALL {
            write_ref_section(&mut s, cat.as_section(), lockfile.refs_for(cat));
        }

        s.push_str("\n[defines]\n");
        s.push_str(&lockfile.defines.join(";"));
        s.push('\n');

        s.push_str("\n[defines.scripting]\n");
        s.push_str(&lockfile.defines_scripting.join(";"));
        s.push('\n');

        write_file_if_changed(path, &s)?;
        Ok(())
    }

    pub fn read(path: &str) -> Result<Lockfile> {
        let content = read_file(path)?;

        let mut unity_version = String::new();
        let mut unity_path = String::new();
        let mut lang_version = String::from("9.0");
        let mut analyzers: Vec<String> = Vec::new();
        let mut refs: BTreeMap<RefCategory, Vec<DllRef>> = BTreeMap::new();
        let mut defines: Vec<String> = Vec::new();
        let mut defines_scripting: Vec<String> = Vec::new();

        enum Section {
            Analyzers,
            Ref(RefCategory),
            Defines,
            DefinesScripting,
        }
        let mut current: Option<Section> = None;

        for line in content.split('\n') {
            if line.is_empty() || line.starts_with('#') {
                continue;
            }

            if line.starts_with('[') && line.ends_with(']') {
                let name = &line[1..line.len() - 1];
                current = if let Some(cat) = RefCategory::from_section(name) {
                    Some(Section::Ref(cat))
                } else {
                    match name {
                        "analyzers" => Some(Section::Analyzers),
                        "defines" => Some(Section::Defines),
                        "defines.scripting" => Some(Section::DefinesScripting),
                        _ => None,
                    }
                };
                continue;
            }

            match &current {
                None => {
                    if let Some((k, v)) = parse_header_line(line) {
                        match k {
                            "unity-version" => unity_version = v.to_string(),
                            "unity-path" => unity_path = v.to_string(),
                            "lang-version" => lang_version = v.to_string(),
                            _ => {}
                        }
                    }
                }
                Some(Section::Analyzers) => analyzers.push(line.to_string()),
                Some(Section::Ref(cat)) => {
                    if let Some(r) = parse_dll_ref(line) {
                        refs.entry(*cat).or_default().push(r);
                    }
                }
                // The writer always emits a single semicolon-delimited line per
                // section, but a hand-edited or future writer could spill across
                // multiple lines. Use `extend` so we don't silently drop everything
                // but the last line.
                Some(Section::Defines) => {
                    if !line.is_empty() {
                        defines.extend(line.split(';').map(str::to_string));
                    }
                }
                Some(Section::DefinesScripting) => {
                    if !line.is_empty() {
                        defines_scripting.extend(line.split(';').map(str::to_string));
                    }
                }
            }
        }

        if unity_version.is_empty() {
            return Err(LockfileError::InvalidLockfile("missing unity-version".into()).into());
        }
        if unity_path.is_empty() {
            return Err(LockfileError::InvalidLockfile("missing unity-path".into()).into());
        }

        Ok(Lockfile {
            unity_version,
            unity_path,
            lang_version,
            analyzers,
            refs,
            defines,
            defines_scripting,
        })
    }
}

fn write_section(s: &mut String, name: &str, lines: &[String]) {
    s.push_str(&format!("\n[{}]\n", name));
    for line in lines {
        s.push_str(line);
        s.push('\n');
    }
}

fn write_ref_section(s: &mut String, name: &str, refs: &[DllRef]) {
    s.push_str(&format!("\n[{}]\n", name));
    for r in refs {
        s.push_str(&r.name);
        s.push('|');
        s.push_str(&r.path);
        s.push('\n');
    }
}

fn parse_header_line(line: &str) -> Option<(&str, &str)> {
    let (key, value) = line.split_once(':')?;
    Some((key, value.trim()))
}

fn parse_dll_ref(line: &str) -> Option<DllRef> {
    let (name, path) = line.split_once('|')?;
    Some(DllRef {
        name: name.to_string(),
        path: path.to_string(),
    })
}

/// Check whether the existing lockfile is still valid. Returns `Some(new_clock)`
/// on cache hit (no Unity-version change, no project-relevant Watchman delta);
/// `None` if anything forces a rescan. Watchman `Unavailable` is surfaced as
/// an error so callers don't silently re-enter the scanner (which would
/// produce a confusingly-different error path) — `Query` errors are logged
/// and treated as a rescan trigger.
fn check_invalidation(
    project_root: &str,
    clock_path: &str,
    existing: &Lockfile,
) -> Result<Option<String>> {
    // 1. Unity version equality — cheap.
    let Some(current_version) = read_unity_version(project_root) else {
        return Ok(None); // Missing ProjectVersion.txt; let the rescanner fail loudly.
    };
    if current_version != existing.unity_version {
        return Ok(None);
    }

    // 2. Watchman delta from the previous successful write.
    let prev_clock = read_file(clock_path).ok();
    let prev_clock_ref = prev_clock.as_deref();
    let delta = match since(Path::new(project_root), prev_clock_ref) {
        Ok(d) => d,
        Err(ScanError::Unavailable) => {
            return Err(GeneratorError::ScanUnavailable(ScanError::Unavailable.to_string()));
        }
        Err(e @ ScanError::Query(_)) => {
            tracing::warn!("lockfile cache check: watchman query failed, rescanning ({e})");
            return Ok(None);
        }
    };
    Ok(match delta {
        Delta::Fresh { .. } => None,
        Delta::Touched { paths, new_clock } => {
            if paths.iter().any(|p| hint_is_relevant(p)) {
                None
            } else {
                Some(new_clock)
            }
        }
    })
}
