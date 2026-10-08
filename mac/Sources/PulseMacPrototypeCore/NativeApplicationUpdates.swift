import Darwin
import Foundation

/// Performs one explicit, read-only Sparkle appcast check for one verified app.
///
/// This adapter does not install, download, execute, or persist update data. The
/// caller owns when checks happen; constructing this object performs no I/O.
@MainActor
public final class NativeApplicationUpdates {
    public enum Error: Swift.Error, Equatable {
        case invalidApplicationPath
        case invalidBundleIdentifier
    }

    fileprivate enum FetchError: Swift.Error {
        case authenticationRejected
        case http(Int)
        case invalidResponse
        case oversize
        case redirectDisallowed
        case transport
    }

    fileprivate struct FeedResponse {
        let body: Data
        let contentType: String?
    }

    private struct AppMetadata {
        let bundleID: String
        let currentVersion: String?
        let currentBuildVersion: String?
        let currentShortVersion: String?
        let feedURL: String?
    }

    private struct Version {
        let components: [Int64]

        func compare(to other: Version) -> ComparisonResult {
            let count = max(components.count, other.components.count)
            for index in 0..<count {
                let left = index < components.count ? components[index] : 0
                let right = index < other.components.count ? other.components[index] : 0
                if left < right { return .orderedAscending }
                if left > right { return .orderedDescending }
            }
            return .orderedSame
        }
    }

    fileprivate struct AppcastItem {
        let version: String?
        let shortVersion: String?
        let minimumSystemVersion: String?
        let enclosureURL: String?
    }

    fileprivate struct AppcastResult {
        let items: [AppcastItem]
        let sawRSS: Bool
        let sawChannel: Bool
        let sawItem: Bool
        let entryLimitExceeded: Bool
    }

    private let maxInfoPlistBytes = 256 * 1024
    private let maxFeedURLLength = 4 * 1024
    private let maxResponseBytes = 1024 * 1024
    private let maxXMLDepth = 32
    private let maxXMLTextBytes = 64 * 1024
    private let maxXMLAttributeBytes = 64 * 1024
    private let maxAppcastEntries = 128
    private let timeout: TimeInterval = 10
    private let dataLessFlag: UInt32 = 0x4000_0000

    public init() {}

