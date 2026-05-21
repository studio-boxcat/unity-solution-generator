//! Tarball-extract cache for Unity-bundled packages.
//!
//! Unity ships some packages as already-extracted directories under
//! `<UnityInstall>/Contents/Resources/PackageManager/BuiltInPackages/`, and
//! others as gzipped tarballs under `.../PackageManager/Editor/*.tgz` that the
//! editor extracts into `Library/PackageCache/` only after a project is
//! opened. On a fresh worktree where Unity hasn't run yet, the tarballs are
//! the only source of those packages' DLLs/asmdefs.
//!
//! This module materialises them on demand into a per-user, per-Unity-version
//! cache (`~/.cache/unity-solution-generator/<unity-version>/<package-name>/`)
//! so the lockfile scanner can find their assemblies without depending on
//! Unity having opened the project. Extraction is idempotent (`.complete`
//! sentinel) and concurrency-safe across worktrees via an exclusive lock file
//! per package.
//!
//! Tarball convention: npm-style with a top-level `package/` directory,
//! stripped on extract. The package's canonical name comes from
//! `package.json`'s `"name"` field — filename-parsing would be fragile because
//! package names contain `-` (e.g. `com.unity.nuget.newtonsoft-json-3.2.1.tgz`).

use std::fs::{self, File};
use std::io::{self, Read};
use std::path::Path;
use std::time::{Duration, Instant};

use crate::io::{create_dir_all, list_directory};
use crate::paths::{join_path, unity_data_subpath, usg_cache_dir};

/// Marker written last to a package's cache dir; absence means a half-baked
/// extraction (crashed worker or in-progress peer).
const COMPLETE_MARKER: &str = ".complete";

/// Wait-for-peer timeout when another process holds the per-package lock.
const PEER_WAIT: Duration = Duration::from_secs(60);

/// Extract just the tarball that resolves to `package_name` (matching
/// `<name>-<version>.tgz` under `<unity>/Contents/Resources/PackageManager/Editor`)
/// into `~/.cache/unity-solution-generator/<unity_version>/<package_name>/`.
///
/// Returns `Some(extract_dir)` on success or when already extracted; `None` if
/// no matching tarball exists or extraction fails. The lockfile scanner only
/// calls this for packages that PackageCache doesn't already cover, so the cost
/// is bounded by the actual gap rather than the full set of bundled tarballs.
pub fn ensure_extracted_for_package(
    unity_path: &str,
    unity_version: &str,
    package_name: &str,
) -> Option<String> {
    let cache_root = usg_cache_dir(unity_version);
    create_dir_all(&cache_root);

    let target = format!("{}/{}", cache_root, package_name);
    let complete = format!("{}/{}", target, COMPLETE_MARKER);
    if Path::new(&complete).exists() {
        return Some(target);
    }

    let editor_dir = join_path(
        unity_path,
        &format!("{}/Resources/PackageManager/Editor", unity_data_subpath()),
    );
    if !Path::new(&editor_dir).exists() {
        return None;
    }

    // Tarball naming: `<package_name>-<version>.tgz`. Package names contain
    // `-`, so we anchor on the prefix and require a `-<digit>` boundary —
    // matches `com.unity.nuget.newtonsoft-json-3.2.1.tgz` but rejects
    // `com.unity.nuget.newtonsoft-json-extra-1.0.0.tgz` (different package).
    let prefix = format!("{}-", package_name);
    let tgz_name = list_directory(&editor_dir).into_iter().find(|entry| {
        entry.ends_with(".tgz")
            && entry.starts_with(&prefix)
            && entry[prefix.len()..]
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_digit())
    })?;
    let tgz_path = format!("{}/{}", editor_dir, tgz_name);

    if let Err(e) = extract_one(&tgz_path, &cache_root) {
        tracing::warn!(
            "package_cache: failed to extract {} for {}: {}",
            tgz_path,
            package_name,
            e
        );
        return None;
    }
    Some(target)
}

