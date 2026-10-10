//! What the Pulse hub runs for the bridge: a heartbeat, a rescan of this
//! computer's chats, replies routed back to linked computers, and the CLI's
//! control requests.
//!
//! Why the hub posts into Claude on behalf of the CLI: a Claude chat accepts a
//! peer message only when the posting process descends from a registered
//! session (one with files in `~/.claude/sessions`). A `pulse` started by sshd
//! descends from nothing, so its posts are dropped without a word. The hub
//! therefore registers itself there (`<pid>.json` plus a key file, named like
//! Claude's own, entrypoint `pulse-hub`) and the CLI hands local deliveries to
//! it through `control` when it runs. On Windows the chat also checks the auth
//! token, so the CLI can post directly; the hub is registered there too
//! (`pidDomain` windows, its own reply pipe as `messagingSocketPath`) so chats
//! list it and can answer it.

use super::control::Request;
use super::deliver_claude::ReplyMessage;
use super::envelope::{Envelope, Sender, Target};
use super::links::{self, Link};
use super::roster::{self, LocalSession};
use super::store::Store;
use super::{ReceiptState, deliver_local_via};
use serde::Serialize;
use serde_json::{Value, json};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

const SESSION_SCAN_EVERY: Duration = Duration::from_secs(5);
/// The registry entry's name, as other Claude chats list it.
pub const REGISTERED_NAME: &str = "Pulse";
/// `entrypoint` in the registry entry; the roster hides it.
pub const ENTRYPOINT: &str = "pulse-hub";

#[derive(Default)]
struct State {
    last_error: Option<String>,
    sessions: Vec<LocalSession>,
    scanned: Option<Instant>,
    registered: Option<std::path::PathBuf>,
    key_file: Option<std::path::PathBuf>,
    /// What each link answered last (chats, or the error).
    link_state: Vec<StatusLink>,
    /// The chats each link listed last, for the page's list.
    remote_chats: Vec<StatusChat>,
    links_asked: Option<Instant>,
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

/// Whether `id` is a chat on this computer (for `ReplyHub`).
pub fn is_known_local_session(id: &str) -> bool {
    store().is_some_and(|s| {
        local_chats(s, SESSION_SCAN_EVERY)
            .iter()
            .any(|c| c.id == id)
    })
}

// ---- registration ------------------------------------------------------------------

/// Put this process in Claude's session registry so its posts are accepted.
#[cfg(any(unix, windows))]
fn register(alias: &str) {
    let dir = super::deliver_claude::sessions_dir();
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let pid = std::process::id();
    // Leftovers: an earlier hub that was killed (an install) never unregistered, and a
    // file with this pid could be ours from before. Only entries this hub wrote
    // (`entrypoint` pulse-hub) whose process is gone are removed.
    let mut stale: Vec<u32> = vec![pid];
    if let Ok(listing) = std::fs::read_dir(&dir) {
        for entry in listing.flatten() {
            let path = entry.path();
            if path.extension().is_none_or(|x| x != "json") {
                continue;
            }
            let Some(value) = std::fs::read(&path)
                .ok()
                .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
            else {
                continue;
            };
            if value["entrypoint"] == ENTRYPOINT
                && let Some(old) = value["pid"].as_u64().and_then(|p| u32::try_from(p).ok())
                && old != pid
                && !roster::live_pids()(old)
            {
                stale.push(old);
            }
        }
    }
    if let Ok(listing) = std::fs::read_dir(&dir) {
        for entry in listing.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let ours = stale.iter().any(|p| {
                name == format!("{p}.json")
                    || (name.starts_with(&format!("{p}.")) && name.ends_with(".key"))
            });
            if ours {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
    let now = super::envelope::now_ms();
    #[cfg(unix)]
    let reply_dir = super::deliver_claude::reply_socket_path("hub")
        .to_string_lossy()
        .into_owned();
    #[cfg(windows)]
    let reply_dir = super::deliver_claude::reply_pipe_name("hub");
    #[cfg(unix)]
    let (home_var, domain) = (
        "HOME",
        if cfg!(target_os = "macos") {
            "darwin"
        } else {
            "linux"
        },
    );
    // Claude's own entries say `win32:<host>`; match that so readers treat ours alike.
    #[cfg(windows)]
    let domain_owned = format!(
        "win32:{}",
        sysinfo::System::host_name()
            .unwrap_or_else(|| "pc".to_string())
            .to_lowercase()
    );
    #[cfg(windows)]
    let (home_var, domain) = ("USERPROFILE", domain_owned.as_str());
    let json_path = dir.join(format!("{pid}.json"));
    let key_hash = crate::localsend::proto::random_hex(32);
    let key_path = dir.join(format!("{pid}.{key_hash}.key"));
    let record = json!({
        "pid": pid,
        "sessionId": super::envelope::new_uuid(),
        "cwd": std::env::var(home_var).unwrap_or_else(|_| "/".to_string()),
        "startedAt": now,
        "version": format!("pulse {}", env!("CARGO_PKG_VERSION")),
        "peerProtocol": super::deliver_claude::PEER_PROTOCOL,
        "peerFeatures": [],
        "kind": "interactive",
        "entrypoint": ENTRYPOINT,
        "hostSessionId": format!("pulse-hub-{alias}"),
        "pidDomain": domain,
        "messagingSocketPath": reply_dir,
        "name": REGISTERED_NAME,
        "nameSince": now,
        "updatedAt": now,
        "status": "idle",
        "statusUpdatedAt": now,
    });
    // No `procStart`: the hub doesn't record a start time, and a reader without one
    // checks only that the pid is running.
    let key = json!({
        "peerToken": crate::localsend::proto::random_hex(32),
        "pidDomain": domain,
    });
    if std::fs::write(&key_path, key.to_string()).is_ok() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600));
        }
    }
    if std::fs::write(&json_path, record.to_string()).is_ok() {
        let mut state = state();
        state.registered = Some(json_path);
        state.key_file = Some(key_path);
    }
}

