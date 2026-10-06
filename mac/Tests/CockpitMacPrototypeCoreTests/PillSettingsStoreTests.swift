import XCTest
import Darwin
@testable import CockpitMacPrototypeCore

/// Trust-policy tests: wrong-uid / permissive-mode / symlink fixtures must all be refused,
/// never repaired. Temp dirs are per-user; `expectedUID` is injected to simulate foreign
/// ownership without needing root.
final class PillSettingsStoreTrustTests: XCTestCase {
    private var root: URL!
    private var dir: URL { root.appendingPathComponent("Cockpit", isDirectory: true) }
    private var file: URL { dir.appendingPathComponent(PillSettingsStore.fileName) }

    override func setUpWithError() throws {
        root = FileManager.default.temporaryDirectory.resolvingSymlinksInPath().appendingPathComponent("cockpit-trust-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: false)
    }

    override func tearDownWithError() throws { try? FileManager.default.removeItem(at: root) }

    private func makeDir(_ mode: Int = 0o700) throws {
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: false,
                                                attributes: [.posixPermissions: mode])
    }

    private func mode(_ url: URL) throws -> Int {
        (try FileManager.default.attributesOfItem(atPath: url.path)[.posixPermissions] as? NSNumber)?.intValue ?? -1
    }

    func testPermissiveDirectoryIsRefusedAndUntouched() throws {
        try makeDir(0o755)
        let store = PillSettingsStore(directory: dir)
        XCTAssertEqual(store.ensureDirectory(), .refused(.ioError(EACCES)))
        XCTAssertEqual(store.load().state, .protected(.ioError(EACCES)))
        assertVoidResultEqual(store.save(.defaults), .failure(.refused(.ioError(EACCES))))
        XCTAssertEqual(try mode(dir), 0o755, "must never chmod a pre-existing directory")
    }

    func testForeignOwnedDirectoryIsRefused() throws {
        try makeDir()
        let store = PillSettingsStore(directory: dir, expectedUID: getuid() &+ 1)
        XCTAssertEqual(store.ensureDirectory(), .refused(.ioError(EPERM)))
        XCTAssertEqual(store.load().state, .protected(.ioError(EPERM)))
        assertVoidResultEqual(store.save(.defaults), .failure(.refused(.ioError(EPERM))))
    }

    func testPermissiveFileIsProtectedOnLoadAndRefusedOnSave() throws {
        try makeDir()
        try Data(#"{"schema_version":1,"visible":false}"#.utf8).write(to: file)
        try FileManager.default.setAttributes([.posixPermissions: 0o644], ofItemAtPath: file.path)
        let store = PillSettingsStore(directory: dir)
        XCTAssertEqual(store.load().state, .protected(.ioError(EACCES)))
        assertVoidResultEqual(store.save(.defaults), .failure(.refused(.ioError(EACCES))))
        XCTAssertEqual(try mode(file), 0o644)
    }

    func testGroupWritableDirectoryIsRefused() throws {
        try makeDir(0o720)
        XCTAssertEqual(PillSettingsStore(directory: dir).ensureDirectory(), .refused(.ioError(EACCES)))
    }

    func testMalformedFileIsPreservedByteForByte() throws {
        try makeDir()
        let original = Data([0xFF, 0x00, 0x7B]) // not valid JSON at all
        try original.write(to: file)
        try FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: file.path)
        let store = PillSettingsStore(directory: dir)
        XCTAssertEqual(store.load(), .init(settings: .defaults, state: .protected(.malformed)))
        XCTAssertEqual(try Data(contentsOf: file), original)
        XCTAssertEqual(store.load(), .init(settings: .defaults, state: .protected(.malformed)))
        XCTAssertEqual(try Data(contentsOf: file), original)
    }

