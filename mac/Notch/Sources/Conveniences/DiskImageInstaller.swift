// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Vorssaint
//
// Pulse: adapted from Vorssaint's DiskImageInstallerService (GPL-3.0-or-later),
// see mac/Notch/FORK.md. The identity checks, collision handling, staged copy,
// Gatekeeper assessment and failure outcomes are Vorssaint's. The prompt is
// replaced: Vorssaint's NSAlert with options becomes cards in the notch
// (`DiskImageCard`), and the options are Pulse preferences instead of
// checkboxes. Installs go to /Applications. Pulse adds the automatic path
// (signed, notarized, not installed, not running), the automatic update of an
// older installed copy (`convDiskImageAutoUpdate`), Undo, and Replace.

import AppKit
import Darwin
import Foundation

/// Installs apps from freshly mounted disk images, then ejects the image and
/// optionally moves the downloaded .dmg to the Trash.
///
/// A signed, notarized app that is not installed and not running goes in
/// without a prompt (when `convDiskImageAuto` is on) and the notch says so,
/// with Undo. With `convDiskImageAutoUpdate` on, a signed, notarized app with
/// the same bundle id as an older, idle installed copy replaces it the same
/// way ("Updated <App> to <version>", with Undo). Everything else is asked in
/// the notch.
@MainActor
final class DiskImageInstaller {
    /// Shows a card on the notch(es), or clears it with nil. Returns whether
    /// any notch could show it (a hidden notch cannot).
    var present: ((DiskImagePrompt?) -> Bool)?

    private let preferences: Preferences
    private var mountObserver: NSObjectProtocol?
    private var unmountObserver: NSObjectProtocol?
    private var pending: [DiskImageFinding] = []
    private var processingMounts = Set<String>()
    /// The finding whose card is up, or being installed.
    private var current: DiskImageFinding?
    private var installing = false
    /// A result card (with or without Undo) is up; the next image waits.
    private var resultShowing = false
    private var lastInstall: InstalledRecord?
    private var dismissTimer: Timer?
    private var hovering = false

    private struct InstalledRecord {
        let destinationURL: URL
        let bundleID: String?
        let name: String
        /// Old copies an automatic update moved to the Trash; empty for a fresh install.
        let replaced: [DiskImageTrashedCopy]
    }

    init(preferences: Preferences) {
        self.preferences = preferences
    }

    /// Follows the preference: watching for mounts only while it is on.
    func sync() {
        preferences.convDiskImageInstaller ? start() : stop()
    }

    func stop() {
        let center = NSWorkspace.shared.notificationCenter
        if let mountObserver { center.removeObserver(mountObserver) }
        if let unmountObserver { center.removeObserver(unmountObserver) }
        mountObserver = nil
        unmountObserver = nil
        pending.removeAll()
        processingMounts.removeAll()
        if !installing {
            finish(presentNext: false)
        }
    }

    private func start() {
        guard mountObserver == nil else { return }
        let center = NSWorkspace.shared.notificationCenter
        mountObserver = center.addObserver(forName: NSWorkspace.didMountNotification,
                                           object: nil, queue: .main) { [weak self] note in
            guard let url = note.userInfo?[NSWorkspace.volumeURLUserInfoKey] as? URL else { return }
            MainActor.assumeIsolated { self?.inspect(mountURL: url) }
        }
        unmountObserver = center.addObserver(forName: NSWorkspace.didUnmountNotification,
                                             object: nil, queue: .main) { [weak self] note in
            guard let url = note.userInfo?[NSWorkspace.volumeURLUserInfoKey] as? URL else { return }
            MainActor.assumeIsolated { self?.unmounted(url) }
        }
    }

    /// The image was ejected by someone else while its offer was showing.
    private func unmounted(_ url: URL) {
        let path = url.standardizedFileURL.path
        pending.removeAll { $0.mountURL.standardizedFileURL.path == path }
        if !installing, !resultShowing, let current, current.mountURL.standardizedFileURL.path == path {
            finish()
        }
    }

    private func inspect(mountURL: URL) {
        guard mountObserver != nil else { return }
        let path = mountURL.standardizedFileURL.resolvingSymlinksInPath().path
        guard processingMounts.insert(path).inserted else { return }
        Task {
            let finding = await Task.detached(priority: .utility) {
                DiskImageInstallWork.finding(mountedAt: mountURL)
            }.value
            processingMounts.remove(path)
            guard mountObserver != nil, let finding else { return }
            pending.append(finding)
            presentNext()
        }
    }