    /// Check one app's declared Sparkle feed. This method performs network I/O
    /// only because the caller explicitly invokes it.
    public func check(applicationPath: URL, expectedBundleID: String) async throws -> [String: Any] {
        let metadata = try readMetadata(applicationPath: applicationPath, expectedBundleID: expectedBundleID)
        let checkedAt = ISO8601DateFormatter().string(from: Date())
        var result: [String: Any] = [
            "schemaVersion": 1,
            "provider": "sparkle",
            "networkPerformed": false,
            "checkedAt": checkedAt,
            "bundleID": metadata.bundleID
        ]
        if let currentVersion = metadata.currentVersion { result["currentVersion"] = currentVersion }
        if let currentBuildVersion = metadata.currentBuildVersion { result["currentBuildVersion"] = currentBuildVersion }
        if let currentShortVersion = metadata.currentShortVersion { result["currentShortVersion"] = currentShortVersion }

        guard let rawFeedURL = metadata.feedURL else {
            result["available"] = false
            result["state"] = "unavailable"
            result["status"] = "unavailable"
            result["reason"] = "bundle_feed_url_not_declared"
            return removingNilValues(result)
        }

        guard let feedURL = validatedHTTPSURL(rawFeedURL), rawFeedURL.utf8.count <= maxFeedURLLength else {
            result["available"] = false
            result["state"] = "unsupported"
            result["status"] = "unsupported"
            result["feedURL"] = rawFeedURL
            result["reason"] = rawFeedURL.utf8.count > maxFeedURLLength
                ? "feed_url_oversize"
                : "feed_url_must_be_https_without_credentials"
            return result
        }
        result["feedURL"] = rawFeedURL
        result["source"] = "existing_bundle_SUFeedURL"

        let request = makeRequest(url: feedURL)
        let response: FeedResponse
        do {
            let client = FeedClient(maxBytes: maxResponseBytes, timeout: timeout)
            response = try await client.fetch(request)
        } catch let error as FetchError {
            result["available"] = false
            result["state"] = "unavailable"
            result["status"] = "unavailable"
            result["networkPerformed"] = true
            switch error {
            case .http(let code):
                result["reason"] = "http_status_\(code)"
                result["failure"] = "http"
            case .oversize:
                result["reason"] = "response_oversize"
                result["failure"] = "oversize"
            case .redirectDisallowed:
                result["reason"] = "redirect_disallowed"
                result["failure"] = "redirect"
            case .authenticationRejected:
                result["reason"] = "authentication_challenge_rejected"
                result["failure"] = "authentication"
            case .invalidResponse:
                result["reason"] = "invalid_http_response"
                result["failure"] = "response"
            case .transport:
                result["reason"] = "network_unavailable"
                result["failure"] = "transport"
            }
            return result
        } catch {
            result["available"] = false
            result["state"] = "unavailable"
            result["status"] = "unavailable"
            result["networkPerformed"] = true
            result["reason"] = "network_unavailable"
            result["failure"] = "transport"
            return result
        }

        result["networkPerformed"] = true
        guard response.contentTypeIsXML else {
            result["available"] = false
            result["state"] = "unsupported"
            result["status"] = "unsupported"
            result["reason"] = "unsupported_content_type"
            return result
        }

        let parsed: AppcastResult
        do {
            parsed = try parseAppcast(response.body)
        } catch {
            result["available"] = false
            result["state"] = "malformed"
            result["status"] = "malformed"
            result["reason"] = "malformed_appcast"
            return result
        }
        guard parsed.sawRSS, parsed.sawChannel, parsed.sawItem, !parsed.entryLimitExceeded else {
            result["available"] = false
            result["state"] = "unsupported"
            result["status"] = "unsupported"
            result["reason"] = parsed.entryLimitExceeded ? "appcast_entry_limit_exceeded" : "unsupported_appcast_format"
            return result
        }

        let candidates = parsed.items.compactMap { item -> (AppcastItem, Version)? in
            guard let rawVersion = item.version,
                  let version = numericVersion(rawVersion),
                  let rawEnclosure = item.enclosureURL,
                  validatedHTTPSURL(rawEnclosure) != nil else { return nil }
            if let minimum = item.minimumSystemVersion {
                guard let minimumVersion = numericVersion(minimum),
                      minimumVersion.compare(to: currentOperatingSystemVersion()) != .orderedDescending else {
                    return nil
                }
            }
            return (item, version)
        }
        guard let candidate = candidates.max(by: { $0.1.compare(to: $1.1) == .orderedAscending }) else {
            result["available"] = false
            result["state"] = "unsupported"
            result["status"] = "unsupported"
            result["reason"] = "no_supported_appcast_entry"
            return result
        }

        let item = candidate.0
        if let value = item.version { result["candidateVersion"] = value }
        if let value = item.shortVersion { result["candidateShortVersion"] = value }
        if let value = item.minimumSystemVersion { result["candidateMinimumSystemVersion"] = value }
        // This URL is metadata only. No request is ever made to it.
        if let value = item.enclosureURL, validatedHTTPSURL(value) != nil { result["enclosureURL"] = value }

        guard let current = metadata.currentBuildVersion,
              let currentNumeric = numericVersion(current) else {
            result["available"] = false
            result["state"] = "unsupported"
            result["status"] = "unsupported"
            result["comparison"] = "unknown"
            result["reason"] = "version_comparison_unknown"
            return result
        }

        let comparison = currentNumeric.compare(to: candidate.1)
        result["available"] = comparison == .orderedAscending
        result["state"] = comparison == .orderedAscending ? "available" : "no-update"
        result["status"] = comparison == .orderedAscending ? "available" : "no-update"
        result["comparison"] = comparison == .orderedAscending ? "newer"
            : (comparison == .orderedSame ? "same" : "older")
        result["reason"] = comparison == .orderedAscending ? "newer_version_available" : "no_newer_version"
        return result
    }

