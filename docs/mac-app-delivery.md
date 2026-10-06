# Cockpit macOS app delivery

macOS payload assembly uses source-only `scripts/release/mac-payload.mjs` modes. SwiftPM & Rust binaries must already exist; script never builds, signs with shell tools, notarizes, or publishes.

`candidate` assembles `Cockpit.app` under `$RIGHT_GIT_ARTIFACT_ROOT/cockpit/mac/`, copies dashboard assets, emits `app.js` from import-free module inside a classic-script closure for WKWebView file loading, writes `Cockpit.app.zip`, & preserves raw Mach-O files at `raw/Cockpit` & `raw/cockpit`. Inputs default to `mac/.build/release/cockpit-mac-prototype`, `target/release/cockpit`, & `dashboard/`; `COCKPIT_MAC_APP_BINARY`, `COCKPIT_CLI_BINARY`, & `COCKPIT_DASHBOARD_ROOT` override paths.

`prepare` adopts existing generated output from `$LEGION_UNSIGNED_CANDIDATE_ROOT` into `dist/staging/` without rebuilding & restores executable modes lost during GitHub artifact transport. Staged paths consumed by RightKit `sign.prePackageFiles` are:

- `dist/staging/Cockpit.app/Contents/MacOS/Cockpit`
- `dist/staging/Cockpit.app/Contents/Helpers/cockpit`
- `dist/staging/Cockpit.app/Contents/Resources/dashboard/`
- `dist/staging/raw/Cockpit`
- `dist/staging/raw/cockpit`

`package` seals staged app payload with published `@electron/osx-sign@2.7.1`, using only `APPLE_DEVELOPER_ID`, `release/entitlements.plist`, hardened runtime, disabled automatic entitlements & disabled Gatekeeper assessment. It then calls published `appdmg@0.6.6` with `release/appdmg.json` & writes `dist/releases/mac/Cockpit.dmg`. RightKit owns raw Mach-O signing, notarization, hardening, sealing, & publication; this repo adds no replacement mechanics.

Commands:

```sh
node scripts/release/mac-payload.mjs candidate
node scripts/release/mac-payload.mjs prepare
node scripts/release/mac-payload.mjs package
```

`@rightkit/release@0.2.111` remains release orchestrator; root release configuration owns its invocation & candidate admission.

## Verified delivery state — 2026-10-06

