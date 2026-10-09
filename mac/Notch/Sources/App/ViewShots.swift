import AppKit
import SwiftUI

// Off-screen renderer for the notch's views (CI only).
//
// `PULSE_RENDER_VIEWS=1 PULSE_VIEWS_JSON=qa/notch-views.json PULSE_VIEW_SHOTS=<dir> Pulse`
// builds every view listed in qa/notch-views.json from its platform-neutral
// fixture, renders it with ImageRenderer at scale 2 on the entry's backdrop
// colour and writes <id>.png. It is run by scripts/gate.sh on the macOS leg and
// never by the shipping app: nothing here is reachable without that variable.

// MARK: - Still rendering

private struct ViewShotStillKey: EnvironmentKey {
    static let defaultValue = false
}

extension EnvironmentValues {
    /// True only while `ViewShots` renders. `ImageRenderer` cannot draw AppKit-backed
    /// controls (`ProgressView`, `ScrollView`), so the cards ask for a drawn stand-in.
    var viewShotStill: Bool {
        get { self[ViewShotStillKey.self] }
        set { self[ViewShotStillKey.self] = newValue }
    }
}

/// A card's progress indicator: the system's own at runtime, a drawn one off-screen.
struct CardProgress: View {
    var value: Double? = nil
    var linear: Bool = true
    @Environment(\.viewShotStill) private var still

    var body: some View {
        if still {
            drawn
        } else if linear {
            Group {
                if let value {
                    ProgressView(value: value)
                } else {
                    ProgressView()
                }
            }
            .progressViewStyle(.linear)
            .tint(Palette.textPrimary)
        } else {
            ProgressView().controlSize(.small).tint(Palette.textPrimary)
        }
    }

    @ViewBuilder private var drawn: some View {
        if linear {
            GeometryReader { proxy in
                ZStack(alignment: .leading) {
                    Capsule().fill(Palette.textPrimary.opacity(0.2))
                    Capsule()
                        .fill(Palette.textPrimary)
                        .frame(width: proxy.size.width * CGFloat(min(max(value ?? 0.35, 0), 1)))
                }
            }
            .frame(height: 4)
        } else {
            Circle()
                .trim(from: 0, to: 0.75)
                .stroke(Palette.textPrimary, style: StrokeStyle(lineWidth: 2, lineCap: .round))
                .frame(width: 14, height: 14)
        }
    }
}

/// A file's Finder icon: drawn live at runtime, a bitmap taken beforehand off-screen. An
/// app's icon drawn lazily inside `ImageRenderer` washes out the whole render (every card
/// with an app icon came out dimmed), so the still render rasterises it first.
struct CardIcon: View {
    let path: String
    @Environment(\.viewShotStill) private var still

    var body: some View {
        Image(nsImage: still ? Self.bitmap(path) : NSWorkspace.shared.icon(forFile: path))
            .resizable()
            .interpolation(.high)
    }

    private static func bitmap(_ path: String) -> NSImage {
        let icon = NSWorkspace.shared.icon(forFile: path)
        let side = 512
        guard let rep = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: side, pixelsHigh: side,
                                         bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true,
                                         isPlanar: false, colorSpaceName: .calibratedRGB,
                                         bytesPerRow: 0, bitsPerPixel: 0),
              let context = NSGraphicsContext(bitmapImageRep: rep)
        else { return icon }
        NSGraphicsContext.saveGraphicsState()
        NSGraphicsContext.current = context
        icon.draw(in: NSRect(x: 0, y: 0, width: side, height: side), from: .zero,
                  operation: .sourceOver, fraction: 1)
        NSGraphicsContext.restoreGraphicsState()
        let image = NSImage(size: NSSize(width: side, height: side))
        image.addRepresentation(rep)
        return image
    }
}

/// A card's vertical list: a scroll view at runtime, the plain stack off-screen.
struct CardScroll<Content: View>: View {
    private let content: Content
    @Environment(\.viewShotStill) private var still

    init(@ViewBuilder content: () -> Content) {
        self.content = content()
    }

    var body: some View {
        if still {
            content.frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
        } else {
            ScrollView(.vertical, showsIndicators: false) { content }
        }
    }
}

// MARK: - Fixture access

private struct Fx {
    let d: [String: Any]
    init(_ d: [String: Any]) { self.d = d }

