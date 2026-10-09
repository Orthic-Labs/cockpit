// Copyright (c) 2026 Damned Ventures LLC, d/b/a Orthic Labs. All rights reserved.

import AppKit

/// **Installing apps from disk images, in the notch.** Watches for mounted
/// disk images; an image holding exactly one app is verified and installed
/// (automatically when the preferences allow it, otherwise after a question on
/// the notch), a .pkg is offered to open, and an image with several apps is
/// offered in Finder. The off-main-thread work lives in `DiskImageWork`.
///
/// Automatic rules:
/// - `convDiskImageAuto`: a verified (valid signature, notarized) app that is
///   not installed and not running goes in without a question.
/// - `convDiskImageAutoUpdate`: a verified app that is newer than the copy in
///   /Applications replaces it, after quitting that copy gracefully, and the
///   copy is reopened without taking focus if it had been running.
/// - Anything else is asked. With every notch hidden, questions are skipped.
///
/// Every install shows a working card with Cancel until the final move
/// begins. Results carry Undo while the card is up.
@MainActor
final class DiskImageInstaller {
    /// Shows a card on the notch(es), or clears it with nil. Returns whether
    /// any notch could show it (a hidden notch cannot).
    var present: ((DiskImagePrompt?) -> Bool)?

    private let preferences: Preferences
    private var observers: [NSObjectProtocol] = []

    /// Mount points being read, so one mount is read once.
    private var reading = Set<String>()
    /// Images read and waiting their turn, and the one being handled.
    private var queue: [DiskImageContents] = []
    private var current: DiskImageContents?

    /// A running install and its token: late callbacks from an earlier one
    /// compare the token and are dropped.
    private var control: DiskImageInstallControl?
    private var jobToken: UUID?
    private var busy = false
    private var cancelOffered = false
    /// The image behind the running install was ejected before the final move.
    private var ejectedWhileBusy = false

    /// A result card is up (and whether it goes away by itself).
    private var resultUp = false
    private var resultExpires = false
    private var dismissTimer: Timer?
    private var hovering = false
    private var undoable: Installation?

    private static let resultLife: TimeInterval = 8
    private static let quitPatience: TimeInterval = 10

    /// What an install left behind, for Undo.
    private struct Installation {
        let destination: URL
        let bundleID: String?
        let name: String
        let replaced: [DiskImageTrashedCopy]
        /// Bundles that were running and were quit; Undo reopens them.
        let wasRunning: [URL]
    }

    init(preferences: Preferences) {
        self.preferences = preferences
    }

    // MARK: - Lifecycle

    /// Follows the preference: watching only while it is on.
    func sync() {
        preferences.convDiskImageInstaller ? start() : stop()
    }

    func stop() {
        let center = NSWorkspace.shared.notificationCenter
        observers.forEach { center.removeObserver($0) }
        observers.removeAll()
        control?.requestCancel()
        queue.removeAll()
        reading.removeAll()
        if current != nil || resultUp { clearCard() }
    }

    private func start() {
        guard observers.isEmpty else { return }
        let center = NSWorkspace.shared.notificationCenter
        observers.append(center.addObserver(forName: NSWorkspace.didMountNotification,
                                            object: nil, queue: .main) { [weak self] note in
            guard let url = note.userInfo?[NSWorkspace.volumeURLUserInfoKey] as? URL else { return }
            Task { @MainActor in self?.mounted(url) }
        })
        observers.append(center.addObserver(forName: NSWorkspace.didUnmountNotification,
                                            object: nil, queue: .main) { [weak self] note in
            guard let url = note.userInfo?[NSWorkspace.volumeURLUserInfoKey] as? URL else { return }
            Task { @MainActor in self?.unmounted(url) }
        })
    }

    // MARK: - Mounts

    private func mounted(_ url: URL) {
        guard preferences.convDiskImageInstaller, Self.openedByUser(url),
              reading.insert(url.path).inserted else { return }
        Task { [weak self] in
            let contents = await Task.detached { DiskImageWork.inspect(mountedAt: url) }.value
            self?.finishedReading(url, contents)
        }
    }

