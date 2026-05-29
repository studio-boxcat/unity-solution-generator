//! PE/COFF + CLR runtime-header inspection.
//!
//! Shared by `typecheck` (filter native DLLs out of csc `/reference:`
//! lists — they fire `CS0009` otherwise) and downstream consumers like
//! `pspec-bake-types` (silent-skip native PEs before invoking dotnetdll's
//! parser). PE format reference:
//! <https://learn.microsoft.com/en-us/windows/win32/debug/pe-format>.
//! CLR header spec: ECMA-335 II.25.3.3.

use std::path::Path;

/// `true` if `bytes` is a managed PE — i.e. carries a non-empty CLR
/// Runtime Header data-directory entry. Native DLLs (Rust cdylib, Unity
/// Windows-side plugins like `BoxcatBridge.dll`, etc.) and non-PE files
/// return `false`.
pub fn check_clr_header(bytes: &[u8]) -> bool {
    // PE optional-header magics.
    const PE32_MAGIC: u16 = 0x10B;
    const PE32_PLUS_MAGIC: u16 = 0x20B;
    // CLR is data-directory entry 14 (15th); each entry is `{VA u32, Size u32}`.
    const CLR_DIR_INDEX: usize = 14;
    const DATA_DIR_ENTRY_SIZE: usize = 8;

    // DOS header → `e_lfanew` u32 LE at byte 0x3C → offset of PE signature.
    if bytes.get(..2) != Some(b"MZ") {
        return false;
    }
    let Some(e_lfanew) = read_u32_le(bytes, 0x3C) else {
        return false;
    };
    let e_lfanew = e_lfanew as usize;
    if bytes.get(e_lfanew..e_lfanew + 4) != Some(b"PE\0\0") {
        return false;
    }
    // PE signature (4) + COFF header (20) → optional header at +24.
    let opt_hdr = e_lfanew + 24;
    let Some(magic) = read_u16_le(bytes, opt_hdr) else {
        return false;
    };
    // PE32 vs PE32+ shifts `NumberOfRvaAndSizes` and the data-directory base.
    let (num_dirs_offset, data_dir_offset) = match magic {
        PE32_MAGIC => (opt_hdr + 92, opt_hdr + 96),
        PE32_PLUS_MAGIC => (opt_hdr + 108, opt_hdr + 112),
        _ => return false,
    };
    let Some(num_dirs) = read_u32_le(bytes, num_dirs_offset) else {
        return false;
    };
    // Array must declare more entries than the CLR index (14 ⇒ ≥15 declared).
    if (num_dirs as usize) <= CLR_DIR_INDEX {
        return false;
    }
    let clr = data_dir_offset + CLR_DIR_INDEX * DATA_DIR_ENTRY_SIZE;
    let (Some(va), Some(size)) = (read_u32_le(bytes, clr), read_u32_le(bytes, clr + 4)) else {
        return false;
    };
    va != 0 && size != 0
}

/// File-based wrapper around [`check_clr_header`] — opens `path`, reads
/// the first 1 KB (the PE-header walk never exceeds that), and runs the
/// in-memory check. Logs and returns `false` on I/O failure.
pub fn is_managed_dll(path: &Path) -> bool {
    use std::io::Read;
    let mut f = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) => {
            tracing::warn!(
                "is_managed_dll: cannot open {} — dropping ref ({})",
                path.display(),
                e,
            );
            return false;
        }
    };
    let mut buf = [0u8; 1024];
    let n = match f.read(&mut buf) {
        Ok(n) => n,
        Err(e) => {
            tracing::warn!(
                "is_managed_dll: read failed for {} — dropping ref ({})",
                path.display(),
                e,
            );
            return false;
        }
    };
    check_clr_header(&buf[..n])
}

/// Bounds-checked little-endian primitive reads. `None` on out-of-bounds.
fn read_u16_le(bytes: &[u8], at: usize) -> Option<u16> {
    bytes
        .get(at..at + 2)
        .map(|s| u16::from_le_bytes(s.try_into().unwrap()))
}
fn read_u32_le(bytes: &[u8], at: usize) -> Option<u32> {
    bytes
        .get(at..at + 4)
        .map(|s| u32::from_le_bytes(s.try_into().unwrap()))
}
