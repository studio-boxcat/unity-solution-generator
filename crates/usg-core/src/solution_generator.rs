//! Renders `.csproj` / `.sln` / `Directory.Build.props` for a platform+config variant.
//! Single entry point driven by `csproj.lock`.

use std::collections::{HashMap, HashSet};

use rayon::prelude::*;

use crate::build_variant::{BuildConfig, BuildPlatform};
use crate::csproj_render::{self, ProjectInfo};
use crate::error::Result;
use crate::io::{create_dir_all, write_file_if_changed};
use crate::lockfile::{DllRef, Lockfile};
use crate::paths::{DEFAULT_GENERATOR_ROOT, join_path, resolve_real_path};
use crate::project_scanner::{self, ProjectCategory, ProjectName, ScanResult};
use crate::xml::deterministic_guid;

#[derive(Debug, Clone)]
pub struct GenerateOptions {
    pub project_root: String,
    pub generator_root: String,
    /// `None` → default variant subdir; `Some(".")` → project root; `Some("rel/path")` otherwise.
    pub output_dir: Option<String>,
    pub extra_refs: Vec<DllRef>,
    pub platform: BuildPlatform,
    pub build_config: BuildConfig,
}

impl GenerateOptions {
    pub fn new(project_root: impl Into<String>, platform: BuildPlatform) -> Self {
        Self {
            project_root: project_root.into(),
            generator_root: DEFAULT_GENERATOR_ROOT.to_string(),
            output_dir: None,
            extra_refs: Vec::new(),
            platform,
            build_config: BuildConfig::Prod,
        }
    }

    pub fn with_generator_root(mut self, generator_root: impl Into<String>) -> Self {
        self.generator_root = generator_root.into();
        self
    }

    pub fn with_build_config(mut self, build_config: BuildConfig) -> Self {
        self.build_config = build_config;
        self
    }

    pub fn with_output_dir(mut self, output_dir: Option<&str>) -> Self {
        self.output_dir = output_dir.map(|d| {
            let trimmed = d.trim_end_matches('/');
            if trimmed.is_empty() {
                ".".to_string()
            } else {
                trimmed.to_string()
            }
        });
        self
    }

    pub fn with_extra_refs(mut self, extra_refs: Vec<DllRef>) -> Self {
        self.extra_refs = extra_refs;
        self
    }
}

#[derive(Debug, Clone)]
pub struct GenerateResult {
    pub warnings: Vec<String>,
    pub variant_csprojs: Vec<String>,
    pub variant_sln_path: String,
}

struct GenerationContext {
    project_root: String,
    scan: ScanResult,
    project_by_name: HashMap<ProjectName, ProjectInfo>,
    patterns_by_project: HashMap<ProjectName, Vec<String>>,
    included_projects: Vec<ProjectInfo>,
    non_runtime_names: HashSet<ProjectName>,
    variant_dir: String,
    result_prefix: String,
    warnings: Vec<String>,
}

