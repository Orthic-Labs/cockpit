import Foundation

/// Read-only projection of durable activity journals for the native dashboard.
///
/// This type does not observe the machine or persist anything.  It only joins
/// already durable compression, cleanup, & CLI activity payloads.
public enum NativeActivityProjection {
    private static let timelineLimit = 512
    private static let inputLimit = 2_048
    private static let weekSeconds: Int64 = 7 * 24 * 60 * 60
    private static let monthSeconds: Int64 = 30 * 24 * 60 * 60

    private struct Event {
        let id: String
        let action: String
        let operation: String
        var status: String
        let source: String
        let occurredAt: Int64?
        let logicalBytes: UInt64?
        let allocationBytes: UInt64?
        let movedBytes: UInt64?
        let sourceBytes: UInt64?
        let outputBytes: UInt64?
        let savedBytes: UInt64?
        let logicalDeltaBytes: Int64?
        let observedVolumeDeltaBytes: Int64?
        let bytesState: String
        let rawKind: String?
        var path: String?

        var timeState: String { occurredAt == nil ? "unknown" : "known" }

        func dictionary() -> [String: Any] {
            var value: [String: Any] = [
                "id": id,
                "action": action,
                "operation": operation,
                "kind": rawKind ?? operation,
                "status": status,
                "source": source,
                "path": path as Any? ?? NSNull(),
                "occurred_at": occurredAt.map { $0 } ?? NSNull(),
                "time_state": timeState,
                "bytes_state": bytesState,
                "logical_bytes": number(logicalBytes),
                "allocation_bytes": number(allocationBytes),
                "moved_bytes": number(movedBytes),
                "source_bytes": number(sourceBytes),
                "output_bytes": number(outputBytes),
                "measured_saved_bytes": number(savedBytes),
                "logical_delta_bytes": signedNumber(logicalDeltaBytes),
                "observed_volume_delta_bytes": signedNumber(observedVolumeDeltaBytes)
            ]
            return value
        }
    }

    private struct Bucket {
        var events = 0
        var knownTimeEvents = 0
        var unknownTimeEvents = 0
        var partialByteEvents = 0
        var unknownByteEvents = 0
        var logicalBytes: UInt64 = 0
        var allocationBytes: UInt64 = 0
        var movedBytes: UInt64 = 0
        var sourceBytes: UInt64 = 0
        var outputBytes: UInt64 = 0
        var savedBytes: UInt64 = 0
        var logicalDeltaBytes: Int64 = 0
        var observedVolumeDeltaBytes: Int64 = 0
        var logicalCount = 0
        var allocationCount = 0
        var movedCount = 0
        var sourceCount = 0
        var outputCount = 0
        var savedCount = 0
        var logicalDeltaCount = 0
        var volumeDeltaCount = 0
        var overflowed = false

        mutating func add(_ event: Event) {
            events += 1
            if event.occurredAt == nil { unknownTimeEvents += 1 } else { knownTimeEvents += 1 }
            switch event.bytesState {
            case "partial": partialByteEvents += 1
            case "unknown": unknownByteEvents += 1
            default: break
            }
            overflowed = Self.add(event.logicalBytes, to: &logicalBytes, count: &logicalCount) || overflowed
            overflowed = Self.add(event.allocationBytes, to: &allocationBytes, count: &allocationCount) || overflowed
            overflowed = Self.add(event.movedBytes, to: &movedBytes, count: &movedCount) || overflowed
            overflowed = Self.add(event.sourceBytes, to: &sourceBytes, count: &sourceCount) || overflowed
            overflowed = Self.add(event.outputBytes, to: &outputBytes, count: &outputCount) || overflowed
            overflowed = Self.add(event.savedBytes, to: &savedBytes, count: &savedCount) || overflowed
            if let delta = event.logicalDeltaBytes {
                let (sum, didOverflow) = logicalDeltaBytes.addingReportingOverflow(delta)
                logicalDeltaBytes = didOverflow ? (delta < 0 ? Int64.min : Int64.max) : sum
                logicalDeltaCount += 1
                overflowed = overflowed || didOverflow
            }
            if let delta = event.observedVolumeDeltaBytes {
                let (sum, didOverflow) = observedVolumeDeltaBytes.addingReportingOverflow(delta)
                observedVolumeDeltaBytes = didOverflow ? (delta < 0 ? Int64.min : Int64.max) : sum
                volumeDeltaCount += 1
                overflowed = overflowed || didOverflow
            }
        }

