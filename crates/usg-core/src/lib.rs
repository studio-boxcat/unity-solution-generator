//! Unity Solution Generator core library.
//!
//! Ported from the Swift `SolutionGeneratorCore` target.

pub mod defines;
pub mod error;
pub mod io;
pub mod json;
pub mod lock_cache;
pub mod lockfile;
pub mod lockfile_scanner;
pub mod paths;
pub mod profile;
pub mod project_scanner;
pub mod solution_generator;
pub mod template_extractor;
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
pub use template_extractor::{ExtractTemplatesOptions, TemplateExtractor};

/// Test-only re-exports of internal helpers. Not part of the stable public API.
#[doc(hidden)]
pub mod __test_only {
    pub use crate::lock_cache::{build_entries, is_valid};
}
