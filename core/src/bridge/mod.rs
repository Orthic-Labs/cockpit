//! Pulse bridge: the cross-machine part of AI chats messaging each other.
//! Chats on one computer already talk natively (Claude to Claude over their
//! sockets, Claude to Codex with `codex queue`). Pulse pairs computers, relays
//! signed messages between them and delivers natively on the receiving side.
//!
//! Pieces
//! * `envelope`: the signed JSON message (`HMAC-SHA256` with a per-pair key).
//! * `store`: inboxes, outbox, roster cache, relay heartbeat.
//! * `roster`: which chats exist (`LocalSession`, `Peer`).
//! * `control`: how the CLI asks the running hub to pair (files in the state folder).
//! * `relay`: what a host with a running `localsend::Service` (the hub) calls: `tick` sends the outbox and publishes the
//!   roster, `on_inbound` handles `Event::Bridge`, `on_local_reply` relays a
//!   chat's reply, `is_known_local_session`, `status`.
//! * `deliver_claude`, `deliver_codex`: native delivery into a chat here.
//! * `install`: puts the Pulse bridge skill where Claude and Codex load it.
//!
//! The agent-facing surface is the CLI (`pulse bridge peers|send|pair|status`),
//! built on `all_peers`, `identify` and `send_text`. `Receipt` carries
//! `ReceiptState::{Delivered, Held, Refused}`; `Held` and `Refused` leave the
//! message in the chat's bridge inbox (`pulse bridge inbox`).
//!
//! Hub wiring: `bridge::on_inbound(&service, env)` for `Event::Bridge(env)`
//! (off the service thread), `bridge::tick(&service)` every ~2 s,
//! `bridge::on_local_reply(&service, reply)` and
//! `bridge::is_known_local_session(id)` for `deliver_claude::ReplyHub`,
//! `bridge::status()` for the page. Pair with
//! `Service::bridge_pair(<alias or fingerprint>)` (a prompt appears on the
//! other computer); `Service::is_paired` tells who is paired.

pub mod control;
pub mod deliver_claude;
pub mod deliver_codex;
pub mod envelope;
pub mod install;
pub mod relay;
pub mod roster;
pub mod store;

pub use envelope::{Envelope, EnvelopeError, Kind, ReplayGuard, Sender, Target};
pub use relay::{Status, is_known_local_session, on_inbound, on_local_reply, status, tick};
pub use roster::{LocalSession, Peer, RosterEntry};
pub use store::Store;

use serde::Serialize;
use std::time::{Duration, Instant};

#[derive(Debug, thiserror::Error)]
pub enum BridgeError {
    /// Not possible here (for example delivery into Claude on Windows).
    #[error("{0}")]
    Unsupported(String),
    #[error("no chat matches \"{0}\"; see `pulse bridge peers`")]
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
    /// The chat's side kept it without showing it yet.
    Held,
    /// The chat's side turned it away (for example too many messages).
    Refused,
}

