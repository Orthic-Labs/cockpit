import AppKit
import Foundation

/// **Pulse's own updater.** It asks GitHub for the latest release, offers a
/// newer one in the notch, and installs it on Update. The mechanics live in
/// `Sources/Updater/`: the feed, the download, the signature checks and the
/// replacement of /Applications/Pulse.app.
///
/// Automatic checks run at most every six hours, and only while "Automatically
/// check" is on. "Check now" always asks. Nothing is downloaded or installed
/// until the user chooses Update.
@MainActor
final class Updater: ObservableObject {
    enum Status: Equatable {
        case idle
        case checking
        case upToDate
        case available(String)
        case downloading(Double?)
        case installing
        case failed(String)
    }

    @Published private(set) var status: Status = .idle
    /// The update the notch is showing, if any.
    @Published private(set) var prompt: UpdatePrompt?
    /// A newer version that is waiting, offered or not. Drives the settings dot.
    @Published private(set) var pending: String?
    @Published private(set) var lastChecked: Date?

    static let checkInterval: TimeInterval = 6 * 60 * 60
    private static let tickInterval: TimeInterval = 30 * 60

    private enum Keys {
        static let etag = "update.etag"
        static let release = "update.release"
        static let lastAttempt = "update.lastAttempt"
        static let lastChecked = "update.lastChecked"
        static let offered = "update.offeredVersion"
    }

    private let preferences: Preferences
    private let defaults = UserDefaults.standard
    /// The latest release as last fetched, kept so an unchanged one costs a 304.
    private var release: PulseRelease?
    private var checkTask: Task<Void, Never>?
    private var installTask: Task<Void, Never>?
    private var timer: Timer?
    private var stalenessTimer: Timer?
    /// The on-disk build the "Pulse was updated" card was last dismissed for.
    private var dismissedDiskBuild: String?

    init(preferences: Preferences) {
        self.preferences = preferences
        if let data = defaults.data(forKey: Keys.release),
           let cached = try? JSONDecoder().decode(PulseRelease.self, from: data) {
            release = cached
        }
        lastChecked = defaults.object(forKey: Keys.lastChecked) as? Date
        // A pending update found on an earlier launch keeps its dot, without
        // reopening the notch card at every launch.
        if let newer = newerRelease, newer.asset != nil {
            pending = newer.version
            status = .available(newer.version)
        }
    }

    var currentVersion: String {
        Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? "0"
    }

    // MARK: - Checking

    /// Starts the schedule: one check now if one is due, then a look every half
    /// hour. Each look asks GitHub only if six hours have passed since the last try.
    func start() {
        checkIfDue()
        checkInstalledCopy()
        stalenessTimer = Timer.scheduledTimer(withTimeInterval: 60, repeats: true) { [weak self] _ in
            MainActor.assumeIsolated { self?.checkInstalledCopy() }
        }
        timer = Timer.scheduledTimer(withTimeInterval: Self.tickInterval, repeats: true) { [weak self] _ in
            MainActor.assumeIsolated { self?.checkIfDue() }
        }
    }

    /// "Check now": asks GitHub regardless of the schedule.
    func checkNow() {
        check(offering: true)
    }

    private func checkIfDue() {
        guard preferences.autoUpdateCheck, isDue else { return }
        check(offering: false)
    }

    private var isDue: Bool {
        guard let last = defaults.object(forKey: Keys.lastAttempt) as? Date else { return true }
        return Date().timeIntervalSince(last) >= Self.checkInterval
    }

    private var isBusy: Bool {
        switch status {
        case .downloading, .installing: return true
        default: return false
        }
    }

    /// `offering` is true for "Check now", which always shows a newer release in
    /// the notch. A scheduled check shows it once per version, then only the dot.
    private func check(offering: Bool) {
        guard checkTask == nil, !isBusy else { return }
        status = .checking
        defaults.set(Date(), forKey: Keys.lastAttempt)
        let etag = release == nil ? nil : defaults.string(forKey: Keys.etag)
        checkTask = Task {
            do {
                let result = try await ReleaseFeed.fetch(etag: etag)
                self.received(result, offering: offering)
            } catch {
                self.checkTask = nil
                self.status = .failed(error.localizedDescription)
            }
        }
    }

