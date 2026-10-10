import AppKit
import ApplicationServices
import Foundation

/// Pulse fork: one action list for the Tools cell's hover card and the
/// middle-click wheel, so both always offer the same utilities. A tool is
/// live or dimmed according to `enabled`; running a dimmed tool does nothing.
struct Tool {
    let id: String
    let title: String
    let symbol: String
    let help: String
    let enabled: () -> Bool
    let run: @MainActor () -> Void
}

@MainActor
final class ToolKit {
    static let shared = ToolKit()

    /// The Tools cell has something new to show.
    var onChange: (() -> Void)?
    private(set) var lastRun: Date?

    var tools: [Tool] {
        [
            Tool(id: "snip", title: "Snip", symbol: "rectangle.dashed",
                 help: L10n.t("Drag a region; it is sent and lands on the other clipboard"),
                 enabled: { NearbySharing.shared.devices.isEmpty == false },
                 run: { NearbySharing.shared.screenshot(.snip) }),
            Tool(id: "screen", title: "Screen", symbol: "rectangle.inset.filled",
                 help: L10n.t("The whole main display, sent to the other clipboard"),
                 enabled: { NearbySharing.shared.devices.isEmpty == false },
                 run: { NearbySharing.shared.screenshot(.screen) }),
            Tool(id: "window", title: "Window", symbol: "macwindow",
                 help: L10n.t("Click a window; it is sent to the other clipboard"),
                 enabled: { NearbySharing.shared.devices.isEmpty == false },
                 run: { NearbySharing.shared.screenshot(.window) }),
            Tool(id: "paste", title: "Paste to other", symbol: "arrow.up.doc.on.clipboard",
                 help: L10n.t("Send the clipboard: files, an image or text"),
                 enabled: { !NearbySharing.shared.devices.isEmpty && NearbySharing.shared.clipboardHasContent },
                 run: { NearbySharing.shared.pasteClipboard() }),
            Tool(id: "copylast", title: "Copy last", symbol: "doc.on.clipboard",
                 help: L10n.t("Put the last received text or files on the clipboard"),
                 enabled: { NearbySharing.shared.hasLast },
                 run: { NearbySharing.shared.copyLast() }),
            Tool(id: "lock", title: "Lock screen", symbol: "lock",
                 help: L10n.t("Lock the screen"),
                 enabled: { true },
                 run: { ToolKit.lockScreen() }),
        ]
    }

    /// Runs the tool with this id when it is live.
    func run(_ id: String) {
        guard let tool = tools.first(where: { $0.id == id }), tool.enabled() else { return }
        tool.run()
        lastRun = Date()
        onChange?()
    }

    private static let sessionTool =
        "/System/Library/CoreServices/Menu Extras/User.menu/Contents/Resources/CGSession"

    private static func lockScreen() {
        guard FileManager.default.isExecutableFile(atPath: sessionTool) else {
            NearbySharing.shared.showNote(title: L10n.t("Couldn't lock the screen"),
                                          detail: L10n.t("macOS has no lock helper on this Mac."),
                                          problem: true)
            return
        }
        let process = Process()
        process.executableURL = URL(fileURLWithPath: sessionTool)
        process.arguments = ["-suspend"]
        try? process.run()
    }

    /// The cell: one row per tool (its state in `detail`, so the snapshot changes when a
    /// tool goes live or dim), then a hint about the middle-click wheel.
    func providerSnapshot() -> ProviderSnapshot {
        var windows = tools.map {
            LimitWindow(id: "tool:" + $0.id, label: L10n.t($0.title), detail: $0.enabled() ? "on" : "off")
        }
        windows.append(LimitWindow(
            id: "hint-wheel", label: "",
            detail: AXIsProcessTrusted() ? L10n.t("Middle-click anywhere for the wheel")
                                         : L10n.t("Allow Accessibility for the middle-click wheel")))
        return ProviderSnapshot(id: SystemProviders.toolsID, displayName: L10n.t("Tools"), glyph: .tools,
                                fidelity: .official, status: .ok, windows: windows,
                                headlineID: nil, kind: .system)
    }
}
