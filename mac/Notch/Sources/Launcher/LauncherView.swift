import AppKit
import SwiftUI

/// The launcher's content: a search field over grouped results, in the notch's
/// dark glass. A command's output replaces the results until Escape.
struct LauncherView: View {
    @ObservedObject var model: LauncherModel
    @FocusState private var focused: Bool

    static let size = CGSize(width: 620, height: 380)

    var body: some View {
        VStack(spacing: 0) {
            HStack(spacing: 10) {
                Image(systemName: "magnifyingglass")
                    .font(.system(size: 17, weight: .regular))
                    .foregroundStyle(.white.opacity(0.5))
                TextField("Search apps, files, clipboard, or run a command", text: $model.query)
                    .textFieldStyle(.plain)
                    .font(.system(size: 20, weight: .regular))
                    .foregroundStyle(.white)
                    .focused($focused)
                    .autocorrectionDisabled()
            }
            .padding(.horizontal, 18)
            .frame(height: 56)

            Divider().overlay(Color.white.opacity(0.1))

            if let output = model.output {
                outputView(output)
            } else if model.items.isEmpty {
                Spacer()
                Text(model.query.trimmingCharacters(in: .whitespaces).isEmpty
                     ? "Type to search" : "No matches")
                    .font(.system(size: 13))
                    .foregroundStyle(.white.opacity(0.4))
                Spacer()
            } else {
                ScrollViewReader { proxy in
                    ScrollView {
                        VStack(alignment: .leading, spacing: 0) {
                            ForEach(Array(model.items.enumerated()), id: \.element.id) { position, item in
                                if position == 0 || model.items[position - 1].section != item.section {
                                    Text(item.section.title.uppercased())
                                        .font(.system(size: 10, weight: .semibold))
                                        .tracking(0.6)
                                        .foregroundStyle(.white.opacity(0.4))
                                        .padding(.horizontal, 18)
                                        .padding(.top, position == 0 ? 8 : 12)
                                        .padding(.bottom, 4)
                                }
                                row(item, position: position)
                                    .id(item.id)
                            }
                        }
                        .padding(.bottom, 8)
                    }
                    .onChange(of: model.selected) { _, new in
                        guard model.items.indices.contains(new) else { return }
                        proxy.scrollTo(model.items[new].id)
                    }
                }
            }
        }
        .frame(width: Self.size.width, height: Self.size.height)
        .background(
            ZStack {
                VisualEffect()
                Color.black.opacity(0.55)
            }
        )
        .clipShape(RoundedRectangle(cornerRadius: 22, style: .continuous))
        .overlay(
            RoundedRectangle(cornerRadius: 22, style: .continuous)
                .strokeBorder(Color.white.opacity(0.12), lineWidth: 1)
        )
        .environment(\.colorScheme, .dark)
        .onAppear { focused = true }
        .onChange(of: model.focusToken) { _, _ in focused = true }
    }

    private func outputView(_ output: LauncherOutput) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(output.title)
                .font(.system(size: 13, weight: .semibold))
                .foregroundStyle(.white)
            ScrollView {
                Text(output.text)
                    .font(.system(size: 12, design: .monospaced))
                    .foregroundStyle(.white.opacity(0.85))
                    .textSelection(.enabled)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
            Text("Esc returns to search")
                .font(.system(size: 10))
                .foregroundStyle(.white.opacity(0.35))
        }
        .padding(16)
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
    }

    private func row(_ item: LauncherItem, position: Int) -> some View {
        let isSelected = position == model.selected
        return HStack(spacing: 12) {
            icon(item)
                .frame(width: 28, height: 28)
            VStack(alignment: .leading, spacing: 1) {
                Text(item.title)
                    .font(.system(size: 14, weight: .medium))
                    .foregroundStyle(.white)
                    .lineLimit(1)
                if let subtitle = item.subtitle {
                    Text(subtitle)
                        .font(.system(size: 11))
                        .foregroundStyle(.white.opacity(0.5))
                        .lineLimit(1)
                        .truncationMode(.middle)
                }
            }
            Spacer(minLength: 8)
            if position < 9 {
                Text("⌘\(position + 1)")
                    .font(.system(size: 11, weight: .medium))
                    .foregroundStyle(.white.opacity(0.35))
            }
        }
        .padding(.horizontal, 10)
        .frame(height: 42)
        .background(
            RoundedRectangle(cornerRadius: 10, style: .continuous)
                .fill(isSelected ? Color.white.opacity(0.14) : Color.clear)
        )
        .padding(.horizontal, 8)
        .contentShape(Rectangle())
        .onTapGesture { model.activate(position) }
    }

    @ViewBuilder
    private func icon(_ item: LauncherItem) -> some View {
        if let image = item.image {
            Image(nsImage: image)
                .resizable()
                .aspectRatio(contentMode: .fill)
                .clipShape(RoundedRectangle(cornerRadius: 5, style: .continuous))
        } else if let path = item.iconPath {
            Image(nsImage: NSWorkspace.shared.icon(forFile: path))
                .resizable()
                .interpolation(.high)
        } else {
            Image(systemName: item.symbol)
                .font(.system(size: 16))
                .foregroundStyle(.white.opacity(0.8))
        }
    }
}

private struct VisualEffect: NSViewRepresentable {
    func makeNSView(context: Context) -> NSVisualEffectView {
        let view = NSVisualEffectView()
        view.material = .hudWindow
        view.blendingMode = .behindWindow
        view.state = .active
        view.appearance = NSAppearance(named: .darkAqua)
        return view
    }

    func updateNSView(_ view: NSVisualEffectView, context: Context) {}
}
