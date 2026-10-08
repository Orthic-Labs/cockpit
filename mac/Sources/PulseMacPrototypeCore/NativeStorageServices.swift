import AppKit
import Darwin
import Foundation
import IOKit.ps
import UniformTypeIdentifiers

/// Native, on-demand observations used by dashboard adapters.
///
/// This type deliberately owns no timer or sampling loop. Callers request a
/// fresh observation when they need one; repeated monitor calls provide rates.
@MainActor
public final class NativeStorageServices {
    private struct CPUSample {
        let total: UInt64
        let idle: UInt64
    }

    private struct NetworkSample {
        let timestamp: TimeInterval
        let bytesIn: UInt64
        let bytesOut: UInt64
        let packetsIn: UInt64
        let packetsOut: UInt64
    }

    private let stateDirectory: URL
    private var previousCPU: CPUSample?
    private var previousNetwork: [String: NetworkSample] = [:]
    private var sharedResources: [String: Any]?
    private var activeCompressionJob: MediaCompressionJob?
    private var lastCompressionOutput: CompressionOutputIdentity?

    private let isoFormatter: ISO8601DateFormatter = {
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return formatter
    }()

    public init(stateDirectory: URL) {
        self.stateDirectory = stateDirectory.standardizedFileURL
    }

    /// Cancel only compression owned by this adapter. No process or unrelated
    /// operation is touched.
    public func cancelCompression() -> [String: Any] {
        guard let job = activeCompressionJob else {
            return ["state": "no-active", "status": "no-active", "phase": NSNull()]
        }
        let phase = job.phase.rawValue
        job.cancel()
        return ["state": "cancel-requested", "status": "cancel-requested", "phase": phase]
    }

    /// Stop adapter-owned work when its host is shutting down.
    public func stop() {
        _ = cancelCompression()
    }

    /// Return last measured compression output only while its parent chain,
    /// leaf identity, size, & modification time still match.
    public func verifiedCompressionOutput() throws -> URL {
        guard let expected = lastCompressionOutput else { throw MediaCompressionError.outputMetadataUnavailable }
        let actual = try compressionOutputIdentity(at: expected.url)
        guard actual == expected else { throw MediaCompressionError.outputMetadataUnavailable }
        return expected.url
    }

    public func updateResources(_ reading: [String: Any]) {
        sharedResources = reading
    }

    public func appsPayload() async throws -> [String: Any] {
        let observedAt = isoFormatter.string(from: Date())
        var reasons: [String] = []
        var apps: [[String: Any]] = []
        let deadline = ProcessInfo.processInfo.systemUptime + 5
        let roots = [
            URL(fileURLWithPath: "/Applications", isDirectory: true),
            URL(fileURLWithPath: "/Applications/Utilities", isDirectory: true),
            URL(fileURLWithPath: "/System/Applications", isDirectory: true),
            URL(fileURLWithPath: "/System/Applications/Utilities", isDirectory: true),
            FileManager.default.homeDirectoryForCurrentUser.appendingPathComponent("Applications", isDirectory: true)
        ]
        for root in roots {
            do {
                apps.append(contentsOf: try enumerateApps(in: root, deadline: deadline, reasons: &reasons))
            } catch {
                reasons.append("apps_unavailable:\(root.path)")
            }
        }
        apps.sort { ($0["path"] as? String ?? "") < ($1["path"] as? String ?? "") }
        let footprintDeadline = ProcessInfo.processInfo.systemUptime + 5
        for index in apps.indices {
            guard ProcessInfo.processInfo.systemUptime < footprintDeadline else {
                apps[index]["incomplete"] = true
                apps[index]["bundleSizeIncomplete"] = true
                if !reasons.contains("bundle_measurement_time_limit") { reasons.append("bundle_measurement_time_limit") }
                continue
            }
            guard let path = apps[index]["path"] as? String else { continue }
            let measured = measureBundle(URL(fileURLWithPath: path), deadline: footprintDeadline)
            apps[index]["bundleBytes"] = measured.bytes
            apps[index]["incomplete"] = measured.incomplete
            apps[index]["bundleSizeIncomplete"] = measured.incomplete
        }
        let startup = startupPayload()
        if let startupReason = startup["reason"] as? String { reasons.append(startupReason) }
        let payload: [String: Any] = [
            "schemaVersion": 1,
            "observedAt": observedAt,
            "apps": apps,
            "startup": startup,
            "inventoryIncomplete": !reasons.isEmpty,
            "reasons": reasons
        ]
        try persist(payload, fileName: "apps-observation.json")
        return payload
    }

