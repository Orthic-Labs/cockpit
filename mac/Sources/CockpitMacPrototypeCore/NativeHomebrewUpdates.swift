import Darwin
import Foundation

/// Performs one bounded, read-only Homebrew cask update check for one app.
///
/// Ownership is accepted only when Homebrew's installed cask record names this
/// exact bundle path. The adapter never invokes a shell or any install,
/// upgrade, update, tap, analytics, or credential operation.
public enum NativeHomebrewUpdates {
    private static let brewPaths = ["/opt/homebrew/bin/brew", "/usr/local/bin/brew"]
    private static let commandTimeout: TimeInterval = 20
    private static let outputLimit = 4 * 1024 * 1024
    private static let rowLimit = 256

    private struct CommandResult {
        let status: Int32
        let output: Data
        let timedOut: Bool
        let truncated: Bool
    }

    private struct CaskRecord {
        let token: String
        let appPaths: Set<String>
    }

    private struct OutdatedRecord {
        let token: String
        let latest: String
        let pinned: Bool
    }

    private final class OutputBuffer: @unchecked Sendable {
        private let lock = NSLock()
        private let limit: Int
        private var data = Data()
        private var truncated = false

        init(limit: Int) { self.limit = limit }

        func append(_ chunk: Data) {
            guard !chunk.isEmpty else { return }
            lock.lock()
            defer { lock.unlock() }
            let room = limit - data.count
            if room <= 0 {
                truncated = true
                return
            }
            if chunk.count > room { truncated = true }
            data.append(chunk.prefix(room))
        }

        func value() -> (Data, Bool) {
            lock.lock()
            defer { lock.unlock() }
            return (data, truncated)
        }
    }

