# Cockpit plan

Date: 2026-10-07. Replaces [implementation-plan.md](implementation-plan.md) (kept for history only).

## Goal

One owned tool that replaces Vorssaint, CodexBar & manual disk cleanup. Mac first; Windows after Mac is done.

## Surfaces

| Surface | What it is | Built with |
| --- | --- | --- |
| Notch | Always on. Right screen edge. Four cells, each a main outer ring with a thin inner ring: Claude (weekly / 5-hour), Codex (weekly / 5-hour), System (memory pressure / CPU), Disks (external / internal). Hover card shows details. | Fork of Codenotch Mac app (Swift), `mac/Notch` |
| Hub | Small window (about 600×400) opened by clicking the notch. Storage, Cleanup, Apps, Monitor, Settings. Closes fully when closed. | Tauri (shared with Windows later), RightKit packages |
| CLI | `cockpit` for agents: same data & actions as the hub, JSON output. | Existing Rust core |

**App presence:** no Dock icon & no menu-bar item, ever. The notch is the only surface. Right-click on the notch shows one item: **Quit**, which exits notch & hub. Launch at login brings it back. The hub runs as an accessory window (no Dock icon while open).

## Architecture

- **Notch:** Codenotch Mac fork, cut to Claude & Codex, plus resource & disk rings. Keeps Codenotch's design, hover card, Option-drag along the edge, settings orb (opens hub Settings) & updater. Swift samples its own cheap counters; nothing heavy runs in the notch.
- **Hub:** Tauri app bundled inside Cockpit.app. Its Rust backend links `cockpit-core` directly; no bridge layer, no separate worker for the hub.
- **Core:** existing `core/` crate: scanner, APFS-aware accounting, filename index, duplicates, growth history, monitor readings, process groups, rule engine, cleanup state machine, CLI & local socket.
- **Storage:** versioned JSON files (current). Move to SQLite only if a feature needs queries JSON can't serve.
- **Not used:** UniFFI, always-running worker, menu-bar/Dock presence.

## Donors (reimplement; copy patterns, not whole apps)

| Feature | Primary donor | Secondary |
| --- | --- | --- |
| Notch, AI usage readers, updater | [Codenotch](https://github.com/vinzdg/codenotch) (fork) | — |
| CPU, GPU, memory, disk, network, battery, sensors | [Stats](https://github.com/exelban/stats) | [mectrics](https://github.com/farukkamcici/mectrics), [slawek19926/Vitals](https://github.com/slawek19926/Vitals) |
| Processes / task manager | [hmarr/vitals](https://github.com/hmarr/vitals) | Stats; [TaskExplorer](https://github.com/DavidXanatos/TaskExplorer) for Windows |
| Disk map, scan, growth | Petal (in `vendor/petal`) & existing core | — |
| Cleanup rules | [Mole](https://github.com/tw93/Mole), [Kudu](https://github.com/AdventDevInc/kudu) `rules/` | [PureMac](https://github.com/momenbasel/PureMac), [MacSai](https://github.com/iliyami/MacSai), [purge-app](https://github.com/jithin-sabu/purge-app) |
| Hub UI reference | Kudu (React; same idea, Electron instead of Tauri) | — |
| Uninstall & leftovers | Pearcleaner | Mole |
| Finder cut/paste, maximizer, Dock click, Auto Quit | Vorssaint (in `upstream/vorssaint`) | — |
| Launcher | Tinycast (in `upstream/tinycast`) | — |
| Windows cleanup (later) | [FluentCleaner](https://github.com/builtbybel/FluentCleaner), Kudu | CleanmgrPlus |

## Phases

Each phase ends with something usable on the Mac, installed as a signed, notarized build.

1. **Notch fork.** Import Codenotch, strip other providers, add system & disk cells, no Dock/menu-bar item, right-click Quit only, launch at login. Repoint release packaging to the new app.
   *Done when:* it looks & behaves like Codenotch with Cockpit's rings, and runs a full day without issues.
   *Status 2026-10-07:* notch built on CI (`xcode-27`) and approved by eye from a preview build — compact body, small size, rings only, paired rings, no working spinner ([mac/Notch/FORK.md](../mac/Notch/FORK.md)). Remaining: release packaging + signed install, launch at login, removing unused Codenotch provider code, full-day run.
2. **Hub: Storage & Monitor.** Tauri hub from the approved mockup; storage list with drilldown & growth, search, monitor readings, Settings. Clicking the notch opens it.
   *Done when:* finding what's using space takes seconds, not a learning curve.
3. **Cleanup.** Rule pack from Mole & Kudu as data in core; review screen; move to Trash only; Activity list with restore.
   *Done when:* reviewing and trashing caches/build leftovers works end to end on the real disk.
4. **Apps & processes.** App inventory, uninstall with leftovers, process list with Quit then explicit Force Quit.
5. **Mac conveniences & launcher.** One shared event tap. Finder cut/paste → maximizer → Dock click → Auto Quit; Fn→Command via Karabiner-Elements config first, own tap only if Karabiner falls short; Tinycast launcher. Turn off each Vorssaint feature as its replacement works; then remove Vorssaint.
6. **Windows.** Native Windows notch (Rust), same Tauri hub, Windows file system provider, Recycle Bin, uninstallers.

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
