//! Which chats can be messaged: live Claude chats read from
//! `~/.claude/sessions/*.json`, registered chats (and Claude without a session
//! file), the most recent Codex threads from
//! the Codex state database (`deliver_codex`), and the chats linked computers list over
//! ssh. A peer is shown as "<chat title> on <device>".

use super::BridgeError;
use super::deliver_claude::{start_identity, text_of};
use super::deliver_codex::{CodexThread, DiscoveryError, find_thread_by_id, list_threads};
use super::links::RemoteChats;
use super::store::Store;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};

/// One chat as a linked computer lists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RosterEntry {
    pub session: String,
    pub name: String,
    /// "claude" or "codex".
    pub kind: String,
    #[serde(default)]
    pub cwd: String,
    #[serde(default)]
    pub status: String,
    /// Milliseconds since the epoch of the chat's last activity, when known.
    #[serde(default)]
    pub updated_ms: Option<u64>,
    /// "live" (process identity verified), "stale" or "unknown" (no way to tell).
    #[serde(default = "unknown_liveness")]
    pub liveness: String,
}

fn unknown_liveness() -> String {
    "unknown".to_string()
}

/// A chat on this computer, with what a delivery needs to reach it.
#[derive(Debug, Clone, Serialize)]
pub struct LocalSession {
    pub id: String,
    /// "claude" or "codex".
    pub kind: String,
    pub name: String,
    pub cwd: String,
    pub status: String,
    pub updated_ms: Option<u64>,
    /// "live" (process and its start time verified), "stale" or "unknown".
    pub liveness: String,
    pub pid: Option<u32>,
    /// Claude's per-chat inbox socket (named pipe on Windows), when it has one.
    pub messaging_socket: Option<String>,
    pub peer_protocol: Option<u64>,
    pub entrypoint: Option<String>,
    /// The whole session file (or registration) as read.
    pub raw: Value,
}

impl LocalSession {
    pub fn entry(&self) -> RosterEntry {
        RosterEntry {
            session: self.id.clone(),
            name: self.name.clone(),
            kind: self.kind.clone(),
            cwd: self.cwd.clone(),
            status: self.status.clone(),
            updated_ms: self.updated_ms,
            liveness: self.liveness.clone(),
        }
    }
}

/// A chat that can be messaged, here or on a linked computer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Peer {
    /// What `pulse bridge send` accepts: the session id here, `<device>:<session>` elsewhere.
    pub id: String,
    pub session: String,
    pub name: String,
    /// "<chat title> on <device>".
    pub display: String,
    /// The computer the chat runs on: this one's alias, or a link's device name.
    pub device: String,
    pub device_alias: String,
    pub local: bool,
    pub kind: String,
    pub cwd: String,
    pub status: String,
    pub updated_ms: Option<u64>,
    /// "live", "stale" or "unknown"; a Codex chat is always "unknown".
    pub liveness: String,
}

/// `~/.claude/sessions` (or `$CLAUDE_CONFIG_DIR/sessions`).
pub fn claude_sessions_dir() -> Option<PathBuf> {
    Some(super::deliver_claude::sessions_dir())
}

/// A check for whether a process id is running, read once for the whole list.
pub fn live_pids() -> impl Fn(u32) -> bool {
    use sysinfo::{Pid, ProcessesToUpdate, System};
    let mut system = System::new();
    system.refresh_processes(ProcessesToUpdate::All, true);
    move |pid| system.process(Pid::from_u32(pid)).is_some()
}

fn title_for(name: &str, cwd: &str, fallback: &str) -> String {
    let name = name.trim();
    if !name.is_empty() {
        return name.to_string();
    }
    Path::new(cwd)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| fallback.to_string())
}

fn parse_claude(value: Value, alive: &dyn Fn(u32) -> bool) -> Option<LocalSession> {
    let pid = u32::try_from(value.get("pid")?.as_u64()?).ok()?;
    if !alive(pid) {
        return None;
    }
    // A reused pid is not this chat: the process must have started when the file says.
    let recorded_start = text_of(&value["procStart"]);
    let domain = text_of(&value["pidDomain"]);
    let liveness = match start_identity(pid, recorded_start.as_deref(), domain.as_deref()) {
        Some(true) => "live",
        Some(false) => return None,
        None => "unknown",
    };
    let text = |key: &str| value.get(key).and_then(Value::as_str).map(str::to_string);
    let id = text("sessionId").filter(|s| !s.is_empty())?;
    // App chats only (Claude Desktop, or the IDE extension): terminal (`cli`) and SDK
    // sessions are reached over ssh, not the bridge. The Pulse hub registers itself so
    // Claude accepts its posts; it is not a chat either. A file without the key is older
    // Claude's and is kept.
    if text("entrypoint")
        .as_deref()
        .is_some_and(|e| e != "claude-desktop" && e != "claude-vscode")
    {
        return None;
    }
    let cwd = text("cwd").unwrap_or_default();
    let name = title_for(&text("name").unwrap_or_default(), &cwd, "Claude chat");
    Some(LocalSession {
        id,
        kind: "claude".to_string(),
        name,
        cwd,
        status: text("status").unwrap_or_else(|| "unknown".to_string()),
        updated_ms: ["statusUpdatedAt", "updatedAt"]
            .iter()
            .find_map(|k| value.get(*k).and_then(Value::as_u64)),
        liveness: liveness.to_string(),
        pid: Some(pid),
        messaging_socket: text("messagingSocketPath").filter(|s| !s.is_empty()),
        peer_protocol: value.get("peerProtocol").and_then(Value::as_u64),
        entrypoint: text("entrypoint"),
        raw: value,
    })
}

