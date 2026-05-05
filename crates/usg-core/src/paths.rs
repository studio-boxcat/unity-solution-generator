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

pub fn lockfile_path(project_root: &str) -> String {
    join_path(project_root, &format!("{}/csproj.lock", DEFAULT_GENERATOR_ROOT))
}
