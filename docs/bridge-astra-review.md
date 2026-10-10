# Pulse agent messaging: adversarial source review

Reviewed commit: `a619e38f3908b6d1f0d3f798ea007574b3a0b164`. Proposal is an untracked working-copy file, absent from that commit; proposal citations refer to its 25-line review input. Other citations refer to committed source. No builds, tests, app launches, network access or Git writes. “Works” below means an implemented source path under stated prerequisites, never observed installed behavior.

## 1 Gateway proposal verdict

**CHANGE. Reject implementation as written.** Telegram can be an optional transport, but current bridge cannot safely provide promised phone → arbitrary chat → phone journey. Proposal assumes phone endpoints, stable identities, reliable receipts & question capture that source does not supply (`docs/phone-gateway-proposal.md:16`, `docs/phone-gateway-proposal.md:18`, `docs/phone-gateway-proposal.md:19`; B01–B09 below).

### Architecture failures

| Issue | Evidence | Failure & required change |
|---|---|---|
| Allowlisted user is insufficient routing authorization | `docs/phone-gateway-proposal.md:16`, `docs/phone-gateway-proposal.md:17` | Specify exact numeric bot, user, group & topic IDs, permitted update types & destination capabilities. Reject absent/ambiguous sender identity, bot/channel/anonymous senders, forwarded commands, edits & unknown topics. Group membership, title & username must never authorize execution. Bind callback actions to user, destination, expiry & single-use nonce. |
| Bot-token protection stops at storage | `docs/phone-gateway-proposal.md:16`, `docs/phone-gateway-proposal.md:24` | Keychain does not address token exposure through HTTP URLs, logs, crash reports, backups or process access. Token theft permits bot control, transcript exposure, misleading outbound messages & polling disruption; it is distinct from takeover of allowlisted human account. Define redaction, rotation, revocation & recovery. Human account takeover can issue valid-looking commands; Telegram identity alone cannot protect against that. |
| Phone text inherits agent power | `docs/phone-gateway-proposal.md:17`; `core/src/bridge/deliver_claude.rs:341`, `core/src/bridge/deliver_claude.rs:440`; `core/src/bridge/deliver_codex.rs:313` | Incoming text becomes a user-role message; Claude wrapper asserts `from-mode="bypass"`. Tag escaping does not make natural-language instructions safe. Distinguish authenticated human instructions from forwarded/quoted content & agent output. Preserve harness permission mode. Authorize target/workspace/actions at gateway; never infer authority from prose saying “Adrian approved.” |
| Phone is not an existing bridge endpoint | `docs/phone-gateway-proposal.md:18`; `core/src/bridge/roster.rs:257`, `core/src/bridge/mod.rs:426`, `core/src/bridge/mod.rs:443`, `core/src/bridge/hub.rs:345` | Roster discovers Claude/Codex; remote sends require SSH links; receive resolves local sessions. `Target{device:"Phone",session:"telegram:<topic>"}` alone cannot receive anything. Add typed transport endpoints & routing, including gateway routes usable from Dell. “Skill unchanged” also conflicts with its computer-only instructions (`plugins/pulse-bridge/skills/pulse-bridge/SKILL.md:8`). |
| Topic identity is underspecified | `docs/phone-gateway-proposal.md:17`, `docs/phone-gateway-proposal.md:25`; `core/src/bridge/roster.rs:297`, `core/src/bridge/roster.rs:314` | Persist `(bot_id, group_id, topic_id) → (device_uuid, harness, session_id)` independently of titles. Rename updates display only. Resume of same session retains topic; fork/new session gets new binding. Closed/archived/missing sessions become tombstones; never resolve their old topics through fuzzy title search. Handle topic deletion, group migration & backup restore explicitly. |
| Long polling lacks commit semantics | `docs/phone-gateway-proposal.md:16`, `docs/phone-gateway-proposal.md:22`; `core/src/bridge/mod.rs:443` | Advancing update offset before durable ingestion loses commands on crash; advancing after delivery without dedupe repeats commands. Persist raw accepted update, authorization decision, destination & stable request ID before offset advance. Separate ingress journal, destination dispatch & outbound Telegram journal. Retries retain original ID. Telegram acknowledgement means “stored by gateway,” not “agent acted.” |
| Single gateway has neither supervision nor recovery design | `docs/phone-gateway-proposal.md:16`, `docs/phone-gateway-proposal.md:22`; `mac/Notch/Sources/Sharing/NearbySharing.swift:189`, `windows/src/send.rs:919` | Mac sleep/reboot, user logout, dead hub, SSH outage & Telegram outage interrupt path. Current notch restart logic depends on Nearby sharing being enabled. Use supervised gateway service independent of notch/LocalSend. Start with one writer; stale heartbeat alone is unsafe failover because partitioned old gateway can keep polling/sending. Failover needs shared durable state & fenced leadership, or explicit manual transfer. |
| Waiting question is not roster status | `docs/phone-gateway-proposal.md:19`; `core/src/bridge/roster.rs:123`, `core/src/bridge/hub.rs:414` | Bridge carries status strings, timestamps & names, not question text, permission request IDs or answer channels. Add harness-specific waiting events with event ID, session generation, question type, expiry & resolution. Deduplicate notifications; reject replies to obsolete prompts. Do not treat generic chat text as approval of native permission dialogs. Codex waiting push remains unspecified despite goal covering both harnesses (`docs/phone-gateway-proposal.md:6`). |
| “Nothing secret” has no enforcement | `docs/phone-gateway-proposal.md:24`; `core/src/bridge/deliver_codex.rs:125`, `core/src/bridge/hub.rs:328` | Titles may derive from first user message/preview; replies & questions may contain secrets even when phone input does not. Default notifications to “input needed” without contents. Explicitly enroll permitted sessions; minimize exported metadata; define retention/deletion & outbound filtering. Filtering is not a confidentiality guarantee. |
| Remote `link` is unnecessary command-execution surface | `docs/phone-gateway-proposal.md:17`; `core/src/bridge/links.rs:65`, `core/src/bridge/links.rs:224` | Account compromise could add destinations or supply shell-active binary paths through proposed control topic. Keep topology changes in local administration. Phone gets roster, status, send, reply & cancellation only, restricted to enrolled endpoints. |

