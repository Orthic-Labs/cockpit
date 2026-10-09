# Pulse bridge

Messages between AI chats on different computers, over ssh. Chats on one computer already talk natively; Pulse only adds the hop between computers and delivers natively at the far end. No network listener, no pairing, no shared secret.

## Commands (`pulse bridge …`)

- `link <device> <ssh-host> [--pulse PATH]` stores a link and checks it by listing that computer's chats. `unlink <device>` removes it.
- `peers [--local] [--json]` lists chats here plus each link's (`ssh <host> <pulse> bridge peers --local --json`, asked in parallel).
- `send "<chat> on <device>" "<text>" [--from CHAT]` delivers and prints the receipt: delivered, held or refused.
- `post <base64 envelope>` is what a link runs over ssh to deliver one message on the receiving computer.
- `status`, `inbox`, `install|uninstall` (the Pulse skill for Claude and Codex).

## Links

`<state>/bridge/links.json` holds `{device, ssh, pulse}`: the name chats show, an ssh alias or `user@host`, and the path of `pulse` there. ssh must work without a prompt (keys, BatchMode, 10 s connect, 25 s per command). Links are one-way: link both computers to talk both ways. `PULSE_BRIDGE_SSH` replaces the `ssh` program (tests).

## Sending

A remote send runs `ssh <host> <pulse> bridge post <base64 envelope>`; the envelope is base64 so no quoting survives two shells. The receiving `pulse` finds the chat, delivers, and prints one JSON receipt that the sender relays. A chat that closed is refused; one that cannot be pushed into is held in its bridge inbox.

## Delivery into Claude

- macOS: a Claude chat accepts a peer message only from a process descending from a registered session. `pulse` started by sshd has no such ancestor and would be dropped silently. So the running Pulse hub registers itself as a Claude peer, and `post` hands the delivery to the hub through control files in the state folder. If the hub is not running the message is held and `status` says "open Pulse".
- Windows: the chat's named pipe takes an auth frame with its peer token, so `pulse` posts directly.
- Codex threads get `codex queue`; whether a thread is open is unknown, so they always list as idle.

## Replies

The hub listens on one reply socket per remote peer and passes it as the message's `from`. A Claude chat answers there; the hub (`hub::on_local_reply`) sends the reply back over the link as a new envelope, shown as "<chat> on <device> via Pulse". The Windows reply socket is not implemented: Claude chats there reply with `pulse bridge send`, as Codex chats do.

## Security

- ssh keys authenticate every hop; nothing listens on the network.
- Only plain text up to 64 KiB, wrapped in a `cross-session-message` tag with any embedded tag escaped.
- Claude keys (`peerToken`) are read for the local chat only and never logged or sent.
- Anyone who can ssh as the user and run `pulse` can message that user's chats; that is the trust boundary.

## Limits

- ssh costs about 0.5 s per message and per `peers`.
- Held and refused messages stay in the chat's inbox until read.
- Codex liveness is unknown; a Codex message to a closed thread is queued, not confirmed.
