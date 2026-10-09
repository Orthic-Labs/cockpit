import AppKit
import SwiftUI

/// What the notch's disk image card can be asked to do.
enum DiskImageChoice: Equatable, Sendable {
    case install, replace, quitAndUpdate, undo, openInstaller, showImage, cancel, dismiss
    /// Nearby sharing's device list: send to this device (its fingerprint), or look again.
    case sendTo(String), refresh
    /// A received message: copy it, or open it when it is a link.
    case copyText, openLink
}

/// **A disk image, as the notch shows it**: an app installed on its own (with
/// Undo), a question about one that could not be, or how an install went.
struct DiskImagePrompt: Equatable {
    enum Style: Equatable {
        /// A question: the buttons answer it.
        case ask
        /// Copying and checking.
        case working
        /// Done.
        case done
        /// Done with a caveat, or failed.
        case problem
    }

    struct Button: Equatable {
        var choice: DiskImageChoice
        var label: String
    }

    /// The path whose icon is drawn: the app, or the volume.
    var iconPath: String
    var title: String
    var detail: String
    /// A line of consequence, in the warning colour.
    var warning: String?
    var style: Style
    var primary: Button?
    var secondary: Button?
    /// Nearby sharing's device list or transfer, drawn in place of the icon layout
    /// and anchored to the Send ring — see `SendCardContent`.
    var send: SendCardContent?

    init(iconPath: String, title: String, detail: String, warning: String? = nil,
         style: Style, primary: Button? = nil, secondary: Button? = nil,
         send: SendCardContent? = nil) {
        self.send = send
        self.iconPath = iconPath
        self.title = title
        self.detail = detail
        self.warning = warning
        self.style = style
        self.primary = primary
        self.secondary = secondary
    }
}

/// **A disk image in the notch**, out of the notch like `UpdateCard` and
/// with the same build: its tail on the notch, the app's icon, a title, a line
/// or two, and one or two pills with a close. After an automatic install it
/// reads "Installed <App>" with Undo and goes by itself; a hover keeps it.
struct DiskImageCard: View {
    let prompt: DiskImagePrompt
    let direction: NotchEdge.TooltipDirection
    var tailOffset: CGFloat = 0
    var onChoice: ((DiskImageChoice) -> Void)?

    @Environment(\.codenotchReduceTransparency) private var reduceTransparency
    @Environment(\.notchSurfaceStyle) private var surfaceStyle
    @Environment(\.colorScheme) private var colorScheme

    static let cardWidth: CGFloat = NotchLayout.updateCardWidth
    static let cardHeight: CGFloat = Design.px(262)
    private static let icon: CGFloat = Design.px(136)
    private static let button: CGFloat = Design.px(66)

    private var glassy: Bool { surfaceStyle.isGlass && !reduceTransparency }
    private var secondaryInk: Color {
        TooltipGlassContrast.secondaryInk(surfaceStyle: surfaceStyle, colorScheme: colorScheme,
                                          reduceTransparency: reduceTransparency)
    }
    private var surfaceFill: Color { glassy ? .clear : Palette.card }

    /// The card's own size, the tail aside, on this edge.
    static func size(for direction: NotchEdge.TooltipDirection) -> CGSize {
        CGSize(width: cardWidth, height: cardHeight)
    }

    /// The same for a prompt: a nearby sharing card is narrower, and its list
    /// sets its height.
    static func size(for direction: NotchEdge.TooltipDirection, prompt: DiskImagePrompt) -> CGSize {
        guard let send = prompt.send else { return size(for: direction) }
        return CGSize(width: SendCardContent.cardWidth, height: send.cardHeight)
    }

    private var width: CGFloat { Self.size(for: direction, prompt: prompt).width }
    private var height: CGFloat { Self.size(for: direction, prompt: prompt).height }

    private var clampedTailOffset: CGFloat {
        let size = TooltipTail.size(for: direction)
        switch direction {
        case .leading, .trailing:
            let most = max(0, height / 2 - NotchLayout.cardCorner - size.height / 2)
            return min(max(tailOffset, -most), most)
        case .up, .down:
            let most = max(0, width / 2 - NotchLayout.cardCorner - size.width / 2)
            return min(max(tailOffset, -most), most)
        }
    }

