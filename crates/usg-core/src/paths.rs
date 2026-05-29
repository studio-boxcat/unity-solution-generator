use std::path::Path;

pub const DEFAULT_GENERATOR_ROOT: &str = "Library/UnitySolutionGenerator";

pub fn parent_directory(path: &str) -> &str {
    match path.rfind('/') {
        Some(i) => &path[..i],
        None => "",
    }
}

pub fn join_path(base: &str, component: &str) -> String {
    if base.ends_with('/') {
        format!("{}{}", base, component)
    } else {
        format!("{}/{}", base, component)
    }
}

/// Return the basename of a forward-slash path. Watchman emits project-relative
/// forward-slash paths and DLL-ref paths are stored slash-normalized, so a
/// simple rsplit suffices.
pub fn path_filename(p: &str) -> &str {
    p.rsplit('/').next().unwrap_or(p)
}

/// `$key` if set and non-empty, else `None` — folds the repeated
/// `env::var(k).ok().filter(non-empty)` pattern in host/cache discovery.
pub(crate) fn env_non_empty(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|s| !s.is_empty())
}

/// Unity convention: a path component that is a dotfile (`.foo`) or a `~`-suffixed
/// backup (`bar~`) is editor-only metadata that never ships into compiled output,
/// so the scanners skip it.
pub(crate) fn is_dotfile_or_backup(component: &str) -> bool {
    component.starts_with('.') || component.ends_with('~')
}

/// Canonicalize a filesystem path. Resolves symlinks, normalizes `..`, and on
/// Windows strips the `\\?\` UNC prefix that `std::fs::canonicalize` adds —
/// downstream string-path code chokes on it. Returns the input unchanged on
/// failure (missing path, permission, etc.).
///
/// On Windows the returned string keeps backslash separators (consumers that
/// embed it in `$(UnityPath)` lockfile entries pass it through to MSBuild,
/// which accepts both). Persisted-format normalization happens at the value
/// boundary, not here.
pub fn resolve_real_path(path: &str) -> String {
    match dunce::canonicalize(path) {
        Ok(p) => p.to_string_lossy().into_owned(),
        Err(_) => path.to_string(),
    }
}

/// Resolve symlinks then climb up to the nearest ancestor containing
/// `ProjectSettings/ProjectVersion.txt`. Falls back to the resolved input
/// if no Unity root is found.
pub fn resolve_project_root(path: &str) -> String {
    let resolved = resolve_real_path(path);
    let mut current = resolved.as_str();
    while !current.is_empty() && current != "/" {
        let marker = join_path(current, "ProjectSettings/ProjectVersion.txt");
        if Path::new(&marker).exists() {
            return current.to_string();
        }
        current = parent_directory(current);
    }
    resolved
}

pub fn lockfile_path(project_root: &str, generator_root: &str) -> String {
    join_path(project_root, &format!("{}/csproj.lock", generator_root))
}

/// Per-host root directory for application caches. Used as the parent of
/// `unity-solution-generator/<unity-version>/...` extracted-tarball state.
///
/// Lookup order (first match wins):
/// - **Windows:** `%LOCALAPPDATA%` → `%USERPROFILE%\AppData\Local`.
/// - **macOS:**   `$XDG_CACHE_HOME` → `$HOME/Library/Caches`.
/// - **Linux/other Unix:** `$XDG_CACHE_HOME` → `$HOME/.cache`.
///
/// Panics if no candidate is set — landing extracted Unity code in
/// world-writable `/tmp` (or its Windows equivalent) would let any local user
/// tamper with our compile inputs. The panic is consistent across platforms
/// rather than silently downgrading to an unsafe location.
fn host_cache_root() -> String {
    if cfg!(target_os = "windows") {
        env_non_empty("LOCALAPPDATA")
            .or_else(|| {
                std::env::var("USERPROFILE")
                    .ok()
                    .map(|h| format!("{}\\AppData\\Local", h))
            })
            .expect("host_cache_root: neither LOCALAPPDATA nor USERPROFILE is set")
    } else if cfg!(target_os = "macos") {
        env_non_empty("XDG_CACHE_HOME")
            .or_else(|| std::env::var("HOME").ok().map(|h| format!("{}/Library/Caches", h)))
            .expect("host_cache_root: neither XDG_CACHE_HOME nor HOME is set")
    } else {
        env_non_empty("XDG_CACHE_HOME")
            .or_else(|| std::env::var("HOME").ok().map(|h| format!("{}/.cache", h)))
            .expect("host_cache_root: neither XDG_CACHE_HOME nor HOME is set")
    }
}

/// Per-user, per-Unity-version cache directory.
pub fn usg_cache_dir(unity_version: &str) -> String {
    let root = host_cache_root();
    let sep = if cfg!(target_os = "windows") { "\\" } else { "/" };
    format!("{}{}unity-solution-generator{}{}", root, sep, sep, unity_version)
}

