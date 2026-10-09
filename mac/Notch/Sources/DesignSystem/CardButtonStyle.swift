import AppKit
import SwiftUI

/// The press for the notch cards' buttons (the Send card's rows, pills and round
/// buttons, the disk image card's pills, the Send ring's Paste row): the label
/// brightens (visible on the black notch) and eases down to 0.96. Reduce Motion keeps the highlight and skips the scale.
struct CardButtonStyle: ButtonStyle {
    func makeBody(configuration: Configuration) -> some View {
        CardButtonBody(configuration: configuration)
    }
}

private struct CardButtonBody: View {
    let configuration: ButtonStyleConfiguration
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @Environment(\.isEnabled) private var isEnabled
    @State private var hovering = false

    var body: some View {
        configuration.label
            .scaleEffect(configuration.isPressed && !reduceMotion ? 0.96 : 1)
            // Hover lifts the pill's dark fill to a clearly lighter grey; press goes further.
            // Smaller values were invisible on the black notch.
            .brightness(configuration.isPressed ? 0.32 : hovering && isEnabled ? 0.22 : 0)
            .animation(reduceMotion ? nil : .easeOut(duration: 0.12), value: configuration.isPressed)
            .animation(reduceMotion ? nil : .easeOut(duration: 0.12), value: hovering)
            .onHover { inside in
                hovering = inside
                if inside && isEnabled { NSCursor.pointingHand.set() }
            }
    }
}

/// The hover lift for a clickable row that is not a Button (the Send ring's
/// device rows): a 16% white plate behind the content, a pointing hand on enter.
/// The notch controller owns the cursor stack, so this only sets, never pushes.
struct CardRowHover: ViewModifier {
    var enabled = true
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var hovering = false

    func body(content: Content) -> some View {
        content
            .background(RoundedRectangle(cornerRadius: 10, style: .continuous)
                .fill(Color.white.opacity(hovering && enabled ? 0.16 : 0)))
            .animation(reduceMotion ? nil : .easeOut(duration: 0.12), value: hovering)
            .onHover { inside in
                hovering = inside
                if inside && enabled { NSCursor.pointingHand.set() }
            }
    }
}

extension View {
    func cardRowHover(enabled: Bool = true) -> some View {
        modifier(CardRowHover(enabled: enabled))
    }
}
