# Handoff to the CodeRight chat: reach running Claude chats from the phone

Written by the Pulse chat, 2026-10-10. Status: DRAFT until the Pulse bridge batch is on origin/main (this file says "FINAL" at the top when it is). Read this whole file first; it is the only context you get from the Pulse side.

## The goal (Adrian's words, paraphrased)
Leave the desk, open CodeRight on the phone, find any running Claude Code chat across both computers (Mac Mini, Dell) and every signed-in account, see its history and live responses, and continue that conversation. Codex is out of scope (its own app works). LocalSend stays the phone's sharing app. No terminal, no file transfer, no Telegram. No new Pulse phone app: CodeRight's Connected Computer mode is the phone side.

## What CodeRight already has
Connected Computer mode (apps/coderight-ios/README.md): pairing by QR or URL+token, session list, live transcript over SSE, send, interactive answers, interrupt/steer, reconnect. The new work is one session kind: "external Claude session" attached to a chat that is already running outside CodeRight.

## What Pulse supplies (reuse, do not rewrite)
Pulse's Rust core, `core/src/bridge/` in github.com/Orthic-Labs/pulse, has the laptop-side pieces. Extract them into a crate CodeRight's daemon depends on; the Pulse chat will do that extraction on request so both products share one copy.
- **Discovery of running Claude chats** (`roster.rs`): reads `~/.claude/sessions/<pid>.json` (and `$CLAUDE_CONFIG_DIR/sessions` for profiles), validates pid + process start, yields id, title, cwd, status (busy/idle/waiting), liveness. Claude Desktop registers each open Code tab there while its process runs.
- **Delivery into the original session** (`deliver_claude.rs`): Claude's cross-session protocol. Unix socket `messagingSocketPath` from the session file, Windows named pipe `\\.\pipe\LOCAL\cc-msg-<hash>` with an auth frame from the `<pid>.<hash>.key` file. Frame: `{"msgV":1,"msg_id","type":"user","message":{...}, "priority":"next","from":"uds:<reply socket>"}` with the body wrapped as `<cross-session-message from=... from-session from-name from-mode="bridge" provenance="agent-unverified">`. The receiver drops frames without a `from` reply address; on macOS it resolves the poster by process ancestry, so post from a registered process (Pulse registers its hub; the CodeRight daemon must register the same way: a `<pid>.json` in the sessions dir). Replies come back on the `from` socket; Pulse's `ReplyHub` shows the listener side. `procStart` is UTC ctime on macOS; `procStartFt` is a FILETIME on Windows.
- **Transcript tailing for history and live output**: Claude Code writes `~/.claude/projects/<cwd-hash>/<cliSessionId>.jsonl`, shared by every Desktop account on that machine. The Mac notch's `mac/Notch/Sources/Sessions/ClaudeTranscript.swift` parses it (Swift); a Rust reader does not exist yet and is part of the extraction.
- **Receipt states** (`mod.rs`): delivered, queued, sent, held, refused, unsupported, unknown; what each means is in `docs/bridge.md`. Show them on the phone as "received by chat", "waiting", "delivery uncertain".
- **Multi-account**: Claude Desktop runs one signed-in account at a time per machine; the transcript store is shared, so every account's chats are visible, and the live ones are those currently running (Desktop or CLI profiles). `docs/claude-account-switch.md` explains the per-account session folders.

## Hard limits
- A Desktop chat whose process is not running cannot take input. Show history and "open it on the Mac to continue". Do not resume it headless: two processes on one transcript is unsafe.
- The laptops are not reachable from the internet; they only dial out. Hetzner is the meeting point: laptops connect outbound (ssh, keys exist: `ssh vendure-auto` from the Mac), the phone reaches Hetzner. Hetzner must hold no key that opens a laptop.
- Conversation content leaving a laptop must be encrypted to the phone's key; Hetzner may see metadata, not transcripts.

## Requirements from the three reviews (read them)
`docs/bridge-astra-review.md` (the bridge), `docs/phone-gateway-hetzner-review.md` and `docs/phone-gateway-hetzner-review-2.md` (the phone design), and the adopted list in `docs/phone-gateway-hetzner.md` under "Adopted from the reviews" and "The experience (v4)". In one line each: phone signs instructions and laptops verify against directly paired keys; a message carries a dispatch deadline and permission is checked at delivery; revocations survive a server restore; chat data on the phone is behind FaceID with the complete file-protection class; feasibility journey first (real iPhone → pinned ssh host → real chat → reply on the phone); Nearby/LocalSend fixes are Pulse's and separate.

## First milestone
Chats from every account on both computers visible in CodeRight → open existing history → send → the response appears by itself → reconnect without losing place.

## Who owns what
CodeRight chat: the daemon adapter, phone UI, Hetzner relay client. Pulse chat: the shared crate extraction (discovery, delivery, transcript reader), and the Mac/Dell desktop Chat. Ask the Pulse chat through `pulse bridge send "Pulse on Adrian’s Mac Mini" "<text>"`.
