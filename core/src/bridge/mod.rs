//! Pulse bridge: AI chats (Claude Desktop's Code tab, Codex) on this computer
//! and on paired computers message each other through Pulse.
//!
//! Pieces
//! * `envelope`: the signed JSON message (`HMAC-SHA256` with a per-pair key).
//! * `store`: inboxes, outbox, roster cache, registered chats, relay heartbeat.
//! * `roster`: which chats exist (`LocalSession`, `Peer`).
//! * `relay`: what a host with a running `localsend::Service` calls (the hub,
//!   or `pulse bridge daemon`): `tick` sends the outbox and publishes the
//!   roster, `on_inbound` handles `Event::Bridge`, `on_local_reply` relays a
//!   chat's reply, `is_known_local_session`, `status`.
//! * `mcp`: `pulse bridge mcp`, the stdio MCP server chats talk to.
//! * `deliver_claude`, `deliver_codex`, `install`: owned by another agent.
//!
//! API for the delivery and install code (all re-exported here)
//! * `Envelope`, `Sender`, `Target`, `Kind`: `body` is the plain text to show;
//!   `from.name` is "<chat title> on <device alias>"; `sig` is empty for an
//!   envelope that never left this computer.
//! * `LocalSession { id, kind, name, cwd, status, pid, messaging_socket,
//!   peer_protocol, entrypoint, raw }`: the receiving chat.
//! * `BridgeError`: use `Unsupported` for "not on this platform" (Windows).
//! * `Receipt { msg_id, session, state, detail }` with
//!   `ReceiptState::{Delivered, Held, Refused}`, the names Claude's
//!   cross-session messaging uses. `Held` and `Refused` leave the message in
//!   the chat's bridge inbox (read with the `bridge_inbox` tool).
//! * `deliver_claude::deliver(session: &LocalSession, env: &Envelope) ->
//!   Result<Receipt, BridgeError>` is called for every message to a local
//!   Claude chat (directly, or relayed from another computer). An `Err` is
//!   treated as `Held` with the error text.
//! * `local_identity(&Store)`, `all_peers(&Store, &Identity)`,
//!   `send_text(...)`, `deliver_local(...)` for hosts that send.
//! * Hub wiring: `bridge::on_inbound(&service, env)` for `Event::Bridge(env)`
//!   (off the service thread), `bridge::tick(&service)` every ~2 s,
//!   `bridge::on_local_reply(&service, reply)` and
//!   `bridge::is_known_local_session(id)` for `deliver_claude::ReplyHub`,
//!   `bridge::status()` for the page. Pair with
//!   `Service::bridge_pair(<alias or fingerprint>)` (a prompt appears on the
//!   other computer); `Service::is_paired` tells who is paired.

pub mod deliver_claude;
pub mod deliver_codex;
pub mod envelope;
pub mod install;
pub mod mcp;
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
    #[error("no chat matches \"{0}\"; see bridge_list")]
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
/// `bridge_inbox`.
pub fn deliver_local(store: &Store, session: &LocalSession, env: &Envelope) -> Receipt {
    let receipt = |state: ReceiptState, detail: String| Receipt {
        msg_id: env.id.clone(),
        session: session.id.clone(),
        state,
        detail,
    };
    let pushed = match session.kind.as_str() {
        "claude" => deliver_claude::deliver(session, env),
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

/// Send `text` from chat `from_session` (shown as `from_title`) to the chat
/// `to` names. Local chats get it at once; a chat on a paired computer goes
/// through the outbox, and this waits up to `wait` for the relay to send it.
pub fn send_text(
    store: &Store,
    me: &Identity,
    from_session: &str,
    from_title: &str,
    to: &str,
    text: &str,
    wait: Duration,
) -> Result<SendOutcome, BridgeError> {
    let peers = all_peers(store, me);
    let peer = roster::resolve(&peers, to)?;
    let env = Envelope::new(
        Sender {
            device: me.device.clone(),
            session: from_session.to_string(),
            name: format!("{from_title} on {}", me.alias),
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
        let receipt = deliver_local(store, session, &env);
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
            detail: "No Pulse relay is running (open the Pulse hub or run `pulse bridge daemon`); it is sent when one starts.".to_string(),
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
