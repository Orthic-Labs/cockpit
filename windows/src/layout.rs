//! Notch geometry and the view model for its five cells. Metrics are in DIPs (1/96 inch)
//! and scale with the monitor DPI; they are the Mac notch's design frame (a 44 pt ring in a
//! 117 px design ring, so one design px is `DESIGN` DIPs). Pure code.

use crate::send;
use crate::sensors::Machine;
use crate::usage::Usage;

pub const CELL_COUNT: usize = 5;
/// Index of the nearby-sharing cell in `Cell::ALL`.
pub const SEND_CELL: usize = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cell {
    /// The System ring: memory on the main arc, CPU on the thin inner one.
    Cpu,
    /// Kept for the card mapping; the notch draws memory inside the System ring.
    #[allow(dead_code)]
    Memory,
    Disk,
    Claude,
    Codex,
    /// Nearby sharing (the Send ring).
    Send,
}

impl Cell {
    pub const ALL: [Cell; CELL_COUNT] =
        [Cell::Claude, Cell::Codex, Cell::Cpu, Cell::Disk, Cell::Send];

    /// Hub section a click opens (same mapping as the Mac notch).
    pub fn section(self) -> &'static str {
        match self {
            Cell::Cpu | Cell::Memory => "monitor",
            Cell::Disk => "storage",
            Cell::Claude | Cell::Codex => "accounts",
            Cell::Send => send::SECTION,
        }
    }

    /// Key of the mark drawn inside the ring (see `glyphs::Glyph::from_key`).
    pub fn glyph(self) -> &'static str {
        match self {
            Cell::Cpu | Cell::Memory => "cpu",
            Cell::Disk => "disk",
            Cell::Claude => "claude",
            Cell::Codex => "openai",
            Cell::Send => send::GLYPH,
        }
    }
}

/// Screen edge the notch is welded to. Top is the Windows default (the Mac default is right);
/// left and right stack the rings vertically, like the Mac side notch.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Edge {
    #[default]
    Top,
    Bottom,
    Left,
    Right,
}

impl Edge {
    pub fn as_str(self) -> &'static str {
        match self {
            Edge::Top => "top",
            Edge::Bottom => "bottom",
            Edge::Left => "left",
            Edge::Right => "right",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "top" => Some(Edge::Top),
            "bottom" => Some(Edge::Bottom),
            "left" => Some(Edge::Left),
            "right" => Some(Edge::Right),
            _ => None,
        }
    }

    /// Left and right edges stack the rings top to bottom.
    pub fn is_vertical(self) -> bool {
        matches!(self, Edge::Left | Edge::Right)
    }
}

/// Notch-wide badges: a newer version is waiting, or an approval is pending.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Badges {
    pub update: bool,
    pub permissions: bool,
}

// Metrics in DIPs.
/// DIPs per Mac design px (the design frame measures the 44 pt ring as 117 px).
pub const DESIGN: f32 = 44.0 / 117.0;
pub const RING: f32 = 44.0;
pub const TRACK_STROKE: f32 = 15.5 * DESIGN;
pub const PROGRESS_STROKE: f32 = 8.0 * DESIGN;
pub const WEEKLY_RADIUS: f32 = 37.5 * DESIGN;
pub const WEEKLY_STROKE: f32 = 5.0 * DESIGN;
/// Space between neighbouring rings along the stack.
pub const SPACING: f32 = 26.0 * DESIGN;
/// Space from the end of the straight body to the first ring (and from the last ring).
pub const PAD_ALONG: f32 = 22.0 * DESIGN;
/// How far the ears' layout reserves at each end of the body (the Mac `curlRadius`).
pub const CURL: f32 = 103.0 * DESIGN;
/// Radius of the body's far corners.
pub const CORNER: f32 = 70.0 * DESIGN;
/// Depth of the open body, bezel band included.
pub const DEPTH: f32 = 161.0 * DESIGN;
/// The band the Mac shape keeps past the screen edge so no hairline of wallpaper shows.
pub const BAND: f32 = 2.0;
/// Length of the whole open shape along the screen edge, ears included.
pub const SHAPE_LENGTH: f32 =
    2.0 * CURL + 2.0 * PAD_ALONG + CELL_COUNT as f32 * RING + (CELL_COUNT - 1) as f32 * SPACING;
