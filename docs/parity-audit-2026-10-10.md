# Pulse parity audit: Mac column, Windows column and comparison (2026-10-10)

Scope: Mac Pulse (`mac/Notch`, hub `hub/`, CLI `core/`) against Windows Pulse (`windows/`, same hub, same CLI). Row ids are those of `docs/parity.md` (125 rows, last verified at `fc0d91a6`). The Windows column now carries desktop evidence from the Dell for every row it tested.

## Evidence and limits

- Code at HEAD `5d981c33` (read, not run). Windows parity commits read: `38caa146`, `e4a78fe8`, `7d80a0fa`, plus `c2b5e3be`, `3c764909`, `8f09e6ad`, `e9af9a93` from `git log`.
- CI run 37973688769 (commit `7d80a0fa`): hub screenshots `qam/screenshots/*` (macOS leg) and `qaw/screenshots/*` (windows-2025 leg); 109 view PNGs each (`qam/views/mac`, `qaw/views/windows`); `windows-gaps.txt` says 0 of 109 views missing on Windows. Mac hub shots are a fixture (mock `notch-state`, `hub/qa-e2e/tests/ui.rs:246`); the Windows leg has a real-notch step. Both legs run on CI runners, so numbers differ by machine, not platform.
- **Dell QA 2026-10-10 (11 messages)**: installed Windows build `7d80a0fa` (Pulse 0.2.0, pid 41584), real Dell, 3456x2160 at 250% DPI, single monitor, notch docked right, hub driven via CDP, CLI read-only. "msg N" below cites the Dell message. Injected input is ignored by the keyboard hook by design, so G9 to G11 could not be exercised. Another agent drove the hub concurrently, so hub reads are counted only where the section matched. Nothing destructive or outbound was run (no send, apply, uninstall, update).
- "Mac works" means the code path exists at the cited file; the Mac app was not run. Installed Mac build is `7d80a0fa`; `5d981c33` (unhide after relaunch, Send card bottom bar, sent toast closes) is not installed on either OS (the Dell Send card still has stacked rows, msg 2).
- Dell rows marked "not triggerable / not testable read-only" are NOT gaps; they are **untested (needs a real event/send)**.

Legend, verdict tags in the Dell column: **works** / **missing** / **different** / **untested**, all "observed on Dell" unless the cell says "code only". NATIVE = OS already does it; N/A = macOS-only problem (both count as works, nothing to build). "removed by design" = removed by owner on both OSes (counted works). DISAGREE = Dell observation contradicts the earlier code or CI verdict; both are kept. H rows were not on the Dell list: code verdicts only.

## Summary counts (125 rows, Dell verdict)

| area | rows | works | missing | different | untested |
|---|---|---|---|---|---|
| A Notch cells | 11 | 6 | 1 | 3 | 1 |
| B Hover cards | 7 | 2 | 0 | 5 | 0 |
| C Notch window | 18 | 9 | 2 | 6 | 1 |
| D Alerts | 8 | 0 | 2 | 0 | 6 |
| E AI sources | 8 | 4 | 0 | 2 | 2 |
| F Nearby sharing | 8 | 6 | 0 | 0 | 2 |
| G Conveniences | 12 | 6 | 0 | 3 | 3 |
| H Launcher (code only) | 8 | 2 | 6 | 0 | 0 |
| I Lifecycle | 10 | 6 | 1 | 1 | 2 |
| J Hub | 15 | 9 | 2 | 4 | 0 |
| K Infrastructure | 8 | 6 | 0 | 1 | 1 |
| L Core and CLI | 12 | 8 | 0 | 3 | 1 |
| **Total** | **125** | **64** | **14** | **28** | **19** |

Before the Dell run the Windows column was 37 rows "pending Dell" (code present, unproven); 19 remain untested because they need a real alert, transfer, update or physical keystroke. D5 and D6 are untested on the Dell but code has no toast or preview support (kept in the top 10).

## 1. Feature table

Columns: Mac | Windows code/CI evidence | Dell observed (verdict, detail, message).

