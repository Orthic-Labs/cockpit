import AppKit
import Darwin
import Foundation

/// Read-only, bounded application detail observations for the Mac dashboard.
///
/// This adapter only attributes paths whose ownership is an exact bundle ID.
/// It never follows symlinks, hydrates cloud placeholders, contacts update
/// feeds, or performs an uninstall/cleanup action.
@MainActor
public final class NativeApplicationDetails {
    public enum Error: Swift.Error, Equatable {
        case invalidApplicationPath
        case invalidBundleIdentifier
        case persistence
        case invalidHistory
    }

    private struct Totals {
        var logical: Int64 = 0
        var allocation: Int64 = 0
        var overflowed = false
        var incomplete = false
        var entries = 0
        var skippedSymlinks = 0
        var skippedPlaceholders = 0

        mutating func add(logicalBytes: Int64, allocationBytes: Int64) {
            let (nextLogical, logicalOverflow) = logical.addingReportingOverflow(logicalBytes)
            let (nextAllocation, allocationOverflow) = allocation.addingReportingOverflow(allocationBytes)
            logical = logicalOverflow ? Int64.max : nextLogical
            allocation = allocationOverflow ? Int64.max : nextAllocation
            overflowed = overflowed || logicalOverflow || allocationOverflow
        }
    }

    private let stateDirectory: URL
    private let historyFileName = "application-history.json"
    private let maxHistoryRecords = 1_024
    private let maxInventoryRows = 512
    private let maxFootprintEntries = 20_000
    private let maxFootprintDepth = 64
    private let maxFootprintBytes: Int64 = 64 * 1024 * 1024 * 1024 * 1024
    private let maxHistoryBytes = 8 * 1024 * 1024
    private let dataLessFlag: UInt32 = 0x4000_0000

    public init(stateDirectory: URL) {
        self.stateDirectory = stateDirectory.standardizedFileURL
    }

    /// Return app metadata, exact related paths, local update-feed metadata,
    /// and currently running process identities for one verified app bundle.
    public func details(appPath: String, bundleID: String?) async throws -> [String: Any] {
        let appURL = try verifiedApplicationURL(appPath: appPath, bundleID: bundleID)
        guard let bundle = Bundle(url: appURL), let actualBundleID = bundle.bundleIdentifier,
              validBundleIdentifier(actualBundleID) else {
            throw Error.invalidBundleIdentifier
        }

        var reasons: [String] = []
        let footprint = footprintPayload(appURL: appURL, bundleID: actualBundleID, reasons: &reasons)
        let process = processPayload(appURL: appURL, bundleID: actualBundleID)
        if process["coverage"] as? String == "partial" {
            reasons.append("process_coverage_partial")
        }

        var result: [String: Any] = [
            "schemaVersion": 1,
            "path": appURL.path,
            "bundleID": actualBundleID,
            "name": (bundle.object(forInfoDictionaryKey: "CFBundleDisplayName") as? String)
                ?? (bundle.object(forInfoDictionaryKey: "CFBundleName") as? String)
                ?? appURL.deletingPathExtension().lastPathComponent,
            "footprint": footprint,
            "processes": process,
            "updateFeed": updateFeedPayload(bundle: bundle),
            "reasons": reasons
        ]
        if let version = (bundle.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String), !version.isEmpty {
            result["version"] = version
        } else if let build = (bundle.object(forInfoDictionaryKey: "CFBundleVersion") as? String), !build.isEmpty {
            result["version"] = build
        }
        return result
    }

