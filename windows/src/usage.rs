//! AI usage readers (Claude and Codex) for the AI rings. Read-only: credentials are only
//! read from the files the tools themselves keep (`~/.claude/.credentials.json`,
//! `~/.codex/auth.json`), kept in memory for one request, never refreshed, never written,
//! never logged. A background thread polls every five minutes with back-off after a 429;
//! unavailable readings stay unavailable (`--`), never zero. Parsing is pure and shared
//! with nothing Win32 so it can be reasoned about on its own.
//!
//! The Claude ring follows the account Claude Desktop is signed into while Desktop runs
//! (Claude Code's account otherwise). An account other than Claude Code's takes its numbers
//! only from Desktop's own cached usage response (`desktop`), never from the Claude Code
//! login, which describes the other account; with no fresh cached reading the ring is empty
//! and the card says so.

use crate::claude_accounts;
use crate::desktop;
use crate::diag;
use crate::http;
use crate::json::{self, Value};
use crate::raii::hwnd_from_key;
use std::ffi::c_void;
use std::io::Read;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU64, Ordering};
use std::sync::{Condvar, Mutex, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use windows::Win32::Foundation::{LPARAM, SYSTEMTIME, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

/// Posted to the controller window when a poll changed a reading (WM_APP + 1).
pub const MSG_USAGE_UPDATED: u32 = 0x8001;

const POLL_SECONDS: u64 = 300;
const REQUEST_TIMEOUT_MS: i32 = 15_000;
const CREDENTIAL_MAX_BYTES: usize = 64 * 1024;
const RESPONSE_MAX_BYTES: usize = 512 * 1024;
const BACKOFF_FLOOR_SECONDS: u64 = 60;
const BACKOFF_CEILING_SECONDS: u64 = 15 * 60;
/// The longest delay a server's `Retry-After` may impose.
const SERVER_BACKOFF_CEILING_SECONDS: u64 = 6 * 3600;
/// `~/.claude.json` carries project history and can be large; only its `oauthAccount` is read.
const CLAUDE_CONFIG_MAX_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Provider {
    Claude,
    Codex,
}

impl Provider {
    pub const ALL: [Provider; 2] = [Provider::Claude, Provider::Codex];

    pub fn index(self) -> usize {
        match self {
            Provider::Claude => 0,
            Provider::Codex => 1,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Provider::Claude => "Claude",
            Provider::Codex => "Codex",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    /// No poll has finished yet.
    Waiting,
    Ok,
    /// No usable sign-in on this machine.
    SignIn,
    /// A sign-in exists but its token has expired; the owning tool refreshes it.
    Expired,
    RateLimited,
    Unavailable,
    /// The saved login exists but Windows refused Pulse access to read it.
    AccessDenied,
    /// Claude Desktop is signed into an account whose usage Desktop has not cached lately.
    NoReading,
}

impl Status {
    pub fn text(self) -> &'static str {
        match self {
            Status::Waiting => "Waiting for the first reading",
            Status::Ok => "Up to date",
            Status::SignIn => "Sign in needed",
            Status::Expired => "Sign-in expired; use the app once to refresh",
            Status::RateLimited => "Rate limited; retrying later",
            Status::Unavailable => "Unavailable",
            Status::AccessDenied => "Windows refused access to the saved login",
            Status::NoReading => "No reading yet",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct LimitWindow {
    /// Stable id: `session`/`weekly_all`/... for Claude, `primary`/`secondary` for Codex.
    pub key: String,
    /// The model or feature a window belongs to (Codex's Spark, code review): windows of a
    /// group show together in a titled box. `None` for the account's own windows.
    pub group: Option<String>,
    pub label: String,
    /// Used share, 0..=1.
    pub fraction: f32,
    /// Unix seconds.
    pub resets_at: Option<u64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Usage {
    pub status: Status,
    pub plan: Option<String>,
    pub windows: Vec<LimitWindow>,
    /// Unix seconds of the last successful reading shown in `windows`.
    pub updated: Option<u64>,
    /// Set while a limit is spent: the account is paused until `resets_at`.
    pub block: Option<Block>,
    /// The account's name when the reading is not simply Claude Code's (Claude Desktop's).
    pub account: Option<String>,
    pub extras: Extras,
}

/// Codex's extra card content beyond its rate-limit windows (the Mac's "Available credits",
/// "Plan active until" and unused-resets rows).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Extras {
    /// The prepaid credit balance as card text: "Unlimited" or a number.
    pub credits: Option<String>,
    /// When the paid plan runs to (unix seconds), from the sign-in's identity token.
    pub plan_until: Option<u64>,
    pub resets: Option<ResetCredits>,
}

/// Unused rate-limit resets on a Codex account.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResetCredits {
    pub available: u32,
    /// The soonest expiry still ahead (unix seconds).
    pub next_expiry: Option<u64>,
}

/// A spent limit that pauses the account.
#[derive(Clone, Debug, PartialEq)]
pub struct Block {
    pub reason: String,
    /// Unix seconds.
    pub resets_at: Option<u64>,
}

/// The pause a set of fresh windows implies: the first account window that is fully used. A
/// spent model or feature group (Spark, code review) does not pause the account.
fn block_of(windows: &[LimitWindow]) -> Option<Block> {
    let own = |w: &&LimitWindow| w.group.is_none() && w.fraction >= 1.0;
    windows.iter().find(own).map(|w| Block {
        reason: if w.key == "session" || w.key == "primary" {
            "Session limit reached".to_string()
        } else {
            format!("{} limit reached", w.label)
        },
        resets_at: w.resets_at,
    })
}

impl LimitWindow {
    /// The window's length in seconds, from what names it: Claude's keys, or Codex's label
    /// ("5h limit", "Weekly limit", "45m limit", "3d limit"). `None` when it says nothing.
    pub fn seconds(&self) -> Option<u64> {
        match self.key.as_str() {
            "session" => return Some(5 * 3600),
            key if key.starts_with("weekly") => return Some(7 * 24 * 3600),
            _ => {}
        }
        match self.label.as_str() {
            "Weekly limit" => return Some(7 * 24 * 3600),
            "Monthly limit" => return Some(30 * 24 * 3600),
            _ => {}
        }
        let body = self.label.strip_suffix(" limit")?;
        let (count, unit) = body.split_at(body.find(|c: char| !c.is_ascii_digit())?);
        let count: u64 = count.parse().ok()?;
        match unit {
            "m" => Some(count * 60),
            "h" => Some(count * 3600),
            "d" => Some(count * 24 * 3600),
            _ => None,
        }
    }
}

impl Usage {
    pub const fn waiting() -> Self {
        Self {
            status: Status::Waiting,
            plan: None,
            windows: Vec::new(),
            updated: None,
            block: None,
            account: None,
            extras: Extras {
                credits: None,
                plan_until: None,
                resets: None,
            },
        }
    }

    /// One line saying where the reading stands, for the hub's account row.
    pub fn summary(&self) -> String {
        let text = match (&self.account, self.status) {
            (Some(name), Status::NoReading) => format!("No reading for {name} yet"),
            (_, status) => status.text().to_string(),
        };
        // A remembered or cached reading says how old it is, so it never passes for live.
        match self.updated {
            Some(updated) if !self.windows.is_empty() => {
                let age = now_secs().saturating_sub(updated);
                if self.status == Status::Ok && age < 120 {
                    text
                } else {
                    format!("{text} (reading from {})", age_text(age))
                }
            }
            _ => text,
        }
    }

    /// The window the main ring means: the session / 5-hour window.
    pub fn headline(&self) -> Option<&LimitWindow> {
        self.windows
            .iter()
            .find(|w| w.key == "session" || w.key == "primary" || w.key == "credits")
    }

    /// The thin inner ring: the all-models weekly / secondary window.
    pub fn weekly(&self) -> Option<&LimitWindow> {
        self.windows
            .iter()
            .find(|w| w.key == "weekly_all" || w.key == "secondary")
    }

    /// Readings shown while the latest poll failed are dimmed.
    pub fn is_stale(&self) -> bool {
        self.status != Status::Ok && !self.windows.is_empty()
    }
}

static USAGE: Mutex<[Usage; 2]> = Mutex::new([Usage::waiting(), Usage::waiting()]);
static STOP: (Mutex<bool>, Condvar) = (Mutex::new(false), Condvar::new());
/// The controller window to notify, set by `start`.
static CONTROLLER: AtomicIsize = AtomicIsize::new(0);
/// Until when (unix seconds) Desktop's account is watched every second instead of every three.
static FAST_UNTIL: AtomicU64 = AtomicU64::new(0);
/// A Claude reading is wanted now rather than at the next poll.
static REFETCH: AtomicBool = AtomicBool::new(false);
/// "Refresh now": every provider is to be read at once.
static REFRESH_ALL: AtomicBool = AtomicBool::new(false);
/// Where the last Claude poll got (or failed to get) its reading, and why Desktop's cache was
/// not usable when it was not, for the refresh log line. Written and read by the one polling
/// thread.
static CLAUDE_SOURCE: Mutex<(&'static str, &'static str)> = Mutex::new(("none", ""));
/// The `Retry-After` seconds of the last 429 seen by `classify` (0: the server gave none).
/// Written and read by the one polling thread, one provider at a time.
static SERVER_RETRY: AtomicU64 = AtomicU64::new(0);
/// How often Desktop's account is looked at between polls, and for how long after a restart
/// the quicker pace applies.
const WATCH_SECONDS: u64 = 3;
const WATCH_FAST_SECONDS: u64 = 1;
const FAST_WINDOW_SECONDS: u64 = 60;

/// The accounts the Claude reading and the hub's account list are about.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ClaudeIds {
    /// The tracked account: Desktop's while it runs, else Claude Code's.
    pub active: Option<String>,
    /// Claude Code's own account and the address it is signed in with.
    pub code: Option<String>,
    pub email: Option<String>,
}

static CLAUDE_IDS: Mutex<ClaudeIds> = Mutex::new(ClaudeIds {
    active: None,
    code: None,
    email: None,
});

pub fn claude_ids() -> ClaudeIds {
    CLAUDE_IDS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
}

/// Asks the notch to redraw from the current readings (cards included).
pub fn notify() {
    let key = CONTROLLER.load(Ordering::Relaxed);
    if key != 0 {
        let _ = unsafe {
            PostMessageW(
                Some(hwnd_from_key(key)),
                MSG_USAGE_UPDATED,
                WPARAM(0),
                LPARAM(0),
            )
        };
    }
}

/// "Refresh now" from the hub (the Mac's `refresh`): reads Claude and Codex at once. A
/// provider inside its rate-limit back-off still waits it out (Claude's endpoint, that is:
/// Desktop's cache file is re-read regardless), as on the Mac. Each re-read is logged as
/// `usage_refresh`.
pub fn refresh_now() {
    REFRESH_ALL.store(true, Ordering::Relaxed);
    // Taking the lock first means the polling thread is either before its check (and sees
    // the flag) or already waiting (and is woken).
    drop(STOP.0.lock().unwrap_or_else(PoisonError::into_inner));
    STOP.1.notify_all();
}

/// "Forget reading": drops what Pulse read for a provider (the ring, the saved copy, the
/// account book's entry for Claude). The next poll reads it again if the login is still there.
pub fn forget(provider: Provider) {
    USAGE.lock().unwrap_or_else(PoisonError::into_inner)[provider.index()] = Usage::waiting();
    match provider {
        Provider::Claude => {
            if let Some(id) = claude_ids().active {
                claude_accounts::clear_reading(&id);
            }
        }
        Provider::Codex => {
            if let Some(path) = last_codex_path() {
                let _ = std::fs::remove_file(path);
            }
        }
    }
    notify();
}

/// After Claude was reopened: read the Claude usage at once, and look at Desktop's account
/// every second for a minute (the owner may be signing in to another account).
pub fn refetch_claude_soon() {
    FAST_UNTIL.store(now_secs() + FAST_WINDOW_SECONDS, Ordering::Relaxed);
    REFETCH.store(true, Ordering::Relaxed);
}

/// "3 min ago" style age for the hub's account line.
fn age_text(seconds: u64) -> String {
    match seconds {
        0..=119 => "moments ago".to_string(),
        120..=7199 => format!("{} min ago", seconds / 60),
        7200..=172_799 => format!("{} h ago", seconds / 3600),
        _ => format!("{} d ago", seconds / 86_400),
    }
}

pub fn snapshot() -> [Usage; 2] {
    USAGE.lock().unwrap_or_else(PoisonError::into_inner).clone()
}

pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ------------------------------------------------------------------ worker

/// Starts the polling thread. `controller_key` is the controller window to notify.
pub fn start(controller_key: isize) {
    CONTROLLER.store(controller_key, Ordering::Relaxed);
    let spawned = std::thread::Builder::new()
        .name("usage".into())
        .spawn(move || worker(controller_key));
    if let Err(error) = spawned {
        diag::info(
            "usage_thread_failed",
            &[("reason", error.to_string().as_str())],
        );
    }
}

pub fn stop() {
    *STOP.0.lock().unwrap_or_else(PoisonError::into_inner) = true;
    STOP.1.notify_all();
}

/// Sleeps up to `seconds`; false when the notch is shutting down.
fn wait(seconds: u64) -> bool {
    let guard = STOP.0.lock().unwrap_or_else(PoisonError::into_inner);
    let (guard, _) = STOP
        .1
        .wait_timeout_while(guard, Duration::from_secs(seconds), |stopped| {
            !*stopped && !REFRESH_ALL.load(Ordering::Relaxed)
        })
        .unwrap_or_else(PoisonError::into_inner);
    !*guard
}

#[derive(Default)]
struct Tracker {
    consecutive_rate_limits: u32,
    backoff_until: u64,
    /// Claude: the account the last poll was for (Desktop's while it runs, else Claude
    /// Code's), when it was first seen to be that (unix seconds, 0 at start-up), and whether
    /// the reading held for the previous account must be dropped by the next outcome.
    account: Option<String>,
    account_since: u64,
    account_seen: bool,
    account_changed: bool,
    /// Claude: the last Desktop cache result that was logged, so a repeat is not.
    note: &'static str,
}

/// What ends the wait between polls.
enum Pause {
    Stopped,
    /// The poll interval ran out: every provider.
    Due,
    /// "Refresh now": every provider, and each re-read is logged.
    Refresh,
    /// Claude Desktop started, quit or changed account: the Claude reading only.
    Claude,
}

/// Waits for the next poll. Between polls Desktop's account (and whether Desktop runs) is
/// looked at every few seconds, quicker for a minute after a restart, so a switch of account
/// is read at once instead of at the next poll.
fn pause() -> Pause {
    let deadline = now_secs() + POLL_SECONDS;
    let mut seen = desktop::signed_in_account();
    loop {
        let fast = now_secs() < FAST_UNTIL.load(Ordering::Relaxed);
        if !wait(if fast {
            WATCH_FAST_SECONDS
        } else {
            WATCH_SECONDS
        }) {
            return Pause::Stopped;
        }
        if REFRESH_ALL.swap(false, Ordering::Relaxed) {
            return Pause::Refresh;
        }
        if REFETCH.swap(false, Ordering::Relaxed) {
            return Pause::Claude;
        }
        let account = desktop::signed_in_account();
        if account != seen {
            return Pause::Claude;
        }
        seen = account;
        if now_secs() >= deadline {
            return Pause::Due;
        }
    }
}

fn worker(controller_key: isize) {
    let mut trackers = [Tracker::default(), Tracker::default()];
    let mut saved_backoff = restore(&mut trackers);
    let mut only_claude = false;
    let mut manual = false;
    loop {
        let now = now_secs();
        let mut changed = false;
        for provider in Provider::ALL {
            if only_claude && provider != Provider::Claude {
                continue;
            }
            let tracker = &mut trackers[provider.index()];
            // Claude's back-off belongs to the usage endpoint only (`poll_claude` honours
            // it there); Desktop's cache is local and is read regardless.
            if now < tracker.backoff_until && provider != Provider::Claude {
                continue;
            }
            SERVER_RETRY.store(0, Ordering::Relaxed);
            let outcome = match provider {
                Provider::Claude => poll_claude(now, tracker),
                Provider::Codex => Some(poll_codex(now)),
            };
            let Some(outcome) = outcome else {
                if manual {
                    // A manual refresh never skips the 429 back-off; say that it waited.
                    log_refresh(
                        provider,
                        source_for(provider, "backoff"),
                        "waiting",
                        tracker.backoff_until.saturating_sub(now),
                    );
                }
                continue;
            };
            let next = apply_outcome(provider, tracker, outcome, now);
            if manual {
                log_refresh(
                    provider,
                    source_for(provider, "endpoint"),
                    next.status.text(),
                    next.updated.map_or(0, |at| now.saturating_sub(at)),
                );
            }
            // A back-off that began or ended is kept across a restart.
            if saved_backoff[provider.index()] != tracker.backoff_until {
                saved_backoff[provider.index()] = tracker.backoff_until;
                save_backoff(saved_backoff);
            }
            if provider == Provider::Codex && next.status == Status::Ok {
                save_last_codex(&next);
            }
            let mut all = USAGE.lock().unwrap_or_else(PoisonError::into_inner);
            if all[provider.index()] != next {
                diag::info(
                    "usage_reading",
                    &[
                        ("provider", provider.name()),
                        ("status", next.status.text()),
                        ("windows", next.windows.len().to_string().as_str()),
                    ],
                );
                all[provider.index()] = next;
                changed = true;
            }
        }
        if changed {
            let _ = unsafe {
                PostMessageW(
                    Some(hwnd_from_key(controller_key)),
                    MSG_USAGE_UPDATED,
                    WPARAM(0),
                    LPARAM(0),
                )
            };
        }
        manual = false;
        match pause() {
            Pause::Stopped => return,
            Pause::Due => only_claude = false,
            Pause::Refresh => {
                only_claude = false;
                manual = true;
            }
            Pause::Claude => only_claude = true,
        }
    }
}

/// Records where the Claude reading of the current poll came from.
fn set_claude_source(source: &'static str) {
    *CLAUDE_SOURCE.lock().unwrap_or_else(PoisonError::into_inner) = (source, "");
}

/// The source and detail to log for `provider`: Claude's as the poll noted them, Codex's
/// `fallback`.
fn source_for(provider: Provider, fallback: &'static str) -> (&'static str, &'static str) {
    if provider == Provider::Claude {
        *CLAUDE_SOURCE.lock().unwrap_or_else(PoisonError::into_inner)
    } else {
        (fallback, "")
    }
}

/// One line per provider for every "Refresh now": the reading is logged even when it did not
/// change (`usage_reading` only logs changes, and a Desktop-cache reading keeps its date).
/// `seconds` is the reading's age, or for a wait the seconds left in the back-off.
fn log_refresh(provider: Provider, (source, detail): (&str, &str), result: &str, seconds: u64) {
    diag::info(
        "usage_refresh",
        &[
            ("provider", provider.name()),
            ("source", source),
            ("detail", detail),
            ("result", result),
            ("seconds", seconds.to_string().as_str()),
        ],
    );
}

// ------------------------------------------------------------------ across restarts

fn data_dir() -> Option<PathBuf> {
    let base = PathBuf::from(std::env::var_os("LOCALAPPDATA")?);
    base.is_absolute().then(|| base.join("Pulse"))
}

fn last_codex_path() -> Option<PathBuf> {
    Some(data_dir()?.join("codex-last-usage.json"))
}

fn backoff_path() -> Option<PathBuf> {
    Some(data_dir()?.join("usage-backoff.txt"))
}

fn write_file(path: &Path, text: &str) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let temp = path.with_extension("tmp");
    if std::fs::write(&temp, text.as_bytes()).is_ok() && std::fs::rename(&temp, path).is_err() {
        let _ = std::fs::remove_file(&temp);
    }
}

fn json_str(out: &mut String, text: &str) {
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 => out.push(' '),
            c => out.push(c),
        }
    }
    out.push('"');
}

/// The unix time each provider's rate-limit back-off ends (0: none), as saved last. The
/// server's `Retry-After` is honoured across a restart this way, not only within a run.
fn save_backoff(until: [u64; 2]) {
    if let Some(path) = backoff_path() {
        write_file(&path, &format!("claude={}\ncodex={}\n", until[0], until[1]));
    }
}

fn load_backoff(now: u64) -> [u64; 2] {
    let mut until = [0u64; 2];
    let Some(text) = backoff_path().and_then(|p| std::fs::read_to_string(p).ok()) else {
        return until;
    };
    for line in text.lines().take(4) {
        let Some((name, value)) = line.split_once('=') else {
            continue;
        };
        let Ok(value) = value.trim().parse::<u64>() else {
            continue;
        };
        // A saved time further ahead than any back-off allows is not believed.
        let value = if value > now + SERVER_BACKOFF_CEILING_SECONDS {
            0
        } else {
            value
        };
        match name.trim() {
            "claude" => until[0] = value,
            "codex" => until[1] = value,
            _ => {}
        }
    }
    until
}

/// Codex's last good reading (Claude's lives in the account book, per account).
fn save_last_codex(reading: &Usage) {
    let (Some(path), Some(updated)) = (last_codex_path(), reading.updated) else {
        return;
    };
    let mut out = format!("{{\"schema\":1,\"updated\":{updated},\"plan\":");
    match &reading.plan {
        Some(plan) => json_str(&mut out, plan),
        None => out.push_str("null"),
    }
    out.push_str(",\"windows\":[");
    for (index, w) in reading.windows.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        out.push_str("{\"key\":");
        json_str(&mut out, &w.key);
        out.push_str(",\"group\":");
        match &w.group {
            Some(group) => json_str(&mut out, group),
            None => out.push_str("null"),
        }
        out.push_str(",\"label\":");
        json_str(&mut out, &w.label);
        out.push_str(&format!(",\"fraction\":{}", f64::from(w.fraction)));
        if let Some(at) = w.resets_at {
            out.push_str(&format!(",\"resetsAt\":{at}"));
        }
        out.push('}');
    }
    out.push_str("]}\n");
    write_file(&path, &out);
}

