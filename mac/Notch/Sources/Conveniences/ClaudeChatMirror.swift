import CoreServices
import Foundation

/// Pulse fork: keeps every Claude Desktop account's chat list complete.
///
/// `pulse claude mirror` copies the signed-in account's chat records into the
/// other accounts' folders. It never writes the signed-in account's folder, so
/// it is safe while Claude runs. This runs it when something changes under
/// `claude-code-sessions/<signed-in account>` (FSEvents, 3 s latency, no
/// timer), once at start, and after the signed-in account changes. A second
/// watcher on `~/.claude/sessions` runs `pulse claude remember` so the chats
/// that were running can be reopened after a restart or an account switch.
/// Rules: docs/claude-account-switch.md.
@MainActor
final class ClaudeChatMirror {
    private var stream: FolderEventStream?
    private var observer: NSObjectProtocol?
    private var watched: String?
    private var running = false
    private var dirty = false

    func start() {
        guard observer == nil else { return }
        observer = NotificationCenter.default.addObserver(
            forName: ClaudeAccountWatcher.accountChanged, object: nil, queue: .main
        ) { [weak self] note in
            let afterRestart = (note.userInfo?["afterRestart"] as? Bool) ?? false
            MainActor.assumeIsolated { self?.accountChanged(afterRestart: afterRestart) }
        }
        rewatch()
        Task { await pass() }
    }

    func stop() {
        stream?.stop()
        stream = nil
        if let observer { NotificationCenter.default.removeObserver(observer) }
        observer = nil
    }

    /// Point the stream at the signed-in account's folder.
    private func rewatch() {
        let account = ClaudeAccountWatcher.desktopAccountUUID()
        guard account != watched || stream == nil else { return }
        stream?.stop()
        stream = nil
        watched = account
        guard let account else { return }
        let folder = ClaudePulseCLI.claudeSupport
            .appendingPathComponent("claude-code-sessions/\(account)").path
        stream = FolderEventStream(path: folder, latency: 3) { [weak self] in
            Task { await self?.pass() }
        }
    }

    /// Desktop rewrites the account it just left a moment after a switch, so
    /// wait before mirroring. A switch the restart button did not cause also
    /// reopens the chats that were running (the button does that itself).
    private func accountChanged(afterRestart: Bool) {
        Task { @MainActor in
            try? await Task.sleep(nanoseconds: 5_000_000_000)
            rewatch()
            while running { try? await Task.sleep(nanoseconds: 500_000_000) }
            await pass()
            if !afterRestart { ClaudePulseCLI.launchDetached(["claude", "reopen", "--json"]) }
        }
    }

    /// One mirror run at a time; a change during a run schedules one more.
    private func pass() async {
        if running { dirty = true; return }
        running = true
        repeat {
            dirty = false
            if let copied = await Task.detached(priority: .utility, operation: {
                ClaudePulseCLI.mirrorOnce()
            }).value, copied > 0 {
                Log.usage.info("claude mirror: copied \(copied, privacy: .public) chat records")
            }
        } while dirty
        running = false
    }
}

/// Pulse fork: runs `pulse claude remember --json` when `~/.claude/sessions`
/// changes (3 s latency), one run at a time.
@MainActor
final class ClaudeChatRemember {
    private var stream: FolderEventStream?
    private var running = false
    private var dirty = false

    func start() {
        guard stream == nil else { return }
        let folder = FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent(".claude/sessions").path
        stream = FolderEventStream(path: folder, latency: 3) { [weak self] in
            Task { await self?.run() }
        }
    }

    func stop() {
        stream?.stop()
        stream = nil
    }

    private func run() async {
        if running { dirty = true; return }
        running = true
        repeat {
            dirty = false
            _ = await Task.detached(priority: .utility, operation: {
                ClaudePulseCLI.run(["claude", "remember", "--json"], timeout: 30)
            }).value
        } while dirty
        running = false
    }
}

/// The bundled command line tool, run as a child process.
enum ClaudePulseCLI {
    static var claudeSupport: URL {
        FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent("Library/Application Support/Claude")
    }

    static var url: URL {
        Bundle.main.bundleURL.appendingPathComponent("Contents/Helpers/pulse")
    }

    /// Runs the tool and returns its output; nil if it is missing, failed to
    /// start or ran past `timeout` seconds (then it is terminated).
    nonisolated static func run(_ arguments: [String], timeout: TimeInterval) -> Data? {
        guard FileManager.default.isExecutableFile(atPath: url.path) else { return nil }
        let process = Process()
        process.executableURL = url
        process.arguments = arguments
        let out = Pipe()
        process.standardOutput = out
        process.standardError = FileHandle.nullDevice
        process.standardInput = FileHandle.nullDevice
        do { try process.run() } catch { return nil }
        DispatchQueue.global().asyncAfter(deadline: .now() + timeout) {
            if process.isRunning { process.terminate() }
        }
        let data = out.fileHandleForReading.readDataToEndOfFile()
        process.waitUntilExit()
        return process.terminationStatus == 0 ? data : nil
    }

    /// Records copied by one `claude mirror` pass; nil when it did not run.
    nonisolated static func mirrorOnce() -> Int? {
        guard let data = run(["claude", "mirror", "--json"], timeout: 120),
              let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let mirror = object["mirror"] as? [String: Any] else { return nil }
        return mirror["copied"] as? Int
    }

    /// Starts the tool and does not wait for it (`reopen` can take minutes).
    static func launchDetached(_ arguments: [String]) {
        guard FileManager.default.isExecutableFile(atPath: url.path) else { return }
        let process = Process()
        process.executableURL = url
        process.arguments = arguments
        process.standardOutput = FileHandle.nullDevice
        process.standardError = FileHandle.nullDevice
        process.standardInput = FileHandle.nullDevice
        do { try process.run() } catch {
            Log.usage.error("claude \(arguments[1], privacy: .public): \(error.localizedDescription, privacy: .public)")
        }
    }
}

/// FSEvents on one folder, recursive, file-level events, main-queue callback.
/// Does nothing when the folder does not exist.
@MainActor
final class FolderEventStream {
    private var stream: FSEventStreamRef?
    private let handler: @MainActor () -> Void

    init?(path: String, latency: CFTimeInterval, handler: @escaping @MainActor () -> Void) {
        var isDirectory: ObjCBool = false
        guard FileManager.default.fileExists(atPath: path, isDirectory: &isDirectory),
              isDirectory.boolValue else { return nil }
        self.handler = handler
        var context = FSEventStreamContext(version: 0, info: Unmanaged.passUnretained(self).toOpaque(),
                                           retain: nil, release: nil, copyDescription: nil)
        let callback: FSEventStreamCallback = { _, info, _, _, _, _ in
            guard let info else { return }
            let owner = Unmanaged<FolderEventStream>.fromOpaque(info).takeUnretainedValue()
            MainActor.assumeIsolated { owner.handler() }
        }
        let flags = FSEventStreamCreateFlags(kFSEventStreamCreateFlagFileEvents
                                             | kFSEventStreamCreateFlagUseCFTypes)
        guard let stream = FSEventStreamCreate(kCFAllocatorDefault, callback, &context,
                                               [path] as CFArray,
                                               FSEventStreamEventId(kFSEventStreamEventIdSinceNow),
                                               latency, flags) else { return nil }
        self.stream = stream
        FSEventStreamSetDispatchQueue(stream, DispatchQueue.main)
        FSEventStreamStart(stream)
    }

    func stop() {
        guard let stream else { return }
        FSEventStreamStop(stream)
        FSEventStreamInvalidate(stream)
        FSEventStreamRelease(stream)
        self.stream = nil
    }
}
