import Darwin
import Foundation

/// Cockpit fork: drive health for the Disks hover card, read from
/// smartmontools. `smartctl -a -j` runs as a subprocess every ten minutes, off
/// every actor the two-second timer touches; the card only ever reads what the
/// last run left behind. A drive the connection hides (a USB enclosure
/// without SMART passthrough) says so and keeps its last good reading, with
/// its date, in `~/Library/Application Support/Cockpit/drive-health.json`.
actor DriveHealth {
    static let shared = DriveHealth()

    struct Reading: Codable, Equatable {
        var passed: Bool?
        var temperature: Double?
        var wearPercent: Int?
        var writtenBytes: Int64?
        var powerOnHours: Int?
        var criticalWarning: Int?
        var mediaErrors: Int?
        var date: Date

        var isWarning: Bool {
            passed == false || (criticalWarning ?? 0) != 0 || (mediaErrors ?? 0) > 0
        }
    }

    private struct Disk {
        var last: Reading?
        var unavailable = false
    }

    private static let interval: TimeInterval = 600
    private static let retry: TimeInterval = 60
    private static let searchPaths = [
        "/opt/homebrew/bin/smartctl", "/usr/local/bin/smartctl",
        Bundle.main.bundlePath + "/Contents/Helpers/smartctl",
    ]

    private var disks: [String: Disk] = [:]
    private var wholeByDevice: [String: String] = [:]
    private var lastRun: Date?
    private var running = false
    private var smartctlMissing = false

    private init() {
        if let data = try? Data(contentsOf: Self.store) {
            let decoder = JSONDecoder()
            decoder.dateDecodingStrategy = .iso8601
            if let saved = try? decoder.decode([String: Reading].self, from: data) {
                disks = saved.mapValues { Disk(last: $0) }
            }
        }
    }

    /// Hover rows for the given volumes (BSD device node and display name),
    /// the temperature per volume device for its name line, and a nudge for
    /// the sampler when a run is due.
    func rows(for volumes: [(device: String, name: String)])
        -> (rows: [LimitWindow], temperatures: [String: String]) {
        refreshIfNeeded(devices: volumes.map(\.device))
        if smartctlMissing {
            return ([LimitWindow(id: "health:install", label: L10n.t("Drive health"),
                                 detail: L10n.t("Install smartmontools for drive health"))], [:])
        }
        var seen = Set<String>()
        var rows: [LimitWindow] = []
        var temperatures: [String: String] = [:]
        for volume in volumes {
            guard let whole = wholeByDevice[volume.device], let disk = disks[whole] else { continue }
            if !disk.unavailable, let celsius = disk.last?.temperature {
                temperatures[volume.device] = Self.temperatureText(celsius)
            }
            guard seen.insert(whole).inserted else { continue }
            rows += Self.rows(whole: whole, name: volume.name, disk: disk)
        }
        return (rows, temperatures)
    }

    /// One plain line per drive (empty label), temperature excluded: it sits
    /// on the drive's name line.
    private static func rows(whole: String, name: String, disk: Disk) -> [LimitWindow] {
        var rows: [LimitWindow] = []
        if disk.unavailable {
            rows.append(LimitWindow(id: "health:\(whole):a", label: "",
                                    detail: "\(name): " + L10n.t("Health n/a over USB")))
            if let last = disk.last {
                rows.append(LimitWindow(
                    id: "health:\(whole):last", label: "",
                    detail: L10n.t("Last reading \(last.date.formatted(date: .abbreviated, time: .omitted))")
                        + ": " + summary(last)))
            }
        } else if let last = disk.last {
            var parts = [last.isWarning ? L10n.t("Health Warning") : L10n.t("Health OK")]
            if let percent = last.wearPercent { parts.append(L10n.t("\(percent)% worn")) }
            if let written = last.writtenBytes {
                parts.append(L10n.t("\(ByteCountFormatter.string(fromByteCount: written, countStyle: .decimal)) written"))
            }
            rows.append(LimitWindow(id: "health:\(whole):a", label: "",
                                    detail: "\(name): " + parts.joined(separator: " · ")))
        }
        return rows
    }

    private static func summary(_ reading: Reading) -> String {
        var parts = [reading.isWarning ? L10n.t("Warning") : L10n.t("OK")]
        if let temperature = reading.temperature { parts.append(temperatureText(temperature)) }
        if let percent = reading.wearPercent { parts.append(L10n.t("\(percent)% worn")) }
        return parts.joined(separator: " · ")
    }

    static func temperatureText(_ celsius: Double) -> String {
        "\(Int(celsius.rounded())) °C"
    }

    // MARK: Sampling

    private func refreshIfNeeded(devices: [String]) {
        guard !running else { return }
        let now = Date()
        let unknown = devices.contains { wholeByDevice[$0] == nil }
        if let lastRun {
            let age = now.timeIntervalSince(lastRun)
            guard age >= Self.interval || (unknown && age >= Self.retry) else { return }
        }
        running = true
        lastRun = now
        let known = wholeByDevice
        Task.detached(priority: .utility) { [devices] in
            let result = Self.sample(devices: devices, known: known)
            await DriveHealth.shared.apply(result)
        }
    }

    private struct Sample {
        var wholeByDevice: [String: String]
        var outcomes: [String: Reading?]   // nil reading: unavailable
        var missing: Bool
    }

    private func apply(_ sample: Sample) {
        running = false
        wholeByDevice.merge(sample.wholeByDevice) { _, new in new }
        smartctlMissing = sample.missing
        for (whole, outcome) in sample.outcomes {
            var disk = disks[whole] ?? Disk()
            if let reading = outcome {
                disk.last = reading
                disk.unavailable = false
            } else {
                disk.unavailable = true
            }
            disks[whole] = disk
        }
        persist()
    }

    private nonisolated static func sample(devices: [String], known: [String: String]) -> Sample {
        var map = known
        var outcomes: [String: Reading?] = [:]
        guard let tool = searchPaths.first(where: { FileManager.default.isExecutableFile(atPath: $0) }) else {
            return Sample(wholeByDevice: map, outcomes: [:], missing: true)
        }
        for device in devices where map[device] == nil {
            if let whole = wholeDisk(of: device) { map[device] = whole }
        }
        for whole in Set(devices.compactMap { map[$0] }) {
            // updateValue: a nil reading is a result, not a removal.
            outcomes.updateValue(read(tool: tool, whole: whole), forKey: whole)
        }
        return Sample(wholeByDevice: map, outcomes: outcomes, missing: false)
    }

    /// The physical disk under a volume. An APFS volume lives on a synthesized
    /// container (disk3), whose physical store (disk0s2) is what smartctl can
    /// ask; otherwise the parent whole disk is the answer.
    private nonisolated static func wholeDisk(of device: String) -> String? {
        guard let data = run("/usr/sbin/diskutil", ["info", "-plist", device], timeout: 10),
              let plist = (try? PropertyListSerialization.propertyList(from: data, format: nil))
                as? [String: Any] else { return nil }
        if let stores = plist["APFSPhysicalStores"] as? [[String: Any]],
           let store = stores.first?["APFSPhysicalStore"] as? String {
            return wholeName(store)
        }
        if let parent = plist["ParentWholeDisk"] as? String { return parent }
        return (plist["DeviceIdentifier"] as? String).map(wholeName)
    }

    /// "disk0s2" -> "disk0".
    private nonisolated static func wholeName(_ identifier: String) -> String {
        guard let range = identifier.range(of: #"^disk\d+"#, options: .regularExpression) else {
            return identifier
        }
        return String(identifier[range])
    }

    /// Nil when the connection does not carry SMART at all.
    private nonisolated static func read(tool: String, whole: String) -> Reading? {
        guard let data = run(tool, ["-a", "-j", "/dev/" + whole], timeout: 30),
              let json = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any],
              let status = (json["smart_status"] as? [String: Any])?["passed"] as? Bool
        else { return nil }
        var reading = Reading(passed: status, date: Date())
        reading.temperature = ((json["temperature"] as? [String: Any])?["current"] as? NSNumber)?.doubleValue
        reading.powerOnHours = ((json["power_on_time"] as? [String: Any])?["hours"] as? NSNumber)?.intValue
        if let nvme = json["nvme_smart_health_information_log"] as? [String: Any] {
            reading.wearPercent = (nvme["percentage_used"] as? NSNumber)?.intValue
            reading.writtenBytes = (nvme["data_units_written"] as? NSNumber).map { $0.int64Value * 512_000 }
            reading.criticalWarning = (nvme["critical_warning"] as? NSNumber)?.intValue
            reading.mediaErrors = (nvme["media_errors"] as? NSNumber)?.intValue
            if reading.temperature == nil {
                reading.temperature = (nvme["temperature"] as? NSNumber)?.doubleValue
            }
        } else if let table = (json["ata_smart_attributes"] as? [String: Any])?["table"] as? [[String: Any]] {
            for attribute in table {
                let name = attribute["name"] as? String ?? ""
                let normalized = ((attribute["value"] as? NSNumber)?.intValue)
                let raw = ((attribute["raw"] as? [String: Any])?["value"] as? NSNumber)?.int64Value
                switch name {
                case "Wear_Leveling_Count", "Media_Wearout_Indicator", "SSD_Life_Left", "Percent_Lifetime_Remain":
                    if let normalized, (0...100).contains(normalized) { reading.wearPercent = 100 - normalized }
                case "Total_LBAs_Written":
                    reading.writtenBytes = raw.map { $0 * 512 }
                case "Reallocated_Sector_Ct", "Reported_Uncorrect":
                    if let raw, raw > 0 { reading.mediaErrors = (reading.mediaErrors ?? 0) + Int(raw) }
                default: break
                }
            }
        }
        return reading
    }

    /// Runs a tool and returns its stdout, nil on launch failure or timeout.
    /// smartctl's exit status is a bit mask of findings, so it is ignored; the
    /// caller judges the output.
    private nonisolated static func run(_ path: String, _ arguments: [String], timeout: TimeInterval) -> Data? {
        let process = Process()
        process.executableURL = URL(fileURLWithPath: path)
        process.arguments = arguments
        let pipe = Pipe()
        process.standardOutput = pipe
        process.standardError = FileHandle.nullDevice
        do { try process.run() } catch { return nil }
        let timer = DispatchWorkItem { if process.isRunning { process.terminate() } }
        DispatchQueue.global().asyncAfter(deadline: .now() + timeout, execute: timer)
        let data = pipe.fileHandleForReading.readDataToEndOfFile()
        process.waitUntilExit()
        timer.cancel()
        return process.terminationReason == .exit ? data : nil
    }

    // MARK: Persistence

    private static var store: URL {
        FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent("Library/Application Support/Cockpit/drive-health.json")
    }

    private func persist() {
        let saved = disks.compactMapValues(\.last)
        let encoder = JSONEncoder()
        encoder.dateEncodingStrategy = .iso8601
        guard let data = try? encoder.encode(saved) else { return }
        try? FileManager.default.createDirectory(
            at: Self.store.deletingLastPathComponent(), withIntermediateDirectories: true)
        try? data.write(to: Self.store, options: .atomic)
    }
}
