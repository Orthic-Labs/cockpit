import AppKit
import Darwin
import Foundation

/// Native, reversible file cleanup.  The only effect this service owns is moving a
/// reviewed regular file into Trash and restoring that exact file once.
@MainActor
public final class NativeCleanupService {
    public enum Error: Swift.Error, LocalizedError, Equatable {
        case invalidPath
        case protectedPath
        case unsupported(String)
        case untrustedPath
        case unknownMetadata
        case presentationRequired
        case cancelled
        case reviewRequired
        case expired
        case planAlreadyClaimed
        case planNotFound
        case stateCorrupt
        case indeterminate(String)
        case io(Int32)

        public var errorDescription: String? {
            switch self {
            case .invalidPath: return "invalid cleanup path"
            case .protectedPath: return "protected cleanup path"
            case .unsupported(let reason): return "unsupported cleanup target: \(reason)"
            case .untrustedPath: return "cleanup path is not trusted"
            case .unknownMetadata: return "cleanup metadata is unknown"
            case .presentationRequired: return "cleanup review requires a presentation window"
            case .cancelled: return "cleanup review was cancelled"
            case .reviewRequired: return "cleanup plan has no live native review token"
            case .expired: return "cleanup review has expired"
            case .planAlreadyClaimed: return "cleanup plan has already been claimed"
            case .planNotFound: return "cleanup plan was not found"
            case .stateCorrupt: return "cleanup journal is corrupt or untrusted"
            case .indeterminate(let reason): return "cleanup effect is indeterminate: \(reason)"
            case .io(let code): return "cleanup I/O failed (errno \(code))"
            }
        }
    }

    private static let journalName = "native-cleanup-journal.json"
    private static let journalSchema = 1
    private static let maxJournalBytes = 2 * 1024 * 1024
    private static let maxPlanItems = 100
    private static let maxPlanPathBytes = 64 * 1024
    private static let journalReservePerItem = 16 * 1024
    private static let journalSlackBytes = 32 * 1024
    private static let reviewLifetime: TimeInterval = 15 * 60

    private let stateDirectory: URL
    private let trashDirectory: URL
    private let injectedTrashDirectory: Bool
    private let fixtureAllowedRoot: String?
    private var didLoadState = false
    private var journal = JournalDocument(schema: journalSchema, plans: [:])
    private var reviewedPlans: [String: ReviewedPlan] = [:]
    internal var crashAfterRenameForTesting = false
    internal var crashAfterUndoRenameForTesting = false

    public init(stateDirectory: URL, trashDirectory: URL? = nil) {
        self.injectedTrashDirectory = trashDirectory != nil
        if let trashDirectory {
            // Fixture canonicalization is limited to construction.  It resolves only
            // ancestors, preserving the final leaf for no-follow checks.
            self.stateDirectory = Self.canonicalFixtureLeaf(stateDirectory)
            self.trashDirectory = Self.canonicalFixtureLeaf(trashDirectory)
            self.fixtureAllowedRoot = Self.canonicalFixtureLeaf(trashDirectory).deletingLastPathComponent().path
        } else {
            self.stateDirectory = stateDirectory
            self.trashDirectory = FileManager.default.homeDirectoryForCurrentUser.appendingPathComponent(".Trash", isDirectory: true)
            self.fixtureAllowedRoot = nil
        }
    }

    /// Shows the native review dialog.  Callers without a window must use
    /// `reviewForTesting`, so an accidental headless caller cannot authorize effects.
    public func review(paths: [URL], root: URL, presenting: NSWindow?) throws -> [String: Any] {
        guard let presenting else { throw Error.presentationRequired }
        return try makeReview(paths: paths, root: root, presenting: presenting)
    }

    /// Fixture-only review entry point.  It still performs all native inspection but
    /// deliberately omits UI so journey tests never touch a user's real Trash.
    internal func reviewForTesting(paths: [URL], root: URL) throws -> [String: Any] {
        guard injectedTrashDirectory else { throw Error.presentationRequired }
        return try makeReview(paths: paths, root: Self.canonicalFixtureLeaf(root), presenting: nil)
    }

    /// Invalidates all in-memory review authorizations after a fresh native scan.
    /// Durable claims/history are intentionally untouched.
    public func revokeReviewedPlans() {
        reviewedPlans.removeAll(keepingCapacity: false)
    }

