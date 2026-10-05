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
    /// Counter names that failed to sample this tick (unavailable, rendered as "--").
    var failures: Set<String> = []
}

/// Mach host calls: every mach_host_self() call returns a send right that must be released.
private func withHostPort<T>(_ body: (mach_port_t) -> T) -> T {
    let host = mach_host_self()
    defer { mach_port_deallocate(mach_task_self_, host) }
    return body(host)
}

/// Process-wide event sink: one JSON object per line on stderr.
private func emit(_ event: String, level: String = "info", _ fields: [String: String] = [:]) {
    let line = StructuredEvent(event: event, level: level, ts: StructuredEvent.timestamp(Date()), fields: fields).jsonLine()
    FileHandle.standardError.write(Data((line + "\n").utf8))
}

private final class SystemReader {
    private var previousCPU: CPUTicks?

    func read() -> SystemReading {
        var failures = Set<String>()
        let cpu = readCPU(&failures)
        let memory = readMemory(&failures)
        let keys: [URLResourceKey] = [
            .volumeUUIDStringKey, .volumeNameKey, .volumeTotalCapacityKey,
            .volumeAvailableCapacityForImportantUsageKey, .volumeIsLocalKey
        ]
        let urls = FileManager.default.mountedVolumeURLs(includingResourceValuesForKeys: keys, options: [.skipHiddenVolumes])
        if urls == nil { failures.insert("disks") }
        let disks = (urls ?? []).compactMap { url -> DiskReading? in
            guard let values = try? url.resourceValues(forKeys: Set(keys)), values.volumeIsLocal == true,
                  let id = values.volumeUUIDString, !id.isEmpty,
                  let total = values.volumeTotalCapacity, total > 0,
                  let available = values.volumeAvailableCapacityForImportantUsage else { return nil }
            return DiskReading(
                id: id,
                name: values.volumeName?.isEmpty == false ? values.volumeName! : url.path,
                free: min(max(Double(available) / Double(total), 0), 1)
            )
        }.sorted { $0.id < $1.id }
        return SystemReading(cpu: cpu, memory: memory, disks: disks, failures: failures)
    }

    private func readCPU(_ failures: inout Set<String>) -> Double? {
        // host_statistics returns by value: no host_processor_info arrays to vm_deallocate here.
        // If per-core sampling is added, its returned array must be released with vm_deallocate.
        var info = host_cpu_load_info_data_t()
        var count = mach_msg_type_number_t(MemoryLayout<host_cpu_load_info_data_t>.size / MemoryLayout<integer_t>.size)
        let result = withHostPort { host in
            withUnsafeMutablePointer(to: &info) {
                $0.withMemoryRebound(to: integer_t.self, capacity: Int(count)) {
                    host_statistics(host, HOST_CPU_LOAD_INFO, $0, &count)
                }
            }
        }
        guard result == KERN_SUCCESS else {
            previousCPU = nil
            failures.insert("cpu")
            return nil
        }
        let ticks = info.cpu_ticks
        let current = CPUTicks(user: ticks.0, system: ticks.1, nice: ticks.3, idle: ticks.2)
        defer { previousCPU = current }
        return CPUMath.utilization(previous: previousCPU, current: current)
    }

    private func readMemory(_ failures: inout Set<String>) -> Double? {
        var vm = vm_statistics64()
        var count = mach_msg_type_number_t(MemoryLayout<vm_statistics64_data_t>.size / MemoryLayout<integer_t>.size)
        let result = withHostPort { host in
            withUnsafeMutablePointer(to: &vm) {
                $0.withMemoryRebound(to: integer_t.self, capacity: Int(count)) {
                    host_statistics64(host, HOST_VM_INFO64, $0, &count)
                }
            }
        }
        guard result == KERN_SUCCESS else { failures.insert("memory"); return nil }
        let page = Double(vm_page_size)
        let used = (Double(vm.active_count) + Double(vm.wire_count) + Double(vm.compressor_page_count)) * page
        return min(max(used / Double(ProcessInfo.processInfo.physicalMemory), 0), 1)
    }
}

private final class RingView: NSView {
    private var gate = RedrawGate()
    private(set) var reading = SystemReading(cpu: nil, memory: nil, disks: [])

    private static func rows(_ r: SystemReading) -> [(name: String, fraction: Double?)] {
        [("CPU", r.cpu), ("RAM", r.memory)] + r.disks.map { (String($0.name.prefix(10)), Optional(1 - $0.free)) }
    }

