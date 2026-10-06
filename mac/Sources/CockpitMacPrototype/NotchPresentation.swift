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
    // Percent stays inside each ring; names & volume detail appear on hover.
    static let ringDiameter: CGFloat = 26
    static let ringTrackWidth: CGFloat = 2
    static let ringProgressWidth: CGFloat = 2
    static let ringMargin: CGFloat = 7
    static let cellExtent: CGFloat = 40
    static let stackLength: CGFloat = 158
    static let restingDepth: CGFloat = 40
    static func restingDepth(for edge: CockpitNotchEdge) -> CGFloat { restingDepth }
    static let expandedDepth: CGFloat = 238

    static func frame(visible: CGRect, edge: CockpitNotchEdge, expanded: Bool = false) -> CGRect {
        let depth = expanded ? expandedDepth : restingDepth(for: edge)
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
    let color: NSColor
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
        setAccessibilityValue(metrics.map { PillFormat.label($0.title, fraction: $0.value) }.joined(separator: ", "))
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

    private var metrics: [NotchMetric] {
        let disk = reading.disks.first
        return [
            NotchMetric(title: "CPU", value: reading.cpu, color: .systemBlue),
            NotchMetric(title: "RAM", value: reading.memory, color: .systemOrange),
            NotchMetric(title: "Volume", value: disk.map { 1 - $0.free }, color: .systemGreen)
        ]
    }

    override func draw(_ dirtyRect: NSRect) {
        let depth = expanded ? NotchPresentationLayout.expandedDepth : NotchPresentationLayout.restingDepth(for: edge)
        let length = NotchPresentationLayout.stackLength
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
            let label = metric.value.map { "\(Int(min(max($0, 0), 1) * 100))%" } ?? "—"
            let attributes: [NSAttributedString.Key: Any] = [
                .font: NSFont.monospacedDigitSystemFont(ofSize: 8, weight: .semibold),
                .foregroundColor: NSColor.white
            ]
            let size = label.size(withAttributes: attributes)
            let labelPoint = CGPoint(x: center.x - size.width / 2,
                                     y: center.y - size.height / 2)
            (label as NSString).draw(at: labelPoint, withAttributes: attributes)
        }
    }

    private func drawDetails(depth: CGFloat, alongOffset: CGFloat) {
        let diskLines = reading.disks.prefix(3).map { "\($0.name.prefix(16))  \(Int((1 - $0.free) * 100))% used" }
        let lines = ["System"] + metrics.prefix(2).map { PillFormat.label($0.title, fraction: $0.value) } + diskLines
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
                (line as NSString).draw(at: CGPoint(x: 18, y: y), withAttributes: index == 1 ? secondary : attributes)
                y += index == 0 ? 22 : 17
            }
        case .left:
            var y: CGFloat = alongOffset + 26
            for (index, line) in lines.enumerated() {
                (line as NSString).draw(at: CGPoint(x: NotchPresentationLayout.ringDiameter + 28, y: y), withAttributes: index == 1 ? secondary : attributes)
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
