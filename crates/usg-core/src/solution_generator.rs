//! Renders `.csproj` / `.sln` / `Directory.Build.props` for a platform+config variant.
//! Single entry point driven by `csproj.lock`.

use std::collections::{HashMap, HashSet};

use rayon::prelude::*;

use crate::error::Result;
use crate::io::{create_dir_all, write_file_if_changed};
use crate::lockfile::{DllRef, Lockfile, RefCategory};
use crate::paths::{DEFAULT_GENERATOR_ROOT, join_path, resolve_real_path};
use crate::project_scanner::{AsmDefRecord, ProjectCategory, ProjectScanner, ScanResult};
use crate::xml::{deterministic_guid, xml_escape};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildPlatform {
    Ios,
    Android,
    Osx,
    Windows,
}

impl BuildPlatform {
    /// All variants in canonical order. Iterating this is the supported way to
    /// walk the variant set; matches in this file are exhaustive so adding a
    /// variant fails to compile until everywhere is updated.
    pub const ALL: &'static [BuildPlatform] = &[
        BuildPlatform::Ios,
        BuildPlatform::Android,
        BuildPlatform::Osx,
        BuildPlatform::Windows,
    ];

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "ios" => BuildPlatform::Ios,
            "android" => BuildPlatform::Android,
            "osx" => BuildPlatform::Osx,
            "windows" => BuildPlatform::Windows,
            _ => return None,
        })
    }

    /// CLI / config string. Round-trip with `parse`.
    /// Equivalent to `Display::fmt`; kept as a pub fn for explicit-API callers.
    pub fn raw(self) -> &'static str {
        match self {
            BuildPlatform::Ios => "ios",
            BuildPlatform::Android => "android",
            BuildPlatform::Osx => "osx",
            BuildPlatform::Windows => "windows",
        }
    }

    /// Unity's `includePlatforms` value. Used for platform-filter matching.
    pub fn unity_platform_name(self) -> &'static str {
        match self {
            BuildPlatform::Ios => "iOS",
            BuildPlatform::Android => "Android",
            BuildPlatform::Osx => "macOSStandalone",
            BuildPlatform::Windows => "WindowsStandalone",
        }
    }

    pub fn platform_defines(self) -> &'static [&'static str] {
        match self {
            BuildPlatform::Ios => &["UNITY_IOS", "UNITY_IPHONE"],
            BuildPlatform::Android => &["UNITY_ANDROID"],
            BuildPlatform::Osx => &["UNITY_STANDALONE", "UNITY_STANDALONE_OSX"],
            BuildPlatform::Windows => &["UNITY_STANDALONE", "UNITY_STANDALONE_WIN"],
        }
    }

    /// PlaybackEngines subdir under the Unity install. `None` for standalone-mac
    /// — that variant uses the shared `PlaybackStandalone` ref category populated
    /// once per install rather than a target-specific subdir.
    pub fn playback_ref_category(self) -> RefCategory {
        match self {
            BuildPlatform::Ios => RefCategory::PlaybackIos,
            BuildPlatform::Android => RefCategory::PlaybackAndroid,
            BuildPlatform::Osx => RefCategory::PlaybackStandalone,
            BuildPlatform::Windows => RefCategory::PlaybackWindows,
        }
    }
}

impl std::fmt::Display for BuildPlatform {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.raw())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildConfig {
    Editor,
    Dev,
    Prod,
}

impl BuildConfig {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "editor" => BuildConfig::Editor,
            "dev" => BuildConfig::Dev,
            "prod" => BuildConfig::Prod,
            _ => return None,
        })
    }

    /// CLI / config string ("editor", "dev", "prod"). Round-trip with `parse`.
    /// Equivalent to `Display::fmt`; kept as a pub fn for explicit-API callers.
    pub fn raw(self) -> &'static str {
        match self {
            BuildConfig::Editor => "editor",
            BuildConfig::Dev => "dev",
            BuildConfig::Prod => "prod",
        }
    }
}

impl std::fmt::Display for BuildConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.raw())
    }
}

/// Editor defines that are platform-target-independent. The host-OS suffix
/// (`UNITY_EDITOR_OSX` / `UNITY_EDITOR_WIN` / `UNITY_EDITOR_LINUX`) is added
/// at render time based on `cfg!(target_os)`.
const EDITOR_DEFINES_BASE: &[&str] = &["UNITY_EDITOR", "UNITY_EDITOR_64"];

