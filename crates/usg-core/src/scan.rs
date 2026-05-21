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

/// Result of a `since` query.
#[derive(Debug)]
pub enum Delta {
    /// Watchman returned `is_fresh_instance` — daemon restart, journal
    /// loss, or brand-new watch. `paths` is the **full enumeration** of
    /// matching files (Watchman emits the complete set when it can't
    /// compute a delta), so callers can use it as a walk substitute and
    /// skip any separate filesystem traversal. Persist `new_clock` for
    /// the next call.
    Fresh {
        paths: Vec<String>,
        new_clock: String,
    },
    /// Steady-state delta. `paths` is only the project-relative paths
    /// that changed (added, modified, or deleted — stat each file to
    /// disambiguate). May be empty (cache hit: nothing relevant changed).
    Touched {
        paths: Vec<String>,
        new_clock: String,
    },
}

impl Delta {
    /// Discard the Fresh/Touched discriminator and return the inner fields.
    /// Useful when the caller has already decided to treat both variants
    /// the same (e.g. cold-scan enumeration: a fresh-instance response and
    /// a since-from-None response both carry the full file set).
    pub fn into_paths_and_clock(self) -> (Vec<String>, String) {
        match self {
            Delta::Fresh { paths, new_clock } | Delta::Touched { paths, new_clock } => {
                (paths, new_clock)
            }
        }
    }
}

/// Errors from [`since`].
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

/// Sidecar file holding the last-known watchman sockname. Lives at
/// `~/.cache/unity-solution-generator/watchman-sock`. Per-user (sockname is
/// per-user-state in watchman's design).
fn sockname_sidecar_path() -> Option<std::path::PathBuf> {
    use crate::paths::host_cache_root_pub;
    Some(std::path::PathBuf::from(host_cache_root_pub()?).join("unity-solution-generator").join("watchman-sock"))
}

/// One-time process setup. Resolves the Watchman socket path and stashes it
/// in `WATCHMAN_SOCK` so subsequent `Connector::connect()` calls skip the
/// per-call socket-discovery hop. Two-tier lookup:
///   1. Cached sidecar file (~1 ms). Validated by `connect_or_retry` —
///      a stale sockname (watchman restarted, sock path changed) trips
///      retry below.
///   2. `watchman get-sockname` subprocess (~120 ms). Result written to
///      the sidecar.
///
/// Idempotent — no-op if `WATCHMAN_SOCK` is already set, or if neither
/// lookup succeeds (the connector still works without it, just slower).
///
/// Why: the `watchman_client` crate's `Connector` spawns
/// `watchman get-sockname` on every `connect()` unless `WATCHMAN_SOCK` is
/// in env. A single CLI invocation issues multiple `since()` calls, so the
/// 120 ms cost compounds without this cache.
pub fn init_socket_env() {
    if std::env::var_os("WATCHMAN_SOCK").is_some() {
        return;
    }
    // Tier 1: sidecar.
    if let Some(sidecar) = sockname_sidecar_path() {
        if let Ok(s) = std::fs::read_to_string(&sidecar) {
            let s = s.trim();
            if !s.is_empty() && std::path::Path::new(s).exists() {
                // SAFETY: top-of-main, single-threaded.
                unsafe { std::env::set_var("WATCHMAN_SOCK", s); }
                return;
            }
        }
    }
    // Tier 2: subprocess.
    let Ok(out) = std::process::Command::new("watchman").arg("get-sockname").output() else {
        return;
    };
    if !out.status.success() {
        return;
    }
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(&out.stdout) else {
        return;
    };
    let Some(sock) = v.get("sockname").and_then(|s| s.as_str()) else {
        return;
    };
    // SAFETY: called before any threads are spawned (top of main()).
    unsafe {
        std::env::set_var("WATCHMAN_SOCK", sock);
    }
    // Best-effort sidecar write — failure just means next invocation pays the
    // subprocess cost again.
    if let Some(sidecar) = sockname_sidecar_path() {
        if let Some(parent) = sidecar.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&sidecar, sock);
    }
}

/// Called if a Watchman connect/query fails — the cached sockname may be
/// stale (daemon restarted with a new sock). Wiping the sidecar forces the
/// next `init_socket_env` to re-spawn `watchman get-sockname`.
pub fn invalidate_sockname_cache() {
    if let Some(sidecar) = sockname_sidecar_path() {
        let _ = std::fs::remove_file(sidecar);
    }
}

/// Query Watchman for everything that changed under `project_root` since
/// `prev_clock`. Pass `None` for the first call on a project (returns
/// `Fresh`); pass `Some(clock)` from a prior call for an incremental delta.
pub fn since(project_root: &Path, prev_clock: Option<&str>) -> Result<Delta, ScanError> {
    // `enable_all` covers IO + time; `watchman_client` internally uses
    // `tokio::time::timeout` on the connect path in newer versions, and the
    // cost of enabling the time driver on a one-shot runtime is negligible.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| ScanError::Query(anyhow::Error::new(e).context("build tokio runtime")))?;
    rt.block_on(since_inner(project_root, prev_clock))
}

