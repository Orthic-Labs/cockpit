import AppKit
import Carbon.HIToolbox
import Combine
import SwiftUI

/// A panel that can take keyboard focus without activating the app, so the
/// launcher never puts Pulse in the Dock or in front of the app you were in.
private final class LauncherPanel: NSPanel {
    var onResignKey: (() -> Void)?
    override var canBecomeKey: Bool { true }
    override var canBecomeMain: Bool { false }
    override func resignKey() {
        super.resignKey()
        onResignKey?()
    }
}

/// Owns the launcher: the hotkeys, the panel, and the settings that switch it on.
@MainActor
final class LauncherController {
    /// Carbon hotkey ids: the launcher, then one per app hotkey, then one per command hotkey.
    private static let mainID: UInt32 = 1
    private static let appIDBase: UInt32 = 100
    private static let commandIDBase: UInt32 = 1_000

    private let preferences: Preferences
    private let hotKey = LauncherHotKey()
    private let model = LauncherModel()
    private var panel: LauncherPanel?
    private var keyMonitor: Any?
    private var cancellables = Set<AnyCancellable>()

    /// Why a hotkey is not working, when it is not. Shown in the hub.
    private(set) var status: String?

    init(preferences: Preferences, snapshots: @escaping () -> [ProviderSnapshot]) {
        self.preferences = preferences
        model.snapshots = snapshots
        model.dismiss = { [weak self] in self?.hide() }
    }

    func start() {
        model.onConfigChange = { [weak self] config in
            self?.preferences.launcherConfigJSON = config.encoded()
        }
        Publishers.CombineLatest3(preferences.$launcherEnabled, preferences.$launcherHotkey,
                                  preferences.$launcherConfigJSON)
            .removeDuplicates { $0 == $1 }
            .sink { [weak self] enabled, choice, json in
                self?.apply(enabled: enabled, choice: choice, json: json)
            }
            .store(in: &cancellables)
        // Feature switches are read after the change lands, hence the hop.
        preferences.objectWillChange
            .receive(on: DispatchQueue.main)
            .sink { [weak self] _ in self?.applyFeatures() }
            .store(in: &cancellables)
        applyFeatures()
    }

    private func applyFeatures() {
        let next = LauncherFeatures(
            clipboard: preferences.launcherClipboard,
            currency: preferences.launcherCurrency,
            dictionary: preferences.launcherDictionary,
            shortcuts: preferences.launcherShortcuts)
        if next != model.features { model.features = next }
        model.clipboard.enabled = preferences.launcherEnabled && next.clipboard
    }

    private func apply(enabled: Bool, choice: LauncherHotkeyChoice, json: String) {
        model.config = LauncherConfig.decode(json)
        hotKey.unbind(from: Self.mainID + 1)
        guard enabled else {
            hotKey.unbind(from: Self.mainID)
            status = nil
            hide()
            return
        }
        var problems: [String] = []
        if let problem = bindMain(choice) { problems.append(problem) }
        problems += bindHotkeys(model.config)
        status = problems.isEmpty ? nil : problems.joined(separator: " ")
    }

    private func bindMain(_ choice: LauncherHotkeyChoice) -> String? {
        let result = hotKey.bind(id: Self.mainID, keyCode: choice.keyCode, modifiers: choice.modifiers) { [weak self] in
            self?.toggle()
        }
        if result == noErr { return nil }
        if result == OSStatus(eventHotKeyExistsErr) {
            var text = "\(choice.display) is already taken by another app or by macOS."
            if choice == .commandSpace { text += " Turn off Spotlight's shortcut in System Settings first." }
            return text + " Pick a different shortcut."
        }
        return "\(choice.display) could not be registered (error \(result))."
    }