    private func received(_ result: ReleaseFeed.Fetch, offering: Bool) {
        checkTask = nil
        if case .fresh(let latest, let etag) = result {
            release = latest
            if let data = try? JSONEncoder().encode(latest) { defaults.set(data, forKey: Keys.release) }
            if let etag { defaults.set(etag, forKey: Keys.etag) } else { defaults.removeObject(forKey: Keys.etag) }
        }
        let now = Date()
        lastChecked = now
        defaults.set(now, forKey: Keys.lastChecked)
        evaluate(offering: offering)
    }

    /// The cached release, if it is newer than this copy.
    private var newerRelease: PulseRelease? {
        guard let release,
              let latest = SemanticVersion(release.version),
              let current = SemanticVersion(currentVersion),
              latest > current else { return nil }
        return release
    }

    private func evaluate(offering: Bool) {
        guard let newer = newerRelease else {
            pending = nil
            status = .upToDate
            return
        }
        guard newer.asset != nil else {
            pending = nil
            status = .failed("Pulse \(newer.version) is released without a Pulse.dmg, so there is nothing to install.")
            return
        }
        pending = newer.version
        status = .available(newer.version)
        if prompt == nil, offering || defaults.string(forKey: Keys.offered) != newer.version {
            prompt = UpdatePrompt(version: newer.version, notes: Self.summary(of: newer.notes), phase: .available)
            defaults.set(newer.version, forKey: Keys.offered)
        }
    }

    private static func summary(of notes: String) -> String {
        String(UpdatePrompt.summary(of: notes).prefix(200))
    }

    // MARK: - Installing

    /// Update: downloads the pending release, verifies it, replaces
    /// /Applications/Pulse.app and relaunches. A failure at any step stops
    /// there, leaves the installed app as it was, and is reported in the hub.
    func install() {
        guard installTask == nil, !isBusy, let newer = newerRelease, let asset = newer.asset else { return }
        let running = Bundle.main.bundleURL.standardizedFileURL.path
        guard running == UpdateInstaller.installLocation.path else {
            prompt = nil
            status = .failed("Pulse is running from \(running). Updates install over /Applications/Pulse.app, so open that copy to update.")
            return
        }
        let version = newer.version
        let current = currentVersion
        prompt = UpdatePrompt(version: version, notes: Self.summary(of: newer.notes), phase: .downloading(nil))
        status = .downloading(nil)
        installTask = Task {
            await self.runInstall(asset: asset, version: version, current: current)
        }
    }

    /// The notch's answer to the offered update.
    func respond(_ choice: UpdateChoice) {
        switch choice {
        case .install: install()
        case .later, .close:
            if prompt?.phase == .restart { dismissedDiskBuild = installedBuildKey }
            prompt = nil
        case .restart:
            restartIntoInstalledCopy()
        }
    }

    // MARK: - Files replaced underneath

    private static func buildKey(of bundle: URL) -> String? {
        let plist = bundle.appendingPathComponent("Contents/Info.plist")
        guard let info = NSDictionary(contentsOf: plist) as? [String: Any],
              let build = info["CFBundleVersion"] as? String else { return nil }
        return "\(info["CFBundleShortVersionString"] as? String ?? "")+\(build)"
    }

    private var runningBuildKey: String {
        let short = Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? ""
        let build = Bundle.main.object(forInfoDictionaryKey: "CFBundleVersion") as? String ?? ""
        return "\(short)+\(build)"
    }

    private var installedBuildKey: String? { Self.buildKey(of: UpdateInstaller.installLocation) }

    /// Cheap (one small plist read). When Pulse runs from /Applications/Pulse.app
    /// and the copy on disk is a different build (the app was dragged over a
    /// running Pulse), offers a Restart card. Never restarts by itself.
    private func checkInstalledCopy() {
        guard Bundle.main.bundleURL.standardizedFileURL.path == UpdateInstaller.installLocation.path,
              let onDisk = installedBuildKey, onDisk != runningBuildKey,
              onDisk != dismissedDiskBuild else { return }
        if let current = prompt, current.phase != .available, current.phase != .restart { return }
        if prompt?.phase == .restart { return }
        prompt = UpdatePrompt(version: onDisk.split(separator: "+").first.map(String.init) ?? currentVersion,
                              notes: "", phase: .restart)
    }

