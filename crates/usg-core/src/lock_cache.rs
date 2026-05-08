//! Mtime fingerprint cache for `lock`.
//!
//! When `LockfileIO::scan_and_write` runs, we record an mtime for every directory
//! and key file the lockfile content depends on. On subsequent invocations we re-stat
//! that set; if every mtime matches, the previous `csproj.lock` is still valid and we
//! skip the entire Unity-install + project-side scan.
//!
//! Storage: a sibling file `Library/UnitySolutionGenerator/lock-fingerprint`, one
//! `path|mtime_ns` per line. A leading `# unity-version: ...` comment lets a human
//! sanity-check what version produced it; the cache is keyed by mtime alone (fast).

use std::collections::BTreeSet;

use crate::io::{create_dir_all, has_matching_version, read_file, write_file_if_changed};
use crate::paths::{join_path, parent_directory};

const FINGERPRINT_FILE: &str = "lock-fingerprint";

/// Routed through the workspace-level `CACHE_VERSION`. See [[architecture.md]].
const LOCK_FINGERPRINT_VERSION: u32 = crate::CACHE_VERSION;

pub fn fingerprint_path(generator_dir: &str) -> String {
    join_path(generator_dir, FINGERPRINT_FILE)
}

/// Returns the recorded set of `(path, mtime_ns)` pairs, or `None` if the
/// fingerprint file is missing or malformed.
pub fn load(path: &str) -> Option<Vec<(String, u128)>> {
    let content = read_file(path).ok()?;
    if !has_matching_version(&content, LOCK_FINGERPRINT_VERSION) {
        return None;
    }
    let mut entries = Vec::new();
    for line in content.split('\n') {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let pipe = line.find('|')?;
        let mtime: u128 = line[pipe + 1..].parse().ok()?;
        entries.push((line[..pipe].to_string(), mtime));
    }
    if entries.is_empty() {
        return None;
    }
    Some(entries)
}

/// Returns true if every recorded path still has the same mtime as when the
/// fingerprint was written.
pub fn is_valid(entries: &[(String, u128)]) -> bool {
    for (p, recorded) in entries {
        let Ok(meta) = std::fs::metadata(p) else {
            return false;
        };
        let Some(now) = mtime_nanos(&meta) else {
            return false;
        };
        if now != *recorded {
            return false;
        }
    }
    true
}

/// Build a fingerprint set from the inputs that influenced a freshly-written lockfile.
/// Includes: top-level project files, the Unity install root, and every project-side
/// directory that contributed a `.dll` or `.asmdef` (plus their ancestors up to the
/// scan root, so adds/removes are caught at any depth).
pub fn build_entries(
    project_root: &str,
    unity_path: &str,
    contributed_paths_relative: &[String],
) -> Vec<(String, u128)> {
    let mut paths: BTreeSet<String> = BTreeSet::new();

    let mut add_abs = |p: String| {
        paths.insert(p);
    };
    add_abs(unity_path.to_string());
    add_abs(join_path(project_root, "ProjectSettings/ProjectVersion.txt"));
    add_abs(join_path(project_root, "ProjectSettings/ProjectSettings.asset"));
    add_abs(join_path(project_root, "Packages"));
    add_abs(join_path(project_root, "Packages/manifest.json"));
    add_abs(join_path(project_root, "Assets"));
    add_abs(join_path(project_root, "Library/PackageCache"));

    // Every directory that contributed a .dll or .asmdef (and its ancestors up to
    // the project root). Adding/removing files there bumps the directory's mtime.
    for rel in contributed_paths_relative {
        let mut dir = parent_directory(rel).to_string();
        loop {
            if dir.is_empty() {
                break;
            }
            paths.insert(join_path(project_root, &dir));
            let parent = parent_directory(&dir).to_string();
            if parent == dir {
                break;
            }
            dir = parent;
        }
    }

    paths
        .into_iter()
        .filter_map(|p| {
            let m = std::fs::metadata(&p).ok()?;
            let ns = mtime_nanos(&m)?;
            Some((p, ns))
        })
        .collect()
}

pub fn write(
    fingerprint_file: &str,
    unity_version: &str,
    entries: &[(String, u128)],
) -> std::io::Result<()> {
    create_dir_all(parent_directory(fingerprint_file));
    let mut s = String::from("# lock-fingerprint — auto-generated, do not edit\n");
    s.push_str(&format!("# version: {}\n", LOCK_FINGERPRINT_VERSION));
    s.push_str(&format!("# unity-version: {}\n", unity_version));
    for (path, mtime) in entries {
        s.push_str(path);
        s.push('|');
        s.push_str(&mtime.to_string());
        s.push('\n');
    }
    write_file_if_changed(fingerprint_file, &s).map(|_| ()).map_err(|e| {
        std::io::Error::other(format!("{}", e))
    })
}

#[cfg(unix)]
fn mtime_nanos(m: &std::fs::Metadata) -> Option<u128> {
    use std::os::unix::fs::MetadataExt;
    let secs: i64 = m.mtime();
    let nanos: i64 = m.mtime_nsec();
    if secs < 0 {
        return None;
    }
    Some((secs as u128) * 1_000_000_000 + (nanos as u128))
}

#[cfg(not(unix))]
fn mtime_nanos(m: &std::fs::Metadata) -> Option<u128> {
    let mt = m.modified().ok()?;
    let d = mt.duration_since(std::time::SystemTime::UNIX_EPOCH).ok()?;
    Some(d.as_nanos())
}
