# Hetzner phone gateway: adversarial review of v2

Reviewed 2026-10-10 against working-tree v2, prior H01–H20 review & source cited below. Source inspection only; no builds, tests, network or Git. Findings describe observed code or explicitly identified design gaps, not reproduced exploits.

## 1 Verdict on v2

**Keep own app, SSH-first access, outbound laptop connections & explicit Nearby trust. Change implementation contract before shipping.** SSH removes substantial custom transport work; it does not supply mailbox routing, durable delivery, phone return identity or device authorization.

V2 contains two incompatible systems. Owner decisions specify SSH commands on Hetzner (`docs/phone-gateway-hetzner.md:10`); retained topology specifies HTTPS, no SSH in message path & opaque E2E ciphertext (`:26`, `:27`, `:34`). Running ordinary `bridge send` on Hetzner exposes plaintext there. Treat owner decisions as controlling; rewrite remaining sections around one trusted-server SSH design. No Telegram work remains.

**No H finding is fully closed by v2 as written.** Conditional disposition after removing superseded promises:

| Prior findings | SSH-first disposition |
|---|---|
| H01 authorship, H03 enrollment | Custom sealed-box/request-signature flaws leave first-slice scope. SSH supplies authenticated hop credentials, not phone-to-laptop authorship through compromised Hetzner. Key→principal/capability bindings, host pinning & revocation remain. |
| H02 key storage | Moves to actual Secure Enclave SSH signer, key access policy, replacement & recovery. “FaceID-gated” alone is insufficient. |
| H04 agent authority | Remains. Forced `post` still reaches agents with tools; terminal adds direct server execution authority. |
| H05 replay, H06 crash gaps, H07 receipts, H08 routes | Remain. SSH replay protection does not deduplicate application retries, preserve native outcomes or create phone mailboxes. |
| H09 supervision/replies, H10 waiting events | Remain. SSH does not create durable native reply routes or waiting-event adapters. |
| H11 confidentiality | Moves to explicit trust in Hetzner, logs, local caches & backups. APNs metadata risks leave first slice only when pushes are explicitly deferred. |
| H12 quotas | Message/rate/disk limits remain; blob-size conflict leaves first slice when files are deferred. |
| H13 blob assembly, H14 new LAN fast path | Deferred, not solved. Existing LocalSend now needs its own authentication fixes. |
| H15 iOS lifecycle | Moves from WebSocket to SSH suspension/reconnect, FaceID unlock & foreground catch-up. Background delivery is not gained. |
| H16 restore, H17 upgrades, H18 durability, H19 operations | Remain. Caddy/HTTPS-specific setup leaves scope; SSH account restrictions, journal backup, restore epochs & deployment replace it. |
| H20 migration | Owner's explicit “keep LocalSend until journeys pass” closes premature-removal direction, once contradictory removal-after-installation wording at `:50` is deleted. Trust UI & Windows parity remain implementation work. |

New/reintroduced findings: phone credential/session lifecycle, forced-command framing, phone-generated remote shell strings, machine-output parsing, terminal privilege crossover, unauthenticated Nearby fingerprints, defective TLS signature verification, text acceptance/receipt mismatch & single-writer policy ownership.

## 2 Findings

Ranked by impact. **High** blocks safe authorization or promised send/reply journey; **Medium** blocks reliable discovery, recovery or honest UX.

### V01 — Allow would authorize attacker-supplied fingerprints

**Severity: High. Evidence:** `core/src/localsend/net.rs:99` disables client authentication. `core/src/localsend/proto.rs:28` defines identity as JSON used in discovery/register/prepare. `core/src/localsend/discovery.rs:266` parses announcements without authentication; `core/src/localsend/mod.rs:291` updates address by claimed fingerprint. `core/src/localsend/receive.rs:165` calls `is_known`; `core/src/localsend/mod.rs:360` checks only remembered fingerprint plus discovered IP.

