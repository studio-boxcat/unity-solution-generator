import Foundation
import XCTest
@testable import unity_solution_generator

final class SolutionGeneratorTests: XCTestCase {
    private let manifestPath = "manifest/projects.json"

    func testNestedAssemblyRootMappingAndLegacyFallback() throws {
        let root = try makeTempProjectRoot()
        defer { try? FileManager.default.removeItem(at: root) }

        try writeBaseProjectFiles(root: root)
        try writeManifest(root: root, projects: [
            ProjectEntry(name: "Main", csprojPath: "Main.csproj", templatePath: "templates/csproj/Main.csproj.template", guid: "{11111111-1111-1111-1111-111111111111}", kind: .asmdef),
            ProjectEntry(name: "Core", csprojPath: "Core.csproj", templatePath: "templates/csproj/Core.csproj.template", guid: "{22222222-2222-2222-2222-222222222222}", kind: .asmdef),
            ProjectEntry(name: "Tests", csprojPath: "Tests.csproj", templatePath: "templates/csproj/Tests.csproj.template", guid: "{33333333-3333-3333-3333-333333333333}", kind: .asmdef),
            ProjectEntry(name: "Assembly-CSharp-firstpass", csprojPath: "Assembly-CSharp-firstpass.csproj", templatePath: "templates/csproj/Assembly-CSharp-firstpass.csproj.template", guid: "{44444444-4444-4444-4444-444444444444}", kind: .legacy),
        ])

        try writeFile(root, "Assets/SystemAssets/Assemblies/Main/Main.asmdef", """
        {
          "name": "Main",
          "references": ["Core"]
        }
        """)
        try writeFile(root, "Assets/SystemAssets/Assemblies/Core/Core.asmdef", """
        {
          "name": "Core"
        }
        """)
        try writeFile(root, "Assets/SystemAssets/Assemblies/Tests/Tests.asmdef", """
        {
          "name": "Tests",
          "references": ["Main"]
        }
        """)

        try writeFile(root, "Assets/Game/Assembly.asmref", "{\"reference\":\"Main\"}\n")
        try writeFile(root, "Assets/Game/Core/Assembly.asmref", "{\"reference\":\"Core\"}\n")
        try writeFile(root, "Assets/Game/Tests/Assembly.asmref", "{\"reference\":\"Tests\"}\n")

        try writeFile(root, "Assets/Game/Foo.cs", "class Foo {}\n")
        try writeFile(root, "Assets/Game/Feature/SubFeature/Fizz.cs", "class Fizz {}\n")
        try writeFile(root, "Assets/Game/Core/Bar.cs", "class Bar {}\n")
        try writeFile(root, "Assets/Game/Tests/Baz.cs", "class Baz {}\n")
        try writeFile(root, "Assets/Plugins/Legacy.cs", "class Legacy {}\n")

        let generator = SolutionGenerator()
        _ = try generator.generate(options: GenerateOptions(projectRoot: root, manifestPath: manifestPath))

        try assertCompileSet(
            root: root,
            csprojPath: "Main.csproj",
            expected: [
                "Assets/Game/Foo.cs",
                "Assets/Game/Feature/SubFeature/Fizz.cs",
            ]
        )

        try assertCompileSet(
            root: root,
            csprojPath: "Core.csproj",
            expected: [
                "Assets/Game/Core/Bar.cs",
            ]
        )

        try assertCompileSet(
            root: root,
            csprojPath: "Tests.csproj",
            expected: [
                "Assets/Game/Tests/Baz.cs",
            ]
        )

        try assertCompileSet(
            root: root,
            csprojPath: "Assembly-CSharp-firstpass.csproj",
            expected: [
                "Assets/Plugins/Legacy.cs",
            ]
        )

        let main = try readFile(root, "Main.csproj")
        XCTAssertTrue(main.contains("<ProjectReference Include=\"Core.csproj\">"))

        let tests = try readFile(root, "Tests.csproj")
        XCTAssertTrue(tests.contains("<ProjectReference Include=\"Main.csproj\">"))
    }

