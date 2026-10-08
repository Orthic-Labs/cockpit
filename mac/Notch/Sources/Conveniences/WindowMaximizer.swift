import AppKit
import ApplicationServices

/// Pulse: the green zoom button fills the screen's visible frame instead of
/// entering a full-screen Space. Option-click keeps macOS's own behaviour, and
/// so do windows already full screen or that cannot be resized.
///
/// A second click on a window that is still where the button put it restores
/// where it was.
final class WindowMaximizer {
    private let lock = NSLock()
    private var swallowingMouseUp = false
    /// Frames to go back to, keyed by the window element's hash.
    private var previous: [CFHashCode: CGRect] = [:]

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
        lock.lock(); swallowingMouseUp = false; previous.removeAll(); lock.unlock()
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
        guard let frame = AX.frame(of: window) else { return }
        // Accessibility coordinates have their origin at the top-left of the
        // primary display; AppKit's at its bottom-left.
        guard let primary = NSScreen.screens.first else { return }
        let center = CGPoint(x: frame.midX, y: primary.frame.height - frame.midY)
        let screen = NSScreen.screens.first { $0.frame.contains(center) } ?? primary
        let visible = screen.visibleFrame
        let target = CGRect(x: visible.minX, y: primary.frame.height - visible.maxY,
                            width: visible.width, height: visible.height)
        let key = CFHash(window)
        lock.lock()
        let back = previous[key]
        lock.unlock()
        if let back, Self.near(frame, target) {
            AX.setFrame(back, of: window)
            lock.lock(); previous[key] = nil; lock.unlock()
        } else {
            if !Self.near(frame, target) {
                lock.lock(); previous[key] = frame; lock.unlock()
            }
            AX.setFrame(target, of: window)
        }
    }

    private static func near(_ a: CGRect, _ b: CGRect) -> Bool {
        abs(a.minX - b.minX) < 3 && abs(a.minY - b.minY) < 3
            && abs(a.width - b.width) < 3 && abs(a.height - b.height) < 3
    }
}
