# Pulse parity audit: Mac column and comparison (2026-10-10)

Scope: Mac Pulse (`mac/Notch`, hub `hub/`, CLI `core/`) against Windows Pulse (`windows/`, same hub, same CLI). Row ids are those of `docs/parity.md` (125 rows, last verified at `fc0d91a6`). The Windows desktop column is being tested separately on the Dell; everything marked "pending Dell" is code-present but not desktop-proven.

## Evidence and limits

- Code at HEAD `5d981c33` (read, not run). Windows parity commits read: `38caa146`, `e4a78fe8`, `7d80a0fa`, plus `c2b5e3be`, `3c764909`, `8f09e6ad`, `e9af9a93` from `git log`.
- CI run 37973688769 (commit `7d80a0fa`): hub screenshots `qam/screenshots/*` (macOS leg) and `qaw/screenshots/*` (windows-2025 leg); 109 view PNGs each (`qam/views/mac`, `qaw/views/windows`); `windows-gaps.txt` says 0 of 109 views missing on Windows. I built side-by-side sheets of the views and read the hub shots.
- Limits: the Mac hub shots are a fixture (mock `notch-state`, `hub/qa-e2e/tests/ui.rs:246` offers Edge options only top/bottom); the Windows leg has a real-notch step (`qaw/.../notch-applies-the-hub-s-edge-change-9da442bc`, passed, plus `13-notch-real-*` shots). Both legs run on CI runners, so numbers, drives and times differ by machine, not by platform. The Mac has no equivalent real-notch QA step.
- "Mac works" below means the code path exists at the cited file; I did not run the Mac app. Installed Mac build is `7d80a0fa`; `5d981c33` (unhide after relaunch, Send card bottom bar, sent toast closes) is not installed.
- Several `docs/parity.md` rows are stale against HEAD; this audit overrides them where HEAD code says otherwise (marked "(stale in parity.md)").

Legend, Windows column: **CI-r** = code builds and views render in CI (`qaw/views`); **pending Dell** = code present at the cited file, needs a desktop run; **MISSING** = no code; **NATIVE** = OS already does it; **N/A** = macOS-only problem; **DIFF** = exists but behaves or reads differently.

## 1. Feature table

### A. Notch cells
| id | feature | Mac | Windows | gap detail |
|---|---|---|---|---|
| A1 | Claude cell | works `Providers/ClaudeOAuthProvider.swift` | CI-r `ring-claude-*`; pending Dell | rings match in sheets; Windows reads Desktop's account (`38caa146`, `windows/src/claude_accounts.rs`) |
| A2 | Codex cell | works `Providers/CodexLocalProvider.swift` | CI-r `ring-codex-*`; pending Dell | none seen |
| A3 | System cell | works `System/SystemProviders.swift` | CI-r `ring-memory-*`, `ring-system-cpu-only` | Windows memory ring = in-use share, not OS pressure |
| A4 | Disks cell | works `SystemProviders.swift` | CI-r `ring-disks-*` | none seen |
| A5 | Send cell | works `Sharing/SendProvider.swift` | CI-r `ring-send-*` | none seen |
| A6 | % under rings | works (`showsNotchReadings`) | DIFF: no labels, no toggle (`windows/src/layout.rs`) | hub toggle absent on Windows |
| A7 | Colour bands, limits, ramp | works `Model/UsageBand.swift` | DIFF: fixed 70/90 bands | `windows/src/layout.rs` band_color; no settings |
| A8 | Second-ring options | works | MISSING | not in `bridge.rs apply_set` keys |
| A9 | Working-activity arc | removed for Claude by owner (both) | removed (both) | none |
| A10 | Stale/blocked states | works | CI-r `ring-claude-blocked/stale/unknown` | none |
| A11 | Order, hide, nicknames, profiles | works `ProviderOrder.swift`, `ClaudeProfile.swift` | DIFF: order/show via `connect`/`order` (`bridge.rs:425`), Claude account rename/forget (`claude_accounts.rs`); no profiles | no nicknames for Codex; no multi-profile |

