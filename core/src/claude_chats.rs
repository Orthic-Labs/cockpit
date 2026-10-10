//! The Claude Desktop Code chats that were running, so "Restart Claude and sync chats" can
//! open them again afterwards. Quitting Desktop (or switching account in it) ends every
//! chat's CLI, and Desktop only starts one again when its page is shown. `remember` notes the
//! running Desktop-hosted chats from `~/.claude/sessions/*.json` (the notch calls it now and
//! then, and just before a restart); `reopen` shows each of the last running set once with
//! Desktop's own `claude://code/continue?session=local_<id>` link. Showing a chat starts its
//! CLI with `--resume`; nothing is sent to it, so no tokens are used. A chat that was mid-reply
//! at the quit is not shown: Desktop would send it "continue". Notes in docs/claude-account-switch.md.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::bridge::roster::{claude_sessions_dir, claude_sessions_in, live_pids};
use crate::claude_sync::{self as sync, SyncError};

const FILE: &str = "claude-open-chats.json";
/// A chat not seen running for this long is forgotten (Desktop's own resume limit).
const FORGET_AFTER_MS: u64 = 24 * 60 * 60 * 1000;
/// Chats seen within this long of the newest sighting were running together.
const SAME_SET_MS: u64 = 3 * 60 * 1000;
/// A set last seen longer ago than this is not reopened: those chats were closed on purpose,
/// not by a restart. When the account has changed since, the set was ended by the switch
/// (sign out, sign in, then the restart button, however long that took) and is reopened
/// for up to a day.
const RECENT_MS: u64 = 30 * 60 * 1000;
/// The first chat waits for Desktop to start and sign in; the rest only for their CLI.
const FIRST_WAIT: Duration = Duration::from_secs(60);
const NEXT_WAIT: Duration = Duration::from_secs(25);
/// After a chat's CLI is up, its page stays shown a moment: a newer link replaces an older
/// one, and a page hidden before its start slot comes round is not started.
const DWELL: Duration = Duration::from_secs(3);

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct OpenChat {
    /// Desktop's record id, `local_<uuid>`; stable across the CLI's own session ids.
    pub host_session_id: String,
    pub name: String,
    pub cwd: String,
    pub last_seen_ms: u64,
    /// The account Desktop was signed in to when the chat was last seen running.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
}

#[derive(Default, Serialize, Deserialize)]
struct Remembered {
    chats: BTreeMap<String, OpenChat>,
}

pub fn default_file() -> Result<PathBuf, SyncError> {
    let backups = sync::default_backups()?;
    Ok(backups.parent().unwrap_or(&backups).join(FILE))
}

