// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Vorssaint
//
// Cockpit: adapted from Vorssaint's DiskImageInstallerService (GPL-3.0-or-later),
// see mac/Notch/FORK.md. The identity checks, collision handling, staged copy,
// Gatekeeper assessment and failure outcomes are Vorssaint's. The prompt is
// replaced: Vorssaint's NSAlert with options becomes a small non-modal card
// at the top of the screen (no Dock icon, no activation), and the options
// are Cockpit preferences instead of checkboxes. Installs go to /Applications.

import AppKit
import Combine
import Darwin
import Foundation
import SwiftUI

/// Offers to install an app from a freshly mounted disk image, then ejects the
/// image and optionally moves the downloaded .dmg to the Trash.
@MainActor
final class DiskImageInstaller {
    private let preferences: Preferences
    private var mountObserver: NSObjectProtocol?
    private var unmountObserver: NSObjectProtocol?
    private var pending: [DiskImageCandidate] = []
    private var processingMounts = Set<String>()
    private var current: DiskImageCandidate?
    private var installing = false
    private let card = DiskImageCard()

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
            current = nil
            card.hide()
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
        if !installing, let current, current.mountURL.standardizedFileURL.path == path {
            self.current = nil
            card.hide()
            presentNext()
        }
    }

    private func inspect(mountURL: URL) {
        guard mountObserver != nil else { return }
        let path = mountURL.standardizedFileURL.resolvingSymlinksInPath().path
        guard processingMounts.insert(path).inserted else { return }
        Task {
            let candidate = await Task.detached(priority: .utility) {
                DiskImageInstallWork.candidate(mountedAt: mountURL)
            }.value
            processingMounts.remove(path)
            guard mountObserver != nil, let candidate else { return }
            pending.append(candidate)
            presentNext()
        }
    }

    private func presentNext() {
        guard current == nil, !installing, mountObserver != nil, !pending.isEmpty else { return }
        let candidate = pending.removeFirst()
        current = candidate
        card.show(.init(
            icon: NSWorkspace.shared.icon(forFile: candidate.appURL.path),
            title: "Install \(candidate.displayName) & eject",
            detail: "Copies it to Applications, then ejects the disk image.",
            kind: .offer,
            primary: "Install & eject",
            secondary: "Not now",
            onPrimary: { [weak self] in self?.install(candidate) },
            onSecondary: { [weak self] in self?.finish() }))
    }

    private func finish() {
        current = nil
        installing = false
        card.hide()
        presentNext()
    }

    private func install(_ candidate: DiskImageCandidate) {
        installing = true
        let trash = preferences.convDiskImageTrashDownload
        card.show(.init(icon: NSWorkspace.shared.icon(forFile: candidate.appURL.path),
                        title: "Installing \(candidate.displayName)",
                        detail: "Copying and checking it.",
                        kind: .working, primary: nil, secondary: nil,
                        onPrimary: {}, onSecondary: {}))
        Task {
            let result = await Task.detached(priority: .utility) {
                DiskImageInstallWork.install(candidate, trashingDownload: trash)
            }.value
            showResult(result, candidate: candidate)
        }
    }

    private func showResult(_ result: DiskImageInstallResult, candidate: DiskImageCandidate) {
        let name = candidate.displayName
        var title = "Installed \(name)"
        var detail = ""
        var warning = false
        switch result.outcome {
        case let .installed(downloadTrashed):
            detail = downloadTrashed
                ? "It is in Applications. The disk image was ejected and the download moved to the Trash."
                : "It is in Applications and the disk image was ejected. The download is still where it was."
        case .installedKeepingMount:
            warning = true
            detail = "It is in Applications, but the disk image is busy and still mounted."
        case .installedKeepingDownload:
            warning = true
            detail = "It is in Applications and the disk image was ejected, but the download could not be moved to the Trash."
        case let .failed(failure):
            warning = true
            title = "Could not install \(name)"
            switch failure {
            case .alreadyInstalled: detail = "\(name) is already in Applications, so nothing was copied."
            case .verification: detail = "The app did not pass the signature check, so it was not installed."
            case .copy: detail = "Copying it failed. Nothing was installed."
            }
        }
        card.show(.init(
            icon: NSWorkspace.shared.icon(forFile: result.destinationURL?.path ?? candidate.appURL.path),
            title: title, detail: detail, kind: warning ? .warning : .done,
            primary: "Done", secondary: nil,
            onPrimary: { [weak self] in self?.finish() }, onSecondary: {}),
            autoHideAfter: warning ? 15 : 8,
            onAutoHide: { [weak self] in self?.finish() })
    }
}

