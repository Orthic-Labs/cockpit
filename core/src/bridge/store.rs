//! Files the bridge keeps under `<Pulse state dir>/bridge`: per-chat inboxes
//! (JSON lines with a read cursor), linked computers (`links.json`),
//! registered chats, the hub's heartbeat, the last send/receive times
//! (`activity.json`, which the notch rings pulse on), the duplicate journal
//! (`journal.jsonl`), persisted reply routes and the outbound retry queue.
//!
//! Every replacement is a temporary file renamed into place; appends and
//! read-modify-write cycles happen under a lock. On unix the lock is an
//! `flock` on a per-target lock file (the kernel drops it when the holder dies,
//! so nothing is ever stolen); elsewhere it is a create-new lock file that
//! records its owner's pid and is stolen only when it is old and that pid is
//! gone. Folders are created 0o700 and files 0o600 on unix, files are opened
//! without following links, and on Windows a link or reparse point is refused.

use super::envelope::{Envelope, now_ms};
use super::links::Link;
use crate::localsend::proto;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Per-inbox size cap; the oldest messages go first.
pub const INBOX_CAP_BYTES: u64 = 10 * 1024 * 1024;
/// A hub that has not written its heartbeat for this long counts as stopped.
pub const RELAY_STALE_MS: u64 = 30_000;
/// The duplicate journal keeps this many entries; the oldest are dropped.
pub const JOURNAL_CAP: usize = 10_000;
/// At most this many undelivered replies wait in the outbound queue.
const OUTBOUND_CAP: usize = 1_000;
/// Reply routes older than this are forgotten.
const ROUTE_TTL: Duration = Duration::from_secs(30 * 24 * 3600);

/// When this computer last sent or received a bridge message, and how many so far.
/// The hub publishes it; the notch pulses the Send ring for a few seconds after either.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Activity {
    #[serde(default)]
    pub sent_ms: u64,
    #[serde(default)]
    pub received_ms: u64,
    #[serde(default)]
    pub sent: u64,
    #[serde(default)]
    pub received: u64,
    /// The receipt state word (`delivered`, `held`, ...) of the latest send or receive.
    #[serde(default)]
    pub last_outcome: String,
}

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

/// Unread messages of a chat, and how many unread ones the size cap has dropped.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UnreadDetail {
    pub unread: usize,
    /// Messages evicted by the inbox cap before they were read (all time).
    pub evicted: u64,
}

/// What the duplicate journal says about a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Seen {
    /// Never seen from that origin.
    New,
    /// Seen with the same content.
    Duplicate,
    /// Seen with this id but different content.
    Conflict,
}

/// Where a reply to a delivered message goes: the sender's chat.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplyRoute {
    /// The id of the message that was delivered here.
    pub msg_id: String,
    /// The sending computer's name.
    pub from_device: String,
    /// The sending chat.
    pub from_session: String,
    /// The chat here that received it (a reply must come from it).
    pub to_session: String,
    pub created_ms: u64,
}

/// A chat that is not discovered from disk (Codex, or Claude without a
/// session file) and registered here.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisteredSession {
    pub id: String,
    /// "claude" or "codex".
    pub kind: String,
    pub name: String,
    pub cwd: String,
    /// The registering process; the chat is gone when it is.
    pub pid: u32,
    pub updated: u64,
}

