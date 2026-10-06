import CoreServices
import Darwin
import Foundation

/// Errors raised when a filename-index request cannot be safely fulfilled.
public enum NativeFilenameIndexError: Error, LocalizedError {
    case invalidRoot
    case rootUnavailable
    case unsupportedNetwork
    case invalidQuery(String)
    case invalidStatePath
    case persistence

    public var errorDescription: String? {
        switch self {
        case .invalidRoot: return "Selected root must be an absolute local directory"
        case .rootUnavailable: return "Selected root is unavailable or unreadable"
        case .unsupportedNetwork: return "Filename index is unavailable for network filesystems"
        case .invalidQuery(let message): return message
        case .invalidStatePath: return "Filename-index state path is unsafe"
        case .persistence: return "Filename-index state could not be read or written"
        }
    }
}

/// Persistent, metadata-only filename search for one explicitly selected root.
///
/// The index is dormant until `refresh(root:)` starts a stream. It never opens
/// file contents, follows symlinks, hydrates cloud placeholders, or scans a
/// root that was not explicitly selected by the caller.
@MainActor
public final class NativeFilenameIndex {
    nonisolated private static let schemaVersion = 1
    nonisolated private static let stateFileName = "native-filename-index.json"
    nonisolated private static let maxEntries = 100_000
    nonisolated private static let maxScanSeconds: TimeInterval = 10
    nonisolated private static let maxPageLimit = 1_000
    nonisolated private static let maxPageOffset = 1_000_000
    nonisolated private static let maxQueryLength = 256
    nonisolated private static let maxExtensionLength = 64
    nonisolated private static let maxStateBytes = 64 * 1024 * 1024
    nonisolated private static let dataLessFlag: UInt32 = 0x4000_0000

    private struct IndexedRow: Codable, Sendable {
        let path: String
        let name: String
        let kind: String
        let sizeBytes: Int64?
        let sizeReason: String?
        let createdAt: Date?
        let creationDateReason: String?
        let modifiedAt: Date?
        let modificationDateReason: String?
    }

    private struct PersistedState: Codable, Sendable {
        let schemaVersion: Int
        let root: String
        let cursor: UInt64
        let rootDevice: UInt64?
        let rootFileID: UInt64?
        let rows: [IndexedRow]
        let incompleteReasons: [String]
        let staleReason: String?
        let rescanRequired: Bool
    }

    private struct EnumerationResult: Sendable {
        let rows: [IndexedRow]
        let cursor: UInt64
        let incompleteReasons: [String]
    }

    private struct RootInfo: Sendable {
        let url: URL
        let device: UInt64
        let fileID: UInt64
    }

    private struct Event: @unchecked Sendable {
        let path: String
        let flags: FSEventStreamEventFlags
        let id: FSEventStreamEventId
    }

    private final class StreamContext: @unchecked Sendable {
        weak var owner: NativeFilenameIndex?
        let generation: UInt64

        init(owner: NativeFilenameIndex, generation: UInt64) {
            self.owner = owner
            self.generation = generation
        }
    }

    private let stateDirectory: URL
    private var rows: [IndexedRow] = []
    private var rootPath: String?
    private var rootDevice: UInt64?
    private var rootFileID: UInt64?
    private var cursor: FSEventStreamEventId?
    private var incompleteReasons: [String] = []
    private var staleReason: String?
    private var rescanRequired = false
    private var persistenceReason: String?
    private var stream: FSEventStreamRef?
    private var streamContext: StreamContext?
    private var lifecycleToken: UInt64 = 0
    private var persistedReplayPending = false
    private var persistedStateResumeEligible = false

    private let isoFormatter: ISO8601DateFormatter = {
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return formatter
    }()

    public init(stateDirectory: URL) {
        self.stateDirectory = stateDirectory.standardizedFileURL
        loadPersistedState()
    }

    deinit {
        if let stream {
            FSEventStreamStop(stream)
            FSEventStreamInvalidate(stream)
            FSEventStreamRelease(stream)
        }
    }

    /// Rebuilds metadata for one selected root, then resumes its native journal.
    /// FSEvents position is captured before enumeration so changes made during
    /// this bounded walk are replayed by the newly started stream.
    public func refresh(root: URL) async throws -> [String: Any] {
        let selectedRoot = try Self.validateRoot(root)
        lifecycleToken &+= 1
        let token = lifecycleToken
        stopStream()

        if persistedReplayPending && persistedStateResumeEligible,
           rootPath == selectedRoot.url.path,
           rootDevice == selectedRoot.device, rootFileID == selectedRoot.fileID,
           let savedCursor = cursor, savedCursor > 0 {
            staleReason = "persisted_state_replay_pending"
            rescanRequired = true
            persistedReplayPending = true
            persistedStateResumeEligible = false
            startStream(from: savedCursor)
            if stream == nil { appendReason("change_journal_unavailable") }
            try? persistState()
            var payload = statusPayload()
            payload["resumed"] = true
            return payload
        }

        let stateDirectory = self.stateDirectory
        let scanDeadline = ProcessInfo.processInfo.systemUptime + Self.maxScanSeconds
        let startingCursor = FSEventsGetCurrentEventId()
        let result = try await Task.detached(priority: .utility) {
            try Self.enumerate(root: selectedRoot.url, stateDirectory: stateDirectory,
                               deadline: scanDeadline, cursor: startingCursor)
        }.value
        guard token == lifecycleToken else { throw CancellationError() }

        rows = result.rows
        rootPath = selectedRoot.url.path
        rootDevice = selectedRoot.device
        rootFileID = selectedRoot.fileID
        cursor = result.cursor
        incompleteReasons = result.incompleteReasons
        staleReason = nil
        rescanRequired = !incompleteReasons.isEmpty
        persistedReplayPending = false
        persistedStateResumeEligible = false
        persistenceReason = nil
        do {
            try persistState()
        } catch NativeFilenameIndexError.unsupportedNetwork {
            markUnsupportedNetwork()
            var payload = statusPayload()
            payload["refreshed"] = false
            return payload
        } catch {
            persistenceReason = "state_persistence_unavailable"
        }
        startStream(from: result.cursor)
        if stream == nil {
            appendReason("change_journal_unavailable")
        }
        do {
            try persistState()
        } catch NativeFilenameIndexError.unsupportedNetwork {
            markUnsupportedNetwork()
        } catch {
            persistenceReason = "state_persistence_unavailable"
        }
        var payload = statusPayload()
        payload["refreshed"] = true
        return payload
    }

