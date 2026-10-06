import Darwin
import Foundation

/// Cockpit fork: the machine's own readings as two notch cells — system (CPU
/// as the main ring, memory pressure as the thin outer ring) and disks
/// (internal drive as the main ring, external as the outer ring). Each is a `UsageProvider` of
/// kind `.system`, so it gets Codenotch's ring, hover card and ordering for
/// free, while the store refreshes it every two seconds and keeps it out of
/// the usage archive and alerts.
///
/// Counters follow Stats (exelban/stats): host CPU load ticks, VM statistics
/// and the kernel's memory-status level. Nothing here launches a process,
/// touches the network or reads file contents.
enum SystemProviders {
    static let cpuID = "system-cpu"
    static let memoryID = "system-memory"
    static let disksID = "system-disks"

    static func isSystem(providerID: String) -> Bool { providerID.hasPrefix("system-") }

    static func all() -> [UsageProvider] {
        [SystemLoadProvider(), DisksProvider()]
    }

    static func bytes(_ value: Int64) -> String {
        ByteCountFormatter.string(fromByteCount: value, countStyle: .file)
    }

    static func snapshot(id: String, name: String, glyph: ProviderGlyph,
                         window: LimitWindow) -> ProviderSnapshot {
        ProviderSnapshot(id: id, displayName: name, glyph: glyph, fidelity: .official,
                         status: .ok, windows: [window], headlineID: window.id,
                         kind: .system)
    }
}

/// One cell: share of all cores busy since the previous reading as the main
/// ring, memory pressure as the thin outer ring.
actor SystemLoadProvider: UsageProvider {
    nonisolated let id = SystemProviders.cpuID
    nonisolated let displayName = "System"
    nonisolated let glyph = ProviderGlyph.cpu
    nonisolated let kind = ProviderKind.system
    nonisolated var signInRoute: SignInRoute { .guidance("") }
    nonisolated func account() -> ProviderAccount? { nil }

    private static let host = mach_host_self()
    private var previous: Ticks?

    private struct Ticks {
        let busy: UInt32
        let idle: UInt32
    }

    func fetchSnapshot() async throws -> ProviderSnapshot {
        let now = try Self.read()
        // First reading: the average since boot, until there is a delta.
        let base = previous ?? Ticks(busy: 0, idle: 0)
        previous = now
        let busy = Double(now.busy &- base.busy)
        let total = busy + Double(now.idle &- base.idle)
        guard total > 0 else { throw UsageProviderError.apiError(L10n.t("No CPU change yet")) }
        let cores = ProcessInfo.processInfo.activeProcessorCount
        let cpu = LimitWindow(id: "cpu", label: L10n.t("CPU"),
                              usedFraction: min(max(busy / total, 0), 1),
                              detail: L10n.t("\(Percent.text(for: busy / total))% busy · \(cores) cores"))
        let memory = try? MemoryProvider.window()
        return ProviderSnapshot(id: id, displayName: displayName, glyph: glyph, fidelity: .official,
                                status: .ok, windows: [cpu] + (memory.map { [$0] } ?? []),
                                headlineID: cpu.id, weeklyID: memory?.id, kind: .system)
    }

    private static func read() throws -> Ticks {
        var info = host_cpu_load_info_data_t()
        var count = mach_msg_type_number_t(
            MemoryLayout<host_cpu_load_info_data_t>.stride / MemoryLayout<integer_t>.stride)
        let result = withUnsafeMutablePointer(to: &info) {
            $0.withMemoryRebound(to: integer_t.self, capacity: Int(count)) {
                host_statistics(host, HOST_CPU_LOAD_INFO, $0, &count)
            }
        }
        guard result == KERN_SUCCESS else {
            throw UsageProviderError.apiError(L10n.t("CPU counters unavailable"))
        }
        let ticks = info.cpu_ticks
        // CPU_STATE_USER, _SYSTEM, _IDLE, _NICE
        return Ticks(busy: ticks.0 &+ ticks.1 &+ ticks.3, idle: ticks.2)
    }
}

/// Memory pressure as macOS reports it: the ring is the kernel's
/// memory-status level and its colour is the pressure state itself, not a
/// threshold on "used" — macOS fills RAM with cache on purpose.
struct MemoryProvider: UsageProvider {
    let id = SystemProviders.memoryID
    let displayName = "Memory"
    let glyph = ProviderGlyph.memory
    let kind = ProviderKind.system
    var signInRoute: SignInRoute { .guidance("") }
    func account() -> ProviderAccount? { nil }

    private static let host = mach_host_self()

    func fetchSnapshot() async throws -> ProviderSnapshot {
        SystemProviders.snapshot(id: id, name: displayName, glyph: glyph, window: try Self.window())
    }

