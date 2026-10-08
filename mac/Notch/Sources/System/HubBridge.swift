import AppKit
import Combine
import Foundation

/// Pulse fork: the notch's settings and accounts, shared with the hub.
///
/// The notch stays the only writer of its preferences. It publishes a JSON
/// snapshot (settings, their choices, displays, accounts) to
/// `~/Library/Application Support/Pulse/notch-state.json` and posts the Darwin
/// notification `dev.orthic.pulse.notch.state`. The hub asks for changes by
/// dropping JSON files into `hub-commands/` and posting
/// `dev.orthic.pulse.hub.command`; the notch applies each one and deletes it.
@MainActor
final class HubBridge {
    static let stateNotification = "dev.orthic.pulse.notch.state"
    static let commandNotification = "dev.orthic.pulse.hub.command"

    struct Actions {
        var refresh: () -> Void = {}
        var resetPosition: () -> Void = {}
        var previewResetAlert: () -> Void = {}
        var previewSessionLimitAlert: () -> Void = {}
        var previewWeeklyLimitAlert: () -> Void = {}
        var sendTestNotification: () -> Void = {}
        /// Mac conveniences: permission state, running apps, Auto Quit list.
        var conveniences: () -> [String: Any] = { [:] }
        /// Why the launcher hotkey is not working, when it is not.
        var launcherStatus: () -> String? = { nil }
        var permissionsChanged: (Bool) -> Void = { _ in }
    }

    private let preferences: Preferences
    private let store: UsageStore
    private let actions: Actions
    private let permissions: PulsePermissions
    private var cancellables = Set<AnyCancellable>()
    private var pendingWrite: DispatchWorkItem?
    private var helperError: String?
    private var helperWatch: Timer?
    private var permissionWatch: Timer?
    private var hubVisibleUntil = Date.distantPast

    static var directory: URL {
        FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
            .appendingPathComponent("Pulse", isDirectory: true)
    }

    private static var commandsDirectory: URL {
        directory.appendingPathComponent("hub-commands", isDirectory: true)
    }

    init(preferences: Preferences, store: UsageStore, actions: Actions) {
        self.preferences = preferences
        self.store = store
        self.actions = actions
        self.permissions = PulsePermissions(preferences: preferences)
    }

    func start() {
        try? FileManager.default.createDirectory(at: Self.commandsDirectory,
                                                 withIntermediateDirectories: true)
        preferences.objectWillChange
            .sink { [weak self] _ in self?.republish() }
            .store(in: &cancellables)
        // Accounts only: system rings refresh every two seconds and carry
        // nothing the hub's Settings shows.
        store.$snapshots
            .map { snapshots in
                snapshots.filter { $0.kind == .usage }.map { "\($0.id)|\($0.status)" }
            }
            .removeDuplicates()
            .sink { [weak self] _ in self?.scheduleWrite() }
            .store(in: &cancellables)
        DarwinNotify.observe(Self.commandNotification) { [weak self] in self?.drainCommands() }
        // The hub remains running when its window closes. A visibility lease
        // also expires if it crashes, so hidden hubs do not keep probing TCC.
        DarwinNotify.observe("dev.orthic.pulse.hub.visible") { [weak self] in
            guard let self else { return }
            let wasHidden = self.hubVisibleUntil < Date()
            self.hubVisibleUntil = Date().addingTimeInterval(6)
            if wasHidden { self.republish() }
        }
        DarwinNotify.observe("dev.orthic.pulse.hub.hidden") { [weak self] in
            self?.hubVisibleUntil = .distantPast
        }
        NotificationCenter.default.publisher(for: NSApplication.didBecomeActiveNotification)
            .sink { [weak self] _ in self?.republish() }
            .store(in: &cancellables)
        NSWorkspace.shared.notificationCenter.publisher(for: NSWorkspace.didActivateApplicationNotification)
            .receive(on: RunLoop.main)
            .sink { [weak self] notification in
                if let app = notification.userInfo?[NSWorkspace.applicationUserInfoKey] as? NSRunningApplication,
                   app.bundleIdentifier == HubLauncher.bundleID { self?.republish() }
            }
            .store(in: &cancellables)
        permissions.onChange = { [weak self] in
            guard let self else { return }
            self.actions.permissionsChanged(self.permissions.missingRequired)
            self.scheduleWrite()
        }
        permissions.refresh()
        permissionWatch = Timer.scheduledTimer(withTimeInterval: 2, repeats: true) { [weak self] _ in
            MainActor.assumeIsolated {
                guard let self, self.hubVisibleUntil > Date() else { return }
                self.permissions.refresh()
                self.scheduleWrite()
            }
        }
        drainCommands()
        scheduleWrite()
    }

