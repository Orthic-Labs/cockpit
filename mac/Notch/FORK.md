# Cockpit notch — Codenotch fork

Forked from [vinzdg/codenotch](https://github.com/vinzdg/codenotch) at `72fb2169ef316834ad632f85de27a415cd0d2298` (MIT, see `LICENSE`). The pristine donor stays in `vendor/codenotch` for diffing; this copy is Cockpit's own.

Build: `xcodegen generate --spec mac/Notch/project.yml --project mac/Notch`, then `xcodebuild -scheme Cockpit` on the `xcode-27` runner (macOS 27, Xcode 27). CI only (see `scripts/gate.sh`).

## Local changes

| Area | Change | Files |
| --- | --- | --- |
| Identity | Name Cockpit, bundle ID `dev.orthic.cockpit`, version 0.2.0, unsigned CI build (RightKit signs releases) | `project.yml`, string literals |
| Presence | `LSUIElement`; accessory from launch; Settings never promotes to a Dock app; status item never shown; app-icon picker removed | `project.yml`, `App/AppDelegate.swift`, `Settings/SettingsWindowController.swift`, `Settings/SettingsView.swift` |
| Right-click | One item: Quit Cockpit | `Notch/NotchWindowController.swift` |
| Updater | Sparkle removed; `Updater` is an inert stub so no Codenotch update can install over Cockpit | `App/Updater.swift`, `project.yml` |
| Providers | Claude and Codex only. Every other provider (Cursor, Antigravity, GLM, MiniMax, Grok, Devin, OpenCode, Command Code, Copilot, Kimi, Kiro, Amp, Apify, Kilo, Ollama, LM Studio, Gemini, DeepSeek, Qianwen, custom endpoints, web-session sign-in) is deleted, with its activity monitors, glyph assets and outlines, and the local-model/cost/usage-detail fields of `ProviderSnapshot` and the card layout. Provider glyphs left: Claude, OpenAI, CPU, memory, disk | `App/AppDelegate.swift`, `Providers/`, `Sessions/`, `Model/`, `Features/`, `Notch/`, `Settings/Preferences.swift`, `Assets.xcassets` |
| Trimmed | PhoneLink (folder, server, SwiftNIO dependency, ATS and local-network plist keys), Costs (folder, "What used it" card section), the never-shown menu-bar status item and its preferences, the app-presence preference, Codenotch's string resources and licences for removed icons | `project.yml`, `Info.plist`, `Settings/Preferences.swift` |
| Hidden sampling | System and Disks refresh every 10 s (not 2 s) while the notch is hidden: visibility Hide, or folded away by a full-screen app on every display. Back to 2 s, with an immediate reading, when it shows again | `Model/UsageStore.swift`, `Notch/NotchFleet.swift`, `Notch/NotchWindowController.swift`, `App/AppDelegate.swift` |
| System cells | System cell (memory pressure + CPU) and Disks cell (external + internal, all drives in the hover card, re-read each refresh). `ProviderKind.system`, 2 s refresh, no archive, no refresh spinner, excluded from usage alerts; SF Symbol glyphs | `System/SystemProviders.swift`, `Model/UsageStore.swift`, `Providers/UsageProvider.swift`, `Providers/ProviderGlyph.swift`, `Settings/Preferences.swift`, `App/AppDelegate.swift` |
| Ring pairs | Main (outer) ring: weekly limit, external drive, memory pressure. Thin inner ring: five-hour session, internal drive, CPU. Weekly leads by default; inner ring stays visible while an agent works | `Settings/Preferences.swift`, `Features/ProviderRing.swift`, `System/SystemProviders.swift` |
| Platform | Minimum macOS 26 (Liquid Glass always available); built with Xcode 27 | `project.yml` |
| Visibility | Always shown by default; one Codex ring (default `~/.codex` profile) | `Settings/Preferences.swift`, `App/AppDelegate.swift` |
| Activity | No spinning arc while an agent works; pulse kept for finished/waiting | `Features/ProviderRing.swift` |
| Size | Small by default, % labels off; compact body (less padding above, below and between rings, slightly less at the sides); inner ring hugs the main track | `Settings/Preferences.swift`, `Notch/NotchLayout.swift` |
| Hub bridge | Settings + accounts published to `~/Library/Application Support/Cockpit/notch-state.json`; hub commands applied from `hub-commands/`; Darwin notifications both ways. Settings handle opens the hub | `System/HubBridge.swift`, `System/HubLauncher.swift`, `App/AppDelegate.swift` |
| Clicks | A click on the open notch opens the hub: disks ring → Storage, system ring → Monitor, account ring → Accounts, elsewhere → Storage | `Notch/NotchWindowController.swift`, `Notch/NotchFleet.swift`, `App/AppDelegate.swift` |
| Login item | Open at login on by default, once, for the copy in /Applications | `App/AppDelegate.swift` |
| Settings window | Codenotch's Settings and What's New windows removed; every route that opened them opens the hub | `App/AppDelegate.swift`, `Settings/` |
| Claude fallback | When Claude Desktop's cache is the only source and older than 30 min, show its last reading marked with its age instead of "Sign in" (unless a window has reset); keep the last reading through rescan waits | `Providers/ClaudeOAuthProvider.swift` |
| First run | No What's New, no first-run Settings window | `App/AppDelegate.swift` |
| Launcher | Option+Space (or Command/Control+Space) panel, off by default: apps, Spotlight file names, calculator, Cockpit commands with Claude/Codex usage inline, typed URLs and paths. Carbon hotkey, no event tap; index loads on first press. Switched on in the hub's Settings > General | `Launcher/`, `Settings/Preferences.swift`, `System/HubBridge.swift`, `App/AppDelegate.swift` |
| Tests | Codenotch's unit tests not carried over | `project.yml` |

## Not yet done

- `Localizable.xcstrings` still carries translations for strings only removed code used; they are inert.
- The Claude and Codex activity monitors, usage alerts and `PiResponseMonitor` are unchanged; Pi's provider-name map still lists removed providers, which simply never match a ring.