### Concrete replacement

1. **Build transport-neutral message service first.** Persist device UUIDs, endpoint IDs, authenticated principal, authorization scope, conversation ID, parent message ID, expiry, body & payload hash. Keep human instructions, agent messages, waiting events & permission requests as distinct types. Reused ID with different content is rejected.
2. **Use durable ingress/outbox on gateway & receiving machines.** Transactionally claim work; record `stored`, `dispatched`, `queued`, `accepted`, `replied`, `refused`, `expired` or `outcome_unknown`. Retry only states whose semantics permit it. Native harnesses without idempotency/acknowledgements cannot support an honest exactly-once execution claim.
3. **Give each machine restricted receiver capability.** Bind SSH credential/device identity to sender metadata; expose bounded `roster`, `post`, `receipt` operations through fixed receiver command. Persist reverse route independently of display alias. Check policy on every ingress, including CLI & native replies.
4. **Add Telegram adapter after that boundary.** Private, dedicated group; explicit user/group/topic allowlists; durable topic table; plain-text outbound rendering; bounded text only initially. Exclude edits, forwards, attachments & topology commands. Persist update IDs before advancing poll cursor. Chunk long replies with message/chunk identity; respect backoff & avoid blind retries after ambiguous outbound acceptance.
5. **Keep one supervised gateway initially.** Store token with restricted Keychain access, redact HTTP diagnostics, document rotation & retain encrypted recoverable routing/journal state. Report last successful poll & device reachability separately. Use content-free notifications until sessions are enrolled for content export.
6. **Return answers through explicit conversations.** `pulse bridge reply <message-id> --stdin` resolves stored route. Codex must explicitly use it until an actual assistant-output adapter exists; Claude native replies must authenticate source & survive restart. Waiting events require separate harness integrations.

These changes address concrete missing contracts in `core/src/bridge/envelope.rs:40`, `core/src/bridge/mod.rs:383`, `core/src/bridge/hub.rs:305`, `core/src/bridge/control.rs:95` & `core/src/bridge/store.rs:247`; they are proposed work, not existing capability.

### Alternatives, compared honestly

Comparison uses proposal & repository source only; vendor feature/version claims in `docs/phone-gateway-proposal.md:9`–`docs/phone-gateway-proposal.md:12` were not independently verified.

| Option | Decision | Tradeoff |
|---|---|---|
| Private web UI over authenticated VPN/overlay, using same message service | Preferred long-term if agent commands or replies can be sensitive | Avoids putting conversation content through Telegram bot transport; offers explicit target, permissions & receipts. Requires browser UI, device enrollment, service reachability & separate push solution. Offline durability still needs implementation. Proposal dismisses this as an app-distribution project, but browser UI needs no native app (`docs/phone-gateway-proposal.md:10`). |
| Telegram adapter above | Acceptable convenience v1 for explicitly enrolled sessions | Lower phone-interface effort & existing notification surface; expands trust to Telegram account, bot token & group administration. Must not be mistaken for secure remote execution merely because polling is outbound (`docs/phone-gateway-proposal.md:16`). |
| Phone SSH client over private network, calling CLI | Narrow bootstrap for deliberate sends | Reuses SSH setup, avoids bot mapping & token. Poor discovery, awkward replies, no proactive waiting notifications; current CLI still has B04–B19 defects (`docs/bridge.md:7`, `docs/bridge.md:29`). |
| Harness-native remote control | Claude-only stopgap, subject to verifying proposal's claims | Potentially avoids reverse-engineered Claude socket protocol & preserves native permission UX. Does not satisfy cross-harness goal as described by proposal (`docs/phone-gateway-proposal.md:12`; `core/src/bridge/deliver_claude.rs:11`). |
| Separate Telegram bot/gateway per machine | Consider only if surviving Mac downtime is essential now | Removes Mac as Dell's relay dependency & avoids competing pollers per bot; doubles token/configuration management & splits roster/conversations. Does not solve authorization or dedupe. Better bounded failure mode than unspecified stale-heartbeat takeover (`docs/phone-gateway-proposal.md:22`). |

## 2 Bridge findings ranked by severity

High = wrong-recipient/forged authority, unrecoverable message loss or core journey failure. Medium = conditional exposure, degraded availability, discovery or recovery defects. Low = localized installation/documentation defects. Same-user/SSH-account access is already powerful; findings distinguish that existing trust boundary from newly granting Telegram access.

### B01 — Sender labels & native replies are forgeable

**Severity: High.** Evidence: `core/src/bridge_cmd.rs:258`; `core/src/bridge/mod.rs:443`; `core/src/bridge/mod.rs:260`; `core/src/bridge/deliver_claude.rs:759`; `core/src/bridge/hub.rs:311`.

**Failure:** Any process running as user, or SSH client authorized as user, can submit arbitrary `from.device/session/name`; receiver neither binds them to SSH identity nor verifies target device. `--from` deliberately permits impersonating any discovered local chat. Reply listener ignores auth frame & accepts any claimed known session ID, then hub re-labels it as that session. Unix permissions & Windows DACL restrict users, not chats (`core/src/bridge/deliver_claude.rs:717`, `core/src/bridge/deliver_claude.rs:1174`). This is not unauthenticated Internet access; it is absence of claimed chat provenance. Phone routing would inherit it.

**Smallest fix:** Mark existing labels unverified; bind receiver to authenticated device principal, validate target device, remove unrestricted identity overrides from ordinary sends & bind native reply credentials/process identity to session. Do not present session IDs as secrets or authorization.