**Failure:** Attacker announces victim fingerprint/alias from attacker's real IP, overwriting discovered address, then submits same fingerprint in `prepare-upload`. Neither alias nor matching IP proves possession of victim key. Adding `nearby_devices.json` lookup at this point merely turns spoofable “known” into spoofable Allow. A callback that finds genuine victim certificate also fails to bind original request to that device.

**Smallest fix:** Authenticate request origin before looking up Allow: Pulse peers use verified TLS client certificates pinned during enrollment, or reviewed request signatures binding method, body digest, recipient & fresh challenge. This requires interoperable Pulse authentication support; ordinary LocalSend JSON cannot provide it. Keep legacy peers on explicitly selected Ask with visibly unverified identity. No silent receive from unverified peers.

### V02 — TLS pinning accepts copied certificates without proof of private key

**Severity: High. Evidence:** `core/src/localsend/net.rs:130` compares DER hash, but `:140` & `:149` unconditionally accept TLS handshake signatures. `:163` silently disables pinning for malformed fingerprints. `core/src/localsend/mod.rs:627` trusts announced protocol when choosing HTTPS; `:598` permits alias fallback.

**Failure:** SHA-256 collision is unnecessary. Certificate bytes are public; malicious TLS peer can present copied certificate while supplying invalid possession proof that this verifier accepts. Discovery can also downgrade protocol to HTTP. Alias+fingerprint adds no cryptographic strength.

**Smallest fix:** Keep self-signed pin comparison, verify TLS 1.2/1.3 signatures using configured crypto provider, require valid canonical SHA-256 pin & HTTPS for trusted sends, reject downgrade. Resolve sends only by verified identity; require explicit owner enrollment against independently compared fingerprint/QR. First-seen network fingerprint is not owner approval.

### V03 — “Second link” is not outbound-only relay routing

**Severity: High. Evidence:** V2 `:11`, `:12`, `:23`. `core/src/bridge/links.rs:211` starts one SSH command; `:480` asks remote `peers --local --json`; `:588` posts directly. `core/src/bridge/mod.rs:642` chooses destination link; `:676` resolves receiving machine's local sessions before delivery.

**Failure:** Links are directed destinations, not maintained bidirectional channels. Laptop→Hetzner grants Hetzner neither reachability nor credentials for return calls. Dell sees Hetzner's local chats, not Mac chats behind it; `post` on Hetzner refuses Mac session rather than forwarding. Existing store is not destination-addressed relay mailbox; CLI bridge dispatch (`core/src/bridge_cmd.rs:26`) supplies no mailbox worker/serve operation. Restricting laptop key to `post` also blocks roster polling/link validation. Retention alone changes none of this.

**Smallest fix:** Add explicit mailbox transport within bridge: laptops authenticate outbound, publish scoped rosters, fetch addressed work, commit locally, upload receipts/replies. Give Hetzner durable phone mailbox & routing table. Reuse native dispatch, not fictitious transitive links. Remote Dell→Mac uses this same new mailbox path; adding another enrolled laptop is configuration **after** that transport exists. Internet files still require separate blob work.

### V04 — Phone SSH session has no usable reply endpoint

**Severity: High. Evidence:** `core/src/bridge/mod.rs:337`, `:405` identify plain SSH caller as `cli`; `:676` accepts only local discovered sessions. `:269` saves reply route only for Claude without direct reply socket. `core/src/bridge_cmd.rs:317` resolves route then calls `send_text`; `:449` reads local session inbox. `core/src/bridge/envelope.rs:47` lacks parent ID, harness, generation & authenticated principal.

**Failure:** Phone `send` may work with direct laptop access, but reply to Hetzner `cli` is not ordinary discoverable agent chat. Reading `inbox --session cli` does not create incoming routing. Codex delivery gets reply guidance without corresponding route saved here. Shared `cli` also merges distinct phone credentials into one label. Current route TTL is 30 days (`core/src/bridge/store.rs:35`), while CLI error still says 24 h (`core/src/bridge_cmd.rs:321`); receive expiry is separately 24 h (`core/src/bridge/mod.rs:658`).

