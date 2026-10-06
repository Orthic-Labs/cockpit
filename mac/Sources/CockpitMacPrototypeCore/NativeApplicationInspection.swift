import AppKit
import CoreFoundation
import Darwin
import Foundation

/// On-demand, read-only observations for one selected application.
///
/// The caller supplies the two process snapshots that bracket this operation.
/// This type never starts a sampler, uses workspace launch metadata as an
/// incarnation, reads process content, or performs a network request.
@MainActor
public final class NativeApplicationInspection {
    public enum Error: Swift.Error, Equatable {
        case invalidApplicationPath
        case invalidObservations
        case processIdentityMissing
        case processIdentityAmbiguous
        case processIdentityMismatch
        case processIdentityUnstable
        case processDenied
        case historyPersistence
    }

    private let stateDirectory: URL
    private let historyFileName = "application-inspection-history.json"
    private let maxSamplesPerApplication = 256
    private let maxApplications = 64
    private let maxHistoryBytes = 2 * 1024 * 1024
    private let maxPIDsPerApplication = 8
    private let maxLsofBytes = 1024 * 1024
    private let inspectionSeconds: TimeInterval = 5
    private let runnerTerminationGrace: TimeInterval = 2.05
    private let isoFormatter: ISO8601DateFormatter = {
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return formatter
    }()

    public init(stateDirectory: URL) {
        self.stateDirectory = stateDirectory.standardizedFileURL
    }