### B. Hover cards
| id | feature | Mac | Windows | gap detail |
|---|---|---|---|---|
| B1 | Card travel, grace delay | works `Notch/NotchViewModel.swift` | CI-r `notch-hover-claude-*`; pending Dell | no animation on Windows |
| B2 | Claude/Codex card | works `Features/TooltipCard.swift` | CI-r `tooltip-claude-*`, `tooltip-codex-*` | Windows adds restart+sync button; Codex credits/plan (`38caa146`); error wording differs in `tooltip-claude-error` |
| B3 | Sessions in card | removed by owner (both) | removed | none |
| B4 | System extras (GPU, net, fans, temp, battery) | works `System/SystemSensors.swift` | DIFF: only GPU temp via NVML, missing rows omitted (`38caa146`) | fans, CPU temp, network, battery rows absent on a real PC; CI view uses fixture rows (`tooltip-system-*`) so it hides this |
| B5 | Memory card | works | CI-r | none |
| B6 | Disks card + drive health | works `System/DriveHealth.swift` | CI-r `tooltip-disks-*`; DIFF: no per-drive temperature in fixture render, text one size larger and wraps | `windows/src/card.rs` secondary-line font size; wording "on this connection" vs "over USB" |
| B7 | Send card | works `Sharing/SendCard.swift` | CI-r `tooltip-send-*`, `send-picker-*`; pending Dell for paste/drop | copy icon glyph differs |

### C. Notch window
| id | feature | Mac | Windows | gap detail |
|---|---|---|---|---|
| C1 | Always on, no focus | works `Notch/NotchPanel.swift` | pending Dell `windows/src/main.rs` | |
| C2 | No Dock/tray | works | pending Dell (no tray created) | |
| C3 | Right-click Quit only | works `NotchWindowController` | CI-r `menu-quit`; DIFF: label "Quit" no shortcut hint, black fill vs bordered grey | `windows/src/render.rs` menu draw |
| C4 | Click opens hub section | works | pending Dell; no empty-area to overview click recorded | `windows/src/layout.rs` |
| C5 | Settings handle, update dot | works `Features/SettingsHandle.swift` | CI-r `notch-expanded-badges`; settings button on open notch (`38caa146`); permissions dot has no data source | amber dot never lit on Windows |
| C6 | Edge any side | works `Notch/NotchEdge.swift` | CI-r `notch-*-left/right/top/bottom`; hub Edge moves notch (`c2b5e3be`, QA step passed) | |
| C7 | Fold pill, visibility | works | CI-r `notch-resting-*`; pending Dell; pill notch-sized (`3c764909`) | no animation; resting render is 2.2x scale vs Mac 2x (size 496x160 vs 222x80) |
| C8 | Peek | works `peekDuration` | MISSING (only 5-6 s alert raise) | `windows/src/main.rs` |
| C9 | Hide for full-screen | works `Notch/FullScreenDetector.swift` | pending Dell; no setting | |
| C10 | Multiple displays | works `NotchFleet.swift` | pending Dell; no follow-active-window | hub Display picker hidden on Windows (`08-appearance`) |
| C11 | Size, scale, surface | works | DIFF: small/medium/large + custom scale (`38caa146`); no surface style | glass/solid choice missing |
| C12 | Move along edge, grip | works | pending Dell; grip added (`38caa146`) | no corner passage |
| C13 | Single instance | works newest wins | DIFF: oldest wins (mutex) `windows/src/runtime.rs` | an update relaunch relies on the old copy exiting |
| C14 | Sampling cadence | works | pending Dell | |
| C15 | Launch at login | works, on by default | DIFF: off by default since `8f09e6ad`; toggle works via `bridge.rs:554` | |
| C16 | Settings persistence | works UserDefaults | pending Dell `pill-settings.json` | |
| C17 | Cursor feedback | works | pending Dell: hand cursor on orb exists (`windows/src/main.rs:2472`) (stale in parity.md) | |
| C18 | Accent, language | works 13 languages | MISSING (English only) | |