    // MARK: - Choosing what to do

    private func presentNext() {
        guard current == nil, !installing, !resultShowing, mountObserver != nil, !pending.isEmpty else { return }
        let finding = pending.removeFirst()
        current = finding
        if case let .app(candidate) = finding {
            if isAutomatic(candidate) {
                runInstall(candidate, replace: false, automatic: true)
                return
            }
            if isAutomaticUpdate(candidate) {
                runInstall(candidate, replace: true, automatic: true)
                return
            }
        }
        guard show(offer(for: finding)) else {
            // No notch can show the question (hidden): leave the image alone.
            current = nil
            presentNext()
            return
        }
    }

    /// Signed and notarized, not installed, not running, and the person has
    /// not turned the automatic path off.
    private func isAutomatic(_ candidate: DiskImageCandidate) -> Bool {
        preferences.convDiskImageAuto
            && candidate.trusted
            && candidate.bundleID != nil
            && candidate.installedURL == nil
            && runningCopies(of: candidate.bundleID).isEmpty
    }

    /// Opt-in: the same bundle id is installed at a LOWER version, the copy is
    /// not running, the image's app is signed and notarized, and the install
    /// would only ever replace that app (the name in /Applications, if taken,
    /// holds the same bundle id). Anything else is asked in the notch.
    private func isAutomaticUpdate(_ candidate: DiskImageCandidate) -> Bool {
        guard preferences.convDiskImageAutoUpdate,
              candidate.trusted,
              let bundleID = candidate.bundleID,
              candidate.installedURL != nil,
              let installed = candidate.installedVersion,
              candidate.installedBundleID?.caseInsensitiveCompare(bundleID) == .orderedSame,
              installed.isOlder(than: candidate.newVersion),
              runningCopies(of: bundleID).isEmpty
        else { return false }
        let fm = FileManager.default
        guard let destination = DiskImageInstallerSupport.collisionURLs(
            for: candidate.appURL, useUserApplications: false, fileManager: fm)?.first
        else { return false }
        if fm.fileExists(atPath: destination.path),
           Bundle(url: destination)?.bundleIdentifier?.caseInsensitiveCompare(bundleID) != .orderedSame {
            return false
        }
        return true
    }

    private func runningCopies(of bundleID: String?) -> [NSRunningApplication] {
        guard let bundleID else { return [] }
        let me = ProcessInfo.processInfo.processIdentifier
        return NSRunningApplication.runningApplications(withBundleIdentifier: bundleID)
            .filter { $0.processIdentifier != me && !$0.isTerminated }
    }

    // MARK: - The notch's answer

    func choose(_ choice: DiskImageChoice) {
        switch choice {
        case .install, .replace:
            guard !installing, case let .app(candidate) = current else { return }
            runInstall(candidate, replace: choice == .replace, automatic: false)
        case .undo:
            undo()
        case .openInstaller:
            if case let .package(_, packageURL, _) = current { NSWorkspace.shared.open(packageURL) }
            finish()
        case .showImage:
            if let current { NSWorkspace.shared.open(current.mountURL) }
            finish()
        case .dismiss:
            guard !installing else { return }
            finish()
        }
    }

    /// The pointer is on the card (or left it): the result card stays while
    /// it is hovered.
    func hoverChanged(_ on: Bool) {
        hovering = on
        guard resultShowing else { return }
        if on {
            dismissTimer?.invalidate()
            dismissTimer = nil
        } else if dismissTimer == nil {
            armDismiss(after: 3)
        }
    }

    private func finish(presentNext next: Bool = true) {
        dismissTimer?.invalidate()
        dismissTimer = nil
        hovering = false
        current = nil
        installing = false
        resultShowing = false
        lastInstall = nil
        _ = present?(nil)
        if next { presentNext() }
    }

    @discardableResult
    private func show(_ prompt: DiskImagePrompt) -> Bool {
        present?(prompt) ?? false
    }

    private func armDismiss(after seconds: TimeInterval) {
        dismissTimer?.invalidate()
        dismissTimer = nil
        guard !hovering else { return }
        dismissTimer = Timer.scheduledTimer(withTimeInterval: seconds, repeats: false) { [weak self] _ in
            MainActor.assumeIsolated { self?.finish() }
        }
    }

    // MARK: - Install

