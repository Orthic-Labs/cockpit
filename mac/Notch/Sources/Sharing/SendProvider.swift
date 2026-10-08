import Foundation

/// Pulse fork: the fifth notch cell. A ring for the file transfer in flight
/// (idle when nothing is moving), a hover card listing nearby devices, ⌘V to
/// send the clipboard and a drop target for files. Everything it shows comes
/// from `NearbySharing`, which reads the hub's sharing service.
struct SendProvider: UsageProvider {
    let id = SystemProviders.sendID
    let displayName = "Send"
    let glyph = ProviderGlyph.send
    let kind = ProviderKind.system
    var signInRoute: SignInRoute { .guidance("") }
    func account() -> ProviderAccount? { nil }

    func fetchSnapshot() async throws -> ProviderSnapshot {
        await MainActor.run { NearbySharing.shared.providerSnapshot() }
    }
}
