import ApplicationServices
import CoreFoundation
import CoreGraphics
import Foundation

/// Crash-safe AX attribute decoding. Every bridge from `CFTypeRef` checks the CF type ID
/// before casting; every AXValue extraction checks both the type ID and the stored value
/// type, and requires AXValueGetValue to report success. Any mismatch returns nil —
/// callers must treat nil as "unavailable" and fall back conservatively, never fabricate.
public enum AXAttributeReader {
    /// Copies an attribute; nil on any error or missing value. Never throws, never traps.
    public static func copyAttribute(_ element: AXUIElement, _ name: String) -> CFTypeRef? {
        var value: CFTypeRef?
        guard AXUIElementCopyAttributeValue(element, name as CFString, &value) == .success else { return nil }
        return value
    }

    /// Returns the value only when it is genuinely an AXUIElement (CF type ID check —
    /// a forced `as! AXUIElement` on a wrong type crashes).
    public static func element(_ value: CFTypeRef?) -> AXUIElement? {
        guard let value, CFGetTypeID(value) == AXUIElementGetTypeID() else { return nil }
        return unsafeDowncast(value as AnyObject, to: AXUIElement.self)
    }

    /// Decodes an AX boolean-ish attribute. Accepts CFBoolean via type ID check; as a
    /// defensive fallback accepts NSNumber booleans. Anything else is unavailable (nil).
    public static func bool(_ value: CFTypeRef?) -> Bool? {
        guard let value else { return nil }
        if CFGetTypeID(value) == CFBooleanGetTypeID() {
            return CFBooleanGetValue((value as! CFBoolean))
        }
        if let number = value as? NSNumber, CFGetTypeID(number) == CFNumberGetTypeID() {
            guard number.doubleValue == 0 || number.doubleValue == 1 else { return nil }
            return number.boolValue
        }
        return nil
    }

    /// Extracts a CGPoint only when the value is an AXValue storing .cgPoint and the
    /// extraction succeeds. Returns nil otherwise (no partial/zero geometry).
    public static func point(_ value: CFTypeRef?) -> CGPoint? {
        guard let ax = axValue(value, expecting: .cgPoint) else { return nil }
        var point = CGPoint.zero
        guard AXValueGetValue(ax, .cgPoint, &point) else { return nil }
        return point
    }

    /// Extracts a CGSize only when the value is an AXValue storing .cgSize and the
    /// extraction succeeds. Returns nil otherwise.
    public static func size(_ value: CFTypeRef?) -> CGSize? {
        guard let ax = axValue(value, expecting: .cgSize) else { return nil }
        var size = CGSize.zero
        guard AXValueGetValue(ax, .cgSize, &size) else { return nil }
        return size
    }

    private static func axValue(_ value: CFTypeRef?, expecting type: AXValueType) -> AXValue? {
        guard let value, CFGetTypeID(value) == AXValueGetTypeID() else { return nil }
        let ax = unsafeDowncast(value as AnyObject, to: AXValue.self)
        guard AXValueGetType(ax) == type else { return nil }
        return ax
    }
}
