# Pulse parity report: Mac vs Windows

Maintained feature-by-feature comparison. Mac column is read from the Swift, Rust and TypeScript source, not from plans. Windows status is judged from code in `windows/`, `hub/` and `core/`.

## Summary

Last verified against commit bea47f005d4d6ff44ad11ce81553f0ebb08a068d (read from `.git/HEAD` -> `refs/heads/main`; the working tree also holds uncommitted `mac/.build/`, which is ignored here). Report written 2026-10-09.

124 feature rows in 12 areas. Windows status, counted by the leading label of each row (rows with a qualifier, for example "In progress ... ; health Not started", count once under the leading label; the qualifier is in the row):

| Windows status | Rows | Meaning |
| --- | --- | --- |
| Done | 0 | Nothing is verified on Windows yet |
| In progress | 50 | Code exists: 37 are windows/ notch code or the hub Windows shell and `H/win_bridge.rs` ("written, uncompiled"), 13 are shared core code not gated off Windows (CI result not verified here) |
| Planned | 15 | In a plan or an owner request, no code (LocalSend on Windows, Alt+A/C/V, Alt+Shift+4, Alt+Shift+5, launcher via Command Palette) |
| Not started | 47 | Wanted for parity, no code and no prior design |
| Not needed | 12 | Native in Windows or macOS-only problem |

Counts by area (In progress / Planned / Not started / Not needed): A cells 7/1/3/0, B hover cards 4/1/2/0, C notch behaviour 12/0/6/0, D alerts 0/0/8/0, E AI sources 3/0/4/1, F nearby sharing 5/3/0/0, G conveniences and screenshots 0/4/1/7, H launcher 0/6/0/2, I lifecycle 4/0/4/2, J hub pages 1/0/14/0, K Windows infrastructure 7/0/0/0, L core and CLI 7/0/5/0.

Bottom line: the Windows notch M1 draws five rings, hover cards, Alt-drag, Quit menu, full-screen hide, hub launch and launch at login, all written and never compiled. The hub backend (everything except `H/win_bridge.rs`) is macOS-only today. Core is portable in parts (scan, store, IPC, LocalSend protocol). The three owner-requested Windows features are Planned with designs below: G9 Alt+A/C/V, G10 Alt+Shift+4, G11 Alt+Shift+5, plus F (LocalSend, no separate app).


## How to read this

Path shorthand (all relative to the repository root `/pulse`):

| Prefix | Expands to |
| --- | --- |
| `S/` | `mac/Notch/Sources/` (Swift notch app) |
| `FX/` | `mac/Notch/FinderExtension/` |
| `HLP/` | `mac/Notch/Helper/`, `mac/Notch/Elevate/`, `mac/Notch/Shared/` |
| `C/` | `core/src/` (Rust core and `pulse` CLI) |
| `H/` | `hub/src-tauri/src/` (Tauri backend) |
| `HU/` | `hub/src/` (React UI, shared by both OSes) |
| `W/` | `windows/src/` (native Windows notch) |

Windows status vocabulary (counted in the summary by the leading word):

- **Done**: implemented and verified on Windows. Nothing qualifies yet: no Windows code has a recorded compile or run result (see below).
- **In progress**: code exists in the tree. The qualifier says whether it is "written, uncompiled" (never built in CI), or "shared code" (core crate paths that are not gated off Windows, CI result not verified by this report).
- **Planned**: designed here or in `docs/plan.md` / `docs/implementation-plan.md` / an owner request; no code yet.
- **Not started**: the feature is wanted for parity but there is no code and no committed design before this report; the design in this file is a proposal.
- **Not needed**: Windows already does it natively, or the Mac feature solves a macOS-only problem (reason given).

Evidence rules: `windows/README.md` states "nothing in M1 has been compiled yet" and the CI gate (`scripts/gate.sh`, Windows runner: `cargo fmt`, `cargo test --locked`, `cargo clippy`) has not been recorded as green for `windows/`. The hub Windows bridge (`H/win_bridge.rs`) is the same. Both are therefore "written, uncompiled". Anything marked "uncertain" below could not be settled from the source.

## Findings that affect parity (read first)

1. **The hub backend does not build for Windows as written (by reading; not compiled).** `H/share.rs` declares libSystem `notify_post/notify_register_check/notify_check` unconditionally; `H/cleanup.rs` imports `trash::macos`; `H/apps.rs` imports `pulse_core::app_manager` and `process_control`, which are `#[cfg(unix)]` in `C/lib.rs`; `H/lib.rs` `home()` reads `HOME` (falls back to `/`) and `bridge_dir()` is `Library/Application Support/Pulse`. `H/win_bridge.rs` is the only Windows-specific hub file.
2. **Core LocalSend uses a weak random fallback on Windows.** `C/localsend/proto.rs` `fill_random` returns false off Unix, so `random_hex` (session ids, tokens, file ids, device fingerprint seed) falls back to `RandomState` mixed with the clock. Needs `BCryptGenRandom` before nearby sharing ships on Windows.
3. **Docs drift.** `docs/implementation-plan.md` ("Keyboard consistency") assigns Alt+letter to Ctrl+letter to PowerToys Keyboard Manager. The owner request of 2026-10-09 moves Alt+A/C/V into Pulse itself; this report follows the owner request. `docs/plan.md` says 22 cleanup rules; `rules/cleanup.json` now has 27. `docs/implementation-plan.md` says Windows memory pressure = commit vs limit; the Windows notch ring is physical memory in use (`W/layout.rs`), commit is shown in the hover card only. `W/hub.rs` `SECTIONS` lacks `appearance` and `notifications`, which `H/lib.rs` `SECTIONS` has.
4. **No Windows release target.** `right-release.config.mjs` has only a `mac` target. No Windows installer, signing, updater or release lane is configured.
5. **No Windows entry to hub Settings.** The Windows notch has no gear/settings handle, so only ring clicks (`monitor`, `storage`, `accounts`) open the hub.
6. **Core CLI strings assume a Mac.** `C/send_cmd.rs` identity sets `device_model: "Mac"` and falls back to host name "Mac".
7. **AGENTS.md gate.** Pulse's rule is no shortcut changes or disabling of existing utilities while feasibility gates are unresolved. Every new global shortcut below (Alt+A/C/V, Alt+Shift+4/5, any hotkey) must ship default off, like the Mac conveniences (all off by default).

## A. Notch cells (rings)

Mac: five cells (Claude, Codex, System, Disks, Send) in `S/Notch/NotchViewModel.swift` / `NotchLayout.swift`, each a `ProviderRing` (`S/Features/ProviderRing.swift`) with an optional thin inner ring. Windows: five cells but a different set (CPU, Memory, Disk, Claude, Codex) in `W/layout.rs`.

