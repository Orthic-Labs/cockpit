import AppKit
import ApplicationServices
import SwiftUI

/// A panel that never takes focus from the app in front, so the wheel opens and closes
/// without Pulse appearing in the Dock or stealing a keystroke. Escape is read by a
/// global monitor instead of a key window.
private final class WheelPanel: NSPanel {
    override var canBecomeKey: Bool { false }
    override var canBecomeMain: Bool { false }
}

/// Pulse fork: a middle-click radial wheel over the same action list as the Tools cell
/// (`ToolKit`). The middle button is taken from the app under the pointer only when the
/// pointer is not on a link (a browser then opens it in a new tab as usual). Needs the
/// Accessibility permission for the event tap; without it nothing is intercepted and the
/// Tools card says so. Switched by `Preferences.toolWheelEnabled`.
@MainActor
final class ToolWheel {
    private static let size: CGFloat = 240

    private let preferences: Preferences
    private let hub = EventTapHub(location: .cgSessionEventTap, events: [.otherMouseDown, .otherMouseUp])
    private var token: EventTapToken?
    private var recheck: Timer?
    private var panel: WheelPanel?
    private var globalMonitor: Any?
    private var keyMonitor: Any?
    private var idleTimer: Timer?

    init(preferences: Preferences) {
        self.preferences = preferences
    }

    /// Starts the tap when Accessibility is granted, and keeps looking every few
    /// seconds until it is, as the other key conveniences do.
    func start() {
        guard recheck == nil else { return }
        attach()
        recheck = Timer.scheduledTimer(withTimeInterval: 3, repeats: true) { [weak self] _ in
            MainActor.assumeIsolated { self?.attach() }
        }
    }

    private func attach() {
        guard preferences.toolWheelEnabled else {
            detach()
            return
        }
        guard token == nil, AXIsProcessTrusted(), hub.start() else { return }
        // The pressed centre button, so its release is swallowed with it.
        let pressed = PressedFlag()
        token = hub.register(priority: 40) { [weak self] type, event in
            let button = event.getIntegerValueField(.mouseEventButtonNumber)
            guard button == 2 else { return .pass }
            if type == .otherMouseUp { return pressed.clear() ? .swallow : .pass }
            guard type == .otherMouseDown else { return .pass }
            if ToolWheel.isOnLink(at: event.location) { return .pass }
            pressed.set()
            let point = event.location
            DispatchQueue.main.async {
                MainActor.assumeIsolated { self?.open(atEventPoint: point) }
            }
            return .swallow
        }
    }

    private func detach() {
        if let token { hub.unregister(token) }
        token = nil
        hub.stop()
        close()
    }

    /// A link under the pointer, or within three parents of it.
    private nonisolated static func isOnLink(at location: CGPoint) -> Bool {
        guard var element = AX.elementAt(location) else { return false }
        for _ in 0...3 {
            if AX.string(element, "AXRole") == "AXLink" { return true }
            if let url = AX.attribute(element, "AXURL") {
                if let url = url as? URL, !url.absoluteString.isEmpty { return true }
                if let text = url as? String, !text.isEmpty { return true }
            }
            guard let parent = AX.element(element, "AXParent") else { return false }
            element = parent
        }
        return false
    }

    // MARK: - The panel

    private func open(atEventPoint point: CGPoint) {
        close()
        // Event locations are in global display coordinates with the origin at the top left
        // of the primary screen; AppKit's are bottom left.
        let primaryHeight = NSScreen.screens.first?.frame.height ?? 0
        let pointer = NSPoint(x: point.x, y: primaryHeight - point.y)
        let screen = NSScreen.screens.first { $0.frame.contains(pointer) } ?? NSScreen.main
        var origin = NSPoint(x: pointer.x - Self.size / 2, y: pointer.y - Self.size / 2)
        if let frame = screen?.visibleFrame {
            origin.x = min(max(origin.x, frame.minX), frame.maxX - Self.size)
            origin.y = min(max(origin.y, frame.minY), frame.maxY - Self.size)
        }

        let panel = WheelPanel(contentRect: NSRect(origin: origin, size: NSSize(width: Self.size, height: Self.size)),
                               styleMask: [.borderless, .nonactivatingPanel], backing: .buffered, defer: false)
        panel.isOpaque = false
        panel.backgroundColor = .clear
        panel.hasShadow = false
        panel.level = .floating
        panel.collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary, .transient]
        panel.hidesOnDeactivate = false
        panel.contentView = NSHostingView(rootView: WheelView(
            tools: ToolKit.shared.tools.map { ($0.id, $0.title, $0.symbol, $0.enabled()) },
            choose: { [weak self] id in
                self?.close()
                ToolKit.shared.run(id)
            },
            touched: { [weak self] in self?.restartIdle() }))
        panel.orderFrontRegardless()
        self.panel = panel

