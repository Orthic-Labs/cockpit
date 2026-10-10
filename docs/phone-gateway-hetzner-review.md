# Hetzner phone gateway: adversarial source review

Reviewed 2026-10-10. Source only: Proposal B (`docs/phone-gateway-hetzner.md`, 52 lines), Telegram proposal (`docs/phone-gateway-proposal.md`, 25 lines), B01–B27 & “Concrete replacement” in `docs/bridge-astra-review.md`, plus current working-tree bridge/share/notch code. No builds, tests, network, app launches or Git writes. Citations below refer to working-tree source read during review; original B findings describe an older snapshot. Concurrent bridge edits were observed during review; final source checks were incorporated without modifying those files. Source changes are credited as source changes, not installed proof.

## 1 Verdict

**CHANGE. Keep owner-operated E2E store-and-forward as preferred direction; reject implementation as written.** Removing SSH from message transport genuinely removes B10 from that path. Direct laptop→relay connections also remove Mac-as-Dell-gateway dependency. Device identity, stable endpoints, durable mailboxes & explicit replies fit prior replacement design better than Telegram's implicit topic routing. They remain proposed contracts, not demonstrated closure.

Minimal changes for **KEEP**:

1. Specify authenticated encryption, owner-authorized enrollment, device capabilities, key epochs, revocation & recovery before exposing ingress. Recipients verify sender signatures & trusted key bindings themselves; relay stamps are insufficient.
2. Replace loose dedupe/cursor language with transactional message journals on **every node**, including iPhone. Define uncertain native dispatch, signed receipts, expiry, cancellation & restored-state reconciliation.
3. Adapt local harness boundaries for typed human/agent messages, exact session generations & durable replies. Drop “adapters unchanged,” “no listener state,” & “no remote execution” assurances.
4. Ship bounded text messages first. Defer files, clipboard, content pushes & LAN transport. Retain LocalSend until replacement journeys pass; installation on three devices is not migration acceptance.
5. Specify one relay's deploy/upgrade/restore runbook, independent hub supervision, generic APNs hints & foreground iOS catch-up. Remove “second relay is a config list” & “files … for free.”

### What B01–B27 actually gain

“Direction” means Proposal B chooses a useful mechanism but omits its safety contract. None of these proposal-only changes establishes executed closure.

| Prior finding | Proposal B disposition against current source |
|---|---|
| B01 forged identity | **Partial direction.** Device authentication can bind remote principal; does not authenticate claimed chat or local Claude reply. Relay can forge its own stamps (H01, H04). |
| B02 authority laundering | **Unresolved.** Current Claude wrapper already says `from-mode="bridge"`, not old `bypass`; Codex already uses opaque reply guidance. Typed human authority & authorization remain missing (H04). |
| B03 false Off | **Not addressed by proposal.** Current core contains policy checks on send/receive; relay ingress, queued work & blob grants must honor same policy (H04). |
| B04 false storage | **Direction, not closure.** Proposed outbox helps; current claims are retained by rename, but transactional ownership through native effects is still required (H06). |
| B05 replay/order | **Direction with incorrect dedupe wording.** Request nonces, message dedupe & harness effect dedupe are separate mechanisms (H05–H07). |
| B06 lost replies | **Direction.** Current source already persists routes/replies, but drains outbox destructively & relies on Claude listeners. Relay does not repair that (H06, H09). |
| B07 alias routing | **Genuine design improvement.** Key-bound endpoints & `in_reply_to` can remove alias routing; conversation definition still omits harness & generations (H08). |
| B08 false delivered | **Direction.** Current adapters already distinguish queue/sent/unknown. Proposed list drops `sent` & lacks evidence/issuer rules (H07). |
| B09 PID/session race | **Not solved by device ID.** Current Claude code now compares expected session/process identity. Keep & qualify that boundary; wire generation still needs binding (H08). |
| B10 SSH shell surface | **Removed from proposed relay data path** if SSH is truly bypassed, including retries/replies. Admin SSH & retained legacy routes remain separate surfaces. |
| B11 cursor/storage race | **Partial direction.** Relay SQLite does not make local outbox, dispatch & cursor atomic. Existing store derives next sequence from log, but is still separate files (H06, H18). |
| B12 store privacy/collisions | **Not solved by E2E.** Decrypted local state, keys, temp files & restored stores still need access controls. Current store adds restrictive Unix opens & hashed stems, not device identity (H02, H19). |
| B13 held recovery | **Partial direction.** Current CLI adds non-destructive inbox/explicit ack. New design needs visible expiry, quota refusal & retained uncertain outcomes (H06, H07). |
| B14 guessed liveness | **Useful age display.** Current Codex roster already says open state unknown; publishing it does not produce waiting events (H10). |
| B15 dropped/archive threads | **Not addressed.** Current Codex adds exact-ID lookup/discovery errors; relay still needs paginated snapshots, tombstones & stale-cache rules (H08). |
| B16 full-roster dependency | **Genuine architectural opportunity.** Durable exact endpoint addressing can avoid polling unrelated laptops; do not dispatch through legacy full-roster resolution (H08). |
| B17 Codex support | **Not solved by naming `codex_queue`.** Current source probes binary capability; export its evidence/age/effective limit, not optimistic capability labels (H10, H12). |
| B18 size/framing | **SSH argv removed; harness limit remains.** Current envelope bounds & Codex size handling improve old code. 64 KiB still cannot be universal (H12). |
| B19 resource exhaustion | **Incomplete.** Blob quota omits messages, nonce rows, receipts, downloads & key-authorized abuse (H12). |
| B20 listener lifecycle | **False closure claim.** Current Claude delivery still needs reply listener addresses; current restoration code demonstrates that obligation (H09). |
| B21 Windows ownership | **Not addressed by relay/systemd.** Keep secured local singleton/claim ownership; remote SQLite cannot fence local workers (H06, H09). |
| B22 Send UI | **Files UI reuse is real; agent UI is work.** Mac has new agent status rows; Windows inspected model still lacks bridge state. Neither is proposed phone conversation/receipt UI (H20). |
| B23 sharing-dependent supervision | **Still present.** Both inspected watchdogs depend on Nearby enablement (H09). |
| B24 CLI parsing | **Current source improvement, not Proposal B closure.** `reply`, stdin, `--` & receipt exit codes exist. Reuse interface; preserve IDs on retry (H06). |
| B25 guidance | **Partly repaired in current skill.** New phone endpoints, typed authority, expiry & relay diagnostics need another update; “resend once” currently creates a fresh ID (H06). |
| B26 missing E2E evidence | **Unaddressed.** Proposal adds phone/server/blob failure boundaries; qualification must grow by complete journeys, not helper counts (section 4). |
| B27 installer ownership | **Independent.** Current installer adds manifest/hash & versioned backups; relay provides no installer fix. Retain ownership behavior when updating skill (section 4). |

