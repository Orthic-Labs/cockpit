---
name: pulse-bridge
description: Message AI chats on paired machines through Pulse; use when the user mentions another machine, the Dell/Mac, a chat elsewhere, or asks to tell/ask a session on another computer.
---

# Pulse bridge

Pulse carries messages between Claude and Codex chats on paired computers. Use it only for chats on ANOTHER machine.

## Same machine

Do not use Pulse. Message a Claude chat on this computer with SendMessage. Message a Codex thread on this computer with `codex queue --thread <id> --message <text>`.

## Other machine

1. List who can be reached:

   `pulse bridge peers` (add `--json` for structured output)

   Each chat is "<chat> on <device>", with its kind (claude or codex) and status. Chats on this computer are listed too.

2. Send:

   `pulse bridge send "<chat> on <device>" "<text>"`

   The command prints delivered, held, refused or queued. Delivered means the chat on the other machine has the message. Queued means the Pulse relay is not running yet; open the Pulse hub or run `pulse bridge daemon`.

Run the commands from your shell tool. If `pulse` is not on PATH, call the binary directly:

- Mac: `/Applications/Pulse.app/Contents/Helpers/pulse`
- Windows: `Pulse\Helpers\pulse.exe` inside the Pulse install folder

## Replies

The reply arrives natively in this chat as a message from "<chat> on <device> via Pulse". Do not poll for it. If the other chat is Codex, its reply arrives the same way once it runs `pulse bridge send` back.

## Pairing

Two computers must be paired once: `pulse bridge pair <device>` (the device name as `pulse bridge peers` or Pulse lists it). Accept the prompt on the other computer. Restart the Pulse hub afterwards if it is running.

## Failures

- No chat matches: run `pulse bridge peers` again; rosters refresh about every 30 seconds and the other chat must be open.
- "More than one chat matches": send the exact "<chat> on <device>" shown by `peers`.
- held or refused: the message could not be delivered natively. Tell the user; the receiving chat reads it with `pulse bridge inbox`.
- `pulse bridge status` shows whether the relay runs and which paired computers are visible.
