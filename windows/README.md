# Pulse Windows native pill — M0 prototype

This is a native Rust `windows` crate feasibility spike. It creates one borderless layered Win32 pane per monitor, paints one CPU ring with memory and system-volume counters, and has no WebView, tray, taskbar integration, input hook, settings, installer, updater, or focus activation. Unavailable or first-sample counters show `--`. `WS_EX_NOACTIVATE` plus `WM_MOUSEACTIVATE` prevents focus steal. Updates invalidate panes only when sampled readings change.

Fullscreen occupancy enumerates visible top-level windows in z-order for each pane's monitor. It ignores owned/tool/desktop/cloaked windows, requires borderless style, and requires coverage of the complete monitor. This is a conservative heuristic; it does not use foreground-window-only state. Monitor identity uses `MONITORINFOEXW.szDevice` in this spike and needs a durable device identity decision before production.

CI-only checks (Windows runner; do not run locally per `AGENTS.md`):

```powershell
cargo fmt --manifest-path windows/Cargo.toml -- --check
cargo test --manifest-path windows/Cargo.toml
cargo build --manifest-path windows/Cargo.toml --release
```

Unverified gates: native fullscreen/video/game matrix, mixed-DPI/display reconnect, private-bytes and 0.5% CPU budget, layered-window rendering across Windows versions, and durable monitor identity. No fake telemetry or production footprint claim is made.