**Smallest fix:** Register durable non-harness mailbox per authorized phone principal; route replies there explicitly. Persist return route before dispatch for both adapters. Carry immutable message ID, parent ID, destination device/harness/session/generation & origin type; alias is display only. Define retention/expiry once & expose it consistently.

### V05 — SSH does not repair loss, duplicates or false success

**Severity: High. Evidence:** `core/src/bridge/store.rs:795` fails open on journal failure; `:904` deletes outbox before posting. `core/src/bridge/mod.rs:680` records seen before native dispatch; duplicates return refusal rather than original result. `:611` allocates fresh envelope per send. `core/src/bridge/hub.rs:394` treats any returned receipt as completed retry, including unknown/refused.

**Failure:** Reconnect/retry can lose reply, suppress work never dispatched or execute twice. Phone termination after remote acceptance leaves ambiguous outcome; repeating text creates new identity. SSH exit 0 also includes held/sent, not just delivered (`core/src/bridge_cmd.rs:283`).

**Smallest fix:** Transactional jobs, dedupe results & receipt outbox on server, laptop & phone; caller-assigned immutable IDs; query/retry same ID. Commit before acknowledging, remove only after durable next-hop acknowledgement. Record native attempt before call; uncertain effects stay unknown pending reconciliation, never automatic replay. Preserve `sent`, `queued`, `held`, refused & unknown distinctions. Restore uses new server epoch & reconciles retained local IDs before dispatch.

### V06 — Forced `post` is underspecified authority, not sandbox

**Severity: High. Evidence:** V2 `:12`, `:60`; `core/src/bridge_cmd.rs:78` requires exactly one encoded argument; `:499` decodes then calls receive. `core/src/bridge/envelope.rs:33` accepts sender labels from envelope. Claude wrapper marks content agent-unverified (`core/src/bridge/deliver_claude.rs:463`); reply listener ignores auth frame & checks known session label (`:1074`).

**Failure:** Literal forced command `pulse bridge post` has no payload argument & fails. Wrapping `SSH_ORIGINAL_COMMAND` in shell evaluation reopens execution. Restricting command without forwarding/PTY restrictions leaves other SSH capabilities. Even perfectly restricted `post` can induce agent tool use under existing harness permissions; it does not prove claimed chat authorship. “No remote execution” & “no key opens laptop” are misleading if this alternate path ships.

**Smallest fix:** Prefer outbound mailbox design: no Hetzner laptop key. If forced delivery is ever added, dedicated fixed executable reads bounded framed stdin, binds principal from server-owned key configuration, validates destination/capabilities/expiry & never evaluates original command. Disable PTY, agent/X11/TCP forwarding & user startup hooks for this credential; verify equivalent effective Windows restrictions. Preserve agent-unverified provenance; separately authenticated owner instructions must never approve native permission prompts implicitly.

### V07 — Phone's own command builder & parser create new injection boundary

**Severity: High. Evidence:** V2 `:10`; current bridge quotes POSIX/PowerShell words (`core/src/bridge/links.rs:152`, `:170`), while actual SSH receives one remote command string (`:227`). `core/src/bridge/links.rs:468` takes last line starting `{`; phone-facing `send` emits `msgId/state` (`core/src/bridge_cmd.rs:288`) versus internal `post` emitting `msg_id/status` (`:505`). No phone implementation is specified here.

**Failure:** SSH-library “arguments” need not bypass remote shell. Chat titles, aliases, IDs, message text or server-selected executable paths interpolated into command strings can execute shell syntax. `--` prevents CLI option parsing, not shell expansion. PTY banners, ANSI sequences, partial JSON, output floods, unexpected fields & disconnected exit-status streams can become false success or UI injection. Existing quoting deserves credit; it does not protect separately written phone code.

**Smallest fix:** One fixed no-PTY command/subsystem, absolute installed executable, bounded versioned JSON frames on stdin/stdout. Dispatch operations to typed core functions or real local argv; never shell-evaluate payload. Bind response request/message IDs, validate schema/status, separate stderr, enforce byte/time limits & treat truncation/disconnect as unknown. Render titles/body as inert text. This is new bridge RPC framing, not an already available CLI contract.

