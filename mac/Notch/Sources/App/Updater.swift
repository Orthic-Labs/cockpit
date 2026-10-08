import Foundation

/// Pulse fork: Codenotch's Sparkle updater is removed, so this copy can never
/// fetch or install a Codenotch build over Pulse. Pulse's own update channel
/// arrives with its release pipeline (docs/plan.md). The interface is kept so
/// the notch's update card and Settings compile unchanged; with no updater,
/// nothing is ever offered.
@MainActor
final class Updater: NSObject, ObservableObject {
    enum Outcome: Equatable {
        case idle
        case checking
        case upToDate(Date)
        case found(String)
        case unreachable
        case failed(String)

        var message: String? {
            switch self {
            case .idle:          return nil
            case .checking:      return L10n.t("Checking…")
            case .upToDate:      return L10n.t("Pulse is up to date.")
            case .found(let v):  return L10n.t("Version \(v) is available.")
            case .unreachable:   return L10n.t("Couldn't reach the update server.")
            case .failed(let why): return why
            }
        }
    }

    @Published private(set) var outcome: Outcome = .idle
    @Published private(set) var prompt: UpdatePrompt?
    @Published private(set) var pending: String?

    var automatic = false

    var currentVersion: String {
        Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? "?"
    }

    var lastChecked: Date? { nil }

    func start() {}

    func checkNow() {
        outcome = .failed(L10n.t("Updates arrive with new Pulse releases."))
    }

    func reoffer() {}

    func preview() {}

    func respond(_ choice: UpdateChoice) { prompt = nil }

    func progressed(_ phase: UpdatePrompt.Phase) {
        guard prompt != nil else { return }
        prompt?.phase = phase
    }

    func promptEnded() { prompt = nil }

    static func nextVersion(after version: String) -> String {
        var parts = version.split(separator: ".").map { Int($0) ?? 0 }
        while parts.count < 3 { parts.append(0) }
        parts[1] += 1
        parts[2] = 0
        return parts.map(String.init).joined(separator: ".")
    }
}

/// **An update, as the notch offers it**: which version, a line of what is in
/// it, and how far along taking it up is.
struct UpdatePrompt: Equatable {
    enum Phase: Equatable {
        case available
        case downloading(Double?)
        case extracting(Double)
        case installing
    }

    var version: String
    var notes: String
    var phase: Phase

    static func summary(of html: String) -> String {
        var text = html.replacingOccurrences(of: "<[^>]+>", with: " ", options: .regularExpression)
        for (entity, character) in [("&amp;", "&"), ("&lt;", "<"), ("&gt;", ">"),
                                    ("&quot;", "\""), ("&#39;", "'"), ("&nbsp;", " ")] {
            text = text.replacingOccurrences(of: entity, with: character)
        }
        return text.replacingOccurrences(of: "\\s+", with: " ", options: .regularExpression)
            .trimmingCharacters(in: .whitespacesAndNewlines)
    }
}

/// What the notch said to the update it offered.
enum UpdateChoice: Equatable {
    case install, later, close
}