fn load_last_codex(now: u64) -> Option<Usage> {
    let bytes = desktop::read_bounded(&last_codex_path()?, 256 * 1024)?;
    let root = json::parse(&bytes, 256 * 1024)?;
    let updated = root.get("updated").and_then(Value::as_f64)? as u64;
    let windows: Vec<LimitWindow> = root
        .get("windows")?
        .as_array()?
        .iter()
        .filter_map(|w| {
            let resets_at = w
                .get("resetsAt")
                .and_then(Value::as_f64)
                .map(|at| at as u64);
            // A window that has since reset says nothing true any more.
            if resets_at.is_some_and(|at| at <= now) {
                return None;
            }
            Some(LimitWindow {
                key: w.get("key")?.as_str()?.to_string(),
                group: w.get("group").and_then(Value::as_str).map(str::to_string),
                label: w.get("label")?.as_str()?.to_string(),
                fraction: (w.get("fraction")?.as_f64()? as f32).clamp(0.0, 1.0),
                resets_at,
            })
        })
        .collect();
    if windows.is_empty() {
        return None;
    }
    Some(Usage {
        status: Status::Waiting,
        plan: root.get("plan").and_then(Value::as_str).map(str::to_string),
        windows,
        updated: Some(updated),
        ..Usage::waiting()
    })
}