### A. Notch cells
| id | feature | Mac | Windows code/CI | Dell observed |
|---|---|---|---|---|
| A1 | Claude cell | works `Providers/ClaudeOAuthProvider.swift` | CI-r `ring-claude-*`; `windows/src/claude_accounts.rs` | **works**: Claude starburst, main arc = session 9%, clockwise from 12. DISAGREE with CI sheets (rings match): NO thin inner weekly ring drawn although card lists weekly "Fable 34%"; Mac draws it (msg 1) |
| A2 | Codex cell | works `CodexLocalProvider.swift` | CI-r `ring-codex-*` | **works**: OpenAI mark, arc 31% (Weekly only), card says Pro (msg 1) |
| A3 | System cell | works `SystemProviders.swift` | CI-r `ring-memory-*` | **works**: memory main ring ~46%, thin inner ring CPU, live (CPU 18% to 36% between hovers) (msg 1) |
| A4 | Disks cell | works | CI-r `ring-disks-*` | **works**: main ring D: 85% orange, inner ring C: 62%; fixed drives only; removable not testable (msg 1) |
| A5 | Send cell | works `Sharing/SendProvider.swift` | CI-r `ring-send-*` | **works** at idle (grey track); in-transfer ring untested (would send a file) (msg 1) |
| A6 | % under rings | works (`showsNotchReadings`) | DIFF: no labels, no toggle | **different**: no labels, no toggle (msg 1) |
| A7 | Colour bands, limits | works `UsageBand.swift` | DIFF: fixed 70/90 (`layout.rs`) | **different**: green under 70, orange at 85%, fixed bands, no settings; red not observed (msg 1) |
| A8 | Second-ring options | works | MISSING (`bridge.rs apply_set`) | **missing**: only a fixed thin ring (not drawn for Claude); no placement/dash/headline/pace options (msg 1) |
| A9 | Working-activity arc | removed by design | removed | **works** (removed by design): no arc, no session rows (msg 1) |
| A10 | Stale/blocked states | works | CI-r `ring-claude-blocked/stale/unknown` | **untested (needs a real event/send)**: all readings "Up to date" (msg 1) |
| A11 | Order, hide, nicknames, profiles | works `ProviderOrder.swift` | DIFF: order via `bridge.rs:425`; no profiles | **different**: fixed order Claude, Codex, System, Disks, Send (Mac default); one Claude ring follows Desktop's account; no hide/nickname on the notch (msg 1) |

### B. Hover cards
| id | feature | Mac | Windows code/CI | Dell observed |
|---|---|---|---|---|
| B1 | Card travel, grace delay | works `NotchViewModel.swift` | CI-r `notch-hover-claude-*` | **works**: card <1 s after pointer reaches a ring, Bezier tail, travels between cells, gone <0.3 s after leaving; instant, no animation; no-activate topmost tool window (msg 1) |
| B2 | Claude/Codex card | works `TooltipCard.swift` | CI-r `tooltip-*`; restart+sync button | **different**: Claude card has restart button, rows session/weekly with resets, no plan line, no "Updated" age; Codex card shows credits 62500, 1 unused reset, expiry; no spend/pace rows (msg 1) |
| B3 | Sessions in card | removed by design | removed | **works** (removed by design): no sessions (msg 1) |
| B4 | System extras | works `SystemSensors.swift` | DIFF: only GPU temp (code) | **different**. DISAGREE with code ("no network row"): card has CPU 36% busy 20 cores, memory used, GPU 0% busy, Network down/up KB/s Wi-Fi, GPU 61 C; NO battery row though the PC has a battery, no fans, no CPU temperature (msg 2) |
| B5 | Memory card | works | CI-r | **different**: memory is a bar inside the single System card; no separate Available/Commit rows (msg 2) |
| B6 | Disks card + drive health | works `DriveHealth.swift` | CI-r `tooltip-disks-*`; text one size larger | **different**: bars for C:, D:, G:; GiB labelled "GB" (card 342/561/360 GB free vs hub 387/603/367 GB); bottom row "Install smartmontools..." because `Helpers\smartctl.exe` is not installed, while hub Storage shows C: "OK 46 C 1% worn" (msg 2) |
| B7 | Send card | works `SendCard.swift` | CI-r `tooltip-send-*` | **different**: "Copy last" at the TOP (click copied last received text), "1 nearby", device row, "Paste clipboard" row, hint "Ctrl+V sends the clipboard"; stacked rows, not the `5d981c33` bottom bar; device click, Paste, Ctrl+V, drop untested (would send to the Mac Mini) (msg 2) |

