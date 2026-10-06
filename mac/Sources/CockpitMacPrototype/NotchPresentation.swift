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
    let detailLines: [String]
    let recovery: AIUsageRecovery?
    var summary: String { "\(title) \(value.map { String(format: "%.0f%%", $0 * 100) } ?? "--")" }
}

private final class NotchAccessibilityElement: NSAccessibilityElement {
    enum Action {
        case metric
        case recovery
    }

    weak var owner: NotchSurfaceView?
    let action: Action
    let metricIndex: Int?

    init(owner: NotchSurfaceView, action: Action, metricIndex: Int? = nil) {
        self.owner = owner
        self.action = action
        self.metricIndex = metricIndex
        super.init()
    }

    @objc func accessibilityPerformPress() -> Bool {
        guard let owner else { return false }
        switch action {
        case .metric:
            if let metricIndex { owner.selectMetric(metricIndex) }
            owner.onClick?()
        case .recovery:
            owner.onRecovery?()
        }
        return true
    }
}

final class NotchSurfaceView: NSView {
    var onClick: (() -> Void)?
    var onRecovery: (() -> Void)?
    var onHoverChanged: ((Bool) -> Void)?
    var edge: CockpitNotchEdge { didSet { needsDisplay = true } }
    private(set) var reading = SystemReading(cpu: nil, memory: nil, disks: [])
    private var expanded = false {
        didSet {
            guard oldValue != expanded else { return }
            needsDisplay = true
            refreshAccessibilityChildren()
            updateTrackingAreas()
            onHoverChanged?(expanded)
        }
    }
    var isExpanded: Bool { expanded }
    private var selectedMetric: Int?
    private var metricTracking: [NSTrackingArea] = []
    private var detailTracking: NSTrackingArea?
    private var accessibilityChildrenElements: [NSAccessibilityElement] = []

    init(edge: CockpitNotchEdge) {
        self.edge = edge
        super.init(frame: .zero)
        setAccessibilityElement(true)
        setAccessibilityRole(.group)
        setAccessibilityLabel("Open Cockpit dashboard")
        setAccessibilityHelp("Live CPU, memory usage & volume allocation; click to open Storage")
        wantsLayer = true
        layer?.contentsScale = NSScreen.main?.backingScaleFactor ?? 2
        refreshAccessibilityChildren()
        rebuildTrackingAreas()
    }

    required init?(coder: NSCoder) { fatalError("init(coder:) has not been implemented") }

    func update(_ reading: SystemReading) {
        self.reading = reading
        toolTip = metrics.map { $0.summary }.joined(separator: " · ")
        setAccessibilityLabel(metrics.map { $0.title }.joined(separator: ", ") + ". Open Cockpit dashboard")
        setAccessibilityValue(metrics.map { "\($0.title): \($0.detailLines.joined(separator: ", "))" }.joined(separator: "; "))
        refreshAccessibilityChildren()
        rebuildTrackingAreas()
        needsDisplay = true
    }

    override var isFlipped: Bool { true }

    override func accessibilityPerformPress() -> Bool {
        false
    }

    fileprivate func selectMetric(_ index: Int) {
        guard metrics.indices.contains(index) else { return }
        selectedMetric = index
        refreshAccessibilityChildren()
        needsDisplay = true
    }

    override func updateTrackingAreas() {
        rebuildTrackingAreas()
        super.updateTrackingAreas()
    }

    override func mouseEntered(with event: NSEvent) {
        if let index = event.trackingArea?.userInfo?["metricIndex"] as? Int {
            selectMetric(index)
            expanded = true
        }
    }

    override func mouseExited(with event: NSEvent) {
        DispatchQueue.main.async { [weak self] in self?.reconcilePointer() }
    }

    override func mouseMoved(with event: NSEvent) {
        if let index = event.trackingArea?.userInfo?["metricIndex"] as? Int, selectedMetric != index {
            selectMetric(index)
        }
    }

    override func mouseDown(with event: NSEvent) {
        let point = convert(event.locationInWindow, from: nil)
        let depth = expanded ? NotchPresentationLayout.expandedDepth : NotchPresentationLayout.restingDepth(for: edge)
        let length = NotchPresentationLayout.stackLength(count: metricCount)
        let alongOffset = edge == .right || edge == .left ? (bounds.height - length) / 2 : (bounds.width - length) / 2
        if expanded, let selectedMetric, metrics.indices.contains(selectedMetric),
           metrics[selectedMetric].recovery.map({ $0 == .allowKeychainAccess }) == true,
           recoveryButtonRect(depth: depth, alongOffset: alongOffset).contains(point) {
            onRecovery?()
            return
        }
        onClick?()
    }