/// Start-up: open on what was known last time rather than on empty rings (the first poll
/// confirms or replaces it, and a failed one keeps it, dimmed and dated), and carry the
/// saved rate-limit back-off over. Returns the back-off times as saved.
fn restore(trackers: &mut [Tracker; 2]) -> [u64; 2] {
    let now = now_secs();
    let until = load_backoff(now);
    for (tracker, saved) in trackers.iter_mut().zip(until) {
        if saved > now {
            tracker.backoff_until = saved;
            tracker.consecutive_rate_limits = 1;
        }
    }
    let cli = claude_cli_account();
    let desktop_account = desktop::signed_in_account();
    let tracked = desktop_account.clone().or_else(|| cli.id.clone());
    let claude = tracked
        .as_deref()
        .and_then(|id| claude_accounts::last_reading(id, now).map(|saved| (id, saved)));
    let codex = load_last_codex(now);
    let mut all = USAGE.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some((id, saved)) = claude {
        all[0] = Usage {
            status: Status::Waiting,
            plan: saved.plan,
            windows: saved.windows,
            updated: Some(saved.captured_at),
            // Claude Desktop's own account is named on the card, as for a live reading.
            account: (cli.id.as_deref() != Some(id)).then(|| account_label(id)),
            ..Usage::waiting()
        };
        // The first poll is for this same account, so it must not drop the reading.
        trackers[0].account = Some(id.to_string());
        trackers[0].account_seen = true;
    }
    if let Some(reading) = codex {
        all[1] = reading;
    }
    drop(all);
    notify();
    until
}