### C. Notch window
| id | feature | Mac | Windows code/CI | Dell observed |
|---|---|---|---|---|
| C1 | Always on, no focus | works `NotchPanel.swift` | `windows/src/main.rs` | **works**: TOPMOST/TOOLWINDOW/NOACTIVATE; foreground handle unchanged after clicks (msg 2) |
| C2 | No Dock/tray | works | no tray created | **works**: no taskbar button, no tray icon (msg 2) |
| C3 | Right-click Quit only | works | CI-r `menu-quit`; DIFF fill/label | **different**: one black "Quit" plate, nothing else; dismissed by clicking an app window but NOT by clicking the taskbar (still shown 2 s later, twice); Esc not tried (msg 2) |
| C4 | Click opens hub section | works | `layout.rs` | **works**: Claude and Codex to Accounts, System to Monitor, Disks to Storage, Send to General; empty-body click not determinable (msg 2) |
| C5 | Settings handle, update dot | works `SettingsHandle.swift` | CI-r `notch-expanded-badges` | **works**: arc turns into gear disc with grip, gear opens Accounts; update/permission dots not shown (nothing pending) (msg 2) |
| C6 | Edge any side | works `NotchEdge.swift` | CI-r all edges; hub Edge moves notch | **different**. DISAGREE with CI: left, top, right work and mirror correctly; BOTTOM edge sits at y 2014-2160 but the taskbar covers all but ~26 px, rings hidden (msg 2) |
| C7 | Fold pill, visibility | works | CI-r `notch-resting-*` | **works**: alwaysShow full; onHover folds to 75x500 px pill within 0.5 s, unfolds on pointer; hidden hides all windows (msg 3) |
| C8 | Peek | works | MISSING (alert raise only) | **missing**: no peek seen (msg 3) |
| C9 | Hide for full-screen | works `FullScreenDetector.swift` | no setting | **works**: borderless full-monitor taskbar window hid the notch within 4 s; non-taskbar window did not; no setting (msg 3) |
| C10 | Multiple displays | works `NotchFleet.swift` | hub Display picker hidden | **untested (needs a real event/send)**: one monitor only; `displays=[]` (msg 3) |
| C11 | Size, scale, surface | works | DIFF: sizes + custom, no surface | **different**: small 117x798, medium 146x997, large 183x1246 px via bridge; solid black only (msg 3) |
| C12 | Move along edge, grip | works | grip added | **works**: grip drag and Alt-drag move and return; `resetPosition` restores; corner passage not tried (msg 3) |
| C13 | Single instance | works newest wins | DIFF oldest wins `runtime.rs` | **different**: second start exited in 4 s, original kept = oldest wins (msg 3) |
| C14 | Sampling cadence | works | | **works**: refresh ~2 s; idle CPU 0.8% of a core, 31 MB private; hidden cadence not measured (msg 3) |
| C15 | Launch at login | works, on by default | DIFF off by default `8f09e6ad` | **different**: no HKCU Run value, `launchAtLogin=false`; hub shows "Start with Windows: Off" (msg 3) |
| C16 | Settings persistence | works | `pill-settings.json` | **works**: rewritten on every move/mode change; ACL user + SYSTEM only (msg 3) |
| C17 | Cursor feedback | works | hand cursor on orb (code) | **different**: hand cursor over gear and grip, arrow over cells and card (msg 3) |
| C18 | Accent, language | works 13 languages | MISSING | **missing**: English only, fixed colours (msg 3) |

### D. Alerts
| id | feature | Mac | Windows code/CI | Dell observed |
|---|---|---|---|---|
| D1 | Threshold alerts | works `ThresholdNotifier.swift` | CI-r `alert-threshold` | **untested (needs a real event/send)**: needs a real 80/100% crossing; flags published true (msg 3) |
| D2 | Reset notice | works | CI-r `alert-reset` | **untested (needs a real event/send)** (msg 3) |
| D3 | Limit reached | works | CI-r `alert-*-limit` | **untested (needs a real event/send)**: usage 9/34/31% (msg 3) |
| D4 | Session finished chime/peek | works `SessionChime.swift` | MISSING | **missing**: no arc, peek or chime (msg 3) |
| D5 | Channel notch/system | works | MISSING toast (code only) | **untested (needs a real event/send)**: no alert fired; no channel setting in notch-state (msg 3) |
| D6 | Sounds, test previews | works | MISSING (`bridge.rs:12`) | **untested (needs a real event/send)**: hub preview buttons not exercised; none seen (msg 3) |
| D7 | Drive-health alert card | works | MISSING (`bridge.rs`) | **missing**: hub Overview shows "A drive is filling up" for D: but no notch card appeared (msg 3) |
| D8 | Update card | works | CI-r `update-*`; placeholder icon | **untested (needs a real event/send)**: no update pending (msg 3) |

### E. AI sources
| id | feature | Mac | Windows code/CI | Dell observed |
|---|---|---|---|---|
| E1 | Claude Desktop cache | works | implemented `claude_accounts.rs`, `zstd.rs` | **untested (needs a real event/send)**: source not observable; Claude readings do arrive (msg 9) |
| E2 | `claude /usage` CLI | works | MISSING (code only) | **untested (needs a real event/send)**: not observable (msg 9) |
| E3 | OAuth usage endpoint | works | read-only, no refresher | **works**: `pulse usage` and hub show session 9-13% and weekly 34%, 9 other accounts "no reading"; log earlier had two "Sign in needed" then "Up to date" (msg 9) |
| E4 | Codex usage | works | | **works**: weekly 31%, plan Pro, fresh (msg 9) |
| E5 | Polling, back-off | works | | **works**: 55 `usage_reading` events, Claude 9 to 13% in 20 min; back-off not exercised (msg 9) |
| E6 | Reset credits, spend, extras | works | DIFF credits (`38caa146`) | **different**. DISAGREE within Dell: Codex card shows credits and unused resets (msg 1) but `pulse usage`, hub Accounts and Overview show none and no spend/extra rows (msg 9) |
| E7 | Keychain prompts | works | N/A | **works** (N/A): no prompt rows, files read directly (msg 9) |
| E8 | Accounts page actions | works | DIFF `bridge.rs:12` | **different**. DISAGREE with code ("two rows only"): full Claude/Codex rows, show switches, order and "9 accounts never seen" work; no sign-in guidance, Allow access or Forget reading controls (msg 10) |

