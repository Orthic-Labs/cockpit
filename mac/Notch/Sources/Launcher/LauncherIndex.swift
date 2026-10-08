import Foundation

struct LauncherApp: Equatable {
    let name: String
    let url: URL
}

/// Installed apps and fuzzy matching. Nothing runs until the first hotkey
/// press; each later open rescans in the background, which is a few hundred
/// directory entries and so cheaper than watching for changes.
@MainActor
final class LauncherIndex {
    private(set) var apps: [LauncherApp] = []
    private(set) var loaded = false
    private var scanning = false
    var onUpdate: (() -> Void)?

    private static var roots: [URL] {
        [URL(fileURLWithPath: "/Applications", isDirectory: true),
         URL(fileURLWithPath: "/System/Applications", isDirectory: true),
         FileManager.default.homeDirectoryForCurrentUser.appendingPathComponent("Applications", isDirectory: true)]
    }

    func refresh() {
        guard !scanning else { return }
        scanning = true
        let roots = Self.roots
        DispatchQueue.global(qos: .userInitiated).async { [weak self] in
            let found = Self.scan(roots)
            DispatchQueue.main.async {
                guard let self else { return }
                self.scanning = false
                self.loaded = true
                if found != self.apps {
                    self.apps = found
                    self.onUpdate?()
                }
            }
        }
    }

    /// Apps directly in a root plus one level of plain subfolders (Utilities,
    /// vendor folders). Bundles are never entered.
    nonisolated private static func scan(_ roots: [URL]) -> [LauncherApp] {
        let fm = FileManager.default
        var seen = Set<String>()
        var result: [LauncherApp] = []
        func add(_ url: URL) {
            let path = url.resolvingSymlinksInPath().path
            guard seen.insert(path).inserted else { return }
            result.append(LauncherApp(name: url.deletingPathExtension().lastPathComponent, url: url))
        }
        for root in roots {
            guard let entries = try? fm.contentsOfDirectory(
                at: root, includingPropertiesForKeys: [.isDirectoryKey],
                options: [.skipsHiddenFiles]) else { continue }
            for entry in entries {
                if entry.pathExtension == "app" {
                    add(entry)
                } else if (try? entry.resourceValues(forKeys: [.isDirectoryKey]))?.isDirectory == true {
                    let inner = (try? fm.contentsOfDirectory(
                        at: entry, includingPropertiesForKeys: nil,
                        options: [.skipsHiddenFiles])) ?? []
                    for app in inner where app.pathExtension == "app" { add(app) }
                }
            }
        }
        return result.sorted { $0.name.localizedCaseInsensitiveCompare($1.name) == .orderedAscending }
    }

    /// Best fuzzy matches. `bonus` adds usage points (frecency) to each score.
    func matches(_ query: String, limit: Int, bonus: (LauncherApp) -> Int) -> [LauncherApp] {
        let scored: [(LauncherApp, Int)] = apps.compactMap { app in
            LauncherIndex.score(query, app.name).map { (app, $0 + bonus(app)) }
        }
        return scored
            .sorted { $0.1 != $1.1 ? $0.1 > $1.1 : $0.0.name.count < $1.0.name.count }
            .prefix(limit)
            .map(\.0)
    }

    /// Nil unless every query character appears in order. Prefix, word-start
    /// and contiguous matches score above scattered ones.
    nonisolated static func score(_ query: String, _ name: String) -> Int? {
        let q = Array(query.lowercased().filter { !$0.isWhitespace })
        let n = Array(name.lowercased())
        guard !q.isEmpty else { return nil }
        var qi = 0
        var score = 0
        var previous = -2
        for (i, c) in n.enumerated() where qi < q.count && c == q[qi] {
            score += 10
            if i == previous + 1 { score += 15 }
            if i == 0 || !n[i - 1].isLetter && !n[i - 1].isNumber { score += 25 }
            previous = i
            qi += 1
        }
        guard qi == q.count else { return nil }
        let lowered = String(n)
        let joined = String(q)
        if lowered.hasPrefix(joined) { score += 200 }
        else if lowered.contains(joined) { score += 80 }
        return score - (n.count - q.count)
    }
}
