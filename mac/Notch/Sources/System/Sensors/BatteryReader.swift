import Foundation
import IOKit
import IOKit.ps

/// Pulse fork: the internal battery's charge, charging state, cycle count and
/// health, for the System sensors. Charge and state come from the power
/// source list (IOPowerSources); cycles and capacities come from the
/// AppleSmartBattery registry entry. A Mac without an internal battery returns
/// nil, and the rows are left out.
enum BatteryReader {
    struct Reading {
        /// 0...100.
        let percent: Int
        let charging: Bool
        let cycles: Int?
        /// Full-charge capacity as a share of the design capacity, 0...1.
        let health: Double?
    }

    static func read() -> Reading? {
        guard let charge = powerSourceCharge() else { return nil }
        let registry = registryFigures()
        return Reading(percent: charge.percent, charging: charge.charging,
                       cycles: registry.cycles, health: registry.health)
    }

    private static func powerSourceCharge() -> (percent: Int, charging: Bool)? {
        let info = IOPSCopyPowerSourcesInfo().takeRetainedValue()
        guard let sources = IOPSCopyPowerSourcesList(info)?.takeRetainedValue() as? [CFTypeRef] else {
            return nil
        }
        for source in sources {
            guard let description = IOPSGetPowerSourceDescription(info, source)?
                    .takeUnretainedValue() as? [String: Any],
                  description["Type"] as? String == "InternalBattery",
                  let current = (description["Current Capacity"] as? NSNumber)?.doubleValue,
                  let maximum = (description["Max Capacity"] as? NSNumber)?.doubleValue,
                  maximum > 0
            else { continue }
            let percent = min(max(Int((current / maximum * 100).rounded()), 0), 100)
            return (percent, description["Is Charging"] as? Bool ?? false)
        }
        return nil
    }

    /// Cycle count and health from the battery's own registry entry. Either
    /// may be missing on a machine that does not publish it.
    private static func registryFigures() -> (cycles: Int?, health: Double?) {
        let service = IOServiceGetMatchingService(kIOMainPortDefault,
                                                  IOServiceMatching("AppleSmartBattery"))
        guard service != 0 else { return (nil, nil) }
        defer { IOObjectRelease(service) }
        var properties: Unmanaged<CFMutableDictionary>?
        guard IORegistryEntryCreateCFProperties(service, &properties, kCFAllocatorDefault, 0)
                == KERN_SUCCESS,
              let dictionary = properties?.takeRetainedValue() as? [String: Any]
        else { return (nil, nil) }
        let cycles = (dictionary["CycleCount"] as? NSNumber).map(\.intValue)
        let design = (dictionary["DesignCapacity"] as? NSNumber)?.doubleValue ?? 0
        let full = ((dictionary["AppleRawMaxCapacity"] ?? dictionary["NominalChargeCapacity"])
            as? NSNumber)?.doubleValue ?? 0
        let health = design > 0 && full > 0 ? min(full / design, 1) : nil
        return (cycles, health)
    }
}
