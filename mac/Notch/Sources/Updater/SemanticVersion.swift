import Foundation

/// A release version as dotted numbers, compared component by component.
///
/// `0.2` reads as `0.2.0`, a leading `v` is ignored, and anything after a `-`
/// or `+` (a pre-release or build tag) is dropped. Text that is not numbers
/// does not parse at all, so a malformed tag can never look like an update.
struct SemanticVersion: Equatable, Comparable {
    private let components: [Int]

    init?(_ text: String) {
        var core = Substring(text.trimmingCharacters(in: .whitespacesAndNewlines))
        if core.first == "v" || core.first == "V" { core = core.dropFirst() }
        if let cut = core.firstIndex(where: { $0 == "-" || $0 == "+" }) { core = core[..<cut] }
        let parts = core.split(separator: ".", omittingEmptySubsequences: false).map { Int($0) }
        let numbers = parts.compactMap { $0 }
        guard !numbers.isEmpty, numbers.count == parts.count, numbers.allSatisfy({ $0 >= 0 }) else {
            return nil
        }
        var padded = numbers
        while padded.count < 3 { padded.append(0) }
        components = padded
    }

    static func < (lhs: SemanticVersion, rhs: SemanticVersion) -> Bool {
        let count = max(lhs.components.count, rhs.components.count)
        for index in 0..<count {
            let left = index < lhs.components.count ? lhs.components[index] : 0
            let right = index < rhs.components.count ? rhs.components[index] : 0
            if left != right { return left < right }
        }
        return false
    }
}
