//! Project-tree scanning via Watchman.
//!
//! Required scanning backend for the mutable project tree. No filesystem walk
//! fallback — `usg lock` / `generate` / `typecheck` / `build` hard-fail with a
//! clear "install watchman" message when the daemon is unreachable.
//!
//! Thin wrapper over the shared [`unity_watch`] crate
//! (<https://github.com/studio-boxcat/unity-watch>): that crate owns the
//! Watchman wire layer (sync facade over `watchman_client`, socket-env
//! pinning, connect/resolve). This module only pins *which* dirs and suffixes
//! the project scan cares about, and maps the shared error into the
//! crate-level `ScanError`.
//!
//! Architecture decision: Watchman watches **only the project tree**
//! (`Assets/`, `Packages/`, `Library/PackageCache/`, `ProjectSettings/`). The
//! Unity Editor install is **never** watched — its multi-GB recursive crawl
//! is one of Watchman's worst cold-start cases (Metro hit ~2 minutes on
//! Windows; see [Watchman troubleshooting]) and it never changes in place
//! within an editor version. The install scan goes through [`unity_install`]
//! and is keyed by `unity-version` string equality, not Watchman.
//!
//! [Watchman troubleshooting]: https://facebook.github.io/watchman/docs/troubleshooting

use std::path::Path;

use unity_watch::{Filter, WatchError};

/// Map a `ScanError` into the crate-level error type, preserving the
/// `Unavailable` discriminant so callers (BoxcatBridge, CLI) can surface
/// the "install watchman" message without string-matching.
pub(crate) fn to_generator_error(e: ScanError) -> crate::error::GeneratorError {
    match e {
        ScanError::Unavailable => crate::error::GeneratorError::ScanUnavailable(e.to_string()),
        ScanError::Query(_) => crate::error::GeneratorError::Other(e.to_string()),
    }
}

/// Errors from [`enumerate`].
#[derive(Debug, thiserror::Error)]
pub enum ScanError {
    /// Watchman CLI not found, daemon socket unreachable, or discovery
    /// failed. User-facing fix:
    ///   - macOS:   `brew install watchman`
    ///   - Linux:   `pacman -S watchman` / `apt install watchman` / build from source
    ///   - Windows: `choco install watchman`
    ///
    /// Then `watchman watch /path/to/project` to seed the daemon.
    #[error("watchman is required for project scanning but is unavailable. Install: macOS `brew install watchman`, Windows `choco install watchman`, Linux per your package manager.")]
    Unavailable,
    /// BSER decode, query rejected, transport error, or non-UTF-8 path in
    /// the response. Exceptional — the daemon was reachable but the
    /// transaction failed.
    #[error("watchman query failed: {0}")]
    Query(#[from] anyhow::Error),
}

impl From<WatchError> for ScanError {
    fn from(e: WatchError) -> Self {
        match e {
            WatchError::Unavailable => ScanError::Unavailable,
            WatchError::Query(e) => ScanError::Query(e),
        }
    }
}

/// File suffixes Watchman should report changes for. Drives the `Suffix`
/// filter so we never see `Temp/`, build artifacts, or unrelated assets even
/// though Watchman roots at a higher ancestor.
///
/// Anything that contributes to lockfile / scan-cache content must be listed:
/// - `cs`/`asmdef`/`asmref` — the project source layout
/// - `dll` — bundled package DLLs under `Library/PackageCache`
/// - `json` — `packages-lock.json`, `manifest.json`, asmdef sidecar files
/// - `asset`, `txt` — `ProjectVersion.txt`, `ProjectSettings.asset`
const SUFFIXES: &[&str] = &["cs", "asmdef", "asmref", "dll", "json", "asset", "txt"];

/// Top-level directories under the project root that we want watched.
/// Everything else (`Temp/`, `Build/`, `obj/`, `.git/`) is implicitly
/// excluded because Watchman only walks these subtrees.
///
/// `Library/PackageCache` is included intentionally despite the cold-crawl
/// cost (can be tens of thousands of files on large projects) because
/// package-bundled `.asmdef` files live there and the scanner needs them.
const TOPLEVEL_DIRS: &[&str] = &[
    "Assets",
    "Packages",
    "Library/PackageCache",
    "ProjectSettings",
];

/// One-time process setup: pin `WATCHMAN_SOCK` so each connect skips the
/// `watchman get-sockname` subprocess fork (~120 ms on macOS). Call once at
/// the top of `main()`, before any threads are spawned. See
/// [`unity_watch::init_socket_env`] for the convention + safety contract.
pub fn init_socket_env() {
    unity_watch::init_socket_env();
}

/// Enumerate every project-tree file matching our suffix filter under
/// `Assets/`, `Packages/`, `Library/PackageCache/`, `ProjectSettings/`.
/// Watchman returns project-relative paths (forward-slash separators).
/// One query per invocation; no clock cursor tracking — invalidation lives
/// in the caller's mtime fingerprint over the persisted enumeration.
///
/// The cold-watch cost (Watchman recursively crawls the project on first
/// contact) lives inside this call; the `scan.watchman_query` span brackets
/// it so a slow first invocation is attributable.
pub fn enumerate(project_root: &Path) -> Result<Vec<String>, ScanError> {
    let _span = tracing::info_span!("scan.watchman_query").entered();
    Ok(unity_watch::enumerate(
        project_root,
        &Filter::new(TOPLEVEL_DIRS, SUFFIXES),
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Integration test against a real Watchman daemon. Gated `#[ignore]` so
    /// `cargo test` is green on machines without Watchman; run via
    /// `cargo test --ignored scan::` to exercise.
    #[test]
    #[ignore = "requires watchman daemon"]
    fn enumerate_returns_project_files() {
        use std::fs;
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("Assets")).unwrap();
        fs::create_dir_all(root.join("Packages")).unwrap();
        fs::create_dir_all(root.join("ProjectSettings")).unwrap();
        fs::write(
            root.join("ProjectSettings/ProjectVersion.txt"),
            "m_EditorVersion: 2022.3.0f1\n",
        )
        .unwrap();
        fs::write(root.join("Assets/Foo.cs"), "// stub\n").unwrap();

        let paths = enumerate(root).expect("watchman should be running");
        assert!(
            paths.iter().any(|p| p.ends_with("Foo.cs")),
            "enumerate should return Foo.cs, got {paths:?}"
        );

        let _ = std::process::Command::new("watchman")
            .arg("watch-del")
            .arg(root)
            .status();
    }

    #[test]
    fn enumerate_errors_on_nonexistent_path_without_panicking() {
        let result = enumerate(Path::new("/nonexistent/usg-test-path-xyz"));
        assert!(result.is_err());
    }
}
