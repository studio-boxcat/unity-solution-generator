use std::fs::{File, OpenOptions};
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

/// Returns true if the file was written, false if it was already byte-identical.
pub fn write_file_if_changed(path: &str, content: &str) -> Result<bool> {
    let bytes = content.as_bytes();
    if let Ok(existing) = read_bytes(path) {
        if existing.as_slice() == bytes {
            return Ok(false);
        }
    }
    let mut f = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(path)
        .map_err(|e| io_err(path, e))?;
    f.write_all(bytes).map_err(|e| io_err(path, e))?;
    Ok(true)
}

pub fn create_dir_all(path: &str) {
    let _ = std::fs::create_dir_all(path);
}

pub fn file_exists(path: &str) -> bool {
    Path::new(path).exists()
}

pub fn is_dir(path: &str) -> bool {
    std::fs::metadata(path).map(|m| m.is_dir()).unwrap_or(false)
}

/// Plain `readdir` returning names (excluding `.` and `..`). Order undefined.
pub fn list_directory(path: &str) -> Vec<String> {
    let Ok(rd) = std::fs::read_dir(path) else {
        return Vec::new();
    };
    rd.filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().into_owned()))
        .collect()
}
