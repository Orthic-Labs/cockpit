# Pulse behavioral verification

> **Historical (2026-10-07):** describes the removed Swift notch & JS dashboard. Current direction: [plan.md](plan.md).

Quality means proving complete user journeys against actual delivered app. New coverage must be a substantial end-to-end journey through a real product boundary. Retain focused security/accounting/component checks until equivalent journey evidence exists; do not delete them or add tests that mirror implementation, assert fixture construction, or pad counts.

## Installed Mac journey

`scripts/qa/mac-installed-journey.mjs` runs through `cua_repl` against actual installed AppKit/WKWebView app. It uses accessibility controls & real keyboard input; it never injects DOM state or calls dashboard test seams.

One journey covers native JSON import → repeat same export from another view → cancel without losing scan → Enter search with exactly one result → Search button with zero results & no stale rows → Clear → invalid import with visible error → recovery of prior scan → complete notch instruments → click to reopen with retained query/results → fullscreen/window transition. It fingerprints fixture files & installed binaries/assets before/after, saves actual screenshots/accessibility state at checkpoints, & preserves first failing state with a non-passing result.

Inputs must be a bounded real scanner export with multiple entries, malformed JSON, one uniquely matching filename, exact entry count & independently observed mounted-volume names. Keep input/output files ignored. Use existing bundled scanner with an explicit isolated `--state-dir` outside scanned tree when preparing export; never use real folders or account data as fixtures.

Inside `cua_repl`, after selecting installed app & loading tool documentation:

```js
var journey = await import(repoRoot + '/scripts/qa/mac-installed-journey.mjs');
var app = await cua.getApp('/Applications/Pulse.app');
var result = await journey.runInstalledJourney(app, {
  appBundle: '/Applications/Pulse.app',
  validScan: validScanPath,
  invalidScan: malformedScanPath,
  output: evidenceDirectory,
  entryName: 'example.txt',
  entryCount: expectedEntryCount,
  volumes: mountedVolumeNames,
  onCheckpoint: name => nodeRepl.write('E2E observed: ' + name),
});
nodeRepl.write(result);
```

Use a fresh evidence directory for every run; retain failure evidence before repair & rerun same journey. Review captured notch silhouette, icon count, table clipping & fullscreen/window dimensions before visual acceptance. Behavioral assertions & screenshots prove different things. Unobserved visual behavior stays open.

## Hosted checks

`scripts/qa/mac-storage-installed-journey.mjs` adds a substantial real-desktop journey: native folder scan → changed-fixture rescan/growth → drilldown/inspector/search → opt-in duplicates → native ordinary-file Trash review/apply → app quit/relaunch → durable Undo → native compression pickers → app/resource/activity refresh. Fixture helper creates only new owner-only files beneath caller-selected Documents directory, on same volume as user Trash; it never overwrites existing files. SHA-256, volume/inode & bundle fingerprints verify preserved originals/restored items. Metric-specific hover uses a separate native observation callback; absent callback is explicitly unrun. Installed execution uses fresh fixtures & retains first failing AX/screenshot. Native scrolling locates pagination & details beyond AX subtree truncation; exact popup labels & retained values prevent ambiguous selection. Escape falls back to observed native Quick Look close button. `continueAfterTrashAccessDenied: true` permits independent phases only after exact native Trash denial, records affected phases as blocked & can never yield a passing full journey; default remains fail-closed.

Generated RightKit CI runs existing component checks. Release candidate check launches bundled native app & real scanner, exercising packaged WKWebView search & window retention. This is package integration coverage; it bypasses native picker & does not replace installed journey. CUA journey requires an active native desktop session, so a hosted component pass never claims its completion.

Every behavioral change should first extend a relevant journey at observable failure boundary. Report delivered build identity, journey outcome & actionable failure; omit test-count celebrations.

## Verified installed journey — 2026-10-06

Full native journey passed on Developer ID signed preview. Assertions confirmed exact keyboard query replacement, one/zero/cleared result sets, same-file reimport, failed-import recovery, seven instruments for three volumes, 40 × 318 px resting notch, 1100 × 788 px window → 1600 × 818 px fullscreen → original window, retained scan/query/results & unchanged fixture/bundle fingerprints. Actual captures were reviewed. E2E exposed & drove fixes for stale failure status, missing standard Edit menu shortcuts & same-export reimport skipping change events. Earlier failing evidence is retained locally.

