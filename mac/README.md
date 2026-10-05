# Cockpit macOS native pill — M0 prototype

This is a feasibility spike: one AppKit edge panel per display, no WebView, tray, Dock icon, input hooks, settings, installation, signing, updater, or provider telemetry. It paints native CPU, memory, and local mounted-volume free-space rings; unavailable or first-sample counters show `--`. Each display is keyed by its CoreGraphics UUID. The panel joins all Spaces with `fullScreenNone`; it never opts into `fullScreenAuxiliary`.

Visibility uses `AXFullScreen` when Accessibility is already trusted. An explicit `false` is respected. Missing metadata gets a conservative borderless-window geometry fallback. If Accessibility is denied, no prompt is shown and the pill stays visible. Visible sampling/drawing runs every 2 seconds; hidden fullscreen displays sample every 10 seconds. The prototype does not claim the M0 footprint gate passes.

CI-only checks (macOS runner; do not run locally per `cockpit/AGENTS.md`):

```sh
swift package describe --package-path mac
swift build --package-path mac -c release
```

Unverified gates: real macOS Spaces/native fullscreen behavior, non-AppKit fullscreen heuristic, display reconnect/DPI changes, TCC denial behavior, physical footprint, and 10-minute CPU average. Donor extraction remains pending the pinned inventory/licence review.