    /// Whether a mount is one the user opened: a browsable volume under
    /// /Volumes. Background mounts (`hdiutil attach -nobrowse`), hidden
    /// volumes, and mounts elsewhere (simulator runtimes) are ignored. A
    /// volume that cannot be read is treated as not opened by the user.
    private static func openedByUser(_ url: URL) -> Bool {
        guard url.standardizedFileURL.path.hasPrefix("/Volumes/") else { return false }
        let values = try? url.resourceValues(forKeys: [.volumeIsBrowsableKey, .isHiddenKey])
        return values?.volumeIsBrowsable == true && values?.isHidden != true
    }

    private func finishedReading(_ url: URL, _ contents: DiskImageContents?) {
        reading.remove(url.path)
        guard preferences.convDiskImageInstaller, let contents else { return }
        // Ejected while it was being read: nothing to offer.
        guard FileManager.default.fileExists(atPath: contents.mountURL.path) else { return }
        queue.append(contents)
        advance()
    }

    /// An image went away: a question about it is moot, and an install still
    /// short of its final move is cancelled (the install's own eject comes
    /// after that point and is ignored here).
    private func unmounted(_ url: URL) {
        queue.removeAll { $0.mountURL.path == url.path }
        guard let current, current.mountURL.path == url.path else { return }
        if busy {
            if jobToken != nil, control?.requestCancel() == true { ejectedWhileBusy = true }
            return
        }
        guard !resultUp else { return }
        clearCard()
    }

    private func advance() {
        guard current == nil, !busy, !queue.isEmpty else { return }
        let next = queue.removeFirst()
        current = next
        switch next {
        case .app(let app):
            handle(app)
        case .installer(_, let pkg):
            ask(DiskImagePrompt(
                iconPath: pkg.path,
                title: pkg.deletingPathExtension().lastPathComponent,
                detail: L10n.t("This disk image holds an installer."),
                style: .ask,
                primary: .init(choice: .openInstaller, label: L10n.t("Open installer"))))
        case .several(let mount):
            ask(DiskImagePrompt(
                iconPath: mount.path,
                title: mount.lastPathComponent,
                detail: L10n.t("This disk image holds more than one app."),
                style: .ask,
                primary: .init(choice: .showImage, label: L10n.t("Show in Finder"))))
        }
    }

    // MARK: - Deciding

    private func handle(_ app: DiskImageApp) {
        if app.nameTaken {
            ask(problem(app, title: L10n.t("Can't install \(app.name)"),
                        detail: L10n.t("A different app already has that name in Applications.")))
            return
        }
        let running = runningCopies(of: app)
        if app.copies.isEmpty {
            if preferences.convDiskImageAuto, app.trusted, running.isEmpty {
                runInstall(app, replacing: false)
            } else {
                ask(question(app, running: !running.isEmpty))
            }
        } else {
            let installedNewest = app.copies.map(\.version).reduce(app.copies[0].version) {
                $1.isNewer(than: $0) ? $1 : $0
            }
            if preferences.convDiskImageAutoUpdate, app.trusted, app.version.isNewer(than: installedNewest) {
                runInstall(app, replacing: true)
            } else {
                ask(question(app, running: !running.isEmpty))
            }
        }
    }

    /// Shows a question; when no notch can show it, the image is left alone.
    private func ask(_ prompt: DiskImagePrompt) {
        guard present?(prompt) == true else {
            current = nil
            advance()
            return
        }
    }