fn load(file: &Path) -> Remembered {
    fs::read(file)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn save(file: &Path, remembered: &Remembered) -> Result<(), SyncError> {
    let io = |e: std::io::Error| SyncError::Io(format!("{}: {e}", file.display()));
    if let Some(dir) = file.parent() {
        fs::create_dir_all(dir).map_err(io)?;
    }
    let bytes = serde_json::to_vec_pretty(remembered).expect("remembered chats serialize");
    let temp = file.with_extension("json.tmp");
    fs::write(&temp, bytes).map_err(io)?;
    fs::rename(&temp, file).map_err(io)
}

fn is_record_id(id: &str) -> bool {
    id.strip_prefix("local_").is_some_and(|rest| {
        (1..=64).contains(&rest.len())
            && rest.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
    })
}

/// Desktop-hosted chats running now, by record id.
fn running_now() -> Vec<OpenChat> {
    let Some(dir) = claude_sessions_dir() else {
        return Vec::new();
    };
    let alive = live_pids();
    claude_sessions_in(&dir, &alive)
        .into_iter()
        // A reused pid was already dropped (its start time differs from the file's).
        .filter(|s| s.entrypoint.as_deref() == Some("claude-desktop"))
        .filter_map(|s| {
            let id = s.raw.get("hostSessionId")?.as_str()?.to_string();
            if !is_record_id(&id) {
                return None;
            }
            Some(OpenChat {
                host_session_id: id,
                name: s.name,
                cwd: s.cwd,
                last_seen_ms: 0,
                account: None,
            })
        })
        .collect()
}

/// Notes the chats running now and forgets ones not seen for a day. Returns what is kept.
pub fn remember(file: &Path, root: Option<&Path>, now_ms: u64) -> Result<Vec<OpenChat>, SyncError> {
    let mut remembered = load(file);
    let account = root.and_then(sync::read_active_account);
    for mut chat in running_now() {
        chat.last_seen_ms = now_ms;
        chat.account = account.clone();
        remembered.chats.insert(chat.host_session_id.clone(), chat);
    }
    remembered
        .chats
        .retain(|_, c| now_ms.saturating_sub(c.last_seen_ms) < FORGET_AFTER_MS);
    save(file, &remembered)?;
    Ok(remembered.chats.into_values().collect())
}

/// Desktop's record of a chat in the signed-in account (any account when that is unknown).
fn record(root: &Path, id: &str) -> Option<Value> {
    let sessions = root.join("claude-code-sessions");
    let accounts: Vec<PathBuf> = match sync::read_active_account(root) {
        Some(account) => vec![sessions.join(account)],
        None => fs::read_dir(&sessions)
            .ok()?
            .flatten()
            .map(|e| e.path())
            .collect(),
    };
    accounts.iter().find_map(|account| {
        fs::read_dir(account).ok()?.flatten().find_map(|org| {
            let bytes = fs::read(org.path().join(format!("{id}.json"))).ok()?;
            serde_json::from_slice(&bytes).ok()
        })
    })
}

#[derive(Serialize)]
pub struct ReopenResult {
    /// Chats whose CLI came back, in the order they were shown.
    pub reopened: Vec<OpenChat>,
    /// Shown but no CLI appeared in time.
    pub not_started: Vec<OpenChat>,
    /// Not shown: archived, scheduled, missing from the signed-in account, or after a
    /// chat that never started (Desktop is not taking links).
    pub skipped: Vec<Skipped>,
}

#[derive(Serialize)]
pub struct Skipped {
    #[serde(flatten)]
    pub chat: OpenChat,
    pub reason: &'static str,
}

/// The last set of chats seen running that are not running now, the most recently focused
/// last (it is the page left showing). With `open`, shows each and waits for its CLI.
pub fn reopen(file: &Path, root: &Path, now_ms: u64, open: bool) -> ReopenResult {
    let mut chats: Vec<OpenChat> = load(file).chats.into_values().collect();
    let last = chats.iter().max_by_key(|c| c.last_seen_ms);
    let newest = last.map_or(0, |c| c.last_seen_ms);
    let switched = last
        .and_then(|c| c.account.as_deref())
        .zip(sync::read_active_account(root))
        .is_some_and(|(then, now)| then != now);
    let window = if switched { FORGET_AFTER_MS } else { RECENT_MS };
    if now_ms.saturating_sub(newest) > window {
        chats.clear();
    }
    let running: Vec<String> = running_now()
        .into_iter()
        .map(|c| c.host_session_id)
        .collect();
    let mut result = ReopenResult {
        reopened: Vec::new(),
        not_started: Vec::new(),
        skipped: Vec::new(),
    };
    let mut queue = Vec::new();
    for chat in chats {
        if newest.saturating_sub(chat.last_seen_ms) > SAME_SET_MS
            || running.contains(&chat.host_session_id)
        {
            continue;
        }
        let Some(record) = record(root, &chat.host_session_id) else {
            result.skipped.push(Skipped {
                chat,
                reason: "not in the signed-in account",
            });
            continue;
        };
        if record.get("isArchived").and_then(Value::as_bool) == Some(true) {
            result.skipped.push(Skipped {
                chat,
                reason: "archived",
            });
        } else if record.get("scheduledTaskId").is_some_and(|v| !v.is_null()) {
            result.skipped.push(Skipped {
                chat,
                reason: "scheduled task",
            });
        } else if record
            .get("interruptedByQuitAt")
            .is_some_and(|v| !v.is_null())
        {
            // Desktop sends such a chat "continue" when it is shown, a paid turn; it is left
            // for the owner to open.
            result.skipped.push(Skipped {
                chat,
                reason: "was mid-reply at the quit",
            });
        } else {
            let focused = record
                .get("lastFocusedAt")
                .and_then(Value::as_f64)
                .unwrap_or(0.0);
            queue.push((focused, chat));
        }
    }
    queue.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut wait = FIRST_WAIT;
    let mut queue = queue.into_iter().map(|(_, chat)| chat);
    for chat in queue.by_ref() {
        if !open {
            result.skipped.push(Skipped {
                chat,
                reason: "dry run",
            });
            continue;
        }
        if !open_link(&chat.host_session_id) || !wait_running(&chat.host_session_id, wait) {
            result.not_started.push(chat);
            break;
        }
        std::thread::sleep(DWELL);
        result.reopened.push(chat);
        wait = NEXT_WAIT;
    }
    // After a chat that did not start, Desktop is not taking links (signed out, links
    // turned off, or not running): the rest are not tried.
    result.skipped.extend(queue.map(|chat| Skipped {
        chat,
        reason: "an earlier chat did not start",
    }));
    result
}

fn wait_running(id: &str, within: Duration) -> bool {
    let until = Instant::now() + within;
    while Instant::now() < until {
        std::thread::sleep(Duration::from_millis(500));
        if running_now().iter().any(|c| c.host_session_id == id) {
            return true;
        }
    }
    false
}

/// Hands Desktop its own deep link; the id is checked so nothing else reaches the handler.
fn open_link(id: &str) -> bool {
    if !is_record_id(id) {
        return false;
    }
    let url = format!("claude://code/continue?session={id}");
    #[cfg(target_os = "macos")]
    let mut command = std::process::Command::new("/usr/bin/open");
    #[cfg(target_os = "macos")]
    command.arg(&url);
    #[cfg(windows)]
    let mut command = {
        use std::os::windows::process::CommandExt;
        let mut c = std::process::Command::new("rundll32.exe");
        c.args(["url.dll,FileProtocolHandler", &url])
            .creation_flags(0x0800_0000);
        c
    };
    #[cfg(not(any(target_os = "macos", windows)))]
    return false;
    #[cfg(any(target_os = "macos", windows))]
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}