    /// Searches only persisted metadata. Empty query matches every indexed name.
    public func query(payload: [String: Any]) throws -> [String: Any] {
        if let requestedRoot = try string(payload, keys: ["root", "root_path"]), requestedRoot != rootPath {
            throw NativeFilenameIndexError.invalidQuery("query root does not match selected root")
        }
        let query = try string(payload, keys: ["query", "substring", "name"]) ?? ""
        guard query.utf8.count <= Self.maxQueryLength, query.count <= Self.maxQueryLength,
              !query.contains("\0") else {
            throw NativeFilenameIndexError.invalidQuery("query must be at most 256 bytes/chars and contain no NUL")
        }

        let requestedExtension = try string(payload, keys: ["extension", "ext"])
        if let requestedExtension {
            let trimmed = requestedExtension.hasPrefix(".") ? String(requestedExtension.dropFirst()) : requestedExtension
            guard !trimmed.isEmpty, trimmed.utf8.count <= Self.maxExtensionLength,
                  trimmed.count <= Self.maxExtensionLength, !trimmed.contains("/"),
                  !trimmed.contains("\\"), !trimmed.contains("\0") else {
                throw NativeFilenameIndexError.invalidQuery("extension must be a non-empty filename suffix")
            }
        }

        let kind = try string(payload, keys: ["kind"])
        if let kind, !["file", "directory", "symlink", "other"].contains(kind.lowercased()) {
            throw NativeFilenameIndexError.invalidQuery("kind must be file, directory, symlink, or other")
        }
        let minSize = try size(payload, keys: ["min_size", "minSize"])
        let maxSize = try size(payload, keys: ["max_size", "maxSize"])
        if let minSize, let maxSize, minSize > maxSize {
            throw NativeFilenameIndexError.invalidQuery("minimum size exceeds maximum size")
        }

        let minDate = try date(payload, keys: ["min_date", "minDate"])
        let maxDate = try date(payload, keys: ["max_date", "maxDate"])
        let minCreated = try date(payload, keys: ["min_created_date", "minCreatedDate"])
        let maxCreated = try date(payload, keys: ["max_created_date", "maxCreatedDate"])
        let minModified = try date(payload, keys: ["min_modified_date", "minModifiedDate"])
        let maxModified = try date(payload, keys: ["max_modified_date", "maxModifiedDate"])

        let offset = try integer(payload, keys: ["offset", "page_offset"]) ?? 0
        let limit = try integer(payload, keys: ["limit", "page_limit"]) ?? 100
        guard limit > 0, limit <= Self.maxPageLimit else {
            throw NativeFilenameIndexError.invalidQuery("limit must be between 1 and 1000")
        }
        guard offset >= 0, offset <= Self.maxPageOffset else {
            throw NativeFilenameIndexError.invalidQuery("offset must be at most 1000000")
        }
        if let minDate, let maxDate, minDate > maxDate {
            throw NativeFilenameIndexError.invalidQuery("minimum date exceeds maximum date")
        }
        if let minCreated, let maxCreated, minCreated > maxCreated {
            throw NativeFilenameIndexError.invalidQuery("minimum creation date exceeds maximum")
        }
        if let minModified, let maxModified, minModified > maxModified {
            throw NativeFilenameIndexError.invalidQuery("minimum modification date exceeds maximum")
        }

        let foldedQuery = query.lowercased()
        let foldedExtension = requestedExtension?.trimmingCharacters(in: CharacterSet(charactersIn: ".")).lowercased()
        let foldedKind = kind?.lowercased()
        let matches = rows.filter { row in
            guard foldedQuery.isEmpty || row.name.lowercased().contains(foldedQuery) else { return false }
            guard foldedKind == nil || row.kind == foldedKind else { return false }
            if let foldedExtension {
                guard row.kind == "file", row.name.split(separator: ".").last.map(String.init)?.lowercased() == foldedExtension else { return false }
            }
            if let minSize, row.sizeBytes.map({ $0 >= minSize }) != true { return false }
            if let maxSize, row.sizeBytes.map({ $0 <= maxSize }) != true { return false }
            if let minDate, (row.modifiedAt ?? row.createdAt).map({ $0 >= minDate }) != true { return false }
            if let maxDate, (row.modifiedAt ?? row.createdAt).map({ $0 <= maxDate }) != true { return false }
            if let minCreated, row.createdAt.map({ $0 >= minCreated }) != true { return false }
            if let maxCreated, row.createdAt.map({ $0 <= maxCreated }) != true { return false }
            if let minModified, row.modifiedAt.map({ $0 >= minModified }) != true { return false }
            if let maxModified, row.modifiedAt.map({ $0 <= maxModified }) != true { return false }
            return true
        }.sorted { $0.path < $1.path }

        let end = min(matches.count, offset + limit)
        let page = offset < matches.count ? Array(matches[offset..<end]) : []
        var result: [String: Any] = [
            "schema_version": Self.schemaVersion,
            "root": rootPath ?? NSNull(),
            "items": page.map(rowPayload),
            "offset": offset,
            "limit": limit,
            "total_matches": matches.count,
            "has_more": end < matches.count,
            "incomplete": isIncomplete,
            "dates": dateAvailabilityPayload()
        ]
        result["stale"] = staleReason != nil
        result["rescan_required"] = rescanRequired
        if let staleReason { result["stale_reason"] = staleReason }
        return result
    }