    private func runInstall(_ candidate: DiskImageCandidate, replace: Bool, automatic: Bool) {
        installing = true
        current = .app(candidate)
        let name = candidate.displayName
        if !automatic {
            show(DiskImagePrompt(iconPath: candidate.appURL.path,
                                 title: L10n.t("Installing \(name)"),
                                 detail: L10n.t("Copying and checking it."),
                                 style: .working))
        }
        let trash = preferences.convDiskImageTrashDownload
        Task {
            // A running copy is asked to quit first; if it will not, nothing is touched.
            let running = runningCopies(of: candidate.bundleID)
            if !running.isEmpty {
                running.forEach { $0.terminate() }
                for _ in 0..<40 where running.contains(where: { !$0.isTerminated }) {
                    try? await Task.sleep(for: .milliseconds(200))
                }
                if running.contains(where: { !$0.isTerminated }) {
                    showProblem(icon: candidate.appURL.path,
                                title: L10n.t("Could not quit \(name)"),
                                detail: L10n.t("Quit it yourself, then install again. Nothing was changed."))
                    return
                }
            }
            let result = await Task.detached(priority: .utility) {
                DiskImageInstallWork.install(candidate, trashingDownload: trash, replacing: replace)
            }.value
            showResult(result, candidate: candidate, replaced: replace, automatic: automatic)
        }
    }

    private func showResult(_ result: DiskImageInstallResult, candidate: DiskImageCandidate,
                            replaced: Bool, automatic: Bool) {
        let name = candidate.displayName
        var detail: String
        var warning = false
        switch result.outcome {
        case let .installed(downloadTrashed):
            detail = downloadTrashed
                ? L10n.t("Disk image ejected · download moved to the Trash")
                : L10n.t("Disk image ejected")
        case .installedKeepingMount:
            warning = true
            detail = L10n.t("The disk image is busy and still mounted.")
        case .installedKeepingDownload:
            warning = true
            detail = L10n.t("Disk image ejected; the download could not be moved to the Trash.")
        case let .failed(failure):
            switch failure {
            case .alreadyInstalled: detail = L10n.t("\(name) is already in Applications, so nothing was copied.")
            case .verification: detail = L10n.t("The app did not pass the signature check, so it was not installed.")
            case .copy: detail = L10n.t("Copying it failed. Nothing was installed.")
            }
            showProblem(icon: candidate.appURL.path,
                        title: L10n.t("Could not install \(name)"), detail: detail)
            return
        }
        let destination = result.destinationURL
        let updated = replaced && automatic
        // A fresh install and an automatic update can be undone; a manual Replace stands.
        let undoable = updated ? !result.replaced.isEmpty : !replaced
        if undoable, let destination {
            lastInstall = InstalledRecord(destinationURL: destination, bundleID: candidate.bundleID,
                                          name: name, replaced: updated ? result.replaced : [])
        }
        installing = false
        resultShowing = true
        let title: String
        if updated {
            let version = candidate.newVersion.display ?? ""
            title = version.isEmpty ? L10n.t("Updated \(name)") : L10n.t("Updated \(name) to \(version)")
        } else {
            title = replaced ? L10n.t("Replaced \(name)") : L10n.t("Installed \(name)")
        }
        let prompt = DiskImagePrompt(
            iconPath: (destination ?? candidate.appURL).path,
            title: title,
            detail: detail,
            style: warning ? .problem : .done,
            primary: undoable ? .init(choice: .undo, label: L10n.t("Undo")) : nil)
        if !show(prompt) {
            // No notch to say it on; the install itself stands.
            finish()
            return
        }
        armDismiss(after: warning ? 15 : 8)
    }

    private func showProblem(icon: String, title: String, detail: String) {
        installing = false
        resultShowing = true
        if !show(DiskImagePrompt(iconPath: icon, title: title, detail: detail, style: .problem)) {
            finish()
            return
        }
        armDismiss(after: 15)
    }