### B02 — Untrusted metadata becomes executable reply guidance

**Severity: High.** Evidence: `core/src/bridge/deliver_codex.rs:313`; `core/src/bridge/deliver_claude.rs:341`; `core/src/bridge/deliver_claude.rs:440`; `plugins/pulse-bridge/skills/pulse-bridge/SKILL.md:16`.

**Failure:** Codex receives sender-controlled name inside `pulse bridge send "{name}" "<text>"`. A name containing quotes/newlines can inject instructions; shell substitutions such as `$(...)` remain active even inside double quotes if agent copies suggested command. Rust does not execute that name directly: exploit crosses model/tool boundary. Both harnesses receive arbitrary instructions as user content; Claude additionally asserts `from-mode="bypass"`. Escaping two XML strings prevents those delimiters, not prompt injection or authorization laundering. Skill contains no incoming-origin trust policy.

**Smallest fix:** Generate reply guidance from opaque validated route/message ID, pass bodies via stdin & keep metadata out of executable examples. Label provenance explicitly, distinguish human vs agent authority, remove fabricated permission-mode assertion & enforce action restrictions outside model prose.

### B03 — “Agent bridge Off” does not stop bridge delivery

**Severity: High.** Evidence: `hub/src-tauri/src/share.rs:473`, `hub/src-tauri/src/share.rs:497`; `core/src/bridge/mod.rs:330`, `core/src/bridge/mod.rs:377`, `core/src/bridge/mod.rs:426`; `hub/src/views/NearbySettings.tsx:98`.

**Failure:** Switch stops hub threads/reply listener; CLI never reads policy. Codex still accepts direct queue calls, Windows Claude still accepts direct pipe posts, trusted Unix Claude callers still deliver & outbound SSH sends still work. Off can silently turn Mac inbound messages into held inbox items instead of rejecting them. User-visible kill switch is false.

**Smallest fix:** Persist policy in shared core & check it on send, receive, control & reply ingress. Distinguish disabled from temporarily unavailable; report refused-disabled without queuing work for later execution.

### B04 — “Held” can mean no copy exists anywhere

**Severity: High.** Evidence: `core/src/bridge/control.rs:83`, `core/src/bridge/control.rs:108`; `core/src/bridge/mod.rs:340`, `core/src/bridge/mod.rs:359`, `core/src/bridge/mod.rs:368`.

**Failure:** Hub claims request by deleting file before processing. Crash after claim loses it. Unclaimed request is removed after ten seconds; `deliver_here` converts failure to Held without appending inbox. Stale heartbeat after hub death triggers same path. Hub-absent branch ignores append errors yet says “Kept.” Sender/skill stops retrying because it believes storage succeeded.

**Smallest fix:** Durable transactional claim with recovery; return Held only after verified inbox commit. Return outcome-unknown after ambiguous dispatch & explicit storage failure if no copy was saved. Never repair this with blind retry alone.

### B05 — No replay protection, deduplication or delivery-order contract

**Severity: High.** Evidence: `core/src/bridge/envelope.rs:104`; `core/src/bridge/mod.rs:443`; `core/src/bridge/links.rs:112`; `core/src/bridge/hub.rs:318`; `hub/src-tauri/src/share.rs:529`.

**Failure:** Reposting same envelope delivers again; `id`/`ts` are not checked against durable history or expiry. SSH can time out after remote side acts, leaving sender unable to know whether retry is safe. Native replies discard incoming message ID & allocate new envelope IDs. Concurrent control workers can reorder “do X” & “cancel X.” Telegram replay/reconnect would amplify these faults.

**Smallest fix:** Journal `(authenticated origin, id, payload hash)` & receipts, preserve IDs across hops, reject conflicting reuse/expired commands & serialize each conversation's dispatch. Expose outcome-unknown where native harness cannot deduplicate effects.

### B06 — Replies disappear on outage or hub restart

**Severity: High.** Evidence: `core/src/bridge/hub.rs:305`, `core/src/bridge/hub.rs:345`, `core/src/bridge/hub.rs:363`; `core/src/bridge/deliver_claude.rs:627`, `core/src/bridge/deliver_claude.rs:637`.

**Failure:** Reply is forwarded once; SSH failure records only `last_error`, losing reply body. Peer→listener table exists only in memory. Restart creates no previous per-peer listeners until another outbound delivery requests them, so outstanding Claude answers address dead sockets/pipes. Closed origin session is refused; no conversation mailbox retains result. CLI-origin `session="cli"` has no discoverable receiving chat (`core/src/bridge/mod.rs:307`, `core/src/bridge/mod.rs:445`).

**Smallest fix:** Persist reply routes & outbound reply messages before acknowledging local receipt; recreate listeners at startup; retain replies by conversation even when original chat/CLI is closed.

### B07 — Reverse routing confuses aliases with identity

**Severity: High.** Evidence: `core/src/bridge/mod.rs:155`, `core/src/bridge/mod.rs:399`; `core/src/bridge/roster.rs:297`; `core/src/bridge/hub.rs:296`, `core/src/bridge/hub.rs:307`; `core/src/bridge/deliver_codex.rs:314`.

**Failure:** Send uses receiver's locally chosen link alias; envelope origin uses sender's hostname/hub name. Reverse link need not have that name. Claude replies silently choose sole link when name lookup fails, potentially exporting answer to wrong machine; with multiple links reply fails. Codex instructed to reply by display name cannot benefit from that fallback & fails after alias/title change. Colons in self hostname/alias also break `split_once(':')`; only link names prohibit colons (`core/src/bridge/links.rs:228`).

**Smallest fix:** Stable device IDs plus stored reverse routes; remove sole-link fallback. Reply by message/conversation ID, never title or concatenated alias string. Store peer identity returned by link handshake.

### B08 — Native transport success is reported as chat delivery

