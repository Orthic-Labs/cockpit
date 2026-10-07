import Foundation

/// What the helper will and will not move. Everything not allowed here is
/// refused; the list is deliberately short.
enum TrashPolicy {
    /// Allowed below /Library, one level deep at least.
    static let libraryFolders: Set<String> = [
        "Application Support", "Caches", "Preferences", "LaunchAgents", "LaunchDaemons",
        "PrivilegedHelperTools", "Logs", "Internet Plug-Ins", "PreferencePanes", "Audio",
    ]

    /// Refused outright, whatever else matches (compared case-insensitively).
    static let denied = [
        "/system", "/usr", "/bin", "/sbin", "/private", "/library/apple", "/applications/utilities",
    ]

    /// nil when `path` may be moved; otherwise the reason it may not.
    static func refusal(for path: String) -> String? {
        guard path.hasPrefix("/"), !path.contains("\0") else { return "Not an absolute path." }
        let parts = path.split(separator: "/", omittingEmptySubsequences: false).dropFirst().map(String.init)
        if parts.isEmpty || parts.contains(where: { $0.isEmpty || $0 == "." || $0 == ".." }) {
            return "Path has empty, . or .. components."
        }
        let lower = path.lowercased()
        for prefix in denied where lower == prefix || lower.hasPrefix(prefix + "/") {
            return "System location."
        }
        if parts[0] == "Applications" {
            guard parts.count >= 2 else { return "Not an item inside /Applications." }
        } else if parts[0] == "Library" {
            guard parts.count >= 3, libraryFolders.contains(parts[1]) else {
                return "Outside the locations Cockpit may move."
            }
            let name = parts[2].lowercased()
            if name == "apple" || name.hasPrefix("com.apple.") { return "Apple item." }
        } else {
            return "Outside the locations Cockpit may move."
        }
        // Real path must be the path itself: no symlinked parent, no symlink leaf.
        var resolved = [CChar](repeating: 0, count: Int(PATH_MAX))
        guard realpath(path, &resolved) != nil else { return "Does not exist." }
        guard String(cString: resolved) == path else { return "Symbolic link or non-canonical path." }
        var info = stat()
        guard lstat(path, &info) == 0 else { return "Does not exist." }
        if (info.st_mode & 0o170000) == 0o120000 { return "Symbolic link." }
        if parts[0] == "Applications", let reason = appleProtection(top: "/Applications/" + parts[1]) {
            return reason
        }
        if let own = Bundle.main.executablePath,
           let real = realpath(own, nil) {
            defer { free(real) }
            if String(cString: real).hasPrefix(path + "/") { return "Contains the helper itself." }
        }
        return nil
    }

    /// An Apple app (`com.apple.` id) without an App Store receipt is protected.
    private static func appleProtection(top: String) -> String? {
        var bundles = [top]
        if !top.hasSuffix(".app") {
            // A vendor folder: look one level down for apps.
            let kids = (try? FileManager.default.contentsOfDirectory(atPath: top)) ?? []
            bundles = kids.filter { $0.hasSuffix(".app") }.map { top + "/" + $0 }
        }
        for bundle in bundles {
            let plist = bundle + "/Contents/Info.plist"
            guard let data = FileManager.default.contents(atPath: plist),
                  let info = try? PropertyListSerialization.propertyList(from: data, format: nil) as? [String: Any],
                  let id = info["CFBundleIdentifier"] as? String else { continue }
            if id.lowercased().hasPrefix("com.apple."),
               !FileManager.default.fileExists(atPath: bundle + "/Contents/_MASReceipt/receipt") {
                return "Apple app without an App Store receipt."
            }
        }
        return nil
    }
}
