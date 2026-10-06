import Foundation
import Security
import CryptoKit

// MIT adaptation notice: this reader adapts Codenotch's credential, header, and
// usage-window semantics from upstream/codenotch (see cockpit/docs/donors.md and
// upstream/codenotch/LICENSE). It is intentionally standalone & dependency-free.

struct AIUsageWindow: Sendable, Equatable {
    let id: String
    let label: String
    let fraction: Double
    let resetsAt: Date?
    let duration: TimeInterval?
}

enum AIUsageStatus: Sendable, Equatable {
    case live
    case stale(observedAt: Date, reason: String)
    case notConfigured
    case keychainLocked
    case permissionDenied
    case authenticationRequired
    case server(status: Int)
    case network
    case rateLimited(retryAt: Date?)
    case unavailable

    var label: String {
        switch self {
        case .live: return "Live"
        case .stale: return "Stale"
        case .notConfigured: return "Not configured"
        case .keychainLocked: return "Keychain locked"
        case .permissionDenied: return "Keychain permission denied"
        case .authenticationRequired: return "Authentication required"
        case .server(let status): return "Server error (HTTP \(status))"
        case .network: return "Network unavailable"
        case .rateLimited: return "Rate limited"
        case .unavailable: return "Unavailable"
        }
    }
}

enum AIUsageRecovery: Sendable, Equatable {
    case configureProvider
    case unlockKeychain
    case allowKeychainAccess
    case signIn
    case retryLater
    case checkNetwork
}

struct AIUsageReading: Sendable {
    let id: String
    /// Claude's five-hour window, or provider's primary window.
    let fraction: Double?
    /// Human-readable status plus real windows for hover/accessibility.
    let detail: String
    let observedAt: Date?
    let windows: [AIUsageWindow]
    let status: AIUsageStatus
    let recovery: AIUsageRecovery?

    init(id: String, fraction: Double?, detail: String, observedAt: Date?,
         windows: [AIUsageWindow] = [], status: AIUsageStatus = .unavailable,
         recovery: AIUsageRecovery? = nil) {
        self.id = id
        self.fraction = fraction
        self.detail = detail
        self.observedAt = observedAt
        self.windows = windows
        self.status = status
        self.recovery = recovery
    }
}

private final class UsageRedirectPolicy: NSObject, URLSessionTaskDelegate {
    func urlSession(_ session: URLSession, task: URLSessionTask,
                    willPerformHTTPRedirection response: HTTPURLResponse,
                    newRequest request: URLRequest,
                    completionHandler: @escaping (URLRequest?) -> Void) {
        completionHandler(nil)
    }
}