    public func apply(planID: String) throws -> [String: Any] {
        try loadStateIfNeeded()
        guard let reviewed = reviewedPlans.removeValue(forKey: planID) else { throw Error.reviewRequired }
        guard reviewed.plan.id == planID else { throw Error.reviewRequired }
        guard Date().timeIntervalSince1970 < Double(reviewed.plan.expiresAt) else { throw Error.expired }
        guard journal.plans[planID] == nil else { throw Error.planAlreadyClaimed }

        let claimID = Self.randomID()
        var record = PlanRecord(plan: reviewed.plan, claimID: claimID, state: "claimed", undoClaimID: nil, items: reviewed.plan.items.map { ItemRecord(item: $0) })
        var candidate = journal
        candidate.plans[planID] = record
        try ensureJournalCapacity(candidate)
        journal = candidate
        try persistState()

        var outcomes: [[String: Any]] = []
        var logicalBytes: UInt64 = 0
        var movedBytes: UInt64 = 0
        for index in record.items.indices {
            let destination: String
            let trashID: DirectoryID
            do {
                let (trashFD, openedTrashID) = try openDirectory(trashDirectory, create: injectedTrashDirectory, purpose: "trash")
                defer { close(trashFD) }
                guard openedTrashID.volume == record.items[index].item.volume else { throw Error.unsupported("different_volume") }
                destination = try chooseDestination(parentPath: record.items[index].item.path, trashFD: trashFD, claimID: claimID, index: index)
                trashID = openedTrashID
            }
            record.items[index].plannedTrashName = destination
            record.items[index].plannedTrashPath = trashDirectory.appendingPathComponent(destination).path
            record.items[index].plannedTrashDirectory = trashID
            record.items[index].state = "started"
            journal.plans[planID] = record
            try persistState() // Started is durable before rename.
            let item = record.items[index].item
            do {
                let result = try move(item: item, destination: destination, trashID: trashID)
                record.items[index].state = "completed"
                record.items[index].outcome = result
                record.items[index].trashName = result.trashName
                record.items[index].trashPath = result.trashPath
                record.items[index].movedBytes = result.movedBytes
                logicalBytes &+= item.logicalBytes
                movedBytes &+= result.movedBytes
                outcomes.append(result.dictionary)
            } catch let failure as MoveAttemptError {
                switch failure {
                case .indeterminate(let result):
                    // Keep Started durable.  A post-rename failure must never be
                    // converted into a replayable failure or a false completion.
                    record.items[index].outcome = result
                    journal.plans[planID] = record
                    try persistState()
                    record.state = "interrupted"
                    journal.plans[planID] = record
                    try persistState()
                    outcomes.append(result.dictionary)
                    return ["plan_id": planID, "state": "interrupted", "items": outcomes, "outcomes": outcomes, "logical_bytes": logicalBytes, "moved_bytes": movedBytes]
                case .ordinary(let failure):
                    let result = MoveResult(item: item, status: "failed", reason: failure.localizedDescription, trashName: nil, trashPath: nil, trashFingerprint: nil, trashEntryIdentity: nil, trashDirectory: nil, movedBytes: 0)
                    record.items[index].state = "completed"
                    record.items[index].outcome = result
                    outcomes.append(result.dictionary)
                }
            } catch let failure as Error {
                let result = MoveResult(item: item, status: "failed", reason: failure.localizedDescription, trashName: nil, trashPath: nil, trashFingerprint: nil, trashEntryIdentity: nil, trashDirectory: nil, movedBytes: 0)
                record.items[index].state = "completed"
                record.items[index].outcome = result
                outcomes.append(result.dictionary)
            } catch {
                let result = MoveResult(item: item, status: "failed", reason: "io", trashName: nil, trashPath: nil, trashFingerprint: nil, trashEntryIdentity: nil, trashDirectory: nil, movedBytes: 0)
                record.items[index].state = "completed"
                record.items[index].outcome = result
                outcomes.append(result.dictionary)
            }
            journal.plans[planID] = record
            try persistState() // Completed is durable after rename or failure.
        }
        record.state = "completed"
        journal.plans[planID] = record
        try persistState()
        return [
            "plan_id": planID,
            "state": "completed",
            "items": outcomes,
            "outcomes": outcomes,
            "logical_bytes": logicalBytes,
            "moved_bytes": movedBytes
        ]
    }

    public func undo(planID: String) throws -> [String: Any] {
        try loadStateIfNeeded()
        guard var record = journal.plans[planID] else { throw Error.planNotFound }
        guard record.state == "completed" || record.state == "interrupted" else { throw Error.unsupported("plan_not_complete") }
        let pending = record.items.filter { $0.outcome?.status == "moved" && $0.undoState == nil }
        guard !pending.isEmpty else { throw Error.planAlreadyClaimed }
        if record.undoClaimID == nil {
            record.undoClaimID = Self.randomID()
            record.undoState = "claimed"
            var candidate = journal
            candidate.plans[planID] = record
            try ensureJournalCapacity(candidate)
            journal = candidate
            try persistState() // Undo claim is durable before the first restore.
        }

        var outcomes: [[String: Any]] = []
        for index in record.items.indices where record.items[index].outcome?.status == "moved" && record.items[index].undoState == nil {
            record.items[index].undoState = "started"
            journal.plans[planID] = record
            try persistState() // A restart never retries this item.
            let item = record.items[index]
            let result = restore(item: item)
            record.items[index].undoState = result.indeterminate ? "started" : "completed"
            record.items[index].undoOutcome = result.payload.reduce(into: [String: String]()) { values, pair in
                if let string = pair.value as? String { values[pair.key] = string }
            }
            outcomes.append(result.payload)
            journal.plans[planID] = record
            try persistState()
            if result.indeterminate {
                record.undoState = "interrupted"
                journal.plans[planID] = record
                try persistState()
                return ["plan_id": planID, "state": "interrupted", "items": outcomes, "outcomes": outcomes]
            }
        }
        record.undoState = record.items.contains { $0.undoState == "started" } ? "interrupted" : "completed"
        journal.plans[planID] = record
        try persistState()
        return ["plan_id": planID, "state": record.undoState == "completed" ? "completed" : "interrupted", "items": outcomes, "outcomes": outcomes]
    }

    public func historyPayload() throws -> [String: Any] {
        try loadStateIfNeeded()
        let plans = journal.plans.values.sorted { $0.plan.createdAt < $1.plan.createdAt }.map { record in
            [
                "plan_id": record.plan.id,
                "created_at": record.plan.createdAt,
                "expires_at": record.plan.expiresAt,
                "state": record.state,
                "claim_id": record.claimID,
                "items": record.items.map { item in
                    var value: [String: Any] = ["path": item.item.path, "state": item.state]
                    if let outcome = item.outcome { value["outcome"] = outcome.dictionary }
                    if let undoState = item.undoState { value["undo_state"] = undoState }
                    if let undoOutcome = item.undoOutcome { value["undo_outcome"] = undoOutcome }
                    return value
                }
            ] as [String: Any]
        }
        return ["schema": Self.journalSchema, "plans": plans]
    }

