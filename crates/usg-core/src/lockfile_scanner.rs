//! Lockfile scanner: materialises every DLL reference, analyzer, and define
//! needed for a `.csproj`, then assembles the `Lockfile`. Two reference sources:
//! - **Unity install** — delegated to [`crate::unity_install`] (direct `std::fs`
//!   walk; never Watchman, since the install is write-once per editor version).
//! - **Project tree** — [`crate::scan`] (Watchman). The same query that powers
//!   `project_scanner` enumerates `.dll`/`.asmdef` paths under `Assets/`,
//!   `Packages/`, `Library/PackageCache/`; a gated fallback covers packages
//!   that PackageCache hasn't resolved (BuiltInPackages + `Editor/*.tgz`).

use std::collections::BTreeSet;
use std::path::Path;

use walkdir::WalkDir;

use crate::defines::{DEFAULT_FEATURE_DEFINES, generate_version_defines, parse_scripting_defines};
use crate::error::{LockfileError, Result};
use crate::io::{list_directory, read_file};
use crate::lockfile::{DllRef, Lockfile, RefCategory};
use crate::paths::{
    is_dotfile_or_backup, join_path, path_filename, resolve_real_path, unity_data_subpath,
    unity_install_root,
};
use crate::project_scanner::parse_version_defines;
use crate::scan::{enumerate, to_generator_error};