    /// Stops native change-journal observation while retaining persisted rows.
    public func stop() {
        lifecycleToken &+= 1
        stopStream()
    }

    /// Current index, coverage, persistence, and change-journal state.
    public func statusPayload() -> [String: Any] {
        var result: [String: Any] = [
            "schema_version": Self.schemaVersion,
            "available": rootPath != nil && staleReason != "unsupported_network"
                && !incompleteReasons.contains("unsupported_network"),
            "root": rootPath ?? NSNull(),
            "entry_count": rows.count,
            "incomplete": isIncomplete,
            "stale": staleReason != nil,
            "rescan_required": rescanRequired,
            "watching": stream != nil,
            "dates": dateAvailabilityPayload(),
            "reasons": Array(Set(incompleteReasons)).sorted()
        ]
        if let cursor { result["cursor"] = NSNumber(value: cursor) }
        else { result["cursor"] = NSNull() }
        result["root_device"] = rootDevice.map { NSNumber(value: $0) } ?? NSNull()
        result["root_file_id"] = rootFileID.map { NSNumber(value: $0) } ?? NSNull()
        if let staleReason { result["stale_reason"] = staleReason }
        if let persistenceReason { result["persistence_reason"] = persistenceReason }
        return result
    }

    private var isIncomplete: Bool {
        !incompleteReasons.isEmpty || staleReason != nil || persistenceReason != nil
    }

    private func appendReason(_ reason: String) {
        if !incompleteReasons.contains(reason) { incompleteReasons.append(reason) }
    }

    private func markUnsupportedNetwork() {
        stopStream()
        staleReason = "unsupported_network"
        rescanRequired = true
        persistedReplayPending = false
        persistedStateResumeEligible = false
        appendReason("unsupported_network")
    }

    private func dateAvailabilityPayload() -> [String: Any] {
        let creationAvailable = !rows.isEmpty && rows.allSatisfy { $0.createdAt != nil }
        let modificationAvailable = !rows.isEmpty && rows.allSatisfy { $0.modifiedAt != nil }
        return [
            "creation": ["available": creationAvailable, "reason": creationAvailable ? NSNull() as Any : "metadata_unavailable_or_empty"],
            "modification": ["available": modificationAvailable, "reason": modificationAvailable ? NSNull() as Any : "metadata_unavailable_or_empty"]
        ]
    }

    private func rowPayload(_ row: IndexedRow) -> [String: Any] {
        var result: [String: Any] = [
            "path": row.path,
            "name": row.name,
            "kind": row.kind,
            "size_bytes": row.sizeBytes ?? NSNull(),
            "size_available": row.sizeBytes != nil,
            "size_reason": row.sizeReason ?? NSNull(),
            "created_at": row.createdAt.map(isoFormatter.string) ?? NSNull(),
            "creation_date_available": row.createdAt != nil,
            "creation_date_reason": row.creationDateReason ?? NSNull(),
            "modified_at": row.modifiedAt.map(isoFormatter.string) ?? NSNull(),
            "modification_date_available": row.modifiedAt != nil,
            "modification_date_reason": row.modificationDateReason ?? NSNull(),
            "metadata_complete": row.sizeBytes != nil && row.createdAt != nil && row.modifiedAt != nil
        ]
        return result
    }

