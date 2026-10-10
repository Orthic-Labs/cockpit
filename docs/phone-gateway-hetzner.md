# Proposal B: Pulse relay on Hetzner, own phone app, one transport for files and agent messages

Status: proposal v2, 2026-10-10, Telegram dropped by the owner. Written against docs/bridge-astra-review.md (Bnn) and revised after docs/phone-gateway-hetzner-review.md (Hnn). Owner decisions folded in: ship the own app, no Telegram; the phone app is an ssh client first; nearby sharing gets a device trust model (allow / ask / deny) in the hub.

## Goal
From a phone anywhere, Adrian messages any Claude Code or Codex chat on the Mac Mini or the Dell, gets replies and "input needed" pushes, and sends files or the clipboard to either computer. The same transport replaces LocalSend between the two computers and the phone, so there is one path to make correct instead of three.

## Owner decisions (2026-10-10)
- **Names.** The product has two sections: **Send** (nearby sharing: files and clipboard to a device) and **Chat** (messages between agent chats on any of the owner's machines, and from the phone). "Chat" replaces "agent bridge" everywhere user-facing; the CLI becomes `pulse chat …` (one rename commit after the current batch), with `pulse bridge` kept as an alias for a while.
- **Terminal** is the third phone section, behind FaceID like Chat: an ssh terminal to Hetzner first (the owner's sudo work, today done in a third-party app), laptops later. Built from SwiftTerm (MIT, terminal view) and swift-nio-ssh / Citadel (Apache 2 / MIT, ssh client) so Chat and Terminal share one key, one FaceID gate and one connection manager; licences verified against the donor inventory before import. Every Terminal connect attaches to a named tmux session on the host (`tmux new -A -s pulse`; tmux 3.4 is already on Hetzner), so the shell survives drops and app restarts. mosh (GPLv3; mosh-server is already on Hetzner, needs UDP 60000–61000 open) is an option for the Terminal section only if the phone app is released under a GPL-compatible licence; Pulse's repository is all-rights-reserved, so this is an owner decision, not a default.
- **FaceID only on Chat and Terminal.** The phone app opens into Send with no FaceID: pick a device from the Allow list, paste or choose a file, send. Chat is the section that can act on the owner's machines, so it alone asks for FaceID, and the ssh key is used only there.
- No Telegram. The app ships first.
- The phone app is an ssh client with a FaceID-gated key (the key in the Secure Enclave), like the Moshi app the owner already uses for Hetzner, with Pulse screens on top. It runs `pulse bridge peers|send|reply|inbox` on Hetzner and draws the results; a terminal tab to Hetzner comes from the same client.
- Hetzner is a linked computer in the existing bridge (a third device beside the Mac and the Dell), not a new protocol. The relay service in the first slice is the existing `pulse` CLI plus the hub's bridge store, headless.
- Direction: the laptops connect out to Hetzner and keep that link; Hetzner holds no key that opens a laptop. Where Hetzner must deliver into a laptop, its key on that laptop is restricted to the forced command `pulse bridge post` (H04, B10). Today Hetzner has no path into either laptop; that stays the rule.
- Nearby sharing (files, clipboard) keeps the LocalSend protocol until the Pulse transport passes its journeys (H20), but gets the device trust model below now.

## Device trust for nearby sharing (hub settings)
The hub's Nearby page lists every device seen on the network (alias, kind, model, address, first seen, last seen) and the owner sets each one to one of three states:
- **Allow** (owner's own devices): send and receive without a prompt, files and text alike. A device is identified by its LocalSend fingerprint (the certificate hash it presents) plus its alias; a changed fingerprint under a known alias drops back to Ask with a notice.
- **Ask**: an incoming file or text raises the notch card and nothing lands until accepted; a send from here to that device is allowed.
- **Deny** (the default for every unknown device): requests are refused at the protocol level (`403`), no card, no toast; the hub page counts refused attempts per device so the owner can see "Neo tried 3 times" and switch it to Ask or Allow.
Revocation is one click (Allow → Deny) and takes effect on the next request. Text messages follow the same rule as files; the LocalSend-app habit of showing text without asking is gone. The notch Send card lists only Allow and Ask devices as targets. The policy lives in the hub's share settings (`nearby_devices.json`: fingerprint → {alias, state, first_seen, last_seen, refused}) and is published in share-state.json so the notch cards and the Windows notch read one truth. The owner's phone, once the app exists, is just an Allow device.

## Remote laptop later (one laptop away from home)
When the Dell is away and must reach the Mac Mini, the two are no longer on one network. The answer is the same meeting point: both laptops already link to Hetzner, so an agent message from the Dell to a Mac chat goes Dell → Hetzner → Mac through the bridge store on Hetzner, with no change to the chats. Nearby sharing over the internet (files to the other laptop) is the file leg of the relay, scheduled after text (H20). Nothing in the first slice blocks it; it is a second link in `links.json` and a retention policy on Hetzner.

## Adopted from the reviews (v3 requirements, 2026-10-10)
Both astra passes and a third read agree on the direction; these six are now requirements, not options:
1. **Hetzner holds no authority.** The phone signs every instruction (Ed25519 over a canonical envelope: destination, session generation, body hash, expiry, reply route); each laptop verifies against the phone key it paired with directly, so a compromised server can store and forward but not forge. Replies and receipts are signed the same way. COSE (RFC 9052) is an acceptable container if a maintained Swift/Rust pair exists; otherwise a fixed canonical JSON form with a detached signature.
2. **Retention is not permission.** A message carries a dispatch deadline (default 10 min, per-message override); past it the laptop refuses with `expired`, never executes. Permission and the phone's chat allowlist are checked at delivery time, not at store time. Owner instructions and agent replies are distinct message kinds.
3. **Restore keeps security decisions.** Revocations and endpoint dedupe records are replicated to every laptop and survive a server restore; after a restore the server pauses dispatch until each laptop reconciles. SQLite runs WAL with `synchronous=FULL`; disk-full refuses new work with a clear state; backups are consistent snapshots.
4. **Phone data behind FaceID too.** Chat storage (conversations, drafts, replies) uses the complete file-protection class and is unlocked with the key; app-switcher previews are hidden for Chat; Send and the share extension keep their own ungated store.
5. **Feasibility first.** Step one is a real iPhone → pinned ssh host → real chat → reply on the phone, with the pinned swift-nio-ssh version, biometric cancellation and reconnect behaviour qualified before any mailbox or UI work. swift-nio-ssh's Secure Enclave P-256 support is the claim to verify there (unverified until that step).
6. **Nearby repair is its own slice.** The LocalSend TLS possession check and the text-consent fix ship now with their own gate; the Allow/Ask/Deny redesign follows; neither blocks phone messaging.

Order: phone feasibility journey → signed mailbox contract → durable send/reply → Mac/Dell × Claude/Codex qualification → revoke/restore/crash journeys → terminal, pushes, files.

## Topology
- **Relay** (`pulse relay`, the Rust core built headless) runs on Adrian's Hetzner server as a systemd service behind Caddy (TLS). It is a store-and-forward message service and blob store. It never executes anything on a laptop.
- **Nodes** are the Mac Mini hub, the Dell hub and the phone app. Every node connects *outbound* to the relay over HTTPS (long-poll or WebSocket). No inbound ports anywhere; no ssh in the message path. ssh keys stay for administration only (B10 removed from the data path).
- **LAN fast path** for files: when two nodes are on the same network the relay hands each the other's candidate address; the sender tries a direct TLS connection first and falls back to the relay. Agent messages always go through the relay (one durable path; B04–B06).

## Identity and trust (B01, B07, B09, B12)
- Each node has a device key pair (Ed25519 for signing, X25519 for encryption) generated at install; `device_id` is derived from the public key. Enrollment: the relay shows nothing; a node is admitted by a one-time code minted on an already-enrolled node (the Mac hub's Agent page, or the phone scanning a QR). The relay stores `(device_id, pubkey, name, kind, enrolled_at, enrolled_by)`.
- Every request is signed by the device key (timestamp + nonce, 5-minute window; relay keeps a nonce journal). The relay stamps the authenticated `from.device_id` onto every envelope; a claimed label never travels unverified.
- Chat identity is `(device_id, harness, session_id, generation)`; titles are display only. Conversations are `(device_id, session_id)` and keep their id across renames and resumes; a fork or a new session is a new conversation.
- Message bodies and file blobs are end-to-end encrypted to the recipient device's key (sealed box). The relay sees device ids, conversation ids, sizes and timestamps, not content. Losing a device key means re-enrolling that device; the relay cannot recover content.

## Message service (B04, B05, B06, B08, B11, B13, B16, B18, B19)
- Relay tables: devices, sessions (published by each hub: id, harness, title, liveness, last_activity, capabilities such as `codex_queue`), conversations, messages `(id, conversation, from_device, from_session, to_device, to_session, kind, body_cipher, hash, created, expires)`, receipts `(message_id, state, detail, at, by_device)`, blobs, events (input-needed), nonces, push tokens.
- Receipt states are the reviewed set: stored (relay has it), dispatched (a hub pulled it), queued (native queue accepted), delivered (chat confirmed), held (hub inbox), refused, unsupported, unknown. A sender sees the chain, not one word.
- Each hub keeps a local outbox (persisted before any network call) and pulls its mailbox with a cursor it only advances after local commit; duplicates are rejected by `(from_device, message_id, hash)`. Per-conversation dispatch is serialized by the hub.
- Replies carry `in_reply_to`; the route is the conversation, never a device alias. Codex and Claude reply with `pulse bridge reply <message-id>`; the hub resolves the conversation.
- Limits are explicit and enforced at the relay: 64 KiB message body, 2 GiB blob in 8 MiB resumable chunks, 30-day retention for messages, 7-day for blobs, 500 MB per-device blob quota. Oversize is refused before upload with the limit in the detail.
- Hubs run the existing local adapters (Claude cross-session socket/pipe, Codex `queue`) unchanged except for the receipt states; nothing in the relay knows the harness protocols.

## Waiting and notifications (B14, B22)
- A hub publishes `input-needed` events `(session, generation, event_id, kind, expires)` when a Claude chat reports waiting or a Codex thread's status requires input; the relay pushes to the phone via APNs. Content-free by default ("Planner on Dell needs input"); a chat can be enrolled for content so the question text rides along (encrypted).
- Liveness on the phone is what the hub last reported plus its age; never a guess.

## Files and clipboard (replaces LocalSend)
- Send from any node to any node: file(s), folder (zipped by the sender), text/clipboard. Blob upload is chunked and resumable; the recipient hub downloads and saves to the existing Downloads location, with the same accept/decline card the notches already show for LocalSend. The notch Send card keeps its shape: nearby devices become "your devices" (all enrolled nodes, with reachability from the relay), paste/drop unchanged.
- LocalSend is removed after all three nodes run the new transport; nothing else on the LAN needs the LocalSend protocol.

## Phone app
- iOS first (Swift, SwiftUI): Devices, Chats (grouped by computer, liveness and age), Conversation (messages, receipts, reply), Send (share-sheet extension: files, text), Inbox (input-needed). Enrolls by QR from the Mac hub. Keys in the Secure Enclave-backed keychain.
- Pushes via APNs through the relay (Apple provisioning exists per workspace rules).

## Lifecycle and recovery (B20, B21, B23)
- Relay: systemd, restart on failure, SQLite with WAL, nightly backup to R2. Hubs: already supervised by the notch; the relay client is part of the hub's bridge service with its own health ("relay: ok 12s" / "offline 3m") shown on the Send card and the hub page. A hub restart replays its outbox and resumes its cursor; no listener state needs to survive.

## What this does not do
- No remote execution: a message is text delivered into a chat; the chat's own permission mode governs what it does. The phone cannot change links, policies or permissions.
- No multi-user relay: one owner, enrolled devices only. Sharing with other people is out of scope.

## Risks
- The relay is one server; if Hetzner is down, nothing crosses machines (the LAN fast path keeps file sends working at home). Acceptable for one person; a second relay later is a config list.
- E2E key management adds a re-enroll step when a device is reinstalled.
- Three codebases touched (core relay + hub client, notches, phone app). Critical path: relay service → hub client and file transport → phone app.

## Comparison with the Telegram proposal
Telegram needs no server and no app but inherits every bridge finding and adds a third-party trust boundary; the reviewer judged it acceptable only as a convenience on top of a message service that did not exist. This proposal builds that message service once, on Adrian's own server, with real device identity and end-to-end encryption, and gets files and clipboard for free. It costs an iOS app and a relay deployment.