/// Live Claude chats described by the `*.json` files in `dir`.
pub fn claude_sessions_in(dir: &Path, alive: &dyn Fn(u32) -> bool) -> Vec<LocalSession> {
    let mut sessions: Vec<LocalSession> = std::fs::read_dir(dir)
        .map(|listing| {
            listing
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|x| x == "json"))
                .filter_map(|p| std::fs::read(p).ok())
                .filter_map(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
                .filter_map(|value| parse_claude(value, alive))
                .collect()
        })
        .unwrap_or_default();
    sessions.sort_by(|a, b| a.id.cmp(&b.id));
    sessions
}

/// Every chat on this computer: live Claude chats from disk, plus live chats
/// registered that disk discovery did not already list.
pub fn local_sessions_in(
    store: &Store,
    claude_dir: Option<&Path>,
    alive: &dyn Fn(u32) -> bool,
) -> Vec<LocalSession> {
    let mut sessions = claude_dir
        .map(|dir| claude_sessions_in(dir, alive))
        .unwrap_or_default();
    for registered in store.registered_sessions() {
        if !alive(registered.pid) || sessions.iter().any(|s| s.id == registered.id) {
            continue;
        }
        sessions.push(LocalSession {
            name: title_for(&registered.name, &registered.cwd, "Chat"),
            id: registered.id.clone(),
            kind: registered.kind.clone(),
            cwd: registered.cwd.clone(),
            status: "unknown".to_string(),
            updated_ms: None,
            liveness: "unknown".to_string(),
            pid: Some(registered.pid),
            messaging_socket: None,
            peer_protocol: None,
            entrypoint: None,
            raw: serde_json::to_value(&registered).unwrap_or(Value::Null),
        });
    }
    sessions
}

/// How many Codex threads (newest first, not archived) are listed.
pub const CODEX_THREAD_LIMIT: usize = 100;

/// A Codex thread as a chat here. Whether the thread is open is unknown, so its
/// liveness is "unknown" and its status only says how recently it was written.
fn codex_session(thread: CodexThread) -> LocalSession {
    let status = if thread.archived {
        "archived".to_string()
    } else {
        codex_status(thread.updated_ms)
    };
    LocalSession {
        name: title_for(&thread.name, "", "Codex chat"),
        raw: serde_json::json!({
            "id": thread.id,
            "thread_name": thread.name,
            "updated_at": thread.updated_at,
            "updated_at_ms": thread.updated_ms,
            "archived": thread.archived,
        }),
        id: thread.id,
        kind: "codex".to_string(),
        cwd: thread.cwd,
        status,
        updated_ms: Some(thread.updated_ms).filter(|m| *m > 0 && !in_future(*m)),
        liveness: "unknown".to_string(),
        pid: None,
        messaging_socket: None,
        peer_protocol: None,
        entrypoint: None,
    }
}

/// Add the Codex threads in `threads` that are not already listed.
pub fn add_codex_threads(sessions: &mut Vec<LocalSession>, threads: Vec<CodexThread>) {
    for thread in threads {
        if sessions.iter().any(|s| s.id == thread.id) {
            continue;
        }
        sessions.push(codex_session(thread));
    }
}

/// The Codex thread with exactly this id (archived ones too), whether or not it is
/// among the recent threads the roster lists. `Ok(None)` when it does not exist.
pub fn resolve_exact_codex(id: &str) -> Result<Option<LocalSession>, DiscoveryError> {
    Ok(find_thread_by_id(id)?.map(codex_session))
}

/// A stamp more than a minute ahead of this computer's clock is not believable.
fn in_future(updated_ms: u64) -> bool {
    updated_ms > super::envelope::now_ms() + 60_000
}

