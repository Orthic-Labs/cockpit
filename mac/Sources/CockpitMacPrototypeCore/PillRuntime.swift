import Foundation

/// Single owner of the pill's settings and its per-user instance lock. It never starts or
/// connects to the worker. Main-thread use only.
public final class PillRuntime {
    public typealias Emit = (_ event: String, _ level: String, _ fields: [String: String]) -> Void

    public enum StartResult: Equatable {
        case started
        case alreadyRunning
        case failed(String)
    }

    public let store: PillSettingsStore
    public private(set) var settings: PillSettings = .defaults
    public private(set) var loadState: SettingsLoadState = .missing
    private var baseline: PillSettings = .defaults
    private var lock: PillInstanceLock?
    private var isShutDown = false
    private let emit: Emit

    public init(directory: URL = PillSettingsStore.defaultDirectory(), emit: @escaping Emit) {
        self.store = PillSettingsStore(directory: directory)
        self.emit = emit
    }

    public var holdsInstanceLock: Bool { lock?.isHeld ?? false }

    /// Acquires the instance lock, then loads settings. On `.alreadyRunning` the caller exits 0.
    /// Re-entrant calls are refused: a running instance never takes a second owner role, and a
    /// shut-down instance cannot restart (create a new PillRuntime instead).
    public func start() -> StartResult {
        if isShutDown { return .failed("already_shut_down") }
        if lock?.isHeld == true { return .alreadyRunning }
        switch store.ensureDirectory() {
        case .created: emit("state_directory_created", "info", [:])
        case .existing: break
        case .refused(let failure):
            emit("instance_lock_failed", "error", ["reason": "directory_" + failure.code])
            return .failed("directory_" + failure.code)
        }
        switch PillInstanceLock.acquire(directory: store.directory, expectedUID: store.expectedUID) {
        case .alreadyHeld:
            emit("instance_already_running", "info", [:])
            return .alreadyRunning
        case .failed(let reason):
            emit("instance_lock_failed", "error", ["reason": reason])
            return .failed(reason)
        case .acquired(let acquired):
            lock = acquired
            emit("instance_lock_acquired", "info", [:])
        }
        let result = store.load()
        settings = result.settings
        baseline = result.settings
        loadState = result.state
        switch result.state {
        case .missing:
            emit("settings_missing_using_defaults", "info", [:])
        case .loaded:
            emit("settings_loaded", "info", [
                "visible": String(settings.visible), "cadence_seconds": String(settings.cadenceSeconds),
                "monitors": String(settings.monitors.count)
            ])
        case .protected(let failure):
            emit("settings_recovery_using_defaults", "warn", ["reason": failure.code, "file_preserved": "true"])
        }
        return .started
    }

    // MARK: Settings access

    public func monitorSetting(for key: String) -> MonitorSetting { settings.monitor(key) }

    public var cadenceSeconds: Int { settings.cadenceSeconds }

    public func setVisible(_ visible: Bool) { settings.visible = visible }

    public func setCadence(_ seconds: Int) { settings.cadenceSeconds = PillSettings.clampCadence(seconds) }

    /// Outcome of a settings mutation; refusals are explicit, never silent truncation.
    public enum MutationResult: Equatable {
        case applied
        case refused(String)
    }

    /// Enforces `PillSettings.maxMonitors` and the UTF-8-byte key bound via the model's
    /// `setMonitor`: inserting a new key at the cap is refused (updating an existing key
    /// is still allowed), matching the strict model's bounds.
    @discardableResult
    public func setMonitor(_ key: String, _ value: MonitorSetting) -> MutationResult {
        guard PillSettings.isValidMonitorKey(key) else {
            emit("settings_mutation_refused", "warn", ["field": "monitor", "reason": "invalid_monitor_key"])
            return .refused("invalid_monitor_key")
        }
        if value == .defaults && settings.monitors[key] == nil { return .applied }
        if !settings.setMonitor(key, value) {
            emit("settings_mutation_refused", "warn",
                 ["field": "monitor", "reason": "monitor_limit_\(PillSettings.maxMonitors)"])
            return .refused("monitor_limit_\(PillSettings.maxMonitors)")
        }
        return .applied
    }

    // MARK: Shutdown

    /// Runs `ShutdownPlan.steps` in order. Idempotent. Hooks are the caller's UI/timer teardown.
    public func shutdown(reason: String, stopTimer: () -> Void, closePanels: () -> Void) {
        guard !isShutDown else { return }
        isShutDown = true
        for step in ShutdownPlan.steps {
            switch step {
            case .stopTimer: stopTimer()
            case .closePanels: closePanels()
            case .persistSettings: persistIfChanged()
            case .releaseLock:
                if let lock {
                    lock.release()
                    self.lock = nil
                    emit("instance_lock_released", "info", [:])
                }
            case .emitShutdown: emit("shutdown", "info", ["reason": reason])
            }
        }
    }

    private func persistIfChanged() {
        switch SettingsWritePolicy.decide(state: loadState, current: settings, baseline: baseline,
                                          holdsInstanceLock: holdsInstanceLock) {
        case .skipUnchanged:
            break
        case .refuse(let reason):
            emit("settings_write_blocked", "warn", ["reason": reason])
        case .write:
            switch store.save(settings) {
            case .success:
                baseline = settings
                loadState = .loaded
                emit("settings_persisted", "info", [:])
            case .failure(let error):
                emit("settings_persist_failed", "error", ["reason": String(describing: error)])
            }
        }
    }
}
