import AppKit
import Foundation

/// Cockpit: the Fn-as-Command keyboard remap, shipped as a Karabiner-Elements
/// complex modification. Cockpit never remaps keys itself; it copies the asset
/// into Karabiner's assets folder and adds or removes exactly its own rules in
/// the selected profile of `karabiner.json`. Karabiner reloads that file itself.
enum KarabinerRemap {
    static let appPath = "/Applications/Karabiner-Elements.app"
    private static let assetName = "cockpit-fn-remap.json"

    private static var configDirectory: URL {
        FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent(".config/karabiner", isDirectory: true)
    }
    private static var configURL: URL { configDirectory.appendingPathComponent("karabiner.json") }
    private static var backupURL: URL { configDirectory.appendingPathComponent("karabiner.json.cockpit-backup") }
    private static var assetURL: URL {
        configDirectory.appendingPathComponent("assets/complex_modifications/\(assetName)")
    }

    static var isInstalled: Bool { FileManager.default.fileExists(atPath: appPath) }

    static var isRunning: Bool {
        if NSWorkspace.shared.runningApplications.contains(where: {
            ($0.bundleIdentifier ?? "").hasPrefix("org.pqrs.Karabiner")
        }) { return true }
        let task = Process()
        task.executableURL = URL(fileURLWithPath: "/usr/bin/pgrep")
        task.arguments = ["-x", "karabiner_grabber"]
        task.standardOutput = FileHandle.nullDevice
        task.standardError = FileHandle.nullDevice
        guard (try? task.run()) != nil else { return false }
        task.waitUntilExit()
        return task.terminationStatus == 0
    }

    /// The bundled asset's rules.
    private static func bundledRules() -> [[String: Any]]? {
        guard let url = Bundle.main.url(forResource: "cockpit-fn-remap", withExtension: "json"),
              let data = try? Data(contentsOf: url),
              let json = try? JSONSerialization.jsonObject(with: data) as? [String: Any]
        else { return nil }
        return json["rules"] as? [[String: Any]]
    }

    private static func descriptions(_ rules: [[String: Any]]) -> Set<String> {
        Set(rules.compactMap { $0["description"] as? String })
    }

    private static func loadConfig() -> [String: Any]? {
        guard let data = try? Data(contentsOf: configURL) else { return nil }
        return try? JSONSerialization.jsonObject(with: data) as? [String: Any]
    }

    private static func selectedIndex(_ config: [String: Any]) -> Int? {
        guard let profiles = config["profiles"] as? [[String: Any]], !profiles.isEmpty else { return nil }
        return profiles.firstIndex { ($0["selected"] as? Bool) == true } ?? 0
    }

    private static func rules(in config: [String: Any]) -> [[String: Any]] {
        guard let index = selectedIndex(config),
              let profiles = config["profiles"] as? [[String: Any]],
              let complex = profiles[index]["complex_modifications"] as? [String: Any]
        else { return [] }
        return complex["rules"] as? [[String: Any]] ?? []
    }

    /// True when every one of Cockpit's rules is in the selected profile.
    static var isEnabled: Bool {
        guard let ours = bundledRules(), let config = loadConfig() else { return false }
        return descriptions(ours).isSubset(of: descriptions(rules(in: config)))
    }

    @discardableResult
    static func enable() -> Bool {
        guard let ours = bundledRules(),
              let bundled = Bundle.main.url(forResource: "cockpit-fn-remap", withExtension: "json")
        else { return false }
        let fm = FileManager.default
        try? fm.createDirectory(at: assetURL.deletingLastPathComponent(), withIntermediateDirectories: true)
        try? fm.removeItem(at: assetURL)
        try? fm.copyItem(at: bundled, to: assetURL)
        return modify { existing in
            let mine = descriptions(ours)
            let others = existing.filter { !mine.contains(($0["description"] as? String) ?? "") }
            return ours + others
        }
    }

    @discardableResult
    static func disable() -> Bool {
        guard let ours = bundledRules() else { return false }
        let mine = descriptions(ours)
        return modify { existing in
            existing.filter { !mine.contains(($0["description"] as? String) ?? "") }
        }
    }

    /// Rewrite the selected profile's rules, backing up the file first. Creates
    /// a minimal config when Karabiner has not written one yet (enable only).
    private static func modify(_ transform: ([[String: Any]]) -> [[String: Any]]) -> Bool {
        let fm = FileManager.default
        var config: [String: Any]
        if fm.fileExists(atPath: configURL.path) {
            guard let loaded = loadConfig() else { return false }  // unreadable: leave it alone
            config = loaded
            try? fm.removeItem(at: backupURL)
            try? fm.copyItem(at: configURL, to: backupURL)
        } else {
            try? fm.createDirectory(at: configDirectory, withIntermediateDirectories: true)
            config = ["profiles": [["name": "Default profile", "selected": true]]]
        }
        var profiles = config["profiles"] as? [[String: Any]] ?? []
        if profiles.isEmpty { profiles = [["name": "Default profile", "selected": true]] }
        let index = selectedIndex(["profiles": profiles]) ?? 0
        var profile = profiles[index]
        var complex = profile["complex_modifications"] as? [String: Any] ?? [:]
        complex["rules"] = transform(complex["rules"] as? [[String: Any]] ?? [])
        profile["complex_modifications"] = complex
        profiles[index] = profile
        config["profiles"] = profiles
        guard let data = try? JSONSerialization.data(
            withJSONObject: config, options: [.prettyPrinted, .sortedKeys])
        else { return false }
        do {
            try data.write(to: configURL, options: .atomic)
            return true
        } catch {
            return false
        }
    }

    static func openApp() {
        NSWorkspace.shared.open(URL(fileURLWithPath: appPath))
    }

    static func status() -> [String: Any] {
        ["installed": isInstalled, "running": isRunning, "enabled": isEnabled]
    }
}