Current-source anchors for credits above: `core/src/bridge/deliver_claude.rs:243`, `:463`; `core/src/bridge/deliver_codex.rs:239`, `:484`, `:561`, `:647`; `core/src/bridge/mod.rs:231`, `:573`, `:659`; `core/src/bridge/control.rs:178`; `core/src/bridge/store.rs:245`, `:591`, `:650`; `core/src/bridge_cmd.rs:33`, `:330`; `core/src/bridge/install.rs:158`; `plugins/pulse-bridge/skills/pulse-bridge/SKILL.md:24`. These credits do not certify complete remediation of earlier findings.

## 2 Findings ranked by severity

High = authorization/confidentiality failure, irreversible loss/duplicate effects, or failure of promised primary journey. Medium = bounded availability, operations or migration defects. Each finding distinguishes missing specification from observed implementation behavior.

### H01 — Sealed boxes & relay stamps do not authenticate end-to-end authorship

**Severity: High. Evidence:** Proposal B lines 14–17, 20; B01.

**Failure:** A sealed box to recipient public key can be created by anyone with that key; it does not establish sender identity. Request signatures checked only by relay are not recipient-verifiable authorship. Compromised relay can substitute enrollment keys, forge `from_device`, alter destination/kind/expiry, or manufacture replies if clients trust its stamps. “A claimed label never travels unverified” overstates protection; device signature also cannot prove which local chat wrote text.

**Smallest fix:** Specify a maintained, versioned cryptographic suite/library, not primitive names alone. Authenticate complete canonical envelope: protocol/domain, sender identity & key epoch, recipient, endpoint/generation, conversation, message/parent IDs, kind, expiry & ciphertext. Recipient verifies sender signature against owner-approved key binding; bind header to ciphertext using authenticated data or signed composition. Trust relay for availability/routing only. Clear hashes must cover ciphertext, not predictable plaintext commands: public plaintext hashes enable guessing content. Keep retry ciphertext immutable. State explicitly that static recipient-key sealed boxes do not provide forward secrecy against later recipient-key compromise; if that protection is required, select an established asynchronous ratcheting protocol rather than inventing one.

### H02 — Key storage claim is inaccurate; key lifecycle changes identity

**Severity: High. Evidence:** Proposal B lines 14, 17, 36, 48; B12.

**Failure:** “Secure Enclave-backed keychain” conflates protected storage with non-exportable cryptographic keys. Proposed Ed25519/X25519 keys cannot simply be treated as Secure Enclave P-256 keys. Key derived `device_id` changes when identity key rotates, breaking routes/history unless modeled. Lost decryption key makes queued ciphertext unreadable; re-enrollment alone cannot repair it. Recipient-only encryption also gives newly installed sender no way to recover sent history.

**Smallest fix:** Choose explicitly between software Ed25519/X25519 secrets protected by OS credential storage, or supported hardware keys with a compatible reviewed suite. Separate stable device record, identity authorization key, encryption key IDs & epochs; owner signs rotation/replacement bindings. Specify Mac/Windows/iOS storage, access groups, non-sync/backup policy, locked-device availability & which operations require user presence. Document whether history is intentionally unrecoverable, locally retained, or encrypted to separately managed recovery/sender keys. Preserve old decrypt keys only for an explicit retention interval; never silently retarget old ciphertext to replacement identity.

### H03 — One-time enrollment code is not an enrollment protocol or revocation system

**Severity: High. Evidence:** Proposal B lines 14–15, 36, 43–44; B01, B03.

**Failure:** Relay can replace candidate public key while forwarding a valid code. Guessable or leaked code admits attacker; non-atomic redemption admits two devices. First device has no already-enrolled sponsor. Any enrolled stolen device may mint more devices unless admission authority differs from messaging authority. No revocation distribution, recovery authority or stolen-phone removal is defined. “Phone cannot change policies” conflicts with unrestricted enrollment sponsorship.

