# Cockpit Mac services (salvage)

The previous Swift notch & WKWebView dashboard were removed on 2026-10-07. The notch is now a fork of Codenotch in [`Notch/`](Notch/FORK.md) (see [docs/plan.md](../docs/plan.md), phase 1).

This package keeps native services pending review for reuse by the new notch or Tauri hub: compression (ImageIO/AVFoundation), filename index, app inspection & update checks, power details, duplicate revalidation, activity projection & native cleanup. Native cleanup bypasses core plan/apply checks and must not ship as-is. `cockpit-probe` measures notch footprint.

CI-only checks (macOS runner; do not run locally per `cockpit/AGENTS.md`):

```sh
swift build --package-path mac -c release
```
