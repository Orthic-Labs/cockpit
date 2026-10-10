//! Usage alert cards: a limit window that reset, a session or weekly limit that was spent, and
//! the 80% / 100% crossings. A port of the Mac's `UsageResetWatcher`, `UsageLimitWatcher` and
//! `ThresholdNotifier` (`mac/Notch/Sources/Model/`) over `usage.rs` readings, drawn by the
//! notch's card window like the Mac's `UsageResetCard` and put away after five or six seconds
//! (or by a click). Pure state: `main.rs` feeds readings in and asks for the card to show.
//!
//! The watchers are difference engines, so they see every reading whether or not an alert is
//! switched on. An archived (stale) reading is never a baseline and the first live reading of
//! a window only records, so nothing rings at start-up.

use crate::card::{CardContent, Head, Mark, Row, Tone};
use crate::notify::{self, Sound};
use crate::send::Panel;
use crate::sensors;
use crate::settings::PillSettings;
use crate::usage::{LimitWindow, Usage};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard, Once, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Seconds a reset card stays up (the Mac's `showResetAlert(duration: 5.0)`).
pub const RESET_SECONDS: u64 = 5;
/// Seconds a limit-reached or threshold card stays up (the Mac uses 6.0).
pub const LIMIT_SECONDS: u64 = 6;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Reset,
    SessionLimitReached,
    WeeklyLimitReached,
    /// An agent's turn ended (the Mac's `SessionChime` peek); the words are in the notice.
    Finished,
    /// A drive crossed the warning level of used space.
    DriveFilling,
    /// A drive crossed the critical level of used space.
    DriveFull,
    /// A drive's SMART reading is a warning.
    DriveHealth,
}

/// Words that replace the kind's own (a threshold crossing; the Mac's `notice*` fields).
#[derive(Clone, Debug, PartialEq)]
pub struct Notice {
    pub title: String,
    pub subtitle: String,
    pub status: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Alert {
    pub kind: Kind,
    /// Display name, "Claude" or "Codex".
    pub provider: String,
    /// The window's own label, "Current session" or "Weekly".
    pub window: String,
    /// Unix seconds the window next resets.
    pub resets_at: Option<u64>,
    pub notice: Option<Notice>,
}

impl Alert {
    pub fn seconds(&self) -> u64 {
        match self.kind {
            Kind::Reset | Kind::Finished => RESET_SECONDS,
            Kind::SessionLimitReached
            | Kind::WeeklyLimitReached
            | Kind::DriveFilling
            | Kind::DriveFull
            | Kind::DriveHealth => LIMIT_SECONDS,
        }
    }
}

/// Which alerts are on: the notification settings (`PillSettings`) in the form the watchers
/// read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Prefs {
    pub reset: bool,
    pub session_limit: bool,
    pub weekly_limit: bool,
    /// Claude, Codex: alerts muted for that provider.
    pub muted: [bool; 2],
    /// The Mac's "Mac notifications" channel: a system toast and the sound, no notch card.
    /// There is no channel setting on Windows yet, so `from_settings` leaves it off.
    pub toast: bool,
}

impl Prefs {
    pub fn from_settings(settings: &PillSettings) -> Self {
        Self {
            reset: settings.announce_usage_reset,
            session_limit: settings.announce_session_limit,
            weekly_limit: settings.announce_weekly_limit,
            muted: [settings.mute_claude_alerts, settings.mute_codex_alerts],
            toast: false,
        }
    }
}

// ---- the watchers -----------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
struct ResetState {
    fraction: f32,
    resets_at: Option<u64>,
    peak: f32,
    last_alerted: Option<u64>,
}

#[derive(Clone, Copy, Debug, Default)]
struct Tracked {
    /// False until this window has been read once: the first reading only records.
    seeded: bool,
    exhausted: bool,
    resets_at: Option<u64>,
}

#[derive(Clone, Copy, Debug, Default)]
struct LimitState {
    session: Tracked,
    weekly: Tracked,
}

