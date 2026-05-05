//! ABI smoke tests for the cdylib's `extern "C"` surface.
//!
//! Calls `usg_generate` / `usg_lock` / `usg_last_error` directly as Rust
//! functions (the cdylib also ships as `rlib`). Covers the same edge cases
//! the C# wrapper in meow-tower exercises:
//!
//! - null `outputDir` / `extraRefs` (Rider passes "."/refs string but the
//!   contract allows null)
//! - null `slnPathOut` with `slnPathOutLen=0` (Rider's actual call)
//! - negative `slnPathOutLen` (rejected — would otherwise sign-extend to a
//!   huge usize and bypass the bounds check)
//! - error path: `usg_last_error` round-trips a formatted message
//! - success path: `usg_last_error` returns null

use std::ffi::CString;
use std::os::raw::c_char;
use std::sync::{Mutex, MutexGuard};

use UnitySolutionGenerator::{usg_generate, usg_last_error, usg_lock};

/// All tests in this file touch the process-global `LAST_ERROR` static, so
/// running them in parallel would race. Take this mutex at the top of every
/// test to serialise. Cargo runs tests across files in parallel, so this only
/// covers within-file ordering — fine for our needs.
static FFI_LOCK: Mutex<()> = Mutex::new(());

fn ffi_serialize() -> MutexGuard<'static, ()> {
    // Don't propagate poison — a panicked test still leaves the static in a
    // recoverable state; subsequent tests should run.
    FFI_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

fn cstring(s: &str) -> CString {
    CString::new(s).unwrap()
}

fn last_error_str() -> Option<String> {
    let p = usg_last_error();
    if p.is_null() {
        return None;
    }
    Some(unsafe { std::ffi::CStr::from_ptr(p) }.to_string_lossy().into_owned())
}

/// Build a minimal valid Unity-shaped fixture: ProjectVersion.txt, an asmdef,
/// a .cs file, and a hand-written csproj.lock so we don't need a real Unity
/// install for `usg_generate` to succeed.
fn fixture() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::create_dir_all(root.join("ProjectSettings")).unwrap();
    std::fs::write(
        root.join("ProjectSettings/ProjectVersion.txt"),
        "m_EditorVersion: 6000.2.7f2\n",
    )
    .unwrap();
    std::fs::create_dir_all(root.join("Assets/A")).unwrap();
    std::fs::write(root.join("Assets/A/Lib.asmdef"), r#"{"name":"Lib"}"#).unwrap();
    std::fs::write(root.join("Assets/A/Code.cs"), "class Code {}\n").unwrap();
    let lf_dir = root.join("Library/UnitySolutionGenerator");
    std::fs::create_dir_all(&lf_dir).unwrap();
    let lf = usg_core::Lockfile::empty("6000.2.7f2", "/test/unity");
    usg_core::LockfileIO::write(&lf, lf_dir.join("csproj.lock").to_str().unwrap()).unwrap();
    tmp
}

#[test]
fn generate_with_null_optionals_succeeds() { let _g = ffi_serialize();
    let tmp = fixture();
    let root = cstring(tmp.path().to_str().unwrap());
    let platform = cstring("ios");
    let config = cstring("editor");
    let rc = unsafe {
        usg_generate(
            root.as_ptr(),
            platform.as_ptr(),
            config.as_ptr(),
            std::ptr::null(),  // outputDir
            std::ptr::null(),  // extraRefs
            std::ptr::null_mut(), // slnPathOut
            0,                 // slnPathOutLen
        )
    };
    assert_eq!(rc, 0, "rc=1, error={:?}", last_error_str());
    assert!(last_error_str().is_none(), "last_error must be null after success");
}

#[test]
fn generate_writes_sln_path_to_buffer() { let _g = ffi_serialize();
    let tmp = fixture();
    let root = cstring(tmp.path().to_str().unwrap());
    let platform = cstring("ios");
    let config = cstring("editor");
    let output_dir = cstring(".");
    let mut buf = [0u8; 256];
    let rc = unsafe {
        usg_generate(
            root.as_ptr(),
            platform.as_ptr(),
            config.as_ptr(),
            output_dir.as_ptr(),
            std::ptr::null(),
            buf.as_mut_ptr() as *mut c_char,
            buf.len() as i32,
        )
    };
    assert_eq!(rc, 0, "rc=1, error={:?}", last_error_str());
    let nul = buf.iter().position(|&b| b == 0).unwrap();
    let path = std::str::from_utf8(&buf[..nul]).unwrap();
    assert!(path.ends_with(".sln"), "expected .sln path, got {:?}", path);
}