/// Settings orb arc: gap inside the ear, and stroke width.
pub const ORB_GAP: f32 = 27.0 * DESIGN;
pub const ORB_STROKE: f32 = 18.0 * DESIGN;
/// How far the orb arc's round cap reaches past the end of the shape.
pub const ORB_OVERHANG: f32 = ORB_STROKE / 2.0;
/// Gap between the notch and a hover card.
pub const CARD_GAP: f32 = 6.0;
/// Folded pill: depth (band included) and length along the edge.
pub const PILL_DEPTH: f32 = 26.0 * DESIGN;
pub const PILL_LONG: f32 = 210.0 * DESIGN;
pub const BADGE_PERMISSIONS: u32 = 0xFFB340;
pub const BADGE_UPDATE: u32 = 0xA51D24;

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

/// Canvas depth (across the notch) in device pixels once the bezel band is cropped away.
fn open_depth(dpi: u32) -> i32 {
    ((DEPTH - BAND) * scale(dpi)).round() as i32
}

/// Notch body size in device pixels on a top or bottom edge: the whole shape, ears
/// included, plus the orb arc's cap.
pub fn body_size(dpi: u32) -> (i32, i32) {
    let s = scale(dpi);
    let along = ((SHAPE_LENGTH + ORB_OVERHANG) * s).round() as i32;
    (along, open_depth(dpi))
}

/// Canvas position of the point `along` the stack and `depth` in from the screen edge,
/// both in device pixels, on a canvas `across` pixels deep.
pub fn to_canvas(edge: Edge, along: f32, depth: f32, across: f32) -> (f32, f32) {
    match edge {
        Edge::Top => (along, depth),
        Edge::Bottom => (along, across - depth),
        Edge::Left => (depth, along),
        Edge::Right => (across - depth, along),
    }
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
    /// Sharing cannot work: the glyph is drawn in the warning colour.
    pub problem: bool,
    /// A limit is spent: both arcs read as exhausted and the glyph is dimmed.
    pub blocked: bool,
    /// Ring colour decided by the reading itself rather than by the used share (Memory).
    /// `None` colours by the share's band.
    pub band: Option<u32>,
}

fn percent(fraction: f32) -> u8 {
    (fraction.clamp(0.0, 1.0) * 100.0).round() as u8
}

/// Memory ring colour. The Mac colours the ring by the kernel's memory-pressure state; Windows
/// has no such state, so it is derived from what the OS exposes: physical memory still
/// available and the commit charge against the commit limit. Under 12.5% available or 85%
/// committed is "watch"; under 5% available or 95% committed is "critical". `None` (no
/// reading) leaves the colour to the used share, like the Mac's unknown pressure.
pub fn memory_band(machine: Option<&Machine>) -> Option<u32> {
    let memory = machine?.memory?;
    if memory.total == 0 {
        return None;
    }
    let free = memory.available as f64 / memory.total as f64;
    let commit = if memory.commit_limit > 0 {
        memory.commit_used as f64 / memory.commit_limit as f64
    } else {
        0.0
    };
    Some(if free < 0.05 || commit >= 0.95 {
        BAND_CRITICAL
    } else if free < 0.125 || commit >= 0.85 {
        BAND_WATCH
    } else {
        BAND_AMPLE
    })
}

