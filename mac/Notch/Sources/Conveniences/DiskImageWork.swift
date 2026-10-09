// Copyright (c) 2026 Damned Ventures LLC, d/b/a Orthic Labs. All rights reserved.

import AppKit
import Darwin
import Security

// The disk image installer's off-main-thread half: reading a mounted image,
// judging its app, and the staged copy into /Applications. `DiskImageInstaller`
// (the main-actor half) decides when to call it and what the notch shows.
// Everything here is synchronous and Sendable so it can run on a detached task.

// MARK: - Values

/// An app's version as its Info.plist states it.
struct DiskImageVersion: Sendable, Equatable {
    var short: String?
    var build: String?

    /// What to show: the marketing version, else the build.
    var display: String? { short ?? build }

    /// Both the marketing version and the build are stated and equal.
    func isSameBuild(as other: DiskImageVersion) -> Bool {
        guard let a = short, let b = other.short, a == b,
              let c = build, let d = other.build, c == d else { return false }
        return true
    }

    /// The version for a "this one versus that one" line: "0.2.0", or
    /// "0.2.0 (build 5)" when the marketing versions are equal and the builds
    /// are what tell the two apart.
    func labelled(comparedWith other: DiskImageVersion) -> String? {
        guard let short else { return build }
        if let build, other.short == short, other.build != nil { return "\(short) (build \(build))" }
        return short
    }

    /// Whether this version is strictly newer than `other`. The marketing
    /// version decides when both have one and they differ; the build decides
    /// otherwise. Numeric ordering, so 1.10 is newer than 1.9.
    func isNewer(than other: DiskImageVersion) -> Bool {
        if let a = short, let b = other.short, a != b {
            return a.compare(b, options: .numeric) == .orderedDescending
        }
        if let a = build, let b = other.build, a != b {
            return a.compare(b, options: .numeric) == .orderedDescending
        }
        return false
    }
}

/// A file's device and inode: the same file stays the same under a rename.
struct DiskImageFileID: Equatable, Sendable {
    let device: UInt64
    let inode: UInt64

    /// nil unless `url` is a regular file.
    init?(of url: URL) {
        guard let attributes = try? FileManager.default.attributesOfItem(atPath: url.path),
              (attributes[.type] as? FileAttributeType) == .typeRegular,
              let device = (attributes[.systemNumber] as? NSNumber)?.uint64Value,
              let inode = (attributes[.systemFileNumber] as? NSNumber)?.uint64Value
        else { return nil }
        self.device = device
        self.inode = inode
    }
}

/// A copy of an app already in /Applications.
struct DiskImageInstalledCopy: Sendable {
    let url: URL
    let version: DiskImageVersion
}

/// An old copy that an install moved to the Trash, and where it came from.
struct DiskImageTrashedCopy: Sendable {
    let original: URL
    let inTrash: URL
}

/// The one app on a mounted image, with everything the decision needs.
struct DiskImageApp: Sendable {
    let mountURL: URL
    let appURL: URL
    let imageURL: URL
    let imageID: DiskImageFileID?
    let name: String
    let bundleID: String?
    let version: DiskImageVersion
    /// The code signature is intact and strictly valid.
    let signatureValid: Bool
    /// Gatekeeper accepts it as notarized.
    let notarized: Bool
    /// Copies with this bundle id already in /Applications.
    let copies: [DiskImageInstalledCopy]
    /// Something that is not a copy of this app sits at the name it would take.
    let nameTaken: Bool

    var trusted: Bool { signatureValid && notarized }
    var destinationURL: URL { DiskImageWork.applicationsFolder.appendingPathComponent(appURL.lastPathComponent) }
}

/// What a mounted image holds, as far as the installer cares.
enum DiskImageContents: Sendable {
    case app(DiskImageApp)
    case installer(mountURL: URL, pkgURL: URL)
    case several(mountURL: URL)

    var mountURL: URL {
        switch self {
        case .app(let app): return app.mountURL
        case .installer(let mount, _): return mount
        case .several(let mount): return mount
        }
    }
}

enum DiskImageFailure: Sendable {
    /// Something took the app's place since the image was read.
    case occupied
    /// The staged copy failed or could not be made.
    case copy
    /// The staged copy did not pass the checks.
    case verification
    /// An old copy could not be moved to the Trash.
    case replace
}

enum DiskImageOutcome: Sendable {
    case installed
    case cancelled
    case failed(DiskImageFailure)
}

