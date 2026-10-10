//! Hover card content: the key numbers behind each ring (same information as the Mac
//! hover cards). Pure data; `render.rs` draws it. Unknown values read `--`.

use crate::alerts;
use crate::drive_health::{self, Report};
use crate::glyphs::{Symbol, Tile};
use crate::layout::{Cell, Edge};
use crate::send::{self, Panel};
use crate::sensors::{Machine, Reading, drive_size_text, size_text};
use crate::usage::{Extras, Status, Usage};

/// The icon beside a card's title (the Mac's provider glyph); drawn by `render.rs`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mark {
    #[default]
    None,
    Claude,
    Codex,
    System,
    Disks,
    Send,
}

/// The speech-bubble tail: the notch's `edge` (the tail leaves the card on the side facing
/// it) and how far, in device pixels along the card's own axis, it sits from the card's
/// middle so its point stays on the hovered ring.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tail {
    pub edge: Edge,
    pub offset: i32,
}

/// Colour of a tinted line or a status dot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    /// Amber: a caveat.
    Warning,
    /// Green: all well.
    Good,
    /// Red: stopped.
    Critical,
}

/// The large icon a card leads with, beside its title block.
#[derive(Clone, Debug, PartialEq)]
pub enum Lead {
    /// A colour icon the renderer draws itself.
    Tile(Tile),
    /// The shell's icon for this file when it has one (live cards), else the package tile.
    File(String),
    /// A symbol on a dim disc (a transfer card); `problem` draws it in the warning colour.
    Disc { symbol: Symbol, problem: bool },
}

/// A round button in the header's top right.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Head {
    /// Look again.
    Refresh,
    /// A scan is running: a spinner where Refresh would be.
    Scanning,
    /// The round close.
    Close,
    /// A bare small cross with no disc (the alert card's dismiss).
    Dismiss,
    /// A check: the Claude restart finished.
    Done,
    /// A red mark: the Claude restart failed (the reason is a line on the card).
    Failed,
}

/// One pill of a button row.
#[derive(Clone, Debug, PartialEq)]
pub struct Button {
    pub label: String,
    pub symbol: Option<Symbol>,
}

impl Button {
    pub fn new(label: impl Into<String>, symbol: Symbol) -> Self {
        Self {
            label: label.into(),
            symbol: Some(symbol),
        }
    }
}

/// What the pointer is over on a card: a header button, or a row's button (index 0 for a
/// row that is one button itself, such as a device).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    Head(usize),
    Row(usize, usize),
}

/// Which of a card's buttons are live (have an action): per header button, and per row per
/// button. A dead one reads dimmed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Live {
    pub head: Vec<bool>,
    pub rows: Vec<Vec<bool>>,
}