#[derive(Default)]
struct Watchers {
    reset: [Option<ResetState>; 2],
    limits: [Option<LimitState>; 2],
    /// Highest threshold (80 or 100) each provider's headline has crossed.
    crossed: [Option<u8>; 2],
}

fn provider_name(index: usize) -> &'static str {
    crate::usage::Provider::ALL[index].name()
}

/// A later reset timestamp alone is not a new window (a relative countdown moves it by
/// seconds on every refresh): the tracked window must have elapsed.
fn rolled_over(previous: Option<u64>, current: Option<u64>, now: u64) -> bool {
    match (previous, current) {
        (Some(p), Some(c)) => p <= now && c > p,
        _ => false,
    }
}

impl Watchers {
    fn observe(&mut self, usage: &[Usage; 2], now: u64, prefs: Prefs) -> Vec<Alert> {
        let mut out = Vec::new();
        for (index, reading) in usage.iter().enumerate() {
            if reading.is_stale() {
                // An archived reading is not a baseline.
                self.reset[index] = None;
                self.limits[index] = None;
                self.crossed[index] = None;
                continue;
            }
            let muted = prefs.muted[index];
            self.threshold(index, reading, muted, &mut out);
            if let Some(alert) = self.reset(index, reading, now, muted)
                && prefs.reset
            {
                out.push(alert);
            }
            self.limit(index, reading, now, muted, prefs, &mut out);
        }
        out
    }

    fn threshold(&mut self, index: usize, reading: &Usage, muted: bool, out: &mut Vec<Alert>) {
        let Some(headline) = reading.headline() else {
            return;
        };
        let percent = headline.fraction * 100.0;
        let level: u8 = if percent >= 100.0 {
            100
        } else if percent >= 80.0 {
            80
        } else {
            0
        };
        let Some(previous) = self.crossed[index] else {
            self.crossed[index] = Some(level);
            return;
        };
        self.crossed[index] = Some(level);
        if level <= previous || muted {
            return;
        }
        let used = percent.round() as u32;
        let name = provider_name(index);
        for threshold in [80u8, 100] {
            if threshold > previous && threshold <= level {
                out.push(Alert {
                    kind: Kind::Reset,
                    provider: name.to_string(),
                    window: headline.label.clone(),
                    resets_at: headline.resets_at,
                    notice: Some(Notice {
                        title: if threshold >= 100 {
                            format!("{name} limit reached")
                        } else {
                            format!("{name} is at {used}%")
                        },
                        subtitle: headline.label.clone(),
                        status: format!("{used}% used"),
                    }),
                });
            }
        }
    }

    fn reset(&mut self, index: usize, reading: &Usage, now: u64, muted: bool) -> Option<Alert> {
        let headline = reading.headline()?;
        let fraction = headline.fraction;
        let Some(mut previous) = self.reset[index] else {
            self.reset[index] = Some(ResetState {
                fraction,
                resets_at: headline.resets_at,
                peak: fraction,
                last_alerted: headline.resets_at,
            });
            return None;
        };
        let elapsed = previous.resets_at.is_some_and(|r| r <= now);
        let date_rolled = elapsed
            && match (headline.resets_at, previous.resets_at) {
                (Some(current), Some(before)) => {
                    current > before && previous.last_alerted.is_none_or(|last| current > last)
                }
                _ => false,
            };
        let dropped = fraction < previous.fraction
            && (previous.fraction - fraction >= 0.20
                || (previous.peak >= 0.30 && fraction <= 0.10));
        let had_usage = previous.peak >= 0.15;
        // A drop without a reset date, or after the date passed, is a reset too.
        let inferred = dropped && (previous.resets_at.is_none() || elapsed);
        let fire = (date_rolled || inferred) && had_usage && !muted;
        let alert = fire.then(|| Alert {
            kind: Kind::Reset,
            provider: provider_name(index).to_string(),
            window: headline.label.clone(),
            resets_at: headline.resets_at,
            notice: None,
        });
        if fire {
            previous.fraction = fraction;
            previous.peak = fraction;
            previous.last_alerted = headline.resets_at;
        } else {
            previous.fraction = fraction;
            previous.peak = previous.peak.max(fraction);
        }
        previous.resets_at = headline.resets_at;
        self.reset[index] = Some(previous);
        alert
    }

