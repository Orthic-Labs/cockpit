import Foundation
import IOKit
import IOKit.ps

/// Read-only battery and power-adapter details from macOS power sources and
/// the bounded `AppleSmartBattery` registry entry.
///
/// This helper owns no sampler or timer. Values are omitted when unavailable;
/// the corresponding field always carries a reason describing that state.
public enum NativePowerDetails {
    private static let maximumBatteryServices = 4

    /// Returns one bounded snapshot. Numeric values are accepted only when
    /// finite and within the documented unit range for that measurement.
    public static func reading() -> [String: Any] {
        let registry = batteryRegistryProperties()
        let powerSources = powerSourceDetails()

        var result: [String: Any] = [
            "schemaVersion": 1,
            "available": registry != nil || powerSources.sourceKnown,
            "supported": true
        ]

        let external = externalPowerMetric(registry: registry, powerSources: powerSources)
        result["externalPower"] = external.metric
        if let value = external.value { result["externalPowerConnected"] = value }

        let design = capacityMetric(registry?["DesignCapacity"], field: "designCapacity")
        let max = maximumCapacityMetric(registry)
        result["designCapacity"] = design.metric
        result["maxCapacity"] = max.metric
        if let value = design.value { result["designCapacityValue"] = value }
        if let value = max.value { result["maxCapacityValue"] = value }

        let health = healthMetric(registry: registry, powerSource: powerSources.battery,
                                  design: design.value, maximum: max.value,
                                  maximumSupportsRatio: max.metric["healthRatioEligible"] as? Bool == true)
        result["health"] = health
        if let value = health["percent"] as? Double { result["healthPercent"] = value }

        let cycles = nonNegativeIntegerMetric(registry?["CycleCount"], field: "cycleCount", unit: "cycles")
        result["cycleCount"] = cycles.metric
        if let value = cycles.value { result["cycles"] = value }

        let temperature = temperatureMetric(registry: registry, powerSource: powerSources.battery)
        result["temperature"] = temperature.metric
        if let value = temperature.value { result["temperatureC"] = value }

        let adapter = adapterWattsMetric(powerSources.adapter)
        result["powerAdapter"] = adapter.metric
        if let value = adapter.value { result["adapterWatts"] = value }

        let batteryPower = batteryPowerMetric(registry: registry)
        result["batteryPower"] = batteryPower.metric
        if let value = batteryPower.value { result["batteryWatts"] = value }

        if let charging = boolMetric(registry?["IsCharging"], field: "charging") {
            result["charging"] = charging
        }

        if registry == nil && !powerSources.sourceKnown {
            result["reason"] = "battery_and_power_source_unavailable"
        }
        return result
    }

    private struct NumberMetric {
        let value: Double?
        let metric: [String: Any]
    }

    private struct SourceDetails {
        let sourceKnown: Bool
        let battery: [String: Any]?
        let adapter: [String: Any]?
        let externalPower: Bool?
    }

    private static func batteryRegistryProperties() -> [String: Any]? {
        guard let matching = IOServiceMatching("AppleSmartBattery") else { return nil }
        var iterator: io_iterator_t = 0
        guard IOServiceGetMatchingServices(kIOMainPortDefault, matching, &iterator) == KERN_SUCCESS else {
            return nil
        }
        defer { IOObjectRelease(iterator) }

        var inspected = 0
        var service = IOIteratorNext(iterator)
        while service != 0 && inspected < maximumBatteryServices {
            inspected += 1
            var unmanaged: Unmanaged<CFMutableDictionary>?
            let status = IORegistryEntryCreateCFProperties(service, &unmanaged, kCFAllocatorDefault, 0)
            IOObjectRelease(service)
            if status == KERN_SUCCESS, let unmanaged {
                let properties = unmanaged.takeRetainedValue() as NSDictionary
                if let result = properties as? [String: Any] { return result }
            }
            service = IOIteratorNext(iterator)
        }
        if service != 0 { IOObjectRelease(service) }
        return nil
    }