    static func window() throws -> LimitWindow {
        let total = Int64(ProcessInfo.processInfo.physicalMemory)
        let used = Self.usedBytes()
        let fraction: Double
        if let level = Self.sysctl("kern.memorystatus_level") {
            fraction = 1 - Double(level) / 100
        } else if let used, total > 0 {
            fraction = Double(used) / Double(total)
        } else {
            throw UsageProviderError.apiError(L10n.t("Memory readings unavailable"))
        }
        let (state, band) = Self.pressure()
        var detail = L10n.t("Pressure \(state)")
        if let used { detail += " · " + L10n.t("\(SystemProviders.bytes(used)) of \(SystemProviders.bytes(total)) used") }
        return LimitWindow(id: "pressure", label: L10n.t("Memory pressure"),
                           usedFraction: min(max(fraction, 0), 1),
                           detail: detail, bandOverride: band)
    }

    /// 1 normal, 2 warning, 4 critical (`DISPATCH_MEMORYPRESSURE_*`).
    private static func pressure() -> (String, UsageBand?) {
        switch sysctl("kern.memorystatus_vm_pressure_level") {
        case 1: return (L10n.t("normal"), .ample)
        case 2: return (L10n.t("warning"), .watch)
        case 4: return (L10n.t("critical"), .critical)
        default: return (L10n.t("unknown"), nil)
        }
    }

    /// Activity Monitor's "Memory Used": app memory + wired + compressed.
    private static func usedBytes() -> Int64? {
        var stats = vm_statistics64_data_t()
        var count = mach_msg_type_number_t(
            MemoryLayout<vm_statistics64_data_t>.stride / MemoryLayout<integer_t>.stride)
        let result = withUnsafeMutablePointer(to: &stats) {
            $0.withMemoryRebound(to: integer_t.self, capacity: Int(count)) {
                host_statistics64(host, HOST_VM_INFO64, $0, &count)
            }
        }
        guard result == KERN_SUCCESS else { return nil }
        let page = Int64(vm_kernel_page_size)
        let app = Int64(stats.internal_page_count) - Int64(stats.purgeable_count)
        let pages = max(app, 0) + Int64(stats.wire_count) + Int64(stats.compressor_page_count)
        return pages * page
    }

    private static func sysctl(_ name: String) -> Int32? {
        var value: Int32 = 0
        var size = MemoryLayout<Int32>.size
        guard sysctlbyname(name, &value, &size, nil, 0) == 0 else { return nil }
        return value
    }
}

/// One cell for every mounted local, writable, browsable volume. The internal
/// (startup) drive is the main ring; the first external drive is the thin
/// outer ring, drawn by Codenotch's second-ring support. Every drive is listed
/// in the hover card with how much is free, as Finder counts it. Volumes are
/// re-read on each refresh, so a drive plugged in later appears without a
/// relaunch and an ejected one drops out.
struct DisksProvider: UsageProvider {
    let id = SystemProviders.disksID
    let displayName = "Disks"
    let glyph = ProviderGlyph.disk
    let kind = ProviderKind.system
    var signInRoute: SignInRoute { .guidance("") }
    func account() -> ProviderAccount? { nil }

    private struct Volume {
        let window: LimitWindow
        let isInternal: Bool
        let isStartup: Bool
    }

    private static let keys: [URLResourceKey] = [
        .volumeIsLocalKey, .volumeIsBrowsableKey, .volumeIsReadOnlyKey, .volumeIsInternalKey,
        .volumeNameKey, .volumeUUIDStringKey, .volumeTotalCapacityKey,
        .volumeAvailableCapacityForImportantUsageKey, .volumeAvailableCapacityKey,
    ]

    func fetchSnapshot() async throws -> ProviderSnapshot {
        let volumes = Self.volumes()
        let internal = volumes.first(where: \.isStartup) ?? volumes.first(where: \.isInternal)
        guard let main = internal ?? volumes.first else {
            throw UsageProviderError.apiError(L10n.t("No local disks found"))
        }
        let outer = volumes.first { !$0.isInternal && $0.window.id != main.window.id }
        let ordered = [main] + volumes.filter { $0.window.id != main.window.id }
        return ProviderSnapshot(id: id, displayName: displayName, glyph: glyph, fidelity: .official,
                                status: .ok, windows: ordered.map(\.window),
                                headlineID: main.window.id, weeklyID: outer?.window.id,
                                kind: .system)
    }

    private static func volumes() -> [Volume] {
        let urls = FileManager.default.mountedVolumeURLs(
            includingResourceValuesForKeys: keys, options: [.skipHiddenVolumes]) ?? []
        return urls.compactMap { url in
            guard let values = try? url.resourceValues(forKeys: Set(keys)),
                  values.volumeIsLocal == true, values.volumeIsBrowsable == true,
                  values.volumeIsReadOnly != true,
                  let total = values.volumeTotalCapacity.map(Int64.init), total > 0
            else { return nil }
            let free = values.volumeAvailableCapacityForImportantUsage
                ?? values.volumeAvailableCapacity.map(Int64.init) ?? 0
            let used = max(total - free, 0)
            let name = values.volumeName ?? url.lastPathComponent
            let window = LimitWindow(
                id: "disk:" + (values.volumeUUIDString ?? url.path), label: name,
                usedFraction: min(Double(used) / Double(total), 1),
                detail: L10n.t("\(SystemProviders.bytes(free)) free of \(SystemProviders.bytes(total))"))
            return Volume(window: window, isInternal: values.volumeIsInternal == true,
                          isStartup: url.path == "/")
        }
    }
}
