import Foundation
import Darwin

/// POSIX-level settings persistence. Never follows symlinks, never chmods an existing
/// directory, writes via exclusive temp + fsync + rename.
///
/// Trust policy: every pre-existing component we rely on (settings directory, settings
/// file, lock file) must be owned by `expectedUID` (the real uid by default) and be
/// owner-only — any group/other permission bits, a foreign owner, or a symlink is an
/// unconditional refusal. Pre-existing paths are never repaired: no chmod, no chown.
/// Ownership/perms failures surface as `.ioError(EPERM)` / `.ioError(EACCES)` so callers
/// get a typed refusal without widening the shared `SettingsFailure` model.
public struct PillSettingsStore {
    public static let fileName = "pill-settings.json"
    public static let lockName = "pill.lock"

    public let directory: URL
    let expectedUID: uid_t

    public init(directory: URL) { self.init(directory: directory, expectedUID: getuid()) }

    init(directory: URL, expectedUID: uid_t) {
        self.directory = directory
        self.expectedUID = expectedUID
    }

    /// Nil when the component may be relied on; the refusal otherwise.
    private func trusted(_ st: stat) -> SettingsFailure? {
        if st.st_uid != expectedUID { return .ioError(EPERM) }
        if (st.st_mode & 0o077) != 0 { return .ioError(EACCES) }
        return nil
    }

    /// Opens every ancestor descriptor without following a symlink. Intermediate system
    /// directories need not be owner-only, but must be real directories in the walked path.
    fileprivate func openParentDirectory() -> (fd: Int32, failure: SettingsFailure?) {
        let path = directory.standardizedFileURL.path
        guard path.hasPrefix("/") else { return (-1, .ioError(EINVAL)) }
        let components = path.split(separator: "/", omittingEmptySubsequences: true).map(String.init)
        guard !components.isEmpty else { return (-1, .ioError(EINVAL)) }
        var fd = open("/", O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)
        guard fd >= 0 else { return (-1, .ioError(errno)) }
        for component in components.dropLast() {
            let next = openat(fd, component, O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)
            if next < 0 {
                let failure: SettingsFailure = errno == ELOOP ? .symlink : .ioError(errno)
                close(fd)
                return (-1, failure)
            }
            close(fd)
            fd = next
        }
        return (fd, nil)
    }

    fileprivate var directoryLeaf: String {
        directory.standardizedFileURL.path
            .split(separator: "/", omittingEmptySubsequences: true).last.map(String.init) ?? ""
    }

    public static func defaultDirectory() -> URL {
        FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent("Library/Application Support/Cockpit", isDirectory: true)
    }

    public struct LoadResult: Equatable {
        public let settings: PillSettings
        public let state: SettingsLoadState
    }

    public enum DirectoryResult: Equatable {
        case existing
        case created
        case refused(SettingsFailure)
    }

    /// Creates the directory (0700) only when absent. An existing directory is never modified.
    public func ensureDirectory() -> DirectoryResult {
        let (parentFD, parentFailure) = openParentDirectory()
        guard parentFD >= 0 else { return .refused(parentFailure ?? .ioError(EIO)) }
        defer { close(parentFD) }
        var st = stat()
        if fstatat(parentFD, directoryLeaf, &st, AT_SYMLINK_NOFOLLOW) == 0 {
            if (st.st_mode & S_IFMT) == S_IFLNK { return .refused(.symlink) }
            if (st.st_mode & S_IFMT) != S_IFDIR { return .refused(.ioError(ENOTDIR)) }
            if let failure = trusted(st) { return .refused(failure) }
            return .existing
        }
        guard errno == ENOENT else { return .refused(.ioError(errno)) }
        if mkdirat(parentFD, directoryLeaf, 0o700) != 0 && errno != EEXIST {
            return .refused(.ioError(errno))
        }
        guard fstatat(parentFD, directoryLeaf, &st, AT_SYMLINK_NOFOLLOW) == 0 else {
            return .refused(.ioError(errno))
        }
        if (st.st_mode & S_IFMT) == S_IFLNK { return .refused(.symlink) }
        if (st.st_mode & S_IFMT) != S_IFDIR { return .refused(.ioError(ENOTDIR)) }
        if let failure = trusted(st) { return .refused(failure) }
        return .created
    }