    /// Record positive inventory evidence. Missing apps become confirmed gone
    /// only when caller supplies qualified, complete source coverage.
    public func recordInventory(_ apps: [[String: Any]], coverageComplete: Bool) throws {
        var history = try loadHistory()
        let now = Int64(Date().timeIntervalSince1970)
        var currentKeys = Set<String>()
        var accepted = 0
        var malformedInput = apps.count > maxInventoryRows

        for app in apps.prefix(maxInventoryRows) {
            guard let path = app["path"] as? String, !path.isEmpty,
                  let bundleID = app["bundleID"] as? String,
                  path.hasPrefix("/"), URL(fileURLWithPath: path).pathExtension.caseInsensitiveCompare("app") == .orderedSame,
                  validBundleIdentifier(bundleID) else {
                malformedInput = true
                continue
            }
        }

        for app in apps.prefix(maxInventoryRows) {
            guard let path = app["path"] as? String, !path.isEmpty,
                  let bundleID = app["bundleID"] as? String,
                  path.hasPrefix("/"), URL(fileURLWithPath: path).pathExtension.caseInsensitiveCompare("app") == .orderedSame,
                  validBundleIdentifier(bundleID) else {
                malformedInput = true
                continue
            }
            let canonicalPath = URL(fileURLWithPath: path).standardizedFileURL.path
            let key = historyKey(bundleID: bundleID, path: canonicalPath)
            currentKeys.insert(key)
            let name = (app["name"] as? String)?.trimmingCharacters(in: .whitespacesAndNewlines)
            let version = (app["version"] as? String)?.trimmingCharacters(in: .whitespacesAndNewlines)
            if var existing = history[key] {
                existing["path"] = canonicalPath
                existing["bundleID"] = bundleID
                existing["historyKey"] = key
                if let name, !name.isEmpty { existing["name"] = name }
                if let version, !version.isEmpty { existing["version"] = version }
                existing["lastSeenAt"] = now
                existing["state"] = "installed"
                existing["lastCoverage"] = coverageComplete && !malformedInput ? "complete" : "partial"
                history[key] = existing
            } else {
                history[key] = [
                    "bundleID": bundleID,
                    "historyKey": key,
                    "path": canonicalPath,
                    "name": name ?? bundleID,
                    "version": version ?? NSNull(),
                    "firstSeenAt": now,
                    "lastSeenAt": now,
                    "state": "installed",
                    "lastCoverage": coverageComplete && !malformedInput ? "complete" : "partial"
                ]
            }
            accepted += 1
            if accepted >= maxInventoryRows { break }
        }

        for key in Array(history.keys) where !currentKeys.contains(key) {
            guard var entry = history[key] else { continue }
            if coverageComplete && !malformedInput {
                entry["state"] = "confirmedGone"
                entry["lastCoverage"] = "complete"
            } else {
                // An incomplete source cannot establish disappearance. Keep
                // positive history while making current liveness unknown.
                entry["state"] = "unknown"
                entry["lastCoverage"] = "partial"
            }
            history[key] = entry
        }

        let qualified = coverageComplete && !malformedInput
        // Normalize every row after the full input pass. A malformed row can
        // appear after valid rows and must downgrade their coverage too.
        for key in Array(history.keys) {
            guard var entry = history[key] else { continue }
            entry["lastCoverage"] = qualified ? "complete" : "partial"
            if !qualified, entry["state"] as? String == "confirmedGone" {
                entry["state"] = "unknown"
            }
            history[key] = entry
        }
        let bounded = boundedHistory(history)
        let payload: [String: Any] = [
            "schemaVersion": 1,
            "updatedAt": now,
            "coverageComplete": qualified,
            "coverageState": qualified ? "complete" : "partial",
            "records": bounded.values.sorted { historySortKey($0) < historySortKey($1) }
        ]
        try persist(payload, fileName: historyFileName)
    }

    /// Return persisted positive history and its current coverage qualification.
    /// Persistence/read failures remain explicit and never become an empty
    /// claim that an app disappeared.
    public func historyPayload() -> [String: Any] {
        do {
            let object = try readJSON(fileName: historyFileName)
            guard let object else {
                return [
                    "schemaVersion": 1,
                    "available": true,
                    "coverageState": "unknown",
                    "qualifiedForDisappearance": false,
                    "records": [],
                    "reason": "history_not_recorded"
                ]
            }
            let validated = try validateHistory(object)
            let coverageComplete = validated.coverageComplete
            return [
                "schemaVersion": 1,
                "available": true,
                "coverageState": coverageComplete ? "complete" : "partial",
                "qualifiedForDisappearance": coverageComplete,
                "records": validated.records.values.sorted { historySortKey($0) < historySortKey($1) },
                "reason": coverageComplete ? NSNull() : "source_coverage_incomplete"
            ]
        } catch {
            return [
                "schemaVersion": 1,
                "available": false,
                "coverageState": "unknown",
                "qualifiedForDisappearance": false,
                "records": [],
                "reason": "history_unavailable"
            ]
        }
    }

