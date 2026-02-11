// swift-tools-version: 6.2

import PackageDescription

let package = Package(
    name: "unity-solution-generator",
    platforms: [
        .macOS(.v13),
    ],
    products: [
        .executable(name: "unity-solution-generator", targets: ["unity-solution-generator"]),
    ],
    targets: [
        .executableTarget(
            name: "unity-solution-generator",
            path: "Sources"
        ),
        .testTarget(
            name: "SolutionGeneratorTests",
            dependencies: ["unity-solution-generator"],
            path: "Tests"
        ),
    ]
)
