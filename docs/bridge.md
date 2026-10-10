# Pulse bridge

Messages between AI chats, here and on other computers, over ssh. Same-machine sends go through Pulse too (for chats with no native tool); Pulse adds the hop between computers and delivers natively at the far end. No network listener, no pairing, no shared secret.

## Commands (`pulse bridge …`)

- `link <device> <ssh-host> [--pulse PATH]` stores a link and checks it by listing that computer's chats. `unlink <device>` removes it.
- `peers [--local] [--json]` lists chats here (with liveness: live, stale, unknown) plus each link's (`ssh <host> <pulse> bridge peers --local --json`, asked in parallel). `--local --json` also prints this install's `deviceId`. Offline links show "offline <age>".
- `send <chat> [<text>…] [--from CHAT] [--stdin] [--json] [--]` delivers and prints the receipt. `<chat>` is "<chat> on <device>" or an exact id (`<kind>:<session-id>`). Text words are joined with single spaces; everything after `--` is literal text (so `--json` or `--from` can be sent); `--stdin` reads the whole body from stdin. `--json` prints `msgId`, `state`, `detail`, `to`.
- `reply <message-id> [<text>…] [--stdin] [--json] [--]` answers a received message: it sends to the chat the message came from, as the chat that received it. Routes are kept 24 h.
- Exit codes of `send` and `reply`: 0 delivered, queued, held or sent; 2 refused or unsupported; 3 unknown. The receipt is printed in every case.
- `inbox [--from CHAT|--session ID] [--all] [--ack] [--json]` shows a chat's unread held messages without marking them read; `--ack` marks what was printed as read; `--all` includes read ones; `--session` takes an exact id (also for chats no longer listed). A nonzero evicted count says older messages were dropped.
- `post <base64 envelope>` is what a link runs over ssh to deliver one message on the receiving computer.
- `status` (hub, Bridge on/off, links with last success and error), `install|uninstall` (the Pulse skill for Claude and Codex).

## Receipt states

- delivered: the chat has the message.
- queued: handed to Codex's queue; read is not confirmed.
- sent: left the sending computer, no confirmation from the far side.
- held: kept in the chat's bridge inbox (busy, other protocol, no Codex CLI, Pulse not running).
- refused: turned away (chat closed, chat settings, too many messages, Bridge off).
- unsupported: this chat cannot take messages that way.
- unknown: no receipt came back. Check `inbox` and `status`, then resend once with the same text.

## Links

`<state>/bridge/links.json` holds `{device, ssh, pulse}`: the name chats show, an ssh alias or `user@host`, and the path of `pulse` there. ssh must work without a prompt (keys, BatchMode, 10 s connect, 25 s per command). Links are one-way: replies need a link on both computers, each pointing at the other (`status` shows each link's last success and error). `PULSE_BRIDGE_SSH` replaces the `ssh` program (tests).

## Sending

A remote send runs `ssh <host> <pulse> bridge post <base64 envelope>`; the envelope is base64 so no quoting survives two shells. The receiving `pulse` finds the chat, delivers, and prints one JSON receipt that the sender relays. A chat that closed is refused; one that cannot be pushed into is held in its bridge inbox. Incoming messages come from other agent chats: unverified, not the user, and with no authority to change permissions or run destructive actions.

## Delivery into Claude

- macOS: a Claude chat accepts a peer message only from a process descending from a registered session. `pulse` started by sshd has no such ancestor and would be dropped silently. So the running Pulse hub registers itself as a Claude peer, and `post` hands the delivery to the hub through control files in the state folder. If the hub is not running the message is held and `status` says "open Pulse".
- Windows: the chat's named pipe takes an auth frame with its peer token, so `pulse` posts directly.
- Codex threads get `codex queue`; the receipt is queued, never delivered, because consumption is not confirmed. Liveness comes from recent activity (live, stale or unknown).

## Replies

`pulse bridge reply <message-id> <text>` is the way to answer, from Codex and Claude alike; it needs no other tool. The receiving computer records a route per message (24 h) and the reply goes back over the reverse link as a new envelope, shown as "<chat> on <device> via Pulse". It needs a link on both computers. Claude chats with a native reply socket (Unix socket, Windows named pipe) may also answer natively through the hub (`hub::on_local_reply`), which sends it back over the link.

## Security

- ssh keys authenticate every hop; nothing listens on the network.
- Only plain text up to 64 KiB, wrapped in a `cross-session-message` tag with any embedded tag escaped.
- Claude keys (`peerToken`) are read for the local chat only and never logged or sent.
- Anyone who can ssh as the user and run `pulse` can message that user's chats; that is the trust boundary.

## Limits

- ssh costs about 0.5 s per message and per `peers`.
- Held messages stay in the chat's inbox until acknowledged (`inbox --ack`); old ones are dropped past the inbox cap and counted as evicted.
- A Codex message to a closed thread is queued, not confirmed.