**Severity: High.** Evidence: `core/src/bridge/deliver_claude.rs:518`, `core/src/bridge/deliver_claude.rs:523`; `core/src/bridge/deliver_codex.rs:318`; `plugins/pulse-bridge/skills/pulse-bridge/SKILL.md:19`.

**Failure:** Claude silence or Unix EOF without verdict becomes Delivered, including silent discard from wrong ancestry/protocol or chat closing before consumption. Codex exit zero becomes Delivered even without expected queue output; valid queue acceptance still does not prove open thread consumed it. Native Held is also copied to Pulse inbox (`core/src/bridge/mod.rs:211`): if harness later processes its held copy, inbox consumption can duplicate it.

**Smallest fix:** Separate sent-unconfirmed, queued-native, stored-Pulse & accepted states. Preserve native queue ID/ownership; require explicit receipt before “chat has it.” Do not create second actionable copy of native-held work without dedupe.

### B09 — PID revalidation can redirect delivery to a different session

**Severity: High.** Evidence: `core/src/bridge/roster.rs:107`; `core/src/bridge/deliver_claude.rs:186`, `core/src/bridge/deliver_claude.rs:224`, `core/src/bridge/deliver_claude.rs:408`; `core/src/bridge/hub.rs:386`.

**Failure:** Roster selects session A/PID P, then `open_session(P)` rereads current registry/key & validates current process start, without comparing `sessionId` to requested A. If P is reused or its chat changes between scan & send, B's fresh files pass process validation & A's message goes to B. First matching key file is chosen arbitrarily when stale keys coexist.

**Smallest fix:** Pass expected session ID, process start & domain into open; require coherent registry/key generation immediately before connection. Refuse mismatches; select matching key generation, not first filename.

### B10 — SSH invocation treats configured path as remote shell code

**Severity: Medium; High if proposed phone `link` accepts these inputs.** Evidence: `core/src/bridge/links.rs:65`, `core/src/bridge/links.rs:79`, `core/src/bridge/links.rs:213`, `core/src/bridge/links.rs:225`.

**Failure:** OpenSSH transmits remote command through remote shell; separate local argv entries do not quote `link.pulse` there. Spaces break paths, metacharacters execute shell syntax. Windows fallback assumes PowerShell without negotiation; cmd or another configured shell will not expand `$env:LOCALAPPDATA`. Host accepts leading option syntax without `--`. Base64 protects envelope argument only. Host-key policy is inherited from SSH config, so “SSH authenticates both ends” depends on external trust configuration (`core/src/bridge/envelope.rs:1`).

**Smallest fix:** Fixed receiver command over stdin, validated host syntax/option termination, explicit OS/shell negotiation when installation path is needed & device/host-key binding. Use restricted authorized key for bridge-only access; avoid exposing arbitrary `--pulse` through phone control.

### B11 — Inbox append & cursor are not one durable transaction

**Severity: Medium.** Evidence: `core/src/bridge/store.rs:186`, `core/src/bridge/store.rs:249`, `core/src/bridge/store.rs:285`, `core/src/bridge/store.rs:288`, `core/src/bridge/store.rs:304`.

**Failure:** Crash after message fsync but before cursor update reuses sequence number on next append. A concurrent reader may then mark both messages read or hide duplicate-sequence entry. Ten-second mtime-based lock stealing can remove live paused writer's lock; original guard later unlinks replacement lock, allowing overlapping writers. Link add/remove also use unlocked read-modify-write, losing concurrent edits (`core/src/bridge/links.rs:248`, `core/src/bridge/links.rs:260`).

**Smallest fix:** Transactional database for messages/cursors/links, or kernel locks with recoverable sequence derived from committed log. Never expire ownership solely by age.

### B12 — Store privacy is assumed; session filenames collide

**Severity: Medium.** Evidence: `core/src/bridge/store.rs:79`, `core/src/bridge/store.rs:99`, `core/src/bridge/store.rs:221`, `core/src/bridge/store.rs:281`; `core/src/bridge/control.rs:44`; `core/src/bridge/hub.rs:192`.

**Failure:** Inbox/link/session files use default permissions/inherited ACLs & pathname-following I/O. No owner/no-symlink validation establishes comment's “only that user” claim. With traversable parents/default Unix umask, message files can be readable to other local users; writable inherited directories permit forgery/symlink redirection. Control chmod covers leaf directories only, ignores failure, & has no Windows ACL check. Key is written before chmod. Session names do not directly traverse via `../`, but lossy sanitization/truncation merges `a/b` with `a_b` & long IDs sharing first 100 characters. Proposed `telegram:<topic>` also enters lossy namespace.

**Smallest fix:** Owner-only validated store root, no-follow/reparse-safe operations, restrictive permissions at creation & explicit Windows DACL. Use collision-resistant digest or reversible encoding of full typed session identity, never lossy sanitized filename.

### B13 — Held-message recovery is destructive, inaccessible or silently evicted

**Severity: Medium.** Evidence: `core/src/bridge/store.rs:261`, `core/src/bridge/store.rs:299`; `core/src/bridge_cmd.rs:233`; `core/src/bridge/mod.rs:260`, `core/src/bridge/mod.rs:453`; `plugins/pulse-bridge/skills/pulse-bridge/SKILL.md:20`.

**Failure:** Cap evicts oldest entries regardless of unread state without loss notice. Inbox marks read before CLI prints; broken pipe/crash consumes visibility. `inbox --from` resolves only currently discovered chats, so closed Claude or Codex thread outside roster limit cannot be selected normally. No replay worker drains inbox on hub recovery, despite CLI “kept until it is” wording (`core/src/bridge_cmd.rs:123`). Gone-session refusal stores nothing, contradicting docs saying all refused messages remain (`docs/bridge.md:41`).