### V08 — FaceID-gated key does not constrain authenticated session or terminal

**Severity: High. Evidence:** V2 `:10`, `:53`, `:60` promises Enclave key, terminal & inability to change policies simultaneously; no signer/session/account contract accompanies them.

**Failure:** Non-exportable key can still authorize unwanted actions through unlocked app or long-lived SSH session. FaceID at login does not gate each channel/message. Lost phone requires server-side revocation; removing authorized key does not itself terminate existing sessions. General terminal using same credential may change links, policy or server files. Compromised terminal output can target clipboard, links or pasted commands.

**Smallest fix:** Implement actual hardware-backed P-256 SSH signing through platform key API; qualify SSH signature encoding/library integration on device. Define biometric enrollment-change behavior, passcode fallback policy, foreground unlock, short idle lock & channel shutdown on background/lock. Pin Hetzner host key independently; never silently accept changed host key. Restricted messaging account/key has no shell/forwarding. Optional terminal reuses client library but uses separate explicit admin profile/key, fresh authentication, bounded escape handling, no automatic clipboard writes or command execution. Keep recovery/admin credential off phone messaging profile; revoke key, kill sessions & cancel undispatched work together.

### V09 — Ask text currently bypasses consent & sender lies before response

**Severity: High. Evidence:** `core/src/localsend/receive.rs:210` autoaccepts every text; `:259` publishes content & returns 204. `core/src/localsend/send.rs:176` invokes sent callback immediately after writing body; `:251` turns it into Delivered. `core/src/localsend/mod.rs:940`, `:956` finalize success & ignore later outcome. Mac `mac/Notch/Sources/Sharing/NearbySharing.swift:777` & Windows `windows/src/send.rs:1002` already have dormant message Ask cards, both revealing preview & appending “Saves to …”.

**Failure:** Adding policy lookup without removing special cases still reveals refused text or reports success after 403. Ask preview itself can disclose private content before consent. Text necessarily arrives inside prepare JSON; “nothing lands” cannot mean zero bytes received.

**Smallest fix:** Same policy gate for file/text before publishing content. Bound text in transient memory until accept; preaccept card shows verified sender, type & size, no body/URL. Deny/timeout discards body without content logs/history/state snapshot. Acceptance publishes text with explicit Copy/Open; no automatic clipboard changes. Mark sender accepted only after successful protocol response; 403/timeout must override waiting. Fix both platform cards' text wording.

### V10 — Silent Allow & next-request revocation are too broad

**Severity: High. Evidence:** V2 `:17`, `:20`; `core/src/localsend/receive.rs:293` creates transfer sessions; `:350` authorizes upload by IP/token, without current trust lookup. `:505` automatically remembers fingerprint after completed file transfer. Mac `mac/Notch/Sources/Sharing/NearbySharing.swift:305` & Windows `windows/src/send.rs:806` implicitly target sole discovered device.

**Failure:** Stolen allowed phone retains valid identity; receiver cannot distinguish thief. Sender can flood disk/text or receive accidental paste. Downgrade during pending Ask or active upload leaves prior grants usable. Accepting one file silently promotes future trust under legacy accept-known behavior. Filtering targets to Allow/Ask can leave one Ask device & unexpectedly activate sole-target paste.

**Smallest fix:** Allow authorizes bounded passive inbox delivery only: never execute/open files, overwrite existing files, auto-copy text or inject agents. Explicit owner action to promote; no accept-once→Allow. Phone requires foreground unlock for sends. Revocation cancels pending decisions/active sessions, invalidates tokens & rechecks policy before final publication; already delivered data remains delivered. Ask targets require explicit selection; sole-target convenience applies only to owner-selected verified Allow device. Publish quiet activity history & one-click global pause/revoke.

### V11 — Hub settings file is not current policy ownership model

