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
    /// The running install's cancellation, and its token (so a late commit
    /// from an earlier install is ignored).
    private var installControl: DiskImageInstallControl?
    private var installToken: UUID?
    /// The working card offers Cancel (manual installs only; automatic ones show no card).
    private var cancelOffered = false
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
        /// Where the old copy was running from when the update quit it; Undo reopens these.
        let relaunch: [URL]
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
                runInstall(candidate, replace: false, automatic: true, updating: false)
                return
            }
            if isAutomaticUpdate(candidate) {
                // A running old copy is asked to quit, then reopened after the update.
                runInstall(candidate, replace: true, automatic: true,
                           updating: !runningCopies(of: candidate.bundleID).isEmpty)
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

    /// Opt-in: the same bundle id is installed at a LOWER version, the image's
    /// app is signed and notarized, and the install would only ever replace
    /// that app (the name in /Applications, if taken, holds the same bundle id).
    /// A running old copy is quit gracefully first (`runInstall`). Anything
    /// else is asked in the notch.
    private func isAutomaticUpdate(_ candidate: DiskImageCandidate) -> Bool {
        guard preferences.convDiskImageAutoUpdate,
              candidate.trusted,
              let bundleID = candidate.bundleID,
              candidate.installedURL != nil,
              let installed = candidate.installedVersion,
              candidate.installedBundleID?.caseInsensitiveCompare(bundleID) == .orderedSame,
              installed.isOlder(than: candidate.newVersion)
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
            runInstall(candidate, replace: choice == .replace, automatic: false, updating: false)
        case .quitAndUpdate:
            guard !installing, case let .app(candidate) = current else { return }
            runInstall(candidate, replace: true, automatic: false, updating: true)
        case .undo:
            undo()
        case .openInstaller:
            if case let .package(_, packageURL, _) = current { NSWorkspace.shared.open(packageURL) }
            finish()
        case .showImage:
            if let current { NSWorkspace.shared.open(current.mountURL) }
            finish()
        case .cancel:
            cancelInstall()
        case .dismiss:
            // While an install is working, the close is Cancel.
            if installing {
                cancelInstall()
                return
            }
            finish()
        }
    }

    /// Stops the install that the working card is showing. Refused (nothing
    /// changes) once the final move has begun.
    private func cancelInstall() {
        guard installing, cancelOffered, let control = installControl,
              case let .app(candidate)? = current, control.requestCancel()
        else { return }
        cancelOffered = false
        show(workingPrompt(candidate, detail: L10n.t("Stopping and cleaning up."), cancellable: false))
    }

    /// The final move has begun: the working card drops Cancel.
    private func installCommitted(_ token: UUID) {
        guard installing, installToken == token, cancelOffered,
              case let .app(candidate)? = current
        else { return }
        cancelOffered = false
        show(workingPrompt(candidate, detail: L10n.t("Copying and checking it."), cancellable: false))
    }

    private func workingPrompt(_ candidate: DiskImageCandidate, detail: String,
                               cancellable: Bool) -> DiskImagePrompt {
        DiskImagePrompt(iconPath: candidate.appURL.path,
                        title: L10n.t("Installing \(candidate.displayName)"),
                        detail: detail,
                        style: .working,
                        primary: cancellable ? .init(choice: .cancel, label: L10n.t("Cancel")) : nil)
    }

    private func endInstall() {
        installing = false
        installControl = nil
        installToken = nil
        cancelOffered = false
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
        endInstall()
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

    /// Installs `candidate`. `updating` means an installed older copy is
    /// running: it is asked to quit gracefully, and after a successful update
    /// the new copy is reopened (without taking focus).
    private func runInstall(_ candidate: DiskImageCandidate, replace: Bool, automatic: Bool, updating: Bool) {
        installing = true
        current = .app(candidate)
        let name = candidate.displayName
        let token = UUID()
        installToken = token
        // Every install shows the working card with Cancel, automatic ones included.
        cancelOffered = true
        let control = DiskImageInstallControl(onCommit: { [weak self] in
            Task { @MainActor in self?.installCommitted(token) }
        })
        installControl = control
        if cancelOffered {
            show(workingPrompt(candidate, detail: L10n.t("Copying and checking it."), cancellable: true))
        }
        let trash = preferences.convDiskImageTrashDownload
        Task {
            // A running copy is asked to quit first; if it will not, nothing is touched.
            let running = runningCopies(of: candidate.bundleID)
            let runningURLs = running.map { $0.bundleURL }
            if !running.isEmpty {
                if cancelOffered {
                    show(workingPrompt(candidate, detail: L10n.t("Asking \(name) to quit."), cancellable: true))
                }
                running.forEach { $0.terminate() }
                let quit = await awaitQuit(running, steps: updating ? 50 : 40, control: control)
                if control.isCancelled {
                    // Cancelled while quitting: whatever quit is reopened, and nothing is copied or moved.
                    await reopenQuit(running, urls: runningURLs)
                    showCancelled(candidate)
                    return
                }
                if !quit {
                    if automatic {
                        showQuitAndUpdateOffer(candidate)
                    } else {
                        showProblem(icon: candidate.appURL.path,
                                    title: L10n.t("Could not quit \(name)"),
                                    detail: L10n.t("Quit it yourself, then install again. Nothing was changed."))
                    }
                    return
                }
                if cancelOffered {
                    show(workingPrompt(candidate, detail: L10n.t("Copying and checking it."), cancellable: true))
                }
            }
            let quitURLs = Array(Set(runningURLs.compactMap { $0 }))
            let result = await Task.detached(priority: .utility) {
                DiskImageInstallWork.install(candidate, trashingDownload: trash, replacing: replace,
                                             control: control)
            }.value
            if case let .cancelled(restored) = result.outcome {
                // Old copies were quit for this update; put back the ones that quit.
                if restored { await reopenQuit(running, urls: runningURLs) }
                showCancelled(candidate, restored: restored)
                return
            }
            var reopened = false
            if updating, !running.isEmpty, result.outcome.installedApp, let destination = result.destinationURL {
                reopened = await reopen(destination)
            }
            showResult(result, candidate: candidate, replaced: replace, automatic: automatic,
                       updating: updating, reopened: reopened, relaunch: quitURLs)
        }
    }

    /// Waits up to `steps` x 200 ms for every instance to exit; stops early on cancel.
    private func awaitQuit(_ running: [NSRunningApplication], steps: Int,
                           control: DiskImageInstallControl? = nil) async -> Bool {
        var waited = 0
        while waited < steps, control?.isCancelled != true, running.contains(where: { !$0.isTerminated }) {
            try? await Task.sleep(for: .milliseconds(200))
            waited += 1
        }
        return !running.contains(where: { !$0.isTerminated })
    }

    /// Reopens the copies that quit (never one that is still running).
    private func reopenQuit(_ running: [NSRunningApplication], urls: [URL?]) async {
        var done = Set<URL>()
        for (app, url) in zip(running, urls) where app.isTerminated {
            guard let url, done.insert(url).inserted else { continue }
            _ = await reopen(url)
        }
    }

    /// Opens an app without taking focus.
    private func reopen(_ url: URL) async -> Bool {
        let configuration = NSWorkspace.OpenConfiguration()
        configuration.activates = false
        return (try? await NSWorkspace.shared.openApplication(at: url, configuration: configuration)) != nil
    }

    /// The old copy did not quit in time: the question is asked in the notch.
    private func showQuitAndUpdateOffer(_ candidate: DiskImageCandidate) {
        endInstall()
        resultShowing = false
        let name = candidate.displayName
        let shown = show(DiskImagePrompt(
            iconPath: candidate.appURL.path,
            title: L10n.t("\(name) is still running"),
            detail: L10n.t("It did not quit, so the update waits. Nothing was changed."),
            warning: L10n.t("Quit & update quits it (unsaved work in it may be lost), moves the old copy to the Trash, then installs and reopens this one."),
            style: .ask,
            primary: .init(choice: .quitAndUpdate, label: L10n.t("Quit & update")),
            secondary: .init(choice: .dismiss, label: L10n.t("Not now"))))
        if !shown { finish() }
    }

    private func showResult(_ result: DiskImageInstallResult, candidate: DiskImageCandidate,
                            replaced: Bool, automatic: Bool, updating: Bool, reopened: Bool,
                            relaunch: [URL]) {
        let name = candidate.displayName
        var detail: String
        var warning = false
        switch result.outcome {
        case let .cancelled(restored):
            showCancelled(candidate, restored: restored)
            return
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
        let updated = replaced && (automatic || updating)
        // A fresh install and an update can be undone; a manual Replace stands.
        let undoable = updated ? !result.replaced.isEmpty : !replaced
        if undoable, let destination {
            lastInstall = InstalledRecord(destinationURL: destination, bundleID: candidate.bundleID,
                                          name: name, replaced: updated ? result.replaced : [],
                                          relaunch: updated ? relaunch : [])
        }
        endInstall()
        resultShowing = true
        let title: String
        if updated {
            let version = candidate.newVersion.display ?? ""
            let base = version.isEmpty ? L10n.t("Updated \(name)") : L10n.t("Updated \(name) to \(version)")
            title = reopened ? L10n.t("\(base) · reopened") : base
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
        endInstall()
        resultShowing = true
        if !show(DiskImagePrompt(iconPath: icon, title: title, detail: detail, style: .problem)) {
            finish()
            return
        }
        armDismiss(after: 15)
    }

    /// A cancelled install: a short card that goes by itself. `restored` is
    /// false when an old copy could not be put back from the Trash.
    private func showCancelled(_ candidate: DiskImageCandidate, restored: Bool = true) {
        let name = candidate.displayName
        guard restored else {
            showProblem(icon: candidate.appURL.path,
                        title: L10n.t("Could not put back \(name)"),
                        detail: L10n.t("The old copy is still in the Trash."))
            return
        }
        endInstall()
        resultShowing = true
        guard show(DiskImagePrompt(iconPath: candidate.appURL.path,
                                   title: L10n.t("Cancelled"),
                                   detail: L10n.t("Nothing was installed. \(name) is unchanged in Applications."),
                                   style: .done))
        else {
            finish()
            return
        }
        armDismiss(after: 3)
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
        // The updated copy may have been reopened: it is quit gracefully first, and
        // Undo stays in progress (the card cannot be dismissed) until it is done.
        dismissTimer?.invalidate()
        dismissTimer = nil
        installing = true
        Task {
            let running = runningCopies(of: record.bundleID)
            running.forEach { $0.terminate() }
            guard await awaitQuit(running, steps: 50) else {
                showProblem(icon: url.path, title: L10n.t("Could not undo"),
                            detail: L10n.t("\(record.name) did not quit. Quit it yourself, then undo again. Nothing was changed."))
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
            // The old copy that had been running is reopened, without taking focus.
            var done = Set<URL>()
            for old in record.relaunch where done.insert(old).inserted {
                _ = await reopen(old)
            }
            endInstall()
            resultShowing = true
            show(DiskImagePrompt(iconPath: (record.replaced.first?.original ?? url).path,
                                 title: L10n.t("Restored \(record.name)"),
                                 detail: L10n.t("The updated copy is in the Trash."), style: .done))
            armDismiss(after: 3)
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
    /// Stopped before the final move; nothing new is in Applications. `restored`
    /// is false when an old copy moved to the Trash could not be put back.
    case cancelled(restored: Bool)

    /// The app is in /Applications (whatever the disk image or download did after).
    var installedApp: Bool {
        switch self {
        case .installed, .installedKeepingMount, .installedKeepingDownload: return true
        case .failed, .cancelled: return false
        }
    }
}

/// One install's cancellation: the subprocess it is running (by the PID it
/// started) and the commit point past which Cancel is refused. Shared by the
/// main actor (Cancel) and the detached install; every field is guarded by `lock`.
final class DiskImageInstallControl: @unchecked Sendable {
    private let lock = NSLock()
    private var cancelled = false
    private var committed = false
    private var running: Process?
    private let onCommit: (@Sendable () -> Void)?

    init(onCommit: (@Sendable () -> Void)? = nil) {
        self.onCommit = onCommit
    }

    var isCancelled: Bool {
        lock.lock()
        defer { lock.unlock() }
        return cancelled
    }

    /// Stops the install and terminates its running subprocess. False once the
    /// install is committed (the final move has begun): nothing changes then.
    func requestCancel() -> Bool {
        lock.lock()
        defer { lock.unlock() }
        guard !committed else { return false }
        cancelled = true
        if let running, running.isRunning { running.terminate() }
        return true
    }

    /// Called before the final move. False if cancel came first; the move must not run.
    func commit() -> Bool {
        lock.lock()
        guard !cancelled else {
            lock.unlock()
            return false
        }
        committed = true
        lock.unlock()
        onCommit?()
        return true
    }

    func attach(_ process: Process) {
        lock.lock()
        defer { lock.unlock() }
        running = process
        if cancelled, process.isRunning { process.terminate() }
    }

    func detach() {
        lock.lock()
        running = nil
        lock.unlock()
    }
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
                        replacing: Bool = false,
                        control: DiskImageInstallControl? = nil) -> DiskImageInstallResult {
        let cancelledResult = DiskImageInstallResult(outcome: .cancelled(restored: true), destinationURL: nil)
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
        let stagedApp = stagingDirectory.appendingPathComponent(candidate.appURL.lastPathComponent,
                                                                 isDirectory: true)
        // Whatever is still staged on the way out (a cancel, a failed check) goes to the Trash.
        defer { clearStaging(stagingDirectory, stagedApp: stagedApp, fileManager: fm) }
        // Carrying the mounted image's quarantine over leaves the installed app eligible for
        // path randomization, so macOS runs it from a read-only random location instead of
        // Applications. The checks below are the same assessment that flag defers to.
        // An app that does not pass them (installed on the person's say-so) keeps whatever
        // quarantine it has, so macOS still asks the first time it opens.
        var flags = ["--rsrc", "--extattr", "--acl"]
        if candidate.trusted { flags.append("--noqtn") }
        let copy = run("/usr/bin/ditto", flags + [candidate.appURL.path, stagedApp.path], control: control)
        if control?.isCancelled == true { return cancelledResult }
        guard copy.status == 0, validBundle(at: stagedApp) else {
            return DiskImageInstallResult(outcome: .failed(.copy), destinationURL: destinationURL)
        }
        if candidate.trusted {
            guard gatekeeperAccepts(stagedApp, control: control) else {
                if control?.isCancelled == true { return cancelledResult }
                return DiskImageInstallResult(outcome: .failed(.verification), destinationURL: destinationURL)
            }
        }

        var trashedOld: [DiskImageTrashedCopy] = []
        var cancelledBeforeMove = false
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
                    // Cancel stops before the next old copy is trashed; those already trashed go back.
                    if control?.isCancelled == true { break }
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
            // The commit point: from here Cancel is refused and the move runs.
            cancelledBeforeMove = control?.commit() == false
            if !cancelledBeforeMove {
                try fm.moveItem(at: stagedApp, to: destinationURL)
            }
        } catch {
            restoreTrashed(trashedOld, fileManager: fm)
            return DiskImageInstallResult(outcome: .failed(.copy), destinationURL: destinationURL)
        }
        if cancelledBeforeMove {
            let restored = restoreTrashed(trashedOld, fileManager: fm)
            return DiskImageInstallResult(outcome: .cancelled(restored: restored), destinationURL: nil)
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

    /// Moves a partial staged copy to the Trash, then removes the staging
    /// folder only if it is empty. Never deletes content.
    private static func clearStaging(_ stagingDirectory: URL, stagedApp: URL, fileManager fm: FileManager) {
        if fm.fileExists(atPath: stagedApp.path) {
            try? fm.trashItem(at: stagedApp, resultingItemURL: nil)
        }
        if (try? fm.contentsOfDirectory(atPath: stagingDirectory.path))?.isEmpty == true {
            try? fm.removeItem(at: stagingDirectory)
        }
    }

    /// Puts old copies back from the Trash. Returns false if any could not go back.
    @discardableResult
    private static func restoreTrashed(_ copies: [DiskImageTrashedCopy], fileManager fm: FileManager) -> Bool {
        var allBack = true
        for entry in copies.reversed() where !fm.fileExists(atPath: entry.original.path) {
            do {
                try fm.moveItem(at: entry.inTrash, to: entry.original)
            } catch {
                allBack = false
            }
        }
        return allBack
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

    private static func gatekeeperAccepts(_ appURL: URL, control: DiskImageInstallControl?) -> Bool {
        let signature = run("/usr/bin/codesign", ["--verify", "--deep", "--strict", appURL.path], control: control)
        guard signature.status == 0 else { return false }
        let status = run("/usr/sbin/spctl", ["--status"], control: control)
        if String(data: status.output, encoding: .utf8)?.localizedCaseInsensitiveContains("disabled") == true {
            return true
        }
        return run("/usr/sbin/spctl", ["-a", "-t", "exec", appURL.path], control: control).status == 0
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
                            mergingErrors: Bool = false,
                            control: DiskImageInstallControl? = nil) -> CommandResult {
        // A cancelled install starts nothing further.
        if control?.isCancelled == true { return CommandResult(status: -1, output: Data()) }
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
        control?.attach(process)
        let data = output.fileHandleForReading.readDataToEndOfFile()
        process.waitUntilExit()
        control?.detach()
        return CommandResult(status: process.terminationStatus, output: data)
    }
}

private extension DiskImageCandidate {
    var asFinding: DiskImageFinding { .app(self) }
}
