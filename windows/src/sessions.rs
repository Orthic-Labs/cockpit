//! Claude Code sessions on this PC, for the Claude ring's inner activity ring and its hover
//! card. Ports the Mac `ClaudeSessionMonitor`: Claude Code writes one JSON record per running
//! session to `<config dir>/sessions/<pid>.json` (`%USERPROFILE%\.claude` unless
//! `CLAUDE_CONFIG_DIR` names another). A record only counts while its process is alive: a
//! crashed session leaves its file behind saying `busy` for ever, so each pid is checked and
//! its creation time compared with the record's start time (a recycled pid must not revive a
//! dead session).
//!
//! Differences from the Mac, deliberately: sessions the Claude desktop app hosts carry no
//! `status`, and the Mac reads their state from the transcript under `projects/`; that reader
//! is not ported, so such a record reads as idle. The Mac gets `success` from the Codex
//! monitor only; here a Claude session that was busy and is now idle holds `Success` for
//! `SUCCESS_HOLD_MS` (the "just completed" ring), tracked across polls.

use crate::card::{Dot, Row, elapsed_text};
use crate::json::{self, Value};
use crate::layout::Activity;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Mutex, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};
use windows::Win32::Foundation::{CloseHandle, ERROR_INVALID_PARAMETER, FILETIME};
use windows::Win32::System::Threading::{
    GetExitCodeProcess, GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::core::HRESULT;

const MAX_RECORD_BYTES: usize = 64 * 1024;
const MAX_RECORDS: usize = 64;
/// Poll cadence: the Mac rescans every two seconds.
const POLL_MS: u64 = 2_000;
/// How long a finished session reads "complete" on the ring.
const SUCCESS_HOLD_MS: u64 = 60_000;
/// Slack between a process starting and its session registering (the Mac allows five
/// minutes); anything further apart is a recycled pid.
const REUSE_TOLERANCE_MS: u64 = 5 * 60 * 1000;
/// `STILL_ACTIVE` exit code of a running process.
const STILL_ACTIVE: u32 = 259;
/// 100 ns ticks between 1601-01-01 and the Unix epoch, in milliseconds.
const FILETIME_UNIX_EPOCH_MS: u64 = 11_644_473_600_000;
/// Sessions a card lists before summarising the rest.
const CARD_ROWS: usize = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Busy,
    Waiting,
    Success,
    Idle,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Session {
    pub id: String,
    pub name: String,
    /// Surface and folder, e.g. `Terminal · demo-app`.
    pub detail: String,
    pub state: State,
    /// What a waiting session wants from you.
    pub waiting_for: Option<String>,
    /// When it entered its current state, Unix milliseconds.
    pub since_ms: u64,
}

/// One registry file, as Claude Code writes it. Decoded leniently: another program writes it
/// on its own release schedule, and an unknown field must never cost a session.
struct Record {
    pid: u32,
    started_ms: Option<u64>,
    session: Session,
}

fn number_ms(value: &Value, key: &str) -> Option<u64> {
    value
        .get(key)
        .and_then(Value::as_f64)
        .filter(|n| *n >= 0.0)
        .map(|n| n as u64)
}

fn surface(entrypoint: Option<&str>) -> &'static str {
    match entrypoint {
        Some("claude-desktop" | "claude-desktop-3p") => "Desktop",
        Some("claude-vscode") => "VS Code",
        Some("local-agent") => "Agent",
        _ => "Terminal",
    }
}

fn parse_record(root: &Value, now_ms: u64) -> Option<Record> {
    let pid = root.get("pid")?.as_f64().filter(|p| *p > 0.0)? as u32;
    let cwd = root.get("cwd")?.as_str()?;
    let text = |key: &str| root.get(key).and_then(Value::as_str);
    let state = match (text("tempo"), text("status")) {
        (Some("blocked"), _) | (_, Some("waiting")) => State::Waiting,
        (Some("active"), _) | (_, Some("busy")) => State::Busy,
        _ => State::Idle,
    };
    let folder = cwd
        .trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("");
    let started_ms = number_ms(root, "startedAt");
    let since_ms = number_ms(root, "statusUpdatedAt")
        .or_else(|| number_ms(root, "updatedAt"))
        .or(started_ms)
        .unwrap_or(now_ms);
    Some(Record {
        pid,
        started_ms,
        session: Session {
            id: format!("claude.{pid}"),
            name: text("name").unwrap_or(folder).to_string(),
            detail: format!("{} \u{b7} {folder}", surface(text("entrypoint"))),
            state,
            waiting_for: text("waitingFor")
                .or_else(|| text("needs"))
                .map(str::to_string),
            since_ms,
        },
    })
}