/// Builds the five cell views (Claude, Codex, System, Disks, Send). Unknown readings are
/// `None` and draw as a dimmed, empty ring.
pub fn views(machine: Option<&Machine>, usage: &[Usage; 2], ring: &send::Ring) -> Vec<CellView> {
    let system = |cell: Cell, main: Option<f32>, inner: Option<f32>| CellView {
        glyph: cell.glyph(),
        main: main.map(percent),
        inner: inner.map(percent),
        stale: main.is_none(),
        problem: false,
        blocked: false,
        band: None,
    };
    let ai = |cell: Cell, usage: &Usage| {
        let main = usage.headline().map(|w| percent(w.fraction));
        CellView {
            glyph: cell.glyph(),
            main,
            inner: usage.weekly().map(|w| percent(w.fraction)),
            stale: usage.is_stale() || main.is_none(),
            problem: false,
            blocked: usage.block.is_some(),
            band: None,
        }
    };
    // System: memory leads, CPU is the thin inner ring (CPU alone when memory is unknown).
    let memory = machine.and_then(Machine::memory_fraction);
    let cpu = machine.and_then(|m| m.cpu);
    let mut system_cell = match memory {
        Some(_) => system(Cell::Cpu, memory, cpu),
        None => system(Cell::Cpu, cpu, None),
    };
    if memory.is_some() {
        system_cell.band = memory_band(machine);
    }
    // Disks: an external (non-system) drive leads and the system drive is the inner ring;
    // with no external drive the system drive leads alone.
    let startup = machine.and_then(Machine::disk_fraction);
    let external = machine
        .and_then(|m| m.drives.iter().find(|d| !d.system))
        .map(|d| d.used_fraction());
    let disks = match external {
        Some(_) => system(Cell::Disk, external, startup),
        None => system(Cell::Disk, startup, None),
    };
    vec![
        ai(Cell::Claude, &usage[0]),
        ai(Cell::Codex, &usage[1]),
        system_cell,
        disks,
        CellView {
            glyph: Cell::Send.glyph(),
            main: ring.fraction.filter(|_| ring.active).map(percent),
            inner: None,
            stale: false,
            problem: ring.problem,
            blocked: false,
            band: None,
        },
    ]
}

// ---- orientation: the notch on any screen edge ----------------------------------------------

/// Notch body size in device pixels on `edge`. Top and bottom keep the horizontal row; left
/// and right stack the five rings downwards.
pub fn body_size_for(edge: Edge, dpi: u32) -> (i32, i32) {
    let (along, depth) = body_size(dpi);
    if edge.is_vertical() {
        (depth, along)
    } else {
        (along, depth)
    }
}

/// Folded pill size in device pixels on `edge`.
pub fn pill_size_for(edge: Edge, dpi: u32) -> (i32, i32) {
    let s = scale(dpi);
    let (thick, long) = (
        ((PILL_DEPTH - BAND) * s).round() as i32,
        (PILL_LONG * s).round() as i32,
    );
    if edge.is_vertical() {
        (thick, long)
    } else {
        (long, thick)
    }
}

/// Window size for the notch: the pill while folded, the body when open.
pub fn panel_size(edge: Edge, folded: bool, dpi: u32) -> (i32, i32) {
    if folded {
        pill_size_for(edge, dpi)
    } else {
        body_size_for(edge, dpi)
    }
}

/// Centre of cell `index`'s ring in notch-local device pixels.
pub fn ring_center(edge: Edge, index: usize, dpi: u32) -> (f32, f32) {
    let s = scale(dpi);
    let along = (CURL + PAD_ALONG + RING / 2.0 + index as f32 * (RING + SPACING)) * s;
    let depth = (DEPTH / 2.0 - BAND) * s;
    to_canvas(edge, along, depth, open_depth(dpi) as f32)
}

