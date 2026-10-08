# Cockpit hub refresh v2

Supersedes proposal.md. BRIEF.md’s Keep-constant list binds: fused RightKit shell, fixed 170px sidebar, eight sections/two groups, native traffic lights, Tanker titles/wordmark, system body at 13px, lucide 1.75, #0a84ff, existing library components, confirmed reversible Trash moves & separate confirmed Force Quit. `hub/package.json` now pins app-shell **0.2.1**.

Keep restrained cards, hairlines, integrated explorer, Safe/Review groups, unchecked Review defaults, selection beside action & grouped Settings with local status/recovery. Primary placement is task-specific.

## v2.1 tightening

- One short clause per visible label/status; one keyboard-reachable **About these numbers** disclosure per screen holds scope, caveats & definitions. Measured badges, ≥, apparent/reclaimable estimates, Unknown, exclusions & operational warnings stay explicit. Row disclosures retain paths; confirmations retain consequences & recovery.
- Storage uses one volume/select/free-total/Rescan line, plus a small installer chip with Eject. Findings own roughly 306px at 820×520; first four closed rows need roughly 289px. Cleanup owns roughly 305px; first four need roughly 261px. These are Ready-state source geometry, without scaling or smaller text. Partial/stale Storage status shares installer line; targets stay at least 32px.
- Wordmark & page titles use Tanker via `var(--rk-wordmark)`; sidebar uses named sun/half-circle theme buttons. Monitor, Apps & all Settings groups share copy diet. Existing eligibility, partial totals, unknown-state rules, retained selection, confirmed Trash moves, restore & separate confirmed Force Quit remain intact.

JavaScript syntax checked across all five files. Browser pane unavailable; installed Chrome preview launch blocked by sandbox, so rendered row counts remain unobserved. No builds, tests, commits, pushes or publishing; test inventory unchanged.

## Changes by finding

| Finding | Resolution |
| --- | --- |
| B1 | One predicate governs eligibility, checkboxes, groups, totals & submission; informational/unknown items have no checkbox. No empty-Trash operation. |
| B2 | Measured/selected-to-move bytes never imply freed space. Retain ≥, apparent/reclaimable distinction, shared-storage explanation & Unknown. |
| B3 | Explicit size toggle changes frame to 900×600 or 820×520; wrapping headers/rows, full-width workspaces, scrollable content & reachable footer. |
| M1 | Persistent actions, Rescan & focusable disclosures; names/reasons/bytes stay visible. |
| M2 | AA text/badge palette in both themes; accessible foreground on unchanged blue. |
| M3 | Shared fixtures derive totals/proportions. Capacity ≠ home scan. Squarify tiles fit bounds; reserve delta column. |
| M4 | Windows adaptation below; unsupported Mac workflows remain unavailable. |
| M5 | Pressure & leading consumer precede readings. Remove rings, occupancy-derived health & unsupported core claims. Null remains Unknown. |
| M6 | Neutral association badges, separate user/shared consequence, backend preselected policy, “Deselect group”, adjacent running/protected/admin warnings. |
| M7 | State switchers on every mock; working History/Undo/restore failure, partial skips, stale guards & Settings recovery branches. |
| M8 | Labeled native inputs/search, named switches, focusable app rows, textual status, modal focus/Escape/return, 32px targets, reduced motion/forced colors. |
| M9 | Explicit interaction/data work below; preserve cancellation, caching, revalidation, refusals & restore. Dependency pin proves availability only. |
| m1 / m2 | Scoped classes, flexible heights, accurate color exceptions, neutral app-icon fallback, complete text at 12–13px. |

## Per-screen spec

**Storage:** capacity first; active-volume selector supports arbitrary drive counts/long names. Fixture: 494 GB = 267 used + 227 free; used bar 54.05%. OS capacity has no category breakdown. Startup scan covers home only. Findings/Folders/Changes use full width; direct moves, Safe group action, Eject & recovery stay visible. Folders combines breadcrumb/search, proportional tiles & complete rows: Library 92 + Developer 48 + Documents 34 + Pictures 26 + Other 10 = 210 GB apparent, independent of disk usage. Changes retains all home deltas, including outside current folder; external growth says unavailable.