/// Extract a single tarball into `<cache_root>/<package-name>/`. Idempotent
/// (no-op if `.complete` is already there). Concurrency-safe via an exclusive
/// per-package lock file (O_CREAT|O_EXCL).
fn extract_one(tgz_path: &str, cache_root: &str) -> io::Result<()> {
    let pkg_name = peek_package_name(Path::new(tgz_path))?;
    let target = Path::new(cache_root).join(&pkg_name);
    let complete = target.join(COMPLETE_MARKER);

    if complete.exists() {
        return Ok(());
    }

    let lock_path = Path::new(cache_root).join(format!(".lock.{}", pkg_name));
    let _lock_guard = match acquire_lock(&lock_path, &complete)? {
        LockOutcome::Acquired(g) => g,
        LockOutcome::PeerCompleted => return Ok(()),
    };

    // Re-check after lock acquisition — another worker may have published in
    // the window between our existence check and lock.
    if complete.exists() {
        return Ok(());
    }
    if target.exists() {
        fs::remove_dir_all(&target)?;
    }

    fs::create_dir_all(&target)?;
    extract_tarball(Path::new(tgz_path), &target)?;
    File::create(&complete)?;
    Ok(())
}

enum LockOutcome {
    /// Held exclusive advisory lock on the lockfile; auto-released when dropped.
    Acquired(File),
    /// Peer beat us to a complete extraction during the wait.
    PeerCompleted,
}

/// Acquire an exclusive advisory lock on a per-package lockfile via stable
/// `std::fs::File::try_lock` (Rust 1.89+). On Unix this maps to `flock(LOCK_EX)`;
/// on Windows to `LockFileEx`. The lock is released automatically when the
/// returned `File` drops — no leaked lock files, no recovery dance after a
/// crashed worker. The lockfile itself stays on disk and is reused next time.
///
/// Polls every 100 ms while the lock is contended, checking the
/// `complete_marker` between attempts so a peer that publishes-then-releases
/// short-circuits us instead of forcing redundant re-extraction. Times out
/// after `PEER_WAIT` (60 s) — extractions of Unity's bundled tarballs run
/// ~1 s each, so a 60 s wait is generous.
fn acquire_lock(lock_path: &Path, complete_marker: &Path) -> io::Result<LockOutcome> {
    let f = File::options()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(lock_path)?;
    let deadline = Instant::now() + PEER_WAIT;
    loop {
        match f.try_lock() {
            Ok(()) => return Ok(LockOutcome::Acquired(f)),
            Err(std::fs::TryLockError::WouldBlock) => {
                if complete_marker.exists() {
                    return Ok(LockOutcome::PeerCompleted);
                }
                if Instant::now() >= deadline {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        format!(
                            "package_cache: peer holding {} did not finish within {:?}",
                            lock_path.display(),
                            PEER_WAIT
                        ),
                    ));
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(std::fs::TryLockError::Error(e)) => return Err(e),
        }
    }
}

/// Read `package.json` from a `.tgz` without extracting the rest. npm-style
/// tarballs put it at exactly `package/package.json` (or top-level
/// `package.json` for hand-rolled archives). Anchor on those exact paths —
/// a deeper match like `package/sub/package.json` is a fixture, not the
/// manifest, and would silently mis-name the cache dir.
fn peek_package_name(tgz: &Path) -> io::Result<String> {
    let f = File::open(tgz)?;
    let gz = flate2::read::GzDecoder::new(f);
    let mut archive = tar::Archive::new(gz);
    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        let is_manifest = path == Path::new("package/package.json")
            || path == Path::new("package.json");
        if !is_manifest {
            continue;
        }
        let mut content = String::new();
        entry.read_to_string(&mut content)?;
        let v: serde_json::Value = serde_json::from_str(&content)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        if let Some(name) = v.get("name").and_then(|s| s.as_str()) {
            return Ok(name.to_string());
        }
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        format!("no package.json in {}", tgz.display()),
    ))
}

/// Extract every entry to `dest`, stripping the leading `package/` directory.
/// Reject symlinks/hardlinks defensively — tar's `unpack` would dereference a
/// relative symlink that escapes `dest` (e.g. `pwn -> /etc` followed by a
/// regular `pwn/foo` entry). Unity's bundled tarballs are trusted, but the
/// mitigation is cheap and the file's docstring promises archive containment.
fn extract_tarball(tgz: &Path, dest: &Path) -> io::Result<()> {
    let f = File::open(tgz)?;
    let gz = flate2::read::GzDecoder::new(f);
    let mut archive = tar::Archive::new(gz);
    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        let stripped = match path.strip_prefix("package") {
            Ok(p) => p.to_path_buf(),
            Err(_) => path.clone(),
        };
        if stripped.as_os_str().is_empty() {
            continue;
        }
        if stripped.is_absolute()
            || stripped
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            continue;
        }
        let header_type = entry.header().entry_type();
        if matches!(header_type, tar::EntryType::Symlink | tar::EntryType::Link) {
            continue;
        }
        let target = dest.join(&stripped);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        entry.unpack(&target)?;
    }
    Ok(())
}