    private func startStream(from cursor: FSEventStreamEventId) {
        guard let rootPath else { return }
        stopStream()
        do {
            _ = try Self.validateRoot(URL(fileURLWithPath: rootPath))
        } catch NativeFilenameIndexError.unsupportedNetwork {
            markUnsupportedNetwork()
            return
        } catch {
            appendReason("root_unavailable")
            staleReason = "root_unavailable"
            rescanRequired = true
            return
        }
        let contextObject = StreamContext(owner: self, generation: lifecycleToken)
        streamContext = contextObject
        var context = FSEventStreamContext(
            version: 0,
            info: Unmanaged.passUnretained(contextObject).toOpaque(),
            retain: { info in
                guard let info else { return nil }
                _ = Unmanaged<StreamContext>.fromOpaque(info).retain()
                return info
            },
            release: { info in
                guard let info else { return }
                Unmanaged<StreamContext>.fromOpaque(info).release()
            },
            copyDescription: nil
        )
        let callback: FSEventStreamCallback = { _, info, count, paths, flags, ids in
            guard let info else { return }
            let context = Unmanaged<StreamContext>.fromOpaque(info).takeUnretainedValue()
            guard let owner = context.owner else { return }
            let changed = unsafeBitCast(paths, to: NSArray.self) as? [String] ?? []
            var events = (0..<min(count, 10_000)).map { index in
                Event(path: index < changed.count ? changed[index] : "",
                      flags: flags[index], id: ids[index])
            }
            if count > 10_000 {
                events.append(Event(path: "", flags: FSEventStreamEventFlags(kFSEventStreamEventFlagMustScanSubDirs), id: ids[count - 1]))
            }
            let generation = context.generation
            Task { @MainActor in owner.consume(events: events, generation: generation) }
        }
        let flags = FSEventStreamCreateFlags(kFSEventStreamCreateFlagUseCFTypes
            | kFSEventStreamCreateFlagFileEvents | kFSEventStreamCreateFlagWatchRoot)
        guard let created = FSEventStreamCreate(
            kCFAllocatorDefault, callback, &context, [rootPath] as CFArray,
            cursor, 0.5, flags
        ) else {
            streamContext = nil
            return
        }
        FSEventStreamSetDispatchQueue(created, DispatchQueue.global(qos: .utility))
        guard FSEventStreamStart(created) else {
            FSEventStreamInvalidate(created)
            FSEventStreamRelease(created)
            streamContext = nil
            return
        }
        stream = created
    }

    private func stopStream() {
        guard let stream else { return }
        FSEventStreamStop(stream)
        FSEventStreamInvalidate(stream)
        FSEventStreamRelease(stream)
        self.stream = nil
        streamContext = nil
    }

    private func consume(events: [Event], generation: UInt64) {
        guard generation == lifecycleToken, stream != nil, let rootPath, !events.isEmpty else { return }
        let currentRoot: RootInfo
        do {
            currentRoot = try Self.validateRoot(URL(fileURLWithPath: rootPath))
        } catch NativeFilenameIndexError.unsupportedNetwork {
            markUnsupportedNetwork()
            return
        } catch {
            staleReason = "root_changed"
            rescanRequired = true
            persistedReplayPending = false
            appendReason("root_changed")
            try? persistState()
            return
        }
        guard currentRoot.device == rootDevice, currentRoot.fileID == rootFileID else {
            staleReason = "root_changed"
            rescanRequired = true
            persistedReplayPending = false
            appendReason("root_changed")
            try? persistState()
            return
        }
        var affected: Set<String> = []
        var latest = cursor ?? 0
        var lost = false
        var mustScanSubdirectories = false
        var historyDone = false
        let lossFlags = FSEventStreamEventFlags(kFSEventStreamEventFlagRootChanged
            | kFSEventStreamEventFlagUserDropped | kFSEventStreamEventFlagKernelDropped
            | kFSEventStreamEventFlagEventIdsWrapped | kFSEventStreamEventFlagMustScanSubDirs)
        for event in events {
            latest = max(latest, event.id)
            if event.flags & FSEventStreamEventFlags(kFSEventStreamEventFlagMustScanSubDirs) != 0 {
                mustScanSubdirectories = true
            }
            if event.flags & lossFlags != 0 {
                lost = true
                continue
            }
            if event.flags & FSEventStreamEventFlags(kFSEventStreamEventFlagHistoryDone) != 0 {
                historyDone = true
                if event.path.isEmpty { continue }
            }
            let normalized = URL(fileURLWithPath: event.path).standardizedFileURL.path
            guard isWithinRoot(normalized, root: rootPath) else { continue }
            affected.insert(normalized)
        }
        cursor = latest
        if lost {
            staleReason = "change_journal_lost_or_root_changed"
            rescanRequired = true
            persistedReplayPending = false
            appendReason("change_journal_lost_or_root_changed")
            if mustScanSubdirectories { appendReason("change_journal_must_scan_subdirs") }
            try? persistState()
            return
        }
        guard !affected.isEmpty else {
            if historyDone && persistedReplayPending {
                persistedReplayPending = false
                staleReason = nil
                rescanRequired = !incompleteReasons.isEmpty
            }
            try? persistState()
            return
        }
        staleReason = "change_journal_update_pending"
        let result = applyIncremental(paths: affected, root: URL(fileURLWithPath: rootPath))
        if result.incompleteReasons.contains("unsupported_network") {
            markUnsupportedNetwork()
            return
        }
        rows = result.rows
        incompleteReasons = Array(Set(incompleteReasons + result.incompleteReasons)).sorted()
        if historyDone && persistedReplayPending {
            persistedReplayPending = false
            staleReason = nil
            rescanRequired = !incompleteReasons.isEmpty
        } else if incompleteReasons.isEmpty && !persistedReplayPending {
            staleReason = nil
            rescanRequired = false
        } else {
            staleReason = "incremental_update_incomplete"
            rescanRequired = true
        }
        try? persistState()
    }