### D. Alerts
| id | feature | Mac | Windows | gap detail |
|---|---|---|---|---|
| D1 | Threshold alerts | works `Model/ThresholdNotifier.swift` | CI-r `alert-threshold` | |
| D2 | Reset notice | works | CI-r `alert-reset` | |
| D3 | Limit reached | works | CI-r `alert-session-limit`, `alert-weekly-limit` | mute keys in hub (`09-notifications` qaw) |
| D4 | Session finished chime/peek | works `SessionChime.swift` | MISSING | no sound code in `windows/src` |
| D5 | Channel notch/system | works `Model/NotificationChannel.swift` | MISSING toast | hub Channel row hidden on Windows |
| D6 | Sounds, test previews | works | MISSING (bridge ignores previews, `bridge.rs:12`) | |
| D7 | Drive-health alert card | works (`driveAlert`) | MISSING | `bridge.rs` ignores `driveAlert` |
| D8 | Update card | works | CI-r `update-*`; DIFF: placeholder spinner icon instead of app icon | `windows/src/update.rs`/`render.rs` icon |

### E. AI sources
| id | feature | Mac | Windows | gap detail |
|---|---|---|---|---|
| E1 | Claude Desktop cache | works `Providers/ClaudeDesktopUsageCache.swift` | pending Dell: implemented (`38caa146`, `windows/src/claude_accounts.rs`, `zstd.rs`) (stale in parity.md) | |
| E2 | `claude /usage` CLI | works | MISSING | |
| E3 | OAuth usage endpoint | works | pending Dell, read-only, no refresher `windows/src/usage.rs` | expired token shows "Sign in needed" |
| E4 | Codex usage | works | pending Dell | |
| E5 | Polling, back-off | works | pending Dell | |
| E6 | Reset credits, spend, extras | works | DIFF: Codex credits/unused resets (`38caa146`); no spend windows/Pi | |
| E7 | Keychain prompts | works | N/A | |
| E8 | Accounts page actions | works (`signIn`,`allowAccess`,`signOut`) | DIFF: show/hide/order work (`07-accounts` qaw); sign-in/forget-reading commands ignored (`bridge.rs:12`) | no Forget reading row on Windows |

### F. Nearby sharing
| id | feature | Mac | Windows | gap detail |
|---|---|---|---|---|
| F1 | Protocol core | works `core/src/localsend/` | pending Dell (same crate) | |
| F2 | Discovery | works | pending Dell; firewall/multicast unproven | |
| F3 | Receive accept/decline | works | pending Dell | |
| F4 | Send with progress | works | pending Dell | |
| F5 | Hub service + IPC | works `hub/src-tauri/src/share.rs` | pending Dell | |
| F6 | Notch cards | works | CI-r 22 `send-*` views; sending card wraps one line more (`send-sending-*`) | |
| F7 | CLI send | works | pending Dell; `send --list` drops own fingerprint (`38caa146`) | Mac strings remain |
| F8 | Settings/permission row | works Local Network | DIFF: Windows Firewall row ("Checking…" in `06-permissions`) | |

