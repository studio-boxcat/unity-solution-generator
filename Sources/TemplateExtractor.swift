import Darwin

struct ExtractTemplatesOptions: Sendable {
    let projectRoot: String
    let generatorRoot: String

    init(projectRoot: String, generatorRoot: String = defaultGeneratorRoot) {
        self.projectRoot = projectRoot
        self.generatorRoot = generatorRoot
    }
}

/// Reads Unity-generated .sln/.csproj files and converts them into template
/// fragments. Dynamic defines are stripped (moved to Directory.Build.props at
/// generation time), absolute paths become $(ProjectRoot), and the closing
/// </Project> tag is removed so the generator can append source/reference
/// entries directly.
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

        for csprojName in slnEntries {
            let csprojPath = joinPath(projectRoot, csprojName)
            guard fileExists(csprojPath) else { continue }

            let content = try readFile(csprojPath)
            let template = templatizeCsproj(content, projectRoot: projectRoot)

            let templateRelPath = "\(generatorRoot)/templates/\(csprojName).template"
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

private func findSolutionFile(projectRoot: String) throws -> String {
    let entries = listDirectory(projectRoot)
    guard let slnFile = entries.first(where: { $0.hasSuffix(".sln") }) else {
        throw GeneratorError.noSolutionFound(projectRoot)
    }
    return joinPath(projectRoot, slnFile)
}

/// Returns csproj filenames referenced in the .sln content.
private func parseSolutionProjects(_ content: String) -> [String] {
    var results: [String] = []
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
        let path = quoted[2]
        guard path.hasSuffix(".csproj"), !path.contains("/") else { continue }
        results.append(path)
    }
    return results
}

// MARK: - Templatization

private let dynamicDefines = DynamicDefines.all

/// Strip dynamic sections, replace absolute paths with $(ProjectRoot),
/// strip dynamic defines from DefineConstants, and cut before </Project>.
private func templatizeCsproj(_ content: String, projectRoot: String) -> String {
    var lines: [String] = []
    var inProjectReference = false
    var inComment = false

    for rawLine in content.split(separator: "\n", omittingEmptySubsequences: false) {
        let line = String(rawLine).replacingAll(projectRoot, with: "$(ProjectRoot)")

        if inComment {
            if line.contains("-->") { inComment = false }
            continue
        }
        if line.contains("<!--") {
            if !line.contains("-->") { inComment = true }
            continue
        }

        // Skip dynamic entries.
        if line.contains("<None Include=\"") { continue }
        if line.contains("<Compile Include=\"") { continue }

        if inProjectReference {
            if line.contains("</ProjectReference>") { inProjectReference = false }
            continue
        }
        if line.contains("<ProjectReference Include=\"") {
            inProjectReference = true
            continue
        }

        // Strip </Project> — generator appends its own closing tag.
        if trimWhitespace(line) == "</Project>" { continue }

        // Strip dynamic defines from DefineConstants.
        if line.contains("<DefineConstants>"), line.contains("</DefineConstants>") {
            lines.append(stripDynamicDefines(line))
            continue
        }

        lines.append(line)
    }

    // Remove empty <ItemGroup></ItemGroup> pairs.
    var cleaned: [String] = []
    var i = 0
    while i < lines.count {
        let trimmed = trimWhitespace(lines[i])
        if trimmed == "<ItemGroup>" && i + 1 < lines.count
            && trimWhitespace(lines[i + 1]) == "</ItemGroup>" {
            i += 2
            continue
        }
        cleaned.append(lines[i])
        i += 1
    }

    // Trim trailing blank lines.
    while let last = cleaned.last, last.allSatisfy({ $0 == " " || $0 == "\t" || $0 == "\n" }) || cleaned.last == "" {
        cleaned.removeLast()
    }

    return cleaned.joined(separator: "\n") + "\n"
}

private func stripDynamicDefines(_ line: String) -> String {
    guard let openTag = line.firstRange(of: "<DefineConstants>"),
          let closeTag = line.firstRange(of: "</DefineConstants>") else {
        return line
    }

    let prefix = line[..<openTag.lowerBound]
    let value = String(line[openTag.upperBound..<closeTag.lowerBound])

    let staticDefines = value.split(separator: ";")
        .map(String.init)
        .filter { !dynamicDefines.contains($0) }

    let newValue: String
    if staticDefines.isEmpty {
        newValue = "$(DefineConstants)"
    } else {
        newValue = "$(DefineConstants);" + staticDefines.joined(separator: ";")
    }

    return "\(prefix)<DefineConstants>\(newValue)</DefineConstants>"
}
