import AppKit
import Foundation

/// Puts a verified Pulse.app in /Applications and starts it again.
///
/// Nothing reaches /Applications until a copy made from the mounted image has
/// passed `UpdateVerifier`. The installed copy is checked again after it lands,
/// and the old app goes back in place if anything fails along the way.
enum UpdateInstaller {
    static let installLocation = URL(fileURLWithPath: "/Applications/Pulse.app")

    /// A fresh, private directory for one update's download, mount and copy.
    static func makeWorkDirectory() throws -> URL {
        let directory = FileManager.default.temporaryDirectory
            .appendingPathComponent("dev.orthic.pulse.update-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        return directory
    }

    /// Mounts `image`, copies its Pulse.app into `workDirectory`, detaches the
    /// image and verifies the copy. Returns the verified copy. The copy is
    /// verified, not the mount, so the bytes installed are the bytes checked.
    static func stage(image: URL, workDirectory: URL, version: String, currentVersion: String) throws -> URL {
        guard let target = SemanticVersion(version), let current = SemanticVersion(currentVersion),
              target > current else {
            throw UpdateError.message("The update is not newer than the copy installed, so it was not installed.")
        }
        let staged = workDirectory.appendingPathComponent("Pulse.app", isDirectory: true)
        try UpdateVerifier.withMounted(image, in: workDirectory) { mounted in
            let source = mounted.appendingPathComponent("Pulse.app", isDirectory: true)
            guard FileManager.default.fileExists(atPath: source.path) else {
                throw UpdateError.message("The disk image does not contain Pulse.app, so it was not installed.")
            }
            try UpdateVerifier.run("/usr/bin/ditto", [source.path, staged.path],
                                   failure: "Couldn't copy the update out of the disk image.")
        }
        try UpdateVerifier.verify(app: staged, version: version)
        return staged
    }

    /// Moves the installed Pulse.app to the Trash and puts `staged` in its
    /// place. If the new copy does not come through intact, the old app is
    /// moved back and the error is thrown.
    static func replace(with staged: URL, version: String) throws {
        let fileManager = FileManager.default
        var trashed: NSURL?
        if fileManager.fileExists(atPath: installLocation.path) {
            do {
                try fileManager.trashItem(at: installLocation, resultingItemURL: &trashed)
            } catch {
                throw UpdateError.message("Couldn't move the installed Pulse to the Trash: \(error.localizedDescription)")
            }
        }
        do {
            try UpdateVerifier.run("/usr/bin/ditto", [staged.path, installLocation.path],
                                   failure: "Couldn't copy the update into /Applications.")
            try UpdateVerifier.verify(app: installLocation, version: version)
        } catch {
            try? fileManager.removeItem(at: installLocation)
            if let previous = trashed as URL? {
                try? fileManager.moveItem(at: previous, to: installLocation)
            }
            throw error
        }
    }

    /// Starts a detached helper that waits for `pid` to exit, opens the
    /// installed app, and removes `workDirectory`. The caller quits right after.
    static func relaunch(after pid: Int32, workDirectory: URL) throws {
        let script = workDirectory.appendingPathComponent("relaunch.sh")
        let body = """
            #!/bin/sh
            pid="$1"; app="$2"; work="$3"
            waited=0
            while kill -0 "$pid" 2>/dev/null && [ "$waited" -lt 300 ]; do
                sleep 0.2
                waited=$((waited + 1))
            done
            /usr/bin/open "$app"
            rm -rf "$work"
            """
        try body.write(to: script, atomically: true, encoding: .utf8)
        let helper = Process()
        helper.executableURL = URL(fileURLWithPath: "/bin/sh")
        helper.arguments = [script.path, String(pid), installLocation.path, workDirectory.path]
        helper.standardInput = FileHandle.nullDevice
        helper.standardOutput = FileHandle.nullDevice
        helper.standardError = FileHandle.nullDevice
        do {
            try helper.run()
        } catch {
            throw UpdateError.message("Couldn't start the relaunch helper: \(error.localizedDescription)")
        }
    }
}
