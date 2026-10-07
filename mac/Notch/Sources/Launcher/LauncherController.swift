import AppKit
import Carbon.HIToolbox
import Combine
import SwiftUI

/// A panel that can take keyboard focus without activating the app, so the
/// launcher never puts Cockpit in the Dock or in front of the app you were in.
private final class LauncherPanel: NSPanel {
    var onResignKey: (() -> Void)?
    override var canBecomeKey: Bool { true }
    override var canBecomeMain: Bool { false }
    override func resignKey() {
        super.resignKey()
        onResignKey?()
    }
}

/// Owns the launcher: the hotkey, the panel, and the settings that switch it on.
@MainActor
final class LauncherController {
    private let preferences: Preferences
    private let hotKey = LauncherHotKey()
    private let model = LauncherModel()
    private var panel: LauncherPanel?
    private var keyMonitor: Any?
    private var cancellables = Set<AnyCancellable>()

    /// Why the hotkey is not working, when it is not. Shown in the hub.
    private(set) var status: String?

    init(preferences: Preferences, snapshots: @escaping () -> [ProviderSnapshot]) {
        self.preferences = preferences
        model.snapshots = snapshots
        model.dismiss = { [weak self] in self?.hide() }
        hotKey.onPress = { [weak self] in self?.toggle() }
    }

    func start() {
        Publishers.CombineLatest(preferences.$launcherEnabled, preferences.$launcherHotkey)
            .removeDuplicates { $0 == $1 }
            .sink { [weak self] enabled, choice in self?.apply(enabled: enabled, choice: choice) }
            .store(in: &cancellables)
    }

    private func apply(enabled: Bool, choice: LauncherHotkeyChoice) {
        if enabled {
            status = hotKey.register(choice)
        } else {
            hotKey.unregister()
            status = nil
            hide()
        }
    }

    // MARK: - Panel

    private func toggle() {
        if panel?.isVisible == true { hide() } else { show() }
    }

    private func show() {
        let panel = self.panel ?? makePanel()
        self.panel = panel
        model.willShow()
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
            hide()
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
        if event.modifierFlags.intersection(.deviceIndependentFlagsMask) == .command,
           let digit = event.charactersIgnoringModifiers.flatMap({ Int($0) }), (1...9).contains(digit) {
            model.activate(digit - 1)
            return true
        }
        return false
    }
}