    private func openDirectory() -> (fd: Int32, failure: SettingsFailure?) {
        let (parentFD, parentFailure) = openParentDirectory()
        guard parentFD >= 0 else { return (-1, parentFailure ?? .ioError(EIO)) }
        let fd = openat(parentFD, directoryLeaf, O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)
        close(parentFD)
        if fd >= 0 {
            var st = stat()
            if fstat(fd, &st) != 0 {
                let failure = SettingsFailure.ioError(errno)
                close(fd)
                return (-1, failure)
            }
            if (st.st_mode & S_IFMT) == S_IFDIR, trusted(st) == nil { return (fd, nil) }
            let failure = (st.st_mode & S_IFMT) == S_IFDIR ? trusted(st)! : .ioError(ENOTDIR)
            close(fd)
            return (-1, failure)
        }
        let err = errno
        var st = stat()
        let (retryParentFD, _) = openParentDirectory()
        if retryParentFD >= 0 {
            if fstatat(retryParentFD, directoryLeaf, &st, AT_SYMLINK_NOFOLLOW) == 0,
               (st.st_mode & S_IFMT) == S_IFLNK {
                close(retryParentFD)
                return (-1, .symlink)
            }
            close(retryParentFD)
        }
        return (-1, .ioError(err))
    }

    public func load() -> LoadResult {
        let (dirFD, dirFailure) = openDirectory()
        if dirFD < 0 {
            if case .ioError(let e)? = dirFailure, e == ENOENT {
                return LoadResult(settings: .defaults, state: .missing)
            }
            return LoadResult(settings: .defaults, state: .protected(dirFailure ?? .ioError(EIO)))
        }
        defer { close(dirFD) }

        let fd = openat(dirFD, Self.fileName, O_RDONLY | O_NOFOLLOW | O_CLOEXEC | O_NONBLOCK)
        if fd < 0 {
            let err = errno
            if err == ENOENT { return LoadResult(settings: .defaults, state: .missing) }
            if err == ELOOP { return LoadResult(settings: .defaults, state: .protected(.symlink)) }
            return LoadResult(settings: .defaults, state: .protected(.ioError(err)))
        }
        defer { close(fd) }

        var st = stat()
        guard fstat(fd, &st) == 0 else { return protectedDefaults(.ioError(errno)) }
        guard (st.st_mode & S_IFMT) == S_IFREG else { return protectedDefaults(.malformed) }
        if let failure = trusted(st) { return protectedDefaults(failure) }
        guard st.st_size <= off_t(PillSettings.maxFileBytes) else { return protectedDefaults(.oversized) }

        var data = Data()
        var buffer = [UInt8](repeating: 0, count: 8192)
        while true {
            let n = read(fd, &buffer, buffer.count)
            if n < 0 {
                if errno == EINTR { continue }
                return protectedDefaults(.ioError(errno))
            }
            if n == 0 { break }
            data.append(buffer, count: n)
            if data.count > PillSettings.maxFileBytes { return protectedDefaults(.oversized) }
        }
        switch PillSettingsCodec.decode(data) {
        case .success(let settings): return LoadResult(settings: settings, state: .loaded)
        case .failure(let failure): return protectedDefaults(failure)
        }
    }

    private func protectedDefaults(_ failure: SettingsFailure) -> LoadResult {
        LoadResult(settings: .defaults, state: .protected(failure))
    }

    public enum SaveError: Error, Equatable {
        case encodeFailed
        case refused(SettingsFailure)
    }

    /// Caller must have consulted `SettingsWritePolicy`. Still refuses symlinked or non-regular targets.
    public func save(_ settings: PillSettings) -> Result<Void, SaveError> {
        guard let data = PillSettingsCodec.encode(settings) else { return .failure(.encodeFailed) }
        switch ensureDirectory() {
        case .refused(let f): return .failure(.refused(f))
        case .existing, .created: break
        }
        let (dirFD, dirFailure) = openDirectory()
        guard dirFD >= 0 else { return .failure(.refused(dirFailure ?? .ioError(EIO))) }
        defer { close(dirFD) }

        var st = stat()
        if fstatat(dirFD, Self.fileName, &st, AT_SYMLINK_NOFOLLOW) == 0 {
            if (st.st_mode & S_IFMT) == S_IFLNK { return .failure(.refused(.symlink)) }
            if (st.st_mode & S_IFMT) != S_IFREG { return .failure(.refused(.malformed)) }
            if let failure = trusted(st) { return .failure(.refused(failure)) }
        } else if errno != ENOENT {
            return .failure(.refused(.ioError(errno)))
        }

        let temp = "\(Self.fileName).tmp-\(getpid())-\(UUID().uuidString)"
        let fd = openat(dirFD, temp, O_WRONLY | O_CREAT | O_EXCL | O_NOFOLLOW | O_CLOEXEC, 0o600)
        guard fd >= 0 else { return .failure(.refused(.ioError(errno))) }
        var ok = true
        var failure = SettingsFailure.ioError(EIO)
        data.withUnsafeBytes { raw in
            var offset = 0
            while offset < raw.count {
                let n = write(fd, raw.baseAddress!.advanced(by: offset), raw.count - offset)
                if n < 0 {
                    if errno == EINTR { continue }
                    ok = false; failure = .ioError(errno); return
                }
                if n == 0 { ok = false; failure = .ioError(EIO); return }
                offset += n
            }
        }
        if ok, fcntl(fd, F_FULLFSYNC) != 0, fsync(fd) != 0 { ok = false; failure = .ioError(errno) }
        if close(fd) != 0 && ok { ok = false; failure = .ioError(errno) }
        if ok, renameat(dirFD, temp, dirFD, Self.fileName) != 0 { ok = false; failure = .ioError(errno) }
        if !ok {
            unlinkat(dirFD, temp, 0) // our own freshly created temp name only
            return .failure(.refused(failure))
        }
        _ = fsync(dirFD)
        return .success(())
    }
}