    /// Inspect one already-selected application using root-owned observations.
    ///
    /// `observations` must contain `before` and `after` CLI `procs --json`
    /// objects plus the selected app's `gui` process payload. Rows are marked
    /// pending until root takes a final process snapshot after this method.
    public func inspect(applicationPath: String,
                        observations: [String: Any]) async throws -> [String: Any] {
        let appURL = try validatedApplicationURL(applicationPath)
        guard let before = observations["before"] as? [String: Any],
              let after = observations["after"] as? [String: Any],
              let gui = observations["gui"] as? [String: Any] else {
            throw Error.invalidObservations
        }

        try validateGUIPath(gui, applicationPath: appURL.path)
        let beforeRows = try processRows(before)
        let afterRows = try processRows(after)
        let guiPIDs = try guiProcessIDs(gui)
        guard !guiPIDs.isEmpty else { throw Error.processIdentityMissing }

        var selected: [(pid: Int32, start: UInt64)] = []
        for pid in guiPIDs {
            guard let beforeIdentity = beforeRows[pid], let afterIdentity = afterRows[pid] else {
                throw Error.processIdentityMissing
            }
            guard beforeIdentity.start > 0, afterIdentity.start > 0 else {
                throw Error.processIdentityMissing
            }
            guard beforeIdentity.start == afterIdentity.start else {
                throw Error.processIdentityMismatch
            }
            selected.append((pid, beforeIdentity.start))
        }
        guard !selected.isEmpty else { throw Error.processIdentityMissing }
        guard selected.count <= maxPIDsPerApplication else { throw Error.processIdentityAmbiguous }

        let deadline = ProcessInfo.processInfo.systemUptime + inspectionSeconds
        var rows: [[String: Any]] = []
        var sampleProcesses: [[String: Any]] = []
        var reasons: [String] = ["post_inspection_resample_required"]
        var historyCPU: Double = 0
        var historyMemory: UInt64 = 0
        var hasCPU = false
        var hasMemory = false
        var missingCPUCount = 0
        var missingMemoryCount = 0
        var missingDiskReadCount = 0
        var missingDiskWriteCount = 0

        for identity in selected {
            guard ProcessInfo.processInfo.systemUptime < deadline else {
                reasons.append("inspection_time_limit")
                break
            }
            guard let nativeStart = try nativeStartTime(pid: identity.pid) else {
                throw Error.processIdentityUnstable
            }
            guard nativeStart == identity.start else {
                throw Error.processIdentityMismatch
            }

            let beforeProcess = beforeRows[identity.pid]
            let afterProcess = afterRows[identity.pid]
            var row: [String: Any] = [
                "pid": Int(identity.pid),
                "start_time": identity.start,
                "identityVerified": true,
                "identityBasis": "CLI_before_after_and_libproc",
                "readyForPresentation": false,
                "requiresPostInspectionResample": true
            ]
            if let cpu = number(afterProcess?.value(for: "cpu_usage_percent")) {
                row["cpuPercent"] = cpu
                historyCPU += cpu
                hasCPU = true
            } else {
                row["cpu"] = ["available": false, "reason": "cli_cpu_unavailable"]
                missingCPUCount += 1
            }
            if let memory = unsignedNumber(metricValue(afterProcess?.value(for: "memory"))) {
                row["memoryBytes"] = memory
                let total = historyMemory.addingReportingOverflow(memory)
                historyMemory = total.overflow ? UInt64.max : total.partialValue
                hasMemory = true
            } else {
                row["memory"] = ["available": false, "reason": "cli_memory_unavailable"]
                missingMemoryCount += 1
            }
            if beforeProcess == nil { reasons.append("before_process_missing") }

            let io = await inspectIO(pid: identity.pid, deadline: deadline)
            row["openFiles"] = io.openFiles
            row["network"] = io.network
            let usage = try nativeUsage(pid: identity.pid)
            var counters = io.counters
            if let read = usage.read {
                row["diskReadBytes"] = read
                counters["diskReadBytes"] = read
            } else {
                missingDiskReadCount += 1
                counters["diskReadBytes"] = NSNull()
            }
            if let written = usage.written {
                row["diskWrittenBytes"] = written
                counters["diskWrittenBytes"] = written
            } else {
                missingDiskWriteCount += 1
                counters["diskWrittenBytes"] = NSNull()
            }
            counters["available"] = usage.read != nil && usage.written != nil
            counters["networkCounters"] = ["available": false, "reason": "network_io_counters_not_implemented"]
            if usage.read != nil && usage.written != nil {
                counters.removeValue(forKey: "reason")
            } else if let reason = usage.reason {
                counters["reason"] = reason
            }
            row["ioCounters"] = counters
            if usage.read == nil || usage.written == nil {
                row["diskIO"] = ["available": false, "reason": "libproc_rusage_unavailable"]
            }
            if let reason = usage.reason { reasons.append(reason) }
            if let ioReason = io.reason { reasons.append(ioReason) }
            rows.append(row)

            if let nativeAfter = try nativeStartTime(pid: identity.pid) {
                guard nativeAfter == identity.start else { throw Error.processIdentityUnstable }
                sampleProcesses.append([
                    "pid": Int(identity.pid),
                    "start_time": identity.start,
                    "cpuPercent": number(afterProcess?.value(for: "cpu_usage_percent")) ?? NSNull(),
                    "memoryBytes": unsignedNumber(metricValue(afterProcess?.value(for: "memory"))) ?? NSNull(),
                    "diskReadBytes": row["diskReadBytes"] ?? NSNull(),
                    "diskWrittenBytes": row["diskWrittenBytes"] ?? NSNull()
                ])
            } else {
                throw Error.processIdentityUnstable
            }
        }

        let unobservedCount = max(0, selected.count - sampleProcesses.count)
        missingCPUCount += unobservedCount
        missingMemoryCount += unobservedCount
        missingDiskReadCount += unobservedCount
        missingDiskWriteCount += unobservedCount
        if unobservedCount > 0 || missingCPUCount > 0 || missingMemoryCount > 0 ||
            missingDiskReadCount > 0 || missingDiskWriteCount > 0 {
            reasons.append("sample_coverage_partial")
        }

        let appKey = appURL.path
        let sample: [String: Any] = [
            "observedAt": isoFormatter.string(from: Date()),
            "processes": sampleProcesses,
            "cpuPercent": hasCPU && missingCPUCount == 0 && sampleProcesses.count == selected.count ? historyCPU : NSNull(),
            "memoryBytes": hasMemory && missingMemoryCount == 0 && sampleProcesses.count == selected.count ? historyMemory : NSNull(),
            "coverage": [
                "selectedProcessCount": selected.count,
                "observedProcessCount": sampleProcesses.count,
                "complete": sampleProcesses.count == selected.count,
                "missingCPUCount": missingCPUCount,
                "missingMemoryCount": missingMemoryCount,
                "missingDiskReadCount": missingDiskReadCount,
                "missingDiskWriteCount": missingDiskWriteCount
            ]
        ]
        let history: [String: Any] = [
            "available": false,
            "pending": true,
            "applicationPath": appKey,
            "sample": sample,
            "maxSamples": maxSamplesPerApplication,
            "maxApplications": maxApplications,
            "reason": "final_resample_required"
        ]

        let postInspectionIdentities: [[String: Any]] = rows.compactMap { row in
            guard let pid = row["pid"] as? Int, let start = unsignedNumber(row["start_time"]) else { return nil }
            return ["pid": pid, "start_time": start]
        }
        return [
            "schemaVersion": 1,
            "available": true,
            "applicationPath": appKey,
            "identity": [
                "basis": "libproc_PROC_PIDTBSDINFO",
                "processes": postInspectionIdentities,
                "requiresPostInspectionResample": true,
                "readyForPresentation": false
            ],
            "processes": rows,
            "history": history,
            "coverage": sample["coverage"] ?? NSNull(),
            "ioCounters": ["available": missingDiskReadCount == 0 && missingDiskWriteCount == 0 && !rows.isEmpty, "basis": "per_process_libproc_rusage_v2", "coverage": "GUI_processes_only"],
            "reasons": reasons
        ]
    }

