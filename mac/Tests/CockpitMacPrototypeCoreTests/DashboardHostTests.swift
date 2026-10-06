import XCTest
@testable import CockpitMacPrototypeCore

// Unit tests for the AppKit-free seams of DashboardHost: request construction, bounded
// process execution (real /bin tools only — no GUI), JSON envelope parsing, injection
// escaping and navigation scoping. Inline synthetic fixtures only; no donor data.

private final class StubRunner: ScanRunning, @unchecked Sendable {
    var lastRequest: ScanRequest?
    var outcome: ScanOutcome?
    var error: Error?
    var cancelled = false

    func run(_ request: ScanRequest) async throws -> ScanOutcome {
        lastRequest = request
        if let error { throw error }
        return outcome!
    }
    func cancel() { cancelled = true }
}

final class DashboardHostTests: XCTestCase {
    private var tempRoot: URL!
    private var dashboardDir: URL!

    override func setUpWithError() throws {
        tempRoot = FileManager.default.temporaryDirectory
            .appendingPathComponent("cockpit-dh-tests-\(UUID().uuidString)")
        dashboardDir = tempRoot.appendingPathComponent("dashboard")
        try FileManager.default.createDirectory(at: dashboardDir, withIntermediateDirectories: true)
    }

    override func tearDownWithError() throws {
        try? FileManager.default.removeItem(at: tempRoot)
    }

    private func config(helper: URL? = nil) -> DashboardHostConfiguration {
        var c = DashboardHostConfiguration(dashboardDirectory: dashboardDir, helperURL: helper)
        c.deadline = 5
        return c
    }

    // MARK: request construction

    func testScanRequestArgvIsExplicitRootNoShell() {
        let helper = URL(fileURLWithPath: "/app/Contents/Helpers/cockpit")
        let req = ScanRequest.scan(helper: helper, root: URL(fileURLWithPath: "/Users/x/My Folder"),
                                   deadline: 120, stdoutLimit: 1 << 20, stderrLimit: 1 << 20)
        XCTAssertEqual(req.arguments, ["scan", "/Users/x/My Folder", "--save", "--json"])
        XCTAssertEqual(req.executable.path, "/app/Contents/Helpers/cockpit")
        XCTAssertEqual(req.deadline, 120)
    }

    // MARK: ProcessScanRunner (real, bounded subprocesses)

    func testRunnerCapturesBoundedStdout() async throws {
        let runner = ProcessScanRunner()
        let req = ScanRequest(executable: URL(fileURLWithPath: "/bin/echo"),
                              arguments: ["hello"], deadline: 5,
                              stdoutLimit: 1 << 20, stderrLimit: 1 << 20)
        let outcome = try await runner.run(req)
        XCTAssertEqual(String(decoding: outcome.stdout, as: UTF8.self).trimmingCharacters(in: .whitespacesAndNewlines), "hello")
        XCTAssertEqual(outcome.status, 0)
        XCTAssertFalse(outcome.truncated)
    }

    func testRunnerTruncatesOverLimitStdout() async throws {
        // yes emits unbounded output; cap must engage. Kill via deadline.
        let runner = ProcessScanRunner()
        let req = ScanRequest(executable: URL(fileURLWithPath: "/usr/bin/yes"),
                              arguments: ["x"], deadline: 1, stdoutLimit: 64 * 1024, stderrLimit: 1024)
        await XCTAssertThrowsErrorAsync(try await runner.run(req)) { error in
            XCTAssertEqual(error as? ScanFailure, .timedOut)
        }
    }

    func testRunnerKillsChildBeforeDeadline() async throws {
        let runner = ProcessScanRunner()
        let req = ScanRequest(executable: URL(fileURLWithPath: "/bin/sleep"),
                              arguments: ["60"], deadline: 0.3, stdoutLimit: 1024, stderrLimit: 1024)
        let start = Date()
        await XCTAssertThrowsErrorAsync(try await runner.run(req)) { error in
            XCTAssertEqual(error as? ScanFailure, .timedOut)
        }
        XCTAssertLessThan(Date().timeIntervalSince(start), 5, "child must be terminate→kill→verified, not left running")
    }

    func testRunnerNonZeroExit() async throws {
        let runner = ProcessScanRunner()
        let req = ScanRequest(executable: URL(fileURLWithPath: "/usr/bin/false"),
                              arguments: [], deadline: 5, stdoutLimit: 1024, stderrLimit: 1024)
        await XCTAssertThrowsErrorAsync(try await runner.run(req)) { error in
            guard case .nonZeroExit(let code, _) = error as? ScanFailure else {
                return XCTFail("expected nonZeroExit, got \(error)")
            }
            XCTAssertEqual(code, 1)
        }
    }

