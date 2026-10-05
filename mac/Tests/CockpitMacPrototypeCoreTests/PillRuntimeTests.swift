import XCTest
@testable import CockpitMacPrototypeCore

/// Mutator bounds and shutdown determinism. (Lifecycle/persist coverage lives in
/// PillSettingsTests.swift's `PillRuntimeTests`.)
final class PillRuntimeMutationTests: XCTestCase {
    private var root: URL!
    private var dir: URL { root.appendingPathComponent("Cockpit", isDirectory: true) }
    private var events: [(event: String, level: String)] = []

    override func setUpWithError() throws {
        root = FileManager.default.temporaryDirectory.resolvingSymlinksInPath().appendingPathComponent("cockpit-mut-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: false)
        events = []
    }

    override func tearDownWithError() throws { try? FileManager.default.removeItem(at: root) }

    private func make() -> PillRuntime {
        PillRuntime(directory: dir, emit: { [unowned self] event, level, _ in events.append((event, level)) })
    }

    func testMonitorCapIsAnExplicitRefusal() {
        let rt = make()
        XCTAssertEqual(rt.start(), .started)
        for i in 0..<PillSettings.maxMonitors {
            XCTAssertEqual(rt.setMonitor("m\(i)", MonitorSetting(enabled: false)), .applied)
        }
        XCTAssertEqual(rt.settings.monitors.count, PillSettings.maxMonitors)
        XCTAssertEqual(rt.setMonitor("overflow", MonitorSetting(enabled: false)),
                       .refused("monitor_limit_\(PillSettings.maxMonitors)"))
        XCTAssertEqual(rt.settings.monitors.count, PillSettings.maxMonitors)
        XCTAssertNil(rt.settings.monitors["overflow"])
        XCTAssertTrue(events.contains { $0.event == "settings_mutation_refused" && $0.level == "warn" })
        // Updating an existing key is still allowed at the cap.
        XCTAssertEqual(rt.setMonitor("m0", MonitorSetting(enabled: true, anchor: .bottomLeft)), .applied)
        XCTAssertEqual(rt.monitorSetting(for: "m0"), MonitorSetting(enabled: true, anchor: .bottomLeft))
        rt.shutdown(reason: "t", stopTimer: {}, closePanels: {})
    }

    func testInvalidMonitorKeysAreRefused() {
        let rt = make()
        XCTAssertEqual(rt.start(), .started)
        XCTAssertEqual(rt.setMonitor("", MonitorSetting()), .refused("invalid_monitor_key"))
        let tooLong = String(repeating: "k", count: PillSettings.maxMonitorKeyLength + 1)
        XCTAssertEqual(rt.setMonitor(tooLong, MonitorSetting()), .refused("invalid_monitor_key"))
        XCTAssertTrue(rt.settings.monitors.isEmpty)
        rt.shutdown(reason: "t", stopTimer: {}, closePanels: {})
    }

    func testSettingDefaultsOnAbsentKeyIsANoOp() {
        let rt = make()
        XCTAssertEqual(rt.start(), .started)
        XCTAssertEqual(rt.setMonitor("ghost", .defaults), .applied)
        XCTAssertTrue(rt.settings.monitors.isEmpty)
        rt.shutdown(reason: "t", stopTimer: {}, closePanels: {})
    }
}

final class PillRuntimeLifecycleTests: XCTestCase {
    private var root: URL!
    private var dir: URL { root.appendingPathComponent("Cockpit", isDirectory: true) }
    private var events: [String] = []

    override func setUpWithError() throws {
        root = FileManager.default.temporaryDirectory.resolvingSymlinksInPath().appendingPathComponent("cockpit-life-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: false)
        events = []
    }

    override func tearDownWithError() throws { try? FileManager.default.removeItem(at: root) }

    private func make() -> PillRuntime {
        PillRuntime(directory: dir, emit: { [unowned self] event, _, _ in events.append(event) })
    }

    func testDoubleStartTakesNoSecondOwnerRole() {
        let rt = make()
        XCTAssertEqual(rt.start(), .started)
        XCTAssertEqual(rt.start(), .alreadyRunning)
        XCTAssertTrue(rt.holdsInstanceLock)
        rt.shutdown(reason: "t", stopTimer: {}, closePanels: {})
    }

    func testStartAfterShutdownIsRefused() {
        let rt = make()
        XCTAssertEqual(rt.start(), .started)
        rt.shutdown(reason: "t", stopTimer: {}, closePanels: {})
        XCTAssertEqual(rt.start(), .failed("already_shut_down"))
    }

    func testShutdownIsIdempotentAndOrdered() {
        let rt = make()
        XCTAssertEqual(rt.start(), .started)
        var order: [String] = []
        rt.shutdown(reason: "first",
                    stopTimer: { order.append("timer") },
                    closePanels: { order.append("panels") })
        XCTAssertEqual(order, ["timer", "panels"])
        XCTAssertFalse(rt.holdsInstanceLock)
        let lockReleases = events.filter { $0 == "instance_lock_released" }.count
        XCTAssertEqual(lockReleases, 1)
        // Second call must be a complete no-op: no hooks, no extra events, single owner released once.
        rt.shutdown(reason: "second",
                    stopTimer: { order.append("timer2") },
                    closePanels: { order.append("panels2") })
        XCTAssertEqual(order, ["timer", "panels"])
        XCTAssertEqual(events.filter { $0 == "instance_lock_released" }.count, lockReleases)
        XCTAssertEqual(events.filter { $0 == "shutdown" }.count, 1)
    }

    func testLockReacquireAfterShutdownSucceeds() {
        let a = make()
        XCTAssertEqual(a.start(), .started)
        a.shutdown(reason: "t", stopTimer: {}, closePanels: {})
        let b = make()
        XCTAssertEqual(b.start(), .started)
        b.shutdown(reason: "t", stopTimer: {}, closePanels: {})
    }

    func testRefusedDirectoryFailsStart() throws {
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: false,
                                                attributes: [.posixPermissions: 0o755])
        let rt = make()
        guard case .failed(let reason) = rt.start() else { return XCTFail("must refuse permissive dir") }
        XCTAssertTrue(reason.hasPrefix("directory_io_error_"))
        XCTAssertFalse(rt.holdsInstanceLock)
        XCTAssertTrue(events.contains("instance_lock_failed"))
    }
}
