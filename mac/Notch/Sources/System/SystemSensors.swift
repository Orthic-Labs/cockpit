import Foundation
import IOKit

/// Pulse fork: GPU utilisation and CPU/SoC temperature for the System hover
/// card. No root and no helper. GPU is the `IOAccelerator` registry entries'
/// `PerformanceStatistics`; temperature is the HID event system's thermal
/// sensors (private symbols, resolved by name, as Stats does on Apple
/// Silicon). Each reading is nil when the machine does not offer it, and the
/// card then simply omits the row.
enum SystemSensors {
    /// Busiest GPU's "Device Utilization %", 0...1.
    static func gpuUtilization() -> Double? {
        guard let matching = IOServiceMatching("IOAccelerator") else { return nil }
        var iterator: io_iterator_t = 0
        guard IOServiceGetMatchingServices(kIOMainPortDefault, matching, &iterator) == KERN_SUCCESS
        else { return nil }
        defer { IOObjectRelease(iterator) }
        var best: Double?
        var service = IOIteratorNext(iterator)
        while service != 0 {
            if let stats = IORegistryEntryCreateCFProperty(
                service, "PerformanceStatistics" as CFString, kCFAllocatorDefault, 0)?
                .takeRetainedValue() as? [String: Any],
                let value = (stats["Device Utilization %"] as? NSNumber)?.doubleValue {
                best = max(best ?? 0, min(max(value / 100, 0), 1))
            }
            IOObjectRelease(service)
            service = IOIteratorNext(iterator)
        }
        return best
    }

    /// Mean of the CPU/SoC die sensors, in °C.
    static func cpuTemperature() -> Double? {
        let values = HIDThermal.shared.temperatures(named: isDieSensor).map(\.celsius)
        guard !values.isEmpty else { return nil }
        return values.reduce(0, +) / Double(values.count)
    }

    /// Every named temperature sensor the HID system offers, in °C, with the
    /// sensor's own name. Read less often than the die mean (see `SystemExtras`).
    static func temperatures() -> [(name: String, celsius: Double)] {
        HIDThermal.shared.temperatures(named: { _ in true })
    }

    private static func isDieSensor(_ name: String) -> Bool {
        let lower = name.lowercased()
        return lower.contains("tdie") || lower.contains("soc mtr")
    }
}

@_silgen_name("IOHIDEventSystemClientCreate")
private func IOHIDEventSystemClientCreate(_ allocator: CFAllocator?) -> Unmanaged<AnyObject>?
@_silgen_name("IOHIDEventSystemClientSetMatching")
private func IOHIDEventSystemClientSetMatching(_ client: AnyObject, _ matching: CFDictionary) -> Int32
@_silgen_name("IOHIDEventSystemClientCopyServices")
private func IOHIDEventSystemClientCopyServices(_ client: AnyObject) -> Unmanaged<CFArray>?
@_silgen_name("IOHIDServiceClientCopyProperty")
private func IOHIDServiceClientCopyProperty(_ service: AnyObject, _ key: CFString) -> Unmanaged<CFTypeRef>?
@_silgen_name("IOHIDServiceClientCopyEvent")
private func IOHIDServiceClientCopyEvent(_ service: AnyObject, _ type: Int64, _ options: Int32,
                                         _ timestamp: Int64) -> Unmanaged<AnyObject>?
@_silgen_name("IOHIDEventGetFloatValue")
private func IOHIDEventGetFloatValue(_ event: AnyObject, _ field: Int32) -> Double

/// The thermal sensors found once (by name), read on demand. Called from the
/// System provider's actor only, one reading at a time.
private final class HIDThermal: @unchecked Sendable {
    static let shared = HIDThermal()

    private static let temperatureType: Int64 = 15      // kIOHIDEventTypeTemperature
    private var sensors: [(name: String, service: AnyObject)]?
    private var client: AnyObject?

    private func load() -> [(name: String, service: AnyObject)] {
        if let sensors { return sensors }
        var found: [(name: String, service: AnyObject)] = []
        if let created = IOHIDEventSystemClientCreate(kCFAllocatorDefault) {
            let client = created.takeRetainedValue()
            self.client = client
            // Apple vendor usage page 0xff00, usage 5: temperature sensors.
            let matching: [String: Any] = ["PrimaryUsagePage": 0xff00, "PrimaryUsage": 5]
            _ = IOHIDEventSystemClientSetMatching(client, matching as CFDictionary)
            if let list = IOHIDEventSystemClientCopyServices(client)?.takeRetainedValue() as? [AnyObject] {
                // Keep the name with each service. Batteries, NAND and board
                // points stay in the list; the caller picks what it reads.
                found = list.compactMap { service in
                    guard let name = IOHIDServiceClientCopyProperty(service, "Product" as CFString)?
                        .takeRetainedValue() as? String else { return nil }
                    return (name, service)
                }
            }
        }
        sensors = found
        return found
    }

    /// Readings, in °C, of the sensors whose name passes `include`. Names are
    /// checked before any event is copied, so a narrow filter reads little.
    func temperatures(named include: (String) -> Bool) -> [(name: String, celsius: Double)] {
        load().compactMap { sensor in
            guard include(sensor.name),
                  let event = IOHIDServiceClientCopyEvent(sensor.service, Self.temperatureType, 0, 0)?
                    .takeRetainedValue() else { return nil }
            let value = IOHIDEventGetFloatValue(event, Int32(Self.temperatureType << 16))
            guard value > 0, value < 150 else { return nil }
            return (sensor.name, value)
        }
    }
}