        func dictionary() -> [String: Any] {
            let knownByteFields = logicalCount + allocationCount + movedCount + sourceCount + outputCount + savedCount + logicalDeltaCount + volumeDeltaCount
            let byteState: String
            if knownByteFields == 0 { byteState = "unknown" }
            else if partialByteEvents > 0 || unknownByteEvents > 0 { byteState = "partial" }
            else { byteState = "complete" }
            let logical = logicalCount > 0 ? number(logicalBytes) : NSNull()
            let allocation = allocationCount > 0 ? number(allocationBytes) : NSNull()
            let moved = movedCount > 0 ? number(movedBytes) : NSNull()
            let source = sourceCount > 0 ? number(sourceBytes) : NSNull()
            let output = outputCount > 0 ? number(outputBytes) : NSNull()
            let saved = savedCount > 0 ? number(savedBytes) : NSNull()
            let logicalDelta = logicalDeltaCount > 0 ? signedNumber(self.logicalDeltaBytes) : NSNull()
            let observedDelta = volumeDeltaCount > 0 ? signedNumber(observedVolumeDeltaBytes) : NSNull()
            return [
                "events": events,
                "known_time_events": knownTimeEvents,
                "unknown_time_events": unknownTimeEvents,
                "partial_byte_events": partialByteEvents,
                "unknown_byte_events": unknownByteEvents,
                "logical_bytes": logical,
                "allocation_bytes": allocation,
                "moved_bytes": moved,
                "source_bytes": source,
                "output_bytes": output,
                "measured_saved_bytes": saved,
                "logical_delta_bytes": logicalDelta,
                "observed_volume_delta_bytes": observedDelta,
                "knownTimeEvents": knownTimeEvents,
                "unknownTimeEvents": unknownTimeEvents,
                "partialByteEvents": partialByteEvents,
                "unknownByteEvents": unknownByteEvents,
                "logicalBytes": logical,
                "allocationBytes": allocation,
                "movedBytes": moved,
                "sourceBytes": source,
                "outputBytes": output,
                "measuredSavedBytes": saved,
                "logicalDeltaBytes": logicalDelta,
                "observedVolumeDeltaBytes": observedDelta,
                "bytes_state": byteState,
                "bytesState": byteState,
                "overflowed": overflowed
            ]
        }

        private static func add(_ value: UInt64?, to total: inout UInt64, count: inout Int) -> Bool {
            guard let value else { return false }
            let (sum, didOverflow) = total.addingReportingOverflow(value)
            total = didOverflow ? UInt64.max : sum
            count += 1
            return didOverflow
        }
    }

    /// Project durable journals into a bounded, sorted timeline & rolling totals.
    public static func project(compression: [String: Any], cleanup: [String: Any], scans: [String: Any], now: Date = Date()) -> [String: Any] {
        let nowSeconds = unixSeconds(now)
        let end = nowSeconds == Int64.max ? nowSeconds : nowSeconds + 1 // End-exclusive second includes effects observed now.
        var byID: [String: Event] = [:]
        appendCompression(compression, to: &byID)
        appendCleanup(cleanup, to: &byID)
        appendScans(scans, to: &byID)

        let allEvents = byID.values.sorted(by: eventSort)
        let shownEvents = Array(allEvents.prefix(timelineLimit))
        let weekStart = max(0, end - weekSeconds)
        let monthStart = max(0, end - monthSeconds)
        let weekValue = periodTotals(allEvents, start: weekStart, end: end, days: 7)
        let monthValue = periodTotals(allEvents, start: monthStart, end: end, days: 30)
        let overall = totals(allEvents)
        let knownTime = allEvents.filter { $0.occurredAt != nil }.count
        let unknownTime = allEvents.count - knownTime
        let partialBytes = allEvents.filter { $0.bytesState == "partial" }.count
        let unknownBytes = allEvents.filter { $0.bytesState == "unknown" }.count
        let coverage: [String: Any] = [
            "events_total": allEvents.count,
            "events_shown": shownEvents.count,
            "timeline_limit": timelineLimit,
            "timeline_truncated": allEvents.count > timelineLimit,
            "known_time_events": knownTime,
            "unknown_time_events": unknownTime,
            "partial_byte_events": partialBytes,
            "unknown_byte_events": unknownBytes
        ]
        let volume = volumeObservation(compression)

        return [
            "schemaVersion": 1,
            "observedAt": isoString(now),
            "events": shownEvents.map { $0.dictionary() },
            "timeline": shownEvents.map { $0.dictionary() },
            "week": weekValue,
            "month": monthValue,
            "weekly": weekValue,
            "monthly": monthValue,
            "totals": overall,
            "volume": volume,
            "coverage": coverage
        ]
    }

