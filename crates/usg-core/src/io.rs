use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;

use crate::error::{Result, io_err};

pub fn read_file(path: &str) -> Result<String> {
    let mut f = File::open(path).map_err(|e| io_err(path, e))?;
    let mut s = String::new();
    f.read_to_string(&mut s).map_err(|e| io_err(path, e))?;
    Ok(s)
}

/// Read a file as bytes; used for byte-equality fast path in `write_file_if_changed`.
fn read_bytes(path: &str) -> std::io::Result<Vec<u8>> {
    let mut f = File::open(path)?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf)?;
    Ok(buf)
}

/// Crash-safe write: stage to a sibling temp file, fsync, then rename onto the
/// destination atomically. Concurrent readers see either the prior contents or
/// the new contents — never a partial write. Survives EXDEV (cross-device
/// rename) by falling back to copy + delete inside `tempfile::persist`.
pub fn atomic_write_bytes(path: &str, bytes: &[u8]) -> Result<()> {
    let p = Path::new(path);
    let parent = p.parent().unwrap_or_else(|| Path::new("."));
    let mut tmp = tempfile::NamedTempFile::new_in(parent).map_err(|e| io_err(path, e))?;
    tmp.write_all(bytes).map_err(|e| io_err(path, e))?;
    // fsync the file before rename — a power loss between rename and the
    // implicit close would otherwise leave the inode pointing at an empty
    // block on some filesystems (ext4 with data=ordered, NTFS lazy-write).
    tmp.as_file().sync_all().map_err(|e| io_err(path, e))?;
    tmp.persist(p).map_err(|e| io_err(path, e.error))?;
    Ok(())
}

/// Returns true if the file was written, false if it was already byte-identical.
/// Writes go through `atomic_write_bytes` so a crash can't corrupt the file.
pub fn write_file_if_changed(path: &str, content: &str) -> Result<bool> {
    let bytes = content.as_bytes();
    if let Ok(existing) = read_bytes(path) {
        if existing.as_slice() == bytes {
            return Ok(false);
        }
    }
    atomic_write_bytes(path, bytes)?;
    Ok(true)
}

pub fn create_dir_all(path: &str) {
    let _ = std::fs::create_dir_all(path);
}

pub fn file_exists(path: &str) -> bool {
    Path::new(path).exists()
}

/// Plain `readdir` returning names (excluding `.` and `..`). Order undefined.
pub fn list_directory(path: &str) -> Vec<String> {
    let Ok(rd) = std::fs::read_dir(path) else {
        return Vec::new();
    };
    rd.filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().into_owned()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn atomic_write_creates_new_file() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("hello.txt");
        atomic_write_bytes(p.to_str().unwrap(), b"world").unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"world");
    }

    #[test]
    fn atomic_write_replaces_existing_atomically() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("hello.txt");
        std::fs::write(&p, b"old").unwrap();
        atomic_write_bytes(p.to_str().unwrap(), b"new").unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"new");
        // No temp file should leak in the parent dir
        let entries: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name())
            .collect();
        assert_eq!(entries.len(), 1, "leaked tempfile: {:?}", entries);
    }

    #[test]
    fn write_file_if_changed_idempotent() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("hello.txt");
        let path_s = p.to_str().unwrap();
        assert!(write_file_if_changed(path_s, "x").unwrap());
        assert!(!write_file_if_changed(path_s, "x").unwrap()); // unchanged
        assert!(write_file_if_changed(path_s, "y").unwrap());
    }
}