#[cfg(not(any(unix, windows)))]
fn register(_alias: &str) {}

fn unregister() {
    let (json_path, key_path) = {
        let mut state = state();
        (state.registered.take(), state.key_file.take())
    };
    for path in [json_path, key_path].into_iter().flatten() {
        let _ = std::fs::remove_file(path);
    }
}

/// The bridge became active: register and write the first heartbeat.
pub fn start(alias: &str) {
    register(alias);
    if let Some(store) = store()
        && let Err(e) = store.write_relay_status(alias)
    {
        note(format!("Couldn't write the heartbeat: {e}"));
    }
}

/// The bridge went inactive or the hub exits.
pub fn stop() {
    unregister();
    if let Some(store) = store() {
        store.clear_relay_status();
    }
}

/// Every couple of seconds while active: heartbeat, chat rescan, a slow poll
/// of linked computers for the page.
pub fn tick(alias: &str) {
    let Some(store) = store() else { return };
    if let Err(e) = store.write_relay_status(alias) {
        note(format!("Couldn't write the heartbeat: {e}"));
    }
    let _ = local_chats(store, SESSION_SCAN_EVERY);
    let registered = state().registered.is_some();
    if cfg!(any(unix, windows)) && !registered {
        register(alias);
    }
    // The registered pipe has to be listening: a chat that finds the hub in the
    // registry may post to it. (Idempotent; a no-op until the reply hub exists.)
    #[cfg(windows)]
    {
        if state().registered.is_some() {
            let _ = super::deliver_claude::reply_address("hub");
        }
    }
    let due = state()
        .links_asked
        .is_none_or(|at| at.elapsed() >= Duration::from_secs(60));
    if due {
        state().links_asked = Some(Instant::now());
        let answers = links::list_all(store);
        let mut remote: Vec<StatusChat> = Vec::new();
        for r in &answers {
            if let Ok(listing) = &r.listing {
                remote.extend(listing.chats.iter().map(|c| StatusChat {
                    name: c.name.clone(),
                    kind: c.kind.clone(),
                    status: chat_status(&c.kind, &c.status),
                    device: r.link.device.clone(),
                    local: false,
                }));
            }
        }
        let polled: Vec<StatusLink> = answers
            .into_iter()
            .map(|r| StatusLink {
                device: r.link.device,
                ssh: r.link.ssh,
                chats: r.listing.as_ref().ok().map(|l| l.chats.len()),
                error: r.listing.err(),
            })
            .collect();
        let mut st = state();
        st.link_state = polled;
        st.remote_chats = remote;
    }
}

// ---- replies -------------------------------------------------------------------------

/// The link a reply to `device` goes over: the link with that name, else the
/// only link there is.
fn link_for(store: &Store, device: &str) -> Option<Link> {
    links::find(store, device).or_else(|| {
        let all = links::all(store);
        (all.len() == 1).then(|| all[0].clone())
    })
}

