import AppKit

/// Pulse fork: opens the hub (Tauri, `dev.orthic.pulse.hub`) from the notch.
///
/// The hub runs as a CHILD process of the notch (`Foundation.Process` on
/// `Contents/MacOS/pulse-hub`), not through Launch Services. macOS then treats
/// the notch as the responsible process, so Pulse's one Full Disk Access
/// entry covers the hub's scans. A running hub is told which section to show
/// with a Darwin notification; a new one gets it as a launch argument.
@MainActor
enum HubLauncher {
    static let bundleID = "dev.orthic.pulse.hub"
    private static var child: Process?
    private static var reaped = false
    private static let hubTail = "Helpers/Pulse.app/Contents/MacOS/pulse-hub"

    private struct ProcInfo { let pid: pid_t; let path: String; let start: Date; let parent: pid_t }

    /// Every process whose executable ends in the hub's bundle path, read
    /// straight from the kernel (no Launch Services: a child started with
    /// `Process` is not reliably listed there, and has no `launchDate`).
    private static func hubProcesses() -> [ProcInfo] {
        let bytes = proc_listpids(UInt32(PROC_ALL_PIDS), 0, nil, 0)
        guard bytes > 0 else { return [] }
        var pids = [pid_t](repeating: 0, count: Int(bytes) / MemoryLayout<pid_t>.size + 64)
        let got = proc_listpids(UInt32(PROC_ALL_PIDS), 0, &pids,
                                Int32(pids.count * MemoryLayout<pid_t>.size))
        guard got > 0 else { return [] }
        var found: [ProcInfo] = []
        for pid in pids.prefix(Int(got) / MemoryLayout<pid_t>.size) where pid > 0 {
            var buffer = [CChar](repeating: 0, count: 4096)
            guard proc_pidpath(pid, &buffer, UInt32(buffer.count)) > 0 else { continue }
            let path = String(cString: buffer)
            guard path.hasSuffix(hubTail) else { continue }
            var info = proc_bsdinfo()
            let size = Int32(MemoryLayout<proc_bsdinfo>.size)
            guard proc_pidinfo(pid, PROC_PIDTBSDINFO, 0, &info, size) == size else { continue }
            let start = Date(timeIntervalSince1970: TimeInterval(info.pbi_start_tvsec)
                + TimeInterval(info.pbi_start_tvusec) / 1_000_000)
            found.append(ProcInfo(pid: pid, path: path, start: start, parent: pid_t(info.pbi_ppid)))
        }
        return found
    }

    private static func alive(_ pid: pid_t) -> Bool {
        kill(pid, 0) == 0 || errno == EPERM
    }

    /// SIGTERM, up to `grace` seconds, then SIGKILL. Returns how many needed SIGKILL.
    @discardableResult
    private static func stop(_ pids: [pid_t], grace: TimeInterval = 3) -> Int {
        for pid in pids { kill(pid, SIGTERM) }
        let deadline = Date().addingTimeInterval(grace)
        while Date() < deadline, pids.contains(where: alive) {
            Thread.sleep(forTimeInterval: 0.05)
        }
        let remaining = pids.filter(alive)
        for pid in remaining { kill(pid, SIGKILL) }
        return remaining.count
    }

    /// A hub that was already running when this notch started belongs to an
    /// earlier Pulse (the hub is only ever the notch's child). It would keep
    /// the old code after an update, so it is stopped once, before this notch
    /// starts or addresses a hub of its own.
    private static func reapStaleHubs() { retireHubsFromEarlierLaunches() }

    /// A hub left behind by an earlier Pulse outlives that notch. Called once
    /// at launch, before this notch starts or addresses a hub. Found by
    /// executable path in the process table, at any install location, so it
    /// does not depend on Launch Services. A hub that started before this
    /// process, and is not our own child, gets SIGTERM, three seconds, then SIGKILL.
    static func retireHubsFromEarlierLaunches() {
        guard !reaped else { return }
        reaped = true
        let me = getpid()
        let launched = ownStart(me) ?? Date()
        let mine = child?.processIdentifier
        let stale = hubProcesses().filter {
            $0.pid != me && $0.pid != mine && $0.start < launched
        }
        guard !stale.isEmpty else {
            Log.usage.info("retired 0 hub(s) from an earlier launch")
            return
        }
        let forced = stop(stale.map(\.pid))
        Log.usage.info("retired \(stale.count, privacy: .public) hub(s) from an earlier launch, \(forced, privacy: .public) forced")
    }