/// Codex keeps no registry of open chats, so this only says how recently a thread was
/// written: "recent" within the last ten minutes, otherwise "idle". A future timestamp
/// is never "recent". Neither word says the chat is open or waiting (see `liveness`).
pub fn codex_status(updated_ms: u64) -> String {
    let now = super::envelope::now_ms();
    let recent = updated_ms > 0 && updated_ms <= now + 60_000 && now <= updated_ms + 10 * 60 * 1000;
    if recent {
        "recent".to_string()
    } else {
        "idle".to_string()
    }
}

/// Seconds since the epoch of an ISO-8601 `YYYY-MM-DDTHH:MM:SS…` UTC stamp.
pub(super) fn iso_epoch(text: &str) -> Option<u64> {
    let b = text.as_bytes();
    if b.len() < 19 {
        return None;
    }
    let num = |a: usize, z: usize| text.get(a..z)?.parse::<i64>().ok();
    let (y, mo, d, h, mi, s) = (
        num(0, 4)?,
        num(5, 7)?,
        num(8, 10)?,
        num(11, 13)?,
        num(14, 16)?,
        num(17, 19)?,
    );
    let yy = if mo <= 2 { y - 1 } else { y };
    let era = yy.div_euclid(400);
    let yoe = yy - era * 400;
    let doy = (153 * (mo + if mo > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    u64::try_from(days * 86_400 + h * 3600 + mi * 60 + s).ok()
}

/// `local_sessions_in` for this computer's real folders, plus the most recent
/// Codex threads from `~/.codex/session_index.jsonl`.
pub fn local_sessions(store: &Store) -> Vec<LocalSession> {
    let alive = live_pids();
    let mut sessions = local_sessions_in(store, claude_sessions_dir().as_deref(), &alive);
    add_codex_threads(&mut sessions, list_threads(CODEX_THREAD_LIMIT));
    sessions
}

pub fn local_entries(sessions: &[LocalSession]) -> Vec<RosterEntry> {
    sessions.iter().map(LocalSession::entry).collect()
}

fn display(name: &str, alias: &str) -> String {
    format!("{name} on {alias}")
}

/// Local chats plus what each linked computer listed, each labelled with its
/// computer. Links that could not be asked contribute nothing.
pub fn merge(local: &[LocalSession], local_alias: &str, remotes: &[RemoteChats]) -> Vec<Peer> {
    let mut peers: Vec<Peer> = local
        .iter()
        .map(|s| Peer {
            id: s.id.clone(),
            session: s.id.clone(),
            name: s.name.clone(),
            display: display(&s.name, local_alias),
            device: local_alias.to_string(),
            device_alias: local_alias.to_string(),
            local: true,
            kind: s.kind.clone(),
            cwd: s.cwd.clone(),
            status: s.status.clone(),
            updated_ms: s.updated_ms,
            liveness: s.liveness.clone(),
        })
        .collect();
    for remote in remotes {
        let Ok(listing) = &remote.listing else {
            continue;
        };
        for entry in &listing.chats {
            peers.push(Peer {
                id: format!("{}:{}", remote.link.device, entry.session),
                session: entry.session.clone(),
                name: entry.name.clone(),
                display: display(&entry.name, &remote.link.device),
                device: remote.link.device.clone(),
                device_alias: remote.link.device.clone(),
                local: false,
                kind: entry.kind.clone(),
                cwd: entry.cwd.clone(),
                status: entry.status.clone(),
                updated_ms: entry.updated_ms,
                liveness: entry.liveness.clone(),
            });
        }
    }
    peers
}

fn matches_tier(tier: usize, p: &Peer, wanted: &str) -> bool {
    match tier {
        0 => p.id.to_lowercase() == wanted,
        1 => p.session.to_lowercase() == wanted,
        2 => p.display.to_lowercase() == wanted || p.name.to_lowercase() == wanted,
        _ => p.display.to_lowercase().contains(wanted),
    }
}

/// The peer a user or chat meant: exact id, then session id, then title or
/// "title on device", then a part of the display name. More than one match is
/// an error that lists them.
pub fn resolve(peers: &[Peer], query: &str) -> Result<Peer, BridgeError> {
    let wanted = query.trim().to_lowercase();
    if wanted.is_empty() {
        return Err(BridgeError::Invalid("who should it go to?".into()));
    }
    for tier in 0..4 {
        let hits: Vec<&Peer> = peers
            .iter()
            .filter(|p| matches_tier(tier, p, &wanted))
            .collect();
        match hits.len() {
            0 => continue,
            1 => return Ok(hits[0].clone()),
            _ => {
                let names: Vec<String> = hits
                    .iter()
                    .map(|p| format!("{} ({})", p.display, p.id))
                    .collect();
                return Err(BridgeError::Invalid(format!(
                    "\"{query}\" matches more than one chat: {}",
                    names.join(", ")
                )));
            }
        }
    }
    Err(BridgeError::NotFound(query.to_string()))
}