    private func makeReview(paths: [URL], root: URL, presenting: NSWindow?) throws -> [String: Any] {
        try loadStateIfNeeded()
        try ensureJournalCapacity(journal)
        guard !paths.isEmpty, paths.count <= Self.maxPlanItems else { throw Error.unsupported("plan_capacity") }
        guard paths.reduce(0, { $0 + $1.path.utf8.count }) <= Self.maxPlanPathBytes else { throw Error.unsupported("plan_capacity") }
        let rootURL = try trustedDirectory(injectedTrashDirectory ? Self.canonicalFixtureLeaf(root) : root, create: false, purpose: "root")
        let rootPath = rootURL.path
        let trashURL = try trustedDirectory(trashDirectory, create: injectedTrashDirectory, purpose: "trash")
        var items: [ReviewedItem] = []
        var names = [String]()
        for path in paths {
            let normalized = try normalizedFilePath(injectedTrashDirectory ? Self.canonicalFixtureLeaf(path) : path)
            guard normalized.path.hasPrefix(rootPath + "/") else { throw Error.invalidPath }
            guard !items.contains(where: { $0.path == normalized.path }) else { throw Error.invalidPath }
            let item = try inspect(path: normalized, root: rootURL, trash: trashURL)
            items.append(item)
            names.append(normalized.lastPathComponent)
        }
        guard items.allSatisfy({ $0.volume == $0.trashVolume }) else { throw Error.unsupported("different_volume") }
        let now = Date().timeIntervalSince1970
        let expires = now + Self.reviewLifetime
        let plan = ReviewedPlanData(id: Self.randomID(), createdAt: UInt64(now), expiresAt: UInt64(expires), items: items.map(ItemData.init))
        if let presenting {
            let alert = NSAlert()
            alert.messageText = "Move selected files to Trash?"
            let bytes = items.reduce(UInt64(0)) { $0 &+ $1.logicalBytes }
            let listing = names.prefix(40).joined(separator: "\n")
            alert.informativeText = "\(items.count) file\(items.count == 1 ? "" : "s") • \(Self.byteDescription(bytes))\nReview expires \(Date(timeIntervalSince1970: expires)).\n\n\(listing)"
            alert.addButton(withTitle: "Move to Trash")
            alert.addButton(withTitle: "Cancel")
            guard alert.runModal() == .alertFirstButtonReturn else { throw Error.cancelled }
            _ = presenting // The window is intentionally retained as caller-owned presentation context.
        }
        let token = Self.randomID()
        reviewedPlans[plan.id] = ReviewedPlan(plan: plan, token: token)
        let publicItems: [[String: Any]] = items.map {
            ["path": $0.path, "filename": URL(fileURLWithPath: $0.path).lastPathComponent, "logical_bytes": $0.logicalBytes]
        }
        return [
            "plan_id": plan.id,
            "reviewed_token": token,
            "reviewed": true,
            "created_at": plan.createdAt,
            "expires_at": plan.expiresAt,
            "items": publicItems,
            "logical_bytes": items.reduce(UInt64(0)) { $0 &+ $1.logicalBytes }
        ]
    }

    private func chooseDestination(parentPath: String, trashFD: Int32, claimID: String, index: Int) throws -> String {
        let sourceName = URL(fileURLWithPath: parentPath).lastPathComponent
        for attempt in 0..<8 {
            let suffix = attempt == 0 ? "\(claimID.prefix(16))-\(index)" : "\(claimID.prefix(16))-\(index)-\(attempt)"
            let candidate = "\(sourceName).cockpit-\(suffix)"
            var value = stat()
            if fstatat(trashFD, candidate, &value, AT_SYMLINK_NOFOLLOW) != 0, errno == ENOENT { return candidate }
        }
        throw Error.unsupported("trash_name_collision")
    }

    private func move(item: ItemData, destination: String, trashID: DirectoryID) throws -> MoveResult {
        let (sourceParentFD, sourceParentID) = try openParent(URL(fileURLWithPath: item.path))
        defer { close(sourceParentFD) }
        guard sourceParentID == item.parent else { throw Error.unsupported("ancestor_replaced") }
        let (trashFD, openedTrashID) = try openDirectory(trashDirectory, create: injectedTrashDirectory, purpose: "trash")
        defer { close(trashFD) }
        guard openedTrashID == trashID, trashID.volume == item.volume else { throw Error.unsupported("trash_directory_changed") }
        let sourceName = URL(fileURLWithPath: item.path).lastPathComponent
        try validateNoPlaceholder(path: URL(fileURLWithPath: item.path), parentFD: sourceParentFD, name: sourceName)
        let (sourceLeafFD, current) = try openRegularFile(parentFD: sourceParentFD, name: sourceName)
        defer { close(sourceLeafFD) }
        guard current == item.fingerprint else { throw Error.unsupported("source_changed") }
        let destinationPath = trashDirectory.appendingPathComponent(destination).path
        let result = sourceName.withCString { sourceCString in
            destination.withCString { destinationCString in
                renameatx_np(sourceParentFD, sourceCString, trashFD, destinationCString, UInt32(RENAME_EXCL))
            }
        }
        guard result == 0 else { throw errno == EEXIST ? Error.unsupported("trash_name_collision") : Error.io(errno) }
        let provenance = MoveResult(item: item, status: "indeterminate", reason: "postrename_verification", trashName: destination, trashPath: destinationPath, trashFingerprint: nil, trashEntryIdentity: try? entryIdentity(parentFD: trashFD, name: destination), trashDirectory: trashID, movedBytes: 0)
        if crashAfterRenameForTesting {
            crashAfterRenameForTesting = false
            throw MoveAttemptError.indeterminate(provenance)
        }
        do {
            let actualEntry = try entryIdentity(parentFD: trashFD, name: destination)
            let trashed = try regularFileFingerprint(parentFD: trashFD, name: destination)
            guard Self.stableMatches(trashed, item.fingerprint) else {
                if rollbackUnexpectedMove(trashFD: trashFD, trashName: destination, sourceParentFD: sourceParentFD, sourceName: sourceName, unexpected: actualEntry) {
                    throw MoveAttemptError.ordinary(.unsupported("source_changed"))
                }
                throw MoveAttemptError.indeterminate(MoveResult(item: item, status: "indeterminate", reason: "unexpected_entry_retained", trashName: destination, trashPath: destinationPath, trashFingerprint: trashed, trashEntryIdentity: actualEntry, trashDirectory: trashID, movedBytes: 0))
            }
            var sourceAfter = stat()
            guard fstat(sourceLeafFD, &sourceAfter) == 0 else { throw Error.io(errno) }
            let sourceFingerprint = Self.fingerprint(from: sourceAfter)
            guard Self.stableMatches(sourceFingerprint, item.fingerprint) else { throw Error.unsupported("source_descriptor_changed") }
            guard fsync(sourceParentFD) == 0, fsync(trashFD) == 0 else { throw Error.io(errno) }
            return MoveResult(item: item, status: "moved", reason: nil, trashName: destination, trashPath: destinationPath, trashFingerprint: trashed, trashEntryIdentity: actualEntry, trashDirectory: trashID, movedBytes: item.logicalBytes)
        } catch let failure as MoveAttemptError {
            throw failure
        } catch let failure as Error {
            if case .unsupported(let reason) = failure,
               ["symlink", "not_regular_file", "trash_identity_changed", "source_descriptor_changed"].contains(reason),
               let unexpected = provenance.trashEntryIdentity,
               rollbackUnexpectedMove(trashFD: trashFD, trashName: destination, sourceParentFD: sourceParentFD, sourceName: sourceName, unexpected: unexpected) {
                throw MoveAttemptError.ordinary(.unsupported("source_changed"))
            }
            throw MoveAttemptError.indeterminate(MoveResult(item: item, status: "indeterminate", reason: failure.localizedDescription, trashName: provenance.trashName, trashPath: provenance.trashPath, trashFingerprint: try? regularFileFingerprint(parentFD: trashFD, name: destination), trashEntryIdentity: try? entryIdentity(parentFD: trashFD, name: destination), trashDirectory: trashID, movedBytes: 0))
        } catch {
            throw MoveAttemptError.indeterminate(provenance)
        }
    }

