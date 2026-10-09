# Pulse bridge

Pulse's only job is cross-machine messaging between AI chats (Claude Desktop's Code tab, Codex). On one computer chats already talk natively: Claude to Claude over their messaging sockets (SendMessage), Claude to Codex with `codex queue`. Pulse pairs computers, relays signed messages and delivers natively on the receiving machine.

## Two commands

The installed CLI is `Pulse.app/Contents/Helpers/pulse` on Mac and `Pulse\Helpers\pulse.exe` on Windows.

- `pulse bridge peers [--json]`: every chat that can be messaged, here and on paired computers, as "<chat> on <device>" with kind (`claude` or `codex`) and status.
- `pulse bridge send "<chat> on <device>" "<text>" [--json]`: a chat on another computer goes through the relay and is delivered natively there; a chat on this computer is delivered natively here, so one command works everywhere. The result is `delivered`, `held`, `refused` or `queued` (no relay running yet).

Also: `pulse bridge status` (relay, chat counts, paired computers seen), `pulse bridge inbox [--from CHAT]` (only for messages that could not be delivered natively: held or refused), `pulse bridge daemon` (relay without the hub).

## Pairing

`pulse bridge pair <device>` asks a nearby computer to pair; accept the prompt there. Both sides keep one random 32-byte key. Pair once per pair of computers; a running hub reads the new key when it restarts (the sharing port is held by one process, so quit the hub to pair from the CLI).

## Skill

`pulse bridge install [--claude] [--codex] [--dry-run]` installs the Pulse skill (`plugins/pulse-bridge`) into `~/.claude/skills/pulse-bridge/` and `$CODEX_HOME/skills/pulse-bridge/` so the harness knows these commands. It is idempotent, writes atomically, keeps a differing existing file once as `SKILL.md.pulse-bak`, and `pulse bridge uninstall` reverses it. The hub's Agent bridge block has a button that runs it. Open chats need a restart.

## How replies route

Each chat sending a message is identified from its environment: `--from`, else the Claude chat owning `CLAUDE_CODE_MESSAGING_SOCKET`, else `CODEX_THREAD_ID`, else "Pulse CLI". The message arrives as "<chat> on <device> via Pulse".

- Claude target: pushed to the chat's own socket in the shape Claude uses. Its SendMessage reply goes to a Pulse reply socket on that machine, is signed and relayed back, and lands in the calling chat natively (a Claude caller via its socket, a Codex caller via `codex queue` to its thread).
- Codex target: `codex queue --thread <id> --message <text>`. The message tells the thread to answer with `pulse bridge send "<chat> on <device>" "<text>"`.
- A local Claude target of a local Claude caller replies straight to the caller's own socket.

Rosters publish every ~30 s; ones older than 2 minutes are hidden. Codex threads (the 50 most recent from `~/.codex/session_index.jsonl`) show as `idle`; their liveness is unknown. On Windows the Claude reply socket is not implemented, so Claude chats there reply with `pulse bridge send`.

## Security

Messages between computers are signed with HMAC-SHA256 under the pair key from the accepted pairing. Unsigned, wrongly signed, replayed, stale (clock skew over 5 minutes) envelopes and unknown senders are dropped. Only text (up to 64 KiB) is carried, never files or commands. Nothing leaves the local network.
