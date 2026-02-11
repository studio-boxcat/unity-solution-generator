import Foundation
import XCTest
@testable import unity_solution_generator

final class SolutionGeneratorTests: XCTestCase {
    private let generatorRoot = "tpl"

    func testNestedAssemblyRootMappingAndLegacyFallback() throws {
        let root = try makeTempProjectRoot()
        defer { try? FileManager.default.removeItem(atPath: root) }

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
        _ = try generator.generate(options: GenerateOptions(
            projectRoot: root, generatorRoot: generatorRoot, platform: .ios, buildConfig: .editor
        ))

        let variant = "tpl/ios-editor"

        try assertCompileSet(
            root: root,
            csprojPath: "\(variant)/Main.csproj",
            expected: [
                "Assets/Game/Foo.cs",
                "Assets/Game/Feature/SubFeature/Fizz.cs",
            ]
        )

        try assertCompileSet(
            root: root,
            csprojPath: "\(variant)/Core.csproj",
            expected: [
                "Assets/Game/Core/Bar.cs",
            ]
        )

        try assertCompileSet(
            root: root,
            csprojPath: "\(variant)/Tests.csproj",
            expected: [
                "Assets/Game/Tests/Baz.cs",
            ]
        )

        try assertCompileSet(
            root: root,
            csprojPath: "\(variant)/Assembly-CSharp-firstpass.csproj",
            expected: [
                "Assets/Plugins/Legacy.cs",
            ]
        )

        let main = try readFile(root, "\(variant)/Main.csproj")
        XCTAssertTrue(main.contains("<ProjectReference Include=\"Core.csproj\">"))

        let tests = try readFile(root, "\(variant)/Tests.csproj")
        XCTAssertTrue(tests.contains("<ProjectReference Include=\"Main.csproj\">"))
    }

    func testAsmRefNameResolutionAndTildeSkip() throws {
        let root = try makeTempProjectRoot()
        defer { try? FileManager.default.removeItem(atPath: root) }

        try writeTemplates(root: root, projectNames: ["Core"])

        try writeFile(root, "Assets/SystemAssets/Assemblies/Core/Core.asmdef", "{\"name\":\"Core\"}\n")
        try writeFile(root, "Assets/Game/Assembly.asmref", "{\"reference\":\"Core\"}\n")

        try writeFile(root, "Assets/Game/Good.cs", "class Good {}\n")
        try writeFile(root, "Packages/com.example/src~/Hidden.cs", "class Hidden {}\n")

        let generator = SolutionGenerator()
        _ = try generator.generate(options: GenerateOptions(
            projectRoot: root, generatorRoot: generatorRoot, platform: .ios, buildConfig: .editor
        ))

        try assertCompileSet(
            root: root,
            csprojPath: "tpl/ios-editor/Core.csproj",
            expected: ["Assets/Game/Good.cs"]
        )
    }

    func testTildeDirectoryExcludedFromScan() throws {
        let root = try makeTempProjectRoot()
        defer { try? FileManager.default.removeItem(atPath: root) }

        try writeTemplates(root: root, projectNames: ["Main"])

        try writeFile(root, "Assets/SystemAssets/Assemblies/Main/Main.asmdef", "{\"name\":\"Main\"}\n")
        try writeFile(root, "Assets/Game/Assembly.asmref", "{\"reference\":\"Main\"}\n")

        try writeFile(root, "Assets/Game/Good.cs", "class Good {}\n")
        try writeFile(root, "Assets/Game/src~/Hidden.cs", "class Hidden {}\n")
        try writeFile(root, "Assets/Game/backup~/Old.cs", "class Old {}\n")

        let generator = SolutionGenerator()
        _ = try generator.generate(options: GenerateOptions(
            projectRoot: root, generatorRoot: generatorRoot, platform: .ios, buildConfig: .editor
        ))

        try assertCompileSet(root: root, csprojPath: "tpl/ios-editor/Main.csproj", expected: ["Assets/Game/Good.cs"])
    }

