//! Hover card content: the key numbers behind each ring (same information as the Mac
//! hover cards). Pure data; `render.rs` draws it. Unknown values read `--`.

use crate::alerts;
use crate::drive_health::{self, Report};
use crate::glyphs::{Symbol, Tile};
use crate::layout::{Cell, Edge};
use crate::send::{self, Panel};
use crate::sensors::{Machine, size_text};
use crate::usage::{Status, Usage};

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

/// What a session is doing: the ring and colour beside its status word.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dot {
    Busy,
    Waiting,
    Success,
    Idle,
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
    /// Label on the left, quieter value on the right. An empty value makes it a button.
    Pair { label: String, value: String },
    /// One metered window: label and trailing text on a line, a bar (none without a share),
    /// then the summary line.
    Meter {
        label: String,
        trailing: String,
        fraction: Option<f32>,
        summary: String,
    },
    /// Secondary-ink paragraph.
    Note(String),
    /// Primary-ink paragraph.
    Text(String),
    /// Critical-ink line that says the account is stopped (a spent limit).
    Alert(String),
    /// Hairline that sets the session list apart from the limit windows.
    Rule,
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
    /// A live session: name and status word with its ring, then detail and age.
    Session {
        name: String,
        dot: Dot,
        word: String,
        detail: String,
        age: String,
    },
}

impl Row {
    /// How many things on the row can be pressed: its buttons, or the row itself.
    pub fn slots(&self) -> usize {
        match self {
            Row::Buttons { buttons, close } => buttons.len() + usize::from(*close),
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
    (Panel::new(content), false)
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

/// The System card: CPU, then memory, each a bar with its detail line.
pub fn system(machine: Option<&Machine>) -> CardContent {
    let mut rows = Vec::new();
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
                    "Memory",
                    Some(mem.used_fraction()),
                    format!("{} of {} used", size_text(mem.used()), size_text(mem.total)),
                )),
                None => rows.push(meter("Memory", None, "Memory readings unavailable".into())),
            }
        }
        None => rows.push(Row::Note("Waiting for the first sample".into())),
    }
    CardContent {
        mark: Mark::System,
        ..CardContent::plain("System Usage", rows)
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
                        size_text(drive.free),
                        size_text(drive.total)
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
fn status_message(name: &str, status: Status) -> String {
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
        // Says what happened and what fixes it, not "sign in" (the login is there).
        Status::AccessDenied => format!(
            "Windows refused Pulse access to {name}'s saved login. Fix the file's permissions to read usage."
        ),
    }
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
        rows.push(Row::Note(status_message(name, usage.status)));
    }
    for window in &usage.windows {
        let (used, left) = halves(window.fraction);
        rows.push(Row::Meter {
            label: window.label.clone(),
            trailing: window
                .resets_at
                .map(|reset| reset_text(reset, now, offset))
                .unwrap_or_default(),
            fraction: Some(window.fraction),
            summary: format!("{used}% Used \u{b7} {left}% left"),
        });
    }
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
