# Pulse Windows notch — M1

Native Rust (`windows` crate) notch, no WebView, no tray or taskbar item, never takes focus. One borderless layered window per monitor, flush with the top edge, drawn by a small software rasteriser (anti-aliased rings, GDI text masks) and published with `UpdateLayeredWindow` (per-pixel alpha). `WS_EX_NOACTIVATE` plus `WM_MOUSEACTIVATE` keeps focus where it is.

## What it shows

Five rings with the Mac notch's metrics and colour bands (green under 70%, orange under 90%, red above): CPU busy share, memory in use, system-drive used share, Claude usage and Codex usage (main ring = session / 5-hour window, thin inner ring = weekly window). Unavailable readings show `--` and draw no arc. Percent label sits under each ring.

- System card (CPU and memory rings share it), as the Mac's: CPU and memory pressure bars, then GPU busy share and network rates, and the temperature at the title's right. A sensor this PC cannot read (fans, CPU temperature) is left out, not printed as "unavailable". The only temperature an unelevated process gets is the NVIDIA GPU's, read through the driver's `nvml.dll` (System32 only, handles cached, every ~12 s on the sensors thread) and labelled "GPU 57 °C"; Thermal Zone counters, `MSAcpi_ThermalZoneTemperature` and `Win32_Fan` give nothing usable on the PC probed (no zones / "Not supported" / no speed).
- Send card hover (the Mac's rows): optional "Copy last: <text>" button with its age at the right, "Nearby sharing" with its status, one row per device (name left, kind right), "Paste clipboard" and the hint line. No rescan button (the click card's round refresh button rescans, and opening that card with no devices rescans). Every label, value and button text is cut with an ellipsis to fit its row or plate; button rows that would pass the card are narrowed in proportion.
- Hover card per ring (dark solid): CPU busy and logical processors; memory in use, available and commit; every fixed drive with free space (system drive first); Claude and Codex windows with percent, reset countdown, plan and reading age. Stale readings (last poll failed) are dimmed and dated.
- Click opens the hub (`monitor` for CPU/memory, `storage` for disk, `accounts` for AI), see `src/hub.rs`.
- Right-click shows only Quit, drawn as a non-activating card (no focus change). Clicking elsewhere dismisses it.
- Settings button and move grip (open notch only): the resting arc at the trailing end fills in as a disc with a gear when the pointer reaches it, and the six-dot grip comes out beside it (the Mac's orb and grip). Clicking the disc opens the hub on `settings` (the Mac's section, shown as Accounts); pressing the grip drags the notch along or between edges with no Alt. The zones are one-in-255 black pixels, because a layered window passes the pointer through fully transparent pixels. The window is 46 DIPs longer than the shape for them. Everything stays `WS_EX_NOACTIVATE`.
- Size: the hub's Size (small 0.8, medium 1, large 1.25) or Custom size (0.5 to 1.5) scales the whole notch, cards included, by multiplying each monitor's DPI, so it stays per-monitor DPI aware.
- Show: Always show keeps the notch open, On hover rests as the pill and opens at the pointer (changing to it folds an open notch at once), Hidden draws nothing and shows no cards.
- Alt-drag moves the notch along its monitor's top edge. The position is stored per monitor (per mille of the monitor width) and written atomically (write-then-rename, restricted DACL).
- Launch at login: `HKCU\...\Run\Pulse`, off by default (pre-install audit, 8f09e6a); turn it on with hub General › Open Pulse at login, or `"launch_at_login": true` in the settings file.
- Hidden whenever the topmost visible window on the notch's monitor is a borderless full-monitor window (not foreground-only). Sampling slows to 10 s while hidden and nothing is drawn.

## Keyboard layer (`src/keys.rs`, `src/shot.rs`)

One `WH_KEYBOARD_LL` hook on its own thread (physical events only; injected events and our own tagged events pass through).