    /// Moves what was just installed to the Trash. Never deletes.
    private func undo() {
        guard let record = lastInstall else { finish(); return }
        lastInstall = nil
        if !record.replaced.isEmpty {
            undoUpdate(record)
            return
        }
        let fm = FileManager.default
        let url = record.destinationURL
        let sameApp = fm.fileExists(atPath: url.path)
            && (record.bundleID == nil || Bundle(url: url)?.bundleIdentifier == record.bundleID)
        if sameApp, (try? fm.trashItem(at: url, resultingItemURL: nil)) != nil {
            resultShowing = true
            show(DiskImagePrompt(iconPath: url.path,
                                 title: L10n.t("Moved \(record.name) to the Trash"),
                                 detail: L10n.t("It is no longer in Applications."), style: .done))
            armDismiss(after: 3)
        } else {
            showProblem(icon: url.path, title: L10n.t("Could not undo"),
                        detail: L10n.t("\(record.name) is no longer where it was installed, or could not be moved."))
        }
    }

    /// Moves the updated copy to the Trash and puts the old copy back from the
    /// Trash. Both bundle ids are re-checked, and nothing moves unless every
    /// check passes first.
    private func undoUpdate(_ record: InstalledRecord) {
        let fm = FileManager.default
        let url = record.destinationURL
        func hasBundleID(_ app: URL) -> Bool {
            guard let bundleID = record.bundleID else { return false }
            return Bundle(url: app)?.bundleIdentifier?.caseInsensitiveCompare(bundleID) == .orderedSame
        }
        let newIsThere = fm.fileExists(atPath: url.path) && hasBundleID(url)
        let oldReady = !record.replaced.isEmpty && record.replaced.allSatisfy { copy in
            fm.fileExists(atPath: copy.inTrash.path) && hasBundleID(copy.inTrash)
                && (copy.original.standardizedFileURL.path == url.path || !fm.fileExists(atPath: copy.original.path))
        }
        guard newIsThere, oldReady else {
            showProblem(icon: url.path, title: L10n.t("Could not undo"),
                        detail: L10n.t("\(record.name) is no longer the updated copy, or the old copy is no longer in the Trash."))
            return
        }
        do {
            try fm.trashItem(at: url, resultingItemURL: nil)
            for copy in record.replaced.reversed() {
                try fm.moveItem(at: copy.inTrash, to: copy.original)
            }
        } catch {
            showProblem(icon: url.path, title: L10n.t("Could not undo"),
                        detail: L10n.t("\(record.name) could not be put back. Check the Trash and Applications."))
            return
        }
        resultShowing = true
        show(DiskImagePrompt(iconPath: (record.replaced.first?.original ?? url).path,
                             title: L10n.t("Restored \(record.name)"),
                             detail: L10n.t("The updated copy is in the Trash."), style: .done))
        armDismiss(after: 3)
    }

    // MARK: - Offers

    private func offer(for finding: DiskImageFinding) -> DiskImagePrompt {
        switch finding {
        case let .app(c):
            return offer(for: c)
        case let .several(mountURL, volumeName, summary):
            return DiskImagePrompt(
                iconPath: mountURL.path,
                title: L10n.t("\(volumeName) has several items"),
                detail: summary,
                style: .ask,
                primary: .init(choice: .showImage, label: L10n.t("Show in Finder")),
                secondary: .init(choice: .dismiss, label: L10n.t("Not now")))
        case let .package(_, packageURL, volumeName):
            return DiskImagePrompt(
                iconPath: packageURL.path,
                title: L10n.t("\(volumeName) holds an installer"),
                detail: packageURL.lastPathComponent,
                style: .ask,
                primary: .init(choice: .openInstaller, label: L10n.t("Open installer")),
                secondary: .init(choice: .dismiss, label: L10n.t("Not now")))
        }
    }

    private func offer(for c: DiskImageCandidate) -> DiskImagePrompt {
        let name = c.displayName
        let installed = c.installedURL != nil
        let running = !runningCopies(of: c.bundleID).isEmpty
        var lines: [String] = []
        var warning: String?
        var title = L10n.t("Install \(name) & eject")
        var label = L10n.t("Install & eject")

        if installed {
            let old = c.installedVersion?.display ?? L10n.t("unknown version")
            let new = c.newVersion.display ?? L10n.t("unknown version")
            title = L10n.t("\(name) is already installed")
            lines.append(L10n.t("Installed \(old) · this image has \(new)"))
            warning = running
                ? L10n.t("Replacing quits \(name) (unsaved work in it may be lost), moves the old copy to the Trash, then installs this one.")
                : L10n.t("Replacing moves the old copy to the Trash, then installs this one.")
            label = c.trusted ? L10n.t("Replace") : L10n.t("Replace anyway")
        } else if running {
            title = L10n.t("\(name) is running")
            warning = L10n.t("It will be quit first (unsaved work in it may be lost).")
            label = c.trusted ? L10n.t("Quit & install") : L10n.t("Quit & install anyway")
        } else if !c.trusted {
            title = L10n.t("Install \(name)?")
            label = L10n.t("Install anyway")
        } else {
            lines.append(L10n.t("Copies it to Applications, then ejects the disk image."))
        }
        if !c.trusted {
            lines.append(L10n.t("It is not signed and notarized, so macOS cannot vouch for it."))
        }
        return DiskImagePrompt(
            iconPath: c.appURL.path,
            title: title,
            detail: lines.joined(separator: " "),
            warning: warning,
            style: .ask,
            primary: .init(choice: installed ? .replace : .install, label: label),
            secondary: .init(choice: .dismiss, label: L10n.t("Not now")))
    }
}

