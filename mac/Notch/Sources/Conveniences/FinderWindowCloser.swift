// Copyright (c) 2026 Damned Ventures LLC, d/b/a Orthic Labs. All rights reserved.

import AppKit
import ApplicationServices

/// **Closes the Finder window Finder opens for a mounted disk image**, through
/// the Accessibility permission Pulse already holds (no Finder Automation).
/// Only a window that shows exactly that volume's root is closed: matched by its
/// document URL, or by its title when it states no document. Finder opens the
/// window a moment after the mount, so the look repeats for about three seconds.
enum FinderWindowCloser {
    private static let finderID = "com.apple.finder"
    private static let patience: TimeInterval = 3
    private static let interval: UInt64 = 300_000_000

    /// Starts the closing in the background; returns at once. Nothing happens
    /// when Accessibility is not granted.
    static func closeWindows(forVolume mount: URL) {
        guard AXIsProcessTrusted() else { return }
        let root = mount.standardizedFileURL.resolvingSymlinksInPath().path
        let name = (try? mount.resourceValues(forKeys: [.volumeNameKey]))?.volumeName
            ?? mount.lastPathComponent
        Task.detached(priority: .utility) {
            let deadline = Date().addingTimeInterval(patience)
            repeat {
                if closeMatching(root: root, name: name) { return }
                try? await Task.sleep(nanoseconds: interval)
            } while Date() < deadline
        }
    }

    /// True when a window was closed.
    private static func closeMatching(root: String, name: String) -> Bool {
        guard let finder = NSRunningApplication.runningApplications(withBundleIdentifier: finderID).first
        else { return false }
        let app = AXUIElementCreateApplication(finder.processIdentifier)
        AXUIElementSetMessagingTimeout(app, 1)
        var value: CFTypeRef?
        guard AXUIElementCopyAttributeValue(app, kAXWindowsAttribute as CFString, &value) == .success,
              let windows = value as? [AXUIElement] else { return false }
        var closed = false
        for window in windows where matches(window, root: root, name: name) {
            var button: CFTypeRef?
            guard AXUIElementCopyAttributeValue(window, kAXCloseButtonAttribute as CFString, &button) == .success,
                  let button, CFGetTypeID(button) == AXUIElementGetTypeID() else { continue }
            // swiftlint:disable:next force_cast
            if AXUIElementPerformAction(button as! AXUIElement, kAXPressAction as CFString) == .success {
                closed = true
            }
        }
        return closed
    }

    private static func matches(_ window: AXUIElement, root: String, name: String) -> Bool {
        var document: CFTypeRef?
        if AXUIElementCopyAttributeValue(window, kAXDocumentAttribute as CFString, &document) == .success,
           let text = document as? String, !text.isEmpty {
            let path = URL(string: text)?.standardizedFileURL.resolvingSymlinksInPath().path
                ?? URL(fileURLWithPath: text).standardizedFileURL.resolvingSymlinksInPath().path
            return path == root
        }
        // No document: the title is all there is.
        var title: CFTypeRef?
        guard AXUIElementCopyAttributeValue(window, kAXTitleAttribute as CFString, &title) == .success,
              let text = title as? String else { return false }
        return text == name
    }
}