    private static func appendCompression(_ payload: [String: Any], to result: inout [String: Event]) {
        for row in rows(payload["events"]) {
            let kind = string(row["kind"])?.lowercased()
            guard kind == nil || kind == "compression" else { continue }
            let source = string(row["sourceURL"] ?? row["source_url"])
            let output = string(row["outputURL"] ?? row["output_url"])
            let timestamp = timestamp(row["occurred_at"] ?? row["occurredAt"] ?? row["timestamp"])
            let id = string(row["id"] ?? row["event_id"] ?? row["eventID"]) ?? stableID("compression", [timestamp.map { String($0) } ?? "", source ?? "", output ?? "", string(row["format"]) ?? "", string(row["sourceBytes"] ?? row["source_bytes"]) ?? "", string(row["outputBytes"] ?? row["output_bytes"]) ?? ""])
            let sourceBytes = integer(row["sourceBytes"] ?? row["source_bytes"])
            let outputBytes = integer(row["outputBytes"] ?? row["output_bytes"])
            let savedBytes = integer(row["measuredSavedBytes"] ?? row["measured_saved_bytes"])
            let bytesState = sourceBytes != nil && outputBytes != nil && savedBytes != nil ? "complete" : (sourceBytes != nil || outputBytes != nil || savedBytes != nil ? "partial" : "unknown")
            insert(Event(id: id, action: "compression", operation: "compress", status: string(row["status"]) ?? "completed", source: "compression", occurredAt: timestamp, logicalBytes: nil, allocationBytes: nil, movedBytes: nil, sourceBytes: sourceBytes, outputBytes: outputBytes, savedBytes: savedBytes, logicalDeltaBytes: nil, observedVolumeDeltaBytes: nil, bytesState: bytesState, rawKind: "compression", path: output ?? source), into: &result)
        }
    }

    private static func volumeObservation(_ payload: [String: Any]) -> [String: Any] {
        let available = payload["freeDiskBytesAvailable"] as? Bool == true
        let free = integer(payload["freeDiskBytes"] ?? payload["free_disk_bytes"])
        return [
            "available": available && free != nil,
            "free_disk_bytes": free.map { number($0) } ?? NSNull(),
            "observed_at": payload["observedAt"] ?? payload["observed_at"] ?? NSNull(),
            "reason": available && free != nil ? NSNull() : "free_space_observation_unavailable"
        ]
    }

    private static func appendCleanup(_ payload: [String: Any], to result: inout [String: Event]) {
        for plan in rows(payload["plans"]) {
            guard let planID = string(plan["plan_id"] ?? plan["planID"] ?? plan["id"]), !planID.isEmpty else { continue }
            let planAction = string(plan["action"] ?? plan["kind"] ?? plan["operation"])?.lowercased()
            let uninstall = planAction == "uninstall" || planAction == "remove_app" || planAction == "app_uninstall"
            for item in rows(plan["items"]) {
                guard let path = string(item["path"]), !path.isEmpty else { continue }
                var logical = integer(item["logical_bytes"] ?? item["logicalBytes"])
                if let outcome = object(item["outcome"]) {
                    appendCleanupOutcome(outcome, planID: planID, path: path, logical: logical, uninstall: uninstall, restore: false, to: &result)
                    logical = integer(outcome["logical_bytes"] ?? outcome["logicalBytes"]) ?? logical
                }
                if let undo = object(item["undo_outcome"] ?? item["undoOutcome"]) {
                    appendCleanupOutcome(undo, planID: planID, path: path, logical: logical, uninstall: uninstall, restore: true, to: &result)
                }
            }
        }
    }

