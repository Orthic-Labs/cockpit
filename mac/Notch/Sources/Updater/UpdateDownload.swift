import Foundation

/// Downloads one release asset into a directory the caller owns.
///
/// Every hop has to be HTTPS, including redirects, and the bytes must be a
/// `200` whose size matches what the release advertised. Progress is reported
/// as a fraction, or nil while the total is still unknown.
final class UpdateDownload: NSObject, URLSessionDownloadDelegate {
    private let destination: URL
    private let expectedSize: Int64
    private let progress: @Sendable (Double?) -> Void
    private var continuation: CheckedContinuation<URL, Error>?
    private var session: URLSession?
    private var finished = false

    private init(destination: URL, expectedSize: Int64, progress: @escaping @Sendable (Double?) -> Void) {
        self.destination = destination
        self.expectedSize = expectedSize
        self.progress = progress
    }

    /// Fetches `url` into `directory/fileName` and returns that file's URL.
    static func fetch(_ url: URL, expectedSize: Int64, fileName: String, into directory: URL,
                      progress: @escaping @Sendable (Double?) -> Void) async throws -> URL {
        guard url.scheme == "https" else {
            throw UpdateError.message("The update's download address is not HTTPS, so it was refused.")
        }
        let download = UpdateDownload(destination: directory.appendingPathComponent(fileName),
                                      expectedSize: expectedSize, progress: progress)
        return try await download.start(url)
    }

    private func start(_ url: URL) async throws -> URL {
        try await withCheckedThrowingContinuation { continuation in
            self.continuation = continuation
            let configuration = URLSessionConfiguration.ephemeral
            configuration.httpCookieStorage = nil
            configuration.httpShouldSetCookies = false
            configuration.timeoutIntervalForResource = 15 * 60
            let session = URLSession(configuration: configuration, delegate: self, delegateQueue: nil)
            self.session = session
            session.downloadTask(with: url).resume()
        }
    }

    private func finish(_ result: Result<URL, Error>) {
        guard !finished else { return }
        finished = true
        session?.finishTasksAndInvalidate()
        session = nil
        continuation?.resume(with: result)
        continuation = nil
    }

    // MARK: URLSessionDownloadDelegate

    func urlSession(_ session: URLSession, downloadTask: URLSessionDownloadTask,
                    didWriteData bytesWritten: Int64, totalBytesWritten: Int64,
                    totalBytesExpectedToWrite: Int64) {
        let total = totalBytesExpectedToWrite > 0 ? totalBytesExpectedToWrite : expectedSize
        guard total > 0 else { return progress(nil) }
        progress(min(1, Double(totalBytesWritten) / Double(total)))
    }

    func urlSession(_ session: URLSession, downloadTask: URLSessionDownloadTask,
                    didFinishDownloadingTo location: URL) {
        // The system deletes `location` once this returns, so the file moves now.
        guard (downloadTask.response as? HTTPURLResponse)?.statusCode == 200 else {
            return finish(.failure(UpdateError.message("The download failed: the server did not return the disk image.")))
        }
        do {
            let size = (try FileManager.default.attributesOfItem(atPath: location.path)[.size] as? NSNumber)?.int64Value ?? -1
            if expectedSize > 0, size != expectedSize {
                throw UpdateError.message("The download was \(size) bytes, but the release lists \(expectedSize).")
            }
            try FileManager.default.createDirectory(at: destination.deletingLastPathComponent(),
                                                    withIntermediateDirectories: true)
            try? FileManager.default.removeItem(at: destination)
            try FileManager.default.moveItem(at: location, to: destination)
            finish(.success(destination))
        } catch {
            finish(.failure(error))
        }
    }

    func urlSession(_ session: URLSession, task: URLSessionTask,
                    willPerformHTTPRedirection response: HTTPURLResponse, newRequest request: URLRequest,
                    completionHandler: @escaping (URLRequest?) -> Void) {
        // A redirect off HTTPS is refused; the task then ends without a file.
        completionHandler(request.url?.scheme == "https" ? request : nil)
    }

    func urlSession(_ session: URLSession, task: URLSessionTask, didCompleteWithError error: Error?) {
        if let error {
            finish(.failure(UpdateError.message("The download failed: \(error.localizedDescription)")))
        } else {
            finish(.failure(UpdateError.message("The download did not produce the disk image.")))
        }
    }
}
