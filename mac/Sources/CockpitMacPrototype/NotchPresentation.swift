import AppKit
import CockpitMacPrototypeCore

// Presentation adaptation of upstream/codenotch's MIT-licensed notch surface.
// Reused ideas: bezel-relative stack coordinates, side-notch flare/corner path,
// compact ring stack, & edge rotation. This file intentionally carries no donor
// provider/runtime code; Cockpit supplies its own SystemReading values.

enum CockpitNotchEdge: Equatable {
    case right, left, top, bottom

    static func from(anchor: PillAnchor) -> CockpitNotchEdge {
        switch anchor {
        case .topRight: return .right // default: centered on right bezel
        case .topLeft: return .top
        case .bottomRight: return .bottom
        case .bottomLeft: return .left
        }
    }
}

enum NotchPresentationLayout {
    // Compact owner-directed adaptation of donor's bezel silhouette.
    // Metric icon stays inside each ring; exact values & names appear on hover.
    static let ringDiameter: CGFloat = 26
    static let ringTrackWidth: CGFloat = 2
    static let ringProgressWidth: CGFloat = 2
    static let ringMargin: CGFloat = 7
    static let cellExtent: CGFloat = 40
    static func stackLength(count: Int) -> CGFloat { CGFloat(max(1, count)) * cellExtent + 38 }
    static let restingDepth: CGFloat = 40
    static func restingDepth(for edge: CockpitNotchEdge) -> CGFloat { restingDepth }
    static let expandedDepth: CGFloat = 238

    static func frame(visible: CGRect, edge: CockpitNotchEdge, count: Int, expanded: Bool = false) -> CGRect {
        let depth = expanded ? expandedDepth : restingDepth(for: edge)
        let stackLength = stackLength(count: count)
        switch edge {
        case .right:
            return CGRect(x: visible.maxX - depth, y: visible.midY - stackLength / 2,
                          width: depth, height: stackLength)
        case .left:
            return CGRect(x: visible.minX, y: visible.midY - stackLength / 2,
                          width: depth, height: stackLength)
        case .top:
            return CGRect(x: visible.midX - stackLength / 2, y: visible.maxY - depth,
                          width: stackLength, height: depth)
        case .bottom:
            return CGRect(x: visible.midX - stackLength / 2, y: visible.minY,
                          width: stackLength, height: depth)
        }
    }
}

private struct NotchMetric {
    let title: String
    let value: Double?
    let symbol: String
    let color: NSColor
    var detail: String? = nil
    var summary: String { detail ?? PillFormat.label(title, fraction: value) }
}

final class NotchSurfaceView: NSView {
    var onClick: (() -> Void)?
    var onHoverChanged: ((Bool) -> Void)?
    var edge: CockpitNotchEdge { didSet { needsDisplay = true } }
    private(set) var reading = SystemReading(cpu: nil, memory: nil, disks: [])
    private var expanded = false { didSet { if oldValue != expanded { needsDisplay = true; onHoverChanged?(expanded) } } }
    var isExpanded: Bool { expanded }
    private var tracking: NSTrackingArea?

    init(edge: CockpitNotchEdge) {
        self.edge = edge
        super.init(frame: .zero)
        setAccessibilityElement(true)
        setAccessibilityRole(.button)
        setAccessibilityLabel("Open Cockpit dashboard")
        setAccessibilityHelp("Live CPU, memory usage & volume allocation; click to open Storage")
        wantsLayer = true
        layer?.contentsScale = NSScreen.main?.backingScaleFactor ?? 2
        tracking = NSTrackingArea(rect: bounds, options: [.activeAlways, .mouseEnteredAndExited], owner: self)
        if let tracking { addTrackingArea(tracking) }
    }

    required init?(coder: NSCoder) { fatalError("init(coder:) has not been implemented") }

    func update(_ reading: SystemReading) {
        self.reading = reading
        toolTip = metrics.map { $0.summary }.joined(separator: " · ")
        setAccessibilityValue(metrics.map { $0.summary }.joined(separator: ", "))
        needsDisplay = true
    }

    override var isFlipped: Bool { true }

    override func accessibilityPerformPress() -> Bool {
        onClick?()
        return onClick != nil
    }

    override func updateTrackingAreas() {
        if let tracking { removeTrackingArea(tracking) }
        tracking = NSTrackingArea(rect: bounds, options: [.activeAlways, .mouseEnteredAndExited], owner: self)
        if let tracking { addTrackingArea(tracking) }
        super.updateTrackingAreas()
    }

    override func mouseEntered(with event: NSEvent) { expanded = true }
    override func mouseExited(with event: NSEvent) { expanded = false }
    override func mouseDown(with event: NSEvent) { onClick?() }

    override func hitTest(_ point: NSPoint) -> NSView? {
        if expanded { return self }
        let hot = NotchPresentationLayout.restingDepth(for: edge)
        let inside: Bool
        switch edge {
        case .right: inside = point.x >= bounds.maxX - hot
        case .left: inside = point.x <= hot
        case .top: inside = point.y <= hot
        case .bottom: inside = point.y >= bounds.maxY - hot
        }
        return inside ? self : nil
    }

