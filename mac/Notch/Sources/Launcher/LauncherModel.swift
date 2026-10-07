import AppKit
import Foundation

struct LauncherItem: Identifiable {
    enum Section: Int {
        case answer, open, apps, commands, files

        var title: String {
            switch self {
            case .answer: return "Calculator"
            case .open: return "Open"
            case .apps: return "Apps"
            case .commands: return "Cockpit"
            case .files: return "Files & folders"
            }
        }
    }

    let id: String
    let section: Section
    let title: String
    let subtitle: String?
    /// A file whose icon stands for the row, or an SF Symbol name.
    let iconPath: String?
    let symbol: String
    let run: () -> Void
}

/// What the panel shows for the text typed so far.
@MainActor
final class LauncherModel: ObservableObject {
    @Published var query = "" { didSet { if query != oldValue { selected = 0; recompute() } } }
    @Published private(set) var items: [LauncherItem] = []
    @Published var selected = 0
    /// Bumped on each show so the text field takes focus again.
    @Published private(set) var focusToken = 0

    let index = LauncherIndex()
    private let files = LauncherFileSearch()
    private var fileHits: [LauncherFileSearch.Hit] = []
    var snapshots: () -> [ProviderSnapshot] = { [] }
    var dismiss: () -> Void = {}

    init() {
        index.onUpdate = { [weak self] in self?.recompute(searchFiles: false) }
    }

    func willShow() {
        query = ""
        fileHits = []
        selected = 0
        focusToken += 1
        index.refresh()
        recompute(searchFiles: false)
    }

    func didHide() { files.cancel() }

    func move(_ delta: Int) {
        guard !items.isEmpty else { return }
        selected = (selected + delta + items.count) % items.count
    }

    func activate(_ position: Int? = nil) {
        let i = position ?? selected
        guard items.indices.contains(i) else { return }
        let item = items[i]
        dismiss()
        item.run()
    }

    // MARK: - Results

    private func recompute(searchFiles: Bool = true) {
        let text = query.trimmingCharacters(in: .whitespaces)
        if searchFiles {
            fileHits = []
            files.search(text) { [weak self] hits in
                guard let self, self.query.trimmingCharacters(in: .whitespaces) == text else { return }
                self.fileHits = hits
                self.recompute(searchFiles: false)
            }
        }
        var next: [LauncherItem] = []
        if !text.isEmpty {
            if let answer = LauncherCalculator.evaluate(text) {
                next.append(LauncherItem(
                    id: "calc", section: .answer, title: answer,
                    subtitle: "\(text) — Return copies the result",
                    iconPath: nil, symbol: "equal.square",
                    run: {
                        NSPasteboard.general.clearContents()
                        NSPasteboard.general.setString(answer, forType: .string)
                    }))
            }
            if let direct = directOpen(text) { next.append(direct) }
            for app in index.matches(text, limit: 6) {
                next.append(LauncherItem(
                    id: "app:\(app.url.path)", section: .apps, title: app.name,
                    subtitle: nil, iconPath: app.url.path, symbol: "app",
                    run: { NSWorkspace.shared.open(app.url) }))
            }
            next += commands(matching: text)
            for hit in fileHits {
                next.append(LauncherItem(
                    id: "file:\(hit.url.path)", section: .files,
                    title: hit.url.lastPathComponent,
                    subtitle: Self.abbreviate(hit.url.deletingLastPathComponent().path),
                    iconPath: hit.url.path, symbol: hit.isDirectory ? "folder" : "doc",
                    run: { NSWorkspace.shared.open(hit.url) }))
            }
        }
        items = next
        selected = min(selected, max(next.count - 1, 0))
        if selected < 0 { selected = 0 }
    }

    private static func abbreviate(_ path: String) -> String {
        (path as NSString).abbreviatingWithTildeInPath
    }

