// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Vorssaint
//
// Pulse: adapted from Vorssaint's DiskImageInstallerService (GPL-3.0-or-later),
// see mac/Notch/FORK.md. The identity checks, collision handling, staged copy,
// Gatekeeper assessment and failure outcomes are Vorssaint's. The prompt is
// replaced: Vorssaint's NSAlert with options becomes cards in the notch
// (`DiskImageCard`), and the options are Pulse preferences instead of
// checkboxes. Installs go to /Applications. Pulse adds the automatic path
// (signed, notarized, not installed, not running), Undo, and Replace.

import AppKit
import Darwin
import Foundation

/// Installs apps from freshly mounted disk images, then ejects the image and
/// optionally moves the downloaded .dmg to the Trash.
///
/// A signed, notarized app that is not installed and not running goes in
/// without a prompt (when `convDiskImageAuto` is on) and the notch says so,
/// with Undo. Everything else is asked in the notch.
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
        if case let .app(candidate) = finding, isAutomatic(candidate) {
            runInstall(candidate, replace: false, automatic: true)
            return
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
            showResult(result, candidate: candidate, replaced: replace)
        }
    }

    private func showResult(_ result: DiskImageInstallResult, candidate: DiskImageCandidate, replaced: Bool) {
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
        if !replaced, let destination {
            lastInstall = InstalledRecord(destinationURL: destination, bundleID: candidate.bundleID, name: name)
        }
        installing = false
        resultShowing = true
        let prompt = DiskImagePrompt(
            iconPath: (destination ?? candidate.appURL).path,
            title: replaced ? L10n.t("Replaced \(name)") : L10n.t("Installed \(name)"),
            detail: detail,
            style: warning ? .problem : .done,
            primary: replaced ? nil : .init(choice: .undo, label: L10n.t("Undo")))
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
            let old = c.installedVersion ?? L10n.t("unknown version")
            let new = c.newVersion ?? L10n.t("unknown version")
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
    let newVersion: String?
    /// The copy already in /Applications (same file name or same bundle id).
    let installedURL: URL?
    let installedVersion: String?
    /// Passes the signature check and Gatekeeper as a notarized Developer ID app.
    let trusted: Bool
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
            newVersion: version(of: appURL),
            installedURL: installedURL,
            installedVersion: installedURL.flatMap { version(of: $0) },
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

    private static func version(of appURL: URL) -> String? {
        let info = Bundle(url: appURL)?.infoDictionary
        let short = info?["CFBundleShortVersionString"] as? String
        let build = info?["CFBundleVersion"] as? String
        return [short, build].compactMap { $0 }.first { !$0.isEmpty }
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

        var trashedOld: [(original: URL, inTrash: URL)] = []
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
                    if let resulting { trashedOld.append((old, resulting as URL)) }
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
            return DiskImageInstallResult(outcome: .installedKeepingMount, destinationURL: destinationURL)
        }

        guard trashingDownload else {
            return DiskImageInstallResult(outcome: .installed(downloadTrashed: false), destinationURL: destinationURL)
        }
        guard fileIdentity(at: candidate.imageURL) == candidate.imageIdentity else {
            return DiskImageInstallResult(outcome: .installedKeepingDownload, destinationURL: destinationURL)
        }
        do {
            try fm.trashItem(at: candidate.imageURL, resultingItemURL: nil)
            return DiskImageInstallResult(outcome: .installed(downloadTrashed: true), destinationURL: destinationURL)
        } catch {
            return DiskImageInstallResult(outcome: .installedKeepingDownload, destinationURL: destinationURL)
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