struct DiskImageInstallResult: Sendable {
    let outcome: DiskImageOutcome
    var replaced: [DiskImageTrashedCopy] = []
    var ejected = false
    var downloadTrashed = false
    /// Pulse replacing itself: the verified copy waiting in `workDirectory`
    /// for the relauncher to swap in once Pulse has quit.
    var stagedSelf: URL?
    var workDirectory: URL?
}

// MARK: - Cancellation

/// The handle for one running install: a cancel flag, the child processes it
/// has started (stopped by pid), and the commit point after which cancelling
/// is no longer possible.
final class DiskImageInstallControl: @unchecked Sendable {
    private let lock = NSLock()
    private var cancelled = false
    private var committed = false
    private var children = Set<pid_t>()
    private let onCommit: (@Sendable () -> Void)?

    init(onCommit: (@Sendable () -> Void)? = nil) {
        self.onCommit = onCommit
    }

    var isCancelled: Bool {
        lock.lock(); defer { lock.unlock() }
        return cancelled
    }

    /// Asks the install to stop and terminates the children it started.
    /// False when the commit point has passed: it is too late.
    @discardableResult
    func requestCancel() -> Bool {
        lock.lock(); defer { lock.unlock() }
        guard !committed else { return false }
        cancelled = true
        for pid in children { kill(pid, SIGTERM) }
        return true
    }

    /// Crossing the point of no return. False when cancelled first, in which
    /// case the install must back out.
    func commit() -> Bool {
        lock.lock()
        if cancelled { lock.unlock(); return false }
        committed = true
        lock.unlock()
        onCommit?()
        return true
    }

    fileprivate func track(_ pid: pid_t) {
        lock.lock(); defer { lock.unlock() }
        if cancelled { kill(pid, SIGTERM) } else { children.insert(pid) }
    }

    fileprivate func untrack(_ pid: pid_t) {
        lock.lock(); defer { lock.unlock() }
        children.remove(pid)
    }
}

// MARK: - Work

enum DiskImageWork {
    static let applicationsFolder = URL(fileURLWithPath: "/Applications", isDirectory: true)

    private struct Output {
        let status: Int32
        let data: Data
        var text: String { String(decoding: data, as: UTF8.self) }
    }

    // MARK: Reading an image

    /// What the volume mounted at `mountURL` holds, or nil when it is not a
    /// disk image mount or holds nothing the installer handles.
    static func inspect(mountedAt mountURL: URL) -> DiskImageContents? {
        guard let imageURL = backingImage(of: mountURL) else { return nil }
        let keys: [URLResourceKey] = [.isSymbolicLinkKey]
        let items = (try? FileManager.default.contentsOfDirectory(
            at: mountURL, includingPropertiesForKeys: keys, options: [.skipsHiddenFiles])) ?? []
        let real = items.filter { (try? $0.resourceValues(forKeys: [.isSymbolicLinkKey]).isSymbolicLink) != true }
        let apps = real.filter { $0.pathExtension.lowercased() == "app" }
        let packages = real.filter { ["pkg", "mpkg"].contains($0.pathExtension.lowercased()) }

        if apps.count > 1 { return .several(mountURL: mountURL) }
        if let appURL = apps.first { return .app(describe(appURL, on: mountURL, image: imageURL)) }
        if let pkg = packages.first { return .installer(mountURL: mountURL, pkgURL: pkg) }
        return nil
    }

    /// The image file behind a mount, from `hdiutil info -plist`. nil when the
    /// volume is not an attached image (a real disk, a network share).
    private static func backingImage(of mountURL: URL) -> URL? {
        guard let output = run("/usr/bin/hdiutil", ["info", "-plist"]), output.status == 0,
              let plist = try? PropertyListSerialization.propertyList(from: output.data, format: nil),
              let images = (plist as? [String: Any])?["images"] as? [[String: Any]]
        else { return nil }
        let target = mountURL.resolvingSymlinksInPath().path
        for image in images {
            guard let entities = image["system-entities"] as? [[String: Any]],
                  let path = image["image-path"] as? String else { continue }
            let mounted = entities.contains { entity in
                guard let point = entity["mount-point"] as? String else { return false }
                return URL(fileURLWithPath: point).resolvingSymlinksInPath().path == target
            }
            if mounted { return URL(fileURLWithPath: path) }
        }
        return nil
    }

