// swift-tools-version: 6.2

import PackageDescription

let package = Package(
    name: "unity-solution-generator",
    platforms: [
        .macOS(.v13),
    ],
    products: [
        .executable(name: "unity-solution-generator", targets: ["unity-solution-generator"]),
        .library(name: "UnitySolutionGenerator", type: .dynamic, targets: ["SolutionGeneratorCore"]),
    ],
    targets: [
        .target(
            name: "SolutionGeneratorCore",
            path: "Sources",
            exclude: ["CLI"]
        ),
        .executableTarget(
            name: "unity-solution-generator",
            dependencies: ["SolutionGeneratorCore"],
            path: "Sources/CLI"
        ),
        .testTarget(
            name: "SolutionGeneratorTests",
            dependencies: ["SolutionGeneratorCore"],
            path: "Tests"
        ),
    ]
)