    /// Confirm an inspection against root's final CLI process snapshot, then
    /// make its sample durable and expose rows as presentation-ready.
    public func confirm(applicationPath: String,
                        inspection: [String: Any],
                        finalObservations: [String: Any]) throws -> [String: Any] {
        let appURL = try validatedApplicationURL(applicationPath)
        guard (inspection["applicationPath"] as? String) == appURL.path else {
            throw Error.invalidObservations
        }
        let finalObject = (finalObservations["after"] as? [String: Any]) ?? finalObservations
        let finalRows = try processRows(finalObject)
        guard !finalRows.isEmpty else { throw Error.processIdentityMissing }

        guard let inspectionRows = inspection["processes"] as? [[String: Any]], !inspectionRows.isEmpty else {
            throw Error.invalidObservations
        }
        var seen = Set<Int32>()
        for row in inspectionRows {
            guard let pidValue = signedNumber(row["pid"]), pidValue > 0,
                  pidValue <= Int64(Int32.max),
                  let start = unsignedNumber(row["start_time"]), start > 0 else {
                throw Error.processIdentityMissing
            }
            let pid = Int32(pidValue)
            guard seen.insert(pid).inserted else { throw Error.processIdentityAmbiguous }
            guard let final = finalRows[pid], final.start == start else {
                throw Error.processIdentityMismatch
            }
            guard let native = try nativeStartTime(pid: pid), native == start else {
                throw Error.processIdentityUnstable
            }
        }

        guard let pendingHistory = inspection["history"] as? [String: Any],
              (pendingHistory["pending"] as? Bool) == true,
              let sample = pendingHistory["sample"] as? [String: Any] else {
            throw Error.invalidObservations
        }
        guard let sampleProcesses = sample["processes"] as? [[String: Any]],
              sampleProcesses.count == inspectionRows.count else {
            throw Error.invalidObservations
        }
        var sampleSeen = Set<Int32>()
        for sampleProcess in sampleProcesses {
            guard let pidValue = signedNumber(sampleProcess["pid"]), pidValue > 0,
                  pidValue <= Int64(Int32.max),
                  let start = unsignedNumber(sampleProcess["start_time"]), start > 0,
                  let final = finalRows[Int32(pidValue)], final.start == start else {
                throw Error.processIdentityMismatch
            }
            guard sampleSeen.insert(Int32(pidValue)).inserted else { throw Error.processIdentityAmbiguous }
        }

        let persisted = persistSample(applicationPath: appURL.path, sample: sample)
        guard (persisted["available"] as? Bool) == true else { throw Error.historyPersistence }
        var result = inspection
        let latestSamples = persisted["samples"] as? [[String: Any]] ?? []
        let latestSampleProcesses = latestSamples.last.flatMap { $0["processes"] as? [[String: Any]] } ?? []
        var ratesByIdentity: [String: [String: Any]] = [:]
        for process in latestSampleProcesses {
            guard let pid = signedNumber(process["pid"]), let start = unsignedNumber(process["start_time"]) else { continue }
            ratesByIdentity["\(pid):\(start)"] = process
        }
        var readyRows: [[String: Any]] = []
        for var row in inspectionRows {
            row["readyForPresentation"] = true
            row["requiresPostInspectionResample"] = false
            row["identityBasis"] = "CLI_before_after_final_and_libproc"
            if let pid = signedNumber(row["pid"]), let start = unsignedNumber(row["start_time"]),
               let rateProcess = ratesByIdentity["\(pid):\(start)"] {
                if let readRate = number(rateProcess["diskReadBytesPerSecond"]) {
                    row["diskReadBytesPerSecond"] = readRate
                }
                if let writtenRate = number(rateProcess["diskWrittenBytesPerSecond"]) {
                    row["diskWrittenBytesPerSecond"] = writtenRate
                }
                if let reason = rateProcess["diskRateReason"] as? String {
                    row["diskRateReason"] = reason
                }
                if var counters = row["ioCounters"] as? [String: Any] {
                    counters["diskReadBytesPerSecond"] = rateProcess["diskReadBytesPerSecond"] ?? NSNull()
                    counters["diskWrittenBytesPerSecond"] = rateProcess["diskWrittenBytesPerSecond"] ?? NSNull()
                    if let reason = rateProcess["diskRateReason"] as? String { counters["diskRateReason"] = reason }
                    row["ioCounters"] = counters
                }
            }
            readyRows.append(row)
        }
        var identity = (inspection["identity"] as? [String: Any]) ?? [:]
        identity["basis"] = "CLI_before_after_final_and_libproc"
        identity["processes"] = sampleProcesses.map { ["pid": $0["pid"] ?? NSNull(), "start_time": $0["start_time"] ?? NSNull()] }
        identity["requiresPostInspectionResample"] = false
        identity["readyForPresentation"] = true
        result["identity"] = identity
        result["processes"] = readyRows
        result["history"] = persisted
        result["readyForPresentation"] = true
        if var reasons = result["reasons"] as? [String] {
            reasons.removeAll { $0 == "post_inspection_resample_required" }
            result["reasons"] = reasons
        }
        return result
    }

    private struct ProcessObservation {
        let identity: UInt64
        let fields: [String: Any]
        var start: UInt64 { identity }
        func value(for key: String) -> Any? { fields[key] }
    }