    /// Checks one bundle against Homebrew's installed casks and current cask
    /// metadata. `bundleURL` and `bundleID` are caller-admitted identity.
    @MainActor
    public static func check(bundleURL: URL, bundleID: String) async -> [String: Any] {
        let baseEvidence: [String: Any] = [
            "commands": [
                ["program": "brew", "arguments": ["info", "--json=v2", "--installed"]],
                ["program": "brew", "arguments": ["outdated", "--cask", "--greedy", "--json=v2"]]
            ],
            "ownershipRule": "one installed cask artifact app path must equal bundlePath",
            "networkPolicy": "HOMEBREW_NO_AUTO_UPDATE=1; HOMEBREW_NO_ANALYTICS=1"
        ]
        let exclusions = [
            "no brew update or metadata refresh command",
            "no install, upgrade, tap, credential, shell, or analytics operation",
            "no latest version is inferred when Homebrew omits current_version"
        ]
        func result(state: String, available: Bool, reason: String,
                    evidence: [String: Any]? = nil) -> [String: Any] {
            [
                "schemaVersion": 1,
                "provider": "homebrew",
                "source": "homebrew",
                "available": available,
                "state": state,
                "status": state,
                "reason": reason,
                "networkPerformed": NSNull(),
                "checkedAt": ISO8601DateFormatter().string(from: Date()),
                "bundleID": bundleID,
                "bundlePath": bundleURL.standardizedFileURL.path,
                "evidence": evidence ?? baseEvidence,
                "exclusions": exclusions
            ]
        }

        guard validBundleURL(bundleURL), !bundleID.isEmpty else {
            return result(state: "unavailable", available: false,
                          reason: "bundle_identity_invalid")
        }
        let bundlePath = bundleURL.standardizedFileURL.path
        var isDirectory: ObjCBool = false
        guard FileManager.default.fileExists(atPath: bundlePath, isDirectory: &isDirectory),
              isDirectory.boolValue else {
            return result(state: "not_installed", available: false,
                          reason: "bundle_path_not_found")
        }
        guard let bundle = Bundle(url: bundleURL.standardizedFileURL),
              bundle.bundleIdentifier == bundleID else {
            return result(state: "unavailable", available: false,
                          reason: "bundle_identity_invalid")
        }
        guard let installedVersion = bundleVersion(bundle), !installedVersion.isEmpty else {
            return result(state: "unavailable", available: false,
                          reason: "bundle_version_unavailable")
        }
        guard let brewPath = brewPaths.first(where: {
            FileManager.default.isExecutableFile(atPath: $0)
        }) else {
            return result(state: "unavailable", available: false,
                          reason: "homebrew_not_available")
        }

        let installed = await runCommand(path: brewPath,
                                          arguments: ["info", "--json=v2", "--installed"])
        guard installed.status == 0, !installed.timedOut, !installed.truncated,
              let installedRoot = jsonObject(installed.output),
              let installedCasks = parseInstalledCasks(installedRoot),
              installedCasks.count <= rowLimit else {
            return result(state: "unavailable", available: false,
                          reason: installed.timedOut ? "installed_command_timed_out"
                            : (installed.truncated ? "installed_output_truncated"
                               : "installed_output_unavailable"),
                          evidence: baseEvidence.merging([
                            "brewPath": brewPath,
                            "installedStatus": Int(installed.status)
                          ]) { _, new in new })
        }

        let matches = installedCasks.filter { $0.appPaths.contains(bundlePath) }
        guard matches.count == 1, let managed = matches.first else {
            let reason = matches.isEmpty ? "exact_cask_bundle_path_not_found"
                                         : "exact_cask_bundle_path_ambiguous"
            return result(state: "not_managed", available: false, reason: reason,
                          evidence: baseEvidence.merging([
                            "brewPath": brewPath,
                            "installedCaskCount": installedCasks.count,
                            "bundlePath": bundlePath
                          ]) { _, new in new })
        }

        let outdated = await runCommand(path: brewPath,
                                        arguments: ["outdated", "--cask", "--greedy", "--json=v2"])
        var checked = baseEvidence.merging([
            "brewPath": brewPath,
            "installedCaskToken": managed.token,
            "installedCaskCount": installedCasks.count,
            "installedVersion": installedVersion
        ]) { _, new in new }
        guard outdated.status == 0, !outdated.timedOut, !outdated.truncated,
              let outdatedRoot = jsonObject(outdated.output),
              let rows = parseOutdatedCasks(outdatedRoot), rows.count <= rowLimit else {
            let reason = outdated.timedOut ? "outdated_command_timed_out"
                : (outdated.truncated ? "outdated_output_truncated" : "outdated_output_unavailable")
            checked["outdatedStatus"] = Int(outdated.status)
            var partial = result(state: "partial", available: false, reason: reason,
                                 evidence: checked)
            partial["networkPerformed"] = NSNull()
            return partial
        }
        checked["outdatedCaskCount"] = rows.count

        let updateRows = rows.filter { $0.token == managed.token }
        guard updateRows.count <= 1 else {
            var partial = result(state: "partial", available: false,
                                 reason: "outdated_cask_rows_ambiguous", evidence: checked)
            partial["networkPerformed"] = NSNull()
            return partial
        }
        guard let update = updateRows.first else {
            var noUpdate = result(state: "no-update", available: true,
                                  reason: "no_newer_cask_version", evidence: checked)
            noUpdate["currentVersion"] = installedVersion
            noUpdate["networkPerformed"] = NSNull()
            return noUpdate
        }
        guard !update.pinned else {
            var noUpdate = result(state: "no-update", available: true,
                                  reason: "cask_pinned", evidence: checked)
            noUpdate["currentVersion"] = installedVersion
            noUpdate["networkPerformed"] = NSNull()
            return noUpdate
        }
        let latest = versionCore(update.latest)
        guard !latest.isEmpty, latest.lowercased() != "latest" else {
            var partial = result(state: "partial", available: false,
                                 reason: "latest_version_uncomparable", evidence: checked)
            partial["networkPerformed"] = NSNull()
            return partial
        }
        checked["latestVersion"] = latest
        var answer = result(state: isNewer(latest, than: installedVersion) ? "available" : "no-update",
                            available: true,
                            reason: isNewer(latest, than: installedVersion)
                                ? "newer_cask_version_available" : "no_newer_cask_version",
                            evidence: checked)
        answer["currentVersion"] = installedVersion
        answer["latestVersion"] = latest
        answer["candidateVersion"] = update.latest
        answer["packageToken"] = managed.token
        answer["networkPerformed"] = NSNull()
        return answer
    }

