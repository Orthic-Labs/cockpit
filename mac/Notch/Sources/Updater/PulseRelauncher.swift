import AppKit
import Darwin
import Foundation

/// **Restarting Pulse into the copy on disk.** Pulse cannot replace the bundle
/// it runs from and keep running, so the swap is done by a tiny shell script in
/// its own session (`POSIX_SPAWN_SETSID`) that outlives Pulse: it waits for
/// Pulse's pid to exit, optionally swaps a verified staged copy in (the old
/// copy goes to the Trash first and comes back if the new one fails
/// `codesign --verify --deep --strict`), then opens /Applications/Pulse.app.
/// Opening Pulse starts its hub helper again as a child, so no stale hub
/// survives a new notch.
enum PulseRelauncher {
    static let installLocation = URL(fileURLWithPath: "/Applications/Pulse.app")

    /// Starts the detached relauncher. The caller quits right after. With
    /// `staged` nil it only reopens the installed copy.
    static func spawn(swapping staged: URL?, workDirectory: URL?) throws {
        let fm = FileManager.default
        let scriptDirectory = fm.temporaryDirectory
            .appendingPathComponent("dev.orthic.pulse.relaunch-\(UUID().uuidString)", isDirectory: true)
        try fm.createDirectory(at: scriptDirectory, withIntermediateDirectories: true)
        let script = scriptDirectory.appendingPathComponent("relaunch.sh")
        let body = """
            #!/bin/sh
            pid="$1"; staged="$2"; app="$3"; work="$4"; self="$5"
            waited=0
            while kill -0 "$pid" 2>/dev/null && [ "$waited" -lt 300 ]; do
                sleep 0.2
                waited=$((waited + 1))
            done
            if kill -0 "$pid" 2>/dev/null; then
                rm -rf "$self"
                exit 1
            fi
            if [ -n "$staged" ] && [ -d "$staged" ]; then
                trash="$HOME/.Trash/Pulse-replaced-$$.app"
                if [ -d "$app" ] && ! /bin/mv "$app" "$trash"; then
                    /usr/bin/open "$app"
                    rm -rf "$self"
                    exit 1
                fi
                if /usr/bin/ditto "$staged" "$app" && /usr/bin/codesign --verify --deep --strict "$app" 2>/dev/null; then
                    :
                else
                    rm -rf "$app"
                    if [ -d "$trash" ]; then /bin/mv "$trash" "$app"; fi
                fi
            fi
            /usr/bin/open "$app"
            if [ -n "$work" ]; then rm -rf "$work"; fi
            rm -rf "$self"
            """
        try body.write(to: script, atomically: true, encoding: .utf8)

        let arguments = ["/bin/sh", script.path,
                         String(ProcessInfo.processInfo.processIdentifier),
                         staged?.path ?? "", installLocation.path,
                         workDirectory?.path ?? "", scriptDirectory.path]
        var attributes: posix_spawnattr_t?
        var actions: posix_spawn_file_actions_t?
        posix_spawnattr_init(&attributes)
        posix_spawn_file_actions_init(&actions)
        defer {
            posix_spawnattr_destroy(&attributes)
            posix_spawn_file_actions_destroy(&actions)
        }
        // POSIX_SPAWN_SETSID (0x0400): the helper leads its own session, so it
        // is not taken down with Pulse.
        posix_spawnattr_setflags(&attributes, Int16(0x0400))
        posix_spawn_file_actions_addopen(&actions, 0, "/dev/null", O_RDONLY, 0)
        posix_spawn_file_actions_addopen(&actions, 1, "/dev/null", O_WRONLY, 0)
        posix_spawn_file_actions_addopen(&actions, 2, "/dev/null", O_WRONLY, 0)

        var argv: [UnsafeMutablePointer<CChar>?] = arguments.map { strdup($0) }
        argv.append(nil)
        defer { argv.forEach { free($0) } }
        var pid: pid_t = 0
        let status = posix_spawn(&pid, "/bin/sh", &actions, &attributes, argv, environ)
        guard status == 0 else {
            try? fm.removeItem(at: scriptDirectory)
            throw UpdateError.message("Couldn't start the relaunch helper (error \(status)).")
        }
    }

    /// Quits Pulse cleanly after a short beat so the card can draw. The hub
    /// helper is stopped in `applicationWillTerminate`.
    @MainActor static func quitSoon() {
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.6) { NSApp.terminate(nil) }
    }
}
