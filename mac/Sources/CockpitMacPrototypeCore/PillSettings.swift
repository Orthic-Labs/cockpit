import Foundation
import CoreGraphics

// Pure pill settings logic (schema v1, shared with the Windows pill). No file IO here.

public enum PillAnchor: String, Codable, Equatable, CaseIterable {
    case topLeft = "top-left"
    case topRight = "top-right"
    case bottomLeft = "bottom-left"
    case bottomRight = "bottom-right"

    public static let defaultAnchor: PillAnchor = .topRight
}

public struct MonitorSetting: Equatable {
    public var enabled: Bool
    public var anchor: PillAnchor

    public init(enabled: Bool = true, anchor: PillAnchor = .defaultAnchor) {
        self.enabled = enabled
        self.anchor = anchor
    }

    public static let defaults = MonitorSetting()
}

public struct PillSettings: Equatable {
    public static let schemaVersion = 1
    public static let cadenceRange: ClosedRange<Int> = 2...10
    public static let defaultCadence = 2
    public static let maxFileBytes = 64 * 1024
    public static let maxMonitors = 64
    /// Maximum length of a settings dictionary key (monitor ids, schema keys) measured in
    /// UTF-8 BYTES (`key.utf8.count`), not Characters. Shared bound: the store lane and any
    /// schema/string key validation must use this constant and `String.utf8.count`.
    public static let maxKeyUTF8Bytes = 256
    /// Legacy Character-count limit. Retained only so concurrent callers keep compiling;
    /// new code must use `isValidMonitorKey` / `maxKeyUTF8Bytes` (UTF-8 bytes).
    @available(*, deprecated, message: "Use PillSettings.isValidMonitorKey / maxKeyUTF8Bytes (UTF-8 bytes)")
    public static let maxMonitorKeyLength = 128

    public var visible: Bool
    public var cadenceSeconds: Int
    public var monitors: [String: MonitorSetting]

    public init(visible: Bool = true, cadenceSeconds: Int = PillSettings.defaultCadence,
                monitors: [String: MonitorSetting] = [:]) {
        self.visible = visible
        self.cadenceSeconds = PillSettings.clampCadence(cadenceSeconds)
        self.monitors = monitors
    }

    public static let defaults = PillSettings()

    public static func clampCadence(_ value: Int) -> Int {
        min(max(value, cadenceRange.lowerBound), cadenceRange.upperBound)
    }

    /// A monitor key is valid iff non-empty and at most `maxKeyUTF8Bytes` UTF-8 bytes long.
    public static func isValidMonitorKey(_ key: String) -> Bool {
        !key.isEmpty && key.utf8.count <= maxKeyUTF8Bytes
    }

    /// Missing monitor entries use defaults (enabled, top-right).
    public func monitor(_ key: String) -> MonitorSetting { monitors[key] ?? .defaults }

    /// Adds or replaces a monitor entry, enforcing the model bounds.
    /// Returns false — leaving `self` unchanged — if the key is invalid or the table
    /// is already at `maxMonitors` entries (replacing an existing key is always allowed).
    @discardableResult
    public mutating func setMonitor(_ key: String, _ setting: MonitorSetting) -> Bool {
        guard PillSettings.isValidMonitorKey(key) else { return false }
        if monitors[key] == nil, monitors.count >= PillSettings.maxMonitors { return false }
        monitors[key] = setting
        return true
    }

    /// True when `monitors` satisfies every serialization bound (count cap, key validity).
    /// `PillSettingsCodec.encode` refuses settings for which this is false rather than
    /// silently dropping or truncating entries.
    public var hasSerializableMonitors: Bool {
        monitors.count <= PillSettings.maxMonitors && monitors.keys.allSatisfy(PillSettings.isValidMonitorKey)
    }
}

/// Why the settings file could not be used. Any of these means: use defaults, emit an event,
/// and never overwrite the file.
public enum SettingsFailure: Error, Equatable {
    case oversized
    case malformed
    case unknownSchema(Int)
    case symlink
    case ioError(Int32)

    public var code: String {
        switch self {
        case .oversized: return "oversized"
        case .malformed: return "malformed"
        case .unknownSchema(let v): return "unknown_schema_\(v)"
        case .symlink: return "symlink"
        case .ioError(let e): return "io_error_\(e)"
        }
    }
}

public enum PillSettingsCodec {
    /// STRICT decode: the whole payload is rejected unless every present field is well-formed.
    /// Wrong types, out-of-range cadence, invalid monitor keys/entries, a non-dictionary
    /// `monitors` value, more than `maxMonitors` entries, or an unknown schema version all
    /// fail the entire decode — never partial settings, never sanitized defaults. Unknown
    /// keys are still ignored (forward compatibility), but known fields must be exact.
    /// Decode is pure: it performs no mutation or IO, so a failure leaves the on-disk
    /// original untouched (the store relies on this to preserve the file byte-for-byte).
    public static func decode(_ data: Data) -> Result<PillSettings, SettingsFailure> {
        guard data.count <= PillSettings.maxFileBytes else { return .failure(.oversized) }
        guard let object = try? JSONSerialization.jsonObject(with: data, options: []),
              let root = object as? [String: Any] else { return .failure(.malformed) }
        guard let versionValue = root["schema_version"], let version = integer(versionValue) else {
            return .failure(.malformed)
        }
        guard version == PillSettings.schemaVersion else { return .failure(.unknownSchema(version)) }

        var settings = PillSettings.defaults
        if let raw = root["visible"] {
            guard let visible = bool(raw) else { return .failure(.malformed) }
            settings.visible = visible
        }
        if let raw = root["cadence_seconds"] {
            guard let cadence = integer(raw), PillSettings.cadenceRange.contains(cadence) else {
                return .failure(.malformed)
            }
            settings.cadenceSeconds = cadence
        }
        if let raw = root["monitors"] {
            guard let monitors = raw as? [String: Any],
                  monitors.count <= PillSettings.maxMonitors else { return .failure(.malformed) }
            for (key, value) in monitors {
                guard PillSettings.isValidMonitorKey(key),
                      let entry = value as? [String: Any] else { return .failure(.malformed) }
                var monitor = MonitorSetting.defaults
                if let raw = entry["enabled"] {
                    guard let enabled = bool(raw) else { return .failure(.malformed) }
                    monitor.enabled = enabled
                }
                if let raw = entry["anchor"] {
                    guard let name = raw as? String, let anchor = PillAnchor(rawValue: name) else {
                        return .failure(.malformed)
                    }
                    monitor.anchor = anchor
                }
                settings.monitors[key] = monitor
            }
        }
        return .success(settings)
    }

