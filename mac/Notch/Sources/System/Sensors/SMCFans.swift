import Foundation
import IOKit

/// Pulse fork: fan speeds from the System Management Controller, read without
/// root. The AppleSMC user client answers reads (key info, then bytes) for the
/// `FNum` count and each `F<n>Ac` actual-speed key. A key that does not read,
/// or a machine without an SMC, yields no fans, and the rows are left out.
///
/// The call takes the kernel's 80-byte `SMCKeyData` record. Its fields sit at
/// fixed offsets, written and read by hand so no Swift struct layout is assumed.
final class SMCFans {
    struct Fan {
        let name: String
        let rpm: Double
    }

    private static let recordSize = 80
    private static let selectorRead: UInt32 = 5      // kSMCReadKey
    private static let commandKeyInfo: UInt8 = 9
    private static let commandRead: UInt8 = 5

    // Offsets inside the record.
    private static let keyOffset = 0
    private static let dataSizeOffset = 28
    private static let dataTypeOffset = 32
    private static let resultOffset = 40
    private static let command8Offset = 42
    private static let bytesOffset = 48

    private var connection: io_connect_t = 0

    init?() {
        let service = IOServiceGetMatchingService(kIOMainPortDefault, IOServiceMatching("AppleSMC"))
        guard service != 0 else { return nil }
        defer { IOObjectRelease(service) }
        guard IOServiceOpen(service, mach_task_self_, 0, &connection) == KERN_SUCCESS else { return nil }
    }

    deinit {
        if connection != 0 { IOServiceClose(connection) }
    }

    /// Fans in SMC order, or nothing when the count key is unreadable.
    func fans() -> [Fan] {
        guard let count = read("FNum"), let first = count.bytes.first, first > 0 else { return [] }
        return (0..<Int(min(first, 8))).compactMap { index -> Fan? in
            guard let value = read("F\(index)Ac"), let rpm = Self.speed(value),
                  rpm >= 0, rpm < 20_000 else { return nil }
            return Fan(name: L10n.t("Fan \(index + 1)"), rpm: rpm)
        }
    }

    // MARK: - Reading keys

    private struct Value {
        let type: String
        let bytes: [UInt8]
    }

    private func read(_ key: String) -> Value? {
        guard let code = Self.fourCC(key) else { return nil }
        // Key info first: the size and type the value is stored as.
        var request = [UInt8](repeating: 0, count: Self.recordSize)
        Self.store(code, at: Self.keyOffset, in: &request)
        request[Self.command8Offset] = Self.commandKeyInfo
        guard let info = call(request), info[Self.resultOffset] == 0 else { return nil }
        let size = Int(Self.load(info, at: Self.dataSizeOffset))
        let type = Self.load(info, at: Self.dataTypeOffset)
        guard size > 0, size <= 32 else { return nil }

        var fetch = [UInt8](repeating: 0, count: Self.recordSize)
        Self.store(code, at: Self.keyOffset, in: &fetch)
        Self.store(UInt32(size), at: Self.dataSizeOffset, in: &fetch)
        fetch[Self.command8Offset] = Self.commandRead
        guard let output = call(fetch), output[Self.resultOffset] == 0 else { return nil }
        let typeName = Self.string(type)
        return Value(type: typeName,
                     bytes: Array(output[Self.bytesOffset..<(Self.bytesOffset + size)]))
    }

    private func call(_ input: [UInt8]) -> [UInt8]? {
        var inputCopy = input
        var output = [UInt8](repeating: 0, count: Self.recordSize)
        var outputSize = Self.recordSize
        let status = inputCopy.withUnsafeMutableBytes { inBuffer in
            output.withUnsafeMutableBytes { outBuffer in
                IOConnectCallStructMethod(connection, Self.selectorRead,
                                          inBuffer.baseAddress, Self.recordSize,
                                          outBuffer.baseAddress, &outputSize)
            }
        }
        guard status == KERN_SUCCESS else { return nil }
        return output
    }

    /// Actual speed in RPM from the value's type: `flt ` (Apple silicon,
    /// little-endian Float) or `fpe2` (Intel, unsigned 14.2 fixed point).
    private static func speed(_ value: Value) -> Double? {
        switch value.type {
        case "flt ":
            guard value.bytes.count >= 4 else { return nil }
            let bits = value.bytes.prefix(4).enumerated().reduce(UInt32(0)) { sum, pair in
                sum | (UInt32(pair.element) << (8 * UInt32(pair.offset)))
            }
            return Double(Float(bitPattern: bits))
        case "fpe2":
            guard value.bytes.count >= 2 else { return nil }
            return Double(Int(value.bytes[0]) << 8 | Int(value.bytes[1])) / 4
        default:
            return nil
        }
    }

    // MARK: - Byte helpers

    private static func fourCC(_ key: String) -> UInt32? {
        let bytes = Array(key.utf8)
        guard bytes.count == 4 else { return nil }
        return bytes.reduce(UInt32(0)) { ($0 << 8) | UInt32($1) }
    }

    private static func string(_ code: UInt32) -> String {
        let bytes = [UInt8(code >> 24), UInt8((code >> 16) & 0xff), UInt8((code >> 8) & 0xff), UInt8(code & 0xff)]
        return String(bytes: bytes, encoding: .ascii) ?? ""
    }

    private static func store(_ value: UInt32, at offset: Int, in buffer: inout [UInt8]) {
        for index in 0..<4 {
            buffer[offset + index] = UInt8((value >> (8 * UInt32(index))) & 0xff)
        }
    }

    private static func load(_ buffer: [UInt8], at offset: Int) -> UInt32 {
        (0..<4).reduce(UInt32(0)) { sum, index in
            sum | (UInt32(buffer[offset + index]) << (8 * UInt32(index)))
        }
    }
}
