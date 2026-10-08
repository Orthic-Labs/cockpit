# Pulse hub design refresh — adversarial review

**Verdict: ship with fixes.** Direction is sound, implementation brief is not ready. Stronger grouping, restrained surfaces & clearer selection improve today’s utility UI, but this is only partly minimal: Storage compresses two competing workspaces, Monitor adds dashboard rings without answering what needs attention, & destructive actions become harder to discover. It can look credible in 2026 without imitating Liquid Glass. As proposed, it fails reversible-cleanup safety, accessible contrast & minimum-window support; Windows requires explicit platform adaptations. Keep overall structure, resolve blockers & major findings before implementation approval.

## Evidence & comparison

Reviewed both briefs, proposal, all five HTML/CSS/JS mockups, current React views, `App.tsx`, `styles.css`, API types, chart mapping & cleanup rules. Browser opening failed because no browser surface was available; geometry below is source-derived, not screenshot-verified. Inspected both supplied PNGs: `hub-now.png` is blank chrome; `monitor.png` shows older disk gauges absent from current `Monitor.tsx`. Neither establishes current five-screen visual coverage.

Against useful category benchmarks, proposal improves grouping but falls short on interaction clarity: [DaisyDisk](https://daisydiskapp.com/) distinguishes physical usage from clone sizes; [TreeSize](https://www.jam-software.com/treesize) combines recognizable folder navigation with size comparison; [Raycast](https://www.raycast.com/) makes keyboard access central. Those are relevant standards, not reasons to copy their appearance. Pulse’s tiny explorer, hidden actions & unexplained measurements undermine comparable confidence.

For requested macOS 26 baseline, Apple explicitly retains rounded rectangles for compact desktop controls; Liquid Glass separates functional controls from content. Solid content cards are therefore reasonable. Windows Fluent similarly gives materials specific jobs: Mica underlies windows; Acrylic serves transient surfaces. Lack of blur is **not** this proposal’s problem. Legibility, hierarchy, platform behavior & honest state are. [Apple design system](https://developer.apple.com/videos/play/wwdc2025/356/), [Windows materials](https://learn.microsoft.com/en-us/windows/apps/design/signature-experiences/materials).

## Three-second assessment

| Screen | Immediate answer | Where understanding breaks |
| --- | --- | --- |
| Storage | Free space & cleanup opportunity | Competing totals, compressed findings, tiny explorer; unclear disk-versus-home scope. Still two lists competing. |
| Cleanup | Selected amount & next action | Strongest task hierarchy, weakened by misleading headline, unsafe Trash row & concealed exclusions. |
| Monitor | Current readings & large consumers | Cannot infer slowdown from utilization alone; unlabeled percentages & hidden controls require exploration. Still process list beneath three equal cards. |
| Apps | Large apps & old usage | Useful shortlist; ambiguous running dots, interchangeable gradient icons & technical confidence labels slow safe review. |
| Settings | Familiar groups & right-aligned switches | Reasonably scannable, necessarily list-based; “on” versus actually operational remains ambiguous. |

## Blockers

### B1 — Cleanup: irreversible operation enters reversible selection

`cleanup.html` preselects **Trash · 0.2 GB** beneath “Regenerates itself”; its reason says emptying frees space. Footer & dialog promise moving those same five items to Trash with restoration. Existing `rules/cleanup.json` correctly defines `trash-info` as informational, action `None`. This is a direct safety-contract violation, not cosmetic placeholder copy.

**Fix:** keep existing Trash informational, excluded from selection & eligible totals. Preserve command-only exclusions too: simulator/Docker rows must not become generic Trash candidates merely because mocks draw checkboxes. Bind controls to actual eligibility/action metadata. Retain confirmation & restore for supported moves; introduce no empty-Trash operation.

### B2 — Storage / Cleanup: concise copy removes measurement truth

`ChromeSnapshotsLine.tsx` distinguishes apparent bytes, known reclaimable bytes, unknown reclaimability & shared storage. Both mocks replace that with ordinary “1.2 GB” findings & shorter reassurance. Existing partial-size `≥` markers also disappear. “Safe” means eligible under known conditions, not guaranteed harmless or immediate free space; moving files to Trash usually retains their storage. Hiding unknown liveness inside generic “held back” copy weakens an explicit project constraint.

**Fix:** retain apparent-versus-reclaimable labels, partial markers, unknown states & reason details. Exclude unknown liveness/metadata from selection. State “selected to move” separately from measured free space; never promise reclaimed bytes from a Trash move. Collapsed exclusions need visible count/reason summary & accessible detail.

### B3 — All screens: required 820×520 layout does not exist

Every `.win` is hard-coded to `900px × 600px`; no responsive override exists. Browser wrapper adds 32px horizontal & 48px vertical padding, so fitting that wrapper is different from fitting native window content. Resizing viewport would leave mock window oversized, not prove adaptation.

Even after correcting wrapper geometry, Storage allocates only **690px** content width at default size: fixed 300px explorer + 12px gap leave 378px for findings. At minimum size, findings fall to **298px**, with roughly **148px** remaining for row name, badge & reason after padding/columns. Drive strip reserves 190px + 170px + gaps, leaving active drive 306px normally & 226px at minimum. Long names & multi-line header competition are inevitable pressure points.

**Fix:** define both native-window sizes explicitly, preserve fixed sidebar, allow header wrapping & reduce competing fixed columns. Specify handling for arbitrary drive counts, long names, expanded rows & text scaling. Verify scroll ownership, reachable footers & uncropped dialogs at both sizes. Smaller type is not an acceptable fit strategy.

## Major findings

### M1 — Storage / Monitor: hover-only actions are hidden functionality

`.acts` uses `visibility: hidden`; only hover, demonstration `.hover`, or `.keep` reveals it. Hidden buttons cannot receive keyboard focus. Storage additionally hides size while exposing Trash, removing decision context at action time. Monitor still reserves 128px for invisible actions, so hiding them does not recover row width. Rescan moves to undrawn drive hover, making freshness harder to manage.

**Fix:** provide visible row action/menu affordance, persistent selected-row controls & keyboard access. Keep name, risk & bytes visible during action. Use `:focus-within` alongside focusable row/disclosure controls; provide pointer-independent access. Keep Rescan visibly discoverable. Force Quit should remain secondary, separate & confirmed.

### M2 — All screens: contrast fails in both themes

Calculated directly from declared sRGB tokens:

| Text / background | Ratio | Consequence |
| --- | --- | --- |
| Dark `#6e6b68` / panel `#1f1f23` | 3.10:1 | 11–11.5px reasons & captions fail ordinary-text minimum. |
| Light `#8a8378` / white panel | 3.75:1 | Same failure; light mode does not solve it. |
| White / accent `#0a84ff` | 3.65:1 | Small primary-button labels fail. |
| White / dark-theme danger `#ff6961` | 2.82:1 | Destructive labels fail even large-text threshold. |

These are token calculations, not rendered contrast measurements; opacity can introduce further failures. Some weaknesses may be inherited from shell, but must still be resolved. [WCAG contrast guidance](https://www.w3.org/WAI/WCAG22/Understanding/contrast-minimum.html).

**Fix:** strengthen secondary text & choose accessible foregrounds on fixed accent/danger backgrounds through supported component tokens. Retain `#0a84ff`; changing foreground need not change brand. Check controls, chart labels, focus & high-contrast states independently.

### M3 — Storage / Cleanup: inconsistent numbers defeat “at a glance”

Storage’s bar segments total **80%**, while 227 GB free of 494 GB implies about **54% used**. Cleanup’s **9.3 GB safe + 6.1 GB review = 15.4 GB**, not headline 14.2 GB. Explorer rows omit empty delta cells, placing sizes in different grid columns. Treemap bottom rectangles end at y=119.5 inside a 108-high viewBox, clipping geometry; rectangle proportions also do not consistently represent listed sizes.

**Fix:** derive every total, proportion & selection summary from one coherent fixture. Reserve delta column consistently, generate tiles with existing `squarify`, show denominator/scope & account for unclassified space. Numerical consistency is design evidence, even in mocks.

### M4 — All screens: Windows is unspecified beyond caption buttons

Finder, Trash, Command/Fn, Login Items, Dock, green zoom button, Spaces, DMG installation & Library paths are Mac-specific workflows. `NSWorkspace` icons are Mac-only. Swapping traffic lights for RightKit caption buttons does not translate those capabilities. Windows also needs text scaling, forced colors, active/inactive chrome & correct caption/drag behavior. [Microsoft title-bar guidance](https://learn.microsoft.com/en-us/windows/apps/design/basics/titlebar-design).

**Fix:** retain shared navigation/layout, but specify OS labels, shortcuts, path formatting, icons, permission destinations & capability availability. Use Recycle Bin/File Explorer language only where equivalent behavior is supported; do not imply every Windows uninstall is reversible. Hide unsupported Mac conveniences. Exercise actual RightKit Windows chrome without reimplementing it. Existing `system-ui` fallback is useful; Tanker titles are an intentional brief-mandated departure from Windows convention, not permission to redesign shell.

### M5 — Monitor: rings change appearance more than understanding

Three equal gauge cards preserve equal emphasis while adding circular decoration. Proposal reuses `tone()` thresholds based on utilization; high memory occupancy can therefore turn red despite normal pressure. “None” plus “Nothing paged out” does not distinguish unknown telemetry from measured zero. Core summary “10 cores · 3 busy” has no corresponding field in current `Status`. CPU percentages lack column labels/normalization context; process CPU can exceed 100% while aggregate CPU uses another denominator.

**Fix:** prioritize actual pressure/status & relevant consumer, label CPU/memory columns, explain scale, keep unknown readings explicit & remove unsupported core claims. Base memory health on available pressure metadata; occupancy alone is insufficient. [Activity Monitor’s memory model](https://support.apple.com/guide/activity-monitor/view-memory-usage-actmntr1004/mac). Stable row identity/selection must survive refresh & sorting before termination controls become actionable.

### M6 — Apps: association confidence looks like deletion safety

Green “Exact id” sits beside preselected “Settings and templates”; ownership confidence does not mean user data regenerates. Mock also colors **Team id green**, contradicting proposal’s neutral mapping. Top-right destructive CTA is discoverable, but outruns review when exclusions, running status & admin requirements live elsewhere. Group “Clear” ambiguously resembles deletion when it actually deselects.

**Fix:** separate ownership confidence from data consequence, keep shared/user-created content explicit & preserve existing `preselected` policy. Use “Deselect group.” Keep selected count, bytes & relevant warning beside CTA; show running/protected states there before confirmation. Unknown last-used must remain unknown & excluded from unused filtering, as current code already does.

### M7 — All screens: state coverage is insufficient to judge safe completion

History is drawn as an inert tab; no history/restore view exists in `cleanup.html`. Undo notice is promised but absent. Loading, permission denial, partial success, stale scan, restore failure & empty results have no proposed layouts. Settings omits existing “Forget reading,” access-refusal recovery & downloaded-DMG Trash toggle despite “behavior unchanged”; enabled launcher shortcut/conflict state is also unshown. “Accessibility required for everything below” contradicts current installer note saying no permission is needed.

**Fix:** map existing branches before replacing markup. Preserve exact recovery actions, exclusions & user choices; draw representative busy, failed, partial & restored states. Keep selection/results stable across History, Back & rescan. Show requested-on versus operational status distinctly. Do not substitute shorter copy for missing capability semantics.

### M8 — All screens: mock primitives cannot demonstrate accessibility

Checkboxes are spans; Apps search is a div; app rows use click-only divs; switches lack accessible names. Dialog scripts show/hide overlays without focus trapping, Escape handling or focus restoration. Settings reorder targets are 22px; switches are 22px high. Size alone does not establish WCAG failure because spacing exceptions exist, but hit areas need explicit design. Running dots are color-only. No reduced-motion override accompanies toggle transition.

**Fix:** implement actual app-shell controls, labeled native inputs & focusable disclosures; define tab order, visible focus, keyboard selection, dialog focus behavior & text status. Ensure at least 24×24 CSS-pixel targets or compliant spacing, preferably larger forgiving hit regions. Respect reduced motion & forced colors. Treat static mock omissions as unproven interaction design, not evidence that RightKit itself is broken. [Target-size guidance](https://www.w3.org/WAI/WCAG22/Understanding/target-size-minimum.html).

### M9 — Implementation: this exceeds a CSS refresh

Current `Volume` exposes capacity/free space, not category breakdowns; `KINDS` is a classifier palette, not disk accounting. Startup scan covers home, so it cannot explain entire disk. Path-keyed growth moved into current folder rows can hide changes outside that folder. Icons require native data plumbing; CPU sorting, group selection, History state & Auto Quit editor require interaction work. Package manifest pins app-shell **0.1.0**; compatible APIs, focus behavior & Windows rendering cannot be inferred from handwritten lookalike controls. Local app-shell package source was unavailable for inspection.

**Fix:** scope work explicitly: lower-risk Settings grouping/type cleanup; medium-risk Apps/list controls & Cleanup selection/history; higher-risk Storage accounting/layout, native icons & platform adapters. Preserve scan cancellation, cached reports, partial-result reporting, protected-app refusal, liveness checks & restore plumbing. Scope new CSS to views; globally replacing `.row`, `.sub`, `.group` or deleting shared classes risks collateral changes. Keep `App.tsx` shell contract intact. Qualify complete native journeys through generated RightKit CI; no local Cargo/Swift builds.

## Minor findings

### m1 — Shared vocabulary: claimed consistency is overstated

“Primary always in same place” conflicts with top-right Storage/Apps versus bottom-right Cleanup; each can be appropriate, but rule is false. Accent is also used for metric bars & every nav icon, not solely selection/action. `tone()` can use red for gauges despite “red only on destructive buttons.” `.row.p` specificity overrides intended 30px sub-row height with 36px.

**Fix:** document actual hierarchy/semantic exceptions, retain required shell accents & reconcile row-height selectors. Prefer stable task-specific placement over inaccurate universal rules.

### m2 — Apps / Storage: decorative details reduce recognition

Gradient app placeholders are arbitrary color noise. Seven-item legend omits rendered “Other” segment; long badge strings compete with names in narrow rows. Fixed 11px captions become especially weak when carrying important scope or safety information.

**Fix:** use real app icons or one neutral recognizable fallback, label every displayed category & spend text budget on identity/consequence before badges. Keep essential context at readable body/secondary size.

## What’s genuinely good — keep it

- Eight-section navigation, fixed sidebar, Tanker limited to wordmark/titles, system body font, lucide strokes & existing accent respect visual brief.
- Selection summary beside Cleanup action makes intended operation easy to verify.
- Distinct Safe/Review groups, unchecked review items & confirmed Force Quit support deliberate decisions when grounded in actual eligibility.
- Settings groups, local status badges & explicit follow-up buttons improve scanability.
- Shared hairlines, restrained cards & integrated explorer can reduce fragmentation once geometry & states are corrected.

## Review checks

Static detector reported five “flat type hierarchy” findings; these overstate risk because it misses variable-based 22/28px stats & compact utility typography is appropriate. Contrast calculations & source geometry supplied stronger evidence. No mock was rendered, no native journey executed, no application code or tests changed.

Static inventory using repository declaration patterns: **391 existing declarations** across core, Windows, Mac tests, script tests & hub QA: **390 component/core-integration declarations plus one read-only hub UI tour**. Backend journeys within that inventory do not establish installed end-to-end UX. Zero added/deleted tests; zero executed for this review. Historical installed-journey runners described in `docs/testing.md` are absent from current `scripts/qa`. Next acceptance evidence should cover scan → informed selection → confirm → partial-result recovery → restore → retained state, plus keyboard operation & both window sizes on each supported OS.
