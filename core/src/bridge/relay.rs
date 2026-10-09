//! What a host with a running sharing `Service` calls to run the bridge: send
//! queued messages, publish the roster to paired computers, hand incoming
//! bridge messages to the right chat, and carry chats' replies back.
//! The hub calls these from its own threads.

use super::deliver_claude::ReplyMessage;
use super::envelope::{Envelope, Kind, Sender, Target, now_ms};
use super::roster::{self, LocalSession, REMOTE_MAX_AGE_MS};
use super::store::{RemoteRoster, Store};
use super::{Identity, Receipt, ReceiptState, deliver_local};
use crate::localsend::Service;
use serde::Serialize;
use serde_json::json;
use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

const ROSTER_REFRESH: Duration = Duration::from_secs(30);
const SESSION_SCAN_EVERY: Duration = Duration::from_secs(5);
/// A queued message whose computer is not nearby is kept this long.
const QUEUE_PATIENCE_MS: u64 = 120_000;

#[derive(Default)]
struct State {
    last_error: Option<String>,
    sessions: Vec<LocalSession>,
    scanned: Option<Instant>,
    /// Roster body and time last sent to each device.
    published: HashMap<String, (String, Instant)>,
    ticks: u64,
}

fn state() -> MutexGuard<'static, State> {
    static STATE: OnceLock<Mutex<State>> = OnceLock::new();
    STATE
        .get_or_init(|| Mutex::new(State::default()))
        .lock()
        .unwrap_or_else(|p| p.into_inner())
}

fn store() -> Option<&'static Store> {
    static STORE: OnceLock<Option<Store>> = OnceLock::new();
    STORE.get_or_init(|| Store::open_default().ok()).as_ref()
}

fn note(error: String) {
    state().last_error = Some(error);
}

/// This computer's chats, rescanned at most every few seconds.
fn local_chats(store: &Store, max_age: Duration) -> Vec<LocalSession> {
    {
        let state = state();
        if let Some(at) = state.scanned
            && at.elapsed() < max_age
        {
            return state.sessions.clone();
        }
    }
    let sessions = roster::local_sessions(store);
    let mut state = state();
    state.sessions = sessions.clone();
    state.scanned = Some(Instant::now());
    sessions
}

fn identity(service: &Service) -> Identity {
    Identity {
        device: service.fingerprint(),
        alias: service.alias(),
    }
}

/// Whether `id` is a chat on this computer (for `ReplyHub`).
pub fn is_known_local_session(id: &str) -> bool {
    let Some(store) = store() else { return false };
    local_chats(store, Duration::from_secs(10))
        .iter()
        .any(|s| s.id == id)
}

/// Send what is queued and publish the roster. Call about every 2 seconds.
/// Blocks while a paired computer is slow to answer (at most 20 seconds).
pub fn tick(service: &Service) {
    let Some(store) = store() else {
        note("Pulse's state folder isn't available.".to_string());
        return;
    };
    let me = identity(service);
    let _ = store.write_relay_status(&me.device, &me.alias);
    send_outbox(service, store);
    publish_roster(service, store, &me);
    let ticks = {
        let mut state = state();
        state.ticks += 1;
        state.ticks
    };
    if ticks.is_multiple_of(60) {
        store.outbox_prune(24 * 60 * 60 * 1000);
    }
}

fn send_outbox(service: &Service, store: &Store) {
    let items = store.outbox_queued();
    if items.is_empty() {
        return;
    }
    let results: Vec<(String, u64, Result<(), String>)> = std::thread::scope(|scope| {
        let handles: Vec<_> = items
            .iter()
            .map(|item| {
                scope.spawn(move || {
                    (
                        item.id.clone(),
                        item.envelope.ts,
                        service.send_bridge(&item.target_device, item.envelope.clone()),
                    )
                })
            })
            .collect();
        handles
            .into_iter()
            .filter_map(|handle| handle.join().ok())
            .collect()
    });
    for (id, ts, result) in results {
        match result {
            Ok(()) => {
                let _ = store.outbox_update(&id, "sent", None);
            }
            Err(e) => {
                let waiting = e.contains("no longer nearby")
                    && now_ms().saturating_sub(ts) < QUEUE_PATIENCE_MS;
                if !waiting {
                    let _ = store.outbox_update(&id, "failed", Some(e.clone()));
                    note(e);
                }
            }
        }
    }
}

fn publish_roster(service: &Service, store: &Store, me: &Identity) {
    let sessions = local_chats(store, SESSION_SCAN_EVERY);
    let body = roster::roster_body(&roster::local_entries(&sessions));
    let due: Vec<String> = {
        let state = state();
        service
            .devices()
            .into_iter()
            .filter(|d| service.is_paired(&d.fingerprint))
            .filter(|d| {
                state
                    .published
                    .get(&d.fingerprint)
                    .is_none_or(|(sent, at)| sent != &body || at.elapsed() >= ROSTER_REFRESH)
            })
            .map(|d| d.fingerprint)
            .collect()
    };
    if due.is_empty() {
        return;
    }
    let results: Vec<(String, Result<(), String>)> = std::thread::scope(|scope| {
        let handles: Vec<_> = due
            .iter()
            .map(|device| {
                let body = body.clone();
                scope.spawn(move || {
                    let sent = Envelope::new(
                        Sender {
                            device: me.device.clone(),
                            session: String::new(),
                            name: me.alias.clone(),
                        },
                        Target {
                            device: device.clone(),
                            session: String::new(),
                        },
                        Kind::Roster,
                        body,
                    )
                    .map_err(|e| e.to_string())
                    .and_then(|envelope| service.send_bridge(device, envelope));
                    (device.clone(), sent)
                })
            })
            .collect();
        handles
            .into_iter()
            .filter_map(|handle| handle.join().ok())
            .collect()
    });
    let mut state = state();
    for (device, result) in results {
        // A failed device is tried again after the refresh interval.
        state
            .published
            .insert(device, (body.clone(), Instant::now()));
        match result {
            Ok(()) => state.last_error = None,
            Err(e) => state.last_error = Some(e),
        }
    }
}