### F. Nearby sharing
| id | feature | Mac | Windows code/CI | Dell observed |
|---|---|---|---|---|
| F1 | Protocol core | works `core/src/localsend/` | same crate | **works**: TCP+UDP 53317, `/api/localsend/v2/info` answers, cert in `%LOCALAPPDATA%\Pulse\localsend` (msg 3, 10) |
| F2 | Discovery | works | firewall unproven | **works**: Mac Mini listed in share-state, Send card, `send --list`; Private firewall rules exist (msg 4, 10) |
| F3 | Receive accept/decline | works | | **untested (needs a real event/send)**: needs a sender; saveDir `D:\Downloads` resolved (msg 4, 10) |
| F4 | Send with progress | works | | **untested (needs a real event/send)**: would send to the Mac Mini (msg 4, 10) |
| F5 | Hub service + IPC | works `share.rs` | | **works**: share-state rewritten every 1-4 s, named events present, notch reads it live (msg 4, 10) |
| F6 | Notch cards | works | CI-r 22 `send-*` | **works** for Send cell, hover card, Copy last; Paste, Ctrl+V, drop, incoming/saved/error cards not run (msg 4) |
| F7 | CLI send | works | `--list` drops own fingerprint | **works** for `send --list` (human and JSON); `send --to` not run (msg 4, 10) |
| F8 | Settings/permission row | works Local Network | DIFF Firewall row | **works**: Firewall "Granted", nearby settings published; editing not exercised (msg 4, 10) |

### G. Conveniences
| id | feature | Mac | Windows code/CI | Dell observed |
|---|---|---|---|---|
| G1 | Finder cut/paste | works | NATIVE | **works** native, not exercised (msg 4) |
| G2 | Copy Path menu | works `FinderSync.swift` | NATIVE | **works** native, not exercised (msg 4) |
| G3 | Green button maximize | works | NATIVE | **works** native (msg 4) |
| G4 | Window shortcuts (34) | works `Conveniences/Windows/` | NATIVE partial | **different**: no Pulse window shortcuts, Win+Arrow native only (Dell verdict: missing) (msg 4) |
| G5 | Dock click minimize | works | NATIVE | **works** native (msg 4) |
| G6 | Auto quit | works | N/A | **works** not needed (msg 4) |
| G7 | Fn as Command | works | N/A | **works** not needed (msg 4) |
| G8 | Disk image installer | works | CI-r 15 `disk-*`; MSIX/MSI | **untested (needs a real event/send)**: `installer_auto=false` in this install though docs say default on; no installer card seen (msg 4) |
| G9 | Alt as Ctrl | N/A | toggle in hub | **different**: OFF by default in `7d80a0fa` (`mac_shortcuts=false`); on since `0cf561e0`, not installed; injected keys ignored (msg 4) |
| G10 | Alt+Shift+4 | N/A | `shot.rs` | **untested (needs a real event/send)**: `screenshot_shortcuts=true`, injected keys produce nothing; physical keys needed (msg 4) |
| G11 | Alt+Shift+5 toolbar | N/A | | **untested (needs a real event/send)**: same (msg 4) |
| G12 | Hub toggles for conveniences | works `ConveniencesService` | DIFF `e4a78fe8` | **different**: General shows Keyboard shortcuts (Mac-style editing Off, Screenshot shortcuts On, Save to Desktop On) and Installers; Mac Fn/window groups absent by design (msg 4, 8) |

### H. Launcher (not on the Dell list; code only)
| id | feature | Mac | Windows code only | Dell observed |
|---|---|---|---|---|
| H1 | Panel and hotkey | works | MISSING (plan: PowerToys Command Palette) | not tested; **missing** (code) |
| H2 | Apps, running, pinned | works | MISSING | not tested; **missing** (code) |
| H3 | File search | works | MISSING | not tested; **missing** (code) |
| H4 | Calculator, conversions | works | MISSING | not tested; **missing** (code) |
| H5 | Clipboard history | works | NATIVE Win+V | not tested; **works** native |
| H6 | Quicklinks, snippets | works | MISSING | not tested; **missing** (code) |
| H7 | Shortcuts, Dictionary | works | N/A | not tested; **works** (N/A) |
| H8 | Pulse commands | works | MISSING | not tested; **missing** (code) |