    private func rollbackUnexpectedMove(trashFD: Int32, trashName: String, sourceParentFD: Int32, sourceName: String, unexpected: EntryIdentity) -> Bool {
        var occupied = stat()
        guard fstatat(sourceParentFD, sourceName, &occupied, AT_SYMLINK_NOFOLLOW) != 0, errno == ENOENT else { return false }
        let result = trashName.withCString { source in sourceName.withCString { destination in renameatx_np(trashFD, source, sourceParentFD, destination, UInt32(RENAME_EXCL)) } }
        guard result == 0 else { return false }
        guard (try? entryIdentity(parentFD: sourceParentFD, name: sourceName)) == unexpected else { return false }
        return fsync(trashFD) == 0 && fsync(sourceParentFD) == 0
    }

    private func rollbackUnexpectedRestore(trashFD: Int32, trashName: String, originalParentFD: Int32, originalName: String, unexpected: EntryIdentity) -> Bool {
        var occupied = stat()
        guard fstatat(trashFD, trashName, &occupied, AT_SYMLINK_NOFOLLOW) != 0, errno == ENOENT else { return false }
        let result = originalName.withCString { source in trashName.withCString { destination in renameatx_np(originalParentFD, source, trashFD, destination, UInt32(RENAME_EXCL)) } }
        guard result == 0 else { return false }
        guard (try? entryIdentity(parentFD: trashFD, name: trashName)) == unexpected else { return false }
        return fsync(trashFD) == 0 && fsync(originalParentFD) == 0
    }

    private func entryIdentity(parentFD: Int32, name: String) throws -> EntryIdentity {
        var value = stat()
        guard fstatat(parentFD, name, &value, AT_SYMLINK_NOFOLLOW) == 0 else { throw Error.io(errno) }
        return EntryIdentity(volume: UInt64(value.st_dev), inode: UInt64(value.st_ino), mode: UInt32(value.st_mode))
    }

    private func restore(item: ItemRecord) -> RestoreResult {
        guard let outcome = item.outcome, outcome.status == "moved", let trashName = outcome.trashName else {
            return RestoreResult(payload: ["path": item.item.path, "status": "missing_trash"], indeterminate: false)
        }
        do {
            let (trashFD, trashID) = try openDirectory(trashDirectory, create: false, purpose: "trash")
            defer { close(trashFD) }
            guard trashID.volume == item.item.volume, trashID == outcome.trashDirectory else { throw Error.unsupported("trash_directory_changed") }
            let (trashLeafFD, current) = try openRegularFile(parentFD: trashFD, name: trashName)
            defer { close(trashLeafFD) }
            guard let saved = outcome.trashFingerprint, Self.stableMatches(current, saved) else { throw Error.unsupported("trash_identity_changed") }
            let (originalParentFD, parentID) = try openParent(URL(fileURLWithPath: item.item.path))
            defer { close(originalParentFD) }
            guard parentID == item.item.parent else { throw Error.unsupported("ancestor_replaced") }
            let originalName = URL(fileURLWithPath: item.item.path).lastPathComponent
            var destinationStat = stat()
            if fstatat(originalParentFD, originalName, &destinationStat, AT_SYMLINK_NOFOLLOW) == 0 {
                return RestoreResult(payload: ["path": item.item.path, "status": "conflict_original_occupied"], indeterminate: false)
            }
            guard errno == ENOENT else { throw Error.io(errno) }
            let result = trashName.withCString { sourceCString in
                originalName.withCString { originalCString in
                    renameatx_np(trashFD, sourceCString, originalParentFD, originalCString, UInt32(RENAME_EXCL))
                }
            }
            guard result == 0 else {
                if errno == EEXIST { return RestoreResult(payload: ["path": item.item.path, "status": "conflict_original_occupied"], indeterminate: false) }
                throw Error.io(errno)
            }
            do {
                if crashAfterUndoRenameForTesting {
                    crashAfterUndoRenameForTesting = false
                    throw Error.indeterminate("checkpoint_after_restore_rename")
                }
                let restored = try regularFileFingerprint(parentFD: originalParentFD, name: originalName)
                guard Self.stableMatches(restored, item.item.fingerprint) else {
                    let unexpected = try entryIdentity(parentFD: originalParentFD, name: originalName)
                    if rollbackUnexpectedRestore(trashFD: trashFD, trashName: trashName, originalParentFD: originalParentFD, originalName: originalName, unexpected: unexpected) {
                        return RestoreResult(payload: ["path": item.item.path, "status": "identity_changed"], indeterminate: false)
                    }
                    return RestoreResult(payload: ["path": item.item.path, "status": "indeterminate", "reason": "unexpected_entry_retained"], indeterminate: true)
                }
                guard fsync(trashFD) == 0, fsync(originalParentFD) == 0 else { throw Error.io(errno) }
                return RestoreResult(payload: ["path": item.item.path, "status": "restored"], indeterminate: false)
            } catch let verification as Error {
                if case .unsupported(let reason) = verification,
                   ["symlink", "not_regular_file", "restore_identity_changed"].contains(reason),
                   let unexpected = try? entryIdentity(parentFD: originalParentFD, name: originalName),
                   rollbackUnexpectedRestore(trashFD: trashFD, trashName: trashName, originalParentFD: originalParentFD, originalName: originalName, unexpected: unexpected) {
                    return RestoreResult(payload: ["path": item.item.path, "status": "identity_changed"], indeterminate: false)
                }
                throw verification
            } catch {
                throw Error.indeterminate("restore_verification")
            }
        } catch let error as Error {
            switch error {
            case .unsupported(let reason) where reason == "trash_identity_changed":
                return RestoreResult(payload: ["path": item.item.path, "status": "identity_changed"], indeterminate: false)
            case .io(let code) where code == ENOENT:
                return RestoreResult(payload: ["path": item.item.path, "status": "missing_trash"], indeterminate: false)
            default:
                return RestoreResult(payload: ["path": item.item.path, "status": "indeterminate", "reason": error.localizedDescription], indeterminate: true)
            }
        } catch {
            return RestoreResult(payload: ["path": item.item.path, "status": "indeterminate", "reason": "io"], indeterminate: true)
        }
    }