enum Outcome {
    Fresh {
        windows: Vec<LimitWindow>,
        plan: Option<String>,
        /// Unix seconds the numbers were true.
        updated: u64,
        /// The account's name when it is not Claude Code's own.
        account: Option<String>,
        /// Whether the usage endpoint answered; its back-off state is reset only then.
        endpoint: bool,
        extras: Extras,
    },
    Failed(Status),
    /// Claude Desktop's account has no usable cached reading: no windows, and never another
    /// account's.
    NoReading {
        account: String,
    },
}

/// Folds one poll into the published reading: fresh data replaces it; a failure keeps the
/// last good windows (dimmed by the UI) unless their reset time has passed, or unless the
/// poll was for a different account than the last one.
fn apply_outcome(provider: Provider, tracker: &mut Tracker, outcome: Outcome, now: u64) -> Usage {
    let changed = std::mem::take(&mut tracker.account_changed);
    let previous = if changed {
        Usage::waiting()
    } else {
        USAGE.lock().unwrap_or_else(PoisonError::into_inner)[provider.index()].clone()
    };
    match outcome {
        Outcome::Fresh {
            windows,
            plan,
            updated,
            account,
            endpoint,
            extras,
        } => {
            if endpoint {
                tracker.consecutive_rate_limits = 0;
                tracker.backoff_until = 0;
            }
            let block = block_of(&windows);
            Usage {
                status: Status::Ok,
                // A cached reading carries no plan; the one already known stays.
                plan: if endpoint {
                    plan
                } else {
                    plan.or(previous.plan)
                },
                windows,
                updated: Some(updated),
                block,
                account,
                extras,
            }
        }
        Outcome::NoReading { account } => Usage {
            status: Status::NoReading,
            plan: None,
            windows: Vec::new(),
            updated: None,
            block: None,
            account: Some(account),
            extras: Extras::default(),
        },
        Outcome::Failed(status) => {
            // A 429 starts or extends the back-off. The same status passed on from inside a
            // back-off (to drop an old account's reading) is not a new 429.
            if status == Status::RateLimited && now >= tracker.backoff_until {
                start_backoff(tracker, now);
            }
            Usage {
                status,
                plan: previous.plan,
                windows: previous
                    .windows
                    .into_iter()
                    .filter(|w| w.resets_at.is_none_or(|reset| reset > now))
                    .collect(),
                updated: previous.updated,
                block: None,
                account: previous.account,
                extras: previous.extras,
            }
        }
    }
}

/// A 429 starts or extends the back-off: the server's own `Retry-After` when it sent one,
/// else 60 s doubling per consecutive 429.
fn start_backoff(tracker: &mut Tracker, now: u64) {
    tracker.consecutive_rate_limits = tracker.consecutive_rate_limits.saturating_add(1);
    let server = SERVER_RETRY.swap(0, Ordering::Relaxed);
    let delay = if server > 0 {
        server.clamp(BACKOFF_FLOOR_SECONDS, SERVER_BACKOFF_CEILING_SECONDS)
    } else {
        backoff_seconds(tracker.consecutive_rate_limits)
    };
    tracker.backoff_until = now + delay;
}

/// 60 s doubling per consecutive 429, capped at 15 min.
pub fn backoff_seconds(consecutive: u32) -> u64 {
    let doublings = consecutive.saturating_sub(1).min(4);
    (BACKOFF_FLOOR_SECONDS << doublings).min(BACKOFF_CEILING_SECONDS)
}

// ------------------------------------------------------------------ credentials

fn home() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
}

/// The saved login file as JSON. A file Windows will not let Pulse read is `AccessDenied`
/// (never "signed out"); a missing or malformed one is `SignIn`.
fn read_small_json(path: &Path) -> Result<Value, Status> {
    let denied = |error: std::io::Error| {
        if error.kind() == std::io::ErrorKind::PermissionDenied {
            Status::AccessDenied
        } else {
            Status::SignIn
        }
    };
    let meta = std::fs::metadata(path).map_err(denied)?;
    if !meta.is_file() || meta.len() > CREDENTIAL_MAX_BYTES as u64 {
        return Err(Status::SignIn);
    }
    let bytes = std::fs::read(path).map_err(denied)?;
    json::parse(&bytes, CREDENTIAL_MAX_BYTES).ok_or(Status::SignIn)
}

struct ClaudeLogin {
    token: String,
    plan: Option<String>,
}

fn claude_login(now: u64) -> Result<ClaudeLogin, Status> {
    let dir = std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| home().map(|h| h.join(".claude")))
        .ok_or(Status::SignIn)?;
    let root = read_small_json(&dir.join(".credentials.json"))?;
    let oauth = root.get("claudeAiOauth").ok_or(Status::SignIn)?;
    let token = oauth
        .get("accessToken")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .ok_or(Status::SignIn)?
        .to_string();
    // `expiresAt` is milliseconds since the epoch; zero or absent means emptied.
    let expires = oauth
        .get("expiresAt")
        .and_then(Value::as_f64)
        .unwrap_or(0.0);
    if expires / 1000.0 <= now as f64 {
        return Err(Status::Expired);
    }
    let plan = oauth
        .get("subscriptionType")
        .and_then(Value::as_str)
        .map(capitalize);
    Ok(ClaudeLogin { token, plan })
}

struct CodexLogin {
    token: String,
    account: String,
    /// The plan named by the identity token, used when the usage payload names none.
    plan: Option<String>,
    /// When the paid plan runs to (unix seconds), if that is still ahead.
    plan_until: Option<u64>,
}

fn codex_login(now: u64) -> Result<CodexLogin, Status> {
    let dir = std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| home().map(|h| h.join(".codex")))
        .ok_or(Status::SignIn)?;
    let root = read_small_json(&dir.join("auth.json"))?;
    let text = |key: &str| {
        root.path(&["tokens", key])
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(str::to_string)
    };
    let token = text("access_token").ok_or(Status::SignIn)?;
    let account = text("account_id").ok_or(Status::SignIn)?;
    if jwt_expiry(&token).is_some_and(|exp| exp <= now) {
        return Err(Status::Expired);
    }
    // Identity claims (the plan and when it runs to) are labels only; the server decides access.
    let claims = text("id_token").and_then(|id| jwt_claims(&id));
    let auth = claims
        .as_ref()
        .and_then(|claims| claims.get("https://api.openai.com/auth"));
    let plan = auth
        .and_then(|auth| auth.get("chatgpt_plan_type"))
        .and_then(Value::as_str)
        .map(capitalize);
    let plan_until = auth
        .and_then(|auth| auth.get("chatgpt_subscription_active_until"))
        .and_then(Value::as_str)
        .and_then(parse_iso8601)
        .filter(|until| *until > now);
    Ok(CodexLogin {
        token,
        account,
        plan,
        plan_until,
    })
}

