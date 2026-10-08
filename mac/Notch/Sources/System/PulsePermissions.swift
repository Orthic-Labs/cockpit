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
        let why: String
        var status: Status
        let required: Bool

        var snapshot: [String: Any] {
            ["id": id, "title": title, "why": why,
             "status": status.rawValue, "required": required]
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
        let accessibilityRequired = preferences.convFnCommand || preferences.convFinderCutPaste
            || preferences.convWindowMaximizer || preferences.convDockClickMinimize || preferences.convAutoQuit
        let finderRequired = preferences.convFinderCutPaste
        var rows = [
            Entry(id: "helper", title: "Background helper",
                  why: "Moves root-owned apps to Trash without an administrator password.",
                  status: helper == "enabled" ? .granted
                    : helper == "requiresApproval" || helper == "needsReenable" ? .needsApproval
                    : helper == "notRegistered" ? .off : .unknown,
                  required: helper == "needsReenable" || helper == "requiresApproval" || helper == "enabled"),
            Entry(id: "accessibility", title: "Accessibility",
                  why: "Lets enabled keyboard & window conveniences respond to your input.",
                  status: AXIsProcessTrusted() ? .granted : accessibilityRequired ? .needsApproval : .off,
                  required: accessibilityRequired),
            Entry(id: "fullDiskAccess", title: "Full Disk Access",
                  why: "Lets Pulse read protected folders during disk scans.",
                  status: entries.first { $0.id == "fullDiskAccess" }?.status ?? .unknown,
                  required: false),
            Entry(id: "finderMenu", title: "Finder menu",
                  why: "Adds Copy Path and Open in Terminal to Finder's right-click menu. Optional.",
                  status: entries.first { $0.id == "finderMenu" }?.status ?? .unknown,
                  required: false),
        ]
        if finderRequired {
            rows.append(Entry(id: "automation", title: "Automation of Finder",
                              why: "Reads Finder selections & destination folders for Cut & Paste.",
                              status: entries.first { $0.id == "automation" }?.status ?? .unknown,
                              required: true))
        }
        rows.append(Entry(id: "login", title: "Launch at login",
                          why: "Starts Pulse automatically when you sign in to your Mac.",
                          status: Self.serviceStatus(SMAppService.mainApp.status), required: false))
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
                if updated[index].id == "finderMenu" { updated[index].status = statuses.2 }
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
        case "fullDiskAccess":
            Self.openPrivacyPane("Privacy_AllFiles")
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
            Self.openExtensionsPane()
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
        guard let line else { return .off }
        return line.drop { $0 == " " }.first == "+" ? .granted : .off
    }

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