- Mac-style editing: Alt+A / C / V / X / Z act as Ctrl+A / C / V / X / Z, Alt+Shift+Z as Ctrl+Y (redo). The key is swallowed and Ctrl+key is injected in one `SendInput` batch with Alt (and Shift) released around it and dummy `VK_E8` presses before and after, so no stuck modifier and no menu activation on the Alt release. AltGr / Ctrl+Alt, Alt+Tab, Alt+F4 and Alt+Space are untouched. On by default (matches Cmd+A/C/V/X/Z on the Mac); setting `"mac_shortcuts": false` or hub General › Mac-style editing turns it off.
- Screenshots: Alt+Shift+4 drags a region (crosshair, dim outside, live size label; Space switches to window pick, Esc or right click cancels); Alt+Shift+5 shows a toolbar (Entire Screen, Window, Selection, destination Desktop / Clipboard, Cancel). Capture uses `BitBlt` from the screen DC after the overlay is gone (per-monitor DPI aware, all monitors, at most 8192 px per side). PNG (WIC) goes to the Desktop as `Screenshot YYYY-MM-DD at HH.MM.SS.png` and the image goes to the clipboard as `CF_DIB`; destination Clipboard skips the file. No sound. A non-activating thumbnail card shows bottom right for 5 s; clicking it opens the file. Overlays never take focus (Esc and Space arrive through the hook). `"screenshot_shortcuts": false` disables the feature, `"screenshot_to_desktop": false` makes clipboard-only the default (the toolbar choice is saved on exit). Win+Shift+S (Snipping Tool) is not touched.
- `keys::set_extra_handler` is the hook point for other Alt chords (for example a send shortcut).

## AI usage sources

Read-only, in memory, never refreshed or written, never logged: `%USERPROFILE%\.claude\.credentials.json` (or `CLAUDE_CONFIG_DIR`) with `GET api.anthropic.com/api/oauth/usage`, and `%USERPROFILE%\.codexuth.json` (or `CODEX_HOME`) with `GET chatgpt.com/backend-api/wham/usage`. Polled every 5 minutes on a worker thread (WinHTTP), 60 s doubling back-off to 15 min after a 429, expired or missing sign-in shows "Sign in needed" and is not retried in a loop.

The Claude ring follows the account Claude Desktop is signed into while Desktop runs (Claude Code's account, `oauthAccount.accountUuid` in `.claude.json`, otherwise). Desktop counts as running while it holds `%APPDATA%\Claude\lockfile`; its account is `lastKnownAccountUuid` in `%APPDATA%\Claude\config.json` (only that member is scanned for; the sign-in material in the file is never parsed).
- Desktop's account differs from Claude Code's: the Claude Code login, the usage endpoint and its back-off all describe the other account and are not used. The numbers come only from the usage response Desktop itself cached (`GET /api/organizations/<org>/usage`) for the organizations filed under that account (folder names under `claude-code-sessions\<account>` and `local-agent-mode-sessions\<account>`). That cache is Chromium's block-file HTTP cache, `%APPDATA%\Claude\Cache\Cache_Data\data_N`, read through shared handles and never written; the body is zstd-encoded and is decoded by `zstd.rs`, dated by its `Date:` header. A reading must be at most 30 minutes old, not older than the moment the account was last seen to change (organizations can be shared between accounts) and have no window past its reset. Otherwise the ring is empty and the card and hub row say "No reading for <name> yet" (the name chosen in Pulse's account registry, else `Claude <first 8 of the id>`); the reason is logged once to `notch.log` as `claude_desktop_cache`. Another account's numbers are never shown in its place, and a held reading is dropped when the tracked account changes.
- Desktop runs as the same account, and the Claude Code login cannot answer (expired token): the same cache answers for that account.
- Desktop is not running: the Claude Code login as before (an expired token still reads "Sign-in expired; use the app once to refresh"). The cache is not consulted then, because it could be another account's.

Codex reads, as the Mac does, `rate_limit` (primary = main ring, secondary = inner ring; labels come from the window length the server sends, so a plan with only a weekly window shows only "Weekly limit"), Spark and code-review windows from `additional_rate_limits` / `code_review_rate_limit`, the `spend_control.individual_limit` cap as a "Credits" window for seats with no rolling windows, `credits` (card row "Available credits"), the identity token's `chatgpt_subscription_active_until` ("Plan active until") and, from `GET /backend-api/wham/rate-limit-reset-credits`, the unused-resets rows (best effort).

Not yet ported: `claude /usage` source, Claude reset credits and spend windows. Unverified on a running notch: the compiled build (CI only).

