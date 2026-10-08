# Pulse macOS app delivery

Current payload: Codenotch fork, Tauri hub & Rust CLI, assembled only in hosted RightKit workflows. `candidate` stages `$RIGHT_GIT_ARTIFACT_ROOT/pulse/mac/Pulse.app`; `prepare` adopts it into `dist/staging/`; `package` writes `dist/releases/mac/Pulse.dmg`. Build outputs can be overridden with `PULSE_NOTCH_APP`, `PULSE_HUB_APP` & `PULSE_CLI_BINARY`; legacy env names remain fallbacks. See [rename & migration](rename-pulse.md).

Signed payload paths:

- `Pulse.app/Contents/MacOS/Pulse`
- `Pulse.app/Contents/Helpers/pulse`
- `Pulse.app/Contents/Helpers/Pulse.app/Contents/MacOS/pulse-hub`
- `Pulse.app/Contents/Helpers/PulseHelper`
- `Pulse.app/Contents/Helpers/pulse-elevate`
- `Pulse.app/Contents/Library/LaunchDaemons/dev.orthic.pulse.helper.plist`

RightKit owns signing, notarization, packaging & publication. Records below describe prior Pulse builds; artifact names, hashes & repo links stay historical.

## Upgrading

Nothing is deleted on upgrade. The first Pulse launch moves existing data:

- The legacy Application Support folder moves to Pulse's folder only when Pulse's folder does not exist yet. If both exist, Pulse uses its own folder and the legacy folder stays intact. Failed moves stop startup and state writes, preserving the original data.
- Preferences, account readings, drive health and archives carry over, as does the hub's native data. The CLI migrates default state before it writes. Explicit `--state-dir` paths stay as given.
- The uninstall helper shows “Off after rename” in the hub. Re-enable it under General > Uninstalling, then approve **Pulse helper** in Login Items. Best-effort cleanup of the legacy helper registration is retried on later launches.
- Re-approve **Pulse notch** in Accessibility and Input Monitoring for any enabled conveniences.

## Verified delivery state — 2026-10-06

