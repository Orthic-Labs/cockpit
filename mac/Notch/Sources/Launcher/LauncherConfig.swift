import Foundation

/// Where the launcher keeps its files, under ~/Library/Application Support/Pulse.
enum LauncherStorage {
    static var pulseDirectory: URL {
        FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
            .appendingPathComponent("Pulse", isDirectory: true)
    }

    static var launcherDirectory: URL {
        pulseDirectory.appendingPathComponent("launcher", isDirectory: true)
    }

    static var clipboardDirectory: URL {
        pulseDirectory.appendingPathComponent("clipboard", isDirectory: true)
    }

    static func ensure(_ directory: URL) {
        try? FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
    }
}

/// An app with its own global hotkey. Pressing it shows the app, hides it when it is in front.
struct LauncherAppHotkey: Codable, Equatable {
    var path: String
    /// Such as "opt+1" or "cmd+shift+k".
    var hotkey: String
}

/// A URL, search or deeplink with placeholders: {query} (the text after the keyword),
/// {clipboard} and {date}.
struct LauncherQuicklink: Codable, Equatable {
    var name: String
    var keyword: String
    var template: String
}

/// Markdown text pasted into the previous app. Placeholders: {date}, {clipboard}, {argument}.
struct LauncherSnippet: Codable, Equatable {
    var name: String
    var keyword: String
    var body: String
}

/// A named shell command, run with /bin/zsh -lc. Its output is shown in the launcher.
struct LauncherCommand: Codable, Equatable {
    var name: String
    var keyword: String
    var command: String
    /// Optional global hotkey, such as "ctrl+opt+d".
    var hotkey: String
}

/// The launcher's user-made items. The hub writes this as one JSON string
/// through the `launcherConfig` key; the notch decodes it leniently.
struct LauncherConfig: Codable, Equatable {
    var pinnedApps: [String]
    var fileFolders: [String]
    var appHotkeys: [LauncherAppHotkey]
    var quicklinks: [LauncherQuicklink]
    var snippets: [LauncherSnippet]
    var commands: [LauncherCommand]

    init() {
        pinnedApps = []
        fileFolders = []
        appHotkeys = []
        quicklinks = []
        snippets = []
        commands = []
    }

    private enum CodingKeys: String, CodingKey {
        case pinnedApps, fileFolders, appHotkeys, quicklinks, snippets, commands
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        pinnedApps = try container.decodeIfPresent([String].self, forKey: .pinnedApps) ?? []
        fileFolders = try container.decodeIfPresent([String].self, forKey: .fileFolders) ?? []
        appHotkeys = try container.decodeIfPresent([LauncherAppHotkey].self, forKey: .appHotkeys) ?? []
        quicklinks = try container.decodeIfPresent([LauncherQuicklink].self, forKey: .quicklinks) ?? []
        snippets = try container.decodeIfPresent([LauncherSnippet].self, forKey: .snippets) ?? []
        commands = try container.decodeIfPresent([LauncherCommand].self, forKey: .commands) ?? []
    }

    /// An empty config when the text is missing or malformed.
    static func decode(_ json: String) -> LauncherConfig {
        guard !json.isEmpty, let data = json.data(using: .utf8),
              let config = try? JSONDecoder().decode(LauncherConfig.self, from: data)
        else { return LauncherConfig() }
        return config
    }

    func encoded() -> String {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.sortedKeys]
        guard let data = try? encoder.encode(self) else { return "" }
        return String(decoding: data, as: UTF8.self)
    }
}

/// How often and how recently each item was used, for ranking. Kept in a small
/// JSON file and saved a moment after the last change.
@MainActor
final class LauncherUsage {
    private struct Record: Codable {
        var count: Double
        var last: Date
    }

    private var records: [String: Record] = [:]
    private var loaded = false
    private var pendingSave: DispatchWorkItem?

    private static var fileURL: URL {
        LauncherStorage.launcherDirectory.appendingPathComponent("usage.json")
    }

    /// Points added to a fuzzy score: frequent and recent use rank higher. At most 80.
    func bonus(for key: String) -> Int {
        loadIfNeeded()
        guard let record = records[key] else { return 0 }
        return Int(min(decayed(record, at: Date()), 20) * 4)
    }

    /// Keys with the prefix, most used first, without the prefix.
    func top(prefix: String, limit: Int) -> [String] {
        loadIfNeeded()
        let now = Date()
        return records
            .filter { $0.key.hasPrefix(prefix) }
            .sorted { decayed($0.value, at: now) > decayed($1.value, at: now) }
            .prefix(limit)
            .map { String($0.key.dropFirst(prefix.count)) }
    }

    func record(_ key: String) {
        loadIfNeeded()
        var record = records[key] ?? Record(count: 0, last: Date())
        record.count += 1
        record.last = Date()
        records[key] = record
        scheduleSave()
    }

    private func decayed(_ record: Record, at now: Date) -> Double {
        let days = max(0, now.timeIntervalSince(record.last)) / 86_400
        return record.count / (1 + days / 14)
    }

    private func loadIfNeeded() {
        guard !loaded else { return }
        loaded = true
        guard let data = try? Data(contentsOf: Self.fileURL),
              let saved = try? JSONDecoder().decode([String: Record].self, from: data) else { return }
        records = saved
    }

    private func scheduleSave() {
        pendingSave?.cancel()
        let work = DispatchWorkItem { [weak self] in
            MainActor.assumeIsolated { self?.save() }
        }
        pendingSave = work
        DispatchQueue.main.asyncAfter(deadline: .now() + 2, execute: work)
    }

    private func save() {
        LauncherStorage.ensure(LauncherStorage.launcherDirectory)
        guard let data = try? JSONEncoder().encode(records) else { return }
        try? data.write(to: Self.fileURL, options: .atomic)
    }
}