    fn limit(
        &mut self,
        index: usize,
        reading: &Usage,
        now: u64,
        muted: bool,
        prefs: Prefs,
        out: &mut Vec<Alert>,
    ) {
        let mut state = self.limits[index].unwrap_or_default();
        if let Some(window) = reading.headline()
            && step(
                &mut state.session,
                window,
                reading.block.is_some(),
                now,
                muted,
            )
            && prefs.session_limit
        {
            out.push(limit_alert(Kind::SessionLimitReached, index, window));
        }
        if let Some(window) = reading.weekly()
            && step(&mut state.weekly, window, false, now, muted)
            && prefs.weekly_limit
        {
            out.push(limit_alert(Kind::WeeklyLimitReached, index, window));
        }
        self.limits[index] = Some(state);
    }
}

/// One window of the limit watcher; true when it has just run out.
fn step(tracked: &mut Tracked, window: &LimitWindow, blocked: bool, now: u64, muted: bool) -> bool {
    let exhausted = window.fraction >= 1.0 || blocked;
    if rolled_over(tracked.resets_at, window.resets_at, now) || window.fraction < 0.95 {
        tracked.exhausted = false;
    }
    let mut fire = false;
    if !tracked.seeded {
        tracked.seeded = true;
        tracked.exhausted = exhausted;
    } else if exhausted && !tracked.exhausted && !muted {
        tracked.exhausted = true;
        fire = true;
    }
    tracked.resets_at = window.resets_at;
    fire
}

fn limit_alert(kind: Kind, index: usize, window: &LimitWindow) -> Alert {
    Alert {
        kind,
        provider: provider_name(index).to_string(),
        window: window.label.clone(),
        resets_at: window.resets_at,
        notice: None,
    }
}

// ---- the card ---------------------------------------------------------------------------------

/// "3:05 PM" for a Unix time shifted by `utc_offset_secs`.
pub fn clock_text(secs: u64, utc_offset_secs: i64) -> String {
    let local = (secs as i64 + utc_offset_secs).rem_euclid(86_400);
    let (hour, minute) = (local / 3600, local % 3600 / 60);
    let twelve = if hour % 12 == 0 { 12 } else { hour % 12 };
    format!(
        "{twelve}:{minute:02} {}",
        if hour < 12 { "AM" } else { "PM" }
    )
}

/// The machine's offset from UTC, from the difference between its clock and UTC.
pub fn local_offset_secs() -> i64 {
    use windows::Win32::System::SystemInformation::GetLocalTime;
    // SAFETY: GetLocalTime has no preconditions.
    let now = unsafe { GetLocalTime() };
    let local = i64::from(now.wHour) * 3600 + i64::from(now.wMinute) * 60 + i64::from(now.wSecond);
    let utc = (crate::usage::now_secs() % 86_400) as i64;
    let mut delta = local - utc;
    if delta > 12 * 3600 {
        delta -= 86_400;
    } else if delta < -12 * 3600 {
        delta += 86_400;
    }
    // Whole quarter hours: the two clocks are read a moment apart.
    (delta as f64 / 900.0).round() as i64 * 900
}