**Smallest fix:** Bootstrap owner trust locally; pin owner/root fingerprint through QR or independently compared code. Bind short-lived single-use invitation to relay identity, enrolling key bundle & narrowly scoped capabilities; require explicit sponsor confirmation of that bundle. High-entropy QR secret, or established password-authenticated pairing for short typed codes, plus rate limits & atomic consume. Give ordinary devices no enrollment/administration authority by default. Owner-signed membership epochs & revocation records must reach all receivers; reject revoked senders at dispatch as well as relay ingress, terminate their sockets/grants, purge pending authorized work by stated policy. Define offline revocation staleness budget & all-devices-lost recovery before launch. Revocation cannot retract plaintext already read.

### H04 — Authenticated phone text still exercises agent authority

**Severity: High. Evidence:** Proposal B lines 20, 25, 43; `core/src/bridge/deliver_codex.rs:575`; `core/src/bridge/deliver_claude.rs:463`; `core/src/bridge/deliver_claude.rs:1070`; B01–B03.

**Failure:** Relay may execute nothing, but authenticated message can cause harness to execute tools under existing permissions. A stolen device key becomes an agent-command credential. Reusing adapters unchanged labels genuine phone instructions as unverified agent text; promoting all relay text to human authority instead launders forwarded content & agent replies. Claude reply listener still ignores auth frame & trusts known session label; signing that reply at hub would merely authenticate hub's acceptance of an unverified label.

**Smallest fix:** Typed `human_instruction`, `agent_message`, `quoted_content`, `waiting_event` & separate permission-response capability. Enforce enrolled device→workspace/session/action scope before native dispatch; normal phone messaging must not approve native permission prompts. Preserve harness mode. Bind local reply provenance where supported; otherwise display “device-authenticated, chat attribution unverified.” Check shared Off/revocation at every ingress & immediately before dispatch; disabled work must not quietly execute after re-enable. Separate file/clipboard capabilities from agent-send authority.

### H05 — Request nonce journal is underspecified & dedupe key is wrong

**Severity: High. Evidence:** Proposal B lines 15, 22; `core/src/bridge/store.rs:795`; `core/src/bridge/mod.rs:680`; B05.

**Failure:** Signing only timestamp+nonce permits replaying signature against another path/body. Separate nonce check/insert races under concurrent requests. `(from_device, message_id, hash)` as a unique key accepts changed content under same ID because changed hash creates another tuple. Five-minute request window cannot dedupe month-old mailbox replay. Current journal returns `New` on locking/storage failure & forgets after 10,000 entries. Final source check shows receive now supplies `payload_hash(env)`; credit that improvement, but it supplies neither transactional receipt replay nor proposed authenticated identity/expiry binding.

**Smallest fix:** Sign canonical method, authority, normalized path/query, bounded body/ciphertext digest, protocol domain, principal/key epoch, timestamp & cryptographic nonce. Atomically reserve unique `(principal, key_epoch, nonce)` with mutation result; fail closed on journal failure. Retain nonce until signed timestamp's entire acceptance window ends, including accepted future skew; define clock rollback behavior. Retry HTTP request with fresh nonce but same application ID/ciphertext. WebSocket handshake authentication does not replace per-operation authorization, frame bounds or revocation checks. Uniqueness is `(authenticated_origin, message_id)`; stored digest must match or reject conflict. Receiver persists dedupe tombstones through maximum accepted message/retry horizon, independent of body retention. Repeated identical messages return recorded status, not a new refusal suggesting original failed.

### H06 — Cursor/outbox language leaves crash gaps & duplicate effects

**Severity: High. Evidence:** Proposal B lines 21–22, 40; `core/src/bridge/store.rs:904`; `core/src/bridge/hub.rs:379`, `:414`; `core/src/bridge/control.rs:263`; `core/src/bridge/mod.rs:680`; `core/src/bridge_cmd.rs:345`; B04–B06, B11, B13, B24–B25.

**Failure:** Crash after dedupe reservation but before saving work loses command; crash after native acceptance but before receipt can duplicate effects on replay. Current `take_outbound` deletes every queued file before network work; current journal marks seen before dispatch without saving resulting receipt. Age-based claim recovery may reissue live/hung work. Cursor advancement over a failed earlier item loses it; reconnecting WebSocket cannot recover data that was never committed. Proposal specifies local outbox only for hubs, leaving phone/share extension exposed.

**Smallest fix:** One transactional journal per node contains immutable outbound envelope, incoming work, dedupe result, per-mailbox contiguous cursor, dispatch attempt & receipt outbox. Commit accepted mailbox batch plus cursor together; durably quarantine malformed entries with visible errors rather than silently skipping or blocking forever. Relay ack/GC cursor is distinct from local fetch cursor, UI read cursor & native consumption. Outbox rows remain until durable next-hop acceptance; recover by querying same ID. Persist attempt ownership with process/lease fencing. After ambiguous native call, retain `outcome_unknown` & reconcile; never auto-redeliver merely because lease expired. A new CLI `send` makes a new ID: provide retry/status by original ID, not “same text.” Define cursor gaps/retention loss & server-epoch reset on restore. Transport can be at-least-once; native execution cannot honestly be exactly-once without harness support.

