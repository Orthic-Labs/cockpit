// SPDX-License-Identifier: GPL-3.0-or-later
// Cockpit adaptation of Vorssaint's SuperKey mapping code
// (SuperKeySupport.swift, SuperKeyMappingGuard.swift).
// Copyright (C) 2026 Vorssaint

import Foundation

/// Cockpit: remaps the Fn/Globe key to F18 with `hidutil`, so macOS never sees
/// Fn and its Globe shortcuts (Fn+C is Control Center) cannot fire. The event
/// tap in `FnCommand` then gives F18 its meaning. Entries already in the
/// keyboards' `UserKeyMapping` are kept; only Cockpit's own are added/removed.
enum FnKeyMapping {
    struct Entry: Equatable {
        let source: UInt64
        let destination: UInt64
    }

    /// Fn on Apple's vendor top-case page and on the vendor keyboard page.
    static let fnSources: [UInt64] = [0xFF_0000_0003, 0xFF01_0000_0003]
    /// F18, delivered like any other key and absent from portable keyboards.
    static let f18: UInt64 = 0x7_0000_006D
    static let f18KeyCode: Int64 = 79

    private static let hidutil = "/usr/bin/hidutil"
    private static var guardProcess: Process?
    private static var guardInput: FileHandle?

    static var ours: [Entry] { fnSources.map { Entry(source: $0, destination: f18) } }

    private static func isOurs(_ e: Entry) -> Bool { ours.contains(e) }

    // MARK: - hidutil

    private static func run(_ arguments: [String]) -> (ok: Bool, output: String) {
        let task = Process()
        let pipe = Pipe()
        task.executableURL = URL(fileURLWithPath: hidutil)
        task.arguments = arguments
        task.standardOutput = pipe
        task.standardError = FileHandle.nullDevice
        guard (try? task.run()) != nil else { return (false, "") }
        let data = pipe.fileHandleForReading.readDataToEndOfFile()
        task.waitUntilExit()
        return (task.terminationStatus == 0, String(decoding: data, as: UTF8.self))
    }

    /// Every distinct mapping across the matched keyboards, or nil on failure.
    static func current() -> [Entry]? {
        let result = run(["property", "--matching", "keyboard", "--get", "UserKeyMapping"])
        guard result.ok else { return nil }
        return parse(result.output)
    }

    static func parse(_ report: String) -> [Entry] {
        var entries: [Entry] = []
        for block in report.components(separatedBy: "{").dropFirst() {
            let body = block.components(separatedBy: "}").first ?? ""
            guard let src = number(after: "HIDKeyboardModifierMappingSrc", in: body),
                  let dst = number(after: "HIDKeyboardModifierMappingDst", in: body)
            else { continue }
            let entry = Entry(source: src, destination: dst)
            if !entries.contains(entry) { entries.append(entry) }
        }
        return entries
    }

    private static func number(after field: String, in body: String) -> UInt64? {
        guard let range = body.range(of: field) else { return nil }
        let rest = body[range.upperBound...].drop { $0 == " " || $0 == "=" || $0 == "\"" }
        return UInt64(rest.prefix { $0.isNumber })
    }

    static func argument(_ entries: [Entry]) -> String {
        let items = entries.map {
            "{\"HIDKeyboardModifierMappingSrc\":\($0.source),\"HIDKeyboardModifierMappingDst\":\($0.destination)}"
        }
        return "{\"UserKeyMapping\":[\(items.joined(separator: ","))]}"
    }

    private static func write(_ entries: [Entry]) -> Bool {
        run(["property", "--matching", "keyboard", "--set", argument(entries)]).ok
    }

    // MARK: - Public

    /// True when every Cockpit entry is in the current table.
    static var isApplied: Bool {
        guard let table = current() else { return false }
        return ours.allSatisfy(table.contains)
    }

    /// Adds Fn to F18 next to whatever was already mapped. Returns a reason on
    /// failure, and leaves the table as it found it when it cannot confirm.
    static func apply() -> String? {
        guard let table = current() else { return "hidutil could not read the key mapping" }
        let others = table.filter { !isOurs($0) }
        if others.contains(where: { fnSources.contains($0.source) }) {
            return "Fn is already remapped by something else"
        }
        startGuard(restoring: others)
        guard write(ours + others) else {
            stopGuard()
            return "hidutil could not write the key mapping"
        }
        guard isApplied else {
            _ = remove()
            return "the key mapping did not take effect"
        }
        return nil
    }

    /// Removes only Cockpit's entries. True when nothing of ours remains.
    @discardableResult
    static func remove() -> Bool {
        defer { stopGuard() }
        guard let table = current() else { return false }
        guard table.contains(where: isOurs) else { return true }
        let others = table.filter { !isOurs($0) }
        return write(others) && !(current() ?? ours).contains(where: isOurs)
    }

    // MARK: - Crash guard

    /// A tiny shell waits on a pipe the app owns. The kernel closes it when the
    /// app dies by any means, and the shell then writes the table that existed
    /// without Cockpit's entries, so Fn is not left remapped. A normal stop
    /// terminates the shell first.
    private static func startGuard(restoring others: [Entry]) {
        stopGuard()
        let process = Process()
        let pipe = Pipe()
        process.executableURL = URL(fileURLWithPath: "/bin/sh")
        process.arguments = [
            "-c", "IFS= read -r _; exec \"$1\" property --matching keyboard --set \"$2\"",
            "sh", hidutil, argument(others),
        ]
        process.standardInput = pipe
        process.standardOutput = FileHandle.nullDevice
        process.standardError = FileHandle.nullDevice
        guard (try? process.run()) != nil else { return }
        guardProcess = process
        guardInput = pipe.fileHandleForWriting
    }

    private static func stopGuard() {
        guardProcess?.terminate()
        try? guardInput?.close()
        guardProcess = nil
        guardInput = nil
    }
}
