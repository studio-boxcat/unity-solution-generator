//! Lockfile scanner: walks the Unity installation + project to materialise
//! every DLL reference, analyzer, and define needed for a `.csproj`.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Mutex;

use ignore::{WalkBuilder, WalkState};
use walkdir::WalkDir;

use crate::defines::{DEFAULT_FEATURE_DEFINES, generate_version_defines, parse_scripting_defines};
use crate::error::{LockfileError, Result};
use crate::io::{file_exists, list_directory, read_file};
use crate::lockfile::{DllRef, Lockfile, RefCategory};
use crate::paths::{join_path, resolve_real_path};
use crate::project_scanner::parse_version_defines;

pub struct LockfileScanner;

/// Output of [`LockfileScanner::scan_with_artifacts`]: the lockfile plus the
/// concrete `.dll`/`.asmdef` paths that contributed to it. The caller (lock-cache)
/// uses the path list to build a fingerprint of contributing directories.
pub struct ScannedLockfile {
    pub lockfile: Lockfile,
    /// Paths relative to `project_root`, of every project-side `.dll` and `.asmdef`
    /// that the scan ingested.
    pub contributing_paths_relative: Vec<String>,
}

impl LockfileScanner {
    pub fn scan(project_root: &str) -> Result<Lockfile> {
        Self::scan_with_artifacts(project_root).map(|s| s.lockfile)
    }

    pub fn scan_with_artifacts(project_root: &str) -> Result<ScannedLockfile> {
        let _span = tracing::info_span!("lockfile_scanner.scan").entered();
        let (version, unity_path) = resolve_unity_path(project_root)?;
        let app_contents = join_path(&unity_path, "Unity.app/Contents");
        let _unity_span = tracing::info_span!("lockfile_scanner.unity_install").entered();

        let managed_engine_dir = join_path(&app_contents, "Managed/UnityEngine");
        let mut engine_refs: Vec<DllRef> = Vec::new();
        let mut editor_refs: Vec<DllRef> = Vec::new();
        let mut managed_dlls: Vec<String> = list_directory(&managed_engine_dir)
            .into_iter()
            .filter(|n| n.ends_with(".dll"))
            .collect();
        managed_dlls.sort();
        for dll in &managed_dlls {
            let name = &dll[..dll.len() - 4];
            if !(name.starts_with("UnityEngine") || name.starts_with("UnityEditor")) {
                continue;
            }
            let path = format!(
                "$(UnityPath)/Unity.app/Contents/Managed/UnityEngine/{}",
                dll
            );
            if name.starts_with("UnityEditor") {
                editor_refs.push(DllRef::new(name, path));
            } else {
                engine_refs.push(DllRef::new(name, path));
            }
        }

        // Lives one level up from Managed/UnityEngine/.
        let graphs_dll = join_path(&app_contents, "Managed/UnityEditor.Graphs.dll");
        if file_exists(&graphs_dll) {
            editor_refs.push(DllRef::new(
                "UnityEditor.Graphs",
                "$(UnityPath)/Unity.app/Contents/Managed/UnityEditor.Graphs.dll",
            ));
        }

        let netstd_base = join_path(&app_contents, "NetStandard");
        let mut netstd_refs: Vec<DllRef> = Vec::new();
        walk_files(&netstd_base, &netstd_base, &[".dll"], false, |rel, name| {
            let n = &name[..name.len() - 4];
            netstd_refs.push(DllRef::new(
                n,
                format!("$(UnityPath)/Unity.app/Contents/NetStandard/{}", rel),
            ));
        });
        netstd_refs.sort_by(|a, b| a.name.cmp(&b.name));

        let playback_base = join_path(&unity_path, "PlaybackEngines");
        let ios_refs = scan_playback_dlls(
            &join_path(&playback_base, "iOSSupport"),
            "PlaybackEngines/iOSSupport",
        );
        let android_refs = scan_playback_dlls(
            &join_path(&playback_base, "AndroidPlayer"),
            "PlaybackEngines/AndroidPlayer",
        );
        let standalone_dir = join_path(&app_contents, "PlaybackEngines/MacStandaloneSupport");
        let standalone_refs = scan_playback_dlls(
            &standalone_dir,
            "Unity.app/Contents/PlaybackEngines/MacStandaloneSupport",
        );

        let source_gen_dir = join_path(&app_contents, "Tools/Unity.SourceGenerators");
        let mut analyzers: Vec<String> = Vec::new();
        let mut sg_dlls: Vec<String> = list_directory(&source_gen_dir)
            .into_iter()
            .filter(|n| n.ends_with(".dll"))
            .collect();
        sg_dlls.sort();
        for dll in sg_dlls {
            analyzers.push(format!(
                "$(UnityPath)/Unity.app/Contents/Tools/Unity.SourceGenerators/{}",
                dll
            ));
        }

        // Deduplicate by assembly name (first wins across Assets > Packages > PackageCache).
        // We walk each root in parallel (via `ignore`), collect the per-root hits into a
        // `Vec<(rel_path, file_name)>`, then iterate sequentially across roots in order
        // to preserve "first wins" semantics. The hot cost is reading `Library/PackageCache`,
        // which is heavily parallelisable.
        drop(_unity_span);
        let _proj_span = tracing::info_span!("lockfile_scanner.project_walk").entered();
        let mut project_refs: Vec<DllRef> = Vec::new();
        let mut seen_project_dlls: BTreeSet<String> = BTreeSet::new();
        let mut seen_analyzers: BTreeSet<String> = BTreeSet::new();
        let mut asmdef_paths: Vec<String> = Vec::new();
        let mut contributing: Vec<String> = Vec::new();
        for root in ["Assets", "Packages", "Library/PackageCache"] {
            let root_dir = join_path(project_root, root);
            let hits = parallel_walk_dlls_and_asmdefs(&root_dir, project_root);
            for (rel, file_name) in hits {
                contributing.push(rel.clone());
                if file_name.ends_with(".dll") {
                    let name = &file_name[..file_name.len() - 4];
                    let path = format!("$(ProjectRoot)/{}", rel);
                    if is_analyzer_dll(name) {
                        if seen_analyzers.insert(name.to_string()) {
                            analyzers.push(path);
                        }
                    } else if seen_project_dlls.insert(name.to_string()) {
                        project_refs.push(DllRef::new(name, path));
                    }
                } else {
                    asmdef_paths.push(join_path(project_root, &rel));
                }
            }
        }

        analyzers.sort();
        project_refs.sort_by(|a, b| a.name.cmp(&b.name));

        drop(_proj_span);
        let _defines_span = tracing::info_span!("lockfile_scanner.defines").entered();
        let version_defines = generate_version_defines(&version);
        let asmdef_defines = collect_asmdef_version_defines(project_root, &asmdef_paths);
        let mut all_defines = version_defines;
        all_defines.extend(DEFAULT_FEATURE_DEFINES.iter().map(|s| s.to_string()));
        all_defines.extend(asmdef_defines);
        let scripting_defines = parse_scripting_defines(project_root);

        let mut refs = std::collections::BTreeMap::new();
        refs.insert(RefCategory::Engine, engine_refs);
        refs.insert(RefCategory::Editor, editor_refs);
        refs.insert(RefCategory::Netstandard, netstd_refs);
        refs.insert(RefCategory::PlaybackIos, ios_refs);
        refs.insert(RefCategory::PlaybackAndroid, android_refs);
        refs.insert(RefCategory::PlaybackStandalone, standalone_refs);
        refs.insert(RefCategory::Project, project_refs);

        let lockfile = Lockfile {
            unity_version: version,
            unity_path,
            lang_version: "9.0".to_string(),
            analyzers,
            refs,
            defines: all_defines,
            defines_scripting: scripting_defines,
        };
        Ok(ScannedLockfile {
            lockfile,
            contributing_paths_relative: contributing,
        })
    }
}

