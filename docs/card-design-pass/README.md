# Notch hover cards: design pass (2026-10-10)

Scope: the hover card of every gauge (Claude, Codex, System, Disks, Send) on Mac and Windows, from the captures in `docs/parity-screens` (Mac right edge, Dell). Mock of the proposed cards: `cards.html` beside this file (open in a browser; before and after side by side).

## What is good and stays
- The card shape, tail, dark material, bar + reading rhythm and the header glyph are right and identical on both platforms.
- Reset times on the Mac already follow one rule: relative under a day ("Resets in 51 min"), else weekday and time.
- Band colours (green / amber / red) read at a glance and match the ring.

## Findings, ranked

1. **Two numbers for one fact.** Every window prints "21% Used · 79% left" under a bar that already shows 21%. The reader has to pick which number matters. Keep one: the one that decides behaviour, what is left. "79% left" in the strong weight, the bar carries the rest. Codex "Weekly limit" the same. (Both platforms.)
2. **Three lines per window where two will do.** Label, bar, reading stack to ~60 px per window; the Claude card with three windows is the tallest thing on the screen. Put the reading on the label line, right-aligned in the strong weight, and the reset time under the bar in the muted weight. Windows shrink by a third; the Claude card with three windows fits in the height of today's two. (Both.)
3. **Truncation inside a fixed card.** Windows cuts "Install smartmontools for dri…", "— · Windows only rep…", "Copy last: gh aut…" and even "Pas…". A card is not a table cell: secondary text wraps to a second line or the copy is shortened, buttons never truncate. Copy fixes: "Install smartmontools" (link row), "Not reported by Windows" for CPU temperature, "Not available" for fans, "Copy last" with the preview as a muted second line. (Windows; the Mac has the same risk with long device names.)
4. **"Usage" in every title.** "Claude Usage" and "Codex Usage" are right; "Disks Usage", "System Usage" are not English, and "Send" has none. Titles become "Claude", "Codex", "System", "Disks", "Send", with the plan or scope as the subtitle ("Max 5x", "Pro", "20 cores · 64 GB", "3 drives", "1 nearby"). (Both.)
5. **Windows Claude card has no plan subtitle** where the Mac shows "Max 5x"; the two cards also set the title ~2 px larger on Windows. Match the Mac: subtitle line, same title size. (Windows.)
6. **Windows session reset is absolute** ("Resets Sat 12:00 PM" for a window under five hours away) where the Mac says "in 51 min". Apply the Mac rule. (Windows.)
7. **Codex numbers.** "Available credits 62500" wants a thousands separator; "Unused resets · 1 unused reset" says it twice: "Unused resets · 1". (Both.)
8. **System header orphan.** "GPU 61 °C" floats top-right with no label on the row that has the GPU; move it onto the GPU row ("1% busy · 61 °C") and keep the header for the subtitle. (Both.)
9. **Send card hint reads as data.** "Ctrl+V sends the clipboard · drop files here" sits in the same weight and position as the device rows; it is a hint: muted, smaller, below the list, one line. The bottom bar keeps "Copy last" left and "Paste" right; neither truncates. (Both; the Mac already dims it slightly.)
10. **Drive health row.** "Drive health · Install smartmontools for dri…" is an instruction, not a reading; make it a quiet link row at the bottom, full text, and only when smartmontools is missing. (Windows.)

## Not changing
- Width (600 px Mac units), corner radius, tail, glyph, bar height, colours.
- The Restart button on the Claude header.

## Implementation map
Mac: `mac/Notch/Sources/Features/TooltipCard.swift` (window row layout: label + reading line, reset line), `UsageModel.swift` `ProviderReading`/`LimitWindow` (one-number reading), provider snapshots for titles/subtitles (`ClaudeOAuthProvider`, `CodexLocalProvider`, `SystemProviders`, `NearbySharing.providerSnapshot`), `ClaudeUsageCLI`/`CodexUsage` for number formatting. Windows (Dell's lane): `windows/src/card.rs` and `send.rs`, `usage.rs` for reset wording and the plan subtitle; `render.rs` wraps secondary text to two lines.

Rendered acceptance: CI `notch-render__*` view shots on both platforms after the change; compare against `cards.html`.
