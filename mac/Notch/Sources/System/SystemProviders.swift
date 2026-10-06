import Darwin
import Foundation

/// Cockpit fork: the machine's own readings as notch rings — memory
/// pressure and one ring per mounted local disk (CPU is available, unused). Each is a `UsageProvider` of
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
    static let diskPrefix = "system-disk:"

    static func isSystem(providerID: String) -> Bool { providerID.hasPrefix("system-") }

    /// Disks are discovered once, at launch; one mounted later gets its ring
    /// on the next launch.
    static func all() -> [UsageProvider] {
        // CPU is left out on purpose: memory pressure is the reading that
        // means something at a glance. `CPUProvider` stays for the hub.
        let fixed: [UsageProvider] = [MemoryProvider()]
        return fixed + DiskProvider.discover().map { $0 as UsageProvider }
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

/// Share of all cores busy since the previous reading.
actor CPUProvider: UsageProvider {
    nonisolated let id = SystemProviders.cpuID
    nonisolated let displayName = "CPU"
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
        let window = LimitWindow(id: "load", label: L10n.t("All cores"),
                                 usedFraction: min(max(busy / total, 0), 1),
                                 detail: L10n.t("\(Percent.text(for: busy / total))% busy · \(cores) cores"))
        return SystemProviders.snapshot(id: id, name: displayName, glyph: glyph, window: window)
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
        let window = LimitWindow(id: "pressure", label: L10n.t("Memory pressure"),
                                 usedFraction: min(max(fraction, 0), 1),
                                 detail: detail, bandOverride: band)
        return SystemProviders.snapshot(id: id, name: displayName, glyph: glyph, window: window)
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

/// One mounted local, writable, browsable volume. The ring is the share used;
/// the hover card says how much is free, as Finder counts it.
struct DiskProvider: UsageProvider {
    let id: String
    let displayName: String
    let url: URL
    let glyph = ProviderGlyph.disk
    let kind = ProviderKind.system
    var signInRoute: SignInRoute { .guidance("") }
    func account() -> ProviderAccount? { nil }

    func fetchSnapshot() async throws -> ProviderSnapshot {
        let values = try url.resourceValues(forKeys: [
            .volumeTotalCapacityKey, .volumeAvailableCapacityForImportantUsageKey,
            .volumeAvailableCapacityKey,
        ])
        guard let total = values.volumeTotalCapacity.map(Int64.init), total > 0 else {
            throw UsageProviderError.apiError(L10n.t("\(displayName) is unavailable"))
        }
        let free = values.volumeAvailableCapacityForImportantUsage
            ?? values.volumeAvailableCapacity.map(Int64.init) ?? 0
        let used = max(total - free, 0)
        let window = LimitWindow(
            id: "space", label: displayName,
            usedFraction: min(Double(used) / Double(total), 1),
            detail: L10n.t("\(SystemProviders.bytes(free)) free of \(SystemProviders.bytes(total))"))
        return SystemProviders.snapshot(id: id, name: displayName, glyph: glyph, window: window)
    }

    static func discover() -> [DiskProvider] {
        let keys: [URLResourceKey] = [
            .volumeIsLocalKey, .volumeIsBrowsableKey, .volumeIsReadOnlyKey,
            .volumeNameKey, .volumeUUIDStringKey, .volumeTotalCapacityKey,
        ]
        let urls = FileManager.default.mountedVolumeURLs(
            includingResourceValuesForKeys: keys, options: [.skipHiddenVolumes]) ?? []
        return urls.compactMap { url in
            guard let values = try? url.resourceValues(forKeys: Set(keys)),
                  values.volumeIsLocal == true, values.volumeIsBrowsable == true,
                  values.volumeIsReadOnly != true,
                  (values.volumeTotalCapacity ?? 0) > 0
            else { return nil }
            let name = values.volumeName ?? url.lastPathComponent
            let identity = values.volumeUUIDString ?? url.path
            return DiskProvider(id: SystemProviders.diskPrefix + identity, displayName: name, url: url)
        }
    }
}
