# Pulse hub — design refresh proposal

Scope: the five hub screens inside the existing `@rightkit/app-shell` fused shell. Shell, navigation, fonts, icons, accent, window size, components and the Trash-with-confirm safety model are unchanged (see BRIEF.md, *Keep constant*). Mockups: `storage.html`, `cleanup.html`, `monitor.html`, `apps.html`, `settings.html`, each 900×600, dark by default, light via the sun/moon button in the sidebar foot.

## Principles

1. **One answer per screen, above the fold.** Each screen opens with the number the user came for: *227 GB free*, *14.2 GB can go*, *CPU 44%*, *142 apps · 61 GB*. Everything else supports that number.
2. **One primary action per screen, always in the same place.** Top right of the first card (Storage, Apps detail) or the sticky footer of the list (Cleanup). Row actions appear on hover only; they never compete with the primary.
3. **Cards group, rows list.** Content lives in `--rk-panel` cards with the shell's card shadow. Rows inside a card are separated by a half-pixel hairline, never by boxes inside boxes.
4. **State is a badge, not a sentence.** *Safe / Review*, *Normal / Needs approval / Running*, *Exact id / Name match*. Explanations drop to an 11.5px sub-line and are cut to one clause.
5. **Colour means one thing.** Accent = selection and the primary action. Green = safe or healthy. Amber = review or attention. Red = destructive, and only on buttons that destroy. Kind colours (from `chart.ts`) appear only on storage bars and tiles.
6. **Spacing on a 4px grid, three row heights.** 36px default row, 30px sub-row, 40–42px for rows with a sub-line. Page padding 16×20, card padding 12×16, gap 12.

## Per screen

### Storage

**Problem today.** Six stacked blocks (drive cards, installers line, findings block, growth chips, search/breadcrumb toolbar, explorer) each with its own typography. The findings block has a per-row *Move to Trash* button on every line, so the real primary (*Clear all safe*) is one of seven identical buttons. Growth and notes are separate paragraphs. The user has to read everything to learn two things: how full the disk is and what to do about it.

**Redesign.** Three regions, top to bottom:

- **Drive strip.** The active disk is a hero card: name, *Startup* badge, `227 GB` free in 28px, a stacked bar coloured by kind with a seven-item legend. Other disks are compact cards (stat at 22px, thin bar). A mounted disk image is a third quiet card with *Eject*. Clicking a card switches the lower half.
- **"12.4 GB can go" card (left).** Headline with the shield icon, primary button *Clear 9.3 GB safe*. Six grouped findings with *Safe* / *Review* badges in the title line and a one-clause reason under it. *Move to Trash* per row appears on hover and overlays the size. *Show 4 more* keeps the list short. Notes collapse to one line: *2 items need Chrome or Xcode closed first · Show*.
- **"Where it goes" card (right, 300px).** Breadcrumb, treemap, and the folder list merged into one explorer. Growth since last scan moves out of its chips line and into the folder rows as a signed delta (`+1.8 GB` amber, `−0.4 GB` green). Search becomes an icon button in the card head that swaps the breadcrumb for a field.

**Primary action.** *Clear 9.3 GB safe* → existing ConfirmDialog → Trash → Undo notice.

**Removed or merged.** Installers line → card in the drive strip. Growth chips → deltas in folder rows. Notes block → one collapsed notice. Search + Rescan toolbar → icon in explorer head; Rescan moves to the drive card hover (not drawn). Legend of kinds added (was only colour dots). Treemap and list share one card instead of a two-column grid under a separate toolbar.

### Cleanup

**Problem today.** Headline, reassurance paragraph, Chrome line, three section headers, checkbox rows with `display:block` spans, a sticky bar whose label wraps, and History appended below the fold. Two groups and the history compete in one scroll.

**Redesign.** Header: `14.2 GB` hero + one reassurance line; right side holds *Scanned 2 min ago*, *Rescan* and a `SegmentedControl` **Findings | History · 3**. One card holds the list:

- **Safe to clear** group with a group checkbox (all on), total, *selected for you* and a green *Regenerates itself* badge.
- **Review** group, group checkbox off, amber *Costs time to get back* badge.
- **Held back · 3** as a collapsed disclosure, replacing the always-visible *Not offered* list.
- Rows: checkbox, name, category as a neutral badge, one-clause reason, size.
- Sticky footer: *5 items · 9.3 GB selected* and the single primary *Move to Trash*.

