# Pulse Windows notch — M1

Native Rust (`windows` crate) notch, no WebView, no tray or taskbar item, never takes focus. One borderless layered window per monitor, flush with the top edge, drawn by a small software rasteriser (anti-aliased rings, GDI text masks) and published with `UpdateLayeredWindow` (per-pixel alpha). `WS_EX_NOACTIVATE` plus `WM_MOUSEACTIVATE` keeps focus where it is.

## What it shows

Five rings with the Mac notch's metrics and colour bands (green under 70%, orange under 90%, red above): CPU busy share, memory in use, system-drive used share, Claude usage and Codex usage (main ring = session / 5-hour window, thin inner ring = weekly window). Unavailable readings show `--` and draw no arc. Percent label sits under each ring.

- Hover card per ring (dark solid): CPU busy and logical processors; memory in use, available and commit; every fixed drive with free space (system drive first); Claude and Codex windows with percent, reset countdown, plan and reading age. Stale readings (last poll failed) are dimmed and dated.
- Click opens the hub (`monitor` for CPU/memory, `storage` for disk, `accounts` for AI), see `src/hub.rs`.
- Right-click shows only Quit, drawn as a non-activating card (no focus change). Clicking elsewhere dismisses it.
- Alt-drag moves the notch along its monitor's top edge. The position is stored per monitor (per mille of the monitor width) and written atomically (write-then-rename, restricted DACL).
- Launch at login: `HKCU\...\Run\Pulse`, on by default; set `"launch_at_login": false` in the settings file to turn it off.
- Hidden whenever the topmost visible window on the notch's monitor is a borderless full-monitor window (not foreground-only). Sampling slows to 10 s while hidden and nothing is drawn.

## AI usage sources

Read-only, in memory, never refreshed or written, never logged: `%USERPROFILE%\.claude\.credentials.json` (or `CLAUDE_CONFIG_DIR`) with `GET api.anthropic.com/api/oauth/usage`, and `%USERPROFILE%\.codex\auth.json` (or `CODEX_HOME`) with `GET chatgpt.com/backend-api/wham/usage`. Polled every 5 minutes on a worker thread (WinHTTP), 60 s doubling back-off to 15 min after a 429, expired or missing sign-in shows "Sign in needed" and is not retried in a loop. Not yet ported: Claude Desktop cache and `claude /usage` sources, reset credits, spend windows.

## Settings

`%LOCALAPPDATA%\Pulse\pill-settings.json`, schema version 1 (`visible`, `cadence_seconds`, `monitors`, plus optional `launch_at_login` and `positions`). Same directory convention as `pulse-core`.

## Hub side (not changed here)

The notch starts `pulse-hub.exe --section <name>` (next to the notch exe, `hub\`, `Helpers\` or `%LOCALAPPDATA%\Programs\Pulse`) and tracks the child. To switch an already running hub, the hub should create manual-reset-free named events `Local\dev.orthic.pulse.hub.show.<section>` and show that section when one is signalled (the Windows equivalent of the Darwin notifications in `hub/src-tauri/src/lib.rs`). Until then a visible hub window is brought forward and a hidden one is restarted on the section. The hub also needs a Windows single-instance guard so a hub started by other means is not duplicated.

## CI-only checks (Windows runner; do not run locally per `AGENTS.md`)

```powershell
cargo fmt --manifest-path windows/Cargo.toml -- --check
cargo test --manifest-path windows/Cargo.toml
cargo build --manifest-path windows/Cargo.toml --release
```

Unverified gates: nothing in M1 has been compiled yet; native fullscreen/video/game matrix, mixed-DPI/display reconnect, private-bytes and 0.5% CPU budget, layered-window rendering across Windows versions, durable monitor identity (`szDevice` is still the key). No fake telemetry or footprint claim is made.