    public func monitorPayload() async throws -> [String: Any] {
        let observedAt = isoFormatter.string(from: Date())
        var reasons: [String] = []
        let resources = resourcePayload(reasons: &reasons)
        let network = networkPayload(reasons: &reasons)
        let battery = batteryPayload(reasons: &reasons)
        let ports = await listeningPortsPayload()
        if ports["available"] as? Bool == false { reasons.append("listening_ports_unavailable") }
        let payload: [String: Any] = [
            "schemaVersion": 1,
            "observedAt": observedAt,
            "resources": resources,
            "network": network,
            "battery": battery,
            "listeningPorts": ports,
            "reasons": reasons
        ]
        try persist(payload, fileName: "monitor-observation.json")
        return payload
    }

    public func activityPayload() throws -> [String: Any] {
        let events = try loadEvents()
        let now = Date()
        let weekStart = Calendar.current.date(byAdding: .day, value: -7, to: now) ?? now
        let monthStart = Calendar.current.date(byAdding: .day, value: -30, to: now) ?? now
        let week = totals(events: events, since: weekStart)
        let month = totals(events: events, since: monthStart)
        let volume = try? stateDirectory.resourceValues(forKeys: [.volumeAvailableCapacityForImportantUsageKey])
        let freeDisk = volume?.volumeAvailableCapacityForImportantUsage
        var payload: [String: Any] = [
            "schemaVersion": 1,
            "observedAt": isoFormatter.string(from: now),
            "events": events,
            "week": week,
            "month": month,
            "freeDiskBytesAvailable": freeDisk != nil
        ]
        if let freeDisk { payload["freeDiskBytes"] = Int64(freeDisk) }
        try persist(payload, fileName: "activity-observation.json")
        return payload
    }

    public func compress(format: String, quality: Double, maxPixelDimension: Int?,
                         targetSizeBytes: Int64?, presenting: NSWindow?) async throws -> [String: Any] {
        guard activeCompressionJob == nil else { throw MediaCompressionError.invalidRequest }
        guard let mediaFormat = MediaCompressionFormat(rawValue: format.lowercased()) else {
            throw MediaCompressionError.unsupportedCodec
        }
        try validateCompressionOptions(quality: quality, maxPixelDimension: maxPixelDimension,
                                       targetSizeBytes: targetSizeBytes)
        let source = try await chooseSource(presenting: presenting)
        let outputDirectory = try await chooseOutputDirectory(presenting: presenting)
        let request = MediaCompressionRequest(
            sourceURL: source,
            inputDirectoryURL: source.deletingLastPathComponent(),
            outputDirectoryURL: outputDirectory,
            format: mediaFormat,
            quality: quality,
            maxPixelDimension: maxPixelDimension,
            targetSizeBytes: targetSizeBytes
        )
        let job = MediaCompressionJob(request: request)
        activeCompressionJob = job
        defer {
            if activeCompressionJob === job { activeCompressionJob = nil }
        }
        let result = await job.run()
        guard case .success(let result) = result else {
            if case .failure(let error) = result { throw error }
            throw MediaCompressionError.encodeFailed
        }
        guard job.phase == .completed else { throw MediaCompressionError.outputMetadataUnavailable }
        let outputIdentity = try compressionOutputIdentity(at: result.outputURL)
        guard outputIdentity.size == result.outputBytes else { throw MediaCompressionError.outputMetadataUnavailable }
        try recordCompressionObservation(result, format: mediaFormat.rawValue)
        _ = try activityPayload()
        lastCompressionOutput = outputIdentity
        return compressionPayload(result, format: mediaFormat.rawValue, outputIdentity: outputIdentity)
    }

    /// Records a completed adapter result. Kept internal so fixture journeys
    /// can exercise persistence without driving modal panels.
    internal func recordCompressionObservation(_ result: MediaCompressionResult, format: String) throws {
        var events = try loadEvents()
        events.append([
            "timestamp": isoFormatter.string(from: Date()),
            "kind": "compression",
            "format": format,
            "sourceURL": result.sourceURL.path,
            "outputURL": result.outputURL.path,
            "sourceBytes": result.sourceBytes,
            "outputBytes": result.outputBytes,
            "measuredSavedBytes": result.measuredSavedBytes
        ])
        if events.count > 512 { events.removeFirst(events.count - 512) }
        try persist(["schemaVersion": 1, "events": events], fileName: "activity-events.json")
    }

