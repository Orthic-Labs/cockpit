import XCTest
@testable import CockpitMacPrototypeCore

final class PillLifecycleTests: XCTestCase {
    func testCPUNoPreviousIsUnavailable() {
        XCTAssertNil(CPUMath.utilization(previous: nil, current: CPUTicks(user: 1, system: 1, nice: 0, idle: 1)))
    }

    func testCPUDelta() {
        let a = CPUTicks(user: 10, system: 10, nice: 0, idle: 80)
        let b = CPUTicks(user: 20, system: 20, nice: 0, idle: 160)
        XCTAssertEqual(CPUMath.utilization(previous: a, current: b)!, 0.2, accuracy: 1e-9)
    }

    func testCPUZeroTotalIsUnavailable() {
        let a = CPUTicks(user: 5, system: 5, nice: 5, idle: 5)
        XCTAssertNil(CPUMath.utilization(previous: a, current: a))
    }

    func testCPUWraparound() {
        let a = CPUTicks(user: UInt32.max - 4, system: 0, nice: 0, idle: UInt32.max - 4)
        let b = CPUTicks(user: 5, system: 0, nice: 0, idle: 5)
        XCTAssertEqual(CPUMath.utilization(previous: a, current: b)!, 0.5, accuracy: 1e-9)
    }

    func testDisplayDiff() {
        let d = DisplayKeyDiff.diff(existing: ["a", "b", "c"], wanted: ["b", "d", "d", "a"])
        XCTAssertEqual(d.added, ["d"])
        XCTAssertEqual(d.removed, ["c"])
        XCTAssertEqual(d.kept, ["b", "a"])
    }

    func testDisplayDiffEmpty() {
        let d = DisplayKeyDiff.diff(existing: ["a"], wanted: [])
        XCTAssertEqual(d.removed, ["a"])
        XCTAssertTrue(d.added.isEmpty && d.kept.isEmpty)
    }

    func testRedrawGate() {
        var gate = RedrawGate()
        XCTAssertTrue(gate.shouldRedraw("x"))
        XCTAssertFalse(gate.shouldRedraw("x"))
        XCTAssertTrue(gate.shouldRedraw("y"))
        gate.reset()
        XCTAssertTrue(gate.shouldRedraw("y"))
    }

    func testUnavailableRendersDashes() {
        XCTAssertEqual(PillFormat.label("CPU", fraction: nil), "CPU --")
        XCTAssertEqual(PillFormat.label("CPU", fraction: .nan), "CPU --")
        XCTAssertEqual(PillFormat.label("CPU", fraction: 0), "CPU 0%")
        XCTAssertEqual(PillFormat.label("MEM", fraction: 1.7), "MEM 100%")
        XCTAssertEqual(PillFormat.signature([("CPU", nil), ("MEM", 0.5)]), "CPU --\nMEM 50%")
    }

    func testArcQuantizationMatchesRedrawLabels() {
        XCTAssertEqual(PillFormat.displayedFraction(0.509), 0.5)
        XCTAssertEqual(PillFormat.displayedFraction(.nan), nil)
        XCTAssertEqual(PillFormat.displayedFraction(1.7), 1)
    }

    func testPlacementStaysInsideVisibleFrame() {
        let visible = CGRect(x: 100, y: 50, width: 800, height: 600)
        let f = PillPlacement.frame(visible: visible, diskCount: 2)
        XCTAssertTrue(visible.contains(f))
        XCTAssertEqual(f.maxX, visible.maxX - PillPlacement.margin)
        let tiny = PillPlacement.frame(visible: CGRect(x: 0, y: 0, width: 50, height: 50), diskCount: 9)
        XCTAssertTrue(CGRect(x: 0, y: 0, width: 50, height: 50).contains(tiny))
    }

    func testFailureTrackerReportsTransitionsOnly() {
        var t = FailureTracker()
        XCTAssertEqual(t.update(failing: ["cpu"]).failed, ["cpu"])
        XCTAssertTrue(t.update(failing: ["cpu"]).failed.isEmpty)
        let r = t.update(failing: [])
        XCTAssertEqual(r.recovered, ["cpu"])
    }

    func testStructuredEventLine() {
        let e = StructuredEvent(event: "startup", level: "info", ts: "T", fields: ["note": "a\nb\"c"])
        let line = e.jsonLine()
        XCTAssertFalse(line.contains("\n"))
        XCTAssertEqual(line, "{\"event\":\"startup\",\"fields\":{\"note\":\"a\\nb\\\"c\"},\"level\":\"info\",\"ts\":\"T\"}")
        let decoded = try? JSONDecoder().decode(StructuredEvent.self, from: Data(line.utf8))
        XCTAssertEqual(decoded, e)
    }

    func testTimestampFormat() {
        XCTAssertEqual(StructuredEvent.timestamp(Date(timeIntervalSince1970: 0)), "1970-01-01T00:00:00.000Z")
    }
}
