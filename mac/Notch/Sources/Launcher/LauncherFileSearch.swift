import Foundation

/// Spotlight search of file names, limited to the folders chosen in the hub.
/// Pulse keeps no index of its own. Queries of two characters or more; one
/// query at a time, and a new one cancels the last.
@MainActor
final class LauncherFileSearch {
    struct Hit: Equatable {
        let url: URL
        let isDirectory: Bool
    }

    private var query: NSMetadataQuery?
    private var observer: NSObjectProtocol?
    private var debounce: DispatchWorkItem?
    private let limit = 6

    /// With no folders chosen, the search is off and `completion` gets no hits.
    func search(_ text: String, folders: [String], completion: @escaping ([Hit]) -> Void) {
        cancel()
        let trimmed = text.trimmingCharacters(in: .whitespaces)
        let scopes = folders.filter { FileManager.default.fileExists(atPath: $0) }
        guard trimmed.count >= 2, !scopes.isEmpty else { completion([]); return }
        let work = DispatchWorkItem { [weak self] in
            MainActor.assumeIsolated { self?.start(trimmed, scopes: scopes, completion: completion) }
        }
        debounce = work
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.15, execute: work)
    }

    func cancel() {
        debounce?.cancel()
        debounce = nil
        query?.stop()
        query = nil
        if let observer { NotificationCenter.default.removeObserver(observer) }
        observer = nil
    }

    private func start(_ text: String, scopes: [String], completion: @escaping ([Hit]) -> Void) {
        let query = NSMetadataQuery()
        // The text is a predicate argument, never part of the format string.
        query.predicate = NSPredicate(format: "%K CONTAINS[cd] %@", NSMetadataItemFSNameKey, text)
        query.searchScopes = scopes.map { URL(fileURLWithPath: $0, isDirectory: true) } as [Any]
        query.sortDescriptors = [NSSortDescriptor(key: NSMetadataItemFSContentChangeDateKey, ascending: false)]
        observer = NotificationCenter.default.addObserver(
            forName: .NSMetadataQueryDidFinishGathering, object: query, queue: .main
        ) { [weak self] _ in
            MainActor.assumeIsolated {
                guard let self, let finished = self.query else { return }
                finished.disableUpdates()
                var hits: [Hit] = []
                let count = min(finished.resultCount, 300)
                for i in 0..<count {
                    guard let item = finished.result(at: i) as? NSMetadataItem,
                          let path = item.value(forAttribute: NSMetadataItemPathKey) as? String
                    else { continue }
                    if path.contains("/Library/") || path.contains("/.") || path.hasSuffix(".app")
                        || path.contains(".app/") { continue }
                    var isDir: ObjCBool = false
                    FileManager.default.fileExists(atPath: path, isDirectory: &isDir)
                    hits.append(Hit(url: URL(fileURLWithPath: path), isDirectory: isDir.boolValue))
                    if hits.count >= self.limit { break }
                }
                self.cancel()
                completion(hits)
            }
        }
        self.query = query
        query.start()
    }
}
