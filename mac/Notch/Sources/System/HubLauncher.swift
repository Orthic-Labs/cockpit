import AppKit

/// Pulse fork: opens the hub (Tauri, `dev.orthic.pulse.hub`) from the notch.
///
/// The hub runs as a CHILD process of the notch (`Foundation.Process` on
/// `Contents/MacOS/pulse-hub`), not through Launch Services. macOS then treats
/// the notch as the responsible process, so Pulse's one Full Disk Access
/// entry covers the hub's scans. A running hub is told which section to show
/// with a Darwin notification; a new one gets it as a launch argument.
@MainActor
enum HubLauncher {
    static let bundleID = "dev.orthic.pulse.hub"
    private static var child: Process?
    private static let processStart = Date()
    private static var reaped = false

    /// A hub that was already running when this notch started belongs to an
    /// earlier Pulse (the hub is only ever the notch's child). It would keep
    /// the old code after an update, so it is stopped once, before this notch
    /// starts or addresses a hub of its own.
    private static func reapStaleHubs() {
        guard !reaped else { return }
        reaped = true
        for app in NSRunningApplication.runningApplications(withBundleIdentifier: bundleID)
        where !app.isTerminated && (app.launchDate ?? .distantPast) < processStart
            && app.processIdentifier != child?.processIdentifier {
            app.forceTerminate()
        }
    }

    /// A hub left behind by an earlier Pulse outlives that notch, because it is
    /// a child that is never told to quit when the app is replaced. Called once
    /// at launch, before this notch starts or addresses a hub. Found by bundle
    /// id, and by its executable for a hub Launch Services does not list under
    /// one. Asked to quit, given three seconds, then forced.
    static func retireHubsFromEarlierLaunches() {
        let launched = NSRunningApplication.current.launchDate ?? Date()
        var seen = Set<pid_t>()
        let candidates = (NSRunningApplication.runningApplications(withBundleIdentifier: bundleID)
            + NSWorkspace.shared.runningApplications.filter {
                $0.executableURL?.path.hasSuffix("Helpers/Pulse.app/Contents/MacOS/pulse-hub") == true
            })
        .filter {
            !$0.isTerminated && seen.insert($0.processIdentifier).inserted
                && ($0.launchDate ?? .distantFuture) < launched
        }
        guard !candidates.isEmpty else { return }
        for app in candidates { app.terminate() }
        let deadline = Date().addingTimeInterval(3)
        while Date() < deadline, candidates.contains(where: { !$0.isTerminated }) {
            Thread.sleep(forTimeInterval: 0.05)
        }
        let remaining = candidates.filter { !$0.isTerminated }
        for app in remaining { app.forceTerminate() }
        Log.usage.info("retired \(candidates.count, privacy: .public) hub(s) from an earlier launch, \(remaining.count, privacy: .public) forced")
    }

    static var location: URL? {
        let embedded = Bundle.main.bundleURL
            .appendingPathComponent("Contents/Helpers/Pulse.app", isDirectory: true)
        if FileManager.default.fileExists(atPath: embedded.path) { return embedded }
        return NSWorkspace.shared.urlForApplication(withBundleIdentifier: bundleID)
    }

    private static var hubRunning: Bool {
        if child?.isRunning == true { return true }
        return !NSRunningApplication.runningApplications(withBundleIdentifier: bundleID).isEmpty
    }

    /// Returns false when no hub is installed, so the caller can fall back.
    @discardableResult
    static func open(section: String) -> Bool {
        reapStaleHubs()
        if hubRunning {
            DarwinNotify.post("dev.orthic.pulse.hub.show.\(section)")
            return true
        }
        guard let url = location else { return false }
        let process = Process()
        process.executableURL = url.appendingPathComponent("Contents/MacOS/pulse-hub")
        process.arguments = ["--section", section]
        process.environment = ProcessInfo.processInfo.environment
        process.standardInput = FileHandle.nullDevice
        process.standardOutput = FileHandle.nullDevice
        process.standardError = FileHandle.nullDevice
        process.terminationHandler = { finished in
            Task { @MainActor in if child === finished { child = nil } }
        }
        do {
            try process.run()
            child = process
            return true
        } catch {
            // Last resort: Launch Services (the hub then carries its own TCC entry).
            let configuration = NSWorkspace.OpenConfiguration()
            configuration.arguments = ["--section", section]
            configuration.activates = true
            NSWorkspace.shared.openApplication(at: url, configuration: configuration)
            return true
        }
    }

    static var isRunning: Bool { hubRunning }

    /// Starts the hub with no window and no Dock icon, so nearby sharing works
    /// while nobody has the hub open. Does nothing when it is already running.
    static func launchInBackground() {
        reapStaleHubs()
        guard !hubRunning, let url = location else { return }
        let process = Process()
        process.executableURL = url.appendingPathComponent("Contents/MacOS/pulse-hub")
        process.arguments = ["--background"]
        process.environment = ProcessInfo.processInfo.environment
        process.standardInput = FileHandle.nullDevice
        process.standardOutput = FileHandle.nullDevice
        process.standardError = FileHandle.nullDevice
        process.terminationHandler = { finished in
            Task { @MainActor in if child === finished { child = nil } }
        }
        do {
            try process.run()
            child = process
        } catch {
            let configuration = NSWorkspace.OpenConfiguration()
            configuration.arguments = ["--background"]
            configuration.activates = false
            NSWorkspace.shared.openApplication(at: url, configuration: configuration)
        }
    }

    /// Asks the hub child to quit (SIGTERM). Called when the notch quits.
    static func terminate() {
        if let process = child, process.isRunning { process.terminate() }
        // A hub started another way (Launch Services fallback) must not outlive
        // the notch either.
        for app in NSRunningApplication.runningApplications(withBundleIdentifier: bundleID)
        where !app.isTerminated && app.processIdentifier != child?.processIdentifier {
            app.terminate()
        }
    }
}
