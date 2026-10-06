# Mac + Windows system tool: decision & implementation plan

Date: 2026-10-02; revised 2026-10-05 (source & machine validation, then owner direction). Status: draft, reviewed (see Review status). Implementation has not started.

## Decision

Build one owned tool with native always-on pills, shared Rust core & shared on-demand windows. It replaces Vorssaint, CodexBar & manual cleanup.

| Layer | Mac | Windows | Runs |
| --- | --- | --- | --- |
| Pill (always on) | Fork of Codenotch's Swift app, providers cut to Claude & Codex, new resource & disk rings | Native Rust pill (`windows` crate, layered window, Direct2D/DirectComposition), Codenotch's design & ring logic redrawn natively | Always; minimal footprint |
| Core (library) | Rust library via UniFFI bindings | Same Rust library, linked directly | Linked into every process below |
| Worker | Rust executable; owns scans, plan/apply & uninstall jobs | Same | Started on demand by pill, dashboard or CLI; exits when idle |
| Dashboard & settings | Tauri app: Resources (task manager), Storage, Cleanup, Uninstall, Settings | Same Tauri app, same screens | Launched on demand; exits when closed |
| CLI | Same Rust core; talks to pill/worker | Same | On demand |
| Mac conveniences | Swift modules inside Mac pill app, reused from Vorssaint | Not needed (native in Windows) | Always |
| Launcher | Tinycast module inside Mac app | PowerToys Command Palette + this tool's extension | On hotkey |

Why this split: always-on part must be minimal, so it is native on both OSes (no web view running in background). Codenotch's Swift Mac app already exists and is more complete than its Windows port, so Mac keeps Swift. Windows pill is native Rust because Codenotch's Windows pill is Tauri/WebView2, whose background web-view processes cost tens of MB permanently. Everything heavier (charts, tables, settings) is one Tauri app on both OSes, so those screens are identical and only cost memory while open. All numbers come from one Rust core, so both machines and CLI agree.

Earlier options rejected: keeping stock Vorssaint (owner wants one owned tool); single Tauri app for everything (web view always resident; Mac pill behaviour risk); GPUI (small community, weaker Windows maturity).

No application installation, settings changes, public forks or product implementation are included in this document task.

## Requirements

- **Always-visible pill**, right screen edge, both OSes: Claude & ChatGPT usage, CPU, memory pressure, disk free for each mounted local disk. Hover card adds swap, GPU, top storage findings & limit reset times. Never steals focus; hidden whenever a fullscreen window is on pill's display, even when focus is on another display.
- **Click opens dashboard** (Tauri):
  - Resources: task-manager view, apps with helpers grouped, sortable CPU, RAM, GPU; expand to individual processes; Quit then explicit Force Quit.
  - Storage: biggest folders per disk, growth since previous scan, clone-aware sizes.
  - Cleanup: rule findings (Chrome copies, stale graphs, orphaned caches, build leftovers…) with review & Trash.
  - Uninstall: remove an app with all its files; find leftovers of apps already deleted.
  - Settings.
- Incidents to cover: Chrome signing-copy accumulation, Xcode simulator devices/runtimes/caches & iOS DeviceSupport, WhatsApp media, swap & APFS shared storage, local Time Machine snapshots, developer & agent artefacts, app leftovers.
- Mac conveniences to replace Vorssaint (owner-selected 2026-10-05): Finder cut/paste (highest priority), window maximizer, Dock click minimize, Auto Quit on last window close. Clipboard history not replaced: owner uses Paste (via Setapp) on Mac, Win+V on Windows. Dropped: audio priority, middle click, notch AI agents, lyrics & queue.
- CLI for agents covering storage, cleanup, uninstall & processes.
- Open source first: reuse maintained open-source code wherever it exists; hand-roll only what does not exist or can be done clearly better.
- Owned updates on both OSes; no dependency on Vorssaint upstream.
- Local metadata only by default; no upload, cloud-placeholder hydration or continuous full-disk scanning.

## Resource budget (always-on pill)

| | Mac pill | Windows pill |
| --- | --- | --- |
| Memory | ≤ 60 MB resident before launcher first use; ≤ 150 MB with launcher index loaded | ≤ 25 MB resident |
| CPU, idle | ≤ 0.5 % average at 2 s sampling | ≤ 0.5 % average |
| While hidden (fullscreen) | Sampling slowed to 10 s, no drawing | Same |

Sampling uses cheap counters only (host statistics, volume totals, cached provider readings). Scans, process trees & GPU detail run in worker or dashboard, never in pill. Pill animations stop when nothing changes; no idle timers faster than sampling.

Budgets are feasibility gates, not assumptions: nothing measured yet proves them. Measured on release builds before integration (milestone 0) and again at acceptance, using:

- Memory: physical footprint (Mac) / private bytes (Windows), plus RSS for reference; steady state after 10 min idle, peak during hover & ring updates, and return to baseline within 60 s after dashboard closes; child processes reported separately, never hidden.
- CPU: percent of one core averaged over 10 min idle with pill visible, no dashboard, providers polling at configured interval.
- If a gate fails, revise budget or design deliberately (e.g. launcher as separate process) rather than discovering it late.

## Open-source sources

Reuse first; hand-roll only gaps.