    private static func appendCleanupOutcome(_ outcome: [String: Any], planID: String, path: String, logical: UInt64?, uninstall: Bool, restore: Bool, to result: inout [String: Event]) {
        guard let status = string(outcome["status"]), !status.isEmpty else { return }
        let normalized = status.lowercased()
        let effectiveRestore = restore || normalized == "restored"
        let action = uninstall && !effectiveRestore ? "uninstall" : "cleanup"
        let operation = effectiveRestore ? "restore" : "move"
        let id = string(outcome["id"] ?? outcome["event_id"] ?? outcome["eventID"]) ?? stableID(action, [planID, path, operation])
        let occurred = effectTimestamp(outcome)
        let logicalBytes = integer(outcome["logical_bytes"] ?? outcome["logicalBytes"]) ?? logical
        let moved = effectiveRestore ? nil : integer(outcome["moved_bytes"] ?? outcome["movedBytes"])
        let allocation = integer(outcome["allocation_bytes"] ?? outcome["attributed_allocation_bytes"] ?? outcome["attributedAllocationBytes"])
        let bytesState: String
        if logicalBytes == nil && moved == nil && allocation == nil { bytesState = "unknown" }
        else if allocation == nil || (operation == "move" && moved == nil) { bytesState = "partial" }
        else { bytesState = "complete" }
        insert(Event(id: id, action: action, operation: operation, status: normalized, source: "cleanup", occurredAt: occurred, logicalBytes: logicalBytes, allocationBytes: allocation, movedBytes: moved, sourceBytes: nil, outputBytes: nil, savedBytes: nil, logicalDeltaBytes: nil, observedVolumeDeltaBytes: nil, bytesState: bytesState, rawKind: action == "uninstall" ? "uninstall" : (effectiveRestore ? "cleanup_restored" : "cleanup_moved"), path: path), into: &result)
    }

    private static func appendScans(_ payload: [String: Any], to result: inout [String: Event]) {
        var activityPayloads = [[String: Any]]()
        if let modules = object(payload["modules"]), let activity = object(modules["activity"]) { activityPayloads.append(activity) }
        if let activity = object(payload["activity"]) { activityPayloads.append(activity) }
        activityPayloads.append(payload)
        for activity in activityPayloads {
            for row in rows(activity["events"]) { appendRustEvent(row, to: &result) }
            if let nested = object(activity["activity"]) {
                for row in rows(nested["events"]) { appendRustEvent(row, to: &result) }
            }
            if let modules = object(activity["modules"]), let history = object(modules["history"]) {
                appendSnapshots(rows(history["snapshots"]), to: &result)
            }
            if let history = object(activity["history"]) {
                appendSnapshots(rows(history["snapshots"]), to: &result)
            }
        }
        if let history = object(payload["history"]) {
            appendSnapshots(rows(history["snapshots"]), to: &result)
        }
    }

    private static func appendRustEvent(_ row: [String: Any], to result: inout [String: Event]) {
        let kindObject = object(row["kind"])
        let kindKey = kindObject.flatMap { $0.keys.first } ?? string(row["kind"]) ?? "unknown"
        let rawKind = kindKey
        let normalized = kindKey.lowercased().replacingOccurrences(of: "_", with: "")
        let body = kindObject.flatMap { object($0[$0.keys.first ?? ""]) } ?? row
        let action: String
        let operation: String
        switch normalized {
        case "cleanupmoved": action = "cleanup"; operation = "move"
        case "cleanuprestored": action = "cleanup"; operation = "restore"
        case "uninstall": action = "uninstall"; operation = "remove"
        case "compression": action = "compression"; operation = "compress"
        case "scan": action = "scan"; operation = "scan"
        case "volumeobservation": action = "volume_observation"; operation = "observe"
        default: return
        }
        let occurred = timestamp(row["occurred_at"] ?? row["occurredAt"] ?? row["timestamp"])
        let id = string(row["id"] ?? row["event_id"] ?? row["eventID"]) ?? stableID(action, [rawKind, occurred.map { String($0) } ?? "", canonicalJSON(body)])
        let logical = integer(body["logical_bytes"] ?? body["logicalBytes"])
        let allocation = integer(body["attributed_bytes"] ?? body["attributed_allocation_bytes"] ?? body["allocation_bytes"] ?? body["attributedBytes"])
        let moved = integer(body["moved_bytes"] ?? body["movedBytes"])
        let sourceBytes = integer(body["source_bytes"] ?? body["sourceBytes"])
        let outputBytes = integer(body["output_bytes"] ?? body["outputBytes"])
        let savedBytes = integer(body["measured_saved_bytes"] ?? body["measuredSavedBytes"])
        let logicalDelta = signedInteger(body["logical_delta_bytes"] ?? body["logicalDeltaBytes"])
        let volumeDelta = signedInteger(body["used_delta_bytes"] ?? body["usedDeltaBytes"] ?? body["observed_volume_delta_bytes"] ?? body["observedVolumeDeltaBytes"])
        let byteValues = [logical, allocation, moved, sourceBytes, outputBytes, savedBytes].compactMap { $0 }.count
        let bytesState: String
        if action == "volume_observation" { bytesState = volumeDelta == nil ? "unknown" : "complete" }
        else if byteValues == 0 && logicalDelta == nil { bytesState = "unknown" }
        else if (action == "scan" && allocation == nil) || (action == "compression" && logicalDelta == nil && (sourceBytes == nil || outputBytes == nil || savedBytes == nil)) { bytesState = "partial" }
        else { bytesState = "complete" }
        insert(Event(id: id, action: action, operation: operation, status: string(row["status"]) ?? "completed", source: "scans", occurredAt: occurred, logicalBytes: logical, allocationBytes: allocation, movedBytes: moved, sourceBytes: sourceBytes, outputBytes: outputBytes, savedBytes: savedBytes, logicalDeltaBytes: logicalDelta, observedVolumeDeltaBytes: volumeDelta, bytesState: bytesState, rawKind: rawKind, path: string(body["path"] ?? body["root"])), into: &result)
    }