    /// Restart: the detached relauncher reopens /Applications/Pulse.app once
    /// this process has gone (the hub helper is stopped on quit).
    private func restartIntoInstalledCopy() {
        do {
            try PulseRelauncher.spawn(swapping: nil, workDirectory: nil)
            NSApp.terminate(nil)
        } catch {
            prompt = nil
            status = .failed(error.localizedDescription)
        }
    }

    private func runInstall(asset: PulseRelease.Asset, version: String, current: String) async {
        var workDirectory: URL?
        do {
            let work = try UpdateInstaller.makeWorkDirectory()
            workDirectory = work
            let image = try await UpdateDownload.fetch(asset.url, expectedSize: asset.size,
                                                       fileName: asset.name, into: work) { share in
                Task { @MainActor in self.downloadProgressed(share) }
            }

            status = .installing
            setPhase(.extracting(0.5))
            let staged = try await Task.detached(priority: .userInitiated) {
                try UpdateInstaller.stage(image: image, workDirectory: work,
                                          version: version, currentVersion: current)
            }.value

            setPhase(.installing)
            let pid = ProcessInfo.processInfo.processIdentifier
            try await Task.detached(priority: .userInitiated) {
                try UpdateInstaller.replace(with: staged, version: version)
                try UpdateInstaller.relaunch(after: pid, workDirectory: work)
            }.value

            // The helper opens the new copy once this process has gone.
            NSApp.terminate(nil)
        } catch {
            if let workDirectory { try? FileManager.default.removeItem(at: workDirectory) }
            installTask = nil
            prompt = nil
            status = .failed(error.localizedDescription)
        }
    }

    private func downloadProgressed(_ share: Double?) {
        guard case .downloading = status else { return }
        status = .downloading(share)
        setPhase(.downloading(share))
    }

    private func setPhase(_ phase: UpdatePrompt.Phase) {
        guard prompt != nil else { return }
        prompt?.phase = phase
    }

    // MARK: - The hub

    /// What the hub's General > Updates group shows, written into the notch's
    /// state file. Strings are kept out of it: the hub words each status itself.
    var hubSnapshot: [String: Any] {
        var state: [String: Any] = [
            "current": currentVersion,
            "build": Bundle.main.object(forInfoDictionaryKey: "CFBundleVersion") as? String ?? NSNull(),
            "available": pending ?? NSNull(),
            "lastChecked": lastChecked.map { ISO8601DateFormatter().string(from: $0) } ?? NSNull(),
        ]
        switch status {
        case .idle:
            state["status"] = "idle"
        case .checking:
            state["status"] = "checking"
        case .upToDate:
            state["status"] = "upToDate"
        case .available(let version):
            state["status"] = "available"
            state["available"] = version
        case .downloading(let share):
            state["status"] = "downloading"
            if let share { state["progress"] = share }
        case .installing:
            state["status"] = "installing"
        case .failed(let reason):
            state["status"] = "failed"
            state["message"] = reason
        }
        return state
    }
}

/// **An update, as the notch offers it**: which version, a line of what is in
/// it, and how far along taking it up is.
struct UpdatePrompt: Equatable {
    enum Phase: Equatable {
        case available
        case downloading(Double?)
        case extracting(Double)
        case installing
        /// The copy in /Applications is a newer build than the running one.
        case restart
    }

    var version: String
    var notes: String
    var phase: Phase

    static func summary(of html: String) -> String {
        var text = html.replacingOccurrences(of: "<[^>]+>", with: " ", options: .regularExpression)
        for (entity, character) in [("&amp;", "&"), ("&lt;", "<"), ("&gt;", ">"),
                                    ("&quot;", "\""), ("&#39;", "'"), ("&nbsp;", " ")] {
            text = text.replacingOccurrences(of: entity, with: character)
        }
        return text.replacingOccurrences(of: "\\s+", with: " ", options: .regularExpression)
            .trimmingCharacters(in: .whitespacesAndNewlines)
    }
}

/// What the notch said to the update it offered.
enum UpdateChoice: Equatable {
    case install, later, close, restart
}