    private func question(_ app: DiskImageApp, running: Bool) -> DiskImagePrompt {
        let installed = !app.copies.isEmpty
        var detail: String
        if installed {
            let have = app.copies.compactMap(\.version.display).first
            switch (have, app.version.display) {
            case let (old?, new?): detail = L10n.t("Installed \(old). This image has \(new).")
            default: detail = L10n.t("A copy is already in Applications.")
            }
        } else {
            detail = app.version.display.map { L10n.t("Version \($0). Copies it to Applications and ejects the image.") }
                ?? L10n.t("Copies it to Applications and ejects the image.")
        }

        var warning: String?
        if !app.signatureValid {
            warning = L10n.t("Its code signature does not verify.")
        } else if !app.notarized {
            warning = L10n.t("Not notarized. macOS checks it again when you open it.")
        } else if running {
            warning = L10n.t("It is running and will be asked to quit first.")
        }

        let choice: DiskImageChoice
        let label: String
        if running {
            choice = .quitAndUpdate
            label = installed ? L10n.t("Quit & update") : L10n.t("Quit & install")
        } else if installed {
            choice = .replace
            label = L10n.t("Replace")
        } else {
            choice = .install
            label = app.trusted ? L10n.t("Install") : L10n.t("Install anyway")
        }
        return DiskImagePrompt(iconPath: app.appURL.path,
                               title: installed ? app.name : L10n.t("Install \(app.name)?"),
                               detail: detail, warning: warning, style: .ask,
                               primary: .init(choice: choice, label: label))
    }

    private func problem(_ app: DiskImageApp, title: String, detail: String) -> DiskImagePrompt {
        DiskImagePrompt(iconPath: app.appURL.path, title: title, detail: detail, style: .problem,
                        primary: .init(choice: .showImage, label: L10n.t("Show in Finder")))
    }

    /// Instances of the app that are running from somewhere other than the
    /// image itself.
    private func runningCopies(of app: DiskImageApp) -> [NSRunningApplication] {
        guard let id = app.bundleID else { return [] }
        let me = ProcessInfo.processInfo.processIdentifier
        let image = app.mountURL.path + "/"
        return NSRunningApplication.runningApplications(withBundleIdentifier: id).filter {
            $0.processIdentifier != me && !$0.isTerminated
                && !($0.bundleURL?.path ?? "").hasPrefix(image)
        }
    }

    // MARK: - The notch's answer

    func choose(_ choice: DiskImageChoice) {
        switch choice {
        case .install, .replace, .quitAndUpdate:
            guard !busy, case .app(let app) = current else { return }
            runInstall(app, replacing: choice != .install && !app.copies.isEmpty)
        case .undo:
            undo()
        case .openInstaller:
            if case .installer(_, let pkg) = current { NSWorkspace.shared.open(pkg) }
            clearCard()
        case .showImage:
            if let mount = current?.mountURL { NSWorkspace.shared.open(mount) }
            clearCard()
        case .sendTo, .refresh, .copyText, .openLink:
            break
        case .cancel:
            control?.requestCancel()
        case .dismiss:
            if busy {
                // On a working card the close means Cancel.
                control?.requestCancel()
            } else {
                clearCard()
            }
        }
    }

    /// The pointer is on the card or left it: a result stays while hovered.
    func hoverChanged(_ on: Bool) {
        hovering = on
        guard resultUp, resultExpires, !busy else { return }
        dismissTimer?.invalidate()
        if !on { scheduleExpiry(after: 3) }
    }

    // MARK: - Installing

    private func runInstall(_ app: DiskImageApp, replacing: Bool) {
        let token = UUID()
        jobToken = token
        let control = DiskImageInstallControl { [weak self] in
            Task { @MainActor in self?.committed(token) }
        }
        self.control = control
        busy = true
        cancelOffered = true
        ejectedWhileBusy = false
        showWorking(app)
        let trashDownload = preferences.convDiskImageTrashDownload

        Task { [weak self] in
            guard let self else { return }
            // Running copies are asked to quit first.
            let running = self.runningCopies(of: app)
            let reopen = running.compactMap(\.bundleURL)
            if !running.isEmpty {
                let allQuit = await self.quit(running, control: control)
                if control.isCancelled {
                    self.finish(app, DiskImageInstallResult(outcome: .cancelled), token, replacing, reopen)
                    return
                }
                guard allQuit else {
                    self.stillOpen(app, token, reopen)
                    return
                }
            }
            let result = await Task.detached {
                DiskImageWork.install(app, replacing: replacing, trashDownload: trashDownload, control: control)
            }.value
            self.finish(app, result, token, replacing, reopen)
        }
    }

