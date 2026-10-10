import AppKit
import SwiftUI

/// Pulse fork: the Claude card's "Restart Claude and sync chats" button. The
/// owner presses it after signing in to a new account. `ClaudeAccountWatcher`
/// then notices the account Claude opens with and refreshes usage at once.
/// Quit Claude politely (up to 20 s), force-quit what is left (up to 10 s
/// more), run the bundled `pulse claude sync --apply --json` over every
/// account, retrying while Claude's helper processes are still winding down,
/// then reopen Claude and, detached, the chats that were running (`remember`
/// before the quit, `reopen` after). A failure stays on the button (red mark, message in
/// its tooltip) until the next press, and is logged.
@MainActor
final class ClaudeRestart: ObservableObject {
    enum Phase: Equatable { case idle, running, done, failed(String) }

    static let shared = ClaudeRestart()
    static let bundleID = "com.anthropic.claudefordesktop"

    @Published private(set) var phase: Phase = .idle

    func run() {
        guard phase != .running else { return }
        phase = .running
        let bundled = Bundle.main.bundleURL.appendingPathComponent("Contents/Helpers/pulse")
        Task { @MainActor in
            let outcome = await Self.sequence(cli: bundled)
            self.finish(outcome)
        }
    }

    private func finish(_ error: String?) {
        guard let error else {
            phase = .done
            Task { @MainActor in
                try? await Task.sleep(nanoseconds: 2_000_000_000)
                if self.phase == .done { self.phase = .idle }
            }
            return
        }
        Log.usage.error("claude restart: \(error, privacy: .public)")
        phase = .failed(error)
    }

    /// nil on success, else the message the button's tooltip shows. Only the
    /// CLI runs off the main thread; the waits are async sleeps.
    private static func sequence(cli: URL) async -> String? {
        let claudeURL = NSWorkspace.shared.urlForApplication(withBundleIdentifier: bundleID)
        // Note the running chats once (up to 5 s) so `reopen` can bring them back.
        _ = await Task.detached(priority: .userInitiated) {
            ClaudePulseCLI.run(["claude", "remember", "--json"], timeout: 5)
        }.value
        if let error = await quitClaude() { return error }
        // Claude's helper processes (crashpad, GPU) can outlive the app by a
        // few seconds, and the CLI refuses while any of them runs: retry.
        var failure: String?
        for attempt in 0..<12 {
            let result = await Task.detached(priority: .userInitiated) { runCLI(cli) }.value
            failure = result.message
            if result.code != "claude_running" { break }
            Log.usage.info("claude restart: helpers still running, retry \(attempt + 1, privacy: .public)")
            try? await Task.sleep(nanoseconds: 1_000_000_000)
        }
        // Claude comes back whatever the sync said.
        if let claudeURL {
            NSWorkspace.shared.openApplication(at: claudeURL,
                                               configuration: NSWorkspace.OpenConfiguration()) { _, _ in }
            NotificationCenter.default.post(name: ClaudeAccountWatcher.claudeReopened, object: nil)
            // Detached: it opens the chats one at a time and can take minutes.
            ClaudePulseCLI.launchDetached(["claude", "reopen", "--json"])
        } else if failure == nil {
            return "Synced, but Claude could not be found to reopen"
        }
        return failure
    }

    /// Polite quit, 20 s; then a force quit, 10 s. Nil when Claude is gone.
    private static func quitClaude() async -> String? {
        var running = NSRunningApplication.runningApplications(withBundleIdentifier: bundleID)
        guard !running.isEmpty else { return nil }
        for app in running { _ = app.terminate() }
        if await gone(within: 80) { return nil }
        running = NSRunningApplication.runningApplications(withBundleIdentifier: bundleID)
        Log.usage.info("claude restart: Claude did not quit in 20 s, force quitting")
        for app in running { _ = app.forceTerminate() }
        if await gone(within: 40) { return nil }
        return "Claude is still open"
    }

    /// Polls every 250 ms up to `ticks` times for the app to be gone.
    private static func gone(within ticks: Int) async -> Bool {
        for _ in 0..<ticks {
            try? await Task.sleep(nanoseconds: 250_000_000)
            if NSRunningApplication.runningApplications(withBundleIdentifier: bundleID).isEmpty {
                return true
            }
        }
        return false
    }

    struct CLIResult { let message: String?; let code: String? }

    nonisolated private static func runCLI(_ cli: URL) -> CLIResult {
        guard FileManager.default.isExecutableFile(atPath: cli.path) else {
            return CLIResult(message: "The Pulse command line tool is missing", code: nil)
        }
        let process = Process()
        process.executableURL = cli
        process.arguments = ["claude", "sync", "--apply", "--json"]
        let out = Pipe()
        process.standardOutput = out
        process.standardError = out
        process.standardInput = FileHandle.nullDevice
        do { try process.run() } catch {
            return CLIResult(message: error.localizedDescription, code: nil)
        }
        let data = out.fileHandleForReading.readDataToEndOfFile()
        process.waitUntilExit()
        guard process.terminationStatus != 0 else { return CLIResult(message: nil, code: nil) }
        let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any]
        return CLIResult(message: (object?["error"] as? String) ?? "Claude sync failed",
                         code: object?["code"] as? String)
    }
}

/// The small round button on the Claude card's header.
struct ClaudeRestartButton: View {
    @ObservedObject private var restart = ClaudeRestart.shared
    private static let size = Design.px(66)

    private var helpText: String {
        if case .failed(let message) = restart.phase { return message }
        return L10n.t("Restart Claude and sync chats")
    }

    var body: some View {
        Button { restart.run() } label: {
            ZStack {
                switch restart.phase {
                case .running:
                    CardProgress(linear: false)
                case .done:
                    Image(systemName: "checkmark")
                        .font(.system(size: 12, weight: .semibold))
                        .foregroundStyle(Palette.textPrimary)
                case .failed:
                    Image(systemName: "exclamationmark")
                        .font(.system(size: 12, weight: .bold))
                        .foregroundStyle(Color.red)
                case .idle:
                    Image(systemName: "arrow.clockwise")
                        .font(.system(size: 12, weight: .semibold))
                        .foregroundStyle(Palette.textPrimary)
                }
            }
            .frame(width: Self.size, height: Self.size)
            .background(Circle().fill(Palette.textPrimary.opacity(0.14)))
            .contentShape(Circle())
        }
        .buttonStyle(CardButtonStyle())
        .disabled(restart.phase == .running)
        .help(helpText)
        .accessibilityLabel(L10n.t("Restart Claude and sync chats"))
    }
}
