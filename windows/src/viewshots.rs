//! Off-screen renderer for the Windows notch's views (CI only).
//!
//! `pulse-windows-prototype --render-views <dir>` (or `PULSE_RENDER_VIEWS=1` with
//! `PULSE_VIEW_SHOTS=<dir>`) reads the platform-neutral fixtures in `qa/notch-views.json`
//! (`PULSE_VIEWS_JSON` overrides the path), maps each onto the notch's own data types, draws
//! it with the software rasteriser at 2x on the entry's backdrop colour and writes
//! `<id>.png`, so CI can set it beside the Mac renderer's output. Views the Windows notch has
//! no equivalent for get a placeholder PNG reading "No Windows equivalent: <title>" and are
//! listed in `windows-gaps.txt` beside the images.
//!
//! It runs before any window, hook, timer or single-instance lock exists and never touches
//! the desktop. The PNG encoder is hand-written (stored deflate blocks), so no codec, COM or
//! extra crate is involved.

use crate::alerts;
use crate::canvas::Canvas;
use crate::card::{self, CardContent, Lead, Mark, Row, Tail};
use crate::drive_health::{self, Report};
use crate::glyphs::{Symbol, Tile};
use crate::json::{self, Value};
use crate::layout::{self, Badges, Cell, CellView, Edge};
use crate::render;
use crate::send::{self, Action, Panel};
use crate::sensors::{Drive, Machine, MemInfo, NetRate, Reading, Temp};
use crate::surface::TextPainter;
use crate::update;
use crate::usage::{Block, LimitWindow, Status, Usage};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// 2x of the 96 DPI design frame, like the Mac renderer's scale of 2.
const DPI: u32 = 192;
/// Backdrop margin around every view, in pixels.
const MARGIN: usize = 48;
/// The instant every fixture's "minutes from now" is measured from (Unix seconds).
const NOW: u64 = 1_800_000_000;
const MAX_JSON_BYTES: usize = 8 * 1024 * 1024;
const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
/// Totals used when a fixture gives a used share but no size text.
const DEFAULT_MEMORY: u64 = 16 << 30;
const DEFAULT_DISK: u64 = 1024 << 30;

const GAP_DISK_IMAGE: &str = "No Windows installer card for this disk image state";

type Parts = Vec<(Canvas, i32, i32)>;
type Outcome = Result<Parts, &'static str>;

/// Runs the renderer when asked for and returns its exit code; `None` means a normal start.
pub fn run_if_requested() -> Option<ExitCode> {
    let args: Vec<String> = std::env::args().collect();
    let default_dir = || PathBuf::from("view-shots");
    let dir = match args.iter().position(|a| a == "--render-views") {
        Some(at) => args.get(at + 1).map_or_else(default_dir, PathBuf::from),
        None if std::env::var("PULSE_RENDER_VIEWS").is_ok_and(|v| v == "1") => {
            std::env::var_os("PULSE_VIEW_SHOTS").map_or_else(default_dir, PathBuf::from)
        }
        None => return None,
    };
    Some(run(&dir))
}

/// Where the fixtures are: `PULSE_VIEWS_JSON`, else `qa/notch-views.json` under the working
/// directory (CI runs from the repository root), else beside the program or in `qa/` under
/// it or one of its parent folders (an installed build run from anywhere, next to a checkout
/// or a copy of the file). The first that exists wins; with none, the CI default is returned
/// so the error names it.
fn fixtures_path() -> PathBuf {
    if let Some(path) = std::env::var_os("PULSE_VIEWS_JSON") {
        return PathBuf::from(path);
    }
    let default = PathBuf::from("qa/notch-views.json");
    let mut candidates = vec![default.clone()];
    if let Some(folder) = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
    {
        for dir in folder.ancestors().take(6) {
            candidates.push(dir.join("notch-views.json"));
            candidates.push(dir.join("qa").join("notch-views.json"));
        }
    }
    candidates
        .into_iter()
        .find(|path| path.is_file())
        .unwrap_or(default)
}