// ------------------------------------------------------------------ polling

/// Claude Code's own account: its id and the address it is signed in with, from the
/// `oauthAccount` member of its global config. Only that member is scanned out of the file;
/// nothing else in it is parsed.
#[derive(Default)]
struct CliAccount {
    id: Option<String>,
    email: Option<String>,
}

fn claude_cli_account() -> CliAccount {
    let dir = std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(home);
    let found = dir.and_then(|dir| {
        let bytes = desktop::read_bounded(&dir.join(".claude.json"), CLAUDE_CONFIG_MAX_BYTES)?;
        let account = desktop::object_member(&bytes, "oauthAccount")?;
        json::parse(account, CREDENTIAL_MAX_BYTES)
    });
    let Some(root) = found else {
        return CliAccount::default();
    };
    let id = root
        .get("accountUuid")
        .and_then(Value::as_str)
        .map(|id| id.trim().to_ascii_lowercase())
        .filter(|id| desktop::is_account_id(id));
    let email = root
        .get("emailAddress")
        .and_then(Value::as_str)
        .map(|email| email.trim().to_string())
        .filter(|email| !email.is_empty());
    CliAccount { id, email }
}

/// What the card calls a Desktop account Claude Code is not signed into: the name chosen in
/// the hub, else the start of its id.
fn account_label(id: &str) -> String {
    claude_accounts::label(id, None)
}

/// The Claude reading. The account tracked is the one Claude Desktop is signed into while it
/// runs, else Claude Code's. When Desktop's differs from Claude Code's, the login, the usage
/// endpoint and its back-off all describe the other account, so only Desktop's own cache can
/// answer. `None` leaves the published reading as it is (inside the endpoint's back-off).
fn poll_claude(now: u64, tracker: &mut Tracker) -> Option<Outcome> {
    let cli = claude_cli_account();
    let outcome = poll_claude_for(now, tracker, &cli)?;
    // A fresh reading is remembered under the tracked account's own id, so the hub can show
    // each account's last reading and the one before it is never overwritten by another's.
    if let Outcome::Fresh {
        windows,
        plan,
        updated,
        ..
    } = &outcome
        && let Some(id) = tracker.account.as_deref()
    {
        let email = if cli.id.as_deref() == Some(id) {
            cli.email.as_deref()
        } else {
            None
        };
        claude_accounts::record(id, email, plan.as_deref(), windows, *updated);
    }
    Some(outcome)
}

fn poll_claude_for(now: u64, tracker: &mut Tracker, cli: &CliAccount) -> Option<Outcome> {
    set_claude_source("none");
    let desktop_account = desktop::signed_in_account();
    let tracked = desktop_account.clone().or_else(|| cli.id.clone());
    *CLAUDE_IDS.lock().unwrap_or_else(PoisonError::into_inner) = ClaudeIds {
        active: tracked.clone(),
        code: cli.id.clone(),
        email: cli.email.clone(),
    };
    if !tracker.account_seen || tracker.account != tracked {
        // Readings cached before this moment may belong to the previous account, and
        // organizations can be shared between accounts.
        tracker.account_since = if tracker.account_seen { now } else { 0 };
        tracker.account_seen = true;
        tracker.account = tracked;
        tracker.account_changed = true;
    }
    if let Some(id) = desktop_account.as_deref()
        && cli.id.as_deref() != Some(id)
    {
        set_claude_source("desktop_cache");
        return Some(desktop_only(id, now, tracker));
    }
    if now < tracker.backoff_until {
        // Cache first (the Mac reads Desktop's cache ahead of the back-off check): the usage
        // endpoint is resting, but Desktop's own file for this account may still answer.
        if let Some(id) = desktop_account.as_deref()
            && let Some(cached) = desktop_cache_outcome(id, now, tracker)
        {
            set_claude_source("desktop_cache");
            return Some(cached);
        }
        set_claude_source("backoff");
        return tracker
            .account_changed
            .then_some(Outcome::Failed(Status::RateLimited));
    }
    set_claude_source("endpoint");
    let mut outcome = poll_claude_endpoint(now);
    if matches!(outcome, Outcome::Failed(Status::RateLimited)) {
        // Begun here, not only when the failure is published: a cached or CLI reading below
        // replaces this outcome, and the endpoint must still rest.
        start_backoff(tracker, now);
    }
    // Desktop runs as the same account Claude Code is signed into but the login cannot answer
    // (an expired token, or a 429 on this account): Desktop's cache for that account still
    // can, and is shown (dated) instead of an error.
    if let Some(id) = desktop_account.as_deref()
        && matches!(
            outcome,
            Outcome::Failed(
                Status::Expired | Status::SignIn | Status::Unavailable | Status::RateLimited
            )
        )
        && let Some(cached) = desktop_cache_outcome(id, now, tracker)
    {
        set_claude_source("desktop_cache");
        outcome = cached;
    }
    // Still nothing: ask `claude /usage` (the Mac's ClaudeUsageCLI), which answers off the
    // credential Claude Code itself holds.
    if matches!(
        outcome,
        Outcome::Failed(
            Status::Expired
                | Status::SignIn
                | Status::Unavailable
                | Status::AccessDenied
                | Status::RateLimited
        )
    ) && let Some(reading) = claude_usage_cli(now)
    {
        set_claude_source("claude_cli");
        outcome = reading;
    }
    Some(outcome)
}

/// Desktop's cached reading for `id` as a fresh outcome, when it has a usable one.
fn desktop_cache_outcome(id: &str, now: u64, tracker: &Tracker) -> Option<Outcome> {
    let organizations = desktop::organizations(id);
    let reading = desktop::cached_usage(&organizations, now, tracker.account_since).ok()?;
    Some(Outcome::Fresh {
        windows: reading.windows,
        plan: None,
        updated: reading.captured.min(now),
        account: None,
        endpoint: false,
        extras: Extras::default(),
    })
}

/// The reading for a Desktop account Claude Code is not signed into: Desktop's cache or
/// nothing, and the reason for nothing goes to the log once.
fn desktop_only(id: &str, now: u64, tracker: &mut Tracker) -> Outcome {
    let account = account_label(id);
    let organizations = desktop::organizations(id);
    match desktop::cached_usage(&organizations, now, tracker.account_since) {
        Ok(reading) => {
            tracker.note = "";
            Outcome::Fresh {
                windows: reading.windows,
                plan: None,
                updated: reading.captured.min(now),
                account: Some(account),
                endpoint: false,
                extras: Extras::default(),
            }
        }
        Err(why) => {
            set_claude_source("desktop_cache");
            CLAUDE_SOURCE
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .1 = why;
            if tracker.note != why {
                tracker.note = why;
                diag::info("claude_desktop_cache", &[("result", why)]);
            }
            Outcome::NoReading { account }
        }
    }
}

fn poll_claude_endpoint(now: u64) -> Outcome {
    let login = match claude_login(now) {
        Ok(login) => login,
        Err(status) => return Outcome::Failed(status),
    };
    let bearer = format!("Bearer {}", login.token);
    let response = http::get(
        "api.anthropic.com",
        "/api/oauth/usage?cedar_ember=1",
        &[
            ("Authorization", bearer.as_str()),
            ("anthropic-beta", "oauth-2025-04-20"),
            ("Accept", "application/json"),
        ],
        REQUEST_TIMEOUT_MS,
    );
    match classify(response, claude_windows) {
        Ok(windows) => Outcome::Fresh {
            windows,
            plan: login.plan,
            updated: now,
            account: None,
            endpoint: true,
            extras: Extras::default(),
        },
        Err(status) => Outcome::Failed(status),
    }
}

