import Darwin
import Foundation

/// A reviewed duplicate selection whose content and native file identities are
/// pinned until the cleanup caller is immediately ready to act.
///
/// This type has no content, delete, network, or hydration side effects.  It
/// only opens local regular files with descriptor-relative no-follow calls and
/// compares their complete byte streams under fixed operation bounds.
@MainActor
public final class NativeDuplicateRevalidation {
    public enum Error: Swift.Error, LocalizedError, Equatable {
        case invalidReport
        case truncatedReport
        case selectionLimit
        case selectedKeptPath
        case unknownSelectedPath
        case contradictoryGroups
        case invalidPath
        case unsupported(String)
        case contentMismatch
        case changedIdentity(String)
        case racedDescriptor(String)
        case deadlineExceeded
        case readBudgetExceeded
        case io(Int32)

        public var errorDescription: String? {
            switch self {
            case .invalidReport: return "duplicate report is invalid"
            case .truncatedReport: return "duplicate report is truncated"
            case .selectionLimit: return "duplicate selection exceeds its bound"
            case .selectedKeptPath: return "duplicate kept file cannot be selected"
            case .unknownSelectedPath: return "selected path is not a duplicate extra"
            case .contradictoryGroups: return "duplicate report groups contradict each other"
            case .invalidPath: return "duplicate path is invalid"
            case .unsupported(let reason): return "duplicate revalidation is unsupported: \(reason)"
            case .contentMismatch: return "duplicate content no longer matches"
            case .changedIdentity(let path): return "duplicate identity changed: \(path)"
            case .racedDescriptor(let path): return "duplicate descriptor raced: \(path)"
            case .deadlineExceeded: return "duplicate revalidation deadline exceeded"
            case .readBudgetExceeded: return "duplicate revalidation read budget exceeded"
            case .io(let code): return "duplicate revalidation I/O failed (errno \(code))"
            }
        }
    }

    private static let maximumSelectedPaths = 100
    private static let maximumGroups = 10_000
    private static let maximumReportPaths = 100_000
    private static let maximumMounts = 4_096
    private static let maximumReadBytes: UInt64 = 1 << 30
    private static let maximumSeconds: TimeInterval = 30
    private static let readChunkBytes = 64 * 1024
    private static let datalessFlag: UInt32 = 0x4000_0000

    private struct Group {
        let keptPath: String
        let extraPaths: [String]
        let size: UInt64
    }

    private struct Selection {
        let keptPath: String
        let extras: [String]
        let size: UInt64
    }

    private struct Identity: Equatable {
        let volume: UInt64
        let inode: UInt64
    }

    private struct MountRecord {
        let mountPoint: String
        let local: Bool
    }

    private struct Fingerprint: Equatable {
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

        init(_ value: stat) {
            volume = UInt64(UInt32(bitPattern: value.st_dev))
            inode = UInt64(value.st_ino)
            owner = UInt32(value.st_uid)
            size = value.st_size >= 0 ? UInt64(value.st_size) : UInt64.max
            mode = UInt32(value.st_mode)
            flags = UInt32(value.st_flags)
            mtimeSeconds = Int64(value.st_mtimespec.tv_sec)
            mtimeNanoseconds = Int64(value.st_mtimespec.tv_nsec)
            ctimeSeconds = Int64(value.st_ctimespec.tv_sec)
            ctimeNanoseconds = Int64(value.st_ctimespec.tv_nsec)
        }

        var identity: Identity { Identity(volume: volume, inode: inode) }
    }

    private final class OpenFile {
        let path: String
        let fd: Int32
        let fingerprint: Fingerprint

        init(path: String, fd: Int32, fingerprint: Fingerprint) {
            self.path = path
            self.fd = fd
            self.fingerprint = fingerprint
        }

        deinit { close(fd) }
    }

    private final class ReadBudget {
        let deadline: TimeInterval
        var bytes: UInt64 = 0

        init() { deadline = ProcessInfo.processInfo.systemUptime + NativeDuplicateRevalidation.maximumSeconds }

        func checkDeadline() throws {
            if ProcessInfo.processInfo.systemUptime >= deadline { throw Error.deadlineExceeded }
        }

        func check() throws {
            try checkDeadline()
            if bytes >= NativeDuplicateRevalidation.maximumReadBytes { throw Error.readBudgetExceeded }
        }

        func reserve(_ requested: Int) throws -> Int {
            try check()
            let remaining = NativeDuplicateRevalidation.maximumReadBytes - bytes
            let count = min(UInt64(requested), remaining)
            guard count > 0, count <= UInt64(Int.max) else { throw Error.readBudgetExceeded }
            return Int(count)
        }