    func testSymlinkedDirectoryAndFileAreRefused() throws {
        let realDir = root.appendingPathComponent("real", isDirectory: true)
        try FileManager.default.createDirectory(at: realDir, withIntermediateDirectories: false,
                                                attributes: [.posixPermissions: 0o700])
        let linkDir = root.appendingPathComponent("link", isDirectory: true)
        try FileManager.default.createSymbolicLink(at: linkDir, withDestinationURL: realDir)
        let linked = PillSettingsStore(directory: linkDir)
        XCTAssertEqual(linked.ensureDirectory(), .refused(.symlink))
        XCTAssertEqual(linked.load().state, .protected(.symlink))
        assertVoidResultEqual(linked.save(.defaults), .failure(.refused(.symlink)))
        XCTAssertTrue(try FileManager.default.contentsOfDirectory(atPath: realDir.path).isEmpty)

        try makeDir()
        let target = root.appendingPathComponent("planted.json")
        try Data(#"{"schema_version":1}"#.utf8).write(to: target)
        try FileManager.default.createSymbolicLink(at: file, withDestinationURL: target)
        let store = PillSettingsStore(directory: dir)
        XCTAssertEqual(store.load().state, .protected(.symlink))
        assertVoidResultEqual(store.save(PillSettings(visible: false)), .failure(.refused(.symlink)))
        XCTAssertEqual(try Data(contentsOf: target), Data(#"{"schema_version":1}"#.utf8))
    }

    func testSymlinkedAncestorIsRefusedBeforeCreation() throws {
        let realParent = root.appendingPathComponent("real-parent", isDirectory: true)
        try FileManager.default.createDirectory(at: realParent, withIntermediateDirectories: false,
                                                attributes: [.posixPermissions: 0o700])
        let linkParent = root.appendingPathComponent("link-parent", isDirectory: true)
        try FileManager.default.createSymbolicLink(at: linkParent, withDestinationURL: realParent)
        let nested = linkParent.appendingPathComponent("Cockpit", isDirectory: true)
        let store = PillSettingsStore(directory: nested)
        XCTAssertEqual(store.ensureDirectory(), .refused(.symlink))
        XCTAssertEqual(store.load().state, .protected(.symlink))
        assertVoidResultEqual(store.save(.defaults), .failure(.refused(.symlink)))
        XCTAssertFalse(FileManager.default.fileExists(atPath: realParent.appendingPathComponent("Cockpit").path))
    }

    func testFreshSaveUsesOwnerOnlyModes() throws {
        let store = PillSettingsStore(directory: dir)
        assertVoidResultEqual(store.save(.defaults), .success(()))
        XCTAssertEqual(try mode(dir), 0o700)
        XCTAssertEqual(try mode(file), 0o600)
    }
}

final class PillInstanceLockTrustTests: XCTestCase {
    private var dir: URL!
    private var lockPath: URL { dir.appendingPathComponent(PillSettingsStore.lockName) }

    override func setUpWithError() throws {
        dir = FileManager.default.temporaryDirectory.resolvingSymlinksInPath().appendingPathComponent("cockpit-lock-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: false,
                                                attributes: [.posixPermissions: 0o700])
    }

    override func tearDownWithError() throws { try? FileManager.default.removeItem(at: dir) }

    private func mode(_ url: URL) throws -> Int {
        (try FileManager.default.attributesOfItem(atPath: url.path)[.posixPermissions] as? NSNumber)?.intValue ?? -1
    }

    func testAcquireCreatesOwnerOnlyLockFile() throws {
        guard case .acquired(let lock) = PillInstanceLock.acquire(directory: dir) else { return XCTFail("acquire") }
        XCTAssertEqual(try mode(lockPath), 0o600)
        lock.release()
    }

    func testPermissiveDirectoryRefusesAcquire() throws {
        try FileManager.default.setAttributes([.posixPermissions: 0o755], ofItemAtPath: dir.path)
        guard case .failed(let reason) = PillInstanceLock.acquire(directory: dir) else { return XCTFail("must refuse") }
        XCTAssertEqual(reason, "lock_dir_untrusted")
        XCTAssertFalse(FileManager.default.fileExists(atPath: lockPath.path))
    }

    func testForeignOwnedDirectoryRefusesAcquire() throws {
        guard case .failed(let reason) = PillInstanceLock.acquire(directory: dir, expectedUID: getuid() &+ 1)
        else { return XCTFail("must refuse") }
        XCTAssertEqual(reason, "lock_dir_untrusted")
    }

    func testPermissivePreexistingLockFileIsRefusedAndPreserved() throws {
        try Data("sentinel".utf8).write(to: lockPath)
        try FileManager.default.setAttributes([.posixPermissions: 0o644], ofItemAtPath: lockPath.path)
        guard case .failed(let reason) = PillInstanceLock.acquire(directory: dir) else { return XCTFail("must refuse") }
        XCTAssertEqual(reason, "lock_file_untrusted")
        // A failed acquire must never delete or repair a file it did not create.
        XCTAssertEqual(try Data(contentsOf: lockPath), Data("sentinel".utf8))
        XCTAssertEqual(try mode(lockPath), 0o644)
    }

    func testSymlinkedLockIsRefusedAndPreserved() throws {
        let target = dir.appendingPathComponent("planted")
        try Data("sentinel".utf8).write(to: target)
        try FileManager.default.createSymbolicLink(at: lockPath, withDestinationURL: target)
        guard case .failed = PillInstanceLock.acquire(directory: dir) else { return XCTFail("must refuse") }
        var st = stat()
        XCTAssertEqual(lstat(lockPath.path, &st), 0)
        XCTAssertEqual(st.st_mode & S_IFMT, S_IFLNK, "the planted symlink must survive a failed acquire")
    }

    func testSymlinkedLockAncestorIsRefused() throws {
        let realParent = dir.deletingLastPathComponent().appendingPathComponent("real-lock-parent", isDirectory: true)
        try FileManager.default.createDirectory(at: realParent, withIntermediateDirectories: false,
                                                attributes: [.posixPermissions: 0o700])
        let linkParent = dir.deletingLastPathComponent().appendingPathComponent("link-lock-parent", isDirectory: true)
        try FileManager.default.createSymbolicLink(at: linkParent, withDestinationURL: realParent)
        let nested = linkParent.appendingPathComponent("Cockpit", isDirectory: true)
        guard case .failed(let reason) = PillInstanceLock.acquire(directory: nested) else {
            return XCTFail("must refuse symlinked ancestor")
        }
        XCTAssertEqual(reason, "lock_symlink")
        XCTAssertFalse(FileManager.default.fileExists(atPath: realParent.appendingPathComponent("Cockpit").path))
    }

    func testNonRegularLockIsRefusedAndPreserved() throws {
        try FileManager.default.createDirectory(at: lockPath, withIntermediateDirectories: false)
        guard case .failed = PillInstanceLock.acquire(directory: dir) else { return XCTFail("must refuse") }
        XCTAssertTrue(FileManager.default.fileExists(atPath: lockPath.path))
    }

    func testMissingDirectoryFailsWithoutCreatingAnything() throws {
        let missing = dir.appendingPathComponent("nope", isDirectory: true)
        guard case .failed = PillInstanceLock.acquire(directory: missing) else { return XCTFail("must fail") }
        XCTAssertFalse(FileManager.default.fileExists(atPath: missing.path))
    }
}
