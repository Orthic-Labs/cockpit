//! Push a bridged message into a Codex thread on this computer, as far as the
//! bundled Codex CLI allows.
//!
//! `codex queue --thread <id> --message <text>` ("Queue a message for an
//! existing session", in the Codex CLI bundled with the ChatGPT app) delivers
//! into a thread the Codex desktop app owns; the thread processes it and can
//! reply. It runs with a 5 s limit and no terminal or console window; stdout and
//! stderr are drained concurrently into 256 KiB buffers. Exit 0 with stdout
//! "Queued message <id> for thread <id>." is a `queued` receipt carrying that
//! message id (the thread may not have consumed it yet); exit 0 without that
//! line, or a timeout, is `unknown`; a nonzero exit is `refused` with Codex's
//! stderr. A Codex without `queue` (probed once per binary path and mtime with
//! `queue --help`, see `codex_capability`) or without a CLI is `unsupported`.
//! `codex queue` takes the text only as `--message <TEXT>` (no stdin form), so a message
//! longer than the command line allows (8000 bytes on Windows, 100 000 elsewhere) is
//! `unsupported` ("message too long for Codex's command line").
//!
//! Where the CLI is looked for:
//! * `$PULSE_CODEX_BIN`, when it names a file, wins everywhere.
//! * macOS: `/Applications/ChatGPT.app/Contents/Resources/codex-cli/bin/codex`
//!   (verified), then `/opt/homebrew/bin/codex`, `/usr/local/bin/codex`, `codex` on PATH.
//! * Windows (not verified on a real install; best guesses): `codex.exe` /
//!   `codex.cmd` on PATH, `%LOCALAPPDATA%\Programs\ChatGPT\resources\codex-cli\bin\codex.exe`,
//!   `%LOCALAPPDATA%\Programs\Codex\resources\codex-cli\bin\codex.exe`,
//!   `%LOCALAPPDATA%\OpenAI\Codex\bin\codex.exe`. A packaged (WindowsApps)
//!   install is not readable and is not searched.
//!
//! Thread discovery: the highest `~/.codex/state_<N>.sqlite` (table `threads`,
//! opened read-only; the Codex app holds it open in WAL mode), not archived,
//! newest first. Only when no state database exists does it fall back to
//! `~/.codex/session_index.jsonl` (one `{id, thread_name, updated_at}` per
//! line), which lags and has no archived flag. Failures are explicit
//! (`DiscoveryError`, `codex_discovery_status`); the last good list is kept with its
//! age. `find_thread_by_id` resolves one id exactly, archived included. The roster lists
//! the most recent threads as reachable peers (`roster::local_sessions`).

use super::deliver_claude::shim;
use super::envelope::plain_label;
use super::{BridgeError, Envelope, LocalSession, Receipt};
use serde::Serialize;
use serde_json::Value;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, mpsc};
use std::time::{Duration, Instant, SystemTime};

const QUEUE_TIMEOUT: Duration = Duration::from_secs(5);
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);
const INDEX_TAIL_BYTES: u64 = 512 * 1024;
/// Most bytes kept from each of Codex's stdout and stderr; the rest is drained and dropped.
const CAPTURE_CAP: usize = 256 * 1024;
/// The longest message `codex queue --message` is given: Windows command lines are
/// short, other systems allow far more.
#[cfg(windows)]
const ARGV_LIMIT: usize = 8_000;
#[cfg(not(windows))]
const ARGV_LIMIT: usize = 100_000;

fn home() -> PathBuf {
    #[cfg(windows)]
    let variable = "USERPROFILE";
    #[cfg(not(windows))]
    let variable = "HOME";
    std::env::var_os(variable)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

/// `~/.codex`, or `$CODEX_HOME`.
pub fn codex_home() -> PathBuf {
    match std::env::var_os("CODEX_HOME").filter(|v| !v.is_empty()) {
        Some(dir) => PathBuf::from(dir),
        None => home().join(".codex"),
    }
}

/// The Codex CLI, if this computer has one: `$PULSE_CODEX_BIN`, else the first known
/// install location or PATH entry.
pub fn codex_binary() -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(chosen) = std::env::var_os("PULSE_CODEX_BIN").filter(|v| !v.is_empty()) {
        candidates.push(PathBuf::from(chosen));
    }
    #[cfg(target_os = "macos")]
    {
        candidates.push(PathBuf::from(
            "/Applications/ChatGPT.app/Contents/Resources/codex-cli/bin/codex",
        ));
        candidates.push(PathBuf::from("/opt/homebrew/bin/codex"));
        candidates.push(PathBuf::from("/usr/local/bin/codex"));
    }
    #[cfg(windows)]
    {
        if let Some(local) = std::env::var_os("LOCALAPPDATA").map(PathBuf::from) {
            candidates.push(local.join(r"Programs\ChatGPT\resources\codex-cli\bin\codex.exe"));
            candidates.push(local.join(r"Programs\Codex\resources\codex-cli\bin\codex.exe"));
            candidates.push(local.join(r"OpenAI\Codex\bin\codex.exe"));
        }
    }
    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            let names: &[&str] = if cfg!(windows) {
                &["codex.exe", "codex.cmd"]
            } else {
                &["codex"]
            };
            for name in names {
                candidates.push(dir.join(name));
            }
        }
    }
    candidates.into_iter().find(|p| p.is_file())
}