### I. Lifecycle
| id | feature | Mac | Windows code/CI | Dell observed |
|---|---|---|---|---|
| I1 | Open hub on section | works `HubLauncher.swift` | `hub.rs` | **works**: cell clicks and `pulse-hub.exe --section X` switch the running hub; one hub window (msg 4, 10) |
| I2 | Hub single instance | works | `win_bridge.rs` | **works**: second launch exits rc 0, first hub switches section and un-hides (msg 4, 6, 10) |
| I3 | Notch-to-hub bridge | works `HubBridge.swift` | QA step passed (stale in parity.md) | **works**: `notch-state.json` published, hub-commands (visibility, size, resetPosition) applied in ~0.3-2 s and consumed; gaps: `displays` empty, no system-readings block, notch permissions say "unknown" while hub says granted (msg 4, 10) |
| I4 | Permissions page | works | DIFF 3 rows | **works**: Notifications Allowed, Start with Windows Off, Firewall Granted; notch shows no dot (msg 5, 10) |
| I5 | Privileged helper | works | N/A | **works** not needed; uninstall asks "Admin required" per item (msg 5, 10) |
| I6 | Updater | works `Updater/` | CI-r cards | **untested (needs a real event/send)**: version 0.2.0, autoCheck on, `offered=` empty; Check now not pressed (msg 5, 10) |
| I7 | Release/packaging | works DMG | `right-release.config.mjs` | **different**: `%LOCALAPPDATA%\Programs\Pulse` has Pulse.exe, pulse-hub.exe, Helpers\pulse.exe, ThirdParty\smartmontools (licence/README only, no smartctl.exe, README describes the Mac one); no uninstaller in the folder (msg 5, 11) |
| I8 | State migration | works | | **untested (needs a real event/send)**: no legacy state (msg 5, 11) |
| I9 | Diagnostics | works | `diag.rs`, `notch.log` | **works**: `notch.log` key=value events; two earlier `UpdateLayeredWindow 0x80070578` errors, none during the run (msg 5, 11) |
| I10 | Localization | works | MISSING | **missing**: English only, no language control (msg 5, 11) |

### J. Hub
| id | feature | Mac | Windows code/CI | Dell observed |
|---|---|---|---|---|
| J1 | Shell, routing | works | DIFF frameless caption buttons | **works**: 900x600 custom title bar, Close hides (same pid), `--section storage` re-shows it; `--background` has no window and port 53317 listening; oddity: `--section overview` came up on Monitor (probably the other agent, unconfirmed) (msg 6) |
| J2 | Overview | works | works `01-overview` | **works**: drive-filling hero, Performance, Network, AI usage, Storage, Cleanup (16.6 GB), Apps (157, 14 updates) (msg 6) |
| J3 | Storage picker | works | DIFF unstyled `<select>` `02-storage` | **different**: functional (C:, D:, G:, Rescan, bar, health chip OK 46 C), visual diff from CI stands; scan header "sizes may be a little low" (msg 6) |
| J4 | Findings | works | win-* rules, CI empty | **works**: 16.6 GB eligible, Old downloads, Installers, Cargo, NuGet, Temp, Chrome cache; "Clear all safe (3.8 GB)" present; Yarn/Edge cache "In use" notes (msg 6) |
| J5 | Folders, treemap | works | | **works**: treemap and bar list; search "pulse" shows "50 matchs" (typo); row menu Open in File Explorer, Copy path, Move to Recycle Bin; junction reported `is_dir` false (msg 6) |
| J6 | Changes | works | pending | **different**: "No comparison yet - snapshot accounting is incomplete"; both home snapshots have 4735 coverage reasons (4634 "cross-volume descendant rejected", 101 "placeholder rejected"), so no comparison is ever possible (msg 6) |
| J7 | Duplicates | works | reader `duplicates.rs:107` | **works**: backend found a group, skipped hard link, refused junction; UI "Find copies" not pressed on Home (msg 6) |
| J8 | Drive health | works | NVMe unelevated | **works**: PC801 NVMe OK, 46 C, 1% worn, 17.4 TB written, sparklines, history for 2 drives; no smartctl.exe, readings from the non-smartctl path (msg 7) |
| J9 | Mounted installers eject | works | MISSING by design | **missing**: "Eject drives from File Explorer." stub (msg 7) |
| J10 | Index, live refresh | works | MISSING | **missing**: index `supported=false`, `live_refresh=false`; name search still works (msg 7) |
| J11 | Cleanup page | works | rules present | **works**: 16.6 GB / 353 items, Safe (3.8 GB) and Review groups, "34 not offered", footer "Move 7 to Recycle Bin" (not pressed); History empty; apply/restore not run (msg 7) |
| J12 | Monitor | works | DIFF pressure | **different**: charts live, banner "Memory pressure: not reported", process groups with Quit/Force Quit, system processes protected; NO Sensors section; history lost on hub restart (msg 7) |
| J13 | Apps | works real icons | generic icons `05-apps` | **different**: 157 apps, filters, winget Update buttons, Protected tag, detail with leftovers; gaps: last-used known for 24 of 157, `running` false for every app, no MSIX/AppX apps; icon gap from CI (msg 7) |
| J14 | Settings groups | works | DIFF gated, "two rows" | **works**. DISAGREE with code: Permissions, Accounts (order, show, 9 never-seen), Appearance (edges, show, fold, reset, size), Notifications (3 limits + Mute) all present; round trips verified (msg 7) |
| J15 | General | works | DIFF | **works**. DISAGREE with code ("no Updates group"): Startup, Nearby sharing, Agent bridge, Keyboard shortcuts, Installers, Updates (Check now, Automatically check), Readings; Mac-only groups hidden (msg 8) |