/// The card's words, as the Mac's `UsageResetCard` gives them: title, subtitle, a status
/// line (the Mac's coloured dot is a bullet here) and the next reset time.
pub fn card_content(alert: &Alert, utc_offset_secs: i64) -> CardContent {
    if let Some(notice) = &alert.notice
        && matches!(
            alert.kind,
            Kind::Finished | Kind::DriveFilling | Kind::DriveFull | Kind::DriveHealth
        )
    {
        return info_card(alert, notice);
    }
    let (provider, window) = (&alert.provider, &alert.window);
    let (title, subtitle, status, prefix) = match alert.kind {
        Kind::Reset => (
            format!("{provider} Reset"),
            format!("{window} limit refreshed"),
            "Quota is available (0% used)",
            "Next reset",
        ),
        Kind::SessionLimitReached => (
            format!("{provider} Limit Reached"),
            format!("{window} limit is spent"),
            "Session limit reached (100% used)",
            "Resets at",
        ),
        Kind::WeeklyLimitReached => (
            format!("{provider} Weekly Limit"),
            format!("{window} limit is spent"),
            "Weekly limit reached (100% used)",
            "Resets at",
        ),
        // These kinds always carry a notice (`info_card`); a bare one reads as a reset.
        Kind::Finished | Kind::DriveFilling | Kind::DriveFull | Kind::DriveHealth => {
            (format!("{provider} Reset"), String::new(), "", "Next reset")
        }
    };
    let (title, subtitle, status) = match &alert.notice {
        Some(notice) => (
            notice.title.clone(),
            notice.subtitle.clone(),
            notice.status.clone(),
        ),
        None => (title, subtitle, status.to_string()),
    };
    let mut rows = Vec::new();
    if !status.is_empty() {
        // The Mac's coloured dot: green once the limit refreshed, red while it is spent.
        let spent = alert.notice.is_none() && alert.kind != Kind::Reset;
        rows.push(Row::Status {
            text: status,
            tone: if spent { Tone::Critical } else { Tone::Good },
        });
    }
    if let Some(resets_at) = alert.resets_at {
        rows.push(Row::Note(format!(
            "{prefix} {}",
            clock_text(resets_at, utc_offset_secs)
        )));
    }
    CardContent {
        // The Mac's bare close at the title's right; a click anywhere on the card puts it
        // away as well.
        head: vec![Head::Dismiss],
        height: Some(ALERT_HEIGHT),
        subtitle: (!subtitle.is_empty()).then_some(subtitle),
        mark: if provider == "Codex" {
            Mark::Codex
        } else {
            Mark::Claude
        },
        ..CardContent::plain(title, rows)
    }
}

/// The card for an agent that finished or a drive that needs attention: the notice's words
/// and one status dot (green finished, amber filling, red full or failing).
fn info_card(alert: &Alert, notice: &Notice) -> CardContent {
    let tone = match alert.kind {
        Kind::DriveFilling => Tone::Warning,
        Kind::DriveFull | Kind::DriveHealth => Tone::Critical,
        _ => Tone::Good,
    };
    let rows = if notice.status.is_empty() {
        Vec::new()
    } else {
        vec![Row::Status {
            text: notice.status.clone(),
            tone,
        }]
    };
    CardContent {
        head: vec![Head::Dismiss],
        height: Some(ALERT_HEIGHT),
        subtitle: (!notice.subtitle.is_empty()).then(|| notice.subtitle.clone()),
        mark: match alert.kind {
            Kind::Finished if alert.provider == "Codex" => Mark::Codex,
            Kind::Finished => Mark::Claude,
            _ => Mark::Disks,
        },
        ..CardContent::plain(notice.title.clone(), rows)
    }
}

/// The Mac's `UsageResetCard.cardHeight` (210 design px).
const ALERT_HEIGHT: f32 = 79.0;

// ---- the live model ---------------------------------------------------------------------------

#[derive(Default)]
struct Model {
    watchers: Watchers,
    drives: DriveWatch,
    /// The card on show and the Unix second it goes.
    current: Option<(Alert, u64)>,
}

static MODEL: Mutex<Option<Model>> = Mutex::new(None);

fn model() -> MutexGuard<'static, Option<Model>> {
    MODEL.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A card raised off the main thread (a finished agent) waits here for the next `observe`