// MARK: - Work off the main actor (Vorssaint's checks and install)

struct DiskImageCandidate: Sendable {
    let mountURL: URL
    let appURL: URL
    let imageURL: URL
    let imageIdentity: DiskImageFileIdentity
    let displayName: String
    let bundleID: String?
    let newVersion: DiskImageAppVersion
    /// The copy already in /Applications (same file name or same bundle id).
    let installedURL: URL?
    let installedVersion: DiskImageAppVersion?
    let installedBundleID: String?
    /// Passes the signature check and Gatekeeper as a notarized Developer ID app.
    let trusted: Bool
}

/// An app's CFBundleShortVersionString and CFBundleVersion.
struct DiskImageAppVersion: Sendable {
    let short: String?
    let build: String?

    /// What the notch shows: the marketing version, else the build.
    var display: String? { short ?? build }

    /// Whether this version is older than `other`. The short versions decide
    /// when both are numeric and differ; otherwise the build versions do. False
    /// when neither can be compared, so an unknown version is never replaced.
    func isOlder(than other: DiskImageAppVersion) -> Bool {
        if let a = short, let b = other.short, let order = Self.numericOrder(a, b), order != 0 {
            return order < 0
        }
        if let a = build, let b = other.build, let order = Self.numericOrder(a, b) {
            return order < 0
        }
        return false
    }

    /// -1, 0 or 1 for dotted numeric versions (components compared as
    /// numbers, so 1.10 is newer than 1.9); nil if a component is not a number.
    static func numericOrder(_ a: String, _ b: String) -> Int? {
        func parts(_ text: String) -> [Int]? {
            var numbers: [Int] = []
            for piece in text.split(separator: ".", omittingEmptySubsequences: false) {
                guard !piece.isEmpty, piece.allSatisfy({ $0.isASCII && $0.isNumber }),
                      let number = Int(piece)
                else { return nil }
                numbers.append(number)
            }
            return numbers
        }
        guard let x = parts(a), let y = parts(b) else { return nil }
        for index in 0..<max(x.count, y.count) {
            let left = index < x.count ? x[index] : 0
            let right = index < y.count ? y[index] : 0
            if left != right { return left < right ? -1 : 1 }
        }
        return 0
    }
}

/// An old copy a replace moved to the Trash, and where it is now.
struct DiskImageTrashedCopy: Sendable {
    let original: URL
    let inTrash: URL
}

enum DiskImageFinding: Sendable {
    case app(DiskImageCandidate)
    case several(mountURL: URL, volumeName: String, summary: String)
    case package(mountURL: URL, packageURL: URL, volumeName: String)

    var mountURL: URL {
        switch self {
        case let .app(c): return c.mountURL
        case let .several(url, _, _): return url
        case let .package(url, _, _): return url
        }
    }
}

struct DiskImageFileIdentity: Equatable, Sendable {
    let device: UInt64
    let inode: UInt64
}

enum DiskImageInstallFailure: Sendable { case alreadyInstalled, verification, copy }

enum DiskImageInstallOutcome: Sendable {
    case installed(downloadTrashed: Bool)
    case installedKeepingMount
    case installedKeepingDownload
    case failed(DiskImageInstallFailure)
}

struct DiskImageInstallResult: Sendable {
    let outcome: DiskImageInstallOutcome
    let destinationURL: URL?
    /// Old copies moved to the Trash by a replace (for Undo of an update).
    var replaced: [DiskImageTrashedCopy] = []
}

enum DiskImageInstallWork {
    private struct CommandResult {
        let status: Int32
        let output: Data
    }