    private static func appendSnapshots(_ snapshots: [[String: Any]], to result: inout [String: Event]) {
        for snapshot in snapshots {
            guard let id = string(snapshot["id"] ?? snapshot["snapshot_id"] ?? snapshot["snapshotId"]) else { continue }
            let report = object(snapshot["report"])
            let accounting = object(snapshot["accounting"]) ?? object(report?["accounting"])
            let logical = integer(snapshot["logical_bytes"] ?? snapshot["logicalBytes"] ?? accounting?["logical_bytes"] ?? accounting?["logicalBytes"])
            let allocation = integer(snapshot["attributed_allocation_bytes"] ?? snapshot["attributedAllocationBytes"] ?? accounting?["attributed_allocation_bytes"] ?? accounting?["attributedAllocationBytes"])
            let occurred = timestamp(snapshot["created_at"] ?? snapshot["createdAt"] ?? snapshot["occurred_at"] ?? snapshot["occurredAt"])
            let bytesState = logical == nil && allocation == nil ? "unknown" : (allocation == nil ? "partial" : "complete")
            insert(Event(id: id, action: "scan", operation: "scan", status: (snapshot["incomplete"] as? Bool == true) ? "partial" : "completed", source: "scans", occurredAt: occurred, logicalBytes: logical, allocationBytes: allocation, movedBytes: nil, sourceBytes: nil, outputBytes: nil, savedBytes: nil, logicalDeltaBytes: nil, observedVolumeDeltaBytes: nil, bytesState: bytesState, rawKind: "scan", path: (snapshot["roots"] as? [String])?.first), into: &result)
        }
    }

    private static func insert(_ event: Event, into result: inout [String: Event]) {
        guard !event.id.isEmpty else { return }
        guard var existing = result[event.id] else { result[event.id] = event; return }
        if eventQuality(event) > eventQuality(existing) { result[event.id] = event }
        else {
            if existing.path == nil { existing.path = event.path }
            if event.action == "scan", event.status == "partial" { existing.status = "partial" }
            result[event.id] = existing
        }
    }

    private static func eventQuality(_ event: Event) -> Int {
        (event.occurredAt == nil ? 0 : 8) + (event.bytesState == "complete" ? 4 : event.bytesState == "partial" ? 2 : 0) + (event.status == "completed" || event.status == "moved" || event.status == "restored" ? 1 : 0)
    }

    private static func eventSort(_ lhs: Event, _ rhs: Event) -> Bool {
        switch (lhs.occurredAt, rhs.occurredAt) {
        case let (left?, right?) where left != right: return left > right
        case (_?, nil): return true
        case (nil, _?): return false
        default: return lhs.id < rhs.id
        }
    }

    private static func periodTotals(_ events: [Event], start: Int64, end: Int64, days: Int) -> [String: Any] {
        let filtered = events.filter { event in
            guard let at = event.occurredAt else { return false }
            return at >= start && at < end
        }
        var value = totals(filtered)
        value["window"] = ["start": start, "end": end, "timezone": "UTC", "duration_days": days]
        value["window_start"] = start
        value["window_end"] = end
        return value
    }

    private static func totals(_ events: [Event]) -> [String: Any] {
        var all = Bucket()
        var actions: [String: Bucket] = [:]
        var operations: [String: Bucket] = [:]
        for event in events {
            all.add(event)
            actions[event.action, default: Bucket()].add(event)
            operations["\(event.action).\(event.operation)", default: Bucket()].add(event)
        }
        var value = all.dictionary()
        value["by_action"] = actions.mapValues { $0.dictionary() }
        value["byAction"] = actions.mapValues { $0.dictionary() }
        value["by_operation"] = operations.mapValues { $0.dictionary() }
        value["compressions"] = actions["compression"]?.events ?? 0
        value["scan_events"] = actions["scan"]?.events ?? 0
        value["uninstall_events"] = actions["uninstall"]?.events ?? 0
        return value
    }

