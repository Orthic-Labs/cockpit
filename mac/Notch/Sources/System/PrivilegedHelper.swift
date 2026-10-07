import Foundation
import ServiceManagement

/// Cockpit fork: registration of the privileged helper (a launch daemon inside
/// the app, `Contents/Library/LaunchDaemons/dev.orthic.cockpit.helper.plist`).
/// The user approves it once in System Settings > Login Items. See docs/helper.md.
enum PrivilegedHelper {
    static let plistName = "dev.orthic.cockpit.helper.plist"

    private static var service: SMAppService { SMAppService.daemon(plistName: plistName) }

    /// notRegistered | requiresApproval | enabled | notFound
    static var state: String {
        switch service.status {
        case .notRegistered: return "notRegistered"
        case .requiresApproval: return "requiresApproval"
        case .enabled: return "enabled"
        case .notFound: return "notFound"
        @unknown default: return "notFound"
        }
    }

    /// Registers the daemon. Returns an error message only when it did not end
    /// up enabled or waiting for approval.
    static func enable() -> String? {
        do {
            try service.register()
            return nil
        } catch {
            let now = state
            if now == "enabled" || now == "requiresApproval" { return nil }
            return error.localizedDescription
        }
    }

    static func disable() -> String? {
        do {
            try service.unregister()
            return nil
        } catch {
            return state == "notRegistered" ? nil : error.localizedDescription
        }
    }

    static func openLoginItems() { SMAppService.openSystemSettingsLoginItems() }
}