fn resolve_unity_path(project_root: &str) -> Result<(String, String)> {
    let version_file = join_path(project_root, "ProjectSettings/ProjectVersion.txt");
    if !file_exists(&version_file) {
        return Err(LockfileError::NoProjectVersion(project_root.to_string()).into());
    }
    let content = read_file(&version_file)?;
    let Some(colon) = content.find(':') else {
        return Err(LockfileError::NoProjectVersion(project_root.to_string()).into());
    };
    let bytes = content.as_bytes();
    let mut i = colon + 1;
    while i < bytes.len() && bytes[i] == b' ' {
        i += 1;
    }
    let mut end = i;
    while end < bytes.len() && bytes[end] != b'\n' && bytes[end] != b'\r' {
        end += 1;
    }
    let version = content[i..end].to_string();
    if version.is_empty() {
        return Err(LockfileError::NoProjectVersion(project_root.to_string()).into());
    }

    let unity_path = format!("/Applications/Unity/Hub/Editor/{}", version);
    if !Path::new(&unity_path).exists() {
        return Err(LockfileError::UnityNotFound(unity_path).into());
    }
    Ok((version, resolve_real_path(&unity_path)))
}

/// Recursively walk `directory` (using walkdir), invoking `handler(relative_to_base, file_name)`
/// for each file with a matching extension. Skips dotfiles, tilde-suffixed entries, and
/// (optionally) native-plugin subdirs.
fn walk_files(
    directory: &str,
    base_path: &str,
    extensions: &[&str],
    skip_native_plugin_dirs: bool,
    mut handler: impl FnMut(&str, &str),
) {
    if !Path::new(directory).exists() {
        return;
    }
    let base = Path::new(base_path);
    let mut iter = WalkDir::new(directory)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| {
            let name = e.file_name().to_string_lossy();
            if name.starts_with('.') || name.ends_with('~') {
                return false;
            }
            if e.file_type().is_dir() && skip_native_plugin_dirs && is_native_plugin_dir(&name) {
                return false;
            }
            true
        });
    while let Some(entry) = iter.next() {
        let Ok(entry) = entry else {
            continue;
        };
        if !entry.file_type().is_file() {
            continue;
        }
        let name_owned = entry.file_name().to_string_lossy().into_owned();
        if !extensions.iter().any(|ext| name_owned.ends_with(ext)) {
            continue;
        }
        let Ok(rel_path) = entry.path().strip_prefix(base) else {
            continue;
        };
        let Some(rel) = rel_path.to_str() else {
            continue;
        };
        handler(rel, &name_owned);
    }
}

