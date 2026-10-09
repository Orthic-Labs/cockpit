---
name: pulse-bridge
description: Message AI chats on paired machines through Pulse; use when the user mentions another machine, the Dell/Mac, a chat elsewhere, or asks to tell/ask a session on another computer.
---

# Pulse bridge

Pulse carries messages between Claude and Codex chats on different computers over ssh. Use it only for a chat on ANOTHER machine.

## Same machine

Do not use Pulse. Message a Claude chat here with SendMessage. Message a Codex thread here with `codex queue --thread <id> --message <text>`.

## Other machine

1. List who can be reached: `pulse bridge peers` (add `--json` for structured output). Each row is "<chat> on <device>", its kind (claude or codex) and status.
2. Send: `pulse bridge send "<chat> on <device>" "<text>"`. Send plain text, up to 64 KiB.
3. Read the result, printed by the receiving computer:
   - delivered: the chat has it.
   - held: the chat could not take it now (busy, other protocol, no Codex CLI). It waits in that chat's bridge inbox; the user or that chat reads it with `pulse bridge inbox` on the receiving computer.
   - refused: the chat's settings turned it away, or the chat is no longer open there. Run `peers` again.

Replies arrive in your chat on their own, labelled "<chat> on <device> via Pulse". Do not poll or resend. If the other chat is Codex, it replies with `pulse bridge send`.

## The binary

If `pulse` is not on PATH:
- Mac: `/Applications/Pulse.app/Contents/Helpers/pulse`
- Windows: `%LOCALAPPDATA%\Programs\Pulse\Helpers\pulse.exe`

## Linking (one time, the user does this)

`pulse bridge link <device> <ssh-host> [--pulse PATH]` on the computer that will send. ssh keys must already work without a password prompt. `--pulse` is the binary's path on the other computer. Add the link in the other direction the same way to let that computer message this one. `pulse bridge unlink <device>` removes it. `pulse bridge status` shows Pulse, chats here and each link.

## Failures

- No match: run `pulse bridge peers` and use a name it printed.
- More than one match: use the exact "<chat> on <device>" or the id from `peers --json`.
- A link answers with an error: `pulse bridge status`; the user fixes ssh or the `--pulse` path.
- "Pulse isn't running": the Mac must have Pulse open to deliver into Claude chats; ask the user to open it. The message waits in the inbox meanwhile.