    private struct IOObservation {
        let openFiles: [String: Any]
        let network: [String: Any]
        let counters: [String: Any]
        let reason: String?
    }

    private enum StateDirectoryError: Swift.Error {
        case missing
    }

    private func validatedApplicationURL(_ path: String) throws -> URL {
        guard path.hasPrefix("/"), path.utf8.count <= 4096 else { throw Error.invalidApplicationPath }
        let url = URL(fileURLWithPath: path).standardizedFileURL
        guard url.pathExtension.caseInsensitiveCompare("app") == .orderedSame else { throw Error.invalidApplicationPath }
        var descriptor = open("/", O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)
        guard descriptor >= 0 else { throw Error.invalidApplicationPath }
        for component in url.pathComponents.dropFirst() {
            let next = component.withCString { openat(descriptor, $0, O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC) }
            close(descriptor)
            guard next >= 0 else { throw Error.invalidApplicationPath }
            descriptor = next
            var value = stat()
            guard fstat(descriptor, &value) == 0, value.st_flags & 0x4000_0000 == 0 else {
                close(descriptor); throw Error.invalidApplicationPath
            }
        }
        close(descriptor)
        return url
    }

    private func validateGUIPath(_ gui: [String: Any], applicationPath: String) throws {
        let possibleKeys = ["path", "bundlePath", "applicationPath"]
        for key in possibleKeys {
            if let path = gui[key] as? String, URL(fileURLWithPath: path).standardizedFileURL.path != applicationPath {
                throw Error.invalidApplicationPath
            }
        }
        for entry in gui["entries"] as? [[String: Any]] ?? [] {
            for key in possibleKeys {
                if let path = entry[key] as? String,
                   URL(fileURLWithPath: path).standardizedFileURL.path != applicationPath {
                    throw Error.invalidApplicationPath
                }
            }
        }
    }

    private func guiProcessIDs(_ gui: [String: Any]) throws -> [Int32] {
        var result: [Int32] = []
        var seen = Set<Int32>()
        for entry in gui["entries"] as? [[String: Any]] ?? [] {
            guard let identity = entry["identity"] as? [String: Any],
                  let pid = signedNumber(identity["pid"]), pid > 0 else {
                throw Error.processIdentityMissing
            }
            // Workspace launch metadata is intentionally ignored.
            // Incarnation proof comes only from CLI snapshots, libproc, and
            // the caller's final resample.
            guard pid <= Int64(Int32.max) else { throw Error.processIdentityMissing }
            let value = Int32(pid)
            if !seen.insert(value).inserted { throw Error.processIdentityAmbiguous }
            result.append(value)
        }
        return result.sorted()
    }

    private func processRows(_ object: [String: Any]) throws -> [Int32: ProcessObservation] {
        var result: [Int32: ProcessObservation] = [:]
        for process in object["processes"] as? [[String: Any]] ?? [] {
            guard let identity = process["identity"] as? [String: Any],
                  let pidValue = signedNumber(identity["pid"]), pidValue > 0,
                  pidValue <= Int64(Int32.max),
                  let start = unsignedNumber(identity["start_time"]), start > 0 else { continue }
            let key = Int32(pidValue)
            if result[key] != nil { throw Error.processIdentityAmbiguous }
            result[key] = ProcessObservation(identity: start, fields: process)
        }
        return result
    }

    private func nativeStartTime(pid: Int32) throws -> UInt64? {
        var info = proc_bsdinfo()
        let expectedSize = Int32(MemoryLayout<proc_bsdinfo>.size)
        let got = proc_pidinfo(pid, PROC_PIDTBSDINFO, 0, &info, expectedSize)
        guard got > 0 else {
            if errno == EACCES || errno == EPERM { throw Error.processDenied }
            if errno == ESRCH { return nil }
            throw Error.processDenied
        }
        guard got == expectedSize, info.pbi_start_tvsec > 0 else { throw Error.processDenied }
        return UInt64(info.pbi_start_tvsec)
    }

    private struct NativeUsageObservation {
        let read: UInt64?
        let written: UInt64?
        let reason: String?
    }

    /// Pinned SDK rusage_info_v2 counters. This is cumulative process disk I/O;
    /// rates are derived only after a later confirmed sample.
    private func nativeUsage(pid: Int32) throws -> NativeUsageObservation {
        var usage = rusage_info_v2()
        let status = withUnsafeMutablePointer(to: &usage) { pointer in
            pointer.withMemoryRebound(to: rusage_info_t?.self, capacity: 1) {
                proc_pid_rusage(pid, RUSAGE_INFO_V2, $0)
            }
        }
        guard status == 0 else {
            if errno == ESRCH { throw Error.processIdentityUnstable }
            if errno == EACCES || errno == EPERM {
                return NativeUsageObservation(read: nil, written: nil, reason: "disk_io_permission_denied")
            }
            return NativeUsageObservation(read: nil, written: nil, reason: "disk_io_rusage_unavailable")
        }
        return NativeUsageObservation(read: usage.ri_diskio_bytesread,
                                      written: usage.ri_diskio_byteswritten,
                                      reason: nil)
    }

