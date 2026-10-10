//! Pulse bridge: the cross-machine part of AI chats messaging each other.
//! Chats on one computer already talk natively (Claude to Claude over their
//! sockets, Claude to Codex with `codex queue`). Pulse links computers over
//! ssh and delivers natively on the receiving side.
//!
//! Pieces
//! * `envelope`: the JSON message (`from`, `to`, `body`).
//! * `links`: linked computers (`links.json`) and the two remote commands run
//!   over ssh: list chats, post one envelope.
//! * `store`: inboxes, links, registered chats, the hub's heartbeat.
//! * `roster`: which chats exist (`LocalSession`, `Peer`).
//! * `control`: how the CLI asks the running hub to deliver (files in the state folder).
//! * `hub`: what the hub runs: registration as a Claude peer, heartbeat,
//!   replies routed back over links, control requests, status.
//! * `deliver_claude`, `deliver_codex`: native delivery into a chat here.
//! * `install`: puts the Pulse bridge skill where Claude and Codex load it.
//!
//! The agent-facing surface is the CLI (`pulse bridge peers|send|link|status`),
//! built on `all_peers`, `identify` and `send_text`. `Receipt` carries a
//! `ReceiptState`: `Delivered` (the chat has it), `Queued` (the chat's own
//! queue has it), `Sent` (posted, not confirmed), `Held` (stored in the chat's
//! bridge inbox, `pulse bridge inbox`; returned only after that write was
//! verified), `Refused`, `Unsupported` (this chat can't be pushed to) and
//! `Unknown` (the outcome could not be established).
//!
//! The bridge switch: when the bridge is off on a computer (`Store::bridge_enabled`),
//! `send_text`, `receive`, `deliver_here` and the hub's control and reply paths
//! return `Refused` ("Chat is off on <device>") and keep no inbox copy.

pub mod control;
pub mod deliver_claude;
pub mod deliver_codex;
pub mod envelope;
pub mod hub;
pub mod install;
pub mod links;
pub mod roster;
pub mod store;

pub use envelope::{Envelope, EnvelopeError, Sender, Target};
pub use roster::{LocalSession, Peer, RosterEntry};
pub use store::Store;

use deliver_codex::DiscoveryError;
use serde::Serialize;
use serde_json::json;
use std::time::Duration;

#[derive(Debug, thiserror::Error)]
pub enum BridgeError {
    /// Not possible here (for example delivery into Claude on Windows).
    #[error("{0}")]
    Unsupported(String),
    #[error("no chat matches \"{0}\"; see `pulse chat peers`")]
    NotFound(String),
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    Io(String),
    #[error("{0}")]
    Delivery(String),
    /// The chat closed or its files are gone.
    #[error("{0}")]
    SessionGone(String),
    /// The chat turned the connection away.
    #[error("{0}")]
    AuthRejected(String),
}

impl From<std::io::Error> for BridgeError {
    fn from(error: std::io::Error) -> Self {
        BridgeError::Io(error.to_string())
    }
}

impl From<EnvelopeError> for BridgeError {
    fn from(error: EnvelopeError) -> Self {
        BridgeError::Invalid(error.to_string())
    }
}

/// How a delivery ended, in the words Claude's cross-session messaging uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ReceiptState {
    /// The chat has the message.
    Delivered,
    /// The chat's own queue has it (it shows it at its next turn).
    Queued,
    /// Posted to the chat; receipt not confirmed.
    Sent,
    /// Stored in the chat's bridge inbox, not shown yet.
    Held,
    /// The chat's side turned it away (for example too many messages).
    Refused,
    /// The chat can't be pushed to (other protocol, no Codex CLI, Windows).
    Unsupported,
    /// The outcome could not be established; it may or may not have arrived.
    Unknown,
}

