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

