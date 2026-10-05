import AppKit
import ApplicationServices
import CockpitMacPrototypeCore
import CoreGraphics
import Darwin
import Foundation

// M0 only: a native, non-interactive edge pill. Provider readers, settings, dashboard,
// permissions prompts, login items, updater, and donor extraction intentionally live later.

private struct DiskReading: Equatable {
    let id: String
    let name: String
    let free: Double
}

private struct SystemReading: Equatable {
    let cpu: Double?
    let memory: Double?
    let disks: [DiskReading]
}

private final class SystemReader {
    private var previousCPU: host_cpu_load_info_data_t?

    func read() -> SystemReading {
        let cpu = readCPU()
        let memory = readMemory()
        let disks = FileManager.default.mountedVolumeURLs(
            includingResourceValuesForKeys: [
                .volumeUUIDStringKey, .volumeNameKey, .volumeTotalCapacityKey,
                .volumeAvailableCapacityForImportantUsageKey, .volumeIsLocalKey
            ], options: [.skipNetworkVolumes, .skipPackageDescendants]
        )?.compactMap { url -> DiskReading? in
            guard let values = try? url.resourceValues(forKeys: [
                .volumeUUIDStringKey, .volumeNameKey, .volumeTotalCapacityKey,
                .volumeAvailableCapacityForImportantUsageKey, .volumeIsLocalKey
            ]), values.volumeIsLocal != false,
                  let id = values.volumeUUIDString, !id.isEmpty,
                  let total = values.volumeTotalCapacity, total > 0,
                  let available = values.volumeAvailableCapacityForImportantUsage else { return nil }
            return DiskReading(
                id: id,
                name: values.volumeName?.isEmpty == false ? values.volumeName! : url.path,
                free: min(max(Double(available) / Double(total), 0), 1)
            )
        }.sorted { $0.id < $1.id } ?? []
        return SystemReading(cpu: cpu, memory: memory, disks: disks)
    }

    private func readCPU() -> Double? {
        var info = host_cpu_load_info_data_t()
        var count = mach_msg_type_number_t(MemoryLayout<host_cpu_load_info_data_t>.size / MemoryLayout<integer_t>.size)
        let result = withUnsafeMutablePointer(to: &info) {
            $0.withMemoryRebound(to: integer_t.self, capacity: Int(count)) {
                host_statistics(mach_host_self(), HOST_CPU_LOAD_INFO, $0, &count)
            }
        }
        guard result == KERN_SUCCESS else { return nil }
        defer { previousCPU = info }
        guard let previousCPU else { return nil }
        let previous = previousCPU.cpu_ticks
        let current = info.cpu_ticks
        let user = current.0 &- previous.0
        let system = current.1 &- previous.1
        let nice = current.2 &- previous.2
        let idle = current.3 &- previous.3
        let total = user &+ system &+ nice &+ idle
        return total == 0 ? 0 : Double(user &+ system &+ nice) / Double(total)
    }

    private func readMemory() -> Double? {
        var vm = vm_statistics64()
        var count = mach_msg_type_number_t(MemoryLayout<vm_statistics64_data_t>.size / MemoryLayout<integer_t>.size)
        let result = withUnsafeMutablePointer(to: &vm) {
            $0.withMemoryRebound(to: integer_t.self, capacity: Int(count)) {
                host_statistics64(mach_host_self(), HOST_VM_INFO64, $0, &count)
            }
        }
        guard result == KERN_SUCCESS else { return nil }
        let page = Double(vm_page_size)
        let used = (Double(vm.active_count) + Double(vm.wire_count) + Double(vm.compressor_page_count)) * page
        return min(max(used / Double(ProcessInfo.processInfo.physicalMemory), 0), 1)
    }
}

private final class RingView: NSView {
    var reading = SystemReading(cpu: nil, memory: nil, disks: []) { didSet { needsDisplay = true } }

