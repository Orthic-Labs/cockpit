import Darwin
import Foundation

/// A bounded, read-only admission check for application-bundle cleanup.
///
/// This deliberately does not use AppKit's workspace process list.  The only
/// successful result is a complete, stable libproc observation of every PID
/// visible to `proc_listallpids`.
@available(macOS 13.0, *)
public enum NativeApplicationLiveness {
    public enum UnknownReason: String, Equatable {
        case processListUnavailable
        case processListTooLarge
        case processIdentityUnavailable
        case processIdentityChanged
        case executablePathUnavailable
        case processSnapshotChanged
        case deadlineExceeded

        fileprivate var message: String {
            switch self {
            case .processListUnavailable: return "native process list is unavailable"
            case .processListTooLarge: return "native process list exceeds bounded coverage"
            case .processIdentityUnavailable: return "native process identity is unavailable"
            case .processIdentityChanged: return "native process identity changed during inspection"
            case .executablePathUnavailable: return "native executable path is unavailable"
            case .processSnapshotChanged: return "native process snapshot changed during inspection"
            case .deadlineExceeded: return "native process inspection exceeded its time limit"
            }
        }
    }

    public enum Error: Swift.Error, LocalizedError, Equatable {
        case invalidBundlePath
        case running
        case unknown(UnknownReason)

        public var errorDescription: String? {
            switch self {
            case .invalidBundlePath:
                return "application bundle path is invalid"
            case .running:
                return "application is still running"
            case .unknown(let reason):
                return reason.message
            }
        }
    }

    private struct StartTime: Equatable {
        let seconds: UInt64
        let microseconds: UInt64
    }

    private struct FileIdentity: Equatable {
        let device: UInt64
        let inode: UInt64
    }

    private struct BundleAdmission {
        let identity: FileIdentity
    }

    private struct ProcessObservation {
        let startTime: StartTime
        let executablePath: String
    }

    private static let maxPIDCount = 8_192
    private static let inspectionLimit: TimeInterval = 2.0

    /// Throws unless no observed executable is inside `bundle`.
    ///
    /// A single retry is permitted only when the PID set changes between the
    /// bracketing snapshots.  No process is quit, killed, or otherwise acted
    /// upon by this adapter.
    public static func requireNotRunning(bundle: URL) throws {
        let root = try canonicalBundleURL(bundle)
        let deadline = ProcessInfo.processInfo.systemUptime + inspectionLimit

        for attempt in 0..<2 {
            do {
                try inspect(root: root, deadline: deadline)
                return
            } catch Error.unknown(.processSnapshotChanged) where attempt == 0 {
                guard ProcessInfo.processInfo.systemUptime < deadline else {
                    throw Error.unknown(.deadlineExceeded)
                }
                continue
            }
        }
    }

    private static func inspect(root: BundleAdmission, deadline: TimeInterval) throws {
        let before = try processIDs(deadline: deadline)
        var observations: [pid_t: ProcessObservation] = [:]
        observations.reserveCapacity(before.count)

        for pid in before.sorted() {
            guard ProcessInfo.processInfo.systemUptime < deadline else {
                throw Error.unknown(.deadlineExceeded)
            }
            observations[pid] = try observe(pid: pid, deadline: deadline)
        }

        guard ProcessInfo.processInfo.systemUptime < deadline else {
            throw Error.unknown(.deadlineExceeded)
        }
        for pid in before.sorted() {
            guard let observation = observations[pid] else {
                throw Error.unknown(.processIdentityUnavailable)
            }
            let finalStartBeforePath = try startTime(pid: pid)
            guard finalStartBeforePath == observation.startTime else {
                throw Error.unknown(.processIdentityChanged)
            }
            guard ProcessInfo.processInfo.systemUptime < deadline else {
                throw Error.unknown(.deadlineExceeded)
            }

            let finalPath = try executablePath(pid: pid)
            let finalStartAfterPath = try startTime(pid: pid)
            guard finalStartAfterPath == finalStartBeforePath else {
                throw Error.unknown(.processIdentityChanged)
            }
            if finalPath != observation.executablePath {
                if try executableBelongsToBundle(finalPath, root: root, deadline: deadline) {
                    throw Error.running
                }
                throw Error.unknown(.processIdentityChanged)
            }
            if try executableBelongsToBundle(finalPath, root: root, deadline: deadline) {
                throw Error.running
            }
        }

        let closing = try processIDs(deadline: deadline)
        guard before == closing else { throw Error.unknown(.processSnapshotChanged) }
        guard ProcessInfo.processInfo.systemUptime < deadline else {
            throw Error.unknown(.deadlineExceeded)
        }
    }

    private static func observe(pid: pid_t, deadline: TimeInterval) throws -> ProcessObservation {
        let before = try startTime(pid: pid)
        guard ProcessInfo.processInfo.systemUptime < deadline else {
            throw Error.unknown(.deadlineExceeded)
        }

        let path = try executablePath(pid: pid)
        guard ProcessInfo.processInfo.systemUptime < deadline else {
            throw Error.unknown(.deadlineExceeded)
        }

        let after = try startTime(pid: pid)
        guard before == after else { throw Error.unknown(.processIdentityChanged) }
        guard ProcessInfo.processInfo.systemUptime < deadline else {
            throw Error.unknown(.deadlineExceeded)
        }
        return ProcessObservation(startTime: before, executablePath: path)
    }