| Area | Source | Licence | Use |
| --- | --- | --- | --- |
| Mac pill & AI usage | Codenotch Mac app (Swift) | MIT | Fork: pill, edge placement, Option-drag, settings orb, Claude & Codex readers, updater |
| Windows pill design & AI usage | Codenotch `windows/` (Rust/Tauri 2) | MIT | Claude & Codex readers (`usage.rs`, `claude_auth.rs`, `codex.rs`), ring geometry, updater wiring; pill redrawn natively |
| Storage engine | Petal main `f5e5b00` (Rust) | MIT | `scan`, `dirlist`, `disk`, `findings`, `classify`, `watch`, `trashing`; not GPUI UI, not `admin.rs` in first version |
| Cross-platform processes, CPU, RAM | `sysinfo` crate (Rust) | MIT | Process list, CPU, memory, disks in core |
| Mac resource readers | Stats (exelban/stats, Swift) | MIT | CPU, GPU, RAM, memory-pressure, disk readers for Mac pill & core reference |
| Windows process & GPU detail | System Informer (C) | MIT | Reference for per-process GPU counters, process identity, termination edge cases |
| Mac uninstaller & leftovers | Pearcleaner (Swift) | Apache-2.0 + Commons Clause | Leftover search logic for Mac app |
| Windows uninstaller & leftovers | Bulk Crap Uninstaller (C#) | Apache-2.0 | Reference for uninstaller discovery & leftover detection; reimplement in Rust |
| Cleanup detection | Mole (tw93/Mole, shell) | GPL-3.0 | Detection rules & paths reused directly |
| Fullscreen hide rule | HeardRight `tauri-app-next` (owner's, proprietary) | Owner's call | `pill/state/windows.rs` reused in Windows pill; Mac logic ported to Swift |
| Mac conveniences | Vorssaint 3.4.0 (Swift) | GPL-3.0-or-later | Reuse Finder cut/paste, Dock minimize, Auto Quit & maximizer code directly |
| Mac launcher | Tinycast (abue-ammar/tinycast, Swift) | AGPL-3.0-or-later (verified pinned source) | Forked into Mac app; weekly upstream merge |
| Rust ↔ Swift bridge | UniFFI (Mozilla) | MPL-2.0 | Generates Swift bindings for core |

Hand-rolled because nothing suitable exists or existing code is wrong: clone/hard-link/snapshot-aware reclaim accounting (Petal findings 1–3), developer & agent artefact rule pack with liveness checks, CLI with plan/apply safety contract, native Windows pill, unified findings store shared by pills, dashboard & CLI.

Repository is public; donor extraction requires checking publication & redistribution rights. Preserve notices & applicable licence obligations. Custom/restricted donor code is not imported until reviewed.

## Pill placement

Default: right screen edge on both OSes and every monitor, so position never differs between machines. Placement is a setting.

- Mac notch area (optional mode): top-centre beside notch on built-in display, expanding downward on hover. Uses otherwise dead space, but competes with menu-bar icons (which already overflow behind notch on MacBook Air), needs hover delay against accidental expansion from menu-bar trips, and has no notch on external displays, where it falls back to right edge.
- Windows taskbar: not offered. Windows 11 removed taskbar toolbars/deskbands; overlaying taskbar fights its z-order, auto-hide, centred icons & multi-monitor layouts and breaks with Windows updates.
- No tray, menu-bar or Dock icon: pill is sole surface (Codenotch Mac setting “neither”). Launch at login restores it. Drop Codenotch's Windows tray code.
- Mac keeps Codenotch's Option-drag along edge & per-edge memory; Windows pill implements same gesture with Alt-drag.

## Settings access

Identical on both OSes; Settings is a screen in shared Tauri dashboard, same layout, sections & wording. Pill is the single settings writer: dashboard and CLI send changes to pill over local channel; pill also persists its own state (edge, position). Writes are atomic (write-then-rename), schema versioned with migrations. If pill is not running, dashboard starts it first. Preferences are machine-local; optional export/import of shared settings between Mac & Windows later.

- Settings orb under pill (Codenotch's arc that becomes a gear on hover) on both OSes: one click opens Settings.
- Clicking pill body opens dashboard on Resources.
- Pill right-click menu: Settings, Pause, Quit.
- Global shortcut to open settings, same key position on both machines: default Command+Shift+Comma on Mac, Alt+Shift+Comma on Windows (matches PowerToys Alt convention; plain Command+Comma is avoided because every Mac app uses it for its own settings). Configurable.
- CLI `settings` command opens window, so agents & terminal can reach it too.
- Single instance, separately for pill and dashboard: per-user instance lock; a second launch (login item, manual, CLI, post-update restart) forwards its request to the running instance and exits. Dashboard cold start target ≤ 1 s.
- Global shortcut registration checks for collisions and reports them in Settings instead of failing silently.
- Only platform differences: Mac-only sections for conveniences & macOS permissions, shown with status & “Open System Settings” buttons; hidden on Windows rather than greyed.

## Pill visibility

Rule: pill is hidden whenever a fullscreen window (video, game, presentation) is on the pill's display, regardless of which display has focus. Start from HeardRight's code and close its one gap:

- Mac: macOS fullscreen apps get their own Space, and the pill panel is deliberately not fullscreen-auxiliary, so it never appears over a fullscreen app even when focus is on another display. HeardRight's foreground check is kept for non-native fullscreen (games, non-AppKit players).
- Windows: HeardRight checks only the foreground window. With two monitors, a fullscreen video on the pill's monitor while you work on the other monitor is no longer foreground, so HeardRight's pill would reappear over the video. This tool instead checks the topmost visible window on the pill's monitor (not just the foreground window) with the same style & geometry test, so the pill stays hidden.

Displays are identified by stable display IDs (not index or coordinates) so placement survives reconnects, resolution & DPI changes.

- Mac (Swift, ported from HeardRight's Rust logic): frontmost app's focused window `AXFullScreen` attribute decides; window-geometry fallback only when attribute is missing (games, non-AppKit windows), never overriding explicit `false`. Panel joins all Spaces but is not fullscreen-auxiliary.
- Windows (reuse HeardRight `pill/state/windows.rs` directly): foreground window counts as fullscreen only when it has no caption or resize frame and covers pill's monitor; shell overlays and maximized windows do not count; fullscreen app on another monitor never hides pill.
- Requires Accessibility permission on Mac (already needed by convenience modules).

A video playing in an ordinary window does not hide pill; only fullscreen does. Same as HeardRight.

Windows' style-plus-geometry check is a heuristic (borderless maximized windows can match; some fullscreen windows keep style flags). Test matrix on both OSes: video & game fullscreen, presentation mode, borderless maximized windows, focus moving between two monitors, macOS Spaces & Stage Manager, display reconnect, mixed DPI, auto-hide Dock/taskbar, Accessibility permission denied (Mac falls back to showing pill).

## AI usage rings

- ChatGPT: Codex plan limits (5-hour & weekly) from local Codex sign-in `~/.codex/auth.json` (present on this Mac), read only, never refreshed; fall back to last session snapshot marked stale. ChatGPT chat usage itself has no public limit API; Codex limits are what CodexBar shows.
- Claude: session & weekly windows. Codenotch's Windows port reads `~/.claude/.credentials.json`; its Mac app reads Claude Desktop's cached usage first, then `claude /usage`, then keychain token. On this Mac neither the credentials file nor standard `Claude Code-credentials` keychain item was found (2026-10-05), so Mac source is chosen in step 1 (Codenotch Mac's Desktop-cache → `claude /usage` → keychain chain is the starting point).
- Never send expired tokens; respect 429 back-off; missing data shows “Unavailable”, not zero. Credentials stay in memory; tool never writes others' credential files.
- Single owner: readers live in Rust core (port of Codenotch's Rust readers, with Mac credential sources from Codenotch's Swift app ported in), polled only by pill; dashboard & CLI `usage` read pill's latest readings. Each reading carries account, timestamp & source; stale readings dimmed with age. Poll interval 5 min with back-off; any subprocess (`claude /usage`, `codex`) bounded to 20 s and reaped. Expired login shows “Sign in needed”, never retries in a loop.

## Vorssaint replacement

Modules are Swift inside Mac pill app, taken from Vorssaint and adapted. Vorssaint 3.4.0 settings re-read 2026-10-05 from `com.vorssaint.utils`. Keep it running until each replacement module reaches parity, disabling each Vorssaint feature as its replacement turns on so two apps never handle same shortcut. Uninstall Vorssaint after last module ships.

| Feature | Mac implementation needs | Windows |
| --- | --- | --- |
| Finder cut/paste | Global event tap for Command-X/V while Finder frontmost; Finder move via Apple Events or file APIs; Accessibility & Input Monitoring permissions | Native in Explorer |
| Window maximizer | Intercept green button; resize via Accessibility API to visible frame | Native |
| Dock click minimize | Detect click on active app's Dock icon; minimize via Accessibility | Native taskbar behaviour |
| Auto Quit | Workspace notifications + Accessibility window count; per-app rules | Not needed |

Mac permissions (Accessibility, Input Monitoring, Automation, Full Disk Access) attach to app identity & signature; stable signing is required or permissions reset on every update (see Updates & distribution).

**Absorption is extraction, not wrapping.** Vorssaint, Codenotch & Tinycast are each whole apps with their own composition root: Vorssaint is an executable target whose Finder service depends directly on its shared session, permissions, preferences & feature services; Tinycast's `AppCore` owns stores, input hooks, activation policy, updater & coordinators and starts its app index at launch. So:

- Mac app has one AppDelegate and one service registry; donor code is compiled as owned library targets behind it.
- Donor updaters, login items, permission prompts, activation-policy changes & input hooks are removed; this app owns each once.
- One central event-tap service with explicit priority & teardown; Fn remap, Finder cut/paste, maximizer, Dock click & launcher hotkey register handlers on it. Callbacks do minimal work on the tap thread and hand off, so launcher or UI work never delays input.
- Milestone 0 inventories each donor's dependencies at a pinned commit before any agent starts extraction.

**Behaviour limits beyond donor parity:**

- Finder cut/paste: Vorssaint's bridge passes selected paths as newline-separated text, so filenames containing newlines break. Use structured path transfer (list of URLs) and per-item results. Test newlines & unusual characters, name collisions, cross-volume moves (copy + delete with failure in the middle), cancellation, clipboard replaced between cut & paste, permission fallback.
- Auto Quit: per-app opt-in, never global; debounce after last window closes; refuse when window state is unknown (inaccessible, other Spaces, minimized, transient replacement); lifecycle quit only, never force. Test apps with background work (downloads, exports).
- Window maximizer & Dock click: share event-tap service; no separate hooks.

## Launchers

Same key position on both machines.

| | Mac | Windows |
| --- | --- | --- |
| Launcher | Tinycast forked & absorbed into Mac app as a module | PowerToys Command Palette (Microsoft) + this tool's extension |
| Hotkey | Command+Space (turn off Spotlight's shortcut in System Settings → Keyboard → Keyboard Shortcuts → Spotlight) | Alt+Space (set in PowerToys). Overrides Windows' own Alt+Space window menu, which owner does not use; disable PowerToys Run so it does not also claim Alt+Space |
| Does | Apps, files & folders, quicklinks (saved URLs, searches, folders), shell commands, calculator, snippets, notes, AI chat (own key) — plus this tool's commands natively | Apps, files & folders, typed URLs & paths (Win+R replacement), commands, calculator, Windows settings, window switching — plus this tool's commands via extension |
| Clipboard | Paste (Setapp); Tinycast's clipboard module removed | Win+V |

Mac absorption: Tinycast (native Swift, zero third-party dependencies) extracted into Mac app as launcher module (see “Absorption is extraction” above). Launcher's search index loads on first hotkey press, not at login (upstream starts it at launch; this is a deliberate change). Feature allowlist: apps, files, quicklinks, shell commands, calculator, snippets, notes, AI chat. Upstream system actions such as Empty Trash or uninstall are removed or routed through this tool's core plan/apply, so the launcher never bypasses cleanup safety. Add raw typed URL/path opening (Win+R style) if upstream lacks it. This tool's commands (disk status, cleanup findings, quit app, Claude/ChatGPT usage, open dashboard) appear as native launcher entries backed by core.

Windows: Command Palette cannot be embedded, but Microsoft's Command Palette extension SDK (.NET) lets this tool add its commands to it; thin C# extension calls CLI and shows its JSON results. Build after CLI exists (step 2).

## Keyboard consistency

Goal: both habits work on both machines. Bottom-left key (Ctrl on Windows, Fn/Globe on MacBook) and thumb key (Alt on Windows, Command on Mac) both trigger copy/paste/cut/select-all, and Alt+Tab / Command+Tab stay untouched.

| Pressed | Windows | Mac |
| --- | --- | --- |
| Thumb key + C/V/X/A/Z/S/F/T/W | Alt+letter → Ctrl+letter (PowerToys Keyboard Manager) | Command+letter, native |
| Bottom-left key + C/V/X/A/Z/S/F/T/W | Ctrl+letter, native; remapping adds Alt as second route, never disables Ctrl | Fn+letter → Command+letter, by this tool |
| App switching | Alt+Tab untouched | Command+Tab untouched |
| Bottom-left key + Left/Right | Ctrl+arrow moves one word, native | Fn+arrow → Option+arrow (one word), by this tool |
| Bottom-left key + Shift + Left/Right | Ctrl+Shift+arrow selects one word, native | Fn+Shift+arrow → Option+Shift+arrow (select word), by this tool |
| Bottom-left key + Up/Down | Ctrl+Up/Down moves by paragraph in most editors, native | Fn+Up/Down → Option+Up/Down (paragraph), with Shift to select, by this tool |

Windows: PowerToys, not this tool. PowerToys Keyboard Manager is itself a keyboard hook with the same limits (needs PowerToys running; elevated apps need PowerToys elevated; does not apply on password/secure screens; Microsoft documents AltGr issues), but it is Microsoft-maintained and already handles more edge cases than a new hook would. Use Left Alt only for the mappings so AltGr typing is unaffected. Alt+letter also has meanings beyond menus in some terminals & editors (e.g. readline word moves in shells); owner accepts losing those for the mapped letters. Test in Windows Terminal, VS Code & a browser before relying on it.

Mac: built into Mac app, sharing the event tap Finder cut/paste already needs (same Accessibility & Input Monitoring permissions). Fn+letter is rewritten to Command+letter before cut/paste module sees it, so Fn+X/Fn+V also move files in Finder. Overrides macOS's own Globe shortcuts on those letters (Globe+A focuses Dock, Globe+C opens Control Center). Fn+arrows lose their macOS meaning (Fn+Left/Right = Home/End, which in most apps scrolls to document top/bottom; Fn+Up/Down = Page Up/Down); line start/end remains on Command+Left/Right. Fn+Delete (forward delete) & Fn+function keys untouched. Word-delete stays native on each OS (Ctrl+Backspace on Windows, Option+Delete on Mac); owner does not need a Fn word-delete. Mapping set configurable in Settings.

Event-tap remapping must handle key-down, key-up, auto-repeat & modifier-release ordering so modifiers never stick; Vorssaint's existing tap handles key-down only, so this is new work. Secure Input (password fields, some terminals) blocks interception; remaps simply do not apply there and Settings shows when Secure Input is active.

Hardware proof first (milestone 0): built-in MacBook Air keyboard and any external keyboard used; Fn/Globe detection; Fn+Delete & function keys preserved; Finder & text fields; sleep/wake; permission revoked & re-granted; Secure Input. Pick event tap or Karabiner-Elements from the results before building on it.

## Updates & distribution

- Mac: Codenotch Mac's existing self-updater, repointed to own release channel; dashboard, worker & CLI ship inside app bundle and update with it. Ad-hoc signing works for personal use but re-prompts permissions after rebuilds.
- Windows: one per-user installer containing native pill, dashboard, worker & CLI; pill runs updater check (Codenotch port's signed-update format), dashboard shows status. Unsigned installer works but shows SmartScreen warning.

**Signing & permission identity is decided in milestone 0, not at the end**, because every Mac permission test depends on it. macOS keeps permissions when the code's designated requirement stays the same (same signing identity & bundle ID); notarization alone does not preserve them, and development vs distribution certificates count as different identities. Decide: bundle IDs for app, dashboard, worker & CLI; one signing identity for all; which executable each permission is attributed to. Test early: Full Disk Access for worker started by pill vs CLI started from Terminal (Terminal-launched CLI otherwise inherits Terminal's permissions, not the app's); Apple Events sent in-process for correct attribution (as Vorssaint's Finder bridge already does); permissions surviving a signed upgrade.

Update safety: pill quiesces worker jobs before installing; all executables check they share one version and refuse mixed versions; settings & findings store migrations are versioned with rollback on failed update.

## Boundaries

- Shared Rust core: scan orchestration, tree, rule schema & findings store, accounting, history, resource sampling (`sysinfo` + platform readers), process actions, app inventory & leftover search, AI usage readers, settings model. Exposed to Swift via UniFFI, to Windows pill & Tauri directly, and through CLI.
- Mac-specific: `getattrlistbulk`, APFS sharing/allocation & snapshots, libproc & physical footprint, app identity & lifecycle quit, GPU readers, Trash, Full Disk Access, `simctl`, edge panel & conveniences (Swift).
- Windows-specific: process identity/counters & GPU Engine counters, filesystem allocation, hard links/reparse points, placeholder-safe enumeration, Recycle Bin, registered uninstallers, native topmost pill.
- Capability flags: unavailable counters display “Unavailable”; permission errors stay visible. Never present missing GPU readings as zero.
- No permanent background daemon beyond pill itself, no remote service, no administrator helper in first version. Petal's `admin.rs` stays disabled; snapshot deletion is separate reviewed action, never cleanup default. Machine-wide Windows uninstallers may raise their own UAC prompt.
- Scans & process trees never run in pill; pill shows scan-derived figures from last completed scan.
- Filesystem provider interface defined before extracting Petal: Petal's `scan.rs` imports Unix metadata & GPUI types directly, `disk.rs` assumes macOS/APFS and volume discovery enumerates `/Volumes`. Extraction therefore = algorithms (tree, aggregation, findings) behind a provider interface + Mac provider from Petal + new Windows provider. The Windows provider is a port, not a reuse.

## Runtime ownership

A shared library is not shared authority: pill, worker, dashboard & CLI are separate processes each linking core. One owner per responsibility:

| Responsibility | Owner | Others |
| --- | --- | --- |
| Live sampling (CPU, memory, volume totals), AI usage polling | Pill | Read pill's latest readings over local channel |
| Settings writes | Pill | Send change requests |
| Scans, findings refresh, plan creation, apply, uninstall | Worker (single instance, job queue) | Pill, dashboard & CLI submit jobs and subscribe to progress |
| Findings & history store | Worker writes; all read | SQLite in WAL mode, schema-versioned |
| Process list & Quit/Force Quit | Worker for CLI & dashboard | Pill never terminates processes |

- Local channel: Unix domain socket (Mac) / named pipe (Windows), per-user, versioned JSON messages; peers check they run the same version.
- Worker is started on demand by whichever client needs it (including CLI when pill is not running), holds an instance lock, exits after a few minutes idle. Only one mutation job runs at a time; read jobs may run concurrently.
- Jobs are journaled before they start; on crash or restart the worker resumes or marks each item's outcome from the journal. Cancellation is per job with per-item results.

## Storage expansion — DiskBuddy coverage (owner-approved 2026-10-06)

Adrian requested adding & implementing gaps identified against [DiskBuddy](https://diskbuddy.com/#inside). This expands product scope; the comparison describes advertised behavior, not independent qualification of DiskBuddy. Keep existing native-pill/on-demand-worker architecture, no always-on full scanner, no telemetry uploads, & no automatic cleanup. Existing feasibility gates still control activation of effects; source implementation proceeds while qualification is open.

| Capability | Required behavior & acceptance | Delivery order |
| --- | --- | --- |
| Storage overview/browser | Largest files/folders, sized map & list, scoped drilldown, kind/age colour, filters, inspector (logical/allocation, item counts, parent share, creation/modification times), Finder/Explorer reveal & platform preview. Unknown fields remain unknown; allocation differences are not automatically compression savings. | First storage batch, then native dashboard wiring |
| Filename search | Explicit-root filename index includes hidden entries when permission permits; substring/kind/extension/size/date filtering, bounded paging & keyboard navigation. Persistent incremental updates resume from native change-journal cursors; journal loss/overflow requires rescan & visible stale state. No content search or cloud hydration. Launcher can consume same index. | First metadata search, then persisted native updates |
| Folder growth | Per-volume/path added/removed/growing/shrinking folders between comparable snapshots; reject changed roots/volumes, incomplete or ambiguous reports; signed logical & allocated changes, never inferred reclaim. | First storage batch |
| Exact duplicates | Explicit opt-in content reads, size/sample narrowing & full content confirmation; configurable minimum size & bounded reads/deadline; stable keep-one/extras staging. Skip placeholders, symlinks/reparse points, unstable identity & known shared clones; hardlinks are one file. Revalidate before/after reads & again before actions. | First detection batch, then cleanup integration |
| Cleanup & Undo | Group caches/logs/builds/dependencies/downloads/media for review; never universal “safe”. Existing identity/effect/protection/liveness/expiry rules apply. Durable per-item journal & one-time claim; Trash/Recycle Bin by default. Whole-job Undo & later restore verify original/trash identities, refuse overwrites, report partial conflicts, & refuse missing/emptied Trash entries. No replay of indeterminate effects. | State engine first; journal/native executors after M0/M2 gates |
| Apps | Installed inventory & conservative bundle/related-file footprint, leftovers backed by install history/coverage, uninstall review; bounded per-app CPU/memory/I/O history with sampling coverage. Read-only update availability from explicit app feeds/Homebrew where supported & startup inventory; explicit user-selected switches later with identity-bound plans. Open-file/network-host inspection exposes permission limitations. | Inventory/history first; native adapters & actions next |
| Monitor | Existing resources plus network rates, per-volume storage, battery charge/health/cycles/temperature/power/time where supported, listening ports mapped to verified process incarnation/owner & network exposure. Quit/Stop only through existing process-action contract. Same readings feed native pill & dashboard; no second sampler. | Read-only models/adapters first; native/UI qualification next |
| Compression | Local image/video encoding with explicit format/quality/resize/target-size options, preview & measured before/after size. Preserve originals; no overwrite; publish only complete verified output, clean only job-owned temporary files on failure/cancel. Original-to-Trash is a separate reviewed cleanup plan. Mac uses ImageIO/AVFoundation; Windows uses supported native codecs with explicit unsupported results. | Portable contracts & Mac adapter first; Windows adapter/UI next |
| Activity | Filterable scan/cleanup/uninstall/compression timeline, weekly/monthly totals by action, per-item restore. Keep moved logical/allocated bytes separate from observed volume free-space change; restoring/emptying Trash updates appropriate events without invented reclaimed totals. | State/projection first; durable journal & dashboard next |
| External/network volumes | Mounted local disks already scoped; add mounted network shares explicitly for read-only inspection. Capability/permission/identity/timeouts visible; UNC scan support distinct from private metadata-store policy. No remote mutations without qualified volume identity & reversible native route. Disconnects remain unavailable. | Provider tests & platform qualification |

Implementation status is evidence-driven: a pure model is not a native adapter, a CLI command is not a finished dashboard, & hosted source tests are not hardware qualification. Track delivered source & hosted evidence in docs/runtime.md; keep remaining adapters/UI/activation work listed until delivered. First batch may develop independent new modules while earlier aggregate CI is resolving; root alone integrates shared exports/entrypoints & publishes aggregate CI. Lead reviews every worker diff after all lanes finish; no per-worker builds.

The complete scope above remains active after each batch. Continue through native adapters, persisted indexing/journals, UI integration & native hosted tests; release/signing/installation are separate owner-directed delivery states.

## Storage correctness before cleanup

Original review pinned Petal `ba07fd6` (0.1.0). Upstream main is now `f5e5b00` (0.4.1, 2026-10-04, 16 commits ahead), adding live FSEvents updates, snapshot & purgeable reporting, administrator reads, Finder-backed Trash & bundle-safe findings. Extract from current main; status of each finding there:

1. **Still present.** Trash confirmation says “This will free up X”, then removes items from chart after moving them. Same-volume Trash does not reclaim bytes until emptied. Label “Moved to Trash”; update current folder tree separately from volume free-space totals. [Cleanup](https://github.com/henrydennis/petal/blob/f5e5b00e42a841304fc437f87b5d4cfee4377d2e/src/app.rs#L1646)
2. **Still present.** Partial clones contribute private bytes only; deleting all related files can free shared extents too. This can understate savings. Hard-link handling returns before clone accounting, another combination needing verification. Treat reclaim as estimate unless supported sharing evidence qualifies it. [Accounting](https://github.com/henrydennis/petal/blob/f5e5b00e42a841304fc437f87b5d4cfee4377d2e/src/scan.rs#L580)
3. **Partly addressed.** Remainder is now “Snapshots and unreadable” when snapshots exist; still single `data_used − scanned` remainder clamped at zero, so clone over-count above volume usage stays hidden. Distinguish observed folder allocation, volume usage & unexplained remainder.
4. **Overestimation too, not only underestimation** (`frees_of`, same logic at `ba07fd6` & main). Hard-linked files return early and count full allocation once all links are selected, without checking whether the data is also a clone shared elsewhere. A failed private-size lookup or missing sharing info falls back to full allocation. Clone families are keyed by clone ID alone, not scoped by volume, so selections across volumes can merge unrelated families. Explicitly selected directory roots are opened without the dataless (cloud placeholder) check applied to nested directories. Fix: compose hard-link & clone ownership, scope every identity by volume, keep unknown/error as explicit states that widen the estimate's bounds instead of defaulting to “full size”, apply dataless check to roots.

**Four separate quantities, never merged:** logical size; attributed allocation (what folder views show); deletion estimate (lower & upper bound, with reason for any gap); observed volume change after action. Snapshot presence means retention is *unknown* for candidate data unless proven otherwise; show it as unknown. Show signed accounting discrepancy (scanned − volume used, which can be positive with clones) instead of clamping it. Growth history attributes each hard-linked file deterministically (e.g. lowest path), not by scan order, so bytes do not jump between folders across scans.

Petal PR #8 history confirms conservative classification matters: a “Safe to delete” `node_modules` finding removed files from 14 installed apps before it was fixed. Prefer “Review” to universal “Safe.”

**Snapshots & purgeable in reclaim reporting.** Bytes still referenced by a local Time Machine snapshot are not freed until that snapshot is thinned or deleted (this Mac holds 9+). Purgeable space also shifts reported free space independently. Reclaim estimate must state when snapshots hold candidate data; before/after readings must record snapshot list & purgeable value.

Require disposable-volume tests covering pure/partial clones, clone + hard-link combinations, failed metadata reads, snapshots, inaccessible folders, sparse files, directory aliases, symlinks to other volumes & cloud placeholders. Windows fixtures separately: NTFS hard links, compression, sparse files, junctions & mounted folders, exFAT, offline/read-only volumes, OneDrive placeholders. Deduplicate overlapping selections, preserve file identity across scan/action, revalidate before mutation, reject protected targets & handle partial cleanup results per item.

**Volumes & placeholders through every stage.** Findings & plans bind to durable volume identity (APFS volume UUID / NTFS volume serial), not mount path; a volume remounted elsewhere or replaced at the same path is a different target and the plan is refused. Disconnected volumes show “Unavailable”, not empty. No-hydration covers enumeration, size inspection, reclaim estimates, icons/previews & deletion. If Trash/Recycle Bin is unavailable on a volume, Trash plans are refused rather than silently deleting permanently. For cloud-synced folders, distinguish “remove local download” (keeps cloud copy) from “delete” (removes synced content everywhere); first version offers only the former.

Simulator runtimes need `simctl` lifecycle actions rather than generic folder removal; DeviceSupport versions may be removed per version but here live on external volume. WhatsApp media should use app-managed cleanup/settings; do not delete database containers. Chrome findings must check ownership/location & running-app state before suggesting cleanup and must report clone-aware unique bytes. Swap needs explanation of memory pressure, not deletion advice. Report before/after volume readings as observed change, not proof every byte came from our action.

## Detection rules

Every incident is a declarative rule in shared rule schema, so new leftovers become new rules rather than code changes. Each rule declares: path pattern & volumes, ownership/liveness check, age threshold, how to measure unique bytes, cleanup route, risk class (“Review” by default).

Initial rule pack:

| Rule | Liveness check before offering cleanup | Cleanup route |
| --- | --- | --- |
| Chrome signing copies | Offered only when no Chrome-family process (Chrome, Chrome for Testing, automation-launched instances) is running; otherwise shown as “in use, quit Chrome to clean”. Verified instance ownership can relax this later | Move to Trash; report clone-aware unique bytes |
| Orphaned build workspaces | Owning tool's lease/prune API when it has one (preferred); else owning tool's state says source root absent (not merely inaccessible), no active residents, volume of recorded root is mounted | Owning tool's own prune command when it has one; else Trash |
| Stale build temp & run folders | No open files, untouched for N days | Trash |
| Stale graph/index files | Generating tool not running; source repo gone or graph older than its replacement | Trash; regenerate on demand |
| Agent temp & model caches | Untouched for N days; model caches flagged “re-downloadable”, never “Safe” | Trash |
| Workspace trash folders | Older than N days | Delete only after explicit review (already discarded once) |
| Obsolete app backups | `*.app.prev-*`, `*.app.pre-*`, not running, newer app present | Trash |
| Xcode | Simulator runtimes/devices via `simctl`; DeviceSupport versions older than newest per device | `simctl`; Trash for DeviceSupport |
| App leftovers | Owning bundle ID / uninstall entry no longer installed; no running process | Trash (see Uninstall) |
| Swap, snapshots, WhatsApp | Explanation only (see above) | App-managed or none |

Scanning covers every mounted local volume, not just startup volume; external volumes report their own free space. Liveness checks (open files, running processes, owning tool state) run again immediately before any mutation.

**Liveness is three-valued: in use / not in use / unknown. Unknown is never eligible.** “Not observed” is not “safe”: a disconnected volume, permission error, renamed repository, paused job or incomplete process inspection all look like absence. Each check records how it knows:

- Path absent is distinguished from inaccessible and from volume not mounted; only confirmed absence counts.
- Open-file & process checks count only when inspection of all processes succeeded.
- Age uses last access/modification as a hint only; age alone never makes an item eligible, it only orders candidates.
- Nullable or missing fields in an owning tool's state file (as in captured RightKit workspace states) mean unknown, not inactive.

## Agent interface (CLI)

Same Rust core ships a command-line binary so coding agents (Claude Code, Codex) can investigate & clear space on request. JSON output on every command; human-readable by default.

| Command | Effect |
| --- | --- |
| `status` | Free/used per volume, purgeable, snapshots, swap |
| `scan [path…]` | Scan & store metadata snapshot; reports growth since previous |
| `findings [--rule …]` | Rule matches with unique bytes, liveness result, risk class, stable finding ids |
| `explain <finding-id>` | Why it exists, what created it, what cleanup does |
| `plan <finding-id…>` | Dry run: exact items, expected reclaim (estimate & reason), snapshot caveats; returns plan id |
| `apply <plan-id>` | Revalidates every item, executes cleanup route, reports per-item result & measured free-space change |
| `history` | Growth over time per folder & rule |
| `settings` | Opens Settings in dashboard |
| `procs [--sort cpu\|ram\|gpu]` | Grouped process list with stable ids (pid + start time) |
| `quit <proc-id>` / `force-quit <proc-id>` | Lifecycle quit, then explicit force; refuses changed identity |
| `apps` / `uninstall-plan <app>` | Installed apps; dry-run uninstall with all related files → plan id for `apply` |
| `usage` | Claude & ChatGPT usage percentages & reset times |

Safety contract (protects against accidental misuse by this tool and by agents using it; it is not containment for an agent that can already run arbitrary shell commands):

- Read commands never mutate.
- `apply` accepts only plan ids from `plan`, never raw paths. Plans expire after a short window; `apply` atomically claims a plan so it runs at most once.
- **Recursive protection.** Protected categories (system, app bundles in use, credential stores, databases, source repositories, user documents outside rule scope) are protected at any depth: a plan item whose subtree contains a protected descendant is refused unless the rule explicitly permits that descendant type (e.g. a build-output rule may permit `.git` inside a disposable build workspace, never a source repo). Inspection must complete for the whole subtree; incomplete inspection refuses the item.
- **Identity binding.** Each item records volume identity, file identity (inode/file ID), and type; traversal never follows symlinks, junctions or mount points. At apply time the worker re-walks the subtree without following links and refuses if any ancestor was replaced (e.g. by a symlink or junction), identities changed, or new protected descendants appeared since planning.
- **Plans bind effects, not just items.** Each plan fixes: action type (Trash, owner-tool command, uninstaller), exact executor & arguments, rule & rule version, reversibility. Changing any of these, including escalating Trash to permanent deletion, requires a new plan; `apply` has no escalation flag. Owner-tool commands (prune, `simctl`) are allowed only when their effect is bounded and previewed by the command itself (dry-run output stored in plan); otherwise item falls back to Trash.
- **Vendor uninstallers** run as a bounded operation: plan states “runs vendor uninstaller X”, cannot preview its file effects, and is followed by a fresh leftover scan producing a new plan.
- Journal before execution; per-item results; crash recovery from journal (see Runtime ownership).
- Every `apply` appends to local audit log readable by `history`.

Pill, dashboard & CLI read same core & findings store, so agent & UI never disagree. Thin MCP server wrapper is optional later; CLI first because every agent can already call it.

## Resources & task manager

Pill shows CPU & memory pressure (not raw “used”, since both OSes fill RAM with cache). Hover adds swap & GPU. Dashboard Resources screen:

- Apps with helpers grouped; expand to individual processes. Columns: CPU, memory, GPU, threads, start time.
- Sort any column; search; per-app history sparkline while dashboard is open.
- Actions: Quit, then explicit Force Quit; Reveal in Finder/Explorer; Uninstall shortcut.
- Same actions via CLI `procs`, `quit`, `force-quit`.

**Metric definitions are per platform and shown, not hidden behind identical labels.** Every reading carries source, age & capability. Memory: physical footprint on Mac (Activity Monitor convention), private working set on Windows; labelled accordingly; not compared across machines. Memory pressure: macOS pressure level on Mac; commit charge vs limit on Windows. CPU: 100 % = one core, with total-machine % alongside. GPU: Windows GPU Engine counters; Mac per-process GPU from accelerator statistics (as Vorssaint does) labelled approximate and never claimed equal to Activity Monitor; baselines reset when process identity or accelerator context changes so PID reuse cannot create false spikes.

**Grouping is presentational; actions target explicit, verified processes.**

- Quit/Force Quit act on a frozen list shown to the user (app root, or root + listed helpers), never on whatever the grouping heuristic finds at execution time; no recursive descendant killing.
- Identity = PID + start time, rechecked immediately before acting. On Windows, open a process handle and verify it, then act through that handle (no reuse race). On Mac, PID signalling after recheck still has a small race; use `NSRunningApplication` for apps and accept the residual risk for helpers.
- Quit: `NSRunningApplication.terminate` on Mac; on Windows `WM_CLOSE` to the app's top-level windows, which requests closure but does not guarantee exit and does not reach windowless processes. Wait a bounded time, then report “still running” or “no graceful quit available”. Never auto-escalate to Force Quit.
- Verify outcome; report permission denial; protected system processes never offered.

Helper-to-app grouping: macOS responsible-process attribution is private API; public fallback is bundle-path & parent-chain heuristics, which mis-group XPC services & launchd-spawned helpers; Stats & System Informer grouping code is reference. Per-process GPU on macOS has no public API (Activity Monitor-style figures come from undocumented IORegistry accelerator statistics); show “Unavailable” where unreadable. Windows exposes public per-process GPU Engine counters.

## Uninstall & app leftovers

Two flows, both in dashboard Uninstall screen and CLI:

1. **Uninstall an app:** quit it, then show bundle plus every related file found, each with size & reason; selected items go to Trash.
2. **Leftovers of already-deleted apps:** scan for files whose owning app is no longer installed, e.g. after dragging an app from Applications to Trash. Appears as Cleanup rule “App leftovers”.

| | Mac | Windows |
| --- | --- | --- |
| Identify app | Bundle ID, name, team ID; installed set from LaunchServices/Spotlight | Registered uninstall entries (HKLM/HKCU `Uninstall` keys), Store packages |
| Leftover locations | `~/Library/{Application Support, Caches, Preferences, Containers, Group Containers, Saved Application State, Logs, HTTPStorages, WebKit, LaunchAgents}`, `/Library/{LaunchAgents, LaunchDaemons, Application Support, Preferences}`, package receipts (`pkgutil`) | `%AppData%`, `%LocalAppData%`, `%ProgramData%`, Start Menu entries, scheduled tasks, services, registry keys (report only) |
| Removal | Trash; unload launch agents; daemons & system extensions reported with removal instructions (need admin) | Run app's own uninstaller first, then offer leftovers; never edit registry automatically in first version |

Matching is conservative: shared vendor folders (e.g. one company's folder used by several apps) and anything matching a still-installed app are excluded; ambiguous matches are “Review”, never pre-selected.

Installed-app inventories are incomplete (portable apps, apps on external drives, moved bundles, Spotlight indexing off, other users' installs), so “not found installed” is weak evidence. Therefore:

- Keep positive installation history: the worker records apps it has seen installed (bundle ID/uninstall entry, path, last seen). Leftovers are proposed mainly for apps seen installed before and now confirmed gone, not merely absent from today's inventory.
- Record inventory coverage (which sources answered, indexing state, external volumes mounted) with each finding; partial coverage makes ownership unknown → not eligible.
- Every match carries an ownership confidence & reason (exact bundle ID container vs name match vs team-ID vendor folder). Only exact-ID containers & caches can be pre-selected; Group Containers & vendor folders are Review.
- Application data is distinguished from user-created data (documents, exports, databases); user data is never pre-selected.
- Package receipts are evidence of what an installer wrote, not a list of files safe to remove.

## Release order

Executed by parallel agents with **one integration owner** (a lead agent that owns interfaces, merges lanes & runs acceptance) and explicit file/interface ownership per lane, so parallel agents do not produce incompatible implementations. Owner needed for macOS permission grants, Windows-machine checks & signing credentials.

**Now, no build:** install PowerToys on Windows (Left-Alt shortcut remaps + Command Palette on Alt+Space, PowerToys Run off); install stock Tinycast on Mac on Cmd+Space (Spotlight shortcut off) as stopgap until module ships; keep Vorssaint & CodexBar running.

**M0 — Feasibility gates (parallel spikes; results decide later design).** Each gate has a written pass/fail result before dependent work starts.
- Combined repo with four upstream subtrees pinned; weekly sync bot skeleton.
- Signing & permission identity matrix (bundle IDs, one signing identity, which executable gets which permission); Full Disk Access for worker & Terminal-launched CLI; permissions survive signed upgrade.
- Fn remap hardware proof on owner's keyboards → event tap or Karabiner.
- Donor extraction inventory at pinned commits (Vorssaint Finder/conveniences, Codenotch Mac pill & readers, Tinycast `AppCore`); single AppDelegate/service-registry design.
- Footprint baselines: stripped Codenotch Mac pill; native Windows pill prototype with one ring; Tinycast index loaded vs not. Budgets confirmed or revised.
- Fullscreen-hide semantics on both OSes against test matrix.
- Runtime ownership: worker, local channel, store schema, job journal.
- Filesystem provider interface; APFS fixture volumes (+ Windows fixtures when Windows machine is available).

**M1 — Read-only storage CLI.** `status`, `scan`, `findings`, `explain`, `history`, `usage`, `procs` on Mac; four-quantity accounting; three-valued liveness; rule pack in report-only mode. Agents can investigate immediately; nothing mutates.

**M2 — Bounded cleanup.** `plan`/`apply` with full safety contract (recursive protection, identity binding, effect binding, journal); Trash & bounded owner-tool actions only; adversarial fixture tests (ancestor swapped for symlink, protected descendant added after planning, remount, crash mid-apply).

**M3 — Surfaces.** Mac pill rings via UniFFI; Windows pill rings; Tauri dashboard screens (Resources read-only, Storage, Cleanup, Settings). Retire CodexBar.

**M4 — Mac conveniences & launcher.** Central event-tap service; Finder cut/paste + Fn shortcuts → maximizer & Dock click → Auto Quit (per-app opt-in); Tinycast launcher module with allowlist. Disable each Vorssaint feature as replacement passes its tests; retire Vorssaint after last one.

**M5 — Uninstall & process actions.** Uninstall flows with install history & ownership confidence; Quit/Force Quit with verified targets; Windows Command Palette extension.

**M6 — Windows parity release.** Windows filesystem provider, Recycle Bin, uninstallers, NTFS/placeholder fixtures on owner's Windows machine.

**Estimate:** M0 + M1 about 2–3 days wall-clock with parallel agents. Later milestones are re-estimated from M0 results; the earlier “5–8 days for everything” described a prototype, not the acceptance list below. Owner-only steps (permission grants, two-machine checks, 24 h footprint runs, signing credentials) are serial and set the pace more than coding.

## Reuse & upstream sync

One combined repository for this tool; each donor stays a tracked upstream, re-pulled weekly by a bot.

```
system-tool/
  upstream/vorssaint/    git subtree of vorssaint/vorssaint-utils
  upstream/codenotch/    git subtree of vinzdg/codenotch (Swift app + windows/)
  upstream/tinycast/     git subtree of abue-ammar/tinycast
  upstream/petal/        git subtree of henrydennis/petal
  core/                  shared Rust core + CLI (own code)
  mac/                   Mac app shell: imports upstream modules through adapters
  windows/               native Windows pill (own code)
  dashboard/             Tauri dashboard & settings (own code)
```

Rules that keep weekly pulls manageable:

- Own behaviour lives in `mac/`, `core/` etc. Upstream directories hold donor code; extraction (one composition root, removed donor updaters/hooks) means some upstream files are edited.
- One patch model: edits are committed directly inside `upstream/<donor>/` and git subtree merges carry them forward, with conflicts surfacing in the merge. Every such edit is listed in `upstream/<donor>/LOCAL-CHANGES.md` with reason. No separate patches folder.
- Each subtree records its upstream commit; updating is moving that pin. Toolchains & dependency locks (Swift, Rust, Tauri, UniFFI, Cargo.lock, Package.resolved) are pinned too.

Weekly bot (scheduled CI, Mac & Windows runners):

1. Fetch each upstream and open **one pull request per donor** (not one combined pull), so a failure is attributable to one donor.
2. Each PR builds Mac app, Windows pill, worker, dashboard & CLI; runs unit, fixture & smoke tests (pill launches, launcher opens, cut/paste fixtures, usage-reader fixtures, CLI plan/apply refusals).
3. Green → PR with upstream changelog summary for owner to merge (or auto-merge if owner prefers).
4. Conflict or failure → PR marked failing with log; a coding agent resolves and re-runs.
5. CI cannot exercise real permissions (TCC), Globe key, fullscreen or signed-in provider accounts. PRs touching those areas are labelled “needs machine check” and wait for a quick check on owner's Mac/Windows before merge.

Not every pull will be clean: Vorssaint, Codenotch & Tinycast are each built as a whole app, so upstream refactors will sometimes conflict with extraction edits. Bot + agent turns that into a reviewed fix instead of a silent break. Stats, Pearcleaner & Mole are referenced for code & rules; copy what is useful and re-check them occasionally rather than tracking as subtrees.

Pin Tauri, UniFFI & plugin versions; upgrade deliberately with Mac/Windows smoke checks.

## Deliverables & acceptance

This task delivers Markdown proposal, adversarial review & revision notes. Future implementation acceptance:

- Reproducible builds from recorded commits on both OSes.
- Settings opens identically on both OSes from settings orb, right-click menu, global shortcut & CLI.
- Pills stay within resource budget on both OSes over 24 h of normal use, measured per the Resource budget definitions (footprint/private bytes, children reported, return to baseline after dashboard closes).
- Dashboard screens identical on both OSes apart from Mac-only sections; metric labels platform-correct.
- Uninstall never pre-selects ambiguous, shared or user-created files; leftover rule proposes only apps with recorded install history, and fixtures with a portable/external-volume install of the “missing” app produce no eligible leftovers.
- Pill never steals focus; on right edge, topmost; hidden whenever a fullscreen window is on its display, including when focus is on another monitor, on both OSes, passing the visibility test matrix.
- Claude & ChatGPT rings agree with each provider's own `/usage` figures; missing data shows “Unavailable”; each reading shows its age.
- CLI: `apply` refuses raw paths, expired, reused or changed plans, protected descendants at any depth, swapped ancestors (symlink/junction), remounted volumes and incomplete inspection; each initial rule has fixture tests proving live and unknown items (running Chrome, active workspace, disconnected volume, permission-denied folder) are never offered; crash mid-apply recovers from journal with correct per-item results.
- On disposable APFS fixture, for each clone/hard-link/snapshot/failed-metadata case, observed free-space change falls within the plan's stated lower–upper bound; bounds per case are agreed in M0 and an “unknown” bound is allowed only where the case is inherently unknowable (e.g. snapshot retention).
- Fn remaps never leave a modifier stuck across 1,000 scripted press/release sequences; Finder cut/paste passes filename & cross-volume edge cases.
- Process actions only touch the frozen, verified target list; Quit never auto-escalates.
- Trash actions labelled as moves; no “frees” claim until emptied.
- No cloud-placeholder hydration (fixture with placeholder files shows no materialization).
- Permission failures visible; tested safe selection; accurate process identity across PID reuse.
- Each convenience module matches Vorssaint behaviour it replaces before that Vorssaint feature is disabled.
- Own updater works on both OSes; Mac permissions survive update.

Recommended next implementation milestone: M0 feasibility gates, then M1 read-only storage CLI.
