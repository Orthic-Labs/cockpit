import Foundation

// pulse-elevate <path>...  — asks the privileged helper to move root-owned
// items to the Trash. Prints one JSON object: {"results":[{path,status,detail}]}
// or {"error":"..."}; exit 0 only when the helper answered.

private func finish(_ object: [String: Any], code: Int32) -> Never {
    let data = (try? JSONSerialization.data(withJSONObject: object)) ?? Data("{}".utf8)
    FileHandle.standardOutput.write(data + Data("\n".utf8))
    exit(code)
}

let paths = Array(CommandLine.arguments.dropFirst())
if paths.isEmpty { finish(["error": "usage: pulse-elevate <path>..."], code: 64) }

let connection = NSXPCConnection(machServiceName: pulseHelperMachService, options: .privileged)
connection.remoteObjectInterface = NSXPCInterface(with: PulseHelperProtocol.self)
// Only the genuine helper (our team, its identifier) may answer.
connection.setCodeSigningRequirement(
    "anchor apple generic and certificate leaf[subject.OU] = \"\(pulseTeamID)\" "
    + "and identifier \"dev.orthic.pulse.helper\"")
connection.resume()

private let done = DispatchSemaphore(value: 0)
private var failure: String?
private var answer: [[String: String]]?
let proxy = connection.remoteObjectProxyWithErrorHandler { error in
    failure = error.localizedDescription
    done.signal()
} as? PulseHelperProtocol
guard let proxy else { finish(["error": "No helper connection."], code: 3) }
proxy.moveToTrash(paths: paths, forUser: UInt32(getuid())) { rows in
    answer = rows
    done.signal()
}
if done.wait(timeout: .now() + 60) == .timedOut { finish(["error": "The helper did not answer."], code: 3) }
if let answer { finish(["results": answer], code: 0) }
finish(["error": failure ?? "The helper is not available."], code: 3)