        func consumed(_ count: Int) throws {
            guard count >= 0 else { throw Error.io(EIO) }
            bytes += UInt64(count)
        }
    }

    private let selections: [Selection]
    private let pinned: [String: Fingerprint]

    private init(selections: [Selection], pinned: [String: Fingerprint]) {
        self.selections = selections
        self.pinned = pinned
    }

    /// Reviews selected duplicate extras.  Returns `nil` when selection has no
    /// duplicate extras, so callers can keep ordinary cleanup handling separate.
    public static func review(report: [String: Any], selectedPaths: [String]) throws -> NativeDuplicateRevalidation? {
        let budget = ReadBudget()
        guard selectedPaths.count <= maximumSelectedPaths,
              selectedPaths.count == Set(selectedPaths).count else { throw Error.selectionLimit }
        guard !selectedPaths.isEmpty else { return nil }
        for path in selectedPaths { try validateCanonicalPath(path) }
        guard let rawGroupValue = report["groups"] else { return nil }
        guard let rawGroups = rawGroupValue as? [[String: Any]], rawGroups.count <= maximumGroups else {
            throw Error.invalidReport
        }

        let groups = try parseGroups(rawGroups)
        let selected = try select(groups: groups, paths: selectedPaths)
        guard !selected.isEmpty else { return nil }
        guard let truncated = report["truncated"] as? Bool else { throw Error.invalidReport }
        guard !truncated else { throw Error.truncatedReport }

        var pinned: [String: Fingerprint] = [:]
        var identities: [(path: String, identity: Identity)] = []
        let selectedFilePaths = selected.flatMap { [$0.keptPath] + $0.extras }
        try preflightLocalMounts(selectedFilePaths)
        try budget.checkDeadline()
        for group in selected {
            for extra in group.extras {
                try budget.check()
                let files = try compare(keptPath: group.keptPath, extraPath: extra, expectedSize: group.size, budget: budget)
                let kept = files.kept
                let extraFile = files.extra
                try ensureDistinct(kept, from: &identities)
                try ensureDistinct(extraFile, from: &identities)
                if let existing = pinned[group.keptPath], existing != kept.fingerprint {
                    throw Error.changedIdentity(group.keptPath)
                }
                pinned[group.keptPath] = kept.fingerprint
                pinned[extra] = extraFile.fingerprint
                try verifyPath(path: group.keptPath, expected: kept.fingerprint)
                try verifyPath(path: extra, expected: extraFile.fingerprint)
                try budget.checkDeadline()
            }
        }
        try preflightLocalMounts(Array(pinned.keys))
        try finalFingerprintClosure(pinned: pinned, budget: budget)
        try budget.checkDeadline()
        return NativeDuplicateRevalidation(selections: selected, pinned: pinned)
    }

    /// Reopens every selected path and repeats complete equality immediately
    /// before the caller's cleanup claim/action.
    public func revalidate() throws {
        let budget = ReadBudget()
        var identities: [(path: String, identity: Identity)] = []
        let selectedFilePaths = selections.flatMap { [$0.keptPath] + $0.extras }
        try Self.preflightLocalMounts(selectedFilePaths)
        try budget.checkDeadline()
        for group in selections {
            for extra in group.extras {
                try budget.check()
                guard let keptPinned = pinned[group.keptPath], let extraPinned = pinned[extra] else {
                    throw Error.racedDescriptor(extra)
                }
                let kept = try Self.openFile(path: group.keptPath, expectedSize: group.size)
                let extraFile = try Self.openFile(path: extra, expectedSize: group.size)
                guard kept.fingerprint == keptPinned else { throw Error.changedIdentity(group.keptPath) }
                guard extraFile.fingerprint == extraPinned else { throw Error.changedIdentity(extra) }
                try Self.ensureDistinct(kept, from: &identities)
                try Self.ensureDistinct(extraFile, from: &identities)
                _ = try Self.compare(openedKept: kept, openedExtra: extraFile, expectedSize: group.size, budget: budget)
                try Self.verifyPath(path: group.keptPath, expected: keptPinned)
                try Self.verifyPath(path: extra, expected: extraPinned)
                try budget.checkDeadline()
            }
        }
        try Self.preflightLocalMounts(Array(pinned.keys))
        try Self.finalFingerprintClosure(pinned: pinned, budget: budget)
        try budget.checkDeadline()
    }