    override var isFlipped: Bool { true }
    override func draw(_ dirtyRect: NSRect) {
        NSColor(calibratedWhite: 0.06, alpha: 0.94).setFill()
        dirtyRect.fill()
        let values: [(String, Double?, NSColor)] = [
            ("CPU", reading.cpu, NSColor.systemBlue),
            ("MEM", reading.memory, NSColor.systemOrange)
        ] + reading.disks.map { (String($0.name.prefix(10)), Optional(1 - $0.free), NSColor.systemGreen) }
        let diameter: CGFloat = 30
        let x: CGFloat = 8
        for (index, value) in values.enumerated() {
            let y = CGFloat(index) * 34 + 8
            let rect = NSRect(x: x, y: y, width: diameter, height: diameter)
            NSColor(calibratedWhite: 0.28, alpha: 1).setStroke()
            NSBezierPath(ovalIn: rect).lineWidth = 3
            NSBezierPath(ovalIn: rect).stroke()
            let text: String
            if let fraction = value.1 {
                value.2.setStroke()
                let path = NSBezierPath()
                path.lineWidth = 3
                path.appendArc(withCenter: NSPoint(x: rect.midX, y: rect.midY), radius: diameter / 2,
                               startAngle: 90, endAngle: 90 - CGFloat(fraction * 360), clockwise: true)
                path.stroke()
                text = "\(value.0) \(Int(fraction * 100))%"
            } else {
                text = "\(value.0) --"
            }
            NSString(string: text).draw(at: NSPoint(x: x + diameter + 8, y: y + 8),
                                        withAttributes: [.font: NSFont.monospacedSystemFont(ofSize: 10, weight: .medium),
                                                         .foregroundColor: NSColor.white])
        }
        NSString(string: "M0 NATIVE PROTOTYPE").draw(at: NSPoint(x: x, y: bounds.height - 18),
            withAttributes: [.font: NSFont.monospacedSystemFont(ofSize: 8, weight: .regular),
                             .foregroundColor: NSColor.secondaryLabelColor])
    }
}

private final class PillPanel: NSPanel {
    let monitorID: String
    let ringView: RingView

    init(screen: NSScreen, monitorID: String) {
        self.monitorID = monitorID
        self.ringView = RingView(frame: .zero)
        let rows = max(2, SystemReader().read().disks.count) + 2
        let height = CGFloat(rows * 34 + 26)
        let width: CGFloat = 132
        let visible = screen.visibleFrame
        let rect = NSRect(x: visible.maxX - width - 8, y: visible.midY - height / 2,
                          width: width, height: height)
        super.init(contentRect: rect, styleMask: [.borderless], backing: .buffered, defer: false, screen: screen)
        level = .statusBar
        collectionBehavior = [.canJoinAllSpaces, .fullScreenNone]
        isOpaque = false
        backgroundColor = .clear
        hasShadow = true
        ignoresMouseEvents = true
        hidesOnDeactivate = false
        ringView.autoresizingMask = [.width, .height]
        contentView = ringView
        orderFrontRegardless()
    }

    override var canBecomeKey: Bool { false }
    override var canBecomeMain: Bool { false }
}

private final class FullscreenDetector {
    private let ownPID = ProcessInfo.processInfo.processIdentifier

    var accessibilityDenied: Bool { !AXIsProcessTrusted() }

    func shouldHide(on screen: NSScreen) -> Bool {
        // A denied TCC check must keep the pill visible and must never prompt.
        let focused = focusedWindowState()
        let monitor = displayBounds(for: screen)
        return VisibilityGeometry.shouldHide(
            accessibilityTrusted: !accessibilityDenied,
            axFullscreen: focused?.fullscreen,
            focusedWindow: focused?.frame,
            geometryFallback: geometryFallback(on: screen, monitor: monitor),
            monitor: monitor
        )
    }