**Smallest fix:** Non-destructive list/read by stable session/message ID, explicit acknowledgement, visible quota refusal/eviction & durable recovery actions. Distinguish refusal with no stored copy from stored pending work.

### B14 — Liveness labels are guesses presented as state

**Severity: Medium.** Evidence: `core/src/bridge/roster.rs:88`, `core/src/bridge/roster.rs:107`, `core/src/bridge/roster.rs:220`; `core/src/bridge/control.rs:58`; `core/src/bridge/hub.rs:239`; `hub/src/views/NearbySettings.tsx:159`.

**Failure:** Claude roster validates PID existence only, so stale registry with reused PID appears live/busy/idle. Codex updated within ten minutes appears active after closure; open waiting/long-running thread with old DB write appears idle. Future timestamp stays active. Hub liveness uses timestamp only & accepts future heartbeat; heartbeat thread blocks on linked-machine polling (`core/src/bridge/hub.rs:262`). None proves reachable chat or readiness for input.

**Smallest fix:** Separate process identity, observed harness status, last activity, transport reachability & unknown liveness. Show Codex “updated … ago; open state unknown.” Heartbeat runs independently of remote I/O & includes process-generation checks.

### B15 — Codex roster silently drops or resurrects threads

**Severity: Medium.** Evidence: `core/src/bridge/deliver_codex.rs:104`, `core/src/bridge/deliver_codex.rs:107`, `core/src/bridge/deliver_codex.rs:166`, `core/src/bridge/deliver_codex.rs:184`; `core/src/bridge/roster.rs:190`.

**Failure:** Highest-numbered state DB is assumed current with fixed columns. Schema mismatch, corruption or busy read becomes empty roster in fresh CLI; hub can retain unbounded-age last-good list. Archived threads correctly disappear on successful query, but stale cache & index fallback can advertise them again. Only newest 100 are addressable: even exact known old thread ID fails through `send_text` because resolution requires current roster.

**Smallest fix:** Versioned discovery adapter with explicit errors/cache age, archive-aware lookup by exact ID independent of list pagination & unknown-archive labels for fallback. Do not call failed discovery “no chats.”

### B16 — Every send depends on successful full-roster discovery

**Severity: Medium.** Evidence: `core/src/bridge/mod.rs:390`; `core/src/bridge/links.rs:285`; `core/src/bridge/roster.rs:291`; `core/src/bridge_cmd.rs:78`.

**Failure:** Even local or exact-ID sends poll all SSH links. Unrelated offline machine adds up to 25 seconds; failed link vanishes from peers & target becomes “no match,” not offline. Phone cannot leave a durable message for temporarily unavailable destination. `peers` suppresses link errors; only separate status reveals them.

**Smallest fix:** Resolve typed exact IDs without full discovery, preserve offline endpoints with freshness/errors & queue only explicitly eligible sends. Fetch chosen destination's capability/status separately.

### B17 — Codex executable discovery does not establish queue support

**Severity: Medium.** Evidence: `core/src/bridge/deliver_codex.rs:59`, `core/src/bridge/deliver_codex.rs:69`, `core/src/bridge/deliver_codex.rs:89`, `core/src/bridge/deliver_codex.rs:301`; `docs/phone-gateway-proposal.md:23`.

**Failure:** First existing executable wins, even if it lacks `queue`; no capability probe or fallback after unsupported command. Mac prioritizes one hard-coded ChatGPT bundle path; Windows searches guessed paths/PATH & omits packaged install discovery. Proposed version threshold does not resolve which binary is selected. Thread remains advertised as reachable while every send is held.

**Smallest fix:** Discover installation through supported locations/configuration, probe queue capability once per binary version & publish unavailable/unsupported status. Keep explicit configured binary override & actionable diagnostic; avoid promising version alone proves support.

### B18 — 64 KiB contract does not survive serialization or argv transport

**Severity: Medium.** Evidence: `core/src/bridge/envelope.rs:86`, `core/src/bridge/envelope.rs:106`; `core/src/bridge/links.rs:164`, `core/src/bridge/links.rs:194`, `core/src/bridge/links.rs:302`; `core/src/bridge/deliver_codex.rs:239`.

**Failure:** Valid 64 KiB body containing many JSON-escaped controls can exceed 128 KiB serialized limit & be rejected remotely. Base64 adds expansion; SSH & Codex place whole message in argv, exceeding Windows command-line capacity well before advertised worst case. Decode allocates before size check; metadata fields have no individual limits. Base64 parser accepts trailing material after first `=` & incomplete encodings rather than requiring canonical representation.

**Smallest fix:** Bound encoded bytes before allocation, validate field lengths/canonical IDs, transport payload through bounded stdin/framing & publish effective destination limit. If harness only supports argv, enforce its smaller limit before accepting message.

### B19 — Resource exhaustion is not bounded end to end

**Severity: Medium.** Evidence: `core/src/bridge/deliver_claude.rs:243`, `core/src/bridge/deliver_claude.rs:483`, `core/src/bridge/deliver_claude.rs:693`, `core/src/bridge/deliver_claude.rs:731`; `hub/src-tauri/src/share.rs:491`, `hub/src-tauri/src/share.rs:531`; `core/src/bridge/links.rs:97`; `core/src/bridge/deliver_codex.rs:252`.

**Failure:** Unix ACK readers have no read timeout/byte cap; each silent socket can retain thread/FD indefinitely after Delivered. Reply listeners/connections & workers grow per peer/request without global quota; per-read five-second timeout permits slow clients to linger. SSH reads stdout fully before stderr, so full stderr pipe can stall child; output is unbounded. Codex waits for exit before draining pipes, causing timeout if child fills them. These are reachable from authorized senders or malfunctioning harnesses, not just malicious networks.

**Smallest fix:** Bounded worker queue, per-principal rates, peer/listener quota, absolute connection deadlines, bounded simultaneous stdout/stderr draining & shutdown/join of ACK readers. Surface overload without pretending storage/delivery succeeded.