    private func readMetadata(applicationPath: URL, expectedBundleID: String) throws -> AppMetadata {
        guard validBundleIdentifier(expectedBundleID) else { throw Error.invalidBundleIdentifier }
        let rawPath = applicationPath.path
        guard applicationPath.isFileURL,
              applicationPath.scheme?.lowercased() == "file",
              applicationPath.host == nil,
              applicationPath.query == nil,
              applicationPath.fragment == nil,
              !rawPath.isEmpty,
              rawPath.hasPrefix("/"),
              !rawPath.utf8.contains(0),
              !rawPath.contains("//"),
              !rawPath.hasSuffix("/"),
              rawPath.split(separator: "/", omittingEmptySubsequences: false).dropFirst().allSatisfy({ $0 != "." && $0 != ".." }),
              applicationPath.pathExtension.caseInsensitiveCompare("app") == .orderedSame else {
            throw Error.invalidApplicationPath
        }

        let appDescriptor = try openVerifiedDirectory(applicationPath)
        defer { close(appDescriptor) }
        var preOpenContentsStat = stat()
        let preOpenContentsResult = "Contents".withCString {
            fstatat(appDescriptor, $0, &preOpenContentsStat, AT_SYMLINK_NOFOLLOW)
        }
        guard preOpenContentsResult == 0,
              preOpenContentsStat.st_mode & S_IFMT == S_IFDIR,
              !isDataLess(preOpenContentsStat),
              isLocalFileSystem(appDescriptor) else {
            throw Error.invalidApplicationPath
        }
        let contentsDescriptor = "Contents".withCString {
            openat(appDescriptor, $0, O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)
        }
        guard contentsDescriptor >= 0 else { throw Error.invalidApplicationPath }
        defer { close(contentsDescriptor) }
        var contentsStat = stat()
        guard fstat(contentsDescriptor, &contentsStat) == 0,
              statMatches(preOpenContentsStat, contentsStat),
              isLocalFileSystem(contentsDescriptor) else {
            throw Error.invalidApplicationPath
        }

        var preOpenPlistStat = stat()
        let preOpenResult = "Info.plist".withCString {
            fstatat(contentsDescriptor, $0, &preOpenPlistStat, AT_SYMLINK_NOFOLLOW)
        }
        guard preOpenResult == 0,
              preOpenPlistStat.st_mode & S_IFMT == S_IFREG,
              !isDataLess(preOpenPlistStat),
              preOpenPlistStat.st_size >= 0,
              preOpenPlistStat.st_size <= off_t(maxInfoPlistBytes),
              isLocalFileSystem(contentsDescriptor) else {
            throw Error.invalidApplicationPath
        }
        let plistDescriptor = "Info.plist".withCString {
            openat(contentsDescriptor, $0, O_RDONLY | O_NONBLOCK | O_NOFOLLOW | O_CLOEXEC)
        }
        guard plistDescriptor >= 0 else { throw Error.invalidApplicationPath }
        defer { close(plistDescriptor) }
        var plistStat = stat()
        guard fstat(plistDescriptor, &plistStat) == 0,
              statMatches(preOpenPlistStat, plistStat),
              isLocalFileSystem(plistDescriptor) else {
            throw Error.invalidApplicationPath
        }
        let data = try readBounded(descriptor: plistDescriptor, size: Int(plistStat.st_size), limit: maxInfoPlistBytes)
        var postReadPlistStat = stat()
        guard fstat(plistDescriptor, &postReadPlistStat) == 0,
              statMatches(preOpenPlistStat, postReadPlistStat) else {
            throw Error.invalidApplicationPath
        }
        guard let plist = try? PropertyListSerialization.propertyList(from: data, options: [], format: nil),
              let dictionary = plist as? [String: Any],
              let bundleID = dictionary["CFBundleIdentifier"] as? String,
              validBundleIdentifier(bundleID) else { throw Error.invalidApplicationPath }
        guard bundleID == expectedBundleID else { throw Error.invalidBundleIdentifier }

        let currentBuildVersion = dictionary["CFBundleVersion"] as? String
        let currentShortVersion = dictionary["CFBundleShortVersionString"] as? String
        let feedURL = (dictionary["SUFeedURL"] as? String)?.trimmingCharacters(in: .whitespacesAndNewlines)
        let displayVersion = currentShortVersion ?? currentBuildVersion
        return AppMetadata(bundleID: bundleID, currentVersion: displayVersion?.isEmpty == true ? nil : displayVersion,
                           currentBuildVersion: currentBuildVersion?.isEmpty == true ? nil : currentBuildVersion,
                           currentShortVersion: currentShortVersion?.isEmpty == true ? nil : currentShortVersion,
                           feedURL: feedURL?.isEmpty == true ? nil : feedURL)
    }

