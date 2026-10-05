# M0 feasibility status

Evidence below distinguishes source review, hosted checks & machine measurements. Mac bootstrap build passed in [run 37357451367](https://github.com/Orthic-Labs/cockpit/actions/runs/37357451367); integrated scanner/history, upstream fixtures & Mac visibility checks passed in [run 37359108437](https://github.com/Orthic-Labs/cockpit/actions/runs/37359108437). Batch 2 Windows validation passed in [run 37368532711](https://github.com/Orthic-Labs/cockpit/actions/runs/37368532711): 84 core Rust tests, 54 native Windows tests & Clippy. Mac validation passed in [run 37375567423](https://github.com/Orthic-Labs/cockpit/actions/runs/37375567423): 88 distinct core Rust tests including real APFS accounting, 27 Swift tests, Swift release build & Clippy. Both platforms passed 22 Node report/parser tests. APFS snapshot creation was skipped by the harness; retained-snapshot behavior remains unverified.

| Gate | Evidence now | Status |
| --- | --- | --- |
| Four donor pins & extraction inventory | `upstream.lock.json`, immutable source reads in `docs/donors.md` | Source-inspected; four subtree imports completed with original licences |
| License & packaging disposition | Vorssaint GPL-3.0-or-later, Codenotch MIT with locked SwiftNIO/Sparkle/zstd, Tinycast AGPL-3.0-or-later, Pearcleaner Apache-2.0 + Commons Clause; Unmodified donor trees retain original licences | Source-vendoring disposition recorded; combined packaging pending |
| Mac one-AppDelegate/service registry | Codenotch, Vorssaint & Tinycast each have separate composition roots and long-lived managers | Composition design recorded in `composition.md`; extraction/runtime pending |
| Windows native ring | Codenotch Windows draws SVG in Tauri/WebView2; native Rust ring is Cockpit-owned prototype; lifecycle/visibility helper tests & compilation passed hosted CI | **Pending native runtime check** |
| Mac/Windows footprint | No release measurement; Codenotch Windows contains WebView2 and Tinycast index is unmeasured | **Pending** |
| Fullscreen hide | HeardRight source inspected; Windows source is foreground-only while Cockpit prototype enumerates topmost windows per monitor | **Pending machine matrix** |
| TCC/signing identity | No signed Cockpit build or permission transition run | **Pending** |
| Fn/remap hardware | No keyboard or Secure Input run | **Pending** |
| Runtime ownership/local channel/store | Plan describes target ownership; bounded, opt-in snapshot history with atomic no-replace publication, explicit corrupt-file diagnostics & store `capability_notes` implemented; opt-in read-only IPC implemented (same-user auth, bounded frames, ops executed in killable `worker exec-op` children under a 20 s operation deadline with bounded termination confirmation); pill settings store implemented; mutation journal remains planned | **Pending real-machine gate** |
| Filesystem provider/APFS fixtures | Metadata provider & hosted sparse-file, hard-link, clone, inaccessible-directory accounting passed; disposable APFS teardown completed; snapshot creation skipped | Hosted fixture passed; snapshot retention & real-volume matrix pending |

Source pins, composition design, read-only CLI & Mac prototype are implemented. Hosted validation is separate from real-machine footprint, TCC, signing, Fn, fullscreen & extraction gates.
