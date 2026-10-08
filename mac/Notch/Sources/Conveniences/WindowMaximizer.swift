import AppKit
import ApplicationServices

/// Pulse: the green zoom button fills the screen's visible frame instead of
/// entering a full-screen Space. Option-click keeps macOS's own behaviour, and
/// so do windows already full screen or that cannot be resized.
///
/// A second click on a window that is still where the button put it restores
/// where it was. The frames and the move come from the window-management
/// engine (`Windows/WindowActions.swift`), so Restore and the shortcuts see the
/// same memory.
final class WindowMaximizer {
    private let lock = NSLock()
    private var swallowingMouseUp = false

    func handle(_ type: CGEventType, _ event: CGEvent) -> TapDecision {
        switch type {
        case .leftMouseDown:
            guard !event.flags.contains(.maskAlternate),
                  let window = zoomButtonWindow(at: event.location)
            else { return .pass }
            lock.lock(); swallowingMouseUp = true; lock.unlock()
            DispatchQueue.main.async { [weak self] in self?.toggle(window) }
            return .swallow
        case .leftMouseUp:
            lock.lock(); defer { lock.unlock() }
            guard swallowingMouseUp else { return .pass }
            swallowingMouseUp = false
            return .swallow
        default:
            return .pass
        }
    }

    func reset() {
        lock.lock(); swallowingMouseUp = false; lock.unlock()
    }

    /// The window whose zoom button is under the pointer, when this should
    /// take over the click.
    private func zoomButtonWindow(at location: CGPoint) -> AXUIElement? {
        guard let hit = AX.elementAt(location),
              AX.string(hit, "AXRole") == "AXButton",
              AX.string(hit, "AXSubrole") == "AXZoomButton",
              let window = AX.element(hit, "AXParent") ?? AX.element(hit, "AXWindow"),
              AX.string(window, "AXRole") == "AXWindow"
        else { return nil }
        var pid: pid_t = 0
        AXUIElementGetPid(window, &pid)
        guard pid != ProcessInfo.processInfo.processIdentifier else { return nil }
        if AX.bool(window, "AXFullScreen") == true { return nil }
        guard AX.isSettable(window, "AXSize"), AX.isSettable(window, "AXPosition") else { return nil }
        return window
    }

    private func toggle(_ window: AXUIElement) {
        WindowActions.toggleMaximize(window)
    }
}