## Storage qualification observations — 2026-10-06

Signed candidate `b682deedef71ebf28473b71c4ff0ee44f4b0332c` completed installed journey with partial receipt: 18 phases passed, six blocked, one unrun. Observed scan/growth/search, index hidden/date/size filters & pagination, confirmed quit/relaunch replay, duplicate detection, image/video encoding, native Quick Look, durable compression Activity, app inventory/details, verified PID/start inspection & repeated persisted history append. Native video decoded as H.264, 96 × 64, 2 seconds. Monitor paged real process rows, reported native battery availability & verified listening-port identities; supplemental resample produced measured network rates. All 15 final fixture files, installed bundle fingerprints & disposable app identity/content were preserved.

Trash review returned `macOS denied access to Trash directory (errno 1)` before effects. Cleanup/tamper/Undo & disposable-app removal remain blocked; native liveness also refused running-app review with unknown process identity. Hover callback & mounted network share remain unrun. Full storage journey has no passing receipt. Earlier import/search/window journey passed independently. Failure evidence remains retained; current runner records original/bundle fingerprints on failures too.

## Release QA receipts (warn-and-record)

`right-release qa --stage source|installed --platform mac|win` (RightKit 0.2.146) runs `rightkit-qa run` on `rightkit-qa.toml` and writes a signed receipt. A missing or failed receipt only warns and is recorded in the sealed manifest; releases are not blocked. `right-release qa audit` lists the status per release. `right-release.config.mjs` `qa` maps evidence check names (the scenario `name`s) onto the floor: `smoke.launch` is `app.launch`, `journey` is `journey.*`.

- **Source** (`qa/release/source/`): `app.launch` (hub launches hidden, Overview renders, clean exit) and `journey.hub-sections` (all ten sections open, no error or broken text, Storage lists the fixture folder). They drive the debug `qa-native` hub, built as `scripts/gate.sh` builds it (`cd hub/src-tauri && cargo build --features qa-native,custom-protocol`, page bundled first; CI only). The runner is the `rightkit-qa` CLI (crate 0.2.14) on `PATH` or `qa.runner`. Run from a clean primary checkout: `right-release qa --stage source --platform mac`. `RIGHTKIT_QA_UI_BINARY` overrides the binary path. On Windows the hub needs plain drive paths, which `rightkit-qa` does not hand out (`\\?\C:\...`); see Gaps.
- **Installed**: `right-release qa --stage installed --platform mac|win --release <sealed-id>` with `RIGHTKIT_QA_UI_BINARY` set to the installed hub (`/Applications/Pulse.app/Contents/Helpers/Pulse.app`, `%LOCALAPPDATA%\Programs\Pulse\pulse-hub.exe`). It runs `qa/release/installed/app-launch.scenario.toml`, which fails today (see Gaps), so installed QA records as rejected.
- **Not in receipts**: the cargo `hub/qa-e2e` journeys. `Harness` writes one `evidence.json` per scenario (check names `hub sections render without errors`, `notch applies the hub's edge change` on Windows, `mac notch applies the hub's edge change and exits cleanly` on the Mac) and prints no `evidence <path>` line, and `rightkit-qa run` cannot execute a Rust test or start the notch. They stay the CI gate (`scripts/gate.sh` `run_hub_qa`) and are component-level evidence for the receipt.

Gaps: (1) the release hub has no control server (`qa-native` is a compile error outside debug), so `rightkit-qa` cannot drive an installed build; installed QA needs a release-safe driver, or OS-level automation like `scripts/qa/mac-installed-journey.mjs` (Mac) and a Windows UI Automation equivalent, wired in as a runner. (2) Nothing mounts a sealed DMG or runs the sealed NSIS installer for QA (`right-release install-app` is Mac only). (3) `rightkit-qa` canonicalises Windows paths to `\\?\C:\...` and TOML scenarios cannot rewrite them as `plain_workspace` does in `ui.rs`; the Windows source plan needs `rightkit-qa` to hand out plain paths or the `qa-native` hub to strip the prefix. (4) The source receipt is built where the qa-native hub exists (a CI runner after the gate); carrying it to the signing job that runs `right-release build` is not wired, so builds currently record `qa.status: missing`.