fn build_context(
    options: &GenerateOptions,
    project_root: String,
    projects: Vec<ProjectInfo>,
    scan: ScanResult,
) -> GenerationContext {
    let project_by_name: HashMap<ProjectName, ProjectInfo> = projects
        .iter()
        .map(|p| (p.name.clone(), p.clone()))
        .collect();

    let mut warnings: Vec<String> = Vec::new();
    if !scan.unresolved_dirs.is_empty() {
        warnings.push(format!(
            "Unresolved source directories: {}",
            scan.unresolved_dirs.len()
        ));
    }
    let depth: usize = if let Some(out) = options.output_dir.as_deref() {
        if out == "." {
            0
        } else {
            out.split('/').filter(|s| !s.is_empty()).count()
        }
    } else {
        // generator_root path components + 1 for the variant subdir
        options
            .generator_root
            .split('/')
            .filter(|s| !s.is_empty())
            .count()
            + 1
    };
    let variant_prefix: String = "../".repeat(depth);

    let mut patterns_by_project: HashMap<ProjectName, Vec<String>> = HashMap::new();
    for project in &projects {
        let mut dirs: Vec<String> = scan
            .dirs_by_project
            .get(&project.name)
            .cloned()
            .unwrap_or_default();
        dirs.sort();
        let pats: Vec<String> = dirs
            .iter()
            .map(|d| {
                if d.is_empty() {
                    format!("{}*.cs", variant_prefix)
                } else {
                    format!("{}{}/*.cs", variant_prefix, d)
                }
            })
            .collect();
        patterns_by_project.insert(project.name.clone(), pats);
    }

    let is_editor = options.build_config == BuildConfig::Editor;
    let mut included_projects: Vec<ProjectInfo> = Vec::new();
    let mut non_runtime_names: HashSet<ProjectName> = HashSet::new();
    if is_editor {
        included_projects = projects.clone();
    } else {
        for project in &projects {
            let (category, matches_platform) = match scan.asm_def_by_name.get(&project.name) {
                Some(asm) => {
                    let platforms: Vec<&str> = asm
                        .include_platforms
                        .iter()
                        .filter(|p| p.as_str() != "Editor")
                        .map(String::as_str)
                        .collect();
                    let matches =
                        platforms.is_empty() || platforms.contains(&options.platform.unity_platform_name());
                    (asm.category, matches)
                }
                None => (ProjectCategory::Runtime, true),
            };
            if category == ProjectCategory::Runtime && matches_platform {
                included_projects.push(project.clone());
            } else {
                non_runtime_names.insert(project.name.clone());
            }
        }
    }

    let config_str = format!("{}-{}", options.platform, options.build_config);
    let (variant_dir, result_prefix) = if let Some(out) = options.output_dir.as_deref() {
        if out == "." {
            (project_root.clone(), String::new())
        } else {
            let dir = join_path(&project_root, out);
            create_dir_all(&dir);
            (dir, format!("{}/", out))
        }
    } else {
        let generator_dir = join_path(&project_root, &options.generator_root);
        let dir = join_path(&generator_dir, &config_str);
        create_dir_all(&dir);
        (dir, format!("{}/{}/", options.generator_root, config_str))
    };

    GenerationContext {
        project_root,
        scan,
        project_by_name,
        patterns_by_project,
        included_projects,
        non_runtime_names,
        variant_dir,
        result_prefix,
        warnings,
    }
}

fn write_variant<R>(ctx: &GenerationContext, render_csproj: R) -> Result<GenerateResult>
where
    R: Fn(&ProjectInfo, &str, &str) -> String + Sync,
{
    let _span = tracing::info_span!("generate.write_variant", n = ctx.included_projects.len()).entered();
    let included = &ctx.included_projects;
    let patterns = &ctx.patterns_by_project;
    let exclude_names = &ctx.non_runtime_names;
    let asm_def_by_name = &ctx.scan.asm_def_by_name;
    let project_by_name = &ctx.project_by_name;
    let variant_dir = &ctx.variant_dir;

    // Parallel csproj write — replaces Swift's DispatchQueue.concurrentPerform.
    let results: Vec<Result<()>> = included
        .par_iter()
        .map(|project| -> Result<()> {
            let empty: Vec<String> = Vec::new();
            let pats = patterns.get(&project.name).unwrap_or(&empty);
            let source_block = csproj_render::render_compile_patterns(pats);
            let reference_block = csproj_render::render_project_references(
                project,
                asm_def_by_name,
                project_by_name,
                exclude_names,
            );
            let rendered = render_csproj(project, &source_block, &reference_block);
            write_file_if_changed(&join_path(variant_dir, &project.csproj_path()), &rendered)?;
            Ok(())
        })
        .collect();
    for r in results {
        r?;
    }

    let project_name = ctx
        .project_root
        .rsplit('/')
        .find(|s| !s.is_empty())
        .unwrap_or("Project")
        .to_string();
    let sln_name = format!("{}.sln", project_name);
    write_file_if_changed(
        &join_path(variant_dir, &sln_name),
        &csproj_render::render_sln(&ctx.included_projects),
    )?;

    let mut variant_csprojs: Vec<String> = ctx
        .included_projects
        .iter()
        .map(|p| format!("{}{}", ctx.result_prefix, p.csproj_path()))
        .collect();
    variant_csprojs.sort();

    Ok(GenerateResult {
        warnings: ctx.warnings.clone(),
        variant_csprojs,
        variant_sln_path: format!("{}{}", ctx.result_prefix, sln_name),
    })
}

