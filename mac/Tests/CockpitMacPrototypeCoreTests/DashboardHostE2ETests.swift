import AppKit
import Foundation
import XCTest
@testable import CockpitMacPrototypeCore

private enum DashboardFixtureError: Error, CustomStringConvertible {
    case missingPackagingMarker
    case unbundledModule

    var description: String {
        switch self {
        case .missingPackagingMarker: return "dashboard index did not contain module packaging marker"
        case .unbundledModule: return "dashboard module still contains import/export declarations"
        }
    }
}

@MainActor
final class DashboardHostE2ETests: XCTestCase {
    func testPackagedDashboardScansFixtureThroughNativeHostAndRetainsResults() async throws {
        let fileManager = FileManager.default
        let helperPath = ProcessInfo.processInfo.environment["COCKPIT_TEST_HELPER"]
        guard let helperPath, !helperPath.isEmpty else {
            XCTFail("COCKPIT_TEST_HELPER must point at built cockpit helper")
            return
        }
        let helper = URL(fileURLWithPath: helperPath).standardizedFileURL
        guard fileManager.isExecutableFile(atPath: helper.path) else {
            XCTFail("COCKPIT_TEST_HELPER is not executable: \(helper.path)")
            return
        }

        let fixture = URL(fileURLWithPath: ProcessInfo.processInfo.environment["RUNNER_TEMP"]
                          ?? fileManager.temporaryDirectory.resolvingSymlinksInPath().path,
                          isDirectory: true)
            .appendingPathComponent("cockpit-dashboard-e2e-\(UUID().uuidString)", isDirectory: true)
        let scanRoot = fixture.appendingPathComponent("scan-root", isDirectory: true)
        let dashboard = fixture.appendingPathComponent("dashboard", isDirectory: true)
        try fileManager.createDirectory(at: scanRoot, withIntermediateDirectories: true)
        try fileManager.createDirectory(at: scanRoot.appendingPathComponent("subfolder", isDirectory: true),
                                        withIntermediateDirectories: true)
        try fileManager.createDirectory(at: dashboard, withIntermediateDirectories: true)
        defer { try? fileManager.removeItem(at: fixture) }

        // verifyBundledScan owns isolated smoke state outside selected scan root.
        // Installed journey separately verifies durable state across app restart.
        try Data("dashboard fixture".utf8)
            .write(to: scanRoot.appendingPathComponent("overview.txt"))
        try Data("nested example".utf8)
            .write(to: scanRoot.appendingPathComponent("subfolder/example.txt"))
        try preparePackagedDashboard(at: dashboard)

        // Touch the native singleton so this journey runs in the same AppKit mode as
        // the delivered application instead of an AppKit-free coordinator seam.
        let application = NSApplication.shared
        let configuration = DashboardHostConfiguration(dashboardDirectory: dashboard,
                                                        helperURL: helper)
        let host = DashboardHost(configuration: configuration,
                                  runner: ProcessScanRunner())
        defer {
            host.stop()
            application.hide(nil)
        }

        try await host.verifyBundledScan(root: scanRoot)

        XCTAssertTrue(fileManager.fileExists(atPath: scanRoot.appendingPathComponent("overview.txt").path))
        XCTAssertTrue(fileManager.fileExists(atPath: scanRoot.appendingPathComponent("subfolder/example.txt").path))
    }

    private func preparePackagedDashboard(at destination: URL) throws {
        let sourceFile = URL(fileURLWithPath: #filePath)
        let repositoryRoot = sourceFile
            .deletingLastPathComponent()
            .deletingLastPathComponent()
            .deletingLastPathComponent()
            .deletingLastPathComponent()
        let sourceDashboard = repositoryRoot.appendingPathComponent("dashboard", isDirectory: true)
        let fileManager = FileManager.default

        let moduleSource = try String(contentsOf: sourceDashboard.appendingPathComponent("app.mjs"), encoding: .utf8)
        let classicSource = moduleSource.replacingOccurrences(
            of: #"(?m)^export (?=(?:const|function)\b)"#,
            with: "",
            options: .regularExpression)
        guard classicSource.range(of: #"(?m)^\s*(?:import|export)\s"#, options: .regularExpression) == nil else {
            throw DashboardFixtureError.unbundledModule
        }
        try "(() => {\n\(classicSource)\n})();\n"
            .write(to: destination.appendingPathComponent("app.js"), atomically: true, encoding: .utf8)

        let marker = #"<script type="module" src="./app.mjs"></script>"#
        let index = try String(contentsOf: sourceDashboard.appendingPathComponent("index.html"), encoding: .utf8)
        guard index.contains(marker) else { throw DashboardFixtureError.missingPackagingMarker }
        let classicIndex = index.replacingOccurrences(of: marker, with: #"<script src="./app.js"></script>"#)
        try classicIndex.write(to: destination.appendingPathComponent("index.html"), atomically: true, encoding: .utf8)
        try fileManager.copyItem(at: sourceDashboard.appendingPathComponent("style.css"),
                                 to: destination.appendingPathComponent("style.css"))
    }
}
