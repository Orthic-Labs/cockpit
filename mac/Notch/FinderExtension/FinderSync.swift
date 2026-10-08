import Cocoa
import FinderSync

// Layout, NSExtension keys and sandbox entitlements adapted from Uninstally's
// UninstallyFinder extension (github.com/gostonx/uninstally, MIT, (c) 2026
// Codenta). See docs/donors.md.

/// Adds Copy Path, Copy Path (escaped) and Open in Terminal as top-level items
/// in Finder's right-click menu, system-wide.
///
/// Finder Sync extensions must be sandboxed, so this does nothing but the
/// pasteboard and a Launch Services open; it never touches file contents.
final class FinderSync: FIFinderSync {
    override init() {
        super.init()
        // Observe the whole file system so the menu appears wherever the user
        // right-clicks (internal disk, external drives, any folder).
        FIFinderSyncController.default().directoryURLs = [URL(fileURLWithPath: "/")]
    }

    // MARK: Menu

    override func menu(for menuKind: FIMenuKind) -> NSMenu? {
        guard menuKind == .contextualMenuForItems || menuKind == .contextualMenuForContainer else { return nil }
        // Top-level items, no submenu: Copy Path first, then the rest.
        let menu = NSMenu(title: "")
        menu.addItem(item("Copy Path", #selector(copyPath(_:)), symbol: "doc.on.clipboard"))
        menu.addItem(item("Copy Path (escaped)", #selector(copyEscapedPath(_:)), symbol: "terminal"))
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

    @objc private func copyEscapedPath(_ sender: AnyObject?) {
        let paths = targets().map { Self.shellQuoted($0.path) }
        guard !paths.isEmpty else { return }
        // Space-separated so the result pastes straight into a command line.
        write(paths.joined(separator: " "))
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

    /// Single-quote for POSIX shells: `it's` becomes `'it'\''s'`.
    static func shellQuoted(_ path: String) -> String {
        "'" + path.replacingOccurrences(of: "'", with: "'\\''") + "'"
    }

    /// A real folder; app bundles and other packages count as files.
    private static func isFolder(_ url: URL) -> Bool {
        guard let values = try? url.resourceValues(forKeys: [.isDirectoryKey, .isPackageKey]) else {
            return url.hasDirectoryPath
        }
        return values.isDirectory == true && values.isPackage != true
    }
}
