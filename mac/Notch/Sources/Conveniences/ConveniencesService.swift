import AppKit
import ApplicationServices
import Combine

/// Pulse: Mac conveniences (Finder cut/paste, window maximizer, Dock click
/// minimize, Auto Quit), all off until chosen in the hub.
///
/// One `EventTapHub` serves the three event features; Auto Quit watches
/// windows through Accessibility notifications. Every feature needs the
/// Accessibility permission. Without it nothing starts, the hub is told, and
/// nothing prompts: the person is pointed at System Settings instead, and the
/// permission is re-read every few seconds while a feature is waiting for it.
@MainActor
final class ConveniencesService {
    private static let settingsURL =
        "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility"

    private let preferences: Preferences
    private let hub = EventTapHub()
    private let finder = FinderCutPaste()
    private let maximizer = WindowMaximizer()
    private let dock = DockClickMinimize()
    private let fn = FnCommand()
    private var fnFailure: String?
    private let fnHub = EventTapHub(location: .cghidEventTap, events: [.keyDown, .keyUp, .flagsChanged])
    private var fnToken: EventTapToken?
    private let autoQuit = AutoQuit()
    /// Window-management shortcuts (`Windows/`); own Carbon hotkeys, no tap.
    private lazy var windowHotKeys = WindowHotKeys(preferences: preferences)
    private lazy var diskImage = DiskImageInstaller(preferences: preferences)
    private var tokens: [String: EventTapToken] = [:]
    private var cancellables = Set<AnyCancellable>()
    private var recheck: Timer?
    private var workspaceTokens: [NSObjectProtocol] = []
    private var published = ""

    /// Called when anything the hub shows may have changed.
    var onChange: (() -> Void)?

    init(preferences: Preferences) {
        self.preferences = preferences
    }

    func start() {
        preferences.objectWillChange
            .sink { [weak self] _ in DispatchQueue.main.async { self?.reconcile() } }
            .store(in: &cancellables)
        // The hub's "add a running app" list follows what is running.
        let center = NSWorkspace.shared.notificationCenter
        for name in [NSWorkspace.didLaunchApplicationNotification, NSWorkspace.didTerminateApplicationNotification] {
            workspaceTokens.append(center.addObserver(forName: name, object: nil, queue: .main) { [weak self] _ in
                MainActor.assumeIsolated { self?.onChange?() }
            })
        }
        windowHotKeys.onChange = { [weak self] in self?.onChange?() }
        windowHotKeys.start()
        reconcile()
    }

    func stop() {
        cancellables.removeAll()
        recheck?.invalidate()
        recheck = nil
        let center = NSWorkspace.shared.notificationCenter
        workspaceTokens.forEach { center.removeObserver($0) }
        workspaceTokens.removeAll()
        tearDown()
        diskImage.stop()
        windowHotKeys.stop()
    }

    /// Disk image cards go to the notch: the app delegate points this at the
    /// fleet, and sends the notch's answers and hover back.
    var presentDiskImage: ((DiskImagePrompt?) -> Bool)? {
        get { diskImage.present }
        set { diskImage.present = newValue }
    }

    func diskImageChoice(_ choice: DiskImageChoice) { diskImage.choose(choice) }

    func diskImageHover(_ on: Bool) { diskImage.hoverChanged(on) }

    func openAccessibilitySettings() {
        if let url = URL(string: Self.settingsURL) { NSWorkspace.shared.open(url) }
    }

    // MARK: - Reconciling

    private var wantsEvents: Bool {
        preferences.convFinderCutPaste || preferences.convWindowMaximizer || preferences.convDockClickMinimize
    }

    private var wantsAnything: Bool { wantsEvents || preferences.convFnCommand || preferences.convAutoQuit }

    private func reconcile() {
        defer { publishIfChanged() }
        // Needs no Accessibility permission, so it runs before that check.
        diskImage.sync()
        guard wantsAnything else {
            tearDown()
            setRecheck(false)
            return
        }
        // Checking is silent. Asking is the person's move, in System Settings.
        guard AXIsProcessTrusted() else {
            tearDown()
            setRecheck(true)
            return
        }
        setRecheck(false)

        syncFn()

        if wantsEvents {
            if !hub.isRunning && !hub.start() {
                setRecheck(true)
                return
            }
            sync("finder", preferences.convFinderCutPaste, priority: 30,
                 handler: { [finder] in finder.handle($0, $1) }, off: { [finder] in finder.reset() })
            sync("maximizer", preferences.convWindowMaximizer, priority: 20,
                 handler: { [maximizer] in maximizer.handle($0, $1) }, off: { [maximizer] in maximizer.reset() })
            sync("dock", preferences.convDockClickMinimize, priority: 10,
                 handler: { [dock] in dock.handle($0, $1) }, off: { [dock] in dock.reset() })
        } else if hub.isRunning {
            unregisterAll()
            hub.stop()
        }

        if preferences.convAutoQuit {
            autoQuit.start(bundleIDs: preferences.convAutoQuitApps)
        } else if autoQuit.isRunning {
            autoQuit.stop()
        }
    }