    private func footprintPayload(appURL: URL, bundleID: String, reasons: inout [String]) -> [String: Any] {
        let home = FileManager.default.homeDirectoryForCurrentUser
        let library = home.appendingPathComponent("Library", isDirectory: true)
        let candidates: [(String, String, URL)] = [
            ("application_bundle", "ApplicationBundle", appURL),
            ("preferences", "Preferences", library.appendingPathComponent("Preferences", isDirectory: true).appendingPathComponent("\(bundleID).plist")),
            ("caches", "Caches", library.appendingPathComponent("Caches", isDirectory: true).appendingPathComponent(bundleID, isDirectory: true)),
            ("application_support", "ApplicationSupport", library.appendingPathComponent("Application Support", isDirectory: true).appendingPathComponent(bundleID, isDirectory: true)),
            ("logs", "Logs", library.appendingPathComponent("Logs", isDirectory: true).appendingPathComponent(bundleID, isDirectory: true)),
            ("containers", "Container", library.appendingPathComponent("Containers", isDirectory: true).appendingPathComponent(bundleID, isDirectory: true))
        ]

        var totals = Totals()
        var seenIdentities = Set<String>()
        let deadline = ProcessInfo.processInfo.systemUptime + 5
        var paths: [[String: Any]] = []
        for (label, kind, url) in candidates {
            var pathReasons: [String] = []
            let observation = inspectPath(url, deadline: deadline, seenIdentities: &seenIdentities,
                                          totals: &totals, reasons: &pathReasons)
            var row: [String: Any] = [
                "label": label,
                "kind": kind,
                "path": url.path,
                "ownership": "exact_bundle_id",
                "shared": false,
                "preselected": false,
                "selectionEligible": false,
                "disposition": "report_only",
                "status": observation["status"] ?? "unknown",
                "coverage": observation["coverage"] ?? "unknown",
                "logicalBytes": observation["logicalBytes"] ?? NSNull(),
                "observedAllocationBytes": observation["observedAllocationBytes"] ?? NSNull(),
                "allocationBasis": "st_blocks_times_512_observed_not_reclaim"
            ]
            if (observation["status"] as? String) != "present" && label == "application_bundle" {
                pathReasons.append("validated_bundle_not_measurable")
            }
            if !pathReasons.isEmpty {
                row["reasons"] = pathReasons
                reasons.append(contentsOf: pathReasons.map { "\(label):\($0)" })
            }
            paths.append(row)
        }

        return [
            "schemaVersion": 1,
            "paths": paths,
            "totals": [
                "logicalBytes": totals.logical,
                "observedAllocationBytes": totals.allocation,
                "allocationBasis": "st_blocks_times_512_observed_not_reclaim",
                "overflowed": totals.overflowed,
                "incomplete": totals.incomplete,
                "entryCount": totals.entries,
                "entryLimit": maxFootprintEntries,
                "byteLimit": maxFootprintBytes,
                "skippedSymlinks": totals.skippedSymlinks,
                "skippedPlaceholders": totals.skippedPlaceholders
            ],
            "excludedScopes": [
                ["scope": "group_containers", "preselected": false, "reason": "shared_or_group_container"],
                ["scope": "vendor_folders", "preselected": false, "reason": "vendor_ownership_not_exact"],
                ["scope": "user_data", "preselected": false, "reason": "user_created_data"]
            ]
        ]
    }