### H07 — Receipt list is neither a receipt chain nor an effect acknowledgement

**Severity: High. Evidence:** Proposal B lines 20–23; `core/src/bridge/mod.rs:84`; `core/src/bridge/deliver_claude.rs:419`, `:662`; `core/src/bridge/deliver_codex.rs:647`; B08, B13.

**Failure:** Relay can claim `delivered`; another device can post receipt for somebody else's message unless authorization is specified. “Hub pulled it” does not prove hub committed it. `delivered` cannot mean agent acted; Codex only acknowledges queue acceptance, Claude may yield `sent` without verdict. Proposed list omits this existing state, expiry & cancellation; late `queued` may overwrite `replied`, or false success may hide retained uncertain work.

**Smallest fix:** Append immutable receipt events bound to origin/message/ciphertext digest, recipient endpoint/generation, issuer, attempt ID & monotonic issuer sequence. Relay may attest `stored`; recipient signs `stored_local`, dispatch/native queue evidence & reply linkage; sender verifies authorized issuer. Receipt detail containing native errors stays encrypted. Define legal transitions as branches, not numeric progress: local-held, native-queued, sent-unconfirmed, refused, unsupported, expired, cancelled & outcome-unknown. `replied` references separate authenticated reply message; it does not retroactively prove every requested action occurred. Show stage/time/evidence & allow late authenticated reconciliation without erasing history. Cancellation prevents only undispatched work unless harness supports acknowledged cancellation.

### H08 — Conversation identity contradicts endpoint identity

**Severity: High. Evidence:** Proposal B lines 16, 20, 23; `core/src/bridge/envelope.rs:40`; `core/src/bridge_cmd.rs:317`; `core/src/bridge/roster.rs:18`; B07, B09, B15–B16.

**Failure:** Chat key includes harness+generation, conversation key drops both. Concurrent resumed generations, cross-harness ID collisions or reinstall can redirect pending command. A tuple naming one session does not define phone return mailbox or multi-device conversation participants. Current `reply` resolves stored alias/session & calls `send_text`, which allocates fresh envelope with no `in_reply_to`; adding field in proposal is real code work. Current `deliver_local_via` saves reply route only for Claude with no direct reply socket (`core/src/bridge/mod.rs:269`); Codex gets reply guidance without this route being created. Command existence is not working Codex return routing.

**Smallest fix:** Stable random conversation ID plus explicit authorized participants/endpoints. Separate logical session identity from execution generation: rename preserves identity; resume maps only under defined continuity rules; stale-generation commands require refusal or deliberate retargeting, never fuzzy resolution. Persist message→return-mailbox route even after origin chat closes. Export signed, versioned roster snapshots/deltas with pagination, tombstones, observation time & expiry; archive/discovery failure must remain visible. Exact endpoint send must bypass unrelated roster polling. Bind native revalidation to expected generation immediately before dispatch.

### H09 — Local reply state & independent supervision still exist

**Severity: High. Evidence:** Proposal B lines 25, 40; `core/src/bridge/deliver_claude.rs:533`; `core/src/bridge/hub.rs:255`; `mac/Notch/Sources/Sharing/NearbySharing.swift:204`; `windows/src/send.rs:919`; B06, B20–B23.

**Failure:** Existing Claude adapter advertises reply socket/pipe. “No listener state needs to survive” is false while keeping it unchanged. Current restart restoration consults persisted routes; replacing network leg does not restore dead local addresses. Both watchdogs inspected still gate hub launch on Nearby sharing. Removing LocalSend or disabling Nearby can strand relay client after hub exit. Server systemd cannot wake sleeping laptop, unlock credentials, start logged-out user's harness or repair hung hub.

**Smallest fix:** Supervise enabled bridge independently of file sharing, with secured single ownership & health beyond process existence. Either persist/recreate native listener routes with generation-owned shutdown or deliberately remove native reply path & qualify explicit CLI reply end to end. Persist replies before acknowledging local intake. Report relay reachability, node last contact, local service health & harness readiness separately; sleeping destination is stored/offline, not delivered.

### H10 — Waiting events are not available just because roster has status

**Severity: High. Evidence:** Proposal B lines 28–29; `core/src/bridge/roster.rs:18`, `:215`; `core/src/bridge/deliver_codex.rs:223`; B14–B17, B22.

**Failure:** Codex source exposes last activity & unknown open state, not permission/question stream. “Status requires input” does not supply question text, event lifecycle or answer channel. Claude status alone cannot distinguish stale waiting, answered question & native authorization prompt. Phone may answer expired event against resumed chat.

**Smallest fix:** First ship explicit messaging without automatic waiting claims. Later add harness-specific authenticated waiting adapters with source event ID, generation, question type, expiry, resolution & supported reply channel. Recheck unresolved state before answer dispatch. Missing capability means unavailable, not polling a guessed waiting heuristic. Publish actual Codex capability probe & age alongside roster.

### H11 — Metadata & notification paths contradict confidentiality claims

**Severity: High. Evidence:** Proposal B lines 17, 20, 28, 37, 40.