    private func inspectIO(pid: Int32, deadline: TimeInterval) async -> IOObservation {
        let tool = URL(fileURLWithPath: "/usr/sbin/lsof")
        guard FileManager.default.isExecutableFile(atPath: tool.path) else {
            return IOObservation(openFiles: ["available": false, "paths": [], "reason": "fixed_tool_unavailable"],
                                 network: ["available": false, "endpoints": [], "reason": "fixed_tool_unavailable"],
                                 counters: ["available": false, "reason": "native_io_counters_not_implemented"],
                                 reason: "io_unavailable_fixed_tool")
        }
        let remaining = deadline - ProcessInfo.processInfo.systemUptime
        guard remaining > runnerTerminationGrace + 0.01 else {
            return IOObservation(openFiles: ["available": false, "paths": [], "reason": "inspection_time_limit"],
                                 network: ["available": false, "endpoints": [], "reason": "inspection_time_limit"],
                                 counters: ["available": false, "reason": "native_io_counters_not_implemented"],
                                 reason: "io_time_limit")
        }
        let request = ScanRequest(executable: tool,
                                  arguments: ["-nP", "-p", String(pid), "-FpcftPn"],
                                  deadline: min(inspectionSeconds, remaining - runnerTerminationGrace),
                                  stdoutLimit: maxLsofBytes,
                                  stderrLimit: 16 * 1024)
        do {
            let outcome = try await ProcessScanRunner().run(request)
            guard !outcome.truncated else {
                return IOObservation(openFiles: ["available": false, "paths": [], "reason": "stdout_limit"],
                                     network: ["available": false, "endpoints": [], "reason": "stdout_limit"],
                                     counters: ["available": false, "reason": "native_io_counters_not_implemented"],
                                     reason: "io_output_limit")
            }
            let parsed = parseLsof(outcome.stdout)
            return IOObservation(openFiles: ["available": true, "paths": parsed.paths, "truncated": parsed.truncated],
                                 network: ["available": true, "endpoints": parsed.endpoints, "truncated": parsed.truncated],
                                 counters: ["available": false, "reason": "native_io_counters_not_implemented"],
                                 reason: parsed.truncated ? "io_row_limit" : nil)
        } catch ScanFailure.nonZeroExit(_, _) {
            return IOObservation(openFiles: ["available": false, "paths": [], "reason": "permission_denied_or_process_vanished"],
                                 network: ["available": false, "endpoints": [], "reason": "permission_denied_or_process_vanished"],
                                 counters: ["available": false, "reason": "native_io_counters_not_implemented"],
                                 reason: "io_permission_or_process_unavailable")
        } catch ScanFailure.timedOut {
            return IOObservation(openFiles: ["available": false, "paths": [], "reason": "time_limit"],
                                 network: ["available": false, "endpoints": [], "reason": "time_limit"],
                                 counters: ["available": false, "reason": "native_io_counters_not_implemented"],
                                 reason: "io_time_limit")
        } catch {
            return IOObservation(openFiles: ["available": false, "paths": [], "reason": "native_lsof_failed"],
                                 network: ["available": false, "endpoints": [], "reason": "native_lsof_failed"],
                                 counters: ["available": false, "reason": "native_io_counters_not_implemented"],
                                 reason: "io_unavailable")
        }
    }

    private func parseLsof(_ data: Data) -> (paths: [[String: Any]], endpoints: [[String: Any]], truncated: Bool) {
        let maxRows = 256
        var paths: [[String: Any]] = []
        var endpoints: [[String: Any]] = []
        var truncated = false
        struct Descriptor {
            var fd: String
            var type: String?
            var protocolName: String?
            var name: String?
        }
        var descriptor: Descriptor?

        func append(_ item: Descriptor) {
            guard let name = item.name, !name.isEmpty else { return }
            let bounded = String(name.prefix(2048))
            if bounded != name { truncated = true }
            if item.type == "IPv4" || item.type == "IPv6" {
                guard let protocolName = item.protocolName?.uppercased(),
                      protocolName == "TCP" || protocolName == "UDP" else { return }
                if endpoints.count < maxRows {
                    endpoints.append(["endpoint": bounded, "host": endpointHost(bounded),
                                      "fd": item.fd, "type": item.type!, "protocol": protocolName])
                } else {
                    truncated = true
                }
            } else if bounded.hasPrefix("/") {
                if paths.count < maxRows {
                    paths.append(["path": bounded, "fd": item.fd])
                } else {
                    truncated = true
                }
            }
        }

        for line in String(decoding: data, as: UTF8.self).split(whereSeparator: \.isNewline) {
            guard let field = line.first else { continue }
            let value = String(line.dropFirst())
            switch field {
            case "f":
                if let descriptor { append(descriptor) }
                descriptor = Descriptor(fd: value, type: nil, protocolName: nil, name: nil)
            case "t": descriptor?.type = value
            case "P": descriptor?.protocolName = value
            case "n": descriptor?.name = value
            default: continue
            }
        }
        if let descriptor { append(descriptor) }
        return (paths, endpoints, truncated)
    }