    private func inspectPath(_ url: URL, deadline: TimeInterval, seenIdentities: inout Set<String>,
                             totals: inout Totals, reasons: inout [String]) -> [String: Any] {
        guard ProcessInfo.processInfo.systemUptime < deadline else {
            totals.incomplete = true
            reasons.append("footprint_time_limit")
            return ["status": "incomplete", "coverage": "incomplete"]
        }
        switch ancestorStatus(url) {
        case .unsafe:
            totals.incomplete = true
            reasons.append("symlink_ancestor_not_followed")
            return ["status": "excluded", "coverage": "incomplete"]
        case .denied:
            totals.incomplete = true
            reasons.append("ancestor_denied")
            return ["status": "denied", "coverage": "denied"]
        case .safe, .missing:
            break
        }
        var root = stat()
        guard lstat(url.path, &root) == 0 else {
            let status = errno == ENOENT ? "missing" : (errno == EACCES || errno == EPERM ? "denied" : "unavailable")
            let coverage = status == "missing" ? "missing" : status
            if status == "denied" || status == "unavailable" { totals.incomplete = true }
            reasons.append(status == "missing" ? "path_missing" : "path_\(status)")
            return ["status": status, "coverage": coverage]
        }
        if root.st_mode & S_IFMT == S_IFLNK {
            totals.skippedSymlinks += 1
            totals.incomplete = true
            reasons.append("symlink_not_followed")
            return ["status": "excluded", "coverage": "incomplete"]
        }
        if isDataLess(root) || isCloudPlaceholder(url) {
            totals.skippedPlaceholders += 1
            totals.incomplete = true
            reasons.append("placeholder_not_hydrated")
            return ["status": "excluded", "coverage": "incomplete"]
        }

        var local = Totals()
        var stack: [(URL, Int)] = [(url, 0)]
        var denied = false
        while let (current, depth) = stack.popLast() {
            if ProcessInfo.processInfo.systemUptime >= deadline {
                local.incomplete = true
                reasons.append("footprint_time_limit")
                break
            }
            guard totals.entries + local.entries < maxFootprintEntries else {
                local.incomplete = true
                reasons.append("entry_limit")
                break
            }
            if depth > maxFootprintDepth {
                local.incomplete = true
                reasons.append("depth_limit")
                continue
            }
            switch ancestorStatus(current) {
            case .safe:
                break
            case .missing:
                local.incomplete = true
                reasons.append("entry_ancestor_disappeared")
                continue
            case .denied:
                local.incomplete = true
                denied = true
                reasons.append("entry_ancestor_denied")
                continue
            case .unsafe:
                local.incomplete = true
                reasons.append("entry_symlink_ancestor_not_followed")
                continue
            }
            var value = stat()
            guard lstat(current.path, &value) == 0 else {
                local.incomplete = true
                denied = denied || errno == EACCES || errno == EPERM
                reasons.append(errno == ENOENT ? "entry_disappeared" : "entry_unavailable")
                continue
            }
            if value.st_mode & S_IFMT == S_IFLNK {
                local.skippedSymlinks += 1
                local.incomplete = true
                continue
            }
            if isDataLess(value) || isCloudPlaceholder(current) {
                local.skippedPlaceholders += 1
                local.incomplete = true
                continue
            }
            local.entries += 1
            let identity = "\(value.st_dev):\(value.st_ino)"
            if !seenIdentities.insert(identity).inserted { continue }
            if value.st_mode & S_IFMT == S_IFDIR {
                let remaining = maxFootprintEntries - totals.entries - local.entries - stack.count
                let children = readDirectoryChildren(current, limit: max(0, remaining), deadline: deadline,
                                                     reasons: &reasons)
                if children.truncated {
                    local.incomplete = true
                    reasons.append("directory_entry_limit")
                }
                if children.denied {
                    local.incomplete = true
                    denied = true
                    reasons.append("directory_unreadable")
                }
                stack.append(contentsOf: children.urls.map { ($0, depth + 1) })
            } else {
                let logical = max(0, Int64(value.st_size))
                let blocks = max(0, Int64(value.st_blocks))
                let (allocationValue, allocationOverflow) = blocks.multipliedReportingOverflow(by: 512)
                let allocation = allocationOverflow ? maxFootprintBytes : allocationValue
                if totals.logical > maxFootprintBytes - min(maxFootprintBytes, local.logical)
                    || logical > maxFootprintBytes - min(maxFootprintBytes, totals.logical + local.logical) {
                    local.logical = maxFootprintBytes
                    local.allocation = maxFootprintBytes
                    local.overflowed = true
                    local.incomplete = true
                    reasons.append("byte_limit")
                } else if allocationOverflow || totals.allocation > maxFootprintBytes - min(maxFootprintBytes, local.allocation)
                            || allocation > maxFootprintBytes - min(maxFootprintBytes, totals.allocation + local.allocation) {
                    local.add(logicalBytes: logical, allocationBytes: maxFootprintBytes)
                    local.overflowed = true
                    local.incomplete = true
                    reasons.append("allocation_byte_limit")
                } else {
                    local.add(logicalBytes: logical, allocationBytes: allocation)
                }
            }
        }

        totals.entries += local.entries
        totals.skippedSymlinks += local.skippedSymlinks
        totals.skippedPlaceholders += local.skippedPlaceholders
        totals.incomplete = totals.incomplete || local.incomplete
        totals.overflowed = totals.overflowed || local.overflowed
        totals.logical = min(maxFootprintBytes, totals.logical + local.logical)
        totals.allocation = min(maxFootprintBytes, totals.allocation + local.allocation)
        let status = denied ? "denied" : (local.incomplete ? "incomplete" : "present")
        return [
            "status": status,
            "coverage": local.incomplete ? "incomplete" : "complete",
            "logicalBytes": local.logical,
            "observedAllocationBytes": local.allocation,
            "entryCount": local.entries,
            "skippedSymlinks": local.skippedSymlinks,
            "skippedPlaceholders": local.skippedPlaceholders
        ]
    }