### G. Conveniences
| id | feature | Mac | Windows | gap detail |
|---|---|---|---|---|
| G1 | Finder cut/paste | works | NATIVE | |
| G2 | Copy Path menu | works `FinderExtension/FinderSync.swift` | NATIVE (Shift+right-click) | |
| G3 | Green button maximize | works | NATIVE | |
| G4 | Window shortcuts (34) | works `Conveniences/Windows/` | NATIVE partial (Win+Arrow) | thirds, display moves absent |
| G5 | Dock click minimize | works | NATIVE | |
| G6 | Auto quit | works | N/A | |
| G7 | Fn as Command | works | N/A | |
| G8 | Disk image installer | works `DiskImageInstaller.swift` | CI-r 15 `disk-*`; DIFF: MSIX/MSI installer, no eject; wording "still in Downloads" | pending Dell for install flow |
| G9 | Alt as Ctrl | N/A (Cmd native) | pending Dell; toggle in hub General (`10-general` qaw) | Windows-only |
| G10 | Alt+Shift+4 | N/A (system) | pending Dell `windows/src/shot.rs` | Windows-only |
| G11 | Alt+Shift+5 toolbar | N/A (system) | pending Dell | Windows-only |
| G12 | Hub toggles for conveniences | works `ConveniencesService` | DIFF: toggles for own keys now exist (`e4a78fe8`, `10-general`) | Mac Fn/window groups absent by design |

### H. Launcher
| id | feature | Mac | Windows | gap detail |
|---|---|---|---|---|
| H1 | Panel and hotkey | works `Launcher/LauncherController.swift` | MISSING (plan: PowerToys Command Palette) | |
| H2 | Apps, running, pinned | works | MISSING | |
| H3 | File search | works | MISSING | |
| H4 | Calculator, conversions | works | MISSING | |
| H5 | Clipboard history | works | NATIVE Win+V | |
| H6 | Quicklinks, snippets, commands | works | MISSING | |
| H7 | Shortcuts, Dictionary | works | N/A | |
| H8 | Pulse commands | works | MISSING | |

### I. Lifecycle
| id | feature | Mac | Windows | gap detail |
|---|---|---|---|---|
| I1 | Open hub on section | works `System/HubLauncher.swift`; supervised since `01c57690` | pending Dell `windows/src/hub.rs` | |
| I2 | Hub single instance | works | pending Dell `hub/src-tauri/src/win_bridge.rs` | |
| I3 | Notch-to-hub bridge | works `System/HubBridge.swift` | CI-proven partly: `windows/src/bridge.rs` exists; QA step passed on windows-2025 (stale in parity.md: was Not started) | no `conveniences`, `helper`, `system` blocks; hub hides what notch omits |
| I4 | Permissions page | works | DIFF: 3 Windows rows (`06-permissions` qaw) | |
| I5 | Privileged helper | works | N/A | |
| I6 | Updater | works `Updater/` | CI-r cards; download/verify/install pending Dell `windows/src/update.rs` | |
| I7 | Release/packaging | works DMG | in progress `right-release.config.mjs` | no signed candidate recorded |
| I8 | State migration | works | pending Dell | |
| I9 | Diagnostics | works os.Logger | pending Dell `diag.rs`, `notch.log` (`8f09e6ad`) | |
| I10 | Localization | works | MISSING | |

### J. Hub
| id | feature | Mac | Windows | gap detail |
|---|---|---|---|---|
| J1 | Shell, routing | works | DIFF: frameless with caption buttons (`c2b5e3be`; `01-overview` qaw) | Mac title bar has no custom buttons (qam) |
| J2 | Overview | works `01-overview` | works `01-overview` qaw; both show "Network not available" while Monitor charts have data | `hub/src/views/Overview.tsx:300` first-paint |
| J3 | Storage picker | works | DIFF: native-styled `<select>` (black box, other font) `02-storage` qaw | `hub/src/views/Storage.tsx` + CSS |
| J4 | Findings | works | win-* rules now exist (`rules/cleanup.json`: 19 win-* rules, e.g. win-temp) (stale in parity.md); CI shows empty Cleanup | pending Dell for real findings |
| J5 | Folders, treemap | works | pending Dell (screenshot works, `02-storage` qaw) | |
| J6 | Changes | works | pending Dell | |
| J7 | Duplicates | works | DIFF: Windows content reader exists `core/src/duplicates.rs:107` (stale in parity.md) | pending Dell |
| J8 | Drive health | works | pending Dell: NVMe unelevated (`e4a78fe8`); smartctl not bundled | |
| J9 | Mounted installers eject | works | MISSING by design | |
| J10 | Index, live refresh | works | MISSING (scan reuse under 6 h, `e4a78fe8`) | |
| J11 | Cleanup page | works | pending Dell; rules present | |
| J12 | Monitor | works | DIFF: "Memory pressure: not reported" (`04-monitor` qaw; `hub/src/views/Monitor.tsx:155`); Sensors none | |
| J13 | Apps | works with real icons | works, generic icon for every app (`05-apps` qaw), 59 apps listed | icon extraction missing |
| J14 | Settings groups | works | DIFF: gated to published keys (`e4a78fe8`); Accounts shows two rows only | |
| J15 | General | works | DIFF: Windows keyboard/installer groups; no Updates group visible, no Uninstalling | |