    private static func powerSourceDetails() -> SourceDetails {
        guard let info = IOPSCopyPowerSourcesInfo()?.takeRetainedValue(),
              let sources = IOPSCopyPowerSourcesList(info)?.takeRetainedValue() as? [CFTypeRef] else {
            return SourceDetails(sourceKnown: false, battery: nil, adapter: nil, externalPower: nil)
        }
        var battery: [String: Any]?
        var sourceKnown = false
        var external: Bool?

        for source in sources.prefix(maximumBatteryServices) {
            guard let description = IOPSGetPowerSourceDescription(info, source)?.takeUnretainedValue()
                    as? [String: Any] else { continue }
            sourceKnown = true
            if description[kIOPSTypeKey] as? String == kIOPSInternalBatteryType {
                battery = description
            }
            if let state = description[kIOPSPowerSourceStateKey] as? String {
                if state == kIOPSACPowerValue { external = true }
                if state == kIOPSBatteryPowerValue { external = false }
            }
        }

        var adapter: [String: Any]?
        if let copied = IOPSCopyExternalPowerAdapterDetails()?.takeRetainedValue() {
            adapter = copied as? [String: Any]
            if adapter != nil { sourceKnown = true }
        }
        return SourceDetails(sourceKnown: sourceKnown, battery: battery, adapter: adapter,
                             externalPower: external)
    }

    private static func externalPowerMetric(registry: [String: Any]?,
                                            powerSources: SourceDetails) -> (metric: [String: Any], value: Bool?) {
        if let raw = registry?["ExternalConnected"] {
            guard let value = raw as? Bool else {
                return (unavailable(supported: true, reason: "malformed_registry_external_power"), nil)
            }
            return (["supported": true, "available": true, "value": value, "unit": "boolean"], value)
        }
        if let value = powerSources.externalPower {
            return (["supported": true, "available": true, "value": value, "unit": "boolean",
                     "source": "IOPS power source state"], value)
        }
        return (unavailable(supported: false, reason: "external_power_unavailable"), nil)
    }

    private static func capacityMetric(_ raw: Any?, field: String) -> NumberMetric {
        guard let raw else {
            return NumberMetric(value: nil, metric: unavailable(supported: false, reason: "\(field)_unsupported"))
        }
        guard let value = positiveInteger(raw), value <= 1_000_000 else {
            return NumberMetric(value: nil, metric: unavailable(supported: true, reason: "malformed_registry_\(field)"))
        }
        // IOPSKeys.h leaves capacity units software-defined. Keep raw
        // AppleSmartBattery values explicit as native units until a qualified
        // capacity mode is published; health ratios still compare like fields.
        return NumberMetric(value: value, metric: ["supported": true, "available": true,
                                                   "value": value, "unit": "native_capacity_units",
                                                   "unitsQualified": false,
                                                   "healthRatioEligible": true,
                                                   "source": "AppleSmartBattery registry \(field)"])
    }

    private static func maximumCapacityMetric(_ registry: [String: Any]?) -> NumberMetric {
        guard let registry else {
            return NumberMetric(value: nil, metric: unavailable(supported: false, reason: "maxCapacity_unsupported"))
        }
        // AppleSmartBattery's raw full-charge fields are preferred. The
        // public IOPS `Max Capacity` key is commonly normalized to percent;
        // never divide that value by a design capacity as if it were mAh.
        for key in ["AppleRawMaxCapacity", "NominalChargeCapacity", "FullChargeCapacity"] {
            if let raw = registry[key] {
                let metric = capacityMetric(raw, field: "maxCapacity")
                if metric.value != nil { return metric }
                return metric
            }
        }
        if let raw = registry["MaxCapacity"] {
            guard let value = positiveInteger(raw), value > 100 else {
                return NumberMetric(value: nil, metric: unavailable(
                    supported: true, reason: "maxCapacity_normalized_percent_or_malformed"))
            }
            // No public SDK contract qualifies this registry field's units.
            // Keep its value visible only as native units; it cannot support
            // a capacity-health ratio without a qualified physical unit.
            return NumberMetric(value: value, metric: [
                "supported": true, "available": true, "value": value,
                "unit": "native_capacity_units", "unitsQualified": false,
                "healthRatioEligible": false,
                "source": "AppleSmartBattery registry MaxCapacity"
            ])
        }
        return NumberMetric(value: nil, metric: unavailable(supported: false, reason: "maxCapacity_unsupported"))
    }