    func testAsmRefGuidResolutionAndTildeSkip() throws {
        let root = try makeTempProjectRoot()
        defer { try? FileManager.default.removeItem(at: root) }

        try writeBaseProjectFiles(root: root)
        try writeManifest(root: root, projects: [
            ProjectEntry(name: "Core", csprojPath: "Core.csproj", templatePath: "templates/csproj/Core.csproj.template", guid: "{22222222-2222-2222-2222-222222222222}", kind: .asmdef),
        ])

        try writeFile(root, "Assets/SystemAssets/Assemblies/Core/Core.asmdef", "{\"name\":\"Core\"}\n")
        try writeFile(root, "Assets/SystemAssets/Assemblies/Core/Core.asmdef.meta", "fileFormatVersion: 2\nguid: abcdefabcdefabcdefabcdefabcdefab\n")
        try writeFile(root, "Assets/Game/Assembly.asmref", "{\"reference\":\"GUID:abcdefabcdefabcdefabcdefabcdefab\"}\n")

        try writeFile(root, "Assets/Game/Good.cs", "class Good {}\n")
        try writeFile(root, "Packages/com.example/src~/Hidden.cs", "class Hidden {}\n")

        let generator = SolutionGenerator()
        _ = try generator.generate(options: GenerateOptions(projectRoot: root, manifestPath: manifestPath))

        try assertCompileSet(
            root: root,
            csprojPath: "Core.csproj",
            expected: [
                "Assets/Game/Good.cs",
            ]
        )
    }

    func testTildeDirectoryExcludedFromGlobPatterns() throws {
        let root = try makeTempProjectRoot()
        defer { try? FileManager.default.removeItem(at: root) }

        try writeBaseProjectFiles(root: root)
        try writeManifest(root: root, projects: [
            ProjectEntry(name: "Main", csprojPath: "Main.csproj", templatePath: "templates/csproj/Main.csproj.template", guid: "{11111111-1111-1111-1111-111111111111}", kind: .asmdef),
        ])

        try writeFile(root, "Assets/SystemAssets/Assemblies/Main/Main.asmdef", "{\"name\":\"Main\"}\n")
        try writeFile(root, "Assets/Game/Assembly.asmref", "{\"reference\":\"Main\"}\n")

        try writeFile(root, "Assets/Game/Good.cs", "class Good {}\n")
        try writeFile(root, "Assets/Game/src~/Hidden.cs", "class Hidden {}\n")
        try writeFile(root, "Assets/Game/backup~/Old.cs", "class Old {}\n")

        let generator = SolutionGenerator()
        _ = try generator.generate(options: GenerateOptions(projectRoot: root, manifestPath: manifestPath))

        let csproj = try readFile(root, "Main.csproj")

        // Tilde dirs should appear as exclusions in the glob pattern.
        XCTAssertTrue(csproj.contains("Assets/Game/src~/**/*.cs"), "Should exclude src~ dir")
        XCTAssertTrue(csproj.contains("Assets/Game/backup~/**/*.cs"), "Should exclude backup~ dir")

        // The actual file set should only contain Good.cs.
        try assertCompileSet(root: root, csprojPath: "Main.csproj", expected: ["Assets/Game/Good.cs"])
    }

    func testDotDirectoryExcludedFromScan() throws {
        let root = try makeTempProjectRoot()
        defer { try? FileManager.default.removeItem(at: root) }

        try writeBaseProjectFiles(root: root)
        try writeManifest(root: root, projects: [
            ProjectEntry(name: "Main", csprojPath: "Main.csproj", templatePath: "templates/csproj/Main.csproj.template", guid: "{11111111-1111-1111-1111-111111111111}", kind: .asmdef),
        ])

        try writeFile(root, "Assets/SystemAssets/Assemblies/Main/Main.asmdef", "{\"name\":\"Main\"}\n")
        try writeFile(root, "Assets/Game/Assembly.asmref", "{\"reference\":\"Main\"}\n")

        try writeFile(root, "Assets/Game/Visible.cs", "class Visible {}\n")
        try writeFile(root, "Assets/Game/.hidden/Secret.cs", "class Secret {}\n")

        let generator = SolutionGenerator()
        _ = try generator.generate(options: GenerateOptions(projectRoot: root, manifestPath: manifestPath))

        // Dot-directory files should not be in the compile set.
        try assertCompileSet(root: root, csprojPath: "Main.csproj", expected: ["Assets/Game/Visible.cs"])
    }