    private static func validBundleURL(_ url: URL) -> Bool {
        guard url.isFileURL, url.scheme?.lowercased() == "file", url.host == nil,
              url.query == nil, url.fragment == nil,
              url.path.hasPrefix("/"), url.pathExtension.lowercased() == "app",
              !url.path.contains("//"), !url.path.utf8.contains(0) else { return false }
        let components = url.path.split(separator: "/", omittingEmptySubsequences: false)
        guard components.dropFirst().allSatisfy({ $0 != "." && $0 != ".." }) else { return false }
        return true
    }

    private static func bundleVersion(_ bundle: Bundle) -> String? {
        let short = bundle.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String
        let build = bundle.object(forInfoDictionaryKey: "CFBundleVersion") as? String
        return [short, build].compactMap { $0?.trimmingCharacters(in: .whitespacesAndNewlines) }
            .first(where: { !$0.isEmpty })
    }

    private static func parseInstalledCasks(_ root: [String: Any]) -> [CaskRecord]? {
        guard root["formulae"] != nil || root["casks"] != nil else { return nil }
        guard root["casks"] == nil || root["casks"] is [[String: Any]] else { return nil }
        let values = root["casks"] as? [[String: Any]] ?? []
        guard values.count <= rowLimit else { return nil }
        var records: [CaskRecord] = []
        for item in values {
            guard let token = item["token"] as? String, validToken(token) else { continue }
            records.append(CaskRecord(token: token, appPaths: appPaths(in: item)))
        }
        return records
    }

    private static func parseOutdatedCasks(_ root: [String: Any]) -> [OutdatedRecord]? {
        guard root["casks"] != nil || root["formulae"] != nil,
              root["casks"] == nil || root["casks"] is [[String: Any]] else { return nil }
        let values = root["casks"] as? [[String: Any]] ?? []
        guard values.count <= rowLimit else { return nil }
        var records: [OutdatedRecord] = []
        for item in values {
            guard let token = (item["token"] as? String) ?? (item["name"] as? String),
                  validToken(token), let latest = item["current_version"] as? String,
                  !latest.isEmpty else { return nil }
            records.append(OutdatedRecord(token: token, latest: latest,
                                          pinned: item["pinned"] as? Bool ?? false))
        }
        return records
    }

    private static func appPaths(in cask: [String: Any]) -> Set<String> {
        var paths = Set<String>()
        for rawArtifact in cask["artifacts"] as? [Any] ?? [] {
            guard let artifact = rawArtifact as? [String: Any] else { continue }
            var candidates: [String] = []
            if let target = artifact["target"] as? String { candidates.append(target) }
            for rawApp in artifact["app"] as? [Any] ?? [] {
                if let value = rawApp as? String {
                    candidates.append(value)
                } else if let mapping = rawApp as? [String: Any],
                          let target = mapping["target"] as? String {
                    candidates.append(target)
                }
            }
            for candidate in candidates where candidate.hasPrefix("/") {
                let path = URL(fileURLWithPath: candidate).standardizedFileURL.path
                if pathExtensionIsApp(path) { paths.insert(path) }
            }
        }
        return paths
    }

    private static func pathExtensionIsApp(_ path: String) -> Bool {
        URL(fileURLWithPath: path).pathExtension.lowercased() == "app"
    }

