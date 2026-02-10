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
    dependencies: [
        .package(url: "https://github.com/apple/swift-argument-parser", from: "1.5.0"),
    ],
    targets: [
        .executableTarget(
            name: "unity-solution-generator",
            dependencies: [
                .product(name: "ArgumentParser", package: "swift-argument-parser"),
            ],
            path: "Sources"
        ),
        .testTarget(
            name: "SolutionGeneratorTests",
            dependencies: ["unity-solution-generator"],
            path: "Tests"
        ),
    ]
)