    /// A typed http(s) address or an absolute or `~` path that exists.
    private func directOpen(_ text: String) -> LauncherItem? {
        let lower = text.lowercased()
        if lower.hasPrefix("http://") || lower.hasPrefix("https://"),
           let url = URL(string: text), url.host?.isEmpty == false {
            return LauncherItem(id: "url", section: .open, title: "Open \(text)",
                                subtitle: "In your default browser", iconPath: nil,
                                symbol: "safari", run: { NSWorkspace.shared.open(url) })
        }
        if text.hasPrefix("/") || text.hasPrefix("~") {
            let path = (text as NSString).expandingTildeInPath
            var isDir: ObjCBool = false
            guard FileManager.default.fileExists(atPath: path, isDirectory: &isDir) else { return nil }
            let url = URL(fileURLWithPath: path)
            return LauncherItem(id: "path", section: .open, title: "Open \(url.lastPathComponent)",
                                subtitle: Self.abbreviate(path), iconPath: path,
                                symbol: isDir.boolValue ? "folder" : "doc",
                                run: { NSWorkspace.shared.open(url) })
        }
        return nil
    }

    private struct Command {
        let title: String
        let keywords: [String]
        let symbol: String
        let section: String?
        let usageProvider: String?
    }

    private static let commandList: [Command] = [
        Command(title: "Open Storage", keywords: ["storage", "disk", "space", "files"], symbol: "internaldrive", section: "storage", usageProvider: nil),
        Command(title: "Open Cleanup", keywords: ["cleanup", "clean", "caches", "trash"], symbol: "sparkles", section: "cleanup", usageProvider: nil),
        Command(title: "Open Monitor", keywords: ["monitor", "cpu", "memory", "activity"], symbol: "waveform.path.ecg", section: "monitor", usageProvider: nil),
        Command(title: "Open Apps", keywords: ["apps", "uninstall", "processes", "quit"], symbol: "square.grid.2x2", section: "apps", usageProvider: nil),
        Command(title: "Open Settings", keywords: ["settings", "preferences", "options"], symbol: "gearshape", section: "general", usageProvider: nil),
        Command(title: "Claude usage", keywords: ["claude", "usage", "limit", "anthropic"], symbol: "gauge.with.dots.needle.50percent", section: "accounts", usageProvider: "claude"),
        Command(title: "Codex usage", keywords: ["codex", "usage", "limit", "openai"], symbol: "gauge.with.dots.needle.50percent", section: "accounts", usageProvider: "codex"),
    ]

    private func commands(matching text: String) -> [LauncherItem] {
        var scored: [(Command, Int)] = []
        for command in Self.commandList {
            let best = ([command.title] + command.keywords)
                .compactMap { LauncherIndex.score(text, $0) }.max()
            if let best { scored.append((command, best)) }
        }
        return scored.sorted { $0.1 > $1.1 }.prefix(4).map { command, _ in
            let section = command.section
            return LauncherItem(
                id: "cmd:\(command.title)", section: .commands, title: command.title,
                subtitle: command.usageProvider.map(usageLine) ?? "Opens Cockpit",
                iconPath: nil, symbol: command.symbol,
                run: { if let section { HubLauncher.open(section: section) } })
        }
    }

    /// The notch's current reading for an account, or why there is none.
    private func usageLine(_ providerPrefix: String) -> String {
        let all = snapshots().filter { $0.kind == .usage && $0.id.hasPrefix(providerPrefix) }
        guard let snapshot = all.first else { return "Not connected" }
        guard case .ok = snapshot.status else {
            if snapshot.status.isStale, let when = snapshot.status.staleSince {
                return "Last reading \(when.formatted(date: .omitted, time: .shortened)) — \(Self.readings(snapshot))"
            }
            return "No reading — open Accounts to see why"
        }
        return Self.readings(snapshot)
    }

    private static func readings(_ snapshot: ProviderSnapshot) -> String {
        let parts = snapshot.windows.filter { $0.group == nil }.prefix(3).compactMap { window -> String? in
            guard let used = window.usedFraction else { return nil }
            return "\(window.label) \(Int((used * 100).rounded()))%"
        }
        return parts.isEmpty ? "No reading yet" : parts.joined(separator: " · ")
    }
}
