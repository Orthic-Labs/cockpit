import CoreGraphics
import XCTest
@testable import CockpitMacPrototypeCore

final class VisibilityGeometryTests: XCTestCase {
    private let monitor = CGRect(x: 0, y: 0, width: 1920, height: 1080)

    func testCoverageRequiresCompleteMonitor() {
        XCTAssertTrue(VisibilityGeometry.covers(monitor, monitor: monitor))
        XCTAssertFalse(VisibilityGeometry.covers(CGRect(x: 1, y: 0, width: 1919, height: 1080), monitor: monitor))
    }

    func testFallbackRequiresUntitledBorderlessCandidate() {
        XCTAssertTrue(VisibilityGeometry.isLikelyBorderless("", window: monitor, monitor: monitor))
        XCTAssertFalse(VisibilityGeometry.isLikelyBorderless("Document", window: monitor, monitor: monitor))
        XCTAssertFalse(VisibilityGeometry.isLikelyBorderless("", window: CGRect(x: 0, y: 0, width: 1800, height: 1080), monitor: monitor))
    }

    func testAccessibilityDenialKeepsPillVisible() {
        XCTAssertFalse(VisibilityGeometry.shouldHide(accessibilityTrusted: false, axFullscreen: true,
            focusedWindow: monitor, geometryFallback: true, monitor: monitor))
    }

    func testExplicitAXFalseWinsOverGeometry() {
        XCTAssertFalse(VisibilityGeometry.shouldHide(accessibilityTrusted: true, axFullscreen: false,
            focusedWindow: monitor, geometryFallback: true, monitor: monitor))
    }

    func testMissingAXUsesGeometryFallback() {
        XCTAssertTrue(VisibilityGeometry.shouldHide(accessibilityTrusted: true, axFullscreen: nil,
            focusedWindow: nil, geometryFallback: true, monitor: monitor))
    }
    func testOtherMonitorAXFalseDoesNotOverrideFullscreenOccupancy() {
        let other = CGRect(x: 1920, y: 0, width: 1920, height: 1080)
        XCTAssertTrue(VisibilityGeometry.shouldHide(accessibilityTrusted: true, axFullscreen: false,
            focusedWindow: other, geometryFallback: true, monitor: monitor))
        XCTAssertFalse(VisibilityGeometry.shouldHide(accessibilityTrusted: true, axFullscreen: true,
            focusedWindow: other, geometryFallback: false, monitor: monitor))
    }
}