    private static func healthMetric(registry: [String: Any]?, powerSource: [String: Any]?,
                                     design: Double?, maximum: Double?,
                                     maximumSupportsRatio: Bool) -> [String: Any] {
        if let design, let maximum, maximumSupportsRatio, design > 0, maximum > 0 {
            let percent = min(100.0, (maximum / design) * 100.0)
            guard percent.isFinite else {
                return unavailable(supported: true, reason: "malformed_capacity_health")
            }
            return ["supported": true, "available": true, "percent": percent, "unit": "percent",
                    "interpretation": "full_charge_capacity_over_design_capacity_in_same_native_capacity_units_clamped_to_100"]
        }
        let hasCapacityField = ["DesignCapacity", "MaxCapacity", "AppleRawMaxCapacity",
                                "NominalChargeCapacity", "FullChargeCapacity"]
            .contains { registry?[$0] != nil }
        if hasCapacityField {
            return unavailable(supported: true, reason: "capacity_health_unavailable_or_malformed")
        }
        if let condition = registry?[kIOPSBatteryHealthConditionKey] as? String, !condition.isEmpty {
            return ["supported": true, "available": true, "condition": condition,
                    "interpretation": "system_reported_battery_health_condition"]
        }
        if let condition = powerSource?[kIOPSBatteryHealthConditionKey] as? String, !condition.isEmpty {
            return ["supported": true, "available": true, "condition": condition,
                    "interpretation": "system_reported_battery_health_condition"]
        }
        return unavailable(supported: false, reason: "battery_health_unsupported")
    }

    private static func nonNegativeIntegerMetric(_ raw: Any?, field: String, unit: String) -> NumberMetric {
        guard let raw else {
            return NumberMetric(value: nil, metric: unavailable(supported: false, reason: "\(field)_unsupported"))
        }
        guard let value = positiveOrZeroInteger(raw), value <= 1_000_000 else {
            return NumberMetric(value: nil, metric: unavailable(supported: true, reason: "malformed_registry_\(field)"))
        }
        return NumberMetric(value: value, metric: ["supported": true, "available": true,
                                                   "value": value, "unit": unit])
    }

    private static func temperatureMetric(registry: [String: Any]?,
                                          powerSource: [String: Any]?) -> NumberMetric {
        if let raw = registry?["Temperature"] {
            guard let centiCelsius = number(raw), centiCelsius.isFinite,
                  (-4_000...15_000).contains(centiCelsius) else {
                return NumberMetric(value: nil, metric: unavailable(supported: true,
                                                                     reason: "malformed_registry_temperature"))
            }
            let value = centiCelsius / 100.0
            return NumberMetric(value: value, metric: ["supported": true, "available": true,
                                                       "value": value, "unit": "celsius",
                                                       "source": "AppleSmartBattery registry"])
        }
        if let raw = powerSource?[kIOPSTemperatureKey] {
            guard let value = number(raw), value.isFinite, (-40...150).contains(value) else {
                return NumberMetric(value: nil, metric: unavailable(supported: true,
                                                                     reason: "malformed_power_source_temperature"))
            }
            return NumberMetric(value: value, metric: ["supported": true, "available": true,
                                                       "value": value, "unit": "celsius",
                                                       "source": "IOPS power source"])
        }
        return NumberMetric(value: nil, metric: unavailable(supported: false, reason: "temperature_unsupported"))
    }

