import AppKit
import ApplicationServices
import CockpitMacPrototypeCore
import CoreGraphics
import Darwin
import Foundation

// Native notch & on-demand storage dashboard. Provider readers & donor extraction
// remain separate integrations.

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

    init(screen: NSScreen, monitorID: String, diskCount: Int, anchor: PillAnchor) {
        self.monitorID = monitorID
        self.ringView = RingView(frame: .zero)
        let rect = AnchoredPlacement.frame(visible: screen.visibleFrame, diskCount: diskCount, anchor: anchor)
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
        // Not shown here: the sampler decides visibility (settings, monitor, fullscreen).
    }

    /// Re-place on the screen's current visible frame; no-op when unchanged.
    func place(on screen: NSScreen, diskCount: Int, anchor: PillAnchor) {
        let rect = AnchoredPlacement.frame(visible: screen.visibleFrame, diskCount: diskCount, anchor: anchor)
        if frame != rect { setFrame(rect, display: true) }
    }

    override var canBecomeKey: Bool { false }
    override var canBecomeMain: Bool { false }
}

private final class FullscreenDetector {
    private let ownPID = ProcessInfo.processInfo.processIdentifier

    var accessibilityDenied: Bool { !AXIsProcessTrusted() }

    func shouldHide(on screen: NSScreen) -> Bool {
        // A denied TCC check must keep the notch visible and must never prompt.
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

    /// nil means "unavailable": any AX call failure or unexpected CF type degrades to the
    /// conservative geometry fallback. Wrong-type CFTypeRefs are never force-cast.
    private func focusedWindowState() -> (fullscreen: Bool, frame: CGRect)? {
        let system = AXUIElementCreateSystemWide()
        guard let app = AXAttributeReader.element(
            AXAttributeReader.copyAttribute(system, kAXFocusedApplicationAttribute)),
              let window = AXAttributeReader.element(
            AXAttributeReader.copyAttribute(app, kAXFocusedWindowAttribute)) else { return nil }
        var frame = CGRect.null
        if let position = AXAttributeReader.point(AXAttributeReader.copyAttribute(window, kAXPositionAttribute)),
           let size = AXAttributeReader.size(AXAttributeReader.copyAttribute(window, kAXSizeAttribute)) {
            frame = CGRect(origin: position, size: size)
        }
        guard let fullscreen = AXAttributeReader.bool(
            AXAttributeReader.copyAttribute(window, "AXFullScreen")) else { return nil }
        return (fullscreen, frame)
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

@MainActor
private final class AppDelegate: NSObject, NSApplicationDelegate {
    private let dashboard = DashboardHost()
    private var statusItem: NSStatusItem?
    private let runtime: PillRuntime
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

    init(runtime: PillRuntime) {
        self.runtime = runtime
        super.init()
    }

    func applicationDidFinishLaunching(_ notification: Notification) {
        NSApp.setActivationPolicy(.regular)
        let menu = NSMenu()
        let open = NSMenuItem(title: "Open Storage", action: #selector(openDashboard), keyEquivalent: "o")
        open.target = self
        menu.addItem(open)
        menu.addItem(.separator())
        menu.addItem(NSMenuItem(title: "Quit Cockpit", action: #selector(NSApplication.terminate(_:)), keyEquivalent: "q"))
        let item = NSStatusBar.system.statusItem(withLength: NSStatusItem.variableLength)
        item.button?.title = "Cockpit"
        item.menu = menu
        statusItem = item
        let mainMenu = NSMenu()
        let appMenu = NSMenuItem()
        appMenu.submenu = menu.copy() as? NSMenu
        mainMenu.addItem(appMenu)
        NSApp.mainMenu = mainMenu
        dashboard.show()
        let args = ProcessInfo.processInfo.arguments
        if let flag = args.firstIndex(of: "--package-smoke-root"), flag + 1 < args.count {
            Task { [self] in
                do {
                    try await dashboard.verifyBundledScan(root: URL(fileURLWithPath: args[flag + 1]))
                    emit("dashboard_smoke_pass")
                    NSApp.terminate(nil)
                } catch {
                    emit("dashboard_smoke_failed", level: "error", ["reason": error.localizedDescription])
                    shutdown(reason: "package_smoke_failure")
                    exit(1)
                }
            }
        }
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

    @objc private func openDashboard() { dashboard.show() }

    func applicationShouldHandleReopen(_ sender: NSApplication, hasVisibleWindows flag: Bool) -> Bool {
        dashboard.show()
        return true
    }

    private func shutdown(reason: String) {
        guard !shuttingDown else { return }
        shuttingDown = true
        dashboard.stop()
        if let statusItem { NSStatusBar.system.removeStatusItem(statusItem) }
        statusItem = nil
        // Order (stop timer, close panels, persist, release lock, emit shutdown) is owned by PillRuntime.
        runtime.shutdown(reason: reason, stopTimer: { [self] in
            timer?.invalidate()
            timer = nil
            if let screenObserver { NotificationCenter.default.removeObserver(screenObserver) }
            screenObserver = nil
            signalSources.forEach { $0.cancel() }
            signalSources.removeAll()
        }, closePanels: { [self] in
            panels.values.forEach { $0.orderOut(nil); $0.close() }
            panels.removeAll()
        })
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
            panels[id] = PillPanel(screen: screen, monitorID: id, diskCount: diskCount,
                                   anchor: runtime.monitorSetting(for: id).anchor)
            emit("monitor_added", ["display": id])
        }
        for id in diff.kept {
            if let screen = byID[id] {
                panels[id]?.place(on: screen, diskCount: diskCount, anchor: runtime.monitorSetting(for: id).anchor)
            }
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
        // Start hidden: zero panels (or screens) means nothing can show, so we keep the
        // 10s hidden cadence rather than burning the fast cadence on an empty set.
        var allHidden = true
        for screen in NSScreen.screens {
            guard let panel = panels[monitorID(for: screen)] else { continue }
            panel.ringView.update(reading)
            let id = monitorID(for: screen)
            let wanted = runtime.settings.visible && runtime.monitorSetting(for: id).enabled
            // Fullscreen detection (Accessibility/CGWindowList) only runs for pills that could show.
            let suppressed = wanted ? detector.shouldHide(on: screen) : false
            if PillVisibility.shouldShow(settingsVisible: runtime.settings.visible,
                                         monitorEnabled: runtime.monitorSetting(for: id).enabled,
                                         fullscreenSuppressed: suppressed) {
                allHidden = false
                panel.orderFrontRegardless()
            } else {
                panel.orderOut(nil)
            }
        }
        schedule(after: PillSchedule.interval(allHidden: allHidden, cadenceSeconds: runtime.cadenceSeconds))
    }

    private func schedule(after interval: TimeInterval) {
        timer?.invalidate()
        timer = nil
        guard !shuttingDown else { return }
        timer = Timer.scheduledTimer(withTimeInterval: interval, repeats: false) { [weak self] _ in self?.sample() }
    }
}

emit("process_start")
// Native notch is independent of the worker: nothing here starts or connects to one.
private let runtime = PillRuntime(emit: { event, level, fields in emit(event, level: level, fields) })
switch runtime.start() {
case .started: break
case .alreadyRunning: exit(0)
case .failed: exit(1)
}
let app = NSApplication.shared
MainActor.assumeIsolated {
    let delegate = AppDelegate(runtime: runtime)
    app.delegate = delegate
    app.run()
    withExtendedLifetime(delegate) {}
}