**Failure:** Relay's sessions table includes plaintext titles, harness, activity, liveness & capabilities beyond stated IDs/sizes/times. Titles can contain first prompt or secret. Relay also sees IPs, device relationships, push tokens & traffic rhythms; active timing/delivery manipulation can reveal which session is active. “Planner on Dell needs input” is metadata-bearing, not content-free. If question goes in normal APNs alert text, encryption elsewhere does not hide it; if encrypted, APNs cannot render plaintext without phone-side decryption. Backup duplicates metadata exposure.

**Smallest fix:** Use opaque mailbox IDs & encrypt session descriptors, titles, event detail & receipt diagnostics to authorized owner devices. Document residual routing/IP/size/timing/token metadata; do not promise traffic anonymity. APNs gets generic “Pulse needs attention” plus opaque fetch hint, no title/question. Only optional bounded encrypted notification extension payload may carry question, with safe generic fallback when key/decryption/time unavailable; no plaintext fallback. Authorize push-token registration per device/app environment, handle token changes & never treat successful APNs submission as message delivery. Redact Caddy/app/APNs logs & encrypt backup metadata.

### H12 — Stolen-key abuse bypasses proposed quotas; size promises conflict

**Severity: High. Evidence:** Proposal B lines 15, 20, 24, 44; `core/src/bridge/deliver_codex.rs:630`; B18–B19.

**Failure:** 2 GiB file cannot fit 500 MB per-device stored-blob quota under ordinary accounting. No definition of sender/recipient quota, reservations, temporary chunks or downloads exists. Stolen valid key can fill messages/nonces/receipts/sessions, flood APNs, reserve many incomplete uploads or repeatedly download same blob to exhaust egress. “One owner” does not constrain compromised enrolled process. Relay's 64 KiB ciphertext/plaintext limit is ambiguous; encryption/framing & harness wrapper consume bytes, while some Codex paths have smaller effective limit.

**Smallest fix:** Pick internally consistent v1 limits; cap complete file below quota or explicitly reserve larger capacity. Charge sender-owned committed+reserved bytes, recipient inbox budgets, temporary data & bounded global disk headroom atomically. Limit request rate/bytes, message count, outstanding transfers, sessions, nonce growth, connections, crypto work, push rate & download/egress volume per principal & globally. Reject overload before reading large bodies; stream with hard deadlines/caps. Cap unauthenticated verification traffic too. Reserve control/receipt capacity so blobs cannot starve commands/revocation. Publish negotiated destination plaintext & wire limits; reject before accepting promise to deliver.

### H13 — Chunking/resume lacks authenticated assembly & durable completion

**Severity: High. Evidence:** Proposal B lines 17, 24, 32.

**Failure:** “8 MiB resumable chunks” supplies no scheme binding chunks to file, index, length or recipient. Re-encrypting changed chunk under reused nonce can break encryption; relay can reorder/splice/drop chunks or lie about resume progress. Retrying finalization may create duplicate files; acknowledgement before durable save loses received file on crash. Folder ZIP adds mutable-source, temporary-space & extraction risks.

**Smallest fix:** Use fresh per-transfer content key wrapped to authorized recipients; authenticated encrypted manifest contains random transfer ID, key epoch, chunk count/lengths, total length, filename/type & whole-file integrity value. Use reviewed chunk AEAD construction with unique nonce per key/index/version; persist immutable encrypted chunks for retry, restart transfer/key if source changes. Bind index/length/transfer via authenticated data. Recipient verifies every chunk plus complete manifest; detect missing/extra/truncated data. Relay publishes upload only after durable finalization; resume exposes authenticated, bounded chunk status & handles expired reservations. Recipient stages privately, validates free space, then atomically publishes with explicit collision policy; signed completion only after durable save. No path traversal, symlink following or automatic ZIP extraction; limit archive expansion if extraction is added. Cancel/expiry/GC must reclaim orphan chunks without deleting live referenced data.

### H14 — LAN fast path breaks topology & creates address-injection surface

**Severity: High. Evidence:** Proposal B lines 10–11, 47.

**Failure:** Direct TLS requires receiver listener/inbound reachability, contradicting “No inbound ports anywhere.” Relay outage prevents acquiring candidate addresses/session grants, so proposed fast path does not inherently preserve home transfers. Malicious relay/device can offer loopback, cloud-metadata, unrelated private-service or rebound DNS addresses, turning clients into scanners. Ordinary TLS alone does not authenticate enrolled recipient; fallback can duplicate transfer or downgrade protection.

**Smallest fix:** Defer LAN from v1. Later explicitly permit bounded LAN listener with local-network permission/firewall UX; authenticate both enrolled device identities & transfer-scoped grant, carrying identical E2E chunks/IDs across paths. Treat candidates as untrusted, reject arbitrary URLs/redirects, constrain interface/addresses/port & verify peer before sending payload. Handle IPv6, changing networks & client isolation; private IP is not proof of same LAN. Offline mode needs previously paired discovery/trust plus explicit revocation-staleness policy, or remove offline claim. Fallback must resume same transfer under same authorization, never silently use unencrypted LocalSend.

### H15 — iOS cannot be an always-connected hub

**Severity: High. Evidence:** Proposal B lines 10, 28, 32, 35–37, 40.

**Failure:** Foreground WebSocket/long poll stops being reliable under suspension, force quit, reboot-before-unlock or network changes. APNs wakeups are not guaranteed durable workers; extension cannot complete arbitrary 2 GiB encryption/upload. “Sends … to phone” also differs from laptop Downloads semantics. Signing every fresh request may require key unavailable while locked; precomputed timestamp signatures can expire before background transfer starts.