**Severity: High. Evidence:** V2 `:20`; `hub/src-tauri/src/share.rs:5`, `:290` read notch-owned preferences, `:345` passes global accept-known flag into core; core writes `known-devices.json` (`core/src/localsend/mod.rs:369`). Hub emits schema 1 snapshots (`hub/src-tauri/src/share.rs:602`) & deletes commands before applying (`:655`). Neither consumer models trust: Mac `mac/Notch/Sources/Sharing/NearbySharing.swift:7`; Windows `windows/src/send.rs:54`, `:140`.

**Failure:** Hub, notch & core writing separate preferences produces lost updates or stale Allow. A crash can lose revocation queued through existing command files. Publishing JSON does not make consumers enforce it; direct core/API sends still bypass UI filtering. Migration from “accepted before” to Allow imports unauthenticated trust.

**Smallest fix:** Hub service is sole runtime writer through shared core policy API; core enforces every ingress/send regardless of UI. Use versioned atomic owner-only policy file, durable mutation acknowledgement & monotonic revision; unknown/malformed policy fails closed. Retire accept-known as authority; migrate remembered peers to unverified Deny records pending owner review. Both notches are read-only projections plus mutation commands; snapshot never authorizes traffic. Detailed rules below.

### V12 — Silent Deny needs deliberate discovery & bounded hostile observations

**Severity: Medium. Evidence:** V2 `:16`, `:19`; `core/src/localsend/mod.rs:291` accepts discovery metadata; `:350` lists volatile devices; `:847` prunes unresponsive devices. Refused prepare path need not register sender (`core/src/localsend/receive.rs:123`).

**Failure:** Legitimate sender gets generic refusal while receiver sees nothing unless already visiting hub. “Every device” is impossible when multicast/firewall/permissions prevent discovery. Random fingerprints/aliases can inflate state, impersonate “Neo tried 3 times” or manufacture endless changed-fingerprint notices. Reinstall cannot safely inherit Ask just by copying known alias.

**Smallest fix:** Explicit Nearby → Add device / Review blocked entry, quiet aggregate badge inside hub, refresh & manual QR/fingerprint pairing. Persist bounded recent observations independently of live targets, including refused-only senders; label unverified claims, rate-limit counters, cap/expire unknown rows while retaining owner trust/revocation records. Changed fingerprint is new Deny identity with grouped notice inside hub; owner explicitly verifies replacement. Never match trust by alias.

### V13 — Foreground SSH does not deliver pushes or supervise sleeping laptops

**Severity: Medium. Evidence:** V2 `:45`, `:54`, `:57`; `core/src/bridge/roster.rs:215` explicitly reports Codex open state unknown. Hub bridge runs independently of sharing while hub lives (`hub/src-tauri/src/share.rs:507`), but relaunch watchdogs still depend on Nearby enablement (Mac `mac/Notch/Sources/Sharing/NearbySharing.swift:205`; Windows `windows/src/send.rs:919`).

**Failure:** iOS suspension stops reliable polling; FaceID-gated signing cannot promise unattended reconnect. SSH supplies no APNs provider or waiting-event source. Disabled Nearby can prevent relaunch of dead hub, stranding mailbox worker. Roster age is not “input needed”.

**Smallest fix:** First slice says “refresh when app opens”; durable phone/server catch-up handles disconnect. Supervise bridge independently of Nearby on both laptops; show last contact, worker health & harness capability separately. Later add genuine waiting adapters plus generic APNs hints, foreground reconciliation & expired-event handling. Never infer waiting from stale roster.

## 3 Device trust model: verdict and the corrected rules

**Verdict: Allow/Ask/Deny is right UX; claimed fingerprint+alias is wrong authorization boundary.** Correct certificate hash identifies certificate bytes. With handshake proof & owner pairing it binds retained key possession, not person, hardware integrity or current owner intent. Reinstallation/key loss creates new identity; stolen key remains same identity until revoked.

