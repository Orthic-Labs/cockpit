import Foundation

/// Pulse fork: the sixth notch cell. A hover card of one-tap utilities (the
/// screenshot buttons, Paste to the other computer, Copy last, Lock screen)
/// that the middle-click wheel shares. Everything it shows comes from
/// `ToolKit`.
struct ToolsProvider: UsageProvider {
    let id = SystemProviders.toolsID
    let displayName = "Tools"
    let glyph = ProviderGlyph.tools
    let kind = ProviderKind.system
    var signInRoute: SignInRoute { .guidance("") }
    func account() -> ProviderAccount? { nil }

    func fetchSnapshot() async throws -> ProviderSnapshot {
        await MainActor.run { ToolKit.shared.providerSnapshot() }
    }
}