fn run(dir: &Path) -> ExitCode {
    let source = fixtures_path();
    let bytes = match std::fs::read(&source) {
        Ok(bytes) => bytes,
        Err(error) => {
            eprintln!(
                "view-shots: cannot read {}: {error} (set PULSE_VIEWS_JSON to qa/notch-views.json)",
                source.display()
            );
            return ExitCode::from(2);
        }
    };
    let Some(list) = json::parse(&numeric_booleans(&bytes), MAX_JSON_BYTES) else {
        eprintln!("view-shots: {} is not valid JSON", source.display());
        return ExitCode::from(2);
    };
    let entries = list.as_array().unwrap_or(&[]);
    if let Err(error) = std::fs::create_dir_all(dir) {
        eprintln!("view-shots: cannot create {}: {error}", dir.display());
        return ExitCode::from(2);
    }
    let Some(mut text) = TextPainter::new() else {
        eprintln!("view-shots: no text rasteriser");
        return ExitCode::from(2);
    };

    let mut drawn = 0usize;
    let mut failures = 0usize;
    let mut gaps: Vec<String> = Vec::new();
    for entry in entries {
        let (Some(id), Some(fixture)) = (str_of(entry, "id"), entry.get("fixture")) else {
            eprintln!("view-shots: an entry has no id or fixture");
            failures += 1;
            continue;
        };
        if !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            eprintln!("view-shots: skipped an unsafe id");
            failures += 1;
            continue;
        }
        let title = str_of(entry, "title").unwrap_or(id);
        let area = str_of(entry, "area").unwrap_or("");
        let backdrop = hex(str_of(entry, "backdrop"));
        let parts = match build(id, area, fixture, &mut text) {
            Ok(parts) => {
                drawn += 1;
                parts
            }
            Err(reason) => {
                let Some(parts) = placeholder(title, &mut text) else {
                    eprintln!("view-shots: could not draw the placeholder for {id}");
                    failures += 1;
                    continue;
                };
                gaps.push(format!("{id}\t{title}\t{reason}"));
                parts
            }
        };
        let file = dir.join(format!("{id}.png"));
        if let Err(error) = std::fs::write(&file, png(&compose(&parts, backdrop))) {
            eprintln!("view-shots: could not write {id}.png: {error}");
            failures += 1;
        }
    }

    let mut report = format!(
        "# {} of {} views have no Windows equivalent (id, title, reason)\n",
        gaps.len(),
        entries.len()
    );
    for line in &gaps {
        report.push_str(line);
        report.push('\n');
    }
    if let Err(error) = std::fs::write(dir.join("windows-gaps.txt"), report) {
        eprintln!("view-shots: could not write windows-gaps.txt: {error}");
        failures += 1;
    }
    println!(
        "view-shots: {} views to {}: {} rendered, {} placeholders, {} failures",
        entries.len(),
        dir.display(),
        drawn,
        gaps.len(),
        failures
    );
    if failures == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

// ---- fixture access ---------------------------------------------------------------------------

/// The JSON reader maps `true` and `false` to the same `Null`, so booleans become 1 and 0
/// before parsing (outside strings).
fn numeric_booleans(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    let (mut in_string, mut escaped) = (false, false);
    let mut at = 0;
    while at < bytes.len() {
        let byte = bytes[at];
        if in_string {
            out.push(byte);
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
            at += 1;
        } else if byte == b'"' {
            in_string = true;
            out.push(byte);
            at += 1;
        } else if bytes[at..].starts_with(b"true") {
            out.push(b'1');
            at += 4;
        } else if bytes[at..].starts_with(b"false") {
            out.push(b'0');
            at += 5;
        } else {
            out.push(byte);
            at += 1;
        }
    }
    out
}

fn str_of<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

fn num_of(value: &Value, key: &str) -> Option<f64> {
    value.get(key).and_then(Value::as_f64)
}

fn flag(value: &Value, key: &str) -> bool {
    num_of(value, key).is_some_and(|n| n >= 1.0)
}

fn arr<'a>(value: &'a Value, key: &str) -> &'a [Value] {
    value.get(key).and_then(Value::as_array).unwrap_or(&[])
}

fn hex(text: Option<&str>) -> u32 {
    text.and_then(|t| u32::from_str_radix(t.trim_start_matches('#'), 16).ok())
        .unwrap_or(0)
}

/// Sizes in a detail string ("417 GB free of 994 GB", "9.1 GB of 24 GB used") as bytes.
fn sizes(detail: &str) -> Vec<u64> {
    let words: Vec<&str> = detail.split_whitespace().collect();
    words
        .windows(2)
        .filter_map(|pair| {
            let value: f64 = pair[0].parse().ok()?;
            let unit = match pair[1] {
                "MB" => 1.0 / 1024.0,
                "GB" => 1.0,
                "TB" => 1024.0,
                _ => return None,
            };
            Some((value * unit * GIB) as u64)
        })
        .collect()
}

fn cores_in(detail: &str) -> u32 {
    let words: Vec<&str> = detail.split_whitespace().collect();
    words
        .windows(2)
        .find(|pair| pair[1] == "cores")
        .and_then(|pair| pair[0].parse().ok())
        .unwrap_or(0)
}

// ---- fixture -> the notch's data types --------------------------------------------------------

fn usage_from(cell: &Value) -> Usage {
    let windows = arr(cell, "windows")
        .iter()
        .filter_map(|w| {
            Some(LimitWindow {
                key: str_of(w, "id")?.to_string(),
                group: str_of(w, "group").map(str::to_string),
                label: str_of(w, "label")?.to_string(),
                fraction: num_of(w, "used")? as f32,
                resets_at: num_of(w, "resetsInMinutes").map(|m| NOW + (m * 60.0) as u64),
            })
        })
        .collect();
    let (status, updated) = match str_of(cell, "status") {
        Some("stale") => {
            let minutes = num_of(cell, "staleMinutes").unwrap_or(30.0);
            (Status::Unavailable, Some(NOW - (minutes * 60.0) as u64))
        }
        Some("needs-auth") => (Status::SignIn, None),
        Some("access-denied") => (Status::AccessDenied, None),
        Some("ok") | None => (Status::Ok, Some(NOW - 20)),
        Some(_) => (Status::Unavailable, None),
    };
    Usage {
        status,
        plan: str_of(cell, "plan").map(str::to_string),
        windows,
        updated,
        block: cell.get("block").map(|block| Block {
            reason: str_of(block, "reason")
                .unwrap_or("Limit reached")
                .to_string(),
            resets_at: num_of(block, "resetsInMinutes").map(|m| NOW + (m * 60.0) as u64),
        }),
        account: None,
        extras: crate::usage::Extras::default(),
    }
}