## Claude accounts and the restart button

The Claude row of `notch-state.json` carries `claudeAccounts`, the list the hub's Accounts section shows: every account folder under `%APPDATA%\Claude\claude-code-sessions` and `local-agent-mode-sessions` (names only), default name `Claude <first 8 of the id>`, Active = the tracked account (Desktop's `lastKnownAccountUuid` while it runs, else Claude Code's), ordered active, last reading, folder time, each with the last saved reading from `%LOCALAPPDATA%\Pulse\claude-account-usage.json` (`claude_accounts.rs`, the Mac's schema; written only by the notch). The hub's `renameClaudeAccount` and `forgetClaudeAccount` commands are applied there (forget only for an account whose folder is gone). The "N accounts never seen signed in" folding is the hub's. `lastKnownAccountUuid` and whether Desktop runs are watched every 3 s (1 s for a minute after the button) and a change refetches the Claude reading.

The Claude hover card has a small round button right of its header (`claude_restart.rs`): `WM_CLOSE` to Claude Desktop's frames only (a process counts as Desktop by its image path under `%LOCALAPPDATA%\AnthropicClaude\`, never by the name `claude.exe`, which Claude Code also uses), wait up to 20 s, `Helpers\pulse.exe claude sync --apply --json` hidden, then reopen Desktop; spinner, a check for ~2 s, or a red mark with the reason. Desktop only quits on `WM_CLOSE` when its tray is off (`preferences.menuBarEnabled` false); with the tray on the button reports that and leaves Desktop running. Nothing is force-killed.

## Settings

`%LOCALAPPDATA%\Pulse\pill-settings.json`, schema version 1 (`visible`, `cadence_seconds`, `monitors`, plus optional `launch_at_login`, `positions`, `mac_shortcuts`, `screenshot_shortcuts`, `screenshot_to_desktop`, `edges`, `edge`, `folds`, `notch_size`, `uses_custom_notch_scale`, `custom_notch_scale`, `notification_channel`). Same directory convention as `pulse-core`.

- Notification channel: the hub's Notifications › Where › Channel (`notificationChannel`, options `notch` and `mac`, the Mac's raw values; written as `"notification_channel":"mac"` and omitted at the default `notch`). `notch` shows the alert cards with their sound; `mac` raises a Windows system toast for each alert (finished agent, usage thresholds, resets and limits, drive alerts) with the sound and keeps the notch quiet. The toast is raised by a hidden `powershell.exe` through WinRT `ToastNotificationManager` under Windows PowerShell's own application id (Pulse registers no Start-menu shortcut or AUMID yet, so the toast reads as from "Windows PowerShell"); if it cannot be shown only the sound plays.
- Updates: `GET api.github.com/repos/Orthic-Labs/pulse/releases/latest` (as the Mac's `ReleaseFeed`). A 404 means the repository has no published release yet and a release without `Pulse-Setup-x64.exe` has no Windows build to offer; both read as `upToDate` with the message "No Windows release yet" (log `update_check result=no_release`), not as a failure.

## Hub side (not changed here)

The notch starts `pulse-hub.exe --section <name>` (next to the notch exe, `hub\`, `Helpers\` or `%LOCALAPPDATA%\Programs\Pulse`) and tracks the child. To switch an already running hub, the hub should create manual-reset-free named events `Local\dev.orthic.pulse.hub.show.<section>` and show that section when one is signalled (the Windows equivalent of the Darwin notifications in `hub/src-tauri/src/lib.rs`). Until then a visible hub window is brought forward and a hidden one is restarted on the section. The hub also needs a Windows single-instance guard so a hub started by other means is not duplicated.

## CI-only checks (Windows runner; do not run locally per `AGENTS.md`)

```powershell
cargo fmt --manifest-path windows/Cargo.toml -- --check
cargo test --manifest-path windows/Cargo.toml
cargo build --manifest-path windows/Cargo.toml --release
```

Unverified gates: nothing in M1 has been compiled yet; native fullscreen/video/game matrix, mixed-DPI/display reconnect, private-bytes and 0.5% CPU budget, layered-window rendering across Windows versions, durable monitor identity (`szDevice` is still the key). No fake telemetry or footprint claim is made.
