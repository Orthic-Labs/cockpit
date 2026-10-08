import Foundation

/// Unit, currency and crypto conversions typed into the launcher, such as
/// "12 km to mi", "100 F in C", "250 usd to eur" or "0.5 btc to usd". Units
/// work offline; money needs the cached rates.
@MainActor
enum LauncherConversion {
    struct Result {
        /// The row's title, such as "1.609 mi".
        let title: String
        /// What Return copies: the number alone.
        let copy: String
        let detail: String
    }

    private enum Scale {
        /// Linear to a base unit of its group (length, mass, volume, data, time, speed).
        case linear(String, Double)
        case celsius, fahrenheit, kelvin
    }

    private enum Money {
        case fiat(String)
        case coin(String)

        var code: String {
            switch self {
            case .fiat(let code), .coin(let code): return code
            }
        }
    }

    private static let units: [String: Scale] = {
        var table: [String: Scale] = [:]
        func add(_ names: [String], _ group: String, _ factor: Double) {
            for name in names { table[name] = .linear(group, factor) }
        }
        add(["m", "meter", "meters"], "length", 1)
        add(["km", "kilometer", "kilometers"], "length", 1_000)
        add(["cm"], "length", 0.01)
        add(["mm"], "length", 0.001)
        add(["mi", "mile", "miles"], "length", 1_609.344)
        add(["yd", "yard", "yards"], "length", 0.9144)
        add(["ft", "foot", "feet"], "length", 0.3048)
        add(["inch", "inches"], "length", 0.0254)
        add(["g", "gram", "grams"], "mass", 1)
        add(["mg"], "mass", 0.001)
        add(["kg", "kilo", "kilos", "kilogram", "kilograms"], "mass", 1_000)
        add(["lb", "lbs", "pound", "pounds"], "mass", 453.59237)
        add(["oz", "ounce", "ounces"], "mass", 28.349523125)
        add(["ml", "milliliter", "milliliters"], "volume", 1)
        add(["l", "liter", "liters", "litre", "litres"], "volume", 1_000)
        add(["gal", "gallon", "gallons"], "volume", 3_785.411784)
        add(["cup", "cups"], "volume", 236.5882365)
        add(["floz"], "volume", 29.5735295625)
        add(["tbsp"], "volume", 14.78676478125)
        add(["tsp"], "volume", 4.92892159375)
        add(["kb"], "data", 1_000)
        add(["mb"], "data", 1_000_000)
        add(["gb"], "data", 1_000_000_000)
        add(["tb"], "data", 1_000_000_000_000)
        add(["kib"], "data", 1_024)
        add(["mib"], "data", 1_048_576)
        add(["gib"], "data", 1_073_741_824)
        add(["s", "sec", "secs", "second", "seconds"], "time", 1)
        add(["min", "mins", "minute", "minutes"], "time", 60)
        add(["h", "hr", "hrs", "hour", "hours"], "time", 3_600)
        add(["d", "day", "days"], "time", 86_400)
        add(["wk", "week", "weeks"], "time", 604_800)
        add(["mps"], "speed", 1)
        add(["kph", "kmh"], "speed", 1_000.0 / 3_600.0)
        add(["mph"], "speed", 0.44704)
        add(["knot", "knots"], "speed", 0.514444)
        table["c"] = .celsius
        table["celsius"] = .celsius
        table["f"] = .fahrenheit
        table["fahrenheit"] = .fahrenheit
        table["k"] = .kelvin
        table["kelvin"] = .kelvin
        return table
    }()

