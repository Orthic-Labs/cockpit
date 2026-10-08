import AppKit
import Darwin

/// Runs before preferences, bridge state or usage archives can be written.
enum ProductMigration {
    static let domain = "dev.orthic.pulse"
    private static let legacyDomain = "dev.orthic.cockpit"

    static func run() throws {
        let defaults = UserDefaults.standard
        let old = defaults.persistentDomain(forName: legacyDomain) ?? [:]
        let support = FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent("Library/Application Support", isDirectory: true)
        let legacy = support.appendingPathComponent("Cockpit", isDirectory: true)
        let hadLegacyState = FileManager.default.fileExists(atPath: legacy.path) || !old.isEmpty

        try moveDirectory(legacy, to: support.appendingPathComponent("Pulse", isDirectory: true))
        // Preserve the hub's native data & WKWebView storage before launching it.
        for parent in ["Library/Application Support", "Library/Caches", "Library/WebKit"] {
            let root = FileManager.default.homeDirectoryForCurrentUser.appendingPathComponent(parent)
            try moveDirectory(root.appendingPathComponent("dev.orthic.cockpit.hub"),
                              to: root.appendingPathComponent("dev.orthic.pulse.hub"))
        }
        copyDomain(legacyDomain, to: domain)
        copyDomain("dev.orthic.cockpit.hub", to: "dev.orthic.pulse.hub")
        if (defaults.persistentDomain(forName: domain) ?? [:]).isEmpty {
            Preferences.migrateFromPreviousName()
        }
        // Keep every original key; also translate the renamed login-item sentinel.
        if defaults.object(forKey: "pulseLoginItemDefaulted") == nil,
           let value = defaults.object(forKey: "cockpitLoginItemDefaulted") {
            defaults.set(value, forKey: "pulseLoginItemDefaulted")
        }
        if hadLegacyState && !defaults.bool(forKey: "pulseHelperRenameHandled") {
            defaults.set(true, forKey: "pulseHelperReenableRequired")
            defaults.set(true, forKey: "pulseHelperRenameHandled")
        }
        PrivilegedHelper.retireLegacyRegistration()
    }

    private static func copyDomain(_ source: String, to destination: String) {
        let defaults = UserDefaults.standard
        guard (defaults.persistentDomain(forName: destination) ?? [:]).isEmpty,
              let old = defaults.persistentDomain(forName: source), !old.isEmpty else { return }
        defaults.setPersistentDomain(old, forName: destination)
    }

    private static func moveDirectory(_ source: URL, to destination: URL) throws {
        let manager = FileManager.default
        // resourceValues would follow a symlink; inspect the path itself instead.
        if (try? manager.attributesOfItem(atPath: destination.path)) != nil { return }
        guard manager.fileExists(atPath: source.path) else { return }
        let attributes = try manager.attributesOfItem(atPath: source.path)
        guard attributes[.type] as? FileAttributeType == .typeDirectory else {
            throw NSError(domain: NSPOSIXErrorDomain, code: Int(EINVAL))
        }
        let result = source.path.withCString { old in
            destination.path.withCString { new in renamex_np(old, new, UInt32(RENAME_EXCL)) }
        }
        if result == 0 { return }
        let error = errno
        if error == EEXIST || (error == ENOENT && manager.fileExists(atPath: destination.path)) { return }
        throw NSError(domain: NSPOSIXErrorDomain, code: Int(error))
    }
}
