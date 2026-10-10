import AppKit
import ApplicationServices
import Darwin
import ServiceManagement

/// Silent status checks belong to the notch: TCC grants are per application.
@MainActor
final class PulsePermissions {
    enum Status: String, Sendable {
        case granted, needsApproval, off, unknown
    }

    struct Entry: Equatable, Sendable {
        let id: String
        let title: String
        var why: String
        var status: Status
        let required: Bool
        /// The features that need this permission, from what is switched on now.
        var needs: [String] = []

        var snapshot: [String: Any] {
            ["id": id, "title": title, "why": why,
             "status": status.rawValue, "required": required, "needs": needs]
        }
    }

    private let preferences: Preferences
    private(set) var entries: [Entry] = []
    private(set) var errors: [String: String] = [:]
    private var refreshing = false
    private var refreshAgain = false
    private var requestingFinder = false
    var onChange: (() -> Void)?

    var missingRequired: Bool { entries.contains { $0.required && $0.status != .granted } }

    init(preferences: Preferences) { self.preferences = preferences }

    func refresh() {
        guard !refreshing else { refreshAgain = true; return }
        let helper = PrivilegedHelper.state
        // Each permission lists the features that need it, from what is on now.
        let sendOn = preferences.nearbyEnabled && preferences.isConnected(SystemProviders.sendID)
        let toolsOn = preferences.isConnected(SystemProviders.toolsID)
        var accessibilityNeeds: [String] = []
        if sendOn { accessibilityNeeds.append("⌘V sends the clipboard from the Send card") }
        if preferences.convWindowMaximizer { accessibilityNeeds.append("Window shortcuts") }
        if preferences.convFnCommand { accessibilityNeeds.append("Fn key command") }
        if preferences.convFinderCutPaste { accessibilityNeeds.append("Finder cut & paste") }
        if preferences.convDockClickMinimize { accessibilityNeeds.append("Dock click minimizes") }
        if preferences.convAutoQuit { accessibilityNeeds.append("Auto Quit") }
        if preferences.toolWheelEnabled { accessibilityNeeds.append("Middle-click tool wheel") }
        // The launcher's hotkey needs no permission; pasting into the front app does.
        if preferences.launcherEnabled { accessibilityNeeds.append("Launcher: pasting a snippet or clipboard item into the front app") }
        let accessibilityRequired = !accessibilityNeeds.isEmpty
        // Cut and paste uses Finder's own Copy and Move Item Here keys, so no
        // Apple Events (Automation of Finder) are needed any more.
        let finderRequired = false
        let screenNeeds = toolsOn ? ["Snip, Screen and Window screenshots in Tools"] : []
        let screenGranted = CGPreflightScreenCaptureAccess()
        let networkNeeds = preferences.nearbyEnabled ? ["Nearby sharing: finding devices and sending"] : []
        let networkState = NearbySharing.shared.localNetworkState
        let loginNeeds = preferences.launchAtLogin ? ["Pulse starts when you sign in"] : []
        var rows = [
            Entry(id: "helper", title: "Background helper",
                  why: "Lets Pulse move apps owned by root to the Trash without asking for an administrator password each time.",
                  status: helper == "enabled" ? .granted
                    : helper == "requiresApproval" || helper == "needsReenable" ? .needsApproval
                    : helper == "notRegistered" ? .off : .unknown,
                  required: helper == "needsReenable" || helper == "requiresApproval" || helper == "enabled",
                  needs: ["Uninstalling root-owned apps without a password"]),
            Entry(id: "accessibility", title: "Accessibility",
                  why: "Lets Pulse watch the keyboard, mouse and Dock, and press keys and move windows for you, in the features listed below.",
                  status: AXIsProcessTrusted() ? .granted : accessibilityRequired ? .needsApproval : .off,
                  required: accessibilityRequired, needs: accessibilityNeeds),
            Entry(id: "screenRecording", title: "Screen Recording",
                  why: "Lets the Snip, Screen and Window tools capture what is on your screen. Pulse keeps nothing it captures unless you save it.",
                  status: screenGranted ? .granted : screenNeeds.isEmpty ? .off : .needsApproval,
                  // Never "required": macOS asks the first time a screenshot tool is used, and a
                  // tool nobody has pressed must not raise the missing-permission prompt.
                  required: false, needs: screenNeeds),
            Entry(id: "fullDiskAccess", title: "Full Disk Access",
                  why: "Lets the Storage scan open folders macOS protects, which would otherwise show as empty or unreadable.",
                  status: entries.first { $0.id == "fullDiskAccess" }?.status ?? .unknown,
                  required: false,
                  needs: ["Storage scans of protected folders (Mail, Messages, Safari, Photos)"]),
            Entry(id: "finderMenu", title: "Finder menu",
                  why: "Adds Cut, Copy Path and Open in Terminal to Finder's right-click menu. Optional.",
                  status: entries.first { $0.id == "finderMenu" }?.status ?? .unknown,
                  required: false,
                  needs: preferences.convFinderCutPaste ? ["Cut, Copy Path, Open in Terminal in Finder's menu"] : []),
            Entry(id: "localNetwork", title: "Local network",
                  why: "Lets Pulse find computers on your network and send files to them. macOS asks the first time a device connects.",
                  status: networkNeeds.isEmpty ? .off
                    : networkState == "granted" ? .granted
                    : networkState == "blocked" ? .needsApproval : .unknown,
                  required: !networkNeeds.isEmpty, needs: networkNeeds),
        ]
        if finderRequired {
            rows.append(Entry(id: "automation", title: "Automation of Finder",
                              why: "Reads Finder selections & destination folders for Cut & Paste.",
                              status: entries.first { $0.id == "automation" }?.status ?? .unknown,
                              required: true, needs: ["Finder cut & paste"]))
        }
        rows.append(Entry(id: "login", title: "Launch at login",
                          why: "Starts Pulse automatically each time you sign in to your Mac.",
                          status: Self.serviceStatus(SMAppService.mainApp.status), required: !loginNeeds.isEmpty,
                          needs: loginNeeds))
        update(rows)
        refreshing = true
        // File access & Apple Events can block. Neither silent probe runs on
        // the UI thread, and probing never launches Finder or requests consent.
        let finderRunning = !NSRunningApplication.runningApplications(withBundleIdentifier: "com.apple.finder").isEmpty
        Task { [weak self] in
            let statuses = await Task.detached(priority: .utility) {
                (PermissionProbe.fullDiskAccess(),
                 finderRequired && finderRunning ? PermissionProbe.finder(ask: false) : .unknown,
                 PermissionProbe.finderMenu())
            }.value
            guard let self else { return }
            var updated = rows
            for index in updated.indices {
                if updated[index].id == "fullDiskAccess" { updated[index].status = statuses.0 }
                if updated[index].id == "automation" { updated[index].status = statuses.1 }
                if updated[index].id == "finderMenu" {
                    updated[index].status = statuses.2
                    if statuses.2 == .unknown { updated[index].why = PermissionProbe.finderMenuUnregistered }
                }
            }
            self.update(updated)
            self.refreshing = false
            if self.refreshAgain {
                self.refreshAgain = false
                self.refresh()
            }
        }
    }

