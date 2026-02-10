import Foundation
import XCTest
@testable import unity_solution_generator

final class SolutionGeneratorTests: XCTestCase {
    private let templateRoot = "tpl"

    func testNestedAssemblyRootMappingAndLegacyFallback() throws {
        let root = try makeTempProjectRoot()
        defer { try? FileManager.default.removeItem(at: root) }

        try writeBaseProjectFiles(root: root)
        try writeTemplates(root: root, projectNames: ["Main", "Core", "Tests", "Assembly-CSharp-firstpass"])

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
        _ = try generator.generate(options: GenerateOptions(projectRoot: root, templateRoot: templateRoot))

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

    func testAsmRefNameResolutionAndTildeSkip() throws {
        let root = try makeTempProjectRoot()
        defer { try? FileManager.default.removeItem(at: root) }

        try writeBaseProjectFiles(root: root)
        try writeTemplates(root: root, projectNames: ["Core"])

        try writeFile(root, "Assets/SystemAssets/Assemblies/Core/Core.asmdef", "{\"name\":\"Core\"}\n")
        try writeFile(root, "Assets/Game/Assembly.asmref", "{\"reference\":\"Core\"}\n")

        try writeFile(root, "Assets/Game/Good.cs", "class Good {}\n")
        try writeFile(root, "Packages/com.example/src~/Hidden.cs", "class Hidden {}\n")

        let generator = SolutionGenerator()
        _ = try generator.generate(options: GenerateOptions(projectRoot: root, templateRoot: templateRoot))

        try assertCompileSet(
            root: root,
            csprojPath: "Core.csproj",
            expected: [
                "Assets/Game/Good.cs",
            ]
        )
    }

    func testTildeDirectoryExcludedFromScan() throws {
        let root = try makeTempProjectRoot()
        defer { try? FileManager.default.removeItem(at: root) }

        try writeBaseProjectFiles(root: root)
        try writeTemplates(root: root, projectNames: ["Main"])

        try writeFile(root, "Assets/SystemAssets/Assemblies/Main/Main.asmdef", "{\"name\":\"Main\"}\n")
        try writeFile(root, "Assets/Game/Assembly.asmref", "{\"reference\":\"Main\"}\n")

        try writeFile(root, "Assets/Game/Good.cs", "class Good {}\n")
        try writeFile(root, "Assets/Game/src~/Hidden.cs", "class Hidden {}\n")
        try writeFile(root, "Assets/Game/backup~/Old.cs", "class Old {}\n")

        let generator = SolutionGenerator()
        _ = try generator.generate(options: GenerateOptions(projectRoot: root, templateRoot: templateRoot))

        // Tilde dirs are skipped during scan — their files never appear.
        try assertCompileSet(root: root, csprojPath: "Main.csproj", expected: ["Assets/Game/Good.cs"])
    }

    func testDotDirectoryExcludedFromScan() throws {
        let root = try makeTempProjectRoot()
        defer { try? FileManager.default.removeItem(at: root) }

        try writeBaseProjectFiles(root: root)
        try writeTemplates(root: root, projectNames: ["Main"])

        try writeFile(root, "Assets/SystemAssets/Assemblies/Main/Main.asmdef", "{\"name\":\"Main\"}\n")
        try writeFile(root, "Assets/Game/Assembly.asmref", "{\"reference\":\"Main\"}\n")

        try writeFile(root, "Assets/Game/Visible.cs", "class Visible {}\n")
        try writeFile(root, "Assets/Game/.hidden/Secret.cs", "class Secret {}\n")

        let generator = SolutionGenerator()
        _ = try generator.generate(options: GenerateOptions(projectRoot: root, templateRoot: templateRoot))

        // Dot-directory files should not be in the compile set.
        try assertCompileSet(root: root, csprojPath: "Main.csproj", expected: ["Assets/Game/Visible.cs"])
    }

    func testE2EGeneratedCompileSetMatchesOriginalCsproj() throws {
        let root = try makeTempProjectRoot()
        defer { try? FileManager.default.removeItem(at: root) }

        try writeBaseProjectFiles(root: root)
        try writeTemplates(root: root, projectNames: ["Main", "Sandbox"])

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
        _ = try generator.generate(options: GenerateOptions(projectRoot: root, templateRoot: templateRoot))

        let originalMain = try readCompileSet(root: root, csprojPath: "Main.original.csproj")
        let generatedMain = try readCompileSet(root: root, csprojPath: "Main.csproj")
        XCTAssertEqual(generatedMain, originalMain)

        let originalSandbox = try readCompileSet(root: root, csprojPath: "Sandbox.original.csproj")
        let generatedSandbox = try readCompileSet(root: root, csprojPath: "Sandbox.csproj")
        XCTAssertEqual(generatedSandbox, originalSandbox)
    }

    // MARK: - Setup helpers

    private func makeTempProjectRoot() throws -> URL {
        let url = URL(fileURLWithPath: NSTemporaryDirectory())
            .appendingPathComponent("solution-generator-tests-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: url, withIntermediateDirectories: true)
        return url
    }

    private func writeBaseProjectFiles(root: URL) throws {
        try writeFile(root, "tpl/test.sln.template", """
        Microsoft Visual Studio Solution File, Format Version 11.00
        {{PROJECT_ENTRIES}}
        Global
        \tGlobalSection(ProjectConfigurationPlatforms) = postSolution
        {{PROJECT_CONFIGS}}
        \tEndGlobalSection
        EndGlobal
        """)
    }

    private func writeTemplates(root: URL, projectNames: [String]) throws {
        for name in projectNames {
            try writeFile(root, "tpl/csproj/\(name).csproj.template", """
            <Project>
              <ItemGroup>
            {{SOURCE_FOLDERS}}
            {{PROJECT_REFERENCES}}
              </ItemGroup>
            </Project>
            """)
        }
    }

    // MARK: - Assertion helpers

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
        let pattern = #"<Compile Include=\"([^\"]+)\"\s*/>"#
        let regex = try NSRegularExpression(pattern: pattern)
        let range = NSRange(csproj.startIndex..<csproj.endIndex, in: csproj)

        return regex.matches(in: csproj, range: range).compactMap { match in
            guard let includeRange = Range(match.range(at: 1), in: csproj) else {
                return nil
            }
            return CompileEntry(include: xmlUnescape(String(csproj[includeRange])))
        }
    }

    private func expandCompileEntries(_ entries: [CompileEntry], root: URL) throws -> Set<String> {
        var result: Set<String> = []
        for entry in entries {
            result.formUnion(try expandPattern(entry.include, root: root))
        }
        return result
    }

    private func expandPattern(_ pattern: String, root: URL) throws -> Set<String> {
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


        // Write asmdef files with category-determining fields
        try writeFile(root, "Assets/Assemblies/Runtime/Runtime.asmdef", "{\"name\":\"Runtime\"}\n")
        try writeFile(root, "Assets/Assemblies/MyEditor/MyEditor.asmdef", """
        {"name":"MyEditor","includePlatforms":["Editor"]}
        """)
        try writeFile(root, "Assets/Assemblies/MyTests/MyTests.asmdef", """
        {"name":"MyTests","defineConstraints":["UNITY_INCLUDE_TESTS"]}
        """)

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
            projectRoot: root, platform: .ios
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


        // Runtime asmdef (no includePlatforms/defineConstraints = runtime)
        try writeFile(root, "Assets/Assemblies/Lib/Lib.asmdef", "{\"name\":\"Lib\"}\n")

        let csproj = "<Project><PropertyGroup><DefineConstants>UNITY_5;UNITY_EDITOR;UNITY_IOS</DefineConstants></PropertyGroup></Project>"
        try writeFile(root, "Lib.csproj", csproj)

        let gen = SolutionGenerator()

        // First run generates
        let first = try gen.prepareBuild(options: PrepareBuildOptions(
            projectRoot: root, platform: .ios
        ))
        XCTAssertEqual(first.generatedCsprojs.count, 1)
        XCTAssertEqual(first.skippedCsprojs.count, 0)

        // Second run skips (suffixed file is newer)
        let second = try gen.prepareBuild(options: PrepareBuildOptions(
            projectRoot: root, platform: .ios
        ))
        XCTAssertEqual(second.generatedCsprojs.count, 0)
        XCTAssertEqual(second.skippedCsprojs.count, 1)
    }

    func testCategoryInferenceFromAsmDefFields() throws {
        let root = try makeTempProjectRoot()
        defer { try? FileManager.default.removeItem(at: root) }


        // Runtime: no special fields
        try writeFile(root, "Assets/A/Runtime.asmdef", "{\"name\":\"Runtime\"}\n")
        // Runtime: platform-specific (iOS + Editor)
        try writeFile(root, "Assets/B/PlatformLib.asmdef", """
        {"name":"PlatformLib","includePlatforms":["iOS","Editor"]}
        """)
        // Editor: includePlatforms = ["Editor"]
        try writeFile(root, "Assets/C/EditorOnly.asmdef", """
        {"name":"EditorOnly","includePlatforms":["Editor"]}
        """)
        // Editor: defineConstraints = ["UNITY_EDITOR"]
        try writeFile(root, "Assets/D/EditorConstrained.asmdef", """
        {"name":"EditorConstrained","defineConstraints":["UNITY_EDITOR"]}
        """)
        // Test: defineConstraints = ["UNITY_INCLUDE_TESTS"]
        try writeFile(root, "Assets/E/PlayTests.asmdef", """
        {"name":"PlayTests","defineConstraints":["UNITY_INCLUDE_TESTS"]}
        """)

        // Write csprojs so prepare-build can categorize
        let csproj = "<Project><PropertyGroup><DefineConstants>UNITY_5</DefineConstants></PropertyGroup></Project>"
        for name in ["Runtime", "PlatformLib", "EditorOnly", "EditorConstrained", "PlayTests"] {
            try writeFile(root, "\(name).csproj", csproj)
        }

        let gen = SolutionGenerator()
        let result = try gen.prepareBuild(options: PrepareBuildOptions(
            projectRoot: root, platform: .ios
        ))

        // Only Runtime and PlatformLib should be treated as runtime
        let generatedNames = Set(result.generatedCsprojs.map {
            String($0.prefix(while: { $0 != "." }))
        })
        XCTAssertEqual(generatedNames, ["Runtime", "PlatformLib"])
    }
}

private struct CompileEntry {
    let include: String
}