    override func hitTest(_ point: NSPoint) -> NSView? {
        return isInteractive(point) ? self : nil
    }

    var metricCount: Int { 4 + reading.disks.count }

    private var metrics: [NotchMetric] {
        let providers = [("claude", "Claude", "claude"), ("chatgpt", "ChatGPT", "openai")].map { id, title, symbol in
            let sample = reading.ai.first { $0.id == id }
            let windows = sample?.windows.map { window in
                "\(window.label): \(percentLabel(window.fraction))" + (window.resetsAt.map { " · resets \(resetLabel($0))" } ?? "")
            } ?? []
            var details = windows.isEmpty ? (sample?.detail.split(separator: " · ").map(String.init) ?? ["Unavailable"]) : windows
            if let status = sample?.status, status != .live { details.append("Status: \(status.label)") }
            if let recovery = sample?.recovery {
                details.append(recovery == .allowKeychainAccess ? "Click to allow keychain access" : "See dashboard for recovery")
            }
            return NotchMetric(title: title, value: sample?.fraction, symbol: symbol,
                               color: id == "claude" ? .systemOrange : .systemTeal,
                               detailLines: details, recovery: sample?.recovery)
        }
        let pressure = reading.memoryPressure
        let memoryStatus = pressure?.label ?? "Unavailable"
        let memoryDetail = [
            "Pressure: \(memoryStatus)",
            "RAM: \(byteLabel(reading.memoryUsedBytes)) / \(byteLabel(reading.memoryTotalBytes)) (\(percentLabel(reading.memory)))",
            "Swap: \(byteLabel(reading.swapUsedBytes)) / \(byteLabel(reading.swapTotalBytes))"
        ]
        return providers + [
            NotchMetric(title: "CPU", value: reading.cpu, symbol: "cpu", color: .systemBlue,
                        detailLines: ["Utilization: \(percentLabel(reading.cpu))", "All cores"], recovery: nil),
            NotchMetric(title: "Memory pressure", value: pressure?.severity, symbol: "memorychip",
                        color: pressure == nil ? .white.withAlphaComponent(0.4)
                            : pressure == .critical ? .systemRed : pressure == .warning ? .systemOrange : .systemGreen,
                        detailLines: memoryDetail, recovery: nil)
        ] + reading.disks.map { disk in
            let total = disk.totalBytes
            let free = disk.freeBytes
            let used: UInt64? = {
                guard let total, let free, total >= free else { return nil }
                return total - free
            }()
            return NotchMetric(title: disk.name, value: disk.free, symbol: disk.isInternal ? "internaldrive" : "externaldrive", color: .systemGreen,
                               detailLines: [
                                   "Free: \(byteLabel(free))",
                                   "Used: \(byteLabel(used))",
                                   "Total: \(byteLabel(total))"
                               ], recovery: nil)
        }
    }

    private func percentLabel(_ fraction: Double?) -> String {
        guard let fraction, fraction.isFinite else { return "Unavailable" }
        return String(format: "%.0f%%", min(max(fraction, 0), 1) * 100)
    }

    private func byteLabel(_ bytes: UInt64?) -> String {
        guard let bytes else { return "Unavailable" }
        return ByteCountFormatter.string(fromByteCount: Int64(min(bytes, UInt64(Int64.max))), countStyle: .file)
    }

    private func resetLabel(_ date: Date) -> String {
        let formatter = DateFormatter()
        formatter.dateStyle = .none
        formatter.timeStyle = .short
        return formatter.string(from: date)
    }

    fileprivate func refreshAccessibilityChildren() {
        let depth = expanded ? NotchPresentationLayout.expandedDepth : NotchPresentationLayout.restingDepth(for: edge)
        let length = NotchPresentationLayout.stackLength(count: metricCount)
        let alongOffset = edge == .right || edge == .left ? (bounds.height - length) / 2 : (bounds.width - length) / 2
        let elements = metrics.enumerated().map { index, metric in
            let element = NotchAccessibilityElement(owner: self, action: .metric, metricIndex: index)
            element.setAccessibilityElement(true)
            element.setAccessibilityRole(.button)
            element.setAccessibilityLabel(metric.title)
            element.setAccessibilityValue(metric.detailLines.joined(separator: ", "))
            element.setAccessibilityHelp("Hover for live \(metric.title) details; click to open its Cockpit view")
            element.setAccessibilityParent(self)
            element.accessibilityFrameInParentSpace = metricRect(index: index, depth: depth, alongOffset: alongOffset)
            return element
        }
        var children: [NSAccessibilityElement] = elements
        if expanded, let selectedMetric, metrics.indices.contains(selectedMetric),
           metrics[selectedMetric].recovery.map({ $0 == .allowKeychainAccess }) == true {
            let recoveryIndex = selectedMetric
            let recovery = NotchAccessibilityElement(owner: self, action: .recovery, metricIndex: recoveryIndex)
            recovery.setAccessibilityElement(true)
            recovery.setAccessibilityRole(.button)
            recovery.setAccessibilityLabel("Allow Claude Keychain access")
            recovery.setAccessibilityValue("Explicit user action; opens Keychain access prompt")
            recovery.setAccessibilityHelp("Allow Cockpit to read Claude usage. This prompt appears only after this action.")
            recovery.setAccessibilityParent(self)
            recovery.accessibilityFrameInParentSpace = recoveryButtonRect(depth: depth, alongOffset: alongOffset)
            children.append(recovery)
        }
        accessibilityChildrenElements = children
        setAccessibilityChildren(children)
    }

