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
        guard let process = child, process.isRunning else { return }
        process.terminate()
    }
}