// ---- drive health fixture rows -> the notch's Report ------------------------------------------

/// Days from 1970-01-01 to a civil date (the inverse of `drive_health::date_text`).
fn date_secs(text: &str) -> Option<u64> {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let mut words = text.split_whitespace();
    let month = MONTHS.iter().position(|m| Some(*m) == words.next())? as i64 + 1;
    let day: i64 = words.next()?.trim_end_matches(',').parse().ok()?;
    let year: i64 = words.next()?.parse().ok()?;
    let y = year - i64::from(month <= 2);
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let doy = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    u64::try_from((era * 146_097 + doe - 719_468) * 86_400).ok()
}

/// A reading from a health line's state part ("OK · 3% worn · 21.4 TB written").
fn reading_from(state: &str, at: u64) -> drive_health::Reading {
    let mut reading = drive_health::Reading {
        at,
        passed: Some(!state.starts_with("Warning")),
        temperature_c: None,
        wear_percent: None,
        written_bytes: None,
        media_errors: None,
        critical_warning: None,
    };
    for part in state.split(" \u{b7} ") {
        let number = part.split_whitespace().next().unwrap_or("");
        if let Some(percent) = part.strip_suffix("% worn") {
            reading.wear_percent = percent.parse().ok();
        } else if let Some(celsius) = part.strip_suffix(" \u{b0}C") {
            reading.temperature_c = celsius.parse().ok();
        } else if part.ends_with(" TB written") {
            reading.written_bytes = number.parse::<f64>().ok().map(|v| (v * 1e12) as u64);
        } else if part.ends_with(" GB written") {
            reading.written_bytes = number.parse::<f64>().ok().map(|v| (v * 1e9) as u64);
        }
    }
    reading
}

fn health_from(windows: &[Value]) -> Report {
    if windows
        .iter()
        .any(|w| str_of(w, "id") == Some("health:install"))
    {
        return Report::Missing;
    }
    let detail_of = |id: &str| {
        windows
            .iter()
            .find(|w| str_of(w, "id") == Some(id))
            .and_then(|w| str_of(w, "detail"))
    };
    let mut drives = Vec::new();
    for window in windows {
        let Some(device) = str_of(window, "id")
            .and_then(|id| id.strip_prefix("health:"))
            .and_then(|id| id.strip_suffix(":a"))
        else {
            continue;
        };
        let detail = str_of(window, "detail").unwrap_or("");
        let Some((name, state)) = detail.split_once(": ") else {
            continue;
        };
        let reachable = !state.starts_with("Health n/a");
        let last = if reachable {
            Some(reading_from(state, NOW))
        } else {
            detail_of(&format!("health:{device}:last"))
                .and_then(|d| d.strip_prefix("Last reading "))
                .and_then(|d| d.split_once(": "))
                .map(|(date, state)| reading_from(state, date_secs(date).unwrap_or(NOW)))
        };
        drives.push(drive_health::Drive {
            name: name.to_string(),
            last,
            reachable,
        });
    }
    if drives.is_empty() {
        Report::Pending
    } else {
        Report::Drives(drives)
    }
}

fn memory_from(window: &Value) -> Option<MemInfo> {
    let used = num_of(window, "used")?.clamp(0.0, 1.0);
    let total = sizes(str_of(window, "detail").unwrap_or(""))
        .get(1)
        .copied()
        .unwrap_or(DEFAULT_MEMORY);
    let used_bytes = (used * total as f64) as u64;
    // The fixture has no commit figure: commit is chosen so the Windows bands (available
    // memory and commit charge) land in the fixture's band. A fixture with no band has an
    // unknown pressure, which a zero commit limit stands for.
    let (commit_limit, commit_used) = match str_of(window, "band") {
        Some("critical") => (total, (total as f64 * 0.97) as u64),
        Some("watch") => (total, (total as f64 * 0.9) as u64),
        Some(_) => (total, used_bytes),
        None => (0, 0),
    };
    Some(MemInfo {
        total,
        available: total.saturating_sub(used_bytes),
        commit_limit,
        commit_used,
    })
}

/// The rate after `arrow` in a detail like "down 1.2 MB/s, up 180 KB/s, Wi-Fi" (the Mac's
/// arrows and dots), as bytes per second.
fn rate_after(detail: &str, arrow: char) -> Option<f64> {
    let rest = detail.split_once(arrow)?.1;
    let mut words = rest.split_whitespace();
    let value: f64 = words.next()?.parse().ok()?;
    let unit = match words.next()? {
        "B/s" => 1.0,
        "KB/s" => 1e3,
        "MB/s" => 1e6,
        "GB/s" => 1e9,
        _ => return None,
    };
    Some(value * unit)
}

fn gpu_from(window: &Value) -> Reading<f32> {
    let busy = str_of(window, "detail")
        .and_then(|d| d.split('%').next())
        .and_then(|n| n.trim().parse::<f32>().ok());
    busy.map_or(Reading::Unavailable, |n| Reading::Value(n / 100.0))
}

