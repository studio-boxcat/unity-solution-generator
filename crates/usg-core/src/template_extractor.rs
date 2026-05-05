//! Reads Unity-generated `.sln`/`.csproj` files and converts them into template
//! fragments. Dynamic defines are stripped (moved to `Directory.Build.props` at
//! generation time), absolute paths become `$(ProjectRoot)`, and the closing
//! `</Project>` tag is removed so the generator can append source/reference
//! entries directly.

use crate::error::{GeneratorError, Result};
use crate::io::{create_dir_all, file_exists, list_directory, read_file, write_file_if_changed};
use crate::json::trim_ws;
use crate::paths::{DEFAULT_GENERATOR_ROOT, join_path, parent_directory, resolve_real_path};
use crate::solution_generator::all_dynamic_defines;

#[derive(Debug, Clone)]
pub struct ExtractTemplatesOptions {
    pub project_root: String,
    pub generator_root: String,
}

impl ExtractTemplatesOptions {
    pub fn new(project_root: impl Into<String>) -> Self {
        Self {
            project_root: project_root.into(),
            generator_root: DEFAULT_GENERATOR_ROOT.to_string(),
        }
    }
}

pub struct TemplateExtractor;

impl TemplateExtractor {
    pub fn extract(options: &ExtractTemplatesOptions) -> Result<Vec<String>> {
        let project_root = resolve_real_path(&options.project_root);
        let generator_root = options.generator_root.clone();

        let sln_path = find_solution_file(&project_root)?;
        let sln_content = read_file(&sln_path)?;
        let entries = parse_solution_projects(&sln_content);
        if entries.is_empty() {
            return Err(GeneratorError::NoProjectsInSolution(sln_path));
        }

        let mut updated = Vec::new();
        for csproj_name in entries {
            let csproj_path = join_path(&project_root, &csproj_name);
            if !file_exists(&csproj_path) {
                continue;
            }
            let content = read_file(&csproj_path)?;
            let template = templatize_csproj(&content, &project_root);
            let template_rel = format!("{}/templates/{}.template", generator_root, csproj_name);
            let template_path = join_path(&project_root, &template_rel);
            create_dir_all(parent_directory(&template_path));
            if write_file_if_changed(&template_path, &template)? {
                updated.push(template_rel);
            }
        }
        updated.sort();
        Ok(updated)
    }
}

fn find_solution_file(project_root: &str) -> Result<String> {
    let entries = list_directory(project_root);
    let sln = entries.into_iter().find(|n| n.ends_with(".sln"));
    match sln {
        Some(s) => Ok(join_path(project_root, &s)),
        None => Err(GeneratorError::NoSolutionFound(project_root.to_string())),
    }
}

/// Returns the .csproj filenames referenced in the .sln content.
fn parse_solution_projects(content: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in content.split('\n') {
        if !line.starts_with("Project(\"") {
            continue;
        }
        let mut quoted: Vec<String> = Vec::new();
        let mut in_quote = false;
        let mut current = String::new();
        for ch in line.chars() {
            if ch == '"' {
                if in_quote {
                    quoted.push(std::mem::take(&mut current));
                }
                in_quote = !in_quote;
            } else if in_quote {
                current.push(ch);
            }
        }
        if quoted.len() < 3 {
            continue;
        }
        let path = &quoted[2];
        if path.ends_with(".csproj") && !path.contains('/') {
            out.push(path.clone());
        }
    }
    out
}

fn templatize_csproj(content: &str, project_root: &str) -> String {
    let dynamic = all_dynamic_defines();
    let mut lines: Vec<String> = Vec::new();
    let mut in_project_reference = false;
    let mut in_comment = false;

    for raw in content.split('\n') {
        let line = raw.replace(project_root, "$(ProjectRoot)");

        if in_comment {
            if line.contains("-->") {
                in_comment = false;
            }
            continue;
        }
        if line.contains("<!--") {
            if !line.contains("-->") {
                in_comment = true;
            }
            continue;
        }

        if line.contains("<None Include=\"") || line.contains("<Compile Include=\"") {
            continue;
        }

        if in_project_reference {
            if line.contains("</ProjectReference>") {
                in_project_reference = false;
            }
            continue;
        }
        if line.contains("<ProjectReference Include=\"") {
            in_project_reference = true;
            continue;
        }

        if trim_ws(&line) == "</Project>" {
            continue;
        }

        if line.contains("<DefineConstants>") && line.contains("</DefineConstants>") {
            lines.push(strip_dynamic_defines(&line, &dynamic));
            continue;
        }

        lines.push(line);
    }

    // Remove empty <ItemGroup></ItemGroup> pairs.
    let mut cleaned: Vec<String> = Vec::with_capacity(lines.len());
    let mut i = 0;
    while i < lines.len() {
        let trimmed = trim_ws(&lines[i]);
        if trimmed == "<ItemGroup>"
            && i + 1 < lines.len()
            && trim_ws(&lines[i + 1]) == "</ItemGroup>"
        {
            i += 2;
            continue;
        }
        cleaned.push(lines[i].clone());
        i += 1;
    }

    while cleaned
        .last()
        .map(|l| l.chars().all(|c| c == ' ' || c == '\t' || c == '\n') || l.is_empty())
        .unwrap_or(false)
    {
        cleaned.pop();
    }

    let mut out = cleaned.join("\n");
    out.push('\n');
    out
}

fn strip_dynamic_defines(line: &str, dynamic: &std::collections::HashSet<String>) -> String {
    let Some(open) = line.find("<DefineConstants>") else {
        return line.to_string();
    };
    let Some(close) = line.find("</DefineConstants>") else {
        return line.to_string();
    };
    let prefix = &line[..open];
    let value = &line[open + "<DefineConstants>".len()..close];

    let static_defines: Vec<&str> = value
        .split(';')
        .filter(|d| !d.is_empty() && !dynamic.contains(*d))
        .collect();

    let new_value = if static_defines.is_empty() {
        "$(DefineConstants)".to_string()
    } else {
        format!("$(DefineConstants);{}", static_defines.join(";"))
    };
    format!("{}<DefineConstants>{}</DefineConstants>", prefix, new_value)
}