    private func inspect(path: URL, root: URL, trash: URL) throws -> ReviewedItem {
        let (parentFD, parentID) = try openParent(path)
        defer { close(parentFD) }
        try validateNoPlaceholder(path: path, parentFD: parentFD, name: path.lastPathComponent)
        let fingerprint = try regularFileFingerprint(parentFD: parentFD, name: path.lastPathComponent)
        guard fingerprint.owner == UInt32(getuid()) else { throw Error.untrustedPath }
        let values = try path.resourceValues(forKeys: [.isUbiquitousItemKey, .ubiquitousItemIsDownloadedKey])
        if values.isUbiquitousItem == true && values.ubiquitousItemIsDownloaded == false { throw Error.unsupported("placeholder") }
        let (trashFD, trashID) = try openDirectory(trash, create: injectedTrashDirectory, purpose: "trash")
        defer { close(trashFD) }
        return ReviewedItem(path: path.path, parent: parentID, volume: fingerprint.volume, trashVolume: trashID.volume,
                            fingerprint: fingerprint, logicalBytes: fingerprint.size)
    }

    private func normalizedFilePath(_ path: URL) throws -> URL {
        guard path.isFileURL, path.path.hasPrefix("/") else { throw Error.invalidPath }
        let normalized = path.standardizedFileURL
        let components = normalized.path.split(separator: "/", omittingEmptySubsequences: true)
        guard !components.isEmpty, !components.contains(where: { $0 == "." || $0 == ".." }) else { throw Error.invalidPath }
        let fixtureAllowed = fixtureAllowedRoot.map { normalized.path == $0 || normalized.path.hasPrefix($0 + "/") } ?? false
        guard fixtureAllowed || !Self.isProtectedPath(normalized.path) else { throw Error.protectedPath }
        guard normalized.lastPathComponent != "." && normalized.lastPathComponent != ".." else { throw Error.invalidPath }
        return normalized
    }

    private func trustedDirectory(_ url: URL, create: Bool, purpose: String) throws -> URL {
        let normalized = try normalizedDirectoryPath(url)
        let (fd, _) = try openDirectory(normalized, create: create, purpose: purpose)
        close(fd)
        return normalized
    }

    private func normalizedDirectoryPath(_ url: URL) throws -> URL {
        guard url.isFileURL, url.path.hasPrefix("/") else { throw Error.invalidPath }
        let normalized = url.standardizedFileURL
        let components = normalized.path.split(separator: "/", omittingEmptySubsequences: true)
        guard !components.isEmpty, !components.contains(where: { $0 == "." || $0 == ".." }) else { throw Error.invalidPath }
        return normalized
    }

    private func openParent(_ path: URL) throws -> (Int32, DirectoryID) {
        let parent = path.deletingLastPathComponent()
        return try openDirectory(parent, create: false, purpose: "source_parent")
    }

    private func openDirectory(_ url: URL, create: Bool, purpose: String) throws -> (Int32, DirectoryID) {
        let normalized = try normalizedDirectoryPath(url)
        let components = normalized.path.split(separator: "/", omittingEmptySubsequences: true).map(String.init)
        var fd = Darwin.open("/", O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)
        guard fd >= 0 else { throw Error.io(errno) }
        for (index, component) in components.enumerated() {
            var next = openat(fd, component, O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)
            if next < 0, create && errno == ENOENT {
                guard mkdirat(fd, component, 0o700) == 0 || errno == EEXIST else { let code = errno; close(fd); throw Error.io(code) }
                next = openat(fd, component, O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)
            }
            if next < 0 { let code = errno; close(fd); throw code == ELOOP ? Error.untrustedPath : Error.io(code) }
            close(fd)
            fd = next
        }
        var value = stat()
        guard fstat(fd, &value) == 0 else { let code = errno; close(fd); throw Error.io(code) }
        guard (value.st_mode & S_IFMT) == S_IFDIR, value.st_uid == getuid(), (value.st_mode & 0o022) == 0 else {
            close(fd); throw Error.untrustedPath
        }
        _ = purpose
        return (fd, DirectoryID(volume: UInt64(value.st_dev), inode: UInt64(value.st_ino)))
    }

    private func regularFileFingerprint(parentFD: Int32, name: String) throws -> FileFingerprint {
        var value = stat()
        guard fstatat(parentFD, name, &value, AT_SYMLINK_NOFOLLOW) == 0 else { throw Error.io(errno) }
        guard (value.st_mode & S_IFMT) == S_IFREG else {
            if (value.st_mode & S_IFMT) == S_IFLNK { throw Error.unsupported("symlink") }
            throw Error.unsupported("not_regular_file")
        }
        let fd = name.withCString { openat(parentFD, $0, O_EVTONLY | O_NOFOLLOW | O_CLOEXEC) }
        guard fd >= 0 else { throw Error.io(errno) }
        defer { close(fd) }
        var opened = stat()
        guard fstat(fd, &opened) == 0 else { throw Error.io(errno) }
        guard opened.st_dev == value.st_dev, opened.st_ino == value.st_ino else { throw Error.unsupported("source_changed") }
        return Self.fingerprint(from: opened)
    }