**Primary action.** *Move to Trash* → ConfirmDialog (replaces the inline Cancel/Confirm swap, which the brief requires as a confirm step and which `ConfirmDialog` already provides on Storage).

**Removed or merged.** History → second segment, same card. *Not offered* → *Held back* disclosure. Reassurance paragraph → one line, repeated inside the dialog. Chrome snapshots line → a normal finding row (*Chrome snapshots · 3*). Inline confirm bar → ConfirmDialog.

### Monitor

**Problem today.** Three identical thin bars read as equals though CPU, memory and swap are different quantities; disks were already moved out. Every row carries *Quit* and *Force Quit*, so sixty rows show one hundred and twenty buttons, and Force Quit sits one pixel from Quit with no visual difference until something gets stuck.

**Redesign.**

- **Three gauge cards**: ring + 22px stat + caption. CPU `44%` with core summary, Memory `12.9 of 25.8 GB` with the pressure as a badge (*Normal*), Swap `None`. Rings use the same `tone()` colour the bars use today.
- **"Apps using the most" card** with a `SegmentedControl` **Memory | CPU** for sort. Rows: disclosure chevron, name + process count, CPU %, inline memory bar + size. Expanded groups show sub-processes at 30px.
- **Actions on hover only**: *Quit* (secondary) and *Force Quit* (quiet). After a failed Quit the row keeps a red *Force Quit* button visible and a sub-line *Did not quit when asked*. The danger ConfirmDialog is unchanged.
- One caption at the foot explains the two verbs once instead of per row.

**Primary action.** None destructive by default; Quit on the hovered row. Force Quit stays separate, red only when relevant, and always confirmed.

**Removed or merged.** Per-row permanent buttons → hover. Unicode `▸` glyphs → lucide chevrons. Status text appended to the name → sub-line. *Apps and processes, most memory first* label → card head with sort control.

### Apps

**Problem today.** The list is a toolbar plus text rows; nothing marks an app as a candidate. The detail view is a flat list of paths with the destructive button at the very bottom, under background items and receipts, and confidence is a word in the sub-text.

**Redesign.**

- **List**: search field, `SegmentedControl` **All | Unused 90+ days · 23 | Running**, refresh icon. Card head *142 apps · 61.3 GB · largest first*. Rows: app icon (22px), name with a green running dot, sub-line *version · last used*, where *last used N months ago* is amber when over the 90-day threshold. Size bar + size + chevron. Click opens the detail in place.
- **Detail**: back button, 30px icon, name, 11px id line; the single red *Move to Trash · 6.4 GB* sits top right. Leftovers are grouped under *Application / User Library / System Library / Background items* section heads with count, total and *needs admin*; rows have a checkbox, path, label sub-line, a confidence badge (*Exact id* green, *Name match* amber) and size. Footer: selection summary and *Everything goes to the Trash and can be put back*.

**Primary action.** *Move to Trash · size* → danger ConfirmDialog → Trash.

**Removed or merged.** Toggle *Unused 90+ days* → segment with count. Receipts line → inside *Installer receipt* group. *Running* and *protected* notes → badge next to the name (not drawn; same `badge--warn`). Confidence text → badge. Bottom action bar → header button plus footer summary.

### Settings (General shown; Accounts behind the Accounts nav item)

**Problem today.** Groups are `--fill-2` boxes with 11px uppercase-ish labels, long notes in full sentences, helper status as a loose paragraph under the row, and the Accounts list uses a different row shape from the rest.

**Redesign.** System Settings idiom: 12px semibold group title in `--rk-ink-2`, one `--rk-panel` card per group, 40px rows split by hairlines, control hard right. Status becomes a badge beside the label (*Needs approval*, *Running*, *Allowed*, *2 apps*). Notes are one clause at 11.5px. Follow-up steps become their own row with a ghost button (*Open Login Items*). Auto Quit's app list collapses to a row *Preview, Calculator · Edit list*. Accounts reuses the same card: order arrows on the left, name + status badge + reading summary, a *Show* toggle on the right, and sign-in as a ghost button in the row.

**Primary action.** None; toggles are the action. Nothing destructive lives here.