### K and L. Infrastructure, core, CLI
| id | feature | Mac | Windows code/CI | Dell observed |
|---|---|---|---|---|
| K1 | Rendering | SwiftUI works | Rust rasteriser CI-r | **works**: layered per-pixel alpha, anti-aliased, crisp at 250% (msg 5) |
| K2 | DPI/monitors | works | | **works**: `per_monitor_v2`, scales with 250%; mixed DPI/hot-plug untested (msg 5) |
| K3 | HTTPS client | works | | **works**: usage endpoints succeed every cycle (msg 5) |
| K4 | JSON | works | | **works** (msg 5) |
| K5 | Resource safety | works | | **works**: ~420 handles stable, 31 MB private (msg 5) |
| K6 | Local file security | works | ACE fix `3c764909` | **works**: settings ACL user + SYSTEM; mutex `Local\Pulse.Pill.v1.<SID>` blocked a second instance (msg 5) |
| K7 | Carried tests | existing | existing | **untested (needs a real event/send)**: not observable on the desktop (msg 5) |
| K8 | View-shot renderer | works | works 109/109 in CI | **different**. DISAGREE with CI: `Pulse.exe --render-views <dir>` on the installed build exited 2, no PNGs (msg 5) |
| L1 | CLI surface | works | adds usage, apps, bridge, claude | **works**: all read-only commands exit 0 with `--json`; quirks in section 4 (msg 8) |
| L2 | Scanner | works | `win_native.rs` | **works**: hard link not double counted, junction skipped and marks coverage incomplete, limits named, access-denied reported; 200,000 entries in 46 s (msg 8) |
| L3 | Snapshot store | works | | **works**: save, history, find, browse, export; growth unavailable whenever a snapshot is incomplete (J6) (msg 8) |
| L4 | Cleanup engine/executor | works Trash | Recycle Bin `cleanup.rs` | **untested (needs a real event/send)**: no CLI cleanup command, apply/restore destructive (msg 8) |
| L5 | Rule data | 27 non-Windows rules | win-* rules | **different**: hub rules work (J4) but CLI `findings` reports 0 and 3 explanation rules after a scan (no CLI cleanup scan) (msg 8) |
| L6 | Process list, Quit | works | hub `taskkill` | **works**: 508 processes, sorts OK, gpu sort errors, CLI actions read-only; hub groups by exe; Quit not pressed (msg 8) |
| L7 | App inventory | works | `apps_windows/` | **works**: 157 apps 0.8 s, 14 winget updates, detail; partial name errors; running always false (msg 9) |
| L8 | System status | works | no pressure `lib.rs:262` | **different**: pressure "unavailable (Windows commit pressure)", Purgeable/Snapshots unavailable; `monitor` rates need a sampler, ports "Unsupported" (msg 9) |
| L9 | IPC worker | works | named pipe | **works**: answered status, procs, scan over a pipe, idle-timeout exit; short endpoint rejected `endpoint_unsafe` (msg 9) |
| L10 | Drive health core | works | `disk_of_mount` None | **different**: no CLI command; works only via hub (msg 9) |
| L11 | Duplicates core | works | `duplicates.rs:107` | **works**: group found, hard link skipped, junction not followed, budgets honoured (msg 9) |
| L12 | Compression, export | works | portable | **works**: `export` emits 5 modules; compression has no consumer (msg 9) |

## 2. Visual comparison

### Hub sections (qam vs qaw, same 900x600 viewport; CI evidence)
| section | difference | evidence |
|---|---|---|
| Title bar | Windows draws min/max/close buttons; Mac none in the capture | `*/01-overview.png` |
| Scrollbar | Windows persistent grey scrollbar on Overview, Apps, General; Mac overlay | `01`, `05`, `10-general` |
| Storage | Windows picker is an unstyled native select; Mac styled. Mac shows "Install smartmontools" link, Windows none | `02-storage` |
| Monitor | Mac "Normal" pressure, Windows "not reported" (Dell confirms, msg 7) | `04-monitor` |
| Apps | Windows generic icon on all rows; Mac real icons | `05-apps` |
| Permissions | rows differ by OS; Windows "Open Settings" buttons | `06-permissions` |
| Accounts | Mac fixture Claude account list overflows the card (see 5.1); Windows two plain rows in CI, full list on the Dell | `07-accounts` |
| Appearance | Windows Edge Top/Bottom/Left/Right, Always show/On hover/Hidden, Fold to pill; no Displays, Surface, Rings, Colour groups (Dell confirms, msg 7) | `08-appearance` |
| Notifications | Windows Limits + Mute only; Mac adds Channel, Open the notch for, sounds | `09-notifications` |
| General | Windows adds Keyboard shortcuts, Installers, Agent bridge; Mac adds Uninstalling | `10-general` |