/// Single-instance lock. Holds the flock for the process lifetime; release never deletes the file.
public final class PillInstanceLock {
    public enum Acquire {
        case acquired(PillInstanceLock)
        case alreadyHeld
        case failed(String)
    }

    private var fd: Int32

    private init(fd: Int32) { self.fd = fd }

    public var isHeld: Bool { fd >= 0 }

    /// The settings directory, the lock file, and any ancestor we rely on must pass the same
    /// ownership/owner-only policy as `PillSettingsStore`. When acquiring fails after we
    /// created a NEW lock file, the partial artifact is unlinked; pre-existing files are
    /// never removed (release only unlocks/closes).
    public static func acquire(directory: URL, expectedUID: uid_t = getuid()) -> Acquire {
        let store = PillSettingsStore(directory: directory, expectedUID: expectedUID)
        let (parentFD, parentFailure) = store.openParentDirectory()
        guard parentFD >= 0 else {
            let code: Int32
            if case .symlink? = parentFailure { return .failed("lock_symlink") }
            if case .ioError(let value)? = parentFailure { code = value } else { code = EIO }
            return .failed("lock_dir_errno_\(code)")
        }
        let dirFD = openat(parentFD, store.directoryLeaf, O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)
        close(parentFD)
        guard dirFD >= 0 else {
            return .failed(errno == ELOOP ? "lock_symlink" : "lock_dir_errno_\(errno)")
        }
        defer { close(dirFD) }

        var dirStat = stat()
        guard fstat(dirFD, &dirStat) == 0, (dirStat.st_mode & S_IFMT) == S_IFDIR,
              dirStat.st_uid == expectedUID, (dirStat.st_mode & 0o077) == 0 else {
            return .failed("lock_dir_untrusted")
        }

        var created = false
        var fd = openat(dirFD, PillSettingsStore.lockName,
                        O_RDWR | O_CREAT | O_EXCL | O_NOFOLLOW | O_CLOEXEC, 0o600)
        if fd < 0 {
            guard errno == EEXIST else {
                return .failed(errno == ELOOP ? "lock_symlink" : "lock_open_errno_\(errno)")
            }
            fd = openat(dirFD, PillSettingsStore.lockName, O_RDWR | O_NOFOLLOW | O_CLOEXEC)
            guard fd >= 0 else {
                return .failed(errno == ELOOP ? "lock_symlink" : "lock_open_errno_\(errno)")
            }
        } else {
            created = true
        }

        func abandon(_ result: Acquire) -> Acquire {
            close(fd)
            if created { unlinkat(dirFD, PillSettingsStore.lockName, 0) } // our partial only
            return result
        }

        var st = stat()
        guard fstat(fd, &st) == 0, (st.st_mode & S_IFMT) == S_IFREG else {
            return abandon(.failed("lock_not_regular_file"))
        }
        guard st.st_uid == expectedUID, (st.st_mode & 0o077) == 0 else {
            return abandon(.failed("lock_file_untrusted"))
        }
        if flock(fd, LOCK_EX | LOCK_NB) != 0 {
            let err = errno
            return abandon(err == EWOULDBLOCK ? .alreadyHeld : .failed("lock_flock_errno_\(err)"))
        }
        return .acquired(PillInstanceLock(fd: fd))
    }

    /// Idempotent. Unlocks and closes only; the lock file itself is left in place.
    public func release() {
        guard fd >= 0 else { return }
        _ = flock(fd, LOCK_UN)
        close(fd)
        fd = -1
    }

    deinit { release() }
}