impl Live {
    pub fn row(&self, row: usize, button: usize) -> bool {
        self.rows
            .get(row)
            .and_then(|buttons| buttons.get(button))
            .copied()
            .unwrap_or(false)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Row {
    /// Label on the left, quieter value on the right (a device on the Send card is one such
    /// row with an action).
    Pair { label: String, value: String },
    /// A full-width button plate, the Mac's "Copy last" and "Paste clipboard" rows: an optional
    /// symbol, the label, and a quieter detail at the right. Without a detail the symbol and
    /// label sit in the plate's middle. Whatever does not fit is cut with an ellipsis.
    Button {
        symbol: Option<Symbol>,
        label: String,
        detail: String,
    },
    /// The Send card's bottom bar (the Mac's `action:bar`): "Copy last: <preview>" as a plate
    /// at the left when there is a label, "Paste" as a plate at the right when `paste`. Two
    /// buttons in one row: slot 0 is Copy last (with its age as `copy_detail`), slot 1 is Paste.
    Bar {
        copy: Option<String>,
        copy_detail: String,
        paste: bool,
    },
    /// One metered window: label and trailing text on a line, a bar (none without a share),
    /// then the summary line.
    Meter {
        label: String,
        trailing: String,
        fraction: Option<f32>,
        summary: String,
    },
    /// A model's or feature's own limits (the Mac's grouped box): its title in bold over a
    /// rounded inset box holding `Meter` rows.
    Group { title: String, rows: Vec<Row> },
    /// Secondary-ink paragraph.
    Note(String),
    /// Primary-ink paragraph.
    Text(String),
    /// Critical-ink line that says the account is stopped (a spent limit).
    Alert(String),
    /// A line in a tone (the amber caveat under a disk image's detail).
    Tinted { text: String, tone: Tone },
    /// A filled dot and a line in its tone (an alert's status).
    Status { text: String, tone: Tone },
    /// Side-by-side pills, then an optional round close.
    Buttons { buttons: Vec<Button>, close: bool },
    /// A bare progress bar; `None` is indeterminate.
    Progress(Option<f32>),
    /// A nearby device: its symbol, its name in bold, its model beneath and a send arrow.
    Device {
        symbol: Symbol,
        alias: String,
        model: String,
    },
    /// A spinner with a line beside it.
    Waiting(String),
}

impl Row {
    /// How many things on the row can be pressed: its buttons, or the row itself.
    pub fn slots(&self) -> usize {
        match self {
            Row::Buttons { buttons, close } => buttons.len() + usize::from(*close),
            Row::Bar { .. } => 2,
            _ => 1,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct CardContent {
    pub title: String,
    /// The large icon beside the title block: makes the card a banner (icon, then title,
    /// detail and the buttons at the bottom), like the Mac's disk image and update cards.
    pub lead: Option<Lead>,
    /// Round buttons at the header's top right, left to right.
    pub head: Vec<Head>,
    /// The wide card of the disk image and update cards.
    pub wide: bool,
    /// The detail line reads in the warning colour.
    pub problem: bool,
    /// The Mac's fixed card height, in DIPs, when it has one.
    pub height: Option<f32>,
    /// Quiet text on the title's line, at the right.
    pub accessory: Option<String>,
    /// Quiet line under the title (the account's plan).
    pub subtitle: Option<String>,
    pub mark: Mark,
    pub tail: Option<Tail>,
    /// The update card's heavier progress bar (the Mac draws it 16 design px tall).
    pub heavy_bar: bool,
    pub rows: Vec<Row>,
}

impl CardContent {
    /// A card with a title and rows and nothing else.
    pub fn plain(title: impl Into<String>, rows: Vec<Row>) -> Self {
        Self {
            title: title.into(),
            rows,
            ..Self::default()
        }
    }
}

fn percent_text(fraction: Option<f32>) -> String {
    fraction.map_or_else(
        || "--".to_string(),
        |f| format!("{}%", (f.clamp(0.0, 1.0) * 100.0).round() as u32),
    )
}

/// A used share and its remainder, from one rounding so they always add up.
fn halves(fraction: f32) -> (u32, u32) {
    let used = (fraction.clamp(0.0, 1.0) * 100.0).round() as u32;
    (used, 100 - used)
}

fn meter(label: &str, fraction: Option<f32>, summary: String) -> Row {
    Row::Meter {
        label: label.to_string(),
        trailing: String::new(),
        fraction,
        summary,
    }
}

/// "just now", "5 min", "2 hr 5 min": how long a session has been in its state.
pub fn elapsed_text(seconds: u64) -> String {
    if seconds < 45 {
        return "just now".into();
    }
    let minutes = ((seconds as f64 / 60.0).round() as u64).max(1);
    if minutes < 60 {
        return format!("{minutes} min");
    }
    let (hours, rest) = (minutes / 60, minutes % 60);
    if rest == 0 {
        format!("{hours} hr")
    } else {
        format!("{hours} hr {rest} min")
    }
}

fn ago_text(seconds: u64) -> String {
    let elapsed = elapsed_text(seconds);
    if elapsed == "just now" {
        elapsed
    } else {
        format!("{elapsed} ago")
    }
}

/// Year-less calendar date of a day count since 1970-01-01 as (month 1..=12, day).
fn civil(days: i64) -> (usize, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    (month as usize, day)
}

/// The Mac's reset copy: seconds in the last minute, minutes under an hour, then the local
/// weekday and time within the week and the date beyond it.
pub fn reset_text(resets_at: u64, now: u64, utc_offset_secs: i64) -> String {
    const WEEKDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    if resets_at <= now {
        return "Resetting\u{2026}".into();
    }
    let seconds = resets_at - now;
    if seconds < 60 {
        return format!("Resets in {} sec", seconds.max(1));
    }
    let minutes = (seconds as f64 / 60.0).round() as u64;
    if minutes < 60 {
        return format!("Resets in {} min", minutes.max(1));
    }
    let day_of = |secs: u64| (secs as i64 + utc_offset_secs).div_euclid(86_400);
    let (reset_day, today) = (day_of(resets_at), day_of(now));
    if reset_day - today >= 7 {
        let (month, day) = civil(reset_day);
        return format!("Resets {} {day}", MONTHS[month - 1]);
    }
    // 1970-01-01 was a Thursday.
    let weekday = WEEKDAYS[(reset_day + 4).rem_euclid(7) as usize];
    format!(
        "Resets {weekday} {}",
        alerts::clock_text(resets_at, utc_offset_secs)
    )
}

/// The card for `cell` and whether it is a nearby-sharing popup (news, shown without hover).
/// Every row of a non-Send card is plain text; the Send cell shows `popup` when there is one,
/// else its hover panel, whose rows with an action are clickable.
pub fn panel_for(
    cell: Cell,
    machine: Option<&Machine>,
    usage: &[Usage; 2],
    now: u64,
    popup: Option<Panel>,
) -> (Panel, bool) {
    if cell == Cell::Send {
        return match popup {
            Some(panel) => (panel, true),
            None => (send::hover_panel(), false),
        };
    }
    let content = content_for(cell, machine, usage, now);
    let mut panel = Panel::new(content);
    if cell == Cell::Claude {
        claude_restart_button(&mut panel);
    }
    (panel, false)
}

/// The Claude card's small round "Restart Claude and sync chats" button, right of its header:
/// a refresh arrow, a spinner while it runs, a check for about two seconds, a red mark (with
/// the reason as a line on the card) when it failed. Clicking it is handled by the notch.
fn claude_restart_button(panel: &mut Panel) {
    use crate::claude_restart::{self, Phase};
    let press = Some(send::Action::Refresh);
    match claude_restart::phase() {
        Phase::Idle => panel.heads(vec![(Head::Refresh, press)]),
        Phase::Running => panel.heads(vec![(Head::Scanning, None)]),
        Phase::Done => panel.heads(vec![(Head::Done, press)]),
        Phase::Failed(reason) => {
            panel.heads(vec![(Head::Failed, press)]);
            panel.row(
                Row::Tinted {
                    text: reason,
                    tone: Tone::Critical,
                },
                None,
            );
        }
    }
}

fn content_for(cell: Cell, machine: Option<&Machine>, usage: &[Usage; 2], now: u64) -> CardContent {
    match cell {
        // One System card whichever of its two rings is hovered (the Mac has one cell).
        Cell::Cpu | Cell::Memory => system(machine),
        Cell::Disk => disks(machine, &drive_health::current()),
        Cell::Claude => provider("Claude", &usage[0], now),
        Cell::Codex => provider("Codex", &usage[1], now),
        Cell::Send => send::hover_panel().content,
    }
}

/// The System card, row for row as the Mac's: CPU and memory pressure as bars with a detail
/// line, then GPU, network and fans as one-line pairs, and the temperature at the title's
/// right. A reading this PC cannot give is left out, as on the Mac; one not sampled yet says
/// "Measuring\u{2026}".
pub fn system(machine: Option<&Machine>) -> CardContent {
    let mut rows = Vec::new();
    let mut accessory = None;
    match machine {
        Some(m) => {
            let busy = if m.cpu.is_some() {
                format!("{} busy", percent_text(m.cpu))
            } else {
                "Usage unavailable".to_string()
            };
            rows.push(meter(
                "CPU",
                m.cpu,
                if m.cores > 0 {
                    format!("{busy} \u{b7} {} cores", m.cores)
                } else {
                    busy
                },
            ));
            match m.memory {
                Some(mem) => rows.push(meter(
                    "Memory pressure",
                    Some(mem.used_fraction()),
                    format!(
                        "Pressure {} \u{b7} {} of {} used",
                        pressure_word(m),
                        size_text(mem.used()),
                        size_text(mem.total)
                    ),
                )),
                None => rows.push(meter(
                    "Memory pressure",
                    None,
                    "Memory readings unavailable".into(),
                )),
            }
            // The Mac leaves out a sensor it cannot read, and so does this card; only a
            // reading still being taken says so.
            rows.extend(pair("GPU", &m.gpu, |share| {
                format!("{} busy", percent_text(Some(*share)))
            }));
            rows.extend(pair("Network", &m.network, |rate| {
                format!(
                    "\u{2193} {} \u{b7} \u{2191} {} \u{b7} {}",
                    rate_text(rate.down),
                    rate_text(rate.up),
                    rate.kind
                )
            }));
            // Battery, CPU temperature and fans stay on the card whatever this PC can read:
            // a reading it cannot give says why instead of leaving the row out.
            rows.push(Row::Pair {
                label: "Battery".to_string(),
                value: match crate::sensors::battery() {
                    Reading::Value(battery) => battery.text(),
                    Reading::Pending => "Measuring\u{2026}".to_string(),
                    Reading::Unavailable => "No battery".to_string(),
                },
            });
            let cpu_temp = match &m.temperature {
                Reading::Value(temps) => temps.iter().find(|t| t.source == "CPU").copied(),
                _ => None,
            };
            rows.push(Row::Pair {
                label: "CPU temperature".to_string(),
                value: match cpu_temp {
                    Some(t) => format!("{} \u{b0}C", t.celsius.round()),
                    None => format!("\u{2014} \u{b7} {}", crate::sensors::CPU_TEMPERATURE_REASON),
                },
            });
            rows.push(Row::Pair {
                label: "Fans".to_string(),
                value: match &m.fans {
                    Reading::Value(fans) if !fans.is_empty() => {
                        let speeds: Vec<String> = fans.iter().map(u32::to_string).collect();
                        format!("{} rpm", speeds.join(" / "))
                    }
                    Reading::Pending => "Measuring\u{2026}".to_string(),
                    _ => crate::sensors::FANS_UNAVAILABLE.to_string(),
                },
            });
            // The Mac shows the CPU temperature at the title's right; here it is whatever
            // Windows lets an unelevated process read, each named by its source.
            if let Reading::Value(temps) = &m.temperature {
                let text: Vec<String> = temps
                    .iter()
                    .map(|t| format!("{} {} \u{b0}C", t.source, t.celsius.round()))
                    .collect();
                if !text.is_empty() {
                    accessory = Some(text.join(" \u{b7} "));
                }
            }
        }
        None => rows.push(Row::Note("Waiting for the first sample".into())),
    }
    CardContent {
        mark: Mark::System,
        accessory,
        ..CardContent::plain("System Usage", rows)
    }
}

/// "normal", "warning" or "critical" (the Mac's words) from the memory bands the ring colours use.
fn pressure_word(machine: &Machine) -> &'static str {
    machine
        .memory
        .and_then(|m| m.pressure())
        .unwrap_or("unknown")
}

/// A label with its value on the right; nothing for a reading this PC cannot give.
fn pair<T>(label: &str, reading: &Reading<T>, text: impl Fn(&T) -> String) -> Option<Row> {
    let value = match reading {
        Reading::Value(value) => text(value),
        Reading::Pending => "Measuring\u{2026}".to_string(),
        Reading::Unavailable => return None,
    };
    Some(Row::Pair {
        label: label.to_string(),
        value,
    })
}

/// A transfer rate the way the Mac words it: "1.2 MB/s", "180 KB/s".
fn rate_text(bytes_per_second: f64) -> String {
    let value = bytes_per_second.max(0.0);
    if value >= 1e9 {
        format!("{:.1} GB/s", value / 1e9)
    } else if value >= 1e6 {
        format!("{:.1} MB/s", value / 1e6)
    } else if value >= 1e3 {
        format!("{:.0} KB/s", value / 1e3)
    } else {
        format!("{value:.0} B/s")
    }
}

/// The Disks card: each volume's free space, then the drive-health lines.
pub fn disks(machine: Option<&Machine>, health: &Report) -> CardContent {
    let mut rows = Vec::new();
    match machine {
        Some(m) if !m.drives.is_empty() => {
            // System drive first, as the ring shows it.
            let mut ordered: Vec<_> = m.drives.iter().collect();
            ordered.sort_by_key(|d| !d.system);
            for drive in ordered {
                let name = drive.root.trim_end_matches('\\');
                let label = if drive.system {
                    format!("{name} (system)")
                } else {
                    name.to_string()
                };
                rows.push(meter(
                    &label,
                    Some(drive.used_fraction()),
                    format!(
                        "{} free of {}",
                        drive_size_text(drive.free),
                        drive_size_text(drive.total)
                    ),
                ));
            }
        }
        Some(_) => rows.push(Row::Note("No drive readings".into())),
        None => rows.push(Row::Note("Waiting for the first sample".into())),
    }
    match health {
        Report::Pending => {}
        Report::Missing => rows.push(Row::Pair {
            label: "Drive health".into(),
            value: "Install smartmontools for drive health".into(),
        }),
        Report::Drives(drives) => {
            rows.extend(drive_health::lines(drives).into_iter().map(Row::Text));
        }
    }
    CardContent {
        mark: Mark::Disks,
        ..CardContent::plain("Disks Usage", rows)
    }
}

/// What a provider card says when it has no windows to show, and what fixes it.
fn status_message(name: &str, status: Status, account: Option<&str>) -> String {
    let tool = if name == "Claude" {
        "Claude Code"
    } else {
        name
    };
    match status {
        Status::Waiting => "Waiting for the first reading".to_string(),
        Status::Ok => "No usage readings yet".to_string(),
        Status::SignIn => format!("Sign in to {tool} to read your usage"),
        Status::Expired => {
            format!("Your {tool} sign-in expired \u{2014} open {tool} once to refresh it")
        }
        Status::RateLimited => {
            "Couldn't read usage \u{2014} rate limited, retrying later".to_string()
        }
        Status::Unavailable => {
            "Couldn't read usage \u{2014} the service is unavailable".to_string()
        }
        // Claude Desktop is signed into an account with no recent cached reading; no other
        // account's numbers stand in for it.
        Status::NoReading => match account {
            Some(account) => format!("No reading for {account} yet"),
            None => "No reading yet".to_string(),
        },
        // Says what happened and what fixes it, not "sign in" (the login is there).
        Status::AccessDenied => format!(
            "Windows refused Pulse access to {name}'s saved login. Fix the file's permissions to read usage."
        ),
    }
}

/// "Oct 9, 2026" for a Unix time in the local zone.
fn date_text(secs: u64, utc_offset_secs: i64) -> String {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let days = (secs as i64 + utc_offset_secs).div_euclid(86_400);
    let (year, month, day) = crate::usage::civil_from_days(days);
    format!("{} {day}, {year}", MONTHS[month - 1])
}

/// The Codex credit balance and plan-renewal rows; each is left out when there is no data.
fn codex_detail_rows(extras: &Extras, utc_offset_secs: i64) -> Vec<Row> {
    let mut rows = Vec::new();
    if let Some(credits) = &extras.credits {
        rows.push(Row::Pair {
            label: "Available credits".to_string(),
            value: credits.clone(),
        });
    }
    if let Some(until) = extras.plan_until {
        rows.push(Row::Pair {
            label: "Plan active until".to_string(),
            value: date_text(until, utc_offset_secs),
        });
    }
    rows
}

/// The Mac's "Unused resets" section: shown only while some are available.
fn reset_rows(extras: &Extras, now: u64, utc_offset_secs: i64) -> Vec<Row> {
    let Some(resets) = extras.resets.filter(|resets| resets.available > 0) else {
        return Vec::new();
    };
    let count = match resets.available {
        1 => "1 unused reset".to_string(),
        many => format!("{many} unused resets"),
    };
    let mut rows = vec![Row::Pair {
        label: "Unused resets".to_string(),
        value: count,
    }];
    if let Some(expiry) = resets.next_expiry.filter(|expiry| *expiry > now) {
        rows.push(Row::Pair {
            label: if resets.available > 1 {
                "Next expires".to_string()
            } else {
                "Expires".to_string()
            },
            value: date_text(expiry, utc_offset_secs),
        });
    }
    rows
}

fn provider(name: &str, usage: &Usage, now: u64) -> CardContent {
    let offset = alerts::local_offset_secs();
    let mut rows = Vec::new();
    if let Some(block) = &usage.block {
        rows.push(Row::Alert(match block.resets_at {
            Some(reset) if reset > now => format!(
                "{} until {}",
                block.reason,
                alerts::clock_text(reset, offset)
            ),
            _ => block.reason.clone(),
        }));
    }
    if usage.windows.is_empty() {
        rows.push(Row::Note(status_message(
            name,
            usage.status,
            usage.account.as_deref(),
        )));
    }
    let mut details_shown = false;
    for window in &usage.windows {
        // The Mac's order: the account's own windows, then its credit and plan rows, then
        // each model's or feature's box.
        if window.group.is_some() && !details_shown {
            rows.extend(codex_detail_rows(&usage.extras, offset));
            details_shown = true;
        }
        let (used, left) = halves(window.fraction);
        let meter = Row::Meter {
            label: window.label.clone(),
            trailing: window
                .resets_at
                .map(|reset| reset_text(reset, now, offset))
                .unwrap_or_default(),
            fraction: Some(window.fraction),
            summary: format!("{used}% Used \u{b7} {left}% left"),
        };
        // Consecutive windows of one group share a box, as on the Mac.
        let shares_box = matches!(
            rows.last(),
            Some(Row::Group { title, .. }) if Some(title) == window.group.as_ref()
        );
        if let (true, Some(Row::Group { rows: inner, .. })) = (shares_box, rows.last_mut()) {
            inner.push(meter);
        } else if let Some(group) = &window.group {
            rows.push(Row::Group {
                title: group.clone(),
                rows: vec![meter],
            });
        } else {
            rows.push(meter);
        }
    }
    if !details_shown {
        rows.extend(codex_detail_rows(&usage.extras, offset));
    }
    rows.extend(reset_rows(&usage.extras, now, offset));
    // Only worth saying when the numbers are not current: a remembered reading has to be
    // dated, or it quietly passes itself off as live.
    let reading_age = if usage.status == Status::Ok || usage.windows.is_empty() {
        None
    } else {
        usage
            .updated
            .map(|updated| ago_text(now.saturating_sub(updated)))
    };
    CardContent {
        accessory: reading_age,
        subtitle: usage.plan.clone(),
        mark: if name == "Codex" {
            Mark::Codex
        } else {
            Mark::Claude
        },
        ..CardContent::plain(format!("{name} Usage"), rows)
    }
}