    private func nonHydratingRegularFileFingerprint(parentFD: Int32, name: String) throws -> FileFingerprint {
        try validateNoPlaceholderMetadata(parentFD: parentFD, name: name)
        return try regularFileFingerprint(parentFD: parentFD, name: name)
    }

    private func openRegularFile(parentFD: Int32, name: String) throws -> (Int32, FileFingerprint) {
        var value = stat()
        guard fstatat(parentFD, name, &value, AT_SYMLINK_NOFOLLOW) == 0 else { throw Error.io(errno) }
        guard (value.st_mode & S_IFMT) == S_IFREG else {
            if (value.st_mode & S_IFMT) == S_IFLNK { throw Error.unsupported("symlink") }
            throw Error.unsupported("not_regular_file")
        }
        let fd = name.withCString { openat(parentFD, $0, O_EVTONLY | O_NOFOLLOW | O_CLOEXEC) }
        guard fd >= 0 else { throw Error.io(errno) }
        var opened = stat()
        guard fstat(fd, &opened) == 0 else { let code = errno; close(fd); throw Error.io(code) }
        guard opened.st_dev == value.st_dev, opened.st_ino == value.st_ino else { close(fd); throw Error.unsupported("source_changed") }
        return (fd, Self.fingerprint(from: opened))
    }

    private func validateNoPlaceholder(path: URL, parentFD: Int32, name: String) throws {
        try validateNoPlaceholderMetadata(parentFD: parentFD, name: name)
        let values = try path.resourceValues(forKeys: [.isUbiquitousItemKey, .ubiquitousItemIsDownloadedKey])
        if values.isUbiquitousItem == true && values.ubiquitousItemIsDownloaded == false { throw Error.unsupported("placeholder") }
    }

    private func validateNoPlaceholderMetadata(parentFD: Int32, name: String) throws {
        var value = stat()
        guard fstatat(parentFD, name, &value, AT_SYMLINK_NOFOLLOW) == 0 else { throw Error.io(errno) }
        guard (value.st_mode & S_IFMT) == S_IFREG else {
            if (value.st_mode & S_IFMT) == S_IFLNK { throw Error.unsupported("symlink") }
            throw Error.unsupported("not_regular_file")
        }
        // SF_DATALESS is intentionally read from lstat metadata before O_RDONLY;
        // opening a dataless File Provider item can hydrate it.
        if (UInt32(value.st_flags) & 0x4000_0000) != 0 { throw Error.unsupported("placeholder") }
    }

    private func loadStateIfNeeded() throws {
        guard !didLoadState else { return }
        let (fd, _) = try openDirectory(stateDirectory, create: true, purpose: "state")
        defer { close(fd) }
        let file = Self.journalName.withCString { openat(fd, $0, O_RDONLY | O_NOFOLLOW | O_CLOEXEC) }
        if file < 0 {
            if errno == ENOENT { didLoadState = true; return }
            throw errno == ELOOP ? Error.untrustedPath : Error.io(errno)
        }
        defer { close(file) }
        var fileStat = stat()
        guard fstat(file, &fileStat) == 0 else { throw Error.io(errno) }
        guard (fileStat.st_mode & S_IFMT) == S_IFREG, fileStat.st_uid == getuid(), (fileStat.st_mode & 0o077) == 0, fileStat.st_nlink == 1, fileStat.st_size >= 0, fileStat.st_size <= 2 * 1024 * 1024 else { throw Error.stateCorrupt }
        var data = Data()
        var buffer = [UInt8](repeating: 0, count: 64 * 1024)
        while true {
            let count = read(file, &buffer, buffer.count)
            if count < 0 { if errno == EINTR { continue }; throw Error.io(errno) }
            if count == 0 { break }
            data.append(buffer, count: count)
            if data.count > 2 * 1024 * 1024 { throw Error.stateCorrupt }
        }
        guard data.count == Int(fileStat.st_size), let decoded = try? JSONDecoder().decode(JournalDocument.self, from: data), decoded.schema == Self.journalSchema else { throw Error.stateCorrupt }
        journal = decoded
        try reconcileStartedRecords()
        try reconcileUndoStartedRecords()
        didLoadState = true
    }