    func s(_ key: String) -> String? { d[key] as? String }
    func n(_ key: String) -> Double? { (d[key] as? NSNumber)?.doubleValue }
    func b(_ key: String) -> Bool { (d[key] as? NSNumber)?.boolValue ?? false }
    func o(_ key: String) -> Fx? { (d[key] as? [String: Any]).map { Fx($0) } }
    func a(_ key: String) -> [Fx] {
        ((d[key] as? [[String: Any]]) ?? []).map { Fx($0) }
    }
}

// MARK: - The renderer

@MainActor
enum ViewShots {
    nonisolated static var requested: Bool {
        ProcessInfo.processInfo.environment["PULSE_RENDER_VIEWS"] == "1"
    }

    /// Renders every view in the list, then ends the process.
    static func runAndExit() -> Never {
        L10n.testLocale = Locale(identifier: "en")
        NSApp.appearance = NSAppearance(named: .darkAqua)

        let env = ProcessInfo.processInfo.environment
        let out = URL(fileURLWithPath: env["PULSE_VIEW_SHOTS"] ?? "view-shots", isDirectory: true)
        guard let path = env["PULSE_VIEWS_JSON"],
              let data = FileManager.default.contents(atPath: path),
              let list = (try? JSONSerialization.jsonObject(with: data)) as? [[String: Any]]
        else {
            fputs("view-shots: PULSE_VIEWS_JSON is missing or unreadable\n", stderr)
            exit(2)
        }
        try? FileManager.default.createDirectory(at: out, withIntermediateDirectories: true)

        let now = Date()
        var failures = 0
        var written = 0
        for entry in list {
            guard let id = entry["id"] as? String,
                  let fixture = entry["fixture"] as? [String: Any] else {
                fputs("view-shots: an entry has no id or fixture\n", stderr)
                failures += 1
                continue
            }
            let backdrop = Self.backdrop(entry["backdrop"] as? String)
            guard let built = Self.build(Fx(fixture), now: now),
                  let png = Self.render(built.view, backdrop: backdrop, crop: built.crop)
            else {
                fputs("view-shots: could not render \(id)\n", stderr)
                failures += 1
                continue
            }
            do {
                try png.write(to: out.appendingPathComponent("\(id).png"))
                written += 1
            } catch {
                fputs("view-shots: could not write \(id).png: \(error)\n", stderr)
                failures += 1
            }
        }
        print("view-shots: wrote \(written) of \(list.count) views to \(out.path)")
        exit(failures == 0 ? 0 : 1)
    }

    // MARK: Rendering

    private static func backdrop(_ hex: String?) -> NSColor {
        let digits = (hex ?? "#000000").replacingOccurrences(of: "#", with: "")
        return NSColor(hex: UInt32(digits, radix: 16) ?? 0)
    }

    private static func render(_ view: AnyView, backdrop: NSColor, crop: Bool) -> Data? {
        let styled = view
            .environment(\.viewShotStill, true)
            .environment(\.isEnabled, true)
            .environment(\.colorScheme, .dark)
            .environment(\.codenotchAccentColor, AccentColorChoice.green.color)
            .environment(\.notchSurfaceStyle, .solid)
            .environment(\.tooltipSecondaryInk, Palette.textSecondary)
            .environment(\.weeklyRingDashed, false)
            .environment(\.usageWatchLimit, 0.70)
            .environment(\.usageCriticalLimit, 0.90)
            .environment(\.colorTransitionStyle, .hardStep)
        let scale: CGFloat = 2
        if crop {
            // Render on transparency, crop to what was actually drawn, then lay the
            // result on the backdrop with a uniform margin.
            let renderer = ImageRenderer(content: styled)
            renderer.scale = scale
            renderer.isOpaque = false
            guard let image = renderer.cgImage,
                  let out = Self.composited(image, backdrop: backdrop, margin: Int(16 * scale))
            else { return nil }
            return NSBitmapImageRep(cgImage: out).representation(using: .png, properties: [:])
        }
        let renderer = ImageRenderer(content: styled.padding(24).background(Color(nsColor: backdrop)))
        renderer.scale = scale
        guard let image = renderer.cgImage else { return nil }
        return NSBitmapImageRep(cgImage: image).representation(using: .png, properties: [:])
    }

