# Pulse bridge

AI chats on paired computers (and on the same computer) message each other through Pulse. A Claude or Codex chat lists who it can reach, sends text, and reads what was sent to it.

## How messages travel

1. A chat calls `bridge_send` (MCP tool) or the CLI sends. The text becomes a signed envelope addressed to a chat on a device.
2. A chat on this computer is delivered to directly. A chat on a paired computer goes into an outbox; the Pulse relay (hub, or `pulse bridge daemon`) sends it to the other computer, whose relay delivers it locally.
3. Each computer publishes its chat roster to its paired computers about every 30 seconds; rosters older than 2 minutes are not shown. A peer reads as "<chat title> on <device alias>".
4. The sender gets a receipt: `delivered`, `held` (kept in the chat's bridge inbox), `refused`, or `queued` (waiting for a relay or the other computer).

## Delivery per app

- Claude: the chat's own messaging socket from `~/.claude/sessions/*.json`. Only chats whose process is running are listed. Pulse sends one JSON line in the shape a Claude Code 2.1.293 session itself sends: `msgV:1`, `msg_id`, `type:"user"`, `message{role,content}`, `priority:"next"`, `from` (`uds:<reply socket>`). The sender identity is inside the content, in a `<cross-session-message from=".." from-session=".." from-name="<peer> via Pulse" from-mode="bypass">` wrapper; wrapper tags inside the text are escaped. No auth line on macOS/Linux; native Windows sends the auth line (key file token) first. Real chats are silent on accept, so no control frame within 3 seconds is `delivered`; an explicit hold or refusal is reported as such. Replies arrive on the reply socket in the same wrapper shape (older top-level `from_name`/`from_session_id` fields are still accepted), and empty probe connections are ignored.
- Codex: `codex queue --thread <thread id> --message <text>` using the Codex CLI bundled with the ChatGPT app (or one on PATH). Exit 0 prints "Queued message <id> for thread <id>." and the message is `delivered` with that id in the receipt detail. The Codex desktop app owns the thread, processes the message and can reply. A failure, or a missing CLI, is `held` with the error text and the message stays readable through `bridge_inbox`.
- Codex threads come from `~/.codex/session_index.jsonl` (read only): the 50 most recently updated, listed as kind `codex` with status `idle` (whether a thread is open is unknown). They are reachable whether or not they ever called `bridge_whoami`.

## Setup

`pulse bridge install` registers the Pulse MCP server (`pulse bridge mcp`) with Claude Code, Claude Desktop and Codex (`--claude`, `--codex`, `--dry-run`, `--json`). Other config keys are left alone and a one-time `.pulse-bak` copy is kept. Running chats need a restart to see the server. `pulse bridge uninstall` removes it. Pair computers from the hub; a prompt appears on the other one.

## MCP tools

- `bridge_list`: reachable chats with `kind` (`claude` or `codex`), device, name, folder, status, and for Codex the `threadId`.
- `bridge_send`: `to` (id or name from `bridge_list`) and `text` (up to 64 KiB); returns message id, status, detail.
- `bridge_inbox`: unread messages for this chat, marked read; use it when a message was held or refused.
- `bridge_whoami`: this chat's id, name, device and whether the relay is running.

## Security

Messages between computers are signed with HMAC-SHA256 using a 32-byte pair key exchanged when the user accepted the pairing. Only known (paired) devices are accepted; unsigned or wrongly signed envelopes, replays and unknown senders are dropped. Only text is carried, never files or commands.

## Limitations

- Windows: the Claude reply socket is not implemented, so Claude chats there cannot reply over it and use the MCP tools. Codex CLI locations on Windows are best guesses.
- Codex replies come back only through MCP (the reply lands in the sender's `bridge_inbox`) or by the Codex thread calling `bridge_send` itself; Pulse does not read Codex thread output.
- Codex thread liveness is unknown, so every listed thread shows as `idle`.
