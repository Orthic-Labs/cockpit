# Pulse Mac services (salvage)

The previous Swift notch & WKWebView dashboard were removed on 2026-10-07. The notch is now a fork of Codenotch in [`Notch/`](Notch/FORK.md) (see [docs/plan.md](../docs/plan.md), phase 1).

This package keeps native services pending review for reuse by the new notch or Tauri hub: compression (ImageIO/AVFoundation), filename index, app inspection & update checks, power details, duplicate revalidation, activity projection & native cleanup. Native cleanup bypasses core plan/apply checks and must not ship as-is. `pulse-probe` measures notch footprint.

This package is not built in CI: it depended on the removed dashboard host (`ScanRequest`, `ProcessScanRunner`). Pieces move into the notch or hub only after review.