### K and L. Infrastructure, core, CLI
| id | feature | Mac | Windows | gap detail |
|---|---|---|---|---|
| K1 | Rendering | SwiftUI works | Rust rasteriser CI-r | font differs by design |
| K2 | DPI/monitors | works | pending Dell | |
| K3 | HTTPS client | works | pending Dell | |
| K4 | JSON | works | pending Dell | |
| K5 | Resource safety | works | pending Dell | |
| K6 | Local file security | works | pending Dell; read-only ACE fix `3c764909` | |
| K7 | Carried tests | existing | existing | no new component tests per `AGENTS.md` |
| K8 | View-shot renderer | works `App/ViewShots.swift` | works, 109/109 | |
| L1 | CLI surface | works | works; adds `usage`, `apps list|updates|detail` (`38caa146`), `bridge`, `claude` (`core/src/main.rs:90`) | `apps uninstall` hub-only on Windows |
| L2 | Scanner | works | pending Dell `core/src/platform/win_native.rs` | |
| L3 | Snapshot store | works | pending Dell | |
| L4 | Cleanup engine/executor | works Trash | pending Dell Recycle Bin `hub/src-tauri/src/cleanup.rs` | |
| L5 | Rule data | 27 non-Windows rules | win-* rules present (stale) | |
| L6 | Process list, Quit | works `core/src/process_control.rs` | DIFF: hub `taskkill`, core control unix only | |
| L7 | App inventory | works | works in core (`core/src/apps_windows/`) | |
| L8 | System status | works | DIFF: no memory pressure `core/src/lib.rs:262` | |
| L9 | IPC worker | works | pending Dell named pipe | |
| L10 | Drive health core | works | DIFF: `disk_of_mount` None (`core/src/drive_health.rs:408`); hub maps instead | |
| L11 | Duplicates core | works | works, Windows reader (`duplicates.rs:107`) | |
| L12 | Compression contracts, export | works | portable | no consumer either OS |

## 2. Visual comparison

