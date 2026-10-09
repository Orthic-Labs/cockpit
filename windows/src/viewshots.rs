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

use crate::canvas::Canvas;
use crate::card::{self, CardContent, Row};
use crate::json::{self, Value};
use crate::layout::{self, Cell, CellView};
use crate::render;
use crate::send::{self, Action, Panel};
use crate::sensors::{Drive, Machine, MemInfo};
use crate::surface::TextPainter;
use crate::usage::{LimitWindow, Status, Usage};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// 2x of the 96 DPI design frame, like the Mac renderer's scale of 2.
const DPI: u32 = 192;
/// Backdrop margin around every view, in pixels.
const MARGIN: usize = 48;
/// Gap between cards shown side by side, in pixels.
const GAP: i32 = 32;
/// The instant every fixture's "minutes from now" is measured from (Unix seconds).
const NOW: u64 = 1_800_000_000;
const MAX_JSON_BYTES: usize = 8 * 1024 * 1024;
const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
/// Totals used when a fixture gives a used share but no size text.
const DEFAULT_MEMORY: u64 = 16 << 30;
const DEFAULT_DISK: u64 = 1024 << 30;

const GAP_FOLDED: &str =
    "No folded resting state: the Windows notch always draws its six rings (parity C7)";
const GAP_EDGE: &str = "The Windows notch sits on the top edge only (parity C6)";
const GAP_BADGES: &str = "No update or permission badges on the Windows notch (parity C5)";
const GAP_BLOCKED: &str = "No blocked (limit reached) state on the Windows rings or cards";
const GAP_SESSIONS: &str = "No agent-session activity arc on the Windows rings (parity A9)";
const GAP_PRESSURE: &str = "Windows has no memory-pressure band, so no unknown-pressure state";
const GAP_DENIED: &str = "Windows reads plain credential files: there is no access-denied state";
const GAP_HEALTH: &str = "No drive-health rows on the Windows disks card (parity B6)";
const GAP_NETWORK: &str = "No Local Network permission gate on Windows";
const GAP_DISK_IMAGE: &str = "No disk-image install card on Windows (a macOS .dmg flow)";
const GAP_UPDATE: &str = "No update card on the Windows notch yet (parity D8)";
const GAP_ALERT: &str = "No usage alert cards on the Windows notch yet (parity D1-D3)";

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