    /// What a mounted disk image holds: exactly one real app (with how it
    /// stands against /Applications), several items, or an installer package.
    /// Volumes that are not disk images are ignored.
    static func finding(mountedAt mountURL: URL) -> DiskImageFinding? {
        let fm = FileManager.default
        let info = run("/usr/bin/hdiutil", ["info", "-plist"])
        guard info.status == 0,
              let imageURL = DiskImageInstallerSupport.imageURL(mountedAt: mountURL, hdiutilInfo: info.output),
              let imageIdentity = fileIdentity(at: imageURL),
              let entries = try? fm.contentsOfDirectory(
                at: mountURL,
                includingPropertiesForKeys: [.isDirectoryKey, .isSymbolicLinkKey],
                options: [.skipsHiddenFiles])
        else { return nil }

        let apps = entries.filter { url in
            guard url.pathExtension.caseInsensitiveCompare("app") == .orderedSame,
                  let values = try? url.resourceValues(forKeys: [.isDirectoryKey, .isSymbolicLinkKey]),
                  values.isDirectory == true, values.isSymbolicLink != true
            else { return false }
            return validBundle(at: url)
        }
        let packages = entries.filter { url in
            let ext = url.pathExtension.lowercased()
            guard ext == "pkg" || ext == "mpkg",
                  let values = try? url.resourceValues(forKeys: [.isSymbolicLinkKey]),
                  values.isSymbolicLink != true
            else { return false }
            return true
        }
        let volumeName = mountURL.lastPathComponent

        if apps.count > 1 {
            return .several(mountURL: mountURL, volumeName: volumeName,
                            summary: "\(apps.count) apps. Pulse installs one at a time; open the image to choose.")
        }
        if apps.isEmpty {
            if packages.count == 1, let packageURL = packages.first {
                return .package(mountURL: mountURL, packageURL: packageURL, volumeName: volumeName)
            }
            if packages.count > 1 {
                return .several(mountURL: mountURL, volumeName: volumeName,
                                summary: "\(packages.count) installers; open the image to choose.")
            }
            return nil
        }

        guard let appURL = apps.first,
              let collisionURLs = DiskImageInstallerSupport.collisionURLs(
                for: appURL, useUserApplications: false, fileManager: fm)
        else { return nil }

        let bundle = Bundle(url: appURL)
        let bundleID = bundle?.bundleIdentifier
        let preferred = bundle?.object(forInfoDictionaryKey: "CFBundleDisplayName") as? String
        let installedURL = installedCopy(bundleID: bundleID, collisionURLs: collisionURLs, fileManager: fm)
        return DiskImageCandidate(
            mountURL: mountURL, appURL: appURL, imageURL: imageURL, imageIdentity: imageIdentity,
            displayName: DiskImageInstallerSupport.displayName(preferred: preferred, appURL: appURL),
            bundleID: bundleID,
            newVersion: appVersion(of: appURL),
            installedURL: installedURL,
            installedVersion: installedURL.map { appVersion(of: $0) },
            installedBundleID: installedURL.flatMap { Bundle(url: $0)?.bundleIdentifier },
            trusted: notarizedAndSigned(appURL)).asFinding
    }

    /// The installed copy: the same file name in Applications, else any app
    /// there (one folder deep, e.g. Utilities) with the same bundle id.
    private static func installedCopy(bundleID: String?, collisionURLs: [URL],
                                      fileManager fm: FileManager) -> URL? {
        if let hit = collisionURLs.first(where: { fm.fileExists(atPath: $0.path) }) { return hit }
        guard let bundleID, let root = fm.urls(for: .applicationDirectory, in: .localDomainMask).first
        else { return nil }
        func matches(_ url: URL) -> Bool {
            url.pathExtension.caseInsensitiveCompare("app") == .orderedSame
                && Bundle(url: url)?.bundleIdentifier?.caseInsensitiveCompare(bundleID) == .orderedSame
        }
        let top = (try? fm.contentsOfDirectory(at: root, includingPropertiesForKeys: [.isDirectoryKey],
                                               options: [.skipsHiddenFiles])) ?? []
        if let hit = top.first(where: matches) { return hit }
        for folder in top where folder.pathExtension.isEmpty {
            let inner = (try? fm.contentsOfDirectory(at: folder, includingPropertiesForKeys: nil,
                                                     options: [.skipsHiddenFiles])) ?? []
            if let hit = inner.first(where: matches) { return hit }
        }
        return nil
    }

