**Pulse hub refresh — adversarial re-review**

Reviewed `review.md`, original `BRIEF.md`, `proposal-v2.md` including v2.1, all five HTML files & relevant current view code. Shared CSS & JavaScript are identical across mocks except screen initialization; state menus differ. Browser inventory exposed no browser surface: findings below are source-derived, with no rendered-fit or native-interaction claim. “Resolved” means design/source remedy, not implementation acceptance.

Brief constants remain specified: fused RightKit shell, fixed 170px sidebar, eight sections/two groups, Tanker titles/wordmark, system body, lucide 1.75, blue accent, both themes & confirmed reversible moves. Production library components are explicitly required; handwritten projections do not prove integration. Task hierarchy is substantially clearer, especially full-width Storage workspaces & Monitor’s pressure-first summary.

| Prior finding | Status | Evidence: file + element |
| --- | --- | --- |
| B1 — Irreversible Trash selection | resolved | `cleanup.html` — `eligible()`, `blockers()` & `move()` exclude informational Trash, simulator/Docker guidance, unknown liveness & missing identity from controls, totals & submission. |
| B2 — Measurement truth | partly | `storage.html` / `cleanup.html` — `blockers()`, `selectionBar()` & confirmation retain apparent/reclaimable/Unknown/≥ distinctions; `partial()` still derives historical precision from current mode, losing durable lower-bound truth (N4). |
| B3 — Minimum-window layout | partly | `storage.html` (shared layout across five mocks) — `#size`, `.win[data-size=small]`, `.scroll` & wrapping footers implement both sizes; v2.1’s closed-row arithmetic does not establish expanded, scaled-text, recovery-footer or dialog fit. |
| M1 — Hidden actions | resolved | `storage.html` — persistent Rescan/row moves; `monitor.html` — persistent `.process-actions`, focusable disclosures & separate confirmed Force Quit; bytes remain visible. |
| M2 — Contrast | resolved | `storage.html` (shared palette across five mocks) — stronger `--rk-ink-2/3`, dark accent ink `#071624` on unchanged `#0a84ff`, white on danger `#b32235`, explicit focus/forced-colors rules remove cited token failures. |
| M3 — Inconsistent numbers | partly | `cleanup.html` — shared eligible totals correctly yield 8.8 + 6.1 = 14.9 GB; `storage.html` — bounded `squarify()` & reserved delta column fix geometry, but external rows reuse home measurements/deltas (N3). |
| M4 — Windows adaptation | resolved | `proposal-v2.md` — Windows section specifies native caption/drag behavior, OS labels/shortcuts, capability gates, unsupported-feature hiding & no blanket reversible-uninstaller promise; mocks explicitly depict Mac. |
| M5 — Monitor semantics | resolved | `monitor.html` — `monitor()` leads with reported pressure/current-sort consumer, labels both CPU scales, distinguishes Unknown from measured zero, disables stale termination & partitions expanded totals. |
| M6 — Association versus safety | partly | `apps.html` — `appReview()` uses neutral match badges, unchecked user/shared content & “Deselect group”; footer omits selected user/shared consequence & offers enabled Move for running apps without actionable quit/review recovery. |
| M7 — Completion/recovery states | partly | `cleanup.html` adds History/restore & representative failures; `settings.html` — `settings()` still omits most Appearance/Notifications controls, helper enabled/not-found/error, renewal & local Input Monitoring recovery; new recovery defects appear below. |
| M8 — Accessibility | partly | `cleanup.html` / `settings.html` (shared primitives) — native inputs, named switches, 32px controls, modal Cancel/Escape & semantic focus restoration improve access; `render()` recreates native disclosures without retaining open state, while keyboard/modal behavior remains unobserved. |
| M9 — Implementation scope | resolved | `proposal-v2.md` — Implementation notes name interaction/data work, preservation contracts, scoped `.ck-*` classes & workflow acceptance; app-shell 0.2.1 availability is explicitly distinguished from integration. |
| m1 — Inaccurate consistency rules | resolved | `proposal-v2.md` — task-specific primary placement replaces universal-position claim; mocks remove `.row.p` conflict & utilization-colored gauges, using flexible rows & explicit semantic colors. |
| m2 — Decorative recognition/density | resolved | `apps.html` — neutral `.app-icon`; `storage.html` — labeled folder rows include Other; shared `.sub`, `.badge` & `.caption` use 12px, with full-width findings & wrapping identity text. |

