import CoreGraphics
import Foundation

/// Cockpit: Fn as Command, on a HID-level event tap. Only Fn+C/V/X/A/Z/S/F/T/W
/// and Fn+arrows are touched: the letters become Command+key (Shift kept), the
/// arrows become Option+arrow (Shift kept), both without the Fn flag. Every
/// other event, Fn alone, Fn+Space, Fn+Delete and Fn+F-keys included, passes
/// untouched.
///
/// Arrows always carry the Fn flag on a Mac keyboard, so for them the real Fn
/// state comes from flagsChanged events for the Function key (keycode 63).
final class FnCommand {
    private static let fnKeyCode: Int64 = 63
    private static let letters: Set<Int64> = [8, 9, 7, 0, 6, 1, 3, 17, 13]  // C V X A Z S F T W
    private static let arrows: Set<Int64> = [123, 124, 125, 126]

    private let lock = NSLock()
    private var fnDown = false

    func reset() {
        lock.lock(); fnDown = false; lock.unlock()
    }

    /// Tap thread.
    func handle(_ type: CGEventType, _ event: CGEvent) -> TapDecision {
        let key = event.getIntegerValueField(.keyboardEventKeycode)
        switch type {
        case .flagsChanged:
            if key == Self.fnKeyCode {
                lock.lock(); fnDown = event.flags.contains(.maskSecondaryFn); lock.unlock()
            }
        case .keyDown, .keyUp:
            let isLetter = Self.letters.contains(key)
            let isArrow = Self.arrows.contains(key)
            guard isLetter || isArrow, event.flags.contains(.maskSecondaryFn) else { return .pass }
            if isArrow {
                lock.lock(); let down = fnDown; lock.unlock()
                guard down else { return .pass }
            }
            var flags = event.flags
            flags.remove(.maskSecondaryFn)
            flags.insert(isLetter ? .maskCommand : .maskAlternate)
            event.flags = flags
        default:
            break
        }
        return .pass
    }
}