    private func openVerifiedDirectory(_ url: URL) throws -> Int32 {
        var descriptor = Darwin.open("/", O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)
        guard descriptor >= 0 else { throw Error.invalidApplicationPath }
        let components = url.pathComponents.dropFirst()
        for component in components {
            guard component != ".", component != "..", !component.isEmpty else {
                close(descriptor)
                throw Error.invalidApplicationPath
            }
            var preOpenStat = stat()
            let preOpenResult = component.withCString {
                fstatat(descriptor, $0, &preOpenStat, AT_SYMLINK_NOFOLLOW)
            }
            guard preOpenResult == 0,
                  preOpenStat.st_mode & S_IFMT == S_IFDIR,
                  !isDataLess(preOpenStat),
                  isLocalFileSystem(descriptor) else {
                close(descriptor)
                throw Error.invalidApplicationPath
            }
            let next = component.withCString {
                openat(descriptor, $0, O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)
            }
            guard next >= 0 else {
                close(descriptor)
                throw Error.invalidApplicationPath
            }
            var value = stat()
            guard fstat(next, &value) == 0,
                  statMatches(preOpenStat, value),
                  isLocalFileSystem(next) else {
                close(next)
                close(descriptor)
                throw Error.invalidApplicationPath
            }
            close(descriptor)
            descriptor = next
        }
        return descriptor
    }

    private func readBounded(descriptor: Int32, size: Int, limit: Int) throws -> Data {
        guard size >= 0, size <= limit else { throw FetchError.oversize }
        var data = Data(count: size)
        var offset = 0
        while offset < size {
            let count = data.withUnsafeMutableBytes { rawBuffer -> Int in
                guard let base = rawBuffer.baseAddress else { return -1 }
                return Darwin.read(descriptor, base.advanced(by: offset), size - offset)
            }
            if count > 0 { offset += count; continue }
            if count < 0, errno == EINTR { continue }
            throw Error.invalidApplicationPath
        }
        return data
    }

    private func makeRequest(url: URL) -> URLRequest {
        var request = URLRequest(url: url)
        request.httpMethod = "GET"
        request.cachePolicy = .reloadIgnoringLocalCacheData
        request.timeoutInterval = timeout
        request.setValue("application/rss+xml, application/xml, text/xml;q=0.9", forHTTPHeaderField: "Accept")
        return request
    }

    private func parseAppcast(_ data: Data) throws -> AppcastResult {
        // Limit admission to UTF-8 feeds without DTD/entity declarations before
        // handing untrusted bytes to XMLParser. Entity expansion can allocate
        // before delegate text limits run; unsupported encodings stay explicit.
        guard let source = String(data: data, encoding: .utf8),
              !source.contains("<!DOCTYPE"), !source.contains("<!ENTITY") else {
            throw FetchError.transport
        }
        let parserDelegate = SparkleAppcastParser(maxDepth: maxXMLDepth, maxTextBytes: maxXMLTextBytes,
                                                   maxAttributeBytes: maxXMLAttributeBytes,
                                                   maxEntries: maxAppcastEntries)
        let parser = XMLParser(data: data)
        parser.delegate = parserDelegate
        parser.shouldResolveExternalEntities = false
        guard parser.parse(), !parserDelegate.failed else { throw FetchError.transport }
        return parserDelegate.result
    }

    private func numericVersion(_ value: String) -> Version? {
        let text = value.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty, text.utf8.count <= 256 else { return nil }
        let parts = text.split(separator: ".", omittingEmptySubsequences: false)
        guard !parts.isEmpty else { return nil }
        var components: [Int64] = []
        for part in parts {
            guard !part.isEmpty, part.utf8.count <= 18,
                  part.unicodeScalars.allSatisfy({ $0.value >= 48 && $0.value <= 57 }),
                  let number = Int64(part) else { return nil }
            components.append(number)
        }
        return Version(components: components)
    }

    private func currentOperatingSystemVersion() -> Version {
        let value = ProcessInfo.processInfo.operatingSystemVersion
        return Version(components: [Int64(value.majorVersion), Int64(value.minorVersion), Int64(value.patchVersion)])
    }