    func request(_ id: String) {
        guard entries.contains(where: { $0.id == id }) else { return }
        errors.removeValue(forKey: id)
        switch id {
        case "helper":
            errors[id] = PrivilegedHelper.enable()
            PrivilegedHelper.openLoginItems()
        case "accessibility":
            let options = [kAXTrustedCheckOptionPrompt.takeUnretainedValue() as String: true] as CFDictionary
            _ = AXIsProcessTrustedWithOptions(options)
            Self.openPrivacyPane("Privacy_Accessibility")
        case "screenRecording":
            _ = CGRequestScreenCaptureAccess()
            Self.openPrivacyPane("Privacy_ScreenCapture")
        case "localNetwork":
            Self.openPrivacyPane("Privacy_LocalNetwork")
        case "fullDiskAccess":
            Self.openPrivacyPane("Privacy_AllFiles")
            Self.revealHubApp()
        case "automation":
            guard preferences.convFinderCutPaste, !requestingFinder else { return }
            requestingFinder = true
            // The permission API needs a running target. Launch only in
            // response to Allow, never while reading status.
            if NSRunningApplication.runningApplications(withBundleIdentifier: "com.apple.finder").isEmpty,
               let url = NSWorkspace.shared.urlForApplication(withBundleIdentifier: "com.apple.finder") {
                let configuration = NSWorkspace.OpenConfiguration()
                configuration.activates = false
                NSWorkspace.shared.openApplication(at: url, configuration: configuration) { [weak self] _, error in
                    Task { @MainActor in
                        guard let self else { return }
                        if let error {
                            self.requestingFinder = false
                            self.errors[id] = error.localizedDescription
                            self.onChange?()
                        } else { self.requestFinder() }
                    }
                }
            } else { requestFinder() }
        case "finderMenu":
            // Turn the extension on directly; the Settings pane that lists it is
            // hard to find, so it only opens when that did not work.
            Task { [weak self] in
                let status = await Task.detached(priority: .userInitiated) {
                    PermissionProbe.enableFinderMenu()
                    return PermissionProbe.finderMenu()
                }.value
                if status != .granted { Self.openExtensionsPane() }
                self?.refresh()
            }
        case "login":
            do { try SMAppService.mainApp.register() }
            catch {
                if SMAppService.mainApp.status != .enabled && SMAppService.mainApp.status != .requiresApproval {
                    errors[id] = error.localizedDescription
                }
            }
            preferences.launchAtLogin = SMAppService.mainApp.status == .enabled
            if SMAppService.mainApp.status == .requiresApproval { PrivilegedHelper.openLoginItems() }
        default: return
        }
        refresh()
        onChange?()
    }

    private func requestFinder() {
        Task { [weak self] in
            let status = await Task.detached(priority: .userInitiated) { PermissionProbe.finder(ask: true) }.value
            guard let self else { return }
            self.requestingFinder = false
            if status != .granted { Self.openPrivacyPane("Privacy_Automation") }
            if status == .unknown { self.errors["automation"] = "Could not determine Finder consent. Check Automation in System Settings." }
            self.refresh()
            self.onChange?()
        }
    }

