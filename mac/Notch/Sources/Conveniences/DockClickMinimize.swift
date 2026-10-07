import AppKit
import ApplicationServices

/// Cockpit: clicking the Dock icon of the app that is already frontmost
/// minimizes its windows. The click itself is never swallowed, so the Dock
/// behaves as usual for every other icon and for an app with nothing to
/// minimize (its minimized windows still come back).
final class DockClickMinimize {
    private let lock = NSLock()
    private var pending: (pid: pid_t, location: CGPoint)?

    func handle(_ type: CGEventType, _ event: CGEvent) -> TapDecision {
        switch type {
        case .leftMouseDown:
            lock.lock(); pending = nil; lock.unlock()
            let blockers: CGEventFlags = [.maskCommand, .maskAlternate, .maskControl, .maskShift]
            guard event.flags.intersection(blockers).isEmpty,
                  let front = NSWorkspace.shared.frontmostApplication,
                  front.processIdentifier != ProcessInfo.processInfo.processIdentifier,
                  let appURL = front.bundleURL,
                  let hit = AX.elementAt(event.location),
                  AX.string(hit, "AXRole") == "AXDockItem",
                  let itemURL = AX.url(hit),
                  itemURL.standardizedFileURL == appURL.standardizedFileURL,
                  !Self.visibleWindows(of: front.processIdentifier).isEmpty
            else { return .pass }
            lock.lock(); pending = (front.processIdentifier, event.location); lock.unlock()
        case .leftMouseUp:
            lock.lock(); let click = pending; pending = nil; lock.unlock()
            guard let click else { return .pass }
            let here = event.location
            // A drag that started on the icon is a rearrangement, not a click.
            guard hypot(here.x - click.location.x, here.y - click.location.y) < 6 else { return .pass }
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.05) {
                for window in Self.visibleWindows(of: click.pid) {
                    AXUIElementSetAttributeValue(window, "AXMinimized" as CFString, kCFBooleanTrue)
                }
            }
        default:
            break
        }
        return .pass
    }

    func reset() {
        lock.lock(); pending = nil; lock.unlock()
    }

    /// Standard windows that are on screen now. Unknown reads as none.
    private static func visibleWindows(of pid: pid_t) -> [AXUIElement] {
        (AX.windows(of: pid) ?? []).filter {
            AX.string($0, "AXSubrole") == "AXStandardWindow" && AX.bool($0, "AXMinimized") == false
        }
    }
}
