//! Notch geometry and the view model for its five cells. Metrics are in DIPs (1/96 inch)
//! and scale with the monitor DPI; they follow the Mac notch's design frame (44 pt ring,
//! 5.8 pt track, 3 pt arc, thin inner ring for the weekly window). Pure code.

use crate::sensors::Machine;
use crate::usage::Usage;

pub const CELL_COUNT: usize = 5;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cell {
    Cpu,
    Memory,
    Disk,
    Claude,
    Codex,
}

impl Cell {
    pub const ALL: [Cell; CELL_COUNT] = [Cell::Cpu, Cell::Memory, Cell::Disk, Cell::Claude, Cell::Codex];

    /// Hub section a click opens (same mapping as the Mac notch).
    pub fn section(self) -> &'static str {
        match self {
            Cell::Cpu | Cell::Memory => "monitor",
            Cell::Disk => "storage",
            Cell::Claude | Cell::Codex => "accounts",
        }
    }

    /// Short text mark drawn inside the ring.
    pub fn glyph(self) -> &'static str {
        match self {
            Cell::Cpu => "CPU",
            Cell::Memory => "RAM",
            Cell::Disk => "DSK",
            Cell::Claude => "Cl",
            Cell::Codex => "Cx",
        }
    }
}

// Metrics in DIPs.
pub const RING: f32 = 44.0;
pub const TRACK_STROKE: f32 = 5.8;
pub const PROGRESS_STROKE: f32 = 3.0;
pub const WEEKLY_RADIUS: f32 = 14.1;
pub const WEEKLY_STROKE: f32 = 1.9;
pub const PAD_X: f32 = 16.0;
pub const PAD_TOP: f32 = 8.3;
pub const PAD_BOTTOM: f32 = 8.3;
pub const SPACING: f32 = 10.0;
pub const LABEL_GAP: f32 = 5.0;
pub const LABEL_HEIGHT: f32 = 13.0;
pub const BODY_CORNER: f32 = 26.0;
/// Gap between the notch and a hover card.
pub const CARD_GAP: f32 = 6.0;

pub const BAND_AMPLE: u32 = 0x2E6B4A;
pub const BAND_WATCH: u32 = 0xC2570F;
pub const BAND_CRITICAL: u32 = 0xA51D24;
pub const INK_PRIMARY: u32 = 0xFFFFFF;
pub const INK_SECONDARY: u32 = 0x808080;
pub const RING_TRACK_ALPHA: f32 = 0.188;
const WATCH_LIMIT: f32 = 0.70;
const CRITICAL_LIMIT: f32 = 0.90;

/// Device pixels per DIP for `dpi`.
pub fn scale(dpi: u32) -> f32 {
    dpi.clamp(48, 480) as f32 / 96.0
}

/// Ring colour for a used fraction: the Mac notch's three bands.
pub fn band_color(fraction: f32) -> u32 {
    if fraction < WATCH_LIMIT {
        BAND_AMPLE
    } else if fraction < CRITICAL_LIMIT {
        BAND_WATCH
    } else {
        BAND_CRITICAL
    }
}

/// Notch body size in device pixels.
pub fn body_size(dpi: u32) -> (i32, i32) {
    let s = scale(dpi);
    let n = CELL_COUNT as f32;
    let width = 2.0 * PAD_X + n * RING + (n - 1.0) * SPACING;
    let height = PAD_TOP + RING + LABEL_GAP + LABEL_HEIGHT + PAD_BOTTOM;
    ((width * s).round() as i32, (height * s).round() as i32)
}

/// Left edge of cell `index` in device pixels.
pub fn cell_left(index: usize, dpi: u32) -> f32 {
    (PAD_X + index as f32 * (RING + SPACING)) * scale(dpi)
}

/// Cell under a point in notch-local device pixels, or `None` outside the body.
pub fn cell_at(x: i32, y: i32, dpi: u32) -> Option<usize> {
    let (width, height) = body_size(dpi);
    if x < 0 || y < 0 || x >= width || y >= height {
        return None;
    }
    let s = scale(dpi);
    let first_edge = (PAD_X - SPACING / 2.0) * s;
    let index = ((x as f32 - first_edge) / ((RING + SPACING) * s)).floor();
    Some((index.max(0.0) as usize).min(CELL_COUNT - 1))
}

/// What one cell shows, quantised to whole percents so identical readings compare equal and
/// a redraw happens only when something visible changed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CellView {
    pub glyph: &'static str,
    /// Main ring, percent used.
    pub main: Option<u8>,
    /// Thin inner ring (weekly window), percent used.
    pub inner: Option<u8>,
    /// Latest poll failed; the reading is dimmed.
    pub stale: bool,
    /// Text under the ring: `42%` or `--`.
    pub label: String,
}

fn percent(fraction: f32) -> u8 {
    (fraction.clamp(0.0, 1.0) * 100.0).round() as u8
}

fn label(value: Option<u8>) -> String {
    value.map_or_else(|| "--".to_string(), |p| format!("{p}%"))
}

/// Builds the five cell views. Unknown readings are `None` and show as `--`.
pub fn views(machine: Option<&Machine>, usage: &[Usage; 2]) -> Vec<CellView> {
    let system = |cell: Cell, fraction: Option<f32>| {
        let main = fraction.map(percent);
        CellView {
            glyph: cell.glyph(),
            main,
            inner: None,
            stale: false,
            label: label(main),
        }
    };
    let ai = |cell: Cell, usage: &Usage| {
        let main = usage.headline().map(|w| percent(w.fraction));
        CellView {
            glyph: cell.glyph(),
            main,
            inner: usage.weekly().map(|w| percent(w.fraction)),
            stale: usage.is_stale(),
            label: label(main),
        }
    };
    vec![
        system(Cell::Cpu, machine.and_then(|m| m.cpu)),
        system(Cell::Memory, machine.and_then(Machine::memory_fraction)),
        system(Cell::Disk, machine.and_then(Machine::disk_fraction)),
        ai(Cell::Claude, &usage[0]),
        ai(Cell::Codex, &usage[1]),
    ]
}

/// Left edge of the notch for a placement along a monitor's top edge. `along` is the
/// per-mille position of the notch's centre across the monitor width.
pub fn left_for_along(monitor_left: i32, monitor_width: i32, notch_width: i32, along: u16) -> i32 {
    let centre = monitor_left as i64 + monitor_width as i64 * i64::from(along.min(1000)) / 1000;
    let left = centre - i64::from(notch_width) / 2;
    let max_left = (monitor_left as i64 + monitor_width as i64 - i64::from(notch_width))
        .max(monitor_left as i64);
    left.clamp(monitor_left as i64, max_left) as i32
}

/// Inverse of `left_for_along`: where the notch centre sits across the monitor, per mille.
pub fn along_for_left(monitor_left: i32, monitor_width: i32, notch_width: i32, left: i32) -> u16 {
    if monitor_width <= 0 {
        return 500;
    }
    let centre = i64::from(left) + i64::from(notch_width) / 2 - i64::from(monitor_left);
    (centre * 1000 / i64::from(monitor_width)).clamp(0, 1000) as u16
}
