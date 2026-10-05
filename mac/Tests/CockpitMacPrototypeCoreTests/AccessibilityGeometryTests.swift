import ApplicationServices
import CoreGraphics
import XCTest
@testable import CockpitMacPrototypeCore

/// AXAttributeReader must never crash and must return nil ("unavailable") on any
/// type mismatch. AXValueCreate needs no TCC grant, so real AXValues are exercised.
final class AccessibilityGeometryTests: XCTestCase {
    private func axPoint(_ point: CGPoint) -> AXValue {
        var p = point
        return AXValueCreate(.cgPoint, &p)!
    }

    private func axSize(_ size: CGSize) -> AXValue {
        var s = size
        return AXValueCreate(.cgSize, &s)!
    }

    func testElementRejectsNonAXUIElement() {
        XCTAssertNil(AXAttributeReader.element(nil))
        XCTAssertNil(AXAttributeReader.element("a string" as CFString))
        XCTAssertNil(AXAttributeReader.element(NSNumber(value: 1)))
        XCTAssertNil(AXAttributeReader.element(axPoint(.zero)))
    }

    func testBoolDecoding() {
        XCTAssertEqual(AXAttributeReader.bool(kCFBooleanTrue), true)
        XCTAssertEqual(AXAttributeReader.bool(kCFBooleanFalse), false)
        XCTAssertEqual(AXAttributeReader.bool(NSNumber(value: 1)), true)
        XCTAssertEqual(AXAttributeReader.bool(NSNumber(value: 0)), false)
        XCTAssertNil(AXAttributeReader.bool(NSNumber(value: 2)))
        // Wrong types are unavailable, never a fabricated answer.
        XCTAssertNil(AXAttributeReader.bool(nil))
        XCTAssertNil(AXAttributeReader.bool("yes" as CFString))
        XCTAssertNil(AXAttributeReader.bool(axPoint(.zero)))
    }

    func testPointDecoding() {
        let expected = CGPoint(x: -40.5, y: 120)
        XCTAssertEqual(AXAttributeReader.point(axPoint(expected)), expected)
        // A .cgSize AXValue must not decode through the .cgPoint slot.
        XCTAssertNil(AXAttributeReader.point(axSize(CGSize(width: 10, height: 10))))
        XCTAssertNil(AXAttributeReader.point(nil))
        XCTAssertNil(AXAttributeReader.point("0,0" as CFString))
        XCTAssertNil(AXAttributeReader.point(kCFBooleanTrue))
    }

    func testSizeDecoding() {
        let expected = CGSize(width: 1920, height: 1080)
        XCTAssertEqual(AXAttributeReader.size(axSize(expected)), expected)
        XCTAssertNil(AXAttributeReader.size(axPoint(.zero)))
        XCTAssertNil(AXAttributeReader.size(nil))
        XCTAssertNil(AXAttributeReader.size(NSNumber(value: 5)))
    }

    func testCopyAttributeOnForeignTypeFails() {
        // Passing a non-AXUIElement would crash unchecked callers; AXUIElementCopyAttributeValue
        // on a real element with a bogus attribute returns an error, not a trap.
        let wide = AXUIElementCreateSystemWide()
        XCTAssertNil(AXAttributeReader.copyAttribute(wide, "AXNotARealAttribute"))
    }
}
