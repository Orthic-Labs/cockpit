# Pulse: Mac ↔ Windows source parity review

Reviewed 2026-10-10 at `feae1650`, including requested Windows commits `38caa146`, `e4a78fe8`, `7d80a0fa`, `0cf561e0` & Mac/bridge commits `5d981c33`, `5f4c0531`, `01c57690`. Inputs: `AGENTS.md`, `docs/plan.md`, `docs/parity.md`, `docs/parity-audit-2026-10-10.md`, latest 25 commits.

Source review only; no builds, tests, app launches or outbound actions. “Identical” below means matching source-level behavior, not observed runtime equivalence. “Absent” means no corresponding implementation in reviewed dispatch/render paths. Existing audit gaps stay in coverage/disagreements; §1 isolates additional mechanisms or journeys it missed.

Citation prefixes: **M/** = `mac/Notch/Sources/`; **W/** = `windows/src/`; **H/** = `hub/src/`; **HT/** = `hub/src-tauri/src/`; **C/** = `core/src/`. Every `prefix/file:line` resolves relative to repository root. **I** identical contract; **D** different; **MISSING** missing behavior; **N/A** platform-specific mechanism. Numbered N references point to evidence & fixes in §1.

## 1. Additional gaps

### N01 — Claude bridge replies have no Windows listener [high, MISSING]
- Mac: `C/bridge/deliver_claude.rs:599` opens per-peer Unix reply listeners; `C/bridge/hub.rs:140` registers Pulse as Claude peer. Windows: listener returns `None` at `C/bridge/deliver_claude.rs:612`; registration is empty at `C/bridge/hub.rs:170`; outgoing frames advertise `pulse-bridge-noreply` at `C/bridge/deliver_claude.rs:406`.
- Impact: successful incoming delivery does not establish native reply routing; chat must explicitly run `pulse bridge send`. This is narrower than “bridge works” from roster/status queries. Smallest fix: implement authenticated named-pipe reply listener & matching Windows peer registration; retain explicit-send fallback.

### N02 — Turning Nearby off also removes hub supervision [high, D]
- Mac: `M/System/HubLauncher.swift:149` supervises hub independently of sharing. Windows: `W/send.rs:900` returns immediately when sharing is disabled; only that watcher respawns its hub child at `W/send.rs:917`.
- Impact: with Nearby off, background hub recovery disappears, including bridge service hosted there (`HT/share.rs:489`). Smallest fix: move hub ownership/restart supervision outside Nearby, keeping sharing enablement separate.

### N03 — “Refresh now” never refreshes Windows provider usage [high, D]
- Mac: `M/App/AppDelegate.swift:449` routes refresh to `.fromSource`; look/work-completion triggers exist at `M/App/AppDelegate.swift:512` & `M/App/AppDelegate.swift:758`. Windows: `W/bridge.rs:446` only repaints; polling waits five minutes through `W/usage.rs:337`; explicit wake at `W/usage.rs:349` is Claude-only.
- Impact: `H/views/Settings.tsx:455` offers working-looking refresh while keeping old Claude/Codex readings. Hover also lacks Mac's spaced refresh (`M/Model/UsageStore.swift:515`). Smallest fix: add both-provider refresh request, wire hub action first, then spaced attention/work triggers respecting backoff.

### N04 — Three settings report success without taking effect until restart [high, D]
- Mac: conveniences reconcile preference changes (`M/Conveniences/ConveniencesService.swift:44`); updater reads current preference (`M/App/Updater.swift:93`). Windows: `W/bridge.rs:484` explicitly defers `mac_shortcuts`, `screenshot_shortcuts`, `autoUpdateCheck` until next launch; keyboard workers capture flags at `W/keys.rs:120`, updater at `W/update.rs:279`.
- Impact: turning Alt remapping/screenshots off leaves interception active; disabling update checks leaves worker enabled. Hub gives no restart instruction (`H/views/Settings.tsx:343`, `H/views/Settings.tsx:355`). Smallest fix: live atomic setters/reconcile hook ownership & updater flag; publish effective state.

### N05 — Update check/install feedback is disconnected from hub [high, MISSING]
- Mac: `M/System/HubBridge.swift:223` publishes updater snapshot; install command handled at `M/System/HubBridge.swift:308`; `M/App/Updater.swift:149` models available/error states. Windows: `W/bridge.rs:387` publishes only current version & auto-check; `installUpdate` falls through `W/bridge.rs:460`; checks log up-to-date/failure only (`W/update.rs:357`), install failure clears prompt (`W/update.rs:463`).
- Impact: hub's status/version/install UI (`H/views/Settings.tsx:355`) cannot complete Windows journey; failed installs disappear without actionable feedback. Smallest fix: publish full updater state, handle install command, retain failed phase with retry/error text.

### N06 — Badge drawings exist, live badge state does not [high, MISSING]
- Mac: `M/Notch/NotchFleet.swift:77` & `M/Notch/NotchFleet.swift:85` propagate permission/update badges; permission click routes to Permissions (`M/Notch/NotchWindowController.swift:1663`). Windows: `W/main.rs:235` initializes both false; subsequent badge usage only passes them to drawing (`W/main.rs:957`); `W/render.rs:294` can draw dots but has no producer.
- Impact: pending updates/access needs never light live notch, despite fixture coverage. Smallest fix: derive badges from live updater/permission state & route permission-badge clicks appropriately.

### N07 — Card controls lack pressed state & press-origin tracking [high, D]
- Mac: `M/DesignSystem/CardButtonStyle.swift:31` uses SwiftUI `isPressed`, disabled state, brightness & scale. Windows: `W/main.rs:2503` has no card `WM_LBUTTONDOWN`; release dispatches current hit at `W/main.rs:2554`; renderer takes hover only (`W/render.rs:2089`). Quit executes on any release inside its window (`W/main.rs:2555`). Screenshot toolbar repeats release-only dispatch (`W/shot.rs:1519`).
- Impact: no pressed feedback or explicit same-control down/up contract for accept, cancel, install, restart, close & Quit. Moving between controls while held can execute release target. Smallest fix: store pressed hit, capture mouse, invalidate on capture loss/disable, execute only matching release; feed pressed state into renderer.

### N08 — New hover renderer skips several real Send controls [medium, D]
- Mac: Copy last/Paste use card button style (`M/Features/TooltipCard.swift:488`, `M/Features/TooltipCard.swift:506`); devices use row hover (`M/Features/TooltipCard.swift:541`). Windows: Copy last/Cancel/Paste are `Row::Button` (`W/send.rs:1511`, `W/send.rs:1565`, `W/send.rs:1623`); hover devices are actionable `Row::Pair` (`W/send.rs:1609`). `W/render.rs:2131` highlights only headers, `Row::Buttons` & `Row::Device`.
- Impact: these live controls get hand cursor/action but no hover fill, even though chooser device rows do. Smallest fix: render hover from actionable hit geometry, covering both single-button & actionable pair rows.

### N09 — Pointer cannot remain over Codex/System/Disks cards [medium, D]
- Mac: every active tooltip belongs to interactive region & keeps current hover target (`M/Notch/NotchWindowController.swift:1400`, `M/Notch/NotchWindowController.swift:1505`). Windows: only Send/Claude/notices/menu accept pointer (`W/main.rs:2510`); leaving other cells clears hover immediately (`W/main.rs:1880`).
- Impact: trying to move onto those readings dismisses card instead of preserving reading area. Smallest fix: include all shown tooltip rectangles in hover grace/pointer ownership, without making informational rows actionable.

### N10 — No restoration of last provider reading after Windows restart [medium, MISSING]
- Mac: archive restores rings as explicitly stale (`M/Model/UsageStore.swift:273`). Windows: runtime usage starts as two empty `Usage::waiting()` values (`W/usage.rs:209`); failure can preserve only current-process previous readings (`W/usage.rs:494`). Claude account-book persistence is separate from this runtime snapshot.
- Impact: restarting offline loses useful stale ring/card readings, especially Codex. Smallest fix: persist account-bound successful snapshots & restore with age/stale state; invalidate on identity change or expired windows.

### N11 — Same-account Claude cache is bypassed during rate limiting [high, D]
- Mac: Desktop cache is tried ahead of endpoint/backoff (`M/Providers/ClaudeOAuthProvider.swift:259`). Windows: same Desktop/CLI identity reaches backoff return before cache (`W/usage.rs:719`), then endpoint first; cache fallback excludes `RateLimited` (`W/usage.rs:724`). Different-account Desktop path does use cache (`W/usage.rs:714`).
- Impact: valid local reading cannot rescue same-account 429; repeated network requests precede available local data. Smallest fix: move identity-validated cache lookup ahead of endpoint/backoff for both identity branches.

### N12 — Retry-After information is discarded [medium, D]
- Mac: Claude parses seconds/date header (`M/Providers/ClaudeOAuthProvider.swift:662`); Codex saves returned backoff (`M/Providers/CodexLocalProvider.swift:63`). Windows: HTTP response contains only status/body (`W/http.rs:79`); usage calculates fixed exponential delay (`W/usage.rs:487`).
- Impact: server-requested wait cannot influence Windows retry timing, & restart loses worker backoff. Smallest fix: carry Retry-After through HTTP result, apply bounded server delay & persist identity-bound deadline.

### N13 — Limit duration disappears between Windows notch, hub & CLI [medium, D]
- Mac: exports `seconds` (`M/System/HubBridge.swift:185`). Windows: exports label/fraction only (`W/bridge.rs:332`), despite shared consumers selecting 5-hour/week by seconds (`H/views/Overview.tsx:522`) & emitting CLI `window_seconds` (`C/usage_snapshot.rs:105`).
- Impact: Windows Overview falls back to first two limits; CLI consumers lose duration metadata. Smallest fix: retain duration in Windows limit model & serialize `seconds`; do not infer grouped limits from localized label text.

### N14 — Windows CLI apps detail/uninstall stops before hub-equivalent journey [medium, MISSING]
- Mac: `apps detail` returns real related items (`C/main.rs:1270`); `apps uninstall` validates selection & invokes app manager (`C/main.rs:1281`). Windows: detail supplies empty items/background/receipts (`C/main.rs:1203`); uninstall explicitly rejects (`C/main.rs:1216`).
- Impact: scripts cannot inspect leftovers or complete supported uninstall workflow available through Windows hub. Smallest fix: reuse Windows hub app-detail/uninstall backend behind CLI with same identity/liveness/selection checks; preserve explicit unknowns.

### N15 — Windows bridge process identity ignores recorded start time [medium, D]
- Mac: `C/bridge/deliver_claude.rs:132` compares recorded process start against UTC process metadata. Windows: `C/bridge/deliver_claude.rs:150` discards start/domain after checking PID exists.
- Impact: reused PID can keep stale chat registration apparently live; this does not prove delivery to another chat because transport authentication is separate. Smallest fix: compare Windows process creation time to registration identity; unknown/mismatched identity must not count as live.

### N16 — Windows Claude pipe writes are outside bounded ACK wait [high, D]
- Mac: stream gets write timeout (`C/bridge/deliver_claude.rs:217`). Windows: synchronous file/pipe handle has no write deadline (`C/bridge/deliver_claude.rs:226`); delivery writes precede ACK receive deadline (`C/bridge/deliver_claude.rs:451`, `C/bridge/deliver_claude.rs:470`).
- Impact: stalled pipe reader can block delivery before timeout handling starts. Smallest fix: use cancellable overlapped pipe I/O with bounded connect/write/read deadlines & propagate stage-specific failure.

### N17 — Windows animation path ignores reduced-motion preference [medium, MISSING]
- Mac: custom ring/button motion consults accessibility Reduce Motion (`M/Features/ProviderRing.swift:168`, `M/DesignSystem/CardButtonStyle.swift:43`). Windows: card phase/timer run solely from content animation flag (`W/main.rs:1492`), with no OS animation preference gate in reviewed Windows path.
- Impact: animated card progress remains active when user disables UI motion. Smallest fix: read Windows animation preference, observe changes & use still phase; keep real progress updates.

### N18 — Empty notch body no longer opens Overview [low, MISSING]
- Mac: `M/Notch/NotchWindowController.swift:1732` opens Overview outside ring targets. Windows: `W/main.rs:2055` returns without recorded cell/orb press.
- Impact: larger body target does nothing; audit C4 left this unresolved. Smallest fix: record body press & map matching release to Overview, excluding drag/grip.

### N19 — No restart offer after installed copy changes externally [medium, MISSING]
- Mac: detects on-disk build replacement & offers Restart (`M/App/Updater.swift:223`). Windows: updater worker only checks remote feed (`W/update.rs:309`); no installed-build observer/restart phase in that flow.
- Impact: replacing installed files while old process survives has no equivalent “use newly installed copy” affordance. Smallest fix: compare installed executable version/build identity periodically & offer controlled relaunch when different.

### N20 — Long Nearby device chooser has no Windows scrolling [medium, MISSING]
- Mac: chooser wraps devices in runtime scroll view (`M/Sharing/SendCard.swift:113`, `M/App/ViewShots.swift:110`). Windows: adds every device (`W/send.rs:457`), lays every row into growing height (`W/render.rs:984`), & card procedure has no wheel/scroll handling (`W/main.rs:2503`).
- Impact: enough nearby devices can exceed work-area height with no way to reach offscreen choices. Smallest fix: cap chooser viewport to available monitor space; add scroll offset, wheel handling & clipped hit testing.

### N21 — Clipboard images use different wire formats [low, D]
- Mac: always converts clipboard image to PNG (`M/Sharing/NearbySharing.swift:258`). Windows: prefers registered PNG, otherwise sends BMP from CF_DIB (`W/send_sys.rs:270`).
- Impact: equivalent paste can create larger/differently supported attachment on receiver. Smallest fix: encode DIB fallback to PNG before creating outgoing file.

### N22 — Cross-volume link copying is explicitly missing on Windows [medium, MISSING]
- Mac: shared hub move path recreates symlinks on Unix (`HT/files.rs:309`). Windows: same path returns “Links cannot be copied” (`HT/files.rs:317`). This is separate from audit's scanner cross-volume rejection.
- Impact: moving folder containing link across drives fails; partial target cleanup follows `HT/files.rs:291`. Smallest fix: implement supported Windows link/reparse copying with identity checks, or preflight entire selection & clearly report unsupported entries before copying.

### N23 — Edge changes snap instead of Mac pickup/corner/settle motion [low, D]
- Mac: pickup/release animation & corner passage are explicit (`M/Notch/NotchWindowController.swift:462`, `M/Notch/NotchWindowController.swift:621`, `M/Notch/NotchWindowController.swift:1145`). Windows: chooses edge then directly moves/resizes window (`W/main.rs:2133`, `W/main.rs:2163`).
- Impact: drag can change orientation/size abruptly at corners. Smallest fix: retain existing placement persistence, interpolate pickup/corner/settle geometry with reduced-motion fallback.

## 2. Disagreements with today's audit

| Audit claim | Source-supported correction |
|---|---|
| B1 “card travel/grace works” (`docs/parity-audit-2026-10-10.md:57`) | Only Claude/Send preserve pointer travel; Codex/System/Disks are transparent & cleared on leave. **D**, N09 (`W/main.rs:1880`, `W/main.rs:2510`). |
| B5 separate Memory card is Mac parity target (`docs/parity-audit-2026-10-10.md:61`) | Mac registers SystemLoad, Disks, Send only; standalone MemoryProvider exists but is not registered. Remove this missing-card claim (`M/System/SystemProviders.swift:23`). |
| B4/top-10 treats missing battery row as Mac notch gap (`docs/parity-audit-2026-10-10.md:60`, `docs/parity-audit-2026-10-10.md:272`) | Mac System card rows contain CPU/memory/GPU/network/fans/CPU-temperature, no battery. Distinguish sensor collection/hub from actual notch rendering (`M/System/SystemProviders.swift:73`). CPU temp/fans remain real differences. |
| C5 badge works; merely nothing pending (`docs/parity-audit-2026-10-10.md:72`) | Live state initializes false without update producer; fixture drawing does not establish behavior. **MISSING**, N06 (`W/main.rs:235`, `W/main.rs:957`). |
| C10 displays merely untested (`docs/parity-audit-2026-10-10.md:77`) | Basic multi-monitor panels may work, but scope/display selection controls are absent: Mac publishes live screens (`M/System/HubBridge.swift:165`); Windows publishes saved monitor-map keys (`W/bridge.rs:294`) & accepts no scope/display selector (`W/bridge.rs:552`). Separate **MISSING controls** from untested docking hardware. |
| C17 arrow over card (`docs/parity-audit-2026-10-10.md:84`) | Preserve installed-build observation; current HEAD explicitly sets hand over actionable card hit (`W/main.rs:2536`). Hover/press gaps N07/N08 still stand. |
| E5 polling/backoff “works” (`docs/parity-audit-2026-10-10.md:106`) | Periodic successful polling proves neither same-account cache recovery nor server-directed retry. **D**, N11/N12 (`W/usage.rs:719`, `W/http.rs:79`). |
| E6 missing CLI/hub credits implies Windows-only loss (`docs/parity-audit-2026-10-10.md:107`) | Both generic snapshot contracts omit credits/spend/reset extras: Mac emits label/fraction/duration (`M/System/HubBridge.swift:185`); Windows emits label/fraction (`W/bridge.rs:332`). Duration omission is Windows-specific, N13; generic credit export is shared debt. |
| E8 show/order “work” (`docs/parity-audit-2026-10-10.md:109`) | Hub-state edits persist (`W/bridge.rs:635`), but live notch still builds fixed cells (`W/layout.rs:291`) & routes clicks through fixed `Cell::ALL` (`W/main.rs:2059`). Audit A11 already notes this; J14/E8 must distinguish stored preference from honored behavior. |
| Account forgetting broadly absent (`docs/parity-audit-2026-10-10.md:109`) | Generic provider Forget reading remains different; orphaned Claude account-book Forget is implemented on both: UI sends `forgetClaudeAccount` (`H/views/Settings.tsx:730`), Windows delegates it to account book (`W/bridge.rs:412`), Mac handles it (`M/System/HubBridge.swift:279`). |
| Drive capacity banner should trigger SMART alert (`docs/parity-audit-2026-10-10.md:252`) | Mac alert covers health warning/wear/media-error changes, not fullness (`M/App/AppDelegate.swift:813`). Windows `driveAlert` omission remains real (`W/bridge.rs:17`), but capacity banner is not valid reproduction of that Mac feature. |
| Update icon replaced by placeholder (`docs/parity-audit-2026-10-10.md:225`) | Distinguish fixtures from runtime: live Windows update panel uses executable file icon (`W/update.rs:213`). N05 concerns actual live behavior, not fixture art. |
| Windows Send differences framed as build predating `5d981c33` (`docs/parity-audit-2026-10-10.md:265`, `docs/parity-audit-2026-10-10.md:286`) | Changes are Mac-side: Mac clears completed send (`M/Sharing/NearbySharing.swift:845`); Windows retains Sending/done prompt with hover-dependent expiry (`W/send.rs:888`, `W/send.rs:1299`). Windows hover still has stacked rows (`W/send.rs:1511`, `W/send.rs:1623`); needs port, not just installation. |
| Restart/Codex credits/network kind/grips/Copy last are Windows-only (`docs/parity-audit-2026-10-10.md:233`) | Mac has restart button (`M/System/ClaudeRestart.swift:123`), Codex extras (`M/Providers/CodexLocalProvider.swift:86`), network kind (`M/System/SystemProviders.swift:85`), edge/grip movement (`M/Notch/NotchFleet.swift:118`) & Copy last (`M/Features/TooltipCard.swift:488`). Windows-specific native APIs are implementations, not additional user journeys. |

## 3. Behavioral walk: remaining coverage & reverse parity

This ledger groups shared card states/actions by actual implementation, including already-known gaps. It does not promote source presence to installed proof.

| Surface / behavior | Classification & counterpart evidence |
|---|---|
| Claude/Codex usage rings; stale/error states | **D**: both render provider states; Windows startup restoration/cache behavior differs (N10/N11); fixed bands/options already audited (`M/Features/ProviderRing.swift:1`; `W/layout.rs:291`; `W/usage.rs:203`). |
| System/disks/Send cells | **D**: corresponding registered cells exist; sensor/SMART and options gaps retained (`M/System/SystemProviders.swift:23`; `W/layout.rs:291`). |
| Provider hide/order/profiles/nicknames | **D/MISSING**: Mac filters/orders actual snapshots (`M/Model/UsageStore.swift:298`); Windows account settings persist but fixed notch ignores them (`W/bridge.rs:635`, `W/layout.rs:291`). Already A11. |
| Claude/Codex account identity, Desktop cache, multi-account book | **D**: Windows has identity-aware Desktop/cache branch & book actions, not “two rows only”; N11 matters within same-account branch (`M/Providers/ClaudeAccountBook.swift:1`; `W/usage.rs:698`; `W/bridge.rs:394`). |
| Sign-in/out/access, OAuth refresh, Claude CLI usage fallback | **MISSING/N/A**: account actions/refresh/fallback remain audited gaps (`M/System/HubBridge.swift:269`; `M/Providers/ClaudeTokenRefresher.swift:1`; `M/Providers/ClaudeUsageCLI.swift:1`; `W/bridge.rs:11`). macOS Keychain-specific permission prompts are **N/A**, not missing Windows keychain UI. |
| Codex credits, reset credit expiry, extra limits | **D** presentation/export; provider parsers exist both (`M/Providers/CodexLocalProvider.swift:86`; `W/usage.rs:1085`). Generic hub export limitations corrected above. |
| Threshold/reset/session-limit/weekly-limit detection | **I** basic seeded-crossing rules; **D** delivery/sounds/peek (`M/Model/ThresholdNotifier.swift:44`; `M/Model/UsageLimitWatcher.swift:57`; `W/alerts.rs:125`). |
| Session completion, peek focus, sounds, channel & previews | **MISSING** Windows as audit says; Mac completion can chime/peek despite removed session rows (`M/App/AppDelegate.swift:735`; `M/Notch/NotchWindowController.swift:1688`; `W/bridge.rs:14`). |
| Alert dismissal | **D**: Mac explicit plain close button (`M/Features/UsageResetCard.swift:132`); Windows any notice-body click dismisses (`W/main.rs:1985`). Do not claim Mac alert close uses pressed CardButtonStyle. |
| Send refresh/close/cancel/accept/decline/show/open/copy | **D** feedback, not wholly missing actions: Mac card buttons (`M/Sharing/SendCard.swift:127`, `M/Features/TooltipCard.swift:524`); Windows dispatch (`W/send.rs:1377`) & N07/N08. |
| Send chooser devices vs hover devices | **D**: Mac styled button/hover row (`M/Sharing/SendCard.swift:222`, `M/Features/TooltipCard.swift:541`); Windows chooser `Row::Device` highlights, hover `Row::Pair` does not (`W/send.rs:457`, `W/send.rs:1609`). Long chooser N20. |
| Claude restart/sync button | **D** feedback; action exists both (`M/System/ClaudeRestart.swift:123`; `W/main.rs:1972`), N07. |
| Update & installer pills/close buttons | **D**: Mac pressed/hover style (`M/Features/UpdateCard.swift:168`, `M/Features/DiskImageCard.swift:223`); Windows shared card hover only (`W/render.rs:2131`), updater click & installer dispatch exist (`W/main.rs:2000`, `W/send.rs:1958`). |
| Settings orb & move grip | **D**: Mac hover/click motion (`M/Features/SettingsHandle.swift:47`, `M/Features/SettingsHandle.swift:262`); Windows records orb press & grip drag (`W/main.rs:2015`, `W/main.rs:2048`), lacks shared card pressed rendering (N07). |
| Quit-only right-click menu | **D**: native Mac NSMenu has key equivalent & standard interaction (`M/Notch/NotchWindowController.swift:2134`); Windows fixed plate has no hover branch, release-only Quit (`W/render.rs:2180`, `W/main.rs:2528`). Existing outside-dismiss defect remains separate. |
| Click ring, empty body, fold/unfold, full-screen | **D**: ring destinations match; body N18; full-screen toggle Mac-only (`M/Notch/NotchWindowController.swift:1719`, `M/Notch/NotchWindowController.swift:1452`; `W/main.rs:2059`, `W/main.rs:904`). |
| Four-edge docking/drag/reset, multi-monitor | **D**: counterpart present; corner motion N23, missing scope controls above. **Reverse advantage:** Windows persists edge/position per monitor (`W/main.rs:2197`); Mac explicitly shares edge/offset across fleet (`M/Notch/NotchFleet.swift:49`). Smallest reverse-parity fix: persist placement by display identity rather than one fleet offset. |
| No Dock/tray, nonactivating notch, single instance | **D** implementation/policy: Mac accessory panel (`M/Notch/NotchPanel.swift:1`, `M/App/Runtime.swift:1`); Windows tool windows/runtime guard (`W/main.rs:2506`, `W/runtime.rs:1`). Oldest/newest winner difference already audited. |
| Size/visibility & persistence | **I** shared setting intent, **D** geometry: Mac bridge (`M/System/HubBridge.swift:392`); Windows setter (`W/bridge.rs:577`). Stored settings alone do not prove layout parity. |
| Surface/accent/transition/language; percentages/watch/critical thresholds; weekly placement/dash/label/headline; pace/extra limits/reset-time format | **MISSING** Windows setting contracts already mostly audited: exact Mac keys `M/System/HubBridge.swift:398`; absent from exhaustive Windows setter `W/bridge.rs:552`. This includes `colorTransitionStyle`, `resetTimeFormat`, `showCodexExtraLimits`, beyond visible toggles cited by audit. |
| Hub settings visibility | **D**, intentional omission rather than broken rendered toggle when key absent: controls check `has(key)` (`H/views/Settings.tsx:197`); genuine sent-but-not-honored operations N03/N04/N05 & hide/order above. |
| Login, Nearby enable/name/folder/accept-known, installer-auto, screenshot destination | **I** supported contracts with platform-native execution; live Windows handlers exist (`M/System/HubBridge.swift:429`, `M/System/HubBridge.swift:444`; `W/bridge.rs:473`, `W/bridge.rs:552`). Login defaults differ as already audited. |
| Mute Claude/Codex alerts — reverse gap | **MISSING on Mac hub**: Windows publishes/accepts toggles (`W/bridge.rs:257`, `W/bridge.rs:575`); shared UI requires keys (`H/views/Settings.tsx:323`). Mac has mute backend (`M/Settings/Preferences.swift:813`) but bridge table omits it (`M/System/HubBridge.swift:414`). Smallest fix: expose backend mute through same keys. Already noted as Windows extra in audit. |
| Finder cut/paste, Copy Path, green-button maximize, Dock minimize | **N/A** direct Mac hooks on Windows; native Explorer/window/taskbar workflows serve corresponding tasks. Mac convenience wiring `M/Conveniences/ConveniencesService.swift:124`; Windows hub uses Explorer (`HT/files.rs:122`). Do not equate native alternatives with all configurable window actions. |
| AutoQuit / Fn-as-Command | **N/A** exact Mac app/Fn semantics; Mac implementations `M/Conveniences/ConveniencesService.swift:135`, `M/Conveniences/ConveniencesService.swift:145`. No Windows counterpart in keyboard dispatcher (`W/keys.rs:256`); configurable auto-quit would be new cross-platform policy, not native Windows parity proven. |
| Window management hotkeys & launcher | **MISSING** Windows Pulse actions/launcher as audit says: Mac hotkey/config/clipboard/currency/dictionary/Shortcuts & window bindings (`M/System/HubBridge.swift:448`); Windows dispatcher only editing/screenshot chords (`W/keys.rs:256`). Shared hub file search is not launcher replacement. |
| Launcher app/file/web/snippet/calculator/conversion/clipboard/history/Shortcuts actions | **MISSING** Windows native launcher: Mac controller & action modules (`M/Launcher/LauncherController.swift:42`, `M/Launcher/LauncherCalculator.swift:1`, `M/Launcher/LauncherConversion.swift:1`, `M/Launcher/LauncherActions.swift:10`); Windows settings whitelist has none (`W/bridge.rs:552`). |
| Alt editing remap — reverse addition | **D / Windows-only Pulse implementation**: Alt+A/C/V/X/Z, Alt+Shift+Z→Ctrl+Y; AltGr/Ctrl+Alt/Win excluded (`W/keys.rs:246`). Mac uses native Cmd editing; Fn adapter is distinct (`M/Keyboard/FnCommand.swift:1`). Not a universal Cmd→Alt mapping; N04 governs live toggle. |
| Alt+Shift+4/5 screenshots — reverse addition | **D / Windows-only Pulse implementation**: region/toolbar mapping (`W/keys.rs:260`); toolbar hover/selected state & release-only click (`W/shot.rs:921`, `W/shot.rs:1519`). No Mac Pulse capture engine; Cmd+Shift+4/5 are OS functionality, not missing Swift module. |
| Screenshot Escape/Space; Desktop/clipboard destination | **D native implementation**: Windows keyboard interception only during active capture (`W/keys.rs:234`); destination changes immediately (`W/bridge.rs:482`). Do not confuse destination setting with deferred enable toggle N04. |
| DMG vs MSIX/MSI installer | **D / N/A package mechanism**: Mac DMG installer (`M/Conveniences/DiskImageInstaller.swift:1`), Windows installer choices/auto state (`W/installer.rs:618`, `W/installer.rs:684`); Mac auto-update/trash-download preferences have no matching Windows bridge keys (`M/System/HubBridge.swift:438`, `W/bridge.rs:572`). |
| Nearby protocol, discovery, receive/send progress, peer selection | **I** shared service contract; **D** native frontend interactions: shared `HT/share.rs:695`; Mac selects pending payload (`M/Sharing/NearbySharing.swift:199`), Windows same (`W/send.rs:1444`). Selecting idle device does not send arbitrary clipboard on either platform. |
| Paste/drop/Copy last & received text/file actions | **D** keyboard/image/card details: Mac paste (`M/Sharing/NearbySharing.swift:239`) & tooltip controls (`M/Features/TooltipCard.swift:488`); Windows Ctrl+V registration (`W/send.rs:1907`) & action dispatcher (`W/send.rs:1377`). Image format N21; completed-send state corrected in §2. |
| Local-network/firewall permission UI | **N/A** OS mechanism; corresponding entry points (`HT/share.rs:186`, `HT/share.rs:199`; `H/views/NearbySettings.tsx:224`). Unknown notch permission state is existing audit gap, not proof access granted. |
| Updater verification | **D** platform trust mechanism, both present: Mac code signature/notarization (`M/Updater/UpdateVerifier.swift:7`); Windows Authenticode & same signer check (`W/update.rs:543`). Do not claim Windows accepts arbitrary signed installer. State/recovery N05/N06/N19. |
| Shared hub pages & platform words | **I** shared component behavior where platform branches absent; explicit differences reviewed in Apps, Storage, Settings, DriveHealth, Nearby, Overview (`H/views/Apps.tsx:726`, `H/views/Storage.tsx:278`, `H/views/Settings.tsx:189`, `H/views/DriveHealth.tsx:19`, `H/views/Overview.tsx:24`). Native backend data differences remain material. |
| Hub live disk index/watch | **MISSING** Windows, already J10: Mac implementation vs empty non-Mac module (`HT/disk_index.rs:66`, `HT/disk_index.rs:428`; `HT/watch.rs:12`). |
| Hub file menu/copy/move/trash | **D** Cmd→Ctrl shortcuts (`H/views/Storage.tsx:278`), Finder→Explorer (`HT/files.rs:114`), Trash→Recycle Bin (`HT/cleanup.rs:45`); cross-volume link gap N22. |
| Hub Apps/updates/health | **D** native backends, existing app-running/icon/MSIX/SMART gaps retained (`HT/lib.rs:5`, `HT/lib.rs:7`, `HT/health.rs:36`); Windows NVMe/winget/UserAssist are platform data sources, not missing Mac user workflows. |
| Agent bridge CLI, links, roster, control queue | **I** shared entry points & file control contract (`C/bridge/control.rs:1`, `C/bridge/links.rs:65`); **D** native Claude transport N01/N15/N16 & hub lifecycle N02. Listing peers does not establish successful two-way delivery. |
| Codex bridge | **D** executable discovery, shared queued delivery: Mac bundle/brew candidates vs Windows local paths/PATH (`C/bridge/deliver_codex.rs:58`, `C/bridge/deliver_codex.rs:66`, `C/bridge/deliver_codex.rs:145`). No evidence here that candidate search resolves every packaged Windows Codex install. |
| CLI status/scan/findings/explain/history/procs/monitor/find/browse/export/duplicates/usage | **I** command availability, **D** platform data/format bugs already audited plus N13 (`C/main.rs:137`, `C/main.rs:310`, `C/main.rs:374`, `C/main.rs:405`, `C/main.rs:550`, `C/main.rs:620`). |
| CLI send/bridge/claude/worker | **I** shared commands, not Windows-only: entry dispatch (`C/main.rs:97`, `C/main.rs:108`, `C/main.rs:113`); worker serve/request (`C/main.rs:930`, `C/main.rs:972`). Native socket/pipe difference is transport, N16 addresses actual failure difference. |
| CLI destructive command gates | **I** top-level `plan/apply/quit/force-quit/uninstall-plan` disabled both (`C/main.rs:626`); **D** separate `apps uninstall` Mac enabled/Windows rejected, N14. Do not conflate those dispatches. |

## 4. Ranked top 15 additional fixes

Ranks prioritize broken user promises/recovery before presentation. Already-audited scan completeness, taskbar overlap, sound/launcher/SMART gaps remain separate backlog; this list prioritizes newly established findings above.

| Rank | Finding | First bounded change / evidence |
|---|---|---|
| 1 | N01: Claude bridge cannot receive native replies | Named-pipe listener + peer registration (`C/bridge/deliver_claude.rs:612`; `C/bridge/hub.rs:170`). |
| 2 | N02: hub recovery depends on Nearby | Independent supervisor (`W/send.rs:900`; `M/System/HubLauncher.swift:149`). |
| 3 | N05: updater hub workflow/failure feedback absent | Publish status & handle install/error (`W/bridge.rs:387`; `W/update.rs:463`). |
| 4 | N04: settings leave interception/checks active | Live setters (`W/bridge.rs:484`; `W/keys.rs:120`). |
| 5 | N03: Refresh now only repaints | Both-provider wake (`W/bridge.rs:447`; `M/App/AppDelegate.swift:449`). |
| 6 | N11: useful cache blocked by same-account 429 | Cache-first branch (`W/usage.rs:719`; `M/Providers/ClaudeOAuthProvider.swift:259`). |
| 7 | N16: named-pipe send can block before timeout | Bounded write/connect (`C/bridge/deliver_claude.rs:226`). |
| 8 | N07: release-only card/menu actions | Press/capture contract (`W/main.rs:2554`; `M/DesignSystem/CardButtonStyle.swift:31`). |
| 9 | N06: update/permission dots never become live | Wire state producers (`W/main.rs:235`; `M/Notch/NotchFleet.swift:77`). |
| 10 | N10: restart loses stale usage reading | Restore account-bound archive (`W/usage.rs:209`; `M/Model/UsageStore.swift:273`). |
| 11 | N20: long device list cannot scroll | Bounded scroll viewport (`W/render.rs:984`; `M/Sharing/SendCard.swift:113`). |
| 12 | N15: PID reuse accepted as chat liveness | Compare creation identity (`C/bridge/deliver_claude.rs:150`). |
| 13 | N14: CLI app inspection/uninstall incomplete | Reuse Windows hub backend (`C/main.rs:1203`; `C/main.rs:1216`). |
| 14 | N12: server retry delay lost | Carry/persist retry deadline (`W/http.rs:79`; `W/usage.rs:487`). |
| 15 | N13: duration lost from hub/CLI limits | Serialize `seconds` (`W/bridge.rs:332`; `C/usage_snapshot.rs:105`). |

Review changed this report only. Builds/tests executed: 0; tests added/deleted: 0; existing test inventory unchanged. Follow-up verification should extend complete installed journeys covering retained state, failure recovery & actual rendered interactions, as required by `AGENTS.md` & `docs/testing.md`.