/// Read-only usage sampling for default Claude Code & Codex profiles.
actor AIUsageSampler {
    private static let maxResponseBytes = 1_048_576
    private static let requestTimeout: TimeInterval = 15
    private static let claudeService = "Claude Code-credentials"
    private static let claudeEndpoint = URL(string: "https://api.anthropic.com/api/oauth/usage?cedar_ember=1")!
    private static let codexEndpoint = URL(string: "https://chatgpt.com/backend-api/wham/usage")!

    private struct Sample: Sendable { let windows: [AIUsageWindow]; let detail: String }
    private enum SamplerError: Error, Sendable { case status(AIUsageStatus); case responseTooLarge; case cancelled }
    private struct ClaudeWindow: Decodable {
        let utilization: Double?
        let resetsAt: Date?
        enum CodingKeys: String, CodingKey { case utilization; case resetsAt = "resets_at" }
    }
    private struct ClaudeLimit: Decodable {
        let kind: String; let percent: Double?; let resetsAt: Date?
        enum CodingKeys: String, CodingKey { case kind, percent; case resetsAt = "resets_at" }
    }
    private struct ClaudePayload: Decodable {
        let limits: [ClaudeLimit]?; let fiveHour: ClaudeWindow?; let sevenDay: ClaudeWindow?; let weekly: ClaudeWindow?
        enum CodingKeys: String, CodingKey { case limits; case fiveHour = "five_hour"; case sevenDay = "seven_day"; case weekly }
    }
    private struct CodexWindow: Decodable {
        let usedPercent: Double?
        let limitWindowSeconds: Double?
        let resetAt: Double?
        let resetAfterSeconds: Double?
        enum CodingKeys: String, CodingKey {
            case usedPercent = "used_percent"
            case limitWindowSeconds = "limit_window_seconds"
            case resetAt = "reset_at"
            case resetAfterSeconds = "reset_after_seconds"
        }
        init(from decoder: Decoder) throws {
            let container = try decoder.container(keyedBy: CodingKeys.self)
            usedPercent = try? container.decode(Double.self, forKey: .usedPercent)
            limitWindowSeconds = try? container.decode(Double.self, forKey: .limitWindowSeconds)
            resetAt = try? container.decode(Double.self, forKey: .resetAt)
            resetAfterSeconds = try? container.decode(Double.self, forKey: .resetAfterSeconds)
        }
    }
    private struct CodexRateLimit: Decodable {
        let primaryWindow: CodexWindow?
        let secondaryWindow: CodexWindow?
        enum CodingKeys: String, CodingKey {
            case primaryWindow = "primary_window"
            case secondaryWindow = "secondary_window"
        }
        init(from decoder: Decoder) throws {
            let container = try decoder.container(keyedBy: CodingKeys.self)
            primaryWindow = try? container.decode(CodexWindow.self, forKey: .primaryWindow)
            secondaryWindow = try? container.decode(CodexWindow.self, forKey: .secondaryWindow)
        }
    }
    private struct CodexPayload: Decodable {
        let rateLimit: CodexRateLimit?
        enum CodingKeys: String, CodingKey { case rateLimit = "rate_limit" }
    }
    private struct CodexAuth: Decodable {
        struct Tokens: Decodable {
            let accessToken: String; let accountID: String
            enum CodingKeys: String, CodingKey { case accessToken = "access_token"; case accountID = "account_id" }
        }
        let tokens: Tokens
    }
    private struct CodexCredential { let accessToken: String; let accountID: String }
    private struct KeychainMatch { let modifiedAt: Date?; let persistentRef: Data }
    private enum CredentialFailure: Error { case notConfigured, keychainLocked, permissionDenied, authenticationRequired }
    private var previous: [String: AIUsageReading] = [:]
    private var retryNotBefore: [String: Date] = [:]
    private let session: URLSession

    init() {
        let config = URLSessionConfiguration.ephemeral
        config.timeoutIntervalForRequest = Self.requestTimeout
        config.timeoutIntervalForResource = Self.requestTimeout
        config.httpShouldSetCookies = false; config.httpCookieStorage = nil; config.urlCredentialStorage = nil; config.urlCache = nil
        self.session = URLSession(configuration: config, delegate: UsageRedirectPolicy(), delegateQueue: nil)
    }

    func refresh() async -> [AIUsageReading] {
        await refresh(interactiveClaude: false)
    }

    private func refresh(interactiveClaude: Bool) async -> [AIUsageReading] {
        guard !Task.isCancelled else { return [] }
        async let claudeResult = readClaude(interactive: interactiveClaude)
        async let codexResult = readCodex()
        return [await reading(id: "claude", result: claudeResult), await reading(id: "chatgpt", result: codexResult)]
    }

    /// User-invoked recovery only. Background refreshes always use fail-fast
    /// keychain access and therefore never put a password dialogue on screen.
    func requestClaudeKeychainAccess() async -> [AIUsageReading] {
        await refresh(interactiveClaude: true)
    }

    private func readClaude(interactive: Bool) async -> Result<Sample, SamplerError> {
        guard !Task.isCancelled else { return .failure(.cancelled) }
        if let until = retryNotBefore["claude"], until > Date() { return .failure(.status(.rateLimited(retryAt: until))) }
        do {
            let token = try readClaudeToken(interactive: interactive)
            var request = URLRequest(url: Self.claudeEndpoint, cachePolicy: .reloadIgnoringLocalCacheData, timeoutInterval: Self.requestTimeout)
            request.httpMethod = "GET"
            request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
            request.setValue("oauth-2025-04-20", forHTTPHeaderField: "anthropic-beta")
            request.setValue("application/json", forHTTPHeaderField: "Accept")
            request.setValue("no-cache, no-store", forHTTPHeaderField: "Cache-Control")
            let (data, response) = try await boundedData(for: request)
            guard let http = response as? HTTPURLResponse else { return .failure(.status(.unavailable)) }
            if http.statusCode == 429 {
                let retry = Date().addingTimeInterval(max(60, retryAfter(from: http) ?? 0)); retryNotBefore["claude"] = retry
                return .failure(.status(.rateLimited(retryAt: retry)))
            }
            guard (200..<300).contains(http.statusCode) else {
                return .failure(.status(http.statusCode == 401 || http.statusCode == 403 ? .authenticationRequired : .server(status: http.statusCode)))
            }
            let decoder = JSONDecoder(); decoder.dateDecodingStrategy = Self.dateStrategy
            let payload = try decoder.decode(ClaudePayload.self, from: data)
            let windows = claudeWindows(payload)
            guard !windows.isEmpty else { return .failure(.status(.unavailable)) }
            retryNotBefore["claude"] = nil
            return .success(Sample(windows: windows, detail: "Claude OAuth · \(describe(windows))"))
        } catch let failure as CredentialFailure { return .failure(.status(status(for: failure))) }
        catch SamplerError.responseTooLarge { return .failure(.responseTooLarge) }
        catch is CancellationError { return .failure(.cancelled) }
        catch is URLError { return .failure(.status(.network)) }
        catch { return .failure(.status(.unavailable)) }
    }

    private func readCodex() async -> Result<Sample, SamplerError> {
        guard !Task.isCancelled else { return .failure(.cancelled) }
        if let until = retryNotBefore["chatgpt"], until > Date() { return .failure(.status(.rateLimited(retryAt: until))) }
        guard let credential = readCodexCredential() else { return .failure(.status(.notConfigured)) }
        var request = URLRequest(url: Self.codexEndpoint, cachePolicy: .reloadIgnoringLocalCacheData, timeoutInterval: Self.requestTimeout)
        request.httpMethod = "GET"; request.setValue("Bearer \(credential.accessToken)", forHTTPHeaderField: "Authorization")
        request.setValue(credential.accountID, forHTTPHeaderField: "ChatGPT-Account-Id"); request.setValue("application/json", forHTTPHeaderField: "Accept")
        request.setValue("no-cache, no-store", forHTTPHeaderField: "Cache-Control")
        do {
            let (data, response) = try await boundedData(for: request)
            guard let http = response as? HTTPURLResponse else { return .failure(.status(.unavailable)) }
            if http.statusCode == 429 {
                let retry = Date().addingTimeInterval(max(60, retryAfter(from: http) ?? 0)); retryNotBefore["chatgpt"] = retry
                return .failure(.status(.rateLimited(retryAt: retry)))
            }
            guard (200..<300).contains(http.statusCode) else { return .failure(.status(http.statusCode == 401 || http.statusCode == 403 ? .authenticationRequired : .server(status: http.statusCode))) }
            let payload = try JSONDecoder().decode(CodexPayload.self, from: data)
            let now = Date()
            let windows = [
                ("primary", payload.rateLimit?.primaryWindow),
                ("secondary", payload.rateLimit?.secondaryWindow)
            ].compactMap { id, window -> AIUsageWindow? in
                guard let window, let fraction = validFraction(window.usedPercent) else { return nil }
                let duration = window.limitWindowSeconds
                let reset = window.resetAt.map { Date(timeIntervalSince1970: $0) }
                    ?? window.resetAfterSeconds.map { now.addingTimeInterval($0) }
                let label: String
                if duration == 5 * 3600 { label = "5-hour" }
                else if duration == 7 * 86400 { label = "Weekly" }
                else { label = id == "primary" ? "Primary window" : "Secondary window" }
                return AIUsageWindow(id: id, label: label, fraction: fraction, resetsAt: reset, duration: duration)
            }
            guard !windows.isEmpty else { return .failure(.status(.unavailable)) }
            guard (windows.first(where: { $0.id == "primary" })?.fraction ?? windows.first?.fraction) != nil else {
                return .failure(.status(.unavailable))
            }
            retryNotBefore["chatgpt"] = nil
            return .success(Sample(windows: windows, detail: "ChatGPT/Codex · \(describe(windows))"))
        } catch SamplerError.responseTooLarge { return .failure(.responseTooLarge) }
        catch is CancellationError { return .failure(.cancelled) }
        catch is URLError { return .failure(.status(.network)) }
        catch { return .failure(.status(.unavailable)) }
    }

    private func reading(id: String, result: Result<Sample, SamplerError>) -> AIUsageReading {
        switch result {
        case .success(let sample):
            let fraction = sample.windows.first(where: { $0.id == "session" })?.fraction ?? sample.windows.first?.fraction
            let reading = AIUsageReading(id: id, fraction: fraction, detail: "Live · \(sample.detail)", observedAt: Date(), windows: sample.windows, status: .live)
            previous[id] = reading
            return reading
        case .failure(let error):
            let status = failureStatus(error)
            guard let prior = previous[id], let fraction = prior.fraction, let observedAt = prior.observedAt else {
                return AIUsageReading(id: id, fraction: nil, detail: "\(status.label) · no live usage reading", observedAt: nil, status: status, recovery: recovery(for: status))
            }
            let stale = AIUsageStatus.stale(observedAt: observedAt, reason: status.label)
            return AIUsageReading(id: id, fraction: fraction, detail: "Stale · \(status.label) · observed \(Self.timestamp(observedAt))", observedAt: observedAt, windows: prior.windows, status: stale, recovery: recovery(for: status))
        }
    }

    private func failureStatus(_ error: SamplerError) -> AIUsageStatus {
        switch error { case .status(let status): return status; case .responseTooLarge, .cancelled: return .unavailable }
    }

    private func recovery(for status: AIUsageStatus) -> AIUsageRecovery? {
        switch status {
        case .notConfigured: return .configureProvider
        case .keychainLocked: return .unlockKeychain
        case .permissionDenied: return .allowKeychainAccess
        case .authenticationRequired: return .signIn
        case .server, .rateLimited, .unavailable: return .retryLater
        case .network: return .checkNetwork
        case .live, .stale: return nil
        }
    }

    private func readClaudeToken(interactive: Bool) throws -> String {
        guard let match = try newestKeychainMatch() else { throw CredentialFailure.notConfigured }
        let query: [CFString: Any] = [kSecClass: kSecClassGenericPassword, kSecValuePersistentRef: match.persistentRef, kSecReturnData: true, kSecMatchLimit: kSecMatchLimitOne, kSecUseAuthenticationUI: interactive ? kSecUseAuthenticationUIAllow : kSecUseAuthenticationUIFail]
        var result: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &result)
        guard status == errSecSuccess, let data = result as? Data else {
            if status == errSecInteractionNotAllowed { throw CredentialFailure.keychainLocked }
            if status == errSecAuthFailed || status == errSecUserCanceled { throw CredentialFailure.permissionDenied }
            if status == errSecItemNotFound { throw CredentialFailure.notConfigured }
            throw CredentialFailure.authenticationRequired
        }
        struct Auth: Decodable {
            struct OAuth: Decodable { let accessToken: String; let expiresAt: Double }
            let oauth: OAuth
            enum CodingKeys: String, CodingKey { case oauth = "claudeAiOauth" }
        }
        guard let auth = try? JSONDecoder().decode(Auth.self, from: data), !auth.oauth.accessToken.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty, auth.oauth.expiresAt > Date().timeIntervalSince1970 * 1000 else { throw CredentialFailure.authenticationRequired }
        return auth.oauth.accessToken
    }

    private func newestKeychainMatch() throws -> KeychainMatch? {
        let services = [Self.claudeService, "\(Self.claudeService)-\(serviceSuffix())"]
        var matches: [KeychainMatch] = []
        for service in services {
            let query: [CFString: Any] = [kSecClass: kSecClassGenericPassword, kSecAttrService: service, kSecReturnAttributes: true, kSecReturnPersistentRef: true, kSecMatchLimit: kSecMatchLimitAll, kSecUseAuthenticationUI: kSecUseAuthenticationUIFail]
            var result: CFTypeRef?
            let status = SecItemCopyMatching(query as CFDictionary, &result)
            if status == errSecItemNotFound { continue }
            if status == errSecInteractionNotAllowed { throw CredentialFailure.keychainLocked }
            if status == errSecAuthFailed || status == errSecUserCanceled { throw CredentialFailure.permissionDenied }
            guard status == errSecSuccess else { continue }
            let items = (result as? [[CFString: Any]]) ?? (result as? [CFString: Any]).map { [$0] } ?? []
            matches.append(contentsOf: items.compactMap { item in
                guard let ref = item[kSecValuePersistentRef] as? Data else { return nil }
                return KeychainMatch(modifiedAt: item[kSecAttrModificationDate] as? Date, persistentRef: ref)
            })
        }
        return matches.max { ($0.modifiedAt ?? .distantPast) < ($1.modifiedAt ?? .distantPast) }
    }

    private func serviceSuffix() -> String {
        let path = URL(fileURLWithPath: NSHomeDirectory()).appendingPathComponent(".claude").path
        return SHA256.hash(data: Data(path.utf8)).prefix(4).map { String(format: "%02x", $0) }.joined()
    }
    private func status(for failure: CredentialFailure) -> AIUsageStatus {
        switch failure { case .notConfigured: return .notConfigured; case .keychainLocked: return .keychainLocked; case .permissionDenied: return .permissionDenied; case .authenticationRequired: return .authenticationRequired }
    }
    private func readCodexCredential() -> CodexCredential? {
        let url = URL(fileURLWithPath: NSHomeDirectory()).appendingPathComponent(".codex/auth.json")
        guard let handle = try? FileHandle(forReadingFrom: url) else { return nil }; defer { try? handle.close() }
        guard let data = try? handle.read(upToCount: Self.maxResponseBytes + 1), data.count <= Self.maxResponseBytes, let auth = try? JSONDecoder().decode(CodexAuth.self, from: data), !auth.tokens.accessToken.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty, !auth.tokens.accountID.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else { return nil }
        return CodexCredential(accessToken: auth.tokens.accessToken, accountID: auth.tokens.accountID)
    }

    private func claudeWindows(_ payload: ClaudePayload) -> [AIUsageWindow] {
        var windows: [AIUsageWindow] = []
        func append(_ id: String, _ label: String, _ value: Double?, _ reset: Date?, _ duration: TimeInterval?) {
            guard let fraction = validFraction(value), !windows.contains(where: { $0.id == id }) else { return }
            windows.append(AIUsageWindow(id: id, label: label, fraction: fraction, resetsAt: reset, duration: duration))
        }
        for limit in payload.limits ?? [] {
            let id = limit.kind == "five_hour" ? "session" : limit.kind == "seven_day" ? "weekly_all" : limit.kind
            append(id, id == "session" ? "5-hour" : id == "weekly_all" ? "Weekly" : limit.kind, limit.percent, limit.resetsAt, duration(for: id))
        }
        append("session", "5-hour", payload.fiveHour?.utilization, payload.fiveHour?.resetsAt, 5 * 3600)
        let weekly = payload.sevenDay ?? payload.weekly
        append("weekly_all", "Weekly", weekly?.utilization, weekly?.resetsAt, 7 * 86400)
        return windows.sorted { rank($0.id) < rank($1.id) }
    }
    private func duration(for id: String) -> TimeInterval? { id == "session" ? 5 * 3600 : (id.hasPrefix("weekly_") || id == "weekly_all" ? 7 * 86400 : nil) }
    private func rank(_ id: String) -> Int { id == "session" ? 0 : id == "weekly_all" ? 1 : 2 }
    private func validFraction(_ percent: Double?) -> Double? { guard let percent, percent.isFinite, (0...100).contains(percent) else { return nil }; return percent / 100 }
    private func percent(_ fraction: Double) -> String { String(format: "%.0f%%", fraction * 100) }
    private func describe(_ windows: [AIUsageWindow]) -> String { windows.map { "\($0.label) \(percent($0.fraction))" + ($0.resetsAt.map { " · resets \(Self.timestamp($0))" } ?? "") }.joined(separator: " · ") }

    private static let dateStrategy: JSONDecoder.DateDecodingStrategy = .custom { decoder in
        let string = try decoder.singleValueContainer().decode(String.self)
        let withFraction = ISO8601DateFormatter(); withFraction.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        let plain = ISO8601DateFormatter(); plain.formatOptions = [.withInternetDateTime]
        guard let date = withFraction.date(from: string) ?? plain.date(from: string) else { throw DecodingError.dataCorrupted(.init(codingPath: decoder.codingPath, debugDescription: "Invalid date")) }
        return date
    }
    private func boundedData(for request: URLRequest) async throws -> (Data, URLResponse) {
        guard !Task.isCancelled else { throw CancellationError() }
        let (bytes, response) = try await session.bytes(for: request); defer { bytes.task.cancel() }
        if response.expectedContentLength > Int64(Self.maxResponseBytes) { throw SamplerError.responseTooLarge }
        var data = Data(); data.reserveCapacity(response.expectedContentLength > 0 ? Int(response.expectedContentLength) : 4096)
        for try await byte in bytes { guard !Task.isCancelled else { throw CancellationError() }; guard data.count < Self.maxResponseBytes else { throw SamplerError.responseTooLarge }; data.append(byte) }
        return (data, response)
    }
    private func retryAfter(from response: HTTPURLResponse) -> TimeInterval? {
        guard let value = response.value(forHTTPHeaderField: "Retry-After")?.trimmingCharacters(in: .whitespacesAndNewlines), !value.isEmpty else { return nil }
        if let seconds = TimeInterval(value) { return max(0, seconds) }
        let formatter = DateFormatter(); formatter.locale = Locale(identifier: "en_US_POSIX"); formatter.timeZone = TimeZone(secondsFromGMT: 0); formatter.dateFormat = "EEE, dd MMM yyyy HH:mm:ss zzz"
        return formatter.date(from: value).map { max(0, $0.timeIntervalSinceNow) }
    }
    private static func timestamp(_ date: Date) -> String { let formatter = ISO8601DateFormatter(); formatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]; return formatter.string(from: date) }
}