    private func compressionPayload(_ result: MediaCompressionResult, format: String,
                                    outputIdentity: CompressionOutputIdentity? = nil) -> [String: Any] {
        var payload: [String: Any] = [
            "schemaVersion": 1,
            "format": format,
            "sourceURL": result.sourceURL.path,
            "outputURL": result.outputURL.path,
            "sourceBytes": result.sourceBytes,
            "outputBytes": result.outputBytes,
            "measuredSavedBytes": result.measuredSavedBytes
        ]
        if let outputIdentity { payload["outputIdentity"] = outputIdentity.dictionary }
        return payload
    }

    private func validateCompressionOptions(quality: Double, maxPixelDimension: Int?, targetSizeBytes: Int64?) throws {
        guard quality.isFinite, (0...1).contains(quality) else { throw MediaCompressionError.invalidRequest }
        if let dimension = maxPixelDimension, !(1...16_384).contains(dimension) {
            throw MediaCompressionError.invalidRequest
        }
        if let target = targetSizeBytes, target <= 0 { throw MediaCompressionError.invalidRequest }
    }

    private func chooseSource(presenting: NSWindow?) async throws -> URL {
        let panel = NSOpenPanel()
        panel.title = "Choose media to compress"
        panel.prompt = "Choose"
        panel.canChooseFiles = true
        panel.canChooseDirectories = false
        panel.allowsMultipleSelection = false
        panel.allowedContentTypes = [.image, .movie]
        let response: NSApplication.ModalResponse
        if let presenting {
            response = await withCheckedContinuation { continuation in
                panel.beginSheetModal(for: presenting) { continuation.resume(returning: $0) }
            }
        } else {
            response = panel.runModal()
        }
        guard response == .OK, let url = panel.url else { throw MediaCompressionError.cancelled }
        return url.standardizedFileURL
    }

    private func chooseOutputDirectory(presenting: NSWindow?) async throws -> URL {
        let panel = NSOpenPanel()
        panel.title = "Choose output folder"
        panel.prompt = "Choose"
        panel.canChooseFiles = false
        panel.canChooseDirectories = true
        panel.allowsMultipleSelection = false
        let response: NSApplication.ModalResponse
        if let presenting {
            response = await withCheckedContinuation { continuation in
                panel.beginSheetModal(for: presenting) { continuation.resume(returning: $0) }
            }
        } else {
            response = panel.runModal()
        }
        guard response == .OK, let url = panel.url else { throw MediaCompressionError.cancelled }
        return url.standardizedFileURL
    }

    private func enumerateApps(in root: URL, deadline: TimeInterval, reasons: inout [String]) throws -> [[String: Any]] {
        var rootIsDirectory: ObjCBool = false
        guard FileManager.default.fileExists(atPath: root.path, isDirectory: &rootIsDirectory), rootIsDirectory.boolValue else {
            throw NativeStorageError.unavailable
        }
        let entries = try FileManager.default.contentsOfDirectory(at: root, includingPropertiesForKeys: [.isDirectoryKey, .isSymbolicLinkKey], options: [.skipsHiddenFiles])
            .sorted { $0.lastPathComponent.localizedStandardCompare($1.lastPathComponent) == .orderedAscending }
        if entries.count > 512 { reasons.append("application_root_entry_limit") }
        var result: [[String: Any]] = []
        for url in entries.prefix(512) where url.pathExtension.caseInsensitiveCompare("app") == .orderedSame {
            if ProcessInfo.processInfo.systemUptime >= deadline { reasons.append("application_inventory_time_limit"); break }
            let values = try? url.resourceValues(forKeys: [.isDirectoryKey, .isSymbolicLinkKey])
            guard values?.isDirectory == true, values?.isSymbolicLink != true else { continue }
            var bundleStat = stat()
            guard lstat(url.path, &bundleStat) == 0, bundleStat.st_flags & 0x4000_0000 == 0 else { reasons.append("application_metadata_unavailable_or_placeholder"); continue }
            let bundle = Bundle(url: url)
            var row: [String: Any] = [
                "path": url.path,
                "installRoot": root.path,
                "name": (bundle?.object(forInfoDictionaryKey: "CFBundleDisplayName") as? String)
                    ?? (bundle?.object(forInfoDictionaryKey: "CFBundleName") as? String)
                    ?? url.deletingPathExtension().lastPathComponent,
                "iconAvailable": bundle?.object(forInfoDictionaryKey: "CFBundleIconFile") != nil
                    || bundle?.object(forInfoDictionaryKey: "CFBundleIconFiles") != nil
            ]
            if let bundleID = bundle?.bundleIdentifier { row["bundleID"] = bundleID }
            if let short = bundle?.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String, !short.isEmpty {
                row["version"] = short
            } else if let build = bundle?.object(forInfoDictionaryKey: "CFBundleVersion") as? String, !build.isEmpty {
                row["version"] = build
            }
            result.append(row)
        }
        return result
    }