### Notch views that differ beyond font (109 ids compared by sheet)
- `menu-quit`: label, shortcut hint, fill and border differ. `update-*` (4 ids) and `disk-*` (15 ids): app icon replaced by placeholder tile; wording differs.
- `tooltip-disks-*`: Windows lacks per-drive temperature and "(system)" suffix; detail lines one size larger. `tooltip-system-*`: header "CPU 88 °C" vs "88 °C".
- `tooltip-claude-access-denied/-error/-signin`, `tooltip-send-network-blocked`: platform wording (intended). `send-sending-*`: Windows card one line taller (309 vs 294 px). `notch-resting-*`: Windows pill 496x160 vs Mac 222x80.
- Fixture clocks (`alert-session-limit` times, "Jan 15, 2027") are not behaviour. Identical within tolerance: all 24 `ring-*`, `notch-expanded-*`, `notch-hover-*`, `alert-reset`, `alert-threshold`, `tooltip-codex-*`.
- Dell cross-check (msg 1): the real Claude ring has no inner weekly ring while the CI `ring-claude-*` sheets match Mac; the CI fixture hides this.

## 3. Windows-only features (observed)
Observed on the Dell unless marked code only.
- Claude hover card restart button for Claude Desktop (not pressed, would close the chat); Codex card shows credits and unused resets (Dell QA msg 5, A extra).
- System card NVIDIA GPU temperature "GPU 61 C" and Wi-Fi/Ethernet kind in the network row (msg 5, B extra).
- Notch mirrors on all four edges; six-dot grip drag works without Alt; hub-driven `resetPosition`; per-monitor edge map in `pill-settings.json` (msg 5, C extra; msg 11).
- Single-instance mutex by user SID; notch hides for any borderless full-monitor taskbar window (msg 5, C extra).
- Send card "Copy last" (last received text) button; `pulse send --list` works without the hub window (msg 5, F extra).
- Alt+Shift+4/5 screenshot overlay (Desktop PNG + clipboard) and Alt+A/C/V/X/Z to Ctrl remap with Alt+Shift+Z redo: present, not exercised (injected input ignored) (msg 5, G extra).
- Bridge through `notch-state.json` / `hub-commands` signalled by named events (msg 5, I extra).
- Pure-Rust software rasteriser, WinHTTP client, SID-named mutex, RAII handle owners, DACL-restricted settings file (msg 5, K extra).
- Hub Permissions "Windows Firewall" and "Start with Windows" rows; General Keyboard shortcuts and Installers groups; "Move to Recycle Bin", "Open in File Explorer", "Admin required" wording; Ctrl+C / Ctrl+Backspace folder-menu shortcuts; C marked Startup (msg 11, J extra).
- CLI: per-user named-pipe worker (default `pulse-worker-<SID>`, full pipe path required); `pulse claude accounts/known/backups/auto/include/exclude/sync --dry-run/restore` and `pulse bridge peers/status/install --dry-run` answer (bridge status: 51 chats here, 55 on the Mac Mini) (msg 11, L extra).
- Code only: signed MSIX/MSI auto-installer (`installer.rs`), winget updates, UserAssist last-used, NVMe IOCTL drive health without elevation (`health_windows.rs`), Mute Claude/Codex alert toggles in hub.

