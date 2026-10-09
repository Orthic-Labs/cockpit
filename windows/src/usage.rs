//! AI usage readers (Claude and Codex) for the AI rings. Read-only: credentials are only
//! read from the files the tools themselves keep (`~/.claude/.credentials.json`,
//! `~/.codex/auth.json`), kept in memory for one request, never refreshed, never written,
//! never logged. A background thread polls every five minutes with back-off after a 429;
//! unavailable readings stay unavailable (`--`), never zero. Parsing is pure and shared
//! with nothing Win32 so it can be reasoned about on its own.

use crate::diag;
use crate::http;
use crate::json::{self, Value};
use crate::raii::hwnd_from_key;
use std::path::{Path, PathBuf};
use std::sync::{Condvar, Mutex, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

/// Posted to the controller window when a poll changed a reading (WM_APP + 1).
pub const MSG_USAGE_UPDATED: u32 = 0x8001;

const POLL_SECONDS: u64 = 300;
const REQUEST_TIMEOUT_MS: i32 = 15_000;
const CREDENTIAL_MAX_BYTES: usize = 64 * 1024;
const RESPONSE_MAX_BYTES: usize = 512 * 1024;
const BACKOFF_FLOOR_SECONDS: u64 = 60;
const BACKOFF_CEILING_SECONDS: u64 = 15 * 60;

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
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct LimitWindow {
    /// Stable id: `session`/`weekly_all`/... for Claude, `primary`/`secondary` for Codex.
    pub key: String,
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
}

impl Usage {
    pub const fn waiting() -> Self {
        Self {
            status: Status::Waiting,
            plan: None,
            windows: Vec::new(),
            updated: None,
        }
    }

    /// The window the main ring means: the session / 5-hour window.
    pub fn headline(&self) -> Option<&LimitWindow> {
        self.windows
            .iter()
            .find(|w| w.key == "session" || w.key == "primary")
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
        .wait_timeout_while(guard, Duration::from_secs(seconds), |stopped| !*stopped)
        .unwrap_or_else(PoisonError::into_inner);
    !*guard
}

#[derive(Default)]
struct Tracker {
    consecutive_rate_limits: u32,
    backoff_until: u64,
}

fn worker(controller_key: isize) {
    let mut trackers = [Tracker::default(), Tracker::default()];
    loop {
        let now = now_secs();
        let mut changed = false;
        for provider in Provider::ALL {
            let tracker = &mut trackers[provider.index()];
            if now < tracker.backoff_until {
                continue;
            }
            let outcome = match provider {
                Provider::Claude => poll_claude(now),
                Provider::Codex => poll_codex(now),
            };
            let next = apply_outcome(provider, tracker, outcome, now);
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
        if !wait(POLL_SECONDS) {
            return;
        }
    }
}

enum Outcome {
    Fresh {
        windows: Vec<LimitWindow>,
        plan: Option<String>,
    },
    Failed(Status),
}

/// Folds one poll into the published reading: fresh data replaces it; a failure keeps the
/// last good windows (dimmed by the UI) unless their reset time has passed.
fn apply_outcome(provider: Provider, tracker: &mut Tracker, outcome: Outcome, now: u64) -> Usage {
    let previous = USAGE.lock().unwrap_or_else(PoisonError::into_inner)[provider.index()].clone();
    match outcome {
        Outcome::Fresh { windows, plan } => {
            tracker.consecutive_rate_limits = 0;
            tracker.backoff_until = 0;
            Usage {
                status: Status::Ok,
                plan,
                windows,
                updated: Some(now),
            }
        }
        Outcome::Failed(status) => {
            if status == Status::RateLimited {
                tracker.consecutive_rate_limits = tracker.consecutive_rate_limits.saturating_add(1);
                tracker.backoff_until = now + backoff_seconds(tracker.consecutive_rate_limits);
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
            }
        }
    }
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

fn read_small_json(path: &Path) -> Option<Value> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > CREDENTIAL_MAX_BYTES as u64 {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    json::parse(&bytes, CREDENTIAL_MAX_BYTES)
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
    let root = read_small_json(&dir.join(".credentials.json")).ok_or(Status::SignIn)?;
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
}

fn codex_login(now: u64) -> Result<CodexLogin, Status> {
    let dir = std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| home().map(|h| h.join(".codex")))
        .ok_or(Status::SignIn)?;
    let root = read_small_json(&dir.join("auth.json")).ok_or(Status::SignIn)?;
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
    Ok(CodexLogin { token, account })
}

// ------------------------------------------------------------------ polling

fn poll_claude(now: u64) -> Outcome {
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
        },
        Err(status) => Outcome::Failed(status),
    }
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
    let result = classify(response, |root| {
        plan = root
            .get("plan_type")
            .and_then(Value::as_str)
            .map(capitalize);
        codex_windows(root, now)
    });
    match result {
        Ok(windows) => Outcome::Fresh { windows, plan },
        Err(status) => Outcome::Failed(status),
    }
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
        429 => Err(Status::RateLimited),
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

/// Primary and secondary rate-limit windows. One unreadable window never drops its sibling.
pub fn codex_windows(root: &Value, now: u64) -> Vec<LimitWindow> {
    let mut windows = Vec::new();
    for (key, member) in [
        ("primary", "primary_window"),
        ("secondary", "secondary_window"),
    ] {
        let Some(window) = root.path(&["rate_limit", member]) else {
            continue;
        };
        let Some(percent) = window.get("used_percent").and_then(Value::as_f64) else {
            continue;
        };
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
        windows.push(LimitWindow {
            key: key.to_string(),
            label: codex_label(seconds, key == "primary"),
            fraction: percent_to_fraction(percent),
            resets_at,
        });
    }
    windows
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
    let payload = token.split('.').nth(1)?;
    let bytes = base64url_decode(payload)?;
    let claims = json::parse(&bytes, CREDENTIAL_MAX_BYTES)?;
    claims.get("exp").and_then(Value::as_f64).map(|e| e as u64)
}

/// "2h 14m", "5d 3h", "12m" until `resets_at`; "now" once it has passed.
pub fn reset_in(resets_at: u64, now: u64) -> String {
    let remaining = resets_at.saturating_sub(now);
    let (days, hours, minutes) = (
        remaining / 86_400,
        remaining % 86_400 / 3600,
        remaining % 3600 / 60,
    );
    if remaining == 0 {
        "now".into()
    } else if days > 0 {
        format!("{days}d {hours}h")
    } else if hours > 0 {
        format!("{hours}h {minutes}m")
    } else {
        format!("{}m", minutes.max(1))
    }
}

/// "just now", "12 min ago", "3 h ago", "2 d ago".
pub fn age_text(updated: u64, now: u64) -> String {
    let seconds = now.saturating_sub(updated);
    if seconds < 90 {
        "just now".into()
    } else if seconds < 3600 {
        format!("{} min ago", seconds / 60)
    } else if seconds < 86_400 {
        format!("{} h ago", seconds / 3600)
    } else {
        format!("{} d ago", seconds / 86_400)
    }
}