    private func measureBundle(_ url: URL, deadline: TimeInterval) -> (bytes: Int64, incomplete: Bool) {
        let keys: [URLResourceKey] = [.isDirectoryKey, .isSymbolicLinkKey, .fileSizeKey]
        var incomplete = false
        guard let enumerator = FileManager.default.enumerator(at: url, includingPropertiesForKeys: keys,
                                                               options: [], errorHandler: { _, _ in incomplete = true; return true }) else {
            return (0, true)
        }
        var total: Int64 = 0
        var count = 0
        for case let entry as URL in enumerator {
            if ProcessInfo.processInfo.systemUptime >= deadline { incomplete = true; break }
            count += 1
            if count > 20_000 { incomplete = true; break }
            var entryStat = stat()
            guard lstat(entry.path, &entryStat) == 0 else { incomplete = true; enumerator.skipDescendants(); continue }
            if entryStat.st_flags & 0x4000_0000 != 0 { incomplete = true; enumerator.skipDescendants(); continue }
            if entryStat.st_mode & S_IFMT == S_IFLNK { enumerator.skipDescendants(); continue }
            guard let values = try? entry.resourceValues(forKeys: Set(keys)) else { incomplete = true; enumerator.skipDescendants(); continue }
            if values.isDirectory != true {
                guard let size = values.fileSize, size >= 0 else { incomplete = true; continue }
                let (next, overflow) = total.addingReportingOverflow(Int64(size))
                if overflow { incomplete = true; break }
                total = next
            }
        }
        return (total, incomplete)
    }

    private func startupPayload() -> [String: Any] {
        let directories = [
            URL(fileURLWithPath: "/Library/LaunchAgents", isDirectory: true),
            URL(fileURLWithPath: "/Library/LaunchDaemons", isDirectory: true),
            FileManager.default.homeDirectoryForCurrentUser.appendingPathComponent("Library/LaunchAgents", isDirectory: true)
        ]
        var items: [[String: Any]] = []
        var readable = false
        for directory in directories {
            guard let names = try? FileManager.default.contentsOfDirectory(atPath: directory.path) else { continue }
            readable = true
            for name in names.sorted().prefix(256) where name.hasSuffix(".plist") {
                let url = directory.appendingPathComponent(name)
                var statValue = stat()
                guard lstat(url.path, &statValue) == 0, (statValue.st_mode & S_IFMT) != S_IFLNK else { continue }
                items.append(["label": name, "path": url.path, "kind": "launchd", "source": directory.path])
            }
        }
        if readable {
            return ["available": true, "supported": true, "items": items]
        }
        return ["available": false, "supported": false, "items": [], "reason": "launchd_login_items_unavailable"]
    }

    private func resourcePayload(reasons: inout [String]) -> [String: Any] {
        if let sharedResources { return sharedResources }
        var resources: [String: Any] = ["available": true]
        if let cpu = cpuLoad() {
            resources["cpuPercent"] = cpu
        } else {
            resources["cpuPercentAvailable"] = false
            reasons.append("cpu_sample_unavailable")
        }
        resources["physicalMemoryBytes"] = Int64(ProcessInfo.processInfo.physicalMemory)
        resources["uptimeSeconds"] = ProcessInfo.processInfo.systemUptime
        var load = [Double](repeating: 0, count: 3)
        if getloadavg(&load, 3) == 3 { resources["loadAverage"] = load }
        var vmstat = vm_statistics64()
        var count = mach_msg_type_number_t(MemoryLayout<vm_statistics64>.stride / MemoryLayout<integer_t>.stride)
        let result = withUnsafeMutablePointer(to: &vmstat) { pointer in
            pointer.withMemoryRebound(to: integer_t.self, capacity: Int(count)) {
                host_statistics64(mach_host_self(), HOST_VM_INFO64, $0, &count)
            }
        }
        if result == KERN_SUCCESS {
            let page = UInt64(vm_page_size)
            let available = (UInt64(vmstat.free_count) + UInt64(vmstat.inactive_count) + UInt64(vmstat.purgeable_count)) * page
            resources["memoryAvailableBytes"] = Int64(min(available, UInt64(Int64.max)))
            let physical = ProcessInfo.processInfo.physicalMemory
            let used = physical - min(available, physical)
            resources["memoryUsedBytes"] = Int64(min(used, UInt64(Int64.max)))
        } else {
            resources["memoryAvailableBytesAvailable"] = false
            reasons.append("memory_stats_unavailable")
        }
        return resources
    }