#[test]
fn generate_rejects_negative_buffer_len() { let _g = ffi_serialize();
    // Round-1 review: a negative C# `int` sign-extends to a huge `usize` and
    // bypasses the bounds check. Verify the fix.
    let tmp = fixture();
    let root = cstring(tmp.path().to_str().unwrap());
    let platform = cstring("ios");
    let config = cstring("editor");
    let mut buf = [0u8; 16];
    let rc = unsafe {
        usg_generate(
            root.as_ptr(),
            platform.as_ptr(),
            config.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            buf.as_mut_ptr() as *mut c_char,
            -1, // negative — must be rejected before `as usize`
        )
    };
    assert_eq!(rc, 1);
    let err = last_error_str().expect("last_error after failure");
    assert!(
        err.contains("Invalid slnPathOutLen"),
        "unexpected error: {}",
        err
    );
}

#[test]
fn generate_rejects_buffer_too_small() { let _g = ffi_serialize();
    let tmp = fixture();
    let root = cstring(tmp.path().to_str().unwrap());
    let platform = cstring("ios");
    let config = cstring("editor");
    let mut buf = [0u8; 4]; // way smaller than any real .sln path
    let rc = unsafe {
        usg_generate(
            root.as_ptr(),
            platform.as_ptr(),
            config.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            buf.as_mut_ptr() as *mut c_char,
            buf.len() as i32,
        )
    };
    assert_eq!(rc, 1);
    let err = last_error_str().expect("last_error after failure");
    assert!(
        err.contains("Buffer too small"),
        "unexpected error: {}",
        err
    );
}

#[test]
fn unknown_platform_returns_error_with_message() { let _g = ffi_serialize();
    let tmp = fixture();
    let root = cstring(tmp.path().to_str().unwrap());
    let platform = cstring("wii");
    let config = cstring("editor");
    let rc = unsafe {
        usg_generate(
            root.as_ptr(),
            platform.as_ptr(),
            config.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null_mut(),
            0,
        )
    };
    assert_eq!(rc, 1);
    let err = last_error_str().unwrap();
    assert!(err.contains("Unknown platform 'wii'"), "got: {}", err);
}

#[test]
fn last_error_clears_on_next_success() { let _g = ffi_serialize();
    let tmp = fixture();
    let root = cstring(tmp.path().to_str().unwrap());
    let bad_platform = cstring("wii");
    let platform = cstring("ios");
    let config = cstring("editor");

    // Trigger an error.
    let _ = unsafe {
        usg_generate(
            root.as_ptr(),
            bad_platform.as_ptr(),
            config.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null_mut(),
            0,
        )
    };
    assert!(last_error_str().is_some());

    // Next successful call must clear it.
    let rc = unsafe {
        usg_generate(
            root.as_ptr(),
            platform.as_ptr(),
            config.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null_mut(),
            0,
        )
    };
    assert_eq!(rc, 0, "rc=1, error={:?}", last_error_str());
    assert!(last_error_str().is_none(), "last_error must be cleared after success");
}

#[test]
fn lock_with_no_unity_install_returns_clean_error() { let _g = ffi_serialize();
    // No real Unity at /Applications — exercises the error path through the FFI.
    let tmp = fixture();
    // Wipe the lockfile so usg_lock has to actually scan.
    std::fs::remove_file(tmp.path().join("Library/UnitySolutionGenerator/csproj.lock"))
        .ok();
    let root = cstring(tmp.path().to_str().unwrap());
    let rc = unsafe { usg_lock(root.as_ptr()) };
    // With a stub ProjectVersion.txt pointing at a real Unity version that's
    // installed locally (6000.2.7f2 in CI? unknown). On a dev machine with
    // Unity installed, this succeeds. On CI without Unity, it returns 1 with
    // a "Unity installation not found" message. Either is acceptable for the
    // ABI test — what we're checking is that the FFI layer cleanly transports
    // the result without crashing.
    assert!(rc == 0 || rc == 1, "unexpected rc {}", rc);
    if rc != 0 {
        let err = last_error_str().expect("error path must populate last_error");
        assert!(!err.is_empty());
    }
}
