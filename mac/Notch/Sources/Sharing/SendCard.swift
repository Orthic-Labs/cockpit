import SwiftUI

/// What nearby sharing's notch card shows, carried on a `DiskImagePrompt`:
/// the devices to pick from (a click sends), or the transfer in flight. It is
/// drawn by `SendCardBody` and hangs from the Send ring, not the notch's middle.
struct SendCardContent: Equatable {
    struct Row: Equatable, Identifiable {
        var id: String          // the device's fingerprint
        var alias: String
        var model: String
        var symbol: String      // SF Symbol for its device type
    }

    struct Transfer: Equatable {
        enum State: Equatable { case waiting, active, done, problem }
        var state: State
        var symbol: String
        /// 0...1 while bytes move; nil while waiting.
        var fraction: Double?
        var canCancel: Bool
    }

    var rows: [Row]
    var scanning: Bool
    var transfer: Transfer?

    static let cardWidth: CGFloat = NotchLayout.cardWidth
    static let rowHeight: CGFloat = Design.px(84)
    static let rowGap: CGFloat = Design.px(10)
    static let headerHeight: CGFloat = Design.px(76)
    static let maxVisibleRows = 4

    /// The transfer card is as tall as the disk image card; the list is its
    /// header plus the rows it shows (a spinner's worth when there are none).
    var cardHeight: CGFloat {
        if transfer != nil { return DiskImageCard.cardHeight }
        let shown = CGFloat(min(max(rows.count, 1), Self.maxVisibleRows))
        return 2 * NotchLayout.cardPadding + Self.headerHeight + Design.px(14)
            + shown * Self.rowHeight + (shown - 1) * Self.rowGap
    }
}

struct SendCardBody: View {
    let prompt: DiskImagePrompt
    let send: SendCardContent
    let width: CGFloat
    let height: CGFloat
    let surfaceFill: Color
    let secondaryInk: Color
    let reduceTransparency: Bool
    var onChoice: ((DiskImageChoice) -> Void)?

    private var detailInk: Color { prompt.style == .problem ? Palette.watch : secondaryInk }
    private static let button = Design.px(66)

