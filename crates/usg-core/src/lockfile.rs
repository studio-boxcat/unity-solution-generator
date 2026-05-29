use std::collections::BTreeMap;

use crate::error::{LockfileError, Result};
use crate::io::{create_dir_all, read_file, write_file_if_changed};
use crate::lockfile_scanner;
use crate::paths::{lockfile_path, parent_directory, path_filename, read_unity_version};

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
                let filename = path_filename(&path);
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
    /// All variants in the canonical iteration order used by `lockfile::write`.
    /// The exhaustive match in `as_section`/`from_section` forces any new variant
    /// to be addressed at compile time.
    pub const ALL: [RefCategory; 8] = [
        RefCategory::Engine,
        RefCategory::Editor,
        RefCategory::Netstandard,
        RefCategory::PlaybackIos,
        RefCategory::PlaybackAndroid,
        RefCategory::PlaybackStandalone,
        RefCategory::PlaybackWindows,
        RefCategory::Project,
    ];

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

/// Load the lockfile, returning a fresh one if the on-disk version is
/// missing, malformed, or stale. Freshness criteria:
///   1. `lockfile.unity-version` matches `ProjectSettings/ProjectVersion.txt`
///   2. The `scan-cache` mtime fingerprint validates (no project-relevant
///      file has changed since the scan was written, which means the
///      derived lockfile is also still valid).
///
/// On miss → full rescan via [`lockfile_scanner::scan`] + atomic rewrite.
/// Watchman is invoked indirectly via the rescan path; the cache-hit path
/// is pure mtime-stat (sub-ms).
pub fn scan_and_write(project_root: &str, generator_root: &str) -> Result<Lockfile> {
    let path = lockfile_path(project_root, generator_root);
    create_dir_all(parent_directory(&path));

    // Fast path: lockfile exists, unity-version still matches, and the
    // scan-cache fingerprint says no project-relevant file has changed
    // since the scan (and therefore the lockfile) was written.
    if std::path::Path::new(&path).exists() {
        if let Ok(existing) = read(&path) {
            if let Some(current) = read_unity_version(project_root) {
                if current == existing.unity_version
                    && crate::project_scanner::scan_cache_fingerprint_matches(
                        project_root,
                        generator_root,
                    )
                {
                    return Ok(existing);
                }
            }
        }
    }

    let lockfile = lockfile_scanner::scan(project_root)?;
    write(&lockfile, &path)?;
    Ok(lockfile)
}

pub fn write(lockfile: &Lockfile, path: &str) -> Result<()> {
    let mut s = String::new();
    s.push_str("# csproj.lock — auto-generated by unity-solution-generator\n");
    s.push_str("# Refreshed on every typecheck/build when Watchman reports relevant changes.\n\n");
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

