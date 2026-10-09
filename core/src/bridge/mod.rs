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
//! built on `all_peers`, `identify` and `send_text`. `Receipt` carries
//! `ReceiptState::{Delivered, Held, Refused}`; `Held` and `Refused` leave the
//! message in the chat's bridge inbox (`pulse bridge inbox`).

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

use serde::Serialize;
use serde_json::json;
use std::time::Duration;

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

/// This computer as the bridge knows it: the name chats are shown under.
#[derive(Debug, Clone)]
pub struct Identity {
    pub alias: String,
}

fn default_alias() -> String {
    let host = sysinfo::System::host_name().unwrap_or_else(|| "Computer".to_string());
    host.trim_end_matches(".local").to_string()
}

/// This computer's name: what the running hub recorded, else the host name.
pub fn local_identity(store: &Store) -> Result<Identity, BridgeError> {
    let alias = store
        .relay_status()
        .map(|s| s.alias)
        .filter(|a| !a.is_empty())
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

/// Put `env` into a chat on this computer from this process: Claude and Codex
/// chats are pushed directly (`deliver_claude::deliver_via`,
/// `deliver_codex::deliver`); anything not `Delivered` keeps the message in
/// the chat's inbox for `pulse bridge inbox`. `reply_socket` (the sending
/// Claude chat's own messaging socket) is where a Claude target replies.
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
    /// delivered, held or refused (as above), from whichever computer the chat is on.
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
            .ok_or(BridgeError::NotFound(wanted));
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
    !cfg!(unix)
        || std::env::var("CLAUDE_CODE_MESSAGING_SOCKET").is_ok_and(|v| !v.trim().is_empty())
}

/// Put `env` into a chat on this computer the way that works here: through
/// the running hub (a registered peer) when a direct post would be dropped,
/// else directly. `reply_socket` as in `deliver_local_via`.
pub fn deliver_here(
    store: &Store,
    session: &LocalSession,
    env: &Envelope,
    reply_socket: Option<&str>,
) -> Receipt {
    let needs_hub = session.kind == "claude" && !direct_post_trusted();
    if needs_hub && control::hub_running(store) {
        let mut args = json!({"session": session.id, "envelope": links::encode_envelope(env)});
        if let Some(socket) = reply_socket {
            args["reply_socket"] = json!(socket);
        }
        return match control::call(store, "deliver", args, Duration::from_secs(20)) {
            Ok(reply) if reply["ok"].as_bool() == Some(true) => Receipt {
                msg_id: env.id.clone(),
                session: session.id.clone(),
                state: reply["status"]
                    .as_str()
                    .and_then(ReceiptState::parse)
                    .unwrap_or(ReceiptState::Held),
                detail: reply["detail"].as_str().unwrap_or("").to_string(),
            },
            Ok(reply) => Receipt {
                msg_id: env.id.clone(),
                session: session.id.clone(),
                state: ReceiptState::Held,
                detail: reply["error"].as_str().unwrap_or("Pulse refused").to_string(),
            },
            Err(e) => Receipt {
                msg_id: env.id.clone(),
                session: session.id.clone(),
                state: ReceiptState::Held,
                detail: e,
            },
        };
    }
    if needs_hub {
        let _ = store.append_inbox(&session.id, env);
        return Receipt {
            msg_id: env.id.clone(),
            session: session.id.clone(),
            state: ReceiptState::Held,
            detail: "Pulse isn't running here; Claude only takes messages posted by it. Kept in the bridge inbox."
                .to_string(),
        };
    }
    deliver_local_via(store, session, env, reply_socket)
}

/// Send `text` from `from` to the chat `to` names. A chat on this computer
/// gets it at once; a chat on a linked computer gets it over ssh, and this
/// waits for that computer's receipt.
pub fn send_text(
    store: &Store,
    me: &Identity,
    from: &Caller,
    to: &str,
    text: &str,
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
        let sessions = roster::local_sessions(store);
        let session = sessions
            .iter()
            .find(|s| s.id == peer.session)
            .ok_or_else(|| BridgeError::NotFound(to.to_string()))?;
        let receipt = deliver_here(store, session, &env, from.reply_socket.as_deref());
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
    Ok(SendOutcome {
        msg_id: env.id,
        to: peer,
        status: receipt["status"].as_str().unwrap_or("held").to_string(),
        detail: receipt["detail"].as_str().unwrap_or("").to_string(),
    })
}

/// A linked computer posted an envelope here (`pulse bridge post`): put it
/// into the chat it names.
pub fn receive(store: &Store, env: &Envelope) -> Receipt {
    let sessions = roster::local_sessions(store);
    match sessions.iter().find(|s| s.id == env.to.session) {
        Some(session) => deliver_here(store, session, env, None),
        None => Receipt {
            msg_id: env.id.clone(),
            session: env.to.session.clone(),
            state: ReceiptState::Refused,
            detail: "That chat isn't open on this computer any more.".to_string(),
        },
    }
}