fn run(dir: &Path) -> ExitCode {
    let source = std::env::var_os("PULSE_VIEWS_JSON")
        .map_or_else(|| PathBuf::from("qa/notch-views.json"), PathBuf::from);
    let bytes = match std::fs::read(&source) {
        Ok(bytes) => bytes,
        Err(error) => {
            eprintln!("view-shots: cannot read {}: {error}", source.display());
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
        // The Windows readers know only the plain windows, not per-model groups.
        .filter(|w| str_of(w, "group").is_none())
        .filter_map(|w| {
            Some(LimitWindow {
                key: str_of(w, "id")?.to_string(),
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
        Some("ok") | None => (Status::Ok, Some(NOW - 20)),
        Some(_) => (Status::Unavailable, None),
    };
    Usage {
        status,
        plan: str_of(cell, "plan").map(str::to_string),
        windows,
        updated,
    }
}

fn memory_from(window: &Value) -> Option<MemInfo> {
    let used = num_of(window, "used")?.clamp(0.0, 1.0);
    let total = sizes(str_of(window, "detail").unwrap_or(""))
        .get(1)
        .copied()
        .unwrap_or(DEFAULT_MEMORY);
    let used_bytes = (used * total as f64) as u64;
    // The fixture has no commit figure; commit mirrors the in-use share.
    Some(MemInfo {
        total,
        available: total.saturating_sub(used_bytes),
        commit_limit: total,
        commit_used: used_bytes,
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
            }
        }
        None => {
            let off = transfer.and_then(|w| str_of(w, "detail")) == Some("Off");
            send::Ring {
                fraction: None,
                active: false,
                problem: false,
                label: if off { "Off" } else { "Idle" }.to_string(),
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
    send_cell: Option<&'a Value>,
}

impl<'a> Scene<'a> {
    fn from_cells(cells: &[&'a Value]) -> Self {
        let mut machine = Machine {
            cpu: None,
            cores: 0,
            memory: None,
            drives: Vec::new(),
        };
        let mut usage = [Usage::waiting(), Usage::waiting()];
        let mut send_cell = None;
        for cell in cells {
            match str_of(cell, "id") {
                Some("claude") => usage[0] = usage_from(cell),
                Some("codex") => usage[1] = usage_from(cell),
                Some("system-send") => send_cell = Some(*cell),
                _ => {}
            }
            for window in arr(cell, "windows") {
                let id = str_of(window, "id").unwrap_or("");
                if id == "cpu" {
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
            send_cell,
        }
    }

    fn views(&self) -> Vec<CellView> {
        layout::views(Some(&self.machine), &self.usage, &ring_from(self.send_cell))
    }

    fn panel(&self, cell: Cell) -> Panel {
        match cell {
            Cell::Send => send_hover(self.send_cell),
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
    actions: Vec<Option<Action>>,
}

impl Rows {
    fn add(&mut self, row: Row, action: Option<Action>) {
        self.rows.push(row);
        self.actions.push(action);
    }

    fn note(&mut self, text: &str) {
        if !text.is_empty() {
            self.add(Row::Note(text.to_string()), None);
        }
    }

    fn button(&mut self, label: &str, action: Option<Action>) {
        self.add(pair(label, ""), action);
    }

    fn finish(self, title: &str) -> Panel {
        Panel {
            content: CardContent {
                title: title.to_string(),
                accessory: None,
                rows: self.rows,
            },
            actions: self.actions,
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
    let running = detail != "Off";
    let mut panel = Rows::default();
    match transfer.and_then(|w| num_of(w, "used").map(|f| (w, f as f32))) {
        Some((window, fraction)) => {
            panel.add(
                Row::Bar {
                    label: str_of(window, "label").unwrap_or("").to_string(),
                    value: percent_text(fraction),
                    fraction: Some(fraction),
                },
                None,
            );
            panel.note(detail);
            panel.button("Cancel", Some(Action::Cancel));
        }
        None => panel.note(detail),
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
    if running {
        if !devices.is_empty() {
            panel.add(pair("Paste clipboard", "Ctrl+V"), Some(Action::Paste));
            panel.note("Ctrl+V sends the clipboard \u{b7} drop files here");
        }
        panel.button("Look again", Some(Action::Refresh));
    }
    panel.finish("Nearby sharing")
}

/// A notch card of the send flow (`send::Prompt::panel`): the device list, a transfer, or a
/// plain note with buttons.
fn prompt_panel(fixture: &Value) -> Panel {
    let title = str_of(fixture, "title").unwrap_or("");
    let detail = str_of(fixture, "detail").unwrap_or("");
    let mut panel = Rows::default();
    match fixture.get("send") {
        Some(send) => match send.get("transfer") {
            Some(transfer) => transfer_rows(&mut panel, detail, transfer),
            None => list_rows(&mut panel, detail, send),
        },
        None => {
            panel.note(detail);
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
                panel.button(str_of(button, "label").unwrap_or(""), Some(action));
            }
        }
    }
    panel.finish(title)
}

fn list_rows(panel: &mut Rows, detail: &str, send: &Value) {
    panel.note(detail);
    let devices = arr(send, "rows");
    if devices.is_empty() {
        panel.note("Waiting for a device to appear\u{2026}");
    }
    for device in devices {
        let alias = str_of(device, "alias").unwrap_or("");
        panel.add(
            pair(alias, str_of(device, "model").unwrap_or("")),
            Some(Action::SendTo(alias.to_string())),
        );
    }
    let scanning = flag(send, "scanning");
    panel.button(
        if scanning {
            "Looking\u{2026}"
        } else {
            "Look again"
        },
        (!scanning).then_some(Action::Refresh),
    );
    panel.button("Cancel", Some(Action::Close));
}

fn transfer_rows(panel: &mut Rows, detail: &str, transfer: &Value) {
    let state = str_of(transfer, "state").unwrap_or("waiting");
    let fraction = num_of(transfer, "fraction").map(|f| f as f32);
    match state {
        "waiting" | "active" => {
            let value = match (state, fraction) {
                ("active", Some(f)) => percent_text(f),
                _ => String::new(),
            };
            panel.add(
                Row::Bar {
                    label: detail.to_string(),
                    value,
                    fraction,
                },
                None,
            );
        }
        "done" => panel.add(
            Row::Bar {
                label: detail.to_string(),
                value: "100%".to_string(),
                fraction: Some(1.0),
            },
            None,
        ),
        _ => panel.note(detail),
    }
    if flag(transfer, "canCancel") {
        panel.button("Cancel", Some(Action::Cancel));
    }
    panel.button("Close", Some(Action::Close));
}

// ---- building a view --------------------------------------------------------------------------

fn build(id: &str, area: &str, fixture: &Value, text: &mut TextPainter) -> Outcome {
    match str_of(fixture, "kind") {
        Some("ring") => ring(fixture, text),
        Some("tooltip") => tooltip(fixture, text),
        Some("card") if area == "disk-image-card" => Err(GAP_DISK_IMAGE),
        Some("card") => Ok(vec![(card_canvas(&prompt_panel(fixture), text), 0, 0)]),
        Some("update") => Err(GAP_UPDATE),
        Some("alert") => Err(GAP_ALERT),
        Some("menu") => Ok(vec![(render::render_menu(DPI, text), 0, 0)]),
        Some("notch") => notch(id, fixture, text),
        _ => Err("Unknown fixture kind"),
    }
}

fn card_canvas(panel: &Panel, text: &mut TextPainter) -> Canvas {
    let clickable: Vec<bool> = panel.actions.iter().map(Option::is_some).collect();
    render::render_card(&panel.content, &clickable, DPI, text)
}

/// One ring. Windows draws CPU and Memory as separate cells, so the System cell's ring is the
/// Memory ring.
fn ring(fixture: &Value, text: &mut TextPainter) -> Outcome {
    let cell = fixture.get("cell").ok_or("Fixture has no cell")?;
    if cell.get("block").is_some() {
        return Err(GAP_BLOCKED);
    }
    if !arr(cell, "sessions").is_empty() {
        return Err(GAP_SESSIONS);
    }
    let index = match str_of(cell, "id") {
        Some("system-cpu") => 1,
        Some("system-disks") => 2,
        Some("claude") => 3,
        Some("codex") => 4,
        Some("system-send") => 5,
        _ => return Err("Unknown ring cell"),
    };
    if index == 1 && pressure_unknown(cell) {
        return Err(GAP_PRESSURE);
    }
    let views = Scene::from_cells(&[cell]).views();
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
    let windows = arr(cell, "windows");
    let scene = Scene::from_cells(&[cell]);
    match id {
        "claude" | "codex" => {
            if str_of(cell, "status") == Some("access-denied") {
                return Err(GAP_DENIED);
            }
            if cell.get("block").is_some() {
                return Err(GAP_BLOCKED);
            }
            let which = if id == "claude" {
                Cell::Claude
            } else {
                Cell::Codex
            };
            Ok(vec![(card_canvas(&scene.panel(which), text), 0, 0)])
        }
        "system-cpu" => {
            if pressure_unknown(cell) {
                return Err(GAP_PRESSURE);
            }
            // The Mac System card is two cards on Windows: CPU and Memory.
            let cpu = card_canvas(&scene.panel(Cell::Cpu), text);
            let memory = card_canvas(&scene.panel(Cell::Memory), text);
            let left = cpu.width as i32 + GAP;
            Ok(vec![(cpu, 0, 0), (memory, left, 0)])
        }
        "system-disks" => {
            let health_gap = windows.iter().any(|w| {
                let id = str_of(w, "id").unwrap_or("");
                id == "health:install"
                    || id.ends_with(":last")
                    || str_of(w, "detail").is_some_and(|d| d.contains("n/a"))
            });
            if health_gap {
                return Err(GAP_HEALTH);
            }
            Ok(vec![(card_canvas(&scene.panel(Cell::Disk), text), 0, 0)])
        }
        "system-send" => {
            if windows
                .iter()
                .any(|w| str_of(w, "id") == Some("hint-network"))
            {
                return Err(GAP_NETWORK);
            }
            Ok(vec![(card_canvas(&scene.panel(Cell::Send), text), 0, 0)])
        }
        _ => Err("Unknown card cell"),
    }
}

/// The notch body, with the hover card hung below the hovered cell. The Windows notch is
/// always unfolded and sits on the top edge; the mixed-band view is drawn although its Mac
/// edge is right, because its point is the band colours.
fn notch(id: &str, fixture: &Value, text: &mut TextPainter) -> Outcome {
    if !flag(fixture, "expanded") {
        return Err(GAP_FOLDED);
    }
    if str_of(fixture, "edge") != Some("top") && id != "notch-expanded-warning-mix" {
        return Err(GAP_EDGE);
    }
    if fixture.get("badges").is_some() {
        return Err(GAP_BADGES);
    }
    let cells: Vec<&Value> = arr(fixture, "cells").iter().collect();
    let scene = Scene::from_cells(&cells);
    let body = render::render_panel(&scene.views(), DPI, text);
    let body_height = body.height as i32;
    let mut parts = vec![(body, 0, 0)];
    if let Some(hover) = str_of(fixture, "hover") {
        let cell = windows_cell(hover).ok_or("Unknown hover cell")?;
        let index = Cell::ALL
            .iter()
            .position(|c| *c == cell)
            .ok_or("Unknown hover cell")?;
        let card = card_canvas(&scene.panel(cell), text);
        let s = layout::scale(DPI);
        let centre = layout::cell_left(index, DPI) + layout::RING * s / 2.0;
        let x = ((centre - card.width as f32 / 2.0).round() as i32).max(0);
        let y = body_height + (layout::CARD_GAP * s).round() as i32;
        parts.push((card, x, y));
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