### B20 — Listener stop/start races can strand new reply routes

**Severity: Medium.** Evidence: `core/src/bridge/deliver_claude.rs:571`, `core/src/bridge/deliver_claude.rs:631`, `core/src/bridge/deliver_claude.rs:719`, `core/src/bridge/deliver_claude.rs:739`, `core/src/bridge/deliver_claude.rs:674`.

**Failure:** Stop sets flag without joining listeners. Replacement Unix hub can unlink/rebind same pathname before old thread exits; old cleanup then unlinks new socket. Windows first-instance creation can collide with old still-live server. Bind failure silently falls back to nonexistent no-reply address (`core/src/bridge/deliver_claude.rs:415`, `core/src/bridge/deliver_claude.rs:429`).

**Smallest fix:** Join listener shutdown, use generation ownership for cleanup & return explicit reply-unavailable state. Recreate persisted listeners only after exclusive ownership is established.

### B21 — Windows hub ownership can be blocked or fail open

**Severity: Medium.** Evidence: `hub/src-tauri/src/win_bridge.rs:76`, `hub/src-tauri/src/win_bridge.rs:82`, `hub/src-tauri/src/win_bridge.rs:125`; `core/src/bridge/control.rs:108`.

**Failure:** Predictable singleton mutex uses default security & mere existence, not authenticated hub ownership. Same-session process can squat name & suppress hub; creation failure permits second hub. Duplicate hubs can both read same control request before either deletes it, double-delivering. Show/select events are UI wakeups, not chat authentication; no remote-chat injection is demonstrated through those events alone.

**Smallest fix:** User-scoped secured singleton with verified ownership, fail-closed handling & durable atomic control claim. UI events must remain payload-free hints.

### B22 — Send cards do not provide agent messaging or reliable failure visibility

**Severity: Medium.** Evidence: `mac/Notch/Sources/Sharing/NearbySharing.swift:70`, `mac/Notch/Sources/Sharing/NearbySharing.swift:230`, `mac/Notch/Sources/Sharing/NearbySharing.swift:281`, `mac/Notch/Sources/Sharing/NearbySharing.swift:473`; `windows/src/send.rs:109`, `windows/src/send.rs:193`, `windows/src/send.rs:1145`, `windows/src/send.rs:1535`.

**Failure:** Both cards send LocalSend payloads to nearby device fingerprints, not agent chats. Mac only shows six-second bridge activity, marked success even for held events because core increments activity for anything except Refused (`core/src/bridge/mod.rs:416`, `core/src/bridge/mod.rs:448`). Windows ignores bridge object entirely. Neither offers agent target selection, receipt detail, unread held items, replies or recovery. “No devices nearby” can coexist with working SSH bridge, or bridge failure with otherwise healthy Send card.

**Smallest fix:** Expose separate agent subsection with machine/chat identity, actual receipt states & retained failures/inbox access. Parse same bridge model on Windows. Keep clipboard/file transfer target distinct from agent-instruction target.

### B23 — “Always-on hub” depends on unrelated sharing preference

**Severity: Medium.** Evidence: `mac/Notch/Sources/Sharing/NearbySharing.swift:189`; `windows/src/send.rs:919`; `hub/src-tauri/src/share.rs:484`; `docs/phone-gateway-proposal.md:16`.

**Failure:** Agent bridge runs independently once hub exists, but notch watchdog starts hub only when Nearby sharing is enabled. User disables LAN sharing & later hub exits: agent bridge/phone gateway does not recover through these watchdogs. Existing-but-unresponsive hub also blocks restart checks.

**Smallest fix:** Dedicated supervised agent service with independent enablement/health, or make lifecycle depend on any enabled hub service rather than LocalSend alone.

### B24 — CLI consumes literal message arguments as options

**Severity: Medium.** Evidence: `core/src/main.rs:120`, `core/src/main.rs:708`; `core/src/bridge_cmd.rs:33`, `core/src/bridge_cmd.rs:128`, `core/src/bridge_cmd.rs:144`.

**Failure:** Sending literal message `"--json"` causes global parser to remove it; literal `"--from"` is consumed as option. Multi-argument text is joined with spaces, losing argument boundaries. Send exits successfully even when receipt says Refused, making exit-code automation misleading. There is no stdin/message-file interface or documented `--` boundary for arbitrary text.

**Smallest fix:** Structured subcommand parser with `--` & stdin payload, explicit receipt-dependent exit codes & machine-readable outcome including original stable message ID.

### B25 — Skill & docs prescribe unsupported or obsolete recovery paths

**Severity: Medium.** Evidence: `plugins/pulse-bridge/skills/pulse-bridge/SKILL.md:8`, `plugins/pulse-bridge/skills/pulse-bridge/SKILL.md:12`, `plugins/pulse-bridge/skills/pulse-bridge/SKILL.md:23`, `plugins/pulse-bridge/skills/pulse-bridge/SKILL.md:33`; `docs/bridge.md:25`, `docs/bridge.md:29`, `AGENTS.md:17`.

**Failure:** Skill forbids same-machine Pulse even though core routes locally; it assumes caller has Claude `SendMessage`, which is not a universal Codex tool. “Replies arrive … on their own” fails on missing reverse link, stopped hub & lost route. Docs say Windows reply listener missing despite implemented pipe server; Codex “always idle” contradicts ten-minute active heuristic. AGENTS says “link once” without explaining reverse link requirement. Skill's “do not poll or resend” provides no outcome-unknown recovery.

**Smallest fix:** Align guidance to capability/status model, document reciprocal links & explicit reply operation, allow core local routing when native tool is unavailable & publish verified installation/harness compatibility matrix. Replace absolute delivery/reply promises.

### B26 — Current tests cannot qualify cross-machine agent messaging

