import Carbon.HIToolbox
import Foundation

/// Pulse: one global shortcut, stored as text such as `ctrl+opt+left` so the
/// hub and the preferences file can read it. Modifiers come in the order
/// ctrl, opt, shift, cmd; at least one is required, so a bare key never
/// becomes a global hotkey. The hub's recorder writes the same names.
struct WindowShortcut: Equatable {
    let keyCode: UInt32
    let modifiers: UInt32
    /// Canonical text, e.g. `ctrl+opt+left`.
    let spec: String

    private static let modifierNames: [(name: String, flag: Int)] = [
        ("ctrl", controlKey), ("opt", optionKey), ("shift", shiftKey), ("cmd", cmdKey),
    ]

    /// Key names the hub can record and the shortcuts can use.
    private static let keyCodes: [String: Int] = {
        let codes: [String: Int] = [
            "a": kVK_ANSI_A, "b": kVK_ANSI_B, "c": kVK_ANSI_C, "d": kVK_ANSI_D, "e": kVK_ANSI_E,
            "f": kVK_ANSI_F, "g": kVK_ANSI_G, "h": kVK_ANSI_H, "i": kVK_ANSI_I, "j": kVK_ANSI_J,
            "k": kVK_ANSI_K, "l": kVK_ANSI_L, "m": kVK_ANSI_M, "n": kVK_ANSI_N, "o": kVK_ANSI_O,
            "p": kVK_ANSI_P, "q": kVK_ANSI_Q, "r": kVK_ANSI_R, "s": kVK_ANSI_S, "t": kVK_ANSI_T,
            "u": kVK_ANSI_U, "v": kVK_ANSI_V, "w": kVK_ANSI_W, "x": kVK_ANSI_X, "y": kVK_ANSI_Y,
            "z": kVK_ANSI_Z,
            "0": kVK_ANSI_0, "1": kVK_ANSI_1, "2": kVK_ANSI_2, "3": kVK_ANSI_3, "4": kVK_ANSI_4,
            "5": kVK_ANSI_5, "6": kVK_ANSI_6, "7": kVK_ANSI_7, "8": kVK_ANSI_8, "9": kVK_ANSI_9,
            "-": kVK_ANSI_Minus, "=": kVK_ANSI_Equal, "[": kVK_ANSI_LeftBracket,
            "]": kVK_ANSI_RightBracket, ",": kVK_ANSI_Comma, ".": kVK_ANSI_Period,
            "/": kVK_ANSI_Slash, ";": kVK_ANSI_Semicolon, "'": kVK_ANSI_Quote,
            "\\": kVK_ANSI_Backslash, "`": kVK_ANSI_Grave,
            "left": kVK_LeftArrow, "right": kVK_RightArrow, "up": kVK_UpArrow, "down": kVK_DownArrow,
            "return": kVK_Return, "space": kVK_Space, "delete": kVK_Delete, "tab": kVK_Tab,
        ]
        return codes
    }()

    /// Parses `ctrl+opt+left`. Nil for unknown key names, unknown modifiers,
    /// a missing key, or no modifier at all.
    init?(spec: String) {
        let parts = spec.lowercased().split(separator: "+").map(String.init)
        guard let key = parts.last, let code = Self.keyCodes[key] else { return nil }
        var flags = 0
        var names: [String] = []
        for part in parts.dropLast() {
            guard let modifier = Self.modifierNames.first(where: { $0.name == part }),
                  !names.contains(part)
            else { return nil }
            flags |= modifier.flag
            names.append(part)
        }
        guard !names.isEmpty else { return nil }
        // Canonical order, whatever order the text came in.
        let ordered = Self.modifierNames.filter { names.contains($0.name) }.map(\.name)
        self.keyCode = UInt32(code)
        self.modifiers = UInt32(flags)
        self.spec = (ordered + [key]).joined(separator: "+")
    }

    /// The symbols shown in the hub and the status, e.g. `⌃⌥←`.
    var display: String {
        let symbols = Self.modifierNames.filter { modifiers & UInt32($0.flag) != 0 }.map { modifierSymbol($0.name) }
        let key = spec.split(separator: "+").last.map(String.init) ?? ""
        return symbols.joined() + Self.keySymbol(key)
    }

    private func modifierSymbol(_ name: String) -> String {
        switch name {
        case "ctrl": return "⌃"
        case "opt": return "⌥"
        case "shift": return "⇧"
        default: return "⌘"
        }
    }

    private static func keySymbol(_ key: String) -> String {
        switch key {
        case "left": return "←"
        case "right": return "→"
        case "up": return "↑"
        case "down": return "↓"
        case "return": return "↩"
        case "delete": return "⌫"
        case "space": return "Space"
        case "tab": return "⇥"
        default: return key.uppercased()
        }
    }
}
