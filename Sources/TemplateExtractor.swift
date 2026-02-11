import Foundation

struct ExtractTemplatesOptions: Sendable {
    let projectRoot: URL
    let templateRoot: String

    init(projectRoot: URL, templateRoot: String = "Library/UnitySolutionGenerator") {
        self.projectRoot = projectRoot
        self.templateRoot = templateRoot
    }
}

/// Reads Unity-generated .sln/.csproj files and converts them into templates
/// with placeholders ({{SOURCE_FOLDERS}}, {{PROJECT_REFERENCES}}, etc.) that
/// the generator can later fill in from the filesystem scan.
struct TemplateExtractor {
    static func extract(options: ExtractTemplatesOptions) throws -> [String] {
        let fileManager = FileManager.default
        let projectRoot = options.projectRoot.standardizedFileURL
        let templateRoot = options.templateRoot

        // Find .sln and parse projects.
        let slnURL = try findSolutionFile(projectRoot: projectRoot, fileManager: fileManager)
        let slnContent = try String(contentsOf: slnURL, encoding: .utf8)
        let slnEntries = parseSolutionProjects(slnContent)
        guard let firstEntry = slnEntries.first else {
            throw GeneratorError.noProjectsInSolution(slnURL)
        }

        let projectRootPath = projectRoot.path

        var updatedFiles: [String] = []

        // Refresh csproj templates.
        for entry in slnEntries {
            let csprojURL = projectRoot.appendingPathComponent(entry.csprojPath)
            guard fileManager.fileExists(atPath: csprojURL.path) else {
                continue
            }

            let content = try String(contentsOf: csprojURL, encoding: .utf8)
            let template = templatizeCsproj(content, projectRoot: projectRootPath)

            let templatePath = "\(templateRoot)/csproj/\(entry.csprojPath).template"
            let templateURL = projectRoot.appendingPathComponent(templatePath)
            try fileManager.createDirectory(at: templateURL.deletingLastPathComponent(), withIntermediateDirectories: true)
            if try writeIfChanged(content: template, to: templateURL) {
                updatedFiles.append(templatePath)
            }
        }

        // Refresh sln template.
        let slnTemplate = templatizeSln(slnContent, projectTypeGuid: firstEntry.typeGuid)
        let slnName = slnURL.lastPathComponent
        let slnBaseName = String(slnName.dropLast(".sln".count))
        let slnTemplatePath = "\(templateRoot)/\(slnBaseName).sln.template"
        let slnTemplateURL = projectRoot.appendingPathComponent(slnTemplatePath)
        try fileManager.createDirectory(at: slnTemplateURL.deletingLastPathComponent(), withIntermediateDirectories: true)
        if try writeIfChanged(content: slnTemplate, to: slnTemplateURL) {
            updatedFiles.append(slnTemplatePath)
        }

        return updatedFiles.sorted()
    }
}

// MARK: - Solution parsing

private struct SlnProjectEntry {
    let typeGuid: String
    let csprojPath: String
}

private func findSolutionFile(projectRoot: URL, fileManager: FileManager) throws -> URL {
    let contents = try fileManager.contentsOfDirectory(
        at: projectRoot,
        includingPropertiesForKeys: [.isRegularFileKey],
        options: [.skipsHiddenFiles]
    )
    guard let slnURL = contents.first(where: { $0.pathExtension == "sln" }) else {
        throw GeneratorError.noSolutionFound(projectRoot)
    }
    return slnURL
}

private func parseSolutionProjects(_ content: String) -> [SlnProjectEntry] {
    var results: [SlnProjectEntry] = []
    for line in content.split(separator: "\n") {
        guard line.hasPrefix("Project(\"") else { continue }

        var quoted: [String] = []
        var inQuote = false
        var current = ""
        for ch in line {
            if ch == "\"" {
                if inQuote {
                    quoted.append(current)
                    current = ""
                }
                inQuote = !inQuote
            } else if inQuote {
                current.append(ch)
            }
        }

        guard quoted.count >= 3 else { continue }

        let typeGuid = quoted[0]
        let csprojPath = quoted[2]

        guard csprojPath.hasSuffix(".csproj"), !csprojPath.contains("/") else {
            continue
        }

        results.append(SlnProjectEntry(typeGuid: typeGuid, csprojPath: csprojPath))
    }
    return results
}

// MARK: - Templatization

/// Replace dynamic sections of a Unity-generated .csproj with placeholders.
/// Strips XML comments, <None Include> entries, <Compile Include> lines, and
/// <ProjectReference> blocks, inserting {{SOURCE_FOLDERS}} and
/// {{PROJECT_REFERENCES}} placeholders. Absolute paths are replaced with
/// {{PROJECT_ROOT}}.
private func templatizeCsproj(_ content: String, projectRoot: String) -> String {
    var lines: [String] = []
    var sourcePlaceholderEmitted = false
    var refsPlaceholderEmitted = false
    var inProjectReference = false
    var inComment = false

    for rawLine in content.split(separator: "\n", omittingEmptySubsequences: false) {
        let line = String(rawLine)
            .replacingOccurrences(of: projectRoot, with: "{{PROJECT_ROOT}}")

        if inComment {
            if line.contains("-->") {
                inComment = false
            }
            continue
        }
        if line.contains("<!--") {
            if !line.contains("-->") {
                inComment = true
            }
            continue
        }

        if line.contains("<None Include=\"") {
            continue
        }

        if inProjectReference {
            if line.contains("</ProjectReference>") {
                inProjectReference = false
            }
            continue
        }
        if line.contains("<ProjectReference Include=\"") {
            if !refsPlaceholderEmitted {
                lines.append("    {{PROJECT_REFERENCES}}")
                refsPlaceholderEmitted = true
            }
            inProjectReference = true
            continue
        }

        if line.contains("<Compile Include=\"") {
            if !sourcePlaceholderEmitted {
                lines.append("    {{SOURCE_FOLDERS}}")
                sourcePlaceholderEmitted = true
            }
            continue
        }

        lines.append(line)
    }

    return lines.joined(separator: "\n")
}

/// Replace dynamic sections of a Unity-generated .sln with placeholders.
/// Collapses Project() blocks into {{PROJECT_ENTRIES}} and per-project
/// config lines into {{PROJECT_CONFIGS}}.
private func templatizeSln(_ content: String, projectTypeGuid: String) -> String {
    var lines: [String] = []
    var inProjectBlock = false
    var projectPlaceholderEmitted = false
    var configPlaceholderEmitted = false

    let projectPrefix = "Project(\"\(projectTypeGuid)\") = "

    for rawLine in content.split(separator: "\n", omittingEmptySubsequences: false) {
        let line = String(rawLine)

        if line.hasPrefix(projectPrefix) {
            if !projectPlaceholderEmitted {
                lines.append("{{PROJECT_ENTRIES}}")
                projectPlaceholderEmitted = true
            }
            inProjectBlock = true
            continue
        }

        if inProjectBlock {
            if line == "Global" {
                inProjectBlock = false
                lines.append(line)
            }
            continue
        }

        if line.contains(".Debug|Any CPU.") {
            let trimmed = line.trimmingCharacters(in: .whitespaces)
            if trimmed.hasPrefix("{") && (trimmed.contains("ActiveCfg") || trimmed.contains("Build.0")) {
                if !configPlaceholderEmitted {
                    lines.append("{{PROJECT_CONFIGS}}")
                    configPlaceholderEmitted = true
                }
                continue
            }
        }

        lines.append(line)
    }

    return lines.joined(separator: "\n")
}