    private static func appVersion(of appURL: URL) -> DiskImageAppVersion {
        let info = Bundle(url: appURL)?.infoDictionary
        func text(_ key: String) -> String? {
            let value = (info?[key] as? String)?.trimmingCharacters(in: .whitespacesAndNewlines)
            return value?.isEmpty == false ? value : nil
        }
        return DiskImageAppVersion(short: text("CFBundleShortVersionString"), build: text("CFBundleVersion"))
    }

    /// Copies the app to /Applications, eject the image, optionally trash the
    /// .dmg. With `replacing`, an existing copy is moved to the Trash first
    /// (after the new copy is staged and checked; put back if the move fails).
    static func install(_ candidate: DiskImageCandidate, trashingDownload: Bool,
                        replacing: Bool = false) -> DiskImageInstallResult {
        let fm = FileManager.default
        let domain = DiskImageInstallerSupport.applicationsDomain(useUserApplications: false)
        guard let applicationsURL = try? fm.url(for: .applicationDirectory, in: domain,
                                                appropriateFor: nil, create: true),
              let destinationURL = DiskImageInstallerSupport.destinationURL(
                for: candidate.appURL, applicationsURL: applicationsURL),
              let collisionURLs = DiskImageInstallerSupport.collisionURLs(
                for: candidate.appURL, useUserApplications: false, fileManager: fm),
              collisionURLs.contains(destinationURL)
        else { return DiskImageInstallResult(outcome: .failed(.copy), destinationURL: nil) }
        guard replacing || collisionURLs.allSatisfy({ !fm.fileExists(atPath: $0.path) }) else {
            return DiskImageInstallResult(outcome: .failed(.alreadyInstalled), destinationURL: destinationURL)
        }

        let stagingDirectory: URL
        do {
            stagingDirectory = try fm.url(for: .itemReplacementDirectory, in: .userDomainMask,
                                          appropriateFor: destinationURL.deletingLastPathComponent(),
                                          create: true)
        } catch {
            return DiskImageInstallResult(outcome: .failed(.copy), destinationURL: destinationURL)
        }
        defer { try? fm.removeItem(at: stagingDirectory) }

        let stagedApp = stagingDirectory.appendingPathComponent(candidate.appURL.lastPathComponent,
                                                                 isDirectory: true)
        // Carrying the mounted image's quarantine over leaves the installed app eligible for
        // path randomization, so macOS runs it from a read-only random location instead of
        // Applications. The checks below are the same assessment that flag defers to.
        // An app that does not pass them (installed on the person's say-so) keeps whatever
        // quarantine it has, so macOS still asks the first time it opens.
        var flags = ["--rsrc", "--extattr", "--acl"]
        if candidate.trusted { flags.append("--noqtn") }
        let copy = run("/usr/bin/ditto", flags + [candidate.appURL.path, stagedApp.path])
        guard copy.status == 0, validBundle(at: stagedApp) else {
            return DiskImageInstallResult(outcome: .failed(.copy), destinationURL: destinationURL)
        }
        if candidate.trusted {
            guard gatekeeperAccepts(stagedApp) else {
                return DiskImageInstallResult(outcome: .failed(.verification), destinationURL: destinationURL)
            }
        }

        var trashedOld: [DiskImageTrashedCopy] = []
        do {
            guard let finalCollisionURLs = DiskImageInstallerSupport.collisionURLs(
                for: candidate.appURL, useUserApplications: false, fileManager: fm),
                finalCollisionURLs.contains(destinationURL)
            else { return DiskImageInstallResult(outcome: .failed(.copy), destinationURL: destinationURL) }
            if replacing {
                var olds = finalCollisionURLs.filter { fm.fileExists(atPath: $0.path) }
                if let installed = candidate.installedURL, fm.fileExists(atPath: installed.path),
                   !olds.contains(installed) { olds.append(installed) }
                for old in olds {
                    var resulting: NSURL?
                    try fm.trashItem(at: old, resultingItemURL: &resulting)
                    if let resulting {
                        trashedOld.append(DiskImageTrashedCopy(original: old, inTrash: resulting as URL))
                    }
                }
            } else {
                guard finalCollisionURLs.allSatisfy({ !fm.fileExists(atPath: $0.path) }) else {
                    return DiskImageInstallResult(outcome: .failed(.alreadyInstalled), destinationURL: destinationURL)
                }
            }
            try fm.moveItem(at: stagedApp, to: destinationURL)
        } catch {
            for entry in trashedOld.reversed() where !fm.fileExists(atPath: entry.original.path) {
                try? fm.moveItem(at: entry.inTrash, to: entry.original)
            }
            return DiskImageInstallResult(outcome: .failed(.copy), destinationURL: destinationURL)
        }

        do {
            try NSWorkspace.shared.unmountAndEjectDevice(at: candidate.mountURL)
        } catch {
            return DiskImageInstallResult(outcome: .installedKeepingMount, destinationURL: destinationURL,
                                          replaced: trashedOld)
        }

        guard trashingDownload else {
            return DiskImageInstallResult(outcome: .installed(downloadTrashed: false), destinationURL: destinationURL,
                                          replaced: trashedOld)
        }
        guard fileIdentity(at: candidate.imageURL) == candidate.imageIdentity else {
            return DiskImageInstallResult(outcome: .installedKeepingDownload, destinationURL: destinationURL,
                                          replaced: trashedOld)
        }
        do {
            try fm.trashItem(at: candidate.imageURL, resultingItemURL: nil)
            return DiskImageInstallResult(outcome: .installed(downloadTrashed: true), destinationURL: destinationURL,
                                          replaced: trashedOld)
        } catch {
            return DiskImageInstallResult(outcome: .installedKeepingDownload, destinationURL: destinationURL,
                                          replaced: trashedOld)
        }
    }