    private static func validToken(_ token: String) -> Bool {
        !token.isEmpty && token.utf8.count <= 256
            && token.range(of: #"^[A-Za-z0-9][A-Za-z0-9._+@/-]*$"#, options: .regularExpression) != nil
            && !token.contains("..") && !token.contains("//")
    }

    private static func versionCore(_ value: String) -> String {
        var result = value.trimmingCharacters(in: .whitespacesAndNewlines)
        if (result.first == "v" || result.first == "V"), result.dropFirst().first?.isNumber == true {
            result.removeFirst()
        }
        if let comma = result.firstIndex(of: ",") { result = String(result[..<comma]) }
        return result
    }

    private static func isNewer(_ candidate: String, than installed: String) -> Bool {
        compareVersions(versionCore(candidate), versionCore(installed)) == .orderedDescending
    }

    private static func compareVersions(_ lhs: String, _ rhs: String) -> ComparisonResult {
        let left = lhs.split(whereSeparator: { !$0.isLetter && !$0.isNumber }).map(String.init)
        let right = rhs.split(whereSeparator: { !$0.isLetter && !$0.isNumber }).map(String.init)
        for index in 0..<max(left.count, right.count) {
            let a = index < left.count ? left[index] : "0"
            let b = index < right.count ? right[index] : "0"
            let aDigits = a.allSatisfy(\.isNumber) && !a.isEmpty
            let bDigits = b.allSatisfy(\.isNumber) && !b.isEmpty
            if aDigits && bDigits {
                let aa = a.drop(while: { $0 == "0" }), bb = b.drop(while: { $0 == "0" })
                if aa.count != bb.count { return aa.count < bb.count ? .orderedAscending : .orderedDescending }
                if aa != bb { return aa < bb ? .orderedAscending : .orderedDescending }
            } else if a != b {
                return a.localizedStandardCompare(b)
            }
        }
        return .orderedSame
    }

    private static func jsonObject(_ data: Data) -> [String: Any]? {
        if let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any] { return object }
        let text = String(decoding: data, as: UTF8.self)
        var start: String.Index?
        var depth = 0
        var inString = false
        var escaping = false
        var index = text.startIndex
        while index < text.endIndex {
            let character = text[index]
            if start == nil {
                if character == "{" { start = index; depth = 1 }
            } else if inString {
                if escaping { escaping = false }
                else if character == "\\" { escaping = true }
                else if character == "\"" { inString = false }
            } else if character == "\"" {
                inString = true
            } else if character == "{" {
                depth += 1
            } else if character == "}" {
                depth -= 1
                if depth == 0, let objectStart = start {
                    let candidate = String(text[objectStart...index])
                    if let object = try? JSONSerialization.jsonObject(with: Data(candidate.utf8)) as? [String: Any] {
                        return object
                    }
                    start = nil
                    depth = 0
                }
            }
            index = text.index(after: index)
        }
        return nil
    }

    private static func runCommand(path: String, arguments: [String]) async -> CommandResult {
        await withCheckedContinuation { continuation in
            DispatchQueue.global(qos: .utility).async {
                continuation.resume(returning: runCommandSync(path: path, arguments: arguments))
            }
        }
    }

    private static func runCommandSync(path: String, arguments: [String]) -> CommandResult {
        let process = Process()
        process.executableURL = URL(fileURLWithPath: path)
        process.arguments = arguments
        var environment = ProcessInfo.processInfo.environment
        environment["HOMEBREW_NO_AUTO_UPDATE"] = "1"
        environment["HOMEBREW_NO_ANALYTICS"] = "1"
        environment["HOMEBREW_NO_INSTALL_CLEANUP"] = "1"
        environment["HOMEBREW_NO_ENV_HINTS"] = "1"
        process.environment = environment
        let pipe = Pipe()
        process.standardOutput = pipe
        process.standardError = pipe
        let output = OutputBuffer(limit: outputLimit)
        pipe.fileHandleForReading.readabilityHandler = { output.append($0.availableData) }
        do { try process.run() } catch {
            pipe.fileHandleForReading.readabilityHandler = nil
            return CommandResult(status: -1, output: Data(), timedOut: false, truncated: false)
        }
        let deadline = ProcessInfo.processInfo.systemUptime + commandTimeout
        var timedOut = false
        while process.isRunning {
            if ProcessInfo.processInfo.systemUptime >= deadline {
                timedOut = true
                process.terminate()
                let grace = ProcessInfo.processInfo.systemUptime + 0.5
                while process.isRunning && ProcessInfo.processInfo.systemUptime < grace {
                    Thread.sleep(forTimeInterval: 0.01)
                }
                if process.isRunning { kill(process.processIdentifier, SIGKILL) }
                break
            }
            Thread.sleep(forTimeInterval: 0.01)
        }
        process.waitUntilExit()
        pipe.fileHandleForReading.readabilityHandler = nil
        let (bytes, truncated) = output.value()
        return CommandResult(status: timedOut ? -1 : process.terminationStatus,
                             output: bytes, timedOut: timedOut,
                             truncated: truncated)
    }
}