// MARK: - Work off the main actor (Vorssaint's checks and install)

struct DiskImageCandidate: Sendable {
    let mountURL: URL
    let appURL: URL
    let imageURL: URL
    let imageIdentity: DiskImageFileIdentity
    let displayName: String
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

    /// A mounted image holding exactly one real app that is not installed yet.
    static func candidate(mountedAt mountURL: URL) -> DiskImageCandidate? {
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
        guard apps.count == 1, let appURL = apps.first,
              let collisionURLs = DiskImageInstallerSupport.collisionURLs(
                for: appURL, useUserApplications: false, fileManager: fm),
              collisionURLs.allSatisfy({ !fm.fileExists(atPath: $0.path) })
        else { return nil }

        let preferred = Bundle(url: appURL)?.object(forInfoDictionaryKey: "CFBundleDisplayName") as? String
        return DiskImageCandidate(mountURL: mountURL, appURL: appURL, imageURL: imageURL,
                                  imageIdentity: imageIdentity,
                                  displayName: DiskImageInstallerSupport.displayName(preferred: preferred,
                                                                                     appURL: appURL))
    }

    static func install(_ candidate: DiskImageCandidate, trashingDownload: Bool) -> DiskImageInstallResult {
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
        guard collisionURLs.allSatisfy({ !fm.fileExists(atPath: $0.path) }) else {
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
        let copy = run("/usr/bin/ditto", ["--rsrc", "--extattr", "--acl", "--noqtn",
                                           candidate.appURL.path, stagedApp.path])
        guard copy.status == 0, validBundle(at: stagedApp) else {
            return DiskImageInstallResult(outcome: .failed(.copy), destinationURL: destinationURL)
        }
        guard gatekeeperAccepts(stagedApp) else {
            return DiskImageInstallResult(outcome: .failed(.verification), destinationURL: destinationURL)
        }

        do {
            guard let finalCollisionURLs = DiskImageInstallerSupport.collisionURLs(
                for: candidate.appURL, useUserApplications: false, fileManager: fm),
                finalCollisionURLs.contains(destinationURL)
            else { return DiskImageInstallResult(outcome: .failed(.copy), destinationURL: destinationURL) }
            guard finalCollisionURLs.allSatisfy({ !fm.fileExists(atPath: $0.path) }) else {
                return DiskImageInstallResult(outcome: .failed(.alreadyInstalled), destinationURL: destinationURL)
            }
            try fm.moveItem(at: stagedApp, to: destinationURL)
        } catch {
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

    private static func fileIdentity(at url: URL) -> DiskImageFileIdentity? {
        var info = stat()
        guard url.path.withCString({ lstat($0, &info) }) == 0,
              (info.st_mode & S_IFMT) == S_IFREG
        else { return nil }
        return DiskImageFileIdentity(device: UInt64(info.st_dev), inode: UInt64(info.st_ino))
    }

    private static func run(_ executable: String, _ arguments: [String]) -> CommandResult {
        let process = Process()
        let output = Pipe()
        process.executableURL = URL(fileURLWithPath: executable)
        process.arguments = arguments
        process.standardOutput = output
        process.standardError = FileHandle.nullDevice
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

// MARK: - The card

/// A small floating card at the top of the screen with the pointer: not key,
/// not activating, no Dock icon. It stays until answered (offer) or for a few
/// seconds (result).
@MainActor
final class DiskImageCard {
    struct Content {
        enum Kind { case offer, working, done, warning }
        let icon: NSImage
        let title: String
        let detail: String
        let kind: Kind
        let primary: String?
        let secondary: String?
        let onPrimary: () -> Void
        let onSecondary: () -> Void
    }

    private var panel: NSPanel?
    private var hideTimer: Timer?

    func show(_ content: Content, autoHideAfter: TimeInterval? = nil, onAutoHide: (() -> Void)? = nil) {
        hideTimer?.invalidate()
        hideTimer = nil
        let host = NSHostingController(rootView: DiskImageCardView(content: content))
        host.view.layoutSubtreeIfNeeded()
        let size = host.view.fittingSize

        let panel = self.panel ?? Self.makePanel()
        self.panel = panel
        panel.contentViewController = host
        let mouse = NSEvent.mouseLocation
        let screen = NSScreen.screens.first { $0.frame.contains(mouse) } ?? NSScreen.main ?? NSScreen.screens[0]
        let area = screen.visibleFrame
        panel.setFrame(NSRect(x: (area.midX - size.width / 2).rounded(),
                              y: (area.maxY - size.height - 12).rounded(),
                              width: size.width, height: size.height), display: true)
        panel.orderFrontRegardless()
        if let autoHideAfter {
            hideTimer = Timer.scheduledTimer(withTimeInterval: autoHideAfter, repeats: false) { _ in
                MainActor.assumeIsolated { onAutoHide?() }
            }
        }
    }

    func hide() {
        hideTimer?.invalidate()
        hideTimer = nil
        panel?.orderOut(nil)
        panel?.contentViewController = nil
    }

    private static func makePanel() -> NSPanel {
        let panel = NSPanel(contentRect: .zero, styleMask: [.borderless, .nonactivatingPanel],
                            backing: .buffered, defer: false)
        panel.level = .statusBar
        panel.isOpaque = false
        panel.backgroundColor = .clear
        panel.hasShadow = true
        panel.hidesOnDeactivate = false
        panel.isReleasedWhenClosed = false
        panel.collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary, .transient, .ignoresCycle]
        return panel
    }
}

private struct DiskImageCardView: View {
    let content: DiskImageCard.Content

    var body: some View {
        HStack(spacing: 12) {
            Image(nsImage: content.icon).resizable().frame(width: 40, height: 40)
            VStack(alignment: .leading, spacing: 4) {
                Text(verbatim: content.title)
                    .font(.system(size: 13, weight: .semibold))
                    .lineLimit(1).truncationMode(.middle)
                Text(verbatim: content.detail)
                    .font(.system(size: 11))
                    .foregroundStyle(content.kind == .warning ? Color.orange : Color.secondary)
                    .fixedSize(horizontal: false, vertical: true)
                if content.kind == .working {
                    ProgressView().progressViewStyle(.linear).controlSize(.small)
                }
                if content.primary != nil || content.secondary != nil {
                    HStack(spacing: 8) {
                        if let primary = content.primary {
                            Button(primary, action: content.onPrimary)
                                .buttonStyle(.borderedProminent).controlSize(.small)
                        }
                        if let secondary = content.secondary {
                            Button(secondary, action: content.onSecondary)
                                .controlSize(.small)
                        }
                    }
                    .padding(.top, 2)
                }
            }
            .frame(width: 270, alignment: .leading)
        }
        .padding(14)
        .background(.regularMaterial, in: RoundedRectangle(cornerRadius: 16, style: .continuous))
        .overlay(RoundedRectangle(cornerRadius: 16, style: .continuous)
            .strokeBorder(Color.white.opacity(0.12), lineWidth: 1))
        .padding(8)
    }
}
