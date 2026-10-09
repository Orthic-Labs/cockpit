import Combine
import Foundation

/// Pulse fork: the last usage reading of every Claude account this Mac has
/// seen, kept by the account's identity (`oauthAccount.accountUuid` in
/// `~/.claude.json`, the id the usage reader itself follows).
///
/// Only the signed-in account can be read, so the others are shown from here:
/// their last percentages and the time each window resets. Nothing is invented
/// for them. Once a window's reset time has passed the hub shows "Reset" and no
/// percentage, because the new value is not known; see `published`.
///
/// Every account folder Claude Desktop has on this Mac (an account id directory
/// under `claude-code-sessions` or `local-agent-mode-sessions`) is listed too,
/// signed in or not, so it can be named before its first reading. Such an
/// account has an entry only once it is renamed, with no windows.
///
/// Holds no secrets: ids, the sign-in address Claude Code already wrote in its
/// own config, a plan name and the window numbers. The notch is the only
/// writer; the hub renames and forgets through commands (`HubBridge`).
/// Stored at `~/Library/Application Support/Pulse/claude-account-usage.json`.
@MainActor
final class ClaudeAccountBook: ObservableObject {
    struct Window: Codable, Equatable {
        var label: String
        var usedFraction: Double
        var resetsAt: Date
        var seconds: Double?
    }

    struct Entry: Codable, Equatable {
        var id: String
        /// Order the account was first seen in; names "Claude account <n>".
        var ordinal: Int
        /// Chosen in the hub. Nil means the default name.
        var customName: String?
        var email: String?
        var plan: String?
        var windows: [Window]
        /// Epoch 0 with no windows: named before any reading.
        var capturedAt: Date

        var hasReading: Bool { !windows.isEmpty }
    }

    private struct File: Codable {
        var schema = 1
        var accounts: [Entry]
    }

    /// Bumped whenever the list or a name changes, for the hub and the notch card.
    @Published private(set) var revision = 0
    private(set) var entries: [Entry] = []
    private let url: URL

    nonisolated static var defaultURL: URL {
        FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
            .appendingPathComponent("Pulse", isDirectory: true)
            .appendingPathComponent("claude-account-usage.json")
    }

    init(url: URL = ClaudeAccountBook.defaultURL) {
        self.url = url
        let decoder = JSONDecoder()
        decoder.dateDecodingStrategy = .secondsSince1970
        if let data = try? Data(contentsOf: url),
           let file = try? decoder.decode(File.self, from: data) {
            entries = file.accounts
        }
    }

    // MARK: - Names

    private func name(of entry: Entry) -> String {
        if let custom = entry.customName, !custom.isEmpty { return custom }
        if let email = entry.email, !email.isEmpty { return email }
        return Self.defaultName(id: entry.id)
    }

    /// Claude Desktop has no non-secret email, so an unnamed account reads as
    /// the first 8 characters of its id.
    nonisolated static func defaultName(id: String) -> String { "Claude \(id.prefix(8))" }

    private var nextOrdinal: Int { (entries.map(\.ordinal).max() ?? 0) + 1 }

    /// The name for the signed-in account: its chosen or default name, or just
    /// its address before the first reading has been saved.
    func activeName(id: String, email: String?) -> String? {
        if let entry = entries.first(where: { $0.id == id }) { return name(of: entry) }
        guard let email, !email.isEmpty else { return nil }
        return email
    }

    // MARK: - Changes

    /// Save the signed-in account's fresh reading. Windows without a reset time
    /// are left out: with no reset time there is no way to say when a saved
    /// percentage stops being true.
    func record(id: String, email: String?, plan: String?, windows: [LimitWindow],
                at now: Date = Date()) {
        let kept = windows.compactMap { window -> Window? in
            guard let used = window.usedFraction, let resetsAt = window.resetsAt else { return nil }
            return Window(label: window.label, usedFraction: used, resetsAt: resetsAt,
                          seconds: window.duration)
        }
        guard !kept.isEmpty else { return }
        let address = (email?.isEmpty ?? true) ? nil : email
        if let index = entries.firstIndex(where: { $0.id == id }) {
            let old = entries[index]
            // Unchanged numbers are not rewritten more than once a minute.
            if old.windows == kept, old.plan == plan, old.email == (address ?? old.email),
               now.timeIntervalSince(old.capturedAt) < 60 { return }
            entries[index].windows = kept
            entries[index].plan = plan
            entries[index].email = address ?? old.email
            entries[index].capturedAt = now
        } else {
            entries.append(Entry(id: id, ordinal: nextOrdinal, customName: nil, email: address,
                                 plan: plan, windows: kept, capturedAt: now))
        }
        commit()
    }

    /// An empty name returns the account to its default name. An account with
    /// no entry yet (a folder on disk, never read) gets one so the name sticks;
    /// an id that is neither saved nor on disk is ignored.
    func rename(id: String, to proposed: String, folders: [String: Date] = ClaudeAccountBook.discoverFolders()) {
        let trimmed = String(proposed.trimmingCharacters(in: .whitespacesAndNewlines).prefix(60))
        let value = trimmed.isEmpty ? nil : trimmed
        guard let index = entries.firstIndex(where: { $0.id == id }) else {
            guard let value, folders[id] != nil else { return }
            entries.append(Entry(id: id, ordinal: nextOrdinal, customName: value, email: nil, plan: nil,
                                 windows: [], capturedAt: Date(timeIntervalSince1970: 0)))
            commit()
            return
        }
        guard entries[index].customName != value else { return }
        entries[index].customName = value
        // A named stub that is renamed back to the default has nothing left to keep.
        if value == nil, !entries[index].hasReading, folders[id] != nil { entries.remove(at: index) }
        commit()
    }