**Severity: Medium.** Evidence: `core/tests/bridge.rs:1`, `core/tests/bridge.rs:179`, `core/tests/bridge.rs:238`, `core/tests/bridge.rs:315`, `core/tests/bridge.rs:457`, `core/tests/bridge.rs:516`; `AGENTS.md:9`.

**Failure:** Named “two_computers_message_each_other_over_ssh,” test uses fake SSH & fake Claude endpoint on one OS. Unix shim injects `CLAUDE_CODE_MESSAGING_SOCKET`, bypassing real SSH→hub trust path. Windows callback captures reply without routing it back over SSH; deliberately accepts token `ignored`. No real Codex queue/DB, installed notches, cross-OS shells, hub restart, duplicate suppression or lost-reply recovery is exercised. Component success cannot establish promised journey.

**Smallest fix:** Extend complete installed journeys covering both machines/harnesses & retained state through interruption; keep existing tests as component/integration evidence. Do not rename helper checks as E2E.

**Scoped inventory, unchanged:** 3 registered bridge tests: `round_trips_and_rejects_bad_input` (`core/src/bridge/envelope.rs:129`), `base64_round_trips` (`core/src/bridge/links.rs:316`) & `two_computers_message_each_other_over_ssh` (`core/tests/bridge.rs:517`). That is 2 unit/component tests + 1 simulated integration journey, 0 installed native cross-machine E2E journeys in reviewed bridge suite. Added 0; deleted 0; executed 0.

### B27 — Skill install/uninstall cannot preserve later user edits reliably

**Severity: Low.** Evidence: `core/src/bridge/install.rs:89`, `core/src/bridge/install.rs:112`, `core/src/bridge/install.rs:123`.

**Failure:** Once backup exists, later differing skill content is overwritten without another backup. Uninstall removes only byte-for-byte current embedded version, so upgraded CLI may leave old installed version reported absent. Concurrent installs share fixed temp filename; interrupted multi-target operation can change one harness & return only error.

**Smallest fix:** Record installed version/hash & per-target ownership, preserve changed content with versioned backup, use unique temp/lock & report partial changes. Remove only manifest-owned unchanged content.

## 3 Cross-harness/cross-machine matrix

Prerequisites: compatible installed harness, reachable same-user SSH link, correct binary path/config home & active receiver hub for Mac Claude. Replies need reverse route; Claude native replies need receiver's ReplyHub. These prerequisites are not established by current test (`core/tests/bridge.rs:1`).

### Endpoint capabilities

| Endpoint | Send through CLI | Receive into chat | Reply |
|---|---|---|---|
| Claude / Mac | **Works:** caller socket/session inference & common SSH path (`core/src/bridge/mod.rs:277`, `core/src/bridge/mod.rs:426`) | **Partial:** hub trust path exists; silence is unconfirmed & control can lose message (`core/src/bridge/mod.rs:330`, `core/src/bridge/deliver_claude.rs:518`) | **Partial:** Unix ReplyHub forwards, but no durable route/outbox or verified chat origin (`core/src/bridge/deliver_claude.rs:712`, `core/src/bridge/hub.rs:305`) |
| Claude / Windows | **Works:** common caller/SSH path (`core/src/bridge/mod.rs:277`, `core/src/bridge/links.rs:85`) | **Partial:** authenticated pipe client implemented; no installed evidence, fallback loses native reply address (`core/src/bridge/deliver_claude.rs:451`, `core/src/bridge/deliver_claude.rs:1071`, `core/src/bridge/deliver_claude.rs:431`) | **Partial, not missing:** named-pipe ReplyHub implemented; source identity/route durability defects remain (`core/src/bridge/deliver_claude.rs:671`, `core/src/bridge/deliver_claude.rs:759`) |
| Codex / Mac | **Works:** `CODEX_THREAD_ID` identifies origin & common SSH path sends (`core/src/bridge/mod.rs:292`, `core/src/bridge/mod.rs:426`) | **Partial:** discovery & `queue`; queue support/consumption not verified (`core/src/bridge/deliver_codex.rs:61`, `core/src/bridge/deliver_codex.rs:301`) | **Partial:** agent must run suggested send; no automatic assistant-output capture (`core/src/bridge/deliver_codex.rs:313`) |
| Codex / Windows | **Works:** same caller/SSH implementation (`core/src/bridge/mod.rs:292`, `core/src/bridge/links.rs:85`) | **Partial:** guessed executable locations, capability not probed, queued ≠ consumed (`core/src/bridge/deliver_codex.rs:69`, `core/src/bridge/deliver_codex.rs:318`) | **Partial:** explicit agent send, vulnerable display-name route (`core/src/bridge/deliver_codex.rs:313`) |
| Phone / Telegram | **Missing** adapter (`docs/phone-gateway-proposal.md:16`; `core/src/bridge/mod.rs:426`) | **Missing** endpoint transport (`docs/phone-gateway-proposal.md:18`; `core/src/bridge/mod.rs:443`) | **Missing** return routing/output adapter (`core/src/bridge/hub.rs:345`) |

### Both cross-machine directions

“Send works” identifies implemented outbound path only; complete conversation remains partial. Endpoint evidence above applies to each row; additional route evidence is explicit below.

