import Foundation
import ServiceManagement

/// Pulse fork: registration of the privileged helper (a launch daemon inside
/// the app, `Contents/Library/LaunchDaemons/dev.orthic.pulse.helper.plist`).
/// The user approves it once in System Settings > Login Items. See docs/helper.md.
enum PrivilegedHelper {
    static let plistName = "dev.orthic.pulse.helper.plist"

    private static var service: SMAppService { SMAppService.daemon(plistName: plistName) }

    /// Best-effort cleanup only: never registers the replacement automatically.
    static func retireLegacyRegistration() {
        guard !UserDefaults.standard.bool(forKey: "pulseLegacyHelperRetired") else { return }
        do {
            try SMAppService.daemon(plistName: "dev.orthic.cockpit.helper.plist").unregister()
            UserDefaults.standard.set(true, forKey: "pulseLegacyHelperRetired")
        } catch {
            Log.usage.notice("legacy helper unregister did not complete; will retry next launch")
        }
    }

    /// notRegistered | needsReenable | requiresApproval | enabled | notFound
    static var state: String {
        if UserDefaults.standard.bool(forKey: "pulseHelperReenableRequired") { return "needsReenable" }
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
            UserDefaults.standard.set(false, forKey: "pulseHelperReenableRequired")
            return nil
        } catch {
            let now = service.status
            if now == .enabled || now == .requiresApproval {
                UserDefaults.standard.set(false, forKey: "pulseHelperReenableRequired")
                return nil
            }
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