    /// Redraws only when the rendered text changes (ring arcs derive from the same values at
    /// whole-percent resolution; sub-percent changes are intentionally not redrawn).
    func update(_ new: SystemReading) {
        reading = new
        if gate.shouldRedraw(PillFormat.signature(Self.rows(new))) { needsDisplay = true }
    }


    override var isFlipped: Bool { true }
    override func draw(_ dirtyRect: NSRect) {
        NSColor(calibratedWhite: 0.06, alpha: 0.94).setFill()
        dirtyRect.fill()
        let values: [(String, Double?, NSColor)] = [
            ("CPU", reading.cpu, NSColor.systemBlue),
            ("RAM", reading.memory, NSColor.systemOrange)
        ] + reading.disks.map { (String($0.name.prefix(10)), Optional(1 - $0.free), NSColor.systemGreen) }
        let diameter: CGFloat = 30
        let x: CGFloat = 8
        for (index, value) in values.enumerated() {
            let y = CGFloat(index) * 34 + 8
            let rect = NSRect(x: x, y: y, width: diameter, height: diameter)
            NSColor(calibratedWhite: 0.28, alpha: 1).setStroke()
            let backgroundRing = NSBezierPath(ovalIn: rect)
            backgroundRing.lineWidth = 3
            backgroundRing.stroke()
            let text: String
            if let fraction = PillFormat.displayedFraction(value.1) {
                value.2.setStroke()
                let path = NSBezierPath()
                path.lineWidth = 3
                path.appendArc(withCenter: NSPoint(x: rect.midX, y: rect.midY), radius: diameter / 2,
                               startAngle: 90, endAngle: 90 - CGFloat(fraction * 360), clockwise: true)
                path.stroke()
                text = PillFormat.label(value.0, fraction: value.1)
            } else {
                text = PillFormat.label(value.0, fraction: nil)
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

    init(screen: NSScreen, monitorID: String, diskCount: Int) {
        self.monitorID = monitorID
        self.ringView = RingView(frame: .zero)
        let rect = PillPlacement.frame(visible: screen.visibleFrame, diskCount: diskCount)
        super.init(contentRect: rect, styleMask: [.borderless, .nonactivatingPanel], backing: .buffered, defer: false, screen: screen)
        level = .statusBar
        collectionBehavior = [.canJoinAllSpaces, .fullScreenNone]
        isOpaque = false
        backgroundColor = .clear
        hasShadow = true
        ignoresMouseEvents = true
        hidesOnDeactivate = false
        // Panels are owned by the delegate's dictionary; AppKit must not release them on close.
        isReleasedWhenClosed = false
        ringView.autoresizingMask = [.width, .height]
        contentView = ringView
        orderFrontRegardless()
    }

    /// Re-place on the screen's current visible frame; no-op when unchanged.
    func place(on screen: NSScreen, diskCount: Int) {
        let rect = PillPlacement.frame(visible: screen.visibleFrame, diskCount: diskCount)
        if frame != rect { setFrame(rect, display: true) }
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
        guard AXUIElementCopyAttributeValue(system, kAXFocusedApplicationAttribute as CFString, &appValue) == .success,
              let appValue else { return nil }
        let app = appValue as! AXUIElement
        var windowValue: CFTypeRef?
        guard AXUIElementCopyAttributeValue(app, kAXFocusedWindowAttribute as CFString, &windowValue) == .success,
              let windowValue else { return nil }
        let window = windowValue as! AXUIElement
        var fullscreenValue: CFTypeRef?
        let status = AXUIElementCopyAttributeValue(window, "AXFullScreen" as CFString, &fullscreenValue)
        var frame = CGRect.null
        var positionValue: CFTypeRef?
        var sizeValue: CFTypeRef?
        if AXUIElementCopyAttributeValue(window, kAXPositionAttribute as CFString, &positionValue) == .success,
           AXUIElementCopyAttributeValue(window, kAXSizeAttribute as CFString, &sizeValue) == .success {
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
                  let bounds = window[kCGWindowBounds as String] as? [String: Any],
                  let rect = CGRect(dictionaryRepresentation: bounds as CFDictionary),
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

private func displayNumber(_ screen: NSScreen) -> CGDirectDisplayID {
    screen.deviceDescription[NSDeviceDescriptionKey("NSScreenNumber")] as? CGDirectDisplayID ?? 0
}

private func monitorID(for screen: NSScreen) -> String {
    let number = displayNumber(screen)
    if let uuid = CGDisplayCreateUUIDFromDisplayID(number)?.takeRetainedValue(), let string = CFUUIDCreateString(nil, uuid) {
        return string as String
    }
    return "display-\(number)"
}

private final class AppDelegate: NSObject, NSApplicationDelegate {
    private let reader = SystemReader()
    private let detector = FullscreenDetector()
    private var panels: [String: PillPanel] = [:]
    /// The single sampling owner. Only schedule(after:) creates or invalidates it.
    private var timer: Timer?
    private var failures = FailureTracker()
    private var diskCount = 0
    private var screenObserver: NSObjectProtocol?
    private var signalSources: [DispatchSourceSignal] = []
    private var shuttingDown = false

    func applicationDidFinishLaunching(_ notification: Notification) {
        NSApp.setActivationPolicy(.accessory)
        emit("startup", [
            "pid": String(ProcessInfo.processInfo.processIdentifier),
            "accessibility": AXIsProcessTrusted() ? "trusted" : "denied"
        ])
        screenObserver = NotificationCenter.default.addObserver(
            forName: NSApplication.didChangeScreenParametersNotification, object: nil, queue: .main
        ) { [weak self] _ in
            guard let self, !self.shuttingDown else { return }
            emit("screen_parameters_changed", ["screens": String(NSScreen.screens.count)])
            self.sample()
        }
        for sig in [SIGTERM, SIGINT] {
            signal(sig, SIG_IGN)
            let source = DispatchSource.makeSignalSource(signal: sig, queue: .main)
            source.setEventHandler { NSApp.terminate(nil) }
            source.resume()
            signalSources.append(source)
        }
        sample()
    }

    func applicationWillTerminate(_ notification: Notification) {
        shutdown(reason: "terminate")
    }

    private func shutdown(reason: String) {
        guard !shuttingDown else { return }
        shuttingDown = true
        timer?.invalidate()
        timer = nil
        if let screenObserver { NotificationCenter.default.removeObserver(screenObserver) }
        screenObserver = nil
        signalSources.forEach { $0.cancel() }
        signalSources.removeAll()
        panels.values.forEach { $0.orderOut(nil); $0.close() }
        panels.removeAll()
        emit("shutdown", ["reason": reason])
    }

    /// Reconcile panels with current screens by display key: add new, drop gone, re-place kept.
    private func rebuildPanels() {
        var byID: [String: NSScreen] = [:]
        var order: [String] = []
        for screen in NSScreen.screens {
            let id = monitorID(for: screen)
            if byID[id] == nil { order.append(id) }
            byID[id] = screen
        }
        let diff = DisplayKeyDiff.diff(existing: Set(panels.keys), wanted: order)
        for id in diff.removed {
            panels[id]?.orderOut(nil)
            panels[id]?.close()
            panels.removeValue(forKey: id)
            emit("monitor_removed", ["display": id])
        }
        for id in diff.added {
            guard let screen = byID[id] else { continue }
            panels[id] = PillPanel(screen: screen, monitorID: id, diskCount: diskCount)
            emit("monitor_added", ["display": id])
        }
        for id in diff.kept {
            if let screen = byID[id] { panels[id]?.place(on: screen, diskCount: diskCount) }
        }
    }

    /// One sample per tick, shared by all monitors. Visibility is decided per monitor, so a
    /// fullscreen monitor never stops sampling for the visible ones.
    private func sample() {
        guard !shuttingDown else { return }
        let reading = reader.read()
        diskCount = reading.disks.count
        let change = failures.update(failing: reading.failures)
        for name in change.failed { emit("sampling_failed", level: "error", ["counter": name]) }
        for name in change.recovered { emit("sampling_recovered", ["counter": name]) }
        rebuildPanels()
        var allHidden = !panels.isEmpty
        for screen in NSScreen.screens {
            guard let panel = panels[monitorID(for: screen)] else { continue }
            panel.ringView.update(reading)
            if detector.shouldHide(on: screen) {
                panel.orderOut(nil)
            } else {
                allHidden = false
                panel.orderFrontRegardless()
            }
        }
        schedule(after: allHidden ? 10 : 2)
    }

    private func schedule(after interval: TimeInterval) {
        timer?.invalidate()
        timer = nil
        guard !shuttingDown else { return }
        timer = Timer.scheduledTimer(withTimeInterval: interval, repeats: false) { [weak self] _ in self?.sample() }
    }
}

emit("process_start")
let app = NSApplication.shared
private let delegate = AppDelegate()
app.delegate = delegate
app.run()