    private static func describe(_ appURL: URL, on mountURL: URL, image imageURL: URL) -> DiskImageApp {
        let info = infoDictionary(of: appURL)
        let bundleID = info["CFBundleIdentifier"] as? String
        let name = (info["CFBundleDisplayName"] as? String)
            ?? (info["CFBundleName"] as? String)
            ?? appURL.deletingPathExtension().lastPathComponent
        let signature = signatureIsValid(appURL)
        let notarized = signature && gatekeeperNotarizes(appURL, control: nil)

        let copies = installedCopies(bundleID: bundleID)
        let destination = applicationsFolder.appendingPathComponent(appURL.lastPathComponent)
        let occupied = FileManager.default.fileExists(atPath: destination.path)
        let isCopy = copies.contains { $0.url.path == destination.path }

        return DiskImageApp(
            mountURL: mountURL, appURL: appURL, imageURL: imageURL,
            imageID: DiskImageFileID(of: imageURL),
            name: name, bundleID: bundleID, version: version(from: info),
            signatureValid: signature, notarized: notarized,
            copies: copies, nameTaken: occupied && !isCopy)
    }

    private static func infoDictionary(of appURL: URL) -> [String: Any] {
        let plist = appURL.appendingPathComponent("Contents/Info.plist")
        return (NSDictionary(contentsOf: plist) as? [String: Any]) ?? [:]
    }

    private static func version(from info: [String: Any]) -> DiskImageVersion {
        DiskImageVersion(short: info["CFBundleShortVersionString"] as? String,
                         build: info["CFBundleVersion"] as? String)
    }

    /// The apps directly inside /Applications that share `bundleID`.
    static func installedCopies(bundleID: String?) -> [DiskImageInstalledCopy] {
        guard let bundleID else { return [] }
        let entries = (try? FileManager.default.contentsOfDirectory(
            at: applicationsFolder, includingPropertiesForKeys: nil, options: [.skipsHiddenFiles])) ?? []
        return entries.compactMap { url in
            guard url.pathExtension.lowercased() == "app" else { return nil }
            let info = infoDictionary(of: url)
            guard info["CFBundleIdentifier"] as? String == bundleID else { return nil }
            return DiskImageInstalledCopy(url: url, version: version(from: info))
        }
    }

    // MARK: Checks

    /// The bundle's code signature validates with the strict flags, all
    /// architectures and nested code included.
    static func signatureIsValid(_ appURL: URL) -> Bool {
        var code: SecStaticCode?
        guard SecStaticCodeCreateWithPath(appURL as CFURL, SecCSFlags(), &code) == errSecSuccess,
              let code else { return false }
        let flags = SecCSFlags(rawValue: kSecCSStrictValidate | kSecCSCheckNestedCode | kSecCSCheckAllArchitectures)
        return SecStaticCodeCheckValidity(code, flags, nil) == errSecSuccess
    }

    /// Gatekeeper accepts the app and names a notarized Developer ID as the
    /// source (so a machine with Gatekeeper off proves nothing).
    static func gatekeeperNotarizes(_ appURL: URL, control: DiskImageInstallControl?) -> Bool {
        guard let output = run("/usr/sbin/spctl", ["-a", "-vv", "-t", "exec", appURL.path],
                               control: control, mergeError: true),
              output.status == 0 else { return false }
        return output.text.contains("Notarized Developer ID")
    }

    // MARK: Installing