    /// Crash recovery is inspection-only.  It never retries a rename: it can only
    /// prove no effect, prove a moved item by its pinned destination, or preserve an
    /// indeterminate Started claim with provenance.
    private func reconcileStartedRecords() throws {
        var changed = false
        guard let (trashFD, trashID) = try? openDirectory(trashDirectory, create: false, purpose: "trash") else {
            for planID in journal.plans.keys {
                guard var record = journal.plans[planID], record.state == "claimed" else { continue }
                record.state = "interrupted"
                journal.plans[planID] = record
                changed = true
            }
            if changed { try persistState() }
            return
        }
        defer { close(trashFD) }
        for planID in journal.plans.keys {
            guard var record = journal.plans[planID] else { continue }
            var hasStarted = false
            for index in record.items.indices where record.items[index].state == "started" && (record.items[index].outcome == nil || record.items[index].outcome?.status == "indeterminate") {
                hasStarted = true
                let item = record.items[index]
                guard let plannedName = item.plannedTrashName, let plannedDirectory = item.plannedTrashDirectory, plannedDirectory == trashID else {
                    record.items[index].outcome = MoveResult(item: item.item, status: "indeterminate", reason: "missing_destination_provenance", trashName: item.plannedTrashName, trashPath: item.plannedTrashPath, trashFingerprint: nil, trashEntryIdentity: nil, trashDirectory: item.plannedTrashDirectory, movedBytes: 0)
                    changed = true
                    continue
                }
                let source = try? existingFingerprint(path: item.item.path)
                let trash = try? nonHydratingRegularFileFingerprint(parentFD: trashFD, name: plannedName)
                if let source, source.parent == item.item.parent, Self.stableMatches(source.fingerprint, item.item.fingerprint), trash == nil {
                    record.items[index].state = "completed"
                    record.items[index].outcome = MoveResult(item: item.item, status: "not_moved", reason: "source_retained", trashName: plannedName, trashPath: item.plannedTrashPath, trashFingerprint: nil, trashEntryIdentity: nil, trashDirectory: trashID, movedBytes: 0)
                    changed = true
                } else if source == nil, let trash, Self.stableMatches(trash, item.item.fingerprint) {
                    record.items[index].state = "completed"
                    record.items[index].outcome = MoveResult(item: item.item, status: "moved", reason: "recovered_after_restart", trashName: plannedName, trashPath: item.plannedTrashPath, trashFingerprint: trash, trashEntryIdentity: try? entryIdentity(parentFD: trashFD, name: plannedName), trashDirectory: trashID, movedBytes: item.item.logicalBytes)
                    changed = true
                } else {
                    record.items[index].outcome = MoveResult(item: item.item, status: "indeterminate", reason: "restart_reconciliation_failed", trashName: plannedName, trashPath: item.plannedTrashPath, trashFingerprint: trash, trashEntryIdentity: try? entryIdentity(parentFD: trashFD, name: plannedName), trashDirectory: trashID, movedBytes: 0)
                    changed = true
                }
            }
            if hasStarted || record.state == "claimed" {
                // A crash can occur after an item is Completed but before the
                // enclosing plan is published Completed. Preserve untouched Planned
                // items and expose plan as interrupted; never replay them.
                let hasPending = record.items.contains { $0.state == "planned" }
                let hasUncertain = record.items.contains { $0.state == "started" }
                record.state = (hasPending || hasUncertain) ? "interrupted" : "completed"
                journal.plans[planID] = record
                changed = true
            }
        }
        if changed { try persistState() }
    }

    private func existingFingerprint(path: String) throws -> (parent: DirectoryID, fingerprint: FileFingerprint) {
        let url = URL(fileURLWithPath: path)
        let (parentFD, parentID) = try openParent(url)
        defer { close(parentFD) }
        let fingerprint = try nonHydratingRegularFileFingerprint(parentFD: parentFD, name: url.lastPathComponent)
        return (parentID, fingerprint)
    }

    /// Undo recovery proves a Started restore only from its final named entry.  It
    /// never retries an uncertain rename; later explicit undo may process untouched
    /// items under the existing durable claim.
    private func reconcileUndoStartedRecords() throws {
        guard let (trashFD, trashID) = try? openDirectory(trashDirectory, create: false, purpose: "trash") else { return }
        defer { close(trashFD) }
        var changed = false
        for planID in journal.plans.keys {
            guard var record = journal.plans[planID], record.undoClaimID != nil else { continue }
            for index in record.items.indices where record.items[index].undoState == "started" && record.items[index].outcome?.status == "moved" {
                let item = record.items[index]
                guard let outcome = item.outcome, let trashName = outcome.trashName, outcome.trashDirectory == trashID else {
                    record.items[index].undoOutcome = ["path": item.item.path, "status": "indeterminate", "reason": "trash_directory_changed"]
                    changed = true
                    continue
                }
                let original = try? existingFingerprint(path: item.item.path)
                var trashEntry = stat()
                let trashResult = fstatat(trashFD, trashName, &trashEntry, AT_SYMLINK_NOFOLLOW)
                let trashAbsent = trashResult != 0 && errno == ENOENT
                if let original, original.parent == item.item.parent, Self.stableMatches(original.fingerprint, item.item.fingerprint), trashAbsent {
                    record.items[index].undoState = "completed"
                    record.items[index].undoOutcome = ["path": item.item.path, "status": "restored"]
                } else {
                    record.items[index].undoOutcome = ["path": item.item.path, "status": "indeterminate", "reason": "restore_reconciliation_failed"]
                }
                changed = true
            }
            if changed { journal.plans[planID] = record }
        }
        if changed { try persistState() }
    }

    private func persistState() throws {
        let data = try JSONEncoder().encode(journal)
        guard data.count <= Self.maxJournalBytes - conservativeJournalReserve(journal) else { throw Error.unsupported("journal_capacity") }
        let (fd, _) = try openDirectory(stateDirectory, create: true, purpose: "state")
        defer { close(fd) }
        var existing = stat()
        if fstatat(fd, Self.journalName, &existing, AT_SYMLINK_NOFOLLOW) == 0 {
            guard (existing.st_mode & S_IFMT) == S_IFREG, existing.st_uid == getuid(), (existing.st_mode & 0o077) == 0 else { throw Error.untrustedPath }
        } else if errno != ENOENT { throw Error.io(errno) }
        let tempName = "native-cleanup-journal.tmp-\(getpid())-\(Self.randomID())"
        let tempFD = tempName.withCString { openat(fd, $0, O_WRONLY | O_CREAT | O_EXCL | O_NOFOLLOW | O_CLOEXEC, 0o600) }
        guard tempFD >= 0 else { throw Error.io(errno) }
        var failed: Int32 = 0
        data.withUnsafeBytes { raw in
            var offset = 0
            while offset < raw.count {
                let result = write(tempFD, raw.baseAddress!.advanced(by: offset), raw.count - offset)
                if result < 0 { if errno == EINTR { continue }; failed = errno; return }
                if result == 0 { failed = EIO; return }
                offset += result
            }
        }
        if failed == 0, fcntl(tempFD, F_FULLFSYNC) != 0, fsync(tempFD) != 0 { failed = errno }
        if close(tempFD) != 0, failed == 0 { failed = errno }
        if failed != 0 {
            _ = tempName.withCString { unlinkat(fd, $0, 0) }
            throw Error.io(failed)
        }
        let renamed = tempName.withCString { source in Self.journalName.withCString { destination in renameat(fd, source, fd, destination) } }
        guard renamed == 0 else { _ = tempName.withCString { unlinkat(fd, $0, 0) }; throw Error.io(errno) }
        guard fsync(fd) == 0 else { throw Error.io(errno) }
    }