    func testDotDirectoryExcludedFromScan() throws {
        let root = try makeTempProjectRoot()
        defer { try? FileManager.default.removeItem(atPath: root) }

        try writeTemplates(root: root, projectNames: ["Main"])

        try writeFile(root, "Assets/SystemAssets/Assemblies/Main/Main.asmdef", "{\"name\":\"Main\"}\n")
        try writeFile(root, "Assets/Game/Assembly.asmref", "{\"reference\":\"Main\"}\n")

        try writeFile(root, "Assets/Game/Visible.cs", "class Visible {}\n")
        try writeFile(root, "Assets/Game/.hidden/Secret.cs", "class Secret {}\n")

        let generator = SolutionGenerator()
        _ = try generator.generate(options: GenerateOptions(
            projectRoot: root, generatorRoot: generatorRoot, platform: .ios, buildConfig: .editor
        ))

        try assertCompileSet(root: root, csprojPath: "tpl/ios-editor/Main.csproj", expected: ["Assets/Game/Visible.cs"])
    }

    func testE2EGeneratedCompileSetMatchesOriginalCsproj() throws {
        let root = try makeTempProjectRoot()
        defer { try? FileManager.default.removeItem(atPath: root) }

        try writeTemplates(root: root, projectNames: ["Main", "Sandbox"])

        try writeFile(root, "Assets/SystemAssets/Assemblies/Main/Main.asmdef", "{\"name\":\"Main\"}\n")
        try writeFile(root, "Assets/SystemAssets/Assemblies/Sandbox/Sandbox.asmdef", "{\"name\":\"Sandbox\"}\n")
        try writeFile(root, "Assets/Game/Assembly.asmref", "{\"reference\":\"Main\"}\n")
        try writeFile(root, "Assets/Game/Sandbox/Assembly.asmref", "{\"reference\":\"Sandbox\"}\n")

        try writeFile(root, "Assets/Game/A.cs", "class A {}\n")
        try writeFile(root, "Assets/Game/Sub/B.cs", "class B {}\n")
        try writeFile(root, "Assets/Game/Tests/CTest.cs", "class CTest {}\n")
        try writeFile(root, "Assets/Game/Sandbox/S.cs", "class S {}\n")

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
        _ = try generator.generate(options: GenerateOptions(
            projectRoot: root, generatorRoot: generatorRoot, platform: .ios, buildConfig: .editor
        ))

        let variant = "tpl/ios-editor"

        let originalMain = try readCompileSet(root: root, csprojPath: "Main.original.csproj")
        let generatedMain = try readCompileSet(root: root, csprojPath: "\(variant)/Main.csproj")
        XCTAssertEqual(generatedMain, originalMain)

        let originalSandbox = try readCompileSet(root: root, csprojPath: "Sandbox.original.csproj")
        let generatedSandbox = try readCompileSet(root: root, csprojPath: "\(variant)/Sandbox.csproj")
        XCTAssertEqual(generatedSandbox, originalSandbox)
    }

    // MARK: - Setup helpers

    private func makeTempProjectRoot() throws -> String {
        let path = NSTemporaryDirectory() + "solution-generator-tests-\(UUID().uuidString)"
        try FileManager.default.createDirectory(atPath: path, withIntermediateDirectories: true)
        return path
    }

    private func writeTemplates(root: String, projectNames: [String], defines: String? = nil) throws {
        for name in projectNames {
            var content = "<Project>\n"
            if let defines {
                content += "  <PropertyGroup>\n    <DefineConstants>$(DefineConstants);\(defines)</DefineConstants>\n  </PropertyGroup>\n"
            }
            try writeFile(root, "tpl/templates/\(name).csproj.template", content)
        }
    }

    // MARK: - Assertion helpers

    private func assertCompileSet(root: String, csprojPath: String, expected: Set<String>) throws {
        let actual = try readCompileSet(root: root, csprojPath: csprojPath)
        XCTAssertEqual(actual, expected)
    }

