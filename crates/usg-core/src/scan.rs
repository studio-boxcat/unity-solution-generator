//! Watchman wire layer.
//!
//! Required scanning backend for the mutable project tree. No filesystem walk
//! fallback — `usg lock` / `generate` / `typecheck` / `build` hard-fail with a
//! clear "install watchman" message when the daemon is unreachable.
//!
//! Sync facade pattern (mirrored from sibling project `unity-assetdb`): build
//! a current-thread tokio runtime per call (~µs cost; one call per CLI
//! invocation). Keeps tokio confined to this module so the rest of the crate
//! stays sync and `BoxcatBridge`'s FFI host doesn't have to host a runtime.
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

use std::path::{Path, PathBuf};

use watchman_client::Error as WatchmanError;
use watchman_client::prelude::*;

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
    /// Then `watchman watch /path/to/project` to seed the daemon.
    #[error("watchman is required for project scanning but is unavailable. Install: macOS `brew install watchman`, Windows `choco install watchman`, Linux per your package manager.")]
    Unavailable,
    /// BSER decode, query rejected, transport error, or non-UTF-8 path in
    /// the response. Exceptional — the daemon was reachable but the
    /// transaction failed.
    #[error("watchman query failed: {0}")]
    Query(#[from] anyhow::Error),
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
/// First-watch cost is surfaced via the `scan.first_watch` tracing span.
const TOPLEVEL_DIRS: &[&str] = &[
    "Assets",
    "Packages",
    "Library/PackageCache",
    "ProjectSettings",
];

/// One-time process setup. Computes the Watchman socket path from the
/// documented per-user convention and stashes it in `WATCHMAN_SOCK` so
/// `Connector::connect()` skips its internal socket-discovery hop (which
/// would otherwise spawn `watchman get-sockname` as a subprocess on every
/// connect — ~120 ms on macOS).
///
/// Conventions ([watchman docs]):
/// - Unix:    `$XDG_STATE_HOME/watchman/<user>-state/sock`,
///            fallback `$HOME/.local/state/watchman/<user>-state/sock`.
/// - Windows: named pipe `\\.\pipe\watchman-<user>`.
///
/// User has overridden `--sockname` in their watchman config? Our path
/// won't exist on disk; we leave `WATCHMAN_SOCK` unset and `watchman_client`
/// falls back to its own (slower) discovery, which honors the config.
///
/// Idempotent — no-op if `WATCHMAN_SOCK` is already set.
///
/// [watchman docs]: https://facebook.github.io/watchman/docs/cli-options#unix-domain-sockets
pub fn init_socket_env() {
    if std::env::var_os("WATCHMAN_SOCK").is_some() {
        return;
    }
    let Some(path) = conventional_sock_path() else {
        return;
    };
    // Validate the path exists before setting the env var. A stale convention
    // (e.g. user uninstalled watchman) would otherwise wedge `Connector::connect()`
    // with a confusing socket-error rather than the cleaner discovery fallback.
    if !std::path::Path::new(&path).exists() {
        return;
    }
    // SAFETY: called at the top of main() before any threads are spawned.
    // `set_var` is unsound under concurrent reads on POSIX; that's why this
    // function explicitly documents the "single-threaded only" contract.
    unsafe {
        std::env::set_var("WATCHMAN_SOCK", path);
    }
}

/// Per-platform default Watchman socket path. Mirrors `compute_user_state_dir()`
/// in watchman's C++ source — keep in sync if upstream conventions change.
fn conventional_sock_path() -> Option<String> {
    if cfg!(target_os = "windows") {
        // Windows uses a named pipe under \\.\pipe\watchman-<user>.
        let user = std::env::var("USERNAME").ok().filter(|s| !s.is_empty())?;
        return Some(format!(r"\\.\pipe\watchman-{}", user));
    }
    let user = std::env::var("USER").ok().filter(|s| !s.is_empty())?;
    if let Ok(xdg) = std::env::var("XDG_STATE_HOME") {
        if !xdg.is_empty() {
            return Some(format!("{}/watchman/{}-state/sock", xdg, user));
        }
    }
    let home = std::env::var("HOME").ok().filter(|s| !s.is_empty())?;
    Some(format!("{}/.local/state/watchman/{}-state/sock", home, user))
}

/// Enumerate every project-tree file matching our suffix filter under
/// `Assets/`, `Packages/`, `Library/PackageCache/`, `ProjectSettings/`.
/// Watchman returns project-relative paths (forward-slash separators).
/// One query per invocation; no clock cursor tracking — invalidation lives
/// in the caller's mtime fingerprint over the persisted enumeration.
pub fn enumerate(project_root: &Path) -> Result<Vec<String>, ScanError> {
    // `enable_all` covers IO + time; `watchman_client` internally uses
    // `tokio::time::timeout` on the connect path in newer versions, and the
    // cost of enabling the time driver on a one-shot runtime is negligible.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| ScanError::Query(anyhow::Error::new(e).context("build tokio runtime")))?;
    rt.block_on(enumerate_inner(project_root))
}

async fn enumerate_inner(project_root: &Path) -> Result<Vec<String>, ScanError> {
    let _span = tracing::info_span!("scan.watchman_query").entered();

    let client = Connector::new().connect().await.map_err(map_connect_err)?;

    let canonical = CanonicalPath::canonicalize(project_root).map_err(|e| {
        ScanError::Query(anyhow::Error::new(e).context("canonicalize project_root"))
    })?;
    // Cold-watch cost lives here — Watchman recursively crawls the project
    // on first contact. Surface it under its own span so users debugging a
    // slow first invocation see "scan.first_watch" rather than blaming the
    // whole subcommand.
    let resolved = {
        let _s = tracing::info_span!("scan.first_watch_or_resolve").entered();
        client
            .resolve_root(canonical)
            .await
            .map_err(|e| ScanError::Query(anyhow::Error::new(e)))?
    };

    let expression = Expr::All(vec![
        Expr::Any(
            TOPLEVEL_DIRS
                .iter()
                .map(|d| {
                    Expr::DirName(DirNameTerm {
                        path: PathBuf::from(d),
                        depth: None,
                    })
                })
                .collect(),
        ),
        Expr::Suffix(SUFFIXES.iter().map(PathBuf::from).collect()),
    ]);

    let request = QueryRequestCommon {
        expression: Some(expression),
        ..Default::default()
    };

    let result = client
        .query::<NameOnly>(&resolved, request)
        .await
        .map_err(|e| ScanError::Query(anyhow::Error::new(e)))?;

    // Watchman returns paths relative to the watch root. With `relative_root`
    // auto-set from `ResolvedRoot`, they're project-relative.
    Ok(result
        .files
        .unwrap_or_default()
        .into_iter()
        .filter_map(|row| row.name.into_inner().into_os_string().into_string().ok())
        .collect())
}

/// Watchman returns very different errors for "binary not installed"
/// (`ConnectionDiscovery`, from the CLI subprocess used to find the socket)
/// vs. "daemon not running" (`Connect`, from the socket open). We treat both
/// as `Unavailable` — the user-facing fix is the same (install + start).
fn map_connect_err(e: WatchmanError) -> ScanError {
    match e {
        WatchmanError::ConnectionDiscovery { .. } | WatchmanError::Connect { .. } => {
            ScanError::Unavailable
        }
        other => ScanError::Query(anyhow::Error::new(other)),
    }
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
