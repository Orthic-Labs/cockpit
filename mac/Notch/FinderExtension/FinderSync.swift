import Cocoa
import FinderSync

// Layout, NSExtension keys and sandbox entitlements adapted from Uninstally's
// UninstallyFinder extension (github.com/gostonx/uninstally, MIT, (c) 2026
// Codenta). See docs/donors.md.

/// Adds Cut, Copy Path and Open in Terminal as top-level items in Finder's
/// right-click menu, system-wide.
///
/// Finder Sync extensions must be sandboxed, so this does nothing but the
/// pasteboard and a Launch Services open; it never touches file contents.
final class FinderSync: FIFinderSync {
    override init() {
        super.init()
        // Finder only shows the menu inside observed folders. "/" covers the
        // startup disk; every other mounted volume (external drives, disk
        // images, network shares) needs its own entry, kept current as
        // volumes come and go.
        updateObservedFolders()
        let center = NSWorkspace.shared.notificationCenter
        for name in [NSWorkspace.didMountNotification, NSWorkspace.didUnmountNotification,
                     NSWorkspace.didRenameVolumeNotification] {
            volumeObservers.append(center.addObserver(forName: name, object: nil, queue: .main) { [weak self] _ in
                self?.updateObservedFolders()
            })
        }
    }

    private var volumeObservers: [NSObjectProtocol] = []

    deinit {
        volumeObservers.forEach { NSWorkspace.shared.notificationCenter.removeObserver($0) }
    }

    private func updateObservedFolders() {
        var folders: Set<URL> = [URL(fileURLWithPath: "/")]
        let volumes = FileManager.default.mountedVolumeURLs(includingResourceValuesForKeys: nil,
                                                            options: [.skipHiddenVolumes]) ?? []
        folders.formUnion(volumes)
        FIFinderSyncController.default().directoryURLs = folders
    }

    // MARK: Menu

    override func menu(for menuKind: FIMenuKind) -> NSMenu? {
        guard menuKind == .contextualMenuForItems || menuKind == .contextualMenuForContainer else { return nil }
        // Top-level items, no submenu. Cut only for selected items.
        let menu = NSMenu(title: "")
        if menuKind == .contextualMenuForItems {
            menu.addItem(item("Cut", #selector(cut(_:)), symbol: "scissors"))
        }
        menu.addItem(item("Copy Path", #selector(copyPath(_:)), symbol: "doc.on.clipboard"))
        menu.addItem(item("Open in Terminal", #selector(openInTerminal(_:)), symbol: "apple.terminal"))
        return menu
    }

    private func item(_ title: String, _ action: Selector, symbol: String) -> NSMenuItem {
        let entry = NSMenuItem(title: title, action: action, keyEquivalent: "")
        entry.target = self
        entry.image = NSImage(systemSymbolName: symbol, accessibilityDescription: title)
        return entry
    }

    // MARK: Actions

    @objc private func copyPath(_ sender: AnyObject?) {
        let paths = targets().map(\.path)
        guard !paths.isEmpty else { return }
        write(paths.joined(separator: "\n"))
    }

    /// Puts the items on the pasteboard as files, as Finder's Copy does, and
    /// tells Pulse, whose next ⌘V in Finder then moves them (Move Item Here).
    @objc private func cut(_ sender: AnyObject?) {
        guard let selected = FIFinderSyncController.default().selectedItemURLs(), !selected.isEmpty
        else { return }
        let pasteboard = NSPasteboard.general
        pasteboard.clearContents()
        pasteboard.writeObjects(selected as [NSURL])
        CFNotificationCenterPostNotification(
            CFNotificationCenterGetDarwinNotifyCenter(),
            CFNotificationName("dev.orthic.pulse.finder.cut" as CFString), nil, nil, true)
    }

    @objc private func openInTerminal(_ sender: AnyObject?) {
        guard let first = targets().first,
              let terminal = NSWorkspace.shared.urlForApplication(withBundleIdentifier: "com.apple.Terminal")
        else { return }
        let folder = Self.isFolder(first) ? first : first.deletingLastPathComponent()
        NSWorkspace.shared.open([folder], withApplicationAt: terminal,
                                configuration: NSWorkspace.OpenConfiguration())
    }

    // MARK: Helpers

    /// The selection, or the folder itself when the click was on its background.
    private func targets() -> [URL] {
        let controller = FIFinderSyncController.default()
        if let selected = controller.selectedItemURLs(), !selected.isEmpty { return selected }
        return controller.targetedURL().map { [$0] } ?? []
    }

    private func write(_ text: String) {
        let pasteboard = NSPasteboard.general
        pasteboard.clearContents()
        pasteboard.setString(text, forType: .string)
    }

    /// A real folder; app bundles and other packages count as files.
    private static func isFolder(_ url: URL) -> Bool {
        guard let values = try? url.resourceValues(forKeys: [.isDirectoryKey, .isPackageKey]) else {
            return url.hasDirectoryPath
        }
        return values.isDirectory == true && values.isPackage != true
    }
}
