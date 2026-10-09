# Pulse Windows notch — M1

Native Rust (`windows` crate) notch, no WebView, no tray or taskbar item, never takes focus. One borderless layered window per monitor, flush with the top edge, drawn by a small software rasteriser (anti-aliased rings, GDI text masks) and published with `UpdateLayeredWindow` (per-pixel alpha). `WS_EX_NOACTIVATE` plus `WM_MOUSEACTIVATE` keeps focus where it is.

## What it shows

Five rings with the Mac notch's metrics and colour bands (green under 70%, orange under 90%, red above): CPU busy share, memory in use, system-drive used share, Claude usage and Codex usage (main ring = session / 5-hour window, thin inner ring = weekly window). Unavailable readings show `--` and draw no arc. Percent label sits under each ring.

- Hover card per ring (dark solid): CPU busy and logical processors; memory in use, available and commit; every fixed drive with free space (system drive first); Claude and Codex windows with percent, reset countdown, plan and reading age. Stale readings (last poll failed) are dimmed and dated.
- Click opens the hub (`monitor` for CPU/memory, `storage` for disk, `accounts` for AI), see `src/hub.rs`.
- Right-click shows only Quit, drawn as a non-activating card (no focus change). Clicking elsewhere dismisses it.
- Alt-drag moves the notch along its monitor's top edge. The position is stored per monitor (per mille of the monitor width) and written atomically (write-then-rename, restricted DACL).
- Launch at login: `HKCU\...\Run\Pulse`, off by default (pre-install audit, 8f09e6a); turn it on with hub General › Open Pulse at login, or `"launch_at_login": true` in the settings file.
- Hidden whenever the topmost visible window on the notch's monitor is a borderless full-monitor window (not foreground-only). Sampling slows to 10 s while hidden and nothing is drawn.

## Keyboard layer (`src/keys.rs`, `src/shot.rs`)

One `WH_KEYBOARD_LL` hook on its own thread (physical events only; injected events and our own tagged events pass through).

- Mac-style editing: Alt+A / C / V / X / Z act as Ctrl+A / C / V / X / Z, Alt+Shift+Z as Ctrl+Y (redo). The key is swallowed and Ctrl+key is injected in one `SendInput` batch with Alt (and Shift) released around it and dummy `VK_E8` presses before and after, so no stuck modifier and no menu activation on the Alt release. AltGr / Ctrl+Alt, Alt+Tab, Alt+F4 and Alt+Space are untouched. Setting `"mac_shortcuts": false` turns it off.
- Screenshots: Alt+Shift+4 drags a region (crosshair, dim outside, live size label; Space switches to window pick, Esc or right click cancels); Alt+Shift+5 shows a toolbar (Entire Screen, Window, Selection, destination Desktop / Clipboard, Cancel). Capture uses `BitBlt` from the screen DC after the overlay is gone (per-monitor DPI aware, all monitors, at most 8192 px per side). PNG (WIC) goes to the Desktop as `Screenshot YYYY-MM-DD at HH.MM.SS.png` and the image goes to the clipboard as `CF_DIB`; destination Clipboard skips the file. No sound. A non-activating thumbnail card shows bottom right for 5 s; clicking it opens the file. Overlays never take focus (Esc and Space arrive through the hook). `"screenshot_shortcuts": false` disables the feature, `"screenshot_to_desktop": false` makes clipboard-only the default (the toolbar choice is saved on exit). Win+Shift+S (Snipping Tool) is not touched.
- `keys::set_extra_handler` is the hook point for other Alt chords (for example a send shortcut).

## AI usage sources

Read-only, in memory, never refreshed or written, never logged: `%USERPROFILE%\.claude\.credentials.json` (or `CLAUDE_CONFIG_DIR`) with `GET api.anthropic.com/api/oauth/usage`, and `%USERPROFILE%\.codex\auth.json` (or `CODEX_HOME`) with `GET chatgpt.com/backend-api/wham/usage`. Polled every 5 minutes on a worker thread (WinHTTP), 60 s doubling back-off to 15 min after a 429, expired or missing sign-in shows "Sign in needed" and is not retried in a loop. Not yet ported: Claude Desktop cache and `claude /usage` sources, reset credits, spend windows.

## Settings

`%LOCALAPPDATA%\Pulse\pill-settings.json`, schema version 1 (`visible`, `cadence_seconds`, `monitors`, plus optional `launch_at_login`, `positions`, `mac_shortcuts`, `screenshot_shortcuts`, `screenshot_to_desktop`). Same directory convention as `pulse-core`.

## Hub side (not changed here)

The notch starts `pulse-hub.exe --section <name>` (next to the notch exe, `hub\`, `Helpers\` or `%LOCALAPPDATA%\Programs\Pulse`) and tracks the child. To switch an already running hub, the hub should create manual-reset-free named events `Local\dev.orthic.pulse.hub.show.<section>` and show that section when one is signalled (the Windows equivalent of the Darwin notifications in `hub/src-tauri/src/lib.rs`). Until then a visible hub window is brought forward and a hidden one is restarted on the section. The hub also needs a Windows single-instance guard so a hub started by other means is not duplicated.

## CI-only checks (Windows runner; do not run locally per `AGENTS.md`)

```powershell
cargo fmt --manifest-path windows/Cargo.toml -- --check
cargo test --manifest-path windows/Cargo.toml
cargo build --manifest-path windows/Cargo.toml --release
```

Unverified gates: nothing in M1 has been compiled yet; native fullscreen/video/game matrix, mixed-DPI/display reconnect, private-bytes and 0.5% CPU budget, layered-window rendering across Windows versions, durable monitor identity (`szDevice` is still the key). No fake telemetry or footprint claim is made.
