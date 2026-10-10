# Pulse product decisions

Owner decisions, in Adrian's words where it matters, with the date they were made and where they live in the code. Newest at the bottom of each section. Add to this file whenever a decision is made in a chat; it is the one place to check before changing behaviour.

## Shape of the product
- **Notch, hub, core.** Mac notch (Swift), Windows notch (Rust), one Tauri hub, one Rust core and CLI. No Dock or menu-bar item; the notch's right-click shows only Quit. (docs/plan.md)
- **The hub is the daemon.** "If the notch is on, the hub has to be on." The notch starts the hub hidden, checks every 5 s, restarts after 30 s if it died, and restarts it when its bridge heartbeat stalls for 90 s. Closing the hub window hides it; only quitting the notch quits the hub. (2026-10-09; HubLauncher.swift, windows/src/hub.rs)
- **The notch edge is right.** "I deliberately set it to right edge, since day 1." Never change it in code or by install. (2026-10-09)
- **The notch must never disappear.** Hidden launches unhide themselves; a blank Windows notch self-heals. (2026-10-09/10)
- **Pulse is exposed under the plain computer name**, no "(Pulse)" suffix. (2026-10-09)

## Gauges
- Six cells: Claude, Codex, CPU, Memory/System, Disks, Send, Tools. Every gauge can be turned on and off from the hub in one place; at least one stays on. (2026-10-10)
- **Ring pulse on Send:** the Send ring pulses for 6 s after an agent message is sent or received: green for delivered or queued, amber for held or sent, no pulse for refused or unknown. (2026-10-10)
- **Hover cards (design pass 2026-10-10, docs/card-design-pass):** one number per window ("79% left"), two lines per window, no truncation, titles without "Usage" except Claude and Codex, plan or scope as subtitle. Awaiting Adrian's pick of which to apply.
- **Claude ring numbers follow Claude Desktop's own cached usage** whenever it is under 30 min old; the CLI estimate and the token endpoint only when Desktop has nothing. (2026-10-10, ClaudeOAuthProvider)

## Send (nearby sharing)
- Keep the LocalSend protocol for files and clipboard between the owner's devices and the phone's LocalSend app. (2026-10-09)
- **Bottom bar: Copy last and Paste as equal buttons.** Paste is live only when the clipboard holds files, an image or text; the Copy last preview lives in its tooltip. (2026-10-10)
- **A paste lands on the other computer's clipboard**, not only in the save folder: text as text, one image as the image, otherwise the files; the card says "Copied from <computer>". Wire: `pulse.clipboard` in the LocalSend request. Drops of files are saved only. (2026-10-10)
- **Device trust: Allow / Ask / Deny.** The hub lists every device seen on the network; unknown devices are Deny by default and refused silently at the protocol level with a refused-attempt count; Ask prompts; Allow sends and receives with no prompt. Text follows the same rule as files (today text shows without asking; that goes away). Revoke is one click. Identity is the certificate fingerprint plus a proof-of-possession check that the reviewer found missing today; that check ships first. (2026-10-10; to build)
- After sending, the toast closes immediately. (2026-10-09)

## Tools
- **A sixth cell, Tools:** a grid of utilities on hover, no keyboard: Snip, Screen, Window (screenshots sent straight to the other computer's clipboard, nothing saved to the Desktop), Paste to other, Copy last, Lock screen. Screenshots do not live in Send. (2026-10-10)
- **Middle-click wheel:** the same tools as a radial dial under the pointer, anywhere. A middle click on a link passes through (the browser opens its tab); the link check is the accessibility element under the pointer, the rule HeardRight already uses. Adrian will disable HeardRight's middle click. Needs Accessibility. (2026-10-10)
- Three screenshot modes match macOS: region (⌘⇧4), whole display (⌘⇧5 first option), one window (⌘⇧5 second option). (2026-10-10)

## Chat (agent messaging, formerly "bridge")
- **Name: Chat.** Two sections in the product: Send and Chat. CLI becomes `pulse chat …` with `pulse bridge` kept as an alias for a while. (2026-10-10)
- Transport between computers is ssh with the owner's keys; the hub registers as a Claude peer; replies route by message id and device id. Receipt states mean one thing each: delivered, queued, sent, held, refused, unsupported, unknown. Incoming messages are unverified agent text, never the user. (docs/bridge.md, docs/bridge-astra-review.md, 2026-10-10)
- Codex threads come from the Codex state database (not archived, newest first) with folder and age. (2026-10-10)
- "Bridge off" is a core policy every path checks. (2026-10-10)

## Phone
- **No Pulse phone app and no Telegram.** The phone side is CodeRight's Connected Computer mode, extended to attach to already-running Claude chats; Codex stays in its own app; LocalSend stays the phone's sharing app; terminal work stays in Moshi. Pulse supplies the laptop connector pieces (chat discovery across accounts, transcript tailing, delivery into the original session). Handoff: docs/coderight-handoff.md. History: docs/phone-gateway-*.md. (2026-10-10)
- Laptops connect out to Hetzner; Hetzner never holds a key that opens a laptop. (2026-10-10)

## Permissions
- The hub's Permissions page states exactly which enabled feature needs which permission ("Needed for: …"), and "Not needed by anything you have on" otherwise. Entries: Accessibility, Screen Recording, Full Disk Access, Local Network, Background helper, Finder menu, Launch at login. (2026-10-10)

## Builds
- Dev builds from CI are signed but not notarized, and macOS will not register an app extension from such an app: the Finder menu (Cut, Copy Path, Open in Terminal) is absent on every dev install and returns with a notarized build. The Permissions row says so instead of "Off". Reported to the RightKit chat (dev-lane notarization). (2026-10-10)

## Claude accounts
- The restart button quits Claude politely for 20 s, force-quits for 10 s more, retries the sync while helpers wind down, and keeps a failure visible. The sync prefers the copy of a session that is further along (turns, then last message, then time), never the newest timestamp alone. (2026-10-10)
- The chat mirror is change-driven (FSEvents on the signed-in account's folder, plus the account change), with no timer. It copies one way, from the signed-in account to the others; the button stays as the restart and the full two-way merge. (2026-10-10)
- Pulse chat (the roster) lists in-app chats only; CLI sessions are reached over ssh (Adrian's words to the Dell chat). (2026-10-10)

## Working rules (how the chats operate)
- The Mac chat owns Mac, core, hub and docs; the Dell chat owns windows/src; one pushes at a time and tells the other first; CI runs must not cancel each other. (2026-10-10)
- No local Cargo or Swift builds for the public repo: CI builds. Dev builds are installed from the CI artifacts. (AGENTS.md)
- Never touch RightKit from the Pulse chat; report RightKit problems to the RightKit chats. (2026-10-09)
- Never capture Adrian's screen, raise windows or post notifications from an agent; QA uses the hidden crawl and CI evidence. (2026-10-09)