/// Unity Hub install path for `version`. Returns the **editor root**, NOT the
/// bundle content directory — append [`unity_data_subpath`] for the standard
/// `Managed/`, `NetStandard/`, etc. layout, but NOT for `PlaybackEngines/`
/// (its location relative to the editor root differs per host: under Contents
/// on macOS only for `MacStandaloneSupport`, at the editor-root level for
/// `iOSSupport`/`AndroidPlayer`, but always under `Editor/Data` on Windows).
///
/// Default Unity Hub layouts:
/// - macOS:   `/Applications/Unity/Hub/Editor/<v>`
/// - Windows: `%ProgramFiles%\Unity\Hub\Editor\<v>`
/// - Linux:   `~/Unity/Hub/Editor/<v>`
///
/// Override via `$UNITY_INSTALL_ROOT` for non-default installs. The override
/// is used **verbatim** — it does NOT auto-append `<version>`. If you have
/// multiple Unity versions installed, point the override at the editor root
/// for the version matching the project's `ProjectVersion.txt`, or unset it
/// to let the Hub-default path resolve per-version.
pub fn unity_install_root(version: &str) -> String {
    if let Ok(over) = std::env::var("UNITY_INSTALL_ROOT") {
        if !over.is_empty() {
            return over;
        }
    }
    if cfg!(target_os = "windows") {
        let program_files =
            env_non_empty("ProgramFiles").unwrap_or_else(|| "C:\\Program Files".to_string());
        format!("{}\\Unity\\Hub\\Editor\\{}", program_files, version)
    } else if cfg!(target_os = "macos") {
        format!("/Applications/Unity/Hub/Editor/{}", version)
    } else {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
        format!("{}/Unity/Hub/Editor/{}", home, version)
    }
}

/// Host-specific subpath under [`unity_install_root`] where the editor bundles
/// `Managed/`, `NetStandard/`, `Tools/`, `Resources/`. macOS uses an `.app`
/// bundle wrapper; Windows/Linux use a flat `Editor/Data` directory.
pub fn unity_data_subpath() -> &'static str {
    if cfg!(target_os = "macos") {
        "Unity.app/Contents"
    } else {
        "Editor/Data"
    }
}

/// Cross-platform nanosecond mtime since the UNIX epoch. `None` on any IO
/// error or unavailable mtime. Built on `std::fs::Metadata::modified()` —
/// resolution is fs-dependent (APFS/ext4: ns; NTFS: 100-ns ticks).
pub fn mtime_nanos_for(path: impl AsRef<Path>) -> Option<u128> {
    let m = std::fs::metadata(path).ok()?;
    let mt = m.modified().ok()?;
    let d = mt.duration_since(std::time::SystemTime::UNIX_EPOCH).ok()?;
    Some(d.as_nanos())
}

/// Read `ProjectSettings/ProjectVersion.txt` and extract the value after
/// `m_EditorVersion:`. Returns `None` for missing or malformed input — the
/// caller decides whether that's a rescan-needed signal or a hard error.
pub fn read_unity_version(project_root: &str) -> Option<String> {
    let p = join_path(project_root, "ProjectSettings/ProjectVersion.txt");
    let content = std::fs::read_to_string(&p).ok()?;
    let (_, value) = content.lines().next()?.split_once(':')?;
    let v = value.trim();
    if v.is_empty() { None } else { Some(v.to_string()) }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Merged into one test: `set_var` / `remove_var` race with any sibling test
    // reading the env, so both halves run serially in the same fn.
    #[test]
    fn unity_install_root_per_host_and_override() {
        unsafe {
            std::env::remove_var("UNITY_INSTALL_ROOT");
        }
        let p = unity_install_root("2022.3.42f1");
        if cfg!(target_os = "windows") {
            assert!(p.contains("Unity\\Hub\\Editor"), "got {p}");
            assert!(p.ends_with("2022.3.42f1"), "got {p}");
        } else if cfg!(target_os = "macos") {
            assert_eq!(p, "/Applications/Unity/Hub/Editor/2022.3.42f1");
        } else {
            assert!(p.contains("/Unity/Hub/Editor/"), "got {p}");
            assert!(p.ends_with("/2022.3.42f1"), "got {p}");
        }

        unsafe {
            std::env::set_var("UNITY_INSTALL_ROOT", "/my/custom/unity");
        }
        let p = unity_install_root("2022.3.42f1");
        unsafe {
            std::env::remove_var("UNITY_INSTALL_ROOT");
        }
        assert_eq!(p, "/my/custom/unity");
    }

    #[test]
    fn unity_data_subpath_per_host() {
        let s = unity_data_subpath();
        if cfg!(target_os = "macos") {
            assert_eq!(s, "Unity.app/Contents");
        } else {
            assert_eq!(s, "Editor/Data");
        }
    }

    #[test]
    fn usg_cache_dir_contains_version() {
        let p = usg_cache_dir("2022.3.42f1");
        assert!(p.contains("unity-solution-generator"));
        assert!(p.ends_with("2022.3.42f1"));
    }
}