    private static func processIDs(deadline: TimeInterval) throws -> Set<pid_t> {
        guard ProcessInfo.processInfo.systemUptime < deadline else {
            throw Error.unknown(.deadlineExceeded)
        }

        var pids = [pid_t](repeating: 0, count: maxPIDCount)
        let returnedCount = pids.withUnsafeMutableBytes { bytes in
            proc_listallpids(bytes.baseAddress, Int32(bytes.count))
        }
        guard returnedCount > 0 else { throw Error.unknown(.processListUnavailable) }
        // libproc returns the number of PID entries copied.  A full buffer is
        // indistinguishable from truncation, so treat it as partial coverage.
        guard returnedCount < Int32(maxPIDCount) else {
            throw Error.unknown(.processListTooLarge)
        }
        guard ProcessInfo.processInfo.systemUptime < deadline else {
            throw Error.unknown(.deadlineExceeded)
        }

        var result = Set<pid_t>(minimumCapacity: Int(returnedCount))
        for pid in pids.prefix(Int(returnedCount)) {
            // PID 0 is the kernel placeholder and is intentionally excluded.
            guard pid > 0 else { continue }
            guard result.insert(pid).inserted else {
                throw Error.unknown(.processListUnavailable)
            }
        }
        guard !result.isEmpty else { throw Error.unknown(.processListUnavailable) }
        return result
    }

    private static func startTime(pid: pid_t) throws -> StartTime {
        var info = proc_bsdinfo()
        let expectedSize = Int32(MemoryLayout<proc_bsdinfo>.size)
        let got = proc_pidinfo(pid, PROC_PIDTBSDINFO, 0, &info, expectedSize)
        guard got == expectedSize,
              info.pbi_start_tvsec > 0 else {
            throw Error.unknown(.processIdentityUnavailable)
        }
        return StartTime(seconds: UInt64(info.pbi_start_tvsec),
                         microseconds: UInt64(info.pbi_start_tvusec))
    }

    private static func executablePath(pid: pid_t) throws -> String {
        var buffer = [UInt8](repeating: 0, count: Int(PROC_PIDPATHINFO_MAXSIZE))
        let length = buffer.withUnsafeMutableBytes { bytes in
            proc_pidpath(pid, bytes.baseAddress, UInt32(bytes.count))
        }
        guard length > 0, Int(length) < buffer.count else {
            throw Error.unknown(.executablePathUnavailable)
        }
        guard let terminator = buffer.firstIndex(of: 0), terminator > 0,
              let path = String(bytes: buffer[..<terminator], encoding: .utf8),
              path.hasPrefix("/"), !path.contains("\0") else {
            throw Error.unknown(.executablePathUnavailable)
        }
        return path
    }

    private static func executableBelongsToBundle(_ path: String,
                                                  root: BundleAdmission,
                                                  deadline: TimeInterval) throws -> Bool {
        guard path.hasPrefix("/"), !path.contains("\0") else {
            throw Error.unknown(.executablePathUnavailable)
        }

        var current = URL(fileURLWithPath: path).standardizedFileURL
        while true {
            guard ProcessInfo.processInfo.systemUptime < deadline else {
                throw Error.unknown(.deadlineExceeded)
            }
            var value = stat()
            let result = current.path.withCString { Darwin.lstat($0, &value) }
            guard result == 0 else {
                throw Error.unknown(.executablePathUnavailable)
            }
            let mode = value.st_mode & S_IFMT
            guard mode != S_IFLNK else {
                throw Error.unknown(.executablePathUnavailable)
            }
            if mode == S_IFDIR, fileIdentity(from: value) == root.identity {
                return true
            }
            guard current.path != "/" else { return false }
            current.deleteLastPathComponent()
        }
    }

    private static func canonicalBundleURL(_ bundle: URL) throws -> BundleAdmission {
        guard bundle.isFileURL,
              bundle.host == nil || bundle.host?.isEmpty == true,
              bundle.query == nil,
              bundle.fragment == nil,
              bundle.path.hasPrefix("/"),
              !bundle.path.contains("\0"),
              bundle.path.utf8.count <= 4_096 else {
            throw Error.invalidBundlePath
        }

        let rawComponents = bundle.path.split(separator: "/", omittingEmptySubsequences: true)
        guard !rawComponents.isEmpty,
              !rawComponents.contains(where: { $0 == "." || $0 == ".." }) else {
            throw Error.invalidBundlePath
        }

        let normalized = bundle.standardizedFileURL
        guard normalized.pathExtension.caseInsensitiveCompare("app") == .orderedSame,
              !normalized.lastPathComponent.isEmpty,
              normalized.lastPathComponent != ".app" else {
            throw Error.invalidBundlePath
        }

        // Caller-side admission uses no-follow traversal.  Repeat that path
        // property here so fresh native admission never trusts a symlinked
        // ancestor or bundle root.
        var current = URL(fileURLWithPath: "/", isDirectory: true)
        var rootIdentity: FileIdentity?
        for component in normalized.path.split(separator: "/", omittingEmptySubsequences: true) {
            current.appendPathComponent(String(component), isDirectory: true)
            var value = stat()
            let result = current.path.withCString { Darwin.lstat($0, &value) }
            guard result == 0,
                  (value.st_mode & S_IFMT) == S_IFDIR else {
                throw Error.invalidBundlePath
            }
            if current.path == normalized.path {
                rootIdentity = fileIdentity(from: value)
            }
        }
        guard let rootIdentity else { throw Error.invalidBundlePath }
        return BundleAdmission(identity: rootIdentity)
    }

    private static func fileIdentity(from value: stat) -> FileIdentity {
        FileIdentity(device: UInt64(UInt32(bitPattern: value.st_dev)),
                     inode: UInt64(value.st_ino))
    }
}
