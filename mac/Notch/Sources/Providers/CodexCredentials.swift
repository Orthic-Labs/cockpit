import Foundation

/// Borrows Codex's local session without refreshing or changing its credentials.
enum CodexCredentials {
    struct Credential {
        let accessToken: String
        let accountID: String
    }

    static var authURL: URL {
        CodexProfile.default().authURL
    }

    static func load(from url: URL = authURL, now: Date = Date()) throws -> Credential {
        struct Auth: Decodable {
            struct Tokens: Decodable {
                let access_token: String
                let account_id: String
            }
            let tokens: Tokens
        }
        guard let data = try? Data(contentsOf: url),
              let auth = try? JSONDecoder().decode(Auth.self, from: data),
              !auth.tokens.access_token.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty,
              !auth.tokens.account_id.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
        else { throw UsageProviderError.needsAuth }

        if let expiry = claims(inJWT: auth.tokens.access_token)?["exp"] as? Double,
           expiry <= now.timeIntervalSince1970 {
            throw UsageProviderError.credentialExpired
        }
        return Credential(accessToken: auth.tokens.access_token, accountID: auth.tokens.account_id)
    }

    static func account(from url: URL = authURL, source: String = "Codex") -> ProviderAccount? {
        guard let data = try? Data(contentsOf: url),
              let root = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let tokens = root["tokens"] as? [String: Any],
              let idToken = tokens["id_token"] as? String,
              let claims = claims(inJWT: idToken)
        else { return nil }

        let auth = claims["https://api.openai.com/auth"] as? [String: Any]
        return ProviderAccount(
            label: claims["email"] as? String,
            plan: auth?["chatgpt_plan_type"] as? String,
            source: source,
            manageURL: URL(string: "https://chatgpt.com/#settings/Account")
        )
    }

    /// When the paid subscription runs to, from the identity token's
    /// `chatgpt_subscription_active_until` claim. Only as fresh as Codex's
    /// last refresh; nil for free plans, when absent, or already past.
    static func subscriptionEnd(from url: URL = authURL, now: Date = Date()) -> Date? {
        guard let data = try? Data(contentsOf: url),
              let root = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let tokens = root["tokens"] as? [String: Any],
              let idToken = tokens["id_token"] as? String,
              let claims = claims(inJWT: idToken),
              let auth = claims["https://api.openai.com/auth"] as? [String: Any],
              let text = auth["chatgpt_subscription_active_until"] as? String
        else { return nil }
        let withFraction = ISO8601DateFormatter()
        withFraction.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        let plain = ISO8601DateFormatter()
        guard let date = withFraction.date(from: text) ?? plain.date(from: text), date > now
        else { return nil }
        return date
    }

    /// Claims supply identity labels and a local expiry hint. The server validates the token.
    static func claims(inJWT token: String) -> [String: Any]? {
        let parts = token.split(separator: ".")
        guard parts.count >= 2 else { return nil }

        var payload = String(parts[1])
            .replacingOccurrences(of: "-", with: "+")
            .replacingOccurrences(of: "_", with: "/")
        payload += String(repeating: "=", count: (4 - payload.count % 4) % 4)

        guard let data = Data(base64Encoded: payload) else { return nil }
        return try? JSONSerialization.jsonObject(with: data) as? [String: Any]
    }
}