    var metricCount: Int { 4 + reading.disks.count }

    private var metrics: [NotchMetric] {
        let providers = [("claude", "Claude", "claude"), ("chatgpt", "ChatGPT", "openai")].map { id, title, symbol in
            let sample = reading.ai.first { $0.id == id }
            return NotchMetric(title: title, value: sample?.fraction, symbol: symbol,
                               color: id == "claude" ? .systemOrange : .systemTeal,
                               detail: sample?.detail ?? "\(title): awaiting provider reading")
        }
        let pressure = reading.memoryPressure
        let memoryDetail = "Memory pressure: \(pressure?.label ?? "Unavailable") · \(PillFormat.label("RAM used", fraction: reading.memory))"
        return providers + [
            NotchMetric(title: "CPU", value: reading.cpu, symbol: "cpu", color: .systemBlue),
            NotchMetric(title: "Memory pressure", value: pressure?.severity, symbol: "memorychip",
                        color: pressure == .critical ? .systemRed : pressure == .warning ? .systemOrange : .systemGreen,
                        detail: memoryDetail)
        ] + reading.disks.map { disk in
            NotchMetric(title: disk.name, value: disk.free, symbol: "externaldrive", color: .systemGreen,
                        detail: "\(disk.name) · \(PillFormat.label("free", fraction: disk.free))")
        }
    }

    override func draw(_ dirtyRect: NSRect) {
        let depth = expanded ? NotchPresentationLayout.expandedDepth : NotchPresentationLayout.restingDepth(for: edge)
        let length = NotchPresentationLayout.stackLength(count: metricCount)
        let alongOffset: CGFloat = edge == .right || edge == .left
            ? (bounds.height - length) / 2 : (bounds.width - length) / 2
        drawNotch(depth: depth, length: length, alongOffset: alongOffset)
        drawMetrics(depth: depth, alongOffset: alongOffset)
        if expanded { drawDetails(depth: depth, alongOffset: alongOffset) }
    }

    // Minimal port of donor SideNotchShape's canonical right-edge outline:
    // fluid quarter-turn flares, a rounded body corner, & bezel-attached fill.
    private func drawNotch(depth: CGFloat, length: CGFloat, alongOffset: CGFloat) {
        let flare = min(14, length / 4)
        let corner = min(12, depth / 2, (length - 2 * flare) / 2)
        let bodyTop = flare
        let bodyBottom = length - flare
        let path = NSBezierPath()
        func map(_ point: CGPoint) -> CGPoint {
            switch edge {
            case .right: return CGPoint(x: bounds.width - depth + point.x, y: alongOffset + point.y)
            case .left: return CGPoint(x: depth - point.x, y: alongOffset + point.y)
            case .top: return CGPoint(x: alongOffset + point.y, y: depth - point.x)
            case .bottom: return CGPoint(x: alongOffset + point.y, y: bounds.height - depth + point.x)
            }
        }
        func move(_ p: CGPoint) { path.move(to: map(p)) }
        func line(_ p: CGPoint) { path.line(to: map(p)) }
        func curve(_ p: CGPoint, _ c1: CGPoint, _ c2: CGPoint) { path.curve(to: map(p), controlPoint1: map(c1), controlPoint2: map(c2)) }
        let k: CGFloat = 0.5523
        move(CGPoint(x: depth, y: 0))
        curve(CGPoint(x: depth - flare, y: bodyTop), CGPoint(x: depth, y: bodyTop * k), CGPoint(x: depth - flare * (1 - k), y: bodyTop))
        line(CGPoint(x: corner, y: bodyTop))
        curve(CGPoint(x: 0, y: bodyTop + corner), CGPoint(x: corner * 0.45, y: bodyTop), CGPoint(x: 0, y: bodyTop + corner * (1 - k)))
        line(CGPoint(x: 0, y: bodyBottom - corner))
        curve(CGPoint(x: corner, y: bodyBottom), CGPoint(x: 0, y: bodyBottom - corner * (1 - k)), CGPoint(x: corner * 0.45, y: bodyBottom))
        line(CGPoint(x: depth - flare, y: bodyBottom))
        curve(CGPoint(x: depth, y: length), CGPoint(x: depth - flare * (1 - k), y: bodyBottom), CGPoint(x: depth, y: length - flare * k))
        line(CGPoint(x: depth, y: 0))
        path.close()
        NSColor.black.setFill()
        path.fill()
    }

    private func map(_ point: CGPoint, depth: CGFloat, alongOffset: CGFloat) -> CGPoint {
        switch edge {
        case .right: return CGPoint(x: bounds.width - depth + point.x, y: alongOffset + point.y)
        case .left: return CGPoint(x: depth - point.x, y: alongOffset + point.y)
        case .top: return CGPoint(x: alongOffset + point.y, y: depth - point.x)
        case .bottom: return CGPoint(x: alongOffset + point.y, y: bounds.height - depth + point.x)
        }
    }

