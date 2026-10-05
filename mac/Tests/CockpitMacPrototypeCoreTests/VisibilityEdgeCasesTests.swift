import CoreGraphics
import XCTest
@testable import CockpitMacPrototypeCore

final class VisibilityEdgeCasesTests: XCTestCase {
    private let primary = CGRect(x: 0, y: 0, width: 1920, height: 1080)
    private let left = CGRect(x: -2560, y: -200, width: 2560, height: 1440)

    private func hide(trusted: Bool = true, ax: Bool?, focused: CGRect?, fallback: Bool, monitor: CGRect) -> Bool {
        VisibilityGeometry.shouldHide(accessibilityTrusted: trusted, axFullscreen: ax,
            focusedWindow: focused, geometryFallback: fallback, monitor: monitor)
    }

    func testNegativeCoordinatesCover() {
        XCTAssertTrue(VisibilityGeometry.covers(left, monitor: left))
        XCTAssertTrue(VisibilityGeometry.covers(left.insetBy(dx: -10, dy: -10), monitor: left))
        XCTAssertFalse(VisibilityGeometry.covers(left.offsetBy(dx: 1, dy: 0), monitor: left))
        XCTAssertFalse(VisibilityGeometry.covers(primary, monitor: left))
    }

    func testMixedMonitorSizes() {
        XCTAssertFalse(VisibilityGeometry.covers(primary, monitor: left))
        XCTAssertTrue(VisibilityGeometry.covers(left, monitor: CGRect(x: -2000, y: 0, width: 1000, height: 500)))
        XCTAssertFalse(VisibilityGeometry.covers(CGRect(x: -2560, y: -200, width: 2560, height: 1439), monitor: left))
    }

    func testPartialCoverageIsNotCovered() {
        let half = CGRect(x: 0, y: 0, width: 960, height: 1080)
        XCTAssertFalse(VisibilityGeometry.covers(half, monitor: primary))
        XCTAssertFalse(VisibilityGeometry.isLikelyBorderless("", window: half, monitor: primary))
        XCTAssertFalse(VisibilityGeometry.isLikelyBorderless("", window: CGRect(x: 0, y: 0, width: 1920, height: 1000), monitor: primary))
    }

    func testDegenerateRectsNeverCover() {
        XCTAssertFalse(VisibilityGeometry.covers(.zero, monitor: .zero))
        XCTAssertFalse(VisibilityGeometry.covers(primary, monitor: .null))
        XCTAssertFalse(VisibilityGeometry.covers(.null, monitor: primary))
    }

    func testFocusedWindowOnAnotherMonitorFallsBackToGeometry() {
        let focused = CGRect(x: -2560, y: -200, width: 2560, height: 1440)
        XCTAssertTrue(hide(ax: true, focused: focused, fallback: true, monitor: primary))
        XCTAssertFalse(hide(ax: true, focused: focused, fallback: false, monitor: primary))
        // Edge-adjacent only: not on this monitor.
        XCTAssertFalse(hide(ax: true, focused: focused, fallback: false, monitor: CGRect(x: 0, y: -200, width: 1920, height: 1080)))
    }

    func testExplicitAXFalseWinsOnSameMonitor() {
        XCTAssertFalse(hide(ax: false, focused: left, fallback: true, monitor: left))
        XCTAssertTrue(hide(ax: true, focused: left, fallback: false, monitor: left))
    }

    func testMissingAXObservations() {
        XCTAssertTrue(hide(ax: nil, focused: primary, fallback: true, monitor: primary))
        XCTAssertFalse(hide(ax: nil, focused: primary, fallback: false, monitor: primary))
        XCTAssertTrue(hide(ax: nil, focused: nil, fallback: true, monitor: primary))
        XCTAssertFalse(hide(ax: true, focused: nil, fallback: false, monitor: primary))
    }

    func testDeniedAccessibilityNeverHides() {
        for ax in [true, false, nil] as [Bool?] {
            for fallback in [true, false] {
                XCTAssertFalse(hide(trusted: false, ax: ax, focused: left, fallback: fallback, monitor: left))
            }
        }
    }
}
