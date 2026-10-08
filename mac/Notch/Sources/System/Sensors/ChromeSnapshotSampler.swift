import Foundation

/// Pulse fork: the daily count of Chrome's leftover signing snapshots, for the
/// same history the hub's cleanup scan keeps (rule `chrome-signing-copies`).
/// The notch counts only the clone folders in the user cache directory (no
/// walk of the tree, no sizes) and appends a sample at most once a day to
/// `~/Library/Application Support/Pulse/chrome-snapshots.json`, using the
/// file format `core/src/cleanup_scan.rs` reads.
enum ChromeSnapshotSampler {
    private static let gap: TimeInterval = 86_400
    private static let keep = 60
    /// A clone is counted once it is a day old, as the rule's age threshold says.
    private static let minimumAge: TimeInterval = 86_400
    private static let cloneFolder = "com.google.Chrome.code_sign_clone"
    private static let clonePrefix = "code_sign_clone."

    private struct Sample: Codable {
        let at: UInt64
        let count: UInt64
    }

    private struct Log: Codable {
        var samples: [Sample] = []
    }

    /// Records a sample if the last one is a day old or more. The file is read
    /// at most once per call; the folders are counted only when a sample is due.
    static func sampleIfDue(now: Date = Date(), home: URL = FileManager.default.homeDirectoryForCurrentUser) {
        let file = home.appendingPathComponent("Library/Application Support/Pulse/chrome-snapshots.json")
        var log = (try? Data(contentsOf: file))
            .flatMap { try? JSONDecoder().decode(Log.self, from: $0) } ?? Log()
        let stamp = UInt64(now.timeIntervalSince1970)
        if let last = log.samples.last, stamp < last.at &+ UInt64(gap) { return }

        let count = cloneCount(now: now)
        log.samples.append(Sample(at: stamp, count: UInt64(count)))
        if log.samples.count > keep {
            log.samples.removeFirst(log.samples.count - keep)
        }
        // Like the core: the very first sample is kept only when it is not empty.
        guard count > 0 || log.samples.count > 1,
              let body = try? JSONEncoder().encode(log)
        else { return }
        try? FileManager.default.createDirectory(at: file.deletingLastPathComponent(),
                                                 withIntermediateDirectories: true)
        try? body.write(to: file, options: .atomic)
    }

    /// Clone folders older than a day, in the per-user cache directory
    /// (`/private/var/folders/…/C`). The temporary directory is its sibling `T`.
    private static func cloneCount(now: Date) -> Int {
        let temporary = URL(fileURLWithPath: NSTemporaryDirectory(), isDirectory: true)
        let cache = temporary.deletingLastPathComponent().appendingPathComponent("C", isDirectory: true)
        let folder = cache.appendingPathComponent(cloneFolder, isDirectory: true)
        let entries = (try? FileManager.default.contentsOfDirectory(
            at: folder, includingPropertiesForKeys: [.contentModificationDateKey],
            options: [.skipsHiddenFiles])) ?? []
        return entries.filter { entry in
            guard entry.lastPathComponent.hasPrefix(clonePrefix),
                  let modified = try? entry.resourceValues(forKeys: [.contentModificationDateKey])
                    .contentModificationDate
            else { return false }
            return now.timeIntervalSince(modified) >= minimumAge
        }.count
    }
}
