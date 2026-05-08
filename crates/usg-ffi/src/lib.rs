//! C ABI for Unity `[DllImport]`. Mirrors the Swift `@_cdecl` exports byte-for-byte
//! so existing C# call sites and `dist/UnitySolutionGenerator.h` keep working.
//!
//! Error reporting model: a single process-global last-error string, valid until
//! the next `usg_*` call. This matches the Swift behaviour and the C# wrapper's
//! expectation. Not thread-safe; Unity calls from the main thread.

#![allow(non_snake_case)]

use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int};
use std::sync::Mutex;

use usg_core::{
    BuildConfig, BuildPlatform, DEFAULT_GENERATOR_ROOT, DllRef, GenerateOptions, LockfileIO,
    SolutionGenerator, resolve_project_root,
};

static LAST_ERROR: Mutex<Option<CString>> = Mutex::new(None);

fn set_last_error(msg: impl Into<String>) {
    let mut guard = LAST_ERROR.lock().unwrap();
    *guard = CString::new(msg.into()).ok();
}

fn clear_last_error() {
    let mut guard = LAST_ERROR.lock().unwrap();
    *guard = None;
}

/// Borrow a NUL-terminated C string as `&str`. The signature ties `'a` to the
/// reference of the pointer *slot* — that prevents one specific footgun (the
/// returned `&str` outliving the local that holds the pointer) but does NOT
/// prove the slice outlives the C buffer the pointer addresses; that is the
/// caller's responsibility under the safety contract below.
///
/// # Safety
/// `*p` must be null or a valid NUL-terminated UTF-8 string that remains live
/// for the duration of `'a`. C# `[DllImport]` marshalling satisfies this for
/// the call duration; if you stash the returned `&str` past the call return,
/// you're on your own.
unsafe fn cstr_to_str<'a>(p: &'a *const c_char) -> Option<&'a str> {
    if p.is_null() {
        return None;
    }
    unsafe { CStr::from_ptr(*p) }.to_str().ok()
}

/// Generate `.csproj`/`.sln` files from an existing lockfile.
/// Auto-runs the equivalent of `unity-solution-generator lock` if no lockfile exists.
///
/// `slnPathOut` / `slnPathOutLen` are optional — pass `NULL, 0` to skip. When
/// non-null, the buffer receives the relative path to the generated `.sln`
/// (NUL-terminated). Negative `slnPathOutLen` is rejected (would otherwise
/// sign-extend in the bounds check).
///
/// # Safety
/// All `*const c_char` arguments must be valid NUL-terminated UTF-8 (or null where allowed).
/// `slnPathOut`, when non-null, must point to a writable buffer of at least `slnPathOutLen` bytes.
/// Single-threaded contract — caller must serialize calls (cache files aren't reentrant-safe).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn usg_generate(
    project_root: *const c_char,
    platform: *const c_char,
    config: *const c_char,
    output_dir: *const c_char,
    extra_refs: *const c_char,
    sln_path_out: *mut c_char,
    sln_path_out_len: c_int,
) -> c_int {
    clear_last_error();

    let Some(platform_s) = (unsafe { cstr_to_str(&platform) }) else {
        set_last_error("Invalid UTF-8 in platform");
        return 1;
    };
    let Some(config_s) = (unsafe { cstr_to_str(&config) }) else {
        set_last_error("Invalid UTF-8 in config");
        return 1;
    };
    let Some(project_root_s) = (unsafe { cstr_to_str(&project_root) }) else {
        set_last_error("Invalid UTF-8 in projectRoot");
        return 1;
    };

    let Some(build_platform) = BuildPlatform::parse(platform_s) else {
        set_last_error(format!(
            "Unknown platform '{}'. Use 'ios', 'android', or 'osx'.",
            platform_s
        ));
        return 1;
    };
    let Some(build_config) = BuildConfig::parse(config_s) else {
        set_last_error(format!(
            "Unknown config '{}'. Use 'prod', 'dev', or 'editor'.",
            config_s
        ));
        return 1;
    };

    let resolved = resolve_project_root(project_root_s);

    let output_dir_s = unsafe { cstr_to_str(&output_dir) };
    let extra_refs_s = unsafe { cstr_to_str(&extra_refs) };
    let extra_refs_vec: Vec<DllRef> = extra_refs_s.map(DllRef::parse_list).unwrap_or_default();

    let options = GenerateOptions::new(resolved.clone(), build_platform)
        .with_build_config(build_config)
        .with_output_dir(output_dir_s)
        .with_extra_refs(extra_refs_vec);

    let lockfile = match LockfileIO::load_or_scan(&resolved, DEFAULT_GENERATOR_ROOT) {
        Ok(l) => l,
        Err(e) => {
            set_last_error(format!("{}", e));
            return 1;
        }
    };
    let result = match SolutionGenerator::new().generate_from_lockfile(&options, &lockfile) {
        Ok(r) => r,
        Err(e) => {
            set_last_error(format!("{}", e));
            return 1;
        }
    };

    if !sln_path_out.is_null() {
        // C# `int` is signed; a negative value here would sign-extend to a huge
        // `usize` and bypass the bounds check, allowing an OOB write into the
        // caller's buffer. Reject up front.
        if sln_path_out_len <= 0 {
            set_last_error(format!(
                "Invalid slnPathOutLen ({}); must be > 0",
                sln_path_out_len
            ));
            return 1;
        }
        let path = result.variant_sln_path.as_bytes();
        let cap = sln_path_out_len as usize;
        if path.len() + 1 > cap {
            set_last_error(format!(
                "Buffer too small ({} bytes) for path ({} needed)",
                cap,
                path.len() + 1
            ));
            return 1;
        }
        unsafe {
            std::ptr::copy_nonoverlapping(path.as_ptr(), sln_path_out as *mut u8, path.len());
            *sln_path_out.add(path.len()) = 0;
        }
    }
    0
}

/// Returns the last error message, or NULL if no error.
/// The returned pointer is valid until the next `usg_*` call.
#[unsafe(no_mangle)]
pub extern "C" fn usg_last_error() -> *const c_char {
    let guard = LAST_ERROR.lock().unwrap();
    match guard.as_ref() {
        Some(c) => c.as_ptr(),
        None => std::ptr::null(),
    }
}