    private static func parseGroups(_ rawGroups: [[String: Any]]) throws -> [Group] {
        var result: [Group] = []
        var seenPaths = Set<String>()
        var reportPathCount = 0
        for raw in rawGroups {
            guard let kept = raw["kept_path"] as? String,
                  let rawExtras = raw["extras"] as? [String],
                  let size = exactUInt64(raw["size_bytes"]) else { throw Error.invalidReport }
            try validateCanonicalPath(kept)
            reportPathCount += rawExtras.count + 1
            guard reportPathCount <= maximumReportPaths else { throw Error.invalidReport }
            guard !seenPaths.contains(kept) else { throw Error.contradictoryGroups }
            seenPaths.insert(kept)
            var extras = [String]()
            for extra in rawExtras {
                try validateCanonicalPath(extra)
                guard extra != kept, !seenPaths.contains(extra), !extras.contains(extra) else {
                    throw Error.contradictoryGroups
                }
                extras.append(extra)
                seenPaths.insert(extra)
            }
            result.append(Group(keptPath: kept, extraPaths: extras, size: size))
        }
        return result
    }

    private static func select(groups: [Group], paths: [String]) throws -> [Selection] {
        let wanted = Set(paths)
        var selected = [Selection]()
        for group in groups {
            if wanted.contains(group.keptPath) { throw Error.selectedKeptPath }
            let extras = group.extraPaths.filter { wanted.contains($0) }
            if !extras.isEmpty {
                selected.append(Selection(keptPath: group.keptPath, extras: extras, size: group.size))
            }
        }
        return selected
    }

    private static func compare(keptPath: String, extraPath: String, expectedSize: UInt64, budget: ReadBudget) throws -> (kept: OpenFile, extra: OpenFile) {
        let kept = try openFile(path: keptPath, expectedSize: expectedSize)
        let extra = try openFile(path: extraPath, expectedSize: expectedSize)
        guard kept.fingerprint != extra.fingerprint else { throw Error.unsupported("hardlink_self") }
        try compare(openedKept: kept, openedExtra: extra, expectedSize: expectedSize, budget: budget)
        return (kept, extra)
    }

    private static func compare(openedKept: OpenFile, openedExtra: OpenFile, expectedSize: UInt64, budget: ReadBudget) throws -> (kept: OpenFile, extra: OpenFile) {
        guard openedKept.fingerprint.size == expectedSize,
              openedExtra.fingerprint.size == expectedSize else { throw Error.changedIdentity(openedExtra.path) }
        guard expectedSize <= NativeDuplicateRevalidation.maximumReadBytes / 2,
              expectedSize <= UInt64(Int64.max) else { throw Error.readBudgetExceeded }

        var offset: UInt64 = 0
        var keptBuffer = [UInt8](repeating: 0, count: readChunkBytes)
        var extraBuffer = [UInt8](repeating: 0, count: readChunkBytes)
        while offset < expectedSize {
            try budget.check()
            let requested = Int(min(UInt64(readChunkBytes), expectedSize - offset))
            let keptCount = try read(fd: openedKept.fd, buffer: &keptBuffer, requested: requested, offset: offset, budget: budget)
            let extraCount = try read(fd: openedExtra.fd, buffer: &extraBuffer, requested: requested, offset: offset, budget: budget)
            guard keptCount == extraCount, keptCount > 0 else { throw Error.contentMismatch }
            guard Array(keptBuffer.prefix(keptCount)) == Array(extraBuffer.prefix(extraCount)) else {
                throw Error.contentMismatch
            }
            offset += UInt64(keptCount)
        }
        try ensureStable(openedKept)
        try ensureStable(openedExtra)
        return (openedKept, openedExtra)
    }

    private static func ensureDistinct(_ file: OpenFile, from identities: inout [(path: String, identity: Identity)]) throws {
        let identity = file.fingerprint.identity
        guard !identities.contains(where: { $0.path != file.path && $0.identity == identity }) else {
            throw Error.unsupported("hardlink_alias")
        }
        if !identities.contains(where: { $0.path == file.path }) {
            identities.append((file.path, identity))
        }
    }

    private static func finalFingerprintClosure(pinned: [String: Fingerprint], budget: ReadBudget) throws {
        for path in pinned.keys.sorted() {
            try budget.checkDeadline()
            guard let expected = pinned[path] else { throw Error.racedDescriptor(path) }
            let current = try openFile(path: path, expectedSize: expected.size)
            guard current.fingerprint == expected else { throw Error.changedIdentity(path) }
        }
    }