    var body: some View {
        ZStack(alignment: .topLeading) {
            RoundedRectangle(cornerRadius: NotchLayout.cardCorner, style: .circular)
                .fill(surfaceFill)
                .frame(width: width, height: height)
            Group {
                if let transfer = send.transfer {
                    transferLayout(transfer)
                } else {
                    listLayout
                }
            }
            .padding(NotchLayout.cardPadding)
            .frame(width: width, height: height, alignment: .topLeading)
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

    // MARK: - The device list

    private var listLayout: some View {
        VStack(alignment: .leading, spacing: Design.px(14)) {
            HStack(alignment: .center, spacing: Design.px(14)) {
                VStack(alignment: .leading, spacing: Design.px(6)) {
                    Text(verbatim: prompt.title)
                        .font(Typography.cardTitle)
                        .foregroundStyle(Palette.textPrimary)
                        .lineLimit(1)
                    Text(verbatim: prompt.detail)
                        .font(Typography.cardBody)
                        .foregroundStyle(detailInk)
                        .lineLimit(1)
                        .truncationMode(.middle)
                }
                Spacer(minLength: 0)
                refreshButton
                closeButton
            }
            .frame(height: SendCardContent.headerHeight)

            if send.rows.isEmpty {
                HStack(spacing: Design.px(16)) {
                    CardProgress(linear: false)
                    Text(verbatim: L10n.t("Waiting for a device to appear…"))
                        .font(Typography.cardBody)
                        .foregroundStyle(secondaryInk)
                }
                .frame(maxWidth: .infinity, minHeight: SendCardContent.rowHeight, alignment: .center)
            } else {
                CardScroll {
                    VStack(spacing: SendCardContent.rowGap) {
                        ForEach(send.rows) { row in
                            SendDeviceRow(row: row, secondaryInk: secondaryInk) {
                                onChoice?(.sendTo(row.id))
                            }
                        }
                    }
                }
            }
        }
    }

    private var refreshButton: some View {
        Button { onChoice?(.refresh) } label: {
            ZStack {
                if send.scanning {
                    CardProgress(linear: false)
                } else {
                    Image(systemName: "arrow.clockwise")
                        .font(.system(size: 12, weight: .semibold))
                        .foregroundStyle(Palette.textPrimary)
                }
            }
            .frame(width: Self.button, height: Self.button)
            .background(Circle().fill(Palette.textPrimary.opacity(0.14)))
            .contentShape(Circle())
        }
        .buttonStyle(CardButtonStyle())
        .disabled(send.scanning)
        .help(L10n.t("Look again"))
    }

    private var closeButton: some View {
        Button { onChoice?(.dismiss) } label: {
            Image(systemName: "xmark")
                .font(.system(size: 11, weight: .bold))
                .foregroundStyle(Palette.textPrimary)
                .frame(width: Self.button, height: Self.button)
                .background(Circle().fill(Palette.textPrimary.opacity(0.14)))
                .contentShape(Circle())
        }
        .buttonStyle(CardButtonStyle())
        .help(L10n.t("Cancel"))
    }

    // MARK: - The transfer

    private func transferLayout(_ transfer: SendCardContent.Transfer) -> some View {
        HStack(alignment: .center, spacing: Design.px(30)) {
            Image(systemName: transfer.symbol)
                .font(.system(size: Design.px(64), weight: .regular))
                .foregroundStyle(transfer.state == .problem ? Palette.watch : Palette.textPrimary)
                .frame(width: Design.px(136), height: Design.px(136))
                .background(Circle().fill(Palette.textPrimary.opacity(0.10)))

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

                Spacer(minLength: Design.px(14))

                if transfer.state == .waiting || transfer.state == .active {
                    CardProgress(value: transfer.fraction.map { min(max($0, 0), 1) })
                        .padding(.bottom, Design.px(14))
                }
                HStack(spacing: Design.px(14)) {
                    if transfer.canCancel {
                        Button { onChoice?(.cancel) } label: {
                            HStack(spacing: Design.px(10)) {
                                Image(systemName: "stop.circle")
                                    .font(.system(size: 12, weight: .semibold))
                                Text(verbatim: L10n.t("Cancel"))
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
                    closeButton
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
    }
}

/// One nearby device: its icon, name and model; a click sends to it.
private struct SendDeviceRow: View {
    let row: SendCardContent.Row
    let secondaryInk: Color
    let action: () -> Void
    @State private var hovering = false

    var body: some View {
        Button(action: action) {
            HStack(spacing: Design.px(22)) {
                Image(systemName: row.symbol)
                    .font(.system(size: Design.px(34), weight: .regular))
                    .foregroundStyle(Palette.textPrimary)
                    .frame(width: Design.px(56), height: Design.px(56))
                VStack(alignment: .leading, spacing: Design.px(2)) {
                    Text(verbatim: row.alias)
                        .font(Typography.cardTitle)
                        .foregroundStyle(Palette.textPrimary)
                        .lineLimit(1)
                        .truncationMode(.tail)
                    if !row.model.isEmpty {
                        Text(verbatim: row.model)
                            .font(Typography.cardBody)
                            .foregroundStyle(secondaryInk)
                            .lineLimit(1)
                    }
                }
                Spacer(minLength: 0)
                Image(systemName: "paperplane")
                    .font(.system(size: 13, weight: .semibold))
                    .foregroundStyle(Palette.textPrimary.opacity(hovering ? 0.9 : 0.35))
            }
            .padding(.horizontal, Design.px(26))
            .frame(maxWidth: .infinity)
            .frame(height: SendCardContent.rowHeight)
            .background(RoundedRectangle(cornerRadius: Design.px(32), style: .circular)
                .fill(Palette.textPrimary.opacity(hovering ? 0.20 : 0.10)))
            .contentShape(RoundedRectangle(cornerRadius: Design.px(32), style: .circular))
        }
        .buttonStyle(CardButtonStyle())
        .onHover { hovering = $0 }
    }
}
