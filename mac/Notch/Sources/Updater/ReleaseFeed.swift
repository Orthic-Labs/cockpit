import Foundation

/// Why an update could not be checked, fetched, verified or installed. The
/// message is shown as is in the hub, so it says what happened in plain words.
enum UpdateError: LocalizedError {
    case message(String)

    var errorDescription: String? {
        switch self {
        case .message(let text): return text
        }
    }
}

/// The newest published Pulse release, as much of it as the updater needs.
struct PulseRelease: Codable, Equatable {
    struct Asset: Codable, Equatable {
        let name: String
        let url: URL
        let size: Int64
    }

    /// The git tag, such as `v0.3.0`.
    let tag: String
    /// The release notes as GitHub returns them (Markdown).
    let notes: String
    let page: URL?
    /// The disk image to install, or nil when the release has none.
    let asset: Asset?

    var version: String {
        tag.hasPrefix("v") ? String(tag.dropFirst()) : tag
    }
}

/// Reads the latest release of Pulse from GitHub's public API.
///
/// The request is anonymous and carries an ETag, so an unchanged release costs
/// a `304` with no body. Only `https` asset URLs are accepted.
enum ReleaseFeed {
    static let endpoint = URL(string: "https://api.github.com/repos/Orthic-Labs/pulse/releases/latest")!

    enum Fetch {
        /// A release, with the ETag to send next time.
        case fresh(PulseRelease, etag: String?)
        /// Nothing changed since the ETag that was sent; keep the cached release.
        case unchanged
    }

    static func fetch(etag: String?) async throws -> Fetch {
        var request = URLRequest(url: endpoint, cachePolicy: .reloadIgnoringLocalCacheData, timeoutInterval: 20)
        request.setValue("application/vnd.github+json", forHTTPHeaderField: "Accept")
        request.setValue("2022-11-28", forHTTPHeaderField: "X-GitHub-Api-Version")
        request.setValue(userAgent, forHTTPHeaderField: "User-Agent")
        if let etag { request.setValue(etag, forHTTPHeaderField: "If-None-Match") }

        let result: (Data, URLResponse)
        do {
            result = try await session.data(for: request)
        } catch {
            throw UpdateError.message("Couldn't reach GitHub to check for updates.")
        }
        let (data, response) = result
        guard let http = response as? HTTPURLResponse else {
            throw UpdateError.message("GitHub sent an unreadable answer.")
        }
        switch http.statusCode {
        case 200:
            let decoder = JSONDecoder()
            decoder.keyDecodingStrategy = .convertFromSnakeCase
            guard let decoded = try? decoder.decode(Latest.self, from: data) else {
                throw UpdateError.message("GitHub's release data was not in the expected form.")
            }
            let release = decoded.release
            return .fresh(release, etag: http.value(forHTTPHeaderField: "ETag"))
        case 304:
            return .unchanged
        case 403, 429:
            throw UpdateError.message("GitHub is limiting update checks. Try again later.")
        default:
            throw UpdateError.message("GitHub answered with status \(http.statusCode).")
        }
    }

    private static let session: URLSession = {
        let configuration = URLSessionConfiguration.ephemeral
        configuration.httpCookieStorage = nil
        configuration.httpShouldSetCookies = false
        return URLSession(configuration: configuration)
    }()

    private static var userAgent: String {
        let version = Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? "0"
        return "Pulse/\(version) (macOS)"
    }

    /// The fields of GitHub's release object that the updater reads.
    private struct Latest: Decodable {
        let tagName: String
        let body: String?
        let htmlUrl: URL?
        let assets: [Entry]

        struct Entry: Decodable {
            let name: String
            let browserDownloadUrl: URL
            let size: Int64
        }

        /// Pulse.dmg, or failing that the first Pulse-<version>.dmg.
        var release: PulseRelease {
            let images = assets.filter { $0.browserDownloadUrl.scheme == "https" }
            let chosen = images.first { $0.name == "Pulse.dmg" }
                ?? images.first { $0.name.hasPrefix("Pulse-") && $0.name.hasSuffix(".dmg") }
            return PulseRelease(
                tag: tagName,
                notes: body ?? "",
                page: htmlUrl,
                asset: chosen.map { PulseRelease.Asset(name: $0.name, url: $0.browserDownloadUrl, size: $0.size) })
        }
    }
}