// ---- running Codex ---------------------------------------------------------------

/// What a finished Codex process left.
struct Captured {
    success: bool,
    status: String,
    stdout: String,
    stderr: String,
}

enum RunFailure {
    /// Codex never ran.
    NotStarted(String),
    /// Codex ran but its outcome is not known (timeout, lost watch).
    Ambiguous(String),
}

/// Read `source` to the end on its own thread, keeping at most `CAPTURE_CAP` bytes.
fn drain(mut source: impl Read + Send + 'static) -> mpsc::Receiver<Vec<u8>> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut kept: Vec<u8> = Vec::new();
        let mut chunk = [0u8; 8192];
        loop {
            match source.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let room = CAPTURE_CAP.saturating_sub(kept.len());
                    kept.extend_from_slice(&chunk[..n.min(room)]);
                }
            }
        }
        let _ = tx.send(kept);
    });
    rx
}

/// Run `binary args` with no console window and no stdin, stdout and stderr
/// drained concurrently, and a kill at `timeout`.
fn run_capture(binary: &Path, args: &[String], timeout: Duration) -> Result<Captured, RunFailure> {
    let mut command = Command::new(binary);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = command
        .spawn()
        .map_err(|e| RunFailure::NotStarted(format!("Couldn't start Codex: {}", e.kind())))?;
    let out = child.stdout.take().map(drain);
    let err = child.stderr.take().map(drain);
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(25));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(RunFailure::Ambiguous(format!(
                    "Codex didn't answer within {} seconds.",
                    timeout.as_secs()
                )));
            }
            Err(e) => {
                return Err(RunFailure::Ambiguous(format!(
                    "Couldn't watch Codex: {}",
                    e.kind()
                )));
            }
        }
    };
    let collect = |rx: Option<mpsc::Receiver<Vec<u8>>>| {
        rx.and_then(|rx| rx.recv_timeout(Duration::from_secs(1)).ok())
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
            .unwrap_or_default()
    };
    Ok(Captured {
        success: status.success(),
        status: status.to_string(),
        stdout: collect(out),
        stderr: collect(err),
    })
}

// ---- capability -------------------------------------------------------------------

/// What the chosen Codex binary can do, probed once per path and mtime.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CodexCapability {
    /// The binary chosen, when there is one.
    pub path: Option<String>,
    /// The first line of `codex --version`, when it answered.
    pub version: Option<String>,
    /// `codex queue --help` succeeded and mentions threads.
    pub queue_supported: bool,
    /// Why probing failed, when it did.
    pub error: Option<String>,
}

type ProbeCache = Mutex<Option<(PathBuf, Option<SystemTime>, CodexCapability)>>;
static CAPABILITY: ProbeCache = Mutex::new(None);

fn probe(binary: &Path) -> CodexCapability {
    let mut found = CodexCapability {
        path: Some(binary.to_string_lossy().into_owned()),
        version: None,
        queue_supported: false,
        error: None,
    };
    let run = |args: &[&str]| {
        let args: Vec<String> = args.iter().map(|a| (*a).to_string()).collect();
        run_capture(binary, &args, PROBE_TIMEOUT)
    };
    if let Ok(done) = run(&["--version"])
        && done.success
    {
        found.version = done.stdout.lines().next().map(|l| l.trim().to_string());
    }
    match run(&["queue", "--help"]) {
        Ok(done) if done.success => {
            let help = format!("{}\n{}", done.stdout, done.stderr).to_ascii_lowercase();
            found.queue_supported = help.contains("thread");
            if !found.queue_supported {
                found.error = Some("`codex queue --help` doesn't mention threads.".to_string());
            }
        }
        Ok(done) => {
            found.error = Some(format!("`codex queue --help` failed ({}).", done.status));
        }
        Err(RunFailure::NotStarted(e) | RunFailure::Ambiguous(e)) => found.error = Some(e),
    }
    found
}