    private func metricCenter(index: Int, depth: CGFloat, alongOffset: CGFloat) -> CGPoint {
        let top = 26 + CGFloat(index) * NotchPresentationLayout.cellExtent
        let ring = NotchPresentationLayout.ringDiameter
        return map(CGPoint(x: depth - NotchPresentationLayout.ringMargin - ring / 2,
                           y: top + ring / 2), depth: depth, alongOffset: alongOffset)
    }

    private func metricRect(index: Int, depth: CGFloat, alongOffset: CGFloat) -> NSRect {
        let center = metricCenter(index: index, depth: depth, alongOffset: alongOffset)
        let ring = NotchPresentationLayout.ringDiameter
        return NSRect(x: center.x - ring / 2 - 5, y: center.y - ring / 2 - 5,
                      width: ring + 10, height: ring + 10)
    }

    private func detailRect(depth: CGFloat, alongOffset: CGFloat) -> NSRect {
        let gap = NotchPresentationLayout.ringDiameter + NotchPresentationLayout.ringMargin * 2
        switch edge {
        case .right:
            return NSRect(x: 0, y: alongOffset, width: max(0, depth - gap), height: NotchPresentationLayout.stackLength(count: metricCount))
        case .left:
            return NSRect(x: gap, y: alongOffset, width: max(0, depth - gap), height: NotchPresentationLayout.stackLength(count: metricCount))
        case .top:
            return NSRect(x: alongOffset, y: 0, width: NotchPresentationLayout.stackLength(count: metricCount), height: max(0, depth - gap))
        case .bottom:
            return NSRect(x: alongOffset, y: gap, width: NotchPresentationLayout.stackLength(count: metricCount), height: max(0, depth - gap))
        }
    }

    private func recoveryButtonRect(depth: CGFloat, alongOffset: CGFloat) -> NSRect {
        let card = detailRect(depth: depth, alongOffset: alongOffset)
        guard expanded else { return NSRect(x: card.minX, y: card.minY, width: 1, height: 1) }
        switch edge {
        case .right, .left, .top:
            return NSRect(x: card.minX + 14, y: card.maxY - 42,
                          width: min(190, max(1, card.width - 28)), height: 28)
        case .bottom:
            return NSRect(x: card.minX + 14, y: card.minY + 14,
                          width: min(190, max(1, card.width - 28)), height: 28)
        }
    }

    private func rebuildTrackingAreas() {
        metricTracking.forEach(removeTrackingArea)
        metricTracking.removeAll(keepingCapacity: true)
        if let detailTracking { removeTrackingArea(detailTracking) }
        detailTracking = nil

        let depth = expanded ? NotchPresentationLayout.expandedDepth : NotchPresentationLayout.restingDepth(for: edge)
        let length = NotchPresentationLayout.stackLength(count: metricCount)
        let alongOffset = edge == .right || edge == .left ? (bounds.height - length) / 2 : (bounds.width - length) / 2
        for index in metrics.indices {
            let area = NSTrackingArea(rect: metricRect(index: index, depth: depth, alongOffset: alongOffset),
                                      options: [.activeAlways, .mouseEnteredAndExited, .mouseMoved],
                                      owner: self, userInfo: ["metricIndex": index])
            addTrackingArea(area)
            metricTracking.append(area)
        }
        if expanded {
            let area = NSTrackingArea(rect: detailRect(depth: depth, alongOffset: alongOffset),
                                      options: [.activeAlways, .mouseEnteredAndExited, .mouseMoved], owner: self)
            addTrackingArea(area)
            detailTracking = area
        }
    }

    private func isInteractive(_ point: NSPoint) -> Bool {
        let depth = expanded ? NotchPresentationLayout.expandedDepth : NotchPresentationLayout.restingDepth(for: edge)
        let length = NotchPresentationLayout.stackLength(count: metricCount)
        let alongOffset = edge == .right || edge == .left ? (bounds.height - length) / 2 : (bounds.width - length) / 2
        if metrics.indices.contains(where: { metricRect(index: $0, depth: depth, alongOffset: alongOffset).contains(point) }) { return true }
        return expanded && detailRect(depth: depth, alongOffset: alongOffset).contains(point)
    }