**Cleanup:** Safe 4.0 + 2.8 + 2.0 = 8.8 GB; Review 3.5 + 2.6 = 6.1; eligible total 14.9. Partial marks affected row/group/total/selection/dialog ≥. Trash, simulator guidance, Docker & unknown liveness remain excluded. Chrome running also excludes its 1.2 GB apparent; Ready shows 0.1 GB reclaimable estimate, Partial shows Unknown. Shared-storage explanation qualifies estimate after deletion, never Trash move. Apply clears only moved IDs, retains skipped paths/reasons & actual activity ID. History switching retains selection; Undo/Restore report restored/skipped separately. Restore requires known Trash path; destination conflict leaves item in Trash with retry.

**Monitor:** actual memory pressure & current sort’s leading consumer first; normal pressure does not make occupancy a slowdown diagnosis. System CPU 0–100% across cores; process 100% = one core, may exceed 100%. Memory/swap nulls retain capability/label; measured zero says 0 B. Sorting/expansion preserves ProcessRow.key, notes & busy state; actions bind lead PID/start_time. Stale/error pauses termination; can_act/refusal controls protected rows. Quit’s still_running warning persists; Force Quit stays separate/confirmed. Expanded member quantities partition parent total.

**Apps:** labeled search, filters & shown-set totals; five fixture bundles = 34.2 GB. Unknown last-used never enters Unused. Neutral icons until native plumbing exists. Detail separates match from consequence: Sketch defaults application 6.4 + cache 0.3 = 6.7 GB / two selected; templates 0.2 & shared Team association 0.1 unchecked. Running app must quit before any move; protected app refuses removal. Confirmation repeats user/shared/admin warnings. Partial results retain failed paths & report moved bytes. Existing API supplies no uninstall activity ID: restore through Finder’s Put Back, no fictional integrated Undo. Preserve per-app selection/query across Back.

**Settings:** preserve every existing option/value/command. Distinguish requested-on from operational helper/Fn/launcher/conveniences status. General retains helper approval/not-found/error, shortcut conflict, Accessibility/Input Monitoring recovery, last-paste result, DMG Trash off-by-default & editable Auto Quit list. Installer offer needs no Accessibility permission. Accounts retains ordering, Show, sign-in renewal, Allow access & Forget reading without provider sign-out. Appearance/Notifications show representative groups; implementation retains full controls. Missing state infers no defaults.

## Tokens, sizing & keyboard

Prototype palette is proposed override, not verified shell internals. `--rk-*` only for shell tokens. Dark chrome/plane/panel: #121215/#19191d/#232328; light: #eeeae4/#f7f4ee/#fff. Dark text #f3f1ee/#c0bcb6/#b4afa8; light #231f1a/#59534b/#655e55. `--rk-accent` #0a84ff + `--rk-accent-ink` #071624: **5.01:1**. View `--ck-danger-bg` #b32235 + white: **6.57:1**. Worst declared secondary fill: **6.02 dark / 5.09 light**. Safe/Review/Error badges: **8.21/9.14/7.92 dark; 6.84/8.18/6.43 light**. No opacity weakening. Focus dark #75b9ff/light #0055aa; visible control borders.

Spacing 4/8/12/16px; radius 12px; title 18px Tanker, stat 24px, body 13px, secondary 12px. Rows grow; no essential ellipsis. Mock controls sit outside native frame; size toggle never scales content. Plane content scrolls; footer remains reachable. Text scaling grows wrapped rows; sidebar scrolls independently. Dialog fits frame/viewport with internal scroll. Tab/Space/Enter reach every action; stable semantic keys retain focus. Cancel initially focused; modal traps focus, Escape closes, opener regains focus. Reduced motion removes transitions; forced colors uses CanvasText/Highlight. Color never carries status alone.

## Windows