    private func validatedHTTPSURL(_ raw: String) -> URL? {
        guard raw.utf8.count <= maxFeedURLLength,
              let components = URLComponents(string: raw),
              components.scheme?.lowercased() == "https",
              let host = components.host, !host.isEmpty,
              components.user == nil, components.password == nil,
              let url = components.url else { return nil }
        return url
    }

    private func validBundleIdentifier(_ value: String) -> Bool {
        let text = value.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty, text.utf8.count <= 512 else { return false }
        return text.split(separator: ".", omittingEmptySubsequences: false).allSatisfy { part in
            !part.isEmpty && part.unicodeScalars.allSatisfy {
                ($0.value >= 48 && $0.value <= 57) || ($0.value >= 65 && $0.value <= 90)
                    || ($0.value >= 97 && $0.value <= 122) || $0.value == 45 || $0.value == 95
            }
        }
    }

    private func isDataLess(_ value: stat) -> Bool {
        (UInt32(value.st_flags) & dataLessFlag) != 0
    }

    private func statMatches(_ expected: stat, _ actual: stat) -> Bool {
        expected.st_dev == actual.st_dev
            && expected.st_ino == actual.st_ino
            && expected.st_mode == actual.st_mode
            && expected.st_flags == actual.st_flags
            && expected.st_size == actual.st_size
            && expected.st_mtimespec.tv_sec == actual.st_mtimespec.tv_sec
            && expected.st_mtimespec.tv_nsec == actual.st_mtimespec.tv_nsec
            && expected.st_ctimespec.tv_sec == actual.st_ctimespec.tv_sec
            && expected.st_ctimespec.tv_nsec == actual.st_ctimespec.tv_nsec
    }

    private func isLocalFileSystem(_ descriptor: Int32) -> Bool {
        var fileSystem = statfs()
        guard fstatfs(descriptor, &fileSystem) == 0 else { return false }
        return (fileSystem.f_flags & UInt32(MNT_LOCAL)) != 0
    }

    private func removingNilValues(_ value: [String: Any]) -> [String: Any] {
        value.filter { !($0.value is NSNull) }
    }
}

private extension NativeApplicationUpdates.FeedResponse {
    var contentTypeIsXML: Bool {
        guard let contentType else { return true }
        let mediaType = contentType.split(separator: ";", maxSplits: 1, omittingEmptySubsequences: true)
            .first.map { $0.trimmingCharacters(in: .whitespacesAndNewlines).lowercased() } ?? ""
        return mediaType.isEmpty || mediaType == "application/rss+xml" || mediaType == "application/xml"
            || mediaType == "text/xml" || mediaType == "application/atom+xml"
    }
}

private final class FeedClient: NSObject, URLSessionDataDelegate, URLSessionTaskDelegate {
    private let maxBytes: Int
    private let timeout: TimeInterval
    private var continuation: CheckedContinuation<NativeApplicationUpdates.FeedResponse, Swift.Error>?
    private var session: URLSession?
    private var task: URLSessionDataTask?
    private var response: HTTPURLResponse?
    private var body = Data()
    private var finished = false

    init(maxBytes: Int, timeout: TimeInterval) {
        self.maxBytes = maxBytes
        self.timeout = timeout
    }

    func fetch(_ request: URLRequest) async throws -> NativeApplicationUpdates.FeedResponse {
        try await withCheckedThrowingContinuation { continuation in
            self.continuation = continuation
            let configuration = URLSessionConfiguration.ephemeral
            configuration.httpCookieStorage = nil
            configuration.urlCredentialStorage = nil
            configuration.httpShouldSetCookies = false
            configuration.httpCookieAcceptPolicy = .never
            configuration.requestCachePolicy = .reloadIgnoringLocalCacheData
            configuration.timeoutIntervalForRequest = min(timeout, 10)
            configuration.timeoutIntervalForResource = min(timeout, 10)
            configuration.waitsForConnectivity = false
            configuration.connectionProxyDictionary = [:]
            let queue = OperationQueue()
            queue.maxConcurrentOperationCount = 1
            queue.qualityOfService = .utility
            let session = URLSession(configuration: configuration, delegate: self, delegateQueue: queue)
            self.session = session
            let task = session.dataTask(with: request)
            self.task = task
            task.resume()
        }
    }