// ------------------------------------------------------------------ claude /usage

const CLI_TIMEOUT: Duration = Duration::from_secs(20);
const CLI_OUTPUT_MAX: u64 = 256 * 1024;
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[allow(non_snake_case)]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn TzSpecificLocalTimeToSystemTime(
        zone: *const c_void,
        local: *const SYSTEMTIME,
        universal: *mut SYSTEMTIME,
    ) -> i32;
}

/// Where Claude Code is installed: every absolute `PATH` entry, then its own install
/// directories (the native installer, the npm global directory).
fn claude_binary() -> Option<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).collect())
        .unwrap_or_default();
    if let Some(home) = home() {
        dirs.push(home.join(".local").join("bin"));
        dirs.push(home.join(".claude").join("local"));
    }
    if let Some(appdata) = std::env::var_os("APPDATA").map(PathBuf::from) {
        dirs.push(appdata.join("npm"));
    }
    dirs.into_iter()
        .filter(|dir| dir.is_absolute())
        .flat_map(|dir| [dir.join("claude.exe"), dir.join("claude.cmd")])
        .find(|path| path.is_file())
}

/// Runs `claude --print /usage` from a scratch folder of its own and returns what it printed.
/// No terminal, no MCP servers, no transcript; a timeout ends it.
fn claude_usage_text() -> Option<String> {
    let binary = claude_binary()?;
    let scratch = data_dir()?.join("usage-scratch");
    std::fs::create_dir_all(&scratch).ok()?;
    let mut child = Command::new(binary)
        .args([
            "--print",
            "--no-session-persistence",
            "--strict-mcp-config",
            "/usage",
        ])
        .current_dir(&scratch)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = (&mut stdout).take(CLI_OUTPUT_MAX).read_to_end(&mut bytes);
        bytes
    });
    let deadline = std::time::Instant::now() + CLI_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(100));
            }
            _ => {
                // The reader is left to finish by itself: a child of the killed process may
                // still hold the pipe.
                let _ = child.kill();
                let _ = child.wait();
                diag::info("claude_usage_cli", &[("result", "timeout")]);
                return None;
            }
        }
    };
    if !status.success() {
        diag::info("claude_usage_cli", &[("result", "declined")]);
        return None;
    }
    String::from_utf8(reader.join().ok()?).ok()
}

fn claude_usage_cli(now: u64) -> Option<Outcome> {
    let text = claude_usage_text()?;
    let windows = parse_cli_usage(&text, now);
    if !windows.iter().any(|w| w.key == "session") {
        diag::info("claude_usage_cli", &[("result", "unreadable")]);
        return None;
    }
    Some(Outcome::Fresh {
        windows,
        plan: cli_plan(&text),
        updated: now,
        account: None,
        endpoint: false,
        extras: Extras::default(),
    })
}

/// The named tier printed in the first lines, as printed.
fn cli_plan(text: &str) -> Option<String> {
    let head: String = text.lines().take(4).collect::<Vec<_>>().join("\n");
    let lower = head.to_ascii_lowercase();
    ["Max 20x", "Max 5x", "extra usage", "Max", "Pro", "Team"]
        .iter()
        .find(|phrase| lower.contains(&phrase.to_ascii_lowercase()))
        .map(|phrase| (*phrase).to_string())
}

/// The lines `/usage` leads with:
/// `Current session: 38% used · resets Sep 7 at 2:59pm (Asia/Jakarta)` and
/// `Current week (all models): 4% used · resets ...`. The prose below them is ignored.
fn parse_cli_usage(text: &str, now: u64) -> Vec<LimitWindow> {
    let mut windows: Vec<LimitWindow> = Vec::new();
    for line in text.lines() {
        let Some(rest) = line.trim().strip_prefix("Current ") else {
            continue;
        };
        let (kind, body) = if let Some(body) = rest.strip_prefix("session:") {
            ("session".to_string(), body)
        } else if let Some((name, body)) = rest
            .strip_prefix("week (")
            .and_then(|week| week.split_once("):"))
        {
            let name = name.trim().to_lowercase();
            let name = if name == "all models" {
                "all".to_string()
            } else {
                name.replace(' ', "_")
            };
            (format!("weekly_{name}"), body)
        } else {
            continue;
        };
        if windows.iter().any(|w| w.key == kind) {
            continue;
        }
        let body = body.trim_start();
        let digits: String = body.chars().take_while(char::is_ascii_digit).collect();
        let Ok(percent) = digits.parse::<f64>() else {
            continue;
        };
        let Some(after) = body[digits.len()..]
            .trim_start()
            .strip_prefix('%')
            .and_then(|a| a.trim_start().strip_prefix("used"))
        else {
            continue;
        };
        let resets_at = after
            .split_once('\u{b7}')
            .and_then(|(_, reset)| reset.trim().strip_prefix("resets"))
            .and_then(|date| cli_reset_date(date.trim(), now));
        windows.push(LimitWindow {
            label: claude_label(&kind),
            key: kind,
            group: None,
            fraction: percent_to_fraction(percent),
            resets_at,
        });
    }
    windows.sort_by_key(|w| claude_order(&w.key));
    windows
}

/// `Sep 7 at 2:59pm (Asia/Jakarta)` or `Sep 7 at 3pm` as unix seconds. No year is printed:
/// the candidate nearest `now` among last, this and next year is taken. The time is read as
/// this PC's local time (the zone in brackets is the one `claude` itself ran in).
fn cli_reset_date(text: &str, now: u64) -> Option<u64> {
    let text = match text.rfind('(') {
        Some(open) if text.ends_with(')') => text[..open].trim(),
        _ => text.trim(),
    };
    let mut parts = text.split_whitespace();
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let name = parts.next()?;
    let month = u16::try_from(
        MONTHS
            .iter()
            .position(|month| month.eq_ignore_ascii_case(name))?,
    )
    .ok()?
        + 1;
    let day: u16 = parts.next()?.parse().ok()?;
    if parts.next()? != "at" {
        return None;
    }
    let clock = parts.next()?.to_ascii_lowercase();
    let (clock, pm) = match (clock.strip_suffix("pm"), clock.strip_suffix("am")) {
        (Some(c), _) => (c.to_string(), true),
        (_, Some(c)) => (c.to_string(), false),
        _ => return None,
    };
    let (hour, minute) = match clock.split_once(':') {
        Some((h, m)) => (h.parse::<u16>().ok()?, m.parse::<u16>().ok()?),
        None => (clock.parse::<u16>().ok()?, 0),
    };
    if !(1..=12).contains(&hour) || minute > 59 || !(1..=31).contains(&day) {
        return None;
    }
    let hour = hour % 12 + if pm { 12 } else { 0 };
    let this_year = civil_from_days(i64::try_from(now / 86_400).ok()?).0;
    (this_year - 1..=this_year + 1)
        .filter_map(|year| {
            let local = SYSTEMTIME {
                wYear: u16::try_from(year).ok()?,
                wMonth: month,
                wDay: day,
                wHour: hour,
                wMinute: minute,
                ..SYSTEMTIME::default()
            };
            let mut utc = SYSTEMTIME::default();
            // SAFETY: both pointers are to live SYSTEMTIMEs; a null zone means the current one.
            let converted =
                unsafe { TzSpecificLocalTimeToSystemTime(std::ptr::null(), &local, &mut utc) };
            if converted == 0 {
                return None;
            }
            let days = days_from_civil(
                i64::from(utc.wYear),
                i64::from(utc.wMonth),
                i64::from(utc.wDay),
            );
            let seconds = days * 86_400
                + i64::from(utc.wHour) * 3600
                + i64::from(utc.wMinute) * 60
                + i64::from(utc.wSecond);
            u64::try_from(seconds).ok()
        })
        .min_by_key(|at| at.abs_diff(now))
}