impl ReceiptState {
    pub fn as_str(self) -> &'static str {
        match self {
            ReceiptState::Delivered => "delivered",
            ReceiptState::Queued => "queued",
            ReceiptState::Sent => "sent",
            ReceiptState::Held => "held",
            ReceiptState::Refused => "refused",
            ReceiptState::Unsupported => "unsupported",
            ReceiptState::Unknown => "unknown",
        }
    }

    pub fn parse(text: &str) -> Option<ReceiptState> {
        match text {
            "delivered" => Some(ReceiptState::Delivered),
            "queued" => Some(ReceiptState::Queued),
            "sent" => Some(ReceiptState::Sent),
            "held" => Some(ReceiptState::Held),
            "refused" => Some(ReceiptState::Refused),
            "unsupported" => Some(ReceiptState::Unsupported),
            "unknown" => Some(ReceiptState::Unknown),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Receipt {
    pub msg_id: String,
    /// The receiving chat.
    pub session: String,
    pub state: ReceiptState,
    pub detail: String,
}

impl Receipt {
    fn with(state: ReceiptState, detail: impl Into<String>) -> Receipt {
        Receipt {
            msg_id: String::new(),
            session: String::new(),
            state,
            detail: detail.into(),
        }
    }

    pub fn delivered(detail: impl Into<String>) -> Receipt {
        Receipt::with(ReceiptState::Delivered, detail)
    }

    pub fn held(detail: impl Into<String>) -> Receipt {
        Receipt::with(ReceiptState::Held, detail)
    }

    pub fn refused(detail: impl Into<String>) -> Receipt {
        Receipt::with(ReceiptState::Refused, detail)
    }

    pub fn queued(detail: impl Into<String>) -> Receipt {
        Receipt::with(ReceiptState::Queued, detail)
    }

    pub fn sent(detail: impl Into<String>) -> Receipt {
        Receipt::with(ReceiptState::Sent, detail)
    }

    pub fn unknown(detail: impl Into<String>) -> Receipt {
        Receipt::with(ReceiptState::Unknown, detail)
    }

    /// The chat can't be pushed to (other protocol, no Codex CLI, Windows). The
    /// delivery path then stores the message in the bridge inbox and reports `Held`.
    pub fn unsupported(detail: impl Into<String>) -> Receipt {
        Receipt::with(ReceiptState::Unsupported, detail)
    }
}

/// This computer as the bridge knows it: the name chats are shown under.
#[derive(Debug, Clone)]
pub struct Identity {
    pub alias: String,
}

fn default_alias() -> String {
    let host = sysinfo::System::host_name().unwrap_or_else(|| "Computer".to_string());
    clean_alias(host.trim_end_matches(".local"))
}

/// A computer name safe to use in `<device>:<session>` ids: no `:`.
pub fn clean_alias(alias: &str) -> String {
    let cleaned: String = alias
        .chars()
        .map(|c| if c == ':' || c.is_control() { '-' } else { c })
        .collect();
    let cleaned = cleaned.trim().to_string();
    if cleaned.is_empty() {
        "Computer".to_string()
    } else {
        cleaned
    }
}

/// This computer's name: what the running hub recorded, else the host name.
pub fn local_identity(store: &Store) -> Result<Identity, BridgeError> {
    let alias = store
        .relay_status()
        .map(|s| s.alias)
        .filter(|a| !a.is_empty())
        .map(|a| clean_alias(&a))
        .unwrap_or_else(default_alias);
    Ok(Identity { alias })
}

/// Every chat that can be messaged: this computer's and the linked ones'
/// (asked over ssh, in parallel).
pub fn all_peers(store: &Store, me: &Identity) -> Vec<Peer> {
    roster::merge(
        &roster::local_sessions(store),
        &me.alias,
        &links::list_all(store),
    )
}

/// This computer's chats only (what a linked computer asks for).
pub fn local_peers(store: &Store, me: &Identity) -> Vec<Peer> {
    roster::merge(&roster::local_sessions(store), &me.alias, &[])
}

/// The refusal every entry point gives while the bridge is off here.
fn bridge_off(store: &Store, msg_id: &str, session: &str) -> Option<Receipt> {
    if store.bridge_enabled() {
        return None;
    }
    let device = local_identity(store)
        .map(|me| me.alias)
        .unwrap_or_else(|_| default_alias());
    Some(Receipt {
        msg_id: msg_id.to_string(),
        session: session.to_string(),
        ..Receipt::refused(format!("Chat is off on {device}"))
    })
}

/// `bridge_off` for the hub (`hub::handle_control`, `hub::on_local_reply`).
pub(crate) fn bridge_off_detail(store: &Store) -> Option<String> {
    bridge_off(store, "", "").map(|r| r.detail)
}

/// Put `env` into a chat on this computer from this process: Claude and Codex
/// chats are pushed directly (`deliver_claude::deliver_via`,
/// `deliver_codex::deliver`); anything not delivered keeps the message in the
/// chat's inbox for `pulse bridge inbox`, and says `Held` only once that write
/// succeeded. `reply_socket` (the sending Claude chat's own messaging socket)
/// is where a Claude target replies; without one the hub's reply listener is
/// used, and the route back is saved for it.
pub fn deliver_local_via(
    store: &Store,
    session: &LocalSession,
    env: &Envelope,
    reply_socket: Option<&str>,
) -> Receipt {
    let receipt = |state: ReceiptState, detail: String| Receipt {
        msg_id: env.id.clone(),
        session: session.id.clone(),
        state,
        detail,
    };
    if session.kind == "claude" && reply_socket.is_none() {
        let _ = store.save_reply_route(&store::ReplyRoute {
            msg_id: env.id.clone(),
            from_device: env.from.device.clone(),
            from_session: env.from.session.clone(),
            to_session: session.id.clone(),
            created_ms: envelope::now_ms(),
        });
    }
    let pushed = match session.kind.as_str() {
        "claude" => deliver_claude::deliver_via(session, env, reply_socket),
        "codex" => deliver_codex::deliver(session, env),
        other => Err(BridgeError::Unsupported(format!(
            "can't push into a {other} chat; it reads its chat inbox"
        ))),
    };
    let (state, detail) = match pushed {
        Ok(r) if reached_chat(r.state) => {
            return Receipt {
                msg_id: env.id.clone(),
                session: session.id.clone(),
                ..r
            };
        }
        Ok(r) => (r.state, r.detail),
        Err(e) => (ReceiptState::Held, e.to_string()),
    };
    match store.append_inbox(&session.id, env) {
        Ok(_) => {
            let state = match state {
                ReceiptState::Refused | ReceiptState::Unknown => state,
                _ => ReceiptState::Held,
            };
            receipt(state, format!("{detail} (kept in the chat inbox)"))
        }
        Err(e) => receipt(
            ReceiptState::Unknown,
            format!("could not store the message: {e} ({detail})"),
        ),
    }
}

fn reached_chat(state: ReceiptState) -> bool {
    matches!(
        state,
        ReceiptState::Delivered | ReceiptState::Queued | ReceiptState::Sent
    )
}

#[derive(Debug, Clone, Serialize)]
pub struct SendOutcome {
    pub msg_id: String,
    pub to: Peer,
    /// A `ReceiptState` word, from whichever computer the chat is on.
    pub status: String,
    pub detail: String,
}

/// The chat a bridge command runs in: who a message is from and where a
/// native reply should land.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Caller {
    pub id: String,
    pub name: String,
    /// The calling Claude chat's own messaging socket.
    pub reply_socket: Option<String>,
}

/// The chat id and name used outside any chat (a plain terminal).
pub const CLI_CALLER_ID: &str = "cli";

/// Work out the calling chat. `from` (`--from`, a chat id or title on this
/// computer) wins; then the Claude chat owning `CLAUDE_CODE_MESSAGING_SOCKET`;
/// then `CODEX_THREAD_ID`; then `CLAUDE_SESSION_ID`; else a plain terminal.
pub fn identify(
    sessions: &[LocalSession],
    env: &dyn Fn(&str) -> Option<String>,
    from: Option<&str>,
) -> Result<Caller, BridgeError> {
    let var = |name: &str| env(name).filter(|v| !v.trim().is_empty());
    let caller_of = |s: &LocalSession| Caller {
        id: s.id.clone(),
        name: s.name.clone(),
        reply_socket: if s.kind == "claude" {
            s.messaging_socket.clone()
        } else {
            None
        },
    };
    if let Some(wanted) = from {
        let wanted = wanted.trim().to_lowercase();
        let named: Vec<&LocalSession> = sessions
            .iter()
            .filter(|s| s.name.to_lowercase() == wanted)
            .collect();
        let hit = sessions
            .iter()
            .find(|s| s.id.to_lowercase() == wanted)
            .or(if named.len() == 1 {
                Some(named[0])
            } else {
                None
            });
        return hit.map(caller_of).ok_or(BridgeError::NotFound(wanted));
    }
    let claude_id = var("CLAUDE_SESSION_ID").or_else(|| var("CLAUDE_CODE_SESSION_ID"));
    if let Some(socket) = var("CLAUDE_CODE_MESSAGING_SOCKET") {
        if let Some(s) = sessions
            .iter()
            .find(|s| s.messaging_socket.as_deref() == Some(socket.as_str()))
        {
            return Ok(caller_of(s));
        }
        if let Some(id) = claude_id.clone() {
            return Ok(Caller {
                id,
                name: "Claude chat".to_string(),
                reply_socket: Some(socket),
            });
        }
    }
    if let Some(id) = var("CODEX_THREAD_ID") {
        return Ok(match sessions.iter().find(|s| s.id == id) {
            Some(s) => caller_of(s),
            None => Caller {
                id,
                name: "Codex chat".to_string(),
                reply_socket: None,
            },
        });
    }
    if let Some(id) = claude_id
        && let Some(s) = sessions.iter().find(|s| s.id == id)
    {
        return Ok(caller_of(s));
    }
    Ok(Caller {
        id: CLI_CALLER_ID.to_string(),
        name: "Pulse CLI".to_string(),
        reply_socket: None,
    })
}

/// Whether a direct post from this process reaches a Claude chat here. On
/// macOS a chat accepts a peer message only from a process descending from a
/// registered session: a chat's own shell is one; `pulse` run by sshd is not.
fn direct_post_trusted() -> bool {
    !cfg!(unix) || std::env::var("CLAUDE_CODE_MESSAGING_SOCKET").is_ok_and(|v| !v.trim().is_empty())
}

/// Put `env` into a chat on this computer the way that works here: through
/// the running hub (a registered peer) when a direct post would be dropped,
/// else directly. `reply_socket` as in `deliver_local_via`. Refused while the
/// bridge is off.
pub fn deliver_here(
    store: &Store,
    session: &LocalSession,
    env: &Envelope,
    reply_socket: Option<&str>,
) -> Receipt {
    if let Some(off) = bridge_off(store, &env.id, &session.id) {
        return off;
    }
    let needs_hub = session.kind == "claude" && !direct_post_trusted();
    // The hub owns the reply listener on both platforms: a delivery it posts advertises a
    // real reply address, one the CLI posts only a placeholder. So the hub is preferred
    // whenever it runs; the direct post stays for when it does not (and is trusted).
    let prefer_hub = session.kind == "claude" && control::hub_running(store);
    if (needs_hub || prefer_hub) && control::hub_running(store) {
        let mut args = json!({"session": session.id, "envelope": links::encode_envelope(env)});
        if let Some(socket) = reply_socket {
            args["reply_socket"] = json!(socket);
        }
        let result = |state: ReceiptState, detail: String| Receipt {
            msg_id: env.id.clone(),
            session: session.id.clone(),
            state,
            detail,
        };
        return match control::call(store, "deliver", args, Duration::from_secs(20)) {
            Ok(reply) if reply["ok"].as_bool() == Some(true) => result(
                reply["status"]
                    .as_str()
                    .and_then(ReceiptState::parse)
                    .unwrap_or(ReceiptState::Unknown),
                reply["detail"].as_str().unwrap_or("").to_string(),
            ),
            Ok(reply) => result(
                ReceiptState::Unknown,
                reply["error"]
                    .as_str()
                    .unwrap_or("Pulse refused")
                    .to_string(),
            ),
            Err(e) => result(ReceiptState::Unknown, e),
        };
    }
    if needs_hub {
        return match store.append_inbox(&session.id, env) {
            Ok(_) => Receipt {
                msg_id: env.id.clone(),
                session: session.id.clone(),
                state: ReceiptState::Held,
                detail: "Pulse isn't running here; Claude only takes messages posted by it. Kept in the chat inbox."
                    .to_string(),
            },
            Err(e) => Receipt {
                msg_id: env.id.clone(),
                session: session.id.clone(),
                state: ReceiptState::Unknown,
                detail: format!("could not store the message: {e}"),
            },
        };
    }
    deliver_local_via(store, session, env, reply_socket)
}

/// Whether `text` names a chat exactly: `<kind>:<id>` or a bare 36-character uuid.
fn exact_local_id(text: &str) -> Option<&str> {
    let text = text.trim();
    if let Some((kind, id)) = text.split_once(':')
        && matches!(kind, "claude" | "codex")
        && !id.is_empty()
    {
        return Some(id);
    }
    let uuid_shaped = text.len() == 36
        && text.char_indices().all(|(i, c)| {
            if matches!(i, 8 | 13 | 18 | 23) {
                c == '-'
            } else {
                c.is_ascii_hexdigit()
            }
        });
    uuid_shaped.then_some(text)
}

/// The chat `to` names, with its local session when it is on this computer.
/// Exact ids are resolved without asking any link; `<device>:<chat>` and
/// "<chat> on <device>" ask only that device's link; everything else (fuzzy
/// titles) falls back to full discovery.
fn resolve_target(
    store: &Store,
    me: &Identity,
    to: &str,
) -> Result<(Peer, Option<LocalSession>), BridgeError> {
    let local_peer = |s: &LocalSession| {
        roster::merge(std::slice::from_ref(s), &me.alias, &[])
            .into_iter()
            .next()
    };
    if let Some(id) = exact_local_id(to) {
        let mut found = roster::local_sessions(store)
            .into_iter()
            .find(|s| s.id.eq_ignore_ascii_case(id));
        if found.is_none() {
            found = match roster::resolve_exact_codex(id) {
                Ok(session) => session,
                Err(DiscoveryError::NoSource) => None,
                Err(e) => {
                    return Err(BridgeError::Delivery(format!(
                        "couldn't look up the Codex chat \"{id}\": {e}"
                    )));
                }
            };
        }
        if let Some(session) = found
            && let Some(peer) = local_peer(&session)
        {
            return Ok((peer, Some(session)));
        }
    }
    let mut devices: Vec<String> = to
        .match_indices(':')
        .map(|(i, _)| to[..i].to_string())
        .collect();
    if let Some((_, device)) = to.rsplit_once(" on ") {
        devices.push(device.trim().to_string());
    }
    for device in devices {
        if device.eq_ignore_ascii_case(&me.alias) {
            continue;
        }
        let Some(link) = links::find(store, &device) else {
            continue;
        };
        let listing = links::list_chats(&link);
        if let Err(e) = &listing
            && to.contains(':')
            && !to.contains(" on ")
        {
            return Err(BridgeError::Delivery(e.clone()));
        }
        let remote = [links::RemoteChats { link, listing }];
        let peers = roster::merge(&[], &me.alias, &remote);
        if let Ok(peer) = roster::resolve(&peers, to) {
            return Ok((peer, None));
        }
    }
    let peers = all_peers(store, me);
    let peer = roster::resolve(&peers, to)?;
    Ok((peer, None))
}

/// Send `text` from `from` to the chat `to` names. A chat on this computer
/// gets it at once; a chat on a linked computer gets it over ssh, and this
/// waits for that computer's receipt. Refused while the bridge is off here.
pub fn send_text(
    store: &Store,
    me: &Identity,
    from: &Caller,
    to: &str,
    text: &str,
) -> Result<SendOutcome, BridgeError> {
    if let Some(off) = bridge_off(store, "", "") {
        let peer = Peer {
            id: to.to_string(),
            session: to.to_string(),
            name: to.to_string(),
            display: to.to_string(),
            device: me.alias.clone(),
            device_alias: me.alias.clone(),
            local: true,
            kind: String::new(),
            cwd: String::new(),
            status: String::new(),
            updated_ms: None,
            liveness: "unknown".to_string(),
        };
        return Ok(SendOutcome {
            msg_id: String::new(),
            to: peer,
            status: off.state.as_str().to_string(),
            detail: off.detail,
        });
    }
    let (peer, known) = resolve_target(store, me, to)?;
    if peer.local && peer.session == from.id {
        return Err(BridgeError::Invalid(
            "that is this chat; pick another one".to_string(),
        ));
    }
    let env = Envelope::new(
        Sender {
            device: me.alias.clone(),
            session: from.id.clone(),
            name: format!("{} on {}", from.name, me.alias),
        },
        Target {
            device: peer.device.clone(),
            session: peer.session.clone(),
        },
        text,
    )?;
    if peer.local {
        let session = match known {
            Some(session) => session,
            None => roster::local_sessions(store)
                .into_iter()
                .find(|s| s.id == peer.session)
                .ok_or_else(|| BridgeError::NotFound(to.to_string()))?,
        };
        let receipt = deliver_here(store, &session, &env, from.reply_socket.as_deref());
        if receipt.state != ReceiptState::Refused {
            store.note_sent(receipt.state.as_str());
        }
        return Ok(SendOutcome {
            msg_id: env.id,
            to: peer,
            status: receipt.state.as_str().to_string(),
            detail: receipt.detail,
        });
    }
    let link = links::find(store, &peer.device)
        .ok_or_else(|| BridgeError::NotFound(format!("no link named {}", peer.device)))?;
    let receipt = links::post(&link, &env).map_err(BridgeError::Delivery)?;
    let status = ReceiptState::parse(receipt["status"].as_str().unwrap_or(""));
    if status != Some(ReceiptState::Refused) {
        store.note_sent(status.unwrap_or(ReceiptState::Unknown).as_str());
    }
    Ok(SendOutcome {
        msg_id: env.id,
        to: peer,
        status: status.unwrap_or(ReceiptState::Unknown).as_str().to_string(),
        detail: receipt["detail"].as_str().unwrap_or("").to_string(),
    })
}

/// Envelopes older than this are refused (a replayed or very late post).
const ENVELOPE_MAX_AGE_MS: u64 = 24 * 60 * 60 * 1000;

/// A linked computer posted an envelope here (`pulse bridge post`): put it
/// into the chat it names. Refused while the bridge is off, when the envelope
/// is older than 24 hours, and when its id was already received (the same
/// content again is a duplicate, other content under the same id a conflict).
pub fn receive(store: &Store, env: &Envelope) -> Receipt {
    let refuse = |detail: &str| Receipt {
        msg_id: env.id.clone(),
        session: env.to.session.clone(),
        ..Receipt::refused(detail)
    };
    if let Some(off) = bridge_off(store, &env.id, &env.to.session) {
        return off;
    }
    if envelope::now_ms().saturating_sub(env.ts) > ENVELOPE_MAX_AGE_MS {
        return refuse("expired");
    }
    let sessions = roster::local_sessions(store);
    let Some(session) = sessions.iter().find(|s| s.id == env.to.session) else {
        return refuse("That chat isn't open on this computer any more.");
    };
    match store.journal_seen(&env.from.device, &env.id, &store::payload_hash(env)) {
        store::Seen::New => {}
        store::Seen::Duplicate => return refuse("duplicate of a message already received"),
        store::Seen::Conflict => return refuse("message id reused with different content"),
    }
    let receipt = deliver_here(store, session, env, None);
    if receipt.state != ReceiptState::Refused {
        store.note_received(receipt.state.as_str());
    }
    receipt
}