    private func applyIncremental(paths: Set<String>, root: URL) -> EnumerationResult {
        var next = rows
        var reasons: [String] = []
        do {
            _ = try Self.validateRoot(root)
        } catch NativeFilenameIndexError.unsupportedNetwork {
            return EnumerationResult(rows: next, cursor: cursor ?? 0,
                                     incompleteReasons: ["unsupported_network"])
        } catch {
            return EnumerationResult(rows: next, cursor: cursor ?? 0,
                                     incompleteReasons: ["root_unavailable"])
        }
        let deadline = ProcessInfo.processInfo.systemUptime + Self.maxScanSeconds
        for path in paths.sorted() {
            if ProcessInfo.processInfo.systemUptime >= deadline {
                reasons.append("time_limit")
                break
            }
            guard isWithinRoot(path, root: root.path) else { continue }
            do {
                let ancestor = try Self.validateNoFollowAncestors(
                    URL(fileURLWithPath: path).deletingLastPathComponent().path)
                let local = Self.isLocalFileSystem(ancestor)
                close(ancestor)
                guard local else { reasons.append("unsupported_network"); continue }
            } catch { reasons.append("ancestor_unavailable_or_replaced"); continue }
            if let localDirectory = Self.localDirectoryStatus(path), !localDirectory {
                reasons.append("unsupported_network")
                continue
            }
            let url = URL(fileURLWithPath: path)
            var value = stat()
            if lstat(path, &value) != 0 {
                if errno == ENOENT || errno == ENOTDIR {
                    next.removeAll { $0.path == path || isWithinRoot($0.path, root: path) }
                } else { reasons.append("permission_denied") }
                continue
            }
            let dataLess = (UInt32(value.st_flags) & Self.dataLessFlag) != 0
            if dataLess {
                if let row = Self.row(for: url, stat: value, dataLess: true) {
                    next.removeAll { $0.path == path || isWithinRoot($0.path, root: path) }
                    if next.count < Self.maxEntries { next.append(row) }
                    else { reasons.append("entry_limit") }
                }
                reasons.append("placeholder_unavailable")
            } else if (value.st_mode & S_IFMT) == S_IFDIR {
                next.removeAll { $0.path == path || isWithinRoot($0.path, root: path) }
                let room = max(0, Self.maxEntries - next.count)
                if room == 0 {
                    reasons.append("entry_limit")
                    continue
                }
                let enumeration = try? Self.enumerate(root: url, stateDirectory: stateDirectory,
                                                      deadline: deadline, cursor: cursor ?? 0,
                                                      entryLimit: room)
                guard let enumeration else { reasons.append("permission_denied"); continue }
                next.append(contentsOf: enumeration.rows.prefix(room))
                if enumeration.rows.count > room { reasons.append("entry_limit") }
                reasons.append(contentsOf: enumeration.incompleteReasons)
            } else if let row = Self.row(for: url, stat: value,
                                        dataLess: (UInt32(value.st_flags) & Self.dataLessFlag) != 0) {
                next.removeAll { $0.path == path }
                next.append(row)
            } else { reasons.append("metadata_unavailable") }
        }
        let sorted = next.sorted { $0.path < $1.path }
        if sorted.count > Self.maxEntries {
            reasons.append("entry_limit")
        }
        return EnumerationResult(rows: Array(sorted.prefix(Self.maxEntries)), cursor: cursor ?? 0,
                                 incompleteReasons: Array(Set(reasons)).sorted())
    }

    private func loadPersistedState() {
        do {
            guard let data = try readStateData() else { return }
            let decoder = JSONDecoder()
            decoder.dateDecodingStrategy = .iso8601
            let state = try decoder.decode(PersistedState.self, from: data)
            guard state.schemaVersion == Self.schemaVersion, state.rows.count <= Self.maxEntries,
                  Self.validatePersistedState(state) else { throw NativeFilenameIndexError.persistence }
            do {
                _ = try Self.validateRoot(URL(fileURLWithPath: state.root))
            } catch NativeFilenameIndexError.unsupportedNetwork {
                rows = state.rows
                rootPath = state.root
                rootDevice = state.rootDevice
                rootFileID = state.rootFileID
                cursor = state.cursor
                incompleteReasons = Array(Set(state.incompleteReasons + ["unsupported_network"])).sorted()
                staleReason = "unsupported_network"
                rescanRequired = true
                persistedReplayPending = false
                persistedStateResumeEligible = false
                return
            }
            rows = state.rows
            rootPath = state.root
            rootDevice = state.rootDevice
            rootFileID = state.rootFileID
            cursor = state.cursor
            incompleteReasons = state.incompleteReasons
            persistedStateResumeEligible = state.rootDevice != nil && state.rootFileID != nil
                && state.cursor > 0 && state.incompleteReasons.isEmpty
                && !state.rescanRequired && state.staleReason == nil
            staleReason = "persisted_state_unverified"
            rescanRequired = true
            persistedReplayPending = true
        } catch {
            rows = []
            rootPath = nil
            cursor = nil
            persistenceReason = "state_unavailable"
        }
    }

