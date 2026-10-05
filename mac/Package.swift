// swift-tools-version: 5.9
// M0 feasibility prototype. This package intentionally has no third-party dependencies.
// Build on a macOS GitHub runner; local builds are disabled by cockpit/AGENTS.md.
import PackageDescription

let package = Package(
    name: "CockpitMacPrototype",
    platforms: [.macOS(.v13)],
    products: [.executable(name: "cockpit-mac-prototype", targets: ["CockpitMacPrototype"])],
    targets: [
        .target(name: "CockpitMacPrototypeCore"),
        .executableTarget(name: "CockpitMacPrototype", dependencies: ["CockpitMacPrototypeCore"]),
        .testTarget(name: "CockpitMacPrototypeCoreTests", dependencies: ["CockpitMacPrototypeCore"])
    ]
)