    private struct DirectoryChildren {
        let urls: [URL]
        let truncated: Bool
        let denied: Bool
    }

    private func readDirectoryChildren(_ url: URL, limit: Int, deadline: TimeInterval,
                                       reasons: inout [String]) -> DirectoryChildren {
        guard limit > 0, let directory = opendir(url.path) else {
            return DirectoryChildren(urls: [], truncated: limit <= 0, denied: limit > 0)
        }
        defer { closedir(directory) }
        var urls: [URL] = []
        var truncated = false
        while let entry = readdir(directory) {
            if ProcessInfo.processInfo.systemUptime >= deadline {
                truncated = true
                reasons.append("footprint_time_limit")
                break
            }
            if urls.count >= limit {
                truncated = true
                break
            }
            let name = withUnsafeBytes(of: entry.pointee.d_name) { bytes -> String in
                let end = bytes.firstIndex(of: 0) ?? bytes.endIndex
                return String(decoding: bytes[..<end], as: UTF8.self)
            }
            guard !name.isEmpty, name != ".", name != ".." else { continue }
            urls.append(url.appendingPathComponent(name))
        }
        return DirectoryChildren(urls: urls, truncated: truncated, denied: false)
    }

    private enum AncestorState {
        case safe
        case missing
        case denied
        case unsafe
    }

    private func ancestorStatus(_ url: URL) -> AncestorState {
        var descriptor = open("/", O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)
        guard descriptor >= 0 else { return .denied }
        defer { close(descriptor) }
        let components = url.standardizedFileURL.pathComponents.dropFirst().dropLast()
        for component in components {
            var next = component.withCString { openat(descriptor, $0, O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC) }
            if next < 0 {
                let result: AncestorState = {
                    switch errno {
                    case ENOENT: return .missing
                    case EACCES, EPERM: return .denied
                    case ELOOP: return .unsafe
                    default: return .unsafe
                    }
                }()
                return result
            }
            close(descriptor)
            descriptor = next
        }
        return .safe
    }

