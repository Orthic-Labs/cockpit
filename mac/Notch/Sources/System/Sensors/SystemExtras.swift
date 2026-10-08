import Foundation

/// Pulse fork: the readings the System cells show beyond CPU and memory, each
/// on its own cadence so the two-second sampler stays light. Network and fans
/// are read on every sample (network needs the previous counters; fans are a
/// few SMC calls). Battery is re-read every thirty seconds, the HID
/// temperatures every ten, and the Chrome snapshot count at most once a day
/// (the file is checked every fifteen minutes, and nothing is counted unless a
/// sample is due).
///
/// Owned by `SystemLoadProvider`, an actor, so its state is never shared.
struct SystemExtras {
    struct Reading {
        var network: NetworkThroughput.Rate?
        var battery: BatteryReader.Reading?
        var fans: [SMCFans.Fan]
        var temperatures: [(name: String, celsius: Double)]

        /// The hub's view, stored as `system` in notch-state.json. Keys with no
        /// reading are left out, so the hub can tell "absent" from zero.
        var hubState: [String: Any] {
            var state: [String: Any] = [:]
            if let network {
                let row: [String: Any] = ["interface": network.interface, "kind": network.kind,
                                          "down": network.down, "up": network.up]
                state["network"] = row
            }
            if let battery {
                var row: [String: Any] = ["percent": battery.percent, "charging": battery.charging]
                if let cycles = battery.cycles { row["cycles"] = cycles }
                if let health = battery.health { row["health"] = health }
                state["battery"] = row
            }
            if !fans.isEmpty {
                let rows: [[String: Any]] = fans.map { ["name": $0.name, "rpm": $0.rpm] }
                state["fans"] = rows
            }
            if !temperatures.isEmpty {
                let rows: [[String: Any]] = temperatures.prefix(40).map { ["name": $0.name, "celsius": $0.celsius] }
                state["temperatures"] = rows
            }
            return state
        }
    }

    private var network = NetworkThroughput()
    private var battery: (at: Date, value: BatteryReader.Reading?)?
    private var temperatures: (at: Date, value: [(name: String, celsius: Double)])?
    private var smc: SMCFans?
    private var smcOpened = false
    private var chromeCheckedAt: Date?

    mutating func sample(now: Date = Date()) -> Reading {
        if chromeCheckedAt.map({ now.timeIntervalSince($0) >= 900 }) ?? true {
            chromeCheckedAt = now
            ChromeSnapshotSampler.sampleIfDue(now: now)
        }
        let reading = Reading(
            network: network.sample(now: now),
            battery: batteryReading(now: now),
            fans: fanSpeeds(),
            temperatures: temperatureReadings(now: now))
        SystemReadingsStore.publish(reading.hubState)
        return reading
    }

    private mutating func batteryReading(now: Date) -> BatteryReader.Reading? {
        if let battery, now.timeIntervalSince(battery.at) < 30 { return battery.value }
        let value = BatteryReader.read()
        battery = (now, value)
        return value
    }

    private mutating func temperatureReadings(now: Date) -> [(name: String, celsius: Double)] {
        if let temperatures, now.timeIntervalSince(temperatures.at) < 10 { return temperatures.value }
        let value = SystemSensors.temperatures()
        temperatures = (now, value)
        return value
    }

    /// The SMC is opened once; a machine that refuses it is not asked again.
    private mutating func fanSpeeds() -> [SMCFans.Fan] {
        if !smcOpened {
            smcOpened = true
            smc = SMCFans()
        }
        return smc?.fans() ?? []
    }
}

/// The latest extra readings, published by the sampler and read by the hub
/// bridge when it writes `notch-state.json` (which it does every two seconds
/// while the hub is open).
enum SystemReadingsStore {
    private static let lock = NSLock()
    private static var latest: [String: Any] = [:]

    static func publish(_ state: [String: Any]) {
        lock.lock()
        latest = state
        lock.unlock()
    }

    static var current: [String: Any] {
        lock.lock()
        defer { lock.unlock() }
        return latest
    }
}