    /// Crops a transparent render to the bounding box of pixels with alpha > 8, then
    /// draws it on the backdrop with `margin` pixels on every side. Falls back to the
    /// whole image when nothing is opaque enough to measure.
    private static func composited(_ image: CGImage, backdrop: NSColor, margin: Int) -> CGImage? {
        let width = image.width, height = image.height
        guard let space = CGColorSpace(name: CGColorSpace.sRGB),
              let probe = CGContext(data: nil, width: width, height: height, bitsPerComponent: 8,
                                    bytesPerRow: width * 4, space: space,
                                    bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue),
              let raw = probe.data
        else { return nil }
        probe.clear(CGRect(x: 0, y: 0, width: width, height: height))
        probe.draw(image, in: CGRect(x: 0, y: 0, width: width, height: height))
        let pixels = raw.assumingMemoryBound(to: UInt8.self)

        var minX = width, minY = height, maxX = -1, maxY = -1
        for y in 0..<height {
            for x in 0..<width where pixels[(y * width + x) * 4 + 3] > 8 {
                if x < minX { minX = x }
                if x > maxX { maxX = x }
                if y < minY { minY = y }
                if y > maxY { maxY = y }
            }
        }
        var content = image
        if maxX >= minX, maxY >= minY {
            let rect = CGRect(x: minX, y: minY, width: maxX - minX + 1, height: maxY - minY + 1)
            content = image.cropping(to: rect) ?? image
        }

        let outW = content.width + margin * 2, outH = content.height + margin * 2
        guard let canvas = CGContext(data: nil, width: outW, height: outH, bitsPerComponent: 8,
                                     bytesPerRow: 0, space: space,
                                     bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)
        else { return nil }
        let fill = backdrop.usingColorSpace(.sRGB) ?? backdrop
        canvas.setFillColor(red: fill.redComponent, green: fill.greenComponent,
                            blue: fill.blueComponent, alpha: 1)
        canvas.fill(CGRect(x: 0, y: 0, width: outW, height: outH))
        canvas.draw(content, in: CGRect(x: margin, y: margin,
                                        width: content.width, height: content.height))
        return canvas.makeImage()
    }

    // MARK: Building views from fixtures

    private static func build(_ fx: Fx, now: Date) -> (view: AnyView, crop: Bool)? {
        switch fx.s("kind") {
        case "ring":
            guard let cell = fx.o("cell") else { return nil }
            let weekly = WeeklyRing(rawValue: fx.s("weeklyRing") ?? "inside") ?? .inside
            let view = ProviderCell(snapshot: snapshot(cell, now: now),
                                    activity: ActivitySummary(sessions: sessions(cell, now: now)),
                                    isRefreshing: false,
                                    weeklyRing: weekly,
                                    showsWeeklyReading: false,
                                    showsReading: false)
            return (AnyView(view), false)

        case "tooltip":
            guard let cell = fx.o("cell") else { return nil }
            let view = TooltipCard(snapshot: snapshot(cell, now: now),
                                   activity: ActivitySummary(sessions: sessions(cell, now: now)),
                                   now: now,
                                   direction: .leading)
            return (AnyView(view), false)

        case "card":
            let view = DiskImageCard(prompt: prompt(fx), direction: .leading, tailOffset: 0, onChoice: nil)
            return (AnyView(view), false)

        case "update":
            let phase: UpdatePrompt.Phase
            switch fx.s("phase") {
            case "downloading": phase = .downloading(fx.n("progress"))
            case "extracting": phase = .extracting(fx.n("progress") ?? 0)
            case "installing": phase = .installing
            case "restart": phase = .restart
            default: phase = .available
            }
            let prompt = UpdatePrompt(version: fx.s("version") ?? "0.0.0",
                                      notes: fx.s("notes") ?? "", phase: phase)
            return (AnyView(UpdateCard(prompt: prompt, direction: .leading, tailOffset: 0, onChoice: nil)), false)

        case "alert":
            let kind: UsageAlertKind
            switch fx.s("alertKind") {
            case "sessionLimitReached": kind = .sessionLimitReached
            case "weeklyLimitReached": kind = .weeklyLimitReached
            default: kind = .reset
            }
            var event = UsageAlertEvent(
                kind: kind,
                providerID: (fx.s("provider") ?? "claude").lowercased(),
                providerName: fx.s("provider") ?? "Claude",
                windowLabel: fx.s("window") ?? "Current session",
                glyph: glyphs[fx.s("glyph") ?? ""] ?? .claude,
                previousFraction: kind == .reset ? 1 : 0.9,
                currentFraction: kind == .reset ? 0 : 1,
                resetsAt: fx.n("resetsInMinutes").map { now.addingTimeInterval($0 * 60) })
            event.noticeTitle = fx.s("noticeTitle")
            event.noticeSubtitle = fx.s("noticeSubtitle")
            event.noticeStatus = fx.s("noticeStatus")
            return (AnyView(UsageResetCard(event: event, direction: .leading, tailOffset: 0, onDismiss: {})), false)

        case "menu":
            let items = fx.a("items").map { (title: $0.s("title") ?? "", shortcut: $0.s("shortcut") ?? "") }
            return (AnyView(MenuFacsimile(items: items)), false)

        case "notch":
            return (notch(fx, now: now), true)

        default:
            return nil
        }
    }