    func urlSession(_ session: URLSession, dataTask: URLSessionDataTask, didReceive response: URLResponse,
                    completionHandler: @escaping (URLSession.ResponseDisposition) -> Void) {
        guard !finished else { completionHandler(.cancel); return }
        guard let http = response as? HTTPURLResponse else {
            finish(.failure(NativeApplicationUpdates.FetchError.invalidResponse))
            completionHandler(.cancel)
            return
        }
        self.response = http
        guard (200...299).contains(http.statusCode) else {
            finish(.failure(NativeApplicationUpdates.FetchError.http(http.statusCode)))
            completionHandler(.cancel)
            return
        }
        if http.expectedContentLength > Int64(maxBytes) {
            finish(.failure(NativeApplicationUpdates.FetchError.oversize))
            completionHandler(.cancel)
            return
        }
        completionHandler(.allow)
    }

    func urlSession(_ session: URLSession, dataTask: URLSessionDataTask, didReceive data: Data) {
        guard !finished else { return }
        guard data.count <= maxBytes - body.count else {
            finish(.failure(NativeApplicationUpdates.FetchError.oversize))
            task?.cancel()
            return
        }
        body.append(data)
    }

    func urlSession(_ session: URLSession, task: URLSessionTask,
                    willPerformHTTPRedirection response: HTTPURLResponse,
                    newRequest request: URLRequest,
                    completionHandler: @escaping (URLRequest?) -> Void) {
        finish(.failure(NativeApplicationUpdates.FetchError.redirectDisallowed))
        completionHandler(nil)
    }

    func urlSession(_ session: URLSession, task: URLSessionTask,
                    didReceive challenge: URLAuthenticationChallenge,
                    completionHandler: @escaping (URLSession.AuthChallengeDisposition, URLCredential?) -> Void) {
        if challenge.protectionSpace.authenticationMethod == NSURLAuthenticationMethodServerTrust {
            completionHandler(.performDefaultHandling, nil)
        } else {
            finish(.failure(NativeApplicationUpdates.FetchError.authenticationRejected))
            completionHandler(.cancelAuthenticationChallenge, nil)
        }
    }

    func urlSession(_ session: URLSession, task: URLSessionTask, didCompleteWithError error: Swift.Error?) {
        guard !finished else { return }
        if error != nil {
            finish(.failure(NativeApplicationUpdates.FetchError.transport))
        } else if let response {
            finish(.success(NativeApplicationUpdates.FeedResponse(body: body,
                                                                  contentType: response.value(forHTTPHeaderField: "Content-Type"))))
        } else {
            finish(.failure(NativeApplicationUpdates.FetchError.invalidResponse))
        }
    }

    private func finish(_ result: Result<NativeApplicationUpdates.FeedResponse, Swift.Error>) {
        guard !finished else { return }
        finished = true
        session?.invalidateAndCancel()
        continuation?.resume(with: result)
        continuation = nil
    }
}

private final class SparkleAppcastParser: NSObject, XMLParserDelegate {
    private struct ItemBuilder {
        var version: String?
        var shortVersion: String?
        var minimumSystemVersion: String?
        var enclosureURL: String?
    }

    private let maxDepth: Int
    private let maxTextBytes: Int
    private let maxAttributeBytes: Int
    private let maxEntries: Int
    private(set) var failed = false
    private(set) var result = NativeApplicationUpdates.AppcastResult(items: [], sawRSS: false, sawChannel: false,
                                                                      sawItem: false, entryLimitExceeded: false)
    private var depth = 0
    private var currentItem: ItemBuilder?
    private var itemDepth = 0
    private var activeField: String?
    private var activeFieldDepth = 0
    private var activeText = ""
    private var items: [NativeApplicationUpdates.AppcastItem] = []
    private var sawRSS = false
    private var sawChannel = false
    private var sawItem = false
    private var entryLimitExceeded = false

    init(maxDepth: Int, maxTextBytes: Int, maxAttributeBytes: Int, maxEntries: Int) {
        self.maxDepth = maxDepth
        self.maxTextBytes = maxTextBytes
        self.maxAttributeBytes = maxAttributeBytes
        self.maxEntries = maxEntries
    }

