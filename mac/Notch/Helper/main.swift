import Foundation
import os

// Cockpit's privileged helper. One operation: move an allowed item to the
// calling user's Trash. See docs/helper.md for the boundary.

private let log = Logger(subsystem: "dev.orthic.cockpit.helper", category: "trash")

/// Only code signed by Cockpit's team with one of these identifiers may connect.
private let clientRequirement =
    "anchor apple generic and certificate leaf[subject.OU] = \"\(cockpitTeamID)\" and "
    + "(identifier \"dev.orthic.cockpit\" or identifier \"dev.orthic.cockpit.hub\" "
    + "or identifier \"dev.orthic.cockpit.elevate\")"

private func result(_ path: String, _ status: String, _ detail: String = "") -> [String: String] {
    ["path": path, "status": status, "detail": detail]
}

private func trashDirectory(forUser uid: uid_t) -> (path: String, gid: gid_t)? {
    guard uid >= 500, let pw = getpwuid(uid), let dir = pw.pointee.pw_dir else { return nil }
    let home = String(cString: dir)
    guard home.hasPrefix("/Users/"), !home.contains("..") else { return nil }
    let trash = home + "/.Trash"
    var info = stat()
    if lstat(trash, &info) != 0 {
        guard mkdir(trash, 0o700) == 0 else { return nil }
        chown(trash, uid, pw.pointee.pw_gid)
        return (trash, pw.pointee.pw_gid)
    }
    guard (info.st_mode & 0o170000) == 0o040000, info.st_uid == uid else { return nil }
    return (trash, pw.pointee.pw_gid)
}

/// Gives the moved item (and everything inside it) to the user, so emptying the
/// Trash needs no password. Physical walk: symlinks are changed themselves
/// (lchown), never followed, and other volumes are not entered. Returns the
/// number of entries that could not be changed.
private func giveToUser(_ root: String, uid: uid_t, gid: gid_t) -> Int {
    var failures = 0
    guard let start = strdup(root) else { return 1 }
    defer { free(start) }
    var argv: [UnsafeMutablePointer<CChar>?] = [start, nil]
    // FTS_PHYSICAL | FTS_NOCHDIR | FTS_XDEV
    guard let walk = fts_open(&argv, 0x10 | 0x04 | 0x40, nil) else { return 1 }
    defer { fts_close(walk) }
    while let entry = fts_read(walk) {
        switch Int32(entry.pointee.fts_info) {
        case 6: continue                       // FTS_DP: directory already handled
        case 4, 7, 10: failures += 1; continue // FTS_DNR, FTS_ERR, FTS_NS
        default: break
        }
        if lchown(entry.pointee.fts_path, uid, gid) != 0 { failures += 1 }
    }
    return failures
}

/// rename(2) into the Trash under a name that does not exist yet. Nothing is
/// ever deleted or overwritten (RENAME_EXCL).
private func move(_ path: String, toTrash trash: String, uid: uid_t, gid: gid_t) -> [String: String] {
    let name = (path as NSString).lastPathComponent
    let stem = (name as NSString).deletingPathExtension
    let ext = (name as NSString).pathExtension
    for n in 1...1000 {
        let candidate = n == 1 ? name : (ext.isEmpty ? "\(stem) \(n)" : "\(stem) \(n).\(ext)")
        let target = trash + "/" + candidate
        if renamex_np(path, target, 0x4 /* RENAME_EXCL */) == 0 {
            log.notice("moved \(path, privacy: .public) to \(target, privacy: .public)")
            let failed = giveToUser(target, uid: uid, gid: gid)
            if failed > 0 {
                log.error("ownership: \(failed) entries of \(target, privacy: .public) not changed")
                return result(path, "moved", "\(target) (ownership of \(failed) entries not changed; emptying the Trash may ask for a password)")
            }
            return result(path, "moved", target)
        }
        if errno != EEXIST {
            let reason = String(cString: strerror(errno))
            log.error("failed \(path, privacy: .public): \(reason, privacy: .public)")
            return result(path, "refused", reason)
        }
    }
    return result(path, "refused", "No free name in the Trash.")
}

private final class Service: NSObject, CockpitHelperProtocol {
    let callerUID: uid_t
    init(callerUID: uid_t) { self.callerUID = callerUID }

    func moveToTrash(paths: [String], forUser uid: UInt32,
                     reply: @escaping ([[String: String]]) -> Void) {
        guard uid == callerUID, let trash = trashDirectory(forUser: uid) else {
            log.error("refused batch: uid \(uid) not the caller's or has no Trash")
            reply(paths.map { result($0, "refused", "Not your Trash.") })
            return
        }
        reply(paths.map { path in
            if let why = TrashPolicy.refusal(for: path) {
                log.notice("refused \(path, privacy: .public): \(why, privacy: .public)")
                return result(path, "refused", why)
            }
            return move(path, toTrash: trash.path, uid: uid, gid: trash.gid)
        })
    }
}

private final class Delegate: NSObject, NSXPCListenerDelegate {
    private var open = 0
    private var idleExit: DispatchWorkItem?

    func listener(_ listener: NSXPCListener, shouldAcceptNewConnection c: NSXPCConnection) -> Bool {
        c.exportedInterface = NSXPCInterface(with: CockpitHelperProtocol.self)
        c.exportedObject = Service(callerUID: c.effectiveUserIdentifier)
        idleExit?.cancel()
        open += 1
        c.invalidationHandler = { [weak self] in
            DispatchQueue.main.async { self?.closed() }
        }
        c.resume()
        return true
    }

    private func closed() {
        open -= 1
        guard open <= 0 else { return }
        // launchd starts the helper again on the next connection.
        let work = DispatchWorkItem { exit(0) }
        idleExit = work
        DispatchQueue.main.asyncAfter(deadline: .now() + 15, execute: work)
    }
}

private let delegate = Delegate()
private let listener = NSXPCListener(machServiceName: cockpitHelperMachService)
listener.setConnectionCodeSigningRequirement(clientRequirement)
listener.delegate = delegate
listener.resume()
RunLoop.main.run()