fn poll_codex(now: u64) -> Outcome {
    let login = match codex_login(now) {
        Ok(login) => login,
        Err(status) => return Outcome::Failed(status),
    };
    let bearer = format!("Bearer {}", login.token);
    let response = http::get(
        "chatgpt.com",
        "/backend-api/wham/usage",
        &[
            ("Authorization", bearer.as_str()),
            ("ChatGPT-Account-Id", login.account.as_str()),
            ("Accept", "application/json"),
        ],
        REQUEST_TIMEOUT_MS,
    );
    let mut plan = None;
    let mut extras = Extras {
        credits: None,
        plan_until: login.plan_until,
        resets: None,
    };
    let result = classify(response, |root| {
        plan = root
            .get("plan_type")
            .and_then(Value::as_str)
            .map(capitalize);
        extras.credits = codex_credits_text(root);
        codex_windows(root, now)
    });
    match result {
        Ok(windows) => {
            // Unused resets are a separate call; it is best effort and never fails the reading.
            extras.resets = codex_reset_credits(&login, &bearer, now);
            Outcome::Fresh {
                windows,
                plan: plan.or(login.plan),
                updated: now,
                account: None,
                endpoint: true,
                extras,
            }
        }
        Err(status) => Outcome::Failed(status),
    }
}

/// The account's unused rate-limit resets, from the same backend and sign-in as the usage.
fn codex_reset_credits(login: &CodexLogin, bearer: &str, now: u64) -> Option<ResetCredits> {
    let response = http::get(
        "chatgpt.com",
        "/backend-api/wham/rate-limit-reset-credits",
        &[
            ("Authorization", bearer),
            ("ChatGPT-Account-Id", login.account.as_str()),
            ("Accept", "application/json"),
            ("OpenAI-Beta", "codex-1"),
        ],
        REQUEST_TIMEOUT_MS,
    )
    .ok()?;
    if !(200..300).contains(&response.status) {
        return None;
    }
    let root = json::parse(&response.body, RESPONSE_MAX_BYTES)?;
    Some(codex_resets(&root, now))
}

/// Maps an HTTP result to windows or a status. Empty window lists are "unavailable", never
/// an invented zero.
fn classify(
    response: Result<http::Response, http::HttpError>,
    windows: impl FnOnce(&Value) -> Vec<LimitWindow>,
) -> Result<Vec<LimitWindow>, Status> {
    let response = response.map_err(|_| Status::Unavailable)?;
    match response.status {
        401 | 403 => Err(Status::SignIn),
        429 => {
            SERVER_RETRY.store(response.retry_after.unwrap_or(0), Ordering::Relaxed);
            Err(Status::RateLimited)
        }
        200..=299 => {
            let root =
                json::parse(&response.body, RESPONSE_MAX_BYTES).ok_or(Status::Unavailable)?;
            let found = windows(&root);
            if found.is_empty() {
                Err(Status::Unavailable)
            } else {
                Ok(found)
            }
        }
        _ => Err(Status::Unavailable),
    }
}

// ------------------------------------------------------------------ response parsing

fn percent_to_fraction(percent: f64) -> f32 {
    (percent / 100.0).clamp(0.0, 1.0) as f32
}

fn capitalize(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) if text.chars().all(|c| c.is_lowercase() || !c.is_alphabetic()) => {
            first.to_uppercase().collect::<String>() + chars.as_str()
        }
        _ => text.to_string(),
    }
}

fn claude_label(kind: &str) -> String {
    match kind {
        "session" => "Current session".into(),
        "weekly_all" => "All models".into(),
        "weekly_opus" => "Opus".into(),
        "weekly_sonnet" => "Sonnet".into(),
        "weekly_scoped" | "scoped" => "Scoped".into(),
        other => capitalize(&other.replace("weekly_", "").replace('_', " ")),
    }
}

fn claude_order(key: &str) -> u8 {
    match key {
        "session" => 0,
        "weekly_all" => 1,
        _ => 2,
    }
}

/// `limits` is the forward-compatible list; the two named windows are merged in so a window
/// that has just rolled over (missing from `limits`) is not lost. A window without a reset
/// time is skipped, as on the Mac notch.
pub fn claude_windows(root: &Value) -> Vec<LimitWindow> {
    let mut windows: Vec<LimitWindow> = Vec::new();
    if let Some(items) = root.get("limits").and_then(Value::as_array) {
        for item in items {
            let Some(kind) = item.get("kind").and_then(Value::as_str) else {
                continue;
            };
            let Some(percent) = item.get("percent").and_then(Value::as_f64) else {
                continue;
            };
            let Some(resets_at) = item
                .get("resets_at")
                .and_then(Value::as_str)
                .and_then(parse_iso8601)
            else {
                continue;
            };
            let model = item
                .path(&["scope", "model", "display_name"])
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|name| !name.is_empty());
            windows.push(LimitWindow {
                key: kind.to_string(),
                group: None,
                label: model.map_or_else(|| claude_label(kind), str::to_string),
                fraction: percent_to_fraction(percent),
                resets_at: Some(resets_at),
            });
        }
    }
    for (member, key) in [("five_hour", "session"), ("seven_day", "weekly_all")] {
        if windows.iter().any(|w| w.key == key) {
            continue;
        }
        let Some(window) = root.get(member) else {
            continue;
        };
        let Some(utilization) = window.get("utilization").and_then(Value::as_f64) else {
            continue;
        };
        let Some(resets_at) = window
            .get("resets_at")
            .and_then(Value::as_str)
            .and_then(parse_iso8601)
        else {
            continue;
        };
        windows.push(LimitWindow {
            key: key.to_string(),
            group: None,
            label: claude_label(key),
            fraction: percent_to_fraction(utilization),
            resets_at: Some(resets_at),
        });
    }
    windows.sort_by_key(|w| claude_order(&w.key));
    windows
}

fn codex_label(window_seconds: f64, fallback_primary: bool) -> String {
    if window_seconds <= 0.0 {
        return if fallback_primary {
            "Current session".into()
        } else {
            "Longer window".into()
        };
    }
    let minutes = window_seconds / 60.0;
    if minutes < 60.0 {
        return format!("{}m limit", minutes as u64);
    }
    if minutes < 60.0 * 24.0 {
        return format!("{}h limit", (minutes / 60.0) as u64);
    }
    let days = (minutes / (60.0 * 24.0)).round() as u64;
    match days {
        7 => "Weekly limit".into(),
        30 => "Monthly limit".into(),
        other => format!("{other}d limit"),
    }
}

/// One Codex window object as a `LimitWindow`, or `None` when it has no used share.
fn codex_window(
    window: &Value,
    key: &str,
    group: Option<&str>,
    primary: bool,
    now: u64,
) -> Option<LimitWindow> {
    let percent = window.get("used_percent").and_then(Value::as_f64)?;
    let seconds = window
        .get("limit_window_seconds")
        .and_then(Value::as_f64)
        .unwrap_or(0.0);
    let resets_at = window
        .get("reset_at")
        .and_then(Value::as_f64)
        .map(|at| at as u64)
        .or_else(|| {
            window
                .get("reset_after_seconds")
                .and_then(Value::as_f64)
                .map(|after| now + after as u64)
        });
    Some(LimitWindow {
        key: key.to_string(),
        group: group.map(str::to_string),
        label: codex_label(seconds, primary),
        fraction: percent_to_fraction(percent),
        resets_at,
    })
}

