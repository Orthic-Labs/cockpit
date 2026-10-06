# Cockpit behavioral verification

Quality means proving complete user journeys against actual delivered app. New coverage must be a substantial end-to-end journey through a real product boundary. Retain focused security/accounting/component checks until equivalent journey evidence exists; do not delete them or add tests that mirror implementation, assert fixture construction, or pad counts.

## Installed Mac journey

`scripts/qa/mac-installed-journey.mjs` runs through `cua_repl` against actual installed AppKit/WKWebView app. It uses accessibility controls & real keyboard input; it never injects DOM state or calls dashboard test seams.

One journey covers native JSON import → repeat same export from another view → cancel without losing scan → Enter search with exactly one result → Search button with zero results & no stale rows → Clear → invalid import with visible error → recovery of prior scan → complete notch instruments → click to reopen with retained query/results → fullscreen/window transition. It fingerprints fixture files & installed binaries/assets before/after, saves actual screenshots/accessibility state at checkpoints, & preserves first failing state with a non-passing result.

Inputs must be a bounded real scanner export with multiple entries, malformed JSON, one uniquely matching filename, exact entry count & independently observed mounted-volume names. Keep input/output files ignored. Use existing bundled scanner with an explicit isolated `--state-dir` outside scanned tree when preparing export; never use real folders or account data as fixtures.

Inside `cua_repl`, after selecting installed app & loading tool documentation:

```js
var journey = await import(repoRoot + '/scripts/qa/mac-installed-journey.mjs');
var app = await cua.getApp('/Applications/Cockpit.app');
var result = await journey.runInstalledJourney(app, {
  appBundle: '/Applications/Cockpit.app',
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

`scripts/qa/mac-storage-installed-journey.mjs` adds a substantial real-desktop journey: native folder scan → changed-fixture rescan/growth → drilldown/inspector/search → opt-in duplicates → native ordinary-file Trash review/apply → app quit/relaunch → durable Undo → native compression pickers → app/resource/activity refresh. Fixture helper creates only new owner-only files beneath caller-selected Documents directory, on same volume as user Trash; it never overwrites existing files. SHA-256, volume/inode & bundle fingerprints verify preserved originals/restored items. Metric-specific hover uses a separate native observation callback; absent callback is explicitly unrun. Journey is authored, with execution evidence pending new installed candidate.

Generated RightKit CI runs existing component checks. Release candidate check launches bundled native app & real scanner, exercising packaged WKWebView search & window retention. This is package integration coverage; it bypasses native picker & does not replace installed journey. CUA journey requires an active native desktop session, so a hosted component pass never claims its completion.

Every behavioral change should first extend a relevant journey at observable failure boundary. Report delivered build identity, journey outcome & actionable failure; omit test-count celebrations.

## Verified installed journey — 2026-10-06

Full native journey passed on Developer ID signed preview. Assertions confirmed exact keyboard query replacement, one/zero/cleared result sets, same-file reimport, failed-import recovery, seven instruments for three volumes, 40 × 318 px resting notch, 1100 × 788 px window → 1600 × 818 px fullscreen → original window, retained scan/query/results & unchanged fixture/bundle fingerprints. Actual captures were reviewed. E2E exposed & drove fixes for stale failure status, missing standard Edit menu shortcuts & same-export reimport skipping change events. Earlier failing evidence is retained locally.
