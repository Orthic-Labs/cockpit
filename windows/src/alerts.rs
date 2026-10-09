//! Usage alert cards: a limit window that reset, a session or weekly limit that was spent, and
//! the 80% / 100% crossings. A port of the Mac's `UsageResetWatcher`, `UsageLimitWatcher` and
//! `ThresholdNotifier` (`mac/Notch/Sources/Model/`) over `usage.rs` readings, drawn by the
//! notch's card window like the Mac's `UsageResetCard` and put away after five or six seconds
//! (or by a click). Pure state: `main.rs` feeds readings in and asks for the card to show.
//!
//! The watchers are difference engines, so they see every reading whether or not an alert is
//! switched on. An archived (stale) reading is never a baseline and the first live reading of
//! a window only records, so nothing rings at start-up.

use crate::card::{CardContent, Mark, Row};
use crate::send::{Action, Panel};
use crate::settings::PillSettings;
use crate::usage::{LimitWindow, Usage};
use std::sync::{Mutex, MutexGuard, PoisonError};

/// Seconds a reset card stays up (the Mac's `showResetAlert(duration: 5.0)`).
pub const RESET_SECONDS: u64 = 5;
/// Seconds a limit-reached or threshold card stays up (the Mac uses 6.0).
pub const LIMIT_SECONDS: u64 = 6;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Reset,
    SessionLimitReached,
    WeeklyLimitReached,
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
            Kind::Reset => RESET_SECONDS,
            Kind::SessionLimitReached | Kind::WeeklyLimitReached => LIMIT_SECONDS,
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
}

impl Prefs {
    pub fn from_settings(settings: &PillSettings) -> Self {
        Self {
            reset: settings.announce_usage_reset,
            session_limit: settings.announce_session_limit,
            weekly_limit: settings.announce_weekly_limit,
            muted: [settings.mute_claude_alerts, settings.mute_codex_alerts],
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
        // A spent limit reads red with the critical dot, as the Mac's red status line does;
        // the rest keep the bullet (the shared renderer has no green status line).
        let spent = alert.notice.is_none() && alert.kind != Kind::Reset;
        rows.push(if spent {
            Row::Alert(status)
        } else {
            Row::Text(format!("\u{25CF} {status}"))
        });
    }
    if let Some(resets_at) = alert.resets_at {
        rows.push(Row::Note(format!(
            "{prefix} {}",
            clock_text(resets_at, utc_offset_secs)
        )));
    }
    CardContent {
        // The Mac's close button; a click anywhere on the card puts it away.
        accessory: Some("\u{d7}".to_string()),
        subtitle: (!subtitle.is_empty()).then_some(subtitle),
        mark: match (alert.notice.is_some(), provider.as_str()) {
            (true, _) => Mark::None,
            (false, "Codex") => Mark::Codex,
            (false, _) => Mark::Claude,
        },
        ..CardContent::plain(title, rows)
    }
}

// ---- the live model ---------------------------------------------------------------------------

#[derive(Default)]
struct Model {
    watchers: Watchers,
    /// The card on show and the Unix second it goes.
    current: Option<(Alert, u64)>,
}

static MODEL: Mutex<Option<Model>> = Mutex::new(None);

fn model() -> MutexGuard<'static, Option<Model>> {
    MODEL.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Feeds a usage publication to the watchers; a card that is due replaces the one on show.
/// True when a card was raised (the caller redraws).
pub fn observe(usage: &[Usage; 2], now: u64, prefs: Prefs) -> bool {
    let mut guard = model();
    let model = guard.get_or_insert_with(Model::default);
    // Thresholds are produced first and limits last, so the most specific card wins.
    let raised = model.watchers.observe(usage, now, prefs);
    let Some(alert) = raised.into_iter().next_back() else {
        return false;
    };
    crate::diag::info(
        "usage_alert",
        &[
            ("provider", alert.provider.as_str()),
            ("kind", kind_name(alert.kind)),
        ],
    );
    model.current = Some((alert.clone(), now + alert.seconds()));
    true
}

fn kind_name(kind: Kind) -> &'static str {
    match kind {
        Kind::Reset => "reset",
        Kind::SessionLimitReached => "session_limit",
        Kind::WeeklyLimitReached => "weekly_limit",
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
        Some((alert, _)) => {
            let content = card_content(alert, local_offset_secs());
            let actions = vec![None::<Action>; content.rows.len()];
            Some(Panel { content, actions })
        }
        None => None,
    }
}

/// Puts the card away (a click on it).
pub fn dismiss() {
    if let Some(model) = model().as_mut() {
        model.current = None;
    }
}
