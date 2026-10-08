import Darwin
import Foundation
import SystemConfiguration

/// Pulse fork: download and upload rate of the active network interface for
/// the System cell. Byte counters come from `getifaddrs` (the link-level
/// entry's `if_data`, as Stats reads them), the active interface is the
/// primary one SystemConfiguration names for IPv4, and its kind (Wi-Fi or
/// Ethernet) comes from the same framework's interface list. No process is
/// launched and nothing is written to disk.
struct NetworkThroughput {
    struct Rate {
        /// BSD name, such as "en0".
        let interface: String
        /// "Wi-Fi", "Ethernet", or the BSD name for anything else.
        let kind: String
        /// Bytes per second.
        let down: Double
        let up: Double
    }

    private struct Counters {
        let received: UInt32
        let sent: UInt32
    }

    private struct Last {
        let name: String
        let at: Date
        let counters: Counters
    }

    private let store = SCDynamicStoreCreate(nil, "dev.orthic.pulse.notch" as CFString, nil, nil)
    private var last: Last?
    private var kinds: [String: String] = [:]

    /// The rate since the previous call. Nil on the first call, after the
    /// active interface changes, when a counter went backwards, or when no
    /// interface is active.
    mutating func sample(now: Date) -> Rate? {
        guard let name = primaryInterface(), let counters = Self.counters(for: name) else {
            last = nil
            return nil
        }
        let previous = last
        last = Last(name: name, at: now, counters: counters)
        guard let previous, previous.name == name else { return nil }
        let elapsed = now.timeIntervalSince(previous.at)
        guard elapsed > 0 else { return nil }
        // 32-bit counters wrap; the difference is taken modulo 2^32. A step of
        // more than 2 GiB means the counter was reset, not a real burst.
        let received = counters.received &- previous.counters.received
        let sent = counters.sent &- previous.counters.sent
        guard received < 0x8000_0000, sent < 0x8000_0000 else { return nil }
        return Rate(interface: name, kind: kind(for: name),
                    down: Double(received) / elapsed, up: Double(sent) / elapsed)
    }

    private func primaryInterface() -> String? {
        guard let store,
              let global = SCDynamicStoreCopyValue(store, "State:/Network/Global/IPv4" as CFString)
                as? [String: Any]
        else { return nil }
        return global["PrimaryInterface"] as? String
    }

    /// The interface list is read only when an interface is not in the map
    /// yet, so the framework is not asked on every two-second sample.
    private mutating func kind(for name: String) -> String {
        if kinds[name] == nil {
            kinds = Self.interfaceKinds()
            if kinds[name] == nil { kinds[name] = name }
        }
        return kinds[name] ?? name
    }

    private static func interfaceKinds() -> [String: String] {
        guard let all = SCNetworkInterfaceCopyAll() as? [SCNetworkInterface] else { return [:] }
        var map: [String: String] = [:]
        for interface in all {
            guard let bsd = SCNetworkInterfaceGetBSDName(interface) as String?,
                  let type = SCNetworkInterfaceGetInterfaceType(interface) as String?
            else { continue }
            if type == kSCNetworkInterfaceTypeIEEE80211 as String {
                map[bsd] = L10n.t("Wi-Fi")
            } else if type == kSCNetworkInterfaceTypeEthernet as String {
                map[bsd] = L10n.t("Ethernet")
            }
        }
        return map
    }

    private static func counters(for name: String) -> Counters? {
        var head: UnsafeMutablePointer<ifaddrs>?
        guard getifaddrs(&head) == 0, let first = head else { return nil }
        defer { freeifaddrs(head) }
        var cursor: UnsafeMutablePointer<ifaddrs>? = first
        while let entry = cursor?.pointee {
            if let address = entry.ifa_addr, Int32(address.pointee.sa_family) == AF_LINK,
               let data = entry.ifa_data, String(cString: entry.ifa_name) == name {
                let stats = data.assumingMemoryBound(to: if_data.self).pointee
                return Counters(received: stats.ifi_ibytes, sent: stats.ifi_obytes)
            }
            cursor = entry.ifa_next
        }
        return nil
    }
}