| Origin → destination | Send | Receive | Reply back | Controlling evidence |
|---|---|---|---|---|
| Mac Claude → Windows Claude | Works | Partial | Partial, native pipe → hub → SSH | `core/src/bridge/mod.rs:426`; `core/src/bridge/deliver_claude.rs:451`, `core/src/bridge/deliver_claude.rs:671`; `core/src/bridge/hub.rs:348` |
| Mac Claude → Windows Codex | Works | Partial | Partial, explicit Codex CLI send | `core/src/bridge/mod.rs:426`; `core/src/bridge/deliver_codex.rs:69`, `core/src/bridge/deliver_codex.rs:313` |
| Mac Codex → Windows Claude | Works | Partial | Partial, native pipe → Mac Codex queue | `core/src/bridge/mod.rs:292`; `core/src/bridge/deliver_claude.rs:671`; `core/src/bridge/hub.rs:348`; `core/src/bridge/mod.rs:198` |
| Mac Codex → Windows Codex | Works | Partial | Partial, explicit send → queue | `core/src/bridge/mod.rs:292`; `core/src/bridge/deliver_codex.rs:69`, `core/src/bridge/deliver_codex.rs:313` |
| Windows Claude → Mac Claude | Works | Partial | Partial, Unix socket → hub → SSH | `core/src/bridge/links.rs:85`; `core/src/bridge/mod.rs:330`; `core/src/bridge/deliver_claude.rs:712`; `core/src/bridge/hub.rs:348` |
| Windows Claude → Mac Codex | Works | Partial | Partial, explicit Codex send → Windows pipe | `core/src/bridge/deliver_codex.rs:61`, `core/src/bridge/deliver_codex.rs:313`; `core/src/bridge/deliver_claude.rs:451` |
| Windows Codex → Mac Claude | Works | Partial | Partial, Unix reply → Windows Codex queue | `core/src/bridge/mod.rs:292`, `core/src/bridge/mod.rs:330`; `core/src/bridge/hub.rs:348`; `core/src/bridge/deliver_codex.rs:69` |
| Windows Codex → Mac Codex | Works | Partial | Partial, explicit send → queue | `core/src/bridge/mod.rs:292`; `core/src/bridge/deliver_codex.rs:61`, `core/src/bridge/deliver_codex.rs:313` |

### Same-machine gaps & surfaces

| Path/surface | Status | Evidence |
|---|---|---|
| Claude → Claude, either OS | **Partial:** core supports direct reply socket; skill instead mandates native tool, whose availability is outside this source review | `core/src/bridge/mod.rs:415`; `plugins/pulse-bridge/skills/pulse-bridge/SKILL.md:12` |
| Claude → Codex, either OS | **Partial:** queue input works conditionally; reply guidance asks for local Pulse send while skill forbids it | `core/src/bridge/mod.rs:198`; `core/src/bridge/deliver_codex.rs:313`; `plugins/pulse-bridge/skills/pulse-bridge/SKILL.md:8` |
| Codex → Claude, either OS | **Partial:** core supports delivery & hub-mediated return to queue; skill assumes caller has `SendMessage`, no capability fallback | `core/src/bridge/mod.rs:330`; `core/src/bridge/hub.rs:333`; `plugins/pulse-bridge/skills/pulse-bridge/SKILL.md:12` |
| Codex → Codex, either OS | **Partial:** queue is explicit; no automatic reply routing or proof of consumption | `core/src/bridge/deliver_codex.rs:239`, `core/src/bridge/deliver_codex.rs:313` |
| Mac Send card: agent send/receive/reply | **Missing / partial indicator / missing:** LocalSend composer & transient bridge pulse only | `mac/Notch/Sources/Sharing/NearbySharing.swift:281`, `mac/Notch/Sources/Sharing/NearbySharing.swift:473` |
| Windows Send card: agent send/receive/reply | **Missing / missing / missing:** bridge state is not parsed | `windows/src/send.rs:109`, `windows/src/send.rs:193`, `windows/src/send.rs:1145` |
| Proactive waiting-question push, either harness/OS | **Missing in bridge:** roster state has no question/event/answer channel | `core/src/bridge/roster.rs:33`; `core/src/bridge/hub.rs:414`; `docs/phone-gateway-proposal.md:19` |

## 4 Top 10 actions

1. **Replace proposal's implicit trust model** with enrolled principals/endpoints, strict Telegram update authorization & local-only topology administration. Keep phone ingress disabled until implemented. Evidence: `docs/phone-gateway-proposal.md:16`–`docs/phone-gateway-proposal.md:18`; B01–B03.
2. **Make receipt states truthful.** Separate durable storage, native queue acceptance, confirmed consumption & unknown outcomes; never report Held without saved copy. Evidence: B04, B08.
3. **Add transactional ingress/outbox, dedupe & recovery.** Preserve IDs through SSH/native replies, serialize conversation dispatch & survive crashes before/after claims. Evidence: B04–B06, B11.
4. **Replace device/title routing with stable IDs & explicit replies.** Persist topic/session mapping, reverse routes & tombstones; delete sole-link fallback. Evidence: B06, B07, B15.
5. **Enforce shared authorization everywhere.** Bridge Off must stop all adapters; native reply identity must be authenticated; permission mode must remain harness-owned. Evidence: B01–B03.
6. **Harden transport & storage boundaries.** Fixed SSH receiver/stdin, host-key/device binding, owner-only store, bounded decoding/resources & collision-free IDs. Evidence: B10, B12, B18–B21.
7. **Publish capability & freshness instead of guessed liveness.** Probe Codex queue, expose DB errors/archive uncertainty, validate Claude session generation & allow exact-ID lookup beyond roster cap. Evidence: B09, B14–B17.
8. **Separate service lifecycle from notch/LocalSend.** Supervise gateway, restore reply listeners, journal outbound Telegram messages & implement waiting events before promising proactive questions. Evidence: `docs/phone-gateway-proposal.md:19`; B06, B20, B23.
9. **Repair user/agent interfaces together.** Add explicit reply/stdin CLI, retained failures & held inbox in both Send cards; update skill/docs, reciprocal-link instructions & installer ownership. Evidence: B13, B22, B24, B25, B27.
10. **Qualify complete installed journeys before rollout.** Exercise all eight cross-machine routes, real reverse replies, stale/renamed/resumed/archived sessions, interrupted gateways, duplicates, permission refusal & retained inbox state; retain existing three tests as component/integration evidence. Evidence: B26; `AGENTS.md:9`–`AGENTS.md:13`.