**Smallest fix:** Phone local transactional inbox/outbox & foreground reconciliation are mandatory; APNs is only hint. Share extension stages durable bounded input into App Group storage, records ownership & returns truthful queued state. Use supported file-backed background transfer where compatible with authorization; define narrowly scoped expiring upload/download grants, revocation & resume after expiry. Stage encrypted chunks before scheduling; do not depend on unbounded extension work or permanent sockets. Specify key-access policy for background vs user-presence operations. Existing Apple provisioning does not establish correct app/extension identifiers, APNs environments, App Group/keychain entitlements or distribution/update path; include those deliverables. Phone receive lands in app inbox with explicit export/copy; no automatic background clipboard read/write. Qualify denied notifications, delayed/coalesced pushes, locked device, extension termination & relaunch.

### H16 — Restore can resurrect revoked devices or re-execute commands

**Severity: High. Evidence:** Proposal B lines 17, 24, 40, 48; B05–B06, B11–B13.

**Failure:** Nightly R2 copy is not consistent backup of SQLite WAL plus separately stored blobs. Restore can lose acknowledged messages, rewind cursors/nonces, revive revoked key or replay pre-crash command whose native effect survived. Relay ciphertext backup cannot restore lost recipient keys or local receipt/effect journals. Seven-day blob purge & thirty-day message retention leave manifest referencing missing payload unless modeled; backups may retain “deleted” content much longer.

**Smallest fix:** Set explicit recovery point/time objectives; nightly-only backup means up to one day of acknowledged relay state may be lost after total server loss unless another durable copy remains. Use SQLite-consistent snapshot/backup method & versioned blob inventory, integrity checks, encrypted backups, scoped R2 credentials, lifecycle & practiced restore. Preserve membership/revocation authority outside rewindable relay state. Restore into new server epoch, reconcile local message IDs/cursors/receipts, never automatically redispatch uncertain commands. Retain sender copy through chosen recovery horizon or disclose stored receipt's exact durability promise. Define local-key backup vs deliberate loss, tombstone retention, orphan GC & backup deletion policy together.

### H17 — Rotation, upgrades & second relay need protocol compatibility

**Severity: High. Evidence:** Proposal B lines 14–17, 40, 47, 49.

**Failure:** Offline old hub can reconnect after schema/envelope/crypto change, misread permission kind, reset cursor or encrypt to retired key. Rolling back relay binary across destructive migration can corrupt state. “Second relay later is a config list” introduces divergent receipts, authorization epochs & duplicate dispatch; independent SQLite files are not replicated leadership.

**Smallest fix:** Version signed envelope, crypto suite, roster, receipts & API separately from app release. Advertise capability/minimum compatible versions; unknown security-sensitive kinds fail closed & retain message for visible recovery. Define old-client upgrade path, key-transition grace & offline drain windows. Pin releases, verify artifacts, back up before transactional migration, drain active work & specify rollback compatibility. Keep one active relay; recovery/failover is fenced manual transfer with reconciled state until an actual replication design exists.

### H18 — SQLite WAL is viable, but not a durability/operations specification

**Severity: Medium. Evidence:** Proposal B lines 20, 40; `core/src/bridge/store.rs:291`, `:589`.

**Failure:** Long reader or blob write starves checkpoint/writer; disk-full breaks journals just when receipts are needed. WAL on unsuitable shared filesystem or copying only main database loses assumed durability. Holding database transaction during WebSocket, APNs or chunk I/O blocks unrelated work. Local store does not become transactional because server uses SQLite.

**Smallest fix:** Single relay process on local persistent filesystem, short bounded transactions, uniqueness/foreign-key constraints, busy handling, explicit sync durability, checkpoint/WAL size policy & disk reserve. Store large encrypted chunks as immutable files with DB commit/finalize & orphan-reconciliation protocol, rather than multi-GiB row transactions. Network I/O follows committed job rows, outside transaction. Monitor integrity/backup/checkpoint errors; database failure refuses new acceptance. Implement local node journals separately with equivalent guarantees.

### H19 — Caddy/TLS & observability are names, not deployable operations

**Severity: Medium. Evidence:** Proposal B lines 9–10, 37, 40.

**Failure:** Public relay necessarily exposes inbound HTTPS even if laptops do not. Caddy TLS termination does not authorize app requests; exposed backend or trusted forged proxy headers bypass intended boundary. Certificate renewal/DNS failure, APNs credentials expiry, disk saturation or broken outbox can all display simplistic “relay ok.” Debug logs can disclose enrollment tokens, titles, blob URLs or plaintext errors despite E2E.

**Smallest fix:** Document public DNS, certificate issuance/renewal method & only required ingress ports; bind relay backend to loopback/Unix socket with restrictive access. Keep admin endpoints private, validate host/origin where applicable, trust forwarding headers only from Caddy & validate signatures at app boundary. Configure stream/body caps, long-poll/WebSocket timeouts & bounded graceful restart; keep Caddy admin interface private. Run unprivileged systemd service with narrow writable state paths, managed APNs/R2 credentials & pinned artifacts. Redacted diagnostics must show last durable mailbox commit, queue age, uncertain dispatches, receipt lag, disk/WAL/quota, backup age/restore result, cert expiry & push failures. Separate liveness from readiness; provide owner-visible failure & recovery actions without logging content or secrets.

