import Foundation

/// A small arithmetic parser, so typed text is never handed to `NSExpression`
/// or any other evaluator that reads a format string.
///
/// Grammar: sum = product (("+"|"-") product)*; product = power (("*"|"/") power)*;
/// power = unary ("^" power)?; unary = "-" unary | postfix; postfix = atom "%"*;
/// atom = number | "(" sum ")" | name | name "(" sum ")".
enum LauncherCalculator {
    /// The formatted result, or nil when the text is not arithmetic. A bare
    /// number is not offered: it is a search, not a sum.
    static func evaluate(_ text: String) -> String? {
        var parser = Parser(text)
        guard let value = parser.parse(), value.isFinite, parser.usedOperator else { return nil }
        return format(value)
    }

    static func format(_ value: Double) -> String {
        if value == value.rounded(), abs(value) < 1e15 { return String(Int64(value)) }
        return String(format: "%.10g", value)
    }

    private struct Parser {
        private let chars: [Character]
        private var index = 0
        private(set) var usedOperator = false

        init(_ text: String) {
            let mapped = text.map { (c: Character) -> Character in
                switch c {
                case "×", "x", "X": return "*"
                case "÷": return "/"
                case "−", "–": return "-"
                default: return c
                }
            }
            chars = mapped.filter { !$0.isWhitespace }
        }

        mutating func parse() -> Double? {
            guard !chars.isEmpty, chars.count < 200 else { return nil }
            guard let value = sum(), index == chars.count else { return nil }
            return value
        }

        private var peek: Character? { index < chars.count ? chars[index] : nil }

        private mutating func sum() -> Double? {
            guard var left = product() else { return nil }
            while let c = peek, c == "+" || c == "-" {
                index += 1
                usedOperator = true
                guard let right = product() else { return nil }
                left = c == "+" ? left + right : left - right
            }
            return left
        }

        private mutating func product() -> Double? {
            guard var left = power() else { return nil }
            while let c = peek, c == "*" || c == "/" {
                index += 1
                usedOperator = true
                if c == "*", peek == "*" {   // "**" is a power
                    index += 1
                    guard let right = unary() else { return nil }
                    left = pow(left, right)
                    continue
                }
                guard let right = power() else { return nil }
                if c == "/" && right == 0 { return nil }
                left = c == "*" ? left * right : left / right
            }
            return left
        }

        private mutating func power() -> Double? {
            guard let base = unary() else { return nil }
            if peek == "^" {
                index += 1
                usedOperator = true
                guard let exponent = power() else { return nil }
                return pow(base, exponent)
            }
            return base
        }

        private mutating func unary() -> Double? {
            if peek == "-" {
                index += 1
                usedOperator = true
                guard let inner = unary() else { return nil }
                return -inner
            }
            if peek == "+" { index += 1; return unary() }
            return postfix()
        }

        private mutating func postfix() -> Double? {
            guard var value = atom() else { return nil }
            while peek == "%" {
                index += 1
                usedOperator = true
                value /= 100
            }
            return value
        }

        private mutating func atom() -> Double? {
            guard let c = peek else { return nil }
            if c == "(" {
                index += 1
                guard let inner = sum(), peek == ")" else { return nil }
                index += 1
                return inner
            }
            if (c.isASCII && c.isNumber) || c == "." { return number() }
            if c.isLetter {
                var name = ""
                while let l = peek, l.isLetter { name.append(l); index += 1 }
                switch name.lowercased() {
                case "pi": return Double.pi
                case "e": return M_E
                case "sqrt":
                    usedOperator = true
                    guard peek == "(" else { return nil }
                    index += 1
                    guard let inner = sum(), peek == ")", inner >= 0 else { return nil }
                    index += 1
                    return inner.squareRoot()
                default: return nil
                }
            }
            return nil
        }

        private mutating func number() -> Double? {
            var literal = ""
            var dots = 0
            while let c = peek, (c.isASCII && c.isNumber) || c == "." || c == "," {
                if c == "." { dots += 1 }
                if c != "," { literal.append(c) }
                index += 1
            }
            guard dots <= 1, literal != ".", let value = Double(literal) else { return nil }
            return value
        }
    }
}