    private func ensureJournalCapacity(_ candidate: JournalDocument, reserveBytes: Int = 0) throws {
        let encoded = try JSONEncoder().encode(candidate)
        let reserve = reserveBytes + conservativeJournalReserve(candidate)
        guard reserve <= Self.maxJournalBytes, encoded.count <= Self.maxJournalBytes - reserve else { throw Error.unsupported("journal_capacity") }
    }

    private func conservativeJournalReserve(_ document: JournalDocument) -> Int {
        let pending = document.plans.values.reduce(0) { total, plan in
            total + plan.items.reduce(0) { count, item in
                if item.state == "planned" || item.state == "started" { return count + 1 }
                if item.outcome?.status == "moved" && item.undoState == nil { return count + 1 }
                return count
            }
        }
        return Self.journalSlackBytes + pending * Self.journalReservePerItem
    }

    private static func isProtectedPath(_ path: String) -> Bool {
        let protected = ["/", "/System", "/Applications", "/Library", "/usr", "/bin", "/sbin", "/private", "/Volumes"]
        return protected.contains(where: { path == $0 || path.hasPrefix($0 + "/") })
    }

    private static func randomID() -> String { UUID().uuidString.replacingOccurrences(of: "-", with: "") }

    private static func byteDescription(_ bytes: UInt64) -> String {
        ByteCountFormatter.string(fromByteCount: Int64(min(bytes, UInt64(Int64.max))), countStyle: .file)
    }

    private static func stableMatches(_ lhs: FileFingerprint, _ rhs: FileFingerprint) -> Bool {
        lhs.volume == rhs.volume && lhs.inode == rhs.inode && lhs.owner == rhs.owner && lhs.size == rhs.size && lhs.mode == rhs.mode && lhs.flags == rhs.flags && lhs.mtimeSeconds == rhs.mtimeSeconds && lhs.mtimeNanoseconds == rhs.mtimeNanoseconds
    }

    private static func fingerprint(from value: stat) -> FileFingerprint {
        FileFingerprint(volume: UInt64(value.st_dev), inode: UInt64(value.st_ino), owner: UInt32(value.st_uid), size: UInt64(value.st_size), mode: UInt32(value.st_mode), flags: UInt32(value.st_flags), mtimeSeconds: Int64(value.st_mtimespec.tv_sec), mtimeNanoseconds: Int64(value.st_mtimespec.tv_nsec), ctimeSeconds: Int64(value.st_ctimespec.tv_sec), ctimeNanoseconds: Int64(value.st_ctimespec.tv_nsec))
    }

    private static func canonicalFixtureLeaf(_ url: URL) -> URL {
        url.deletingLastPathComponent().resolvingSymlinksInPath().appendingPathComponent(url.lastPathComponent, isDirectory: url.hasDirectoryPath)
    }
}

private enum MoveAttemptError: Swift.Error {
    case ordinary(NativeCleanupService.Error)
    case indeterminate(MoveResult)
}

private struct RestoreResult {
    let payload: [String: Any]
    let indeterminate: Bool
}

private struct JournalDocument: Codable {
    let schema: Int
    var plans: [String: PlanRecord]
}

private struct ReviewedPlan {
    let plan: ReviewedPlanData
    let token: String
}

private struct ReviewedPlanData: Codable {
    let id: String
    let createdAt: UInt64
    let expiresAt: UInt64
    let items: [ItemData]
}

private struct PlanRecord: Codable {
    let plan: ReviewedPlanData
    let claimID: String
    var state: String
    var undoClaimID: String?
    var undoState: String?
    var items: [ItemRecord]
}

private struct ItemRecord: Codable {
    let item: ItemData
    var state: String = "planned"
    var plannedTrashName: String?
    var plannedTrashPath: String?
    var plannedTrashDirectory: DirectoryID?
    var outcome: MoveResult?
    var trashName: String?
    var trashPath: String?
    var movedBytes: UInt64?
    var undoState: String?
    var undoOutcome: [String: String]?

    init(item: ItemData) { self.item = item }
}

private struct ItemData: Codable, Equatable {
    let path: String
    let parent: DirectoryID
    let volume: UInt64
    let fingerprint: FileFingerprint
    let logicalBytes: UInt64

    init(_ item: ReviewedItem) {
        self.path = item.path
        self.parent = item.parent
        self.volume = item.volume
        self.fingerprint = item.fingerprint
        self.logicalBytes = item.logicalBytes
    }
}

private struct DirectoryID: Codable, Equatable {
    let volume: UInt64
    let inode: UInt64
}

private struct FileFingerprint: Codable, Equatable {
    let volume: UInt64
    let inode: UInt64
    let owner: UInt32
    let size: UInt64
    let mode: UInt32
    let flags: UInt32
    let mtimeSeconds: Int64
    let mtimeNanoseconds: Int64
    let ctimeSeconds: Int64
    let ctimeNanoseconds: Int64
}

private struct EntryIdentity: Codable, Equatable {
    let volume: UInt64
    let inode: UInt64
    let mode: UInt32
}

private struct ReviewedItem {
    let path: String
    let parent: DirectoryID
    let volume: UInt64
    let trashVolume: UInt64
    let fingerprint: FileFingerprint
    let logicalBytes: UInt64

    init(path: String, parent: DirectoryID, volume: UInt64, trashVolume: UInt64, fingerprint: FileFingerprint, logicalBytes: UInt64) {
        self.path = path; self.parent = parent; self.volume = volume; self.trashVolume = trashVolume; self.fingerprint = fingerprint; self.logicalBytes = logicalBytes
    }
}

private struct MoveResult: Codable {
    let item: ItemData
    let status: String
    let reason: String?
    let trashName: String?
    let trashPath: String?
    let trashFingerprint: FileFingerprint?
    let trashEntryIdentity: EntryIdentity?
    let trashDirectory: DirectoryID?
    let movedBytes: UInt64

    var dictionary: [String: Any] {
        var value: [String: Any] = ["path": item.path, "status": status, "logical_bytes": item.logicalBytes, "moved_bytes": movedBytes]
        if let reason { value["reason"] = reason }
        if let trashPath { value["trash_path"] = trashPath }
        return value
    }
}