    private static func validBundle(at appURL: URL) -> Bool {
        guard let bundle = Bundle(url: appURL),
              let executableURL = bundle.executableURL,
              FileManager.default.isExecutableFile(atPath: executableURL.path)
        else { return false }
        let root = appURL.standardizedFileURL.resolvingSymlinksInPath().path + "/"
        let executable = executableURL.standardizedFileURL.resolvingSymlinksInPath().path
        return executable.hasPrefix(root)
    }

    private static func gatekeeperAccepts(_ appURL: URL) -> Bool {
        let signature = run("/usr/bin/codesign", ["--verify", "--deep", "--strict", appURL.path])
        guard signature.status == 0 else { return false }
        let status = run("/usr/sbin/spctl", ["--status"])
        if String(data: status.output, encoding: .utf8)?.localizedCaseInsensitiveContains("disabled") == true {
            return true
        }
        return run("/usr/sbin/spctl", ["-a", "-t", "exec", appURL.path]).status == 0
    }

    /// Stricter than `gatekeeperAccepts`, which is Vorssaint's and passes
    /// anything when Gatekeeper is off: the signature verifies and Gatekeeper
    /// itself names the source "Notarized Developer ID". This is what lets an
    /// app install without a question.
    private static func notarizedAndSigned(_ appURL: URL) -> Bool {
        guard run("/usr/bin/codesign", ["--verify", "--deep", "--strict", appURL.path]).status == 0 else {
            return false
        }
        let assessment = run("/usr/sbin/spctl", ["-a", "-vv", "-t", "exec", appURL.path], mergingErrors: true)
        guard assessment.status == 0 else { return false }
        return String(data: assessment.output, encoding: .utf8)?
            .localizedCaseInsensitiveContains("Notarized Developer ID") == true
    }

    private static func fileIdentity(at url: URL) -> DiskImageFileIdentity? {
        var info = stat()
        guard url.path.withCString({ lstat($0, &info) }) == 0,
              (info.st_mode & S_IFMT) == S_IFREG
        else { return nil }
        return DiskImageFileIdentity(device: UInt64(info.st_dev), inode: UInt64(info.st_ino))
    }

    private static func run(_ executable: String, _ arguments: [String],
                            mergingErrors: Bool = false) -> CommandResult {
        let process = Process()
        let output = Pipe()
        process.executableURL = URL(fileURLWithPath: executable)
        process.arguments = arguments
        process.standardOutput = output
        process.standardError = mergingErrors ? output : FileHandle.nullDevice
        do {
            try process.run()
        } catch {
            return CommandResult(status: -1, output: Data())
        }
        let data = output.fileHandleForReading.readDataToEndOfFile()
        process.waitUntilExit()
        return CommandResult(status: process.terminationStatus, output: data)
    }
}

private extension DiskImageCandidate {
    var asFinding: DiskImageFinding { .app(self) }
}