/// Cell under a point in notch-local device pixels on `edge`.
pub fn cell_at_for(edge: Edge, x: i32, y: i32, dpi: u32) -> Option<usize> {
    let (width, height) = body_size_for(edge, dpi);
    if x < 0 || y < 0 || x >= width || y >= height {
        return None;
    }
    let s = scale(dpi);
    let along = (if edge.is_vertical() { y } else { x }) as f32;
    let first_edge = (CURL + PAD_ALONG - SPACING / 2.0) * s;
    let index = ((along - first_edge) / ((RING + SPACING) * s)).floor();
    Some((index.max(0.0) as usize).min(CELL_COUNT - 1))
}

/// Top-left of a notch of `size` docked to `edge` of `monitor` (`left, top, right, bottom`),
/// its centre `along` per mille of the way along the edge, kept inside the monitor.
pub fn origin_for(
    edge: Edge,
    monitor: (i32, i32, i32, i32),
    size: (i32, i32),
    along: u16,
) -> (i32, i32) {
    let (ml, mt, mr, mb) = monitor;
    let (w, h) = size;
    match edge {
        Edge::Top => (left_for_along(ml, mr - ml, w, along), mt),
        Edge::Bottom => (left_for_along(ml, mr - ml, w, along), (mb - h).max(mt)),
        Edge::Left => (ml, left_for_along(mt, mb - mt, h, along)),
        Edge::Right => ((mr - w).max(ml), left_for_along(mt, mb - mt, h, along)),
    }
}

/// Inverse of `origin_for` along the edge: the per-mille position of the notch's centre for a
/// notch whose top-left is `origin`.
pub fn along_for_origin(
    edge: Edge,
    monitor: (i32, i32, i32, i32),
    size: (i32, i32),
    origin: (i32, i32),
) -> u16 {
    let (ml, mt, mr, mb) = monitor;
    if edge.is_vertical() {
        along_for_left(mt, mb - mt, size.1, origin.1)
    } else {
        along_for_left(ml, mr - ml, size.0, origin.0)
    }
}

/// Edge of `monitor` nearest the cursor, staying on `current` unless another edge is clearly
/// closer (a margin of 16 DIPs stops the notch flickering between edges near a corner).
pub fn edge_for_cursor(
    current: Edge,
    monitor: (i32, i32, i32, i32),
    cursor: (i32, i32),
    dpi: u32,
) -> Edge {
    let (ml, mt, mr, mb) = monitor;
    let distance = |edge: Edge| -> i64 {
        i64::from(match edge {
            Edge::Top => cursor.1 - mt,
            Edge::Bottom => mb - 1 - cursor.1,
            Edge::Left => cursor.0 - ml,
            Edge::Right => mr - 1 - cursor.0,
        })
    };
    let margin = (16.0 * scale(dpi)) as i64;
    let mut best = current;
    for edge in [Edge::Top, Edge::Bottom, Edge::Left, Edge::Right] {
        if distance(edge) + margin < distance(best) {
            best = edge;
        }
    }
    best
}

/// Top-left of a card of `card` size beside a notch (`left, top, right, bottom`) on `edge`,
/// centred on `centre` (screen coordinate of the cell along the edge) and kept inside the
/// monitor. The card opens away from the screen edge.
pub fn card_origin(
    edge: Edge,
    notch: (i32, i32, i32, i32),
    centre: i32,
    card: (i32, i32),
    gap: i32,
    monitor: (i32, i32, i32, i32),
) -> (i32, i32) {
    let (nl, nt, nr, nb) = notch;
    let (cw, ch) = card;
    let (ml, mt, mr, mb) = monitor;
    let across = |value: i32, size: i32, lo: i32, hi: i32| value.clamp(lo, (hi - size).max(lo));
    match edge {
        Edge::Top => (across(centre - cw / 2, cw, ml, mr), nb + gap),
        Edge::Bottom => (across(centre - cw / 2, cw, ml, mr), nt - gap - ch),
        Edge::Left => (nr + gap, across(centre - ch / 2, ch, mt, mb)),
        Edge::Right => (nl - gap - cw, across(centre - ch / 2, ch, mt, mb)),
    }
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