[Native CI 37397555119](https://github.com/Orthic-Labs/cockpit/actions/runs/37397555119) passed. [Candidate run 37397594286](https://github.com/Orthic-Labs/cockpit/actions/runs/37397594286) built Mac app, passed bundled scanner check, launched native notch & dashboard, imported real fixture scan, & verified rendered entries. Candidate source: `2588a8c25fd5649c4209767185d7190f9341e7bb`.

CI input `Cockpit.app.zip` SHA-256: `0b4d4de104574eac639e82b5b78dfa0ac0c7deef6b836ce1819997dfdb96a4cb`; 997543 bytes. Downloaded bundle matches candidate stage summary. This unsigned input was adopted into local signed preview packaging without rebuilding.

Protected signing failed before packaging: `APPLE_CERTIFICATE_BASE64`, certificate password, keychain password & notarization key were empty; `security import` reported `Unable to decode the provided data`. Cockpit's `release` environment has no secret names. Existing workspace Apple signer remains provisioned locally; this failure is missing repository CI bindings. This describes hosted signing failure; local preview delivery below supersedes installer blockage. Credential values were neither inspected nor transferred; workflow & RightKit infrastructure were preserved.

User explicitly authorized local or unsigned delivery on 2026-10-06. `package-local` reuses staged candidate, published signing SDK & appdmg with provisioned local Developer ID identity; it skips hosted-only qualification invocation & performs no compilation, notarization or publication. Pinned osx-sign 2.7.1 exports `sign`, which this adapter now uses.

Local signed preview delivered:

- `dist/releases/mac/Cockpit.dmg`, SHA-256 `4e2d0029f8899c56424f79ee138404ee14b7f9cda02e14632d3f83b324108dc9`.
- App & DMG signatures verified; nested scanner signature verified with strict deep verification. Developer ID team: `6KLGD3LLKF`.
- Mounted DMG contained Cockpit.app & Applications shortcut. Installed binaries match signed staged payload byte-for-byte.
- `/Applications/Cockpit.app` installed & launched. Installed smoke emitted `dashboard_smoke_pass`; native folder chooser loaded fixture & visible dashboard displayed 3 entries, 32 B logical & 4.00 KB attributed allocation.
- Signed local preview is not notarized or published. Hardware qualification & retained feature expansion continue separately.



## Installed UI repair — 2026-10-06

Owner rejected M0 debug panel & subsequent oversized donor-scale notch. Current preview adapts donor bezel outline to 26 pt rings with centered metric icons & exact percentages on hover, 40 pt resting depth with height derived from full instrument count (40 × 318 pt for seven rings), hover details, dashboard click & icon-only menu-bar status entry. MIT attribution ships at `Contents/Resources/codeNOTCH-LICENSE.txt`.

Dashboard fixes add working Search submission, native JSON open panel, fixed table columns, independent content scrolling & retained scan state across window close/reopen. Bundled native smoke now exercises rendered one/zero-match searches & close/reopen retention.

Owner-authorized local Swift rebuild reused hosted scanner build; Developer ID app & DMG signatures passed strict verification. Installed compact app emitted `dashboard_smoke_pass`. Manual installed UI checks covered JSON import/cancel/error, searches, folder inspector, scrolling, fullscreen, retained scans & notch opening. Current DMG SHA-256: `48e72ffdf759b3c37cefd34f3be6d9355f0b0a583955bafa3428377e3b72b66b`. Local screenshot evidence remains ignored; this records installed UI repair, not completion of pending product modules.

Full set restored: Claude, ChatGPT/Codex, CPU, native memory pressure & one free-space ring per mounted local volume. Provider marks reuse MIT donor outlines. Read-only default-profile readers use no-prompt Claude keychain lookup & bounded Codex auth-file read, with concurrent quota GETs, no redirects/cookies/cache, 15-second resource deadlines, 1 MiB response cap, 429 backoff & explicitly stale retained readings. No token refresh or credential writes. Installed UI showed seven rings for three mounted volumes; ChatGPT returned live usage & Claude returned Unavailable. Installed full-set scanner/search/retention smoke passed; native fixture chooser loaded three entries.

Installed E2E recovery repair: standard native Edit menu restores Cmd+A/C/X/V keyboard routing; dashboard navigation restores loaded-scan status after rejected import; resetting file input permits same export to be chosen repeatedly. Reusable native journey in `scripts/qa/mac-installed-journey.mjs` passed with actual dialogs, keyboard input, complete instrument readings & captured window dimensions; see docs/testing.md. Native binary rebuilt locally under existing owner authorization; hosted scanner reused. Final app & DMG strict signatures passed.

## Storage candidate adoption — 2026-10-06

[Hosted candidate 37451708406](https://github.com/Orthic-Labs/cockpit/actions/runs/37451708406) passed at source `03de859209e235ecc96164b6c2c648f14b0bb319`, including 105 Swift tests & packaged native dashboard checks. Candidate ZIP SHA-256 is `caeb8bf53fc391948b64deeac45dc55d8f24fb77c9970dd6189ce4eecc9823ea`; all extracted bundle files matched hosted stage summary before signing.

Existing local preview packaging signed exact candidate with Developer ID team `6KLGD3LLKF`. App & DMG strict signature checks passed; DMG SHA-256 is `bfe39f8fe5e30dd5fa608c55c24e7931263df55cec61742a8d3aaae24bb383c4`. Installed `/Applications/Cockpit.app` matches signed staged binaries & dashboard assets. Installed storage journey is in progress; early chooser-harness failures are retained. No notarization or publication performed.


## Dashboard route repair adoption — 2026-10-06

[Hosted candidate 37455199423](https://github.com/Orthic-Labs/cockpit/actions/runs/37455199423) passed at source `e631454320dcbd1355bd70986c2e53d4911e8f24`: native release build, Rust runtime/Clippy, 105 Swift declarations & packaged eight-route dashboard journey. ZIP SHA-256: `e42e9c69b3ae3702f2b8d84aade94826048d5b0993de58e4996f94f5263dd595`. Every hosted bundle file matched stage summary before signing.

Exact candidate was signed with existing Developer ID team `6KLGD3LLKF` & installed. Entire installed regular-file tree matches signed stage; strict app & DMG signatures pass. DMG SHA-256: `5bcfaa817335dca07b7a12f839ff4e918dcdc619c6f92ed6a1bca9321395a617`. Prior signed bundle was preserved by copy across volumes. No notarization/publication.

Installed journey passed launch/native fixture scan, then failed Monitor accessibility traversal: pixels render content but its process subtree is unavailable to AX. All eight route screenshots were captured. Complete storage effects/restart journey remains unproven; source pagination repair is pending hosted & installed qualification. Six current notch instruments expose metric-specific AX readings; per-ring hover remains unrun.
