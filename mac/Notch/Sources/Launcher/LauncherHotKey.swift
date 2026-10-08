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

/// One system-wide hotkey through Carbon's `RegisterEventHotKey`. It needs no
/// permission and no event tap, which belongs to the conveniences.
final class LauncherHotKey {
    var onPress: (() -> Void)?
    private var hotKeyRef: EventHotKeyRef?
    private var handlerRef: EventHandlerRef?

    /// Returns nil on success, otherwise a sentence for the hub to show.
    func register(_ choice: LauncherHotkeyChoice) -> String? {
        unregister()
        if handlerRef == nil {
            var spec = EventTypeSpec(eventClass: OSType(kEventClassKeyboard),
                                     eventKind: UInt32(kEventHotKeyPressed))
            let status = InstallEventHandler(
                GetApplicationEventTarget(),
                { _, _, userData in
                    guard let userData else { return noErr }
                    let me = Unmanaged<LauncherHotKey>.fromOpaque(userData).takeUnretainedValue()
                    DispatchQueue.main.async { me.onPress?() }
                    return noErr
                },
                1, &spec, Unmanaged.passUnretained(self).toOpaque(), &handlerRef)
            if status != noErr { return "Could not listen for keyboard shortcuts (error \(status))." }
        }
        let id = EventHotKeyID(signature: OSType(0x434B4C4E), id: 1)
        let status = RegisterEventHotKey(choice.keyCode, choice.modifiers, id,
                                         GetApplicationEventTarget(), 0, &hotKeyRef)
        if status == noErr { return nil }
        hotKeyRef = nil
        if status == OSStatus(eventHotKeyExistsErr) {
            var text = "\(choice.display) is already taken by another app or by macOS."
            if choice == .commandSpace { text += " Turn off Spotlight's shortcut in System Settings first." }
            return text + " Pick a different shortcut."
        }
        return "\(choice.display) could not be registered (error \(status))."
    }

    func unregister() {
        if let hotKeyRef { UnregisterEventHotKey(hotKeyRef) }
        hotKeyRef = nil
    }

    deinit {
        unregister()
        if let handlerRef { RemoveEventHandler(handlerRef) }
    }
}