/// Handle a verified envelope from a paired computer (`Event::Bridge`).
/// Blocks while the chat acknowledges; call it off the service thread.
pub fn on_inbound(service: &Service, envelope: Envelope) {
    let Some(store) = store() else { return };
    let me = identity(service);
    match envelope.kind {
        Kind::Roster => {
            let entries = roster::parse_roster_body(&envelope.body);
            let record = RemoteRoster {
                device: envelope.from.device.clone(),
                alias: envelope.from.name.clone(),
                received: now_ms(),
                entries,
            };
            if let Err(e) = store.save_remote_roster(&record) {
                note(format!("Couldn't save a roster: {e}"));
            }
        }
        Kind::Receipt => {
            let value: serde_json::Value = serde_json::from_str(&envelope.body).unwrap_or_default();
            let id = value["msg_id"].as_str().unwrap_or("");
            let state = value["state"].as_str().and_then(ReceiptState::parse);
            if let (false, Some(state)) = (id.is_empty(), state) {
                let detail = value["detail"].as_str().map(str::to_string);
                let _ = store.outbox_update(id, state.as_str(), detail);
            }
        }
        Kind::Message => {
            let sessions = local_chats(store, Duration::from_secs(2));
            let receipt = match sessions.iter().find(|s| s.id == envelope.to.session) {
                Some(session) => deliver_local(store, session, &envelope),
                None => Receipt {
                    msg_id: envelope.id.clone(),
                    session: envelope.to.session.clone(),
                    state: ReceiptState::Refused,
                    detail: "That chat isn't open on this computer any more.".to_string(),
                },
            };
            let body = json!({
                "msg_id": envelope.id,
                "state": receipt.state.as_str(),
                "detail": receipt.detail,
            })
            .to_string();
            let sent = Envelope::new(
                Sender {
                    device: me.device.clone(),
                    session: envelope.to.session.clone(),
                    name: me.alias.clone(),
                },
                Target {
                    device: envelope.from.device.clone(),
                    session: envelope.from.session.clone(),
                },
                Kind::Receipt,
                body,
            )
            .map_err(|e| e.to_string())
            .and_then(|r| service.send_bridge(&envelope.from.device, r));
            if let Err(e) = sent {
                note(format!("Couldn't send a receipt: {e}"));
            }
        }
    }
}

/// A local chat answered a message it was pushed (`ReplyHub`): send the
/// answer to the chat that wrote, wherever it runs.
pub fn on_local_reply(service: &Service, reply: ReplyMessage) {
    let Some(store) = store() else { return };
    let Some((device, session)) = reply.peer_key.split_once(':') else {
        return;
    };
    let sessions = local_chats(store, Duration::from_secs(10));
    let Some(from) = sessions.iter().find(|s| s.id == reply.from_session_id) else {
        return;
    };
    let me = identity(service);
    let envelope = match Envelope::new(
        Sender {
            device: me.device.clone(),
            session: from.id.clone(),
            name: format!("{} on {}", from.name, me.alias),
        },
        Target {
            device: device.to_string(),
            session: session.to_string(),
        },
        Kind::Message,
        reply.text,
    ) {
        Ok(envelope) => envelope,
        Err(e) => return note(format!("Couldn't send a reply: {e}")),
    };
    if device == me.device {
        match sessions.iter().find(|s| s.id == session) {
            Some(target) => {
                deliver_local(store, target, &envelope);
            }
            None => note("A reply's chat is no longer open.".to_string()),
        }
    } else if let Err(e) = service.send_bridge(device, envelope) {
        note(format!("Couldn't send a reply: {e}"));
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusPeer {
    pub device: String,
    pub alias: String,
    pub chats: usize,
    pub age_seconds: u64,
}

/// What the hub's page shows.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub local_chats: usize,
    pub remote_chats: usize,
    /// Paired computers that published a roster recently.
    pub peers: Vec<StatusPeer>,
    pub last_error: Option<String>,
}

pub fn status() -> Status {
    let (local, last_error) = {
        let state = state();
        (state.sessions.len(), state.last_error.clone())
    };
    let now = now_ms();
    let peers: Vec<StatusPeer> = store()
        .map(|s| s.remote_rosters())
        .unwrap_or_default()
        .into_iter()
        .filter(|r| now.saturating_sub(r.received) <= REMOTE_MAX_AGE_MS)
        .map(|r| StatusPeer {
            chats: r.entries.len(),
            age_seconds: now.saturating_sub(r.received) / 1000,
            device: r.device,
            alias: r.alias,
        })
        .collect();
    Status {
        local_chats: local,
        remote_chats: peers.iter().map(|p| p.chats).sum(),
        peers,
        last_error,
    }
}