    private func endpointHost(_ endpoint: String) -> String {
        let first = endpoint.components(separatedBy: "->").first ?? endpoint
        if first.hasPrefix("[") {
            if let close = first.firstIndex(of: "]") { return String(first[first.startIndex...close]) }
        }
        guard let colon = first.lastIndex(of: ":") else { return first }
        return String(first[..<colon])
    }

    private func persistSample(applicationPath: String, sample: [String: Any]) -> [String: Any] {
        do {
            var apps = try loadHistory()
            var appSamples = apps[applicationPath] ?? []
            let enrichedSample = sampleWithDiskRates(sample, previous: appSamples.last)
            appSamples.append(enrichedSample)
            apps[applicationPath] = Array(appSamples.suffix(maxSamplesPerApplication))
            let sortedPaths = apps.keys.sorted()
            if sortedPaths.count > maxApplications {
                let removable = sortedPaths.filter { $0 != applicationPath }
                for path in removable.prefix(sortedPaths.count - maxApplications) { apps.removeValue(forKey: path) }
            }
            var payload = try historyPayload(apps)
            while encodedSize(payload) > maxHistoryBytes {
                let candidates = apps.keys.sorted()
                let path = candidates.first(where: { $0 != applicationPath }) ?? candidates.first
                guard let path,
                      var samples = apps[path], !samples.isEmpty else { throw Error.historyPersistence }
                samples.removeFirst()
                if samples.isEmpty { apps.removeValue(forKey: path) } else { apps[path] = samples }
                payload = try historyPayload(apps)
            }
            try persist(payload)
            return ["available": true, "pending": false, "applicationPath": applicationPath,
                    "samples": apps[applicationPath] ?? [], "maxSamples": maxSamplesPerApplication,
                    "maxApplications": maxApplications]
        } catch {
            return ["available": false, "applicationPath": applicationPath, "samples": [],
                    "reason": "history_persistence_unavailable"]
        }
    }

    private func sampleWithDiskRates(_ sample: [String: Any],
                                     previous: [String: Any]?) -> [String: Any] {
        var result = sample
        guard var currentProcesses = sample["processes"] as? [[String: Any]] else { return result }
        let previousProcesses = previous?["processes"] as? [[String: Any]] ?? []
        var previousIndex: [String: [String: Any]] = [:]
        for process in previousProcesses {
            guard let pid = signedNumber(process["pid"]), pid > 0, pid <= Int64(Int32.max),
                  let start = unsignedNumber(process["start_time"]), start > 0 else { continue }
            previousIndex["\(pid):\(start)"] = process
        }
        let elapsed: TimeInterval? = {
            guard let previous,
                  let currentDate = sampleDate(sample["observedAt"]),
                  let previousDate = sampleDate(previous["observedAt"]) else { return nil }
            let value = currentDate.timeIntervalSince(previousDate)
            return value.isFinite && value > 0 ? value : nil
        }()
        var availableCount = 0
        var reasonCount = 0
        for index in currentProcesses.indices {
            var process = currentProcesses[index]
            guard let pid = signedNumber(process["pid"]), pid > 0, pid <= Int64(Int32.max),
                  let start = unsignedNumber(process["start_time"]), start > 0 else {
                process["diskRateReason"] = "invalid_process_identity"
                reasonCount += 1
                currentProcesses[index] = process
                continue
            }
            guard let previousProcess = previousIndex["\(pid):\(start)"] else {
                process["diskRateReason"] = "no_consecutive_same_incarnation"
                reasonCount += 1
                currentProcesses[index] = process
                continue
            }
            guard let elapsed,
                  let currentRead = unsignedNumber(process["diskReadBytes"]),
                  let currentWritten = unsignedNumber(process["diskWrittenBytes"]),
                  let previousRead = unsignedNumber(previousProcess["diskReadBytes"]),
                  let previousWritten = unsignedNumber(previousProcess["diskWrittenBytes"]) else {
                process["diskRateReason"] = "disk_counter_unavailable_or_elapsed_invalid"
                reasonCount += 1
                currentProcesses[index] = process
                continue
            }
            guard currentRead >= previousRead, currentWritten >= previousWritten else {
                process["diskRateReason"] = "disk_counter_reset"
                reasonCount += 1
                currentProcesses[index] = process
                continue
            }
            let readRate = Double(currentRead - previousRead) / elapsed
            let writtenRate = Double(currentWritten - previousWritten) / elapsed
            guard readRate.isFinite, writtenRate.isFinite else {
                process["diskRateReason"] = "disk_rate_overflow"
                reasonCount += 1
                currentProcesses[index] = process
                continue
            }
            process["diskReadBytesPerSecond"] = readRate
            process["diskWrittenBytesPerSecond"] = writtenRate
            availableCount += 1
            currentProcesses[index] = process
        }
        result["processes"] = currentProcesses
        result["diskRateCoverage"] = [
            "observedProcessCount": currentProcesses.count,
            "availableProcessCount": availableCount,
            "missingRateCount": reasonCount,
            "complete": !currentProcesses.isEmpty && availableCount == currentProcesses.count
        ]
        return result
    }