    /// Reads one bounded, non-waiting mount snapshot before any candidate path
    /// traversal.  A missing or malformed mount table fails closed.
    private static func preflightLocalMounts(_ paths: [String]) throws {
        guard !paths.isEmpty else { throw Error.invalidReport }
        var mountPointer: UnsafeMutablePointer<statfs>?
        let count = Int(getmntinfo(&mountPointer, MNT_NOWAIT))
        guard count > 0, count <= maximumMounts, let mountPointer else {
            throw Error.unsupported("mount_table_unavailable")
        }
        var mounts: [MountRecord] = []
        mounts.reserveCapacity(count)
        for index in 0..<count {
            let value = mountPointer[index]
            guard let mountPoint = boundedCString(value.f_mntonname),
                  mountPoint.hasPrefix("/"), !mountPoint.isEmpty,
                  mountPoint == "/" || !mountPoint.hasSuffix("/") else {
                throw Error.unsupported("mount_table_invalid")
            }
            mounts.append(MountRecord(mountPoint: mountPoint, local: (value.f_flags & UInt32(MNT_LOCAL)) != 0))
        }
        for path in paths {
            let matching = mounts.filter {
                path == $0.mountPoint || path.hasPrefix($0.mountPoint == "/" ? "/" : $0.mountPoint + "/")
            }
            guard !matching.isEmpty else { throw Error.unsupported("mount_table_incomplete") }
            guard matching.allSatisfy({ $0.local }) else { throw Error.unsupported("nonlocal") }
        }
    }

    private static func boundedCString<T>(_ value: T) -> String? {
        var copy = value
        let capacity = MemoryLayout<T>.size
        return withUnsafePointer(to: &copy) { pointer in
            pointer.withMemoryRebound(to: UInt8.self, capacity: capacity) { bytes in
                let buffer = UnsafeBufferPointer(start: bytes, count: capacity)
                guard let end = buffer.firstIndex(of: 0) else { return nil }
                return String(decoding: buffer[..<end], as: UTF8.self)
            }
        }
    }

    private static func read(fd: Int32, buffer: inout [UInt8], requested: Int, offset: UInt64, budget: ReadBudget) throws -> Int {
        let count = try budget.reserve(requested)
        while true {
            try budget.check()
            let result = buffer.withUnsafeMutableBytes { bytes in
                Darwin.pread(fd, bytes.baseAddress, count, off_t(offset))
            }
            if result >= 0 {
                try budget.consumed(result)
                return result
            }
            let code = errno
            if code == EINTR || code == EAGAIN || code == EWOULDBLOCK { continue }
            throw Error.io(code)
        }
    }

    private static func openFile(path: String, expectedSize: UInt64) throws -> OpenFile {
        let components = try pathComponents(path)
        guard let leaf = components.last else { throw Error.invalidPath }
        var parent = Darwin.open("/", O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)
        guard parent >= 0 else { throw Error.io(errno) }
        defer { close(parent) }
        var rootStat = stat()
        guard fstat(parent, &rootStat) == 0 else { throw Error.io(errno) }
        try ensureDirectoryEntry(rootStat)
        try ensureLocal(directoryFD: parent, flags: UInt32(rootStat.st_flags))

        for component in components.dropLast() {
            var entry = stat()
            guard component.withCString({ fstatat(parent, $0, &entry, AT_SYMLINK_NOFOLLOW) }) == 0 else {
                throw Error.io(errno)
            }
            try ensureDirectoryEntry(entry)
            let next = component.withCString { openat(parent, $0, O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC) }
            guard next >= 0 else {
                let code = errno
                throw code == ELOOP ? Error.unsupported("symlink_ancestor") : Error.io(code)
            }
            close(parent)
            parent = next
            var opened = stat()
            guard fstat(parent, &opened) == 0 else { throw Error.io(errno) }
            guard Fingerprint(opened) == Fingerprint(entry) else { throw Error.racedDescriptor(path) }
            try ensureLocal(directoryFD: parent, flags: UInt32(opened.st_flags))
        }

        var entry = stat()
        guard leaf.withCString({ fstatat(parent, $0, &entry, AT_SYMLINK_NOFOLLOW) }) == 0 else { throw Error.io(errno) }
        try ensureRegularEntry(entry, expectedSize: expectedSize, path: path)
        let fd = leaf.withCString { openat(parent, $0, O_RDONLY | O_NONBLOCK | O_NOFOLLOW | O_CLOEXEC) }
        guard fd >= 0 else {
            let code = errno
            throw code == ELOOP ? Error.unsupported("symlink") : Error.io(code)
        }
        var ownsFD = true
        defer {
            if ownsFD { close(fd) }
        }
        var opened = stat()
        guard fstat(fd, &opened) == 0 else { let code = errno; throw Error.io(code) }
        let expected = Fingerprint(entry)
        let actual = Fingerprint(opened)
        guard actual == expected else { throw Error.racedDescriptor(path) }
        try ensureLocal(fileFD: fd, flags: actual.flags)
        ownsFD = false
        return OpenFile(path: path, fd: fd, fingerprint: actual)
    }

