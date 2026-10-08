import AppKit
import ApplicationServices

/// Pulse: Cut and Paste for files in Finder.
///
/// Finder has no file cut; its move is Copy, then Option-Command-V ("Move Item
/// Here"). So ⌘X posts a real ⌘C to Finder and remembers the pasteboard's
/// change count right after the copy, and the next ⌘V posts ⌥⌘V instead, as
/// long as the count has not changed. Any other copy changes the count (or is
/// a ⌘C, which clears the cut), so the cut never outlives what it marked.
/// Finder does the moving: clashes, other volumes and undo are its own.
///
/// This sees ⌘X from the real Command key and from Fn as Command alike: the
/// tap sits at the session level, downstream of the HID tap that Fn as Command
/// posts to. Chords posted here carry `marker` and pass straight through.
/// While a Finder text field has focus (renaming, search, path bar) the keys
/// keep their normal meaning.
final class FinderCutPaste {
    private static let finderID = "com.apple.finder"
    private static let keyC: Int64 = 8
    private static let keyX: Int64 = 7
    private static let keyV: Int64 = 9
    private static let marker: Int64 = 0x4366_7470  // "Cftp", on posted events

    private struct Cut {
        let before: Int
        var changeCount: Int?
    }

    private let lock = NSLock()
    private var cut: Cut?
    /// Keys whose keyDown was swallowed, so their keyUp is too.
    private var swallowedKeys: Set<Int64> = []
    private let source = CGEventSource(stateID: .hidSystemState)

    func reset() {
        lock.lock(); cut = nil; swallowedKeys = []; lock.unlock()
    }

    // MARK: - Tap handler (tap thread)

    func handle(_ type: CGEventType, _ event: CGEvent) -> TapDecision {
        guard type == .keyDown || type == .keyUp else { return .pass }
        if event.getIntegerValueField(.eventSourceUserData) == Self.marker { return .pass }
        let key = event.getIntegerValueField(.keyboardEventKeycode)
        if type == .keyUp {
            lock.lock(); let swallow = swallowedKeys.remove(key) != nil; lock.unlock()
            return swallow ? .swallow : .pass
        }
        let modifiers: CGEventFlags = [.maskCommand, .maskShift, .maskAlternate, .maskControl]
        guard event.flags.intersection(modifiers) == .maskCommand,
              key == Self.keyC || key == Self.keyX || key == Self.keyV,
              let finder = NSWorkspace.shared.frontmostApplication,
              finder.bundleIdentifier == Self.finderID,
              Self.textInputIsNotFocused(pid: finder.processIdentifier)
        else { return .pass }

        switch key {
        case Self.keyC:
            reset()
            return .pass
        case Self.keyX:
            let before = NSPasteboard.general.changeCount
            lock.lock(); cut = Cut(before: before, changeCount: nil); swallowedKeys.insert(key); lock.unlock()
            Self.post(Self.keyC, flags: .maskCommand)
            DispatchQueue.global().asyncAfter(deadline: .now() + 0.25) { [weak self] in self?.settle() }
            return .swallow
        default:
            // ⌘V: only ours while the cut's copy is still the pasteboard's content.
            guard settle(wait: 0.3) else { return .pass }
            lock.lock(); cut = nil; swallowedKeys.insert(key); lock.unlock()
            Self.post(Self.keyV, flags: [.maskCommand, .maskAlternate])
            return .swallow
        }
    }

    /// Records the change count once Finder's copy has landed. With `wait`,
    /// polls briefly for it and answers whether a cut is pending and intact.
    @discardableResult
    private func settle(wait: TimeInterval = 0) -> Bool {
        let deadline = Date().addingTimeInterval(wait)
        while true {
            lock.lock()
            guard var pending = cut else { lock.unlock(); return false }
            let now = NSPasteboard.general.changeCount
            if pending.changeCount == nil, now != pending.before {
                pending.changeCount = now
                cut = pending
            }
            let intact = pending.changeCount == now
            let landed = pending.changeCount != nil
            if landed && !intact { cut = nil }
            lock.unlock()
            if landed { return intact }
            if Date() >= deadline {
                // The copy never landed (nothing selected): nothing was cut.
                if wait > 0 { lock.lock(); cut = nil; lock.unlock() }
                return false
            }
            Thread.sleep(forTimeInterval: 0.02)
        }
    }

    private static func post(_ key: Int64, flags: CGEventFlags) {
        for down in [true, false] {
            guard let event = CGEvent(keyboardEventSource: nil, virtualKey: CGKeyCode(key), keyDown: down) else { continue }
            event.flags = flags
            event.setIntegerValueField(.eventSourceUserData, value: marker)
            event.post(tap: .cghidEventTap)
        }
    }

    /// True only when the focused element is positively known and is not a
    /// text input. Unknown keeps the key's normal meaning.
    private static func textInputIsNotFocused(pid: pid_t) -> Bool {
        guard AXIsProcessTrusted() else { return false }
        let app = AX.application(pid, timeout: 0.1)
        guard let focused = AX.element(app, "AXFocusedUIElement") else { return false }
        let role = AX.string(focused, "AXRole") ?? ""
        let subrole = AX.string(focused, "AXSubrole") ?? ""
        return !["AXTextField", "AXTextArea", "AXComboBox"].contains(role) && subrole != "AXSearchField"
    }
}
