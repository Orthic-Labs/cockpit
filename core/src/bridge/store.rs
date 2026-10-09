//! Files the bridge keeps under `<Pulse state dir>/bridge`: per-chat inboxes
//! (JSON lines with a read cursor), the outbox the relay drains, the cache of
//! remote rosters, chats that registered through the MCP server, and the
//! relay's heartbeat. Every replacement is a temporary file renamed into place;
//! inbox appends happen under a lock file.

use super::envelope::{Envelope, now_ms};
use super::roster::RosterEntry;
use crate::localsend::proto;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Per-inbox size cap; the oldest messages go first.
pub const INBOX_CAP_BYTES: u64 = 10 * 1024 * 1024;
/// A relay that has not written its heartbeat for this long counts as stopped.
pub const RELAY_STALE_MS: u64 = 90_000;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InboxEntry {
    /// Position in this inbox, growing by one for every message.
    pub seq: u64,
    /// When this computer received it (ms).
    pub received: u64,
    pub env: Envelope,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Cursor {
    /// Highest `seq` already read.
    #[serde(default)]
    read: u64,
    /// The `seq` the next message gets.
    #[serde(default)]
    next: u64,
}

/// A message waiting for the relay to send it to another computer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutboxItem {
    pub id: String,
    /// Fingerprint of the computer it goes to.
    pub target_device: String,
    /// Unsigned; the relay signs it with the pair key when it sends.
    pub envelope: Envelope,
    /// queued, sent, failed, or a receipt state: delivered, held, refused.
    pub status: String,
    #[serde(default)]
    pub detail: Option<String>,
    pub updated: u64,
}

/// The last roster a paired computer published.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteRoster {
    pub device: String,
    pub alias: String,
    /// When it arrived (ms).
    pub received: u64,
    pub entries: Vec<RosterEntry>,
}

/// A chat that is not discovered from disk (Codex, or Claude without a
/// session file) and announced itself through `pulse bridge mcp`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisteredSession {
    pub id: String,
    /// "claude" or "codex".
    pub kind: String,
    pub name: String,
    pub cwd: String,
    /// The MCP server's process; the chat is gone when it is.
    pub pid: u32,
    pub updated: u64,
}

/// What the relay says about itself.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelayStatus {
    pub pid: u32,
    pub ts: u64,
    pub device: String,
    pub alias: String,
}

fn safe_name(raw: &str) -> String {
    let cleaned: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '_'
            }
        })
        .take(100)
        .collect();
    let trimmed = cleaned.trim_matches('.');
    if trimmed.is_empty() {
        "_".to_string()
    } else {
        trimmed.to_string()
    }
}

fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temp = path.with_extension(format!("tmp-{}", proto::random_hex(4)));
    let result = fs::File::create(&temp).and_then(|mut file| {
        file.write_all(bytes)?;
        file.sync_all()
    });
    if let Err(e) = result {
        let _ = fs::remove_file(&temp);
        return Err(e);
    }
    fs::rename(&temp, path).inspect_err(|_| {
        let _ = fs::remove_file(&temp);
    })
}

fn read_json<T: DeserializeOwned>(path: &Path) -> Option<T> {
    serde_json::from_slice(&fs::read(path).ok()?).ok()
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> io::Result<()> {
    let bytes = serde_json::to_vec(value).map_err(io::Error::other)?;
    write_atomic(path, &bytes)
}

fn json_files(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = fs::read_dir(dir)
        .map(|listing| {
            listing
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|x| x == "json"))
                .collect()
        })
        .unwrap_or_default();
    files.sort();
    files
}

struct LockFile(PathBuf);

impl Drop for LockFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn fnv(text: &str) -> u64 {
    let mut state = 0xcbf2_9ce4_8422_2325u64;
    for b in text.bytes() {
        state ^= u64::from(b);
        state = state.wrapping_mul(0x0100_0000_01b3);
    }
    state
}

#[derive(Debug, Clone)]
pub struct Store {
    root: PathBuf,
    inbox_cap: u64,
}

impl Store {
    /// The bridge folder inside `state_dir` (the Pulse state directory).
    pub fn new(state_dir: &Path) -> Store {
        Store {
            root: state_dir.join("bridge"),
            inbox_cap: INBOX_CAP_BYTES,
        }
    }

    /// The store in Pulse's default state directory.
    pub fn open_default() -> io::Result<Store> {
        Ok(Store::new(&crate::store::default_directory()?))
    }