### H20 — “Same card” & “all three installed” do not qualify LocalSend replacement

**Severity: Medium. Evidence:** Proposal B lines 31–33, 49, 52; `hub/src-tauri/src/share.rs:33`, `:655`, `:710`; `mac/Notch/Sources/Sharing/NearbySharing.swift:303`; `windows/src/send.rs:109`; `core/src/lib.rs:9`; `core/Cargo.toml:32`.

**Failure:** Existing card sends to LocalSend fingerprints; new target is enrolled identity, possibly offline. Existing share-command drain removes file before applying command. Enrollment is not user consent to accept files, overwrite files or write clipboard. Nearby→your-devices changes implicit one-target paste behavior. Removing `localsend` feature also removes bridge module/dependencies currently gated by it. Cutover after installations can lose in-flight transfers, known-device preferences & LAN availability; rollback might resend already completed file.

**Smallest fix:** Reuse presentation controls & safe file-save utilities after audit; introduce transport-neutral durable transfer model/commands, device-ID targets, explicit acceptance, retained failures & structured completion receipts. Distinguish text delivery, explicit Copy action & agent instruction. Keep transfer-specific identity across restart/path change; translate preferences explicitly, never equate LocalSend trust fingerprints with enrolled keys. Decouple bridge feature/common crypto utilities before removing protocol. Migrate one direction/device pair at a time, drain old work, preserve old receipts & offer explicit rollback for new transfers only. Remove LocalSend only after real file/clipboard, reject/cancel/resume, offline, permissions & retained-state journeys pass on all intended nodes.

### Reuse versus rewrite

| Existing surface | Reuse | Required replacement/extension |
|---|---|---|
| `core/src/bridge/store.rs` | Inbox/reply/outbox concepts, explicit ack UX, bounded state & migration knowledge | Transactional authenticated journal, durable claims/receipts/cursors, typed identities, key epochs; destructive queue drain & fail-open dedupe cannot back new promise. |
| `core/src/bridge/roster.rs`, `deliver_codex.rs` | Local discovery, exact thread lookup, capability probes, unknown/freshness fields | Enrolled export filter, versioned snapshots/tombstones, stable endpoint generation, privacy; waiting integration is new. |
| `deliver_claude.rs`, `deliver_codex.rs` | Native socket/pipe & queue mechanics, process revalidation, bounded I/O, honest partial receipts | Typed provenance/policy adapters, authenticated local attribution where possible, idempotency reconciliation, durable explicit replies; not “unchanged.” |
| `core/src/bridge/mod.rs`, `hub.rs`, `bridge_cmd.rs` | CLI `send/reply/inbox/status`, local dispatch boundary, Off concept | Transport interface, relay client, immutable envelope/parent routes, transaction-owned dispatch & retry/status by ID. SSH link adapter leaves new path. |
| `hub/src-tauri/src/share.rs` | Tauri events, platform wake hints, save-folder configuration, accept/decline/cancel controls | Replace `Service`/`SendItem` coupling & delete-before-apply commands with durable transfer service; independent lifecycle. |
| Mac `NearbySharing.swift`, `SendCard.swift`; Windows `send.rs` | Paste/drop, picker, progress & acceptance presentation | Enrolled device IDs, offline/pending states, consent, retained receipts; Windows bridge model & agent conversation UX need work. |
| New components | Shared Rust protocol logic where compatible | Headless Linux relay/deployment, encrypted blob store, SwiftUI app, share extension, APNs, iOS persistence/key/background integration, recovery tooling. None is supplied by existing Send card. |

RightKit ownership can guide implementation selection, but package availability/declarations are not integration evidence. Repository's own `core/Cargo.toml:24` records current SQLite/toolchain constraint; do not assume dropping in a dependency supplies relay storage, crypto or mobile bindings.

## 3 Telegram versus Hetzner relay

Compare both **after** shared message-service repair. Neither raw proposal is safe to ship.

| Dimension | Telegram adapter | Owner's Hetzner relay + iOS app |
|---|---|---|
| Security/content | Bot path places content with Telegram; user/group/topic authorization plus bot-token lifecycle. No E2E claim. | Can hide content from relay with recipient-verified E2E/key enrollment. Owner controls server; endpoint compromise, metadata & APNs remain trust boundaries. |
| Authorization | Numeric allowlists/topic binding; account takeover commands agents. | Key-scoped capabilities; stolen key commands agents unless revoked/scoped. Human intent still distinct from agent/forwarded content. |
| Reliability | Mature phone interface; custom poll offset/outbox still required. Mac gateway + SSH are extra Dell dependencies. | Each hub connects independently; sleeping laptop queues centrally. Owner now operates sole relay; iOS suspension needs foreground recovery. LAN claim absent until separately implemented. |
| Effort | Shared journal/policy work plus bot adapter, topic mapping, notification/receipt rendering. Smallest phone UI effort. | Same journal/policy work plus enrollment/E2E, Linux deployment, iOS app/distribution/background sync. Blob/LAN replacement adds separate substantial project. |
| Operations | Token rotation, account/group security, gateway supervision, API failures/limits. Service infrastructure outsourced. | TLS/DNS, service updates, capacity/abuse controls, SQLite/WAL, encrypted backup/restore, APNs credentials, key revocation/rotation & client compatibility. Existing server does not make operations free. |
| User experience | Familiar chat/push, quick first interaction; topics & receipt chains can be awkward. | Purpose-built device/chat selection, receipts & consent; initial QR setup, app lifecycle & releases. Better integrated file UX only after implementing it. |
| Privacy during failure | Sensitive replies/questions can enter Telegram before user sees them; filtering cannot guarantee secrecy. | Locked/lost keys can make ciphertext unavailable; explicit recovery/loss policy. Relay outage should show retained queued state, never silent success. |
| Files/clipboard | Outside first proposal; LocalSend continues independently. | Unified identity/transport useful, but acceptance, secure save, quota, chunks/resume & extensions are additional protocols. |