    private static func verifyPath(path: String, expected: Fingerprint) throws {
        let file = try openFile(path: path, expectedSize: expected.size)
        guard file.fingerprint == expected else { throw Error.changedIdentity(path) }
    }

    private static func ensureStable(_ file: OpenFile) throws {
        var value = stat()
        guard fstat(file.fd, &value) == 0 else { throw Error.io(errno) }
        guard Fingerprint(value) == file.fingerprint else { throw Error.racedDescriptor(file.path) }
    }

    private static func ensureDirectoryEntry(_ value: stat) throws {
        guard (value.st_mode & S_IFMT) == S_IFDIR else {
            if (value.st_mode & S_IFMT) == S_IFLNK { throw Error.unsupported("symlink_ancestor") }
            throw Error.unsupported("ancestor_not_directory")
        }
        guard UInt32(value.st_flags) & datalessFlag == 0 else {
            throw Error.unsupported("placeholder_ancestor")
        }
    }

    private static func ensureRegularEntry(_ value: stat, expectedSize: UInt64, path: String) throws {
        guard (value.st_mode & S_IFMT) == S_IFREG else {
            if (value.st_mode & S_IFMT) == S_IFLNK { throw Error.unsupported("symlink") }
            throw Error.unsupported("not_regular_file")
        }
        guard UInt32(value.st_flags) & datalessFlag == 0 else { throw Error.unsupported("placeholder") }
        guard UInt32(value.st_uid) == UInt32(getuid()) else { throw Error.unsupported("foreign_owner") }
        guard value.st_size >= 0, UInt64(value.st_size) == expectedSize else {
            throw Error.changedIdentity(path)
        }
    }

    private static func ensureLocal(directoryFD: Int32, flags: UInt32) throws {
        guard flags & datalessFlag == 0 else { throw Error.unsupported("placeholder_ancestor") }
        var volume = statfs()
        guard fstatfs(directoryFD, &volume) == 0 else { throw Error.io(errno) }
        guard volume.f_flags & UInt32(MNT_LOCAL) != 0 else { throw Error.unsupported("nonlocal") }
    }

    private static func ensureLocal(fileFD: Int32, flags: UInt32) throws {
        guard flags & datalessFlag == 0 else { throw Error.unsupported("placeholder") }
        var volume = statfs()
        guard fstatfs(fileFD, &volume) == 0 else { throw Error.io(errno) }
        guard volume.f_flags & UInt32(MNT_LOCAL) != 0 else { throw Error.unsupported("nonlocal") }
    }

    private static func validateCanonicalPath(_ path: String) throws {
        guard path.hasPrefix("/"), !path.isEmpty, path.utf8.count <= 4096,
              !path.utf8.contains(0),
              URL(fileURLWithPath: path).standardizedFileURL.path == path else { throw Error.invalidPath }
        _ = try pathComponents(path)
    }

    private static func pathComponents(_ path: String) throws -> [String] {
        guard path.hasPrefix("/"), !path.contains("//") else { throw Error.invalidPath }
        let components = path.split(separator: "/", omittingEmptySubsequences: true).map(String.init)
        guard !components.isEmpty,
              !components.contains(where: { $0 == "." || $0 == ".." || $0.isEmpty }) else { throw Error.invalidPath }
        return components
    }

    private static func exactUInt64(_ value: Any?) -> UInt64? {
        if let number = value as? NSNumber, CFGetTypeID(number) == CFBooleanGetTypeID() { return nil }
        switch value {
        case let number as UInt64: return number
        case let number as UInt: return UInt64(number)
        case let number as Int where number >= 0: return UInt64(number)
        case let number as Int64 where number >= 0: return UInt64(number)
        case let number as NSNumber:
            guard String(cString: number.objCType) != "c" else { return nil }
            let double = number.doubleValue
            guard double.isFinite, double >= 0, double.rounded(.towardZero) == double,
                  double <= Double(maximumReadBytes) else { return nil }
            return UInt64(double)
        default: return nil
        }
    }
}