        globalMonitor = NSEvent.addGlobalMonitorForEvents(
            matching: [.leftMouseDown, .rightMouseDown, .otherMouseDown]) { [weak self] _ in
            MainActor.assumeIsolated { self?.close() }
        }
        keyMonitor = NSEvent.addGlobalMonitorForEvents(matching: [.keyDown]) { [weak self] event in
            if event.keyCode == 53 { MainActor.assumeIsolated { self?.close() } }
        }
        restartIdle()
    }

    private func restartIdle() {
        idleTimer?.invalidate()
        idleTimer = Timer.scheduledTimer(withTimeInterval: 6, repeats: false) { [weak self] _ in
            MainActor.assumeIsolated { self?.close() }
        }
    }

    private func close() {
        idleTimer?.invalidate()
        idleTimer = nil
        if let globalMonitor { NSEvent.removeMonitor(globalMonitor) }
        if let keyMonitor { NSEvent.removeMonitor(keyMonitor) }
        globalMonitor = nil
        keyMonitor = nil
        panel?.orderOut(nil)
        panel = nil
    }
}

/// Whether the centre button's press was taken, shared with the tap thread.
private final class PressedFlag: @unchecked Sendable {
    private let lock = NSLock()
    private var value = false
    func set() { lock.lock(); value = true; lock.unlock() }
    /// Clears the flag and says whether it was set.
    func clear() -> Bool { lock.lock(); defer { lock.unlock() }; let was = value; value = false; return was }
}

/// A dark dial with one slice per tool; a dim slice is a tool that cannot run now.
private struct WheelView: View {
    let tools: [(id: String, title: String, symbol: String, enabled: Bool)]
    let choose: (String) -> Void
    let touched: () -> Void
    @State private var hovered: Int?

    private let outer: CGFloat = 116
    private let inner: CGFloat = 34

    var body: some View {
        ZStack {
            Circle().fill(Color.black.opacity(0.88))
            ForEach(Array(tools.enumerated()), id: \.offset) { index, tool in
                let mid = angle(index) + sweep / 2
                let radius = (outer + inner) / 2
                SliceShape(start: angle(index), end: angle(index) + sweep, inner: inner, outer: outer)
                    .fill(hovered == index && tool.enabled ? Color.white.opacity(0.22) : Color.white.opacity(0.06))
                    .overlay(
                        VStack(spacing: 2) {
                            Image(systemName: tool.symbol).font(.system(size: 17, weight: .medium))
                            Text(tool.title).font(.system(size: 9, weight: .medium)).lineLimit(1)
                        }
                        .foregroundStyle(Color.white.opacity(tool.enabled ? 1 : 0.35))
                        .frame(width: 62)
                        .offset(x: cos(mid) * radius, y: sin(mid) * radius))
            }
        }
        .frame(width: 240, height: 240)
        .contentShape(Circle())
        .onContinuousHover { phase in
            switch phase {
            case .active(let point):
                hovered = slice(at: point)
                touched()
            case .ended:
                hovered = nil
            }
        }
        .gesture(SpatialTapGesture().onEnded { value in
            if let index = slice(at: value.location), tools[index].enabled { choose(tools[index].id) }
        })
    }

    private var sweep: Double { 2 * .pi / Double(max(tools.count, 1)) }
    /// Slice 0 starts at the top and runs clockwise.
    private func angle(_ index: Int) -> Double { -.pi / 2 + Double(index) * sweep }

    private func slice(at point: CGPoint) -> Int? {
        let dx = Double(point.x - 120), dy = Double(point.y - 120)
        let distance = (dx * dx + dy * dy).squareRoot()
        guard distance >= Double(inner), distance <= Double(outer), !tools.isEmpty else { return nil }
        var turn = atan2(dy, dx) - (-.pi / 2)
        while turn < 0 { turn += 2 * .pi }
        return min(Int(turn / sweep), tools.count - 1)
    }
}

/// An annular wedge between two angles (radians, clockwise on screen).
private struct SliceShape: Shape {
    let start: Double
    let end: Double
    let inner: CGFloat
    let outer: CGFloat

    func path(in rect: CGRect) -> Path {
        let centre = CGPoint(x: rect.midX, y: rect.midY)
        let gap = 0.02
        var path = Path()
        path.addArc(center: centre, radius: outer, startAngle: .radians(start + gap),
                    endAngle: .radians(end - gap), clockwise: false)
        path.addArc(center: centre, radius: inner, startAngle: .radians(end - gap),
                    endAngle: .radians(start + gap), clockwise: true)
        path.closeSubpath()
        return path
    }
}