/// to report it, because `main.rs` redraws the notch only when `observe` says so.
static PENDING: AtomicBool = AtomicBool::new(false);
static TOAST_CHANNEL: AtomicBool = AtomicBool::new(false);
static MUTED: [AtomicBool; 2] = [AtomicBool::new(false), AtomicBool::new(false)];
static SESSIONS: Once = Once::new();

/// Feeds a usage publication to the watchers; a card that is due replaces the one on show.
/// True when a card was raised (the caller redraws). Also the place the drive watcher runs and
/// the agent-finished watcher starts, so `main.rs` needs nothing more.
pub fn observe(usage: &[Usage; 2], now: u64, prefs: Prefs) -> bool {
    TOAST_CHANNEL.store(prefs.toast, Ordering::Relaxed);
    for (flag, muted) in MUTED.iter().zip(prefs.muted) {
        flag.store(muted, Ordering::Relaxed);
    }
    SESSIONS.call_once(|| {
        let _ = std::thread::Builder::new()
            .name("pulse-agent-finish".into())
            .spawn(finish_loop);
    });
    let mut guard = model();
    let model = guard.get_or_insert_with(Model::default);
    // Thresholds are produced first and limits last, so the most specific card wins; a
    // drive that needs attention comes after them.
    let mut raised = model.watchers.observe(usage, now, prefs);
    raised.extend(
        model
            .drives
            .observe(&sensors::read_drives(), &crate::drive_health::warnings()),
    );
    let pending = PENDING.swap(false, Ordering::Relaxed);
    let Some(alert) = raised.into_iter().next_back() else {
        return pending;
    };
    crate::diag::info(
        "usage_alert",
        &[
            ("provider", alert.provider.as_str()),
            ("kind", kind_name(alert.kind)),
        ],
    );
    announce(&alert, prefs.toast);
    if prefs.toast {
        // The Mac's notification channel: the banner and the sound, the notch stays quiet.
        return pending;
    }
    model.current = Some((alert.clone(), now + alert.seconds()));
    true
}

/// The sound of an alert and, on the toast channel, its system toast.
fn announce(alert: &Alert, toast: bool) {
    let sound = match alert.kind {
        Kind::Finished => Sound::Finished,
        Kind::Reset | Kind::DriveFilling => Sound::Notice,
        Kind::SessionLimitReached
        | Kind::WeeklyLimitReached
        | Kind::DriveFull
        | Kind::DriveHealth => Sound::Attention,
    };
    notify::play(sound);
    if toast {
        let (title, body) = words(alert);
        notify::toast(&title, &body);
    }
}

/// Title and one line of body for a toast.
fn words(alert: &Alert) -> (String, String) {
    if let Some(notice) = &alert.notice {
        let body = [notice.subtitle.as_str(), notice.status.as_str()]
            .iter()
            .filter(|t| !t.is_empty())
            .copied()
            .collect::<Vec<_>>()
            .join(" \u{b7} ");
        return (notice.title.clone(), body);
    }
    let provider = &alert.provider;
    match alert.kind {
        Kind::SessionLimitReached => (
            format!("{provider} Limit Reached"),
            format!("{} limit is spent", alert.window),
        ),
        Kind::WeeklyLimitReached => (
            format!("{provider} Weekly Limit"),
            format!("{} limit is spent", alert.window),
        ),
        _ => (
            format!("{provider} Reset"),
            format!("{} limit refreshed", alert.window),
        ),
    }
}

// ---- drives -----------------------------------------------------------------------------------

/// Used share where a drive is "filling up" and "almost full": the hub's own bands
/// (`levelFor` in `hub/src/views/Overview.tsx`), so the card and the hub banner agree.
const DRIVE_WARN: f32 = 0.7;
const DRIVE_CRITICAL: f32 = 0.9;

