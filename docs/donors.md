# M0 donor inventory

Checked 2026-10-05 from public GitHub source. Donor pins were inspected before import. Pins below are the only extraction inputs; `upstream-check.mjs` reads remote refs with unauthenticated `git ls-remote` and never edits this file.

| Donor | Pin | License observed in source | Extraction/package implication |
| --- | --- | --- | --- |
| [Vorssaint](https://github.com/vorssaint/vorssaint-utils/tree/c9cfa0d1014c885119bf35bbac850d670267a723) | `v3.4.0` peeled commit `c9cfa0d1014c885119bf35bbac850d670267a723` (`v3.4.0` tag object `6204c75d048c95570c0a14c37eafb9685bf2670f`) | GPL-3.0-or-later; `LICENSE`, README badge & `Package.swift` SPDX agree | Direct copied/adapted code carries GPL obligations. `Package.swift` is one executable target with `HIDEventSystem` & `VMStatisticsCompat` system libraries. Candidate modules: `Services/Finder`, `WindowMaximizer`, `DockClick`, `AutoQuit`; composition root is `Sources/Vorssaint/App/AppDelegate.swift`. Keep upstream notices & record local edits. |
| [Codenotch](https://github.com/vinzdg/codenotch/tree/72fb2169ef316834ad632f85de27a415cd0d2298) | `72fb2169ef316834ad632f85de27a415cd0d2298` (`main`) | MIT; root `LICENSE` | Mac target is generated from `project.yml`, macOS 15+, with SwiftNIO 2.102.0 & Sparkle 2.9.6 locked in `Package.resolved`, plus vendored zstd 1.5.7 under BSD-3-Clause. `Sources/App/AppDelegate.swift` owns `NotchFleet`, `UsageStore`, providers & updater; ring view is `Sources/Features/ProviderRing.swift`; provider readers are `Sources/Providers/`. Reuse requires preserving MIT, dependency notices, lock revisions & updater assets. |
| [Tinycast](https://github.com/abue-ammar/tinycast/tree/46beb10a7d23d9dcfa1e977f1144c99e47804c44) | `46beb10a7d23d9dcfa1e977f1144c99e47804c44` (`main`) | AGPL-3.0-or-later; `LICENSE`, README license section & contributor agreement. GitHub API reports `NOASSERTION`; source text is authoritative here. | This is not a permissive/custom license at pinned source. `Tinycast/App/AppCore.swift` is a large `@MainActor @Observable` singleton owning `AppIndex`, `HotKeyManager`, `PaletteState`, stores, coordinators, updater & permissions; `AppDelegate.swift` starts/stops it. It has no third-party Swift packages, but extracted code remains AGPL-covered. Vendor source under its original AGPL notices; combined product packaging must retain applicable licence obligations. |
| [Petal](https://github.com/henrydennis/petal/tree/f5e5b00e42a841304fc437f87b5d4cfee4377d2e) | `f5e5b00e42a841304fc437f87b5d4cfee4377d2e` (`main`) | MIT; root `LICENSE` & `Cargo.toml` | Algorithm candidates are `src/scan.rs`, `src/dirlist.rs`, `src/disk.rs`, `src/findings.rs`, `src/classify/`, `src/watch.rs` & `src/trashing.rs`. `src/main.rs` is GPUI app composition root; `Cargo.toml` pulls GPUI from pinned git revision plus `rayon`, `libc`, `palette`, `trash` & optional `image`. Do not extract `main.rs`, GPUI UI or `admin.rs` in M0. |

## Reference-only sources

[Pearcleaner main at `7724df7`](https://github.com/alienator88/Pearcleaner/tree/7724df7111bff82ae243301cf701992ef05ecf19) is reference-only for Mac leftover search. Its `LICENSE.md` is Apache-2.0 with Commons Clause: redistribution must retain notices & the Commons Clause, and “Sell” is excluded. No Pearcleaner source is copied; it remains reference-only.

HeardRight is owner code, not a public donor. Fullscreen reference paths inspected in owner's HeardRight checkout (`tauri-app-next`) are `src-tauri/src/pill/state/macos.rs`, `src-tauri/src/pill/state/windows.rs`, `src-tauri/src/pill/state.rs` & `heardright_core/src/pill/model.rs`. Windows foreground-only logic is insufficient for Cockpit's multi-monitor rule; the prototype must enumerate topmost visible windows on notch's monitor.

## Entrypoint inventory

Mac composition currently has three incompatible roots that must collapse behind one Cockpit registry:

- Codenotch `AppDelegate` creates edge fleet, usage store, readers, updater, status item & settings. Its Swift ring is presentation-only; providers own credential/cache/network reads.
- Vorssaint `AppDelegate` calls `FeatureRuntime.shared.syncAtLaunch`; Finder cut/paste, Dock click & Auto Quit are singleton services with their own permissions, observers, event taps & teardown. `WindowMaximizer` shares this input surface.
- Tinycast `AppCore.shared` constructs its stores/coordinators in `init`, then `start()` starts `AppIndex`, extensions, update checks, hotkeys, palette & feature watchers. Launcher index is therefore an explicit cold-start boundary for Cockpit.

Windows Codenotch is Tauri/WebView2, not a native notch: `windows/codenotch/src/main.rs` owns Tauri state and `ui/notch.html` draws SVG rings (`svgArc`, provider state and usage snapshots); `usage.rs`, `claude_auth.rs` & `codex.rs` own readers. `windows/codenotch/Cargo.toml` includes Tauri 2.11, updater, single-instance, HTTP/TLS, SQLite and Win32 features. Cockpit may reuse reader semantics & ring geometry as reference, but native Rust ring code must be owned & redrawn; no Windows runtime claim is made by this inventory.

## Public-repository boundary

Source-vendoring disposition: four pinned donor trees remain unmodified & retain original licences. Cockpit-owned code is separate; donor extraction into a combined executable must preserve applicable GPL/AGPL obligations & dependency notices. Pearcleaner remains reference-only. No donor binary is packaged or distributed by this bootstrap.

Cockpit's `mac/Sources/CockpitMacPrototype/NotchPresentation.swift` adapts codeNOTCH's canonical bezel flare/corner outline & ring-stack presentation to owner's compact footprint. Provider/runtime code remains Cockpit-owned. Original MIT notice is included in app bundle; full donor motion, cutout joining & provider UI are not claimed absorbed.