    func testE2EGeneratedCompileSetMatchesOriginalCsproj() throws {
        let root = try makeTempProjectRoot()
        defer { try? FileManager.default.removeItem(at: root) }

        try writeBaseProjectFiles(root: root)
        try writeManifest(root: root, projects: [
            ProjectEntry(name: "Main", csprojPath: "Main.csproj", templatePath: "templates/csproj/Main.csproj.template", guid: "{11111111-1111-1111-1111-111111111111}", kind: .asmdef),
            ProjectEntry(name: "Sandbox", csprojPath: "Sandbox.csproj", templatePath: "templates/csproj/Sandbox.csproj.template", guid: "{22222222-2222-2222-2222-222222222222}", kind: .asmdef),
        ])

        try writeFile(root, "Assets/SystemAssets/Assemblies/Main/Main.asmdef", "{\"name\":\"Main\"}\n")
        try writeFile(root, "Assets/SystemAssets/Assemblies/Sandbox/Sandbox.asmdef", "{\"name\":\"Sandbox\"}\n")
        try writeFile(root, "Assets/Game/Assembly.asmref", "{\"reference\":\"Main\"}\n")
        try writeFile(root, "Assets/Game/Sandbox/Assembly.asmref", "{\"reference\":\"Sandbox\"}\n")

        try writeFile(root, "Assets/Game/A.cs", "class A {}\n")
        try writeFile(root, "Assets/Game/Sub/B.cs", "class B {}\n")
        try writeFile(root, "Assets/Game/Tests/CTest.cs", "class CTest {}\n")
        try writeFile(root, "Assets/Game/Sandbox/S.cs", "class S {}\n")

        // Simulates the original Unity-generated per-file projects.
        try writeFile(root, "Main.original.csproj", """
        <Project>
          <ItemGroup>
            <Compile Include="Assets/Game/A.cs" />
            <Compile Include="Assets/Game/Sub/B.cs" />
            <Compile Include="Assets/Game/Tests/CTest.cs" />
          </ItemGroup>
        </Project>
        """)

        try writeFile(root, "Sandbox.original.csproj", """
        <Project>
          <ItemGroup>
            <Compile Include="Assets/Game/Sandbox/S.cs" />
          </ItemGroup>
        </Project>
        """)

        let generator = SolutionGenerator()
        _ = try generator.generate(options: GenerateOptions(projectRoot: root, manifestPath: manifestPath))

        let originalMain = try readCompileSet(root: root, csprojPath: "Main.original.csproj")
        let generatedMain = try readCompileSet(root: root, csprojPath: "Main.csproj")
        XCTAssertEqual(generatedMain, originalMain)

        let originalSandbox = try readCompileSet(root: root, csprojPath: "Sandbox.original.csproj")
        let generatedSandbox = try readCompileSet(root: root, csprojPath: "Sandbox.csproj")
        XCTAssertEqual(generatedSandbox, originalSandbox)
    }

