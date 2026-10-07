import Foundation

/// The one operation the privileged helper offers. Compiled into the helper
/// and into `cockpit-elevate`; the notch app does not link it.
let cockpitHelperMachService = "dev.orthic.cockpit.helper"
let cockpitTeamID = "6KLGD3LLKF"

@objc protocol CockpitHelperProtocol {
    /// Moves each path to the Trash of `uid` (never deletes). `uid` must be the
    /// caller's own. The reply has one `["path", "status", "detail"]` per input
    /// path, in order; status is `moved` or `refused`.
    func moveToTrash(paths: [String], forUser uid: UInt32,
                     reply: @escaping ([[String: String]]) -> Void)
}
