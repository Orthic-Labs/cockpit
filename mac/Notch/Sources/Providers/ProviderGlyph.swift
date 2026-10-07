import SwiftUI

/// Which mark a provider cell draws.
enum ProviderGlyph: String, Codable, Equatable {
    case claude
    case openai
    /// Cockpit fork: the machine's own readings, drawn with SF Symbols.
    case cpu
    case memory
    case disk

    /// An SF Symbol for marks that have no traced outline.
    var symbolName: String? {
        switch self {
        case .cpu: return "cpu"
        case .memory: return "memorychip"
        case .disk: return "internaldrive"
        default: return nil
        }
    }

    /// How much to scale this mark so it reads the same size as the others.
    /// Measured from a render: each value brings that glyph's ink to the same
    /// extent as Claude's.
    var opticalScale: CGFloat {
        switch self {
        case .claude: return 0.97
        case .openai: return 0.94
        case .cpu, .memory, .disk: return 0.9
        }
    }

    var outline: [[CGPoint]] {
        switch self {
        case .claude: return GlyphOutline.claude
        case .openai: return GlyphOutline.openai
        case .cpu, .memory, .disk: return []
        }
    }
}

/// A traced outline scaled into the view's bounds, filled even-odd so the
/// counters inside a knot stay open.
struct GlyphShape: Shape {
    let outline: [[CGPoint]]

    func path(in rect: CGRect) -> Path {
        var path = Path()
        for loop in outline {
            guard let first = loop.first else { continue }
            path.move(to: point(first, in: rect))
            for p in loop.dropFirst() { path.addLine(to: point(p, in: rect)) }
            path.closeSubpath()
        }
        return path
    }

    private func point(_ p: CGPoint, in rect: CGRect) -> CGPoint {
        CGPoint(x: rect.minX + p.x * rect.width, y: rect.minY + p.y * rect.height)
    }
}

struct ProviderGlyphView: View {
    let glyph: ProviderGlyph
    var size: CGFloat = Design.px(46)

    var body: some View {
        Group {
            if let symbol = glyph.symbolName {
                Image(systemName: symbol)
                    .resizable()
                    .scaledToFit()
            } else {
                GlyphShape(outline: glyph.outline)
                    .fill(style: FillStyle(eoFill: true))
            }
        }
        // Scaled inside a frame of the fixed size, so the *layout* stays on a
        // single grid — every row still reserves the same width — while the ink
        // is evened out within it.
        .scaleEffect(glyph.opticalScale)
        .frame(width: size, height: size)
    }
}
