import AppKit
import Foundation

struct LauncherItem: Identifiable {
    enum Section: Int {
        case answer, open, pinned, apps, running, quicklinks, snippets, commands
        case shortcuts, clipboard, dictionary, files, pulse

        var title: String {
            switch self {
            case .answer: return "Calculate"
            case .open: return "Open"
            case .pinned: return "Pinned"
            case .apps: return "Apps"
            case .running: return "Running"
            case .quicklinks: return "Quicklinks"
            case .snippets: return "Snippets"
            case .commands: return "Commands"
            case .shortcuts: return "Shortcuts"
            case .clipboard: return "Clipboard"
            case .dictionary: return "Dictionary"
            case .files: return "Files & folders"
            case .pulse: return "Pulse"
            }
        }
    }

    let id: String
    let section: Section
    let title: String
    let subtitle: String?
    /// A file whose icon stands for the row, or nil to use `symbol`.
    let iconPath: String?
    let symbol: String
    var image: NSImage? = nil
    /// The running app a row stands for: Command-Q quits it.
    var app: NSRunningApplication? = nil
    /// The app a row can pin or unpin with Command-P.
    var pinPath: String? = nil
    /// Keeps the launcher open after running, for commands whose output is shown.
    var keepsOpen = false
    let run: () -> Void
}

/// Text shown in place of the results, such as a command's output.
struct LauncherOutput: Equatable {
    let title: String
    var text: String
}

/// Which optional sources the person switched on in the hub.
struct LauncherFeatures: Equatable {
    var clipboard = false
    var currency = false
    var dictionary = true
    var shortcuts = true
}

/// What the panel shows for the text typed so far.
@MainActor
final class LauncherModel: ObservableObject {
    @Published var query = "" { didSet { if query != oldValue { selected = 0; recompute() } } }
    @Published private(set) var items: [LauncherItem] = []
    @Published var selected = 0
    /// Bumped on each show so the text field takes focus again.
    @Published private(set) var focusToken = 0
    @Published private(set) var output: LauncherOutput?

    let index = LauncherIndex()
    let clipboard = LauncherClipboard()
    let usage = LauncherUsage()
    let rates = LauncherRates()
    let shortcuts = LauncherShortcuts()
    private let files = LauncherFileSearch()
    private var fileHits: [LauncherFileSearch.Hit] = []
    private var previousApp: NSRunningApplication?

    var config = LauncherConfig()
    var features = LauncherFeatures()
    var snapshots: () -> [ProviderSnapshot] = { [] }
    var dismiss: () -> Void = {}
    /// Hands back a config the launcher changed itself (pins), for saving.
    var onConfigChange: (LauncherConfig) -> Void = { _ in }

    init() {
        index.onUpdate = { [weak self] in self?.recompute(searchFiles: false) }
        rates.onUpdate = { [weak self] in self?.recompute(searchFiles: false) }
        shortcuts.onUpdate = { [weak self] in self?.recompute(searchFiles: false) }
    }

    /// `previousApp` is the app that was in front; pasted text goes there.
    func willShow(previousApp: NSRunningApplication?) {
        self.previousApp = previousApp
        output = nil
        fileHits = []
        focusToken += 1
        index.refresh()
        query = ""
        selected = 0
        recompute(searchFiles: false)
    }

    func didHide() { files.cancel() }

    func clearOutput() { output = nil }

    func move(_ delta: Int) {
        guard !items.isEmpty else { return }
        selected = (selected + delta + items.count) % items.count
    }

    func activate(_ position: Int? = nil) {
        guard output == nil else { return }
        let i = position ?? selected
        guard items.indices.contains(i) else { return }
        let item = items[i]
        if !item.keepsOpen { dismiss() }
        item.run()
    }

    /// Pins or unpins the selected app.
    func togglePinSelected() {
        guard items.indices.contains(selected), let path = items[selected].pinPath else { return }
        var next = config
        if let position = next.pinnedApps.firstIndex(of: path) {
            next.pinnedApps.remove(at: position)
        } else {
            next.pinnedApps.append(path)
        }
        config = next
        onConfigChange(next)
        recompute(searchFiles: false)
    }