## 4. Windows defects observed on the Dell
Severity: H blocks a feature or hides data; M visible wrong behaviour; L cosmetic or CLI.
| sev | defect | evidence |
|---|---|---|
| H | 4634 files under the home folder rejected as "cross-volume descendant" (plus 101 placeholders) make every home scan incomplete, so Changes/growth can never compare | msg 6, 8, 11 |
| H | No `smartctl.exe` shipped; ThirdParty README is the Mac one; Disks card says "Install smartmontools" although hub Storage shows drive health | msg 2, 5, 11 |
| H | Bottom edge: taskbar covers all but ~26 px, rings hidden | msg 2 |
| H | Drive-filling condition shows a hub banner but no notch drive alert card (D7) | msg 3 |
| M | Claude weekly inner ring not drawn although the weekly reading exists | msg 1 |
| M | Apps `running` flag always false (hub and CLI, even for Claude); last-used known for 24 of 157; no MSIX/AppX apps | msg 7, 9 |
| M | Alt-remap default off in installed `7d80a0fa` (`mac_shortcuts=false`); on since `0cf561e0`, not installed. `installer_auto=false` though docs say default on | msg 4 |
| M | Single-instance is oldest-wins (new process exits), Mac is newest-wins | msg 3 |
| M | Quit plate not dismissed by a click on the taskbar | msg 2 |
| M | Disk card sizes in GiB labelled "GB" and differ from hub Overview (387 vs 342 GB for G:) | msg 2 |
| M | Notch-state permissions say "unknown" for notifications/firewall while hub says granted; `displays` empty | msg 10 |
| M | `Pulse.exe --render-views` exits 2 on the installed build, no PNGs | msg 5 |
| L | Search count label "50 matchs"; junction reported `is_dir` false | msg 6 |
| L | Hub chart history lost on hub restart; a hub launched with `--section overview` came up on Monitor (unconfirmed) | msg 6, 7 |
| L | CLI panics "failed printing to stdout" on a closed pipe (`monitor | head`); `monitor` prints JSON without `--json`; `procs --groups` header says "not grouped" and groups follow the parent chain (wininit.exe 293 members) unlike the hub's per-exe groups; `scan` of a missing path exits 0; relative path roots print mixed separators; `apps detail` needs the exact name | msg 8, 9, 11 |
| L | Two earlier `UpdateLayeredWindow 0x80070578` errors in `notch.log` (before the Dell run started) | msg 5 |
| L | Claude card has no plan line and no "Updated" age; Send card still stacked rows (build predates `5d981c33`) | msg 1, 2 |

## 5. Top 10 gaps by user impact (Dell evidence)
1. Home scans always incomplete on Windows (4634 cross-volume rejects), so Changes/growth and every snapshot comparison fail (J6, L3): scanner volume check in `core/src/platform/win_native.rs`.
2. Alerts: no sound, no peek, no drive-alert card even when a drive is filling up (D4, D7, C8; D5/D6 untested, no code): `windows/src/alerts.rs` plus a sound/toast module.
3. Drive health on the Disks card and in the CLI: no `smartctl.exe` shipped, card says "Install smartmontools" while hub shows NVMe health (B6, J8, L10, I7): `core/src/drive_health.rs:408`, `right-release.config.mjs`.
4. Bottom edge unusable behind the taskbar (C6): `windows/src/layout.rs` work-area clamp.
5. System/Monitor data: no battery, fans or CPU temperature rows; Monitor "Memory pressure: not reported" and no Sensors; no separate memory card (B4, B5, J12, L8): `core/src/lib.rs:262`, `windows/src/sensors.rs`, `hub/src/views/Monitor.tsx:155`.
6. Hub Apps: `running` always false, generic icons, last-used 24/157, no MSIX apps (J13, L7): `hub/src-tauri/src/apps_windows.rs`.
7. Claude weekly inner ring not drawn and no second-ring options (A1, A8): `windows/src/render.rs`.
8. No launcher on Windows (H1 to H8): planned PowerToys Command Palette extension.
9. Appearance and polish: no Surface style, accent, language (C11, C18, I10), no peek, notch-state permissions "unknown" (I3), Accounts sign-in/Forget and `claude /usage` absent (E8, E2): `windows/src/settings.rs`, `bridge.rs apply_set`.
10. Packaging defaults and release hygiene: Alt-remap and installer defaults off in the installed build (G8, G9), oldest-wins instance (C13), `--render-views` exit 2 (K8), CLI closed-pipe panic (L1): `windows/src/runtime.rs`, `core/src/main.rs`.

Prior list (before Dell): sound/toast, memory pressure, System rows, launcher, Apps icons, Appearance, peek/dot, sign-in, drive alert, unstyled Storage picker. Changes: Storage picker (J3) dropped (cosmetic); drive-health packaging, bottom edge and incomplete scans added.

## 6. Mac-side findings from the evidence
1. **Accounts layout overflow (broken).** `qam/screenshots/07-accounts.png` at 900x600 (`hub/src-tauri/tauri.conf.json:19`): Claude account rows (Work, old@example.test, Claude 5e6f7a8b) run past the right edge of the card; "resets in" cut off. Likely `.ck-claude-meters { width: 400px }` in `hub/src/views/settings.css:23-38` (cause not verified).
2. Mac hub QA runs only a fixture; a Mac real-notch hub journey is missing (`hub/qa-e2e/tests/ui.rs`).
3. Overview "Network not available" on both platforms in CI while Monitor charts show throughput seconds later (`hub/src/views/Overview.tsx:162-164,300`). The Dell Overview showed live Network KB/s (msg 6), so this is a first-paint fixture effect.
4. `13-notch-size` and `13-notch-real-appearance` (qaw) share a sha256, so the "size" shot is the appearance page. The Dell exercised size via the bridge (msg 3).
5. `5d981c33` is not installed; the Send card bottom bar and sent-toast behaviour are unverified on both installed builds.
6. `docs/parity.md` is stale: I3, E1, C17, J4, J7, J11, L5, L7, L11, B4, E6, D8, J14, J15 and C15 differ from HEAD and the Dell; refresh it from the verdicts above.
