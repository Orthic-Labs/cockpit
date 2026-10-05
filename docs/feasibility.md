# M0 feasibility status

Status is source-inspected only unless a row says otherwise. No build, import, runtime probe, TCC/Fn check or footprint measurement ran in this inventory lane.

| Gate | Evidence now | Status |
| --- | --- | --- |
| Four donor pins & extraction inventory | `upstream.lock.json`, immutable source reads in `docs/donors.md` | Source-inspected; subtree import pending |
| License & packaging disposition | Vorssaint GPL-3.0-or-later, Codenotch MIT with locked SwiftNIO/Sparkle/zstd, Tinycast AGPL-3.0-or-later, Pearcleaner Apache-2.0 + Commons Clause; Cockpit has no declared license | **Pending legal/public-boundary decision** |
| Mac one-AppDelegate/service registry | Codenotch, Vorssaint & Tinycast each have separate composition roots and long-lived managers | **Pending design/extraction spike** |
| Windows native ring | Codenotch Windows draws SVG in Tauri/WebView2; native Rust ring is Cockpit-owned prototype | **Pending native runtime check** |
| Mac/Windows footprint | No release measurement; Codenotch Windows contains WebView2 and Tinycast index is unmeasured | **Pending** |
| Fullscreen hide | HeardRight source inspected; Windows source is foreground-only while Cockpit prototype enumerates topmost windows per monitor | **Pending machine matrix** |
| TCC/signing identity | No signed Cockpit build or permission transition run | **Pending** |
| Fn/remap hardware | No keyboard or Secure Input run | **Pending** |
| Runtime ownership/local channel/store | Plan describes target ownership; no implementation evidence in this lane | **Pending** |
| Filesystem provider/APFS fixtures | Petal source inspected; no extraction or fixture run | **Pending** |

The only safe M0 conclusion is that source entrypoints & pins are recorded. Runtime, footprint, TCC, signing, Fn, fullscreen and extraction gates remain open.
