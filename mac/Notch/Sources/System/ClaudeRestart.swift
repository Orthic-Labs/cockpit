import AppKit
import SwiftUI

/// Pulse fork: the Claude card's "Restart Claude and sync chats" button. The
/// owner presses it after signing in to a new account; nothing is detected.
/// Quit Claude politely (never a force-kill, up to 20 s), run the bundled
/// `pulse claude sync --apply --json` over every account, reopen Claude.
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
        phase = .failed(error)
    }

    /// nil on success, else the message the button's tooltip shows. Only the
    /// CLI runs off the main thread; the waits are async sleeps.
    private static func sequence(cli: URL) async -> String? {
        let claudeURL = NSWorkspace.shared.urlForApplication(withBundleIdentifier: bundleID)
        let running = NSRunningApplication.runningApplications(withBundleIdentifier: bundleID)
        if !running.isEmpty {
            for app in running { _ = app.terminate() }
            var closed = false
            for _ in 0..<80 {
                try? await Task.sleep(nanoseconds: 250_000_000)
                if NSRunningApplication.runningApplications(withBundleIdentifier: bundleID).isEmpty {
                    closed = true
                    break
                }
            }
            if !closed { return "Claude is still open" }
        }
        let failure = await Task.detached(priority: .userInitiated) { runCLI(cli) }.value
        // Claude comes back whatever the sync said.
        if let claudeURL {
            NSWorkspace.shared.openApplication(at: claudeURL,
                                               configuration: NSWorkspace.OpenConfiguration()) { _, _ in }
        } else if failure == nil {
            return "Synced, but Claude could not be found to reopen"
        }
        return failure
    }

    nonisolated private static func runCLI(_ cli: URL) -> String? {
        guard FileManager.default.isExecutableFile(atPath: cli.path) else {
            return "The Pulse command line tool is missing"
        }
        let process = Process()
        process.executableURL = cli
        process.arguments = ["claude", "sync", "--apply", "--json"]
        let out = Pipe()
        process.standardOutput = out
        process.standardError = out
        process.standardInput = FileHandle.nullDevice
        do { try process.run() } catch { return error.localizedDescription }
        let data = out.fileHandleForReading.readDataToEndOfFile()
        process.waitUntilExit()
        guard process.terminationStatus != 0 else { return nil }
        let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any]
        return (object?["error"] as? String) ?? "Claude sync failed"
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
