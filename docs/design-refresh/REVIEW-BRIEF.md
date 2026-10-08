# Adversarial review brief — Cockpit hub design refresh

Review the proposed design refresh adversarially. Assume it has real flaws and find them. Do not rewrite the design; judge it.

## What to read

- `docs/design-refresh/BRIEF.md`: the brief the designer was given, including what had to stay constant.
- `docs/design-refresh/proposal.md`: the proposal.
- The mockups: `storage.html`, `cleanup.html`, `monitor.html`, `apps.html`, `settings.html` (900×600; a sun/moon button toggles light/dark). Open them in a browser and screenshot them if you can; otherwise read the HTML/CSS.
- The current app for comparison: `hub/src/views/*.tsx`, `hub/src/styles.css`, and the screenshots in `docs/design-refresh/current/`.

## Questions to answer

1. **Is this good design for 2026?** Compare against current best-in-class desktop utilities and macOS 26 / Windows 11 conventions (Liquid Glass era on macOS, Fluent on Windows). Is it modern and minimal, or dated, busy or generic dashboard-ware?
2. **Cross-OS.** The hub is Tauri and will ship on Windows later with the same UI (RightKit app-shell draws caption buttons on Windows). What in the design is Mac-only, breaks on Windows, or reads as a Mac app wearing a costume on Windows?
3. **Intuitive at a glance.** For each screen: can a first-time user tell in 3 seconds what it says and what to do? Where is it still a list of lists?
4. **Density and the 900×600 window.** Does it fit at 900×600 and at the 820×520 minimum without clipping, overflow or cramped hit targets?
5. **Consistency with the brief.** Anything that violates "Keep constant" (shell, nav, Tanker for wordmark and titles only, system font elsewhere, lucide icons, `#0a84ff` accent, app-shell components, Trash and confirm safety).
6. **Safety and trust.** Hover-only row actions, a single "Clear safe" primary, collapsed "held back" items: do any of these hide risk or make destructive actions too easy or too hidden?
7. **Accessibility.** Contrast in both themes, colour-only meaning, keyboard and focus order, hover-only affordances, target sizes, reduced motion.
8. **Implementation cost and risk** against the current React views and the RightKit app-shell.

## Output

Write `docs/design-refresh/review.md`:
- A verdict in one paragraph: ship as is, ship with fixes, or rethink.
- Findings ranked by severity (blocker / major / minor). Each finding gives the screen, the issue, why it matters, and a concrete fix.
- A short "what's genuinely good, keep it" list.

Do not edit any other file. Do not commit.