/// The Codex CLI's abilities, cached per binary path and modification time.
pub fn codex_capability() -> CodexCapability {
    let Some(binary) = codex_binary() else {
        return CodexCapability {
            path: None,
            version: None,
            queue_supported: false,
            error: Some("No Codex CLI found; install Codex or set PULSE_CODEX_BIN.".to_string()),
        };
    };
    let mtime = std::fs::metadata(&binary).and_then(|m| m.modified()).ok();
    let mut cache = CAPABILITY.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((path, stamp, known)) = cache.as_ref()
        && *path == binary
        && *stamp == mtime
    {
        return known.clone();
    }
    let fresh = probe(&binary);
    *cache = Some((binary, mtime, fresh.clone()));
    fresh
}

// ---- thread discovery ------------------------------------------------------------

/// A Codex thread listed in the session index.
#[derive(Clone, Debug)]
pub struct CodexThread {
    pub id: String,
    pub name: String,
    pub updated_at: String,
    pub cwd: String,
    /// Milliseconds since the epoch of the last write (0 when unknown).
    pub updated_ms: u64,
    /// The thread is archived (only the state database knows).
    pub archived: bool,
}

/// Why Codex threads could not be listed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DiscoveryError {
    /// Neither a state database nor a session index exists.
    #[error("no Codex state database or session index was found")]
    NoSource,
    /// The state database or index could not be read (locked, corrupt, changed schema).
    #[error("couldn't read Codex's threads: {0}")]
    Unreadable(String),
}

const THREAD_COLUMNS: &str =
    "id, name, title, first_user_message, preview, cwd, updated_at_ms, archived";

/// The highest-numbered `state_<N>.sqlite` in the Codex home.
fn state_db() -> Option<PathBuf> {
    std::fs::read_dir(codex_home())
        .ok()?
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let n = name
                .strip_prefix("state_")?
                .strip_suffix(".sqlite")?
                .parse::<u64>()
                .ok()?;
            Some((n, e.path()))
        })
        .max_by_key(|(n, _)| *n)
        .map(|(_, p)| p)
}

fn display_name(name: &str, title: &str, first: &str, preview: &str) -> String {
    for candidate in [name, title, first, preview] {
        let line = candidate.split_whitespace().collect::<Vec<_>>().join(" ");
        if !line.is_empty() {
            return line.chars().take(60).collect();
        }
    }
    String::new()
}

fn thread_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<CodexThread> {
    let text = |i: usize| -> rusqlite::Result<String> {
        Ok(row.get::<_, Option<String>>(i)?.unwrap_or_default())
    };
    let ms = row.get::<_, Option<i64>>(6)?.unwrap_or(0).max(0) as u64;
    let archived = row.get::<_, Option<i64>>(7)?.unwrap_or(0) != 0;
    Ok(CodexThread {
        id: text(0)?,
        name: display_name(&text(1)?, &text(2)?, &text(3)?, &text(4)?),
        updated_at: String::new(),
        cwd: text(5)?,
        updated_ms: ms,
        archived,
    })
}

fn open_state(path: &Path) -> Result<rusqlite::Connection, rusqlite::Error> {
    use rusqlite::{Connection, OpenFlags};
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    conn.busy_timeout(Duration::from_millis(250))?;
    Ok(conn)
}

