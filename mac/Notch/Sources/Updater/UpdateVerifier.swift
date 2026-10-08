import Foundation
import Security

/// Mounts a downloaded disk image and checks the app inside it before anything
/// is installed. Every check must pass; there is no partial acceptance.
enum UpdateVerifier {
    static let bundleIdentifier = "dev.orthic.pulse"
    static let teamIdentifier = "6KLGD3LLKF"

    /// Apple-anchored Developer ID signature, from this bundle identifier, by
    /// this team. The two certificate OIDs pin it to a Developer ID Application
    /// certificate, not any other kind of Apple-issued signature.
    static let requirementText = """
        anchor apple generic and identifier "\(bundleIdentifier)" \
        and certificate 1[field.1.2.840.113635.100.6.2.6] \
        and certificate leaf[field.1.2.840.113635.100.6.1.13] \
        and certificate leaf[subject.OU] = "\(teamIdentifier)"
        """

    /// Mounts `image` read-only without showing it in Finder, runs `body` with
    /// the mount point, and always detaches afterwards.
    static func withMounted<T>(_ image: URL, in directory: URL, _ body: (URL) throws -> T) throws -> T {
        let mountPoint = directory.appendingPathComponent("mount", isDirectory: true)
        try FileManager.default.createDirectory(at: mountPoint, withIntermediateDirectories: true)
        try run("/usr/bin/hdiutil", [
            "attach", "-nobrowse", "-readonly", "-noautoopen",
            "-mountpoint", mountPoint.path, image.path,
        ], failure: "Couldn't open the downloaded disk image.")
        defer {
            if (try? run("/usr/bin/hdiutil", ["detach", mountPoint.path], failure: "")) == nil {
                _ = try? run("/usr/bin/hdiutil", ["detach", "-force", mountPoint.path], failure: "")
            }
        }
        return try body(mountPoint)
    }

    /// Checks a Pulse.app on disk: its identity, its version, its signature
    /// and whether Gatekeeper accepts it as notarized.
    static func verify(app: URL, version: String) throws {
        guard let bundle = Bundle(url: app),
              bundle.bundleIdentifier == bundleIdentifier else {
            throw UpdateError.message("The update is not Pulse (its bundle identifier differs), so it was not installed.")
        }
        let shipped = bundle.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? ""
        guard SemanticVersion(shipped) == SemanticVersion(version) else {
            throw UpdateError.message("The update says it is version \(shipped), not \(version), so it was not installed.")
        }

        var code: SecStaticCode?
        guard SecStaticCodeCreateWithPath(app as CFURL, [], &code) == errSecSuccess, let code else {
            throw UpdateError.message("Couldn't read the update's code signature, so it was not installed.")
        }
        var requirement: SecRequirement?
        guard SecRequirementCreateWithString(requirementText as CFString, [], &requirement) == errSecSuccess,
              let requirement else {
            throw UpdateError.message("Couldn't build the signature requirement for Pulse.")
        }
        let flags = SecCSFlags(rawValue: UInt32(kSecCSStrictValidate)
                               | UInt32(kSecCSCheckAllArchitectures)
                               | UInt32(kSecCSCheckNestedCode))
        var error: Unmanaged<CFError>?
        guard SecStaticCodeCheckValidityWithErrors(code, flags, requirement, &error) == errSecSuccess else {
            let reason = error?.takeRetainedValue().localizedDescription ?? "the signature does not match Pulse's"
            throw UpdateError.message("The update's signature failed verification (\(reason)), so it was not installed.")
        }

        // Gatekeeper: notarized by Apple for Developer ID distribution. A
        // stapled ticket is accepted offline; otherwise Gatekeeper asks Apple.
        try run("/usr/sbin/spctl", ["--assess", "--type", "execute", app.path],
                failure: "Gatekeeper did not accept the update as notarized, so it was not installed.")
    }

    /// Runs a tool to completion and returns its standard output. A non-zero
    /// exit throws `failure`, or the tool's own error text when `failure` is empty.
    @discardableResult
    static func run(_ tool: String, _ arguments: [String], failure: String) throws -> Data {
        let process = Process()
        process.executableURL = URL(fileURLWithPath: tool)
        process.arguments = arguments
        process.standardInput = FileHandle.nullDevice
        let output = Pipe()
        let errors = Pipe()
        process.standardOutput = output
        process.standardError = errors
        do {
            try process.run()
        } catch {
            throw UpdateError.message(failure.isEmpty ? "Couldn't run \(tool)." : failure)
        }
        let data = output.fileHandleForReading.readDataToEndOfFile()
        let text = errors.fileHandleForReading.readDataToEndOfFile()
        process.waitUntilExit()
        guard process.terminationStatus == 0 else {
            let detail = String(data: text, encoding: .utf8)?
                .trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
            throw UpdateError.message(failure.isEmpty ? (detail.isEmpty ? "\(tool) failed." : detail) : failure)
        }
        return data
    }
}