    private func processPayload(appURL: URL, bundleID: String) -> [String: Any] {
        var rows: [[String: Any]] = []
        var reasons: [String] = ["command_line_agents_excluded", "NSWorkspace_launchDate_is_not_libproc_incarnation_verification"]
        let applications = NSWorkspace.shared.runningApplications
            .filter { $0.bundleIdentifier == bundleID }
            .sorted { $0.processIdentifier < $1.processIdentifier }
        for application in applications.prefix(64) {
            guard let runningURL = application.bundleURL?.standardizedFileURL,
                  runningURL.path == appURL.path else { continue }
            guard application.processIdentifier > 0 else { continue }
            guard let launchDate = application.launchDate,
                  launchDate.timeIntervalSince1970 >= 0 else {
                reasons.append("process_start_time_unavailable")
                continue
            }
            let identity: [String: Any] = [
                "pid": application.processIdentifier,
                "start_time": Int64(launchDate.timeIntervalSince1970)
            ]
            rows.append([
                "name": application.localizedName ?? bundleID,
                "bundleID": bundleID,
                "kind": "gui_application",
                "coverage": "partial",
                "identity": identity,
                "running": !application.isTerminated,
                "history": ["available": false, "reason": "explicit sampler input required"],
                "openFiles": ["available": false, "reason": "verified PID/start-time sampler required"],
                "network": ["available": false, "reason": "verified PID/start-time sampler required"]
            ])
        }
        let coverage = "partial"
        var result: [String: Any] = [
            "available": true,
            "coverage": coverage,
            "entries": rows,
            "identityBasis": "bundle_path_and_pid_NSWorkspace_launchDate",
            "excludedProcessKinds": ["command_line_agents"],
            "reasons": reasons
        ]
        if applications.count > 64 { result["truncated"] = true }
        return result
    }

    private func updateFeedPayload(bundle: Bundle) -> [String: Any] {
        guard let raw = bundle.object(forInfoDictionaryKey: "SUFeedURL") as? String,
              !raw.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else {
            return ["available": false, "networkPerformed": false, "status": "unavailable", "reason": "bundle_feed_url_not_declared"]
        }
        guard let components = URLComponents(string: raw),
              let scheme = components.scheme?.lowercased(), ["http", "https"].contains(scheme),
              let host = components.host, !host.isEmpty, components.user == nil,
              components.password == nil else {
            return ["available": false, "networkPerformed": false, "status": "invalid_url", "feedURL": raw]
        }
        return [
            "available": true,
            "networkPerformed": false,
            "status": "candidate_unavailable",
            "feedURL": raw,
            "source": "existing_bundle_SUFeedURL",
            "reason": "network_request_requires_explicit_request"
        ]
    }

    private func verifiedApplicationURL(appPath: String, bundleID: String?) throws -> URL {
        let url = URL(fileURLWithPath: appPath).standardizedFileURL
        guard url.isFileURL, url.path.hasPrefix("/"), url.pathExtension.caseInsensitiveCompare("app") == .orderedSame else {
            throw Error.invalidApplicationPath
        }
        guard case .safe = ancestorStatus(url) else { throw Error.invalidApplicationPath }
        var value = stat()
        guard lstat(url.path, &value) == 0, value.st_mode & S_IFMT == S_IFDIR,
              value.st_mode & S_IFMT != S_IFLNK, !isDataLess(value), !isCloudPlaceholder(url),
              let bundle = Bundle(url: url), let actual = bundle.bundleIdentifier,
              validBundleIdentifier(actual) else {
            throw Error.invalidApplicationPath
        }
        if let bundleID {
            guard validBundleIdentifier(bundleID), bundleID == actual else { throw Error.invalidBundleIdentifier }
        }
        return url
    }

    private func validBundleIdentifier(_ value: String) -> Bool {
        let value = value.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !value.isEmpty else { return false }
        return value.split(separator: ".", omittingEmptySubsequences: false).allSatisfy { part in
            !part.isEmpty && part.unicodeScalars.allSatisfy {
                ($0.value >= 48 && $0.value <= 57) || ($0.value >= 65 && $0.value <= 90)
                    || ($0.value >= 97 && $0.value <= 122) || $0.value == 45 || $0.value == 95
            }
        }
    }

