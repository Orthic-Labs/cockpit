import AppKit
import ApplicationServices

/// Pulse: quits apps the person opted in once their last window closes.
///
/// A window closing (an Accessibility notification), then a short debounce,
/// then every check below must pass or nothing happens:
/// - the app lists its windows through Accessibility and the list is empty;
/// - the system's window list, which includes minimized windows and windows on
///   other Spaces, has no normal window for that process either;
/// - the app is an ordinary foreground app that has finished launching.
/// Anything unknown refuses. The quit is `terminate()`, the polite request
/// the app can still answer with a save prompt, never a forced kill.
@MainActor
final class AutoQuit {
    private static let debounce: TimeInterval = 1.5
    private static let neverQuit: Set<String> = ["com.apple.finder", "com.apple.dock", "dev.orthic.pulse"]
    nonisolated(unsafe) fileprivate static weak var current: AutoQuit?

    private var bundleIDs: Set<String> = []
    private var observers: [pid_t: AXObserver] = [:]
    private var pending: [pid_t: DispatchWorkItem] = [:]
    private var workspaceTokens: [NSObjectProtocol] = []

    var isRunning: Bool { !workspaceTokens.isEmpty }

    func start(bundleIDs: [String]) {
        Self.current = self
        self.bundleIDs = Set(bundleIDs).subtracting(Self.neverQuit)
        if workspaceTokens.isEmpty {
            let center = NSWorkspace.shared.notificationCenter
            workspaceTokens.append(center.addObserver(
                forName: NSWorkspace.didLaunchApplicationNotification, object: nil, queue: .main
            ) { [weak self] note in
                guard let app = note.userInfo?[NSWorkspace.applicationUserInfoKey] as? NSRunningApplication
                else { return }
                MainActor.assumeIsolated { self?.attachIfListed(app) }
            })
            workspaceTokens.append(center.addObserver(
                forName: NSWorkspace.didTerminateApplicationNotification, object: nil, queue: .main
            ) { [weak self] note in
                guard let app = note.userInfo?[NSWorkspace.applicationUserInfoKey] as? NSRunningApplication
                else { return }
                MainActor.assumeIsolated { self?.detach(app.processIdentifier) }
            })
        }
        for pid in Array(observers.keys) {
            let id = NSRunningApplication(processIdentifier: pid)?.bundleIdentifier
            if id == nil || !self.bundleIDs.contains(id!) { detach(pid) }
        }
        for app in NSWorkspace.shared.runningApplications { attachIfListed(app) }
    }

    func stop() {
        let center = NSWorkspace.shared.notificationCenter
        workspaceTokens.forEach { center.removeObserver($0) }
        workspaceTokens.removeAll()
        for pid in Array(observers.keys) { detach(pid) }
        pending.values.forEach { $0.cancel() }
        pending.removeAll()
        bundleIDs.removeAll()
        if Self.current === self { Self.current = nil }
    }

    // MARK: - Watching windows

    private func attachIfListed(_ app: NSRunningApplication) {
        guard let id = app.bundleIdentifier, bundleIDs.contains(id),
              app.activationPolicy == .regular, observers[app.processIdentifier] == nil
        else { return }
        let pid = app.processIdentifier
        var created: AXObserver?
        let callback: AXObserverCallback = { _, element, notification, refcon in
            let pid = pid_t(Int(bitPattern: refcon))
            let name = notification as String
            DispatchQueue.main.async {
                MainActor.assumeIsolated { AutoQuit.current?.windowEvent(pid: pid, name: name, element: element) }
            }
        }
        guard AXObserverCreate(pid, callback, &created) == .success, let observer = created else { return }
        let refcon = UnsafeMutableRawPointer(bitPattern: Int(pid))
        let application = AX.application(pid)
        AXObserverAddNotification(observer, application, "AXWindowCreated" as CFString, refcon)
        AXObserverAddNotification(observer, application, "AXFocusedWindowChanged" as CFString, refcon)
        for window in AX.windows(of: pid) ?? [] {
            AXObserverAddNotification(observer, window, "AXUIElementDestroyed" as CFString, refcon)
        }
        CFRunLoopAddSource(CFRunLoopGetMain(), AXObserverGetRunLoopSource(observer), .commonModes)
        observers[pid] = observer
    }

    private func detach(_ pid: pid_t) {
        pending[pid]?.cancel()
        pending[pid] = nil
        guard let observer = observers.removeValue(forKey: pid) else { return }
        CFRunLoopRemoveSource(CFRunLoopGetMain(), AXObserverGetRunLoopSource(observer), .commonModes)
    }

    fileprivate func windowEvent(pid: pid_t, name: String, element: AXUIElement) {
        guard let observer = observers[pid] else { return }
        if name == "AXWindowCreated" {
            // A new window: the app is not finished, and this one needs
            // watching too.
            pending[pid]?.cancel()
            pending[pid] = nil
            AXObserverAddNotification(observer, element, "AXUIElementDestroyed" as CFString,
                                      UnsafeMutableRawPointer(bitPattern: Int(pid)))
            return
        }
        pending[pid]?.cancel()
        let work = DispatchWorkItem { [weak self] in
            MainActor.assumeIsolated { self?.quitIfFinished(pid) }
        }
        pending[pid] = work
        DispatchQueue.main.asyncAfter(deadline: .now() + Self.debounce, execute: work)
    }

    // MARK: - The decision

    private func quitIfFinished(_ pid: pid_t) {
        pending[pid] = nil
        guard let app = NSRunningApplication(processIdentifier: pid), !app.isTerminated,
              app.isFinishedLaunching, app.activationPolicy == .regular,
              let id = app.bundleIdentifier, bundleIDs.contains(id), !Self.neverQuit.contains(id)
        else { return }
        // Known and empty through Accessibility. An unreadable list is unknown.
        guard let windows = AX.windows(of: pid), windows.isEmpty else { return }
        // And none anywhere else: minimized, hidden, or on another Space.
        guard !Self.hasAnyWindow(pid) else { return }
        app.terminate()
    }

    private static func hasAnyWindow(_ pid: pid_t) -> Bool {
        guard let list = CGWindowListCopyWindowInfo([.optionAll], kCGNullWindowID) as? [[String: Any]]
        else { return true }
        return list.contains { info in
            guard (info[kCGWindowOwnerPID as String] as? Int32) == pid,
                  (info[kCGWindowLayer as String] as? Int) == 0
            else { return false }
            let bounds = info[kCGWindowBounds as String] as? [String: CGFloat]
            return (bounds?["Width"] ?? 100) >= 40 && (bounds?["Height"] ?? 100) >= 40
        }
    }
}