    /// Nil unless the text is "<number> <unit> to|in|as <unit>" and both sides convert.
    static func evaluate(_ text: String, currencies: Bool, rates: LauncherRates) -> Result? {
        let lowered = text.lowercased().replacingOccurrences(of: "°", with: "")
        let tokens = lowered.split(whereSeparator: { $0.isWhitespace }).map(String.init)
        guard tokens.count >= 3, ["to", "in", "as"].contains(tokens[tokens.count - 2]) else { return nil }
        let target = tokens[tokens.count - 1]
        let source = tokens[0..<(tokens.count - 2)].joined(separator: " ")
        guard let parsed = amountAndUnit(source) else { return nil }
        let (amount, from) = parsed

        if let a = units[from], let b = units[target] {
            guard let value = convert(amount, a, b) else { return nil }
            let shown = format(value)
            return Result(title: "\(shown) \(target)", copy: shown,
                          detail: "\(format(amount)) \(from) to \(target)")
        }

        guard currencies,
              let fromMoney = money(from, rates), let toMoney = money(target, rates),
              let usd = usdValue(amount, fromMoney, rates),
              let value = fromUSD(usd, toMoney, rates)
        else { return nil }
        let shown = formatMoney(value, toMoney)
        let when = rates.date.map { $0.formatted(date: .abbreviated, time: .omitted) } ?? "unknown date"
        return Result(title: "\(shown) \(toMoney.code)", copy: shown,
                      detail: "\(from.uppercased()) to \(toMoney.code) · rates from \(when)")
    }

    /// "12km" or "12 km" gives (12, "km"). A thousands separator is allowed.
    private static func amountAndUnit(_ text: String) -> (Double, String)? {
        var number = ""
        var rest = Substring(text)
        while let c = rest.first, c.isNumber || c == "." || c == "," {
            number.append(c)
            rest = rest.dropFirst()
        }
        let unit = rest.trimmingCharacters(in: .whitespaces)
        guard !unit.isEmpty, let value = Double(number.replacingOccurrences(of: ",", with: "")) else { return nil }
        return (value, unit)
    }

    private static func convert(_ value: Double, _ from: Scale, _ to: Scale) -> Double? {
        if case .linear(let groupA, let factorA) = from, case .linear(let groupB, let factorB) = to {
            return groupA == groupB ? value * factorA / factorB : nil
        }
        guard let celsius = toCelsius(value, from) else { return nil }
        return fromCelsius(celsius, to)
    }

    private static func toCelsius(_ value: Double, _ scale: Scale) -> Double? {
        switch scale {
        case .celsius: return value
        case .fahrenheit: return (value - 32) * 5 / 9
        case .kelvin: return value - 273.15
        case .linear: return nil
        }
    }

    private static func fromCelsius(_ celsius: Double, _ scale: Scale) -> Double? {
        switch scale {
        case .celsius: return celsius
        case .fahrenheit: return celsius * 9 / 5 + 32
        case .kelvin: return celsius + 273.15
        case .linear: return nil
        }
    }

    private static func money(_ token: String, _ rates: LauncherRates) -> Money? {
        let code = token.uppercased()
        if LauncherRates.coinIDs[code] != nil, rates.usdPerCoin(code) != nil { return .coin(code) }
        if code == "USD" || rates.perUSD(code) != nil { return .fiat(code) }
        return nil
    }

    private static func usdValue(_ amount: Double, _ money: Money, _ rates: LauncherRates) -> Double? {
        switch money {
        case .fiat(let code):
            if code == "USD" { return amount }
            guard let perUSD = rates.perUSD(code), perUSD > 0 else { return nil }
            return amount / perUSD
        case .coin(let symbol):
            guard let price = rates.usdPerCoin(symbol) else { return nil }
            return amount * price
        }
    }

    private static func fromUSD(_ usd: Double, _ money: Money, _ rates: LauncherRates) -> Double? {
        switch money {
        case .fiat(let code):
            if code == "USD" { return usd }
            guard let perUSD = rates.perUSD(code) else { return nil }
            return usd * perUSD
        case .coin(let symbol):
            guard let price = rates.usdPerCoin(symbol), price > 0 else { return nil }
            return usd / price
        }
    }

    static func format(_ value: Double) -> String {
        if value == 0 { return "0" }
        if abs(value) >= 100 { return String(format: "%.2f", value) }
        return String(format: "%.4g", value)
    }

    private static func formatMoney(_ value: Double, _ money: Money) -> String {
        switch money {
        case .fiat:
            return String(format: "%.2f", value)
        case .coin:
            return value >= 0.01 ? String(format: "%.4f", value) : String(format: "%.8g", value)
        }
    }
}
