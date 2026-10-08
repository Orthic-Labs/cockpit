import AppKit
import ApplicationServices

/// Pulse: Cut and Paste for files in Finder.
///
/// ⌘X marks the selected items as cut: their file URLs go on the pasteboard as
/// URLs (never text) and are remembered with the pasteboard's change count.
/// ⌘V in a Finder window then moves them into that window's folder. Any other
/// write to the pasteboard in between changes the count, which cancels the
/// cut. While a Finder text field has focus (renaming, search) both keys keep
/// their normal meaning.
final class FinderCutPaste {
    struct ItemResult {
        let name: String
        let ok: Bool
        let detail: String
    }

    private static let finderID = "com.apple.finder"
    private static let keyX: Int64 = 7
    private static let keyV: Int64 = 9

    private let lock = NSLock()
    private var cut: (urls: [URL], changeCount: Int)?
    private var busy = false
    private let work = DispatchQueue(label: "dev.orthic.pulse.finder-cutpaste")

    /// Called on the main thread after each paste with a result per item.
    var onResults: (([ItemResult]) -> Void)?

    func reset() {
        lock.lock(); cut = nil; lock.unlock()
    }

    // MARK: - Tap handler (tap thread)

    func handle(_ type: CGEventType, _ event: CGEvent) -> TapDecision {
        guard type == .keyDown else { return .pass }
        let modifiers: CGEventFlags = [.maskCommand, .maskShift, .maskAlternate, .maskControl]
        guard event.flags.intersection(modifiers) == .maskCommand else { return .pass }
        let key = event.getIntegerValueField(.keyboardEventKeycode)
        guard key == Self.keyX || key == Self.keyV,
              let finder = NSWorkspace.shared.frontmostApplication,
              finder.bundleIdentifier == Self.finderID,
              Self.textInputIsNotFocused(pid: finder.processIdentifier)
        else { return .pass }

        if key == Self.keyX {
            DispatchQueue.main.async { [weak self] in self?.markCut() }
            return .swallow
        }
        // ⌘V: only ours while a cut is still the pasteboard's content.
        lock.lock()
        let pending = cut
        lock.unlock()
        guard let pending else { return .pass }
        if NSPasteboard.general.changeCount != pending.changeCount {
            lock.lock(); cut = nil; lock.unlock()
            return .pass
        }
        DispatchQueue.main.async { [weak self] in self?.paste(pending.urls) }
        return .swallow
    }

    /// True only when the focused element is positively known and is not a
    /// text input. Unknown keeps the key's normal meaning.
    private static func textInputIsNotFocused(pid: pid_t) -> Bool {
        let app = AX.application(pid, timeout: 0.1)
        guard let focused = AX.element(app, "AXFocusedUIElement") else { return false }
        let role = AX.string(focused, "AXRole") ?? ""
        let subrole = AX.string(focused, "AXSubrole") ?? ""
        return !["AXTextField", "AXTextArea", "AXComboBox"].contains(role) && subrole != "AXSearchField"
    }

    // MARK: - Cut (main thread)

    private func markCut() {
        let urls = fileURLs(from: Self.run("tell application \"Finder\" to return (selection as alias list)"))
        lock.lock()
        if urls.isEmpty {
            cut = nil
            lock.unlock()
            return
        }
        lock.unlock()
        let board = NSPasteboard.general
        board.clearContents()
        guard board.writeObjects(urls as [NSURL]) else { return }
        lock.lock(); cut = (urls, board.changeCount); lock.unlock()
    }

    // MARK: - Paste (main thread, then a worker)

    private func paste(_ urls: [URL]) {
        lock.lock()
        if busy { lock.unlock(); return }
        busy = true
        lock.unlock()
        let script = """
        tell application "Finder"
            if (count of Finder windows) is 0 then return (desktop as alias)
            return (target of front Finder window) as alias
        end tell
        """
        guard let folder = fileURLs(from: Self.run(script)).first else {
            lock.lock(); busy = false; lock.unlock()
            return
        }
        work.async { [weak self] in
            let results = urls.map { Self.move($0, into: folder) }
            DispatchQueue.main.async {
                guard let self else { return }
                self.lock.lock()
                self.cut = nil
                self.busy = false
                self.lock.unlock()
                self.onResults?(results)
            }
        }
    }

    // MARK: - Moving