    private func cpuLoad() -> Double? {
        var cpuInfo: processor_info_array_t?
        var cpuCount: mach_msg_type_number_t = 0
        var infoCount: mach_msg_type_number_t = 0
        guard host_processor_info(mach_host_self(), PROCESSOR_CPU_LOAD_INFO, &cpuCount, &cpuInfo, &infoCount) == KERN_SUCCESS,
              let info = cpuInfo else { return nil }
        defer {
            vm_deallocate(mach_task_self_, vm_address_t(bitPattern: info), vm_size_t(infoCount) * vm_size_t(MemoryLayout<integer_t>.stride))
        }
        let stride = Int(CPU_STATE_MAX)
        var total: UInt64 = 0
        var idle: UInt64 = 0
        for index in 0..<Int(cpuCount) {
            let offset = index * stride
            total += (0..<stride).reduce(UInt64(0)) { $0 + UInt64(UInt32(bitPattern: info[offset + $1])) }
            idle += UInt64(UInt32(bitPattern: info[offset + Int(CPU_STATE_IDLE)]))
        }
        let sample = CPUSample(total: total, idle: idle)
        defer { previousCPU = sample }
        guard let previousCPU, sample.total > previousCPU.total, sample.idle >= previousCPU.idle else { return nil }
        let totalDelta = sample.total - previousCPU.total
        let idleDelta = min(sample.idle - previousCPU.idle, totalDelta)
        return max(0, min(100, (1 - Double(idleDelta) / Double(totalDelta)) * 100))
    }

    private func networkPayload(reasons: inout [String]) -> [String: Any] {
        var addresses: UnsafeMutablePointer<ifaddrs>?
        guard getifaddrs(&addresses) == 0, let first = addresses else {
            reasons.append("network_counters_unavailable")
            return ["available": false, "interfaces": [], "ratesAvailable": false, "reason": "getifaddrs_failed"]
        }
        defer { freeifaddrs(first) }
        let now = Date().timeIntervalSince1970
        var rows: [[String: Any]] = []
        var seen = Set<String>()
        var cursor: UnsafeMutablePointer<ifaddrs>? = first
        while let current = cursor {
            defer { cursor = current.pointee.ifa_next }
            guard let address = current.pointee.ifa_addr, address.pointee.sa_family == UInt8(AF_LINK),
                  let namePointer = current.pointee.ifa_name,
                  let dataPointer = current.pointee.ifa_data else { continue }
            let name = String(cString: namePointer)
            guard !name.isEmpty, seen.insert(name).inserted else { continue }
            let data = dataPointer.assumingMemoryBound(to: if_data.self).pointee
            let sample = NetworkSample(timestamp: now, bytesIn: UInt64(data.ifi_ibytes), bytesOut: UInt64(data.ifi_obytes),
                                        packetsIn: UInt64(data.ifi_ipackets), packetsOut: UInt64(data.ifi_opackets))
            var row: [String: Any] = ["name": name, "bytesIn": Int64(sample.bytesIn), "bytesOut": Int64(sample.bytesOut),
                                      "packetsIn": Int64(sample.packetsIn), "packetsOut": Int64(sample.packetsOut)]
            if let previous = previousNetwork[name], sample.timestamp > previous.timestamp {
                let seconds = sample.timestamp - previous.timestamp
                if seconds > 0 && sample.bytesIn >= previous.bytesIn && sample.bytesOut >= previous.bytesOut {
                    row["bytesInPerSecond"] = Double(sample.bytesIn - previous.bytesIn) / seconds
                    row["bytesOutPerSecond"] = Double(sample.bytesOut - previous.bytesOut) / seconds
                    row["ratesAvailable"] = true
                } else {
                    row["ratesAvailable"] = false
                    row["reason"] = "counter_reset_or_invalid_interval"
                }
            }
            rows.append(row)
            previousNetwork[name] = sample
        }
        rows.sort { ($0["name"] as? String ?? "") < ($1["name"] as? String ?? "") }
        let ratesAvailable = rows.contains { $0["ratesAvailable"] as? Bool == true }
        return ["available": true, "interfaces": rows, "ratesAvailable": ratesAvailable]
    }