impl ReceiptState {
    pub fn as_str(self) -> &'static str {
        match self {
            ReceiptState::Delivered => "delivered",
            ReceiptState::Held => "held",
            ReceiptState::Refused => "refused",
        }
    }

    pub fn parse(text: &str) -> Option<ReceiptState> {
        match text {
            "delivered" => Some(ReceiptState::Delivered),
            "held" => Some(ReceiptState::Held),
            "refused" => Some(ReceiptState::Refused),
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

    /// The chat can't be pushed to (other protocol, no Codex CLI, Windows):
    /// the message stays pending in the bridge inbox, so it counts as held.
    pub fn unsupported(detail: impl Into<String>) -> Receipt {
        Receipt::with(ReceiptState::Held, detail)
    }
}

/// This computer as the bridge knows it.
#[derive(Debug, Clone)]
pub struct Identity {
    /// Certificate fingerprint, the `device` in envelopes.
    pub device: String,
    /// How other computers list this one.
    pub alias: String,
}

fn default_alias() -> String {
    let host = sysinfo::System::host_name().unwrap_or_else(|| "Computer".to_string());
    format!("{host} (Pulse)")
}

/// This computer's bridge identity: what the running relay recorded, else the
/// sharing certificate in the state directory (created if absent, as the hub
/// would) and a name made from the host name.
pub fn local_identity(store: &Store) -> Result<Identity, BridgeError> {
    if let Some(status) = store.relay_status()
        && !status.device.is_empty()
    {
        return Ok(Identity {
            device: status.device,
            alias: status.alias,
        });
    }
    let identity = crate::localsend::net::Identity::load_or_create(&store.localsend_dir())?;
    Ok(Identity {
        device: identity.fingerprint,
        alias: default_alias(),
    })
}

/// Every chat that can be messaged: this computer's and the paired ones'.
pub fn all_peers(store: &Store, me: &Identity) -> Vec<Peer> {
    roster::merge(
        &roster::local_sessions(store),
        &me.device,
        &me.alias,
        &store.remote_rosters(),
        envelope::now_ms(),
    )
}

/// Put `env` into a chat on this computer. Claude and Codex chats are tried
/// directly first (`deliver_claude::deliver`, `deliver_codex::deliver`);
/// anything not `Delivered` keeps the message in the chat's inbox for
/// `pulse bridge inbox`.
pub fn deliver_local(store: &Store, session: &LocalSession, env: &Envelope) -> Receipt {
    deliver_local_via(store, session, env, None)
}

/// `deliver_local`, with `reply_socket` (the sending Claude chat's own
/// messaging socket) as the address a Claude target replies to.
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
    let pushed = match session.kind.as_str() {
        "claude" => deliver_claude::deliver_via(session, env, reply_socket),
        "codex" => deliver_codex::deliver(session, env),
        other => Err(BridgeError::Unsupported(format!(
            "can't push into a {other} chat; it reads its bridge inbox"
        ))),
    };
    let (state, detail) = match pushed {
        Ok(r) if r.state == ReceiptState::Delivered => {
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
        Ok(_) => receipt(state, format!("{detail} (kept in the bridge inbox)")),
        Err(e) => receipt(ReceiptState::Refused, format!("couldn't store it: {e}")),
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct SendOutcome {
    pub msg_id: String,
    pub to: Peer,
    /// delivered, held, refused (as above), or queued / sent / failed for a
    /// message going to another computer.
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
        return hit
            .map(caller_of)
            .ok_or_else(|| BridgeError::NotFound(wanted));
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

/// Send `text` from `from` to the chat `to` names. A chat on this computer
/// gets it at once, natively; a chat on a paired computer goes through the
/// outbox, and this waits up to `wait` for the relay to send it.
pub fn send_text(
    store: &Store,
    me: &Identity,
    from: &Caller,
    to: &str,
    text: &str,
    wait: Duration,
) -> Result<SendOutcome, BridgeError> {
    let peers = all_peers(store, me);
    let peer = roster::resolve(&peers, to)?;
    if peer.local && peer.session == from.id {
        return Err(BridgeError::Invalid(
            "that is this chat; pick another one".to_string(),
        ));
    }
    let env = Envelope::new(
        Sender {
            device: me.device.clone(),
            session: from.id.clone(),
            name: format!("{} on {}", from.name, me.alias),
        },
        Target {
            device: peer.device.clone(),
            session: peer.session.clone(),
        },
        Kind::Message,
        text,
    )?;
    if peer.local {
        let sessions = roster::local_sessions(store);
        let session = sessions
            .iter()
            .find(|s| s.id == peer.session)
            .ok_or_else(|| BridgeError::NotFound(to.to_string()))?;
        let receipt = deliver_local_via(store, session, &env, from.reply_socket.as_deref());
        return Ok(SendOutcome {
            msg_id: env.id,
            to: peer,
            status: receipt.state.as_str().to_string(),
            detail: receipt.detail,
        });
    }
    store.enqueue_outbox(&env, &peer.device)?;
    if !store.relay_alive() {
        return Ok(SendOutcome {
            msg_id: env.id,
            to: peer,
            status: "queued".to_string(),
            detail: "No Pulse relay is running (open Pulse); it is sent when one starts.".to_string(),
        });
    }
    let started = Instant::now();
    loop {
        if let Some(item) = store.outbox_get(&env.id)
            && item.status != "queued"
        {
            return Ok(SendOutcome {
                msg_id: env.id,
                to: peer,
                status: item.status,
                detail: item.detail.unwrap_or_default(),
            });
        }
        if started.elapsed() >= wait {
            return Ok(SendOutcome {
                msg_id: env.id,
                to: peer,
                status: "queued".to_string(),
                detail: "The relay has not sent it yet.".to_string(),
            });
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}
