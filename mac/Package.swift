// swift-tools-version: 5.9
// M0 feasibility prototype. This package intentionally has no third-party dependencies.
// Build on a macOS GitHub runner; local builds are disabled by cockpit/AGENTS.md.
import PackageDescription

let package = Package(
    name: "CockpitMacPrototype",
    platforms: [.macOS(.v13)],
    products: [
        .executable(name: "cockpit-mac-prototype", targets: ["CockpitMacPrototype"]),
        .executable(name: "cockpit-probe", targets: ["CockpitProbe"])
    ],
    targets: [
        .target(name: "CockpitMacPrototypeCore", exclude: ["MediaCompression.swift"]),
        .executableTarget(name: "CockpitMacPrototype", dependencies: ["CockpitMacPrototypeCore"]),
        .executableTarget(name: "CockpitProbe"),
        .testTarget(name: "CockpitMacPrototypeCoreTests", dependencies: ["CockpitMacPrototypeCore"], exclude: ["MediaCompressionTests.swift"])
    ]
)