    private func focusedWindowState() -> (fullscreen: Bool, frame: CGRect)? {
        let system = AXUIElementCreateSystemWide()
        var appValue: CFTypeRef?
        guard AXUIElementCopyAttributeValue(system, kAXFocusedApplicationAttribute, &appValue) == .success,
              let appValue else { return nil }
        let app = appValue as! AXUIElement
        var windowValue: CFTypeRef?
        guard AXUIElementCopyAttributeValue(app, kAXFocusedWindowAttribute, &windowValue) == .success,
              let windowValue else { return nil }
        let window = windowValue as! AXUIElement
        var fullscreenValue: CFTypeRef?
        let status = AXUIElementCopyAttributeValue(window, "AXFullScreen" as CFString, &fullscreenValue)
        var frame = CGRect.null
        var positionValue: CFTypeRef?
        var sizeValue: CFTypeRef?
        if AXUIElementCopyAttributeValue(window, kAXPositionAttribute, &positionValue) == .success,
           AXUIElementCopyAttributeValue(window, kAXSizeAttribute, &sizeValue) == .success {
            var point = CGPoint.zero
            var size = CGSize.zero
            if let positionValue { AXValueGetValue(positionValue as! AXValue, .cgPoint, &point) }
            if let sizeValue { AXValueGetValue(sizeValue as! AXValue, .cgSize, &size) }
            frame = CGRect(origin: point, size: size)
        }
        if status == .success, let value = fullscreenValue {
            if let bool = value as? Bool { return (bool, frame) }
            if let number = value as? NSNumber { return (number.boolValue, frame) }
        }
        return nil
    }

    private func geometryFallback(on screen: NSScreen, monitor: CGRect) -> Bool {
        guard let windows = CGWindowListCopyWindowInfo([.optionOnScreenOnly, .excludeDesktopElements], kCGNullWindowID)
                as? [[String: Any]] else { return false }
        for window in windows {
            guard let pid = window[kCGWindowOwnerPID as String] as? Int32, pid != ownPID,
                  let layer = window[kCGWindowLayer as String] as? Int, layer == 0,
                  let bounds = window[kCGWindowBounds as String] as? CFDictionary,
                  let rect = CGRect(dictionaryRepresentation: bounds),
                  rect.intersects(monitor) else { continue }
            // Borderless windows generally have no window title. Requiring it keeps the
            // fallback conservative around ordinary maximized AppKit windows.
            let title = window[kCGWindowName as String] as? String ?? ""
            return VisibilityGeometry.isLikelyBorderless(title, window: rect, monitor: monitor)
        }
        return false
    }

    private func displayBounds(for screen: NSScreen) -> CGRect {
        let number = screen.deviceDescription[NSDeviceDescriptionKey("NSScreenNumber")] as? CGDirectDisplayID ?? 0
        return CGDisplayBounds(number)
    }
}

private final class AppDelegate: NSObject, NSApplicationDelegate {
    private let reader = SystemReader()
    private let detector = FullscreenDetector()
    private var panels: [String: PillPanel] = [:]
    private var timer: Timer?
    private var hidden = false

    func applicationDidFinishLaunching(_ notification: Notification) {
        NSApp.setActivationPolicy(.accessory)
        rebuildPanels()
        schedule(after: 0)
    }

    private func rebuildPanels() {
        let wanted = Set(NSScreen.screens.map { monitorID(for: $0) })
        for (id, panel) in panels where !wanted.contains(id) { panel.orderOut(nil); panels.removeValue(forKey: id) }
        for screen in NSScreen.screens {
            let id = monitorID(for: screen)
            if panels[id] == nil { panels[id] = PillPanel(screen: screen, monitorID: id) }
        }
    }

    private func refresh() {
        rebuildPanels()
        let reading = reader.read()
        hidden = false
        for screen in NSScreen.screens {
            let id = monitorID(for: screen)
            guard let panel = panels[id] else { continue }
            let hide = detector.shouldHide(on: screen)
            hidden = hidden || hide
            panel.ringView.reading = reading
            if hide { panel.orderOut(nil) } else { panel.orderFrontRegardless() }
        }
        schedule(after: hidden ? 10 : 2)
    }

    private func schedule(after interval: TimeInterval) {
        timer?.invalidate()
        if interval == 0 { refresh(); return }
        timer = Timer.scheduledTimer(withTimeInterval: interval, repeats: false) { [weak self] _ in self?.refresh() }
    }

    private func monitorID(for screen: NSScreen) -> String {
        let number = screen.deviceDescription[NSDeviceDescriptionKey("NSScreenNumber")] as? CGDirectDisplayID ?? 0
        if let uuid = CGDisplayCreateUUIDFromDisplayID(number), let string = CFUUIDCreateString(nil, uuid) {
            return string as String
        }
        return "display-\(number)"
    }
}

let app = NSApplication.shared
let delegate = AppDelegate()
app.delegate = delegate
app.run()