    /// Drop an old account from the list. The signed-in account stays, and so
    /// does any account whose folder is still on disk: it would reappear at
    /// once, and forgetting it would only lose its name.
    func forget(id: String, keepingActive activeID: String?,
                folders: [String: Date] = ClaudeAccountBook.discoverFolders()) {
        guard id != activeID, folders[id] == nil, entries.contains(where: { $0.id == id }) else { return }
        entries.removeAll { $0.id == id }
        commit()
    }

    // MARK: - Account folders

    /// Account id to the newest modification time of its folder, from the
    /// union of `claude-code-sessions` and `local-agent-mode-sessions` under
    /// Claude Desktop's data folder. Names only; nothing inside is read. Empty
    /// when Claude Desktop is not installed.
    nonisolated static func discoverFolders(
        root: URL = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
            .appendingPathComponent("Claude", isDirectory: true)
    ) -> [String: Date] {
        var found: [String: Date] = [:]
        for parent in ["claude-code-sessions", "local-agent-mode-sessions"] {
            let dir = root.appendingPathComponent(parent, isDirectory: true)
            guard let children = try? FileManager.default.contentsOfDirectory(
                at: dir, includingPropertiesForKeys: [.isDirectoryKey, .contentModificationDateKey],
                options: [.skipsHiddenFiles]) else { continue }
            for child in children where isAccountID(child.lastPathComponent) {
                let values = try? child.resourceValues(forKeys: [.isDirectoryKey, .contentModificationDateKey])
                guard values?.isDirectory == true else { continue }
                let modified = values?.contentModificationDate ?? .distantPast
                let id = child.lastPathComponent
                found[id] = max(found[id] ?? .distantPast, modified)
            }
        }
        return found
    }

    /// A canonical 8-4-4-4-12 hex id; anything else in those folders is not an account.
    nonisolated static func isAccountID(_ name: String) -> Bool {
        let parts = name.split(separator: "-", omittingEmptySubsequences: false)
        guard parts.map(\.count) == [8, 4, 4, 4, 12] else { return false }
        return parts.allSatisfy { $0.allSatisfy(\.isHexDigit) }
    }

    private func commit() {
        revision &+= 1
        let encoder = JSONEncoder()
        encoder.dateEncodingStrategy = .secondsSince1970
        encoder.outputFormatting = [.sortedKeys]
        guard let data = try? encoder.encode(File(accounts: entries)) else { return }
        try? FileManager.default.createDirectory(at: url.deletingLastPathComponent(),
                                                 withIntermediateDirectories: true)
        try? data.write(to: url, options: .atomic)
    }

    // MARK: - For the hub

    /// Every account for `notch-state.json`: saved ones plus every account
    /// folder on disk, the one Claude Desktop is signed in to first, then by
    /// last reading, then by folder time. Dates are epoch seconds; the hub
    /// decides "resets in" and "Reset" with its own clock. An account with no
    /// saved reading has no `windows` and a null `capturedAt`; `onDisk` is
    /// false only for an account whose folder is gone (the only kind the hub
    /// may forget). `activeID` is Claude Desktop's account (else Claude Code's);
    /// `codeID` and `codeEmail` are Claude Code's, whose address applies only to
    /// the account with that id.
    func published(activeID: String?, codeID: String?, codeEmail: String?,
                   folders: [String: Date] = ClaudeAccountBook.discoverFolders()) -> [[String: Any]] {
        func row(_ entry: Entry, active: Bool) -> [String: Any] {
            var row: [String: Any] = [
                "id": entry.id,
                "name": name(of: entry),
                "active": active,
                "onDisk": folders[entry.id] != nil,
                "capturedAt": entry.hasReading ? entry.capturedAt.timeIntervalSince1970 : NSNull(),
                "windows": entry.windows.map { window -> [String: Any] in
                    var item: [String: Any] = [
                        "label": window.label,
                        "usedFraction": window.usedFraction,
                        "resetsAt": window.resetsAt.timeIntervalSince1970,
                    ]
                    if let seconds = window.seconds { item["seconds"] = seconds }
                    return item
                },
            ]
            if let email = entry.email { row["email"] = email }
            if let plan = entry.plan { row["plan"] = plan }
            return row
        }
        let address = (codeEmail?.isEmpty ?? true) ? nil : codeEmail
        var all = entries
        for id in folders.keys where !all.contains(where: { $0.id == id }) {
            all.append(Entry(id: id, ordinal: 0, customName: nil, email: id == codeID ? address : nil,
                             plan: nil, windows: [], capturedAt: Date(timeIntervalSince1970: 0)))
        }
        if let activeID, !all.contains(where: { $0.id == activeID }) {
            all.append(Entry(id: activeID, ordinal: 0, customName: nil, email: activeID == codeID ? address : nil,
                             plan: nil, windows: [], capturedAt: Date(timeIntervalSince1970: 0)))
        }
        // An unread account with a folder shows Claude Code's address when it is that account.
        all = all.map { entry in
            var entry = entry
            if entry.email == nil, entry.id == codeID, !entry.hasReading { entry.email = address }
            return entry
        }
        func rank(_ entry: Entry) -> (Bool, Date, Date, String) {
            (entry.id == activeID, entry.hasReading ? entry.capturedAt : .distantPast,
             folders[entry.id] ?? .distantPast, entry.id)
        }
        all.sort { a, b in
            let x = rank(a), y = rank(b)
            if x.0 != y.0 { return x.0 }
            if x.1 != y.1 { return x.1 > y.1 }
            if x.2 != y.2 { return x.2 > y.2 }
            return x.3 < y.3
        }
        return all.map { row($0, active: $0.id == activeID) }
    }
}