/// Highest level (0 fine, 1 filling, 2 full or failing) each volume and SMART drive has
/// reached. A level that goes up raises a card; it must fall back below to raise it again.
/// The first sight of a drive that is already full or failing alerts once; one that is merely
/// filling only records, so a PC that has run at 75% for a year does not announce it at
/// every start.
#[derive(Default)]
struct DriveWatch {
    levels: BTreeMap<String, u8>,
    seeded: bool,
}

impl DriveWatch {
    fn observe(&mut self, drives: &[sensors::Drive], failing: &[String]) -> Vec<Alert> {
        let mut out = Vec::new();
        let mut seen = Vec::new();
        for drive in drives {
            let fraction = drive.used_fraction();
            let level = if fraction >= DRIVE_CRITICAL {
                2
            } else if fraction >= DRIVE_WARN {
                1
            } else {
                0
            };
            let letter = drive.root.trim_end_matches('\\').to_string();
            let key = format!("volume|{letter}");
            if let Some(kind) = self.rise(&key, level, &mut seen) {
                let free = sensors::drive_size_text(drive.free);
                out.push(drive_alert(
                    kind,
                    if kind == Kind::DriveFull {
                        format!("{letter} is almost full")
                    } else {
                        format!("{letter} is filling up")
                    },
                    format!("{free} free of {}", sensors::drive_size_text(drive.total)),
                    format!("{}% used", (fraction * 100.0).round() as u32),
                ));
            }
        }
        for name in failing {
            let key = format!("smart|{name}");
            if let Some(kind) = self.rise(&key, 2, &mut seen)
                && kind == Kind::DriveFull
            {
                out.push(drive_alert(
                    Kind::DriveHealth,
                    format!("{name} reports a problem"),
                    "The drive's own health check is warning".to_string(),
                    "Back up what matters".to_string(),
                ));
            }
        }
        // A drive that went away or recovered starts again from fine.
        self.levels.retain(|key, _| seen.contains(key));
        self.seeded = true;
        out
    }

    /// The kind to raise when `level` went up (`DriveFilling` or, for level 2, `DriveFull`).
    fn rise(&mut self, key: &str, level: u8, seen: &mut Vec<String>) -> Option<Kind> {
        seen.push(key.to_string());
        let before = self
            .levels
            .get(key)
            .copied()
            .unwrap_or(if self.seeded { 0 } else { 1 });
        self.levels.insert(key.to_string(), level);
        if level <= before {
            return None;
        }
        Some(if level >= 2 {
            Kind::DriveFull
        } else {
            Kind::DriveFilling
        })
    }
}

fn drive_alert(kind: Kind, title: String, subtitle: String, status: String) -> Alert {
    Alert {
        kind,
        provider: String::new(),
        window: String::new(),
        resets_at: None,
        notice: Some(Notice {
            title,
            subtitle,
            status,
        }),
    }
}

// ---- agent finished ---------------------------------------------------------------------------

/// A transcript that has not been written for this long means the agent's turn is over.
const IDLE_SECONDS: u64 = 10;
/// A turn must have run at least this long to count: a stray write is not a finished turn.
const MIN_TURN_SECONDS: u64 = 6;
const POLL: Duration = Duration::from_secs(3);

/// The Mac's `SessionCompletionWatcher`, from the files the agents write. Windows has no
/// session monitor, so a provider is "working" while its newest transcript (Claude Code's
/// `.claude/projects/*/*.jsonl`, Codex's `.codex/sessions/Y/M/D/*.jsonl`) was written in the
/// last ten seconds, and "finished" the moment that stops after a turn of at least six. Every
/// provider that finishes at once gets one chime and one card. There is no blocked signal
/// without a session monitor.
fn finish_loop() {
    let mut busy_since: [Option<u64>; 2] = [None, None];
    let mut last_write: [u64; 2] = [0, 0];
    loop {
        std::thread::sleep(POLL);
        let now = crate::usage::now_secs();
        let mut done = None;
        for index in 0..2 {
            let newest = newest_transcript(index);
            if let Some(at) = newest {
                last_write[index] = last_write[index].max(at);
            }
            let working = newest.is_some_and(|at| now.saturating_sub(at) < IDLE_SECONDS);
            match (busy_since[index], working) {
                (None, true) => busy_since[index] = Some(now),
                (Some(since), false) => {
                    busy_since[index] = None;
                    if last_write[index].saturating_sub(since) >= MIN_TURN_SECONDS {
                        done = Some(index);
                    }
                }
                _ => {}
            }
        }
        if let Some(index) = done
            && !MUTED[index].load(Ordering::Relaxed)
        {
            finished(index, now);
        }
    }
}