fn network_from(window: &Value) -> Reading<NetRate> {
    let detail = str_of(window, "detail").unwrap_or("");
    match (
        rate_after(detail, '\u{2193}'),
        rate_after(detail, '\u{2191}'),
    ) {
        (Some(down), Some(up)) => Reading::Value(NetRate {
            down,
            up,
            kind: detail
                .rsplit(" \u{b7} ")
                .next()
                .unwrap_or("Network")
                .to_string(),
        }),
        _ => Reading::Unavailable,
    }
}

fn fans_from(window: &Value) -> Reading<Vec<u32>> {
    let speeds: Vec<u32> = str_of(window, "detail")
        .unwrap_or("")
        .split(" rpm")
        .next()
        .unwrap_or("")
        .split(" / ")
        .filter_map(|n| n.trim().parse().ok())
        .collect();
    if speeds.is_empty() {
        Reading::Unavailable
    } else {
        Reading::Value(speeds)
    }
}

/// The header note ("54" and the degree sign C) as one degrees reading; the Mac's is the
/// CPU's, and the Windows card names its source.
fn temperature_from(cell: &Value) -> Reading<Vec<Temp>> {
    str_of(cell, "headerNote")
        .and_then(|n| n.split_whitespace().next())
        .and_then(|n| n.parse().ok())
        .map_or(Reading::Unavailable, |celsius| {
            Reading::Value(vec![Temp {
                source: "CPU",
                celsius,
            }])
        })
}

fn drive_from(window: &Value, system: bool) -> Option<Drive> {
    let used = num_of(window, "used")?.clamp(0.0, 1.0);
    let (total, free) = match sizes(str_of(window, "detail").unwrap_or("")).as_slice() {
        [free, total, ..] => (*total, *free),
        _ => (DEFAULT_DISK, (DEFAULT_DISK as f64 * (1.0 - used)) as u64),
    };
    Some(Drive {
        root: str_of(window, "label")?.to_string(),
        total,
        free,
        system,
    })
}

fn ring_from(cell: Option<&Value>) -> send::Ring {
    let transfer = cell.and_then(|c| {
        arr(c, "windows")
            .iter()
            .find(|w| str_of(w, "id") == Some("transfer"))
    });
    match transfer.and_then(|w| num_of(w, "used")) {
        Some(used) => {
            let fraction = used as f32;
            send::Ring {
                fraction: Some(fraction),
                active: true,
                problem: false,
                label: percent_text(fraction),
                complete: false,
            }
        }
        None => {
            let off = transfer.and_then(|w| str_of(w, "detail")) == Some("Off");
            send::Ring {
                fraction: None,
                active: false,
                problem: false,
                label: if off { "Off" } else { "Idle" }.to_string(),
                complete: false,
            }
        }
    }
}

fn percent_text(fraction: f32) -> String {
    format!("{}%", (fraction.clamp(0.0, 1.0) * 100.0).round() as u32)
}

/// Everything the notch reads, rebuilt from the fixture's cells.
struct Scene<'a> {
    machine: Machine,
    usage: [Usage; 2],
    health: Report,
    send_cell: Option<&'a Value>,
}

impl<'a> Scene<'a> {
    fn from_cells(cells: &[&'a Value]) -> Self {
        let mut machine = Machine {
            cpu: None,
            cores: 0,
            memory: None,
            drives: Vec::new(),
            // A reading the fixture does not state is one this PC cannot give.
            gpu: Reading::Unavailable,
            network: Reading::Unavailable,
            temperature: Reading::Unavailable,
            fans: Reading::Unavailable,
        };
        let mut usage = [Usage::waiting(), Usage::waiting()];
        let mut send_cell = None;
        let mut health = Report::Pending;
        for cell in cells {
            match str_of(cell, "id") {
                Some("claude") => usage[0] = usage_from(cell),
                Some("codex") => usage[1] = usage_from(cell),
                Some("system-send") => send_cell = Some(*cell),
                Some("system-disks") => health = health_from(arr(cell, "windows")),
                _ => {}
            }
            if str_of(cell, "id") == Some("system-cpu") {
                machine.temperature = temperature_from(cell);
            }
            for window in arr(cell, "windows") {
                let id = str_of(window, "id").unwrap_or("");
                if id == "gpu" {
                    machine.gpu = gpu_from(window);
                } else if id == "network" {
                    machine.network = network_from(window);
                } else if id == "fans" {
                    machine.fans = fans_from(window);
                } else if id == "cpu" {
                    machine.cpu = num_of(window, "used").map(|u| u as f32);
                    machine.cores = cores_in(str_of(window, "detail").unwrap_or(""));
                } else if id == "pressure" {
                    machine.memory = memory_from(window);
                } else if id.starts_with("disk:") {
                    machine
                        .drives
                        .extend(drive_from(window, id == "disk:internal"));
                }
            }
        }
        Self {
            machine,
            usage,
            health,
            send_cell,
        }
    }

    fn views(&self) -> Vec<CellView> {
        layout::views(Some(&self.machine), &self.usage, &ring_from(self.send_cell))
    }

    fn panel(&self, cell: Cell) -> Panel {
        match cell {
            Cell::Send => send_hover(self.send_cell),
            Cell::Disk => Panel::new(card::disks(Some(&self.machine), &self.health)),
            _ => card::panel_for(cell, Some(&self.machine), &self.usage, NOW, None).0,
        }
    }
}

fn windows_cell(id: &str) -> Option<Cell> {
    match id {
        "claude" => Some(Cell::Claude),
        "codex" => Some(Cell::Codex),
        "system-cpu" => Some(Cell::Cpu),
        "system-disks" => Some(Cell::Disk),
        "system-send" => Some(Cell::Send),
        _ => None,
    }
}

// ---- panels the send module builds privately (same rows, same order) ---------------------------

#[derive(Default)]
struct Rows {
    rows: Vec<Row>,
    actions: Vec<Vec<Option<Action>>>,
}

impl Rows {
    fn add(&mut self, row: Row, action: Option<Action>) {
        self.rows.push(row);
        self.actions.push(vec![action]);
    }