    private func batteryPayload(reasons: inout [String]) -> [String: Any] {
        var result = NativePowerDetails.reading()
        guard let copied = IOPSCopyPowerSourcesInfo(),
              let sources = IOPSCopyPowerSourcesList(copied.takeUnretainedValue()) else {
            result["available"] = false
            result["reason"] = "power_source_unavailable"
            reasons.append("battery_unavailable")
            return result
        }
        let blob = copied.takeRetainedValue()
        let list = sources.takeRetainedValue() as [CFTypeRef]
        for source in list.prefix(4) {
            guard let description = IOPSGetPowerSourceDescription(blob, source)?.takeUnretainedValue() as? [String: Any],
                  description[kIOPSTypeKey] as? String == kIOPSInternalBatteryType,
                  let current = description[kIOPSCurrentCapacityKey] as? Int,
                  let maximum = description[kIOPSMaxCapacityKey] as? Int,
                  maximum > 0, current >= 0, current <= maximum else { continue }
            result["available"] = true
            result["percent"] = (Double(current) / Double(maximum)) * 100
            if let charging = description[kIOPSIsChargingKey] as? Bool { result["charging"] = charging }
            if let remaining = description[kIOPSTimeToEmptyKey] as? Int, (0...10_080).contains(remaining) {
                result["timeRemainingSeconds"] = remaining * 60
            }
            return result
        }
        reasons.append("battery_unavailable")
        result["available"] = false
        result["reason"] = "internal_battery_unavailable"
        return result
    }

    private func listeningPortsPayload() async -> [String: Any] {
        let tool = URL(fileURLWithPath: "/usr/sbin/lsof")
        guard FileManager.default.isExecutableFile(atPath: tool.path) else {
            return ["available": false, "ports": [], "reason": "fixed_tool_unavailable"]
        }
        do {
            let request = ScanRequest(executable: tool,
                                      arguments: ["-nP", "-iTCP", "-sTCP:LISTEN", "-Fpcn"],
                                      deadline: 2, stdoutLimit: 256 * 1024, stderrLimit: 16 * 1024)
            let outcome = try await ProcessScanRunner().run(request)
            guard !outcome.truncated else { throw NativeStorageError.outputLimit }
            let ports = parseListeningPorts(outcome.stdout)
            return ["available": true, "ports": ports]
        } catch {
            return ["available": false, "ports": [], "reason": "fixed_tool_failed"]
        }
    }

    private func parseListeningPorts(_ data: Data) -> [[String: Any]] {
        guard let text = String(data: data, encoding: .utf8) else { return [] }
        var pid: Int?
        var command: String?
        var result: [[String: Any]] = []
        for line in text.split(whereSeparator: \.isNewline) {
            guard let field = line.first else { continue }
            let value = String(line.dropFirst())
            switch field {
            case "p": pid = Int(value)
            case "c": command = value
            case "n":
                guard let separator = value.lastIndex(of: ":"), let port = Int(value[value.index(after: separator)...]) else { continue }
                var row: [String: Any] = ["port": port, "address": String(value[..<separator])]
                if let pid { row["pid"] = pid }
                if let command { row["process"] = command }
                result.append(row)
                if result.count >= 512 { return result }
            default: continue
            }
        }
        return result
    }

    private func totals(events: [[String: Any]], since: Date) -> [String: Any] {
        var count = 0
        var source: Int64 = 0
        var output: Int64 = 0
        var saved: Int64 = 0
        for event in events {
            guard let timestamp = event["timestamp"] as? String, let date = isoFormatter.date(from: timestamp), date >= since,
                  event["kind"] as? String == "compression" else { continue }
            count += 1
            source += event["sourceBytes"] as? Int64 ?? Int64(event["sourceBytes"] as? Int ?? 0)
            output += event["outputBytes"] as? Int64 ?? Int64(event["outputBytes"] as? Int ?? 0)
            saved += event["measuredSavedBytes"] as? Int64 ?? Int64(event["measuredSavedBytes"] as? Int ?? 0)
        }
        return ["compressions": count, "sourceBytes": source, "outputBytes": output,
                "measuredSavedBytes": saved]
    }