fn read_state(path: &Path, limit: usize) -> Result<Vec<CodexThread>, rusqlite::Error> {
    let conn = open_state(path)?;
    let sql = format!(
        "SELECT {THREAD_COLUMNS} FROM threads WHERE archived = 0 \
         ORDER BY updated_at_ms DESC LIMIT ?1"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([i64::try_from(limit).unwrap_or(i64::MAX)], thread_from_row)?;
    Ok(rows
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .filter(|t| !t.id.is_empty())
        .collect())
}

/// One thread by exact id, archived or not, whatever the list limit.
fn read_state_by_id(path: &Path, id: &str) -> Result<Option<CodexThread>, rusqlite::Error> {
    use rusqlite::OptionalExtension;
    let conn = open_state(path)?;
    let sql = format!("SELECT {THREAD_COLUMNS} FROM threads WHERE id = ?1");
    conn.query_row(&sql, [id], thread_from_row).optional()
}

/// What the last discovery served and how it went.
struct Discovery {
    /// "state_db", "index" or "cache".
    source: &'static str,
    /// The state database the cached list came from.
    db: Option<PathBuf>,
    good: Vec<CodexThread>,
    good_at: Option<Instant>,
    error: Option<String>,
}

static DISCOVERY: Mutex<Discovery> = Mutex::new(Discovery {
    source: "none",
    db: None,
    good: Vec::new(),
    good_at: None,
    error: None,
});

/// `(source, age_ms, error)` of the last thread listing: where the threads came from
/// ("state_db", "index", "cache" for the last good list after a failure, or "none"), how
/// old that data is, and the last error, if any.
pub fn codex_discovery_status() -> (String, u64, Option<String>) {
    let state = DISCOVERY.lock().unwrap_or_else(|e| e.into_inner());
    let age = state.good_at.map_or(0, |at| {
        u64::try_from(at.elapsed().as_millis()).unwrap_or(u64::MAX)
    });
    (state.source.to_string(), age, state.error.clone())
}

/// The most recently updated threads (newest first, at most `limit`), or why they
/// can't be listed. After a read failure the last good list for the same database is
/// returned (see `codex_discovery_status` for its age and the error).
pub fn try_list_threads(limit: usize) -> Result<Vec<CodexThread>, DiscoveryError> {
    let mut state = DISCOVERY.lock().unwrap_or_else(|e| e.into_inner());
    let Some(db) = state_db() else {
        return match list_threads_from_index(limit) {
            Ok(threads) => {
                state.source = "index";
                state.error = None;
                state.good = threads.clone();
                state.good_at = Some(Instant::now());
                state.db = None;
                Ok(threads)
            }
            Err(e) => {
                state.source = "none";
                state.error = Some(e.to_string());
                Err(e)
            }
        };
    };
    match read_state(&db, limit) {
        Ok(threads) => {
            state.source = "state_db";
            state.error = None;
            state.good = threads.clone();
            state.good_at = Some(Instant::now());
            state.db = Some(db);
            Ok(threads)
        }
        Err(e) => {
            let error = DiscoveryError::Unreadable(e.to_string());
            state.error = Some(error.to_string());
            if state.db.as_ref() == Some(&db) && state.good_at.is_some() {
                state.source = "cache";
                return Ok(state.good.iter().take(limit).cloned().collect());
            }
            state.source = "none";
            Err(error)
        }
    }
}

/// `try_list_threads`, empty on failure; the failure is in `codex_discovery_status`.
pub fn list_threads(limit: usize) -> Vec<CodexThread> {
    try_list_threads(limit).unwrap_or_default()
}

/// One thread by its exact id, archived threads included, independent of how many
/// threads are listed. `Ok(None)` means the sources were read and the id is not there.
pub fn find_thread_by_id(id: &str) -> Result<Option<CodexThread>, DiscoveryError> {
    if id.is_empty() {
        return Ok(None);
    }
    match state_db() {
        Some(db) => {
            read_state_by_id(&db, id).map_err(|e| DiscoveryError::Unreadable(e.to_string()))
        }
        None => Ok(list_threads_from_index(usize::MAX)?
            .into_iter()
            .find(|t| t.id == id)),
    }
}

fn list_threads_from_index(limit: usize) -> Result<Vec<CodexThread>, DiscoveryError> {
    let path = codex_home().join("session_index.jsonl");
    let mut file = std::fs::File::open(&path).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => DiscoveryError::NoSource,
        kind => DiscoveryError::Unreadable(format!("session index: {kind}")),
    })?;
    let length = file.metadata().map(|m| m.len()).unwrap_or(0);
    if length > INDEX_TAIL_BYTES {
        use std::io::{Seek, SeekFrom};
        file.seek(SeekFrom::Start(length - INDEX_TAIL_BYTES))
            .map_err(|e| DiscoveryError::Unreadable(format!("session index: {}", e.kind())))?;
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|e| DiscoveryError::Unreadable(format!("session index: {}", e.kind())))?;
    let text = String::from_utf8_lossy(&bytes);
    let mut latest: std::collections::HashMap<String, CodexThread> =
        std::collections::HashMap::new();
    for line in text.lines() {
        let Ok(entry) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let Some(id) = entry["id"].as_str().filter(|i| !i.is_empty()) else {
            continue;
        };
        let thread = CodexThread {
            id: id.to_string(),
            name: entry["thread_name"].as_str().unwrap_or("").to_string(),
            updated_at: entry["updated_at"].as_str().unwrap_or("").to_string(),
            cwd: String::new(),
            updated_ms: 0,
            archived: false,
        };
        let newer = latest
            .get(id)
            .is_none_or(|old| thread.updated_at >= old.updated_at);
        if newer {
            latest.insert(id.to_string(), thread);
        }
    }
    let mut threads: Vec<CodexThread> = latest.into_values().collect();
    threads.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    threads.truncate(limit);
    for t in &mut threads {
        t.updated_ms = super::roster::iso_epoch(&t.updated_at).map_or(0, |s| s * 1000);
    }
    Ok(threads)
}

