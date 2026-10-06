import XCTest
import CoreGraphics
@testable import CockpitMacPrototypeCore

final class PillSettingsCodecTests: XCTestCase {
    private func decode(_ json: String) -> Result<PillSettings, SettingsFailure> {
        PillSettingsCodec.decode(Data(json.utf8))
    }

    func testFullDocument() throws {
        let s = try decode("""
        {"schema_version":1,"visible":false,"cadence_seconds":5,
         "monitors":{"A":{"enabled":false,"anchor":"bottom-left"}},"extra":{"x":1}}
        """).get()
        XCTAssertFalse(s.visible)
        XCTAssertEqual(s.cadenceSeconds, 5)
        XCTAssertEqual(s.monitors["A"], MonitorSetting(enabled: false, anchor: .bottomLeft))
    }

    func testDefaultsForAbsentFields() throws {
        XCTAssertEqual(try decode(#"{"schema_version":1}"#).get(), .defaults)
        XCTAssertEqual(try decode(#"{"schema_version":1,"monitors":{}}"#).get(), .defaults)
    }

    /// STRICT: any malformed known field rejects the whole payload — no defaults, no
    /// coercion, no partial settings — so the original file is never sanitized away.
    func testMalformedFieldsRejectWholePayload() {
        // cadence: out of range, fractional, boolean, string — all reject.
        for bad in ["0", "-5", "11", "999999999999", "3.5", "true", "\"7\""] {
            XCTAssertEqual(decode("{\"schema_version\":1,\"cadence_seconds\":\(bad)}"),
                           .failure(.malformed), "cadence_seconds=\(bad)")
        }
        // visible: anything but a JSON boolean rejects.
        for bad in ["1", "0", "\"false\"", "null"] {
            XCTAssertEqual(decode("{\"schema_version\":1,\"visible\":\(bad)}"),
                           .failure(.malformed), "visible=\(bad)")
        }
        // monitors: non-dictionary rejects instead of being dropped.
        for bad in ["5", "\"x\"", "true", "[]", "null"] {
            XCTAssertEqual(decode("{\"schema_version\":1,\"monitors\":\(bad)}"),
                           .failure(.malformed), "monitors=\(bad)")
        }
        // Monitor entries: non-dictionary values or bad fields reject the whole payload.
        XCTAssertEqual(decode(#"{"schema_version":1,"monitors":{"A":5}}"#), .failure(.malformed))
        XCTAssertEqual(decode(#"{"schema_version":1,"monitors":{"A":{"enabled":1}}}"#), .failure(.malformed))
        XCTAssertEqual(decode(#"{"schema_version":1,"monitors":{"A":{"anchor":"middle"}}}"#), .failure(.malformed))
        XCTAssertEqual(decode(#"{"schema_version":1,"monitors":{"A":{"anchor":3}}}"#), .failure(.malformed))
        // One bad entry among many good ones still rejects everything.
        XCTAssertEqual(decode(#"{"schema_version":1,"monitors":{"A":{"enabled":true},"B":5}}"#),
                       .failure(.malformed))
        // Empty monitor key rejects.
        XCTAssertEqual(decode(#"{"schema_version":1,"monitors":{"":{}}}"#), .failure(.malformed))
    }

    /// Model level: a failed decode returns no settings at all, so there is nothing to
    /// mutate or persist — the store can keep the original bytes untouched.
    func testDecodeFailureYieldsNoSettings() {
        switch decode(#"{"schema_version":1,"monitors":{"A":{"enabled":"yes"}}}"#) {
        case .success: XCTFail("malformed entry must not produce settings")
        case .failure(let f): XCTAssertEqual(f, .malformed)
        }
    }

    func testFailures() {
        XCTAssertEqual(decode(#"{"schema_version":2}"#), .failure(.unknownSchema(2)))
        XCTAssertEqual(decode(#"{"schema_version":0}"#), .failure(.unknownSchema(0)))
        XCTAssertEqual(decode(#"{"schema_version":-1}"#), .failure(.unknownSchema(-1)))
        XCTAssertEqual(decode(#"{"visible":true}"#), .failure(.malformed))
        XCTAssertEqual(decode(#"{"schema_version":true}"#), .failure(.malformed))
        XCTAssertEqual(decode(#"{"schema_version":"1"}"#), .failure(.malformed))
        XCTAssertEqual(decode(#"{"schema_version":1.5}"#), .failure(.malformed))
        XCTAssertEqual(decode("not json"), .failure(.malformed))
        XCTAssertEqual(decode("[1]"), .failure(.malformed))
        let big = Data(repeating: 0x20, count: PillSettings.maxFileBytes + 1)
        XCTAssertEqual(PillSettingsCodec.decode(big), .failure(.oversized))
    }

    func testMonitorCountCapIsStrict() throws {
        let ok = (0..<PillSettings.maxMonitors).map { "\"m\($0)\":{}" }.joined(separator: ",")
        XCTAssertEqual(try decode("{\"schema_version\":1,\"monitors\":{\(ok)}}").get().monitors.count,
                       PillSettings.maxMonitors)
        // More than the cap rejects the whole payload rather than truncating.
        let over = (0...PillSettings.maxMonitors).map { "\"m\($0)\":{}" }.joined(separator: ",")
        XCTAssertEqual(decode("{\"schema_version\":1,\"monitors\":{\(over)}}"), .failure(.malformed))
    }

    func testMonitorKeyUTF8ByteLimit() throws {
        let key255 = String(repeating: "k", count: 255)
        let key256 = String(repeating: "k", count: 256)
        let key257 = String(repeating: "k", count: 257)
        XCTAssertEqual(key255.utf8.count, 255)
        XCTAssertEqual(key256.utf8.count, 256)
        XCTAssertEqual(key257.utf8.count, 257)
        XCTAssertEqual(try decode("{\"schema_version\":1,\"monitors\":{\"\(key255)\":{}}}").get().monitors.count, 1)
        XCTAssertEqual(try decode("{\"schema_version\":1,\"monitors\":{\"\(key256)\":{}}}").get().monitors.count, 1)
        XCTAssertEqual(decode("{\"schema_version\":1,\"monitors\":{\"\(key257)\":{}}}"), .failure(.malformed))
        // Multi-byte characters are measured in UTF-8 bytes, not Characters:
        // 128 × 'é' (2 bytes each in UTF-8) = 256 bytes → ok; 129 × 'é' = 258 bytes → reject.
        let e128 = String(repeating: "é", count: 128)
        let e129 = String(repeating: "é", count: 129)
        XCTAssertEqual(e128.utf8.count, 256)
        XCTAssertEqual(e129.utf8.count, 258)
        XCTAssertEqual(try decode("{\"schema_version\":1,\"monitors\":{\"\(e128)\":{}}}").get().monitors.count, 1)
        XCTAssertEqual(decode("{\"schema_version\":1,\"monitors\":{\"\(e129)\":{}}}"), .failure(.malformed))
    }

    func testSetMonitorMutatorEnforcesBounds() {
        var s = PillSettings.defaults
        XCTAssertTrue(s.setMonitor("A", MonitorSetting(enabled: false)))
        XCTAssertEqual(s.monitors["A"], MonitorSetting(enabled: false))
        // Invalid keys refused, settings unchanged.
        let bad = String(repeating: "k", count: PillSettings.maxKeyUTF8Bytes + 1)
        XCTAssertFalse(s.setMonitor("", .defaults))
        XCTAssertFalse(s.setMonitor(bad, .defaults))
        XCTAssertEqual(s.monitors.count, 1)
        // Cap: existing keys replace; new keys beyond the cap are refused.
        var full = PillSettings.defaults
        for i in 0..<PillSettings.maxMonitors { XCTAssertTrue(full.setMonitor("m\(i)", .defaults)) }
        XCTAssertFalse(full.setMonitor("overflow", .defaults))
        XCTAssertEqual(full.monitors.count, PillSettings.maxMonitors)
        XCTAssertTrue(full.setMonitor("m0", MonitorSetting(enabled: false)))
    }

    func testEncodeRefusesUnserializableMonitors() {
        var s = PillSettings.defaults
        s.monitors = Dictionary(uniqueKeysWithValues: (0...PillSettings.maxMonitors).map { ("m\($0)", MonitorSetting.defaults) })
        XCTAssertNil(PillSettingsCodec.encode(s))
        s.monitors = [String(repeating: "k", count: PillSettings.maxKeyUTF8Bytes + 1): .defaults]
        XCTAssertNil(PillSettingsCodec.encode(s))
        s.monitors = ["": .defaults]
        XCTAssertNil(PillSettingsCodec.encode(s))
    }

    func testEncodeRoundTripAndDeterminism() throws {
        let s = PillSettings(visible: false, cadenceSeconds: 7, monitors: [
            "B": MonitorSetting(enabled: true, anchor: .bottomRight),
            "A": MonitorSetting(enabled: false, anchor: .topLeft)
        ])
        let a = try XCTUnwrap(PillSettingsCodec.encode(s))
        XCTAssertEqual(a, PillSettingsCodec.encode(s))
        XCTAssertEqual(try PillSettingsCodec.decode(a).get(), s)
        XCTAssertTrue(String(decoding: a, as: UTF8.self).contains("\"schema_version\":1"))
    }

    func testEncodeClampsCadenceAndRejectsOversize() throws {
        var s = PillSettings.defaults
        s.cadenceSeconds = 99
        XCTAssertEqual(try PillSettingsCodec.decode(try XCTUnwrap(PillSettingsCodec.encode(s))).get().cadenceSeconds, 10)
    }
}

final class PillSettingsPolicyTests: XCTestCase {
    private var changed: PillSettings { PillSettings(visible: false) }

    func testDecisionTable() {
        let d = PillSettings.defaults
        XCTAssertEqual(SettingsWritePolicy.decide(state: .missing, current: d, baseline: d, holdsInstanceLock: true), .skipUnchanged)
        XCTAssertEqual(SettingsWritePolicy.decide(state: .loaded, current: d, baseline: d, holdsInstanceLock: true), .skipUnchanged)
        XCTAssertEqual(SettingsWritePolicy.decide(state: .missing, current: changed, baseline: d, holdsInstanceLock: true), .write)
        XCTAssertEqual(SettingsWritePolicy.decide(state: .loaded, current: changed, baseline: d, holdsInstanceLock: true), .write)
        XCTAssertEqual(SettingsWritePolicy.decide(state: .loaded, current: changed, baseline: d, holdsInstanceLock: false),
                       .refuse("instance_lock_not_held"))
        for failure in [SettingsFailure.malformed, .oversized, .unknownSchema(9), .symlink, .ioError(5)] {
            guard case .refuse = SettingsWritePolicy.decide(state: .protected(failure), current: changed, baseline: d, holdsInstanceLock: true)
            else { return XCTFail("protected \(failure) must refuse") }
            XCTAssertEqual(SettingsWritePolicy.decide(state: .protected(failure), current: d, baseline: d, holdsInstanceLock: true), .skipUnchanged)
        }
    }
}

final class PillPolicyTests: XCTestCase {
    func testShutdownOrder() {
        XCTAssertEqual(ShutdownPlan.steps, [.stopTimer, .closePanels, .persistSettings, .releaseLock, .emitShutdown])
    }

    func testVisibilityAndSchedule() {
        XCTAssertTrue(PillVisibility.shouldShow(settingsVisible: true, monitorEnabled: true, fullscreenSuppressed: false))
        XCTAssertFalse(PillVisibility.shouldShow(settingsVisible: false, monitorEnabled: true, fullscreenSuppressed: false))
        XCTAssertFalse(PillVisibility.shouldShow(settingsVisible: true, monitorEnabled: false, fullscreenSuppressed: false))
        XCTAssertFalse(PillVisibility.shouldShow(settingsVisible: true, monitorEnabled: true, fullscreenSuppressed: true))
        XCTAssertEqual(PillSchedule.interval(allHidden: true, cadenceSeconds: 3), 10)
        XCTAssertEqual(PillSchedule.interval(allHidden: false, cadenceSeconds: 3), 3)
        XCTAssertEqual(PillSchedule.interval(allHidden: false, cadenceSeconds: 100), 10)
        XCTAssertEqual(PillSchedule.interval(allHidden: false, cadenceSeconds: 0), 2)
    }

    func testAnchoredPlacement() {
        let v = CGRect(x: 0, y: 0, width: 1000, height: 800)
        let h = PillPlacement.height(diskCount: 2), w = PillPlacement.width, m = PillPlacement.margin
        XCTAssertEqual(AnchoredPlacement.frame(visible: v, diskCount: 2, anchor: .topRight),
                       CGRect(x: 1000 - w - m, y: 800 - h - m, width: w, height: h))
        XCTAssertEqual(AnchoredPlacement.frame(visible: v, diskCount: 2, anchor: .topLeft).origin, CGPoint(x: m, y: 800 - h - m))
        XCTAssertEqual(AnchoredPlacement.frame(visible: v, diskCount: 2, anchor: .bottomLeft).origin, CGPoint(x: m, y: m))
        XCTAssertEqual(AnchoredPlacement.frame(visible: v, diskCount: 2, anchor: .bottomRight).origin, CGPoint(x: 1000 - w - m, y: m))
        let tiny = CGRect(x: 10, y: 20, width: 50, height: 60)
        for a in PillAnchor.allCases {
            let f = AnchoredPlacement.frame(visible: tiny, diskCount: 5, anchor: a)
            XCTAssertTrue(tiny.contains(f), "\(a)")
        }
    }
}

final class PillStoreFileTests: XCTestCase {
    private var root: URL!
    private var dir: URL { root.appendingPathComponent("Cockpit", isDirectory: true) }
    private var file: URL { dir.appendingPathComponent(PillSettingsStore.fileName) }

    override func setUpWithError() throws {
        root = FileManager.default.temporaryDirectory.resolvingSymlinksInPath().appendingPathComponent("cockpit-pill-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: false)
    }

    override func tearDownWithError() throws {
        try? FileManager.default.removeItem(at: root)
    }

    private func mode(_ url: URL) throws -> Int {
        (try FileManager.default.attributesOfItem(atPath: url.path)[.posixPermissions] as? NSNumber)?.intValue ?? -1
    }

    func testMissingDirectoryLoadsDefaults() {
        XCTAssertEqual(PillSettingsStore(directory: dir).load(), .init(settings: .defaults, state: .missing))
    }

    func testSaveCreatesPrivateDirectoryAndFileAndRoundTrips() throws {
        let store = PillSettingsStore(directory: dir)
        let s = PillSettings(visible: false, cadenceSeconds: 4, monitors: ["K": MonitorSetting(enabled: true, anchor: .bottomLeft)])
        assertVoidResultEqual(store.save(s), .success(()))
        XCTAssertEqual(try mode(dir), 0o700)
        XCTAssertEqual(try mode(file), 0o600)
        XCTAssertEqual(store.load(), .init(settings: s, state: .loaded))
        let leftovers = try FileManager.default.contentsOfDirectory(atPath: dir.path).filter { $0.contains(".tmp-") }
        XCTAssertTrue(leftovers.isEmpty)
    }

    func testExistingDirectoryModeIsPreserved() throws {
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: false,
                                                attributes: [.posixPermissions: 0o755])
        XCTAssertEqual(PillSettingsStore(directory: dir).ensureDirectory(), .existing)
        assertVoidResultEqual(PillSettingsStore(directory: dir).save(.defaults), .success(()))
        XCTAssertEqual(try mode(dir), 0o755)
    }

    func testUnreadableFilesAreProtectedAndNeverOverwritten() throws {
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: false)
        let store = PillSettingsStore(directory: dir)
        let cases: [(Data, SettingsFailure)] = [
            (Data("garbage".utf8), .malformed),
            (Data(#"{"schema_version":7,"visible":false}"#.utf8), .unknownSchema(7)),
            (Data(repeating: 0x20, count: PillSettings.maxFileBytes + 1), .oversized)
        ]
        for (data, failure) in cases {
            try data.write(to: file)
            XCTAssertEqual(store.load(), .init(settings: .defaults, state: .protected(failure)))
            // A runtime refuses the write for protected state; the file stays byte-identical.
            XCTAssertEqual(SettingsWritePolicy.decide(state: .protected(failure), current: PillSettings(visible: false),
                                                       baseline: .defaults, holdsInstanceLock: true),
                           .refuse("settings_protected_\(failure.code)"))
            XCTAssertEqual(try Data(contentsOf: file), data)
        }
    }

    func testSymlinkedFileAndDirectoryAreRefused() throws {
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: false)
        let target = root.appendingPathComponent("target.json")
        try Data(#"{"schema_version":1}"#.utf8).write(to: target)
        try FileManager.default.createSymbolicLink(at: file, withDestinationURL: target)
        let store = PillSettingsStore(directory: dir)
        XCTAssertEqual(store.load().state, .protected(.symlink))
        assertVoidResultEqual(store.save(PillSettings(visible: false)), .failure(.refused(.symlink)))
        XCTAssertEqual(try Data(contentsOf: target), Data(#"{"schema_version":1}"#.utf8))

        let realDir = root.appendingPathComponent("real", isDirectory: true)
        let linkDir = root.appendingPathComponent("link", isDirectory: true)
        try FileManager.default.createDirectory(at: realDir, withIntermediateDirectories: false)
        try FileManager.default.createSymbolicLink(at: linkDir, withDestinationURL: realDir)
        let linked = PillSettingsStore(directory: linkDir)
        XCTAssertEqual(linked.ensureDirectory(), .refused(.symlink))
        XCTAssertEqual(linked.load().state, .protected(.symlink))
        assertVoidResultEqual(linked.save(PillSettings(visible: false)), .failure(.refused(.symlink)))
        XCTAssertTrue(try FileManager.default.contentsOfDirectory(atPath: realDir.path).isEmpty)
    }
}

final class PillInstanceLockTests: XCTestCase {
    private var root: URL!

    override func setUpWithError() throws {
        root = FileManager.default.temporaryDirectory.resolvingSymlinksInPath().appendingPathComponent("cockpit-lock-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: false)
    }

    override func tearDownWithError() throws { try? FileManager.default.removeItem(at: root) }

    func testSecondAcquireIsRejectedAndReleaseKeepsFile() throws {
        guard case .acquired(let first) = PillInstanceLock.acquire(directory: root) else { return XCTFail("first") }
        guard case .alreadyHeld = PillInstanceLock.acquire(directory: root) else { return XCTFail("second") }
        first.release()
        first.release() // idempotent
        let lockPath = root.appendingPathComponent(PillSettingsStore.lockName).path
        XCTAssertTrue(FileManager.default.fileExists(atPath: lockPath))
        guard case .acquired(let again) = PillInstanceLock.acquire(directory: root) else { return XCTFail("reacquire") }
        again.release()
    }

    func testSymlinkedLockIsRefused() throws {
        let target = root.appendingPathComponent("elsewhere")
        try Data().write(to: target)
        try FileManager.default.createSymbolicLink(at: root.appendingPathComponent(PillSettingsStore.lockName),
                                                   withDestinationURL: target)
        guard case .failed = PillInstanceLock.acquire(directory: root) else { return XCTFail("must refuse symlink") }
    }
}

final class PillRuntimeTests: XCTestCase {
    private var root: URL!
    private var dir: URL { root.appendingPathComponent("Cockpit", isDirectory: true) }
    private var events: [String] = []

    override func setUpWithError() throws {
        root = FileManager.default.temporaryDirectory.resolvingSymlinksInPath().appendingPathComponent("cockpit-rt-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: false)
        events = []
    }

    override func tearDownWithError() throws { try? FileManager.default.removeItem(at: root) }

    private func make() -> PillRuntime {
        PillRuntime(directory: dir, emit: { [unowned self] event, _, _ in events.append(event) })
    }

    func testSecondRuntimeReportsAlreadyRunning() {
        let a = make(), b = make()
        XCTAssertEqual(a.start(), .started)
        XCTAssertEqual(b.start(), .alreadyRunning)
        XCTAssertTrue(events.contains("instance_already_running"))
        XCTAssertFalse(b.holdsInstanceLock)
        a.shutdown(reason: "t", stopTimer: {}, closePanels: {})
        let c = make()
        XCTAssertEqual(c.start(), .started)
        c.shutdown(reason: "t", stopTimer: {}, closePanels: {})
    }

    func testShutdownOrderAndNoWriteWhenUnchanged() throws {
        let rt = make()
        XCTAssertEqual(rt.start(), .started)
        var order: [String] = []
        let base = events.count
        rt.shutdown(reason: "test", stopTimer: { order.append("timer") }, closePanels: { order.append("panels") })
        XCTAssertEqual(order, ["timer", "panels"])
        XCTAssertEqual(Array(events[base...]), ["instance_lock_released", "shutdown"])
        XCTAssertFalse(FileManager.default.fileExists(atPath: dir.appendingPathComponent(PillSettingsStore.fileName).path))
        // Idempotent.
        rt.shutdown(reason: "again", stopTimer: { order.append("x") }, closePanels: {})
        XCTAssertEqual(order, ["timer", "panels"])
    }

    func testChangedSettingsPersistBeforeLockRelease() throws {
        let rt = make()
        XCTAssertEqual(rt.start(), .started)
        rt.setCadence(6)
        rt.setMonitor("K", MonitorSetting(enabled: false, anchor: .bottomRight))
        let base = events.count
        rt.shutdown(reason: "t", stopTimer: {}, closePanels: {})
        XCTAssertEqual(Array(events[base...]), ["settings_persisted", "instance_lock_released", "shutdown"])
        let next = make()
        XCTAssertEqual(next.start(), .started)
        XCTAssertEqual(next.cadenceSeconds, 6)
        XCTAssertEqual(next.monitorSetting(for: "K"), MonitorSetting(enabled: false, anchor: .bottomRight))
        XCTAssertEqual(next.monitorSetting(for: "other"), .defaults)
        next.shutdown(reason: "t", stopTimer: {}, closePanels: {})
    }

    func testCorruptFileUsesDefaultsAndIsPreserved() throws {
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: false)
        let file = dir.appendingPathComponent(PillSettingsStore.fileName)
        let original = Data(#"{"schema_version":3,"cadence_seconds":9}"#.utf8)
        try original.write(to: file)
        let rt = make()
        XCTAssertEqual(rt.start(), .started)
        XCTAssertEqual(rt.settings, .defaults)
        XCTAssertTrue(events.contains("settings_recovery_using_defaults"))
        rt.setVisible(false)
        rt.shutdown(reason: "t", stopTimer: {}, closePanels: {})
        XCTAssertTrue(events.contains("settings_write_blocked"))
        XCTAssertEqual(try Data(contentsOf: file), original)
    }
}