/// A local chat answered a message it was pushed (`ReplyHub`): send the
/// answer to the chat that wrote, wherever it runs.
pub fn on_local_reply(reply: ReplyMessage) {
    let Some(store) = store() else { return };
    let Some((device, session)) = reply.peer_key.split_once(':') else {
        return;
    };
    let sessions = local_chats(store, Duration::from_secs(10));
    let Some(from) = sessions.iter().find(|s| s.id == reply.from_session_id) else {
        return;
    };
    let alias = store
        .relay_status()
        .map(|s| s.alias)
        .unwrap_or_else(|| "this computer".to_string());
    let envelope = match Envelope::new(
        Sender {
            device: alias.clone(),
            session: from.id.clone(),
            name: format!("{} on {}", from.name, alias),
        },
        Target {
            device: device.to_string(),
            session: session.to_string(),
        },
        reply.text,
    ) {
        Ok(envelope) => envelope,
        Err(e) => return note(format!("Couldn't send a reply: {e}")),
    };
    if device == alias {
        match sessions.iter().find(|s| s.id == session) {
            Some(target) => {
                deliver_local_via(store, target, &envelope, None);
            }
            None => note("A reply's chat is no longer open.".to_string()),
        }
        return;
    }
    let Some(link) = link_for(store, device) else {
        return note(format!("No link named {device} to send a reply over."));
    };
    match links::post(&link, &envelope) {
        Ok(receipt) => {
            let status = receipt["status"].as_str().unwrap_or("");
            if ReceiptState::parse(status) != Some(ReceiptState::Delivered) {
                note(format!(
                    "{}: reply {}: {}",
                    link.device,
                    status,
                    receipt["detail"].as_str().unwrap_or("")
                ));
            }
        }
        Err(e) => note(format!("Couldn't send a reply: {e}")),
    }
}

// ---- control -------------------------------------------------------------------------

/// Answer a request from the CLI. `deliver` `{session, envelope, reply_socket?}`
/// pushes an envelope into a chat here and returns `{ok, status, detail}`.
pub fn handle_control(request: &Request) -> Value {
    let Some(store) = store() else {
        return json!({"ok": false, "error": "The bridge store is not available."});
    };
    match request.op.as_str() {
        "deliver" => {
            let session_id = request.args["session"].as_str().unwrap_or("");
            let envelope = match request.args["envelope"]
                .as_str()
                .ok_or_else(|| "missing envelope".to_string())
                .and_then(links::decode_envelope)
            {
                Ok(env) => env,
                Err(e) => return json!({"ok": false, "error": format!("Bad request: {e}")}),
            };
            let sessions = local_chats(store, Duration::from_secs(2));
            let Some(session) = sessions.iter().find(|s| s.id == session_id) else {
                return json!({"ok": true, "status": "refused",
                              "detail": "That chat isn't open on this computer any more."});
            };
            let reply_socket = request.args["reply_socket"].as_str();
            let receipt = deliver_local_via(store, session, &envelope, reply_socket);
            json!({"ok": true, "status": receipt.state.as_str(), "detail": receipt.detail})
        }
        other => json!({"ok": false, "error": format!("Unknown request: {other}")}),
    }
}

// ---- status ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusLink {
    pub device: String,
    pub ssh: String,
    /// Chats listed there at the last poll, or None when it could not be asked.
    pub chats: Option<usize>,
    pub error: Option<String>,
}

/// One chat as the page lists it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusChat {
    pub name: String,
    /// "claude" or "codex".
    pub kind: String,
    /// Claude: busy or idle (live). Codex: "recent" (no open-chat registry exists).
    pub status: String,
    pub device: String,
    pub local: bool,
}

/// Codex threads come from the session index, so their status is "recent", not a liveness.
fn chat_status(kind: &str, status: &str) -> String {
    if kind == "codex" {
        "recent".to_string()
    } else {
        status.to_string()
    }
}

/// What the hub's page shows.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub local_chats: usize,
    pub links: Vec<StatusLink>,
    /// Every chat, here and on linked computers, as "<name> on <device>" material.
    pub chats: Vec<StatusChat>,
    pub last_error: Option<String>,
}

pub fn status() -> Status {
    let state = state();
    let alias = store()
        .and_then(|s| s.relay_status())
        .map(|s| s.alias)
        .unwrap_or_else(|| "this computer".to_string());
    let mut chats: Vec<StatusChat> = state
        .sessions
        .iter()
        .map(|s| StatusChat {
            name: s.name.clone(),
            kind: s.kind.clone(),
            status: chat_status(&s.kind, &s.status),
            device: alias.clone(),
            local: true,
        })
        .collect();
    chats.extend(state.remote_chats.iter().cloned());
    Status {
        chats,
        local_chats: state.sessions.len(),
        links: state.link_state.clone(),
        last_error: state.last_error.clone(),
    }
}
