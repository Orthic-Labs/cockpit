# Proposal: reach Claude Code and Codex chats from a phone (Telegram gateway)

Status: proposal, 2026-10-10, not approved. Author: Pulse Mac chat. For adversarial review.

## Goal
Adrian, away from home (phone only), sends a message to any Claude Code or Codex chat on the Mac Mini or the Dell and gets the reply, through Pulse. Waiting chats (blocked on his input) reach him unprompted.

## Options considered
- WhatsApp: official Cloud API needs a Meta business app, dedicated number, public HTTPS webhook, template rules after a 24 h silence; personal-account automation breaks the terms. Rejected.
- Own mobile app / web page: tunnel, auth, push notifications, distribution before the first message works. Deferred (v2).
- LocalSend: LAN only. Not applicable.
- Claude Code's own channel plugins / Remote Control from the Claude app: Claude only, one session at a time, no Codex, not routed through Pulse. Stopgap.
- Telegram bot gateway in the Pulse hub: chosen.

## Shape
1. The Mac Mini hub (already the always-on daemon with the roster and ssh links) long-polls the Telegram Bot API outbound over HTTPS. No inbound port. One bot; Adrian's Telegram user id allowlisted; bot token in the Keychain (RightKit secrets owner); HTTP via the RightKit http owner.
2. One Telegram forum group "Pulse", one topic per chat, created on first contact, named as the roster shows it ("Planner on Dell"). A message in a topic is delivered to that chat through the existing bridge (`send_text`), so the Dell is reached over the ssh link that exists. A "Pulse" topic takes commands (peers, status, link).
3. The phone is a roster device: "Adrian on Phone" (Target{device:"Phone", session:"telegram:<topic>"}). Chats reply with the `pulse bridge send` they already use (skill plugin unchanged); Codex replies therefore work with no Codex-side feature.
4. Waiting chats push: the hub knows when a Claude chat is blocked on input and posts "Planner on Dell is waiting: <question>" to that topic.

## Known risks
- One poller per bot: the Mac is the single gateway; if it is down the phone path is down (Dell could take over on a stale heartbeat, later).
- Dell Codex delivery needs its CLI past 0.130 for `codex queue`.
- Messages transit Telegram's servers; nothing secret goes this way.
- Topic-per-chat means chat identity must be stable across restarts (session id vs title).