    private func isDataLess(_ value: stat) -> Bool {
        (UInt32(value.st_flags) & dataLessFlag) != 0
    }

    private func isCloudPlaceholder(_ url: URL) -> Bool {
        guard let values = try? url.resourceValues(forKeys: [.isUbiquitousItemKey, .ubiquitousItemIsDownloadedKey]),
              values.isUbiquitousItem == true else { return false }
        return values.ubiquitousItemIsDownloaded != true
    }

    private func loadHistory() throws -> [String: [String: Any]] {
        guard let object = try readJSON(fileName: historyFileName) else { return [:] }
        return try validateHistory(object).records
    }

    private func boundedHistory(_ history: [String: [String: Any]]) -> [String: [String: Any]] {
        let sorted = history.values.sorted { historySortKey($0) > historySortKey($1) }
        return Dictionary(uniqueKeysWithValues: sorted.prefix(maxHistoryRecords).compactMap { record in
            guard let key = record["historyKey"] as? String else { return nil }
            return (key, record)
        })
    }

    private func historyKey(bundleID: String, path: String) -> String {
        "\(bundleID)|\(path)"
    }

    private func integerValue(_ value: Any?) -> Int64? {
        if let value = value as? Int64 { return value }
        if let value = value as? Int { return Int64(value) }
        if let value = value as? NSNumber {
            if String(cString: value.objCType) == "c" { return nil }
            return value.int64Value
        }
        return nil
    }

    private func validateHistory(_ object: [String: Any]) throws -> (records: [String: [String: Any]], coverageComplete: Bool) {
        guard integerValue(object["schemaVersion"]) == 1,
              let updatedAt = integerValue(object["updatedAt"]), updatedAt >= 0,
              let coverageComplete = object["coverageComplete"] as? Bool,
              let coverageState = object["coverageState"] as? String,
              coverageState == (coverageComplete ? "complete" : "partial"),
              let records = object["records"] as? [[String: Any]], records.count <= maxHistoryRecords else {
            throw Error.invalidHistory
        }
        var result: [String: [String: Any]] = [:]
        for record in records {
            guard let bundleID = record["bundleID"] as? String,
                  validBundleIdentifier(bundleID),
                  let historyKey = record["historyKey"] as? String,
                  let path = record["path"] as? String,
                  path.hasPrefix("/"),
                  URL(fileURLWithPath: path).pathExtension.caseInsensitiveCompare("app") == .orderedSame,
                  URL(fileURLWithPath: path).standardizedFileURL.path == path,
                  historyKey == self.historyKey(bundleID: bundleID, path: path),
                  let name = record["name"] as? String, !name.isEmpty,
                  let firstSeenAt = integerValue(record["firstSeenAt"]), firstSeenAt >= 0,
                  let lastSeenAt = integerValue(record["lastSeenAt"]), lastSeenAt >= firstSeenAt,
                  lastSeenAt <= updatedAt,
                  let state = record["state"] as? String,
                  ["installed", "confirmedGone", "unknown"].contains(state),
                  let lastCoverage = record["lastCoverage"] as? String,
                  ["complete", "partial"].contains(lastCoverage),
                  lastCoverage == (coverageComplete ? "complete" : "partial"),
                  state != "confirmedGone" || lastCoverage == "complete",
                  state != "unknown" || lastCoverage == "partial",
                  result[historyKey] == nil else {
                throw Error.invalidHistory
            }
            result[historyKey] = record
        }
        return (result, coverageComplete)
    }

    private func historySortKey(_ record: [String: Any]) -> Int64 {
        if let value = record["lastSeenAt"] as? Int64 { return value }
        if let value = record["lastSeenAt"] as? NSNumber { return value.int64Value }
        return 0
    }