    var body: some View {
        stack
            .background {
                if glassy {
                    if #available(macOS 26.0, *) {
                        Color.clear
                            .glassEffect(surfaceStyle.glass,
                                         in: TooltipSilhouette(direction: direction, tailOffset: clampedTailOffset))
                            .background {
                                if let dim = TooltipGlassContrast.dim(surfaceStyle: surfaceStyle,
                                                                      colorScheme: colorScheme,
                                                                      reduceTransparency: reduceTransparency) {
                                    TooltipSilhouette(direction: direction, tailOffset: clampedTailOffset).fill(dim)
                                }
                            }
                    }
                }
            }
    }

    // MARK: - Contents

    private var detailInk: Color { prompt.style == .problem ? Palette.watch : secondaryInk }

    @ViewBuilder private var card: some View {
        if let send = prompt.send {
            SendCardBody(prompt: prompt, send: send, width: width, height: height,
                         surfaceFill: surfaceFill, secondaryInk: secondaryInk,
                         reduceTransparency: reduceTransparency, onChoice: onChoice)
        } else {
            diskCard
        }
    }

    private var diskCard: some View {
        ZStack(alignment: .topLeading) {
            RoundedRectangle(cornerRadius: NotchLayout.cardCorner, style: .circular)
                .fill(surfaceFill)
                .frame(width: width, height: height)

            HStack(alignment: .center, spacing: Design.px(30)) {
                Image(nsImage: NSWorkspace.shared.icon(forFile: prompt.iconPath))
                    .resizable()
                    .interpolation(.high)
                    .frame(width: Self.icon, height: Self.icon)

                VStack(alignment: .leading, spacing: 0) {
                    Text(verbatim: prompt.title)
                        .font(Typography.cardTitle)
                        .foregroundStyle(Palette.textPrimary)
                        .lineLimit(1)
                        .truncationMode(.middle)

                    Text(verbatim: prompt.detail)
                        .font(Typography.cardBody)
                        .foregroundStyle(detailInk)
                        .lineLimit(2)
                        .fixedSize(horizontal: false, vertical: true)
                        .padding(.top, Design.px(6))

                    if let warning = prompt.warning {
                        Text(verbatim: warning)
                            .font(Typography.cardBody)
                            .foregroundStyle(Palette.watch)
                            .lineLimit(2)
                            .fixedSize(horizontal: false, vertical: true)
                            .padding(.top, Design.px(6))
                    }

                    Spacer(minLength: Design.px(14))

                    if prompt.style == .working {
                        ProgressView()
                            .progressViewStyle(.linear)
                            .tint(Palette.textPrimary)
                            .padding(.bottom, Design.px(prompt.primary == nil ? 20 : 14))
                            .transition(.opacity)
                        // A working card offers Cancel (and the close) only while cancelling is possible.
                        if prompt.primary != nil {
                            buttons
                                .transition(.opacity)
                        }
                    } else {
                        buttons
                            .transition(.opacity)
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
            }
            .padding(NotchLayout.cardPadding)
            .frame(width: width, height: height, alignment: .leading)
        }
        .frame(width: width, height: height, alignment: .top)
        .clipShape(RoundedRectangle(cornerRadius: NotchLayout.cardCorner, style: .circular))
        .overlay {
            if reduceTransparency {
                RoundedRectangle(cornerRadius: NotchLayout.cardCorner, style: .circular)
                    .strokeBorder(Palette.ringTrack, lineWidth: 1)
            }
        }
        .animation(.easeOut(duration: 0.2), value: prompt)
    }

    private var buttons: some View {
        HStack(spacing: Design.px(14)) {
            if let primary = prompt.primary {
                pill(primary.label, symbol: symbol(for: primary.choice)) { onChoice?(primary.choice) }
            }
            if let secondary = prompt.secondary {
                pill(secondary.label, symbol: symbol(for: secondary.choice)) { onChoice?(secondary.choice) }
            }
            Button { onChoice?(.dismiss) } label: {
                Image(systemName: "xmark")
                    .font(.system(size: 11, weight: .bold))
                    .foregroundStyle(Palette.textPrimary)
                    .frame(width: Self.button, height: Self.button)
                    .background(Circle().fill(Palette.textPrimary.opacity(0.14)))
                    .contentShape(Circle())
            }
            .buttonStyle(CardButtonStyle())
            .help(prompt.style == .working ? L10n.t("Cancel") : L10n.t("Close"))
        }
    }

    private func symbol(for choice: DiskImageChoice) -> String {
        switch choice {
        case .install:       return "arrow.down.app"
        case .replace, .quitAndUpdate: return "arrow.triangle.2.circlepath"
        case .undo:          return "arrow.uturn.backward"
        case .openInstaller: return "shippingbox"
        case .showImage:     return "folder"
        case .cancel:        return "stop.circle"
        case .dismiss:       return "clock"
        case .sendTo:        return "paperplane"
        case .refresh:       return "arrow.clockwise"
        case .copyText:      return "doc.on.doc"
        case .openLink:      return "safari"
        }
    }

    private func pill(_ title: String, symbol: String, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            HStack(spacing: Design.px(10)) {
                Image(systemName: symbol)
                    .font(.system(size: 12, weight: .semibold))
                Text(verbatim: title)
                    .font(Typography.cardBody.weight(.semibold))
                    .lineLimit(1)
            }
            .foregroundStyle(Palette.textPrimary)
            .padding(.horizontal, Design.px(28))
            .frame(height: Self.button)
            .background(Capsule().fill(Palette.textPrimary.opacity(0.14)))
            .contentShape(Capsule())
        }
        .buttonStyle(CardButtonStyle())
    }

    private var tail: some View {
        let size = TooltipTail.size(for: direction)
        return TooltipTail(direction: direction)
            .fill(surfaceFill)
            .frame(width: size.width, height: size.height)
            .offset(x: direction == .up || direction == .down ? clampedTailOffset : 0,
                    y: direction == .leading || direction == .trailing ? clampedTailOffset : 0)
    }

    @ViewBuilder private var stack: some View {
        switch direction {
        case .leading:  HStack(spacing: 0) { card; tail }
        case .trailing: HStack(spacing: 0) { tail; card }
        case .down:     VStack(spacing: 0) { tail; card }
        case .up:       VStack(spacing: 0) { card; tail }
        }
    }
}