    /// Deterministic (sorted keys) encoding. Never emits filtered or truncated data: it
    /// returns nil if `settings` violates a bound (monitor count cap, invalid keys) or if
    /// the result would exceed the size bound.
    public static func encode(_ settings: PillSettings) -> Data? {
        guard settings.hasSerializableMonitors else { return nil }
        var monitors: [String: [String: Any]] = [:]
        for (key, value) in settings.monitors.sorted(by: { $0.key < $1.key }) {
            monitors[key] = ["enabled": value.enabled, "anchor": value.anchor.rawValue]
        }
        let root: [String: Any] = [
            "schema_version": PillSettings.schemaVersion,
            "visible": settings.visible,
            "cadence_seconds": PillSettings.clampCadence(settings.cadenceSeconds),
            "monitors": monitors
        ]
        guard let data = try? JSONSerialization.data(withJSONObject: root, options: [.sortedKeys]),
              data.count <= PillSettings.maxFileBytes else { return nil }
        return data
    }

    private static func isBoolean(_ number: NSNumber) -> Bool {
        CFGetTypeID(number) == CFBooleanGetTypeID()
    }

    private static func bool(_ value: Any?) -> Bool? {
        guard let number = value as? NSNumber, isBoolean(number) else { return nil }
        return number.boolValue
    }

    /// Whole JSON numbers only (not booleans, not fractional). No clamping: the caller
    /// rejects out-of-range values so malformed input never becomes sanitized output.
    private static func integer(_ value: Any) -> Int? {
        guard let number = value as? NSNumber, !isBoolean(number) else { return nil }
        let d = number.doubleValue
        guard d.isFinite, d == d.rounded(), abs(d) < 9e15 else { return nil }
        return Int(d)
    }
}

// MARK: - Write decision table

public enum SettingsLoadState: Equatable {
    case missing
    case loaded
    /// File exists but must not be overwritten (bad/unknown/oversized/symlinked).
    case protected(SettingsFailure)
}

public enum WriteDecision: Equatable {
    case write
    case skipUnchanged
    case refuse(String)
}

public enum SettingsWritePolicy {
    /// - Unchanged settings are never written (including defaults for a missing file).
    /// - A protected file is never overwritten.
    /// - Only the lock holder may write.
    public static func decide(state: SettingsLoadState, current: PillSettings, baseline: PillSettings,
                              holdsInstanceLock: Bool) -> WriteDecision {
        if current == baseline { return .skipUnchanged }
        guard holdsInstanceLock else { return .refuse("instance_lock_not_held") }
        switch state {
        case .protected(let failure): return .refuse("settings_protected_\(failure.code)")
        case .missing, .loaded: return .write
        }
    }
}

// MARK: - Anchored placement

public enum AnchoredPlacement {
    /// Frame in AppKit coordinates (origin bottom-left) inside `visible`, clamped to it.
    public static func frame(visible: CGRect, diskCount: Int, anchor: PillAnchor) -> CGRect {
        let h = min(PillPlacement.height(diskCount: diskCount), max(visible.height, 0))
        let w = min(PillPlacement.width, max(visible.width, 0))
        let m = PillPlacement.margin
        let x: CGFloat
        switch anchor {
        case .topLeft, .bottomLeft: x = visible.minX + m
        case .topRight, .bottomRight: x = visible.maxX - w - m
        }
        let y: CGFloat
        switch anchor {
        case .topLeft, .topRight: y = visible.maxY - h - m
        case .bottomLeft, .bottomRight: y = visible.minY + m
        }
        return CGRect(x: min(max(x, visible.minX), max(visible.maxX - w, visible.minX)),
                      y: min(max(y, visible.minY), max(visible.maxY - h, visible.minY)),
                      width: w, height: h)
    }
}

// MARK: - Visibility & cadence

public enum PillVisibility {
    /// Fullscreen suppression is only applied on top of user intent; it never overrides "hidden".
    public static func shouldShow(settingsVisible: Bool, monitorEnabled: Bool, fullscreenSuppressed: Bool) -> Bool {
        settingsVisible && monitorEnabled && !fullscreenSuppressed
    }
}

public enum PillSchedule {
    public static let hiddenPollSeconds: TimeInterval = 10

    public static func interval(allHidden: Bool, cadenceSeconds: Int) -> TimeInterval {
        allHidden ? hiddenPollSeconds : TimeInterval(PillSettings.clampCadence(cadenceSeconds))
    }
}

// MARK: - Shutdown ordering

public enum ShutdownStep: String, Equatable, CaseIterable {
    case stopTimer = "stop_timer"
    case closePanels = "close_panels"
    case persistSettings = "persist_settings"
    case releaseLock = "release_lock"
    case emitShutdown = "emit_shutdown"
}

public enum ShutdownPlan {
    public static let steps: [ShutdownStep] = [.stopTimer, .closePanels, .persistSettings, .releaseLock, .emitShutdown]
}