    private func sampleDate(_ value: Any?) -> Date? {
        guard let string = value as? String else { return nil }
        if let date = isoFormatter.date(from: string) { return date }
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime]
        return formatter.date(from: string)
    }

    private func loadHistory() throws -> [String: [[String: Any]]] {
        guard let data = try readHistoryData() else { return [:] }
        guard let object = try JSONSerialization.jsonObject(with: data) as? [String: Any],
              unsignedNumber(object["schemaVersion"]) == 1,
              let records = object["applications"] as? [[String: Any]], records.count <= maxApplications else { throw Error.historyPersistence }
        var result: [String: [[String: Any]]] = [:]
        for record in records {
            guard let path = record["applicationPath"] as? String,
                  let samples = record["samples"] as? [[String: Any]], path.hasPrefix("/"), path.utf8.count <= 4096,
                  samples.count <= maxSamplesPerApplication, result[path] == nil else { throw Error.historyPersistence }
            result[path] = samples
        }
        return result
    }

    private func historyPayload(_ apps: [String: [[String: Any]]]) throws -> [String: Any] {
        let records = apps.keys.sorted().prefix(maxApplications).map { path in
            ["applicationPath": path, "samples": Array(apps[path]!.suffix(maxSamplesPerApplication))] as [String: Any]
        }
        return ["schemaVersion": 1, "applications": records]
    }

    private func encodedSize(_ object: [String: Any]) -> Int {
        (try? JSONSerialization.data(withJSONObject: object, options: [.sortedKeys]).count) ?? (maxHistoryBytes + 1)
    }

    private func persist(_ object: [String: Any]) throws {
        guard JSONSerialization.isValidJSONObject(object) else { throw Error.historyPersistence }
        let data = try JSONSerialization.data(withJSONObject: object, options: [.sortedKeys])
        guard data.count <= maxHistoryBytes else { throw Error.historyPersistence }
        let directory = try openStateDirectory(createIfMissing: true)
        defer { close(directory) }
        let temporary = ".\(historyFileName).\(UUID().uuidString).tmp"
        let fd = temporary.withCString { openat(directory, $0, O_WRONLY | O_CREAT | O_EXCL | O_NOFOLLOW | O_CLOEXEC, 0o600) }
        guard fd >= 0 else { throw Error.historyPersistence }
        var temporaryStat = stat()
        guard fstat(fd, &temporaryStat) == 0,
              temporaryStat.st_mode & S_IFMT == S_IFREG,
              temporaryStat.st_uid == geteuid(),
              temporaryStat.st_mode & 0o077 == 0,
              temporaryStat.st_nlink == 1 else {
            close(fd)
            _ = temporary.withCString { unlinkat(directory, $0, 0) }
            throw Error.historyPersistence
        }
        var open = true
        do {
            try data.withUnsafeBytes { buffer in
                var offset = 0
                while offset < data.count {
                    let written = write(fd, buffer.baseAddress!.advanced(by: offset), data.count - offset)
                    guard written > 0 else { throw Error.historyPersistence }
                    offset += written
                }
            }
            guard fsync(fd) == 0 else { throw Error.historyPersistence }
            close(fd); open = false
            let renamed = temporary.withCString { source in
                historyFileName.withCString { destination in
                    renameat(directory, source, directory, destination)
                }
            }
            guard renamed == 0,
                  fsync(directory) == 0 else { throw Error.historyPersistence }
        } catch {
            if open { close(fd) }
            _ = temporary.withCString { unlinkat(directory, $0, 0) }
            throw error
        }
    }

    private func openStateDirectory(createIfMissing: Bool) throws -> Int32 {
        guard stateDirectory.path != "/" else { throw Error.historyPersistence }
        let components = stateDirectory.pathComponents
        guard components.first == "/", components.count > 1 else { throw Error.historyPersistence }
        var descriptor = "/".withCString { open($0, O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC) }
        guard descriptor >= 0 else { throw Error.historyPersistence }
        for component in components.dropFirst() {
            var next = component.withCString {
                openat(descriptor, $0, O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)
            }
            if next < 0, createIfMissing, errno == ENOENT {
                let made = component.withCString { mkdirat(descriptor, $0, 0o700) }
                guard made == 0 || errno == EEXIST else {
                    close(descriptor)
                    throw Error.historyPersistence
                }
                next = component.withCString {
                    openat(descriptor, $0, O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)
                }
            }
            guard next >= 0 else {
                let failure = errno
                close(descriptor)
                if !createIfMissing, failure == ENOENT { throw StateDirectoryError.missing }
                throw Error.historyPersistence
            }
            close(descriptor)
            descriptor = next
        }
        var statInfo = stat()
        guard fstat(descriptor, &statInfo) == 0,
              statInfo.st_mode & S_IFMT == S_IFDIR,
              statInfo.st_uid == geteuid(),
              statInfo.st_mode & 0o077 == 0 else {
            close(descriptor)
            throw Error.historyPersistence
        }
        return descriptor
    }

    private func readHistoryData() throws -> Data? {
        let directory: Int32
        do {
            directory = try openStateDirectory(createIfMissing: false)
        } catch StateDirectoryError.missing {
            return nil
        }
        defer { close(directory) }
        let fd = historyFileName.withCString { openat(directory, $0, O_RDONLY | O_NOFOLLOW | O_CLOEXEC) }
        if fd < 0 {
            if errno == ENOENT { return nil }
            throw Error.historyPersistence
        }
        defer { close(fd) }
        var statInfo = stat()
        guard fstat(fd, &statInfo) == 0,
              statInfo.st_mode & S_IFMT == S_IFREG,
              statInfo.st_uid == geteuid(),
              statInfo.st_mode & 0o077 == 0,
              statInfo.st_nlink == 1,
              statInfo.st_size >= 0,
              statInfo.st_size <= off_t(maxHistoryBytes) else { throw Error.historyPersistence }
        var data = Data()
        data.reserveCapacity(Int(statInfo.st_size))
        var buffer = [UInt8](repeating: 0, count: 64 * 1024)
        while data.count < maxHistoryBytes {
            let remaining = maxHistoryBytes - data.count
            let capacity = min(buffer.count, remaining)
            let count = buffer.withUnsafeMutableBytes { bytes in
                read(fd, bytes.baseAddress, capacity)
            }
            if count == 0 { break }
            guard count > 0, count <= remaining else { throw Error.historyPersistence }
            data.append(contentsOf: buffer.prefix(count))
        }
        if data.count == maxHistoryBytes {
            var extra = UInt8.zero
            let count = withUnsafeMutableBytes(of: &extra) { bytes in
                read(fd, bytes.baseAddress, 1)
            }
            guard count == 0 else { throw Error.historyPersistence }
        }
        guard data.count <= maxHistoryBytes else { throw Error.historyPersistence }
        return data
    }

    private func metricValue(_ value: Any?) -> Any? {
        guard let metric = value as? [String: Any] else { return value }
        return metric["value"]
    }

    private func signedNumber(_ value: Any?) -> Int64? {
        guard !isBoolean(value) else { return nil }
        if let value = value as? Int64 { return value }
        if let value = value as? Int { return Int64(exactly: value) }
        if let value = value as? Int32 { return Int64(value) }
        if let value = value as? UInt64 { return value <= UInt64(Int64.max) ? Int64(value) : nil }
        if let value = value as? UInt { return value <= UInt(Int64.max) ? Int64(value) : nil }
        if let n = value as? NSNumber {
            let type = String(cString: n.objCType)
            guard isPlainCFNumber(n), !["f", "d"].contains(type),
                  let candidate = Int64(n.stringValue) else { return nil }
            guard NSNumber(value: candidate).compare(n) == .orderedSame else { return nil }
            return candidate
        }
        if let s = value as? String { return Int64(s) }
        return nil
    }

    private func unsignedNumber(_ value: Any?) -> UInt64? {
        guard !isBoolean(value) else { return nil }
        if let value = value as? UInt64 { return value }
        if let value = value as? UInt { return UInt64(value) }
        if let value = value as? UInt32 { return UInt64(value) }
        if let value = value as? Int64 { return value >= 0 ? UInt64(value) : nil }
        if let value = value as? Int { return value >= 0 ? UInt64(value) : nil }
        if let n = value as? NSNumber {
            let type = String(cString: n.objCType)
            guard isPlainCFNumber(n), !["f", "d"].contains(type),
                  let candidate = UInt64(n.stringValue) else { return nil }
            guard NSNumber(value: candidate).compare(n) == .orderedSame else { return nil }
            return candidate
        }
        if let s = value as? String { return UInt64(s) }
        return nil
    }

    private func number(_ value: Any?) -> Double? {
        guard !isBoolean(value) else { return nil }
        if let value = value as? Double { return value.isFinite ? value : nil }
        if let value = value as? Float { return value.isFinite ? Double(value) : nil }
        if let value = value as? Int64 { return Double(value) }
        if let value = value as? UInt64 {
            let result = Double(value)
            return result.isFinite ? result : nil
        }
        if let n = value as? NSNumber, isPlainCFNumber(n), n.doubleValue.isFinite { return n.doubleValue }
        if let s = value as? String, let d = Double(s), d.isFinite { return d }
        return nil
    }

    private func isPlainCFNumber(_ value: NSNumber) -> Bool {
        let typeID = CFGetTypeID(value as CFTypeRef)
        return typeID == CFNumberGetTypeID()
    }

    private func isBoolean(_ value: Any?) -> Bool {
        if value is Bool { return true }
        guard let number = value as? NSNumber else { return false }
        return CFGetTypeID(number) == CFBooleanGetTypeID()
    }
}
