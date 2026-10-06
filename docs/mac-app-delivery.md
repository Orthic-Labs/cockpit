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

Unsigned `Cockpit.app.zip` SHA-256: `0b4d4de104574eac639e82b5b78dfa0ac0c7deef6b836ce1819997dfdb96a4cb`; 997543 bytes. Downloaded bundle matches candidate stage summary. It is a validated development candidate, not a signed installer.

Protected signing failed before packaging: `APPLE_CERTIFICATE_BASE64`, certificate password, keychain password & notarization key were empty; `security import` reported `Unable to decode the provided data`. Cockpit's `release` environment has no secret names. Existing workspace Apple signer remains provisioned locally; this failure is missing repository CI bindings. No DMG, notarization, publication or local installation completed. Credential values were neither inspected nor transferred; workflow & RightKit infrastructure were preserved.

Attach existing Apple bindings to protected `release` environment before dispatching a fresh exact-source candidate. Failed signing attempt is configuration failure; retrying unchanged inputs cannot help. Source feature expansion remains deferred until installer delivery.