    private func loadEvents() throws -> [[String: Any]] {
        guard let object = try readJSON(fileName: "activity-events.json"), let events = object["events"] as? [[String: Any]] else { return [] }
        return Array(events.suffix(512))
    }

    private func persist(_ object: [String: Any], fileName: String) throws {
        guard JSONSerialization.isValidJSONObject(object) else { throw NativeStorageError.invalidJSON }
        let data = try JSONSerialization.data(withJSONObject: object, options: [.sortedKeys])
        let directoryFD = try openStateDirectory()
        defer { close(directoryFD) }
        let temporaryName = ".\(fileName).\(UUID().uuidString).tmp"
        let fd = temporaryName.withCString { openat(directoryFD, $0, O_WRONLY | O_CREAT | O_EXCL | O_NOFOLLOW | O_CLOEXEC, 0o600) }
        guard fd >= 0 else { throw NativeStorageError.persistence }
        var isOpen = true
        do {
            try writeAll(data, to: fd)
            guard fsync(fd) == 0 else { throw NativeStorageError.persistence }
            close(fd)
            isOpen = false
            try validateDestination(directoryFD: directoryFD, fileName: fileName)
            let renameResult = temporaryName.withCString { temporary in
                fileName.withCString { destination in renameat(directoryFD, temporary, directoryFD, destination) }
            }
            guard renameResult == 0, fsync(directoryFD) == 0 else { throw NativeStorageError.persistence }
        } catch {
            if isOpen { close(fd) }
            _ = temporaryName.withCString { unlinkat(directoryFD, $0, 0) }
            throw error
        }
    }

    private func readJSON(fileName: String) throws -> [String: Any]? {
        let directoryFD = try openStateDirectory()
        defer { close(directoryFD) }
        let fd = fileName.withCString { openat(directoryFD, $0, O_RDONLY | O_NOFOLLOW | O_CLOEXEC) }
        guard fd >= 0 else {
            if errno == ENOENT { return nil }
            throw NativeStorageError.persistence
        }
        defer { close(fd) }
        var value = stat()
        guard fstat(fd, &value) == 0, (value.st_mode & S_IFMT) == S_IFREG,
              value.st_uid == getuid(), (value.st_mode & 0o077) == 0,
              value.st_nlink == 1, value.st_size >= 0, value.st_size <= 8 * 1024 * 1024 else {
            throw NativeStorageError.persistence
        }
        let data = try readAll(fd: fd, count: Int(value.st_size))
        guard let object = try JSONSerialization.jsonObject(with: data) as? [String: Any] else { throw NativeStorageError.invalidJSON }
        return object
    }

    private func openStateDirectory() throws -> Int32 {
        guard stateDirectory.isFileURL, stateDirectory.path.hasPrefix("/"), stateDirectory.path != "/",
              !stateDirectory.pathComponents.contains("."), !stateDirectory.pathComponents.contains("..") else {
            throw NativeStorageError.unsafePath
        }
        var descriptor = open("/", O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)
        guard descriptor >= 0 else { throw NativeStorageError.persistence }
        for component in stateDirectory.path.split(separator: "/", omittingEmptySubsequences: true) {
            let name = String(component)
            var next = name.withCString { openat(descriptor, $0, O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC) }
            if next < 0, errno == ENOENT {
                let made = name.withCString { mkdirat(descriptor, $0, 0o700) }
                guard made == 0 || errno == EEXIST else { close(descriptor); throw NativeStorageError.persistence }
                next = name.withCString { openat(descriptor, $0, O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC) }
            }
            guard next >= 0 else { close(descriptor); throw errno == ELOOP ? NativeStorageError.unsafePath : NativeStorageError.persistence }
            close(descriptor)
            descriptor = next
        }
        var state = stat()
        guard fstat(descriptor, &state) == 0, (state.st_mode & S_IFMT) == S_IFDIR,
              state.st_uid == getuid(), (state.st_mode & 0o077) == 0 else {
            close(descriptor)
            throw NativeStorageError.unsafePath
        }
        return descriptor
    }

    private func validateDestination(directoryFD: Int32, fileName: String) throws {
        var existing = stat()
        let result = fileName.withCString { fstatat(directoryFD, $0, &existing, AT_SYMLINK_NOFOLLOW) }
        guard result == 0 else {
            if errno == ENOENT { return }
            throw NativeStorageError.persistence
        }
        guard (existing.st_mode & S_IFMT) == S_IFREG, existing.st_uid == getuid(),
              (existing.st_mode & 0o077) == 0, existing.st_nlink == 1 else {
            throw NativeStorageError.persistence
        }
    }