fn sessions_dir() -> Option<PathBuf> {
    let config = std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| {
            std::env::var_os("USERPROFILE")
                .map(PathBuf::from)
                .filter(|p| p.is_absolute())
                .map(|home| home.join(".claude"))
        })?;
    Some(config.join("sessions"))
}

/// Is `pid` still the process that registered at `started_ms`? Unprovable counts as alive
/// (a probably-real session is better shown than hidden), like the Mac.
fn is_alive(pid: u32, started_ms: Option<u64>) -> bool {
    let handle = match unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) } {
        Ok(handle) => handle,
        // No such pid: gone. Any other refusal (access denied) means it exists.
        Err(error) => return error.code() != HRESULT::from_win32(ERROR_INVALID_PARAMETER.0),
    };
    let mut code = 0u32;
    let running =
        unsafe { GetExitCodeProcess(handle, &mut code) }.is_ok_and(|()| code == STILL_ACTIVE);
    let created = {
        let (mut created, mut exited, mut kernel, mut user) = (
            FILETIME::default(),
            FILETIME::default(),
            FILETIME::default(),
            FILETIME::default(),
        );
        unsafe { GetProcessTimes(handle, &mut created, &mut exited, &mut kernel, &mut user) }
            .ok()
            .map(|()| {
                let ticks =
                    (u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime);
                (ticks / 10_000).saturating_sub(FILETIME_UNIX_EPOCH_MS)
            })
    };
    let _ = unsafe { CloseHandle(handle) };
    if !running {
        return false;
    }
    match (started_ms, created) {
        (Some(started), Some(created)) => started.abs_diff(created) < REUSE_TOLERANCE_MS,
        _ => true,
    }
}

/// The live sessions in the registry, newest first.
fn read_registry(now_ms: u64) -> Vec<Session> {
    let Some(dir) = sessions_dir() else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut newest: BTreeMap<String, Record> = BTreeMap::new();
    let mut unidentified: Vec<Record> = Vec::new();
    for entry in entries.flatten().take(MAX_RECORDS) {
        let path = entry.path();
        if path.extension().is_none_or(|e| e != "json") {
            continue;
        }
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if !meta.is_file() || meta.len() > MAX_RECORD_BYTES as u64 {
            continue;
        }
        let Some(root) = std::fs::read(&path)
            .ok()
            .and_then(|bytes| json::parse(&bytes, MAX_RECORD_BYTES))
        else {
            continue;
        };
        let Some(record) = parse_record(&root, now_ms) else {
            continue;
        };
        if !is_alive(record.pid, record.started_ms) {
            continue;
        }
        // Resuming registers a new pid while the old one winds down: keep the newest record
        // of a session id, as Claude Code does.
        match root.get("sessionId").and_then(Value::as_str) {
            Some(id) => {
                let candidate = record.started_ms.unwrap_or(0);
                let held = newest.get(id).map(|r| r.started_ms.unwrap_or(0));
                if held.is_none_or(|held| candidate > held) {
                    newest.insert(id.to_string(), record);
                }
            }
            None => unidentified.push(record),
        }
    }
    let mut found: Vec<Session> = newest
        .into_values()
        .chain(unidentified)
        .map(|r| r.session)
        .collect();
    // The id breaks ties so the order cannot flicker between two polls that read the same.
    found.sort_by(|a, b| b.since_ms.cmp(&a.since_ms).then_with(|| a.id.cmp(&b.id)));
    found
}

// ---- completion tracking ---------------------------------------------------------------------

/// Remembers each session's last state to notice a busy session going idle (the Mac's
/// `SessionCompletionWatcher`: only leaving busy counts, nothing is announced from the first
/// reading, and a vanished session is dropped, not announced).
struct Tracker {
    previous: BTreeMap<String, State>,
    /// Sessions that finished, until when they read as complete, Unix ms.
    finished: BTreeMap<String, u64>,
    seeded: bool,
    polled_ms: u64,
    shown: Vec<Session>,
}

