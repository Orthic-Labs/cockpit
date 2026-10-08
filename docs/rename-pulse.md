# Pulse rename — 2026-10-08

| Identifier family | Previous → current |
| --- | --- |
| Product, wordmark, menus, alerts, help | Cockpit → Pulse; Tanker unchanged |
| App & installer | Cockpit.app / Cockpit.dmg → Pulse.app / Pulse.dmg |
| Hub name & window title | Cockpit / Cockpit Hub → Pulse Hub; wordmark Pulse |
| Bundle/signing/log identifiers | dev.orthic.cockpit{,.hub,.helper,.elevate,.dmg} → dev.orthic.pulse with same suffix; team 6KLGD3LLKF unchanged |
| Notch project, target, scheme & executable | Cockpit → Pulse; Cockpit.entitlements → generated Pulse.entitlements |
| Helper target, binary & XPC protocol | CockpitHelper / CockpitHelperProtocol → PulseHelper / PulseHelperProtocol |
| Helper shared constants | cockpitHelperMachService / cockpitTeamID → pulseHelperMachService / pulseTeamID |
| Elevation target & binary | CockpitElevate / cockpit-elevate → PulseElevate / pulse-elevate |
| CLI, core package/library | cockpit / cockpit-core / cockpit_core → pulse / pulse-core / pulse_core |
| Hub package, library & binary | cockpit-hub / cockpit_hub → pulse-hub / pulse_hub; QA package cockpit-hub-qa → pulse-hub-qa |
| Root package & release app key | cockpit → pulse; installer key pulse/installers/mac/current/Pulse.dmg |
| Launch daemon | dev.orthic.cockpit.helper.plist → dev.orthic.pulse.helper.plist; Label & MachServices dev.orthic.pulse.helper; BundleProgram Contents/Helpers/PulseHelper |
| Darwin notifications | dev.orthic.cockpit.{notch.state,hub.command,hub.show.settings,hub.show.storage,hub.show.monitor,hub.show.accounts,hub.show.appearance,hub.show.notifications,hub.show.general} → dev.orthic.pulse with same suffix |
| Internal queue labels | dev.orthic.cockpit.{eventtap,finder-cutpaste} → dev.orthic.pulse with same suffix |
| State roots | ~/Library/Application Support/Cockpit → Pulse; %LOCALAPPDATA%/Cockpit → Pulse; $XDG_STATE_HOME/cockpit or ~/.local/state/cockpit → pulse |
| Hub native storage | dev.orthic.cockpit.hub → dev.orthic.pulse.hub beneath Library/{Application Support,Caches,WebKit}; preferences domain likewise |
| Worker IPC | cockpit-worker-<SID> pipe → pulse-worker-<SID>; runtime cockpit directory → pulse; Mac run/worker.sock beneath Pulse |
| SwiftPM salvage targets/paths | CockpitMacPrototype{,Core,CoreTests}, CockpitProbe / cockpit-probe → PulseMacPrototype{,Core,CoreTests}, PulseProbe / pulse-probe |
| Probe record kinds | cockpit.{probe.sample,footprint,footprint.header,footprint.sample,footprint.summary} → pulse with same suffix; parser also reads legacy probe.sample |
| Windows prototype identifiers | cockpit-windows-prototype, cockpit-windows diagnostics, CockpitM0NativePill, CockpitM0Controller, Cockpit M0 titles, Local\\Cockpit.Pill.v1 → corresponding Pulse names |
| Login-item sentinel | cockpitLoginItemDefaulted → pulseLoginItemDefaulted; original key retained during migration |
| Environment overrides | COCKPIT_{SCAN_LOG,NOTCH_APP,CLI_BINARY,HUB_APP,CHECK_APP,HUB_BIN,QA_SHOTS,SKIP_HUB_QA,TEST_HELPER,APFS_FIXTURE,APFS_FIXTURE_STATE,APFS_FIXTURE_ROOT,APFS_FIXTURE_BASELINE_USED,APFS_FIXTURE_SNAPSHOT} → PULSE_*; legacy reads remain fallbacks |
| QA launch environment | RIGHTKIT_COCKPIT_QA_HOME → RIGHTKIT_PULSE_QA_HOME, legacy fallback |
| CI output markers & compile-time binary lookup | COCKPIT_{FORMAT_PATCH,CARGO_LOCK,WINDOWS_LOCK}_{BEGIN,END} → PULSE_*; CARGO_BIN_EXE_cockpit → CARGO_BIN_EXE_pulse |
| Packaging & temporary paths | product subdirectory cockpit/mac → pulse/mac; raw/Cockpit & raw/cockpit → raw/Pulse & raw/pulse; owned temporary prefixes use pulse |

First notch launch moves legacy Application Support folder by exclusive rename only when Pulse folder is absent. All contents survive, including scan.log, notch-state.json, hub-commands/, drive-health.json, chrome-snapshots.json, apps-activity.json, cleanup-activity.json, snapshots & activity ledgers. Empty Pulse UserDefaults domain receives every legacy key; existing Pulse domain is untouched. Hub migrates before creating its webview; CLI migrates default state before writes. Explicit CLI state directories stay explicit. Both folders existing means Pulse uses its own folder & leaves legacy folder intact. Failed moves stop startup/state writes & preserve original data.

Legacy helper unregister is best-effort, retried on later notch launches. Replacement remains off, shown as “Off after rename” in hub, until user re-enables it. Bridge marks fresh state with product Pulse; CLI & hub never trust enabled helper status copied from legacy snapshot. Re-approve **Pulse helper in Login Items** & **Pulse notch in Accessibility/Input Monitoring** for enabled conveniences. Existing preferences, account readings & archives remain retained.

GitHub repo/remote Orthic-Labs/cockpit, .rightgit.json workflow identities, release workflow names, cockpit-rust cache key & cockpit-candidate workflow artifact root remain unchanged. Vendor contents, third-party notices & historical plan/delivery/test evidence retain original identities.

Test inventory unchanged: 390 declarations before/after (257 core, 96 Windows, 29 Node, seven Swift salvage, one hub UI journey). Existing component/integration/service declarations: 389; existing hub UI journey: one. Core durable-storage & two native salvage service journeys count as component evidence, not installed E2E coverage. Installed journeys executed: zero. No declarations added/deleted or replacement coverage claimed; no builds/tests run. Three retained Swift files moved from mac/Tests/CockpitMacPrototypeCoreTests/ to mac/Tests/PulseMacPrototypeCoreTests/: MediaCompressionTests.swift (five names), NativeCleanupJourneyTests.swift (testReviewApplyRestartUndoConflictAndAncestorProtection), NativeStorageServicesTests.swift (testNativeCompressionJourneyPersistsActivityAndProtectsExistingOutput). Original test names remain unchanged; historical inventories stay frozen.

[Remaining-hit audit](rename-pulse-grep.md) explains every requested grep result.
