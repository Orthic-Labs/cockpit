// swift-tools-version: 5.9
// Native Mac services kept for salvage review (docs/plan.md); the notch is being replaced by a Codenotch fork. This package intentionally has no third-party dependencies.
// Build on a macOS GitHub runner; local builds are disabled by cockpit/AGENTS.md.
import PackageDescription

let package = Package(
    name: "CockpitMacPrototype",
    platforms: [.macOS(.v13)],
    products: [
        .executable(name: "cockpit-probe", targets: ["CockpitProbe"])
    ],
    targets: [
        .target(name: "CockpitMacPrototypeCore"),
        .executableTarget(name: "CockpitProbe"),
        .testTarget(name: "CockpitMacPrototypeCoreTests", dependencies: ["CockpitMacPrototypeCore"])
    ]
)