pub(crate) fn editor_host_define() -> &'static str {
    if cfg!(target_os = "windows") {
        "UNITY_EDITOR_WIN"
    } else if cfg!(target_os = "linux") {
        "UNITY_EDITOR_LINUX"
    } else {
        "UNITY_EDITOR_OSX"
    }
}

const DEBUG_DEFINES: &[&str] = &["DEBUG", "TRACE", "UNITY_ASSERTIONS"];

#[derive(Debug, Clone)]
pub struct GenerateOptions {
    pub project_root: String,
    pub generator_root: String,
    pub verbose: bool,
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
            verbose: false,
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

    pub fn with_verbose(mut self, verbose: bool) -> Self {
        self.verbose = verbose;
        self
    }
}

#[derive(Debug, Clone)]
pub struct GenerateResult {
    pub warnings: Vec<String>,
    pub variant_csprojs: Vec<String>,
    pub variant_sln_path: String,
}

#[derive(Debug, Clone)]
struct ProjectInfo {
    name: String,
    guid: String,
}

impl ProjectInfo {
    fn csproj_path(&self) -> String {
        format!("{}.csproj", self.name)
    }
}

struct GenerationContext {
    project_root: String,
    scan: ScanResult,
    project_by_name: HashMap<String, ProjectInfo>,
    patterns_by_project: HashMap<String, Vec<String>>,
    included_projects: Vec<ProjectInfo>,
    non_runtime_names: HashSet<String>,
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
    let project_by_name: HashMap<String, ProjectInfo> = projects
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
    if options.verbose {
        for d in scan.unresolved_dirs.iter().take(20) {
            warnings.push(format!("Unresolved: {}/", d));
        }
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

    let mut patterns_by_project: HashMap<String, Vec<String>> = HashMap::new();
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
    let mut non_runtime_names: HashSet<String> = HashSet::new();
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

    let config_str = format!(
        "{}-{}",
        options.platform.raw(),
        options.build_config.raw()
    );
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
            let source_block = render_compile_patterns(pats);
            let reference_block = render_project_references(
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
        &render_sln(&ctx.included_projects),
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

pub struct SolutionGenerator;

impl SolutionGenerator {
    pub fn new() -> Self {
        SolutionGenerator
    }

    pub fn generate_from_lockfile(
        &self,
        options: &GenerateOptions,
        lockfile: &Lockfile,
    ) -> Result<GenerateResult> {
        let _span = tracing::info_span!("generate.from_lockfile").entered();
        let project_root = resolve_real_path(&options.project_root);

        // No separate generate-output fingerprint cache: Watchman-backed
        // ProjectScanner::scan is fast on cache-hit, render is microseconds,
        // and write_file_if_changed skips bytes-identical writes (so a warm
        // generate doesn't touch disk mtimes or rewrite csproj content).
        let scan = ProjectScanner::scan(&project_root, &options.generator_root)?;

        let mut projects: Vec<ProjectInfo> = Vec::new();
        let mut all_names: HashSet<String> = HashSet::new();
        for name in scan.asm_def_by_name.keys() {
            if !scan.dirs_by_project.contains_key(name) {
                continue;
            }
            projects.push(ProjectInfo {
                name: name.clone(),
                guid: deterministic_guid(name),
            });
            all_names.insert(name.clone());
        }
        for name in scan.dirs_by_project.keys() {
            if !all_names.contains(name) {
                projects.push(ProjectInfo {
                    name: name.clone(),
                    guid: deterministic_guid(name),
                });
            }
        }
        projects.sort_by(|a, b| a.name.cmp(&b.name));

        let ctx = build_context(options, project_root.clone(), projects, scan);

        let mut static_defines: Vec<String> = lockfile.defines.clone();
        static_defines.extend(lockfile.defines_scripting.iter().cloned());
        write_file_if_changed(
            &join_path(&ctx.variant_dir, "Directory.Build.props"),
            &render_directory_build_props(
                &ctx.project_root,
                Some(&lockfile.unity_path),
                Some(&crate::paths::usg_cache_dir(&lockfile.unity_version)),
                options.platform,
                options.build_config,
                &static_defines,
            ),
        )?;

        let refs_block = collect_references_block(
            lockfile,
            options.platform,
            options.build_config == BuildConfig::Editor,
            &options.extra_refs,
        );
        let analyzer_block = render_analyzers(&lockfile.analyzers);
        let lang_version = lockfile.lang_version.clone();
        let asm_def_by_name = ctx.scan.asm_def_by_name.clone();

        let result = write_variant(&ctx, |project, source_block, reference_block| {
            let allow_unsafe = asm_def_by_name
                .get(&project.name)
                .map(|a| a.allow_unsafe_code)
                .unwrap_or(false);
            let mut out = render_csproj_header(
                &project.name,
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

}

impl Default for SolutionGenerator {
    fn default() -> Self {
        Self::new()
    }
}

// ── rendering ─────────────────────────────────────────────────────────────

fn render_compile_patterns(patterns: &[String]) -> String {
    patterns
        .iter()
        .map(|p| format!("    <Compile Include=\"{}\" />", xml_escape(p)))
        .collect::<Vec<_>>()
        .join("\n")
}

fn render_project_references(
    project: &ProjectInfo,
    asm_def_by_name: &HashMap<String, AsmDefRecord>,
    project_by_name: &HashMap<String, ProjectInfo>,
    exclude_names: &HashSet<String>,
) -> String {
    let Some(asm_def) = asm_def_by_name.get(&project.name) else {
        return String::new();
    };
    let mut seen: HashSet<String> = HashSet::new();
    let mut blocks: Vec<String> = Vec::new();
    for reference in &asm_def.references {
        if exclude_names.contains(reference) {
            continue;
        }
        let Some(ref_proj) = project_by_name.get(reference) else {
            continue;
        };
        if !seen.insert(reference.clone()) {
            continue;
        }
        blocks.push(format!(
            "    <ProjectReference Include=\"{}\">\n      <Project>{}</Project>\n      <Name>{}</Name>\n    </ProjectReference>",
            xml_escape(&ref_proj.csproj_path()),
            ref_proj.guid,
            xml_escape(&ref_proj.name),
        ));
    }
    blocks.join("\n")
}

fn render_csproj_header(
    project_name: &str,
    project_guid: &str,
    lang_version: &str,
    allow_unsafe_blocks: bool,
) -> String {
    let unsafe_str = if allow_unsafe_blocks { "True" } else { "False" };
    format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n\
<Project ToolsVersion=\"4.0\" DefaultTargets=\"Build\" xmlns=\"http://schemas.microsoft.com/developer/msbuild/2003\">\n\
  <PropertyGroup>\n\
    <LangVersion>{lang_version}</LangVersion>\n\
    <_TargetFrameworkDirectories>non_empty_path_generated_by_unity.rider.package</_TargetFrameworkDirectories>\n\
    <_FullFrameworkReferenceAssemblyPaths>non_empty_path_generated_by_unity.rider.package</_FullFrameworkReferenceAssemblyPaths>\n\
    <DisableHandlePackageFileConflicts>true</DisableHandlePackageFileConflicts>\n\
  </PropertyGroup>\n\
  <PropertyGroup>\n\
    <Configuration Condition=\" '$(Configuration)' == '' \">Debug</Configuration>\n\
    <Platform Condition=\" '$(Platform)' == '' \">AnyCPU</Platform>\n\
    <ProductVersion>10.0.20506</ProductVersion>\n\
    <SchemaVersion>2.0</SchemaVersion>\n\
    <RootNamespace></RootNamespace>\n\
    <ProjectGuid>{project_guid}</ProjectGuid>\n\
    <ProjectTypeGuids>{{E097FAD1-6243-4DAD-9C02-E9B9EFC3FFC1}};{{FAE04EC0-301F-11D3-BF4B-00C04F79EFBC}}</ProjectTypeGuids>\n\
    <OutputType>Library</OutputType>\n\
    <AppDesignerFolder>Properties</AppDesignerFolder>\n\
    <AssemblyName>{project_name}</AssemblyName>\n\
    <TargetFrameworkVersion>v4.7.1</TargetFrameworkVersion>\n\
    <FileAlignment>512</FileAlignment>\n\
    <BaseDirectory>.</BaseDirectory>\n\
  </PropertyGroup>\n\
  <PropertyGroup Condition=\" '$(Configuration)|$(Platform)' == 'Debug|AnyCPU' \">\n\
    <DebugSymbols>true</DebugSymbols>\n\
    <DebugType>full</DebugType>\n\
    <Optimize>false</Optimize>\n\
    <OutputPath>Temp\\Bin\\Debug\\{project_name}\\</OutputPath>\n\
    <DefineConstants>$(DefineConstants)</DefineConstants>\n\
    <ErrorReport>prompt</ErrorReport>\n\
    <WarningLevel>4</WarningLevel>\n\
    <NoWarn>0169,0649,8524,8597,8600,8601,8602,8603,8604,8605,8607,8608,8609,8610,8611,8612,8613,8614,8615,8616,8617,8618,8619,8620,8621,8622,8624,8625,8629,8631,8632,8633,8634,8643,8644,8645,8653,8654,8655,8667,8669,8670</NoWarn>\n\
    <AllowUnsafeBlocks>{unsafe_str}</AllowUnsafeBlocks>\n\
    <TreatWarningsAsErrors>False</TreatWarningsAsErrors>\n\
  </PropertyGroup>\n\
  <PropertyGroup>\n\
    <NoConfig>true</NoConfig>\n\
    <NoStdLib>true</NoStdLib>\n\
    <AddAdditionalExplicitAssemblyReferences>false</AddAdditionalExplicitAssemblyReferences>\n\
    <ImplicitlyExpandNETStandardFacades>false</ImplicitlyExpandNETStandardFacades>\n\
    <ImplicitlyExpandDesignTimeFacades>false</ImplicitlyExpandDesignTimeFacades>\n\
  </PropertyGroup>\n",
    )
}

fn render_analyzers(analyzers: &[String]) -> String {
    if analyzers.is_empty() {
        return String::new();
    }
    let mut s = String::from("  <ItemGroup>\n");
    for path in analyzers {
        s.push_str(&format!("    <Analyzer Include=\"{}\" />\n", xml_escape(path)));
    }
    s.push_str("  </ItemGroup>\n");
    s
}

fn collect_references_block(
    lockfile: &Lockfile,
    platform: BuildPlatform,
    is_editor: bool,
    extra_refs: &[DllRef],
) -> String {
    let mut refs: Vec<&DllRef> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut cats: Vec<RefCategory> = vec![RefCategory::Engine];
    if is_editor {
        cats.push(RefCategory::Editor);
    }
    // PlaybackStandalone holds engine DLLs shared across the desktop targets
    // (and historically referenced for non-desktop too). Target-specific
    // additions come from `playback_ref_category()`; if the target IS standalone
    // already we'd double-add, so de-dup via the seen-set below.
    cats.push(RefCategory::PlaybackStandalone);
    let target_cat = platform.playback_ref_category();
    if target_cat != RefCategory::PlaybackStandalone {
        cats.push(target_cat);
    }
    cats.push(RefCategory::Project);
    cats.push(RefCategory::Netstandard);
    for cat in cats {
        for r in lockfile.refs_for(cat) {
            if seen.insert(r.name.clone()) {
                refs.push(r);
            }
        }
    }
    for r in extra_refs {
        if seen.insert(r.name.clone()) {
            refs.push(r);
        }
    }

    if refs.is_empty() {
        return String::new();
    }
    let mut s = String::from("  <ItemGroup>\n");
    for r in &refs {
        s.push_str(&format!("    <Reference Include=\"{}\">\n", xml_escape(&r.name)));
        s.push_str(&format!("      <HintPath>{}</HintPath>\n", xml_escape(&r.path)));
        s.push_str("    </Reference>\n");
    }
    s.push_str("  </ItemGroup>\n");
    s
}

pub fn render_directory_build_props(
    project_root: &str,
    unity_path: Option<&str>,
    usg_cache: Option<&str>,
    platform: BuildPlatform,
    build_config: BuildConfig,
    static_defines: &[String],
) -> String {
    let mut dynamic: Vec<&str> = platform.platform_defines().to_vec();
    if build_config == BuildConfig::Editor {
        dynamic.extend_from_slice(EDITOR_DEFINES_BASE);
        dynamic.push(editor_host_define());
    }
    if matches!(build_config, BuildConfig::Editor | BuildConfig::Dev) {
        dynamic.extend_from_slice(DEBUG_DEFINES);
    }
    let mut all: Vec<String> = static_defines.to_vec();
    all.extend(dynamic.iter().map(|s| s.to_string()));

    let mut props = format!(
        "<Project>\n<PropertyGroup>\n<ProjectRoot>{}</ProjectRoot>\n",
        project_root
    );
    if let Some(up) = unity_path {
        props.push_str(&format!("<UnityPath>{}</UnityPath>\n", up));
    }
    if let Some(uc) = usg_cache {
        props.push_str(&format!("<UsgCache>{}</UsgCache>\n", uc));
    }
    props.push_str(&format!(
        "<DefineConstants>$(DefineConstants);{}</DefineConstants>\n</PropertyGroup>\n</Project>\n",
        all.join(";")
    ));
    props
}

const CSHARP_PROJECT_TYPE_GUID: &str = "{FAE04EC0-301F-11D3-BF4B-00C04F79EFBC}";

fn render_sln(projects: &[ProjectInfo]) -> String {
    let mut lines: Vec<String> = vec![
        "Microsoft Visual Studio Solution File, Format Version 11.00".into(),
        "# Visual Studio 2010".into(),
    ];
    for p in projects {
        lines.push(format!(
            "Project(\"{}\") = \"{}\", \"{}\", \"{}\"",
            CSHARP_PROJECT_TYPE_GUID,
            p.name,
            p.csproj_path(),
            p.guid
        ));
        lines.push("EndProject".into());
    }
    lines.push("Global".into());
    lines.push("\tGlobalSection(SolutionConfigurationPlatforms) = preSolution".into());
    lines.push("\t\tDebug|Any CPU = Debug|Any CPU".into());
    lines.push("\tEndGlobalSection".into());
    lines.push("\tGlobalSection(ProjectConfigurationPlatforms) = postSolution".into());
    for p in projects {
        lines.push(format!("\t\t{}.Debug|Any CPU.ActiveCfg = Debug|Any CPU", p.guid));
        lines.push(format!("\t\t{}.Debug|Any CPU.Build.0 = Debug|Any CPU", p.guid));
    }
    lines.push("\tEndGlobalSection".into());
    lines.push("EndGlobal".into());
    lines.push(String::new());
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_round_trips() {
        assert_eq!(BuildPlatform::parse("windows"), Some(BuildPlatform::Windows));
        assert_eq!(BuildPlatform::Windows.raw(), "windows");
        assert_eq!(BuildPlatform::Windows.to_string(), "windows");
    }

    #[test]
    fn windows_platform_metadata() {
        let p = BuildPlatform::Windows;
        assert_eq!(p.unity_platform_name(), "WindowsStandalone");
        assert_eq!(p.platform_defines(), &["UNITY_STANDALONE", "UNITY_STANDALONE_WIN"]);
        assert_eq!(p.playback_ref_category(), RefCategory::PlaybackWindows);
    }

    #[test]
    fn playback_ref_category_per_target() {
        assert_eq!(BuildPlatform::Ios.playback_ref_category(), RefCategory::PlaybackIos);
        assert_eq!(BuildPlatform::Android.playback_ref_category(), RefCategory::PlaybackAndroid);
        assert_eq!(BuildPlatform::Osx.playback_ref_category(), RefCategory::PlaybackStandalone);
        assert_eq!(BuildPlatform::Windows.playback_ref_category(), RefCategory::PlaybackWindows);
    }

    #[test]
    fn all_covers_every_variant() {
        // If a new variant is added, `playback_ref_category`'s exhaustive match
        // forces it to be addressed and this length check forces ALL to be
        // updated. (The match alone won't fire — ALL is a const array literal.)
        assert_eq!(BuildPlatform::ALL.len(), 4);
    }

    #[test]
    fn editor_host_define_matches_target_os() {
        // On the running host, the define must end with the OS suffix that
        // the build machine has — that's the only one consumers can sensibly
        // wire `#if UNITY_EDITOR_*` against.
        let d = editor_host_define();
        let host_suffix = if cfg!(target_os = "windows") {
            "WIN"
        } else if cfg!(target_os = "linux") {
            "LINUX"
        } else {
            "OSX"
        };
        assert!(d.ends_with(host_suffix), "got {d}, expected suffix {host_suffix}");
    }

    #[test]
    fn windows_target_editor_emits_host_suffix() {
        let props = render_directory_build_props(
            "/tmp/proj",
            None,
            None,
            BuildPlatform::Windows,
            BuildConfig::Editor,
            &[],
        );
        assert!(props.contains("UNITY_STANDALONE_WIN"));
        // The editor-host suffix is whichever OS is running this test.
        let suffix = editor_host_define();
        assert!(props.contains(suffix), "missing host suffix {suffix} in {props}");
    }
}