    private func persistState() throws {
        guard let rootPath else { return }
        do {
            _ = try Self.validateRoot(URL(fileURLWithPath: rootPath))
        } catch NativeFilenameIndexError.unsupportedNetwork {
            throw NativeFilenameIndexError.unsupportedNetwork
        } catch {
            // Preserve stale/root-changed state even after local root disappearance.
        }
        let state = PersistedState(schemaVersion: Self.schemaVersion, root: rootPath,
                                   cursor: cursor ?? 0, rootDevice: rootDevice, rootFileID: rootFileID,
                                   rows: Array(rows.prefix(Self.maxEntries)),
                                   incompleteReasons: Array(Set(incompleteReasons)).sorted(),
                                   staleReason: staleReason, rescanRequired: rescanRequired)
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.sortedKeys]
        encoder.dateEncodingStrategy = .iso8601
        let data = try encoder.encode(state)
        guard data.count <= Self.maxStateBytes else { throw NativeFilenameIndexError.persistence }
        try writeStateData(data)
    }

    nonisolated private static func validatePersistedState(_ state: PersistedState) -> Bool {
        guard !state.root.isEmpty, state.root.utf8.count <= 4_096,
              state.root.hasPrefix("/"), state.root != "/",
              URL(fileURLWithPath: state.root).standardizedFileURL.path == state.root,
              state.incompleteReasons.count <= 128,
              state.incompleteReasons.allSatisfy({ !$0.isEmpty && $0.utf8.count <= 256 }) else { return false }
        var seen: Set<String> = []
        for row in state.rows {
            guard row.path.utf8.count <= 4_096,
                  URL(fileURLWithPath: row.path).standardizedFileURL.path == row.path,
                  isWithinRoot(row.path, root: state.root),
                  row.name == URL(fileURLWithPath: row.path).lastPathComponent,
                  ["file", "directory", "symlink", "other"].contains(row.kind),
                  row.sizeBytes.map({ $0 >= 0 }) ?? true,
                  row.createdAt.map({ $0.timeIntervalSince1970.isFinite }) ?? true,
                  row.modifiedAt.map({ $0.timeIntervalSince1970.isFinite }) ?? true,
                  seen.insert(row.path).inserted else { return false }
        }
        return true
    }

    private func readStateData() throws -> Data? {
        let descriptor = try openStateDirectory()
        defer { close(descriptor) }
        let fd = Self.stateFileName.withCString { openat(descriptor, $0, O_RDONLY | O_NOFOLLOW | O_CLOEXEC) }
        guard fd >= 0 else {
            if errno == ENOENT { return nil }
            throw NativeFilenameIndexError.persistence
        }
        defer { close(fd) }
        var value = stat()
        guard fstat(fd, &value) == 0, value.st_uid == getuid(), (value.st_mode & S_IFMT) == S_IFREG,
              (value.st_mode & 0o077) == 0, value.st_nlink == 1, value.st_size >= 0,
              value.st_size <= off_t(Self.maxStateBytes) else { throw NativeFilenameIndexError.persistence }
        var data = Data(capacity: Int(value.st_size))
        var buffer = [UInt8](repeating: 0, count: 64 * 1024)
        while data.count < Int(value.st_size) {
            let count = Darwin.read(fd, &buffer, min(buffer.count, Int(value.st_size) - data.count))
            if count < 0, errno == EINTR { continue }
            guard count >= 0 else { throw NativeFilenameIndexError.persistence }
            if count == 0 { break }
            data.append(buffer, count: count)
        }
        guard data.count == Int(value.st_size) else { throw NativeFilenameIndexError.persistence }
        return data
    }

    private func writeStateData(_ data: Data) throws {
        let descriptor = try openStateDirectory()
        defer { close(descriptor) }
        let temporaryName = ".\(Self.stateFileName).\(UUID().uuidString).tmp"
        let fd = temporaryName.withCString { openat(descriptor, $0, O_WRONLY | O_CREAT | O_EXCL | O_NOFOLLOW | O_CLOEXEC, 0o600) }
        guard fd >= 0 else { throw NativeFilenameIndexError.persistence }
        var openFD = true
        do {
            try data.withUnsafeBytes { bytes in
                guard let base = bytes.baseAddress else { return }
                var offset = 0
                while offset < bytes.count {
                    let written = Darwin.write(fd, base.advanced(by: offset), bytes.count - offset)
                    if written < 0, errno == EINTR { continue }
                    guard written > 0 else { throw NativeFilenameIndexError.persistence }
                    offset += written
                }
            }
            guard fsync(fd) == 0 else { throw NativeFilenameIndexError.persistence }
            close(fd)
            openFD = false
            try validateStateDestination(descriptor)
            let result = temporaryName.withCString { temporary in
                Self.stateFileName.withCString { destination in renameat(descriptor, temporary, descriptor, destination) }
            }
            guard result == 0, fsync(descriptor) == 0 else { throw NativeFilenameIndexError.persistence }
        } catch {
            if openFD { close(fd) }
            _ = temporaryName.withCString { unlinkat(descriptor, $0, 0) }
            throw error
        }
    }

    private func openStateDirectory() throws -> Int32 {
        guard stateDirectory.isFileURL, stateDirectory.path.hasPrefix("/"), stateDirectory.path != "/",
              !stateDirectory.pathComponents.contains("."), !stateDirectory.pathComponents.contains("..") else {
            throw NativeFilenameIndexError.invalidStatePath
        }
        var descriptor = open("/", O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)
        guard descriptor >= 0 else { throw NativeFilenameIndexError.persistence }
        for component in stateDirectory.path.split(separator: "/", omittingEmptySubsequences: true) {
            let name = String(component)
            var next = name.withCString { openat(descriptor, $0, O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC) }
            if next < 0, errno == ENOENT {
                let made = name.withCString { mkdirat(descriptor, $0, 0o700) }
                guard made == 0 || errno == EEXIST else { close(descriptor); throw NativeFilenameIndexError.persistence }
                next = name.withCString { openat(descriptor, $0, O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC) }
            }
            guard next >= 0 else {
                close(descriptor)
                throw errno == ELOOP ? NativeFilenameIndexError.invalidStatePath : NativeFilenameIndexError.persistence
            }
            close(descriptor)
            descriptor = next
        }
        var state = stat()
        guard fstat(descriptor, &state) == 0, (state.st_mode & S_IFMT) == S_IFDIR,
              state.st_uid == getuid(), (state.st_mode & 0o077) == 0 else {
            close(descriptor)
            throw NativeFilenameIndexError.invalidStatePath
        }
        return descriptor
    }

    private func validateStateDestination(_ descriptor: Int32) throws {
        var existing = stat()
        let result = Self.stateFileName.withCString { fstatat(descriptor, $0, &existing, AT_SYMLINK_NOFOLLOW) }
        guard result == 0 else {
            if errno == ENOENT { return }
            throw NativeFilenameIndexError.persistence
        }
        guard (existing.st_mode & S_IFMT) == S_IFREG, existing.st_uid == getuid(),
              (existing.st_mode & 0o077) == 0, existing.st_nlink == 1 else {
            throw NativeFilenameIndexError.persistence
        }
    }

    nonisolated private static func validateRoot(_ root: URL) throws -> RootInfo {
        guard root.isFileURL, root.path.hasPrefix("/"), root.path != "/",
              !root.path.contains("\0") else { throw NativeFilenameIndexError.invalidRoot }
        let selected = root.standardizedFileURL
        let descriptor = try validateNoFollowAncestors(selected.path)
        defer { close(descriptor) }
        guard isLocalFileSystem(descriptor) else { throw NativeFilenameIndexError.unsupportedNetwork }
        var value = stat()
        guard lstat(selected.path, &value) == 0, (value.st_mode & S_IFMT) == S_IFDIR else {
            throw NativeFilenameIndexError.rootUnavailable
        }
        guard (UInt32(value.st_flags) & dataLessFlag) == 0 else {
            throw NativeFilenameIndexError.rootUnavailable
        }
        return RootInfo(url: selected, device: UInt64(UInt32(bitPattern: value.st_dev)), fileID: UInt64(value.st_ino))
    }

    nonisolated private static func validateNoFollowAncestors(_ path: String) throws -> Int32 {
        var descriptor = open("/", O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)
        guard descriptor >= 0 else { throw NativeFilenameIndexError.rootUnavailable }
        for component in path.split(separator: "/", omittingEmptySubsequences: true) {
            let name = String(component)
            let next = name.withCString { openat(descriptor, $0, O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC) }
            guard next >= 0 else {
                close(descriptor)
                throw errno == ELOOP ? NativeFilenameIndexError.invalidRoot : NativeFilenameIndexError.rootUnavailable
            }
            close(descriptor)
            descriptor = next
        }
        return descriptor
    }

    nonisolated private static func isLocalFileSystem(_ descriptor: Int32) -> Bool {
        var fileSystem = statfs()
        guard fstatfs(descriptor, &fileSystem) == 0 else { return false }
        return (fileSystem.f_flags & UInt32(MNT_LOCAL)) != 0
    }

    nonisolated private static func localDirectoryStatus(_ path: String) -> Bool? {
        guard let descriptor = try? validateNoFollowAncestors(path) else { return nil }
        defer { close(descriptor) }
        return isLocalFileSystem(descriptor)
    }

    nonisolated private static func enumerate(root: URL, stateDirectory: URL, deadline: TimeInterval,
                                              cursor: FSEventStreamEventId,
                                              entryLimit: Int = NativeFilenameIndex.maxEntries) throws -> EnumerationResult {
        let rootDescriptor = try validateNoFollowAncestors(root.path)
        defer { close(rootDescriptor) }
        guard isLocalFileSystem(rootDescriptor) else { throw NativeFilenameIndexError.unsupportedNetwork }
        var rootStat = stat()
        guard lstat(root.path, &rootStat) == 0, (rootStat.st_mode & S_IFMT) == S_IFDIR else {
            throw NativeFilenameIndexError.rootUnavailable
        }
        guard (UInt32(rootStat.st_flags) & dataLessFlag) == 0 else {
            throw NativeFilenameIndexError.rootUnavailable
        }
        let excludedState = stateDirectory.standardizedFileURL.path
        var reasons: [String] = []
        var result: [IndexedRow] = []
        if entryLimit > 0, let rootRow = row(for: root, stat: rootStat) { result.append(rootRow) }
        guard let enumerator = FileManager.default.enumerator(
            at: root, includingPropertiesForKeys: nil, options: [],
            errorHandler: { _, _ in reasons.append("permission_denied"); return true }
        ) else {
            reasons.append("permission_denied")
            return EnumerationResult(rows: result, cursor: cursor, incompleteReasons: reasons)
        }
        for case let url as URL in enumerator {
            if ProcessInfo.processInfo.systemUptime >= deadline {
                reasons.append("time_limit")
                break
            }
            let path = url.standardizedFileURL.path
            if isWithinRoot(path, root: excludedState) {
                enumerator.skipDescendants()
                continue
            }
            do {
                let ancestor = try validateNoFollowAncestors(url.deletingLastPathComponent().path)
                let local = isLocalFileSystem(ancestor)
                close(ancestor)
                guard local else {
                    reasons.append("unsupported_network")
                    enumerator.skipDescendants()
                    continue
                }
            } catch {
                reasons.append("ancestor_unavailable_or_replaced")
                enumerator.skipDescendants()
                continue
            }
            if let localDirectory = localDirectoryStatus(path), !localDirectory {
                reasons.append("unsupported_network")
                enumerator.skipDescendants()
                continue
            }
            var value = stat()
            guard lstat(path, &value) == 0 else {
                reasons.append(errno == EACCES ? "permission_denied" : "metadata_unavailable")
                enumerator.skipDescendants()
                continue
            }
            let isDataLess = (UInt32(value.st_flags) & dataLessFlag) != 0
            guard let item = row(for: url, stat: value, dataLess: isDataLess) else {
                reasons.append("metadata_unavailable")
                enumerator.skipDescendants()
                continue
            }
            if result.count >= entryLimit {
                reasons.append("entry_limit")
                break
            }
            result.append(item)
            if isDataLess { reasons.append("placeholder_unavailable") }
            if isDataLess || item.kind == "symlink" || item.kind != "directory" { enumerator.skipDescendants() }
        }
        return EnumerationResult(rows: result.sorted { $0.path < $1.path }, cursor: cursor,
                                 incompleteReasons: Array(Set(reasons)).sorted())
    }

    nonisolated private static func row(for url: URL, stat value: stat, dataLess: Bool = false) -> IndexedRow? {
        let mode = value.st_mode & S_IFMT
        let kind: String
        if mode == S_IFREG { kind = "file" }
        else if mode == S_IFDIR { kind = "directory" }
        else if mode == S_IFLNK { kind = "symlink" }
        else { kind = "other" }
        let size: Int64? = !dataLess && value.st_size >= 0 ? Int64(value.st_size) : nil
        let creation = !dataLess && value.st_birthtimespec.tv_sec > 0 ? date(seconds: Int64(value.st_birthtimespec.tv_sec), nanoseconds: Int64(value.st_birthtimespec.tv_nsec)) : nil
        let modified = !dataLess && value.st_mtimespec.tv_sec >= 0 ? date(seconds: Int64(value.st_mtimespec.tv_sec), nanoseconds: Int64(value.st_mtimespec.tv_nsec)) : nil
        return IndexedRow(path: url.standardizedFileURL.path, name: url.lastPathComponent, kind: kind,
                          sizeBytes: size, sizeReason: size == nil ? (dataLess ? "placeholder_unavailable" : "metadata_unavailable") : nil,
                          createdAt: creation, creationDateReason: creation == nil ? (dataLess ? "placeholder_unavailable" : "metadata_unavailable") : nil,
                          modifiedAt: modified, modificationDateReason: modified == nil ? (dataLess ? "placeholder_unavailable" : "metadata_unavailable") : nil)
    }

    nonisolated private static func date(seconds: Int64, nanoseconds: Int64) -> Date {
        Date(timeIntervalSince1970: Double(seconds) + Double(nanoseconds) / 1_000_000_000)
    }

    private func size(_ payload: [String: Any], keys: [String]) throws -> Int64? {
        guard let value = value(payload, keys: keys) else { return nil }
        guard let number = Self.strictInteger(value), number >= 0 else {
            throw NativeFilenameIndexError.invalidQuery("size bounds must be non-negative integers")
        }
        return number
    }

    private func date(_ payload: [String: Any], keys: [String]) throws -> Date? {
        guard let value = try string(payload, keys: keys) else { return nil }
        guard let parsed = isoFormatter.date(from: value) else {
            throw NativeFilenameIndexError.invalidQuery("date bounds must be ISO-8601 strings")
        }
        return parsed
    }

    private func integer(_ payload: [String: Any], keys: [String]) throws -> Int? {
        guard let raw = value(payload, keys: keys) else { return nil }
        guard let number = raw as? NSNumber, CFGetTypeID(number) != CFBooleanGetTypeID() else {
            throw NativeFilenameIndexError.invalidQuery("page bounds must be integers")
        }
        // WKScriptMessage carries JavaScript numbers as floating NSNumber values.
        // Admit exact integral values while preserving CFBoolean rejection above.
        guard let exact = Int(exactly: number.doubleValue),
              NSNumber(value: exact).compare(number) == .orderedSame else {
            throw NativeFilenameIndexError.invalidQuery("page bounds must be integers")
        }
        return exact
    }

    nonisolated private static func strictInteger(_ value: Any) -> Int64? {
        guard let number = value as? NSNumber, CFGetTypeID(number) != CFBooleanGetTypeID() else { return nil }
        let type = String(cString: number.objCType)
        guard !["c", "C", "B", "f", "d"].contains(type),
              let exact = Int64(number.stringValue),
              NSNumber(value: exact).compare(number) == .orderedSame else { return nil }
        return exact
    }

    private func string(_ payload: [String: Any], keys: [String]) throws -> String? {
        guard let raw = value(payload, keys: keys) else { return nil }
        guard let string = raw as? String else {
            throw NativeFilenameIndexError.invalidQuery("query fields must be strings")
        }
        return string
    }

    private func value(_ payload: [String: Any], keys: [String]) -> Any? {
        for key in keys where payload[key] != nil { return payload[key] }
        return nil
    }

    nonisolated private static func isWithinRoot(_ path: String, root: String) -> Bool {
        path == root || path.hasPrefix(root == "/" ? "/" : root + "/")
    }

    private func isWithinRoot(_ path: String, root: String) -> Bool {
        Self.isWithinRoot(path, root: root)
    }
}
