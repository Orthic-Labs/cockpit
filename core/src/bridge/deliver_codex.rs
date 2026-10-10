//! Push a bridged message into a Codex thread on this computer, as far as the
//! bundled Codex CLI allows.
//!
//! `codex queue --thread <id> --message <text>` ("Queue a message for an
//! existing session", in the Codex CLI bundled with the ChatGPT app) delivers
//! into a thread the Codex desktop app owns; the thread processes it and can
//! reply. It runs with a 5 s limit and no terminal or console window. Exit 0
//! (stdout "Queued message <id> for thread <id>.") is a `delivered` receipt
//! carrying that message id. If the CLI can't be found, fails or times out the
//! receipt is `held` with the reason, and the message stays in the bridge
//! inbox (`pulse bridge inbox`).
//!
//! Where the CLI is looked for:
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
//! line), which lags and has no archived flag. The roster lists the most recent
//! threads as reachable peers (`roster::local_sessions`).

use super::deliver_claude::shim;
use super::{BridgeError, Envelope, LocalSession, Receipt};
use serde_json::Value;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const QUEUE_TIMEOUT: Duration = Duration::from_secs(5);
const INDEX_TAIL_BYTES: u64 = 512 * 1024;

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

/// The Codex CLI, if this computer has one.
pub fn codex_binary() -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
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

/// A Codex thread listed in the session index.
#[derive(Clone, Debug)]
pub struct CodexThread {
    pub id: String,
    pub name: String,
    pub updated_at: String,
    pub cwd: String,
    /// Milliseconds since the epoch of the last write (0 when unknown).
    pub updated_ms: u64,
}

/// The SQL read from the Codex state database.
const THREADS_SQL: &str = "SELECT id, name, title, first_user_message, preview, cwd, updated_at_ms \
     FROM threads WHERE archived = 0 ORDER BY updated_at_ms DESC LIMIT ?1";

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

fn read_state(path: &Path, limit: usize) -> Result<Vec<CodexThread>, rusqlite::Error> {
    use rusqlite::{Connection, OpenFlags};
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    conn.busy_timeout(Duration::from_millis(250))?;
    let mut stmt = conn.prepare(THREADS_SQL)?;
    let rows = stmt.query_map([i64::try_from(limit).unwrap_or(i64::MAX)], |row| {
        let text = |i: usize| -> rusqlite::Result<String> {
            Ok(row.get::<_, Option<String>>(i)?.unwrap_or_default())
        };
        let ms = row.get::<_, Option<i64>>(6)?.unwrap_or(0).max(0) as u64;
        Ok(CodexThread {
            id: text(0)?,
            name: display_name(&text(1)?, &text(2)?, &text(3)?, &text(4)?),
            updated_at: String::new(),
            cwd: text(5)?,
            updated_ms: ms,
        })
    })?;
    Ok(rows
        .filter_map(Result::ok)
        .filter(|t| !t.id.is_empty())
        .collect())
}

static LAST_GOOD: std::sync::Mutex<Option<(PathBuf, Vec<CodexThread>)>> =
    std::sync::Mutex::new(None);

/// The most recently updated threads (newest first, at most `limit`).
pub fn list_threads(limit: usize) -> Vec<CodexThread> {
    let Some(db) = state_db() else {
        return list_threads_from_index(limit);
    };
    let mut last = LAST_GOOD.lock().unwrap_or_else(|e| e.into_inner());
    match read_state(&db, limit) {
        Ok(threads) => {
            *last = Some((db, threads.clone()));
            threads
        }
        // Locked or busy: the last good list for this database, else nothing.
        Err(_) => match last.as_ref() {
            Some((path, threads)) if *path == db => threads.iter().take(limit).cloned().collect(),
            _ => Vec::new(),
        },
    }
}

fn list_threads_from_index(limit: usize) -> Vec<CodexThread> {
    let path = codex_home().join("session_index.jsonl");
    let Ok(mut file) = std::fs::File::open(&path) else {
        return Vec::new();
    };
    let length = file.metadata().map(|m| m.len()).unwrap_or(0);
    if length > INDEX_TAIL_BYTES {
        use std::io::{Seek, SeekFrom};
        if file
            .seek(SeekFrom::Start(length - INDEX_TAIL_BYTES))
            .is_err()
        {
            return Vec::new();
        }
    }
    let mut bytes = Vec::new();
    if file.read_to_end(&mut bytes).is_err() {
        return Vec::new();
    }
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
    threads
}

fn run_queue(binary: &Path, thread: &str, message: &str) -> Result<String, String> {
    let mut command = Command::new(binary);
    command
        .args(["queue", "--thread", thread, "--message", message])
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
        .map_err(|e| format!("Couldn't start Codex: {}", e.kind()))?;
    let deadline = Instant::now() + QUEUE_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if status.success() {
                    let mut out = String::new();
                    if let Some(mut stdout) = child.stdout.take() {
                        let _ = stdout.by_ref().take(2000).read_to_string(&mut out);
                    }
                    return Ok(out.trim().to_string());
                }
                let mut detail = String::new();
                if let Some(mut err) = child.stderr.take() {
                    let _ = err.by_ref().take(2000).read_to_string(&mut detail);
                }
                let detail: String = detail
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
                    .chars()
                    .take(200)
                    .collect();
                return Err(if detail.is_empty() {
                    format!("Codex couldn't queue the message ({status}).")
                } else {
                    format!("Codex couldn't queue the message: {detail}")
                });
            }
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("Codex didn't answer within 5 seconds.".to_string());
            }
            Err(e) => return Err(format!("Couldn't watch Codex: {}", e.kind())),
        }
    }
}

/// The message id in "Queued message <id> for thread <id>.".
fn queued_id(stdout: &str) -> Option<&str> {
    stdout
        .strip_prefix("Queued message ")?
        .split_whitespace()
        .next()
}

/// Queue the message into the Codex thread. Never an `Err` for "can't push":
/// that is a `held` receipt, and the message stays in the inbox.
pub fn deliver(session: &LocalSession, env: &Envelope) -> Result<Receipt, BridgeError> {
    let Some(binary) = codex_binary() else {
        return Ok(shim::unsupported(
            "Codex isn't installed here, so the message waits in the inbox.",
        ));
    };
    let thread = session.id.as_str();
    if thread.is_empty() {
        return Ok(shim::unsupported(
            "This Codex chat has no thread id, so the message waits in the inbox.",
        ));
    }
    let text = format!(
        "[Message from {name} via Pulse. Reply with: pulse bridge send \"{name}\" \"<text>\"]\n{body}",
        name = shim::env_from_name(env),
        body = shim::env_text(env)
    );
    Ok(match run_queue(&binary, thread, &text) {
        Ok(out) => match queued_id(&out) {
            Some(id) => shim::delivered(&format!("Queued in the Codex thread (message {id}).")),
            None => shim::delivered("Queued in the Codex thread."),
        },
        Err(reason) => shim::unsupported(&format!("{reason} The message waits in the inbox.")),
    })
}