pub fn scan(project_root: &str) -> Result<Lockfile> {
    let _span = tracing::info_span!("lockfile_scanner.scan").entered();
    let (version, unity_path) = resolve_unity_path(project_root)?;

    // `data_sub` is reused below for the BuiltInPackages fallback paths.
    let data_sub = unity_data_subpath();
    let install = crate::unity_install::scan_unity_install(&unity_path, data_sub);

    // Project-side enumeration via Watchman. The same query that powers
    // project_scanner returns all `.dll` and `.asmdef` paths under
    // `Assets/`, `Packages/`, and `Library/PackageCache/`. We treat the
    // first call here as the seed (it returns `Fresh` with the full
    // enumeration); incremental delta handling is the project_scanner's
    // job. Both share the same Watchman watch.
    //
    // Dedupe by assembly name preserves the historical "first wins" rule
    // across roots. The Watchman response order isn't guaranteed
    // root-by-root, so we partition then iterate in the canonical order.
    let _proj_span = tracing::info_span!("lockfile_scanner.project_walk").entered();

    let all_paths = enumerate_project_paths(project_root)?;
    let mut by_root: [Vec<&str>; 3] = [Vec::new(), Vec::new(), Vec::new()];
    for p in &all_paths {
        let n = path_filename(p);
        if !(n.ends_with(".dll") || n.ends_with(".asmdef")) {
            continue;
        }
        if has_skipped_component(p) {
            continue;
        }
        if p.starts_with("Assets/") {
            by_root[0].push(p.as_str());
        } else if p.starts_with("Packages/") {
            by_root[1].push(p.as_str());
        } else if p.starts_with("Library/PackageCache/") {
            by_root[2].push(p.as_str());
        }
    }

    let mut project_refs: Vec<DllRef> = Vec::new();
    // Seeded with the install's source-generator analyzers; the project walk
    // + missing-package fallback append project-side analyzers (deduped).
    let mut analyzers: Vec<String> = install.analyzers;
    let mut seen_project_dlls: BTreeSet<String> = BTreeSet::new();
    let mut seen_analyzers: BTreeSet<String> = BTreeSet::new();
    let mut asmdef_paths: Vec<String> = Vec::new();
    for hits in &mut by_root {
        hits.sort();
        for rel in hits.iter() {
            let rel = (*rel).to_string();
            let file_name = path_filename(&rel);
            if let Some(name) = file_name.strip_suffix(".dll") {
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

    // Targeted fallback for packages that `Library/PackageCache/` doesn't
    // cover — typically a fresh worktree where Unity hasn't run yet, so
    // registry/builtin packages haven't been resolved into the per-project
    // cache. We read `packages-lock.json` to find what *should* be there;
    // for each gap we look up the package in `BuiltInPackages/` (already
    // extracted, lives in the Unity install) or extract from
    // `PackageManager/Editor/<name>-<version>.tgz` into a per-user cache.
    // Walking those sources in bulk would drown the cold-lock path
    // (~28 s on meow-tower); the missing-package set is usually empty in
    // practice, so the gated approach reverts to the original ~30 ms.
    let missing_packages = compute_missing_packages(project_root);
    if !missing_packages.is_empty() {
        tracing::info!(
            "lockfile_scanner: {} package(s) missing from PackageCache; falling back to BuiltInPackages + tgz extract",
            missing_packages.len()
        );
    }
    // Each fallback source roots its walk at the per-package directory and
    // emits ref paths using a single placeholder + prefix. Walks the
    // fallback dirs directly (these are outside any Watchman root and are
    // small per-package trees — no Watchman cost benefit).
    let mut ingest = |pkg_dir: &str, ref_prefix: &str| {
        for (rel, file_name) in fs_walk_dlls_and_asmdefs(pkg_dir, pkg_dir) {
            if file_name.ends_with(".dll") {
                let name = &file_name[..file_name.len() - 4];
                let path = format!("{}/{}", ref_prefix, rel);
                if is_analyzer_dll(name) {
                    if seen_analyzers.insert(name.to_string()) {
                        analyzers.push(path);
                    }
                } else if seen_project_dlls.insert(name.to_string()) {
                    project_refs.push(DllRef::new(name, path));
                }
            } else {
                asmdef_paths.push(format!("{}/{}", pkg_dir, rel));
            }
        }
    };
    // BuiltInPackages lives under the bundle-content subpath on every host
    // (macOS Contents/Resources, Windows/Linux Editor/Data/Resources).
    let builtin_base = format!(
        "{}/{}/Resources/PackageManager/BuiltInPackages",
        unity_path, data_sub
    );
    let builtin_prefix_base =
        format!("$(UnityPath)/{}/Resources/PackageManager/BuiltInPackages", data_sub);
    for entry in &missing_packages {
        let builtin = format!("{}/{}", builtin_base, entry.name);
        if Path::new(&builtin).exists() {
            let prefix = format!("{}/{}", builtin_prefix_base, entry.name);
            ingest(&builtin, &prefix);
            continue;
        }
        let Some(extract_root) = crate::package_cache::ensure_extracted_for_package(
            &unity_path,
            &version,
            &entry.name,
        ) else {
            tracing::warn!(
                "lockfile_scanner: package '{}' missing from PackageCache, BuiltInPackages, and Editor/*.tgz",
                entry.name
            );
            continue;
        };
        let prefix = format!("$(UsgCache)/{}", entry.name);
        ingest(&extract_root, &prefix);
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
    refs.insert(RefCategory::Engine, install.engine);
    refs.insert(RefCategory::Editor, install.editor);
    refs.insert(RefCategory::Netstandard, install.netstandard);
    refs.insert(RefCategory::PlaybackIos, install.playback_ios);
    refs.insert(RefCategory::PlaybackAndroid, install.playback_android);
    refs.insert(RefCategory::PlaybackStandalone, install.playback_standalone);
    refs.insert(RefCategory::PlaybackWindows, install.playback_windows);
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
Ok(lockfile)
}

fn resolve_unity_path(project_root: &str) -> Result<(String, String)> {
    let Some(version) = crate::paths::read_unity_version(project_root) else {
        return Err(LockfileError::NoProjectVersion(project_root.to_string()).into());
    };
    let unity_path = unity_install_root(&version);
    if !Path::new(&unity_path).exists() {
        return Err(LockfileError::UnityNotFound(unity_path).into());
    }
    Ok((version, resolve_real_path(&unity_path)))
}

/// Enumerate project-tree paths via Watchman. Returns the full file list under
/// `Assets/`, `Packages/`, `Library/PackageCache/`, `ProjectSettings/` filtered
/// by the scan query's suffix list, plus the Watchman clock cursor at the
/// time of the query (callers persist it for incremental invalidation).
///
/// This shares the same Watchman watch as `project_scanner` — the daemon
/// dedupes watches per project, so two callers in one CLI invocation cost no
/// more than one cold-crawl on the first call.
fn enumerate_project_paths(project_root: &str) -> Result<Vec<String>> {
    let _s = tracing::info_span!("lockfile_scanner.watchman_query").entered();
    enumerate(Path::new(project_root)).map_err(to_generator_error)
}

/// Recursively walk `directory` and return every `.dll` / `.asmdef` as
/// `(relative_to_strip_base, file_name)`. Skips dotfiles, tilde-suffixed
/// entries, and native-plugin dirs. Used only for the BuiltInPackages /
/// tgz-extract fallback paths — those are tiny (single-package subtrees)
/// and outside any Watchman root.
fn fs_walk_dlls_and_asmdefs(directory: &str, strip_base: &str) -> Vec<(String, String)> {
    if !Path::new(directory).exists() {
        return Vec::new();
    }
    let strip_path = Path::new(strip_base);
    let mut hits: Vec<(String, String)> = Vec::new();
    for entry in WalkDir::new(directory)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| {
            let name = e.file_name().to_string_lossy();
            if is_dotfile_or_backup(&name) {
                return false;
            }
            if e.file_type().is_dir() && is_native_plugin_dir(&name) {
                return false;
            }
            true
        })
        .filter_map(|e| e.ok())
    {
        if !entry.file_type().is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if !(name.ends_with(".dll") || name.ends_with(".asmdef")) {
            continue;
        }
        let Ok(rel) = entry.path().strip_prefix(strip_path) else {
            continue;
        };
        let Some(rel_str) = rel.to_str() else { continue };
        hits.push((rel_str.to_string(), name));
    }
    hits.sort();
    hits
}

/// Skip path predicate: any component starts with `.` or ends with `~`, or any
/// component is a native-plugin platform dir (e.g. `x86_64`, `arm64-v8a`).
fn has_skipped_component(p: &str) -> bool {
    p.split('/').any(|c| {
        is_dotfile_or_backup(c) || is_native_plugin_dir(c)
    })
}

/// Package entry that `Library/PackageCache/` is expected to cover but doesn't.
/// Driven by `Packages/packages-lock.json`: an entry is "expected" when its
/// `source` is something other than `embedded`/`local` (those live under
/// `Packages/` directly and are picked up by the project walk already).
#[derive(Debug)]
struct MissingPackage {
    name: String,
}

fn compute_missing_packages(project_root: &str) -> Vec<MissingPackage> {
    // Snapshot of currently-resolved packages by canonical name. Unity uses
    // `<name>@<hash>` for PackageCache directories; we strip the suffix.
    let pc_dir = join_path(project_root, "Library/PackageCache");
    let mut resolved: BTreeSet<String> = BTreeSet::new();
    for entry in list_directory(&pc_dir) {
        let name = match entry.find('@') {
            Some(i) => entry[..i].to_string(),
            None => entry,
        };
        resolved.insert(name);
    }

    let lock_path = join_path(project_root, "Packages/packages-lock.json");
    let Ok(content) = read_file(&lock_path) else {
        return Vec::new();
    };
    let v: serde_json::Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(
                "lockfile_scanner: malformed packages-lock.json ({}); skipping missing-package fallback",
                e
            );
            return Vec::new();
        }
    };
    let Some(deps) = v.get("dependencies").and_then(|x| x.as_object()) else {
        return Vec::new();
    };

    let mut missing = Vec::new();
    for (name, meta) in deps {
        let source = meta
            .get("source")
            .and_then(|s| s.as_str())
            .unwrap_or("");
        // `embedded` lives in `Packages/<name>/`; `local` is a `file:` path —
        // both are scanned by the project walk. Everything else (registry,
        // builtin, git) lands in `Library/PackageCache/` after Unity resolves.
        if matches!(source, "embedded" | "local") {
            continue;
        }
        if resolved.contains(name) {
            continue;
        }
        missing.push(MissingPackage {
            name: name.clone(),
        });
    }
    missing
}

fn is_native_plugin_dir(name: &str) -> bool {
    matches!(
        name,
        "x86" | "x86_64" | "arm64-v8a" | "armeabi-v7a" | "ARM64" | "x64"
    ) || name.ends_with(".framework")
        || name.ends_with(".bundle")
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

