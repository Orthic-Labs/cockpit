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
///
/// Some apps (Flutter ones such as LocalSend) track modifier keys from their
/// own press/release events and ignore the flag on the key itself, so the
/// first remapped key also posts a real Command (or Option) press, released
/// when Fn comes up.
final class FnCommand {
    private static let fnKeyCode: Int64 = 63
    private static let letters: Set<Int64> = [8, 9, 7, 0, 6, 1, 3, 17, 13]  // C V X A Z S F T W
    private static let arrows: Set<Int64> = [123, 124, 125, 126]

    private let lock = NSLock()
    private var fnDown = false
    /// Modifier key codes this tap pressed and has not yet released.
    private var held: [Int64: CGEventFlags] = [:]
    private static let marker: Int64 = 0x436B_666E  // "Ckfn", on posted events
    private static let commandKey: Int64 = 55
    private static let optionKey: Int64 = 58

    func reset() {
        lock.lock(); fnDown = false; let keys = held; held = [:]; lock.unlock()
        for key in keys.keys { post(key, down: false, flags: []) }
    }

    private func post(_ key: Int64, down: Bool, flags: CGEventFlags) {
        guard let event = CGEvent(keyboardEventSource: nil, virtualKey: CGKeyCode(key), keyDown: down) else { return }
        event.type = .flagsChanged
        event.flags = flags
        event.setIntegerValueField(.eventSourceUserData, value: Self.marker)
        event.post(tap: .cghidEventTap)
    }

    /// Tap thread.
    func handle(_ type: CGEventType, _ event: CGEvent) -> TapDecision {
        if event.getIntegerValueField(.eventSourceUserData) == Self.marker { return .pass }
        let key = event.getIntegerValueField(.keyboardEventKeycode)
        switch type {
        case .flagsChanged:
            if key == Self.fnKeyCode {
                let down = event.flags.contains(.maskSecondaryFn)
                lock.lock(); fnDown = down; let release = down ? [:] : held; if !down { held = [:] }; lock.unlock()
                for code in release.keys { post(code, down: false, flags: []) }
            }
        case .keyDown, .keyUp:
            let isLetter = Self.letters.contains(key)
            let isArrow = Self.arrows.contains(key)
            guard isLetter || isArrow, event.flags.contains(.maskSecondaryFn) else { return .pass }
            if isArrow {
                lock.lock(); let down = fnDown; lock.unlock()
                guard down else { return .pass }
            }
            let modifier: CGEventFlags = isLetter ? .maskCommand : .maskAlternate
            let modifierKey = isLetter ? Self.commandKey : Self.optionKey
            var isNew = false
            if type == .keyDown {
                lock.lock(); isNew = held[modifierKey] == nil; if isNew { held[modifierKey] = modifier }; lock.unlock()
            }
            var flags = event.flags
            flags.remove(.maskSecondaryFn)
            flags.insert(modifier)
            event.flags = flags
            // Post the modifier press, then the key, so the press always lands first.
            if isNew, let copy = event.copy() {
                post(modifierKey, down: true, flags: modifier)
                copy.setIntegerValueField(.eventSourceUserData, value: Self.marker)
                copy.post(tap: .cghidEventTap)
                return .swallow
            }
        default:
            break
        }
        return .pass
    }
}
