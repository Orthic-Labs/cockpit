# Pulse plan

Date: 2026-10-07. Replaces [implementation-plan.md](implementation-plan.md) (kept for history only).

Product renamed to Pulse on 2026-10-08; [upgrade notes](mac-app-delivery.md#upgrading). Dated status lines retain prior names.

## Goal

One owned tool that replaces Vorssaint, CodexBar & manual disk cleanup. Mac first; Windows after Mac is done.

## Surfaces

| Surface | What it is | Built with |
| --- | --- | --- |
| Notch | Always on. Right screen edge. Four cells, each a main outer ring with a thin inner ring: Claude (weekly / 5-hour), Codex (weekly / 5-hour), System (memory pressure / CPU), Disks (external / internal). Hover card shows details: System adds GPU busy and CPU/SoC temperature; Disks adds drive health, temperature, wear and writes per physical drive. | Fork of Codenotch Mac app (Swift), `mac/Notch` |
| Hub | Small window (about 600×400) opened by clicking the notch. Storage, Cleanup, Apps, Monitor, Settings. Closes fully when closed. | Tauri (shared with Windows later), RightKit packages |
| CLI | `pulse` for agents: same data & actions as the hub, JSON output. | Existing Rust core |

**App presence:** no Dock icon & no menu-bar item, ever. The notch is the only surface. Right-click on the notch shows one item: **Quit**, which exits notch & hub. Launch at login brings it back. The hub runs as an accessory window (no Dock icon while open).

## Architecture

- **Notch:** Codenotch Mac fork, cut to Claude & Codex, plus resource & disk rings. Keeps Codenotch's design, hover card, Option-drag along the edge, settings orb (opens hub Settings) & updater. Swift samples its own cheap counters; nothing heavy runs in the notch.
- **Hub:** Tauri app bundled inside Pulse.app. Its Rust backend links `pulse-core` directly; no bridge layer, no separate worker for the hub.
- **Core:** existing `core/` crate: scanner, APFS-aware accounting, filename index, duplicates, growth history, monitor readings, process groups, rule engine, cleanup state machine, CLI & local socket.
- **Storage:** versioned JSON files (current). Move to SQLite only if a feature needs queries JSON can't serve.
- **Not used:** UniFFI, always-running worker, menu-bar/Dock presence.

## Donors (reimplement; copy patterns, not whole apps)

| Feature | Primary donor | Secondary |
| --- | --- | --- |
| Notch, AI usage readers, updater | [Codenotch](https://github.com/vinzdg/codenotch) (fork) | — |
| CPU, GPU, memory, disk, network, battery, sensors | [Stats](https://github.com/exelban/stats) | [mectrics](https://github.com/farukkamcici/mectrics), [slawek19926/Vitals](https://github.com/slawek19926/Vitals) |
| Processes / task manager | [hmarr/vitals](https://github.com/hmarr/vitals) | Stats; [TaskExplorer](https://github.com/DavidXanatos/TaskExplorer) for Windows |
| Drive health (SMART) | [smartmontools](https://github.com/smartmontools/smartmontools) `smartctl -j` (GPL-2.0, run as a separate program) | [Scrutiny](https://github.com/AnalogJ/scrutiny) for history ideas; CrystalDiskInfo to cross-check on Windows |
| Disk map, scan, growth | Petal (in `vendor/petal`) & existing core | — |
| Cleanup rules | [Mole](https://github.com/tw93/Mole), [Kudu](https://github.com/AdventDevInc/kudu) `rules/` | [PureMac](https://github.com/momenbasel/PureMac), [MacSai](https://github.com/iliyami/MacSai), [purge-app](https://github.com/jithin-sabu/purge-app) |
| Hub UI reference | Kudu (React; same idea, Electron instead of Tauri) | — |
| Uninstall & leftovers | Pearcleaner | Mole |
| Finder cut/paste, maximizer, Dock click, Auto Quit | Vorssaint behaviour (feature reference only, no code; Vorssaint removed 2026-10-09) | — |
| Launcher | Tinycast feature list (feature reference only, no code; checkout removed) | — |
| Windows cleanup (later) | [FluentCleaner](https://github.com/builtbybel/FluentCleaner), Kudu | CleanmgrPlus |

## Phases

Each phase ends with something usable on the Mac, installed as a signed, notarized build.

1. **Notch fork.** Import Codenotch, strip other providers, add system & disk cells, no Dock/menu-bar item, right-click Quit only, launch at login. Repoint release packaging to the new app.
   *Done when:* it looks & behaves like Codenotch with Pulse's rings, and runs a full day without issues.
   *Status 2026-10-07:* done except the full-day run — signed, notarized Pulse.app installed from the RightKit release lane; unused Codenotch code removed (#5); launch at login on by default; notch clicks open the hub.
2. **Hub: Storage & Monitor.** Tauri hub from the approved mockup; storage list with drilldown & growth, search, monitor readings, Settings. Clicking the notch opens it.
   *Done when:* finding what's using space takes seconds, not a learning curve.
   *Status 2026-10-07:* built (#2) on RightKit app-shell — Storage, Monitor, Settings (Accounts, Appearance, Notifications, General) via the notch bridge; Codenotch Settings window removed; installed. Growth is a compact line in the hub Storage view (done 2026-10-07); no separate growth view yet.
   *Status 2026-10-07 (Storage redesign):* Storage now follows Petal's model. Every mounted user-visible volume is a card (name, used/free bar, capacity); clicking one scans it (startup disk from home). "You can free X" lists findings (caches, Xcode, package caches, build outputs `node_modules`/`target`/`.build`, installers, Trash and simulators as notes) with a plain reason and Move to Trash through the existing cleanup path (re-validated, history, Undo). The folder list has percent bars, Petal's kind colours and a squarified SVG treemap. Scan budget raised (2M entries, 500k per directory); iCloud placeholders are never reported, and a real partial scan shows one line with an Open Full Disk Access button. Growth is a compact line. Unverified on the Mac until CI build is run.
   *Status 2026-10-09 (hub v2.1):* Overview, Storage, Cleanup, Monitor, Apps and Settings redesigned; Storage has a Changes pane (folders grown and shrunk since the previous comparable scan), which is the separate growth view. Monitor shows 30 minutes of CPU, memory, swap and network history with real memory pressure. Overview is a six-card grid that fits the default 900x600 window without scrolling. File-name search uses rightkit-fsindex 0.1.0 (per-volume background index, content search not compiled in), falling back to the scan path until the startup disk is indexed; crawl time and memory on the real volumes not yet measured. CI hidden QA journey passes on c832ed8e.
3. **Cleanup.** Rule pack from Mole & Kudu as data in core; review screen; move to Trash only; Activity list with restore.
   *Done when:* reviewing and trashing caches/build leftovers works end to end on the real disk.
   *Status 2026-10-07:* disk image installer reimplemented clean-room after Vorssaint's behaviour (no Vorssaint code; on by default); hub Storage lists mounted installers with Eject. Merged (#3): 22 rules (rules/cleanup.json), review, Move to Trash with re-validation, history with Restore. Not yet tried end to end on the Mac.
   *Status 2026-10-07 (Chrome snapshots):* the `chrome-signing-copies` rule in `rules/cleanup.json` now covers Chrome, Beta, Dev, Canary, Chrome for Testing and Chromium `*.code_sign_clone` folders (per-user `X` and `T` temp areas). It is offered only when no Chrome-family process is running (otherwise Storage and Cleanup say "Close Chrome to clear N snapshots"), re-checked with the path pattern right before the Trash move. Size is clone-aware: apparent blocks plus APFS private size (`ATTR_CMNEXT_PRIVATESIZE`, bytes no clone shares); when that cannot be read it says "apparent X, reclaimable much less" and never counts apparent size as freed. The count is sampled at most daily during the hub cleanup scan into `~/Library/Application Support/Pulse/chrome-snapshots.json` and shown as "N Chrome snapshots (+M since date)". Notch sampling is wired: `mac/Notch/Sources/System/Sensors/SystemExtras.swift` calls `ChromeSnapshotSampler.sampleIfDue`. Unverified on the Mac until CI build is run.
4. **Apps & processes.** App inventory, uninstall with leftovers, process list with Quit then explicit Force Quit.
   *Status 2026-10-07:* merged (#4): Apps view with leftovers + uninstall to Trash; Monitor processes with Quit and confirmed Force Quit. Not yet tried end to end on the Mac.
   *Helper 2026-10-07:* optional privileged helper (`SMAppService.daemon`, `PulseHelper` + `pulse-elevate`) so root-owned items uninstall with no password; Finder batch stays the fallback. Boundary in `docs/helper.md`. Registration, approval and signing unverified until the signed build runs on the Mac.
5. **Mac conveniences & launcher.** One shared event tap. Finder cut/paste → maximizer → Dock click → Auto Quit; disk image installer (default system-wide, in the notch: signed, notarized, not-yet-installed apps install automatically with an Undo card; everything else asks in the notch; eject, optional download cleanup); Fn→Command through our own HID-level event tap; launcher (independent implementation; Tinycast feature reference only, no code). Vorssaint removed 2026-10-09: app uninstalled, no Vorssaint code in Pulse.
   *Status 2026-10-08 (disk images):* the floating panel is gone. `convDiskImageAuto` (default on, hub General > Conveniences) installs a mounted image's single .app with no prompt when it verifies as signed and notarized (`spctl` source "Notarized Developer ID"), no app with its bundle id is in /Applications and it is not running; the notch then shows `DiskImageCard` ("Installed <App>", disk image ejected, Undo = Trash, auto-dismiss ~8 s, held while hovered). Unsigned/not notarized, already installed (Replace: quit, old copy to the Trash), running, several apps or a .pkg ask in the notch instead. Written without a build; unverified on the Mac.
   *Status 2026-10-07:* merged, all off by default: launcher on a Carbon hotkey (#6); event-tap conveniences — Finder cut/paste, maximizer, Dock click minimize, per-app Auto Quit (#7). Unverified on the Mac (permissions, Finder automation). Fn remap: own HID event tap; other Fn shortcuts untouched; Karabiner dropped. Verified by the owner on the Logitech keyboard 2026-10-07: Fn+C/V copy/paste (no Control Center), Fn+Space unchanged.
5a. **Drive health.** Core runs `smartctl -a -j` per physical drive (bundled or Homebrew), keeps timestamped raw readings, and shows temperature, wear %, total writes, power-on hours, critical warnings, media errors & self-test results in the hub's Storage view; alerts on changes. When the connection blocks it (USB NVMe on macOS has no passthrough), show "Health unavailable through this connection" and keep the last good reading with its date. Same backend on Windows.
   *Status 2026-10-07:* notch Disks hover done (not the hub Storage view): `smartctl -a -j` every 10 min off the main actor (Homebrew, then bundled path; not bundled yet), health, temperature, wear, written; unavailable connections show the sentence above with the last good reading and date, kept in `~/Library/Application Support/Pulse/drive-health.json`; "Install smartmontools for drive health" when absent. Unverified on the Mac until CI build is run.
   *Checked 2026-10-07:* smartctl 7.5 reads the internal APPLE SSD AP0512Z (NVMe log: temperature, % used, writes, power-on hours, warnings, media errors); the external disk6 fails with "Operation not supported by device".
   *Status 2026-10-09:* the hub Storage view shows health, temperature, wear and writes per volume with a Drive health panel. smartctl 7.5 (RightKit signed build, SHA-256 pinned, GPL-2.0 notice and source link in Resources/ThirdParty) is bundled at Contents/Helpers/smartctl and used first; Homebrew is the fallback. Not yet in a signed build: release signing waits on ownership approval enrollment.
5b. **Nearby sharing (LocalSend replacement).** Core implements the open LocalSend protocol v2 (`core/src/localsend`: multicast and `/register` discovery, HTTPS receive with accept or decline, send with progress and cancel, self-signed certificate with SHA-256 fingerprint). The hub runs it as a background service (`hub/src-tauri/src/share.rs`), the `pulse send <file…> --to <alias>` command sends without the hub, and the notch has a fifth Send cell (progress ring, device hover card, ⌘V paste, file drop, incoming request and "Saved to Downloads" cards). Settings: hub General, Nearby sharing; Local Network permission row.
   *Done when:* the owner's iPhone LocalSend app sees this Mac, files go both ways, and the LocalSend app on the Mac is no longer needed.
   *Status 2026-10-09:* built, signed and used on the Mac. The owner sent and received with an iPhone and a Windows PC (LocalSend 2.1): discovery including an HTTP subnet sweep when multicast misses, a device list card at the Send ring, a Paste clipboard button, received text shown with Copy/Open (no file), and text counted as sent once it is on the other screen. Files still ask the receiver to accept.
6. **Windows.** Native Windows notch (Rust), same Tauri hub, Windows file system provider, Recycle Bin, uninstallers.
   *Status 2026-10-09:* M1 in progress: the M0 spike in `windows/` is being extended to the Mac notch's rings (CPU, memory, disk, AI usage), hover cards, hub launch, Alt-drag and launch at login. Not yet built in CI.

## Safety rules (kept from the old plan)

- Cleanup only moves to Trash; label it "Moved to Trash: X GB". Never claim space freed until Trash is emptied.
- Nothing is pre-selected unless the rule is confident; "unknown" is never eligible.
- Re-check each item (still exists, same file, not in use) immediately before acting.
- Quit never escalates to Force Quit on its own.
- No cloud-placeholder downloads; no telemetry; no automatic cleanup.

## Working rules

- Builds, tests & signing run in GitHub Actions (RightKit workflows); no local compilation.
- Tests: end-to-end journeys through the installed app over large unit-test counts.
- Judge each phase by using the installed app, not by receipts or test counts.