async fn since_inner(
    project_root: &Path,
    prev_clock: Option<&str>,
) -> Result<Delta, ScanError> {
    let _span = tracing::info_span!("scan.watchman_query", fresh = prev_clock.is_none()).entered();

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
        since: prev_clock.map(|c| Clock::Spec(ClockSpec::StringClock(c.to_owned()))),
        expression: Some(expression),
        ..Default::default()
    };

    let result = client
        .query::<NameOnly>(&resolved, request)
        .await
        .map_err(|e| ScanError::Query(anyhow::Error::new(e)))?;

    let new_clock = clock_to_string(result.clock)?;

    // Watchman returns paths relative to the watch root. With `relative_root`
    // auto-set from `ResolvedRoot`, they're project-relative — same shape our
    // scan-cache uses.
    let paths: Vec<String> = result
        .files
        .unwrap_or_default()
        .into_iter()
        .filter_map(|row| row.name.into_inner().into_os_string().into_string().ok())
        .collect();

    if result.is_fresh_instance {
        Ok(Delta::Fresh { paths, new_clock })
    } else {
        Ok(Delta::Touched { paths, new_clock })
    }
}

fn clock_to_string(clock: Clock) -> Result<String, ScanError> {
    match clock {
        Clock::Spec(ClockSpec::StringClock(s)) => Ok(s),
        // UnixTimestamp clocks aren't normally emitted; the server returns
        // a string clock for our request. Stringify defensively so we don't
        // drop data on the floor if it ever happens.
        Clock::Spec(ClockSpec::UnixTimestamp(t)) => Ok(t.to_string()),
        // We don't request SCM-aware queries; receiving one would indicate
        // a server-config mismatch. Refuse to store a partial cursor.
        Clock::ScmAware(_) => Err(ScanError::Query(anyhow::anyhow!(
            "watchman returned an SCM-aware clock; not requested",
        ))),
    }
}

/// Watchman returns very different errors for "binary not installed"
/// (`ConnectionDiscovery`, from the CLI subprocess used to find the socket)
/// vs. "daemon not running" (`Connect`, from the socket open). We treat both
/// as `Unavailable` — the user-facing fix is the same (install + start).
fn map_connect_err(e: WatchmanError) -> ScanError {
    match e {
        WatchmanError::ConnectionDiscovery { .. } | WatchmanError::Connect { .. } => {
            // Sidecar sockname may be stale (daemon restarted with new sock
            // path). Nuke it so the next invocation re-runs `watchman get-sockname`.
            invalidate_sockname_cache();
            ScanError::Unavailable
        }
        other => ScanError::Query(anyhow::Error::new(other)),
    }
}

/// Test whether a Watchman hint is relevant to the project / lockfile scan.
/// Used by `project_scanner` to decide whether a `Touched` delta requires a
/// cache invalidation.
pub fn hint_is_relevant(hint: &str) -> bool {
    hint.ends_with(".cs")
        || hint.ends_with(".asmdef")
        || hint.ends_with(".asmref")
        || hint.ends_with(".dll")
        || hint.ends_with("manifest.json")
        || hint.ends_with("packages-lock.json")
        || hint.ends_with("ProjectVersion.txt")
        || hint.ends_with("ProjectSettings.asset")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hint_relevance_filters_noise() {
        assert!(hint_is_relevant("Assets/Foo/Bar.cs"));
        assert!(hint_is_relevant("Packages/com.unity.x/Runtime.asmdef"));
        assert!(hint_is_relevant("Library/PackageCache/x@1.0.0/x.dll"));
        assert!(hint_is_relevant("Packages/manifest.json"));
        assert!(hint_is_relevant("Packages/packages-lock.json"));
        assert!(hint_is_relevant("ProjectSettings/ProjectVersion.txt"));
        assert!(hint_is_relevant("ProjectSettings/ProjectSettings.asset"));

        // Irrelevant — Unity scene/asset files that don't affect csproj content.
        assert!(!hint_is_relevant("Assets/Foo.prefab"));
        assert!(!hint_is_relevant("Assets/Foo.meta"));
        assert!(!hint_is_relevant("Library/Bee/Build.txt"));
    }

    /// Integration test against a real Watchman daemon. Gated `#[ignore]` so
    /// `cargo test` is green on machines without Watchman; run via
    /// `cargo test --ignored scan::` to exercise.
    #[test]
    #[ignore = "requires watchman daemon"]
    fn since_returns_fresh_then_touched() {
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

        let first = since(root, None).expect("watchman should be running");
        let clock = match first {
            Delta::Fresh { paths, new_clock } => {
                assert!(
                    paths.iter().any(|p| p.ends_with("Foo.cs")),
                    "Fresh paths should enumerate Foo.cs, got {paths:?}"
                );
                new_clock
            }
            Delta::Touched { .. } => panic!("expected Fresh on first call"),
        };

        std::thread::sleep(std::time::Duration::from_millis(50));
        fs::write(root.join("Assets/Foo.cs"), "// changed\n").unwrap();

        let second = since(root, Some(&clock)).expect("watchman should be running");
        match second {
            Delta::Touched { paths, .. } => {
                assert!(
                    paths.iter().any(|p| p.ends_with("Foo.cs")),
                    "expected touched path for Foo.cs, got {paths:?}",
                );
            }
            Delta::Fresh { .. } => panic!("expected Touched on second call"),
        }

        let _ = std::process::Command::new("watchman")
            .arg("watch-del")
            .arg(root)
            .status();
    }

    #[test]
    fn since_errors_on_nonexistent_path_without_panicking() {
        let result = since(Path::new("/nonexistent/usg-test-path-xyz"), None);
        assert!(result.is_err());
    }
}