    /// Copies the image's app into /Applications through a staging folder on
    /// the same volume, checks the copy, and renames it into place. Nothing is
    /// overwritten: with `replacing`, the old copies are moved to the Trash
    /// first and put back if anything fails. Then the image is ejected and,
    /// if asked, the downloaded file trashed.
    static func install(_ app: DiskImageApp, replacing: Bool, trashDownload: Bool,
                        control: DiskImageInstallControl) -> DiskImageInstallResult {
        let fm = FileManager.default
        let destination = app.destinationURL

        // Look again: /Applications may have changed since the image was read.
        let copies = installedCopies(bundleID: app.bundleID)
        let occupied = fm.fileExists(atPath: destination.path)
        let occupiedByCopy = copies.contains { $0.url.path == destination.path }
        if replacing {
            if occupied && !occupiedByCopy { return .init(outcome: .failed(.occupied)) }
        } else if occupied || !copies.isEmpty {
            return .init(outcome: .failed(.occupied))
        }
        if control.isCancelled { return .init(outcome: .cancelled) }

        guard let staging = makeStagingFolder(beside: applicationsFolder) else {
            return .init(outcome: .failed(.copy))
        }
        let staged = staging.appendingPathComponent(destination.lastPathComponent)

        // Copy, then check the copy as strictly as the original was checked.
        let copied = run("/usr/bin/ditto", [app.appURL.path, staged.path], control: control)
        if control.isCancelled { discard(staging); return .init(outcome: .cancelled) }
        guard copied?.status == 0 else { discard(staging); return .init(outcome: .failed(.copy)) }

        let sameApp = infoDictionary(of: staged)["CFBundleIdentifier"] as? String == app.bundleID
        var passes = sameApp
        if passes && app.signatureValid { passes = signatureIsValid(staged) }
        if passes && app.trusted { passes = gatekeeperNotarizes(staged, control: control) }
        if control.isCancelled { discard(staging); return .init(outcome: .cancelled) }
        guard passes else { discard(staging); return .init(outcome: .failed(.verification)) }

        // Make room, remembering where everything went.
        var trashed: [DiskImageTrashedCopy] = []
        if replacing {
            for copy in copies {
                var landed: NSURL?
                do {
                    try fm.trashItem(at: copy.url, resultingItemURL: &landed)
                } catch {
                    putBack(trashed); discard(staging)
                    return .init(outcome: .failed(.replace))
                }
                if let landed { trashed.append(.init(original: copy.url, inTrash: landed as URL)) }
                if control.isCancelled { putBack(trashed); discard(staging); return .init(outcome: .cancelled) }
            }
        }

        // The point of no return.
        guard control.commit() else {
            putBack(trashed); discard(staging)
            return .init(outcome: .cancelled)
        }
        guard renameWithoutOverwriting(staged, to: destination) else {
            putBack(trashed); discard(staging)
            return .init(outcome: .failed(.copy))
        }
        discard(staging)

        var result = DiskImageInstallResult(outcome: .installed, replaced: trashed)
        result.ejected = eject(app.mountURL)
        if trashDownload, result.ejected, let id = app.imageID, DiskImageFileID(of: app.imageURL) == id {
            result.downloadTrashed = (try? fm.trashItem(at: app.imageURL, resultingItemURL: nil)) != nil
        }
        return result
    }

    /// Pulse replacing itself. Pulse cannot copy over the bundle it runs from, so
    /// this only makes a verified copy in a private temp folder (checked as
    /// strictly as `install` checks its staged copy), crosses the commit point,
    /// and ejects the image. The detached relauncher does the swap after Pulse
    /// exits (`PulseRelauncher`).
    static func prepareSelfReplacement(_ app: DiskImageApp, trashDownload: Bool,
                                       control: DiskImageInstallControl) -> DiskImageInstallResult {
        let fm = FileManager.default
        if control.isCancelled { return .init(outcome: .cancelled) }
        let work = fm.temporaryDirectory
            .appendingPathComponent("dev.orthic.pulse.selfinstall-\(UUID().uuidString)", isDirectory: true)
        guard (try? fm.createDirectory(at: work, withIntermediateDirectories: true)) != nil else {
            return .init(outcome: .failed(.copy))
        }
        let staged = work.appendingPathComponent(app.destinationURL.lastPathComponent)
        func abandon(_ outcome: DiskImageOutcome) -> DiskImageInstallResult {
            try? fm.removeItem(at: work)
            return .init(outcome: outcome)
        }

        let copied = run("/usr/bin/ditto", [app.appURL.path, staged.path], control: control)
        if control.isCancelled { return abandon(.cancelled) }
        guard copied?.status == 0 else { return abandon(.failed(.copy)) }

        var passes = infoDictionary(of: staged)["CFBundleIdentifier"] as? String == app.bundleID
        if passes && app.signatureValid { passes = signatureIsValid(staged) }
        if passes && app.trusted { passes = gatekeeperNotarizes(staged, control: control) }
        if control.isCancelled { return abandon(.cancelled) }
        guard passes else { return abandon(.failed(.verification)) }
        guard control.commit() else { return abandon(.cancelled) }

        var result = DiskImageInstallResult(outcome: .installed)
        result.stagedSelf = staged
        result.workDirectory = work
        result.ejected = eject(app.mountURL)
        if trashDownload, result.ejected, let id = app.imageID, DiskImageFileID(of: app.imageURL) == id {
            result.downloadTrashed = (try? fm.trashItem(at: app.imageURL, resultingItemURL: nil)) != nil
        }
        return result
    }