    func parser(_ parser: XMLParser, didStartElement elementName: String, namespaceURI: String?,
                qualifiedName qName: String?, attributes attributeDict: [String: String] = [:]) {
        guard !failed else { return }
        depth += 1
        guard depth <= maxDepth else { abort(parser); return }
        let local = localName(elementName)
        if attributeDict.count > 32 || attributeDict.reduce(0, { $0 + $1.key.utf8.count + $1.value.utf8.count }) > maxAttributeBytes {
            abort(parser)
            return
        }
        if depth == 1, local == "rss" { sawRSS = true }
        if local == "channel" { sawChannel = true }
        if local == "item" {
            sawItem = true
            guard currentItem == nil else { abort(parser); return }
            if items.count >= maxEntries {
                entryLimitExceeded = true
                abort(parser)
                return
            }
            currentItem = ItemBuilder()
            itemDepth = depth
        } else if local == "enclosure", currentItem != nil {
            currentItem?.enclosureURL = attributeDict.first(where: { localName($0.key) == "url" })?.value
            if let value = attributeDict.first(where: { localName($0.key) == "version" })?.value {
                currentItem?.version = value
            }
            if let value = attributeDict.first(where: { localName($0.key) == "shortversionstring" })?.value {
                currentItem?.shortVersion = value
            }
            if let value = attributeDict.first(where: { localName($0.key) == "minimumsystemversion" })?.value {
                currentItem?.minimumSystemVersion = value
            }
        }
        if ["version", "shortversionstring", "minimumsystemversion"].contains(local), currentItem != nil {
            activeField = local
            activeFieldDepth = depth
            activeText = ""
        }
    }

    func parser(_ parser: XMLParser, foundCharacters string: String) {
        appendText(parser, string: string)
    }

    func parser(_ parser: XMLParser, foundCDATA CDATABlock: Data) {
        appendText(parser, string: String(decoding: CDATABlock, as: UTF8.self))
    }

    func parser(_ parser: XMLParser, didEndElement elementName: String, namespaceURI: String?, qualifiedName qName: String?) {
        guard !failed else { return }
        let local = localName(elementName)
        if let activeField, depth == activeFieldDepth, var item = currentItem {
            let value = activeText.trimmingCharacters(in: .whitespacesAndNewlines)
            if activeField == "version" { item.version = value }
            if activeField == "shortversionstring" { item.shortVersion = value }
            if activeField == "minimumsystemversion" { item.minimumSystemVersion = value }
            currentItem = item
            self.activeField = nil
            activeText = ""
        }
        if local == "item", depth == itemDepth, let item = currentItem {
            items.append(NativeApplicationUpdates.AppcastItem(version: item.version,
                                                              shortVersion: item.shortVersion,
                                                              minimumSystemVersion: item.minimumSystemVersion,
                                                              enclosureURL: item.enclosureURL))
            currentItem = nil
            itemDepth = 0
        }
        depth -= 1
    }

    func parser(_ parser: XMLParser, parseErrorOccurred parseError: Swift.Error) {
        failed = true
    }

    func parser(_ parser: XMLParser, validationErrorOccurred validationError: Swift.Error) {
        failed = true
    }

    func parser(_ parser: XMLParser, resolveExternalEntityName name: String, systemID: String?) -> Data? {
        abort(parser)
        return nil
    }

    func parser(_ parser: XMLParser, foundExternalEntityWithName name: String, systemID: String?) {
        abort(parser)
    }

    func parser(_ parser: XMLParser, foundIgnorableWhitespace whitespaceString: String) {}

    func parserDidEndDocument(_ parser: XMLParser) {
        result = NativeApplicationUpdates.AppcastResult(items: items, sawRSS: sawRSS, sawChannel: sawChannel,
                                                        sawItem: sawItem, entryLimitExceeded: entryLimitExceeded)
    }

    private func appendText(_ parser: XMLParser, string: String) {
        guard !failed, activeField != nil, depth >= activeFieldDepth else { return }
        guard activeText.utf8.count + string.utf8.count <= maxTextBytes else { abort(parser); return }
        activeText.append(string)
    }

    private func abort(_ parser: XMLParser) {
        failed = true
        parser.abortParsing()
    }

    private func localName(_ name: String) -> String {
        name.split(separator: ":", maxSplits: 1).last.map(String.init)?.lowercased() ?? name.lowercased()
    }
}