**New problems, ranked.** No new blocker established; major findings below require correction before implementation consumes these flows.

1. **Major N1 — Restore retry changes transaction.** `cleanup.html` & `storage.html`, `restore(index)` / `action('retry-restore')`: failure stores only global `failedRestore`; Retry always calls `restore(history.length-1)`. With two batches, failing restoration of older batch then retrying targets newer batch. “Undo last move” can also remain offered solely because an older batch is unrestored while newest batch is already restored. Preserve failed activity ID, scope error/retry to its row & derive Undo availability from its actual target.

2. **Major N2 — Partial uninstall strands failed paths after Back.** `apps.html`, `action('app-move')`, `appsView()` & `appState`: Partial with default Sketch selection moves application, retains failed cache, then excludes Sketch via `uninstalled`. Back removes every visible route to cached failed-path review. Keep pending cleanup entry or durable result/retry surface reachable after bundle removal; distinguish bundle absence from completed uninstall.

3. **Major N3 — External browsing relabels home data.** `storage.html`, `folders()`, `folderRows` & `#drive` change handler: changing volume preserves home folder/query/cache, relabels root “External mount” & displays identical 210 GB home rows with +1.8/+0.7/−0.4 GB deltas. This contradicts `changes()` & About’s “external growth unavailable.” Key data/navigation by volume, show external fixture or unavailable state & suppress unsupported deltas.

4. **Major N4 — Scan refresh rewrites historical measurement certainty.** `cleanup.html` & `storage.html`, `partial()`, `move()` & `historyView()`: ≥ depends on `mode==='partial' && id==='logs'`; history copies no partial-size flag. Select logs plus archives so Partial moves logs & skips archives: subsequent Rescan makes moved logs’ historical lower bound appear exact. Conversely, switching into Partial retroactively marks an earlier exact logs move incomplete. Persist measurement provenance per finding/result/activity; current scan mode must not alter history.

5. **Major N5 — Settings status contradicts chosen values.** `settings.html`, `general()` / `action('switch:…')`: switching Fn off leaves “Requested on · running”; Partial Auto Quit retains “Requested on” when off; Launcher defaults to Option Space while warning specifically about Command Space. Shortcut, Surface & Channel selects have no change-state handling, so another render resets displayed choices. Model requested/operational/error states independently, condition warnings on actual values & retain every represented preference across navigation/recovery.

6. **Minor N6 — New Apps state switcher mixes app identities.** `apps.html`, `#state.onchange`: Running/Protected assigns Sketch directly instead of using `openApp()`. After opening Xcode, switching state can show Sketch’s 6.4 GB bundle row but retain Xcode’s 14.8 GB application size in selected total/confirmation, alongside prior choices. Initialize scenarios through one identity-bound detail loader; isolate each app’s fixture state.

7. **Minor N7 — Settings deep links highlight wrong destination.** `settings.html`, initial `.nav a[aria-current]` & `settingsPane`: opening Accounts/Appearance/Notifications from another mock loads requested pane while General stays highlighted. Initialize active navigation from same section state used by title/content.

8. **Minor N8 — Selection closes reviewed details.** `cleanup.html`, `findingRows()` / `render()`: opening path disclosure then selecting its checkbox replaces entire content tree, closing disclosure; excluded-item details also reset. Preserve disclosure state by finding/group identity so review context survives selection changes.

9. **Minor N9 — Capacity spec & mock diverge.** `proposal-v2.md`, Storage spec promises 54.05% used bar; `storage.html`, `storage()` renders only free/total text, leaving `.bar` unused. Restore compact used/free visualization or explicitly revise spec; current glance requires mental subtraction.

Closure requires correcting N1–N9, completing outstanding M6/M7 layouts & visually inspecting both sizes/themes with expanded rows, long names, text scaling, partial results & dialogs.

Only this review file was written. No builds, tests, commits, pushes or publishing. Prior review’s recorded test inventory: **391 declarations — 390 component/core-integration + one read-only UI tour**; zero added/deleted/executed here, no replacement-E2E claim.

**Verdict: approve with listed fixes.**
