import Foundation
import Security

// MIT adaptation notice: this reader adapts Codenotch's credential, header, and
// usage-window semantics from upstream/codenotch (see cockpit/docs/donors.md and
// upstream/codenotch/LICENSE). It is intentionally standalone & dependency-free.

struct AIUsageReading: Sendable {
    let id: String
    let fraction: Double?
    let detail: String
    let observedAt: Date?
}

private final class UsageRedirectPolicy: NSObject, URLSessionTaskDelegate {
    func urlSession(_ session: URLSession, task: URLSessionTask,
                    willPerformHTTPRedirection response: HTTPURLResponse,
                    newRequest request: URLRequest,
                    completionHandler: @escaping (URLRequest?) -> Void) {
        completionHandler(nil)
    }
}

/// Read-only usage sampling for the default Claude Code & Codex profiles.
actor AIUsageSampler {
    private static let maxResponseBytes = 1_048_576
    private static let requestTimeout: TimeInterval = 15
    private static let claudeService = "Claude Code-credentials"
    private static let claudeEndpoint = URL(string: "https://api.anthropic.com/api/oauth/usage?cedar_ember=1")!
    private static let codexEndpoint = URL(string: "https://chatgpt.com/backend-api/wham/usage")!

    private struct Sample: Sendable {
        let fraction: Double
        let detail: String
    }

    private enum SamplerError: Error, Sendable {
        case unavailable
        case cancelled
        case rateLimited(TimeInterval)
        case responseTooLarge
    }

    private struct ClaudeWindow: Decodable {
        let utilization: Double?
    }

    private struct ClaudeLimit: Decodable {
        let kind: String
        let percent: Double?
    }

    private struct ClaudePayload: Decodable {
        let fiveHour: ClaudeWindow?
        let sevenDay: ClaudeWindow?
        let weekly: ClaudeWindow?
        let limits: [ClaudeLimit]?

        private enum CodingKeys: String, CodingKey {
            case fiveHour = "five_hour"
            case sevenDay = "seven_day"
            case weekly
            case limits
        }
    }

    private struct CodexWindow: Decodable {
        let usedPercent: Double?

        private enum CodingKeys: String, CodingKey {
            case usedPercent = "used_percent"
        }
    }

    private struct CodexRateLimit: Decodable {
        let primaryWindow: CodexWindow?

        private enum CodingKeys: String, CodingKey {
            case primaryWindow = "primary_window"
        }
    }

    private struct CodexPayload: Decodable {
        let rateLimit: CodexRateLimit?

        private enum CodingKeys: String, CodingKey {
            case rateLimit = "rate_limit"
        }
    }

    private struct CodexAuth: Decodable {
        struct Tokens: Decodable {
            let accessToken: String
            let accountID: String

            private enum CodingKeys: String, CodingKey {
                case accessToken = "access_token"
                case accountID = "account_id"
            }
        }

        let tokens: Tokens
    }

    private struct KeychainMatch {
        let modifiedAt: Date?
        let persistentRef: Data
    }

    private var previous: [String: AIUsageReading] = [:]
    private var retryNotBefore: [String: Date] = [:]
    private let session: URLSession

    init() {
        let config = URLSessionConfiguration.ephemeral
        config.timeoutIntervalForRequest = Self.requestTimeout
        config.timeoutIntervalForResource = Self.requestTimeout
        config.httpShouldSetCookies = false
        config.httpCookieStorage = nil
        config.urlCredentialStorage = nil
        config.urlCache = nil
        self.session = URLSession(configuration: config, delegate: UsageRedirectPolicy(), delegateQueue: nil)
    }

    func refresh() async -> [AIUsageReading] {
        guard !Task.isCancelled else { return [] }

        async let claudeResult = readClaude()
        async let codexResult = readCodex()
        let claude = await reading(id: "claude", result: claudeResult)
        let chatgpt = await reading(id: "chatgpt", result: codexResult)
        return [claude, chatgpt]
    }

    private func readClaude() async -> Result<Sample, SamplerError> {
        guard !Task.isCancelled else { return .failure(.cancelled) }
        if let retryNotBefore = retryNotBefore["claude"], retryNotBefore > Date() {
            return .failure(.rateLimited(retryNotBefore.timeIntervalSinceNow))
        }

        guard let token = readClaudeToken(), !token.isEmpty else {
            return .failure(.unavailable)
        }
        guard !Task.isCancelled else { return .failure(.cancelled) }

        var request = URLRequest(url: Self.claudeEndpoint,
                                 cachePolicy: .reloadIgnoringLocalCacheData,
                                 timeoutInterval: Self.requestTimeout)
        request.httpMethod = "GET"
        request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        request.setValue("oauth-2025-04-20", forHTTPHeaderField: "anthropic-beta")
        request.setValue("application/json", forHTTPHeaderField: "Accept")
        request.setValue("no-cache, no-store", forHTTPHeaderField: "Cache-Control")

        do {
            let (data, response) = try await boundedData(for: request)
            guard let http = response as? HTTPURLResponse else { return .failure(.unavailable) }
            if http.statusCode == 429 {
                let delay = max(60, retryAfter(from: http) ?? 0)
                retryNotBefore["claude"] = Date().addingTimeInterval(delay)
                return .failure(.rateLimited(delay))
            }
            guard (200..<300).contains(http.statusCode) else { return .failure(.unavailable) }

            let payload = try JSONDecoder().decode(ClaudePayload.self, from: data)
            let fiveHour = validFraction(payload.fiveHour?.utilization)
            let weeklyWindow = payload.sevenDay ?? payload.weekly
            let weekly = validFraction(weeklyWindow?.utilization)
                ?? payload.limits?.first(where: { $0.kind == "weekly_all" || $0.kind.hasPrefix("weekly_") }).flatMap { validFraction($0.percent) }
            let session = fiveHour
                ?? payload.limits?.first(where: { $0.kind == "five_hour" || $0.kind == "session" }).flatMap { validFraction($0.percent) }
            guard let fraction = session ?? weekly else { return .failure(.unavailable) }
            retryNotBefore["claude"] = nil
            return .success(Sample(fraction: fraction, detail: detail(provider: "Claude", session: session, weekly: weekly)))
        } catch SamplerError.responseTooLarge {
            return .failure(.responseTooLarge)
        } catch is CancellationError {
            return .failure(.cancelled)
        } catch {
            return .failure(.unavailable)
        }
    }

    private func readCodex() async -> Result<Sample, SamplerError> {
        guard !Task.isCancelled else { return .failure(.cancelled) }
        if let retryNotBefore = retryNotBefore["chatgpt"], retryNotBefore > Date() {
            return .failure(.rateLimited(retryNotBefore.timeIntervalSinceNow))
        }

        guard let credential = readCodexCredential() else {
            return .failure(.unavailable)
        }
        guard !Task.isCancelled else { return .failure(.cancelled) }

        var request = URLRequest(url: Self.codexEndpoint,
                                 cachePolicy: .reloadIgnoringLocalCacheData,
                                 timeoutInterval: Self.requestTimeout)
        request.httpMethod = "GET"
        request.setValue("Bearer \(credential.accessToken)", forHTTPHeaderField: "Authorization")
        request.setValue(credential.accountID, forHTTPHeaderField: "ChatGPT-Account-Id")
        request.setValue("application/json", forHTTPHeaderField: "Accept")
        request.setValue("no-cache, no-store", forHTTPHeaderField: "Cache-Control")

        do {
            let (data, response) = try await boundedData(for: request)
            guard let http = response as? HTTPURLResponse else { return .failure(.unavailable) }
            if http.statusCode == 429 {
                let delay = max(60, retryAfter(from: http) ?? 0)
                retryNotBefore["chatgpt"] = Date().addingTimeInterval(delay)
                return .failure(.rateLimited(delay))
            }
            guard (200..<300).contains(http.statusCode) else { return .failure(.unavailable) }

            let payload = try JSONDecoder().decode(CodexPayload.self, from: data)
            guard let fraction = validFraction(payload.rateLimit?.primaryWindow?.usedPercent) else {
                return .failure(.unavailable)
            }
            retryNotBefore["chatgpt"] = nil
            return .success(Sample(fraction: fraction,
                                   detail: "ChatGPT/Codex · primary window \(percent(fraction))"))
        } catch SamplerError.responseTooLarge {
            return .failure(.responseTooLarge)
        } catch is CancellationError {
            return .failure(.cancelled)
        } catch {
            return .failure(.unavailable)
        }
    }

    private func reading(id: String, result: Result<Sample, SamplerError>) -> AIUsageReading {
        switch result {
        case .success(let sample):
            let reading = AIUsageReading(id: id, fraction: sample.fraction,
                                         detail: "Live · \(sample.detail)", observedAt: Date())
            previous[id] = reading
            return reading
        case .failure(let error):
            guard let prior = previous[id], let fraction = prior.fraction else {
                return AIUsageReading(id: id, fraction: nil,
                                      detail: failureDetail(for: id, error: error), observedAt: nil)
            }
            let timestamp = prior.observedAt.map(Self.timestamp) ?? "unknown"
            return AIUsageReading(id: id, fraction: fraction,
                                  detail: "Stale · \(failureDetail(for: id, error: error)) · observed \(timestamp)",
                                  observedAt: prior.observedAt)
        }
    }

    private func readClaudeToken() -> String? {
        guard let match = newestKeychainMatch() else { return nil }
        var query: [CFString: Any] = [
            kSecClass: kSecClassGenericPassword,
            kSecValuePersistentRef: match.persistentRef,
            kSecReturnData: true,
            kSecMatchLimit: kSecMatchLimitOne,
            kSecUseAuthenticationUI: kSecUseAuthenticationUIFail
        ]
        var result: CFTypeRef?
        guard SecItemCopyMatching(query as CFDictionary, &result) == errSecSuccess,
              let data = result as? Data,
              let auth = try? JSONDecoder().decode(ClaudeAuth.self, from: data),
              auth.expiresAt > Date().timeIntervalSince1970 * 1000,
              !auth.accessToken.isEmpty else { return nil }
        query.removeAll(keepingCapacity: false)
        return auth.accessToken
    }

    private struct ClaudeAuth: Decodable {
        struct OAuth: Decodable {
            let accessToken: String
            let expiresAt: Double

            private enum CodingKeys: String, CodingKey {
                case accessToken
                case expiresAt
            }
        }

        let oauth: OAuth

        private enum CodingKeys: String, CodingKey {
            case oauth = "claudeAiOauth"
        }

        var accessToken: String { oauth.accessToken }
        var expiresAt: Double { oauth.expiresAt }
    }

    private struct CodexCredential {
        let accessToken: String
        let accountID: String
    }

    private func readCodexCredential() -> CodexCredential? {
        let url = URL(fileURLWithPath: NSHomeDirectory()).appendingPathComponent(".codex/auth.json")
        guard let handle = try? FileHandle(forReadingFrom: url) else { return nil }
        defer { try? handle.close() }
        guard let data = try? handle.read(upToCount: Self.maxResponseBytes + 1),
              data.count <= Self.maxResponseBytes,
              let auth = try? JSONDecoder().decode(CodexAuth.self, from: data),
              !auth.tokens.accessToken.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty,
              !auth.tokens.accountID.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else { return nil }
        return CodexCredential(accessToken: auth.tokens.accessToken, accountID: auth.tokens.accountID)
    }

    private func newestKeychainMatch() -> KeychainMatch? {
        let query: [CFString: Any] = [
            kSecClass: kSecClassGenericPassword,
            kSecAttrService: Self.claudeService,
            kSecReturnAttributes: true,
            kSecReturnPersistentRef: true,
            kSecMatchLimit: kSecMatchLimitAll,
            kSecUseAuthenticationUI: kSecUseAuthenticationUIFail
        ]
        var result: CFTypeRef?
        guard SecItemCopyMatching(query as CFDictionary, &result) == errSecSuccess else { return nil }
        let items = (result as? [[CFString: Any]]) ?? (result as? [CFString: Any]).map { [$0] } ?? []
        return items.compactMap { item in
            guard let ref = item[kSecValuePersistentRef] as? Data else { return nil }
            return KeychainMatch(modifiedAt: item[kSecAttrModificationDate] as? Date, persistentRef: ref)
        }.max { ($0.modifiedAt ?? .distantPast) < ($1.modifiedAt ?? .distantPast) }
    }

    private func boundedData(for request: URLRequest) async throws -> (Data, URLResponse) {
        guard !Task.isCancelled else { throw CancellationError() }
        let (bytes, response) = try await session.bytes(for: request)
        defer { bytes.task.cancel() }
        if response.expectedContentLength > Int64(Self.maxResponseBytes) {
            throw SamplerError.responseTooLarge
        }
        var data = Data()
        data.reserveCapacity(response.expectedContentLength > 0 ? Int(response.expectedContentLength) : 4096)
        for try await byte in bytes {
            guard !Task.isCancelled else { throw CancellationError() }
            guard data.count < Self.maxResponseBytes else { throw SamplerError.responseTooLarge }
            data.append(byte)
        }
        return (data, response)
    }

    private func validFraction(_ percent: Double?) -> Double? {
        guard let percent, percent.isFinite, (0...100).contains(percent) else { return nil }
        return percent / 100
    }

    private func detail(provider: String, session: Double?, weekly: Double?) -> String {
        var values = [String]()
        if let session { values.append("5h \(percent(session))") }
        if let weekly { values.append("weekly \(percent(weekly))") }
        return "\(provider) OAuth · " + values.joined(separator: " · ")
    }

    private func percent(_ fraction: Double) -> String {
        String(format: "%.0f%%", fraction * 100)
    }

    private func failureDetail(for id: String, error: SamplerError) -> String {
        let provider = id == "claude" ? "Claude" : "ChatGPT/Codex"
        switch error {
        case .rateLimited(let delay):
            return "\(provider) rate limited · retry after \(Int(ceil(delay)))s"
        case .cancelled:
            return "\(provider) unavailable"
        case .unavailable, .responseTooLarge:
            return "\(provider) unavailable"
        }
    }

    private static func timestamp(_ date: Date) -> String {
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return formatter.string(from: date)
    }

    private func retryAfter(from response: HTTPURLResponse) -> TimeInterval? {
        guard let value = response.value(forHTTPHeaderField: "Retry-After")?.trimmingCharacters(in: .whitespacesAndNewlines),
              !value.isEmpty else { return nil }
        if let seconds = TimeInterval(value) { return max(0, seconds) }
        let formatter = DateFormatter()
        formatter.locale = Locale(identifier: "en_US_POSIX")
        formatter.timeZone = TimeZone(secondsFromGMT: 0)
        formatter.dateFormat = "EEE, dd MMM yyyy HH:mm:ss zzz"
        return formatter.date(from: value).map { max(0, $0.timeIntervalSinceNow) }
    }
}