Keep navigation/layout & Tanker titles; use actual RightKit 0.2.1 caption buttons/drag regions, maximize/snap & active/inactive chrome. Body fallback `"Segoe UI Variable", system-ui`. Mica beneath fused chrome; solid readable plane/cards, opaque fallback & forced-colors/text-scaling support.

Capability-gate native icons, paths, Ctrl shortcuts, startup/account actions & permission destinations. File Explorer/Recycle Bin labels only for supported reversible moves; Windows uninstaller never promises Trash restoration. Hide unsupported Fn, Finder, Dock, Spaces/green zoom, Login Items helper & DMG conveniences. Auto Quit appears only with verified support. Unknown telemetry stays Unknown. HTML depicts Mac shell.

## Implementation notes

| Exact file | Required change |
| --- | --- |
| `hub/src/styles.css` | Add `.ck-storage/.ck-cleanup/.ck-monitor/.ck-apps/.ck-settings` roots; scope `.ck-card/.ck-head/.ck-row/.ck-scroll/.ck-selection/.ck-measure/.ck-notice/.ck-badge/.ck-process/.ck-folder-row/.ck-setting`. Adapt mock classes under roots; never globally replace/delete `.row/.sub/.group`. Supported `--rk-*` overrides or scoped library Button classes for foregrounds. |
| `hub/src/views/Storage.tsx` | `.volumes/.volume-card`, `.card-block/.findings-scroll/.explorer` → selector & one workspace. Preserve cachedReport, scanToken, follow/lastScan, Eject, search, confirmMove/undoMove, partial/access/snapshot notices. `hub/src/chart.ts` squarify; KINDS is folder classification, not disk accounting. |
| `hub/src/views/Cleanup.tsx` | Replace rowStyle/inline confirm with scoped rows, SegmentedControl/ConfirmDialog & working History. Shared predicate: eligible + supported Trash action + safe/review + valid dev/ino. Backend unknown-liveness decision remains authoritative. Initialize preselected once; intersect retained IDs with fresh eligibility. |
| `hub/src/views/ChromeSnapshots.tsx` | Keep apparent/reclaimable_known/reclaimable_bytes/running/since; replace “to gain” with qualified after-deletion estimate. Unknown never enters totals. |
| `hub/src/views/Monitor.tsx` | Replace Gauge emphasis/null→0 with pressure/consumer lead. `.row.procs .name` becomes focusable disclosure; retain notes/stuck/busy, identity binding & separate confirmation. |
| `hub/src/views/Apps.tsx` | `.row.apps` becomes focusable Button/disclosure; `.row.items` separates consequence/confidence; Clear → Deselect group. Preserve preselected/refusal/admin/background/receipts; cache detail choices; remove result’s “freed”. |
| `hub/src/views/Settings.tsx` | Group/Row → scoped `.ck-sgroup/.ck-setting/.ck-account`; retain full branch/command/options inventory. Enlarge/name reorder controls; operational status comes from reported fields. |
| `hub/src/api.ts`, `rules/cleanup.json` | Existing fields/rules govern eligibility; no rule mutation. Cleanup’s duplicated types should use shared types. Fixture liveness is explanation, not new API field. Docker is informational; current pack has no Docker rule. Unknown identity/liveness stays excluded. |
| `hub/src/App.tsx` | Preserve shell props/groups/titles. Production uses library Button/Toggle/SegmentedControl/Badge/ConfirmDialog/Card/EmptyState; native HTML controls are self-contained projections. |

Implement Settings grouping first, Cleanup/Apps retention next, Storage/Monitor scope/identity next; native icons/platform adapters separately. Acceptance through generated RightKit workflows: installed scan → select → confirm → partial recovery → restore → retained-state journeys, keyboard, both sizes/themes & supported OS chrome. No helper-test padding.

Evidence: source geometry/arithmetic, JavaScript syntax & declared sRGB contrast inspected. V2.1 browser attempts are recorded above; rendered/native acceptance unexecuted. No builds/tests, commits, pushes or publishing. Inventory unchanged: review records **391 declarations** (390 component/core-integration + one read-only UI tour); zero added/deleted/executed. No replacement-E2E claim.
