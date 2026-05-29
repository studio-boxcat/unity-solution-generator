//! Pure XML string-builders for `.csproj` / `.sln` / `Directory.Build.props`,
//! factored out of [`crate::solution_generator`]. These take a resolved render
//! model (`ProjectInfo`, the lockfile, the scan) and emit text; the ownership
//! walk + file I/O that *drives* them lives in `solution_generator.rs`.

use std::collections::{HashMap, HashSet};

use crate::defines::{DEBUG_DEFINES, EDITOR_DEFINES_BASE, editor_host_define};
use crate::lockfile::{DllRef, Lockfile};
use crate::project_scanner::{AsmDefRecord, ProjectName};
use crate::build_variant::{BuildConfig, BuildPlatform};
use crate::xml::xml_escape;

/// One row of the generated solution: an assembly name + its deterministic
/// project GUID. The `.csproj` lives flat next to the `.sln`, so the path is
/// just `<name>.csproj`.
#[derive(Debug, Clone)]
pub(crate) struct ProjectInfo {
    pub(crate) name: ProjectName,
    pub(crate) guid: String,
}

impl ProjectInfo {
    pub(crate) fn csproj_path(&self) -> String {
        format!("{}.csproj", self.name)
    }
}

pub(crate) fn render_compile_patterns(patterns: &[String]) -> String {
    patterns
        .iter()
        .map(|p| format!("    <Compile Include=\"{}\" />", xml_escape(p)))
        .collect::<Vec<_>>()
        .join("\n")
}

pub(crate) fn render_project_references(
    project: &ProjectInfo,
    asm_def_by_name: &HashMap<ProjectName, AsmDefRecord>,
    project_by_name: &HashMap<ProjectName, ProjectInfo>,
    exclude_names: &HashSet<ProjectName>,
) -> String {
    let Some(asm_def) = asm_def_by_name.get(&project.name) else {
        return String::new();
    };
    let mut seen: HashSet<ProjectName> = HashSet::new();
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
            xml_escape(ref_proj.name.as_str()),
        ));
    }
    blocks.join("\n")
}

pub(crate) fn render_csproj_header(
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
    <ProjectTypeGuids>{{E097FAD1-6243-4DAD-9C02-E9B9EFC3FFC1}};{CSHARP_PROJECT_TYPE_GUID}</ProjectTypeGuids>\n\
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

pub(crate) fn render_analyzers(analyzers: &[String]) -> String {
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

pub(crate) fn collect_references_block(
    lockfile: &Lockfile,
    platform: BuildPlatform,
    is_editor: bool,
    extra_refs: &[DllRef],
) -> String {
    let mut refs: Vec<&DllRef> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for cat in platform.ref_categories(is_editor) {
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

pub(crate) fn render_directory_build_props(
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

pub(crate) fn render_sln(projects: &[ProjectInfo]) -> String {
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

    #[test]
    fn render_directory_build_props_unified() {
        let with_unity = render_directory_build_props(
            "/project",
            Some("/unity"),
            Some("/cache/2024"),
            BuildPlatform::Ios,
            BuildConfig::Editor,
            &["CUSTOM".to_string()],
        );
        assert!(with_unity.contains("<UnityPath>/unity</UnityPath>"));
        assert!(with_unity.contains("<UsgCache>/cache/2024</UsgCache>"));
        assert!(with_unity.contains("CUSTOM"));
        assert!(with_unity.contains("UNITY_IOS"));
        assert!(with_unity.contains("UNITY_EDITOR"));

        let without = render_directory_build_props(
            "/project",
            None,
            None,
            BuildPlatform::Android,
            BuildConfig::Prod,
            &[],
        );
        assert!(!without.contains("<UnityPath>"));
        assert!(!without.contains("<UsgCache>"));
        assert!(without.contains("UNITY_ANDROID"));
        assert!(!without.contains("UNITY_EDITOR"));
    }
}
