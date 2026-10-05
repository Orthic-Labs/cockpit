import Foundation
import CoreGraphics

/// Pure M0 pill lifecycle logic. AppKit/Mach integration in main.swift calls these helpers.

// MARK: - CPU tick math

public struct CPUTicks: Equatable {
    public let user: UInt32
    public let system: UInt32
    public let nice: UInt32
    public let idle: UInt32

    public init(user: UInt32, system: UInt32, nice: UInt32, idle: UInt32) {
        self.user = user; self.system = system; self.nice = nice; self.idle = idle
    }
}

public enum CPUMath {
    /// Busy fraction in 0...1 between two cumulative samples. Counters are 32-bit and wrap, so
    /// deltas use wrapping subtraction. Returns nil (unavailable, never 0) when there is no
    /// previous sample or no ticks elapsed.
    public static func utilization(previous: CPUTicks?, current: CPUTicks) -> Double? {
        guard let previous else { return nil }
        let user = UInt64(current.user &- previous.user)
        let system = UInt64(current.system &- previous.system)
        let nice = UInt64(current.nice &- previous.nice)
        let idle = UInt64(current.idle &- previous.idle)
        let total = user + system + nice + idle
        guard total > 0 else { return nil }
        return min(max(Double(user + system + nice) / Double(total), 0), 1)
    }
}

// MARK: - Display-key diffing

public struct DisplayKeyDiff: Equatable {
    public let added: [String]
    public let removed: [String]
    public let kept: [String]

    /// `wanted` order is preserved (duplicates dropped); `removed` is sorted for determinism.
    public static func diff(existing: Set<String>, wanted: [String]) -> DisplayKeyDiff {
        var seen = Set<String>()
        var ordered: [String] = []
        for key in wanted where seen.insert(key).inserted { ordered.append(key) }
        return DisplayKeyDiff(
            added: ordered.filter { !existing.contains($0) },
            removed: existing.subtracting(seen).sorted(),
            kept: ordered.filter { existing.contains($0) }
        )
    }
}

// MARK: - Redraw dedupe

public struct RedrawGate: Equatable {
    private var last: String?
    public init() {}

    /// True only when `text` differs from the last accepted text.
    public mutating func shouldRedraw(_ text: String) -> Bool {
        if last == text { return false }
        last = text
        return true
    }

    public mutating func reset() { last = nil }
}

// MARK: - Display text

public enum PillFormat {
    /// Unavailable counters render as "--", never as 0%.
    public static func displayedFraction(_ fraction: Double?) -> Double? {
        guard let fraction, fraction.isFinite else { return nil }
        return Double(Int(min(max(fraction, 0), 1) * 100)) / 100
    }

    public static func label(_ name: String, fraction: Double?) -> String {
        guard let fraction, fraction.isFinite else { return "\(name) --" }
        return "\(name) \(Int(min(max(fraction, 0), 1) * 100))%"
    }

    public static func signature(_ rows: [(name: String, fraction: Double?)]) -> String {
        rows.map { label($0.name, fraction: $0.fraction) }.joined(separator: "\n")
    }
}

// MARK: - Placement

public enum PillPlacement {
    public static let rowHeight: CGFloat = 34
    public static let width: CGFloat = 132
    public static let margin: CGFloat = 8

    public static func height(diskCount: Int) -> CGFloat {
        CGFloat((max(2, max(0, diskCount)) + 2) * 34 + 26)
    }

    /// Right-edge, vertically centred in the visible frame; clamped so it never leaves it.
    public static func frame(visible: CGRect, diskCount: Int) -> CGRect {
        let h = min(height(diskCount: diskCount), max(visible.height, 0))
        let w = min(width, max(visible.width, 0))
        let x = max(visible.minX, visible.maxX - w - margin)
        let y = min(max(visible.midY - h / 2, visible.minY), visible.maxY - h)
        return CGRect(x: x, y: y, width: w, height: h)
    }
}

// MARK: - Failure transitions

public struct FailureTracker: Equatable {
    private var failing = Set<String>()
    public init() {}

    /// Reports only transitions so a persistent failure logs once, and recovery logs once.
    public mutating func update(failing now: Set<String>) -> (failed: [String], recovered: [String]) {
        let failed = now.subtracting(failing).sorted()
        let recovered = failing.subtracting(now).sorted()
        failing = now
        return (failed, recovered)
    }
}

// MARK: - Structured events

public struct StructuredEvent: Codable, Equatable {
    public let event: String
    public let level: String
    public let ts: String
    public let fields: [String: String]

    public init(event: String, level: String = "info", ts: String, fields: [String: String] = [:]) {
        self.event = event; self.level = level; self.ts = ts; self.fields = fields
    }

    public static func timestamp(_ date: Date) -> String {
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return formatter.string(from: date)
    }

    /// One JSON object, sorted keys, no trailing newline and never any embedded newline.
    public func jsonLine() -> String {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.sortedKeys]
        if let data = try? encoder.encode(self), let text = String(data: data, encoding: .utf8) {
            return text
        }
        return "{\"event\":\"event_encode_failed\",\"level\":\"error\"}"
    }
}