    private func drawMetrics(depth: CGFloat, alongOffset: CGFloat) {
        let margin = NotchPresentationLayout.ringMargin
        let ring = NotchPresentationLayout.ringDiameter
        for (index, metric) in metrics.enumerated() {
            let top = 26 + CGFloat(index) * NotchPresentationLayout.cellExtent
            let center = map(CGPoint(x: depth - margin - ring / 2, y: top + ring / 2), depth: depth, alongOffset: alongOffset)
            let ringRect = NSRect(x: center.x - ring / 2, y: center.y - ring / 2, width: ring, height: ring)
            NSColor.white.withAlphaComponent(0.22).setStroke()
            let track = NSBezierPath(ovalIn: ringRect)
            track.lineWidth = NotchPresentationLayout.ringTrackWidth
            track.stroke()
            if let value = metric.value, value.isFinite {
                metric.color.setStroke()
                let arc = NSBezierPath()
                arc.lineWidth = NotchPresentationLayout.ringProgressWidth
                arc.lineCapStyle = .round
                arc.appendArc(withCenter: center, radius: ring / 2 - 1,
                              startAngle: 90, endAngle: 90 - CGFloat(min(max(value, 0), 1) * 360), clockwise: true)
                arc.stroke()
            }
            let iconRect = NSRect(x: center.x - 6, y: center.y - 6, width: 12, height: 12)
            let outline = metric.symbol == "claude" ? ProviderGlyphOutlines.claude
                : metric.symbol == "openai" ? ProviderGlyphOutlines.openai : nil
            if let outline {
                let path = NSBezierPath()
                path.windingRule = .evenOdd
                for loop in outline {
                    guard let first = loop.first else { continue }
                    path.move(to: CGPoint(x: iconRect.minX + first.x * iconRect.width,
                                          y: iconRect.minY + first.y * iconRect.height))
                    for point in loop.dropFirst() {
                        path.line(to: CGPoint(x: iconRect.minX + point.x * iconRect.width,
                                              y: iconRect.minY + point.y * iconRect.height))
                    }
                    path.close()
                }
                NSColor.white.setFill()
                path.fill()
            } else if let image = NSImage(systemSymbolName: metric.symbol, accessibilityDescription: metric.title) {
                let tinted = image.copy() as! NSImage
                tinted.lockFocus()
                NSColor.white.setFill()
                NSRect(origin: .zero, size: tinted.size).fill(using: .sourceAtop)
                tinted.unlockFocus()
                tinted.draw(in: NSRect(x: center.x - 6, y: center.y - 6, width: 12, height: 12),
                            from: .zero, operation: .sourceOver, fraction: 1)
            }
        }
    }

    private func drawDetail(_ text: String, at point: CGPoint, attributes: [NSAttributedString.Key: Any]) {
        let paragraph = NSMutableParagraphStyle()
        paragraph.lineBreakMode = .byTruncatingTail
        var styled = attributes
        styled[.paragraphStyle] = paragraph
        (text as NSString).draw(in: CGRect(x: point.x, y: point.y, width: 170, height: 16), withAttributes: styled)
    }

    private func drawDetails(depth: CGFloat, alongOffset: CGFloat) {
        let lines = ["System"] + metrics.map { $0.summary }
        let attributes: [NSAttributedString.Key: Any] = [
            .font: NSFont.systemFont(ofSize: 11, weight: .medium),
            .foregroundColor: NSColor.white.withAlphaComponent(0.9)
        ]
        let secondary: [NSAttributedString.Key: Any] = [
            .font: NSFont.systemFont(ofSize: 10, weight: .regular),
            .foregroundColor: NSColor.white.withAlphaComponent(0.58)
        ]
        switch edge {
        case .right:
            var y: CGFloat = alongOffset + 26
            for (index, line) in lines.enumerated() {
                drawDetail(line, at: CGPoint(x: 18, y: y), attributes: index == 1 ? secondary : attributes)
                y += index == 0 ? 22 : 17
            }
        case .left:
            var y: CGFloat = alongOffset + 26
            for (index, line) in lines.enumerated() {
                drawDetail(line, at: CGPoint(x: NotchPresentationLayout.ringDiameter + 20, y: y), attributes: index == 1 ? secondary : attributes)
                y += index == 0 ? 22 : 17
            }
        case .top:
            var y: CGFloat = 88
            for (index, line) in lines.prefix(5).enumerated() {
                let size = line.size(withAttributes: index == 1 ? secondary : attributes)
                (line as NSString).draw(at: CGPoint(x: (bounds.width - size.width) / 2, y: y), withAttributes: index == 1 ? secondary : attributes)
                y += 16
            }
        case .bottom:
            var y: CGFloat = bounds.height - 88
            for (index, line) in lines.prefix(5).enumerated() {
                let size = line.size(withAttributes: index == 1 ? secondary : attributes)
                (line as NSString).draw(at: CGPoint(x: (bounds.width - size.width) / 2, y: y), withAttributes: index == 1 ? secondary : attributes)
                y -= 16
            }
        }
    }
}