    /// Quits the selected running app. The panel stays open so the list can be checked.
    func quitSelected() {
        guard items.indices.contains(selected), let app = items[selected].app else { return }
        app.terminate()
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.6) { [weak self] in
            MainActor.assumeIsolated { self?.recompute(searchFiles: false) }
        }
    }

    /// Runs a custom command and shows its output in the panel.
    func runCommand(_ command: LauncherCommand) {
        usage.record("command:" + command.name)
        output = LauncherOutput(title: command.name, text: "Running…")
        LauncherShell.run(command.command) { [weak self] result in
            guard let self, self.output?.title == command.name else { return }
            self.output = LauncherOutput(title: command.name, text: result)
        }
    }

    // MARK: - Results

    private func recompute(searchFiles: Bool = true) {
        let text = query.trimmingCharacters(in: .whitespaces)
        if searchFiles {
            fileHits = []
            files.search(text, folders: config.fileFolders) { [weak self] hits in
                guard let self, self.query.trimmingCharacters(in: .whitespaces) == text else { return }
                self.fileHits = hits
                self.recompute(searchFiles: false)
            }
        }

        if text.isEmpty {
            items = pinnedItems() + recentItems() + runningItems()
            clamp()
            return
        }
        if let rest = clipboardRest(text) {
            items = clipboardItems(rest)
            clamp()
            return
        }

        let lowered = text.lowercased()
        if features.currency, [" to ", " in ", " as "].contains(where: { lowered.contains($0) }) {
            rates.refreshIfStale()
        }
        var next = answerItems(text) + openItems(text) + appItems(text)
        next += quicklinkItems(text) + snippetItems(text) + commandItems(text)
        next += shortcutItems(text) + dictionaryItems(text, fallback: false)
        if next.isEmpty { next += dictionaryItems(text, fallback: true) }
        next += fileItems()
        items = next
        clamp()
    }

    private func clamp() {
        selected = min(max(selected, 0), max(items.count - 1, 0))
    }

    private func runningByPath() -> [String: NSRunningApplication] {
        var map: [String: NSRunningApplication] = [:]
        for app in LauncherApps.running() {
            if let path = app.bundleURL?.path { map[path] = app }
        }
        return map
    }

    private func appItem(_ url: URL, section: LauncherItem.Section, subtitle: String?,
                         app: NSRunningApplication?) -> LauncherItem {
        let path = url.path
        return LauncherItem(
            id: "\(section.rawValue):\(path)", section: section,
            title: url.deletingPathExtension().lastPathComponent,
            subtitle: subtitle, iconPath: path, symbol: "app",
            app: app, pinPath: path,
            run: { [weak self] in
                self?.usage.record("app:" + path)
                NSWorkspace.shared.open(url)
            })
    }

    private func pinnedItems() -> [LauncherItem] {
        let running = runningByPath()
        return config.pinnedApps.compactMap { path -> LauncherItem? in
            guard FileManager.default.fileExists(atPath: path) else { return nil }
            return appItem(URL(fileURLWithPath: path), section: .pinned,
                           subtitle: running[path] != nil ? "Pinned · Running" : "Pinned",
                           app: running[path])
        }
    }

    /// Apps used most often and most recently, when nothing is typed.
    private func recentItems() -> [LauncherItem] {
        let pinned = Set(config.pinnedApps)
        let running = runningByPath()
        let paths = usage.top(prefix: "app:", limit: 10)
            .filter { !pinned.contains($0) && FileManager.default.fileExists(atPath: $0) }
            .prefix(6)
        return paths.map { path in
            appItem(URL(fileURLWithPath: path), section: .apps,
                    subtitle: running[path] != nil ? "Running" : nil, app: running[path])
        }
    }

    private func runningItems() -> [LauncherItem] {
        let pinned = Set(config.pinnedApps)
        let apps = LauncherApps.running().prefix(10).compactMap { app -> LauncherItem? in
            guard let url = app.bundleURL else { return nil }
            let subtitle = pinned.contains(url.path) ? "Pinned · Running" : "Running · ⌘Q quits"
            return appItem(url, section: .running, subtitle: subtitle, app: app)
        }
        return Array(apps)
    }

    private func appItems(_ text: String) -> [LauncherItem] {
        var out: [LauncherItem] = []
        if text.lowercased() == "quit all" {
            out.append(LauncherItem(
                id: "quit-all", section: .running, title: "Quit all running apps",
                subtitle: "Asks every app to quit, except Pulse", iconPath: nil, symbol: "power",
                run: { LauncherApps.quitAll() }))
        }
        let running = runningByPath()
        let pinned = Set(config.pinnedApps)
        let matches = index.matches(text, limit: 6) { app in
            self.usage.bonus(for: "app:" + app.url.path)
        }
        for app in matches {
            let path = app.url.path
            var parts: [String] = []
            if pinned.contains(path) { parts.append("Pinned") }
            if running[path] != nil { parts.append("Running · ⌘Q quits") }
            out.append(appItem(app.url, section: .apps,
                               subtitle: parts.isEmpty ? nil : parts.joined(separator: " · "),
                               app: running[path]))
        }
        return out
    }

    private func answerItems(_ text: String) -> [LauncherItem] {
        if let conversion = LauncherConversion.evaluate(text, currencies: features.currency, rates: rates) {
            return [LauncherItem(
                id: "conv", section: .answer, title: conversion.title, subtitle: conversion.detail,
                iconPath: nil, symbol: "arrow.left.arrow.right",
                run: { LauncherActions.copy(conversion.copy) })]
        }
        if let answer = LauncherCalculator.evaluate(text) {
            return [LauncherItem(
                id: "calc", section: .answer, title: answer,
                subtitle: "\(text) — Return copies the result",
                iconPath: nil, symbol: "equal.square",
                run: { LauncherActions.copy(answer) })]
        }
        return []
    }

    /// A typed http(s) address or an absolute or `~` path that exists.
    private func openItems(_ text: String) -> [LauncherItem] {
        let lower = text.lowercased()
        if lower.hasPrefix("http://") || lower.hasPrefix("https://"),
           let url = URL(string: text), url.host?.isEmpty == false {
            return [LauncherItem(id: "url", section: .open, title: "Open \(text)",
                                 subtitle: "In your default browser", iconPath: nil,
                                 symbol: "safari", run: { NSWorkspace.shared.open(url) })]
        }
        if text.hasPrefix("/") || text.hasPrefix("~") {
            let path = (text as NSString).expandingTildeInPath
            var isDir: ObjCBool = false
            guard FileManager.default.fileExists(atPath: path, isDirectory: &isDir) else { return [] }
            let url = URL(fileURLWithPath: path)
            return [LauncherItem(id: "path", section: .open, title: "Open \(url.lastPathComponent)",
                                 subtitle: Self.abbreviate(path), iconPath: path,
                                 symbol: isDir.boolValue ? "folder" : "doc",
                                 run: { NSWorkspace.shared.open(url) })]
        }
        return []
    }

    /// The argument after a keyword typed as the first word, or nil when the keyword does not match.
    private func keywordArgument(_ text: String, _ keyword: String) -> String? {
        let key = keyword.trimmingCharacters(in: .whitespaces).lowercased()
        guard !key.isEmpty else { return nil }
        let parts = text.split(separator: " ", maxSplits: 1, omittingEmptySubsequences: true)
        guard let first = parts.first, first.lowercased() == key else { return nil }
        return parts.count > 1 ? parts[1].trimmingCharacters(in: .whitespaces) : ""
    }

    /// Whether a keyword matches, or the text fuzzily matches the name (two characters or more).
    private func matchesItem(_ text: String, keyword: String, name: String) -> String? {
        if let argument = keywordArgument(text, keyword) { return argument }
        guard text.count >= 2, LauncherIndex.score(text, name) != nil else { return nil }
        return text
    }

    private func quicklinkItems(_ text: String) -> [LauncherItem] {
        var out: [LauncherItem] = []
        for (position, link) in config.quicklinks.enumerated() where !link.template.isEmpty {
            guard let argument = matchesItem(text, keyword: link.keyword, name: link.name) else { continue }
            let target = LauncherTemplate.fill(link.template, [
                "query": LauncherTemplate.encode(argument),
                "clipboard": LauncherTemplate.clipboardText(),
                "date": LauncherTemplate.today(),
            ])
            let name = link.name.isEmpty ? link.keyword : link.name
            out.append(LauncherItem(
                id: "quicklink:\(position)", section: .quicklinks,
                title: argument.isEmpty ? name : "\(name): \(argument)",
                subtitle: target, iconPath: nil, symbol: "link",
                run: { LauncherActions.open(target) }))
            if out.count >= 4 { break }
        }
        return out
    }

    private func snippetItems(_ text: String) -> [LauncherItem] {
        var out: [LauncherItem] = []
        for (position, snippet) in config.snippets.enumerated() where !snippet.body.isEmpty {
            guard let argument = matchesItem(text, keyword: snippet.keyword, name: snippet.name) else { continue }
            let expanded = LauncherTemplate.fill(snippet.body, [
                "argument": argument,
                "clipboard": LauncherTemplate.clipboardText(),
                "date": LauncherTemplate.today(),
            ])
            let firstLine = expanded.split(whereSeparator: { $0.isNewline }).first.map(String.init) ?? ""
            out.append(LauncherItem(
                id: "snippet:\(position)", section: .snippets,
                title: snippet.name.isEmpty ? snippet.keyword : snippet.name,
                subtitle: String(firstLine.prefix(140)), iconPath: nil, symbol: "text.alignleft",
                run: { [weak self] in
                    guard let self else { return }
                    LauncherActions.paste(expanded, into: self.previousApp, clipboard: self.clipboard)
                }))
            if out.count >= 4 { break }
        }
        return out
    }

    private func commandItems(_ text: String) -> [LauncherItem] {
        var out: [LauncherItem] = []
        for (position, command) in config.commands.enumerated() where !command.command.isEmpty {
            guard matchesItem(text, keyword: command.keyword, name: command.name) != nil else { continue }
            out.append(LauncherItem(
                id: "command:\(position)", section: .commands,
                title: command.name.isEmpty ? command.keyword : command.name,
                subtitle: "Runs in your shell · output shows here", iconPath: nil, symbol: "terminal",
                keepsOpen: true,
                run: { [weak self] in self?.runCommand(command) }))
            if out.count >= 4 { break }
        }
        return out + pulseCommands(text)
    }

    private func shortcutItems(_ text: String) -> [LauncherItem] {
        guard features.shortcuts, text.count >= 2 else { return [] }
        shortcuts.refreshIfStale()
        let matches = shortcuts.names
            .compactMap { name -> (String, Int)? in LauncherIndex.score(text, name).map { (name, $0) } }
            .sorted { $0.1 > $1.1 }
            .prefix(4)
        return matches.map { name, _ in
            LauncherItem(id: "shortcut:\(name)", section: .shortcuts, title: name,
                         subtitle: "Runs in the Shortcuts app", iconPath: nil,
                         symbol: "bolt.horizontal", run: { LauncherShortcuts.run(name) })
        }
    }

    /// "define <word>" always; a bare word only when nothing else matched.
    private func dictionaryItems(_ text: String, fallback: Bool) -> [LauncherItem] {
        guard features.dictionary else { return [] }
        var term: String?
        if text.lowercased().hasPrefix("define ") {
            term = String(text.dropFirst(7)).trimmingCharacters(in: .whitespaces)
        } else if fallback, Self.isWord(text) {
            term = text
        }
        guard let word = term, !word.isEmpty else { return [] }
        let subtitle = LauncherDictionary.definition(of: word)
            .map { String($0.replacingOccurrences(of: "\n", with: " ").prefix(140)) }
            ?? "No entry; Return opens Dictionary"
        return [LauncherItem(id: "define:\(word)", section: .dictionary, title: "Define \(word)",
                             subtitle: subtitle, iconPath: nil, symbol: "book",
                             run: { LauncherDictionary.open(word) })]
    }

    private func clipboardRest(_ text: String) -> String? {
        let lower = text.lowercased()
        for prefix in ["clipboard", "clip"] {
            if lower == prefix { return "" }
            if lower.hasPrefix(prefix + " ") {
                return String(text.dropFirst(prefix.count + 1)).trimmingCharacters(in: .whitespaces)
            }
        }
        return nil
    }

    private func clipboardItems(_ rest: String) -> [LauncherItem] {
        guard features.clipboard else {
            return [LauncherItem(
                id: "clip-off", section: .clipboard, title: "Clipboard history is off",
                subtitle: "Turn it on in Pulse's hub, under Settings, General, Launcher",
                iconPath: nil, symbol: "clipboard", run: {})]
        }
        var out: [LauncherItem] = []
        for entry in clipboard.matching(rest, limit: 12) {
            let when = entry.createdAt.formatted(.relative(presentation: .named))
            switch entry.kind {
            case .text:
                out.append(LauncherItem(
                    id: "clip:\(entry.id)", section: .clipboard, title: entry.firstLine,
                    subtitle: "Text · \(when)", iconPath: nil, symbol: "doc.text",
                    run: { [weak self] in self?.pasteEntry(entry) }))
            case .png, .tiff:
                out.append(LauncherItem(
                    id: "clip:\(entry.id)", section: .clipboard, title: "Image",
                    subtitle: "Image · \(when)", iconPath: nil, symbol: "photo",
                    image: clipboard.thumbnail(entry),
                    run: { [weak self] in self?.pasteEntry(entry) }))
            }
        }
        if rest.isEmpty || rest.lowercased().hasPrefix("clear") {
            out.append(LauncherItem(
                id: "clip-clear", section: .clipboard, title: "Clear clipboard history",
                subtitle: "Removes every stored copy", iconPath: nil, symbol: "trash",
                run: { [weak self] in self?.clipboard.clear() }))
        }
        return out
    }

    private func pasteEntry(_ entry: LauncherClipboard.Entry) {
        clipboard.copy(entry)
        LauncherActions.send(into: previousApp)
    }

    private func fileItems() -> [LauncherItem] {
        fileHits.map { hit in
            LauncherItem(
                id: "file:\(hit.url.path)", section: .files,
                title: hit.url.lastPathComponent,
                subtitle: Self.abbreviate(hit.url.deletingLastPathComponent().path),
                iconPath: hit.url.path, symbol: hit.isDirectory ? "folder" : "doc",
                run: { NSWorkspace.shared.open(hit.url) })
        }
    }

    /// A single word, or two, of letters.
    private static func isWord(_ text: String) -> Bool {
        text.count >= 3 && text.split(separator: " ").count <= 2
            && text.allSatisfy { $0.isLetter || $0 == " " || $0 == "-" || $0 == "'" }
    }

    private static func abbreviate(_ path: String) -> String {
        (path as NSString).abbreviatingWithTildeInPath
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
        Command(title: "Clear clipboard history", keywords: ["clipboard", "clear", "history", "copies"], symbol: "trash", section: nil, usageProvider: nil),
    ]

    private func pulseCommands(_ text: String) -> [LauncherItem] {
        var scored: [(Command, Int)] = []
        for command in Self.commandList {
            let best = ([command.title] + command.keywords)
                .compactMap { LauncherIndex.score(text, $0) }.max()
            if let best { scored.append((command, best)) }
        }
        return scored.sorted { $0.1 > $1.1 }.prefix(4).map { command, _ in
            let section = command.section
            let clearsClipboard = command.title == "Clear clipboard history"
            return LauncherItem(
                id: "cmd:\(command.title)", section: .pulse, title: command.title,
                subtitle: command.usageProvider.map(usageLine) ?? (clearsClipboard ? "Removes every stored copy" : "Opens Pulse"),
                iconPath: nil, symbol: command.symbol,
                run: { [weak self] in
                    if clearsClipboard {
                        self?.clipboard.clear()
                    } else if let section {
                        HubLauncher.open(section: section)
                    }
                })
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
