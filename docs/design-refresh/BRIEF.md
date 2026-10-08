# Design refresh brief — Cockpit hub

You are proposing an intuitive design refresh of the **Cockpit hub**: the small window the notch opens. Your output is a proposal and mockups, not app code.

## What Cockpit is

A personal Mac utility. A **notch** on the right screen edge (Swift, a Codenotch fork) shows always-on rings: Codex usage, Claude usage, System (memory outer, CPU inner), Disks (external outer, internal inner), with hover cards. Clicking it opens the **hub** (Tauri 2 + React, `hub/src`), which has:

- **Storage:** drive cards for every disk, a "safe to clear" findings list grouped by kind (Safe/Review), a folder list with drilldown, a small treemap, "since last scan" growth, mounted installers (DMGs) with Eject. See `hub/src/views/Storage.tsx`.
- **Cleanup:** rule-based scan, review, Move to Trash, history with Restore. See `hub/src/views/Cleanup.tsx`.
- **Monitor:** CPU, memory, swap gauges; apps and processes by memory, with Quit and a confirmed Force Quit. See `hub/src/views/Monitor.tsx`.
- **Apps:** installed apps by size; per-app review of leftovers grouped by location with confidence; uninstall to Trash. See `hub/src/views/Apps.tsx`.
- **Settings:** Accounts, Appearance, Notifications, General (startup, launcher, keyboard, conveniences, uninstall helper). See `hub/src/views/Settings.tsx`.

Read the views, `hub/src/styles.css`, `hub/src/App.tsx`, and the screenshots in `docs/design-refresh/current/` before proposing anything.

## Keep constant (do not change)

- **Shell:** RightKit `@rightkit/app-shell` fused layout: sidebar + titlebar on one chrome surface, page plane, native macOS traffic lights. Sidebar is fixed (no collapse toggle), ~170px.
- **Navigation:** the same eight sections, in two groups (Storage, Cleanup, Monitor, Apps / Settings: Accounts, Appearance, Notifications, General).
- **Fonts:** **Tanker** for the wordmark and the page titles in the titlebar; system font (SF Pro) for everything else, 13px base. Do not introduce other typefaces.
- **Icons:** lucide-react, stroke 1.75.
- **Colour:** accent/brand `#0a84ff`; app-shell `--rk-*` tokens; must work in light and dark.
- **Window:** 900×600 default, 820×520 minimum. Design for that size; no wide-screen layouts.
- **Components:** app-shell's `Button`, `Toggle`, `SegmentedControl`, `Badge`, `ConfirmDialog`, `Card`, `EmptyState` (restyle via tokens/classes if needed, don't replace the library).
- **Behaviour and safety:** everything destructive goes to the Trash with a confirm step and is restorable; Force Quit stays separate and confirmed. Do not remove these steps.
- **The notch** is out of scope.

## What we want

The hub works but feels like a collection of lists. Make it **intuitive at a glance**: what's using space, what's safe to clear, what's slowing the Mac, what to uninstall — with fewer words, clearer hierarchy, consistent spacing, and obvious primary actions per screen. Concise, calm, Mac-native.

## Deliverables (write only inside `docs/design-refresh/`)

1. `proposal.md`: principles (short), then per screen: the problem today, the redesign, the primary action, and what was removed or merged. Include a small spacing/type scale and any new tokens.
2. One self-contained HTML mockup per screen (`storage.html`, `cleanup.html`, `monitor.html`, `apps.html`, `settings.html`) at 900×600, using the same fonts (load Tanker from `../../hub/public/fonts/Tanker-400.woff2`), lucide icons (inline SVG is fine), and the accent colour, in dark mode, with a light-mode toggle.
3. A short "implementation notes" section in `proposal.md`: which files and classes change.

Do not edit anything outside `docs/design-refresh/`. Do not commit.
