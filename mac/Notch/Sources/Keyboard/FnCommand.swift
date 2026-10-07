import CoreGraphics
import Foundation

/// Cockpit: Fn as Command. `FnKeyMapping` turns Fn into F18; this is the event
/// tap handler that gives F18 its meaning. While F18 is held, the letters
/// C V X A Z S F T W carry Command (Shift passes through, so Fn+Shift+Z is
/// Cmd+Shift+Z) and the arrows carry Option instead, which moves by word.
/// F18 itself is swallowed. Everything else passes unchanged.
final class FnCommand {
    private static let letters: Set<Int64> = [8, 9, 7, 0, 6, 1, 3, 17, 13]  // C V X A Z S F T W
    private static let arrows: Set<Int64> = [123, 124, 125, 126]

    private let lock = NSLock()
    private var held = false

    func reset() {
        lock.lock(); held = false; lock.unlock()
    }

    /// Tap thread.
    func handle(_ type: CGEventType, _ event: CGEvent) -> TapDecision {
        guard type == .keyDown || type == .keyUp else { return .pass }
        let key = event.getIntegerValueField(.keyboardEventKeycode)
        if key == FnKeyMapping.f18KeyCode {
            lock.lock(); held = (type == .keyDown); lock.unlock()
            return .swallow
        }
        lock.lock(); let isHeld = held; lock.unlock()
        guard isHeld else { return .pass }
        // A lost key-up must not leave the keyboard stuck: trust the hardware.
        guard CGEventSource.keyState(.hidSystemState, key: CGKeyCode(FnKeyMapping.f18KeyCode)) else {
            reset()
            return .pass
        }
        if Self.letters.contains(key) {
            event.flags.insert(.maskCommand)
        } else if Self.arrows.contains(key) {
            event.flags.remove(.maskCommand)
            event.flags.insert(.maskAlternate)
        }
        return .pass
    }
}