    /// The real root view over a model in the given state, as the panel hosts it.
    private static func notch(_ fx: Fx, now: Date) -> AnyView {
        let model = NotchViewModel()
        model.edge = NotchEdge(rawValue: fx.s("edge") ?? "right") ?? .right
        model.surfaceStyle = .solid
        model.weeklyRing = .inside
        model.accentColor = .green
        model.now = now

        let cells = fx.a("cells")
        model.snapshots = cells.map { snapshot($0, now: now) }
        var byProvider: [String: [AgentSession]] = [:]
        for cell in cells {
            let list = sessions(cell, now: now)
            if !list.isEmpty { byProvider[cell.s("id") ?? ""] = list }
        }
        model.sessions = byProvider
        model.isExpanded = fx.b("expanded")
        if let hover = fx.s("hover") {
            model.hoveredIndex = model.snapshots.firstIndex { $0.id == hover }
        }
        if let badges = fx.o("badges") {
            model.updatePending = badges.b("update")
            model.permissionsPending = badges.b("permissions")
        }
        let size = model.panelSize
        return AnyView(NotchRootView(model: model).frame(width: size.width, height: size.height))
    }

    // MARK: Cells

    private static let glyphs: [String: ProviderGlyph] = [
        "claude": .claude, "openai": .openai, "cpu": .cpu,
        "memory": .memory, "disk": .disk, "send": .send,
    ]

    private static func band(_ name: String?) -> UsageBand? {
        name.flatMap { UsageBand(rawValue: $0) }
    }

    private static func window(_ w: Fx, now: Date) -> LimitWindow {
        var window = LimitWindow(
            id: w.s("id") ?? UUID().uuidString,
            group: w.s("group"),
            label: w.s("label") ?? "",
            usedFraction: w.n("used"),
            usedText: w.s("usedText"),
            detail: w.s("detail"),
            resetsAt: w.n("resetsInMinutes").map { now.addingTimeInterval($0 * 60) },
            bandOverride: band(w.s("band")),
            prefersUsedText: w.b("prefersUsedText"))
        window.trailingText = w.s("trailing")
        return window
    }

    private static func snapshot(_ c: Fx, now: Date) -> ProviderSnapshot {
        let status: ProviderStatus
        switch c.s("status") {
        case "stale":
            status = .stale(since: now.addingTimeInterval(-(c.n("staleMinutes") ?? 30) * 60))
        case "needs-auth": status = .needsAuth
        case "access-denied": status = .accessDenied
        case "error": status = .error(c.s("message") ?? "network unreachable")
        default: status = .ok
        }
        var block: UsageBlock?
        if let b = c.o("block") {
            block = UsageBlock(reason: b.s("reason") ?? "Limit reached",
                               resetsAt: b.n("resetsInMinutes").map { now.addingTimeInterval($0 * 60) })
        }
        let id = c.s("id") ?? "cell"
        return ProviderSnapshot(
            id: id,
            displayName: c.s("name") ?? "Provider",
            glyph: glyphs[c.s("glyph") ?? ""] ?? .claude,
            fidelity: .official,
            status: status,
            windows: c.a("windows").map { window($0, now: now) },
            headlineID: c.s("headline"),
            weeklyID: c.s("weekly"),
            block: block,
            kind: SystemProviders.isSystem(providerID: id) ? .system : .usage,
            plan: c.s("plan"),
            headerAccessory: c.s("headerNote"))
    }