    pub fn with_inbox_cap(mut self, bytes: u64) -> Store {
        self.inbox_cap = bytes;
        self
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The directory the LocalSend service keeps its certificate and
    /// `known-devices.json` in (a sibling of the bridge folder).
    pub fn localsend_dir(&self) -> PathBuf {
        self.root
            .parent()
            .map(|p| p.join("localsend"))
            .unwrap_or_else(|| self.root.join("localsend"))
    }

    fn lock(&self, target: &Path) -> io::Result<LockFile> {
        let path = target.with_extension("lock");
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let started = Instant::now();
        loop {
            match fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(_) => return Ok(LockFile(path)),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                    let stale = fs::metadata(&path)
                        .and_then(|m| m.modified())
                        .ok()
                        .and_then(|t| t.elapsed().ok())
                        .is_some_and(|age| age > Duration::from_secs(10));
                    if stale {
                        let _ = fs::remove_file(&path);
                        continue;
                    }
                    if started.elapsed() > Duration::from_secs(3) {
                        return Err(io::Error::new(io::ErrorKind::TimedOut, "inbox is busy"));
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(e) => return Err(e),
            }
        }
    }

    // ---- inboxes -----------------------------------------------------------

    fn inbox_path(&self, session: &str) -> PathBuf {
        self.root
            .join("inbox")
            .join(format!("{}.jsonl", safe_name(session)))
    }

    fn cursor_path(&self, session: &str) -> PathBuf {
        self.root
            .join("inbox")
            .join(format!("{}.cursor.json", safe_name(session)))
    }

    fn read_cursor(&self, session: &str) -> Cursor {
        read_json(&self.cursor_path(session)).unwrap_or_default()
    }

    fn read_lines(path: &Path) -> Vec<InboxEntry> {
        fs::read_to_string(path)
            .map(|text| {
                text.lines()
                    .filter_map(|line| serde_json::from_str(line).ok())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Add a message to a chat's inbox; returns its `seq`. When the file would
    /// pass the cap, the oldest messages are dropped first.
    pub fn append_inbox(&self, session: &str, env: &Envelope) -> io::Result<u64> {
        let path = self.inbox_path(session);
        let _guard = self.lock(&path)?;
        let mut cursor = self.read_cursor(session);
        let seq = cursor.next.max(1);
        let entry = InboxEntry {
            seq,
            received: now_ms(),
            env: env.clone(),
        };
        let line = serde_json::to_string(&entry).map_err(io::Error::other)? + "\n";
        let size = fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        if size + line.len() as u64 > self.inbox_cap {
            let mut kept: Vec<String> = Self::read_lines(&path)
                .iter()
                .filter_map(|e| serde_json::to_string(e).ok())
                .map(|l| l + "\n")
                .collect();
            let mut total: u64 =
                kept.iter().map(|l| l.len() as u64).sum::<u64>() + line.len() as u64;
            let mut drop_count = 0;
            while total > self.inbox_cap && drop_count < kept.len() {
                total -= kept[drop_count].len() as u64;
                drop_count += 1;
            }
            kept.drain(..drop_count);
            kept.push(line);
            write_atomic(&path, kept.concat().as_bytes())?;
        } else {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            let mut file = fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)?;
            file.write_all(line.as_bytes())?;
            file.sync_all()?;
        }
        cursor.next = seq + 1;
        write_json(&self.cursor_path(session), &cursor)?;
        Ok(seq)
    }

    /// Every stored message of a chat, oldest first.
    pub fn read_inbox(&self, session: &str) -> Vec<InboxEntry> {
        Self::read_lines(&self.inbox_path(session))
    }

    /// Unread messages (received after `since`, ms, when given), marked read.
    pub fn take_unread(&self, session: &str, since: Option<u64>) -> io::Result<Vec<InboxEntry>> {
        let path = self.inbox_path(session);
        let _guard = self.lock(&path)?;
        let mut cursor = self.read_cursor(session);
        let entries = Self::read_lines(&path);
        let newest = entries.iter().map(|e| e.seq).max().unwrap_or(cursor.read);
        let unread: Vec<InboxEntry> = entries
            .into_iter()
            .filter(|e| e.seq > cursor.read && since.is_none_or(|s| e.received > s))
            .collect();
        if newest > cursor.read {
            cursor.read = newest;
            cursor.next = cursor.next.max(newest + 1);
            write_json(&self.cursor_path(session), &cursor)?;
        }
        Ok(unread)
    }

    /// How many messages of a chat are still unread.
    pub fn unread_count(&self, session: &str) -> usize {
        let read = self.read_cursor(session).read;
        self.read_inbox(session)
            .iter()
            .filter(|e| e.seq > read)
            .count()
    }

    // ---- outbox ------------------------------------------------------------

    fn outbox_path(&self, id: &str) -> PathBuf {
        self.root
            .join("outbox")
            .join(format!("{}.json", safe_name(id)))
    }

    pub fn enqueue_outbox(&self, envelope: &Envelope, target_device: &str) -> io::Result<()> {
        let item = OutboxItem {
            id: envelope.id.clone(),
            target_device: target_device.to_string(),
            envelope: envelope.clone(),
            status: "queued".to_string(),
            detail: None,
            updated: now_ms(),
        };
        write_json(&self.outbox_path(&item.id), &item)
    }

    pub fn outbox_get(&self, id: &str) -> Option<OutboxItem> {
        read_json(&self.outbox_path(id))
    }

    /// Items still waiting for the relay, oldest first.
    pub fn outbox_queued(&self) -> Vec<OutboxItem> {
        let mut items: Vec<OutboxItem> = json_files(&self.root.join("outbox"))
            .iter()
            .filter_map(|p| read_json::<OutboxItem>(p))
            .filter(|i| i.status == "queued")
            .collect();
        items.sort_by_key(|i| i.updated);
        items
    }

    pub fn outbox_update(&self, id: &str, status: &str, detail: Option<String>) -> io::Result<()> {
        let Some(mut item) = self.outbox_get(id) else {
            return Ok(());
        };
        item.status = status.to_string();
        item.detail = detail;
        item.updated = now_ms();
        write_json(&self.outbox_path(id), &item)
    }

    /// Forget finished outbox items older than `max_age_ms`.
    pub fn outbox_prune(&self, max_age_ms: u64) {
        let now = now_ms();
        for path in json_files(&self.root.join("outbox")) {
            if let Some(item) = read_json::<OutboxItem>(&path)
                && item.status != "queued"
                && now.saturating_sub(item.updated) > max_age_ms
            {
                let _ = fs::remove_file(path);
            }
        }
    }

    // ---- remote rosters ----------------------------------------------------

    fn roster_path(&self, device: &str) -> PathBuf {
        self.root
            .join("roster")
            .join(format!("{}.json", safe_name(device)))
    }

    pub fn save_remote_roster(&self, roster: &RemoteRoster) -> io::Result<()> {
        write_json(&self.roster_path(&roster.device), roster)
    }

    pub fn remote_rosters(&self) -> Vec<RemoteRoster> {
        json_files(&self.root.join("roster"))
            .iter()
            .filter_map(|p| read_json::<RemoteRoster>(p))
            .collect()
    }

    // ---- chats registered through the MCP server ---------------------------

    pub fn register_session(&self, session: &RegisteredSession) -> io::Result<()> {
        write_json(
            &self
                .root
                .join("sessions")
                .join(format!("{}.json", safe_name(&session.id))),
            session,
        )
    }

    pub fn registered_sessions(&self) -> Vec<RegisteredSession> {
        json_files(&self.root.join("sessions"))
            .iter()
            .filter_map(|p| read_json::<RegisteredSession>(p))
            .collect()
    }

    pub fn forget_session(&self, id: &str) {
        let _ = fs::remove_file(
            self.root
                .join("sessions")
                .join(format!("{}.json", safe_name(id))),
        );
    }

    /// A chat id for a working folder that has none from its host: made once
    /// and kept, so the same folder is the same chat next time.
    pub fn cwd_session_id(&self, cwd: &str, kind: &str) -> io::Result<String> {
        let path = self
            .root
            .join("sessions")
            .join(format!("cwd-{:016x}.id", fnv(cwd)));
        if let Ok(text) = fs::read_to_string(&path) {
            let id = text.trim().to_string();
            if !id.is_empty() {
                return Ok(id);
            }
        }
        let id = format!("{kind}-{}", proto::random_hex(8));
        write_atomic(&path, id.as_bytes())?;
        Ok(id)
    }

    // ---- relay heartbeat ---------------------------------------------------

    fn relay_path(&self) -> PathBuf {
        self.root.join("relay.json")
    }

    pub fn write_relay_status(&self, device: &str, alias: &str) -> io::Result<()> {
        write_json(
            &self.relay_path(),
            &RelayStatus {
                pid: std::process::id(),
                ts: now_ms(),
                device: device.to_string(),
                alias: alias.to_string(),
            },
        )
    }

    pub fn relay_status(&self) -> Option<RelayStatus> {
        read_json(&self.relay_path())
    }

    /// Whether a relay (the hub or `pulse bridge daemon`) is running.
    pub fn relay_alive(&self) -> bool {
        self.relay_status()
            .is_some_and(|s| now_ms().saturating_sub(s.ts) < RELAY_STALE_MS)
    }
}