[Native CI 37397555119](https://github.com/Orthic-Labs/pulse/actions/runs/37397555119) passed. [Candidate run 37397594286](https://github.com/Orthic-Labs/pulse/actions/runs/37397594286) built Mac app, passed bundled scanner check, launched native notch & dashboard, imported real fixture scan, & verified rendered entries. Candidate source: `2588a8c25fd5649c4209767185d7190f9341e7bb`.

CI input `Pulse.app.zip` SHA-256: `0b4d4de104574eac639e82b5b78dfa0ac0c7deef6b836ce1819997dfdb96a4cb`; 997543 bytes. Downloaded bundle matches candidate stage summary. This unsigned input was adopted into local signed preview packaging without rebuilding.

Protected signing failed before packaging: `APPLE_CERTIFICATE_BASE64`, certificate password, keychain password & notarization key were empty; `security import` reported `Unable to decode the provided data`. Pulse's `release` environment has no secret names. Existing workspace Apple signer remains provisioned locally; this failure is missing repository CI bindings. This describes hosted signing failure; local preview delivery below supersedes installer blockage. Credential values were neither inspected nor transferred; workflow & RightKit infrastructure were preserved.

User explicitly authorized local or unsigned delivery on 2026-10-06. `package-local` reuses staged candidate, published signing SDK & appdmg with provisioned local Developer ID identity; it skips hosted-only qualification invocation & performs no compilation, notarization or publication. Pinned osx-sign 2.7.1 exports `sign`, which this adapter now uses.

Local signed preview delivered:

- `dist/releases/mac/Pulse.dmg`, SHA-256 `4e2d0029f8899c56424f79ee138404ee14b7f9cda02e14632d3f83b324108dc9`.
- App & DMG signatures verified; nested scanner signature verified with strict deep verification. Developer ID team: `6KLGD3LLKF`.
- Mounted DMG contained Pulse.app & Applications shortcut. Installed binaries match signed staged payload byte-for-byte.
- `/Applications/Pulse.app` installed & launched. Installed smoke emitted `dashboard_smoke_pass`; native folder chooser loaded fixture & visible dashboard displayed 3 entries, 32 B logical & 4.00 KB attributed allocation.
- Signed local preview is not notarized or published. Hardware qualification & retained feature expansion continue separately.



## Installed UI repair — 2026-10-06

Owner rejected M0 debug panel & subsequent oversized donor-scale notch. Current preview adapts donor bezel outline to 26 pt rings with centered metric icons & exact percentages on hover, 40 pt resting depth with height derived from full instrument count (40 × 318 pt for seven rings), hover details, dashboard click & icon-only menu-bar status entry. MIT attribution ships at `Contents/Resources/codeNOTCH-LICENSE.txt`.

Dashboard fixes add working Search submission, native JSON open panel, fixed table columns, independent content scrolling & retained scan state across window close/reopen. Bundled native smoke now exercises rendered one/zero-match searches & close/reopen retention.

Owner-authorized local Swift rebuild reused hosted scanner build; Developer ID app & DMG signatures passed strict verification. Installed compact app emitted `dashboard_smoke_pass`. Manual installed UI checks covered JSON import/cancel/error, searches, folder inspector, scrolling, fullscreen, retained scans & notch opening. Current DMG SHA-256: `48e72ffdf759b3c37cefd34f3be6d9355f0b0a583955bafa3428377e3b72b66b`. Local screenshot evidence remains ignored; this records installed UI repair, not completion of pending product modules.

Full set restored: Claude, ChatGPT/Codex, CPU, native memory pressure & one free-space ring per mounted local volume. Provider marks reuse MIT donor outlines. Read-only default-profile readers use no-prompt Claude keychain lookup & bounded Codex auth-file read, with concurrent quota GETs, no redirects/cookies/cache, 15-second resource deadlines, 1 MiB response cap, 429 backoff & explicitly stale retained readings. No token refresh or credential writes. Installed UI showed seven rings for three mounted volumes; ChatGPT returned live usage & Claude returned Unavailable. Installed full-set scanner/search/retention smoke passed; native fixture chooser loaded three entries.

Installed E2E recovery repair: standard native Edit menu restores Cmd+A/C/X/V keyboard routing; dashboard navigation restores loaded-scan status after rejected import; resetting file input permits same export to be chosen repeatedly. Reusable native journey in `scripts/qa/mac-installed-journey.mjs` passed with actual dialogs, keyboard input, complete instrument readings & captured window dimensions; see docs/testing.md. Native binary rebuilt locally under existing owner authorization; hosted scanner reused. Final app & DMG strict signatures passed.

## Storage candidate adoption — 2026-10-06

[Hosted candidate 37451708406](https://github.com/Orthic-Labs/pulse/actions/runs/37451708406) passed at source `03de859209e235ecc96164b6c2c648f14b0bb319`, including 105 Swift tests & packaged native dashboard checks. Candidate ZIP SHA-256 is `caeb8bf53fc391948b64deeac45dc55d8f24fb77c9970dd6189ce4eecc9823ea`; all extracted bundle files matched hosted stage summary before signing.

Existing local preview packaging signed exact candidate with Developer ID team `6KLGD3LLKF`. App & DMG strict signature checks passed; DMG SHA-256 is `bfe39f8fe5e30dd5fa608c55c24e7931263df55cec61742a8d3aaae24bb383c4`. Installed `/Applications/Pulse.app` matches signed staged binaries & dashboard assets. Installed storage journey is in progress; early chooser-harness failures are retained. No notarization or publication performed.


## Dashboard route repair adoption — 2026-10-06

[Hosted candidate 37455199423](https://github.com/Orthic-Labs/pulse/actions/runs/37455199423) passed at source `e631454320dcbd1355bd70986c2e53d4911e8f24`: native release build, Rust runtime/Clippy, 105 Swift declarations & packaged eight-route dashboard journey. ZIP SHA-256: `e42e9c69b3ae3702f2b8d84aade94826048d5b0993de58e4996f94f5263dd595`. Every hosted bundle file matched stage summary before signing.

Exact candidate was signed with existing Developer ID team `6KLGD3LLKF` & installed. Entire installed regular-file tree matches signed stage; strict app & DMG signatures pass. DMG SHA-256: `5bcfaa817335dca07b7a12f839ff4e918dcdc619c6f92ed6a1bca9321395a617`. Prior signed bundle was preserved by copy across volumes. No notarization/publication.

Installed journey passed launch/native fixture scan, then failed Monitor accessibility traversal: pixels render content but its process subtree is unavailable to AX. All eight route screenshots were captured. Complete storage effects/restart journey remains unproven; source pagination repair is pending hosted & installed qualification. Six current notch instruments expose metric-specific AX readings; per-ring hover remains unrun.

## Storage inspection repair adoption — 2026-10-06

[Hosted candidate 37470104466](https://github.com/Orthic-Labs/pulse/actions/runs/37470104466) & [aggregate CI 37470080918](https://github.com/Orthic-Labs/pulse/actions/runs/37470080918) passed exact source `b682deedef71ebf28473b71c4ff0ee44f4b0332c`. Repairs admit exact integral WK query numbers, explain native Trash access denial & distinguish JSON schema version 1 from actual CFBoolean during resource-history reload. Candidate ZIP SHA-256: `4f490e9c58a56454029258032db2d1bb99dec935f81efc50fef08ee89976fc8d`. All 13 hosted summary artifacts matched hashes/sizes before signing.

Existing Developer ID team `6KLGD3LLKF` signed exact hosted payload. Installed `/Applications/Pulse.app` matches all 11 regular files in signed stage; app & DMG strict signatures pass. DMG SHA-256: `c6dd140d16431eb64d9f09a2b681ae770ef5286c8c819f74852fdeb55665919c`. Prior signed app/stage/DMGs preserved; no local compilation, notarization or publication performed.

Installed storage journey completed with partial receipt: 18 phases passed, six Trash-dependent phases blocked, hover unrun. Native scan/growth/index/restart replay/duplicates, image/video encoding & previews, durable Activity, app pagination/details/resource-history reload, process pagination/battery/listening ports passed. Supplemental network resample rendered measured rates. All 15 fixture files & disposable app remain intact. Real Trash directory access returned EPERM before review/effects; native liveness separately refused running-app review with unknown process identity. Network-share qualification lacks mounted fixture. No full storage completion is claimed.