Hetzner wins architectural control/confidentiality potential. Telegram wins time to useful phone messaging once shared core is trustworthy. Choose Hetzner for those properties, not a claim of less work or universally better reliability.

## 4 Recommended build order & first shippable slice

1. **Freeze contracts & baseline.** Define threat boundary (malicious relay, stolen enrolled key, same-user processes), typed identity/capabilities, cryptographic profile, receipt state graph & loss/recovery policy. Inventory current improvements against B01–B27; no wholesale rewrite of useful adapters. Decide key recovery, offline revocation budget & migration compatibility now.
2. **Repair local service before Internet ingress.** Transactional ingress/outbox/receipts, fail-closed dedupe, same-ID retries, retained unknown outcomes, exact-generation dispatch, reliable reply routes & independent supervised hub. Remove alias routing from new protocol. Exercise complete local-to-remote reply/restart journey through existing native adapters.
3. **Build single relay for text only.** Owner bootstrap/enrollment/revocation, signed E2E envelopes, mailbox/cursor protocol, quotas, SQLite durability, TLS/systemd, redacted diagnostics & backup/restore. Two hubs connect outbound; keep legacy LocalSend independent. Prove restored server cannot re-execute already dispatched command or resurrect revoked sender.
4. **First shippable owner preview: foreground iPhone text conversation.** Minimal SwiftUI Devices/Chats/Conversation app, QR enrollment, durable phone outbox & inbox. Explicitly enrolled Mac Claude + Dell Codex endpoints, bounded text negotiated to destination, send→stored→native queued/sent→explicit `pulse bridge reply`→phone. Preserve conversation through laptop offline/reconnect, phone relaunch & hub restart; display unknown without auto-resend. Generic APNs can follow; this slice promises neither automatic waiting capture nor files. Close first complete phone journey before broadening harness/platform matrix.
5. **Finish agent messaging qualification.** Mac/Dell × Claude/Codex supported routes, both reply directions, stale/renamed/resumed/archived targets, wrong generation, duplicate/conflicting IDs, Off/revocation during pending work, crash before/after native acceptance, relay restore, outbox full & unsupported old clients. Add generic APNs & iOS foreground catch-up; then genuine waiting-event adapters with expiring answers. Native permission prompts stay outside ordinary chat approval.
6. **Add clipboard/text share, then small single files.** Explicit targets/accept/copy, durable transfer intent, encrypted manifest/chunks, quotas, safe destination save & interruption recovery. Stage phone share-extension work durably. Expand sizes/folders only after locked-phone, terminated-extension, disk-full, quota, cancellation & whole-file integrity journeys pass.
7. **Migrate LocalSend; add LAN last if justified.** Qualify three-node transfer/recovery matrix & retained state, migrate preferences, drain legacy work, then remove protocol without removing bridge feature. Measure relay bandwidth/latency first. LAN is optional second transport with inbound listener/trust/offline-revocation design, not prerequisite to first message.

**Honest cost:** This is four workstreams: durable agent service; secure relay/operations; iOS product; file-transfer replacement. Planning allowance for one experienced engineer, reusing maintained libraries & existing adapters: shared core roughly 2–4 engineer-weeks; relay/enrollment/recovery 2–4; foreground iOS slice 2–3; broader qualification/notifications 2–4; file/extension/migration 3–6; optional LAN another 1–3. These are review estimates, not measured delivery commitments. Sequential text preview is roughly 6–11 engineer-weeks; full replacement roughly 11–21 before optional LAN. Telegram would still pay shared-core cost but can avoid most mobile/E2E/server product work. Harness protocol surprises & platform qualification belong in schedule, not an assumption that library tests prove delivery. Budget recurring storage/backup/egress, Apple distribution upkeep & operator time; no live vendor pricing was consulted.

**Qualification inventory:** Inspected bridge suite still totals **3 registered tests**: `round_trips_and_rejects_bad_input` (`core/src/bridge/envelope.rs:190`), `base64_round_trips` (`core/src/bridge/links.rs:600`) & `two_computers_message_each_other_over_ssh` (`core/tests/bridge.rs:517`). That is **2 unit/component tests + 1 simulated integration journey; 0 installed native cross-machine/phone E2E journeys in this scoped suite**. Before/after unchanged; added 0, deleted 0, executed 0. Existing repository storage/dashboard journeys are not gateway evidence. Follow `docs/testing.md`: extend complete installed journeys, retain existing tests, record build identity, observed failures/recovery & retained state. Builds/signing/packaging run only through generated RightKit workflows. Ship text slice after its real phone→harness→phone recovery journey passes; expand scope from that evidence.
