//! Unity-install reference enumeration, factored out of [`crate::lockfile_scanner`].
//! Walks the installed editor's `Managed/`, `NetStandard/`, `PlaybackEngines/`,
//! and `Tools/` trees directly (`std::fs`, never Watchman — the install is a
//! multi-GB write-once tree that never changes within an editor version) to
//! build the engine/editor/netstandard/playback DLL refs + source-generator
//! analyzers. Host-specific bundle layout (macOS `.app` vs Windows/Linux
//! `Editor/Data`) is resolved here. See [[architecture.md]] (Lockfile auto-refresh).

use std::path::Path;

use walkdir::WalkDir;

use crate::io::{file_exists, list_directory};
use crate::lockfile::DllRef;
use crate::paths::{is_dotfile_or_backup, join_path};

/// Install-side DLL references, grouped by the `RefCategory` they populate.
/// `analyzers` are the source-generator DLLs under `Tools/` (the project walk
/// appends project-side analyzers to the same list).
pub(crate) struct InstallRefs {
    pub engine: Vec<DllRef>,
    pub editor: Vec<DllRef>,
    pub netstandard: Vec<DllRef>,
    pub playback_ios: Vec<DllRef>,
    pub playback_android: Vec<DllRef>,
    pub playback_standalone: Vec<DllRef>,
    pub playback_windows: Vec<DllRef>,
    pub analyzers: Vec<String>,
}

/// Enumerate every install-side ref for `unity_path`. `data_sub` is the
/// host bundle-content subpath ([`crate::paths::unity_data_subpath`]).
pub(crate) fn scan_unity_install(unity_path: &str, data_sub: &str) -> InstallRefs {
    let _span = tracing::info_span!("lockfile_scanner.unity_install").entered();

    // Bundle-content prefix. macOS bundles via `Unity.app/Contents`;
    // Windows/Linux flatten under `Editor/Data`. All `Managed/*`,
    // `NetStandard/*`, `Tools/*` paths sit under this subpath.
    let app_contents = join_path(unity_path, data_sub);

    let managed_engine_dir = join_path(&app_contents, "Managed/UnityEngine");
    let mut engine: Vec<DllRef> = Vec::new();
    let mut editor: Vec<DllRef> = Vec::new();
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
        let path = format!("$(UnityPath)/{}/Managed/UnityEngine/{}", data_sub, dll);
        if name.starts_with("UnityEditor") {
            editor.push(DllRef::new(name, path));
        } else {
            engine.push(DllRef::new(name, path));
        }
    }

    // Lives one level up from Managed/UnityEngine/.
    let graphs_dll = join_path(&app_contents, "Managed/UnityEditor.Graphs.dll");
    if file_exists(&graphs_dll) {
        editor.push(DllRef::new(
            "UnityEditor.Graphs",
            format!("$(UnityPath)/{}/Managed/UnityEditor.Graphs.dll", data_sub),
        ));
    }

    let netstd_base = join_path(&app_contents, "NetStandard");
    let mut netstandard: Vec<DllRef> = Vec::new();
    walk_files(&netstd_base, &netstd_base, &[".dll"], |rel, name| {
        let n = &name[..name.len() - 4];
        // Drop the WCF family. `System.Private.ServiceModel.dll` declares a
        // dep on `System.Reflection.DispatchProxy` v4.0.6.0, but Unity only
        // ships the v4.0.5.0 shim — MSBuild's RAR can't unify and emits a
        // multi-line MSB3277 per csproj on `dotnet build`. WCF isn't usable
        // from a Unity runtime anyway, so excluding the family is safe.
        if n == "System.Private.ServiceModel" || n.starts_with("System.ServiceModel") {
            return;
        }
        netstandard.push(DllRef::new(
            n,
            format!("$(UnityPath)/{}/NetStandard/{}", data_sub, rel),
        ));
    });
    netstandard.sort_by(|a, b| a.name.cmp(&b.name));

    // PlaybackEngines layout: macOS keeps iOSSupport/AndroidPlayer at the
    // editor-root level (outside the `.app` bundle for code-signing), but
    // tucks MacStandaloneSupport under `Contents/`. Windows/Linux put
    // everything under `Editor/Data/PlaybackEngines/`. The `playback_base`
    // here resolves to whichever applies on the host.
    let playback_base = if cfg!(target_os = "macos") {
        join_path(unity_path, "PlaybackEngines")
    } else {
        join_path(&app_contents, "PlaybackEngines")
    };
    let playback_ref_prefix = if cfg!(target_os = "macos") {
        "PlaybackEngines".to_string()
    } else {
        format!("{}/PlaybackEngines", data_sub)
    };
    let playback_ios = scan_playback_dlls(
        &join_path(&playback_base, "iOSSupport"),
        &format!("{}/iOSSupport", playback_ref_prefix),
    );
    let playback_android = scan_playback_dlls(
        &join_path(&playback_base, "AndroidPlayer"),
        &format!("{}/AndroidPlayer", playback_ref_prefix),
    );
    // Mac standalone is the macOS exception: lives under Contents on mac,
    // under Editor/Data/PlaybackEngines on non-mac. Use the unified base.
    let standalone_base = if cfg!(target_os = "macos") {
        join_path(&app_contents, "PlaybackEngines/MacStandaloneSupport")
    } else {
        join_path(&playback_base, "MacStandaloneSupport")
    };
    let standalone_prefix = if cfg!(target_os = "macos") {
        format!("{}/PlaybackEngines/MacStandaloneSupport", data_sub)
    } else {
        format!("{}/MacStandaloneSupport", playback_ref_prefix)
    };
    let playback_standalone = scan_playback_dlls(&standalone_base, &standalone_prefix);
    let playback_windows = scan_playback_dlls(
        &join_path(&playback_base, "WindowsStandaloneSupport"),
        &format!("{}/WindowsStandaloneSupport", playback_ref_prefix),
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
            "$(UnityPath)/{}/Tools/Unity.SourceGenerators/{}",
            data_sub, dll
        ));
    }

    InstallRefs {
        engine,
        editor,
        netstandard,
        playback_ios,
        playback_android,
        playback_standalone,
        playback_windows,
        analyzers,
    }
}

/// Recursively walk `directory`, invoking `handler(relative_to_base, file_name)`
/// for each file with a matching extension. Skips dotfiles and tilde backups.
fn walk_files(
    directory: &str,
    base_path: &str,
    extensions: &[&str],
    mut handler: impl FnMut(&str, &str),
) {
    if !Path::new(directory).exists() {
        return;
    }
    let base = Path::new(base_path);
    let iter = WalkDir::new(directory)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| {
            let name = e.file_name().to_string_lossy();
            !is_dotfile_or_backup(&name)
        });
    for entry in iter {
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