    private static func bucket(_ events: [Event]) -> Bucket {
        var bucket = Bucket()
        for event in events { bucket.add(event) }
        return bucket
    }

    private static func rows(_ value: Any?) -> [[String: Any]] {
        if let rows = value as? [[String: Any]] { return Array(rows.prefix(inputLimit)) }
        if let rows = value as? [Any] { return Array(rows.prefix(inputLimit).compactMap { $0 as? [String: Any] }) }
        return []
    }

    private static func object(_ value: Any?) -> [String: Any]? { value as? [String: Any] }

    private static func string(_ value: Any?) -> String? {
        if let value = value as? String { return value.isEmpty ? nil : value }
        if let value = value as? NSNumber, CFGetTypeID(value) != CFBooleanGetTypeID() { return value.stringValue }
        return nil
    }

    private static func integer(_ value: Any?) -> UInt64? {
        if let number = value as? NSNumber {
            guard CFGetTypeID(number) != CFBooleanGetTypeID() else { return nil }
            return UInt64(number.stringValue)
        }
        guard let value = value as? String else { return nil }
        return UInt64(value.trimmingCharacters(in: .whitespacesAndNewlines))
    }

    private static func signedInteger(_ value: Any?) -> Int64? {
        if let number = value as? NSNumber {
            guard CFGetTypeID(number) != CFBooleanGetTypeID() else { return nil }
            return Int64(number.stringValue)
        }
        guard let value = value as? String else { return nil }
        return Int64(value.trimmingCharacters(in: .whitespacesAndNewlines))
    }

    private static func canonicalJSON(_ value: [String: Any]) -> String {
        guard let data = try? JSONSerialization.data(withJSONObject: value, options: [.sortedKeys]),
              let encoded = String(data: data, encoding: .utf8) else { return "invalid" }
        return encoded
    }

    private static func timestamp(_ value: Any?) -> Int64? {
        if let number = signedInteger(value), number >= 0 { return number }
        guard let value = value as? String else { return nil }
        if let date = isoFormatter.date(from: value) { return unixSeconds(date) }
        if let date = isoFormatterWithoutFraction.date(from: value) { return unixSeconds(date) }
        guard let numeric = Double(value), numeric.isFinite, numeric >= 0 else { return nil }
        let seconds = numeric > 100_000_000_000 ? numeric / 1_000 : numeric
        guard seconds < Double(Int64.max) else { return nil }
        return Int64(seconds)
    }

    private static func effectTimestamp(_ outcome: [String: Any]) -> Int64? {
        // created_at belongs to review/plan lifecycle, so it is deliberately excluded.
        for key in ["occurred_at", "occurredAt", "effected_at", "effectedAt", "effect_at", "effectAt", "completed_at", "completedAt", "finished_at", "finishedAt", "applied_at", "appliedAt", "restored_at", "restoredAt", "timestamp"] {
            if let value = timestamp(outcome[key]) { return value }
        }
        return nil
    }

    private static func unixSeconds(_ date: Date) -> Int64 {
        let seconds = date.timeIntervalSince1970
        guard seconds.isFinite, seconds > 0 else { return 0 }
        guard seconds < Double(Int64.max) else { return Int64.max }
        return Int64(seconds)
    }

    private static let isoFormatter: ISO8601DateFormatter = {
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return formatter
    }()

    private static let isoFormatterWithoutFraction: ISO8601DateFormatter = {
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime]
        return formatter
    }()

    private static func isoString(_ date: Date) -> String { isoFormatter.string(from: date) }

    private static func number(_ value: UInt64?) -> Any {
        guard let value else { return NSNull() }
        return value <= UInt64(Int64.max) ? Int64(value) : String(value)
    }

    private static func signedNumber(_ value: Int64?) -> Any {
        guard let value else { return NSNull() }
        return value
    }

    private static func stableID(_ prefix: String, _ parts: [String]) -> String {
        var hash: UInt64 = 14695981039346656037
        for byte in parts.joined(separator: "\u{1f}").utf8 {
            hash ^= UInt64(byte)
            hash = hash &* 1099511628211
        }
        return "\(prefix)-\(String(hash, radix: 16))"
    }
}
