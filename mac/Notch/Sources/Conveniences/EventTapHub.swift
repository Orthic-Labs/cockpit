import ApplicationServices
import CoreGraphics
import Foundation

enum TapDecision { case pass, swallow }

/// Called on the tap's own thread. Decide quickly and hop off it for anything
/// that takes time: a slow handler stalls every keystroke and click.
typealias TapHandler = (CGEventType, CGEvent) -> TapDecision

struct EventTapToken: Hashable { fileprivate let id = UUID() }

/// Cockpit: the one session event tap behind the Mac conveniences. Features
/// register ordered handlers (highest priority first; the first to swallow
/// wins). The tap lives on a thread of its own so the main thread never sits
/// in the event path, and `stop()` returns only once that thread has finished.
final class EventTapHub {
    private struct Entry {
        let token: EventTapToken
        let priority: Int
        let handler: TapHandler
    }

    private let lock = NSLock()
    private var entries: [Entry] = []
    private var tap: CFMachPort?
    private var runLoop: CFRunLoop?
    private var finished: DispatchSemaphore?

    var isRunning: Bool {
        lock.lock(); defer { lock.unlock() }
        return tap != nil
    }

    func register(priority: Int, handler: @escaping TapHandler) -> EventTapToken {
        let token = EventTapToken()
        lock.lock()
        entries.append(Entry(token: token, priority: priority, handler: handler))
        entries.sort { $0.priority > $1.priority }
        lock.unlock()
        return token
    }

    func unregister(_ token: EventTapToken) {
        lock.lock()
        entries.removeAll { $0.token == token }
        lock.unlock()
    }

    /// False when macOS refuses the tap (Accessibility or Input Monitoring not
    /// granted). Never prompts.
    @discardableResult
    func start() -> Bool {
        if isRunning { return true }
        let events: [CGEventType] = [.keyDown, .keyUp, .leftMouseDown, .leftMouseUp]
        var mask = CGEventMask(0)
        for event in events { mask |= CGEventMask(1) << CGEventMask(event.rawValue) }
        let callback: CGEventTapCallBack = { _, type, event, info in
            guard let info else { return Unmanaged.passUnretained(event) }
            return Unmanaged<EventTapHub>.fromOpaque(info).takeUnretainedValue().dispatch(type, event)
        }
        guard let port = CGEvent.tapCreate(
            tap: .cgSessionEventTap, place: .headInsertEventTap, options: .defaultTap,
            eventsOfInterest: mask, callback: callback,
            userInfo: Unmanaged.passUnretained(self).toOpaque())
        else { return false }

        let ready = DispatchSemaphore(value: 0)
        let done = DispatchSemaphore(value: 0)
        lock.lock(); tap = port; finished = done; lock.unlock()
        let thread = Thread { [weak self] in
            guard let self else { ready.signal(); done.signal(); return }
            let loop = CFRunLoopGetCurrent()
            let source = CFMachPortCreateRunLoopSource(kCFAllocatorDefault, port, 0)
            self.lock.lock(); self.runLoop = loop; self.lock.unlock()
            CFRunLoopAddSource(loop, source, .commonModes)
            CGEvent.tapEnable(tap: port, enable: true)
            ready.signal()
            CFRunLoopRun()
            CFRunLoopRemoveSource(loop, source, .commonModes)
            done.signal()
        }
        thread.name = "dev.orthic.cockpit.eventtap"
        thread.qualityOfService = .userInteractive
        thread.start()
        ready.wait()
        return true
    }

    /// Disables the tap, stops its thread and waits for any callback in flight.
    func stop() {
        lock.lock()
        let port = tap, loop = runLoop, done = finished
        tap = nil; runLoop = nil; finished = nil
        lock.unlock()
        guard let port else { return }
        CGEvent.tapEnable(tap: port, enable: false)
        if let loop { CFRunLoopStop(loop) }
        _ = done?.wait(timeout: .now() + 2)
        CFMachPortInvalidate(port)
    }

    fileprivate func dispatch(_ type: CGEventType, _ event: CGEvent) -> Unmanaged<CGEvent>? {
        if type == .tapDisabledByTimeout || type == .tapDisabledByUserInput {
            lock.lock(); let port = tap; lock.unlock()
            if let port { CGEvent.tapEnable(tap: port, enable: true) }
            return Unmanaged.passUnretained(event)
        }
        lock.lock(); let current = entries; lock.unlock()
        for entry in current where entry.handler(type, event) == .swallow {
            return nil
        }
        return Unmanaged.passUnretained(event)
    }
}