| ID | Feature | Mac implementation | Windows equivalent | Windows status | Windows files |
| --- | --- | --- | --- | --- | --- |
| A1 | Claude cell | `S/Providers/ClaudeOAuthProvider.swift` (one provider per `~/.claude*` profile); main ring = 5-hour session, thin inner ring = weekly (`S/Settings/WeeklyRing.swift`); ring drawn by `ProviderRing` (track + clockwise arc from 12 o'clock) | Same metrics (44 DIP ring, 5.8 track, 3.0 arc, 14.1 inner radius) drawn by a software rasteriser into a layered window; headline window = main ring, weekly = inner ring | In progress (written, uncompiled) | `W/layout.rs`, `W/canvas.rs`, `W/render.rs`, `W/usage.rs` |
| A2 | Codex cell | `S/Providers/CodexLocalProvider.swift`, `CodexUsage.swift`, `CodexProfile.swift` (default `~/.codex` profile only); same ring layout | Same as A1 with the Codex reader | In progress (written, uncompiled) | `W/usage.rs`, `W/layout.rs` |
| A3 | System cell: memory pressure (main) + CPU (inner) | `S/System/SystemProviders.swift` `SystemLoadProvider`: `host_statistics` CPU ticks delta; memory ring = `kern.memorystatus_level`, colour from `kern.memorystatus_vm_pressure_level` (normal/warning/critical), not a used% threshold | Two separate cells: CPU busy share from `GetSystemTimes` deltas; Memory = physical in use from `GlobalMemoryStatusEx`. No pressure concept on Windows; commit charge shown in the card | In progress (written, uncompiled) | `W/sensors.rs`, `W/lifecycle.rs`, `W/layout.rs` |
| A4 | Disks cell: external (main) + internal (inner) | `SystemProviders.swift` `DisksProvider`: every local, writable, browsable volume (`mountedVolumeURLs`), startup volume = inner ring, first external = main ring; free space via `volumeAvailableCapacityForImportantUsage` | One Disk ring = system-drive used share; card lists every fixed drive (`GetDriveTypeW` fixed only). Removable/external drives not shown | In progress (written, uncompiled) | `W/sensors.rs`, `W/card.rs`, `W/layout.rs` |
| A5 | Send cell (nearby sharing) | `S/Sharing/NearbySharing.swift`, `SendProvider.swift`: ring = transfer in flight, device list in card; `S/Sharing/SendCard.swift` | Sixth cell with the same behaviour (see F) | Planned | none |
| A6 | Percent under each ring | `showsNotchReadings` preference; `NotchLayout` label height | Always drawn (`--` when unknown, never zero); no toggle | In progress (written, uncompiled) | `W/layout.rs`, `W/render.rs` |
| A7 | Colour bands and thresholds | `S/Model/UsageBand.swift` ample/watch/critical; `watchLimit`/`criticalLimit` sliders; `ColorTransitionStyle` hardStep or continuous ramp; `AccentColor` (10 choices) | Fixed bands: under 70% green, under 90% orange, else red (`band_color`); no settings | In progress (written, uncompiled) for fixed bands; configurable limits, ramp, accent are Not started | `W/layout.rs` |
| A8 | Second-ring options | `WeeklyRing` (off/inside/outside), `weeklyRingDashed`, `WeeklyHeadline` (weekly as main), `weeklyReading` (both readings), `DailyPace` (Claude daily pace ring), `UsagePace`, `showCodexExtraLimits` | Weekly thin ring always on, fixed style | Not started | none |
| A9 | Working-activity inner arc | `S/Sessions/ActivityCoordinator.swift`, `ClaudeSessionMonitor`, `CodexActivityMonitor`, `PiResponseMonitor`: thin neutral arc inside the ring while an agent session is busy | Not implemented | Not started | none |
| A10 | Stale / blocked / refreshing ring states | `ProviderRing`: blocked reads as spent, refreshing indicator, stale dim (`ProviderSnapshot.status`) | Stale readings dimmed and dated; unavailable draws no arc | In progress (written, uncompiled) | `W/usage.rs`, `W/render.rs`, `W/card.rs` |
| A11 | Cell order, hide/show, nicknames, multiple Claude profiles | `Preferences.providerOrder`, `connectedProviders`, `accountNicknames`; `ClaudeProfile` discovers `~/.claude` and `~/.claude-<slug>` | Fixed order, single default profile | Not started | none |

## B. Hover cards

Mac: `S/Features/TooltipCard.swift` (glass card, tail, per-cell content), positioned by `NotchWindowController.tooltipRect`. Windows: one lazily created layered card window shared with the Quit menu (`ensure_card`, `show_card`).

| ID | Feature | Mac implementation | Windows equivalent | Windows status | Windows files |
| --- | --- | --- | --- | --- | --- |
| B1 | Card appearance, travel between cells, grace delay | `cursorMoved()` polls the cursor (the panel ignores mouse events until hovered); `hoveredIndex` set with spring, cleared after `hoverGrace`; card moves with `NotchMotion.tooltip`; clicks on card do not fall through | Card follows `WM_MOUSEMOVE` + `WM_MOUSELEAVE` arming; dark solid; clamped to the monitor; no animation | In progress (written, uncompiled) | `W/main.rs`, `W/render.rs`, `W/card.rs` |
| B2 | Claude / Codex card content | `ProviderTooltip`, `LimitWindowRow` (percent, reset countdown in absolute or relative form via `resetTimeFormat`), `UsageResetCreditsSection`, `MoneyBreakdownView` (spend), `BlockedRow`, plan/age header, `UsagePace` | Windows rows with percent, `Resets in …`, plan accessory, `Updated <age>` or status text with last reading | In progress (written, uncompiled) for windows/reset/plan/age; spend, reset credits, pace, blocked row Not started | `W/card.rs`, `W/usage.rs` |
| B3 | Live agent sessions in card, click focuses terminal | `SessionList`/`SessionRow`; `S/Sessions/SessionFocus.swift` walks the process tree to the owning app; `TerminalTabFocus.swift` selects the tab for cmux, Terminal.app, iTerm2, Ghostty via AppleScript | None | Not started | none |
| B4 | System card extras: GPU, network, fans, CPU temperature, battery | `SystemSensors.swift` (IOAccelerator `PerformanceStatistics`, HID thermal sensors), `Sensors/NetworkThroughput.swift` (`getifaddrs`), `SMCFans.swift`, `BatteryReader.swift`, `SystemExtras.swift` cadence | Card shows CPU busy % and logical processors only | Not started (CPU and memory rows In progress, written, uncompiled) | `W/card.rs`, `W/sensors.rs` |
| B5 | Memory card | `MemoryProvider.window()`: pressure state, used of total (app + wired + compressed) | In use of total, Available, Commit of limit | In progress (written, uncompiled) | `W/card.rs`, `W/sensors.rs` |
| B6 | Disks card with drive health | `DisksProvider` rows + `S/System/DriveHealth.swift`: `smartctl -a -j` every 10 min, temperature trailing text, wear, written, "unavailable through this connection" with last good date | Per-drive free/total bars (system drive first); no health rows | In progress (written, uncompiled) for bars; health Not started | `W/card.rs`, `W/sensors.rs` |
| B7 | Send card (device list, click to send, Paste button, drop target) | `S/Sharing/SendCard.swift`, `NearbySharing.select/pasteClipboard/drop` | Same via Windows card window and clipboard/drop APIs | Planned | none |

## C. Notch window and behaviours

| ID | Feature | Mac implementation | Windows equivalent | Windows status | Windows files |
| --- | --- | --- | --- | --- | --- |
| C1 | Always-on, never takes focus | `S/Notch/NotchPanel.swift`: borderless non-activating `NSPanel` above the menu bar and full-screen apps; `BackgroundCursor.swift` lets a background panel set the cursor | One borderless layered window per monitor, `WS_EX_LAYERED / NOACTIVATE / TOOLWINDOW / TOPMOST`, `WM_MOUSEACTIVATE` returns `MA_NOACTIVATE`, `UpdateLayeredWindow` per-pixel alpha | In progress (written, uncompiled) | `W/main.rs`, `W/surface.rs`, `W/raii.rs` |
| C2 | No Dock icon, no menu-bar item, no tray | `NSApp.setActivationPolicy(.accessory)` + `LSUIElement` (`AppDelegate`, `mac/Notch/project.yml`) | No taskbar button (`WS_EX_TOOLWINDOW`), no tray icon is created | In progress (written, uncompiled) | `W/main.rs` |
| C3 | Right-click menu shows only Quit | `NotchWindowController.contextMenu()` ("Quit Pulse"), `NotchPanel` handles right click before SwiftUI | Non-activating card drawn on `WM_RBUTTONUP`, polled by a menu timer, dismissed by clicking elsewhere, Quit posts `WM_CLOSE` | In progress (written, uncompiled) | `W/main.rs`, `W/render.rs` |
| C4 | Click opens the hub on a section | `handleClick`: disks to `storage`, system to `monitor`, send to `general`, accounts to `accounts`, empty notch area to `overview`; permissions-pending click goes to `permissions` | Claude/Codex to `accounts`, CPU/Memory to `monitor`, Disk to `storage` | In progress (written, uncompiled); no `overview`/Send mapping | `W/layout.rs`, `W/hub.rs`, `W/main.rs` |
| C5 | Settings handle (gear) and update dot | `S/Features/SettingsHandle.swift`: arc at rest, gear on hover, opens hub Settings; red dot when `Updater.pending` | None | Not started | none |
| C6 | Edge choice (left/right/top/bottom) and stack direction | `S/Notch/NotchEdge.swift`, `NotchPlacement.swift`, `NotchLayout.swift`, `SideNotchShape.swift`; default right edge; stack turns on its side on top/bottom | Top edge only, horizontal stack | Not started (other edges) | `W/lifecycle.rs`, `W/layout.rs` |
| C7 | Pill that unfolds on hover; Always show / Show on hover / Hidden | `NotchVisibility` + `NotchWindowController.setExpanded`, `NotchMotion` springs, `SideNotchShape` is animatable | Full-size body always drawn; only a boolean `visible` in the settings file | Not started | `W/settings.rs` (flag only) |
| C8 | Peek (opens by itself for 3 / 5 / 10 s) | `PeekDuration`, `peek(for:focusing:)`, used for completions, alerts, size changes | None | Not started | none |
| C9 | Hide for full-screen apps | `S/Notch/FullScreenDetector.swift` (layer-0 window spanning the display of the frontmost app) plus `AXFullScreen`; `foldsForFullScreen` preference folds instead of hiding | Topmost visible non-owned, non-tool, non-cloaked window on the notch monitor that covers the monitor and has no caption or resize frame hides the panel; slower sampling while hidden | In progress (written, uncompiled); no setting to turn it off | `W/visibility.rs`, `W/visibility_cases.rs`, `W/main.rs` |
| C10 | Multiple displays | `NotchFleet` owns one `NotchWindowController` per display; `NotchScreenScope` main/all; `DisplayPreference` follow active window or a named display | One panel per monitor from `EnumDisplayMonitors`, reconciled on `WM_DISPLAYCHANGE`/`WM_DPICHANGED`; per-monitor `enabled` flag in settings; no follow-active-window | In progress (written, uncompiled) | `W/main.rs`, `W/lifecycle.rs`, `W/settings.rs` |
| C11 | Size, custom scale, surface style | `NotchSize` small/medium/large, custom 0.5-1.5, `NotchSurfaceStyle` glass / dark glass / solid | Scale follows monitor DPI only; solid dark surface only | In progress (written, uncompiled) for DPI scale and solid; size choice and glass Not started | `W/layout.rs`, `W/render.rs` |
| C12 | Move along the edge (Option-drag), grip dots, corner passage, reset | `NotchPanel` Option-drag deltas, `MoveHandle.swift` six-dot grip, `CornerPassage.swift` goes round screen corners, per-edge offset in `Preferences.offset(for:)`, hub "Reset position" | Alt-drag moves along the top edge; position stored per monitor as per-mille of width; atomic write | In progress (written, uncompiled) for Alt-drag and persistence; grip, corner travel, reset Not started | `W/main.rs`, `W/layout.rs`, `W/settings.rs` |
| C13 | Single instance | `AppDelegate.retireOlderInstances()` (newest wins, quits strictly older copies) | Named mutex `Local\Pulse.Pill.v1.<user-sid>` (older copy keeps running, newcomer exits) | In progress (written, uncompiled); behaviour differs (oldest wins) | `W/runtime.rs`, `W/main.rs` |
| C14 | Sampling cadence | System cells every 2 s, 5x slower while the notch is hidden (`setSystemSamplingSlow`) | `cadence_seconds` (default 2) while any panel is visible, long interval when all hidden | In progress (written, uncompiled) | `W/lifecycle.rs`, `W/settings.rs` |
| C15 | Launch at login | `Preferences.launchAtLogin` via `SMAppService.mainApp` (on by default once, only for `/Applications` copy) | `HKCU\Software\Microsoft\Windows\CurrentVersion\Run\Pulse`, on by default, `"launch_at_login": false` turns it off | In progress (written, uncompiled) | `W/autostart.rs`, `W/settings.rs` |
| C16 | Settings persistence | `Preferences.swift` (UserDefaults, ~100 keys) published to `notch-state.json` (see I3) | `%LOCALAPPDATA%\Pulse\pill-settings.json` schema 1 (`visible`, `cadence_seconds`, `monitors`, `launch_at_login`, `positions`), hand-written bounded parser, write-then-rename, restricted DACL; unreadable file means defaults and never overwritten | In progress (written, uncompiled) | `W/settings.rs`, `W/json.rs` |
| C17 | Cursor feedback (pointing hand, open hand) | `NotchWindowController.setCursor` | Not seen in source | Not started (uncertain: not found by search) | none |
| C18 | Accent colour, language | `AccentColor` (system + 9 colours), `AppLanguage` (13 languages, `Localizable.xcstrings`), `L10n.swift` | English only | Not started | none |

## D. Alerts and notifications

| ID | Feature | Mac implementation | Windows equivalent | Windows status | Windows files |
| --- | --- | --- | --- | --- | --- |
| D1 | Threshold crossing alerts (watch, critical) | `S/Model/ThresholdNotifier.swift` fed by `UsageStore.snapshots` (usage only, never system cells) | Same logic in the Windows notch, shown as a notch card | Not started | none |
| D2 | Usage reset notice | `UsageResetWatcher`, `UsageResetCard.swift`, `previewUsageResetAlert` | Notch card | Not started | none |
| D3 | Session/weekly limit reached notice | `UsageLimitWatcher`, `announceUsageLimit`, preview buttons in hub | Notch card | Not started | none |
| D4 | Agent session finished / blocked: peek + chime | `ActivityCoordinator` + `announceCompletions`, `SessionCompletionWatcher`, `SessionChime.swift` (AVAudioPlayer, two-tone for blocked) | Peek (C8) plus sound | Not started | none |
| D5 | Channel: notch or Notification Center | `NotificationChannel.swift`, `ChannelNotifications` (UNUserNotificationCenter), fallback to banner when notch hidden | Notch card, or a Windows toast | Not started | none |
| D6 | Sounds and test/preview buttons | `sessionEndSound`, `usageResetSound`, `limitReachedSound` with name pickers; hub "Send a test" and three previews | Sound plus the same hub buttons | Not started | none |
| D7 | Drive-health alert card | `announceDriveAlert` from hub (`driveAlert` action); skipped while notch hidden | Notch card | Not started | none |
| D8 | Update card | `S/Features/UpdateCard.swift`, `Updater.$prompt` | Notch card (see I6) | Not started | none |

## E. AI usage sources

| ID | Feature | Mac implementation | Windows equivalent | Windows status | Windows files |
| --- | --- | --- | --- | --- | --- |
| E1 | Claude via Claude Desktop HTTP cache | `ClaudeDesktopUsageCache.swift` reads the Chromium Simple Cache entry for the usage URL, vendored zstd decoder (`S/Vendor/zstd`); no token, no network | Windows Claude Desktop cache layout is unverified; may be inapplicable | Not started (uncertain) | none |
| E2 | Claude via `claude /usage` | `ClaudeUsageCLI.swift`, throttled by `ClaudeOAuthProvider.cliRefreshInterval`, scratch directory ignored by session monitors | Spawn `claude` hidden (`CREATE_NO_WINDOW`) | Not started (README: not yet ported) | none |
| E3 | Claude via OAuth usage endpoint | `ClaudeCredentials.swift`/`KeychainItem.swift` read the `Claude Code-credentials` keychain item; `GET api.anthropic.com/api/oauth/usage`; `ClaudeTokenRefresher` renews by running `claude -p` with empty stdin | `%USERPROFILE%\.claude\.credentials.json` (or `CLAUDE_CONFIG_DIR`), WinHTTP GET, read-only, never refreshed or written | In progress (written, uncompiled); refresher Not started | `W/usage.rs`, `W/http.rs`, `W/json.rs` |
| E4 | Codex usage | `CodexLocalProvider`: reads credentials Codex owns, live account limits; honours server retry deadline | `%USERPROFILE%\.codex\auth.json` (or `CODEX_HOME`), `GET chatgpt.com/backend-api/wham/usage` | In progress (written, uncompiled) | `W/usage.rs`, `W/http.rs` |
| E5 | Polling and back-off | `UsageStore`: busy/idle schedules, refresh when looked at (`refreshBecauseSomeoneIsLooking`), on work finishing | Every 5 min on a worker thread; 60 s doubling to 15 min after 429; expired or missing sign-in shows "Sign in needed", no retry loop | In progress (written, uncompiled) | `W/usage.rs` |
| E6 | Reset credits, spend windows, extra limits, Pi stream | `ClaudeResetCredits`, `UsageResetCredits`, `MoneyBreakdownView`, `PiResponseMonitor` | Not ported | Not started | none |
| E7 | Keychain consent prompts and "Allow access" | `KeychainPrompt.swift`, hub Accounts "Allow access…" | Windows reads plain credential files; no OS prompt exists | Not needed (no keychain gate on Windows) | none |
| E8 | Accounts page actions: show/hide, order, sign in guidance, Forget reading | Hub Accounts via `HubBridge` commands `connect`, `order`, `signIn`, `allowAccess`, `signOut` | Depends on the Windows bridge (I3) | Not started | none |

## F. Nearby sharing (LocalSend)

Owner requirement: LocalSend (nearby sharing) the same as on the Mac; no separate LocalSend app needed on Windows.

| ID | Feature | Mac implementation | Windows equivalent | Windows status | Windows files |
| --- | --- | --- | --- | --- | --- |
| F1 | Protocol core (LocalSend v2) | `C/localsend/` (`mod.rs` `Service`, `proto.rs` wire types and name rules, `net.rs` self-signed rustls identity and HTTP/1.1 subset, `discovery.rs`, `receive.rs`, `send.rs`); written from the open spec, no LocalSend code | Same crate, no change to protocol code. Off-Unix helpers already exist (`bind_shared`, `use_interface`, `write_private`) but are weaker; `fill_random` is a stub | In progress (shared code; not exercised on Windows; weak RNG fallback) | `C/localsend/*.rs` |
| F2 | Discovery | UDP multicast `224.0.0.167:53317` on every IPv4 interface (`SO_REUSEADDR` + `SO_REUSEPORT`), `/register` announce, HTTP subnet sweep when multicast is lost | Same; Windows `bind_shared` is plain `UdpSocket::bind` (no reuse flags), `use_interface` is a no-op, so multicast out-interface and shared-port binding need Windows work and a firewall rule | In progress (shared code; gaps noted) | `C/localsend/discovery.rs` |
| F3 | Receive with accept / decline | HTTPS routes `register`, `info`, `prepare-upload`, `upload`, `cancel`; nothing written until accepted; `sanitize_relative`, `reserve_unique`; save folder default `~/Downloads`; text messages shown with Copy/Open | Same; save folder from `SHGetKnownFolderPath(FOLDERID_Downloads)` (Mac code uses `$HOME/Downloads`) | In progress (shared code); Downloads lookup Planned | `C/localsend/receive.rs`, `C/localsend/proto.rs` |
| F4 | Send with progress and cancel | `C/localsend/send.rs`: `prepare-upload` then per-file upload; progress and cancel events | Same | In progress (shared code) | `C/localsend/send.rs` |
| F5 | Hub-side service and IPC with the notch | `H/share.rs` runs `Service` in the hub (also with window closed); notch reads `share-state.json`, writes `share-commands/*.json`, Darwin notifications `dev.orthic.pulse.share.state/command` | Windows transport: files under `%LOCALAPPDATA%\Pulse` plus named events (same pattern as `win_bridge.rs`), or the existing per-user named pipe. `H/share.rs` needs the libSystem externs gated | Planned | `H/share.rs`, `H/win_bridge.rs` |
| F6 | Notch Send cell, incoming request card, "Saved to Downloads" card, device list card, Paste button, file drop | `NearbySharing.swift` (card state machine over `DiskImagePrompt`), `SendCard.swift`; ⌘V while hovering the cell via a short-lived key tap; drop via panel drag destination | Sixth cell in `W/layout.rs`; cards through the existing card window; clipboard via `OpenClipboard` (`CF_HDROP`, `CF_DIB`, `CF_UNICODETEXT`); drop via OLE `IDropTarget` registered on the panel | Planned | none |
| F7 | CLI `pulse send <file...> --to <alias>`, `pulse send --list` | `C/send_cmd.rs`: discovery scan, subnet sweep fallback, progress to a terminal, device must accept | Same code; identity strings say "Mac" | In progress (shared code; cosmetic Mac strings) | `C/send_cmd.rs`, `C/main.rs` |
| F8 | Settings and permission row | Hub General "Nearby sharing": enable, device name, save folder, accept from known devices; Permissions "Local Network" (`NSLocalNetworkUsageDescription`, `NSBonjourServices`) | Same settings section; replace the Local Network row with a Windows Defender Firewall check (inbound TCP 53317, UDP 53317, private networks) | Planned | none |

## G. Mac conveniences, keyboard and screenshots

| ID | Feature | Mac implementation | Windows equivalent | Windows status | Windows files |
| --- | --- | --- | --- | --- | --- |
| G1 | Finder cut and paste | `S/Conveniences/FinderCutPaste.swift` on `EventTapHub`: ⌘X posts ⌘C and records pasteboard change count; ⌘V posts ⌥⌘V (Move Item Here) while the count is unchanged; skipped in Finder text fields (AX role check) | Native in Windows: Explorer cuts and pastes files with Ctrl+X / Ctrl+V | Not needed (native Explorer behaviour) | none |
| G2 | Finder right-click: Copy Path (also Cut, Open in Terminal) | `FX/FinderSync.swift` (`FIFinderSync`, sandboxed): menu items Cut, Copy Path (newline-joined POSIX paths, or the folder when clicking background), Open in Terminal; observes `/` and every mounted volume; Cut signals the app via Darwin notification `dev.orthic.pulse.finder.cut` | Native in Windows: "Copy as path" (Shift+right-click menu on Windows 10 and 11, Ctrl+Shift+C in Explorer; the plain Windows 11 menu varies by build, uncertain), "Open in Terminal" (Windows 11), Cut in the menu | Not needed (native; exact menu location varies by Windows build, uncertain) | none |
| G3 | Green button maximizes without full screen | `WindowMaximizer.swift`: swallows the click on an `AXZoomButton`, calls `WindowActions.toggleMaximize`; second click restores; Option-click keeps macOS behaviour | Native: the Windows maximize button fills the work area and never creates a full-screen Space | Not needed (native) | none |
| G4 | Window-management shortcuts (34 actions) | `S/Conveniences/Windows/` (`WindowAction`, `WindowActions`, `WindowGeometry`, `WindowHotKeys`, `WindowShortcut`): Carbon `RegisterEventHotKey`, Rectangle-style defaults (Control+Option+arrows etc.), Accessibility `AXSize/AXPosition`, per-action shortcut editor in hub, shared restore memory with G3 | Win+Arrow snap and Snap Layouts cover halves, quarters and maximize natively; thirds, fourths, nudge, next-display, almost-maximize have no native key. Optional port with `RegisterHotKey` plus `SetWindowPos`/`MonitorFromWindow`. Not in any plan; owner call | Not started (uncertain whether wanted) | none |
| G5 | Dock click minimizes frontmost app | `DockClickMinimize.swift`: on mouse down/up over an `AXDockItem` of the frontmost app, set `AXMinimized` on its visible windows; click never swallowed | Native: clicking the taskbar button of the active window minimizes it | Not needed (native taskbar behaviour) | none |
| G6 | Auto Quit on last window close | `AutoQuit.swift`: AX window-closed notifications, 1.5 s debounce, AX list empty AND `CGWindowList` has no normal window, `terminate()` only, never force; per-app opt-in; never Finder/Dock/Pulse | Windows apps normally exit with their last window; apps that stay resident do so on purpose. Plan: "Not needed" | Not needed (plan decision) | none |
| G7 | Fn works as Command (Fn+C/V/X/A/Z/S/F/T/W, Fn+arrows as Option+arrows) | `S/Keyboard/FnCommand.swift` on a HID-level `EventTapHub`; marker field on posted events; synthesizes a real Command/Option press for apps that ignore the flag (Flutter) | Windows keyboards do not expose Fn to the OS and Ctrl already sits in the bottom-left position | Not needed (hardware/OS difference) | none |
| G8 | Disk image installer | `S/Conveniences/DiskImageInstaller.swift`, `DiskImageWork.swift`, `S/Features/DiskImageCard.swift`: watches mounts, `hdiutil info -plist` to find the image, one `.app` verified by `codesign` strict + `spctl` (notarized Developer ID), staged copy then rename into `/Applications`, auto-install, auto-update (quit old copy, reopen), eject, optional Trash of the .dmg, Undo card | Windows installs via .exe/.msi/MSIX, not mounted images; an .iso mounts as a drive with no app-copy model | Not needed (no equivalent workflow) | none |
| G9 | Alt+A / Alt+C / Alt+V behave as Ctrl+A / Ctrl+C / Ctrl+V (match Cmd+A/C/V). Owner request | Cmd is the native modifier on macOS, so ⌘A/⌘C/⌘V need no code; Fn variants are G7 | Pulse-owned `WH_KEYBOARD_LL` hook that rewrites Left Alt + A/C/V into Ctrl + key with `SendInput` (design in the detailed section) | Planned | none (new `W/keymap.rs` proposed) |
| G10 | Alt+Shift+4 region screenshot (match Cmd+Shift+4). Owner request | macOS system feature (screencapture UI), no Pulse code | Pulse-owned overlay: `RegisterHotKey(MOD_ALT\|MOD_SHIFT, '4')`, dimmed full-virtual-screen layered overlay, crosshair and drag selection, capture with GDI `BitBlt`/`Windows.Graphics.Capture`, save PNG and copy to clipboard | Planned | none (new `W/capture.rs` proposed) |
| G11 | Alt+Shift+5 screenshot toolbar with Window, Full screen, Region options (match Cmd+Shift+5). Owner request | macOS system feature | Pulse-owned non-activating toolbar window drawn with the existing software canvas; buttons Capture entire screen, Capture selected window, Capture selected portion, Options, Close | Planned | none (new `W/capture.rs` proposed) |
| G12 | Hub toggles and status for conveniences | `ConveniencesService.stateSnapshot()` to `notch-state.json`; hub General "Conveniences", "Keyboard", "Window management" groups; all off by default | Toggles for G9-G11 in hub General (needs the Windows bridge, I3) | Planned (depends on I3) | none |

## H. Launcher

Mac: independent Swift launcher (`S/Launcher/`), off by default, panel is a non-activating `NSPanel` (`LauncherController.swift`), hotkey via Carbon (`LauncherHotKey.swift`). Windows plan (`docs/implementation-plan.md`, "Launchers"): do not embed; use Microsoft PowerToys Command Palette with a thin C# extension that calls the `pulse` CLI. No code exists for it.

| ID | Feature | Mac implementation | Windows equivalent | Windows status | Windows files |
| --- | --- | --- | --- | --- | --- |
| H1 | Launcher panel and hotkey | Option+Space (default), Command+Space, Control+Space (`LauncherHotkeyChoice`); Return runs, Escape closes, command output replaces the list (`LauncherView.swift`) | PowerToys Command Palette on Alt+Space (set in PowerToys; PowerToys Run disabled) | Planned (plan decision; no code) | none |
| H2 | Apps, running apps, pinned apps | `LauncherIndex.swift` (rescans on each open), `LauncherModel` sections Open, Pinned, Apps, Running; ⌘P pins; "Quit all running apps"; per-app global hotkeys (`LauncherConfig`) | Command Palette built-in apps and window switching | Planned (native in Command Palette) | none |
| H3 | File search | `LauncherFileSearch.swift`: Spotlight file-name query limited to hub-chosen folders, 2+ characters | Command Palette file search (Windows index) | Planned (native in Command Palette) | none |
| H4 | Calculator and conversions | `LauncherCalculator.swift` (own parser: + - * / ^ % parentheses functions), `LauncherConversion.swift` units offline, currency and crypto with `LauncherRates.swift` (open.er-api.com, CoinGecko, daily cache) | Command Palette calculator and unit extensions | Planned (native in Command Palette; rates uncertain) | none |
| H5 | Clipboard history | `LauncherClipboard.swift`: text and images, 500 items or 200 MB, concealed/transient copies skipped, off until enabled | Native Win+V clipboard history (plan decision) | Not needed (native) | none |
| H6 | Quicklinks, snippets, shell commands | `LauncherConfig` JSON edited in hub (`LauncherSettings.tsx`): `{query}`, `{clipboard}`, `{date}`, `{argument}` placeholders; commands run `/bin/zsh -lc` | Command Palette quicklinks; snippets and commands via extension | Planned | none |
| H7 | Apple Shortcuts and Dictionary | `launcherShortcuts` (Shortcuts app), `launcherDictionary` (macOS dictionaries, "define word") | No Windows equivalent of Apple Shortcuts or the system dictionary service | Not needed (macOS-only services) | none |
| H8 | Pulse commands (Open Storage / Cleanup / Monitor / Apps / Settings, Claude usage, Codex usage, Clear clipboard history) | `LauncherModel.commandList` calls `HubLauncher.open(section:)`, usage lines from live snapshots | C# extension that shells to `pulse` and to `pulse-hub --section` | Planned | none |

## I. App lifecycle, bridge, updater, permissions

| ID | Feature | Mac implementation | Windows equivalent | Windows status | Windows files |
| --- | --- | --- | --- | --- | --- |
| I1 | Open the hub from the notch on a section | `S/System/HubLauncher.swift`: hub runs as a child `Foundation.Process` of the notch (`Contents/MacOS/pulse-hub`) so the notch is the responsible process for TCC; running hub told by Darwin notification, new hub gets `--section` | `W/hub.rs`: spawns `pulse-hub.exe --section <name>` (searched next to the notch exe, `hub\`, `Helpers\`, `%LOCALAPPDATA%\Programs\Pulse`), `CREATE_NO_WINDOW`, tracks the child, signals named event `Local\dev.orthic.pulse.hub.show.<section>`, else raises the visible hub window, else restarts a hidden hub; child terminated on exit | In progress (written, uncompiled) | `W/hub.rs` |
| I2 | Hub single instance and show-section listener | `H/lib.rs` `watch_notch` (Darwin notifications `show-section`, `select-section`), hub hides to accessory on close | `H/win_bridge.rs`: mutex `Local\dev.orthic.pulse.hub.instance`, one manual-reset event per section (`hub.show.<s>`, `hub.select.<s>`), `WaitForMultipleObjects` thread emits Tauri events; second launch signals the first and exits unless `--background` | In progress (written, uncompiled) | `H/win_bridge.rs`, `H/lib.rs` |
| I3 | Notch to hub settings bridge (single settings writer) | `S/System/HubBridge.swift` publishes `notch-state.json` (settings, choices, displays, accounts, permissions, conveniences, updates, `system` readings) and applies `hub-commands/*.json` (atomic temp+rename, Darwin notify); `H/lib.rs` `notch_state`, `notch_command` | Equivalent state file and command folder under `%LOCALAPPDATA%\Pulse` with named events, or the per-user named pipe already built in `C/ipc/windows.rs`. Needs a Windows notch writer and hub `bridge_dir()` fix | Not started | none |
| I4 | Permissions page | `S/System/PulsePermissions.swift` rows: Background helper, Accessibility, Full Disk Access (probe in `H/permissions.rs`), Finder menu (extension enablement), Automation of Finder (no longer required), Launch at login; stale TCC cleanup (`tcc_stale_scan`, `tcc_reset` via `tccutil`) | No TCC. The plan hides Mac-only sections on Windows. Possible Windows rows: firewall for nearby sharing, "Run at login" | Not needed (hidden on Windows; plan decision) | none |
| I5 | Privileged helper for root-owned uninstall | `HLP/main.swift` (`PulseHelper` launch daemon, `SMAppService.daemon`), `pulse-elevate`, `TrashPolicy.swift` allow-list, hub General "Uninstall without password" | Windows uninstallers raise their own UAC prompt; plan: "no administrator helper in first version" | Not needed (plan decision) | none |
| I6 | Updater | `S/App/Updater.swift` + `S/Updater/`: GitHub releases feed, at most every 6 h when "Automatically check" is on, HTTPS-only download with size check, `UpdateVerifier` (Developer ID team and bundle id, notarized), replaces `/Applications/Pulse.app` and relaunches, rollback on failure; hub General Updates group | Per-user installer plus signed update check from the notch (plan). Design: WinHTTP GET GitHub latest release, download, `WinVerifyTrust` plus publisher check, helper process swaps files after the notch exits, restart | Not started | none |
| I7 | Release and packaging | `right-release.config.mjs` `mac` target: signed, notarized `Pulse.dmg` from RightKit CI | RightKit Windows target, Azure signing, per-user installer; none configured | Not started | none |
| I8 | State migration from the old product name | `S/App/ProductMigration.swift`, `C/state_migration.rs` (`migrate_mac_state`), exclusive rename | `C/state_migration.rs` has a Windows `rename_exclusive` and `legacy_metadata_dir` ("Cockpit"); hub only calls the Mac migration | In progress (shared code) | `C/state_migration.rs` |
| I9 | Diagnostics and logging | `S/App/Log.swift` (os.Logger) | `W/diag.rs` structured one-line `key=value` events to stderr (no console in release) | In progress (written, uncompiled) | `W/diag.rs` |
| I10 | Localization | 13 languages in `Localizable.xcstrings`, `L10n.t` | English only | Not started | none |

## J. Hub (shared Tauri window: `hub/src`, `hub/src-tauri`)

The React UI (`HU/`) is one code base for both OSes (`@rightkit/app-shell`). The Tauri backend is where Windows work is needed (see "Findings" item 1). Window: 900x600, minimum 820x520, hides instead of closing; sections in `HU/App.tsx`: Overview, Storage, Cleanup, Monitor, Apps, Settings (Permissions, Accounts, Appearance, Notifications, General).

| ID | Feature | Mac implementation | Windows equivalent | Windows status | Windows files |
| --- | --- | --- | --- | --- | --- |
| J1 | Hub shell: window, section routing, close-to-notch | `H/lib.rs`: `--section`, `show-section` event, `show_in_dock` while open (accessory otherwise), `CloseRequested` hides, `--background` for sharing | Same Tauri shell; Windows has no Dock concept (taskbar button while open is fine) | In progress (written, uncompiled; shell shared, see I2) | `H/lib.rs`, `H/win_bridge.rs` |
| J2 | Overview | `HU/views/Overview.tsx`: permission banner, hero headline (`chooseHeadline`), six cards Performance, Network and swap, AI usage, Storage, Cleanup, Apps; live status every 3 s, volumes every 30 s, saved cleanup and apps results only | Same page. Needs Windows backends for `status`, `volumes`, `cleanup_cached`, `apps_cached`, `apps_updates_cached` and notch state | Not started (UI shared; backend gaps) | `HU/views/Overview.tsx` |
| J3 | Storage: volume picker, rescan, free/used bar, FDA notice | `HU/views/Storage.tsx`: `volumes` (startup disk, externals, mounted disk images flagged, `H/lib.rs`), `scan`, `scan_status`, `last_scan`; "Open Full Disk Access" | `volumes` via `GetLogicalDrives` / core `win_native` `FindFirstVolumeW`; scan scope "Home folder" for system drive; no FDA concept | Not started | none |
| J4 | Storage: Findings | Safe/Review groups from cleanup rules for home only, "Clear all safe", per-group and per-item Move to Trash, notes (informational), Chrome snapshots line (`ChromeSnapshots.tsx`) | Same UI with a Windows rule pack (see L5) and Recycle Bin backend | Not started | none |
| J5 | Storage: Folders (drilldown, treemap, search, item actions) | `children`, squarified SVG treemap, percent bars and kind colours, file-name search via `search`; item menu: Reveal in Finder, Copy path, Copy name, Move to..., Move to Trash with Undo (`files.rs`: `file_identity`, `file_move_plan`, `file_move`, `file_trash`, `finder_open`, `file_choose_folder`) | "Reveal in Explorer" (`SHOpenFolderAndSelectItems`), folder picker (`IFileOpenDialog`), move via `MoveFileExW`/`IFileOperation`, trash via Recycle Bin (see L4); same-item identity re-check with NTFS file id | Not started | none |
| J6 | Storage: Changes | `growth` compares the newest home snapshot with the previous comparable one (`C/folder_growth.rs`); "Home changes" list with signed deltas, click opens the folder | Same; core store already has Windows paths | Not started (core In progress, shared code) | `C/folder_growth.rs`, `C/store.rs` |
| J7 | Storage: Duplicates | `HU/views/Duplicates.tsx` + `H/duplicates.rs`: pick folder, "Find copies", bounded exact-content compare (`C/duplicates.rs`), keeps one, Move extras to Trash | Core `find_duplicates` returns "unsupported by this native adapter" off Unix (`UnixContentReader` only); needs a Windows content reader (handle-relative `NtCreateFile`/`ReadFile`, skip reparse points and OneDrive placeholders) | Not started | `C/duplicates.rs` |
| J8 | Storage: Drive health panel | `HU/views/DriveHealth.tsx`, `H/health.rs`, `C/drive_health.rs`: smartctl per physical disk, 10 min sample, 90 days history, temperature, wear, written, power-on hours, critical warnings, media errors, self-tests, sparklines, alerts; "unavailable through this connection" keeps last good reading | smartctl.exe via `Helpers` path; map volume to physical drive with `IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS`; NVMe and SATA modes; `disk_of_mount` is Mac only today (returns `None`) | Not started | `C/drive_health.rs` |
| J9 | Storage: mounted installers with Eject | `volumes` flags `disk_image`; `eject` accepts only a disk image direct child of `/Volumes`, never forced | ISO/VHD mounts could be listed and ejected, low value | Not started (uncertain need) | none |
| J10 | Storage index and live refresh | `H/disk_index.rs`: `rightkit-fsindex` per-volume name index; `H/watch.rs` FSEvents refresh of changed folders; `H/scanner.rs` budgets (2M entries, 500k per directory), idle unload after 120 s | `disk_index` already has a non-macOS stub (search falls back to scan); live refresh needs `ReadDirectoryChangesW` or the USN journal; NTFS MFT enumeration is a possible index (undecided) | Not started | `H/disk_index.rs` (stub only) |
| J11 | Cleanup page | `HU/views/Cleanup.tsx`: Findings (Safe to regenerate, Review, "not offered" held list), About these numbers, Rescan, selection footer, Move to Trash confirm; History (moves, restore per activity, retry) | Same UI; backends per L4/L5 | Not started | none |
| J12 | Monitor page | `HU/views/Monitor.tsx`: memory-pressure banner, 30 min CPU / memory / swap / network charts (`metrics_history.rs`), collapsible Sensors (battery, fans, temperatures from notch state), process list search/sort (Memory, CPU, Name), Quit and confirmed Force Quit, protected-process list | Charts: core `sysinfo` already portable; pressure shows commit; sensors from Windows APIs (see B4); process Quit via `WM_CLOSE`, Force Quit via `TerminateProcess` (see L6) | Not started | none |
| J13 | Apps page | `HU/views/Apps.tsx` + `H/apps.rs` + `C/app_manager.rs`: inventory (`/Applications`, `~/Applications`), search, All / Unused 90+ days / Updates filters, Refresh, Check updates, detail with leftovers streamed by source and confidence, login and background items, installer receipts, uninstall to Trash with admin-password note, Update (Homebrew, Sparkle, App Store) | Registry uninstall keys, MSIX/AppX, winget; leftovers; see L7 | Not started | none |
| J14 | Settings: Permissions, Accounts, Appearance, Notifications | `HU/views/Settings.tsx` groups bound to notch state (see I3, E8, C, D) | Same pages; Mac-only groups hidden | Not started (needs I3) | none |
| J15 | Settings: General | Startup, Nearby sharing (`NearbySettings.tsx`), Updates, Uninstalling helper, Launcher (`LauncherSettings.tsx`), Keyboard (Fn), Readings, Conveniences, Window management, version line | Startup, Nearby sharing, Updates, plus Windows conveniences G9-G11; Mac-only groups hidden | Not started (needs I3) | none |

## K. Windows-only infrastructure (what `windows/` contains that has no Mac twin)

Everything here is "written, uncompiled". The Mac notch gets the same jobs from AppKit/SwiftUI/CoreGraphics.

| ID | Feature | Mac implementation | Windows implementation | Windows status | Windows files |
| --- | --- | --- | --- | --- | --- |
| K1 | Rendering | SwiftUI shapes in an `NSHostingView` (`NotchRootView`, `SideNotchShape`, `Palette`) | Pure-Rust software rasteriser: anti-aliased rounded rects, ring arcs, text masks composited to premultiplied `0xAARRGGBB`; top-down 32-bit DIB section; GDI text rasteriser to grey masks; `UpdateLayeredWindow` | In progress (written, uncompiled) | `W/canvas.rs`, `W/surface.rs`, `W/render.rs` |
| K2 | DPI and monitors | `NSScreen` frames in points | Per-monitor DPI awareness V2 at startup, metrics in DIPs scaled by monitor DPI (`W/layout.rs` `scale`, clamps 48-480 dpi), monitor key from `szDevice` (durable monitor identity not yet solved, per README) | In progress (written, uncompiled) | `W/main.rs`, `W/layout.rs`, `W/lifecycle.rs`, `W/runtime.rs` |
| K3 | HTTPS client | `URLSession` | Minimal WinHTTP GET (system proxy and certificate store, bearer tokens never logged, bounded response size) declared directly against `winhttp.dll` | In progress (written, uncompiled) | `W/http.rs` |
| K4 | JSON | `Codable` / `JSONSerialization` | Bounded recursive-descent readers (no serde) for credential files and provider responses; separate bounded settings parser | In progress (written, uncompiled) | `W/json.rs`, `W/settings.rs` |
| K5 | Resource safety | ARC | RAII owners for window classes, windows, DCs, bitmaps, handles; no `Drop` takes the app-state lock; locking rule documented in `W/main.rs` | In progress (written, uncompiled) | `W/raii.rs`, `W/main.rs` |
| K6 | Security of local files and singleton | Sandboxed Finder extension, helper allow-list | Current-user SID, restrictive SDDL for settings directory and files, mutex named by SID | In progress (written, uncompiled) | `W/runtime.rs`, `W/settings.rs` |
| K7 | Tests carried in the crate | `mac/Tests/PulseMacPrototypeCoreTests` | `#[cfg(test)]` cases for visibility rules and policy helpers (`W/visibility_cases.rs`, plus `#[cfg(test)]` modules in `W/diag.rs`, `W/main.rs`, `W/lifecycle.rs`, `W/runtime.rs`, `W/settings.rs`); CI runs `cargo test --locked` on the Windows runner (`scripts/gate.sh`). Per `AGENTS.md`, no new small tests are added; journeys are the target | In progress (existing component tests; no end-to-end Windows journey) | `W/visibility_cases.rs` |

## L. Core library and CLI (`core/`, binary `pulse`)

"Shared code" means the module is not `#[cfg(unix)]`/macOS-gated, so it is intended to compile on Windows; this report did not see a green Windows CI result.

| ID | Feature | Mac implementation | Windows equivalent | Windows status | Windows files |
| --- | --- | --- | --- | --- | --- |
| L1 | CLI surface | `C/main.rs`: `status`, `scan <path...>` (`--max-depth`, `--max-entries`, `--save`, `--state-dir`, `--exclude-state`), `findings`, `explain`, `history`, `procs [--sort cpu\|ram\|gpu] [--groups]`, `monitor`, `find`, `browse`, `export`, `duplicates`, `worker serve\|request`, `usage` (placeholder: always unavailable), `send`, `apps list\|updates\|detail\|uninstall`. `plan`, `apply`, `quit`, `force-quit`, `uninstall-plan` are disabled ("until feasibility and safety gates pass"); `--json` everywhere, human view in `C/presentation.rs` | Same binary; `apps` returns "not available on this platform yet" | In progress (shared code; `apps` Not started) | `C/main.rs`, `C/presentation.rs` |
| L2 | Scanner (metadata-only, bounded) | `C/scan.rs`, `C/platform/mac_bulk.rs` (`getattrlistbulk`), `mac_native.rs` (volume UUID, APFS private size `ATTR_CMNEXT_PRIVATESIZE`, dataless detection), `unix_native.rs`; entry budgets, never reads content, never hydrates placeholders | `C/platform/win_native.rs`: attribute-only handles, `FILE_FLAG_OPEN_REPARSE_POINT`, file id via `FileIdInfo`, allocation size via `FileStandardInfo`, `FileIdBothDirectoryInfo` enumeration, `FindFirstVolumeW` volume identity, offline/recall attributes marked as placeholders. APFS clone accounting has no NTFS counterpart (hard links via file id only) | In progress (shared code) | `C/platform/win_native.rs`, `C/scan.rs` |
| L3 | Snapshot store, history, growth | `C/store.rs` (pinned directory descriptor, `*at` operations), `C/history.rs`, `C/folder_growth.rs`; default dir `~/Library/Application Support/Pulse` | Windows branch pins the directory with a backup-semantics handle, handle-relative NT operations; default dir `%LOCALAPPDATA%\Pulse`; recent commits (`bea47f00`, `72e94f5a`) fix Windows CI fixtures | In progress (shared code; Windows CI being repaired) | `C/store.rs`, `C/history.rs`, `C/folder_growth.rs` |
| L4 | Cleanup engine and Trash executor | `C/cleanup.rs` (plans, journal, undo contracts, no FS code), `C/cleanup_scan.rs` (rule expansion, liveness by running process names, clone-aware size, `apply` with re-validation, activity log in `cleanup-activity.json`, `restore`), `C/activity.rs`; Trash move supplied by the hub (`H/cleanup.rs` via `trash::macos` Finder method) | Recycle Bin executor via `IFileOperation` with `FOFX_RECYCLEONDELETE` (the `trash` crate pinned at `=5.2.9` already supports Windows including restore listing); re-validate with NTFS file id | Not started (engine is shared code; executor missing) | `C/cleanup_scan.rs`, `H/cleanup.rs` |
| L5 | Cleanup rule data | `rules/cleanup.json`: 27 rules (App caches, Xcode DerivedData and Archives, device support, simulators info, npm, pnpm, Yarn, pip, Cargo, Go, Gradle, Homebrew, Chrome/Safari/Firefox/Edge/Brave caches, Logs, Installers, Old downloads, Chrome signing snapshots, `node_modules`, Rust `target`, Swift `.build`, Trash info); risk safe/review/info; liveness by owner process, name or age; `rules/initial.json`: 12 report-only rules (`C/rules.rs`) | New Windows rule pack (data only): `%TEMP%`, browser caches under `%LOCALAPPDATA%`, npm/pnpm/Cargo/pip/NuGet/Gradle caches, `node_modules`/`target` discovery, Downloads installers, WER reports; admin-only locations (Windows Update cache, Delivery Optimization) review-only or excluded. Donors named in `docs/plan.md`: FluentCleaner, Kudu, CleanmgrPlus | Not started (planned in `docs/plan.md` phase 6) | `rules/` |
| L6 | Process list and Quit / Force Quit | `C/processes.rs` pure grouping by PID + start time; `C/process_control.rs` (unix) lists by app, re-checks identity, polite Quit then separate Force Quit, never other users', system or Pulse itself | Listing: `C/processes.rs` and `sysinfo` are portable. Control: `WM_CLOSE` to the process's top-level windows and bounded wait, Force Quit via a process handle opened and verified by creation time, then `TerminateProcess` | In progress (listing, shared code); control Not started | `C/processes.rs`, `C/process_control.rs` |
| L7 | App inventory, leftovers, uninstall, updates | `C/app_manager.rs` (unix, 2.9k lines): `/Applications` inventory, leftover matching ported from Uninstally (bundle id, helper ids, app groups, team id, receipts, name review-only), login/background items, update sources (Homebrew cask upgrade, Sparkle, App Store), uninstall via Trash with re-validation; `C/apps.rs` models | Inventory from `HKLM/HKCU\...\Uninstall` (including WOW6432Node), MSIX/AppX, winget; run the registered `QuietUninstallString` or `UninstallString` (its own UAC), then leftover review under `%APPDATA%`, `%LOCALAPPDATA%`, `%PROGRAMDATA%`, Start Menu; reference: Bulk Crap Uninstaller | Not started (`C/apps.rs` models are portable) | `C/apps.rs`, `C/app_manager.rs` |
| L8 | System status and extended monitor | `C/lib.rs` `system_status` (`sysinfo`: CPU, memory, swap, disks) + macOS pressure (`sysctl`); `C/monitor.rs`: network counters, battery, rate sampler | Status via `sysinfo`; pressure label "Windows commit pressure" exists but `read_memory_pressure` returns `None` (not implemented); battery via `GetSystemPowerStatus` is implemented; network counters via `sysinfo`, but the virtual-interface filter lists Mac names (`lo`, `utun`, `awdl`...) so Windows virtual adapters are not excluded; listening ports "not wired" | In progress (shared code; gaps noted) | `C/lib.rs`, `C/monitor.rs` |
| L9 | Local IPC worker | `C/worker.rs` + `C/ipc/unix.rs`: read-only `status`, `processes`, `scan`, opt-in `pulse worker serve`, peer checked before reading a byte, 4-byte length framing | `C/ipc/windows.rs`: per-user named pipe, single instance, remote clients rejected, DACL for the current SID, client and server SID verified, overlapped I/O with bounded waits | In progress (shared code) | `C/ipc/windows.rs`, `C/worker.rs` |
| L10 | Drive health | `C/drive_health.rs` (smartctl JSON, history file, alerts) + `disk_of_mount` via `diskutil info -plist` | `disk_of_mount` returns `None` off macOS; needs volume-to-physical-drive mapping and a bundled smartctl.exe | Not started (module portable; mapping missing) | `C/drive_health.rs` |
| L11 | Duplicates | `C/duplicates.rs` `UnixContentReader`, bounded bytes, time and files | Windows branch returns an "unsupported" diagnostic | Not started | `C/duplicates.rs` |
| L12 | Compression contracts and dashboard export | `C/compression.rs` (contracts only, no encoder; no caller found in hub or CLI), `C/dashboard_export.rs` | Portable types | In progress (shared code; no consumer on either OS) | `C/compression.rs`, `C/dashboard_export.rs` |

# Detailed implementations

Each subsection gives the Mac implementation as read from source, then the Windows implementation (existing code, or the design where it is Planned or Not started). Designs are proposals until the owner accepts them; APIs named are real Win32/WinRT APIs but none of the new designs has been prototyped.

## A. Notch cells

### A1, A2 Claude and Codex cells
**Mac.** `ClaudeOAuthProvider` (one instance per `ClaudeProfile`) and `CodexLocalProvider` produce a `ProviderSnapshot` with `LimitWindow`s; `headlineID` names the main ring window and `weeklyID` the thin inner ring. `ProviderRing` strokes a grey track plus a clockwise arc from 12 o'clock; blocked windows draw as spent; a refresh in flight and stale status have their own marks. `AppDelegate.drawn` lays `WeeklyHeadline` and `DailyPace` over the vendor snapshots on the way out, so stored data stays what the vendor said.
**Windows (existing).** `W/layout.rs::views` builds `CellView {glyph, main, inner, stale, label}`: `main` = `usage.headline()`, `inner` = `usage.weekly()`. `W/render.rs` draws the notch body and rings through `W/canvas.rs` into a DIB and `UpdateLayeredWindow`. Glyphs are text marks (`Cl`, `Cx`), not the provider artwork (`S/Providers/ProviderGlyph.swift`, `GlyphOutline.swift`).
**Gaps.** Provider artwork, activity arc (A9), weekly options (A8), multiple profiles (A11).

### A3 System cell (memory pressure + CPU)
**Mac.** `SystemLoadProvider` samples `host_statistics(HOST_CPU_LOAD_INFO)`, busy = user + system + nice over total since the previous reading (first reading is the average since boot). `MemoryProvider.window()` uses `kern.memorystatus_level` for the arc and `kern.memorystatus_vm_pressure_level` (1, 2, 4) for the colour band via `bandOverride`, because macOS fills RAM with cache on purpose. Used bytes = app (internal minus purgeable) + wired + compressed, as Activity Monitor.
**Windows.** CPU: `GetSystemTimes` deltas (`W/lifecycle.rs::cpu_fraction`, kernel includes idle). Memory: `GlobalMemoryStatusEx` (`W/sensors.rs`), ring = physical in use over total; card adds available and commit used over commit limit. A failing counter is `None` and renders `--`. The Mac "pressure" idea has no Windows API; commit charge is the closest signal and is only shown in the card (this differs from the written plan, see Findings 3).

### A4 Disks cell
**Mac.** `DisksProvider.volumes()` filters `volumeIsLocal`, `volumeIsBrowsable`, not read-only, total over 0; free = `volumeAvailableCapacityForImportantUsage` (what Finder shows). Ring order: first external = main, startup/internal = inner. Device node from `statfs` feeds drive-health rows.
**Windows.** `W/sensors.rs` enumerates fixed drives (`GetLogicalDrives` or `GetLogicalDriveStringsW` with `GetDriveTypeW == DRIVE_FIXED`, free via `GetDiskFreeSpaceExW`), system drive singled out. Parity gap: removable/USB drives are skipped. Design: also include `DRIVE_REMOVABLE` volumes that are writable and mounted, treat the first non-system one as the main ring and the system drive as inner, matching the Mac ordering.

### A5 Send cell
See F6.

### A6, A7 Percent labels, colours
**Mac.** `UsageBand` (ample, watch, critical) with `watchLimit`/`criticalLimit` (hub sliders 0.1-0.95 and 0.15-1); `ColorTransitionStyle.ramp` blends continuously through the palette's yellow; `AccentColor` colours positive usage and activity.
**Windows.** `band_color` thresholds 0.70 and 0.90, colours `0x2E6B4A`, `0xC2570F`, `0xA51D24`. Design for parity: add the same keys to `pill-settings.json` (schema stays version 1 with optional fields, as `launch_at_login` was added) and read them in `layout.rs`; hub sliders arrive with I3.

### A8 Second-ring options
**Mac.** `WeeklyRing` off/inside/outside; `weeklyRingDashed`; `WeeklyHeadline.apply` swaps windows only when the second window runs about a week and is longer than the leading one; `DailyPace` synthesises a window `(days elapsed + 1) / 7` counted from the weekly reset; `UsagePace` expresses used quota minus elapsed time in points.
**Windows design.** Pure functions on the `Usage` windows in `W/usage.rs` (no Win32), applied in `layout::views`; port the arithmetic, not the UI.

### A9 Activity arc
**Mac.** `ClaudeSessionMonitor` watches `profile.sessionsDirectory` (files Claude Code writes, one per session, with pid), liveness via `ProcessLiveness`, ownership rules for multi-account (`ClaudeSessionOwnership`); `CodexActivityMonitor` for Codex; `ActivityCoordinator` merges into `AgentSession` (busy, blocked, waitingFor).
**Windows design.** Same directories under `%USERPROFILE%\.claude\sessions` (verify layout on Windows; uncertain), liveness by `OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION)` plus `GetProcessTimes` creation time (identity = pid + start time, as core does). Poll on the existing sampling timer.

### A10, A11
A10: stale and "Sign in needed" states already flow from `W/usage.rs::Status`. A11: provider list, order and visibility move into `pill-settings.json` and are edited in hub Accounts once I3 exists.

## B. Hover cards

### B1 Card behaviour
**Mac.** The panel ignores mouse events until the cursor is over it, so `NotchWindowController` polls the cursor (`startWatchingCursor`, `cursorMoved`) to open it; `hoveredIndex` picks the cell, a `hoverGrace` delay prevents flicker while crossing to the card, the card position comes from `tooltipRect(index:)` and moves with a spring.
**Windows (existing).** `WM_MOUSEMOVE` over a panel computes `layout::cell_at`, arms `WM_MOUSELEAVE` (`TrackMouseEvent`), `show_card` places the shared card window below the notch with `CARD_GAP` 6 DIP clamped by `clamp_x`; `refresh_card` redraws on each sample. No transition animation (cards appear and vanish).

### B2 Claude / Codex card
**Mac.** Rows per `LimitWindow` with label, percent, reset text (absolute or "in N min", `ElapsedCopy`, `ResetCopy`), group sub-rows, extra spend (`MoneyBreakdownView`), reset credits, blocked explanation, plan in the header.
**Windows (existing).** `W/card.rs::provider`: one `Row::Bar` per window, `Resets in <duration>` note, plan as header accessory, status note (`Updated <age>`, or status text plus last reading age). Missing: spend, credits, pace, blocked row.

### B3 Session rows
**Mac.** `SessionRow` shows each live session; a click calls `SessionFocus.focus(pid:)`: walk parent pids to the first process `NSRunningApplication` knows, raise it; `TerminalTabFocus` selects the tab for cmux (by cwd), Terminal.app and iTerm2 (by tty, AppleScript), Ghostty (by cwd).
**Windows design.** Walk parents with `CreateToolhelp32Snapshot` (`PROCESSENTRY32.th32ParentProcessID`) to the first process owning a visible top-level window (`EnumWindows` + `GetWindowThreadProcessId`); raise with `SetForegroundWindow` after the standard foreground-lock workaround (`AllowSetForegroundWindow` is not available to the notch, so use the Alt key tap trick or `AttachThreadInput`; uncertain which is reliable). Windows Terminal tab selection via UI Automation `SelectionItemPattern` on the tab whose title or cwd matches (uncertain; fall back to raising the window, as the Mac does for Warp).

### B4 System card extras
**Mac.** GPU: busiest `IOAccelerator` "Device Utilization %". CPU/SoC temperature: HID event system thermal sensors (private symbols resolved by name, as Stats). Network: `getifaddrs` link-level `if_data` counters on the primary IPv4 interface, kind Wi-Fi or Ethernet from SystemConfiguration. Fans: AppleSMC `FNum` and `F<n>Ac`. Battery: IOPowerSources plus `AppleSmartBattery` (cycles, health). Cadence: network and fans each 2 s sample, battery every 30 s, temperatures every 10 s (`SystemExtras`). Published to the hub as `system` in `notch-state.json`.
**Windows design.** GPU: PDH counter `\GPU Engine(*)\Utilization Percentage` summed per engine type (`PdhOpenQuery`, `PdhAddEnglishCounterW`, `PdhGetFormattedCounterArray`), the "Windows exposes public per-process GPU counters" note in the plan. Network: `GetIfTable2` (`MIB_IF_ROW2.InOctets/OutOctets`) of the interface owning the default route (`GetBestRoute2`), kind from `IfType` (`IF_TYPE_IEEE80211` vs Ethernet). Battery: `GetSystemPowerStatus` (already used in core `C/monitor.rs`); cycles and health need `IOCTL_BATTERY_QUERY_INFORMATION`. Temperatures and fans: no unprivileged public API for CPU die temperature or fan RPM on most PCs; `MSAcpi_ThermalZoneTemperature` over WMI is unreliable. Show the rows only when a source answers, like the Mac ("a reading is nil when the machine does not offer it"). Do not ship a kernel driver.

### B5, B6 Memory and Disks cards
B5 exists. B6 health rows follow J8: smartctl on a worker thread every 10 minutes, never on the 2 s timer, last good reading kept with date.

### B7 Send card
See F6.

## C. Notch window and behaviours

### C1, C2 Panel, no Dock/tray
**Mac.** `NotchPanel` (non-activating `NSPanel`, level above menu bar, joins all Spaces, not full-screen auxiliary). Accessory activation policy removes Dock tile and menu bar item. Quit exits notch and hub.
**Windows (existing).** Layered popup windows with `WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TOPMOST`; controller window is a hidden `WS_POPUP` so it receives `WM_DISPLAYCHANGE`. `WM_NCHITTEST` returns `HTCLIENT`. No `Shell_NotifyIcon` call exists. Quit path: `WM_CLOSE` to the controller, then `usage::stop`, `hub::terminate`, `persist_settings`.

### C3 Quit menu
**Mac.** `contextMenu()` returns an `NSMenu` with one item ("Quit Pulse"); `NotchPanel.sendEvent` handles the right click.
**Windows (existing).** Not a Win32 menu (it would take focus): a card window drawn by `render.rs`, polled every 80 ms (`MENU_POLL_MS`) so a click elsewhere dismisses it without activation.

### C4 Click mapping
**Mac.** See the table; a click on a cell opens its section, anywhere else on the open notch opens Overview, the click aimed at a pending peek focuses the session instead.
**Windows (existing).** `on_lbutton_down/up` records the cell, `Cell::section()` maps to the hub (`W/hub.rs::open`). Add `overview` for non-cell clicks and `general` for Send once F6 exists.

### C5 Settings handle and update dot
**Mac.** `SettingsHandle` is an arc tucked in the notch flare that becomes a gear on hover (`DiscToArc`), opens hub Settings, shows a red dot when an update is pending; `MoveHandle` six dots come out beside it.
**Windows design.** Smaller scope: a gear hot-zone at the notch's trailing end drawn by the canvas, click calls `hub::open("settings")`; dot driven by I6. Needed because there is currently no route into Settings (Findings 5).

### C6 Edges
**Mac.** `NotchEdge` is persisted; `NotchPlacement` hides axis differences (`along`, `across`), `NotchLayout` is 1-D; top and bottom stacks are deeper to fit labels.
**Windows design.** Today `lifecycle::notch_bounds` places a top-edge body only. Right edge (the Mac default) needs a vertical `layout.rs` and a rotated hit-test; left and bottom follow the same pattern. Panels are layered windows, so a change of edge is a new `SetWindowPos` and a re-render.

### C7 Fold, visibility modes, peek
**Mac.** Three modes (`NotchVisibility`). On hover, `setExpanded(true)` unfolds a pill into the full stack with `NotchMotion.unfold` (damped spring on an animatable `SideNotchShape`); folds after a pause unless Always show. `peek(for:)` opens for an event for 3/5/10 s.
**Windows design.** Render a pill state (small window) and swap to the full body on `WM_MOUSEMOVE` over the pill hot zone (a slightly larger invisible hit region, as the Mac does), animating via a 16 ms timer interpolating the DIB size (no `Animatable` equivalent exists; draw frames with the software canvas). Modes stored as an enum in settings.

### C9 Full-screen hide
**Mac.** `FullScreenDetector` finds layer-0 windows of the frontmost PID matching the screen bounds; `AXFullScreen` decides when readable; fallback to geometry only when the attribute is missing.
**Windows (existing).** `W/visibility.rs`: the topmost visible non-owned, non-tool, non-cloaked, non-shell window overlapping the notch's monitor decides (so a video on the notch's monitor stays covered even when another monitor has focus); it hides only if it covers the whole monitor and has no `WS_CAPTION` (`WS_BORDER | WS_DLGFRAME`) and no `WS_THICKFRAME`. Enumerated with `EnumWindows`, cloak via `DwmGetWindowAttribute(DWMWA_CLOAKED)`. Unknown attributes keep the panel visible. Open verification items (README): game/video/presentation matrix, mixed DPI, display reconnect.

### C10 Displays
**Mac.** One controller per display so hovering one never opens the others.
**Windows (existing).** `enumerate_monitors`, `desired_placements`, `plan_placements`, `apply_monitor_set` create or retire panels; hidden panels slow sampling. Monitor identity is `szDevice` (not durable across reconnects; recorded open item).

### C12 Alt-drag
**Mac.** Option-drag moves along the edge; offsets are remembered per edge; `CornerPassage` and `showOverlay` draw the notch going round a screen corner; hold-and-drag the six-dot grip does the same; hub has Reset position.
**Windows (existing).** `begin_drag/drag_to/finish_drag` while Alt is held (`GetKeyState(VK_MENU)`), position clamped on the monitor top edge, converted to per-mille (`along_for_left`), persisted per monitor key. Gap: reset action and grip; corner travel only makes sense after C6.

### C13-C16
Covered in the tables. Windows launch at login writes `HKCU\...\Run\Pulse` (`W/autostart.rs`, `RegSetValueExW`, rewritten only when it differs); settings written to a temp file then renamed with a restricted DACL.

### C17, C18
Cursor: use `SetCursor(LoadCursorW(IDC_HAND))` in `WM_SETCURSOR` for the clickable cells (uncertain whether the current build does). Accent and language: settings keys plus a string table (e.g. `.rc`-free Rust `match` tables), after the Mac catalogue in `Localizable.xcstrings` is exported.

## D. Alerts and notifications
**Mac.** `ThresholdNotifier`, `UsageResetWatcher`, `UsageLimitWatcher` are difference engines fed on every publication (skipping a reading would compare against stale state). Delivery is routed by `NotificationChannel`: notch card/peek (`announceThreshold`, `announceUsageReset`, `announceUsageLimit`) or a Notification Center banner via `ChannelNotifications`; hidden notch falls back to banner. `SessionChime` plays the sound file through `AVAudioPlayer` (not `NSSound`, which routes through the interface-sound-effects channel users often mute). Hub offers a test and three previews.
**Windows design.** Port the three watchers as pure state machines in a new module fed from `W/usage.rs` (no Win32); delivery = notch card (needs C8 peek and a card kind with title, detail, buttons: the existing card window is the base) with a toast fallback: WinRT `ToastNotificationManager` needs an AppUserModelID (a Start Menu shortcut carrying `System.AppUserModel.ID`, created by the installer); no tray icon. Sounds: `PlaySoundW` (`SND_FILENAME | SND_ASYNC | SND_NODEFAULT`) or `waveOut` on the default render device; system sound names map to `%WINDIR%\Media\*.wav`. Hub previews and tests travel over the I3 bridge.

## E. AI usage sources
**Mac.** `ClaudeOAuthProvider` order: (1) Claude Desktop HTTP cache (`ClaudeDesktopUsageCache`: reads only entries whose URL is this account's usage endpoint, vendored zstd decoder, no token or network), (2) `claude /usage` (`ClaudeUsageCLI`, throttled), (3) OAuth token from the keychain + `GET api.anthropic.com/api/oauth/usage`. `ClaudeTokenRefresher` runs `claude -p` with empty stdin to renew an aging keychain token and judges success by the expiry moving. `UsageStore` owns schedules (busy interval while an agent works, idle otherwise, one extra read when work finishes, a read when someone looks).
**Windows (existing).** `W/usage.rs`: reads `.credentials.json`/`auth.json` (64 KiB cap), keeps tokens in memory for one request, WinHTTP GET with 15 s timeout and 512 KiB response cap, shared bounded JSON parser, 300 s poll, back-off on 429 (60 s floor, doubling, 900 s ceiling), expired token shows "Sign in needed" with no loop; posts `WM_APP + 1` to the controller when a reading changed.
**Windows design for gaps.** E1: inspect Claude Desktop's Electron cache under `%APPDATA%\Claude` on a Windows machine first; reuse the Simple Cache reader as shared Rust (the Swift one is not reusable) only if the layout matches. E2: `CreateProcessW` with `CREATE_NO_WINDOW` for `claude "/usage"`. E3 refresher: on Windows Claude Code owns the file; measure whether it ages like the Mac keychain item before building a refresher.

## F. Nearby sharing (LocalSend)

### F1-F4 Protocol core
**Mac and shared.** `Service` announces, keeps the device list, serves HTTPS (rustls, self-signed certificate, SHA-256 fingerprint stored with `write_private`: mode 0600 on Unix), accepts or declines with the user, sends with `prepare-upload` then uploads. Multicast group `224.0.0.167` port 53317. Received names are sanitised and made unique; nothing is written before accept; text messages are returned as text, not files.
**Windows work items.** (a) Replace the `fill_random` stub with `BCryptGenRandom(NULL, buf, len, BCRYPT_USE_SYSTEM_PREFERRED_RNG)` (needs the `Win32_Security_Cryptography` feature of the `windows` crate already depended on). (b) `bind_shared`: Windows has no `SO_REUSEPORT`; `SO_REUSEADDR` lets several sockets share a UDP port and is only needed if another listener (a leftover LocalSend app) holds 53317. Since the owner wants no separate LocalSend app, keep plain bind but report a clear error if the port is taken. (c) Per-interface multicast: `IP_MULTICAST_IF` through `setsockopt` on Windows for each local IPv4 (the Unix `use_interface` is a no-op off Unix). (d) `write_private` writes with default ACLs off Unix; apply a current-user-only DACL (the pattern exists in `W/runtime.rs` and `C/store.rs` Windows branch). (e) Inbound firewall: the first listen triggers the Windows Defender Firewall prompt; Pulse should state this in Settings and not add rules silently (AGENTS.md: no cleanup or system changes without gates).

### F5 Hub service and notch IPC
**Mac.** `H/share.rs` hosts `Service` while the hub runs (hub started `--background` by the notch when sharing is on and the hub is not running, `NearbySharing.watch`). State flows by files plus Darwin notifications; the notch treats `share-state.json` older than 15 s as "hub not running".
**Windows design.** Same file contracts in `%LOCALAPPDATA%\Pulse`; replace Darwin notifications with named events created by the hub (`Local\dev.orthic.pulse.share.state`, `...share.command`), signalled with `SetEvent`, awaited by the notch on a worker thread feeding the controller with a `WM_APP` message, as `win_bridge.rs` already does for sections. Alternative without events: poll file mtime every 1 s while a transfer card is up.

### F6 Notch Send cell
**Mac.** `NearbySharing.providerSnapshot()` builds the cell (ring = active transfer progress, empty when idle; device rows; hint). Hovering turns on a key tap so ⌘V is taken only while the pointer is on the cell (needs Accessibility); a paste sends clipboard files, an image (saved to a temp PNG) or text; a drop on the cell sends files; with exactly one nearby device sends go straight, with several a device list card appears ("so nothing goes to a device by habit"). Incoming request, progress, "Saved to Downloads", text-with-Copy cards reuse `DiskImageCard`.
**Windows design.** Sixth `Cell::Send` in `W/layout.rs` (section `general`). Paste while hovering: instead of a hook, handle the Ctrl+V / Alt+V keystroke with a short-lived `RegisterHotKey` registered only while the pointer is over the cell (the same moment the Mac installs its tap); read the clipboard with `OpenClipboard` and `GetClipboardData` for `CF_HDROP` (files via `DragQueryFileW`), `CF_DIBV5`/`CF_DIB` (encode PNG with WIC to a temp file), `CF_UNICODETEXT`. Drop: `OleInitialize` on the UI thread, `RegisterDragDrop(hwnd, IDropTarget)` on each panel (works on `WS_EX_NOACTIVATE` windows; `DROPEFFECT_COPY`), `CF_HDROP` payload. Cards: extend the shared card window with button rows (Accept, Decline, Cancel, Copy, Open, Undo-like dismiss) and click regions; timeouts as on the Mac (8 s result cards held while hovered).

### F7, F8
CLI works unchanged after the random fix; change `identity()` model and fallback name to the OS (`std::env::consts::OS`). Settings keys `nearbyEnabled`, `nearbyAlias`, `nearbySaveFolder`, `nearbyAcceptKnown` map one to one. Save-folder default: `SHGetKnownFolderPath(FOLDERID_Downloads)` (a `FOLDERID` lookup is correct even when Downloads is redirected). Alias default: `GetComputerNameExW(ComputerNameDnsHostname)` (`sysinfo::System::host_name` is already used on the CLI path).

## G. Conveniences, keyboard and screenshots

### G1, G2, G3, G5, G6, G7, G8: why Windows needs nothing
- G1 Finder cut/paste: Explorer already moves files with Ctrl+X then Ctrl+V. Mac needs it because Finder's cut is Copy then Option-Command-V.
- G2 Copy Path: Explorer has "Copy as path" (Shift+right-click on Windows 10; context menu on Windows 11; Ctrl+Shift+C in Explorer).
- G3 maximizer: the Windows maximize button never enters a separate full-screen Space.
- G5 Dock click minimize: the taskbar already minimizes the active window on a second click.
- G6 Auto Quit: plan decision ("Not needed"). Mac design for reference: AX observer per opted-in app, 1.5 s debounce, two independent window checks, `terminate()` only.
- G7 Fn: the Fn key is handled in keyboard firmware on Windows PCs and is not delivered to the OS; the "bottom-left key" is Ctrl.
- G8 disk image installer: macOS-specific distribution format. Mac detail kept as reference: mount watcher, `hdiutil info -plist` to find the backing image, install only when `codesign --verify --strict` and `spctl` name a notarized Developer ID source, copy to a staging folder on the destination volume then atomic rename, old copy to Trash first and restored on failure, quit-then-reopen when updating, ejects with `hdiutil detach` (never forced, one retry), cancel until the commit point, Undo while the card is up.

### G4 Window management shortcuts
**Mac.** `WindowAction` enumerates 34 actions in seven groups with Rectangle-style defaults (Control+Option plus key); `WindowHotKeys` registers one Carbon hotkey per enabled action (`RegisterEventHotKey`, signature `PWM1`), shortcuts stored as text like `ctrl+opt+left`, hub records shortcuts and shows per-action failures; `WindowActions` computes frames in Accessibility coordinates against the screen's visible frame, remembers the previous frame (128 entries) so Restore and the maximizer share memory.
**Windows design (if wanted).** `RegisterHotKey` per action (Win key is reserved by the shell for Win+Arrow; use Ctrl+Alt as default modifiers), foreground window from `GetForegroundWindow`, target rect from `GetMonitorInfoW(...).rcWork` (work area, equals the Mac's visible frame), correct for invisible resize borders with `DwmGetWindowAttribute(DWMWA_EXTENDED_FRAME_BOUNDS)`, move with `SetWindowPos(SWP_NOACTIVATE | SWP_NOZORDER)`, restore a maximized window first with `ShowWindow(SW_RESTORE)`. Skip elevated targets unless Pulse is elevated (UIPI).

### G9 Alt+A / Alt+C / Alt+V as Ctrl+A / Ctrl+C / Ctrl+V (owner request)
**Mac.** Command is the native shortcut modifier; nothing to build. The Mac also remaps Fn (G7) so both thumb and bottom-left keys work.
**Windows design.**
- Mechanism: `SetWindowsHookExW(WH_KEYBOARD_LL, proc, GetModuleHandleW(None), 0)` on a dedicated thread with its own message loop (the notch's UI thread must not stall: a low-level hook must return within `LowLevelHooksTimeout`, 300 ms default, or Windows silently drops the hook callback). The callback only classifies and queues; the injection is a handful of `SendInput` calls.
- Match: `KBDLLHOOKSTRUCT.vkCode` in {`VK_A`, `VK_C`, `VK_V`}, Left Alt physically down (track `VK_LMENU` = 0xA4 from non-injected events; ignore `VK_RMENU`, so AltGr typing and the `LCtrl + RMenu` pair that AltGr generates are untouched), no Shift, no Win, no real Ctrl down. Skip events with `LLKHF_INJECTED` whose `dwExtraInfo` carries Pulse's marker (prevents loops), and pass everything else.
- Rewrite: swallow the letter key-down and its key-up (return 1, remember the swallowed key like `swallowedKeys` in `FinderCutPaste.swift`); inject, tagged with the marker: `LMENU` up, `LCONTROL` down, letter down, letter up, `LCONTROL` up, then `LMENU` down again only if Alt is still physically down. This way the target app never sees Ctrl+Alt+letter. To stop the foreground app reacting to a bare Alt release (menu bar focus), the sequence contains a key press between Alt down and Alt up, which Windows treats as "Alt was used"; if the menu still flashes, inject the same unassigned key (`VK_NONAME` 0xFC) pair that AutoHotkey-style tools use.
- Auto-repeat: key-down repeats while held; swallow repeats and re-inject a repeating Ctrl+letter.
- Limits (same as PowerToys Keyboard Manager, per `docs/implementation-plan.md`): does not see keystrokes going to elevated windows unless Pulse is elevated (UIPI), nothing on the secure desktop (UAC, Ctrl+Alt+Del, lock screen); Alt+letter chords that apps own (menu mnemonics, readline word moves) are lost for A, C, V only.
- Terminals: Ctrl+C is the interrupt in a console unless Windows Terminal has a selection. Recommended design choice (owner call, uncertain): when the foreground process is `cmd.exe`, `conhost.exe`, `WindowsTerminal.exe`, `powershell.exe` or `pwsh.exe`, send Ctrl+Shift+C / Ctrl+Shift+V for Alt+C / Alt+V so copy never sends an interrupt.
- Scope not requested: Alt+X, Alt+Z, Alt+S, Alt+F, Alt+T, Alt+W (the old plan listed them, matching the Mac Fn set). Adding them is the same table entry per key. Ask the owner before including X (cut) and Z (undo).
- Gating: default off, toggle in hub General, a self-test on enable (hook installed, thread alive), and a guard that pauses the hook while a full-screen app is foreground (reuse `W/visibility.rs`). Files: new `W/keymap.rs`; `Win32_UI_Input_KeyboardAndMouse` and `Win32_UI_WindowsAndMessaging` features are already enabled in `windows/Cargo.toml`.

### G10 Alt+Shift+4 region screenshot (owner request)
**Mac.** macOS's own Cmd+Shift+4 crosshair; Space toggles window capture; result is a PNG file on the Desktop (or the clipboard with Control). No Pulse code.
**Windows design.**
- Hotkey: `RegisterHotKey(hwnd, id, MOD_ALT | MOD_SHIFT | MOD_NOREPEAT, '4')` on the controller window (Alt+Shift alone toggles the input language only on release without another key; with a digit it should not fire; verify on a multi-layout machine). Fails with `ERROR_HOTKEY_ALREADY_REGISTERED` if another app owns it: report in hub Settings, never override. Does not touch the native Win+Shift+S snipping tool.
- Overlay: one layered topmost window spanning the virtual screen (`GetSystemMetrics(SM_XVIRTUALSCREEN...)`), per-monitor DPI v2, drawn with `W/canvas.rs` (dim layer, cut-out rectangle, crosshair, pixel size badge). `SetCapture` for the drag; Esc cancels; Space toggles to window mode (highlight the window under the cursor via `WindowFromPoint` then top-level via `GetAncestor(GA_ROOT)`, bounds by `DwmGetWindowAttribute(DWMWA_EXTENDED_FRAME_BOUNDS)` to exclude shadows).
- Capture: hide the overlay, `DwmFlush`, then `BitBlt(desktopDC, ..., SRCCOPY | CAPTUREBLT)` from `GetDC(NULL)` for the rectangle (simple, works on every supported version); `Windows.Graphics.Capture` (`IGraphicsCaptureItemInterop::CreateForMonitor` / `CreateForWindow`, `Direct3D11CaptureFramePool::CreateFreeThreaded`, copy the frame to a staging `ID3D11Texture2D`) is the better path for hardware-accelerated surfaces and DRM-excluded windows (returns black), needs Windows 10 1903+; yellow border removed with `GraphicsCaptureSession::IsBorderRequired(false)` where available (Windows 11; uncertain which builds).
- Output: PNG through WIC (`IWICImagingFactory`, `GUID_ContainerFormatPng`) to the Desktop by default (`FOLDERID_Desktop`, name `Screenshot YYYY-MM-DD at HH.MM.SS.png`), configurable; also place the image on the clipboard (`CF_DIB`, plus the registered `PNG` format) as the option the Mac offers with Control. A small result card in the notch with Show in Explorer (`SHOpenFolderAndSelectItems`).
- Privacy: nothing leaves the machine; no screen content logged.

### G11 Alt+Shift+5 screenshot toolbar (owner request)
**Mac.** macOS's Cmd+Shift+5 toolbar: Capture Entire Screen, Capture Selected Window, Capture Selected Portion, two record buttons, Options (save location, timer, floating thumbnail, remember last selection, show pointer), Capture, Close.
**Windows design.** Same hotkey mechanism as G10 with `'5'`. A non-activating layered toolbar (same window recipe as the Quit menu) centred near the bottom of the monitor under the cursor, drawn by the software canvas with hit regions for: Full screen (captures the monitor under the cursor), Window (enters the window-pick mode of G10, then captures that window), Region (enters the G10 overlay), Options (popup card: save to Desktop / Pictures / clipboard, timer off / 5 s / 10 s via `SetTimer`, show pointer via drawing `GetCursorInfo` + `DrawIconEx` into the bitmap), Close. Recording buttons are not part of the request and are omitted. Remember the last mode in settings. Esc closes. The toolbar shares capture code with G10 (`W/capture.rs`).

### G12 Hub toggles
Mac: `ConveniencesService` publishes `accessibility`, `inputMonitoring`, `wanted`, `fnStatus`, `active`, running apps, Auto Quit list, window-management state; hub shows each with status. Windows: keys `convAltClipboardKeys`, `convRegionShot`, `convShotToolbar` (names are proposals), statuses "hook running", "hotkey in use by another app", "needs elevation for elevated windows". Needs I3.

## H. Launcher
**Mac.** `LauncherController` owns two Carbon hotkeys (launcher and per-app), a keyboard-capable non-activating panel, and `LauncherModel` which builds sections: `answer` (calculator or conversion), `open` (URL or path typed raw), `pinned`, `apps`, `running`, `quicklinks`, `snippets`, `commands`, `shortcuts`, `clipboard`, `dictionary`, `files`, `pulse`. The index loads on the first hotkey press (not at login) and rescans on each open. The calculator is a hand-written parser so typed text is never given to an evaluator. Conversions: units offline, currency and crypto from a daily cache (`open.er-api.com`, CoinGecko), failures ignored. Clipboard history stores text and images under Application Support, 500 items or 200 MB, skips concealed or transient pasteboard types. Quicklinks fill `{query}`, `{clipboard}`, `{date}`; snippets paste Markdown text into the previous app with `{date}`, `{clipboard}`, `{argument}`; commands run through `/bin/zsh -lc`.
**Windows.** Plan: PowerToys Command Palette extension (.NET SDK) written as a thin C# wrapper over the `pulse` CLI JSON; Alt+Space set in PowerToys; Win+V for clipboard; PowerToys Run disabled to avoid two handlers on one key. No code exists. If the owner prefers an in-notch launcher later, the pieces are the hotkey (`RegisterHotKey`), a keyboard-capable popup (`WS_EX_NOACTIVATE` removed, so it would take focus), start-menu enumeration (`FOLDERID_Programs`, `.lnk` resolution with `IShellLinkW`) and the calculator/conversion Rust ports, but that is a different scope from the current plan.

## I. App lifecycle, bridge, updater

### I1, I2 Hub launch and section routing
**Mac.** The hub is a child of the notch so macOS attributes Full Disk Access to Pulse's single entry. An already-running hub gets `show-section` or `select-section` by Darwin notification; a new hub gets `--section`.
**Windows (existing).** `W/hub.rs` locates `pulse-hub.exe`, spawns with `--section`, signals `Local\dev.orthic.pulse.hub.show.<section>` through `OpenEventW`/`SetEvent`, falls back to `EnumWindows` + raise, or kills and restarts a hidden hub. `H/win_bridge.rs` claims the instance mutex, creates one manual-reset event per section for `show` and `select`, and a thread waits on up to 64 handles (`WaitForMultipleObjects`) and emits `show-section` to the page. Declared kernel32 symbols avoid a second `windows` crate version beside Tauri's.

### I3 Bridge for settings
**Mac.** The notch is the only writer. It publishes `notch-state.json` after any change and the hub drops commands (`set`, `connect`, `order`, `signIn`, `refresh`, `resetPosition`, previews, `checkUpdates`, `installUpdate`, `permissionRequest`, `helperEnable`) into `hub-commands/` which the notch applies and deletes.
**Windows design.** Keep the contract (JSON files, atomic rename). Directory `%LOCALAPPDATA%\Pulse` with the existing DACL helpers; notifications through named events (`...notch.state`, `...hub.command`). Hub `bridge_dir()` and `home()` use `HOME` today; replace with `pulse_core::store::default_directory()` which already handles `LOCALAPPDATA`. The Windows notch must grow a settings model beyond `PillSettings` (all the A, C, D keys above) before the hub pages can drive it.

### I4, I5 Permissions and helper
**Mac.** `PulsePermissions` probes silently (never prompts) and lists Accessibility (needed when any key convenience is on), Full Disk Access (reads a protected file), Finder menu extension, Launch at login, helper (`SMAppService.daemon`). `PulseHelper` moves root-owned items to the user's Trash through an XPC allow-list (`TrashPolicy`) and hands ownership back so emptying Trash needs no password; `pulse-elevate` is the client.
**Windows.** No equivalent gates. A uninstall that needs elevation is the uninstaller's own UAC prompt; Pulse itself stays unelevated. If Alt-key hooks must reach elevated windows later, that requires a separate decision about running elevated, which the plan rejects for v1.

### I6, I7 Updater and release
**Mac.** `Updater` reads the GitHub latest release, offers it in the notch, on Update downloads (HTTPS only, redirects too, 200 and advertised size) into a private directory, mounts the DMG read-only without Finder, copies the app out, verifies the copy (Apple-anchored Developer ID signature from this bundle id and team, notarization), swaps `/Applications/Pulse.app` and relaunches, with rollback if the installed copy fails the second check.
**Windows design.** Same feed and 6-hour rule. Installer is a per-user package under `%LOCALAPPDATA%\Programs\Pulse` (the path `W/hub.rs` already searches). Download with WinHTTP (`W/http.rs` would need a streaming variant and redirect policy), verify Authenticode with `WinVerifyTrust(WINTRUST_ACTION_GENERIC_VERIFY_V2)` and check the signer subject against the Azure-signed publisher, then a small updater helper replaces files after the notch and hub exit (a running exe cannot be overwritten) and relaunches. RightKit `right-release` needs a `windows` target in `right-release.config.mjs`.

### I8-I10
Migration: `C/state_migration.rs` already has `rename_exclusive` for Windows. Diagnostics: `W/diag.rs`. Localization: see C18.

## J. Hub pages

### J1 Shell
`H/lib.rs` keeps the hub alive for nearby sharing after the window closes (`prevent_close`, hide). `--background` starts without a window.

### J2 Overview
**Mac.** Reads `status` (CPU, memory, swap, pressure) every 3 s, `volumes` every 30 s, and only saved results for Cleanup (`cleanup_cached`), Apps (`apps_cached`) and updates (`apps_updates_cached`); never scans on open. `chooseHeadline` picks one statement by severity (bad, warn, ok) from memory level, CPU level, fullest drive, swap, eligible cleanup bytes, pending app updates. A banner appears when required permissions are missing.
**Windows.** Same React page unchanged; blockers are backend commands (J3, J4, L7) and `notch_state` (I3). Until I3 exists the page's `notchDown` branch shows the notch-unavailable state.

### J3-J6 Storage
**Mac.** `volumes`: mounted user-visible volumes from `/Volumes` (startup disk named as Finder shows it; disk images flagged `disk_image` via `hdiutil`), each with total/available/removable/internal. `scan(mount)` starts at the home folder for the startup disk, otherwise the volume root; budgets 2M entries, 500k per directory, files under 256 KiB only counted into their folder, top 200 children per folder, iCloud placeholders never reported; a partial scan without Full Disk Access shows an Open Full Disk Access button. `children(path)` serves drilldown from the in-memory index (unloaded after 120 s idle). `search` uses `rightkit-fsindex` names with fallback to the scan rows. Live refresh by FSEvents. Findings use `cleanup_scan` for home only, grouped by category with Safe/Review badges, partial sizes marked with ≥, notes for informational rules. Changes = `compare_folders` on the newest two comparable home snapshots.
**Windows design.** Volumes: `GetLogicalDriveStringsW` + `GetVolumeInformationW`/`GetDiskFreeSpaceExW`, label via `GetVolumeInformationW`, removable via `GetDriveTypeW`; startup drive scan scope = `%USERPROFILE%`. Scanner: core `win_native` (see L2). Findings rule pack: L5. Item actions: Explorer reveal `SHOpenFolderAndSelectItems`; Move via `MoveFileExW` without `MOVEFILE_REPLACE_EXISTING` (the Mac `rename_no_replace` refuses replacement), cross-volume copy-then-recycle exactly like `files.rs::copy_then_trash`; Trash via Recycle Bin. NTFS junctions and symlinks reported as links, never followed (already the `win_native` contract). OneDrive placeholders (`FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS`, `..._ON_OPEN`, `OFFLINE`) never opened.

### J7 Duplicates
**Mac.** Candidate selection is metadata only; contents are read only on explicit "Find copies", bounded by file count, bytes read and time; groups keep one path and offer Move extras to Trash with the kept copy untouched; links and placeholders skipped.
**Windows design.** Provide a `ContentReader` for Windows with `CreateFileW(FILE_FLAG_SEQUENTIAL_SCAN, FILE_SHARE_READ)` after confirming the item is not a reparse point or placeholder; file identity from `FILE_ID_INFO` so hard links are not reported as duplicates of themselves.

### J8 Drive health
**Mac.** `smartctl -a -j` per whole disk (APFS physical store resolved through `diskutil info -plist`), at most every 10 minutes, readings kept 90 days with time, alerts on Warning, wear up 5 points or more, or new media errors; unreachable connections (USB NVMe on macOS has no passthrough) keep the last good reading and date. Checked: smartctl 7.5 reads the internal NVMe, fails on an external USB disk.
**Windows design.** `smartctl.exe` bundled (RightKit signed smartmontools 7.5 build noted in `docs/plan.md`), device name `/dev/sdX` or `/dev/nvmeX`: obtain the physical drive number from `DeviceIoControl(IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS)` on `\\.\C:` and map to smartctl's `/dev/pdN` scheme (uncertain, confirm with `smartctl --scan`). USB bridges often need `-d sat`; keep the "unavailable through this connection" state.

### J10 Search index and watch
**Mac.** `rightkit-fsindex` builds a per-volume background name index (content search compiled out), FSEvents drives live folder refresh. Crawl time and memory on real volumes are not yet measured (`docs/plan.md`).
**Windows design.** Name search falls back to scan rows today. Options for an index (undecided): NTFS MFT read via `FSCTL_ENUM_USN_DATA` (needs volume-handle access; usually elevated, so unsuitable unprivileged), or a per-folder `FindFirstFileExW(FIND_FIRST_EX_LARGE_FETCH)` crawl; live refresh via `ReadDirectoryChangesW`.

### J11-J12 Cleanup and Monitor
**Mac.** Cleanup: `cleanup_scan::scan` expands rule patterns, measures, decides eligibility (unknown owner or running owner means held, not offered); `apply` re-checks each item right before the move and records an activity entry (`cleanup-activity.json`) including Trash paths so `restore` can put items back; the Trash move says "Moved to Trash", never "freed". Monitor: charts from `metrics_history.rs` (30 minutes, 2 s samples, kept filling while the window is closed), process rows from `process_control::process_rows` grouped by app, Quit and a separately confirmed Force Quit, protected names (`kernel_task`, `windowserver`, `launchd`, `loginwindow`) and Pulse itself never actionable.
**Windows design.** Restore path in `cleanup_scan` uses `home.join(".Trash")` today; the Windows executor must return the Recycle Bin item identity (`SHFileOperation`/`IFileOperation` do not return it directly; use the `trash` crate `os_limited::list` after the move, matched by original path and deletion time) so Restore is reliable. Protected process names become `System`, `smss.exe`, `csrss.exe`, `wininit.exe`, `winlogon.exe`, `services.exe`, `lsass.exe`, `dwm.exe`.

### J13 Apps
**Mac.** `app_manager`: inventory of app bundles with size, running state, last-used (an unknown last use never counts as unused), protected flag; leftovers streamed by source with a confidence label (Exact id, Helper id, App group, Id prefix, Team id, Name, Receipt); only exact and helper matches preselected; Uninstall quits politely first, re-validates every item, moves to Trash (Finder asks for a password on root-owned items, or the helper does it silently); updates: Homebrew upgrade, Sparkle app opened to run its own updater, App Store app opened in the store.
**Windows design.** See L7. Confidence labels map to: exact Uninstall key id, publisher and install location, MSIX package family name, name-only review. Update sources: winget (`winget upgrade --id`), Microsoft Store (`ms-windows-store://`), vendor updaters opened as-is.

### J14, J15 Settings
All groups listed in `HU/views/Settings.tsx` are bound to `NotchState`; the Windows hub must hide the Mac-only groups (permissions rows beyond launch at login, helper, Fn, Finder menu, window management, disk image options, launcher until Command Palette exists) and show Windows ones (G9-G11, nearby, updates).

## K, L. Windows infrastructure and core
K rows are described in their table (single sentence each). L rows: the Windows work is concentrated in four places: the Recycle Bin executor and rule pack (L4, L5), process control (L6), app manager (L7) and drive health mapping (L10). Everything else in core is shared code awaiting a verified Windows CI run.