/// A rate-limit object's primary then secondary window under the given keys. One unreadable
/// window never drops its sibling; keys already present are not added twice.
fn push_codex_pair(
    windows: &mut Vec<LimitWindow>,
    limit: &Value,
    keys: (&str, &str),
    group: Option<&str>,
    now: u64,
) {
    for (key, member, primary) in [
        (keys.0, "primary_window", true),
        (keys.1, "secondary_window", false),
    ] {
        if windows.iter().any(|w| w.key == key) {
            continue;
        }
        if let Some(window) = limit
            .get(member)
            .and_then(|w| codex_window(w, key, group, primary, now))
        {
            windows.push(window);
        }
    }
}

fn is_spark(extra: &Value) -> bool {
    ["limit_name", "metered_feature"].iter().any(|name| {
        extra
            .get(name)
            .and_then(Value::as_str)
            .is_some_and(|text| text.to_ascii_lowercase().contains("spark"))
    })
}

/// The account's primary and secondary windows, then (as on the Mac) the Spark model's own
/// windows and code review's, each grouped under a title for the hover card.
pub fn codex_windows(root: &Value, now: u64) -> Vec<LimitWindow> {
    let mut windows = Vec::new();
    if let Some(limit) = root.get("rate_limit") {
        push_codex_pair(&mut windows, limit, ("primary", "secondary"), None, now);
    }
    if let Some(extras) = root.get("additional_rate_limits").and_then(Value::as_array) {
        for extra in extras.iter().filter(|extra| is_spark(extra)) {
            if let Some(limit) = extra.get("rate_limit") {
                let keys = ("spark", "spark-secondary");
                push_codex_pair(&mut windows, limit, keys, Some("Spark"), now);
            }
        }
    }
    if let Some(limit) = root.get("code_review_rate_limit") {
        let keys = ("code-review", "code-review-secondary");
        push_codex_pair(&mut windows, limit, keys, Some("Code review"), now);
    }
    // No rolling windows at all: a credit-based (Business or Team) seat, whose spend cap is
    // the only allowance it can show.
    if windows.is_empty()
        && let Some(window) = codex_credit_cap(root)
    {
        windows.push(window);
    }
    windows
}

/// A number the backend sends either as a JSON number or as a decimal string ("374.92").
fn json_number(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_str().and_then(|text| text.trim().parse().ok()))
        .filter(|number| number.is_finite())
}

/// The workspace spend control's cap as one window.
fn codex_credit_cap(root: &Value) -> Option<LimitWindow> {
    let limit = root.path(&["spend_control", "individual_limit"])?;
    let percent = limit.get("used_percent").and_then(json_number)?;
    Some(LimitWindow {
        key: "credits".to_string(),
        group: None,
        label: "Credits".to_string(),
        fraction: percent_to_fraction(percent),
        resets_at: limit
            .get("reset_at")
            .and_then(json_number)
            .map(|at| at as u64),
    })
}

/// The prepaid balance as card text: "Unlimited", or the number when there is a balance (or
/// the account says it has credits). `None` when the payload carries none.
pub fn codex_credits_text(root: &Value) -> Option<String> {
    let credits = root.get("credits")?;
    if credits.get("unlimited").and_then(Value::as_bool) == Some(true) {
        return Some("Unlimited".to_string());
    }
    let balance = credits.get("balance").and_then(json_number)?;
    let has_credits = credits.get("has_credits").and_then(Value::as_bool) == Some(true);
    if balance <= 0.0 && !has_credits {
        return None;
    }
    let text = format!("{balance:.2}");
    Some(text.trim_end_matches('0').trim_end_matches('.').to_string())
}

/// Unused resets: `available_count` is trusted even when the `credits` list is truncated; an
/// available credit already past its expiry is not counted.
pub fn codex_resets(root: &Value, now: u64) -> ResetCredits {
    let items = root.get("credits").and_then(Value::as_array).unwrap_or(&[]);
    let expiries: Vec<Option<u64>> = items
        .iter()
        .filter(|item| item.get("status").and_then(Value::as_str) == Some("available"))
        .map(|item| {
            item.get("expires_at")
                .and_then(Value::as_str)
                .and_then(parse_iso8601)
        })
        .collect();
    let reported = root
        .get("available_count")
        .and_then(Value::as_f64)
        .map(|count| count.max(0.0) as u32);
    let expired = expiries
        .iter()
        .flatten()
        .filter(|expiry| **expiry <= now)
        .count() as u32;
    ResetCredits {
        available: reported
            .unwrap_or(expiries.len() as u32)
            .saturating_sub(expired),
        next_expiry: expiries
            .iter()
            .flatten()
            .copied()
            .filter(|expiry| *expiry > now)
            .min(),
    }
}

// ------------------------------------------------------------------ time and token helpers

/// Seconds since the epoch for `YYYY-MM-DDTHH:MM:SS[.fff](Z|+hh:mm|-hh:mm)`.
pub fn parse_iso8601(text: &str) -> Option<u64> {
    let bytes = text.as_bytes();
    let number = |start: usize, end: usize| -> Option<i64> { text.get(start..end)?.parse().ok() };
    let year = number(0, 4)?;
    let month = number(5, 7)?;
    let day = number(8, 10)?;
    let hour = number(11, 13)?;
    let minute = number(14, 16)?;
    let second = number(17, 19)?;
    if bytes.get(4) != Some(&b'-')
        || bytes.get(7) != Some(&b'-')
        || !matches!(bytes.get(10), Some(b'T' | b't' | b' '))
        || bytes.get(13) != Some(&b':')
        || bytes.get(16) != Some(&b':')
        || !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 60
    {
        return None;
    }
    let mut index = 19;
    if bytes.get(index) == Some(&b'.') {
        index += 1;
        while bytes.get(index).is_some_and(u8::is_ascii_digit) {
            index += 1;
        }
    }
    let offset_seconds = match bytes.get(index) {
        None | Some(b'Z' | b'z') => 0,
        Some(sign @ (b'+' | b'-')) => {
            let hours = number(index + 1, index + 3)?;
            let minutes_at = if bytes.get(index + 3) == Some(&b':') {
                index + 4
            } else {
                index + 3
            };
            let minutes = number(minutes_at, minutes_at + 2)?;
            let total = hours * 3600 + minutes * 60;
            if *sign == b'-' { -total } else { total }
        }
        _ => return None,
    };
    let days = days_from_civil(year, month, day);
    let seconds = days * 86_400 + hour * 3600 + minute * 60 + second - offset_seconds;
    u64::try_from(seconds).ok()
}

/// Days since 1970-01-01 for a proleptic Gregorian date (Howard Hinnant's algorithm).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let shifted_month = (month + 9) % 12;
    let day_of_year = (153 * shifted_month + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

fn base64url_decode(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let mut buffer = 0u32;
    let mut bits = 0u32;
    for byte in text.bytes() {
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'-' | b'+' => 62,
            b'_' | b'/' => 63,
            b'=' => break,
            _ => return None,
        };
        buffer = (buffer << 6) | u32::from(value);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buffer >> bits) as u8);
            buffer &= (1 << bits) - 1;
        }
    }
    Some(out)
}

/// The `exp` claim (seconds) of a JWT, used only as a local expiry hint; the server decides.
pub fn jwt_expiry(token: &str) -> Option<u64> {
    jwt_claims(token)?
        .get("exp")
        .and_then(Value::as_f64)
        .map(|e| e as u64)
}

/// The claims object of a JWT, unverified: identity labels and local hints only.
fn jwt_claims(token: &str) -> Option<Value> {
    let payload = token.split('.').nth(1)?;
    let bytes = base64url_decode(payload)?;
    json::parse(&bytes, CREDENTIAL_MAX_BYTES)
}

/// Calendar date (year, month 1..=12, day) of a day count since 1970-01-01.
pub fn civil_from_days(days: i64) -> (i64, usize, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted + 2) / 5 + 1;
    let month = if shifted < 10 {
        shifted + 3
    } else {
        shifted - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month as usize, day)
}
