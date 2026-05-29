//! Unity Solution Generator core library.
//!
//! Ported from the Swift `SolutionGeneratorCore` target.

/// Version of the user-visible `csproj.lock` format. Bump only on intentional
/// schema changes; `csproj.lock` may be checked in, so a bump means consumers
/// re-lock against possibly different Unity installs. See [[architecture.md]].
///
/// v2: added `refs.playback.windows` section (Windows build target support).
pub const LOCKFILE_VERSION: u32 = 2;


pub(crate) mod csc;
pub(crate) mod csproj_render;
pub mod defines;
pub mod error;
pub mod io;
pub mod lockfile;
pub mod lockfile_scanner;
pub(crate) mod package_cache;
pub mod paths;
pub mod pe;
pub mod project_scanner;
pub mod scan;
pub mod solution_generator;
pub mod typecheck;
pub mod xml;

pub use defines::{generate_version_defines, parse_scripting_defines};
pub use error::{GeneratorError, LockfileError, Result};
pub use lockfile::{DllRef, Lockfile, RefCategory};
pub use paths::{
    DEFAULT_GENERATOR_ROOT, lockfile_path, parent_directory, resolve_project_root, resolve_real_path,
};
pub use project_scanner::{AsmDefRecord, ProjectCategory, ProjectName, ScanResult, VersionDefine};
pub use solution_generator::{
    BuildConfig, BuildPlatform, GenerateOptions, GenerateResult,
};
pub use typecheck::{TypecheckOptions, TypecheckResult};

use std::path::{Path, PathBuf};

/// Directory holding per-asmdef script DLLs for the given build variant,
/// under the default generator root (`Library/UnitySolutionGenerator`).
///
/// Consumed by external reflection tools (e.g. pspec bake-types) to locate
/// compiled script assemblies after a `typecheck` or `build` run, without
/// hardcoding the on-disk layout. The directory itself isn't created here —
/// callers either invoke `typecheck`/`build` themselves or fail-loud if the
/// path doesn't exist.
pub fn script_dll_dir(
    project_root: impl AsRef<Path>,
    platform: BuildPlatform,
    config: BuildConfig,
) -> PathBuf {
    let s = typecheck::typecheck_output_dir(
        project_root
            .as_ref()
            .to_str()
            .expect("project_root must be UTF-8"),
        DEFAULT_GENERATOR_ROOT,
        platform,
        config,
    );
    PathBuf::from(s)
}

/// High-level convenience: parse string args + run the full generate pipeline.
///
/// Used by the CLI and by external FFI hosts (see meow-tower's BoxcatBridge).
/// `platform` accepts `ios|android|osx|windows`; `config` accepts `prod|dev|editor`.
/// `extra_refs` is a comma-separated list of DLL paths (see `DllRef::parse_list`).
/// Loads or scans the lockfile as needed.
pub fn generate(
    project_root: &str,
    platform: &str,
    config: &str,
    output_dir: Option<&str>,
    extra_refs: Option<&str>,
) -> Result<()> {
    let build_platform = BuildPlatform::parse(platform).ok_or_else(|| {
        GeneratorError::Other(format!(
            "Unknown platform '{platform}'. Use 'ios', 'android', 'osx', or 'windows'."
        ))
    })?;
    let build_config = BuildConfig::parse(config).ok_or_else(|| {
        GeneratorError::Other(format!(
            "Unknown config '{config}'. Use 'prod', 'dev', or 'editor'."
        ))
    })?;
    let resolved = resolve_project_root(project_root);
    let extra_refs_vec: Vec<DllRef> = extra_refs.map(DllRef::parse_list).unwrap_or_default();
    let options = GenerateOptions::new(resolved.clone(), build_platform)
        .with_build_config(build_config)
        .with_output_dir(output_dir)
        .with_extra_refs(extra_refs_vec);
    let lockfile = lockfile::scan_and_write(&resolved, DEFAULT_GENERATOR_ROOT)?;
    let scan = project_scanner::scan(&resolved, DEFAULT_GENERATOR_ROOT)?;
    solution_generator::generate(&options, &lockfile, scan)?;
    Ok(())
}