    private func writeAll(_ data: Data, to descriptor: Int32) throws {
        try data.withUnsafeBytes { bytes in
            guard let base = bytes.baseAddress else { return }
            var offset = 0
            while offset < bytes.count {
                let written = Darwin.write(descriptor, base.advanced(by: offset), bytes.count - offset)
                if written < 0, errno == EINTR { continue }
                guard written > 0 else { throw NativeStorageError.persistence }
                offset += written
            }
        }
    }

    private func readAll(fd: Int32, count: Int) throws -> Data {
        var data = Data(capacity: count)
        var buffer = [UInt8](repeating: 0, count: min(64 * 1024, max(1, count)))
        while data.count < count {
            let bytes = Darwin.read(fd, &buffer, min(buffer.count, count - data.count))
            if bytes < 0, errno == EINTR { continue }
            guard bytes >= 0 else { throw NativeStorageError.persistence }
            if bytes == 0 { break }
            data.append(buffer, count: bytes)
        }
        return data
    }

    private func compressionOutputIdentity(at url: URL) throws -> CompressionOutputIdentity {
        guard url.isFileURL, url.path.hasPrefix("/"),
              !url.pathComponents.contains("."), !url.pathComponents.contains(".."),
              !url.lastPathComponent.isEmpty else {
            throw MediaCompressionError.invalidRequest
        }
        let parent = url.deletingLastPathComponent()
        let parentFD = try openPreviewDirectory(parent)
        defer { close(parentFD) }
        var value = stat()
        let result = url.lastPathComponent.withCString {
            fstatat(parentFD, $0, &value, AT_SYMLINK_NOFOLLOW)
        }
        guard result == 0 else { throw MediaCompressionError.outputMetadataUnavailable }
        guard (value.st_mode & S_IFMT) == S_IFREG,
              value.st_size > 0, value.st_dev != 0, value.st_ino != 0,
              value.st_flags & UInt32(SF_DATALESS) == 0 else {
            throw MediaCompressionError.outputNotRegular
        }
        return CompressionOutputIdentity(
            url: url.standardizedFileURL,
            device: UInt64(UInt32(bitPattern: value.st_dev)),
            inode: UInt64(value.st_ino),
            size: Int64(value.st_size),
            modifiedSeconds: Int64(value.st_mtimespec.tv_sec),
            modifiedNanoseconds: Int64(value.st_mtimespec.tv_nsec),
            changedSeconds: Int64(value.st_ctimespec.tv_sec),
            changedNanoseconds: Int64(value.st_ctimespec.tv_nsec)
        )
    }

    private func openPreviewDirectory(_ url: URL) throws -> Int32 {
        guard url.isFileURL, url.path.hasPrefix("/"),
              !url.pathComponents.contains("."), !url.pathComponents.contains("..") else {
            throw MediaCompressionError.invalidRequest
        }
        var descriptor = open("/", O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)
        guard descriptor >= 0 else { throw MediaCompressionError.outputMetadataUnavailable }
        for component in url.path.split(separator: "/", omittingEmptySubsequences: true) {
            let name = String(component)
            let next = name.withCString {
                openat(descriptor, $0, O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)
            }
            guard next >= 0 else {
                let error = errno
                close(descriptor)
                throw error == ELOOP ? MediaCompressionError.unsafeAncestor : MediaCompressionError.outputMetadataUnavailable
            }
            close(descriptor)
            descriptor = next
        }
        return descriptor
    }
}

private struct CompressionOutputIdentity: Equatable {
    let url: URL
    let device: UInt64
    let inode: UInt64
    let size: Int64
    let modifiedSeconds: Int64
    let modifiedNanoseconds: Int64
    let changedSeconds: Int64
    let changedNanoseconds: Int64

    var dictionary: [String: Any] {
        [
            "device": device,
            "inode": inode,
            "size": size,
            "modifiedSeconds": modifiedSeconds,
            "modifiedNanoseconds": modifiedNanoseconds,
            "changedSeconds": changedSeconds,
            "changedNanoseconds": changedNanoseconds
        ]
    }
}

private enum NativeStorageError: Error {
    case unavailable
    case timeout
    case outputLimit
    case invalidJSON
    case persistence
    case unsafePath
}