    private static func move(_ source: URL, into folder: URL) -> ItemResult {
        let fm = FileManager.default
        let name = source.lastPathComponent
        let source = source.standardizedFileURL
        let folder = folder.standardizedFileURL
        guard (try? source.checkResourceIsReachable()) == true else {
            return ItemResult(name: name, ok: false, detail: "No longer there")
        }
        if source.deletingLastPathComponent().path == folder.path {
            return ItemResult(name: name, ok: true, detail: "Already in this folder")
        }
        if folder.path == source.path || folder.path.hasPrefix(source.path + "/") {
            return ItemResult(name: name, ok: false, detail: "Cannot move a folder into itself")
        }
        let destination = freeName(for: name, in: folder)
        let sourceVolume = try? source.resourceValues(forKeys: [.volumeIdentifierKey]).volumeIdentifier
        let folderVolume = try? folder.resourceValues(forKeys: [.volumeIdentifierKey]).volumeIdentifier
        do {
            if let a = sourceVolume as? NSObject, let b = folderVolume as? NSObject, a.isEqual(b) {
                try fm.moveItem(at: source, to: destination)
            } else {
                // Another volume: copy, check the copy, and only then remove
                // the original. A copy that does not check out is discarded
                // and the original stays.
                try fm.copyItem(at: source, to: destination)
                guard verifyCopy(of: source, at: destination) else {
                    try? fm.removeItem(at: destination)
                    return ItemResult(name: name, ok: false, detail: "Copy did not verify; original kept")
                }
                try fm.removeItem(at: source)
            }
        } catch {
            return ItemResult(name: name, ok: false, detail: error.localizedDescription)
        }
        let renamed = destination.lastPathComponent != name
        return ItemResult(name: name, ok: true,
                          detail: renamed ? "Moved as \(destination.lastPathComponent)" : "Moved")
    }

    /// `name`, or `name 2`, `name 3`… before the extension: nothing is ever
    /// overwritten.
    private static func freeName(for name: String, in folder: URL) -> URL {
        func taken(_ url: URL) -> Bool {
            var info = stat()
            return lstat(url.path, &info) == 0
        }
        let first = folder.appendingPathComponent(name)
        guard taken(first) else { return first }
        let base = (name as NSString).deletingPathExtension
        let ext = (name as NSString).pathExtension
        var n = 2
        while true {
            let candidate = folder.appendingPathComponent(ext.isEmpty ? "\(base) \(n)" : "\(base) \(n).\(ext)")
            if !taken(candidate) { return candidate }
            n += 1
        }
    }

    /// Every entry of the original exists in the copy with the same kind, and
    /// every regular file has the same size.
    private static func verifyCopy(of source: URL, at copy: URL) -> Bool {
        let keys: [URLResourceKey] = [.isDirectoryKey, .isSymbolicLinkKey, .fileSizeKey]
        func matches(_ a: URL, _ b: URL) -> Bool {
            guard let x = try? a.resourceValues(forKeys: Set(keys)),
                  let y = try? b.resourceValues(forKeys: Set(keys))
            else { return false }
            return x.isDirectory == y.isDirectory && x.isSymbolicLink == y.isSymbolicLink
                && (x.isDirectory == true || x.fileSize == y.fileSize)
        }
        guard matches(source, copy) else { return false }
        guard let walker = FileManager.default.enumerator(at: source, includingPropertiesForKeys: keys)
        else { return false }
        let prefix = source.path.hasSuffix("/") ? source.path : source.path + "/"
        for case let item as URL in walker {
            let relative = String(item.standardizedFileURL.path.dropFirst(prefix.count))
            if !matches(item, copy.appendingPathComponent(relative)) { return false }
        }
        return true
    }

    // MARK: - Finder over Apple Events (main thread)

    private static func run(_ source: String) -> NSAppleEventDescriptor? {
        var error: NSDictionary?
        guard let script = NSAppleScript(source: source) else { return nil }
        let result = script.executeAndReturnError(&error)
        return error == nil ? result : nil
    }

    /// File URLs from a list (or single) descriptor: structured, never text.
    private func fileURLs(from descriptor: NSAppleEventDescriptor?) -> [URL] {
        guard let descriptor else { return [] }
        func url(_ item: NSAppleEventDescriptor) -> URL? {
            guard let coerced = item.coerce(toDescriptorType: DescType(typeFileURL)) else { return nil }
            return URL(dataRepresentation: coerced.data, relativeTo: nil)
        }
        if descriptor.numberOfItems > 0 {
            return (1...descriptor.numberOfItems).compactMap { descriptor.atIndex($0).flatMap(url) }
        }
        return url(descriptor).map { [$0] } ?? []
    }
}
