# Privileged helper (uninstall without a password)

Optional. Off until the owner switches on General > Uninstalling > "Uninstall without password" in the hub and approves Pulse once in System Settings > Login Items. Without it, root-owned items go through one Finder request (one administrator password).

## What it is

`PulseHelper` is a launch daemon registered with `SMAppService.daemon` (plist `Contents/Library/LaunchDaemons/dev.orthic.pulse.helper.plist`, `BundleProgram` `Contents/Helpers/PulseHelper`, Mach service `dev.orthic.pulse.helper`). It runs as root only while a client is connected and exits 15 s after the last one leaves. `pulse-elevate` (`Contents/Helpers`) is its client; `pulse` (the Rust core) calls it from the Apps uninstall when the notch reports the helper `enabled`, and falls back to Finder when it is off, unapproved, errors or refuses an item.

## Who may connect

Only code signed by Apple-anchored team `6KLGD3LLKF` with identifier `dev.orthic.pulse`, `dev.orthic.pulse.hub` or `dev.orthic.pulse.elevate` (`NSXPCListener.setConnectionCodeSigningRequirement`). `pulse-elevate` in turn only talks to the helper signed by that team as `dev.orthic.pulse.helper`. The `uid` argument must equal the caller's own uid (and be an ordinary user, 500 or above).

## What it can do

One call: `moveToTrash(paths, forUser)`. Each path is renamed (`renamex_np`, `RENAME_EXCL`, so nothing is overwritten) into `/Users/<name>/.Trash` under a unique name. It never deletes, copies or runs anything, and changes ownership only of the item it just moved. Every decision is logged with os_log (subsystem `dev.orthic.pulse.helper`).

## What it refuses

- Anything not absolute, or with an empty, `.` or `..` component; anything that does not exist; any path whose real path differs from itself (symlinked parent or symlink).
- Anything outside `/Applications/<item>` and `/Library/{Application Support, Caches, Preferences, LaunchAgents, LaunchDaemons, PrivilegedHelperTools, Logs, Internet Plug-Ins, PreferencePanes, Audio}/<item>` (the folders themselves are refused too). `/Library/Receipts` and `/Library/Extensions` are not allowed.
- `/System`, `/usr`, `/bin`, `/sbin`, `/private`, `/Library/Apple`, `/Applications/Utilities`, `com.apple.*` and `Apple` entries under those /Library folders, any app (or app inside a vendor folder) whose bundle id starts `com.apple.` and has no App Store receipt, and anything containing the helper itself.

After a move it gives the moved item to the user: `lchown` to the caller's uid and primary gid over an `fts` walk with `FTS_PHYSICAL` (symlinks never followed) and `FTS_XDEV`, only inside the Trash path it just created. If that fails partway the item is still reported `moved`, with a note in `detail`. This is the only other write it makes, so emptying the Trash needs no password.

Each path gets its own `moved` or `refused` (with reason) result.

## Remove it

Switch the toggle off in the hub (calls `SMAppService.unregister`), or turn Pulse off in System Settings > Login Items. Deleting Pulse.app removes the daemon's program; if the registration lingers, `sudo launchctl bootout system/dev.orthic.pulse.helper`.

## Unverified

Registration, approval, the signing identifiers and whether a root daemon may rename other vendors' signed apps (macOS App Management protection) need the signed build on the Mac. A refusal there shows up as an error and the Finder fallback runs.

## Rename migration

First Pulse launch attempts to unregister `dev.orthic.cockpit.helper` best-effort, retrying on later launches if needed. Pulse never registers its replacement during migration. Hub shows “Off after rename”; switch Uninstall without password on again & approve Pulse in Login Items. Signing team remains `6KLGD3LLKF`. See [identifier migration](rename-pulse.md).