### Hub sections (qam vs qaw, same 900x600 viewport)
| section | difference | evidence |
|---|---|---|
| Title bar | Windows draws min/max/close buttons top right; Mac has none in the capture | `*/01-overview.png` |
| Scrollbar | Windows shows a persistent grey scrollbar on Overview, Apps, General; Mac overlay | `01`, `05`, `10-general` |
| Overview | same layout; Windows lists 4 AI rows (Claude, Codex) and two drives; Mac one Codex row and one drive. Network "not available" on both | `01-overview` |
| Storage | Windows volume picker is an unstyled native select (black border, Segoe, full width); Mac styled picker. Mac shows "Install smartmontools / Drive health" link, Windows none. Windows treemap shows one block | `02-storage` |
| Cleanup | identical empty state, tab pill slightly wider on Mac | `03-cleanup` |
| Monitor | Mac "Normal" pressure, Windows "not reported"; Windows swap "0 B of 3.1 GB" vs "No swap space" | `04-monitor` |
| Apps | Windows generic window icon on all rows, Refresh button live; Mac real icons, "Refreshing…/Checking…" mid-load | `05-apps` |
| Permissions | rows differ by OS (Accessibility/Automation/Open at login vs Notifications/Start with Windows/Firewall); Windows "Open Settings" buttons | `06-permissions` |
| Accounts | Mac fixture shows Codex+Claude cards, Work/old@example/Claude 5e6f7a8b account list, which overflows the card on the right (see 5.1); Windows shows two plain rows "Up to date" | `07-accounts` |
| Appearance | Windows: Edge Top/Bottom/Left/Right, Show Always show/On hover/Hidden, Fold to pill, no Displays/Display/Fold for full-screen/Surface/Rings/Colour groups. Mac fixture: Edge Top/Bottom only, Show Always/On hover | `08-appearance`, `12-notch-edge-after` |
| Notifications | Windows: Limits + Mute alerts only; Mac fixture adds Channel, Open the notch for, Agent sessions, sounds | `09-notifications` |
| General | Windows adds Keyboard shortcuts, Installers, Agent bridge, Pulse skill, Device name; Mac adds Updates, Uninstalling. Windows "Open at login" off, Mac on | `10-general`, `13-notch-real-general` |
| NaN / empty pickers | none seen on either leg (fixed by `e4a78fe8`); no "NaN" text in any of the 20 shots | all |

### Notch views that differ beyond font (109 ids compared by sheet; 74 sampled directly, the rest share a template with a viewed id)
- `menu-quit`: label, shortcut hint, fill and border differ.
- `update-available`, `update-available-no-notes`, `update-downloading`, `update-installing`: app icon replaced by a spinner/placeholder tile on Windows.
- `disk-*` (15 ids, e.g. `disk-installed-undo`): Windows shows a generic cube tile instead of the app icon; wording differs (Downloads vs Trash, Windows installers).
- `tooltip-disks-critical`, `-healthy`, `-health-*`: Windows lacks per-drive temperature, "(system)" suffix, detail lines one size larger and wrapping.
- `tooltip-system-*`: Windows header "CPU 88 °C", Mac "88 °C"; detail line size differs.
- `tooltip-claude-access-denied`, `-error`, `-signin`: platform wording (Allow access vs file permissions; network lost vs service unavailable).
- `tooltip-send-network-blocked`: Local Network vs Windows Firewall wording (intended). `tooltip-send-copy-last`, `send-picker-*`: different copy-glyph, spinner glyph; text sizes match.
- `send-sending-*`: Windows card one line taller (309 vs 294 px).
- `notch-resting-*`: Windows pill drawn at a larger scale (496x160 vs 222x80), shape matches.
- Time-of-day text (`alert-session-limit` "7:26 PM" vs "8:51 AM", `tooltip-*` resets) and Windows fixture "Jan 15, 2027" (`windows/src/viewshots.rs:36` NOW = 1.8e9) come from fixed fixture clocks, not behaviour.
- Identical within rasteriser tolerance: all `ring-*` (24), `notch-expanded-*`, `notch-hover-*`, `alert-reset`, `alert-threshold`, `tooltip-codex-*`.

## 3. Windows has, Mac lacks
- Alt+A/C/V/X/Z as Ctrl keys (`windows/src/keys.rs`; toggle in hub General).
- Alt+Shift+4 region and Alt+Shift+5 toolbar screenshots (`windows/src/shot.rs`); Mac relies on the system feature.
- Signed MSIX/MSI auto-installer from Downloads (`installer.rs`, `installer_auto`).
- winget app updates, registry inventory, UserAssist last-used, Recycle Bin restore (`hub/src-tauri/src/apps_windows*.rs`).
- Drive health through NVMe IOCTL without elevation and PowerShell fallback (`hub/src-tauri/src/health_windows.rs`).
- GPU temperature via NVML in the System card (`windows/src/sensors.rs`).
- Per-monitor edge storage and per-monitor enable (`pill-settings.json` `edges`, `monitors`).
- Windows Firewall permission row, Mute Claude/Codex alert toggles in hub (`09-notifications` qaw).
- Hub `bridge` and `pulse usage` readable from the published snapshot work on both, but the real-notch CI journey exists only on Windows (`hub/qa-e2e/tests/ui.rs:1256`).

