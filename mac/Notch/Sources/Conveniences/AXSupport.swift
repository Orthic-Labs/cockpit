import ApplicationServices
import AppKit

/// Pulse: thin Accessibility helpers shared by the Mac conveniences.
/// Every read has a short messaging timeout so a hung app cannot stall the
/// event tap, and every failure reads as "unknown" (nil), never as a guess.
enum AX {
    static let systemWide: AXUIElement = {
        let element = AXUIElementCreateSystemWide()
        AXUIElementSetMessagingTimeout(element, 0.15)
        return element
    }()

    static func application(_ pid: pid_t, timeout: Float = 0.25) -> AXUIElement {
        let element = AXUIElementCreateApplication(pid)
        AXUIElementSetMessagingTimeout(element, timeout)
        return element
    }

    static func attribute(_ element: AXUIElement, _ name: String) -> CFTypeRef? {
        var value: CFTypeRef?
        guard AXUIElementCopyAttributeValue(element, name as CFString, &value) == .success else { return nil }
        return value
    }

    static func string(_ element: AXUIElement, _ name: String) -> String? {
        attribute(element, name) as? String
    }

    static func bool(_ element: AXUIElement, _ name: String) -> Bool? {
        (attribute(element, name) as? NSNumber)?.boolValue
    }

    static func element(_ element: AXUIElement, _ name: String) -> AXUIElement? {
        guard let value = attribute(element, name), CFGetTypeID(value) == AXUIElementGetTypeID()
        else { return nil }
        return (value as! AXUIElement)
    }

    static func elements(_ element: AXUIElement, _ name: String) -> [AXUIElement]? {
        attribute(element, name) as? [AXUIElement]
    }

    static func url(_ element: AXUIElement) -> URL? {
        attribute(element, "AXURL") as? URL
    }

    static func point(_ element: AXUIElement, _ name: String) -> CGPoint? {
        guard let value = attribute(element, name), CFGetTypeID(value) == AXValueGetTypeID() else { return nil }
        var point = CGPoint.zero
        return AXValueGetValue(value as! AXValue, .cgPoint, &point) ? point : nil
    }

    static func size(_ element: AXUIElement, _ name: String) -> CGSize? {
        guard let value = attribute(element, name), CFGetTypeID(value) == AXValueGetTypeID() else { return nil }
        var size = CGSize.zero
        return AXValueGetValue(value as! AXValue, .cgSize, &size) ? size : nil
    }

    static func frame(of window: AXUIElement) -> CGRect? {
        guard let origin = point(window, "AXPosition"), let size = size(window, "AXSize") else { return nil }
        return CGRect(origin: origin, size: size)
    }

    @discardableResult
    static func setFrame(_ rect: CGRect, of window: AXUIElement) -> Bool {
        var origin = rect.origin
        var size = rect.size
        guard let p = AXValueCreate(.cgPoint, &origin), let s = AXValueCreate(.cgSize, &size) else { return false }
        // Position, size, position: an app that clamps the size at the old
        // origin still lands where asked.
        let a = AXUIElementSetAttributeValue(window, "AXPosition" as CFString, p)
        let b = AXUIElementSetAttributeValue(window, "AXSize" as CFString, s)
        let c = AXUIElementSetAttributeValue(window, "AXPosition" as CFString, p)
        return a == .success && b == .success && c == .success
    }

    static func isSettable(_ element: AXUIElement, _ name: String) -> Bool {
        var settable = DarwinBoolean(false)
        guard AXUIElementIsAttributeSettable(element, name as CFString, &settable) == .success else { return false }
        return settable.boolValue
    }

    static func elementAt(_ location: CGPoint) -> AXUIElement? {
        var found: AXUIElement?
        let result = AXUIElementCopyElementAtPosition(systemWide, Float(location.x), Float(location.y), &found)
        return result == .success ? found : nil
    }

    static func windows(of pid: pid_t) -> [AXUIElement]? {
        elements(application(pid), "AXWindows")
    }
}