    private static func adapterWattsMetric(_ adapter: [String: Any]?) -> NumberMetric {
        guard let adapter else {
            return NumberMetric(value: nil, metric: unavailable(supported: false, reason: "adapter_watts_unsupported"))
        }
        guard let raw = adapter[kIOPSPowerAdapterWattsKey] else {
            return NumberMetric(value: nil, metric: unavailable(supported: false, reason: "adapter_watts_unsupported"))
        }
        guard let value = number(raw), value.isFinite, (0..<1_000).contains(value) else {
            return NumberMetric(value: nil, metric: unavailable(supported: true,
                                                                 reason: "malformed_power_adapter_watts"))
        }
        return NumberMetric(value: value, metric: ["supported": true, "available": true,
                                                   "value": value, "unit": "watts"])
    }

    private static func batteryPowerMetric(registry: [String: Any]?) -> NumberMetric {
        guard let registry else {
            return NumberMetric(value: nil, metric: unavailable(supported: false, reason: "battery_power_unsupported"))
        }
        guard let voltageRaw = registry["Voltage"], let currentRaw = registry["Amperage"] ?? registry["InstantAmperage"],
              let voltage = number(voltageRaw), let current = signedElectricalValue(currentRaw),
              voltage.isFinite, current.isFinite, (0..<100_000).contains(voltage), abs(current) < 100_000 else {
            return NumberMetric(value: nil, metric: unavailable(supported: true, reason: "malformed_battery_power"))
        }
        let watts = voltage * current / 1_000_000.0
        guard watts.isFinite, abs(watts) <= 10_000 else {
            return NumberMetric(value: nil, metric: unavailable(supported: true, reason: "malformed_battery_power"))
        }
        return NumberMetric(value: watts, metric: ["supported": true, "available": true,
                                                   "value": watts, "unit": "watts",
                                                   "interpretation": "battery_voltage_millivolts_times_signed_current_milliamps"])
    }

    private static func boolMetric(_ raw: Any?, field: String) -> Bool? {
        guard let raw else { return nil }
        guard let value = raw as? Bool else { return nil }
        _ = field
        return value
    }

    private static func unavailable(supported: Bool, reason: String) -> [String: Any] {
        ["supported": supported, "available": false, "reason": reason]
    }

    private static func number(_ raw: Any) -> Double? {
        if let value = raw as? NSNumber {
            guard CFGetTypeID(value) != CFBooleanGetTypeID() else { return nil }
            let result = value.doubleValue
            return result.isFinite ? result : nil
        }
        if raw is Bool { return nil }
        if let value = raw as? Int { return Double(value) }
        if let value = raw as? Int64 { return Double(value) }
        if let value = raw as? UInt { return Double(value) }
        if let value = raw as? UInt64 { return Double(value) }
        if let value = raw as? Double { return value.isFinite ? value : nil }
        if let value = raw as? Float { return value.isFinite ? Double(value) : nil }
        return nil
    }

    /// Reads signed electrical current without losing a negative value that
    /// arrived as a two's-complement UInt64 through an IOKit NSNumber.
    private static func signedElectricalValue(_ raw: Any) -> Double? {
        if let value = raw as? NSNumber {
            guard CFGetTypeID(value) != CFBooleanGetTypeID() else { return nil }
            let type = String(cString: value.objCType)
            let signed: Int64
            switch type {
            case "Q", "L":
                signed = Int64(bitPattern: value.uint64Value)
            default:
                signed = value.int64Value
            }
            return Double(signed)
        }
        if raw is Bool { return nil }
        if let value = raw as? Int { return Double(value) }
        if let value = raw as? Int64 { return Double(value) }
        if let value = raw as? UInt64 { return Double(Int64(bitPattern: value)) }
        if let value = raw as? UInt { return Double(Int64(bitPattern: UInt64(value))) }
        if let value = raw as? Double { return value.isFinite ? value : nil }
        if let value = raw as? Float { return value.isFinite ? Double(value) : nil }
        return nil
    }

    private static func positiveInteger(_ raw: Any) -> Double? {
        guard let value = number(raw), value.isFinite, value > 0, value.rounded() == value else { return nil }
        return value
    }

    private static func positiveOrZeroInteger(_ raw: Any) -> Double? {
        guard let value = number(raw), value.isFinite, value >= 0, value.rounded() == value else { return nil }
        return value
    }
}