    private func makeTempProjectRoot() throws -> URL {
        let url = URL(fileURLWithPath: NSTemporaryDirectory())
            .appendingPathComponent("solution-generator-tests-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: url, withIntermediateDirectories: true)
        return url
    }

    private func writeBaseProjectFiles(root: URL) throws {
        try writeFile(root, "ProjectSettings/ProjectVersion.txt", "m_EditorVersion: 6000.2.7f2\n")
        try writeFile(root, "templates/test.sln.template", """
        Microsoft Visual Studio Solution File, Format Version 11.00
        {{PROJECT_ENTRIES}}
        Global
        \tGlobalSection(ProjectConfigurationPlatforms) = postSolution
        {{PROJECT_CONFIGS}}
        \tEndGlobalSection
        EndGlobal
        """)
    }

    private func writeManifest(root: URL, projects: [ProjectEntry]) throws {
        let manifest = GeneratorManifest(
            solutionPath: "test.sln",
            solutionTemplatePath: "templates/test.sln.template",
            projectTypeGuid: "{FAE04EC0-301F-11D3-BF4B-00C04F79EFBC}",
            projects: projects
        )

        let encoder = JSONEncoder()
        encoder.outputFormatting = [.prettyPrinted, .sortedKeys]
        let data = try encoder.encode(manifest)
        try writeFile(root, "manifest/projects.json", String(decoding: data, as: UTF8.self) + "\n")

        for project in projects {
            try writeFile(root, project.templatePath, """
            <Project>
              <ItemGroup>
            {{SOURCE_FOLDERS}}
            {{PROJECT_REFERENCES}}
              </ItemGroup>
            </Project>
            """)
        }
    }

    private func assertCompileSet(root: URL, csprojPath: String, expected: Set<String>) throws {
        let actual = try readCompileSet(root: root, csprojPath: csprojPath)
        XCTAssertEqual(actual, expected)
    }

    private func readCompileSet(root: URL, csprojPath: String) throws -> Set<String> {
        let content = try readFile(root, csprojPath)
        let entries = try parseCompileEntries(from: content)
        return try expandCompileEntries(entries, root: root)
    }

    private func parseCompileEntries(from csproj: String) throws -> [CompileEntry] {
        let pattern = #"<Compile Include=\"([^\"]+)\"(?: Exclude=\"([^\"]+)\")?\s*/>"#
        let regex = try NSRegularExpression(pattern: pattern)
        let range = NSRange(csproj.startIndex..<csproj.endIndex, in: csproj)

        return regex.matches(in: csproj, range: range).compactMap { match in
            guard
                let includeRange = Range(match.range(at: 1), in: csproj)
            else {
                return nil
            }

            let include = xmlUnescape(String(csproj[includeRange]))

            let exclude: [String]
            if
                match.range(at: 2).location != NSNotFound,
                let excludeRange = Range(match.range(at: 2), in: csproj)
            {
                exclude = xmlUnescape(String(csproj[excludeRange]))
                    .split(separator: ";")
                    .map(String.init)
                    .filter { !$0.isEmpty }
            } else {
                exclude = []
            }

            return CompileEntry(include: include, exclude: exclude)
        }
    }

    private func expandCompileEntries(_ entries: [CompileEntry], root: URL) throws -> Set<String> {
        var result: Set<String> = []

        for entry in entries {
            let included = try expandPattern(entry.include, root: root)
            var excluded: Set<String> = []
            for pattern in entry.exclude {
                excluded.formUnion(try expandPattern(pattern, root: root))
            }
            result.formUnion(included.subtracting(excluded))
        }

        return result
    }

    private func expandPattern(_ pattern: String, root: URL) throws -> Set<String> {
        if pattern == "**/*.cs" {
            return try listCsFilesRecursively(root: root, relativeDirectory: "")
        }

        if pattern.hasSuffix("/**/*.cs") {
            let directory = String(pattern.dropLast("/**/*.cs".count))
            return try listCsFilesRecursively(root: root, relativeDirectory: directory)
        }

        if pattern == "*.cs" {
            return try listCsFiles(root: root, relativeDirectory: "")
        }

        if pattern.hasSuffix("/*.cs") {
            let directory = String(pattern.dropLast("/*.cs".count))
            return try listCsFiles(root: root, relativeDirectory: directory)
        }

        if pattern.hasSuffix(".cs") {
            let path = pattern.replacingOccurrences(of: "\\", with: "/")
            let fileURL = root.appendingPathComponent(path)
            if FileManager.default.fileExists(atPath: fileURL.path) {
                return [path]
            }
            return []
        }

        return []
    }

    private func listCsFilesRecursively(root: URL, relativeDirectory: String) throws -> Set<String> {
        let directoryURL = relativeDirectory.isEmpty
            ? root
            : root.appendingPathComponent(relativeDirectory)

        guard FileManager.default.fileExists(atPath: directoryURL.path) else {
            return []
        }

        var result: Set<String> = []
        guard let enumerator = FileManager.default.enumerator(
            at: directoryURL,
            includingPropertiesForKeys: [.isRegularFileKey, .isDirectoryKey],
            options: [.skipsHiddenFiles]
        ) else {
            return []
        }

        let rootPath = root.standardizedFileURL.path
        while let url = enumerator.nextObject() as? URL {
            let standardized = url.standardizedFileURL
            let values = try standardized.resourceValues(forKeys: [.isRegularFileKey, .isDirectoryKey])
            if values.isDirectory == true {
                continue
            }
            guard values.isRegularFile == true else {
                continue
            }
            guard standardized.path.hasSuffix(".cs") else {
                continue
            }
            guard standardized.path.hasPrefix(rootPath + "/") else {
                continue
            }
            let relative = String(standardized.path.dropFirst(rootPath.count + 1)).replacingOccurrences(of: "\\", with: "/")
            result.insert(relative)
        }

        return result
    }

    private func listCsFiles(root: URL, relativeDirectory: String) throws -> Set<String> {
        let directoryURL = relativeDirectory.isEmpty
            ? root
            : root.appendingPathComponent(relativeDirectory)

        guard FileManager.default.fileExists(atPath: directoryURL.path) else {
            return []
        }

        let rootPath = root.standardizedFileURL.path
        let urls = try FileManager.default.contentsOfDirectory(
            at: directoryURL,
            includingPropertiesForKeys: [.isRegularFileKey],
            options: [.skipsHiddenFiles]
        )

        var result: Set<String> = []
        for url in urls {
            let standardized = url.standardizedFileURL
            let values = try standardized.resourceValues(forKeys: [.isRegularFileKey])
            guard values.isRegularFile == true else {
                continue
            }
            guard standardized.path.hasSuffix(".cs") else {
                continue
            }
            guard standardized.path.hasPrefix(rootPath + "/") else {
                continue
            }
            let relative = String(standardized.path.dropFirst(rootPath.count + 1)).replacingOccurrences(of: "\\", with: "/")
            result.insert(relative)
        }

        return result
    }

    private func xmlUnescape(_ value: String) -> String {
        var unescaped = value
        unescaped = unescaped.replacingOccurrences(of: "&quot;", with: "\"")
        unescaped = unescaped.replacingOccurrences(of: "&apos;", with: "'")
        unescaped = unescaped.replacingOccurrences(of: "&lt;", with: "<")
        unescaped = unescaped.replacingOccurrences(of: "&gt;", with: ">")
        unescaped = unescaped.replacingOccurrences(of: "&amp;", with: "&")
        return unescaped
    }

    private func writeFile(_ root: URL, _ relativePath: String, _ content: String) throws {
        let fileURL = root.appendingPathComponent(relativePath)
        try FileManager.default.createDirectory(at: fileURL.deletingLastPathComponent(), withIntermediateDirectories: true)
        try content.write(to: fileURL, atomically: true, encoding: .utf8)
    }

    private func readFile(_ root: URL, _ relativePath: String) throws -> String {
        try String(contentsOf: root.appendingPathComponent(relativePath), encoding: .utf8)
    }

    // MARK: - prepareBuild tests

    func testStripEditorDefines() {
        let gen = SolutionGenerator()
        let input = "<DefineConstants>UNITY_5;UNITY_EDITOR;UNITY_EDITOR_64;UNITY_EDITOR_OSX;DEBUG;TRACE;UNITY_IOS</DefineConstants>"

        let release = gen.stripEditorDefines(input, debugBuild: false)
        XCTAssertEqual(release, "<DefineConstants>UNITY_5;UNITY_IOS</DefineConstants>")

        let debug = gen.stripEditorDefines(input, debugBuild: true)
        XCTAssertEqual(debug, "<DefineConstants>UNITY_5;DEBUG;TRACE;UNITY_IOS</DefineConstants>")
    }

    func testSwapPlatformDefinesIos() {
        let gen = SolutionGenerator()
        let input = "<DefineConstants>UNITY_ANDROID;UNITY_IPHONE;OTHER</DefineConstants>"
        let result = gen.swapPlatformDefines(input, platform: .ios)
        XCTAssertEqual(result, "<DefineConstants>UNITY_IOS;UNITY_IPHONE;OTHER</DefineConstants>")
    }

    func testSwapPlatformDefinesAndroid() {
        let gen = SolutionGenerator()
        let input = "<DefineConstants>UNITY_IOS;UNITY_IPHONE;OTHER</DefineConstants>"
        let result = gen.swapPlatformDefines(input, platform: .android)
        XCTAssertEqual(result, "<DefineConstants>UNITY_ANDROID;OTHER</DefineConstants>")
    }

    func testStripNonRuntimeReferences() {
        let gen = SolutionGenerator()
        let input = """
        <ItemGroup>
            <ProjectReference Include="Core.csproj">
            </ProjectReference>
            <ProjectReference Include="App.Editor.csproj">
            </ProjectReference>
            <ProjectReference Include="Game.Tests.Runner.csproj">
            </ProjectReference>
        </ItemGroup>
        """
        let result = gen.stripNonRuntimeReferences(input, nonRuntimeNames: ["App.Editor", "Game.Tests.Runner"])
        XCTAssertTrue(result.contains("Core.csproj"))
        XCTAssertFalse(result.contains("App.Editor.csproj"))
        XCTAssertFalse(result.contains("Game.Tests.Runner.csproj"))
    }

    func testRewriteReferenceSuffix() {
        let gen = SolutionGenerator()
        let input = """
        <ProjectReference Include="Core.csproj">
        </ProjectReference>
        <ProjectReference Include="Utils.csproj">
        </ProjectReference>
        """
        let result = gen.rewriteReferenceSuffix(input, suffix: ".v.ios-prod")
        XCTAssertTrue(result.contains("Core.v.ios-prod.csproj"))
        XCTAssertTrue(result.contains("Utils.v.ios-prod.csproj"))
        XCTAssertFalse(result.contains("\"Core.csproj\""))
    }

    func testPrepareBuildCategoryFiltering() throws {
        let root = try makeTempProjectRoot()
        defer { try? FileManager.default.removeItem(at: root) }

        try writeBaseProjectFiles(root: root)
        try writeManifest(root: root, projects: [
            ProjectEntry(name: "Runtime", csprojPath: "Runtime.csproj", templatePath: "templates/csproj/Runtime.csproj.template", guid: "{11111111-1111-1111-1111-111111111111}", kind: .asmdef, category: .runtime),
            ProjectEntry(name: "MyEditor", csprojPath: "MyEditor.csproj", templatePath: "templates/csproj/MyEditor.csproj.template", guid: "{22222222-2222-2222-2222-222222222222}", kind: .asmdef, category: .editor),
            ProjectEntry(name: "MyTests", csprojPath: "MyTests.csproj", templatePath: "templates/csproj/MyTests.csproj.template", guid: "{33333333-3333-3333-3333-333333333333}", kind: .asmdef, category: .test),
        ])

        // Write minimal csprojs for prepare-build to read
        let csprojContent = """
        <Project>
          <PropertyGroup>
            <DefineConstants>UNITY_5;UNITY_EDITOR;UNITY_IOS</DefineConstants>
          </PropertyGroup>
          <ItemGroup>
            <ProjectReference Include="MyEditor.csproj">
            </ProjectReference>
          </ItemGroup>
        </Project>
        """
        try writeFile(root, "Runtime.csproj", csprojContent)
        try writeFile(root, "MyEditor.csproj", csprojContent)
        try writeFile(root, "MyTests.csproj", csprojContent)

        let gen = SolutionGenerator()
        let result = try gen.prepareBuild(options: PrepareBuildOptions(
            projectRoot: root, manifestPath: manifestPath, platform: .ios
        ))

        // Only runtime project should get a variant
        XCTAssertEqual(result.generatedCsprojs.count, 1)
        XCTAssertTrue(result.generatedCsprojs[0].hasPrefix("Runtime"))
        XCTAssertTrue(result.generatedCsprojs[0].contains(".v.ios-prod"))

        // Verify editor defines stripped and editor ref removed
        let variant = try readFile(root, result.generatedCsprojs[0])
        XCTAssertFalse(variant.contains("UNITY_EDITOR"))
        XCTAssertFalse(variant.contains("MyEditor.csproj\">"))
    }

    func testPrepareBuildSkipsFreshCsproj() throws {
        let root = try makeTempProjectRoot()
        defer { try? FileManager.default.removeItem(at: root) }

        try writeBaseProjectFiles(root: root)
        try writeManifest(root: root, projects: [
            ProjectEntry(name: "Lib", csprojPath: "Lib.csproj", templatePath: "templates/csproj/Lib.csproj.template", guid: "{11111111-1111-1111-1111-111111111111}", kind: .asmdef, category: .runtime),
        ])

        let csproj = "<Project><PropertyGroup><DefineConstants>UNITY_5;UNITY_EDITOR;UNITY_IOS</DefineConstants></PropertyGroup></Project>"
        try writeFile(root, "Lib.csproj", csproj)

        let gen = SolutionGenerator()

        // First run generates
        let first = try gen.prepareBuild(options: PrepareBuildOptions(
            projectRoot: root, manifestPath: manifestPath, platform: .ios
        ))
        XCTAssertEqual(first.generatedCsprojs.count, 1)
        XCTAssertEqual(first.skippedCsprojs.count, 0)

        // Second run skips (suffixed file is newer)
        let second = try gen.prepareBuild(options: PrepareBuildOptions(
            projectRoot: root, manifestPath: manifestPath, platform: .ios
        ))
        XCTAssertEqual(second.generatedCsprojs.count, 0)
        XCTAssertEqual(second.skippedCsprojs.count, 1)
    }
}

private struct CompileEntry {
    let include: String
    let exclude: [String]
}