    static func openPrivacyPane(_ pane: String) {
        let modern = "x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension?\(pane)"
        if let url = URL(string: modern), NSWorkspace.shared.open(url) { return }
        if let url = URL(string: "x-apple.systempreferences:com.apple.preference.security?\(pane)") {
            NSWorkspace.shared.open(url)
        }
    }

    /// Disk scans run in the hub, a separate app with its own Full Disk Access
    /// grant. Reveal it in Finder so it can be dragged into the list or added with +.
    static func revealHubApp() {
        let hub = Bundle.main.bundleURL.appendingPathComponent("Contents/Helpers/Pulse.app", isDirectory: true)
        guard FileManager.default.fileExists(atPath: hub.path) else { return }
        NSWorkspace.shared.activateFileViewerSelecting([hub])
    }

    /// Login Items & Extensions; falls back to the older Extensions pane id.
    static func openExtensionsPane() {
        for scheme in ["x-apple.systempreferences:com.apple.ExtensionsPreferences",
                       "x-apple.systempreferences:com.apple.LoginItems-Settings.extension"] {
            if let url = URL(string: scheme), NSWorkspace.shared.open(url) { return }
        }
    }

    private static func serviceStatus(_ status: SMAppService.Status) -> Status {
        switch status {
        case .enabled: return .granted
        case .requiresApproval: return .needsApproval
        case .notRegistered: return .off
        case .notFound: return .unknown
        @unknown default: return .unknown
        }
    }

    private func update(_ rows: [Entry]) {
        let previousErrors = errors
        for row in rows where row.status == .granted { errors.removeValue(forKey: row.id) }
        guard entries != rows || errors != previousErrors else { return }
        entries = rows
        onChange?()
    }
}

/// Worker-only probes. No protected file contents are kept or published.
private enum PermissionProbe {
    static func fullDiskAccess() -> PulsePermissions.Status {
        let home = FileManager.default.homeDirectoryForCurrentUser
        for path in ["Library/Safari/Bookmarks.plist", "Library/Application Support/com.apple.TCC/TCC.db"] {
            let fd = open(home.appendingPathComponent(path).path, O_RDONLY | O_CLOEXEC)
            guard fd >= 0 else {
                if errno == EACCES || errno == EPERM { return .needsApproval }
                continue
            }
            var byte: UInt8 = 0
            let count = read(fd, &byte, 1)
            let failure = errno
            close(fd)
            if count >= 0 { return .granted }
            if failure == EACCES || failure == EPERM { return .needsApproval }
        }
        return .unknown
    }

    /// `pluginkit -e use` enables the Finder Sync extension, as its switch in
    /// System Settings does.
    static func enableFinderMenu() {
        let process = Process()
        process.executableURL = URL(fileURLWithPath: "/usr/bin/pluginkit")
        process.arguments = ["-e", "use", "-i", "dev.orthic.pulse.finder"]
        process.standardOutput = FileHandle.nullDevice
        process.standardError = FileHandle.nullDevice
        do { try process.run() } catch { return }
        process.waitUntilExit()
    }

    /// `pluginkit -m -i` prints a leading "+" for an enabled extension, "-" for
    /// a disabled one, and nothing when it is not registered.
    static func finderMenu() -> PulsePermissions.Status {
        let process = Process()
        process.executableURL = URL(fileURLWithPath: "/usr/bin/pluginkit")
        process.arguments = ["-m", "-i", "dev.orthic.pulse.finder"]
        let pipe = Pipe()
        process.standardOutput = pipe
        process.standardError = FileHandle.nullDevice
        do { try process.run() } catch { return .unknown }
        let data = pipe.fileHandleForReading.readDataToEndOfFile()
        process.waitUntilExit()
        let line = String(decoding: data, as: UTF8.self)
            .split(separator: "\n").first { $0.contains("dev.orthic.pulse.finder") }
        // Not listed at all: macOS never registered the extension. That is what an
        // app that is signed but not notarized gets (every dev build), and no switch
        // here or in System Settings can turn it on, so it is not "off".
        guard let line else { return .unknown }
        return line.drop { $0 == " " }.first == "+" ? .granted : .off
    }

    /// The reason shown when the Finder menu's status is unknown: the system does not
    /// list the extension, which on a dev build means the app is not notarized.
    static let finderMenuUnregistered =
        "Not available in this build: macOS has not registered Pulse's Finder extension "
        + "(dev builds are signed but not notarized). A notarized build brings it back."

    static func finder(ask: Bool) -> PulsePermissions.Status {
        let target = NSAppleEventDescriptor(bundleIdentifier: "com.apple.finder")
        guard let address = target.aeDesc else { return .unknown }
        let result = AEDeterminePermissionToAutomateTarget(address, AEEventClass(typeWildCard), AEEventID(typeWildCard), ask)
        switch result {
        case noErr: return .granted
        case OSStatus(errAEEventNotPermitted), OSStatus(errAEEventWouldRequireUserConsent): return .needsApproval
        default: return .unknown
        }
    }
}
