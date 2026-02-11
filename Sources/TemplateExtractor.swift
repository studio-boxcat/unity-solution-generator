import Darwin

struct ExtractTemplatesOptions: Sendable {
    let projectRoot: String
    let generatorRoot: String

    init(projectRoot: String, generatorRoot: String = "Library/UnitySolutionGenerator") {
        self.projectRoot = projectRoot
        self.generatorRoot = generatorRoot
    }
}

/// Reads Unity-generated .sln/.csproj files and converts them into templates
/// with placeholders ({{SOURCE_FOLDERS}}, {{PROJECT_REFERENCES}}, etc.) that
/// the generator can later fill in from the filesystem scan.
struct TemplateExtractor {
    static func extract(options: ExtractTemplatesOptions) throws -> [String] {
        let projectRoot = resolveRealPath(options.projectRoot)
        let generatorRoot = options.generatorRoot

        // Find .sln and parse projects.
        let slnPath = try findSolutionFile(projectRoot: projectRoot)
        let slnContent = try readFile(slnPath)
        let slnEntries = parseSolutionProjects(slnContent)
        guard !slnEntries.isEmpty else {
            throw GeneratorError.noProjectsInSolution(slnPath)
        }

        var updatedFiles: [String] = []

        for entry in slnEntries {
            let csprojPath = joinPath(projectRoot, entry.csprojPath)
            guard fileExists(csprojPath) else { continue }

            let content = try readFile(csprojPath)
            let template = templatizeCsproj(content, projectRoot: projectRoot)

            let templateRelPath = "\(generatorRoot)/templates/\(entry.csprojPath).template"
            let templatePath = joinPath(projectRoot, templateRelPath)
            createDirectoryRecursive(parentDirectory(of: templatePath))
            if try writeFileIfChanged(templatePath, template) {
                updatedFiles.append(templateRelPath)
            }
        }

        return updatedFiles.sorted()
    }
}

// MARK: - Solution parsing

private struct SlnProjectEntry {
    let csprojPath: String
}

private func findSolutionFile(projectRoot: String) throws -> String {
    let entries = listDirectory(projectRoot)
    guard let slnFile = entries.first(where: { $0.hasSuffix(".sln") }) else {
        throw GeneratorError.noSolutionFound(projectRoot)
    }
    return joinPath(projectRoot, slnFile)
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

        let csprojPath = quoted[2]

        guard csprojPath.hasSuffix(".csproj"), !csprojPath.contains("/") else {
            continue
        }

        results.append(SlnProjectEntry(csprojPath: csprojPath))
    }
    return results
}

// MARK: - Templatization

/// Replace dynamic sections of a Unity-generated .csproj with placeholders.
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