fn finished(index: usize, now: u64) {
    let name = provider_name(index);
    crate::diag::info("agent_finished", &[("provider", name)]);
    let alert = Alert {
        kind: Kind::Finished,
        provider: name.to_string(),
        window: String::new(),
        resets_at: None,
        notice: Some(Notice {
            title: format!("{name} finished"),
            subtitle: "The agent's turn is done.".to_string(),
            status: String::new(),
        }),
    };
    let toast = TOAST_CHANNEL.load(Ordering::Relaxed);
    announce(&alert, toast);
    if !toast {
        let seconds = alert.seconds();
        model().get_or_insert_with(Model::default).current = Some((alert, now + seconds));
        PENDING.store(true, Ordering::Relaxed);
        // Redraw now: the controller runs `observe`, which reports the pending card.
        crate::usage::notify();
    }
}

fn unix_seconds(time: SystemTime) -> Option<u64> {
    time.duration_since(UNIX_EPOCH).ok().map(|d| d.as_secs())
}

/// The newest `.jsonl` write under the provider's transcript folder. Directory entries carry
/// their times, so this costs no extra stat per file.
fn newest_transcript(index: usize) -> Option<u64> {
    let home = std::env::var_os("USERPROFILE").map(PathBuf::from)?;
    if index == 0 {
        let mut newest = None;
        for project in std::fs::read_dir(home.join(".claude").join("projects")).ok()? {
            let Ok(project) = project else { continue };
            newest = newest.max(newest_jsonl(&project.path()));
        }
        newest
    } else {
        newest_codex(&home.join(".codex").join("sessions"), 3)
    }
}

fn newest_jsonl(dir: &Path) -> Option<u64> {
    let mut newest = None;
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        if entry.path().extension().is_some_and(|e| e == "jsonl")
            && let Some(at) = entry
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(unix_seconds)
        {
            newest = newest.max(Some(at));
        }
    }
    newest
}

/// Codex keeps `sessions/YYYY/MM/DD/rollout-*.jsonl`: only the two newest names at each of
/// the three folder levels are read.
fn newest_codex(dir: &Path, levels: u32) -> Option<u64> {
    if levels == 0 {
        return newest_jsonl(dir);
    }
    let mut names: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| e.path())
        .collect();
    names.sort();
    names
        .iter()
        .rev()
        .take(2)
        .map(|p| newest_codex(p, levels - 1))
        .max()
        .flatten()
}

fn kind_name(kind: Kind) -> &'static str {
    match kind {
        Kind::Reset => "reset",
        Kind::SessionLimitReached => "session_limit",
        Kind::WeeklyLimitReached => "weekly_limit",
        Kind::Finished => "agent_finished",
        Kind::DriveFilling => "drive_filling",
        Kind::DriveFull => "drive_full",
        Kind::DriveHealth => "drive_health",
    }
}

/// The card to show at `now`, or `None` once it has run its seconds.
pub fn panel(now: u64) -> Option<Panel> {
    let mut guard = model();
    let model = guard.as_mut()?;
    match &model.current {
        Some((_, until)) if now >= *until => {
            model.current = None;
            None
        }
        Some((alert, _)) => Some(Panel::new(card_content(alert, local_offset_secs()))),
        None => None,
    }
}

/// Puts the card away (a click on it).
pub fn dismiss() {
    if let Some(model) = model().as_mut() {
        model.current = None;
    }
}