impl Tracker {
    const fn new() -> Self {
        Self {
            previous: BTreeMap::new(),
            finished: BTreeMap::new(),
            seeded: false,
            polled_ms: 0,
            shown: Vec::new(),
        }
    }

    fn absorb(&mut self, mut sessions: Vec<Session>, now_ms: u64) {
        let mut current = BTreeMap::new();
        for session in &mut sessions {
            if self.seeded
                && self.previous.get(&session.id) == Some(&State::Busy)
                && matches!(session.state, State::Idle | State::Success)
            {
                self.finished
                    .insert(session.id.clone(), now_ms + SUCCESS_HOLD_MS);
            }
            current.insert(session.id.clone(), session.state);
        }
        self.previous = current;
        self.seeded = true;
        self.finished.retain(|id, until| {
            *until > now_ms
                && sessions
                    .iter()
                    .any(|s| s.id == *id && s.state == State::Idle)
        });
        for session in &mut sessions {
            if session.state == State::Idle && self.finished.contains_key(&session.id) {
                session.state = State::Success;
            }
        }
        self.shown = sessions;
    }
}

static TRACKER: Mutex<Tracker> = Mutex::new(Tracker::new());

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// The current sessions; the registry is re-read at most every two seconds.
pub fn snapshot() -> Vec<Session> {
    let now = now_ms();
    {
        let tracker = TRACKER.lock().unwrap_or_else(PoisonError::into_inner);
        if tracker.seeded && now.saturating_sub(tracker.polled_ms) < POLL_MS {
            return tracker.shown.clone();
        }
    }
    // Read with no lock held: it touches the disk and the process table.
    let found = read_registry(now);
    let mut tracker = TRACKER.lock().unwrap_or_else(PoisonError::into_inner);
    tracker.polled_ms = now;
    tracker.absorb(found, now);
    tracker.shown.clone()
}

// ---- what the notch shows --------------------------------------------------------------------

/// The Claude ring's inner ring: blocked-on-you outranks everything, a busy session draws
/// nothing (no spinner, like the Mac fork), a just-completed one draws green.
pub fn activity(sessions: &[Session]) -> Activity {
    if sessions.iter().any(|s| s.state == State::Waiting) {
        Activity::Waiting
    } else if sessions.iter().any(|s| s.state == State::Busy) {
        Activity::None
    } else if sessions.iter().any(|s| s.state == State::Success) {
        Activity::Success
    } else {
        Activity::None
    }
}

fn word(state: State) -> &'static str {
    match state {
        State::Waiting => "waiting",
        State::Busy => "working",
        State::Success => "complete",
        State::Idle => "idle",
    }
}

fn rank(state: State) -> u8 {
    match state {
        State::Waiting => 0,
        State::Busy => 1,
        State::Success => 2,
        State::Idle => 3,
    }
}

/// Hover-card rows for the sessions, as the Mac card lists them: a rule, then each session's
/// name and status (waiting first) over what it is waiting for or where it runs, with how
/// long it has been so.
pub fn card_rows(sessions: &[Session], now_ms: u64) -> Vec<Row> {
    let mut ordered: Vec<&Session> = sessions.iter().collect();
    ordered.sort_by_key(|s| rank(s.state));
    let mut rows = Vec::new();
    if !ordered.is_empty() {
        rows.push(Row::Rule);
    }
    for session in ordered.iter().take(CARD_ROWS) {
        rows.push(Row::Session {
            name: session.name.clone(),
            dot: match session.state {
                State::Busy => Dot::Busy,
                State::Waiting => Dot::Waiting,
                State::Success => Dot::Success,
                State::Idle => Dot::Idle,
            },
            word: word(session.state).to_string(),
            detail: match (&session.state, &session.waiting_for) {
                (State::Waiting, Some(question)) => question.clone(),
                _ => session.detail.clone(),
            },
            age: elapsed_text(now_ms.saturating_sub(session.since_ms) / 1000),
        });
    }
    if ordered.len() > CARD_ROWS {
        rows.push(Row::Note(format!("and {} more", ordered.len() - CARD_ROWS)));
    }
    rows
}
