use std::collections::BTreeMap;
use std::path::Path;

use crate::error::{LockfileError, Result};
use crate::io::{create_dir_all, read_file, write_file_if_changed};
use crate::json::trim_ws;
use crate::lock_cache;
use crate::lockfile_scanner::LockfileScanner;
use crate::paths::{DEFAULT_GENERATOR_ROOT, join_path, lockfile_path, parent_directory};

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
    Project,
}

impl RefCategory {
    pub const ALL: [RefCategory; 7] = [
        RefCategory::Engine,
        RefCategory::Editor,
        RefCategory::Netstandard,
        RefCategory::PlaybackIos,
        RefCategory::PlaybackAndroid,
        RefCategory::PlaybackStandalone,
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
    pub fn total_ref_count(&self) -> usize {
        self.refs.values().map(|v| v.len()).sum()
    }

    pub fn refs_for(&self, cat: RefCategory) -> &[DllRef] {
        self.refs.get(&cat).map(Vec::as_slice).unwrap_or(&[])
    }
}

pub struct LockfileIO;

impl LockfileIO {
    /// Scan + write the lockfile for `project_root` (creating the generator dir if needed).
    ///
    /// Short-circuits when the recorded fingerprint (see [`lock_cache`]) shows nothing
    /// has changed since the last `lock`. In the cache-hit path no Unity-install scan
    /// or project-side walk runs; we just refresh the fingerprint timestamps and return
    /// the existing lockfile.
    pub fn scan_and_write(project_root: &str) -> Result<Lockfile> {
        let path = lockfile_path(project_root);
        let generator_dir = join_path(project_root, DEFAULT_GENERATOR_ROOT);
        let fp_path = lock_cache::fingerprint_path(&generator_dir);
        create_dir_all(parent_directory(&path));

        // Fast path: fingerprint matches and lockfile is still on disk.
        if std::path::Path::new(&path).exists() {
            if let Some(entries) = lock_cache::load(&fp_path) {
                if lock_cache::is_valid(&entries) {
                    if let Ok(existing) = Self::read(&path) {
                        return Ok(existing);
                    }
                }
            }
        }

        let scanned = LockfileScanner::scan_with_artifacts(project_root)?;
        Self::write(&scanned.lockfile, &path)?;

        let entries = lock_cache::build_entries(
            project_root,
            &scanned.lockfile.unity_path,
            &scanned.contributing_paths_relative,
        );
        // Best-effort: a fingerprint write failure must not fail the user-facing
        // operation; we'd just rescan next time.
        let _ = lock_cache::write(&fp_path, &scanned.lockfile.unity_version, &entries);
        Ok(scanned.lockfile)
    }

    /// Read the lockfile if present, else scan + write a fresh one.
    pub fn load_or_scan(project_root: &str) -> Result<Lockfile> {
        let path = lockfile_path(project_root);
        if Path::new(&path).exists() {
            Self::read(&path)
        } else {
            Self::scan_and_write(project_root)
        }
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
                Some(Section::Defines) => {
                    if !line.is_empty() {
                        defines = line.split(';').map(str::to_string).collect();
                    }
                }
                Some(Section::DefinesScripting) => {
                    if !line.is_empty() {
                        defines_scripting = line.split(';').map(str::to_string).collect();
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
    let colon = line.find(':')?;
    let key = &line[..colon];
    let value = trim_ws(&line[colon + 1..]);
    Some((key, value))
}

fn parse_dll_ref(line: &str) -> Option<DllRef> {
    let pipe = line.find('|')?;
    Some(DllRef {
        name: line[..pipe].to_string(),
        path: line[pipe + 1..].to_string(),
    })
}