    /// Undoes an install: the new copy goes to the Trash, the copies it
    /// replaced come back. Every precondition is checked before anything moves.
    static func undo(installed: URL, bundleID: String?, replaced: [DiskImageTrashedCopy]) -> Bool {
        let fm = FileManager.default
        guard fm.fileExists(atPath: installed.path),
              infoDictionary(of: installed)["CFBundleIdentifier"] as? String == bundleID else { return false }
        for old in replaced {
            guard fm.fileExists(atPath: old.inTrash.path),
                  infoDictionary(of: old.inTrash)["CFBundleIdentifier"] as? String == bundleID,
                  old.original.path == installed.path || !fm.fileExists(atPath: old.original.path)
            else { return false }
        }
        guard (try? fm.trashItem(at: installed, resultingItemURL: nil)) != nil else { return false }
        var allBack = true
        for old in replaced where !renameWithoutOverwriting(old.inTrash, to: old.original) {
            allBack = false
        }
        return allBack
    }

    /// `hdiutil detach`, never forced. One retry: Finder and Spotlight can
    /// hold a volume for a moment after a copy.
    static func eject(_ mountURL: URL) -> Bool {
        for attempt in 0..<2 {
            if attempt > 0 { Thread.sleep(forTimeInterval: 1) }
            if let output = run("/usr/bin/hdiutil", ["detach", mountURL.path]), output.status == 0 { return true }
        }
        return false
    }

    // MARK: Files

    /// A fresh folder on the volume that holds `folder`, so the last step is a
    /// rename.
    private static func makeStagingFolder(beside folder: URL) -> URL? {
        let fm = FileManager.default
        if let temporary = try? fm.url(for: .itemReplacementDirectory, in: .userDomainMask,
                                       appropriateFor: folder, create: true) {
            if sameVolume(temporary, folder) { return temporary }
            rmdir(temporary.path)
        }
        let hidden = folder.appendingPathComponent(".staging-\(UUID().uuidString)", isDirectory: true)
        do {
            try fm.createDirectory(at: hidden, withIntermediateDirectories: false)
            return hidden
        } catch { return nil }
    }

    private static func sameVolume(_ a: URL, _ b: URL) -> Bool {
        let key: Set<URLResourceKey> = [.volumeIdentifierKey]
        guard let x = try? a.resourceValues(forKeys: key).volumeIdentifier as? NSObject,
              let y = try? b.resourceValues(forKeys: key).volumeIdentifier as? NSObject else { return false }
        return x.isEqual(y)
    }

    /// Removes a staging folder. Anything left in it goes to the Trash; an
    /// empty one is removed with `rmdir`, which cannot take data with it.
    private static func discard(_ staging: URL) {
        if rmdir(staging.path) == 0 { return }
        try? FileManager.default.trashItem(at: staging, resultingItemURL: nil)
    }

    /// An atomic rename that fails rather than replace what is at `to`.
    private static func renameWithoutOverwriting(_ from: URL, to: URL) -> Bool {
        let exclusive: UInt32 = 0x4 // RENAME_EXCL
        return renamex_np(from.path, to.path, exclusive) == 0
    }

    private static func putBack(_ trashed: [DiskImageTrashedCopy]) {
        for old in trashed { _ = renameWithoutOverwriting(old.inTrash, to: old.original) }
    }

    // MARK: Processes

    /// Runs a tool to completion. Its pid is registered with `control` so a
    /// cancel can stop it. nil when it could not start or was cancelled first.
    private static func run(_ path: String, _ arguments: [String],
                            control: DiskImageInstallControl? = nil,
                            mergeError: Bool = false) -> Output? {
        if control?.isCancelled == true { return nil }
        let process = Process()
        process.executableURL = URL(fileURLWithPath: path)
        process.arguments = arguments
        let pipe = Pipe()
        process.standardOutput = pipe
        process.standardError = mergeError ? pipe : FileHandle.nullDevice
        process.standardInput = FileHandle.nullDevice
        do { try process.run() } catch { return nil }
        let pid = process.processIdentifier
        control?.track(pid)
        let data = pipe.fileHandleForReading.readDataToEndOfFile()
        process.waitUntilExit()
        control?.untrack(pid)
        return Output(status: process.terminationStatus, data: data)
    }
}
