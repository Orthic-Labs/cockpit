import AppKit

/// Cockpit fork: opens the hub (Tauri, `dev.orthic.cockpit.hub`) from the notch.
///
/// Looks for the hub inside Cockpit.app first (`Contents/Helpers`), then
/// wherever Launch Services knows it. A running hub is told which section to
/// show with a Darwin notification; a new one gets it as a launch argument.
@MainActor
enum HubLauncher {
    static let bundleID = "dev.orthic.cockpit.hub"

    static var location: URL? {
        let embedded = Bundle.main.bundleURL
            .appendingPathComponent("Contents/Helpers/Cockpit Hub.app", isDirectory: true)
        if FileManager.default.fileExists(atPath: embedded.path) { return embedded }
        return NSWorkspace.shared.urlForApplication(withBundleIdentifier: bundleID)
    }

    /// Returns false when no hub is installed, so the caller can fall back.
    @discardableResult
    static func open(section: String) -> Bool {
        if !NSRunningApplication.runningApplications(withBundleIdentifier: bundleID).isEmpty {
            DarwinNotify.post("dev.orthic.cockpit.hub.show.\(section)")
            return true
        }
        guard let url = location else { return false }
        let configuration = NSWorkspace.OpenConfiguration()
        configuration.arguments = ["--section", section]
        configuration.activates = true
        NSWorkspace.shared.openApplication(at: url, configuration: configuration)
        return true
    }
}