    /// Asks each app to quit and waits up to ten seconds. True if all did.
    private func quit(_ apps: [NSRunningApplication], control: DiskImageInstallControl?) async -> Bool {
        apps.forEach { _ = $0.terminate() }
        let deadline = Date().addingTimeInterval(Self.quitPatience)
        while apps.contains(where: { !$0.isTerminated }) {
            if control?.isCancelled == true || Date() >= deadline { break }
            try? await Task.sleep(nanoseconds: 200_000_000)
        }
        return apps.allSatisfy(\.isTerminated)
    }

    /// The final move has begun: the card stops offering Cancel.
    private func committed(_ token: UUID) {
        guard jobToken == token, busy, case .app(let app) = current else { return }
        cancelOffered = false
        showWorking(app)
    }

    private func showWorking(_ app: DiskImageApp) {
        _ = present?(DiskImagePrompt(
            iconPath: app.appURL.path,
            title: L10n.t("Installing \(app.name)"),
            detail: L10n.t("Copying and checking it."),
            style: .working,
            primary: cancelOffered ? .init(choice: .cancel, label: L10n.t("Cancel")) : nil))
    }

    /// A copy did not quit in time: nothing was changed, so ask again.
    private func stillOpen(_ app: DiskImageApp, _ token: UUID, _ reopen: [URL]) {
        guard jobToken == token else { return }
        reopenApps(reopen)
        busy = false
        control = nil
        jobToken = nil
        let prompt = DiskImagePrompt(
            iconPath: app.appURL.path,
            title: L10n.t("\(app.name) is still open"),
            detail: L10n.t("It did not quit. Save your work, then try again."),
            style: .problem,
            primary: .init(choice: .quitAndUpdate, label: L10n.t("Quit & update")))
        if present?(prompt) != true { current = nil; advance() }
    }

    private func finish(_ app: DiskImageApp, _ result: DiskImageInstallResult, _ token: UUID,
                        _ replacing: Bool, _ reopen: [URL]) {
        guard jobToken == token else { return }
        busy = false
        control = nil
        jobToken = nil
        let ejectedMidway = ejectedWhileBusy
        ejectedWhileBusy = false

        if ejectedMidway {
            switch result.outcome {
            case .cancelled, .failed:
                reopenApps(reopen)
                showResult(DiskImagePrompt(iconPath: app.appURL.path,
                                           title: L10n.t("Could not install \(app.name)"),
                                           detail: L10n.t("The disk image was ejected."), style: .problem),
                           expires: false)
                return
            case .installed:
                break
            }
        }

        switch result.outcome {
        case .installed:
            let destination = app.destinationURL
            if !reopen.isEmpty { reopenApps([destination]) }
            undoable = Installation(destination: destination, bundleID: app.bundleID, name: app.name,
                                    replaced: result.replaced, wasRunning: reopen)
            let updated = replacing
            let title: String
            if updated, let version = app.version.display {
                title = L10n.t("Updated \(app.name) to \(version)")
            } else if updated {
                title = L10n.t("Updated \(app.name)")
            } else {
                title = L10n.t("Installed \(app.name)")
            }
            var notes: [String] = []
            notes.append(result.ejected ? L10n.t("Disk image ejected.") : L10n.t("Disk image is still open."))
            if result.downloadTrashed { notes.append(L10n.t("Download moved to the Trash.")) }
            if !reopen.isEmpty { notes.append(L10n.t("Reopened.")) }
            showResult(DiskImagePrompt(iconPath: destination.path, title: title,
                                       detail: notes.joined(separator: " "), style: .done,
                                       primary: .init(choice: .undo, label: L10n.t("Undo"))),
                       expires: true)
        case .cancelled:
            reopenApps(reopen)
            showResult(DiskImagePrompt(iconPath: app.appURL.path, title: L10n.t("Cancelled"),
                                       detail: L10n.t("\(app.name) was not installed."), style: .done),
                       expires: true)
        case .failed(let failure):
            reopenApps(reopen)
            let detail: String
            switch failure {
            case .occupied: detail = L10n.t("Applications changed while it was being installed.")
            case .copy: detail = L10n.t("It could not be copied to Applications.")
            case .verification: detail = L10n.t("The copy did not pass its signature check.")
            case .replace: detail = L10n.t("The installed copy could not be moved to the Trash.")
            }
            showResult(DiskImagePrompt(iconPath: app.appURL.path, title: L10n.t("Could not install \(app.name)"),
                                       detail: detail, style: .problem,
                                       primary: .init(choice: .showImage, label: L10n.t("Show in Finder"))),
                       expires: false)
        }
    }