1. **Identity:** canonical certificate hash is policy key only after proof of possession; owner label is separate from untrusted advertised alias/model/address. No alias fallback, HTTP downgrade or automatic replacement. Phone SSH identity & Nearby TLS identity are separate credentials with separate grants. SSH-only phone app is not automatically LocalSend device.
2. **States:** unknown/reinstalled peer = Deny. Ask = one request, bounded memory, explicit acceptance, no trust promotion. Allow = paired authenticated Pulse peer, explicit owner opt-in, limited passive file/text receipt. Legacy LocalSend peers may use owner-selected Ask; no fabricated authentication claim or silent Allow.
3. **Discovery:** network observations remain visible in hub's bounded “Seen / Blocked” list, including first/last observed times, claimed metadata, verification status & refused-attempt counts. “Devices observed” replaces “every device”. Quiet hub badge & Add device path explain how to enable legitimate sender without unsolicited notch/toast prompts. Counts are rate-limited observations, not proof named person attempted transfer.
4. **Content:** Deny returns 403 for transfer requests without prompts or content retention; limited discovery remains available. Ask text shows sender/type/size first, then accepted content with explicit Copy/Open. Allow may save/show passive content quietly with history; locked-screen body previews stay hidden. Limits apply before large allocation & before publishing files.
5. **Revocation:** Deny increments policy revision, rejects new work, cancels pending Ask & active sessions, invalidates upload tokens & fences completion. Blocked send target disappears immediately; stale card cannot bypass core. Owner can revoke from either laptop's hub; policy remains per receiving machine unless explicit synchronization is implemented.
6. **Storage/ownership:** use `~/Library/Application Support/Pulse/nearby_devices.json` on Mac & `%LOCALAPPDATA%\Pulse\nearby_devices.json` on Windows, following `hub/src-tauri/src/share.rs:168`. Hub process is sole writer via core policy module; restrict parent/file permissions or Windows DACL, atomically persist before success response, serialize updates by revision. Keep bounded observation/counter data separate from durable trust records. A same-user compromised process is outside this file's protection boundary; remote content must never write policy.
7. **Projection/parity:** version `share-state.json`; publish effective trust, verification, policy revision, blocked aggregate & eligible targets. Both notches consume identical meanings, reject unsupported security schema & use request IDs/revisions for actions. Windows currently needs numeric `isMessageN` alongside Mac boolean (`hub/src-tauri/src/share.rs:610`, `windows/src/send.rs:161`); update both decoders intentionally. Add fingerprint/verification to both Ask models, remove preaccept preview & “Saves to” for text, retain explicit Copy/Open. Hub provides acceptance fallback when notch unavailable; timeout declines. Core remains authoritative if UI is stale, absent or directly bypassed.

## 4 The recommended solution (one page)

**Ship own SwiftUI phone app over SSH to one trusted Hetzner mailbox, with laptop workers connecting outbound.** Keep familiar Devices → Chats → Conversation screens, explicit receipts & refresh-on-open. No Telegram. Keep LocalSend for nearby files/text while fixing authentication & trust; internet files come later.

Phone enrolls by owner-confirmed server host fingerprint plus restricted SSH public key. Secure Enclave signer requires foreground FaceID unlock; app closes transport on lock/background & reconnects explicitly. Use fixed no-PTY bridge RPC with bounded JSON frames, not dynamically assembled shell commands. Restricted server account authorizes only phone mailbox operations. Optional terminal is separate admin profile/key with fresh authentication; same SSH library, separate authority.

Hetzner is third bridge device **plus new mailbox service**, not an existing agent chat pretending to be relay. Each laptop has its own restricted outbound credential, publishes approved session roster & pulls only its mailbox. Server holds no laptop SSH key, so neither inbound laptop listener nor forced laptop command is needed. Shared bridge code dispatches locally after policy/session-generation checks. Dell-away→Mac then uses identical mailbox routing, including return path.

Use explicit phone principal & return mailbox; bind message IDs, parent IDs, endpoint generations & provenance. Server derives principal from SSH key, never caller labels. Hetzner sees message plaintext under this deliberately small design; protect account, storage, diagnostics & encrypted backups accordingly. Do not call hop-encrypted SSH end-to-end encryption. Laptop adapter enforces permitted targets & normal harness permissions; messages cannot approve native permission prompts.