## 4. Top 10 gaps by user impact
1. Alerts have no sound or system toast on Windows (D4, D5, D6): `windows/src/alerts.rs` plus a new sound/toast module.
2. Windows hub Monitor shows "Memory pressure: not reported" and no Sensors (J12, L8): `core/src/lib.rs:262` `read_memory_pressure` (commit-charge band) and `hub/src/views/Monitor.tsx:155`.
3. Windows System card lacks CPU temperature, fans, network, battery rows (B4): `windows/src/sensors.rs`, `windows/src/card.rs`.
4. No launcher on Windows (H1 to H8): planned PowerToys Command Palette extension; no file exists yet.
5. Hub Apps page shows generic icons on Windows (J13): `hub/src-tauri/src/apps_windows.rs` (extract exe icon, as `windows/src/fileicon.rs` already does for the notch).
6. Appearance has no Surface style, Rings, Colour, accent controls on Windows (A7, A8, C11, C18): `windows/src/settings.rs`, `windows/src/bridge.rs apply_set`, `windows/src/render.rs`.
7. No peek and no permissions dot data on the Windows notch (C8, C5): `windows/src/main.rs`.
8. Windows sign-in/Forget reading and `claude /usage` fallback absent (E2, E8): `windows/src/bridge.rs apply`, `windows/src/usage.rs`.
9. Drive-health alert card missing on Windows (D7) and `disk_of_mount` unmapped in core (L10): `windows/src/alerts.rs`, `core/src/drive_health.rs:408`.
10. Storage volume picker unstyled on Windows (J3): `hub/src/views/Storage.tsx` and `hub/src/views/storage.css` (use the `ck-select` class like the Appearance Display picker).

## 5. Mac-side findings from the evidence
1. **Accounts layout overflow (broken).** `qam/screenshots/07-accounts.png` at the default 900x600 window (`hub/src-tauri/tauri.conf.json:19`): the Claude account rows (Work, old@example.test, Claude 5e6f7a8b) run past the right edge of the card; "resets in" for Session/Weekly is cut off and "no r..." clipped. Likely `.ck-claude-meters { width: 400px }` with the name column in `hub/src/views/settings.css:23-38` (cause not verified). The Windows leg could not show it because its fixture has no Claude account list.
2. Mac hub QA runs only a fixture; the Windows leg proved a real notch publishes state and moves on an Edge change. A Mac real-notch hub journey is missing (`hub/qa-e2e/tests/ui.rs`).
3. Overview "Network not available" on both platforms while Monitor charts show throughput seconds later (`qam/01-overview` vs `04-monitor`): first-paint fallback in `hub/src/views/Overview.tsx:162-164,300`; the `e4a78fe8` fix did not change the Windows screenshot either.
4. `13-notch-size` and `13-notch-real-appearance` (qaw) have the same sha256, so the "size" evidence is the appearance page, not a distinct size shot (Windows leg; for the Dell agent).
5. `5d981c33` is not installed; the Send card bottom bar and sent-toast behaviour are unverified on the installed build. `tooltip-send-copy-last` and `send-sent` Mac renders predate or include it only if CI ran at `7d80a0fa` (they are from `7d80a0fa`, so they do not show the new bottom bar).
6. Stale `docs/parity.md`: I3, E1, C17, J4, J7, J11, L5, L11, L7, B4, E6, D8 status are older than the code; the file needs a refresh from the Dell results.