    func testRunnerCancelTerminatesChild() async throws {
        let runner = ProcessScanRunner()
        let req = ScanRequest(executable: URL(fileURLWithPath: "/bin/sleep"),
                              arguments: ["60"], deadline: 60, stdoutLimit: 1024, stderrLimit: 1024)
        let task = Task { try await runner.run(req) }
        try await Task.sleep(nanoseconds: 200_000_000)
        task.cancel()
        await XCTAssertThrowsErrorAsync(try await task.value) { _ in }
    }

    // MARK: JSON envelope

    private func syntheticEnvelope() throws -> Data {
        let json = """
        {"snapshot":{"id":"s1","report":{"roots":["/tmp/x"],"entries":[{"path":"/tmp/x","logical_bytes":4}],
        "folders":[],"accounting":{"logical_bytes":4},"incomplete_reasons":[]}},"saved_to":"/state/1.json"}
        """
        return Data(json.utf8)
    }

    func testScanObjectAcceptsSnapshotEnvelope() throws {
        let object = try DashboardInjection.scanObject(from: syntheticEnvelope(), truncated: false) as? [String: Any]
        XCTAssertNotNil(object?["snapshot"])
    }

    func testScanObjectRejectsTruncated() {
        XCTAssertThrowsError(try DashboardInjection.scanObject(from: syntheticEnvelope(), truncated: true)) {
            XCTAssertEqual($0 as? ScanFailure, .outputTruncated)
        }
    }

    func testScanObjectRejectsEmptyAndMalformed() {
        XCTAssertThrowsError(try DashboardInjection.scanObject(from: Data(), truncated: false)) {
            XCTAssertEqual($0 as? ScanFailure, .emptyReport)
        }
        XCTAssertThrowsError(try DashboardInjection.scanObject(from: Data("{\"nope\":1}".utf8), truncated: false)) {
            XCTAssertEqual($0 as? ScanFailure, .invalidJSON("expected {snapshot:{report:…}}"))
        }
    }

    // MARK: injection

    func testImportJavaScriptCallsAllowlistedEntry() throws {
        let script = try DashboardInjection.importJavaScript(for: ["snapshot": ["report": ["roots": []]]])
        XCTAssertTrue(script.contains("window.CockpitDashboard"))
        XCTAssertTrue(script.contains("importScan"))
    }

    func testImportJavaScriptEscapesBreakoutSequences() throws {
        let script = try DashboardInjection.importJavaScript(for: ["x": "</script>\u{2028}bad"])
        XCTAssertFalse(script.contains("</script>"), "literal must not terminate surrounding context")
        XCTAssertFalse(script.contains("\u{2028}"))
        XCTAssertTrue(script.contains("\\u003c"))
    }

    // MARK: navigation policy

    func testNavigationAllowsOnlyBundledSubtree() {
        let root = dashboardDir!
        XCTAssertTrue(DashboardNavigation.isAllowed(root.appendingPathComponent("index.html"), dashboardRoot: root))
        XCTAssertTrue(DashboardNavigation.isAllowed(root.appendingPathComponent("app.mjs"), dashboardRoot: root))
        XCTAssertFalse(DashboardNavigation.isAllowed(root.deletingLastPathComponent().appendingPathComponent("secret.txt"), dashboardRoot: root))
        XCTAssertFalse(DashboardNavigation.isAllowed(URL(string: "https://evil.example")!, dashboardRoot: root))
        XCTAssertFalse(DashboardNavigation.isAllowed(URL(fileURLWithPath: "/etc/passwd"), dashboardRoot: root))
    }

    func testScriptMessageAllowlistIsClosed() {
        XCTAssertTrue(DashboardNavigation.allowedScriptMessages.contains("cockpitStatus"))
        XCTAssertFalse(DashboardNavigation.allowedScriptMessages.contains("eval"))
    }

    // MARK: coordinator

    func testCoordinatorMissingHelper() async {
        let coordinator = ScanCoordinator(configuration: config(helper: nil), runner: StubRunner())
        await XCTAssertThrowsErrorAsync(try await coordinator.importScript(for: tempRoot)) {
            XCTAssertEqual($0 as? ScanFailure, .helperMissing)
        }
    }

    func testCoordinatorBuildsImportScript() async throws {
        let stub = StubRunner()
        stub.outcome = ScanOutcome(stdout: try syntheticEnvelope(), stderr: "", status: 0,
                                   truncated: false)
        let helper = URL(fileURLWithPath: "/app/Contents/Helpers/cockpit")
        let coordinator = ScanCoordinator(configuration: config(helper: helper), runner: stub)
        let script = try await coordinator.importScript(for: tempRoot)
        XCTAssertEqual(stub.lastRequest?.arguments[1], tempRoot.path)
        XCTAssertTrue(script.contains("importScan"))
    }
}

// async variant of XCTAssertThrowsError
private func XCTAssertThrowsErrorAsync<T>(_ expression: @autoclosure () async throws -> T,
                                        _ handler: (Error) -> Void,
                                        file: StaticString = #filePath, line: UInt = #line) async {
    do { _ = try await expression() } catch { handler(error); return }
    XCTFail("expected throw", file: file, line: line)
}
