use std::ffi::{CStr, CString};
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

/// Resolve symlinks via libc::realpath. Returns the input unchanged on failure.
pub fn resolve_real_path(path: &str) -> String {
    let Ok(c) = CString::new(path) else {
        return path.to_string();
    };
    unsafe {
        let resolved = libc::realpath(c.as_ptr(), std::ptr::null_mut());
        if resolved.is_null() {
            return path.to_string();
        }
        let s = CStr::from_ptr(resolved).to_string_lossy().into_owned();
        libc::free(resolved as *mut libc::c_void);
        s
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

/// Per-user cache root for tarball-extracted Unity packages. Subdirectory by
/// Unity version isolates installs (each ships its own bundled tarballs).
/// Honours `XDG_CACHE_HOME`; falls back to `$HOME/.cache`. Panics if neither
/// is set — landing extracted Unity code in a world-writable `/tmp` would let
/// anyone tamper with our compile inputs.
pub fn usg_cache_dir(unity_version: &str) -> String {
    let cache_home = std::env::var("XDG_CACHE_HOME")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| std::env::var("HOME").ok().map(|h| format!("{}/.cache", h)))
        .expect("usg_cache_dir: neither XDG_CACHE_HOME nor HOME is set");
    format!("{}/unity-solution-generator/{}", cache_home, unity_version)
}