    /// Publish again soon: something the hub shows changed outside Preferences.
    func republish() {
        scheduleWrite()
        // Preferences emits before storing its new value.
        DispatchQueue.main.async { [weak self] in self?.permissions.refresh() }
    }

    func stop() {
        permissions.onChange = nil
        permissionWatch?.invalidate()
        helperWatch?.invalidate()
        pendingWrite?.cancel()
        cancellables.removeAll()
    }

    // MARK: - State out

    private func scheduleWrite() {
        pendingWrite?.cancel()
        let work = DispatchWorkItem { [weak self] in
            MainActor.assumeIsolated { self?.writeState() }
        }
        pendingWrite = work
        // After `objectWillChange` the new value is in place on the next turn.
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.2, execute: work)
    }

    private func writeState() {
        var settings: [String: Any] = [:]
        var options: [String: [String]] = [:]
        for (key, setting) in Self.table {
            settings[key] = setting.get(preferences)
            if let choices = setting.options { options[key] = choices }
        }
        settings["displayPreference"] = {
            if case .display(let id) = preferences.displayPreference { return id }
            return "followActiveWindow"
        }()
        settings["launchAtLogin"] = permissions.entries.first { $0.id == "login" }?.status == .granted
        let displays = DisplayOption.connected.map { ["id": $0.id, "name": $0.name] }
        let accounts: [[String: Any]] = store.providerSummaries
            .filter { $0.kind == .usage }
            .map { summary in
                var row: [String: Any] = [
                    "id": summary.id,
                    "name": summary.name,
                    "connected": preferences.isConnected(summary.id),
                    "usesKeychain": summary.usesKeychain,
                    "refusedAccess": summary.wasRefusedAccess,
                    "needsRenewal": summary.needsSignInRenewal,
                    "signInExplanation": summary.signIn.explanation,
                ]
                if let account = summary.account {
                    row["summary"] = account.summary
                    row["label"] = account.label ?? NSNull()
                    row["plan"] = account.plan ?? NSNull()
                }
                if let title = summary.signIn.actionTitle { row["signInTitle"] = title }
                return row
            }
        let state: [String: Any] = [
            "schema": 1,
            "product": "Pulse",
            "version": Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") ?? "?",
            "settings": settings,
            "options": options,
            "displays": displays,
            "accounts": accounts,
            "providerOrder": preferences.providerOrder,
            "conveniences": actions.conveniences(),
            "launcherStatus": actions.launcherStatus() ?? NSNull(),
            "helper": PrivilegedHelper.state,
            "helperError": helperError ?? NSNull(),
            "permissions": permissions.entries.map(\.snapshot),
            "permissionErrors": permissions.errors,
        ]
        guard let data = try? JSONSerialization.data(withJSONObject: state, options: [.sortedKeys])
        else { return }
        let url = Self.directory.appendingPathComponent("notch-state.json")
        try? FileManager.default.createDirectory(at: Self.directory, withIntermediateDirectories: true)
        try? data.write(to: url, options: .atomic)
        DarwinNotify.post(Self.stateNotification)
    }

    // MARK: - Commands in

    private func drainCommands() {
        let files = (try? FileManager.default.contentsOfDirectory(
            at: Self.commandsDirectory, includingPropertiesForKeys: nil)) ?? []
        for file in files.filter({ $0.pathExtension == "json" })
            .sorted(by: { $0.lastPathComponent < $1.lastPathComponent }) {
            defer { try? FileManager.default.removeItem(at: file) }
            guard let data = try? Data(contentsOf: file),
                  let command = try? JSONSerialization.jsonObject(with: data) as? [String: Any]
            else { continue }
            apply(command)
        }
        republish()
    }

    private func apply(_ command: [String: Any]) {
        let provider = command["provider"] as? String
        switch command["command"] as? String {
        case "set":
            guard let key = command["key"] as? String else { return }
            if key == "displayPreference", let id = command["value"] as? String {
                preferences.displayPreference = id == "followActiveWindow" ? .followActiveWindow : .display(id)
            } else {
                Self.table[key]?.set(preferences, command["value"])
            }
        case "connect":
            if let provider, let on = command["value"] as? Bool { preferences.setConnected(on, for: provider) }
        case "order":
            if let ids = command["value"] as? [String] { preferences.setProviderOrder(ids) }
        case "signIn":
            if let provider { store.signIn(providerID: provider) }
        case "signOut":
            if let provider { store.signOut(providerID: provider) }
        case "allowAccess":
            if let provider { store.reauthorize(providerID: provider) }
        case "refresh": actions.refresh()
        case "resetPosition": actions.resetPosition()
        case "previewResetAlert": actions.previewResetAlert()
        case "previewSessionLimitAlert": actions.previewSessionLimitAlert()
        case "previewWeeklyLimitAlert": actions.previewWeeklyLimitAlert()
        case "sendTestNotification": actions.sendTestNotification()
        case "openAccessibilitySettings": permissions.request("accessibility")
        case "permissionRequest":
            if let id = command["id"] as? String {
                permissions.request(id)
                if id == "helper" {
                    helperError = permissions.errors[id]
                    watchHelper()
                }
            }
        case "helperEnable":
            helperError = PrivilegedHelper.enable()
            watchHelper()
        case "helperDisable":
            helperError = PrivilegedHelper.disable()
            watchHelper()
        case "openLoginItems": PrivilegedHelper.openLoginItems()
        default: break
        }
    }

    /// While the helper waits for approval in System Settings, nothing tells us
    /// when it is given, so look again every few seconds (for up to ten minutes).
    private func watchHelper() {
        helperWatch?.invalidate()
        helperWatch = nil
        guard PrivilegedHelper.state == "requiresApproval" else { return }
        let until = Date().addingTimeInterval(600)
        helperWatch = Timer.scheduledTimer(withTimeInterval: 3, repeats: true) { [weak self] timer in
            MainActor.assumeIsolated {
                guard let self else { timer.invalidate(); return }
                self.republish()
                if PrivilegedHelper.state != "requiresApproval" || Date() > until {
                    timer.invalidate()
                    self.helperWatch = nil
                }
            }
        }
    }

    // MARK: - The settings the hub can read and change

    private struct Setting {
        let get: (Preferences) -> Any
        let set: (Preferences, Any?) -> Void
        let options: [String]?
    }

    private static func bool(_ path: ReferenceWritableKeyPath<Preferences, Bool>) -> Setting {
        Setting(get: { $0[keyPath: path] },
                set: { prefs, value in if let v = value as? Bool { prefs[keyPath: path] = v } },
                options: nil)
    }

    private static func number(_ path: ReferenceWritableKeyPath<Preferences, Double>) -> Setting {
        Setting(get: { $0[keyPath: path] },
                set: { prefs, value in if let v = value as? NSNumber { prefs[keyPath: path] = v.doubleValue } },
                options: nil)
    }

    private static func text(_ path: ReferenceWritableKeyPath<Preferences, String>) -> Setting {
        Setting(get: { $0[keyPath: path] },
                set: { prefs, value in if let v = value as? String { prefs[keyPath: path] = v } },
                options: nil)
    }

    private static func stringList(_ path: ReferenceWritableKeyPath<Preferences, [String]>) -> Setting {
        Setting(get: { $0[keyPath: path] },
                set: { prefs, value in
                    if let v = value as? [String] {
                        var seen = Set<String>()
                        prefs[keyPath: path] = v.filter { seen.insert($0).inserted }
                    }
                },
                options: nil)
    }

    private static func choice<E: RawRepresentable & CaseIterable>(
        _ path: ReferenceWritableKeyPath<Preferences, E>
    ) -> Setting where E.RawValue == String {
        Setting(get: { $0[keyPath: path].rawValue },
                set: { prefs, value in
                    if let raw = value as? String, let v = E(rawValue: raw) { prefs[keyPath: path] = v }
                },
                options: E.allCases.map(\.rawValue))
    }

    private static let table: [String: Setting] = [
        // Appearance
        "notchEdge": choice(\.notchEdge),
        "notchScope": choice(\.notchScope),
        "notchVisibility": choice(\.notchVisibility),
        "notchSize": choice(\.notchSize),
        "usesCustomNotchScale": bool(\.usesCustomNotchScale),
        "customNotchScale": number(\.customNotchScale),
        "notchSurfaceStyle": choice(\.notchSurfaceStyle),
        "accentColor": choice(\.accentColor),
        "colorTransitionStyle": choice(\.colorTransitionStyle),
        "watchLimit": number(\.watchLimit),
        "criticalLimit": number(\.criticalLimit),
        "showsNotchReadings": bool(\.showsNotchReadings),
        "weeklyRing": choice(\.weeklyRing),
        "weeklyRingDashed": bool(\.weeklyRingDashed),
        "weeklyReading": bool(\.weeklyReading),
        "weeklyHeadline": bool(\.weeklyHeadline),
        "claudeDailyPaceRing": bool(\.claudeDailyPaceRing),
        "showUsagePace": bool(\.showUsagePace),
        "showCodexExtraLimits": bool(\.showCodexExtraLimits),
        "resetTimeFormat": choice(\.resetTimeFormat),
        "foldsForFullScreen": bool(\.foldsForFullScreen),
        "language": choice(\.language),
        // Notifications
        "notificationChannel": choice(\.notificationChannel),
        "peekDuration": choice(\.peekDuration),
        "announceSessionEnd": bool(\.announceSessionEnd),
        "sessionEndSound": bool(\.sessionEndSound),
        "sessionEndSoundName": text(\.sessionEndSoundName),
        "sessionBlockedSoundName": text(\.sessionBlockedSoundName),
        "announceUsageReset": bool(\.announceUsageReset),
        "usageResetSound": bool(\.usageResetSound),
        "usageResetSoundName": text(\.usageResetSoundName),
        "announceSessionLimitReached": bool(\.announceSessionLimitReached),
        "announceWeeklyLimitReached": bool(\.announceWeeklyLimitReached),
        "limitReachedSound": bool(\.limitReachedSound),
        "limitReachedSoundName": text(\.limitReachedSoundName),
        // General
        "launchAtLogin": bool(\.launchAtLogin),
        "asksProviderOnLook": bool(\.asksProviderOnLook),
        // Mac conveniences (all off until chosen)
        "convFinderCutPaste": bool(\.convFinderCutPaste),
        "convWindowMaximizer": bool(\.convWindowMaximizer),
        "convDockClickMinimize": bool(\.convDockClickMinimize),
        "convFnCommand": bool(\.convFnCommand),
        "convAutoQuit": bool(\.convAutoQuit),
        "convDiskImageInstaller": bool(\.convDiskImageInstaller),
        "convDiskImageTrashDownload": bool(\.convDiskImageTrashDownload),
        "convAutoQuitApps": stringList(\.convAutoQuitApps),
        "launcherEnabled": bool(\.launcherEnabled),
        "launcherHotkey": choice(\.launcherHotkey),
    ]
}

/// Darwin (system-wide, payload-free) notifications through CoreFoundation.
@MainActor
enum DarwinNotify {
    private static var handlers: [String: () -> Void] = [:]

    static func post(_ name: String) {
        CFNotificationCenterPostNotification(
            CFNotificationCenterGetDarwinNotifyCenter(),
            CFNotificationName(name as CFString), nil, nil, true)
    }

    /// One handler per name, called on the main thread.
    static func observe(_ name: String, _ handler: @escaping () -> Void) {
        handlers[name] = handler
        CFNotificationCenterAddObserver(
            CFNotificationCenterGetDarwinNotifyCenter(), nil,
            { _, _, cfName, _, _ in
                guard let raw = cfName?.rawValue else { return }
                let key = raw as String
                DispatchQueue.main.async {
                    MainActor.assumeIsolated { DarwinNotify.handlers[key]?() }
                }
            },
            name as CFString, nil, .deliverImmediately)
    }
}
