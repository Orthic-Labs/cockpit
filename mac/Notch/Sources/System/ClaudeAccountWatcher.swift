import Foundation

/// Pulse fork: notices when Claude Desktop's signed-in account changes and has
/// the notch read the new account's usage at once.
///
/// Reads only the non-secret `lastKnownAccountUuid` in Claude's `config.json`,
/// and only when the file's modification date moves. Polled, not watched by
/// vnode: Claude rewrites the file by rename, which ends a file watch.
@MainActor
final class ClaudeAccountWatcher {
    private let configURL: URL
    private let onSwitch: () -> Void
    private var lastAccount: String?
    private var lastModified: Date?
    private var timer: Timer?
    private var fastUntil = Date.distantPast
    private var observer: NSObjectProtocol?

    /// Posted by `ClaudeRestart` once it has reopened Claude.
    static let claudeReopened = Notification.Name("dev.orthic.pulse.claudeReopened")

    init(onSwitch: @escaping () -> Void) {
        self.configURL = FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent("Library/Application Support/Claude/config.json")
        self.onSwitch = onSwitch
    }

    func start() {
        guard timer == nil else { return }
        lastAccount = Self.account(at: configURL)
        lastModified = Self.modified(configURL)
        schedule(every: 3)
        observer = NotificationCenter.default.addObserver(
            forName: Self.claudeReopened, object: nil, queue: .main
        ) { [weak self] _ in
            MainActor.assumeIsolated { self?.expectSwitch() }
        }
    }

    /// After the restart button reopens Claude: look every second for a minute.
    func expectSwitch() {
        fastUntil = Date().addingTimeInterval(60)
        schedule(every: 1)
    }

    private func schedule(every interval: TimeInterval) {
        timer?.invalidate()
        let timer = Timer(timeInterval: interval, repeats: true) { [weak self] _ in
            MainActor.assumeIsolated { self?.poll() }
        }
        timer.tolerance = interval / 4
        RunLoop.main.add(timer, forMode: .common)
        self.timer = timer
    }

    private func poll() {
        if fastUntil != .distantPast, Date() > fastUntil {
            fastUntil = .distantPast
            schedule(every: 3)
        }
        let modified = Self.modified(configURL)
        guard modified != lastModified else { return }
        lastModified = modified
        let account = Self.account(at: configURL)
        // A missing id (signed out mid-write) is not a switch.
        guard let account, account != lastAccount else { return }
        lastAccount = account
        onSwitch()
    }

    nonisolated private static func modified(_ url: URL) -> Date? {
        (try? FileManager.default.attributesOfItem(atPath: url.path))?[.modificationDate] as? Date
    }

    nonisolated private static func account(at url: URL) -> String? {
        guard let data = try? Data(contentsOf: url),
              let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let id = object["lastKnownAccountUuid"] as? String, !id.isEmpty else { return nil }
        return id
    }
}