**Removed or merged.** Helper status paragraph → badge. *Approve Pulse…* long label → *Allow Pulse in the background* row. Conveniences' *Last paste* diagnostic → hidden unless a failure (not drawn). Version line stays as an 11px foot.

## Spacing, type and tokens

| Role | Value |
| --- | --- |
| Page padding | 16px vertical · 20px horizontal |
| Card padding / gap | 12px × 16px · 12px between cards |
| Row heights | 36px default · 30px sub-row · 40–42px with sub-line |
| Radii | 12px card (`--rk-radius`) · 8px row hover · 6px small button |
| Body | 13px SF Pro, `-.003em` |
| Secondary | 12px · sub-line 11.5px `--rk-ink-3` · caption 11px |
| Stat | 22px / 600 / `-.02em`, tabular |
| Hero | 28px / 600 / `-.02em`, tabular |
| Titles | Tanker 18px titlebar, 20px wordmark (unchanged) |

New tokens (all in `hub/src/styles.css`, derived from `--rk-*`):

```css
--ck-space-1..6: 4 8 12 16 20 24px;
--ck-radius-sm: 6px; --ck-radius-md: 10px;
--ck-row: 36px; --ck-row-sm: 30px;
--ck-stat: 22px; --ck-hero: 28px;
--ck-track: var(--rk-fill-2);          /* bar and ring track */
--kind-apps … --kind-mixed              /* the nine chart.ts colours as CSS vars */
```

Light and dark both come from the shell's own `--rk-*` definitions; the mockups copy them verbatim with `--rk-brand: #0a84ff`.

## Implementation notes

**`hub/src/styles.css`** — replace the view vocabulary. Keep `.view`, `.muted`, `.small`, `.strong`, `.error`, `.rk-top__title`, `.wordmark`. Add `.card`, `.head`, `.hero`, `.stat`, `.track`, `.row` (new grid + hairline + hover rules), `.row--sub`, `.sub`, `.acts` / `.acts--over`, `.sechead`, `.notice`, `.footer`, `.appicon`, `.ring`, `.delta`, `.legend`, `.drives` / `.drive`, `.gauges` / `.gauge`, `.settings` / `.sgroup` / `.sbody` / `.srow`. Delete `.volume-card`, `.card-block`, `.findings-scroll`, `.finding*`, `.notes*`, `.growth*`, `.chip`, `.installers`, `.installer-chip`, `.explorer`, `.folder-row`, `.group-body`, `.setting`, `.account*`, `.order`, `.mini`, `.btn`, `.search` (use app-shell `Button` and a `.field`). Prefer `Card` from app-shell where it already matches; the `.card` class here only tightens its padding.

**`hub/src/views/Storage.tsx`** — split into `DriveStrip`, `Findings`, `Explorer`. Move growth deltas into explorer rows (`Growth.grown/shrunk` keyed by path). Installers render as a `.drive.quiet` card. Search toggles the breadcrumb for an input. `Bar` gains a `segments` prop for the stacked kind bar; kinds come from `KINDS` already imported.

**`hub/src/views/Cleanup.tsx`** — add `SegmentedControl` (Findings/History), group checkboxes, *Held back* disclosure; swap the inline confirm for `ConfirmDialog`; render category as `Badge`. Delete the `rowStyle` inline styles.

**`hub/src/views/Monitor.tsx`** — new `Ring` component (SVG, same `tone()`), gauge cards, sort `SegmentedControl`, hover actions via `.acts`, `stuck` rows keep `.acts.keep` and the `danger` button variant. Replace `▸ ▾` with lucide `ChevronRight/Down`.

**`hub/src/views/Apps.tsx`** — list: `.row.a` grid, app icon via `NSWorkspace` icon or a neutral gradient fallback, segment filter replacing `Toggle`. Detail: header with the danger button, `CONFIDENCE` map → `Badge` tone (exact/helper/group → ok, prefix/team/receipt → neutral, name → warn), `.sechead` per location.

**`hub/src/views/Settings.tsx`** — `Group` renders `.sgroup > h3 + .sbody`; `Row` renders `.srow` with an optional `badge` prop; helper and Fn status become badges; Accounts uses `.srow.acct`.

**`hub/src/App.tsx`** — no change. Shell props, groups and titles stay as they are.

**Not in scope.** Notch, app-shell internals, any behaviour: every destructive path still ends in `ConfirmDialog` → Trash → restore, and Force Quit stays a separate confirmed action.