/// What the hub says about itself while its bridge runs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelayStatus {
    pub pid: u32,
    pub ts: u64,
    /// This computer's name, as chats are shown ("<chat> on <alias>").
    pub alias: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct JournalLine {
    o: String,
    id: String,
    h: String,
    ts: u64,
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

fn fnv(text: &str) -> u64 {
    let mut state = 0xcbf2_9ce4_8422_2325u64;
    for b in text.bytes() {
        state ^= u64::from(b);
        state = state.wrapping_mul(0x0100_0000_01b3);
    }
    state
}

/// The file stem for a chat id: a readable prefix plus a hash of the whole id, so two
/// ids that sanitize alike never share files.
fn session_stem(id: &str) -> String {
    let prefix: String = safe_name(id).chars().take(40).collect();
    format!("{prefix}-{:016x}", fnv(id))
}

/// The stem older versions used, when it can only have belonged to this id.
fn legacy_stem(id: &str) -> Option<String> {
    let old = safe_name(id);
    (old == id).then_some(old)
}

/// A hex SHA-256 of `bytes`.
fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The content hash `journal_seen` compares: sender, target and body of an envelope.
pub fn payload_hash(env: &Envelope) -> String {
    let mut data = Vec::new();
    for part in [
        env.from.device.as_str(),
        env.from.session.as_str(),
        env.to.device.as_str(),
        env.to.session.as_str(),
        env.body.as_str(),
    ] {
        data.extend_from_slice(part.as_bytes());
        data.push(0);
    }
    sha256_hex(&data)
}

// ---- files that stay private ---------------------------------------------------

fn refuse_link(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "{} is a link; the chat store does not follow links",
                path.display()
            ),
        )),
        _ => Ok(()),
    }
}

/// Create `dir` (0o700 on unix, also when it exists) unless it is a link.
fn secure_dir(dir: &Path) -> io::Result<()> {
    refuse_link(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)?;
        let _ = fs::set_permissions(dir, fs::Permissions::from_mode(0o700));
    }
    #[cfg(not(unix))]
    fs::create_dir_all(dir)?;
    Ok(())
}

/// `secure_dir` for `dir` and, when `dir` is directly inside the bridge folder, for that.
fn secure_chain(dir: &Path) -> io::Result<()> {
    secure_dir(dir)?;
    if let Some(parent) = dir.parent()
        && parent.file_name().is_some_and(|n| n == "bridge")
    {
        secure_dir(parent)?;
    }
    Ok(())
}

fn file_opts() -> fs::OpenOptions {
    #[allow(unused_mut)] // only unix adds options
    let mut opts = fs::OpenOptions::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    opts
}

fn open_with(opts: &fs::OpenOptions, path: &Path) -> io::Result<fs::File> {
    refuse_link(path)?;
    opts.open(path)
}

fn read_bytes(path: &Path) -> io::Result<Vec<u8>> {
    let mut file = open_with(file_opts().read(true), path)?;
    let mut out = Vec::new();
    file.read_to_end(&mut out)?;
    Ok(out)
}

/// The last `max` bytes of a file, starting at a line boundary.
fn read_tail(path: &Path, max: u64) -> Vec<u8> {
    let Ok(mut file) = open_with(file_opts().read(true), path) else {
        return Vec::new();
    };
    let len = file.metadata().map(|m| m.len()).unwrap_or(0);
    let start = len.saturating_sub(max);
    if file.seek(SeekFrom::Start(start)).is_err() {
        return Vec::new();
    }
    let mut buf = Vec::new();
    let _ = file.read_to_end(&mut buf);
    if start > 0 {
        match buf.iter().position(|b| *b == b'\n') {
            Some(i) => buf.drain(..=i),
            None => buf.drain(..),
        };
    }
    buf
}

fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        secure_chain(parent)?;
    }
    let temp = path.with_extension(format!("tmp-{}", proto::random_hex(4)));
    let result = open_with(file_opts().write(true).create_new(true), &temp).and_then(|mut file| {
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
    serde_json::from_slice(&read_bytes(path).ok()?).ok()
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

/// Whether a file's last byte is not a line break (a write was cut off).
fn ends_torn(path: &Path) -> bool {
    let Ok(mut file) = open_with(file_opts().read(true), path) else {
        return false;
    };
    let mut last = [0u8; 1];
    file.seek(SeekFrom::End(-1)).is_ok() && file.read_exact(&mut last).is_ok() && last[0] != b'\n'
}

/// Append `text` to a file, creating it private; a torn last line is closed first.
fn append_text(path: &Path, text: &str) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        secure_chain(parent)?;
    }
    let torn = ends_torn(path);
    let mut file = open_with(file_opts().create(true).append(true), path)?;
    if torn {
        file.write_all(b"\n")?;
    }
    file.write_all(text.as_bytes())?;
    file.sync_all()
}