    private func reconcilePointer() {
        guard let window else { expanded = false; selectedMetric = nil; return }
        let point = convert(window.mouseLocationOutsideOfEventStream, from: nil)
        guard expanded, isInteractive(point) else {
            expanded = false
            selectedMetric = nil
            return
        }
        needsDisplay = true
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
        let ring = NotchPresentationLayout.ringDiameter
        for (index, metric) in metrics.enumerated() {
            let center = metricCenter(index: index, depth: depth, alongOffset: alongOffset)
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
            drawMetricIcon(metric, in: NSRect(x: center.x - 6, y: center.y - 6, width: 12, height: 12))
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
        guard let selectedMetric, metrics.indices.contains(selectedMetric) else { return }
        let metric = metrics[selectedMetric]
        let titleAttributes: [NSAttributedString.Key: Any] = [
            .font: NSFont.systemFont(ofSize: 12, weight: .semibold),
            .foregroundColor: NSColor.white.withAlphaComponent(0.95)
        ]
        let bodyAttributes: [NSAttributedString.Key: Any] = [
            .font: NSFont.systemFont(ofSize: 10, weight: .regular),
            .foregroundColor: NSColor.white.withAlphaComponent(0.72)
        ]
        let card = detailRect(depth: depth, alongOffset: alongOffset)
        let x = card.minX + 16
        let y: CGFloat
        switch edge {
        case .right, .left: y = card.minY + 20
        case .top: y = card.minY + 16
        case .bottom: y = card.maxY - 32
        }
        drawMetricIcon(metric, in: NSRect(x: x, y: y, width: 14, height: 14))
        let titleX = x + 20
        drawDetail(metric.title, at: CGPoint(x: titleX, y: y), attributes: titleAttributes)
        let hasRecoveryButton = metric.recovery.map({ $0 == .allowKeychainAccess }) == true
        let lineLimit = hasRecoveryButton ? 3 : 4
        switch edge {
        case .right, .left:
            for (offset, line) in metric.detailLines.prefix(lineLimit).enumerated() {
                drawDetail(line, at: CGPoint(x: x, y: y + 20 + CGFloat(offset) * 16), attributes: bodyAttributes)
            }
        case .top:
            for (offset, line) in metric.detailLines.prefix(lineLimit).enumerated() {
                drawDetail(line, at: CGPoint(x: x, y: y + 20 + CGFloat(offset) * 16), attributes: bodyAttributes)
            }
        case .bottom:
            for (offset, line) in metric.detailLines.prefix(lineLimit).enumerated() {
                drawDetail(line, at: CGPoint(x: x, y: y - 20 - CGFloat(offset) * 16), attributes: bodyAttributes)
            }
        }
        if hasRecoveryButton {
            let button = recoveryButtonRect(depth: depth, alongOffset: alongOffset)
            NSColor.systemBlue.withAlphaComponent(0.9).setFill()
            NSBezierPath(roundedRect: button, xRadius: 6, yRadius: 6).fill()
            let buttonAttributes: [NSAttributedString.Key: Any] = [
                .font: NSFont.systemFont(ofSize: 10, weight: .semibold),
                .foregroundColor: NSColor.white
            ]
            let label = "Allow Claude Keychain access"
            let size = label.size(withAttributes: buttonAttributes)
            (label as NSString).draw(at: CGPoint(x: button.midX - size.width / 2,
                                                  y: button.midY - size.height / 2),
                                     withAttributes: buttonAttributes)
        }
    }

    private func drawMetricIcon(_ metric: NotchMetric, in rect: NSRect) {
        let outline = metric.symbol == "claude" ? ProviderGlyphOutlines.claude
            : metric.symbol == "openai" ? ProviderGlyphOutlines.openai : nil
        if let outline {
            let path = NSBezierPath()
            path.windingRule = .evenOdd
            for loop in outline {
                guard let first = loop.first else { continue }
                path.move(to: CGPoint(x: rect.minX + first.x * rect.width,
                                      y: rect.minY + first.y * rect.height))
                for point in loop.dropFirst() {
                    path.line(to: CGPoint(x: rect.minX + point.x * rect.width,
                                          y: rect.minY + point.y * rect.height))
                }
                path.close()
            }
            NSColor.white.setFill()
            path.fill()
            return
        }
        guard let image = NSImage(systemSymbolName: metric.symbol, accessibilityDescription: metric.title) else { return }
        image.draw(in: rect, from: .zero, operation: .sourceOver, fraction: 1)
    }
}