/// Convenience wrapper for the test suite and any caller that hasn't already
/// loaded a `ScanResult`. Does the scan internally; the CLI driver and
/// `unity_solution_generator::generate(...)` call [`generate`] directly to
/// avoid duplicate scans.
pub fn generate_from_lockfile(
    options: &GenerateOptions,
    lockfile: &Lockfile,
) -> Result<GenerateResult> {
    let project_root = resolve_real_path(&options.project_root);
    let scan = project_scanner::scan(&project_root, &options.generator_root)?;
    generate(options, lockfile, scan)
}

/// Render `.csproj`/`.sln`/`Directory.Build.props` for one platform+config
/// variant. Callers pre-compute the scan (via `project_scanner::scan`) and
/// lockfile (via `lockfile::scan_and_write`) so a single CLI invocation hits
/// the project tree exactly once.
pub fn generate(
    options: &GenerateOptions,
    lockfile: &Lockfile,
    scan: ScanResult,
) -> Result<GenerateResult> {
    let _span = tracing::info_span!("generate.from_lockfile").entered();
    let project_root = resolve_real_path(&options.project_root);

    let mut projects: Vec<ProjectInfo> = Vec::new();
    let mut all_names: HashSet<ProjectName> = HashSet::new();
    for name in scan.asm_def_by_name.keys() {
        if !scan.dirs_by_project.contains_key(name) {
            continue;
        }
        projects.push(ProjectInfo {
            name: name.clone(),
            guid: deterministic_guid(name.as_str()),
        });
        all_names.insert(name.clone());
    }
    for name in scan.dirs_by_project.keys() {
        if !all_names.contains(name) {
            projects.push(ProjectInfo {
                name: name.clone(),
                guid: deterministic_guid(name.as_str()),
            });
        }
    }
    projects.sort_by(|a, b| a.name.cmp(&b.name));

    let ctx = build_context(options, project_root.clone(), projects, scan);

    let mut static_defines: Vec<String> = lockfile.defines.clone();
    static_defines.extend(lockfile.defines_scripting.iter().cloned());
    write_file_if_changed(
        &join_path(&ctx.variant_dir, "Directory.Build.props"),
        &csproj_render::render_directory_build_props(
            &ctx.project_root,
            Some(&lockfile.unity_path),
            Some(&crate::paths::usg_cache_dir(&lockfile.unity_version)),
            options.platform,
            options.build_config,
            &static_defines,
        ),
    )?;

    let refs_block = csproj_render::collect_references_block(
        lockfile,
        options.platform,
        options.build_config == BuildConfig::Editor,
        &options.extra_refs,
    );
    let analyzer_block = csproj_render::render_analyzers(&lockfile.analyzers);
    let lang_version = lockfile.lang_version.clone();
    let asm_def_by_name = ctx.scan.asm_def_by_name.clone();

    let result = write_variant(&ctx, |project, source_block, reference_block| {
        let allow_unsafe = asm_def_by_name
            .get(&project.name)
            .map(|a| a.allow_unsafe_code)
            .unwrap_or(false);
        let mut out = csproj_render::render_csproj_header(
            project.name.as_str(),
            &project.guid,
            &lang_version,
            allow_unsafe,
        );
        out.push_str(&analyzer_block);
        out.push_str(&refs_block);
        out.push_str("  <ItemGroup>\n");
        if !source_block.is_empty() {
            out.push_str(source_block);
            out.push('\n');
        }
        if !reference_block.is_empty() {
            out.push_str(reference_block);
            out.push('\n');
        }
        out.push_str("  </ItemGroup>\n");
        out.push_str("  <Import Project=\"$(MSBuildToolsPath)\\Microsoft.CSharp.targets\" />\n");
        out.push_str("</Project>\n");
        out
    })?;

    Ok(result)
}