    /// Fn as Command runs on its own tap at the HID level, ahead of the
    /// system's hotkey handling. All or nothing: when the tap cannot be
    /// created nothing is applied and the status says so.
    private func syncFn() {
        guard preferences.convFnCommand else {
            stopFn()
            return
        }
        if !fnHub.isRunning {
            guard fnHub.start() else {
                fnFailure = "could not create the HID event tap"
                setRecheck(true)
                return
            }
        }
        fnFailure = nil
        if fnToken == nil {
            fnToken = fnHub.register(priority: 0, handler: { [fn] in fn.handle($0, $1) })
        }
    }

    private func stopFn() {
        if let token = fnToken { fnHub.unregister(token) }
        fnToken = nil
        fnHub.stop()
        fn.reset()
        fnFailure = nil
    }

    private func sync(_ name: String, _ on: Bool, priority: Int,
                      handler: @escaping TapHandler, off: () -> Void) {
        if on, tokens[name] == nil {
            tokens[name] = hub.register(priority: priority, handler: handler)
        } else if !on, let token = tokens.removeValue(forKey: name) {
            hub.unregister(token)
            off()
        }
    }

    private func unregisterAll() {
        for token in tokens.values { hub.unregister(token) }
        tokens.removeAll()
        finder.reset()
        maximizer.reset()
        dock.reset()
    }

    private func tearDown() {
        unregisterAll()
        stopFn()
        hub.stop()
        if autoQuit.isRunning { autoQuit.stop() }
    }

    private func setRecheck(_ on: Bool) {
        if on, recheck == nil {
            recheck = Timer.scheduledTimer(withTimeInterval: 3, repeats: true) { [weak self] _ in
                MainActor.assumeIsolated { self?.reconcile() }
            }
        } else if !on {
            recheck?.invalidate()
            recheck = nil
        }
    }

    // MARK: - What the hub shows

    private func publishIfChanged() {
        let signature = "\(fnStatus.state)|\(AXIsProcessTrusted())|\(hub.isRunning)|\(fnHub.isRunning)|\(autoQuit.isRunning)|\(CGPreflightListenEventAccess())"
        guard signature != published else { return }
        published = signature
        onChange?()
    }

    /// state: off, running, needsAccessibility or failed.
    private var fnStatus: (state: String, detail: String) {
        guard preferences.convFnCommand else { return ("off", "") }
        guard AXIsProcessTrusted() else { return ("needsAccessibility", "") }
        if let fnFailure { return ("failed", fnFailure) }
        return fnHub.isRunning ? ("running", "") : ("failed", "the key tap is not running")
    }

    func stateSnapshot() -> [String: Any] {
        let running = NSWorkspace.shared.runningApplications
            .filter { $0.activationPolicy == .regular && $0.bundleIdentifier != nil }
            .compactMap { app -> [String: String]? in
                guard let id = app.bundleIdentifier, id != Bundle.main.bundleIdentifier,
                      id != "com.apple.finder" else { return nil }
                return ["id": id, "name": app.localizedName ?? id]
            }
            .sorted { ($0["name"] ?? "").localizedCaseInsensitiveCompare($1["name"] ?? "") == .orderedAscending }
        let listed = preferences.convAutoQuitApps.map { id -> [String: String] in
            ["id": id, "name": Self.displayName(for: id)]
        }
        return [
            "accessibility": AXIsProcessTrusted(),
            "inputMonitoring": CGPreflightListenEventAccess(),
            "wanted": wantsAnything,
            "fnStatus": fnStatus.state,
            "fnDetail": fnStatus.detail,
            "active": hub.isRunning || autoQuit.isRunning,
            "runningApps": running,
            "autoQuitApps": listed,
            "cutPasteResults": [] as [[String: Any]],
            "windowManagement": windowHotKeys.snapshot(),
        ]
    }

    private static func displayName(for bundleID: String) -> String {
        if let app = NSRunningApplication.runningApplications(withBundleIdentifier: bundleID).first,
           let name = app.localizedName { return name }
        if let url = NSWorkspace.shared.urlForApplication(withBundleIdentifier: bundleID) {
            return FileManager.default.displayName(atPath: url.path)
                .replacingOccurrences(of: ".app", with: "")
        }
        return bundleID
    }
}