    private func bindHotkeys(_ config: LauncherConfig) -> [String] {
        var problems: [String] = []
        for (position, binding) in config.appHotkeys.enumerated() {
            guard let combo = LauncherKeyCombo.parse(binding.hotkey) else {
                problems.append("\"\(binding.hotkey)\" is not a shortcut Pulse understands.")
                continue
            }
            let url = URL(fileURLWithPath: binding.path)
            let result = hotKey.bind(id: Self.appIDBase + UInt32(position),
                                     keyCode: combo.keyCode, modifiers: combo.modifiers) {
                LauncherApps.toggle(url)
            }
            if result != noErr {
                problems.append("\(binding.hotkey) for \(url.deletingPathExtension().lastPathComponent) is taken.")
            }
        }
        for (position, command) in config.commands.enumerated() where !command.hotkey.isEmpty {
            guard let combo = LauncherKeyCombo.parse(command.hotkey) else {
                problems.append("\"\(command.hotkey)\" is not a shortcut Pulse understands.")
                continue
            }
            let result = hotKey.bind(id: Self.commandIDBase + UInt32(position),
                                     keyCode: combo.keyCode, modifiers: combo.modifiers) { [weak self] in
                self?.runFromHotkey(command)
            }
            if result != noErr {
                problems.append("\(command.hotkey) for \(command.name) is taken.")
            }
        }
        return problems
    }

    /// A command hotkey opens the panel on the command's output.
    private func runFromHotkey(_ command: LauncherCommand) {
        show()
        model.runCommand(command)
    }

    // MARK: - Panel

    private func toggle() {
        if panel?.isVisible == true { hide() } else { show() }
    }

    private func show() {
        let panel = self.panel ?? makePanel()
        self.panel = panel
        let frontmost = NSWorkspace.shared.frontmostApplication
        let ownPID = ProcessInfo.processInfo.processIdentifier
        model.willShow(previousApp: frontmost?.processIdentifier == ownPID ? nil : frontmost)
        let screen = NSScreen.screens.first { $0.frame.contains(NSEvent.mouseLocation) } ?? NSScreen.main
        if let area = screen?.visibleFrame {
            let size = LauncherView.size
            panel.setFrameOrigin(NSPoint(x: area.midX - size.width / 2,
                                         y: area.maxY - area.height / 3 - size.height / 2))
        }
        panel.makeKeyAndOrderFront(nil)
        installKeyMonitor()
    }

    private func hide() {
        removeKeyMonitor()
        model.didHide()
        panel?.orderOut(nil)
    }

    private func makePanel() -> LauncherPanel {
        let size = LauncherView.size
        let panel = LauncherPanel(
            contentRect: NSRect(origin: .zero, size: size),
            styleMask: [.borderless, .nonactivatingPanel],
            backing: .buffered, defer: false)
        panel.isFloatingPanel = true
        panel.level = .floating
        panel.isOpaque = false
        panel.backgroundColor = .clear
        panel.hasShadow = true
        panel.hidesOnDeactivate = false
        panel.isReleasedWhenClosed = false
        panel.collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary, .transient]
        panel.appearance = NSAppearance(named: .darkAqua)
        let host = NSHostingView(rootView: LauncherView(model: model))
        host.frame = NSRect(origin: .zero, size: size)
        panel.contentView = host
        // Click outside, or switching away, closes it.
        panel.onResignKey = { [weak self] in self?.hide() }
        return panel
    }

    // MARK: - Keys

    private func installKeyMonitor() {
        guard keyMonitor == nil else { return }
        keyMonitor = NSEvent.addLocalMonitorForEvents(matching: .keyDown) { [weak self] event in
            guard let self, self.panel?.isKeyWindow == true else { return event }
            return self.handle(event) ? nil : event
        }
    }

    private func removeKeyMonitor() {
        if let keyMonitor { NSEvent.removeMonitor(keyMonitor) }
        keyMonitor = nil
    }

    private func handle(_ event: NSEvent) -> Bool {
        switch Int(event.keyCode) {
        case kVK_Escape:
            if model.output != nil {
                model.clearOutput()
            } else {
                hide()
            }
            return true
        case kVK_DownArrow:
            model.move(1)
            return true
        case kVK_UpArrow:
            model.move(-1)
            return true
        case kVK_Return, kVK_ANSI_KeypadEnter:
            model.activate()
            return true
        default:
            break
        }
        guard event.modifierFlags.intersection(.deviceIndependentFlagsMask) == .command,
              let key = event.charactersIgnoringModifiers?.lowercased() else { return false }
        if let digit = Int(key), (1...9).contains(digit) {
            model.activate(digit - 1)
            return true
        }
        switch key {
        case "p":
            model.togglePinSelected()
            return true
        case "q":
            model.quitSelected()
            return true
        default:
            return false
        }
    }
}