    /// A button plate: its symbol, label and the detail at the right.
    fn button(&mut self, (symbol, label, detail): (Symbol, &str, &str), action: Option<Action>) {
        self.add(
            Row::Button {
                symbol: Some(symbol),
                label: label.to_string(),
                detail: detail.to_string(),
            },
            action,
        );
    }

    fn finish(self, title: &str) -> Panel {
        Panel {
            content: CardContent {
                title: title.to_string(),
                accessory: None,
                rows: self.rows,
                ..CardContent::default()
            },
            actions: self.actions,
            head: Vec::new(),
        }
    }
}

fn pair(label: &str, value: &str) -> Row {
    Row::Pair {
        label: label.to_string(),
        value: value.to_string(),
    }
}

/// The Send cell's hover card (`send::hover_panel` reads live hub state, so it is rebuilt
/// here from the fixture).
fn send_hover(cell: Option<&Value>) -> Panel {
    let windows = cell.map_or(&[][..], |c| arr(c, "windows"));
    let transfer = windows.iter().find(|w| str_of(w, "id") == Some("transfer"));
    let detail = transfer.and_then(|w| str_of(w, "detail")).unwrap_or("");
    let mut panel = Rows::default();
    // The Mac's order: the headline, the devices, the hint, then one bottom bar. The
    // fixtures still carry the old separate Copy last and Paste rows (the Mac renderer folds
    // them the same way); "action:bar" is read too.
    let copy = windows
        .iter()
        .find(|w| matches!(str_of(w, "id"), Some("action:copylast" | "action:bar")))
        .and_then(|w| str_of(w, "label"))
        .filter(|label| !label.is_empty());
    match transfer.and_then(|w| num_of(w, "used").map(|f| (w, f as f32))) {
        Some((window, fraction)) => {
            panel.add(
                Row::Meter {
                    label: str_of(window, "label").unwrap_or("").to_string(),
                    trailing: String::new(),
                    fraction: Some(fraction),
                    summary: detail.to_string(),
                },
                None,
            );
            panel.button((Symbol::Stop, "Cancel", ""), Some(Action::Cancel));
        }
        None if !detail.is_empty() => panel.add(pair("Nearby sharing", detail), None),
        None => {}
    }
    let devices: Vec<(&str, &str, &str)> = windows
        .iter()
        .filter_map(|w| {
            let fingerprint = str_of(w, "id")?.strip_prefix("nearby:")?;
            Some((
                fingerprint,
                str_of(w, "label")?,
                str_of(w, "detail").unwrap_or(""),
            ))
        })
        .collect();
    for (fingerprint, alias, kind) in &devices {
        panel.add(
            pair(alias, kind),
            Some(Action::SendTo((*fingerprint).to_string())),
        );
    }
    let blocked = windows
        .iter()
        .any(|w| str_of(w, "id") == Some("hint-network"));
    if blocked {
        panel.add(Row::Text(send::FIREWALL_HINT.to_string()), None);
    } else if !devices.is_empty() {
        panel.add(
            Row::Text("Ctrl+V sends the clipboard \u{b7} drop files here".to_string()),
            None,
        );
    }
    let paste = !devices.is_empty();
    if copy.is_some() || paste {
        panel.rows.push(Row::Bar {
            copy: copy.map(str::to_string),
            copy_detail: copy.map_or_else(String::new, |_| "3 min ago".to_string()),
            paste,
        });
        panel.actions.push(vec![
            copy.map(|_| Action::CopyLast),
            paste.then_some(Action::Paste),
        ]);
    }
    let mut panel = panel.finish("Send");
    panel.content.mark = Mark::Send;
    panel
}

/// A notch card of the send flow: the fixture's words and buttons as a `send::Prompt`, laid
/// out by the same `Prompt::panel` the live card uses.
fn prompt_panel(fixture: &Value) -> Panel {
    let mut prompt = send::Prompt {
        title: str_of(fixture, "title").unwrap_or("").to_string(),
        detail: str_of(fixture, "detail").unwrap_or("").to_string(),
        problem: str_of(fixture, "style") == Some("problem"),
        buttons: Vec::new(),
        view: send::View::Plain,
        lead: Lead::Tile(match str_of(fixture, "icon") {
            Some("messages") => Tile::Message,
            Some("app" | "installer") => Tile::Package,
            _ => Tile::Folder,
        }),
    };
    match fixture.get("send") {
        Some(send) => {
            prompt.view = match send.get("transfer") {
                Some(transfer) => send::View::Transfer {
                    stage: match str_of(transfer, "state") {
                        Some("active") => send::Stage::Active,
                        Some("done") => send::Stage::Done,
                        Some("problem") => send::Stage::Problem,
                        _ => send::Stage::Waiting,
                    },
                    fraction: num_of(transfer, "fraction").map(|f| f as f32),
                    can_cancel: flag(transfer, "canCancel"),
                    symbol: Symbol::from_name(str_of(transfer, "symbol").unwrap_or("")),
                },
                None => send::View::List {
                    rows: arr(send, "rows")
                        .iter()
                        .map(|device| {
                            let alias = str_of(device, "alias").unwrap_or("").to_string();
                            (
                                alias.clone(),
                                alias,
                                str_of(device, "model").unwrap_or("").to_string(),
                                Symbol::from_name(str_of(device, "symbol").unwrap_or("")),
                            )
                        })
                        .collect(),
                    scanning: flag(send, "scanning"),
                    above: 0,
                    below: 0,
                },
            };
        }
        None => {
            for key in ["primary", "secondary"] {
                let Some(button) = fixture.get(key) else {
                    continue;
                };
                let action = match str_of(button, "choice") {
                    Some("install") => Action::Accept,
                    Some("cancel") => Action::Decline,
                    Some("showImage") => Action::Show,
                    Some("copyText") => Action::Copy,
                    Some("openLink") => Action::OpenLink,
                    _ => Action::Close,
                };
                prompt
                    .buttons
                    .push((str_of(button, "label").unwrap_or("").to_string(), action));
            }
        }
    }
    prompt.panel()
}

// ---- building a view --------------------------------------------------------------------------

fn build(id: &str, area: &str, fixture: &Value, text: &mut TextPainter) -> Outcome {
    match str_of(fixture, "kind") {
        Some("ring") => ring(fixture, text),
        Some("tooltip") => tooltip(fixture, text),
        Some("card") if area == "disk-image-card" => crate::installer::sample(id)
            .map(|panel| vec![(card_canvas(&panel, text), 0, 0)])
            .ok_or(GAP_DISK_IMAGE),
        Some("card") => Ok(vec![(card_canvas(&prompt_panel(fixture), text), 0, 0)]),
        Some("update") => Ok(vec![(card_canvas(&update_panel(fixture), text), 0, 0)]),
        Some("alert") => Ok(vec![(card_canvas(&alert_panel(fixture), text), 0, 0)]),
        Some("menu") => {
            let state = str_of(fixture, "state");
            let pressed = state == Some("pressed");
            render::set_pressed(pressed);
            let canvas =
                render::render_menu_hover(DPI, text, state.is_some() && state != Some("rest"));
            render::set_pressed(false);
            Ok(vec![(canvas, 0, 0)])
        }
        Some("notch") => notch(id, fixture, text),
        _ => Err("Unknown fixture kind"),
    }
}

/// The update card (`update::card`) for an update fixture.
fn update_panel(fixture: &Value) -> Panel {
    let phase = match str_of(fixture, "phase") {
        Some("downloading") => {
            update::Phase::Downloading(num_of(fixture, "progress").map(|p| p as f32))
        }
        Some("extracting") => update::Phase::Preparing,
        Some("installing") => update::Phase::Installing,
        _ => update::Phase::Available,
    };
    update::card(
        str_of(fixture, "version").unwrap_or(""),
        str_of(fixture, "notes").unwrap_or(""),
        phase,
    )
}

/// The usage alert card (`alerts::card_content`) for an alert fixture; clock times are UTC.
fn alert_panel(fixture: &Value) -> Panel {
    let kind = match str_of(fixture, "alertKind") {
        Some("sessionLimitReached") => alerts::Kind::SessionLimitReached,
        Some("weeklyLimitReached") => alerts::Kind::WeeklyLimitReached,
        _ => alerts::Kind::Reset,
    };
    let notice = str_of(fixture, "noticeTitle").map(|title| alerts::Notice {
        title: title.to_string(),
        subtitle: str_of(fixture, "noticeSubtitle").unwrap_or("").to_string(),
        status: str_of(fixture, "noticeStatus").unwrap_or("").to_string(),
    });
    let alert = alerts::Alert {
        kind,
        provider: str_of(fixture, "provider").unwrap_or("").to_string(),
        window: str_of(fixture, "window").unwrap_or("").to_string(),
        resets_at: num_of(fixture, "resetsInMinutes").map(|m| NOW + (m * 60.0) as u64),
        notice,
    };
    Panel::new(alerts::card_content(&alert, 0))
}

/// A card with its tail pointing back at a notch on the right, like the Mac's card shots.
fn card_canvas(panel: &Panel, text: &mut TextPainter) -> Canvas {
    card_canvas_at(panel, Edge::Right, text)
}

/// A card with its tail toward a notch on `edge`.
fn card_canvas_at(panel: &Panel, edge: Edge, text: &mut TextPainter) -> Canvas {
    let live = panel.live();
    let mut content = panel.content.clone();
    content.tail = Some(Tail { edge, offset: 0 });
    render::render_card(&content, &live, DPI, text, None)
}

/// One ring, by its place in the notch (the System ring carries memory and CPU).
fn ring(fixture: &Value, text: &mut TextPainter) -> Outcome {
    let cell = fixture.get("cell").ok_or("Fixture has no cell")?;
    let index = match str_of(cell, "id") {
        Some("claude") => 0,
        Some("codex") => 1,
        Some("system-cpu") => 2,
        Some("system-disks") => 3,
        Some("system-send") => 4,
        _ => return Err("Unknown ring cell"),
    };
    let mut views = Scene::from_cells(&[cell]).views();
    overlay(&mut views, &[cell]);
    let view = views.get(index).ok_or("No such ring")?;
    Ok(vec![(render::render_cell(view, DPI, text), 0, 0)])
}

fn pressure_unknown(cell: &Value) -> bool {
    arr(cell, "windows")
        .iter()
        .any(|w| str_of(w, "detail").is_some_and(|d| d.starts_with("Pressure unknown")))
}

fn tooltip(fixture: &Value, text: &mut TextPainter) -> Outcome {
    let cell = fixture.get("cell").ok_or("Fixture has no cell")?;
    let id = str_of(cell, "id").unwrap_or("");
    let scene = Scene::from_cells(&[cell]);
    match id {
        "claude" | "codex" => {
            let which = if id == "claude" {
                Cell::Claude
            } else {
                Cell::Codex
            };
            let panel = scene.panel(which);
            Ok(vec![(card_canvas(&panel, text), 0, 0)])
        }
        // One System card: CPU, memory pressure, GPU, network, fans (and temperature).
        "system-cpu" => Ok(vec![(card_canvas(&scene.panel(Cell::Cpu), text), 0, 0)]),
        "system-disks" => Ok(vec![(card_canvas(&scene.panel(Cell::Disk), text), 0, 0)]),
        "system-send" => Ok(vec![(card_canvas(&scene.panel(Cell::Send), text), 0, 0)]),
        _ => Err("Unknown card cell"),
    }
}

/// What the fixture states that the Windows readers do not derive on their own: the Memory
/// ring's colour band (`band`, or none when the pressure is unknown, so the share decides).
/// A spent limit (`block`) already arrives through the cell's `Usage`.
fn overlay(views: &mut [CellView], cells: &[&Value]) {
    for cell in cells {
        if str_of(cell, "id") == Some("system-cpu") {
            {
                let band = arr(cell, "windows")
                    .iter()
                    .find(|w| str_of(w, "id") == Some("pressure"))
                    .map(|w| match str_of(w, "band") {
                        Some("ample") => Some(layout::BAND_AMPLE),
                        Some("watch") => Some(layout::BAND_WATCH),
                        Some("critical") => Some(layout::BAND_CRITICAL),
                        _ => None,
                    });
                match (views.get_mut(2), band) {
                    (Some(view), Some(Some(colour))) => view.band = Some(colour),
                    (Some(view), Some(None)) if pressure_unknown(cell) => view.band = None,
                    _ => {}
                }
            }
        }
    }
}

/// The notch on its fixture's edge, folded or open, with its badges and the hover card beside
/// the hovered ring on the side away from the edge.
fn notch(_id: &str, fixture: &Value, text: &mut TextPainter) -> Outcome {
    let edge = Edge::parse(str_of(fixture, "edge").unwrap_or("top")).ok_or("Unknown notch edge")?;
    let folded = !flag(fixture, "expanded");
    let badges = fixture.get("badges").map_or(Badges::default(), |b| Badges {
        update: flag(b, "update"),
        permissions: flag(b, "permissions"),
    });
    let cells: Vec<&Value> = arr(fixture, "cells").iter().collect();
    let scene = Scene::from_cells(&cells);
    let mut views = scene.views();
    overlay(&mut views, &cells);
    let body = render::render_notch_shot(&views, edge, folded, badges, DPI, text);
    // The body keeps the Mac's bezel band along the welded side; the notch proper starts after it.
    let band = (layout::BAND * layout::scale(DPI)).round() as i32;
    let (offset_x, offset_y) = match edge {
        Edge::Top => (0, band),
        Edge::Left => (band, 0),
        Edge::Bottom | Edge::Right => (0, 0),
    };
    let (panel_w, panel_h) = layout::panel_size(edge, folded, DPI);
    let notch_rect = (offset_x, offset_y, offset_x + panel_w, offset_y + panel_h);
    let mut parts = vec![(body, 0, 0)];
    if let Some(hover) = str_of(fixture, "hover").filter(|_| !folded) {
        let cell = windows_cell(hover).ok_or("Unknown hover cell")?;
        let index = layout::shown()
            .iter()
            .position(|c| *c == cell)
            .ok_or("Unknown hover cell")?;
        let panel = scene.panel(cell);
        let card = card_canvas_at(&panel, edge, text);
        let (ring_x, ring_y) = layout::ring_center(edge, index, DPI);
        let centre = if edge.is_vertical() {
            ring_y as i32 + offset_y
        } else {
            ring_x as i32 + offset_x
        };
        let gap = (layout::CARD_GAP * layout::scale(DPI)).round() as i32;
        // The card may start left of or above the notch: the open monitor lets it, and the
        // parts are shifted back into the image below.
        let (x, y) = layout::card_origin(
            edge,
            notch_rect,
            centre,
            (card.width as i32, card.height as i32),
            gap,
            (i32::MIN / 2, i32::MIN / 2, i32::MAX / 2, i32::MAX / 2),
        );
        parts.push((card, x, y));
    }
    let (shift_x, shift_y) = parts
        .iter()
        .fold((0, 0), |(sx, sy), (_, x, y)| (sx.min(*x), sy.min(*y)));
    for (_, x, y) in &mut parts {
        *x -= shift_x;
        *y -= shift_y;
    }
    Ok(parts)
}

fn placeholder(title: &str, text: &mut TextPainter) -> Option<Parts> {
    let mask = text.render(&format!("No Windows equivalent: {title}"), 28, false)?;
    let mut canvas = Canvas::new(mask.width.max(1), mask.height.max(1));
    canvas.draw_mask(&mask, 0, 0, layout::INK_PRIMARY, 1.0);
    Some(vec![(canvas, 0, 0)])
}

// ---- composing and encoding -------------------------------------------------------------------

struct Image {
    width: usize,
    height: usize,
    /// Row-major RGB, top row first.
    rgb: Vec<u8>,
}

/// Lays the premultiplied canvases over an opaque backdrop with a margin all round.
fn compose(parts: &[(Canvas, i32, i32)], backdrop: u32) -> Image {
    let (mut inner_width, mut inner_height) = (1usize, 1usize);
    for (canvas, x, y) in parts {
        inner_width = inner_width.max(*x as usize + canvas.width);
        inner_height = inner_height.max(*y as usize + canvas.height);
    }
    let width = inner_width + 2 * MARGIN;
    let height = inner_height + 2 * MARGIN;
    let colour = [
        (backdrop >> 16) as u8,
        (backdrop >> 8) as u8,
        backdrop as u8,
    ];
    let mut rgb = Vec::with_capacity(width * height * 3);
    for _ in 0..width * height {
        rgb.extend_from_slice(&colour);
    }
    for (canvas, x, y) in parts {
        let left = *x as usize + MARGIN;
        let top = *y as usize + MARGIN;
        for row in 0..canvas.height {
            for column in 0..canvas.width {
                let pixel = canvas.pixels[row * canvas.width + column];
                let alpha = pixel >> 24;
                if alpha == 0 {
                    continue;
                }
                let at = ((top + row) * width + left + column) * 3;
                for (channel, shift) in [16u32, 8, 0].into_iter().enumerate() {
                    let source = (pixel >> shift) & 0xFF;
                    let below = u32::from(rgb[at + channel]);
                    let mixed = source + (below * (255 - alpha) + 127) / 255;
                    rgb[at + channel] = mixed.min(255) as u8;
                }
            }
        }
    }
    Image { width, height, rgb }
}

fn crc_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    for (n, slot) in table.iter_mut().enumerate() {
        let mut c = n as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 {
                0xEDB8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
        }
        *slot = c;
    }
    table
}

