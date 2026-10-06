# Cockpit notch — Codenotch fork

Forked from [vinzdg/codenotch](https://github.com/vinzdg/codenotch) at `72fb2169ef316834ad632f85de27a415cd0d2298` (MIT, see `LICENSE`). The pristine donor stays in `upstream/codenotch` for diffing; this copy is Cockpit's own.

Build: `xcodegen generate --spec mac/Notch/project.yml --project mac/Notch`, then `xcodebuild -scheme Cockpit` on the `xcode-27` runner (macOS 27, Xcode 27). CI only (see `scripts/gate.sh`).

## Local changes

| Area | Change | Files |
| --- | --- | --- |
| Identity | Name Cockpit, bundle ID `dev.orthic.cockpit`, version 0.2.0, unsigned CI build (RightKit signs releases) | `project.yml`, string literals |
| Presence | `LSUIElement`; accessory from launch; Settings never promotes to a Dock app; status item never shown; app-icon picker removed | `project.yml`, `App/AppDelegate.swift`, `Settings/SettingsWindowController.swift`, `Settings/SettingsView.swift` |
| Right-click | One item: Quit Cockpit | `Notch/NotchWindowController.swift` |
| Updater | Sparkle removed; `Updater` is an inert stub so no Codenotch update can install over Cockpit | `App/Updater.swift`, `project.yml` |
| Providers | Claude and Codex only (other provider code still compiled, not instantiated) | `App/AppDelegate.swift` |
| System cells | System: CPU main ring, memory pressure inner ring. Disks: internal drive main ring, first external drive inner ring, all drives in the hover card, re-read each refresh. `ProviderKind.system`, 2 s refresh, no archive, no refresh spinner, excluded from usage alerts; SF Symbol glyphs | `System/SystemProviders.swift`, `Model/UsageStore.swift`, `Providers/UsageProvider.swift`, `Providers/ProviderGlyph.swift`, `Settings/Preferences.swift`, `App/AppDelegate.swift` |
| Second ring | Second reading drawn as a thinner ring inside the main ring by default (Codenotch's "Inside" option), also while an agent works | `Settings/Preferences.swift`, `Features/ProviderRing.swift` |
| Platform | Minimum macOS 26 (Liquid Glass always available); built with Xcode 27 | `project.yml` |
| Visibility | Always shown by default; one Codex ring (default `~/.codex` profile) | `Settings/Preferences.swift`, `App/AppDelegate.swift` |
| Activity | No spinning arc while an agent works; pulse kept for finished/waiting | `Features/ProviderRing.swift` |
| First run | No What's New, no first-run Settings window | `App/AppDelegate.swift` |
| Tests | Codenotch's unit tests not carried over | `project.yml` |

## Not yet done

- Remove unused provider, phone-link and costs code.
- Sampling does not yet slow while the notch is hidden for fullscreen.
- Clicking the notch should open the hub (phase 2).
