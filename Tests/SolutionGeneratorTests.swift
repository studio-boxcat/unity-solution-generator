import Dispatch
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

    // MARK: - Lockfile tests

    func testLockfileRoundTrip() throws {
        let lockfile = Lockfile(
            unityVersion: "6000.2.7f2",
            unityPath: "/Applications/Unity/Hub/Editor/6000.2.7f2",
            langVersion: "9.0",
            analyzers: [
                "$(UnityPath)/Unity.app/Contents/Tools/Unity.SourceGenerators/Unity.SourceGenerators.dll",
                "$(ProjectRoot)/Assets/Zenject.Analyzers.dll",
            ],
            refsEngine: [
                DllRef(name: "UnityEngine", path: "$(UnityPath)/Unity.app/Contents/Managed/UnityEngine/UnityEngine.dll"),
                DllRef(name: "UnityEngine.CoreModule", path: "$(UnityPath)/Unity.app/Contents/Managed/UnityEngine/UnityEngine.CoreModule.dll"),
            ],
            refsEditor: [
                DllRef(name: "UnityEditor", path: "$(UnityPath)/Unity.app/Contents/Managed/UnityEngine/UnityEditor.dll"),
            ],
            refsNetstandard: [
                DllRef(name: "netstandard", path: "$(UnityPath)/Unity.app/Contents/NetStandard/ref/2.1.0/netstandard.dll"),
                DllRef(name: "System.Collections", path: "$(UnityPath)/Unity.app/Contents/NetStandard/compat/2.1.0/shims/netstandard/System.Collections.dll"),
            ],
            refsPlaybackIos: [
                DllRef(name: "UnityEditor.iOS.Extensions", path: "$(UnityPath)/PlaybackEngines/iOSSupport/UnityEditor.iOS.Extensions.dll"),
            ],
            refsPlaybackAndroid: [
                DllRef(name: "UnityEditor.Android.Extensions", path: "$(UnityPath)/PlaybackEngines/AndroidPlayer/UnityEditor.Android.Extensions.dll"),
            ],
            refsPlaybackStandalone: [
                DllRef(name: "UnityEditor.OSXStandalone.Extensions", path: "$(UnityPath)/Unity.app/Contents/PlaybackEngines/MacStandaloneSupport/UnityEditor.OSXStandalone.Extensions.dll"),
            ],
            refsProject: [
                DllRef(name: "Firebase.App", path: "$(ProjectRoot)/Packages/com.google.firebase.app-pkg/Firebase/Plugins/Firebase.App.dll"),
            ],
            defines: ["UNITY_6000_2_7", "UNITY_6000", "ENABLE_AR"],
            definesScripting: ["ODIN_INSPECTOR", "SINGULAR_SDK_IAP_ENABLED"]
        )

        let root = try makeTempProjectRoot()
        defer { try? FileManager.default.removeItem(atPath: root) }

        let lockfilePath = "\(root)/csproj.lock"
        try LockfileIO.write(lockfile, to: lockfilePath)
        let reloaded = try LockfileIO.read(from: lockfilePath)

        XCTAssertEqual(reloaded.unityVersion, lockfile.unityVersion)
        XCTAssertEqual(reloaded.unityPath, lockfile.unityPath)
        XCTAssertEqual(reloaded.langVersion, lockfile.langVersion)
        XCTAssertEqual(reloaded.analyzers, lockfile.analyzers)
        XCTAssertEqual(reloaded.refsEngine.count, lockfile.refsEngine.count)
        XCTAssertEqual(reloaded.refsEngine.map(\.name), lockfile.refsEngine.map(\.name))
        XCTAssertEqual(reloaded.refsEngine.map(\.path), lockfile.refsEngine.map(\.path))
        XCTAssertEqual(reloaded.refsEditor.map(\.name), lockfile.refsEditor.map(\.name))
        XCTAssertEqual(reloaded.refsNetstandard.count, lockfile.refsNetstandard.count)
        XCTAssertEqual(reloaded.refsPlaybackIos.map(\.name), lockfile.refsPlaybackIos.map(\.name))
        XCTAssertEqual(reloaded.refsPlaybackAndroid.map(\.name), lockfile.refsPlaybackAndroid.map(\.name))
        XCTAssertEqual(reloaded.refsPlaybackStandalone.map(\.name), lockfile.refsPlaybackStandalone.map(\.name))
        XCTAssertEqual(reloaded.refsProject.map(\.name), lockfile.refsProject.map(\.name))
        XCTAssertEqual(reloaded.defines, lockfile.defines)
        XCTAssertEqual(reloaded.definesScripting, lockfile.definesScripting)

        // Write again and verify idempotency
        let firstWrite = try String(contentsOfFile: lockfilePath, encoding: .utf8)
        let secondPath = "\(root)/csproj2.lock"
        try LockfileIO.write(reloaded, to: secondPath)
        let secondWrite = try String(contentsOfFile: secondPath, encoding: .utf8)
        XCTAssertEqual(firstWrite, secondWrite)
    }

    func testVersionDefinesGeneration() throws {
        let defines = generateVersionDefines(version: "6000.2.7f2")

        // Exact version defines
        XCTAssertTrue(defines.contains("UNITY_6000_2_7"))
        XCTAssertTrue(defines.contains("UNITY_6000_2"))
        XCTAssertTrue(defines.contains("UNITY_6000"))

        // OR_NEWER chain must be contiguous (no gaps)
        XCTAssertTrue(defines.contains("UNITY_5_3_OR_NEWER"))
        XCTAssertTrue(defines.contains("UNITY_5_6_OR_NEWER"))
        XCTAssertTrue(defines.contains("UNITY_2017_1_OR_NEWER"))
        XCTAssertTrue(defines.contains("UNITY_2022_3_OR_NEWER"))
        XCTAssertTrue(defines.contains("UNITY_2023_3_OR_NEWER"))
        XCTAssertTrue(defines.contains("UNITY_6000_0_OR_NEWER"))
        XCTAssertTrue(defines.contains("UNITY_6000_1_OR_NEWER"))
        XCTAssertTrue(defines.contains("UNITY_6000_2_OR_NEWER"))

        // Should NOT have future versions
        XCTAssertFalse(defines.contains("UNITY_6000_3_OR_NEWER"))

        // Verify no duplicates
        XCTAssertEqual(defines.count, Set(defines).count)
    }

    func testVersionDefinesForOlderVersion() throws {
        let defines = generateVersionDefines(version: "2022.3.10f1")

        XCTAssertTrue(defines.contains("UNITY_2022_3_10"))
        XCTAssertTrue(defines.contains("UNITY_2022_3"))
        XCTAssertTrue(defines.contains("UNITY_2022"))
        XCTAssertTrue(defines.contains("UNITY_2022_3_OR_NEWER"))
        XCTAssertTrue(defines.contains("UNITY_2022_1_OR_NEWER"))
        XCTAssertTrue(defines.contains("UNITY_5_3_OR_NEWER"))

        // Should NOT have Unity 6000+ defines
        XCTAssertFalse(defines.contains("UNITY_6000_0_OR_NEWER"))
        XCTAssertFalse(defines.contains("UNITY_2023_1_OR_NEWER"))
    }

    func testScriptingDefinesParsing() throws {
        let root = try makeTempProjectRoot()
        defer { try? FileManager.default.removeItem(atPath: root) }

        try writeFile(root, "ProjectSettings/ProjectSettings.asset", """
        %YAML 1.1
        %TAG !u! tag:unity3d.com,2011:
        --- !u!129 &1
        PlayerSettings:
          scriptingDefineSymbols:
            Android: ENABLE_SPAN_T;ODIN_INSPECTOR;CUSTOM_DEFINE
            iPhone: ENABLE_SPAN_T;ODIN_INSPECTOR
            Standalone: ENABLE_SPAN_T
          someOtherSetting: true
        """)

        let defines = parseScriptingDefines(projectRoot: root)

        XCTAssertTrue(defines.contains("ENABLE_SPAN_T"))
        XCTAssertTrue(defines.contains("ODIN_INSPECTOR"))
        XCTAssertTrue(defines.contains("CUSTOM_DEFINE"))
        // Union of all platforms
        XCTAssertEqual(defines.count, 3)
    }

    func testLockfileGenerateReferencesInCsproj() throws {
        let root = try makeTempProjectRoot()
        defer { try? FileManager.default.removeItem(atPath: root) }

        let lockfile = makeMinimalLockfile()
        let lockfilePath = "\(root)/\(generatorRoot)/csproj.lock"
        try FileManager.default.createDirectory(
            atPath: "\(root)/\(generatorRoot)",
            withIntermediateDirectories: true
        )
        try LockfileIO.write(lockfile, to: lockfilePath)

        try writeFile(root, "Assets/Assemblies/Main/Main.asmdef", """
        {"name":"Main","references":["Lib"]}
        """)
        try writeFile(root, "Assets/Assemblies/Lib/Lib.asmdef", """
        {"name":"Lib"}
        """)
        try writeFile(root, "Assets/Assemblies/Main/Foo.cs", "class Foo {}\n")
        try writeFile(root, "Assets/Assemblies/Lib/Bar.cs", "class Bar {}\n")

        let result = try SolutionGenerator().generateFromLockfile(
            options: GenerateOptions(
                projectRoot: root, generatorRoot: generatorRoot,
                platform: .ios, buildConfig: .editor
            ),
            lockfile: lockfile
        )

        XCTAssertEqual(result.variantCsprojs.count, 2)

        let mainCsproj = try readFile(root, "\(generatorRoot)/ios-editor/Main.csproj")

        // Engine refs present
        XCTAssertTrue(mainCsproj.contains("<Reference Include=\"UnityEngine\">"))
        XCTAssertTrue(mainCsproj.contains("<Reference Include=\"UnityEngine.CoreModule\">"))
        // Editor refs present (editor config)
        XCTAssertTrue(mainCsproj.contains("<Reference Include=\"UnityEditor\">"))
        // NetStandard refs present
        XCTAssertTrue(mainCsproj.contains("<Reference Include=\"netstandard\">"))
        // Analyzer present
        XCTAssertTrue(mainCsproj.contains("<Analyzer Include="))
        // Project reference present
        XCTAssertTrue(mainCsproj.contains("<ProjectReference Include=\"Lib.csproj\">"))
        // Source patterns present
        XCTAssertTrue(mainCsproj.contains("<Compile Include="))
        // iOS playback present
        XCTAssertTrue(mainCsproj.contains("UnityEditor.iOS.Extensions"))
        // Android playback absent (platform is ios)
        XCTAssertFalse(mainCsproj.contains("UnityEditor.Android.Extensions"))
    }

    func testLockfileGenerateEditorRefsNotInProd() throws {
        let root = try makeTempProjectRoot()
        defer { try? FileManager.default.removeItem(atPath: root) }

        let lockfile = makeMinimalLockfile()

        try writeFile(root, "Assets/Assemblies/Runtime/Runtime.asmdef", """
        {"name":"Runtime"}
        """)
        try writeFile(root, "Assets/Assemblies/Runtime/Code.cs", "class Code {}\n")

        let prodResult = try SolutionGenerator().generateFromLockfile(
            options: GenerateOptions(
                projectRoot: root, generatorRoot: generatorRoot,
                platform: .android, buildConfig: .prod
            ),
            lockfile: lockfile
        )

        XCTAssertEqual(prodResult.variantCsprojs.count, 1)
        let csproj = try readFile(root, "\(generatorRoot)/android-prod/Runtime.csproj")

        // Engine refs present
        XCTAssertTrue(csproj.contains("<Reference Include=\"UnityEngine\">"))
        // Editor refs ABSENT in prod
        XCTAssertFalse(csproj.contains("<Reference Include=\"UnityEditor\">"))
        // Android playback present
        XCTAssertTrue(csproj.contains("UnityEditor.Android.Extensions"))
        // iOS playback absent
        XCTAssertFalse(csproj.contains("UnityEditor.iOS.Extensions"))
    }

    func testLockfileGenerateAllowUnsafeBlocks() throws {
        let root = try makeTempProjectRoot()
        defer { try? FileManager.default.removeItem(atPath: root) }

        let lockfile = makeMinimalLockfile()

        try writeFile(root, "Assets/Assemblies/SafeLib/SafeLib.asmdef", """
        {"name":"SafeLib"}
        """)
        try writeFile(root, "Assets/Assemblies/UnsafeLib/UnsafeLib.asmdef", """
        {"name":"UnsafeLib","allowUnsafeCode":true}
        """)
        try writeFile(root, "Assets/Assemblies/SafeLib/S.cs", "class S {}\n")
        try writeFile(root, "Assets/Assemblies/UnsafeLib/U.cs", "class U {}\n")

        _ = try SolutionGenerator().generateFromLockfile(
            options: GenerateOptions(
                projectRoot: root, generatorRoot: generatorRoot,
                platform: .ios, buildConfig: .editor
            ),
            lockfile: lockfile
        )

        let safeCsproj = try readFile(root, "\(generatorRoot)/ios-editor/SafeLib.csproj")
        XCTAssertTrue(safeCsproj.contains("<AllowUnsafeBlocks>False</AllowUnsafeBlocks>"))

        let unsafeCsproj = try readFile(root, "\(generatorRoot)/ios-editor/UnsafeLib.csproj")
        XCTAssertTrue(unsafeCsproj.contains("<AllowUnsafeBlocks>True</AllowUnsafeBlocks>"))
    }

    func testLockfileGenerateDefinesInProps() throws {
        let root = try makeTempProjectRoot()
        defer { try? FileManager.default.removeItem(atPath: root) }

        let lockfile = Lockfile(
            unityVersion: "6000.2.7f2",
            unityPath: "/test/unity",
            langVersion: "9.0",
            analyzers: [],
            refsEngine: [], refsEditor: [], refsNetstandard: [],
            refsPlaybackIos: [], refsPlaybackAndroid: [], refsPlaybackStandalone: [],
            refsProject: [],
            defines: ["UNITY_6000", "ENABLE_AR"],
            definesScripting: ["ODIN_INSPECTOR"]
        )

        try writeFile(root, "Assets/Assemblies/Lib/Lib.asmdef", "{\"name\":\"Lib\"}\n")
        try writeFile(root, "Assets/Assemblies/Lib/Code.cs", "class Code {}\n")

        _ = try SolutionGenerator().generateFromLockfile(
            options: GenerateOptions(
                projectRoot: root, generatorRoot: generatorRoot,
                platform: .ios, buildConfig: .editor
            ),
            lockfile: lockfile
        )

        let props = try readFile(root, "\(generatorRoot)/ios-editor/Directory.Build.props")

        // Static defines from lockfile
        XCTAssertTrue(props.contains("UNITY_6000"))
        XCTAssertTrue(props.contains("ENABLE_AR"))
        XCTAssertTrue(props.contains("ODIN_INSPECTOR"))
        // Dynamic per-variant defines
        XCTAssertTrue(props.contains("UNITY_IOS"))
        XCTAssertTrue(props.contains("UNITY_EDITOR"))
        XCTAssertTrue(props.contains("DEBUG"))
    }

    func testLockfileGenerateSourcePatternsPreserved() throws {
        let root = try makeTempProjectRoot()
        defer { try? FileManager.default.removeItem(atPath: root) }

        let lockfile = makeMinimalLockfile()

        try writeFile(root, "Assets/Assemblies/Main/Main.asmdef", "{\"name\":\"Main\"}\n")
        try writeFile(root, "Assets/Game/Assembly.asmref", "{\"reference\":\"Main\"}\n")
        try writeFile(root, "Assets/Assemblies/Main/A.cs", "class A {}\n")
        try writeFile(root, "Assets/Game/B.cs", "class B {}\n")
        try writeFile(root, "Assets/Game/Sub/C.cs", "class C {}\n")

        _ = try SolutionGenerator().generateFromLockfile(
            options: GenerateOptions(
                projectRoot: root, generatorRoot: generatorRoot,
                platform: .ios, buildConfig: .editor
            ),
            lockfile: lockfile
        )

        try assertCompileSet(
            root: root,
            csprojPath: "\(generatorRoot)/ios-editor/Main.csproj",
            expected: [
                "Assets/Assemblies/Main/A.cs",
                "Assets/Game/B.cs",
                "Assets/Game/Sub/C.cs",
            ]
        )
    }

    func testLockfileGeneratePlatformFiltering() throws {
        let root = try makeTempProjectRoot()
        defer { try? FileManager.default.removeItem(atPath: root) }

        let lockfile = makeMinimalLockfile()

        try writeFile(root, "Assets/Assemblies/IOSOnly/IOSOnly.asmdef", """
        {"name":"IOSOnly","includePlatforms":["iOS"]}
        """)
        try writeFile(root, "Assets/Assemblies/AllPlatforms/AllPlatforms.asmdef", """
        {"name":"AllPlatforms"}
        """)
        try writeFile(root, "Assets/Assemblies/IOSOnly/Code.cs", "class IOSCode {}\n")
        try writeFile(root, "Assets/Assemblies/AllPlatforms/Code.cs", "class AllCode {}\n")

        // iOS prod should include both
        let iosResult = try SolutionGenerator().generateFromLockfile(
            options: GenerateOptions(
                projectRoot: root, generatorRoot: generatorRoot,
                platform: .ios, buildConfig: .prod
            ),
            lockfile: lockfile
        )
        let iosNames = Set(iosResult.variantCsprojs.map {
            String($0.split(separator: "/").last!.dropLast(".csproj".count))
        })
        XCTAssertTrue(iosNames.contains("IOSOnly"))
        XCTAssertTrue(iosNames.contains("AllPlatforms"))

        // Android prod should exclude IOSOnly
        let androidResult = try SolutionGenerator().generateFromLockfile(
            options: GenerateOptions(
                projectRoot: root, generatorRoot: generatorRoot,
                platform: .android, buildConfig: .prod
            ),
            lockfile: lockfile
        )
        let androidNames = Set(androidResult.variantCsprojs.map {
            String($0.split(separator: "/").last!.dropLast(".csproj".count))
        })
        XCTAssertFalse(androidNames.contains("IOSOnly"))
        XCTAssertTrue(androidNames.contains("AllPlatforms"))
    }

    func testLockfileGenerateAsmdefVersionDefines() throws {
        let root = try makeTempProjectRoot()
        defer { try? FileManager.default.removeItem(atPath: root) }

        // Test that asmdef versionDefines are parsed
        try writeFile(root, "Assets/Assemblies/Lib/Lib.asmdef", """
        {"name":"Lib","versionDefines":[{"name":"com.unity.modules.physics2d","expression":"","define":"PACKAGE_PHYSICS2D"},{"name":"Unity","expression":"","define":"MY_FEATURE"}]}
        """)
        try writeFile(root, "Assets/Assemblies/Lib/Code.cs", "class Code {}\n")

        let scan = try ProjectScanner.scan(projectRoot: root)
        let asmDef = scan.asmDefByName["Lib"]!

        XCTAssertEqual(asmDef.versionDefines.count, 2)
        XCTAssertEqual(asmDef.versionDefines[0].packageName, "com.unity.modules.physics2d")
        XCTAssertEqual(asmDef.versionDefines[0].define, "PACKAGE_PHYSICS2D")
        XCTAssertEqual(asmDef.versionDefines[1].define, "MY_FEATURE")
    }

    func testLockfileGenerateCsprojXmlWellFormed() throws {
        let root = try makeTempProjectRoot()
        defer { try? FileManager.default.removeItem(atPath: root) }

        let lockfile = makeMinimalLockfile()

        try writeFile(root, "Assets/Assemblies/Main/Main.asmdef", "{\"name\":\"Main\"}\n")
        try writeFile(root, "Assets/Assemblies/Main/Code.cs", "class Code {}\n")

        _ = try SolutionGenerator().generateFromLockfile(
            options: GenerateOptions(
                projectRoot: root, generatorRoot: generatorRoot,
                platform: .ios, buildConfig: .editor
            ),
            lockfile: lockfile
        )

        let csproj = try readFile(root, "\(generatorRoot)/ios-editor/Main.csproj")

        // Check essential XML structure
        XCTAssertTrue(csproj.hasPrefix("<?xml version=\"1.0\""))
        XCTAssertTrue(csproj.contains("<Project ToolsVersion=\"4.0\""))
        XCTAssertTrue(csproj.contains("</Project>"))
        XCTAssertTrue(csproj.contains("<AssemblyName>Main</AssemblyName>"))
        XCTAssertTrue(csproj.contains("<LangVersion>9.0</LangVersion>"))
        XCTAssertTrue(csproj.contains("<TargetFrameworkVersion>v4.7.1</TargetFrameworkVersion>"))
        XCTAssertTrue(csproj.contains("<NoStdLib>true</NoStdLib>"))
        XCTAssertTrue(csproj.contains("<Import Project=\"$(MSBuildToolsPath)"))

        // Verify all opening tags have closing tags
        let openItemGroups = csproj.components(separatedBy: "<ItemGroup>").count - 1
        let closeItemGroups = csproj.components(separatedBy: "</ItemGroup>").count - 1
        XCTAssertEqual(openItemGroups, closeItemGroups)
    }

    // MARK: - Performance tests

    func testLockfileGeneratePerformance() throws {
        let root = try makeTempProjectRoot()
        defer { try? FileManager.default.removeItem(atPath: root) }

        let lockfile = makeMinimalLockfile()

        // Create 20 assemblies with source files
        for i in 0..<20 {
            let name = "Assembly\(i)"
            try writeFile(root, "Assets/Assemblies/\(name)/\(name).asmdef", "{\"name\":\"\(name)\"}\n")
            for j in 0..<50 {
                try writeFile(root, "Assets/Assemblies/\(name)/File\(j).cs", "class C\(i)_\(j) {}\n")
            }
        }

        let options = GenerateOptions(
            projectRoot: root, generatorRoot: generatorRoot,
            platform: .ios, buildConfig: .editor
        )

        // Warm up
        _ = try SolutionGenerator().generateFromLockfile(options: options, lockfile: lockfile)

        let start = DispatchTime.now()
        let iterations = 10
        for _ in 0..<iterations {
            _ = try SolutionGenerator().generateFromLockfile(options: options, lockfile: lockfile)
        }
        let elapsed = Double(DispatchTime.now().uptimeNanoseconds - start.uptimeNanoseconds) / 1_000_000
        let perIteration = elapsed / Double(iterations)

        // 20 assemblies x 50 files should generate in < 100ms per iteration
        XCTAssertLessThan(perIteration, 100.0, "generateFromLockfile took \(perIteration)ms per call (20 assemblies, 50 files each)")
    }

    // MARK: - Lockfile test helpers

    private func makeMinimalLockfile() -> Lockfile {
        Lockfile(
            unityVersion: "6000.2.7f2",
            unityPath: "/test/unity",
            langVersion: "9.0",
            analyzers: [
                "$(UnityPath)/Unity.app/Contents/Tools/Unity.SourceGenerators/Unity.SourceGenerators.dll",
            ],
            refsEngine: [
                DllRef(name: "UnityEngine", path: "$(UnityPath)/Unity.app/Contents/Managed/UnityEngine/UnityEngine.dll"),
                DllRef(name: "UnityEngine.CoreModule", path: "$(UnityPath)/Unity.app/Contents/Managed/UnityEngine/UnityEngine.CoreModule.dll"),
            ],
            refsEditor: [
                DllRef(name: "UnityEditor", path: "$(UnityPath)/Unity.app/Contents/Managed/UnityEngine/UnityEditor.dll"),
            ],
            refsNetstandard: [
                DllRef(name: "netstandard", path: "$(UnityPath)/Unity.app/Contents/NetStandard/ref/2.1.0/netstandard.dll"),
            ],
            refsPlaybackIos: [
                DllRef(name: "UnityEditor.iOS.Extensions", path: "$(UnityPath)/PlaybackEngines/iOSSupport/UnityEditor.iOS.Extensions.dll"),
            ],
            refsPlaybackAndroid: [
                DllRef(name: "UnityEditor.Android.Extensions", path: "$(UnityPath)/PlaybackEngines/AndroidPlayer/UnityEditor.Android.Extensions.dll"),
            ],
            refsPlaybackStandalone: [],
            refsProject: [
                DllRef(name: "Firebase.App", path: "$(ProjectRoot)/Packages/com.google.firebase.app-pkg/Firebase/Plugins/Firebase.App.dll"),
            ],
            defines: ["UNITY_6000", "ENABLE_AR"],
            definesScripting: ["ODIN_INSPECTOR"]
        )
    }

    // MARK: - Regression tests

    /// Lockfile generate must produce the same project set and source assignments as template generate.
    func testLockfileAndTemplateProduceSameProjects() throws {
        let root = try makeTempProjectRoot()
        defer { try? FileManager.default.removeItem(atPath: root) }

        let lockfile = makeMinimalLockfile()
        let templateRoot = "tpl-template"
        let lockfileRoot = "tpl-lockfile"

        // Write templates for the template-based path
        for name in ["Runtime", "EditorLib", "Tests"] {
            try writeFile(root, "\(templateRoot)/templates/\(name).csproj.template", "<Project>\n")
        }

        try writeFile(root, "Assets/A/Runtime.asmdef", "{\"name\":\"Runtime\",\"references\":[\"EditorLib\"]}\n")
        try writeFile(root, "Assets/B/EditorLib.asmdef", "{\"name\":\"EditorLib\",\"includePlatforms\":[\"Editor\"]}\n")
        try writeFile(root, "Assets/C/Tests.asmdef", "{\"name\":\"Tests\",\"defineConstraints\":[\"UNITY_INCLUDE_TESTS\"]}\n")
        try writeFile(root, "Assets/A/Code.cs", "class A {}\n")
        try writeFile(root, "Assets/B/Code.cs", "class B {}\n")
        try writeFile(root, "Assets/C/Code.cs", "class C {}\n")

        let gen = SolutionGenerator()

        // Template-based (writes to tpl-template/ios-prod/)
        let templateResult = try gen.generate(options: GenerateOptions(
            projectRoot: root, generatorRoot: templateRoot, platform: .ios, buildConfig: .prod
        ))
        // Lockfile-based (writes to tpl-lockfile/ios-prod/)
        let lockfileResult = try gen.generateFromLockfile(options: GenerateOptions(
            projectRoot: root, generatorRoot: lockfileRoot, platform: .ios, buildConfig: .prod
        ), lockfile: lockfile)

        // Same project set
        let templateNames = Set(templateResult.variantCsprojs.map {
            String($0.split(separator: "/").last!)
        })
        let lockfileNames = Set(lockfileResult.variantCsprojs.map {
            String($0.split(separator: "/").last!)
        })
        XCTAssertEqual(templateNames, lockfileNames)

        // Same compile sets (read from separate output directories)
        for name in templateNames {
            let templateSources = try readCompileSet(root: root, csprojPath: "\(templateRoot)/ios-prod/\(name)")
            let lockfileSources = try readCompileSet(root: root, csprojPath: "\(lockfileRoot)/ios-prod/\(name)")
            XCTAssertEqual(templateSources, lockfileSources, "Compile set mismatch for \(name)")
        }
    }

    /// Template-based generate must still work after the refactor (shared scaffolding).
    func testTemplateLegacyPathStillWorks() throws {
        let root = try makeTempProjectRoot()
        defer { try? FileManager.default.removeItem(atPath: root) }

        try writeTemplates(root: root, projectNames: ["Main"], defines: "MY_DEFINE")

        try writeFile(root, "Assets/Assemblies/Main/Main.asmdef", "{\"name\":\"Main\"}\n")
        try writeFile(root, "Assets/Assemblies/Main/Code.cs", "class Code {}\n")

        let result = try SolutionGenerator().generate(options: GenerateOptions(
            projectRoot: root, generatorRoot: generatorRoot, platform: .android, buildConfig: .dev
        ))

        XCTAssertEqual(result.variantCsprojs.count, 1)

        let csproj = try readFile(root, "\(generatorRoot)/android-dev/Main.csproj")
        XCTAssertTrue(csproj.contains("<Compile Include="))
        XCTAssertTrue(csproj.contains("</Project>"))

        let props = try readFile(root, "\(generatorRoot)/android-dev/Directory.Build.props")
        XCTAssertTrue(props.contains("UNITY_ANDROID"))
        XCTAssertTrue(props.contains("DEBUG"))
        XCTAssertFalse(props.contains("UNITY_EDITOR"))
        // Template path should NOT have UnityPath
        XCTAssertFalse(props.contains("<UnityPath>"))
    }

    /// extractJsonObjectKeys must return keys, not values.
    func testExtractJsonObjectKeys() throws {
        let json = """
        {
          "dependencies": {
            "com.unity.modules.audio": "1.0.0",
            "com.unity.modules.physics2d": "2.0.0",
            "singular-unity-package": "3.1.0"
          }
        }
        """
        let keys = extractJsonObjectKeys(json, key: "dependencies")
        XCTAssertEqual(Set(keys), ["com.unity.modules.audio", "com.unity.modules.physics2d", "singular-unity-package"])
    }

    func testIsDirectoryHelper() throws {
        let root = try makeTempProjectRoot()
        defer { try? FileManager.default.removeItem(atPath: root) }

        try writeFile(root, "test.txt", "content")
        XCTAssertTrue(isDirectory(root))
        XCTAssertFalse(isDirectory("\(root)/test.txt"))
        XCTAssertFalse(isDirectory("\(root)/nonexistent"))
    }

    func testScanCacheInvalidatesOnNewCsFile() throws {
        let root = try makeTempProjectRoot()
        defer { try? FileManager.default.removeItem(atPath: root) }

        let lockfile = makeMinimalLockfile()

        try writeFile(root, "Assets/Assemblies/Main/Main.asmdef", "{\"name\":\"Main\"}\n")
        try writeFile(root, "Assets/Assemblies/Main/A.cs", "class A {}\n")

        // First generate populates scan cache
        _ = try SolutionGenerator().generateFromLockfile(
            options: GenerateOptions(projectRoot: root, generatorRoot: generatorRoot, platform: .ios, buildConfig: .editor),
            lockfile: lockfile
        )
        try assertCompileSet(root: root, csprojPath: "\(generatorRoot)/ios-editor/Main.csproj", expected: ["Assets/Assemblies/Main/A.cs"])

        // Add a new .cs file in a previously empty dir (the cache bug scenario)
        try writeFile(root, "Assets/Assemblies/Main/Sub/B.cs", "class B {}\n")

        // Second generate must pick up the new file
        _ = try SolutionGenerator().generateFromLockfile(
            options: GenerateOptions(projectRoot: root, generatorRoot: generatorRoot, platform: .ios, buildConfig: .editor),
            lockfile: lockfile
        )
        try assertCompileSet(root: root, csprojPath: "\(generatorRoot)/ios-editor/Main.csproj", expected: [
            "Assets/Assemblies/Main/A.cs",
            "Assets/Assemblies/Main/Sub/B.cs",
        ])
    }

    /// ProjectRoot in Directory.Build.props must be an absolute path, not relative.
    func testProjectRootIsAbsoluteInProps() throws {
        let root = try makeTempProjectRoot()
        defer { try? FileManager.default.removeItem(atPath: root) }

        let lockfile = makeMinimalLockfile()
        try writeFile(root, "Assets/Assemblies/Lib/Lib.asmdef", "{\"name\":\"Lib\"}\n")
        try writeFile(root, "Assets/Assemblies/Lib/Code.cs", "class Code {}\n")

        _ = try SolutionGenerator().generateFromLockfile(
            options: GenerateOptions(projectRoot: root, generatorRoot: generatorRoot, platform: .ios, buildConfig: .editor),
            lockfile: lockfile
        )

        let props = try readFile(root, "\(generatorRoot)/ios-editor/Directory.Build.props")
        // Must contain an absolute path, not "." or a relative path
        XCTAssertTrue(props.contains("<ProjectRoot>/"), "ProjectRoot must be absolute, got: \(props)")
    }

    func testBuildPlatformUnityName() throws {
        XCTAssertEqual(BuildPlatform.ios.unityPlatformName, "iOS")
        XCTAssertEqual(BuildPlatform.android.unityPlatformName, "Android")
    }

    /// Merged renderDirectoryBuildProps must handle both lockfile (with unityPath) and template (without) paths.
    func testRenderDirectoryBuildPropsUnified() throws {
        // With unityPath (lockfile path)
        let withUnity = SolutionGenerator.renderDirectoryBuildProps(
            projectRoot: "/project",
            unityPath: "/unity",
            platform: .ios,
            buildConfig: .editor,
            staticDefines: ["CUSTOM"]
        )
        XCTAssertTrue(withUnity.contains("<UnityPath>/unity</UnityPath>"))
        XCTAssertTrue(withUnity.contains("CUSTOM"))
        XCTAssertTrue(withUnity.contains("UNITY_IOS"))
        XCTAssertTrue(withUnity.contains("UNITY_EDITOR"))

        // Without unityPath (template path)
        let withoutUnity = SolutionGenerator.renderDirectoryBuildProps(
            projectRoot: "/project",
            platform: .android,
            buildConfig: .prod
        )
        XCTAssertFalse(withoutUnity.contains("<UnityPath>"))
        XCTAssertTrue(withoutUnity.contains("UNITY_ANDROID"))
        XCTAssertFalse(withoutUnity.contains("UNITY_EDITOR"))
    }

    // MARK: - Legacy template tests

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
