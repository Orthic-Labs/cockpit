import Carbon.HIToolbox
import Combine
import Foundation

/// Four-character signature on every window hotkey, so the shared Carbon
/// handler ignores hotkeys registered by anyone else.
private let windowHotKeySignature = OSType(0x50574D31) // "PWM1"

/// Pulse: the window-management shortcuts. Each enabled action gets one
/// Carbon hotkey through `RegisterEventHotKey`, the same mechanism as the
/// launcher; it needs no event tap and no permission to register. The action
/// itself needs Accessibility, which the hub reports.
///
/// Off until the hub's master toggle is on. A shortcut is text such as
/// `ctrl+opt+left` in `Preferences.windowHotkeys`, keyed by action id; a
/// missing key means the action's default, and "" means no shortcut.
@MainActor
final class WindowHotKeys {
    /// The hub changes one action with a "set" whose key is this prefix plus
    /// the action id, so two quick edits cannot overwrite each other.
    static let settingPrefix = "windowHotkey."

    /// Called after the registrations change, so the hub can show them.
    var onChange: (() -> Void)?

    private let preferences: Preferences
    private var registered: [EventHotKeyRef] = []
    private var handlerRef: EventHandlerRef?
    /// Why an action's shortcut is not working, when it is not.
    private var failures: [WindowAction: String] = [:]
    private var cancellables = Set<AnyCancellable>()

    init(preferences: Preferences) {
        self.preferences = preferences
    }

    func start() {
        Publishers.CombineLatest(preferences.$windowManagementEnabled, preferences.$windowHotkeys)
            .sink { [weak self] enabled, bindings in
                self?.apply(enabled: enabled, bindings: bindings)
            }
            .store(in: &cancellables)
    }

    func stop() {
        cancellables.removeAll()
        unregisterAll()
        failures = [:]
    }

    /// The shortcut text an action uses: its own choice, else its default.
    /// Empty when it has none.
    static func shortcut(for action: WindowAction, bindings: [String: String]) -> String {
        bindings[action.id] ?? action.defaultShortcut
    }

    /// Data for the hub: the master switch and each action with its shortcut.
    func snapshot() -> [String: Any] {
        let bindings = preferences.windowHotkeys
        let actions = WindowAction.allCases.map { action -> [String: Any] in
            let parsed = WindowShortcut(spec: Self.shortcut(for: action, bindings: bindings))
            return [
                "id": action.id,
                "title": action.title,
                "group": action.group.rawValue,
                "groupTitle": action.group.title,
                "shortcut": parsed?.spec ?? "",
                "display": parsed?.display ?? "",
                "default": action.defaultShortcut,
                "error": failures[action] ?? "",
            ]
        }
        return ["enabled": preferences.windowManagementEnabled, "actions": actions]
    }

    // MARK: - Registering

    private func apply(enabled: Bool, bindings: [String: String]) {
        unregisterAll()
        failures = [:]
        if enabled {
            registerAll(bindings: bindings)
        }
        onChange?()
    }

    private func registerAll(bindings: [String: String]) {
        let listening = installHandler()
        var taken: [String: WindowAction] = [:]
        for (index, action) in WindowAction.allCases.enumerated() {
            let text = Self.shortcut(for: action, bindings: bindings)
            guard !text.isEmpty else { continue }
            guard let shortcut = WindowShortcut(spec: text) else {
                failures[action] = "\"\(text)\" is not a shortcut Pulse knows."
                continue
            }
            if let other = taken[shortcut.spec] {
                failures[action] = "\(shortcut.display) is also used by \(other.title)."
                continue
            }
            taken[shortcut.spec] = action
            guard listening else {
                failures[action] = "Could not listen for keyboard shortcuts."
                continue
            }
            var ref: EventHotKeyRef?
            let id = EventHotKeyID(signature: windowHotKeySignature, id: UInt32(index + 1))
            let status = RegisterEventHotKey(shortcut.keyCode, shortcut.modifiers, id,
                                             GetApplicationEventTarget(), 0, &ref)
            if status == noErr, let ref {
                registered.append(ref)
            } else if status == OSStatus(eventHotKeyExistsErr) {
                failures[action] = "\(shortcut.display) is already taken by another app or by macOS."
            } else {
                failures[action] = "\(shortcut.display) could not be registered (error \(status))."
            }
        }
    }

    private func unregisterAll() {
        for ref in registered { UnregisterEventHotKey(ref) }
        registered.removeAll()
    }

    /// One handler serves every window hotkey. Installed once; returns false
    /// when the system refuses it.
    private func installHandler() -> Bool {
        if handlerRef != nil { return true }
        var spec = EventTypeSpec(eventClass: OSType(kEventClassKeyboard),
                                 eventKind: UInt32(kEventHotKeyPressed))
        let status = InstallEventHandler(
            GetApplicationEventTarget(),
            { _, event, userData in
                guard let userData else { return noErr }
                var hotKeyID = EventHotKeyID()
                let read = GetEventParameter(event, EventParamName(kEventParamDirectObject),
                                             EventParamType(typeEventHotKeyID), nil,
                                             MemoryLayout<EventHotKeyID>.size, nil, &hotKeyID)
                guard read == noErr, hotKeyID.signature == windowHotKeySignature else { return noErr }
                let me = Unmanaged<WindowHotKeys>.fromOpaque(userData).takeUnretainedValue()
                let id = hotKeyID.id
                DispatchQueue.main.async {
                    MainActor.assumeIsolated { me.pressed(id) }
                }
                return noErr
            },
            1, &spec, Unmanaged.passUnretained(self).toOpaque(), &handlerRef)
        return status == noErr
    }

    private func pressed(_ id: UInt32) {
        let actions = WindowAction.allCases
        guard id >= 1, Int(id) <= actions.count else { return }
        WindowActions.perform(actions[Int(id) - 1])
    }

    deinit {
        // The handler holds this object unretained; it must go with it.
        if let handlerRef { RemoveEventHandler(handlerRef) }
    }
}