Persist outbound message before network, incoming job before acknowledgement, reply route before native dispatch & receipts before upload. Retries preserve ID; ambiguous native outcomes remain visible unknown. Phone conversation merges retained messages/receipts on foreground refresh. Server restart/restore, laptop sleep & phone termination preserve recovery path. Version protocol & publish truthful stored/held/queued/sent/refused/unknown states.

Nearby hub lists observed devices with default Deny & deliberate pairing. Fix TLS possession checks first; verified Pulse authentication gates Allow. Ask supports files/text equally, with content hidden until consent. One hub-owned policy module enforces all paths; Mac & Windows show same effective decisions. Revocation cancels pending/active grants; silent Allow means passive bounded receipt, never silent execution or clipboard writes.

**First shippable slice:** phone foreground send → real Claude/Codex chat on Mac or Dell → explicit reply → phone conversation, including destination sleep, disconnect & relaunch recovery. Ship no background-push promise, terminal administration, internet files or LocalSend replacement in this slice. Add terminal profile next, actual waiting events/APNs next, durable internet file transport last. Each addition extends already working owner journey.

## 5 Build order

1. **Freeze honest v3 contract.** Remove superseded HTTPS/E2E/no-SSH sections; adopt trusted Hetzner & outbound mailbox extension. Define principal grants, phone return route, fixed RPC schema, receipt meanings, limits, retention & restore behavior. Preserve `sent`; distinguish relay storage from native acceptance.
2. **Close Nearby authorization holes before enabling Allow.** Fix TLS signatures/downgrade; implement authenticated Pulse peer requests, explicit pairing, core-owned enforcement with hub as sole writer, bounded discovery & immediate revocation. Remove automatic text consent/trust promotion; fix premature text success. Deliver hub settings plus Mac/Windows cards together.
3. **Build bridge mailbox core & headless host.** Transactional durable jobs/dedupe/receipts, roster export, phone endpoint, both adapters' reply routes, exact endpoint validation & local uncertain-dispatch recovery. Separate bridge feature/lifecycle from LocalSend (`core/src/lib.rs:9`). Package headless Linux service with restricted SSH entrypoint, quotas, pinned releases, backup/restore & redacted health. No laptop ingress credential.
4. **Build own phone app against that contract.** Enclave SSH signer, host pin enrollment, local outbox/inbox, foreground lock lifecycle, scoped chat picker, send/reply & truthful receipts. Mac & Windows outbound workers restart independently of Nearby. Complete first-slice journey before adding terminal or push surfaces.
5. **Qualify substantial end-to-end journeys through generated RightKit workflows & installed devices.** Messaging journey covers both harnesses/laptops, adversarial command strings, malformed/truncated output, stolen/revoked credential, offline destination, termination around acceptance/native dispatch, retained state after restart/restore & unknown-outcome recovery. Nearby journey covers spoofed fingerprint/certificate, discovery→pair→Allow, Deny/Ask files/text, refusal on sender, reinstall, revocation during transfer, locked phone, stale notch snapshot & both platform cards. Observe rendered results, saved content & absence of unauthorized effects; helper passes are not substitutes.
6. **Extend proven path.** Separate terminal admin profile with escape/clipboard/paste protections; real waiting events & generic APNs hints; then remote file journal/resume/integrity/quota/consent. Retire LocalSend only after replacement journeys cover every intended direction & retained state.

Static inventory at review: **397 declared tests** — 396 existing component/check declarations (core 268, Windows 91, Mac 7, scripts 30; `scripts/qa/test-inventory.mjs`) plus one existing UI harness test (`hub/qa-e2e/tests/ui.rs:2145`) composing hub tour & platform notch journey. Runtime counts depend on platform/configuration. This review ran zero tests, added/deleted zero tests & claims zero executed phone/relay/trust journeys. Existing inventory is retained; build acceptance is completed owner journeys with recovery evidence.