// ---- locks -------------------------------------------------------------------

struct LockFile {
    #[cfg(unix)]
    _file: fs::File,
    #[cfg(not(unix))]
    path: PathBuf,
}

#[cfg(not(unix))]
impl Drop for LockFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

#[cfg(windows)]
fn pid_alive(pid: u32) -> bool {
    use ::windows::Win32::Foundation::{CloseHandle, WAIT_TIMEOUT};
    use ::windows::Win32::System::Threading::{
        OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, WaitForSingleObject,
    };
    // SAFETY: a plain OpenProcess; the handle is waited on once and closed.
    unsafe {
        match OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
            false,
            pid,
        ) {
            Ok(handle) => {
                let alive = WaitForSingleObject(handle, 0) == WAIT_TIMEOUT;
                let _ = CloseHandle(handle);
                alive
            }
            Err(_) => false,
        }
    }
}

/// Only a lock file that is old and whose recorded owner is gone may be stolen.
#[cfg(not(unix))]
fn lock_is_stale(path: &Path) -> bool {
    let age = fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok());
    let Some(age) = age else { return false };
    if age <= Duration::from_secs(10) {
        return false;
    }
    let owner = fs::read_to_string(path)
        .ok()
        .and_then(|t| t.trim().parse::<u32>().ok());
    match owner {
        #[cfg(windows)]
        Some(pid) => !pid_alive(pid),
        #[cfg(not(windows))]
        Some(_) => false,
        // Created but never stamped: its owner died between the two steps.
        None => age > Duration::from_secs(60),
    }
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

    #[cfg(unix)]
    fn lock(&self, target: &Path) -> io::Result<LockFile> {
        use std::os::fd::AsRawFd;
        let path = target.with_extension("lock");
        if let Some(parent) = path.parent() {
            secure_chain(parent)?;
        }
        let file = open_with(file_opts().write(true).create(true).truncate(false), &path)?;
        let started = Instant::now();
        loop {
            // SAFETY: `file` owns a valid descriptor for the whole call.
            let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
            if result == 0 {
                break;
            }
            let err = io::Error::last_os_error();
            let busy = err.kind() == io::ErrorKind::WouldBlock;
            if !busy && err.raw_os_error() != Some(libc::EINTR) {
                return Err(err);
            }
            if started.elapsed() > Duration::from_secs(3) {
                return Err(io::Error::new(io::ErrorKind::TimedOut, "inbox is busy"));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let _ = file.set_len(0);
        let _ = (&file).write_all(std::process::id().to_string().as_bytes());
        Ok(LockFile { _file: file })
    }

    #[cfg(not(unix))]
    fn lock(&self, target: &Path) -> io::Result<LockFile> {
        let path = target.with_extension("lock");
        if let Some(parent) = path.parent() {
            secure_chain(parent)?;
        }
        let started = Instant::now();
        loop {
            match open_with(file_opts().write(true).create_new(true), &path) {
                Ok(mut file) => {
                    let _ = file.write_all(std::process::id().to_string().as_bytes());
                    return Ok(LockFile { path });
                }
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                    if lock_is_stale(&path) {
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

    // ---- generic state files -------------------------------------------------

    /// A JSON file directly in the bridge folder (`name` is a plain file name).
    pub fn read_state<T: DeserializeOwned>(&self, name: &str) -> Option<T> {
        read_json(&self.root.join(safe_name(name)))
    }

    /// Read-modify-write a JSON file in the bridge folder under its lock.
    pub fn update_state<T, F>(&self, name: &str, change: F) -> io::Result<()>
    where
        T: Serialize + DeserializeOwned + Default,
        F: FnOnce(&mut T),
    {
        let path = self.root.join(safe_name(name));
        let _guard = self.lock(&path)?;
        let mut value: T = read_json(&path).unwrap_or_default();
        change(&mut value);
        write_json(&path, &value)
    }

    // ---- inboxes -----------------------------------------------------------

    fn inbox_path(&self, session: &str) -> PathBuf {
        self.root
            .join("inbox")
            .join(format!("{}.jsonl", session_stem(session)))
    }

    fn cursor_path(&self, session: &str) -> PathBuf {
        self.root
            .join("inbox")
            .join(format!("{}.cursor.json", session_stem(session)))
    }

    fn evicted_path(&self, session: &str) -> PathBuf {
        self.root
            .join("inbox")
            .join(format!("{}.evicted.jsonl", session_stem(session)))
    }

    /// Move a chat's files from the name older versions used to the current one.
    fn migrate_inbox(&self, session: &str) {
        let Some(old) = legacy_stem(session) else {
            return;
        };
        let dir = self.root.join("inbox");
        let new = session_stem(session);
        for suffix in [".jsonl", ".cursor.json"] {
            let from = dir.join(format!("{old}{suffix}"));
            let to = dir.join(format!("{new}{suffix}"));
            if from.exists() && !to.exists() {
                let _ = fs::rename(from, to);
            }
        }
    }

    fn read_cursor(&self, session: &str) -> Cursor {
        read_json(&self.cursor_path(session)).unwrap_or_default()
    }

    fn read_lines(path: &Path) -> Vec<InboxEntry> {
        read_bytes(path)
            .map(|bytes| {
                String::from_utf8_lossy(&bytes)
                    .lines()
                    .filter_map(|line| serde_json::from_str(line).ok())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The `seq` of the last complete line of an inbox file.
    fn last_seq(path: &Path) -> u64 {
        let tail = read_tail(path, 512 * 1024);
        String::from_utf8_lossy(&tail)
            .lines()
            .rev()
            .find_map(|line| serde_json::from_str::<InboxEntry>(line).ok())
            .map_or(0, |e| e.seq)
    }

    /// Add a message to a chat's inbox; returns its `seq`. When the file would
    /// pass the cap, the oldest messages are dropped first (and noted, see
    /// `inbox_evicted`). The `seq` follows the last committed line of the file.
    pub fn append_inbox(&self, session: &str, env: &Envelope) -> io::Result<u64> {
        self.migrate_inbox(session);
        let path = self.inbox_path(session);
        let _guard = self.lock(&path)?;
        let mut cursor = self.read_cursor(session);
        let seq = (Self::last_seq(&path) + 1).max(cursor.next).max(1);
        let entry = InboxEntry {
            seq,
            received: now_ms(),
            env: env.clone(),
        };
        let line = serde_json::to_string(&entry).map_err(io::Error::other)? + "\n";
        let size = fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        if size + line.len() as u64 > self.inbox_cap {
            let entries = Self::read_lines(&path);
            let mut kept: Vec<String> = entries
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
            let lost = entries
                .iter()
                .take(drop_count)
                .filter(|e| e.seq > cursor.read)
                .count();
            if drop_count > 0 {
                let note = serde_json::json!({
                    "ts": now_ms(),
                    "count": lost,
                    "dropped": drop_count,
                });
                let _ = append_text(&self.evicted_path(session), &format!("{note}\n"));
            }
        } else {
            append_text(&path, &line)?;
        }
        cursor.next = seq + 1;
        write_json(&self.cursor_path(session), &cursor)?;
        Ok(seq)
    }

    /// Every stored message of a chat, oldest first.
    pub fn read_inbox(&self, session: &str) -> Vec<InboxEntry> {
        self.migrate_inbox(session);
        Self::read_lines(&self.inbox_path(session))
    }

    /// Unread messages (received after `since`, ms, when given), marked read.
    pub fn take_unread(&self, session: &str, since: Option<u64>) -> io::Result<Vec<InboxEntry>> {
        self.take_unread_ack(session, since, true)
    }

    /// Unread messages (received after `since`, ms, when given). Only with `ack`
    /// are they marked read; otherwise they stay unread until `ack_inbox`.
    pub fn take_unread_ack(
        &self,
        session: &str,
        since: Option<u64>,
        ack: bool,
    ) -> io::Result<Vec<InboxEntry>> {
        self.migrate_inbox(session);
        let path = self.inbox_path(session);
        let _guard = self.lock(&path)?;
        let mut cursor = self.read_cursor(session);
        let entries = Self::read_lines(&path);
        let newest = entries.iter().map(|e| e.seq).max().unwrap_or(cursor.read);
        let unread: Vec<InboxEntry> = entries
            .into_iter()
            .filter(|e| e.seq > cursor.read && since.is_none_or(|s| e.received > s))
            .collect();
        if ack && newest > cursor.read {
            cursor.read = newest;
            cursor.next = cursor.next.max(newest + 1);
            write_json(&self.cursor_path(session), &cursor)?;
        }
        Ok(unread)
    }

    /// Mark every message up to and including `up_to_seq` read.
    pub fn ack_inbox(&self, session: &str, up_to_seq: u64) -> io::Result<()> {
        self.migrate_inbox(session);
        let path = self.inbox_path(session);
        let _guard = self.lock(&path)?;
        let mut cursor = self.read_cursor(session);
        if up_to_seq > cursor.read {
            cursor.read = up_to_seq;
            cursor.next = cursor.next.max(up_to_seq + 1);
            write_json(&self.cursor_path(session), &cursor)?;
        }
        Ok(())
    }

    /// How many messages of a chat are still unread.
    pub fn unread_count(&self, session: &str) -> usize {
        let read = self.read_cursor(session).read;
        self.read_inbox(session)
            .iter()
            .filter(|e| e.seq > read)
            .count()
    }

    /// Unread count plus how many unread messages the size cap has dropped.
    pub fn unread_detail(&self, session: &str) -> UnreadDetail {
        UnreadDetail {
            unread: self.unread_count(session),
            evicted: self.inbox_evicted(session),
        }
    }

    /// How many messages the inbox size cap dropped before they were read.
    pub fn inbox_evicted(&self, session: &str) -> u64 {
        read_bytes(&self.evicted_path(session))
            .map(|bytes| {
                String::from_utf8_lossy(&bytes)
                    .lines()
                    .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
                    .filter_map(|v| v["count"].as_u64())
                    .sum()
            })
            .unwrap_or(0)
    }

    // ---- linked computers --------------------------------------------------

    fn links_path(&self) -> PathBuf {
        self.root.join("links.json")
    }

    pub fn links(&self) -> Vec<Link> {
        read_json(&self.links_path()).unwrap_or_default()
    }

    pub fn save_links(&self, links: &[Link]) -> io::Result<()> {
        let path = self.links_path();
        let _guard = self.lock(&path)?;
        write_json(&path, &links)
    }

    // ---- registered chats ---------------------------

    fn session_file(&self, id: &str) -> PathBuf {
        self.root
            .join("sessions")
            .join(format!("{}.json", session_stem(id)))
    }

    pub fn register_session(&self, session: &RegisteredSession) -> io::Result<()> {
        write_json(&self.session_file(&session.id), session)?;
        if let Some(old) = legacy_stem(&session.id) {
            let _ = fs::remove_file(self.root.join("sessions").join(format!("{old}.json")));
        }
        Ok(())
    }

    /// Registered chats; files older versions wrote under another name still count.
    pub fn registered_sessions(&self) -> Vec<RegisteredSession> {
        json_files(&self.root.join("sessions"))
            .iter()
            .filter_map(|p| read_json::<RegisteredSession>(p))
            .collect()
    }

    pub fn forget_session(&self, id: &str) {
        let _ = fs::remove_file(self.session_file(id));
        if let Some(old) = legacy_stem(id) {
            let _ = fs::remove_file(self.root.join("sessions").join(format!("{old}.json")));
        }
    }

    /// A chat id for a working folder that has none from its host: made once
    /// and kept, so the same folder is the same chat next time.
    pub fn cwd_session_id(&self, cwd: &str, kind: &str) -> io::Result<String> {
        let path = self
            .root
            .join("sessions")
            .join(format!("cwd-{:016x}.id", fnv(cwd)));
        if let Ok(bytes) = read_bytes(&path) {
            let id = String::from_utf8_lossy(&bytes).trim().to_string();
            if !id.is_empty() {
                return Ok(id);
            }
        }
        let id = format!("{kind}-{}", proto::random_hex(8));
        write_atomic(&path, id.as_bytes())?;
        Ok(id)
    }

    // ---- duplicate journal -------------------------------------------------

    fn journal_path(&self) -> PathBuf {
        self.root.join("journal.jsonl")
    }

    /// Record `(origin_device, msg_id)` with its content hash (see `payload_hash`) and
    /// say whether it was seen before. The journal keeps the last `JOURNAL_CAP`
    /// entries. A journal that cannot be read or written reports `New`.
    pub fn journal_seen(&self, origin_device: &str, msg_id: &str, payload_hash: &str) -> Seen {
        let path = self.journal_path();
        let Ok(_guard) = self.lock(&path) else {
            return Seen::New;
        };
        let mut lines: Vec<JournalLine> = read_bytes(&path)
            .map(|bytes| {
                String::from_utf8_lossy(&bytes)
                    .lines()
                    .filter_map(|l| serde_json::from_str(l).ok())
                    .collect()
            })
            .unwrap_or_default();
        if let Some(hit) = lines
            .iter()
            .find(|l| l.o == origin_device && l.id == msg_id)
        {
            return if hit.h == payload_hash {
                Seen::Duplicate
            } else {
                Seen::Conflict
            };
        }
        let line = JournalLine {
            o: origin_device.to_string(),
            id: msg_id.to_string(),
            h: payload_hash.to_string(),
            ts: now_ms(),
        };
        let text = serde_json::to_string(&line).unwrap_or_default() + "\n";
        if lines.len() < JOURNAL_CAP {
            let _ = append_text(&path, &text);
        } else {
            lines.push(line);
            let skip = lines.len() - JOURNAL_CAP;
            let body: String = lines
                .iter()
                .skip(skip)
                .filter_map(|l| serde_json::to_string(l).ok())
                .map(|l| l + "\n")
                .collect();
            let _ = write_atomic(&path, body.as_bytes());
        }
        Seen::New
    }

    // ---- reply routes and the outbound queue -------------------------------

    fn route_path(&self, msg_id: &str) -> PathBuf {
        self.root
            .join("reply-routes")
            .join(format!("{}.json", session_stem(msg_id)))
    }

    /// Remember where a reply to delivered message `route.msg_id` goes.
    pub fn save_reply_route(&self, route: &ReplyRoute) -> io::Result<()> {
        write_json(&self.route_path(&route.msg_id), route)?;
        if let Ok(listing) = fs::read_dir(self.root.join("reply-routes")) {
            for entry in listing.flatten() {
                let old = entry
                    .metadata()
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| t.elapsed().ok())
                    .is_some_and(|age| age > ROUTE_TTL);
                if old {
                    let _ = fs::remove_file(entry.path());
                }
            }
        }
        Ok(())
    }

    /// The route saved for message `msg_id`, when there is one.
    pub fn reply_route(&self, msg_id: &str) -> Option<ReplyRoute> {
        read_json::<ReplyRoute>(&self.route_path(msg_id)).filter(|r| r.msg_id == msg_id)
    }

    /// Every saved route created at or after `since_ms`.
    pub fn reply_routes_since(&self, since_ms: u64) -> Vec<ReplyRoute> {
        let mut routes: Vec<ReplyRoute> = fs::read_dir(self.root.join("reply-routes"))
            .map(|listing| {
                listing
                    .flatten()
                    .filter_map(|e| read_json::<ReplyRoute>(&e.path()))
                    .filter(|r| r.created_ms >= since_ms)
                    .collect()
            })
            .unwrap_or_default();
        routes.sort_by_key(|r| r.created_ms);
        routes
    }

    fn outbound_dir(&self) -> PathBuf {
        self.root.join("outbound")
    }

    /// Keep an envelope that could not be sent so the hub can retry it.
    pub fn queue_outbound(&self, env: &Envelope) -> io::Result<()> {
        let dir = self.outbound_dir();
        let count = fs::read_dir(&dir).map(|l| l.flatten().count()).unwrap_or(0);
        if count >= OUTBOUND_CAP {
            return Err(io::Error::other("the outbound queue is full"));
        }
        write_json(&dir.join(format!("{:016x}.json", fnv(&env.id))), env)
    }

    /// Take every queued envelope, oldest first (the files are removed; queue again
    /// the ones that still cannot be sent).
    pub fn take_outbound(&self) -> Vec<Envelope> {
        let mut out: Vec<Envelope> = Vec::new();
        for file in json_files(&self.outbound_dir()) {
            if let Some(env) = read_json::<Envelope>(&file) {
                out.push(env);
            }
            let _ = fs::remove_file(&file);
        }
        out.sort_by_key(|e| e.ts);
        out
    }

    // ---- relay heartbeat ---------------------------------------------------

    fn relay_path(&self) -> PathBuf {
        self.root.join("relay.json")
    }

    pub fn write_relay_status(&self, alias: &str) -> io::Result<()> {
        write_json(
            &self.relay_path(),
            &RelayStatus {
                pid: std::process::id(),
                ts: now_ms(),
                alias: alias.to_string(),
            },
        )
    }

    /// Forget the heartbeat (the hub's bridge stopped).
    pub fn clear_relay_status(&self) {
        let _ = fs::remove_file(self.relay_path());
    }

    pub fn relay_status(&self) -> Option<RelayStatus> {
        read_json(&self.relay_path())
    }

    /// Whether the hub's bridge is running.
    pub fn relay_alive(&self) -> bool {
        self.relay_status()
            .is_some_and(|s| now_ms().saturating_sub(s.ts) < RELAY_STALE_MS)
    }

    // ---- the bridge switch -------------------------------------------------

    fn policy_path(&self) -> PathBuf {
        self.root.join("policy.json")
    }

    /// Whether the bridge is on here (`policy.json`; on when there is no file).
    pub fn bridge_enabled(&self) -> bool {
        read_json::<serde_json::Value>(&self.policy_path())
            .and_then(|v| v["enabled"].as_bool())
            .unwrap_or(true)
    }

    /// Turn the bridge on or off on this computer.
    pub fn set_bridge_enabled(&self, on: bool) -> io::Result<()> {
        let path = self.policy_path();
        let _guard = self.lock(&path)?;
        write_json(&path, &serde_json::json!({"enabled": on}))
    }

    // ---- activity ----------------------------------------------------------

    fn activity_path(&self) -> PathBuf {
        self.root.join("activity.json")
    }

    /// The last send and receive on this computer (zeros when there were none).
    pub fn activity(&self) -> Activity {
        read_json(&self.activity_path()).unwrap_or_default()
    }

    /// A bridge message left this computer (or went between two chats here);
    /// `outcome` is its receipt state word.
    pub fn note_sent(&self, outcome: &str) {
        self.note_activity(true, outcome);
    }

    /// A bridge message arrived from a linked computer; `outcome` as for `note_sent`.
    pub fn note_received(&self, outcome: &str) {
        self.note_activity(false, outcome);
    }

    fn note_activity(&self, sent: bool, outcome: &str) {
        let path = self.activity_path();
        let Ok(_lock) = self.lock(&path) else { return };
        let mut activity = self.activity();
        if sent {
            activity.sent_ms = now_ms();
            activity.sent += 1;
        } else {
            activity.received_ms = now_ms();
            activity.received += 1;
        }
        activity.last_outcome = outcome.to_string();
        let _ = write_json(&path, &activity);
    }
}