    private static func ownStart(_ pid: pid_t) -> Date? {
        var info = proc_bsdinfo()
        let size = Int32(MemoryLayout<proc_bsdinfo>.size)
        guard proc_pidinfo(pid, PROC_PIDTBSDINFO, 0, &info, size) == size else { return nil }
        return Date(timeIntervalSince1970: TimeInterval(info.pbi_start_tvsec)
            + TimeInterval(info.pbi_start_tvusec) / 1_000_000)
    }

    static var location: URL? {
        let embedded = Bundle.main.bundleURL
            .appendingPathComponent("Contents/Helpers/Pulse.app", isDirectory: true)
        if FileManager.default.fileExists(atPath: embedded.path) { return embedded }
        return NSWorkspace.shared.urlForApplication(withBundleIdentifier: bundleID)
    }

    private static var hubRunning: Bool {
        if child?.isRunning == true { return true }
        return !NSRunningApplication.runningApplications(withBundleIdentifier: bundleID).isEmpty
    }

    /// Returns false when no hub is installed, so the caller can fall back.
    @discardableResult
    static func open(section: String) -> Bool {
        reapStaleHubs()
        if hubRunning {
            DarwinNotify.post("dev.orthic.pulse.hub.show.\(section)")
            return true
        }
        guard let url = location else { return false }
        let process = Process()
        process.executableURL = url.appendingPathComponent("Contents/MacOS/pulse-hub")
        process.arguments = ["--section", section]
        process.environment = ProcessInfo.processInfo.environment
        process.standardInput = FileHandle.nullDevice
        process.standardOutput = FileHandle.nullDevice
        process.standardError = FileHandle.nullDevice
        process.terminationHandler = { finished in
            Task { @MainActor in if child === finished { child = nil } }
        }
        do {
            try process.run()
            child = process
            return true
        } catch {
            // Last resort: Launch Services (the hub then carries its own TCC entry).
            let configuration = NSWorkspace.OpenConfiguration()
            configuration.arguments = ["--section", section]
            configuration.activates = true
            NSWorkspace.shared.openApplication(at: url, configuration: configuration)
            return true
        }
    }

    static var isRunning: Bool { hubRunning }

    private static var quitting = false
    private static var supervisor: Timer?
    private static var lastLaunch = Date.distantPast

    /// The hub is Pulse's daemon: while the notch runs, the hub runs. Starts it now
    /// and every few seconds starts it again if it is gone (30 s between attempts so
    /// a hub that dies at once does not spin). Stops with `terminate()`.
    static func supervise() {
        guard supervisor == nil else { return }
        ensureRunning()
        let timer = Timer(timeInterval: 5, repeats: true) { _ in
            Task { @MainActor in ensureRunning() }
        }
        timer.tolerance = 1
        RunLoop.main.add(timer, forMode: .common)
        supervisor = timer
    }

    private static func ensureRunning() {
        guard !quitting, !hubRunning else { return }
        guard Date().timeIntervalSince(lastLaunch) >= 30 else { return }
        lastLaunch = Date()
        launchInBackground()
    }

    /// Starts the hub with no window and no Dock icon, so nearby sharing works
    /// while nobody has the hub open. Does nothing when it is already running.
    static func launchInBackground() {
        reapStaleHubs()
        guard !hubRunning, let url = location else { return }
        let process = Process()
        process.executableURL = url.appendingPathComponent("Contents/MacOS/pulse-hub")
        process.arguments = ["--background"]
        process.environment = ProcessInfo.processInfo.environment
        process.standardInput = FileHandle.nullDevice
        process.standardOutput = FileHandle.nullDevice
        process.standardError = FileHandle.nullDevice
        process.terminationHandler = { finished in
            Task { @MainActor in if child === finished { child = nil } }
        }
        do {
            try process.run()
            child = process
        } catch {
            let configuration = NSWorkspace.OpenConfiguration()
            configuration.arguments = ["--background"]
            configuration.activates = false
            NSWorkspace.shared.openApplication(at: url, configuration: configuration)
        }
    }

    /// Stops every hub this process started (SIGTERM, then SIGKILL after 3 s).
    /// Called when the notch quits, including on SIGTERM from an installer.
    static func terminate() {
        quitting = true
        supervisor?.invalidate()
        supervisor = nil
        let me = getpid()
        var pids = hubProcesses().filter { $0.parent == me }.map(\.pid)
        if let process = child, process.isRunning,
           !pids.contains(process.processIdentifier) { pids.append(process.processIdentifier) }
        if !pids.isEmpty { stop(pids) }
        // A hub started another way (Launch Services fallback) must not outlive
        // the notch either.
        for app in NSRunningApplication.runningApplications(withBundleIdentifier: bundleID)
        where !app.isTerminated && !pids.contains(app.processIdentifier) {
            app.terminate()
        }
    }
}