fn crc32(table: &[u32; 256], bytes: &[u8]) -> u32 {
    let mut c = 0xFFFF_FFFFu32;
    for byte in bytes {
        c = table[((c ^ u32::from(*byte)) & 0xFF) as usize] ^ (c >> 8);
    }
    !c
}

fn adler32(bytes: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for byte in bytes {
        a = (a + u32::from(*byte)) % 65_521;
        b = (b + a) % 65_521;
    }
    (b << 16) | a
}

fn chunk(out: &mut Vec<u8>, table: &[u32; 256], kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    let mut body = Vec::with_capacity(4 + data.len());
    body.extend_from_slice(kind);
    body.extend_from_slice(data);
    out.extend_from_slice(&body);
    out.extend_from_slice(&crc32(table, &body).to_be_bytes());
}

/// An 8-bit RGB PNG whose zlib stream is stored (uncompressed) deflate blocks.
fn png(image: &Image) -> Vec<u8> {
    let table = crc_table();
    let mut raw = Vec::with_capacity((image.width * 3 + 1) * image.height);
    for row in image.rgb.chunks(image.width * 3) {
        raw.push(0); // filter: none
        raw.extend_from_slice(row);
    }
    let mut zlib = vec![0x78, 0x01];
    let blocks: Vec<&[u8]> = raw.chunks(65_535).collect();
    for (index, block) in blocks.iter().enumerate() {
        zlib.push(u8::from(index + 1 == blocks.len()));
        let length = block.len() as u16;
        zlib.extend_from_slice(&length.to_le_bytes());
        zlib.extend_from_slice(&(!length).to_le_bytes());
        zlib.extend_from_slice(block);
    }
    zlib.extend_from_slice(&adler32(&raw).to_be_bytes());

    let mut header = Vec::with_capacity(13);
    header.extend_from_slice(&(image.width as u32).to_be_bytes());
    header.extend_from_slice(&(image.height as u32).to_be_bytes());
    header.extend_from_slice(&[8, 2, 0, 0, 0]);
    let mut out = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    chunk(&mut out, &table, b"IHDR", &header);
    chunk(&mut out, &table, b"IDAT", &zlib);
    chunk(&mut out, &table, b"IEND", &[]);
    out
}
