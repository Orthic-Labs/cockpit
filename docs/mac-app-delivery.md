# Cockpit macOS app delivery

macOS payload assembly uses source-only `scripts/release/mac-payload.mjs` modes. SwiftPM & Rust binaries must already exist; script never builds, signs with shell tools, notarizes, or publishes.

`candidate` assembles `Cockpit.app` under `$RIGHT_GIT_ARTIFACT_ROOT/cockpit/mac/`, copies dashboard assets, writes `Cockpit.app.zip`, & preserves raw Mach-O files at `raw/Cockpit` & `raw/cockpit`. Inputs default to `mac/.build/release/cockpit-mac-prototype`, `target/release/cockpit`, & `dashboard/`; `COCKPIT_MAC_APP_BINARY`, `COCKPIT_CLI_BINARY`, & `COCKPIT_DASHBOARD_ROOT` override paths.

`prepare` adopts existing generated output from `$LEGION_UNSIGNED_CANDIDATE_ROOT` into `dist/staging/` without rebuilding. Staged paths consumed by RightKit `sign.prePackageFiles` are:

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