    private func readCompileSet(root: String, csprojPath: String) throws -> Set<String> {
        let content = try readFile(root, csprojPath)
        let pattern = #"<Compile Include=\"([^\"]+)\"\s*/>"#
        guard let regex = try? NSRegularExpression(pattern: pattern) else { return [] }
        let range = NSRange(content.startIndex..<content.endIndex, in: content)

        var result: Set<String> = []
        for match in regex.matches(in: content, range: range) {
            guard let r = Range(match.range(at: 1), in: content) else { continue }
            result.formUnion(expandPattern(xmlUnescape(String(content[r])), root: root))
        }
        return result
    }

    private func expandPattern(_ pattern: String, root: String) -> Set<String> {
        var stripped = pattern
        while stripped.hasPrefix("../") {
            stripped = String(stripped.dropFirst(3))
        }

        if stripped.hasSuffix("/*.cs") {
            let directory = String(stripped.dropLast("/*.cs".count))
            return listCsFiles(root: root, relativeDirectory: directory)
        }

        if stripped.hasSuffix(".cs") {
            let path = stripped.replacingOccurrences(of: "\\", with: "/")
            let fullPath = "\(root)/\(path)"
            if FileManager.default.fileExists(atPath: fullPath) {
                return [path]
            }
            return []
        }

        return []
    }