    private func reopenApps(_ urls: [URL]) {
        let configuration = NSWorkspace.OpenConfiguration()
        configuration.activates = false
        for url in urls {
            NSWorkspace.shared.openApplication(at: url, configuration: configuration)
        }
    }

    // MARK: - Undo

    private func undo() {
        guard !busy, let installation = undoable else { return }
        busy = true
        dismissTimer?.invalidate()
        let icon = installation.destination.path
        _ = present?(DiskImagePrompt(iconPath: icon, title: L10n.t("Undoing \(installation.name)"),
                                     detail: L10n.t("Putting things back."), style: .working))

        Task { [weak self] in
            guard let self else { return }
            // The new copy is quit gracefully before it is moved.
            let running = NSRunningApplication.runningApplications(
                withBundleIdentifier: installation.bundleID ?? "").filter {
                    installation.bundleID != nil && !$0.isTerminated
                        && $0.bundleURL?.path == installation.destination.path
            }
            let quit = running.isEmpty ? true : await self.quit(running, control: nil)
            var undone = false
            if quit {
                undone = await Task.detached {
                    DiskImageWork.undo(installed: installation.destination, bundleID: installation.bundleID,
                                       replaced: installation.replaced)
                }.value
            }
            self.busy = false
            self.undoable = nil
            if undone {
                let restored = installation.replaced.map(\.original)
                if !installation.wasRunning.isEmpty { self.reopenApps(restored) }
                self.showResult(DiskImagePrompt(
                    iconPath: restored.first?.path ?? icon,
                    title: restored.isEmpty ? L10n.t("Removed \(installation.name)")
                                            : L10n.t("Restored the previous \(installation.name)"),
                    detail: L10n.t("The new copy is in the Trash."), style: .done), expires: true)
            } else {
                self.showResult(DiskImagePrompt(
                    iconPath: icon, title: L10n.t("Could not undo \(installation.name)"),
                    detail: quit ? L10n.t("Something changed since the install.")
                                 : L10n.t("It is still open. Quit it, then try again."),
                    style: .problem), expires: false)
            }
        }
    }

    // MARK: - The card

    private func showResult(_ prompt: DiskImagePrompt, expires: Bool) {
        resultUp = true
        resultExpires = expires
        dismissTimer?.invalidate()
        guard present?(prompt) == true else {
            // No notch to show it on: the work is done, move on.
            clearCard()
            return
        }
        if expires && !hovering { scheduleExpiry(after: Self.resultLife) }
    }

    private func scheduleExpiry(after seconds: TimeInterval) {
        dismissTimer?.invalidate()
        dismissTimer = Timer.scheduledTimer(withTimeInterval: seconds, repeats: false) { [weak self] _ in
            Task { @MainActor in
                guard let self, self.resultUp, !self.busy, !self.hovering else { return }
                self.clearCard()
            }
        }
    }

    /// Takes the card down and starts the next image's turn.
    private func clearCard() {
        dismissTimer?.invalidate()
        dismissTimer = nil
        resultUp = false
        resultExpires = false
        undoable = nil
        current = nil
        _ = present?(nil)
        advance()
    }
}