    private func persist(_ object: [String: Any], fileName: String) throws {
        guard JSONSerialization.isValidJSONObject(object), fileName == historyFileName else { throw Error.persistence }
        let data: Data
        do { data = try JSONSerialization.data(withJSONObject: object, options: [.sortedKeys]) } catch { throw Error.persistence }
        guard data.count <= maxHistoryBytes else { throw Error.persistence }
        let directory = try openStateDirectory()
        defer { close(directory) }
        let temporary = ".\(fileName).\(UUID().uuidString).tmp"
        let fd = temporary.withCString { openat(directory, $0, O_WRONLY | O_CREAT | O_EXCL | O_NOFOLLOW | O_CLOEXEC, 0o600) }
        guard fd >= 0 else { throw Error.persistence }
        var open = true
        do {
            try writeAll(data, descriptor: fd)
            guard fsync(fd) == 0 else { throw Error.persistence }
            close(fd); open = false
            let result = temporary.withCString { source in fileName.withCString { target in renameat(directory, source, directory, target) } }
            guard result == 0, fsync(directory) == 0 else { throw Error.persistence }
        } catch {
            if open { close(fd) }
            _ = temporary.withCString { unlinkat(directory, $0, 0) }
            throw Error.persistence
        }
    }

    private func readJSON(fileName: String) throws -> [String: Any]? {
        let directory = try openStateDirectory()
        defer { close(directory) }
        let fd = fileName.withCString { openat(directory, $0, O_RDONLY | O_NOFOLLOW | O_CLOEXEC) }
        guard fd >= 0 else {
            if errno == ENOENT { return nil }
            throw Error.persistence
        }
        defer { close(fd) }
        var value = stat()
        guard fstat(fd, &value) == 0, value.st_mode & S_IFMT == S_IFREG,
              value.st_uid == getuid(), value.st_nlink == 1, value.st_size >= 0,
              value.st_size <= 8 * 1024 * 1024, value.st_mode & 0o077 == 0 else {
            throw Error.persistence
        }
        var data = Data(capacity: Int(value.st_size))
        var buffer = [UInt8](repeating: 0, count: 64 * 1024)
        while data.count < Int(value.st_size) {
            let count = Darwin.read(fd, &buffer, min(buffer.count, Int(value.st_size) - data.count))
            if count < 0, errno == EINTR { continue }
            guard count >= 0 else { throw Error.persistence }
            if count == 0 { break }
            data.append(buffer, count: count)
        }
        guard let object = try JSONSerialization.jsonObject(with: data) as? [String: Any] else { throw Error.invalidHistory }
        return object
    }

    private func openStateDirectory() throws -> Int32 {
        guard stateDirectory.isFileURL, stateDirectory.path.hasPrefix("/"), stateDirectory.path != "/",
              !stateDirectory.pathComponents.contains("."), !stateDirectory.pathComponents.contains("..") else {
            throw Error.persistence
        }
        var descriptor = open("/", O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)
        guard descriptor >= 0 else { throw Error.persistence }
        for component in stateDirectory.path.split(separator: "/", omittingEmptySubsequences: true) {
            let name = String(component)
            var next = name.withCString { openat(descriptor, $0, O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC) }
            if next < 0, errno == ENOENT {
                let made = name.withCString { mkdirat(descriptor, $0, 0o700) }
                guard made == 0 || errno == EEXIST else { close(descriptor); throw Error.persistence }
                next = name.withCString { openat(descriptor, $0, O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC) }
            }
            guard next >= 0 else { close(descriptor); throw Error.persistence }
            close(descriptor); descriptor = next
        }
        var value = stat()
        guard fstat(descriptor, &value) == 0, value.st_mode & S_IFMT == S_IFDIR,
              value.st_uid == getuid(), value.st_mode & 0o077 == 0 else {
            close(descriptor); throw Error.persistence
        }
        return descriptor
    }

    private func writeAll(_ data: Data, descriptor: Int32) throws {
        try data.withUnsafeBytes { bytes in
            guard let base = bytes.baseAddress else { return }
            var offset = 0
            while offset < bytes.count {
                let count = Darwin.write(descriptor, base.advanced(by: offset), bytes.count - offset)
                if count < 0, errno == EINTR { continue }
                guard count > 0 else { throw Error.persistence }
                offset += count
            }
        }
    }
}