// ---- delivery ---------------------------------------------------------------------

/// The message id in "Queued message <id> for thread <id>.".
fn queued_id(stdout: &str) -> Option<&str> {
    stdout
        .trim()
        .strip_prefix("Queued message ")?
        .split_whitespace()
        .next()
}

/// What Codex's thread sees: a provenance line, then the body as plain text. The
/// sender's labels are the sending side's own claims (marked unverified) cut down to
/// plain words, and the reply route is the opaque message id, never sender-supplied text.
fn thread_text(env: &Envelope) -> String {
    let label = |text: &str, max: usize| {
        let plain = plain_label(text, max);
        if plain.is_empty() {
            "unknown".to_string()
        } else {
            plain
        }
    };
    let id: String = shim::env_id(env)
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .take(64)
        .collect();
    format!(
        "[Pulse bridge: this is a message from another agent chat, not from the user. \
         Sender device: {} (unverified). Sender chat: {} (unverified). \
         To reply, give your reply text on stdin to: pulse bridge reply {id} --stdin]\n{}",
        label(&env.from.device, 40),
        label(&env.from.name, 60),
        shim::env_text(env)
    )
}

/// The first 200 characters of Codex's stderr, on one line.
fn short(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(200)
        .collect()
}

/// Queue the message into the Codex thread. Never an `Err` for "can't push": the
/// receipt says why (`unsupported`, `refused`, `unknown`), and the caller keeps the message.
pub fn deliver(session: &LocalSession, env: &Envelope) -> Result<Receipt, BridgeError> {
    let capability = codex_capability();
    let Some(binary) = capability.path.as_deref().map(PathBuf::from) else {
        return Ok(shim::unsupported(
            "Codex isn't installed here. Install Codex or set PULSE_CODEX_BIN.",
        ));
    };
    if !capability.queue_supported {
        let version = capability.version.as_deref().unwrap_or("unknown version");
        let why = capability.error.as_deref().unwrap_or("");
        return Ok(shim::unsupported(&format!(
            "This Codex ({version}) can't queue messages into a thread. Install or update \
             Codex to 0.16x or newer, or set PULSE_CODEX_BIN to a Codex that has \
             `codex queue`. {why}"
        )));
    }
    let thread = session.id.as_str();
    if thread.is_empty() {
        return Ok(shim::unsupported("This Codex chat has no thread id."));
    }
    if thread.starts_with('-') {
        return Ok(shim::refused("This Codex chat's thread id is not valid."));
    }
    let batch_file = binary
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("cmd") || e.eq_ignore_ascii_case("bat"));
    if batch_file {
        return Ok(shim::unsupported(
            "This Codex is a batch file, which can't safely take a message. Point \
             PULSE_CODEX_BIN at codex.exe.",
        ));
    }
    let text = thread_text(env);
    if text.len() > ARGV_LIMIT {
        return Ok(shim::unsupported(&format!(
            "message too long for Codex's command line (limit {ARGV_LIMIT} bytes)"
        )));
    }
    let args = [
        "queue".to_string(),
        "--thread".to_string(),
        thread.to_string(),
        "--message".to_string(),
        text,
    ];
    Ok(match run_capture(&binary, &args, QUEUE_TIMEOUT) {
        Ok(done) if done.success => match queued_id(&done.stdout) {
            Some(id) => shim::queued(&format!(
                "Queued in the Codex thread (message {id}); not confirmed read by the thread."
            )),
            None => shim::unknown(
                "Codex exited 0 but did not print the expected \"Queued message\" line, so \
                 the message may not be queued.",
            ),
        },
        Ok(done) => {
            let detail = short(&done.stderr);
            shim::refused(&if detail.is_empty() {
                format!("Codex couldn't queue the message ({}).", done.status)
            } else {
                format!("Codex couldn't queue the message: {detail}")
            })
        }
        Err(RunFailure::NotStarted(reason)) => shim::unsupported(&reason),
        Err(RunFailure::Ambiguous(reason)) => shim::unknown(&format!(
            "{reason} The message may or may not have been queued."
        )),
    })
}
