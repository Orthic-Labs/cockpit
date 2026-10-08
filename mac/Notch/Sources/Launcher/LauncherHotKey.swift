import Carbon.HIToolbox
import Foundation

/// Pulse launcher: which key combination opens it.
enum LauncherHotkeyChoice: String, CaseIterable {
    case optionSpace
    case commandSpace
    case controlSpace

    var keyCode: UInt32 { UInt32(kVK_Space) }

    var modifiers: UInt32 {
        switch self {
        case .optionSpace: return UInt32(optionKey)
        case .commandSpace: return UInt32(cmdKey)
        case .controlSpace: return UInt32(controlKey)
        }
    }

    var display: String {
        switch self {
        case .optionSpace: return "Option+Space"
        case .commandSpace: return "Command+Space"
        case .controlSpace: return "Control+Space"
        }
    }
}

/// A key combination typed as text, such as "cmd+opt+k" or "ctrl+shift+1".
/// At least one modifier is required, so a binding never takes plain typing.
struct LauncherKeyCombo: Equatable {
    let keyCode: UInt32
    let modifiers: UInt32

    static func parse(_ text: String) -> LauncherKeyCombo? {
        let parts = text.lowercased()
            .split(separator: "+")
            .map { $0.trimmingCharacters(in: .whitespaces) }
        guard parts.count >= 2, let key = parts.last, let code = keyCodes[key] else { return nil }
        var modifiers: UInt32 = 0
        for part in parts.dropLast() {
            switch part {
            case "cmd", "command": modifiers |= UInt32(cmdKey)
            case "opt", "option", "alt": modifiers |= UInt32(optionKey)
            case "ctrl", "control": modifiers |= UInt32(controlKey)
            case "shift": modifiers |= UInt32(shiftKey)
            default: return nil
            }
        }
        return LauncherKeyCombo(keyCode: code, modifiers: modifiers)
    }

    private static let keyCodes: [String: UInt32] = {
        var table: [String: UInt32] = [
            "space": UInt32(kVK_Space),
            "return": UInt32(kVK_Return),
            "tab": UInt32(kVK_Tab),
        ]
        let letters: [(String, Int)] = [
            ("a", kVK_ANSI_A), ("b", kVK_ANSI_B), ("c", kVK_ANSI_C), ("d", kVK_ANSI_D),
            ("e", kVK_ANSI_E), ("f", kVK_ANSI_F), ("g", kVK_ANSI_G), ("h", kVK_ANSI_H),
            ("i", kVK_ANSI_I), ("j", kVK_ANSI_J), ("k", kVK_ANSI_K), ("l", kVK_ANSI_L),
            ("m", kVK_ANSI_M), ("n", kVK_ANSI_N), ("o", kVK_ANSI_O), ("p", kVK_ANSI_P),
            ("q", kVK_ANSI_Q), ("r", kVK_ANSI_R), ("s", kVK_ANSI_S), ("t", kVK_ANSI_T),
            ("u", kVK_ANSI_U), ("v", kVK_ANSI_V), ("w", kVK_ANSI_W), ("x", kVK_ANSI_X),
            ("y", kVK_ANSI_Y), ("z", kVK_ANSI_Z),
            ("0", kVK_ANSI_0), ("1", kVK_ANSI_1), ("2", kVK_ANSI_2), ("3", kVK_ANSI_3),
            ("4", kVK_ANSI_4), ("5", kVK_ANSI_5), ("6", kVK_ANSI_6), ("7", kVK_ANSI_7),
            ("8", kVK_ANSI_8), ("9", kVK_ANSI_9),
        ]
        for (name, code) in letters { table[name] = UInt32(code) }
        return table
    }()
}

/// Carbon hotkeys: one event handler, any number of bindings by id. Needs no
/// permission and no event tap.
final class LauncherHotKey {
    private var actions: [UInt32: () -> Void] = [:]
    private var refs: [UInt32: EventHotKeyRef] = [:]
    private var handlerRef: EventHandlerRef?

    /// Binds a combination to an id, replacing any earlier binding with that id.
    /// Returns noErr on success, otherwise the Carbon error.
    func bind(id: UInt32, keyCode: UInt32, modifiers: UInt32, _ action: @escaping () -> Void) -> OSStatus {
        unbind(id)
        let installed = installHandler()
        guard installed == noErr else { return installed }
        var ref: EventHotKeyRef?
        let hotKeyID = EventHotKeyID(signature: OSType(0x434B4C4E), id: id)
        let status = RegisterEventHotKey(keyCode, modifiers, hotKeyID, GetApplicationEventTarget(), 0, &ref)
        if status == noErr, let ref {
            refs[id] = ref
            actions[id] = action
        }
        return status
    }

    func unbind(_ id: UInt32) {
        if let ref = refs.removeValue(forKey: id) { UnregisterEventHotKey(ref) }
        actions[id] = nil
    }

    /// Removes every binding whose id is at or above `start`.
    func unbind(from start: UInt32) {
        for id in Array(Set(refs.keys).union(actions.keys)) where id >= start {
            unbind(id)
        }
    }

    private func installHandler() -> OSStatus {
        if handlerRef != nil { return noErr }
        var spec = EventTypeSpec(eventClass: OSType(kEventClassKeyboard),
                                 eventKind: UInt32(kEventHotKeyPressed))
        return InstallEventHandler(
            GetApplicationEventTarget(),
            { _, event, userData in
                guard let event, let userData else { return noErr }
                var hotKeyID = EventHotKeyID()
                let read = GetEventParameter(event, EventParamName(kEventParamDirectObject),
                                             EventParamType(typeEventHotKeyID), nil,
                                             MemoryLayout<EventHotKeyID>.size, nil, &hotKeyID)
                guard read == noErr else { return noErr }
                let me = Unmanaged<LauncherHotKey>.fromOpaque(userData).takeUnretainedValue()
                let id = hotKeyID.id
                DispatchQueue.main.async { me.fire(id) }
                return noErr
            },
            1, &spec, Unmanaged.passUnretained(self).toOpaque(), &handlerRef)
    }

    private func fire(_ id: UInt32) {
        actions[id]?()
    }

    deinit {
        for ref in refs.values { UnregisterEventHotKey(ref) }
        if let handlerRef { RemoveEventHandler(handlerRef) }
    }
}
