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

    var body: some View {
        configuration.label
            .scaleEffect(configuration.isPressed && !reduceMotion ? 0.96 : 1)
            .brightness(configuration.isPressed ? 0.18 : 0)
            .animation(.easeOut(duration: 0.12), value: configuration.isPressed)
    }
}