/// Walk `directory` in parallel using `ignore::WalkBuilder::build_parallel` and return
/// every file ending in `.dll` or `.asmdef`, as `(relative_to_project_root, file_name)`.
/// Skips dotfiles, tilde-suffixed entries, and native-plugin directories.
fn parallel_walk_dlls_and_asmdefs(directory: &str, project_root: &str) -> Vec<(String, String)> {
    if !Path::new(directory).exists() {
        return Vec::new();
    }
    // Component-aware prefix strip — see project_scanner.rs for rationale.
    let project_root_path = Path::new(project_root);
    let aggregate: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());

    struct Flusher<'a> {
        local: Vec<(String, String)>,
        aggregate: &'a Mutex<Vec<(String, String)>>,
    }
    impl Drop for Flusher<'_> {
        fn drop(&mut self) {
            if self.local.is_empty() {
                return;
            }
            let mut g = self.aggregate.lock().unwrap();
            g.append(&mut self.local);
        }
    }

    let agg_ref: &Mutex<Vec<(String, String)>> = &aggregate;
    WalkBuilder::new(directory)
        .standard_filters(false)
        .hidden(false)
        .ignore(false)
        .git_ignore(false)
        .git_global(false)
        .git_exclude(false)
        .parents(false)
        .follow_links(false)
        .build_parallel()
        .run(|| {
            let mut flusher = Flusher {
                local: Vec::new(),
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
                if ft.is_dir() {
                    if is_native_plugin_dir(&name) {
                        return WalkState::Skip;
                    }
                    return WalkState::Continue;
                }
                if !ft.is_file() {
                    return WalkState::Continue;
                }
                let n: &str = name.as_ref();
                if !(n.ends_with(".dll") || n.ends_with(".asmdef")) {
                    return WalkState::Continue;
                }
                let Ok(rel) = entry.path().strip_prefix(project_root_path) else {
                    return WalkState::Continue;
                };
                let Some(rel_str) = rel.to_str() else {
                    return WalkState::Continue;
                };
                flusher.local.push((rel_str.to_string(), n.to_string()));
                WalkState::Continue
            })
        });

    let mut hits = aggregate.into_inner().unwrap();
    // Stable order across the roots so the "first wins" dedupe pass is deterministic
    // even though the parallel walker fans out non-deterministically per thread.
    hits.sort();
    hits
}

fn is_native_plugin_dir(name: &str) -> bool {
    matches!(
        name,
        "x86" | "x86_64" | "arm64-v8a" | "armeabi-v7a" | "ARM64" | "x64"
    ) || name.ends_with(".framework")
        || name.ends_with(".bundle")
}

fn scan_playback_dlls(directory: &str, prefix: &str) -> Vec<DllRef> {
    let mut dlls: Vec<String> = list_directory(directory)
        .into_iter()
        .filter(|n| n.ends_with(".dll"))
        .collect();
    dlls.sort();
    dlls.into_iter()
        .filter_map(|dll| {
            let name = dll[..dll.len() - 4].to_string();
            if name.starts_with("UnityEditor.") || name.starts_with("Unity.Android.") {
                Some(DllRef::new(name, format!("$(UnityPath)/{}/{}", prefix, dll)))
            } else {
                None
            }
        })
        .collect()
}

fn is_analyzer_dll(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.contains("analyzer") || lower.contains("sourcegenerator")
}

fn collect_asmdef_version_defines(project_root: &str, asmdef_paths: &[String]) -> Vec<String> {
    let mut installed_packages: BTreeSet<String> = BTreeSet::new();
    installed_packages.insert("Unity".to_string());

    let manifest_path = join_path(project_root, "Packages/manifest.json");
    if let Ok(manifest) = read_file(&manifest_path) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&manifest) {
            if let Some(deps) = v.get("dependencies").and_then(|x| x.as_object()) {
                for pkg in deps.keys() {
                    installed_packages.insert(pkg.clone());
                }
            }
        }
    }
    for entry in list_directory(&join_path(project_root, "Packages")) {
        if entry.ends_with(".json") || entry.starts_with('.') {
            continue;
        }
        installed_packages.insert(entry);
    }

    let mut all: BTreeSet<String> = BTreeSet::new();
    for path in asmdef_paths {
        let Ok(content) = read_file(path) else {
            continue;
        };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&content) else {
            continue;
        };
        for vd in parse_version_defines(&v) {
            if installed_packages.contains(&vd.package_name) {
                all.insert(vd.define);
            }
        }
    }
    all.into_iter().collect()
}