    private func listCsFiles(root: String, relativeDirectory: String) -> Set<String> {
        let dirPath = relativeDirectory.isEmpty ? root : "\(root)/\(relativeDirectory)"

        guard FileManager.default.fileExists(atPath: dirPath) else {
            return []
        }

        guard let entries = try? FileManager.default.contentsOfDirectory(atPath: dirPath) else {
            return []
        }

        var result: Set<String> = []
        for entry in entries {
            guard entry.hasSuffix(".cs") else { continue }
            let relativePath = relativeDirectory.isEmpty ? entry : "\(relativeDirectory)/\(entry)"
            result.insert(relativePath)
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

    private func writeFile(_ root: String, _ relativePath: String, _ content: String) throws {
        let path = "\(root)/\(relativePath)"
        let dir = String(path[..<path.lastIndex(of: "/")!])
        try FileManager.default.createDirectory(atPath: dir, withIntermediateDirectories: true)
        try content.write(toFile: path, atomically: true, encoding: .utf8)
    }

    private func readFile(_ root: String, _ relativePath: String) throws -> String {
        try String(contentsOfFile: "\(root)/\(relativePath)", encoding: .utf8)
    }

    // MARK: - Platform variant integration tests

    func testProdVariantCategoryFiltering() throws {
        let root = try makeTempProjectRoot()
        defer { try? FileManager.default.removeItem(atPath: root) }

        try writeTemplates(root: root, projectNames: ["Runtime", "MyEditor", "MyTests"], defines: "UNITY_5")

        try writeFile(root, "Assets/Assemblies/Runtime/Runtime.asmdef", """
        {"name":"Runtime","references":["MyEditor"]}
        """)
        try writeFile(root, "Assets/Assemblies/MyEditor/MyEditor.asmdef", """
        {"name":"MyEditor","includePlatforms":["Editor"]}
        """)
        try writeFile(root, "Assets/Assemblies/MyTests/MyTests.asmdef", """
        {"name":"MyTests","defineConstraints":["UNITY_INCLUDE_TESTS"]}
        """)

        try writeFile(root, "Assets/Assemblies/Runtime/Foo.cs", "class Foo {}\n")
        try writeFile(root, "Assets/Assemblies/MyEditor/Bar.cs", "class Bar {}\n")
        try writeFile(root, "Assets/Assemblies/MyTests/Baz.cs", "class Baz {}\n")

        let gen = SolutionGenerator()

        let prodResult = try gen.generate(options: GenerateOptions(
            projectRoot: root, generatorRoot: generatorRoot, platform: .ios, buildConfig: .prod
        ))
        XCTAssertEqual(prodResult.variantCsprojs, ["tpl/ios-prod/Runtime.csproj"])

        let prodProps = try readFile(root, "tpl/ios-prod/Directory.Build.props")
        XCTAssertFalse(prodProps.contains("UNITY_EDITOR"))
        XCTAssertTrue(prodProps.contains("UNITY_IOS"))

        let variant = try readFile(root, "tpl/ios-prod/Runtime.csproj")
        XCTAssertFalse(variant.contains("MyEditor.csproj\">"))

        let prodSln = try readFile(root, prodResult.variantSlnPath)
        XCTAssertTrue(prodSln.contains("\"Runtime\""))
        XCTAssertFalse(prodSln.contains("\"MyEditor\""))

        let editorResult = try gen.generate(options: GenerateOptions(
            projectRoot: root, generatorRoot: generatorRoot, platform: .ios, buildConfig: .editor
        ))
        XCTAssertEqual(editorResult.variantCsprojs.count, 3)
        let editorProps = try readFile(root, "tpl/ios-editor/Directory.Build.props")
        XCTAssertTrue(editorProps.contains("UNITY_EDITOR"))

        let editorSln = try readFile(root, editorResult.variantSlnPath)
        XCTAssertTrue(editorSln.contains("\"MyEditor\""))
        XCTAssertTrue(editorSln.contains("\"MyTests\""))
    }

    func testCategoryInferenceFromAsmDefFields() throws {
        let root = try makeTempProjectRoot()
        defer { try? FileManager.default.removeItem(atPath: root) }

        try writeTemplates(root: root,
            projectNames: ["Runtime", "PlatformLib", "EditorOnly", "EditorConstrained", "PlayTests"],
            defines: "UNITY_5"
        )

        try writeFile(root, "Assets/A/Runtime.asmdef", "{\"name\":\"Runtime\"}\n")
        try writeFile(root, "Assets/B/PlatformLib.asmdef", """
        {"name":"PlatformLib","includePlatforms":["iOS","Editor"]}
        """)
        try writeFile(root, "Assets/C/EditorOnly.asmdef", """
        {"name":"EditorOnly","includePlatforms":["Editor"]}
        """)
        try writeFile(root, "Assets/D/EditorConstrained.asmdef", """
        {"name":"EditorConstrained","defineConstraints":["UNITY_EDITOR"]}
        """)
        try writeFile(root, "Assets/E/PlayTests.asmdef", """
        {"name":"PlayTests","defineConstraints":["UNITY_INCLUDE_TESTS"]}
        """)

        try writeFile(root, "Assets/A/Code.cs", "class Code {}\n")
        try writeFile(root, "Assets/B/Code.cs", "class Code2 {}\n")
        try writeFile(root, "Assets/C/Code.cs", "class Code3 {}\n")
        try writeFile(root, "Assets/D/Code.cs", "class Code4 {}\n")
        try writeFile(root, "Assets/E/Code.cs", "class Code5 {}\n")

        let gen = SolutionGenerator()

        let prodResult = try gen.generate(options: GenerateOptions(
            projectRoot: root, generatorRoot: generatorRoot, platform: .ios, buildConfig: .prod
        ))
        let prodNames = Set(prodResult.variantCsprojs.map {
            String($0.split(separator: "/").last!.dropLast(".csproj".count))
        })
        XCTAssertEqual(prodNames, ["Runtime", "PlatformLib"])

        let editorResult = try gen.generate(options: GenerateOptions(
            projectRoot: root, generatorRoot: generatorRoot, platform: .ios, buildConfig: .editor
        ))
        let editorNames = Set(editorResult.variantCsprojs.map {
            String($0.split(separator: "/").last!.dropLast(".csproj".count))
        })
        XCTAssertEqual(editorNames, ["Runtime", "PlatformLib", "EditorOnly", "EditorConstrained", "PlayTests"])
    }
}
