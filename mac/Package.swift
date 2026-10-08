// swift-tools-version: 5.9
// Native Mac services kept for salvage review (docs/plan.md); the notch is being replaced by a Codenotch fork. This package intentionally has no third-party dependencies.
// Build on a macOS GitHub runner; local builds are disabled by AGENTS.md.
import PackageDescription

let package = Package(
    name: "PulseMacPrototype",
    platforms: [.macOS(.v13)],
    products: [
        .executable(name: "pulse-probe", targets: ["PulseProbe"])
    ],
    targets: [
        .target(name: "PulseMacPrototypeCore"),
        .executableTarget(name: "PulseProbe"),
        .testTarget(name: "PulseMacPrototypeCoreTests", dependencies: ["PulseMacPrototypeCore"])
    ]
)