    private static func sessions(_ c: Fx, now: Date) -> [AgentSession] {
        var result: [AgentSession] = []
        for (index, s) in c.a("sessions").enumerated() {
            let state: AgentSession.State
            switch s.s("state") {
            case "waiting": state = .waiting
            case "success": state = .success
            case "idle": state = .idle
            default: state = .busy
            }
            result.append(AgentSession(
                id: "session-\(index)",
                name: s.s("name") ?? "Session",
                detail: s.s("detail") ?? "",
                state: state,
                waitingFor: s.s("waitingFor"),
                since: now.addingTimeInterval(-(s.n("sinceMinutes") ?? 1) * 60)))
        }
        return result
    }

    // MARK: Cards

    private static func icon(_ name: String?) -> String {
        switch name {
        case "app": return "/Applications/Safari.app"
        case "messages": return "/System/Applications/Messages.app"
        case "installer": return "/System/Library/CoreServices/Installer.app"
        default: return "/Applications"
        }
    }

    private static func choice(_ name: String?) -> DiskImageChoice {
        switch name {
        case "replace": return .replace
        case "quitAndUpdate": return .quitAndUpdate
        case "undo": return .undo
        case "openInstaller": return .openInstaller
        case "showImage": return .showImage
        case "cancel": return .cancel
        case "dismiss": return .dismiss
        case "refresh": return .refresh
        case "copyText": return .copyText
        case "openLink": return .openLink
        default: return .install
        }
    }

    private static func prompt(_ v: Fx) -> DiskImagePrompt {
        let style: DiskImagePrompt.Style
        switch v.s("style") {
        case "working": style = .working
        case "done": style = .done
        case "problem": style = .problem
        default: style = .ask
        }
        func button(_ b: Fx?) -> DiskImagePrompt.Button? {
            guard let b else { return nil }
            return DiskImagePrompt.Button(choice: choice(b.s("choice")), label: b.s("label") ?? "")
        }
        var send: SendCardContent?
        if let s = v.o("send") {
            var transfer: SendCardContent.Transfer?
            if let t = s.o("transfer") {
                let state: SendCardContent.Transfer.State
                switch t.s("state") {
                case "active": state = .active
                case "done": state = .done
                case "problem": state = .problem
                default: state = .waiting
                }
                transfer = SendCardContent.Transfer(state: state,
                                                    symbol: t.s("symbol") ?? "display",
                                                    fraction: t.n("fraction"),
                                                    canCancel: t.b("canCancel"))
            }
            var rows: [SendCardContent.Row] = []
            for (index, row) in s.a("rows").enumerated() {
                rows.append(SendCardContent.Row(id: "device-\(index)",
                                                alias: row.s("alias") ?? "",
                                                model: row.s("model") ?? "",
                                                symbol: row.s("symbol") ?? "display"))
            }
            send = SendCardContent(rows: rows, scanning: s.b("scanning"), transfer: transfer)
        }
        return DiskImagePrompt(iconPath: icon(v.s("icon")),
                               title: v.s("title") ?? "",
                               detail: v.s("detail") ?? "",
                               warning: v.s("warning"),
                               style: style,
                               primary: button(v.o("primary")),
                               secondary: button(v.o("secondary")),
                               send: send)
    }
}

/// The notch's right-click menu. The real one is a native `NSMenu`, which cannot
/// be drawn off-screen, so this is a drawn stand-in with the same item and key.
private struct MenuFacsimile: View {
    let items: [(title: String, shortcut: String)]

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            ForEach(Array(items.enumerated()), id: \.offset) { _, item in
                HStack {
                    Text(verbatim: item.title)
                    Spacer(minLength: 24)
                    Text(verbatim: item.shortcut).foregroundStyle(Palette.textSecondary)
                }
                .font(.system(size: 13))
                .foregroundStyle(Palette.textPrimary)
                .padding(.horizontal, 10)
                .padding(.vertical, 4)
            }
        }
        .padding(6)
        .frame(width: 180)
        .background(RoundedRectangle(cornerRadius: 10).fill(Color(white: 0.16)))
        .overlay(RoundedRectangle(cornerRadius: 10).strokeBorder(Color.white.opacity(0.14), lineWidth: 1))
    }
}
