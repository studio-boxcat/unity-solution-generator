//! Unity Solution Generator core library.
//!
//! Ported from the Swift `SolutionGeneratorCore` target.

/// Version of the user-visible `csproj.lock` format. Bump only on intentional
/// schema changes; `csproj.lock` may be checked in, so a bump means consumers
/// re-lock against possibly different Unity installs. See [[architecture.md]].
pub const LOCKFILE_VERSION: u32 = 1;

/// Version of the dev-local cache files (`scan-cache`, `lock-fingerprint`,
/// `.fingerprints/<hash>`). Bumping invalidates ALL three caches wholesale —
/// no migrations. These files are gitignored under `Library/`, so cold-rebuild
/// on bump is harmless. Cargo / Bazel idiom.
pub const CACHE_VERSION: u32 = 1;

pub mod defines;
pub mod error;
pub(crate) mod generate_cache;
pub mod io;
pub(crate) mod lock_cache;
pub mod lockfile;
pub mod lockfile_scanner;
pub mod paths;
pub mod profile;
pub mod project_scanner;
pub mod solution_generator;
pub mod typecheck;
pub(crate) mod walk;
pub mod xml;

pub use defines::{generate_version_defines, parse_scripting_defines};
pub use error::{GeneratorError, LockfileError, Result};
pub use lockfile::{DllRef, Lockfile, LockfileIO, RefCategory};
pub use lockfile_scanner::LockfileScanner;
pub use paths::{
    DEFAULT_GENERATOR_ROOT, lockfile_path, parent_directory, resolve_project_root, resolve_real_path,
};
pub use project_scanner::{AsmDefRecord, ProjectCategory, ProjectScanner, ScanResult, VersionDefine};
pub use solution_generator::{
    BuildConfig, BuildPlatform, GenerateOptions, GenerateResult, SolutionGenerator,
};
pub use typecheck::{TypecheckOptions, TypecheckResult};
/// Test-only re-exports of internal helpers. Not part of the stable public API.
#[doc(hidden)]
pub mod __test_only {
    pub use crate::lock_cache::{build_entries, is_valid};
    pub use crate::typecheck::__test_only_build_rsp as build_rsp;
}
