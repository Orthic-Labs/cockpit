import Foundation

/// Cockpit fork: Claude gets two rings of its own — the five-hour session and
/// the week — side by side, rather than one ring with the week tucked inside
/// it or left to the hover card. Applied on the way to the notch only; the
/// store, archive and alerts keep the vendor's single reading.
///
/// The weekly cell points back at its account through `sourceProviderID`, so
/// clicking it refreshes Claude and switching Claude off removes both rings.
enum SplitWeeklyRing {
    static func apply(to snapshots: [ProviderSnapshot]) -> [ProviderSnapshot] {
        snapshots.flatMap { snapshot -> [ProviderSnapshot] in
            guard ClaudeProfile.isClaude(providerID: snapshot.id),
                  let weeklyID = snapshot.weeklyID, weeklyID != snapshot.headlineID,
                  let weekly = snapshot.windows.first(where: { $0.id == weeklyID })
            else { return [snapshot] }

            var session = snapshot
            session.weeklyID = nil
            if let headlineID = snapshot.headlineID {
                session.windows = snapshot.windows.filter { $0.id == headlineID }
            }

            var week = ProviderSnapshot(
                id: snapshot.id + ":weekly",
                displayName: L10n.t("\(snapshot.displayName) weekly"),
                glyph: snapshot.glyph,
                fidelity: snapshot.fidelity,
                status: snapshot.status,
                windows: [weekly],
                headlineID: weekly.id,
                kind: snapshot.kind
            )
            week.block = snapshot.block
            week.plan = snapshot.plan
            week.sourceProviderID = snapshot.id
            week.customIconFilename = snapshot.customIconFilename
            return [session, week]
        }
    }
}
