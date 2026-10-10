//! Draws the notch body, the hover card and the Quit menu into canvases (premultiplied BGRA)
//! using the software rasteriser and GDI text. Dark solid style, like the Mac notch's solid
//! surface. Pure of window state: callers pass data and get pixels.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use crate::canvas::{Canvas, Mask};
use crate::card::{Button, CardContent, Head, Hit, Lead, Live, Mark, Row, Tone};
use crate::fileicon;
use crate::glyphs::{self, Glyph, Symbol, Tile};
use crate::layout::{
    self, Badges, CellView, Edge, Handle, INK_PRIMARY, INK_SECONDARY, PROGRESS_STROKE, RING,
    RING_TRACK_ALPHA, TRACK_STROKE, WEEKLY_RADIUS, WEEKLY_STROKE,
};
use crate::surface::TextPainter;

const STALE_DIM: f32 = 0.45;
/// Glyph opacity while a limit is spent (the Mac dims it to 0.35).
const BLOCKED_GLYPH: f32 = 0.35;
/// Permission dot on the folded pill (the Mac's four-point amber dot), in DIPs.
const PILL_DOT: f32 = 4.0;
const BORDER: u32 = 0x2A2A2A;

/// The hub's Colour settings, as the Mac's notch reads them from its preferences: the colour of
/// the ample state (`accentColor`) and where the bands turn orange and red (`watchLimit`,
/// `criticalLimit`, in thousandths). Set by the settings bridge at start and on every change.
static ACCENT: AtomicU32 = AtomicU32::new(layout::BAND_AMPLE);
static WATCH_MILLI: AtomicU32 = AtomicU32::new(700);
static CRITICAL_MILLI: AtomicU32 = AtomicU32::new(900);

/// Applies the Colour settings to everything drawn from now on.
pub fn set_appearance(accent: u32, watch_milli: u16, critical_milli: u16) {
    ACCENT.store(accent, Ordering::Relaxed);
    WATCH_MILLI.store(u32::from(watch_milli), Ordering::Relaxed);
    CRITICAL_MILLI.store(u32::from(critical_milli), Ordering::Relaxed);
}

/// The ample colour with the chosen accent standing in for it. The warning and critical colours
/// never change (the Mac's `UsageBand.color(accent:)`).
fn tint(color: u32) -> u32 {
    if color == layout::BAND_AMPLE {
        ACCENT.load(Ordering::Relaxed)
    } else {
        color
    }
}

/// Ring and bar colour for a used fraction: the Mac's three bands at the chosen limits, the
/// ample one in the accent.
fn band_color(fraction: f32) -> u32 {
    let watch_milli = WATCH_MILLI.load(Ordering::Relaxed);
    let critical_milli = CRITICAL_MILLI.load(Ordering::Relaxed);
    let watch = watch_milli as f32 / 1000.0;
    let critical = critical_milli as f32 / 1000.0;
    if fraction < watch {
        tint(layout::BAND_AMPLE)
    } else if fraction < critical {
        layout::BAND_WATCH
    } else {
        layout::BAND_CRITICAL
    }
}

const MENU_PAD: f32 = 14.0;
const MENU_RADIUS: f32 = 12.0;
const MENU_SIZE: f32 = 12.0;
const MENU_LINE: f32 = 18.0;

pub const MENU_WIDTH: f32 = 112.0;
pub const MENU_HEIGHT: f32 = 36.0;

fn px(dip: f32, scale: f32) -> i32 {
    (dip * scale).round() as i32
}

#[allow(clippy::too_many_arguments)]
fn draw_text(
    canvas: &mut Canvas,
    text: &mut TextPainter,
    value: &str,
    (x, y): (i32, i32),
    (size_dip, bold): (f32, bool),
    scale: f32,
    color: u32,
    alpha: f32,
) -> i32 {
    match text.render(value, px(size_dip, scale), bold) {
        Some(Mask { width: 0, .. }) | None => 0,
        Some(mask) => {
            canvas.draw_mask(&mask, x, y, color, alpha);
            mask.width as i32
        }
    }
}

/// Width in pixels a string would occupy.
fn text_width(text: &mut TextPainter, value: &str, size_dip: f32, bold: bool, scale: f32) -> i32 {
    text.render(value, px(size_dip, scale), bold)
        .map_or(0, |mask| mask.width as i32)
}

/// Where points of the notch's own frame (along the screen edge, depth in from it) land on
/// the canvas. `across` is the canvas depth with the bezel band cropped away; `pad` is the
/// band the view shots keep (the Mac's offscreen renders show it as a thin strip).
#[derive(Clone, Copy)]
struct Frame {
    edge: Edge,
    across: f32,
    pad: f32,
}

impl Frame {
    fn at(self, along: f32, depth: f32) -> (f32, f32) {
        self.shift(layout::to_canvas(self.edge, along, depth, self.across))
    }

    /// A point already in cropped-canvas pixels, moved for the band.
    fn shift(self, (x, y): (f32, f32)) -> (f32, f32) {
        match self.edge {
            Edge::Top => (x, y + self.pad),
            Edge::Left => (x + self.pad, y),
            Edge::Bottom | Edge::Right => (x, y),
        }
    }
}

/// The Mac's `fluidTurn` walk, a quarter turn whose bend ramps in from nothing and back out
/// (ramp 0.5), as `(u, v, heading)` with `u` and `v` normalised to 0..1.
fn fluid_walk() -> Vec<(f32, f32, f32)> {
    const STEPS: usize = 96;
    let bend = std::f32::consts::FRAC_PI_2 / 0.5;
    let (mut heading, mut u, mut v) = (0.0f32, 0.0f32, 0.0f32);
    let mut walk = vec![(0.0, 0.0, 0.0)];
    for step in 0..STEPS {
        let t = (step as f32 + 0.5) / STEPS as f32;
        let share = if t < 0.5 { t / 0.5 } else { (1.0 - t) / 0.5 };
        heading += bend * share / STEPS as f32;
        u += heading.cos() / STEPS as f32;
        v += heading.sin() / STEPS as f32;
        walk.push((u, v, heading));
    }
    let (end_u, end_v, _) = walk[walk.len() - 1];
    walk.into_iter()
        .map(|(u, v, heading)| (u / end_u, v / end_v, heading))
        .collect()
}

/// A fluid turn from `from`, leaving along `leaving` and arriving along `arriving`, reaching
/// `along` and `across` pixels on those axes.
fn flare(
    from: (f32, f32),
    (leaving, arriving): ((f32, f32), (f32, f32)),
    (along, across): (f32, f32),
) -> Vec<(f32, f32)> {
    fluid_walk()
        .into_iter()
        .map(|(u, v, _)| {
            (
                from.0 + leaving.0 * u * along + arriving.0 * v * across,
                from.1 + leaving.1 * u * along + arriving.1 * v * across,
            )
        })
        .collect()
}

/// Cubic Bezier through `p`, appended without its first point.
fn curve_to(out: &mut Vec<(f32, f32)>, p: [(f32, f32); 4]) {
    const STEPS: usize = 12;
    for step in 1..=STEPS {
        let t = step as f32 / STEPS as f32;
        let m = 1.0 - t;
        let w = [m * m * m, 3.0 * m * m * t, 3.0 * m * t * t, t * t * t];
        out.push((
            (0..4).map(|i| w[i] * p[i].0).sum(),
            (0..4).map(|i| w[i] * p[i].1).sum(),
        ));
    }
}

/// Outline of the notch (the Mac `SideNotchShape`) as `(along, depth)` pixels, `depth`
/// counted in from the top of the shape, which includes the bezel band. Ears flare out to
/// the screen edge at both ends; the far corners are circular.
fn shape_outline(length: f32, depth: f32, s: f32) -> Vec<(f32, f32)> {
    const REACH: f32 = 0.5523;
    let wanted = (layout::CORNER * s).min(depth / 2.0);
    let band = (layout::BAND * s).min(depth - wanted).max(0.0);
    let curl = (layout::CURL * s).min(depth - wanted - band).max(0.0);
    let corner = wanted.min((length - 2.0 * curl) / 2.0).max(0.0);
    let mut points = vec![(0.0, 0.0), (0.0, band)];
    if curl > 0.001 {
        points.extend(flare((0.0, band), ((1.0, 0.0), (0.0, 1.0)), (curl, curl)));
    }
    let (near, far) = (curl + corner, length - curl - corner);
    points.push((curl, depth - corner));
    curve_to(
        &mut points,
        [
            (curl, depth - corner),
            (curl, depth - corner + REACH * corner),
            (near - REACH * corner, depth),
            (near, depth),
        ],
    );
    points.push((far, depth));
    curve_to(
        &mut points,
        [
            (far, depth),
            (far + REACH * corner, depth),
            (length - curl, depth - corner + REACH * corner),
            (length - curl, depth - corner),
        ],
    );
    points.push((length - curl, band + curl));
    if curl > 0.001 {
        points.extend(flare(
            (length - curl, band + curl),
            ((0.0, -1.0), (1.0, 0.0)),
            (curl, curl),
        ));
    }
    points.push((length, 0.0));
    points
}

/// The notch on `edge`: the open body with its five rings, ears and settings arc, or the
/// folded pill. A permissions badge draws as an amber dot (the pill's centre, or the middle
/// of the settings arc); otherwise a pending update draws as a red one on the arc. With the
/// pointer on the settings handle (`handle`) the arc fills in as the settings button with a
/// gear, and the move grip comes out beside it.
pub fn render_notch(
    views: &[CellView],
    edge: Edge,
    folded: bool,
    badges: Badges,
    dpi: u32,
    text: &mut TextPainter,
    (handle, press): (Option<Handle>, Option<Press>),
) -> Canvas {
    draw_notch(
        views,
        edge,
        folded,
        badges,
        dpi,
        text,
        (false, handle, press),
    )
}

/// The part of the notch the primary button is held down on: it reads pressed until release.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Press {
    /// A ring cell (its index in the shown cells).
    Cell(usize),
    /// The settings gear.
    Orb,
    /// The move grip.
    Grip,
}

/// A pressed ring cell reads dimmer, as the Mac's pressed orb does.
const CELL_PRESS_DIM: f32 = 0.55;
/// White laid over the gear disc or the grip plate while pressed.
const HANDLE_PRESS_ALPHA: f32 = 0.2;

/// `render_notch` with the bezel band kept: the Mac's view renders show the 2 pt the shape
/// pushes past the screen edge as a thin strip along the welded side.
pub fn render_notch_shot(
    views: &[CellView],
    edge: Edge,
    folded: bool,
    badges: Badges,
    dpi: u32,
    text: &mut TextPainter,
) -> Canvas {
    draw_notch(views, edge, folded, badges, dpi, text, (true, None, None))
}

/// `keep_band` and the handle part shown out: the live notch keeps no band and carries the
/// pointer zones (see `draw_handle_zone`); the view shots keep the band and draw neither.
fn draw_notch(
    views: &[CellView],
    edge: Edge,
    folded: bool,
    badges: Badges,
    dpi: u32,
    _text: &mut TextPainter,
    (keep_band, handle, press): (bool, Option<Handle>, Option<Press>),
) -> Canvas {
    let s = layout::scale(dpi);
    let (width, height) = layout::panel_size(edge, folded, dpi);
    let pad = if keep_band {
        (layout::BAND * s).round() as i32
    } else {
        0
    };
    let vertical = edge.is_vertical();
    let (canvas_width, canvas_height) = if vertical {
        (width + pad, height)
    } else {
        (width, height + pad)
    };
    let mut canvas = Canvas::new(canvas_width as usize, canvas_height as usize);
    let frame = Frame {
        edge,
        across: (if vertical { width } else { height }) as f32,
        pad: pad as f32,
    };
    let band = layout::BAND * s;
    let (depth, length) = if folded {
        (layout::PILL_DEPTH * s, layout::PILL_LONG * s)
    } else {
        (layout::DEPTH * s, layout::shape_length() * s)
    };
    let outline: Vec<(f32, f32)> = shape_outline(length, depth, s)
        .into_iter()
        .map(|(along, down)| frame.at(along, down - band))
        .collect();
    if !folded && !keep_band {
        draw_handle_zone(&mut canvas, frame, s, handle.is_some());
    }
    canvas.fill_polygon(&[outline], 0x000000, 1.0);
    if folded {
        if badges.permissions {
            dot(
                &mut canvas,
                frame.at(length / 2.0, depth / 2.0 - band),
                PILL_DOT / 2.0 * s,
                layout::BADGE_PERMISSIONS,
            );
        }
        return canvas;
    }
    // The settings orb's resting arc: the ear's own curve pushed one gap into its pocket, at
    // the trailing end, in the notch's black.
    let span = (layout::DEPTH - layout::CORNER) * s;
    let gap = layout::ORB_GAP * s;
    let (centre_along, centre_depth) = (length, layout::CURL * s);
    let arc: Vec<(f32, f32)> = fluid_walk()
        .into_iter()
        .map(|(u, v, heading)| {
            frame.at(
                centre_along - span * u + gap * heading.sin(),
                centre_depth - span * (1.0 - v) + gap * heading.cos(),
            )
        })
        .collect();
    // Out, the button replaces the arc: it is the same circle filled in.
    if handle.is_none() {
        canvas.stroke_polyline(&arc, layout::ORB_STROKE * s, false, 0x000000, 1.0);
    }
    for (index, view) in views.iter().enumerate() {
        let centre = frame.shift(layout::ring_center(edge, index, dpi));
        draw_cell(
            &mut canvas,
            view,
            centre,
            s,
            press == Some(Press::Cell(index)),
        );
    }
    if let Some(part) = handle {
        draw_handle(&mut canvas, frame, s, (part, press));
    }
    // One badge dot on the middle of the arc: permissions (amber) wins over update (red).
    let reach = (layout::CURL - layout::ORB_GAP) * s * std::f32::consts::FRAC_1_SQRT_2;
    let mid = frame.at(centre_along - reach, centre_depth - reach);
    if badges.permissions {
        dot(&mut canvas, mid, 1.5 * s, layout::BADGE_PERMISSIONS);
    } else if badges.update {
        dot(&mut canvas, mid, 26.0 * layout::DESIGN / 2.0 * s, 0x000000);
        dot(
            &mut canvas,
            mid,
            (13.0 * layout::DESIGN - 1.5) * s,
            layout::BADGE_UPDATE,
        );
    }
    canvas
}

/// Fills the rectangle spanning `from` to `to` (`(along, depth)` points of the notch's frame),
/// its corners rounded by `radius`.
fn fill_frame_rect(
    canvas: &mut Canvas,
    frame: Frame,
    from: (f32, f32),
    to: (f32, f32),
    radius: f32,
    (colour, alpha): (u32, f32),
) {
    let (x0, y0) = frame.at(from.0, from.1);
    let (x1, y1) = frame.at(to.0, to.1);
    let (left, top) = (x0.min(x1), y0.min(y1));
    canvas.fill_round_rect(
        left,
        top,
        x0.max(x1) - left,
        y0.max(y1) - top,
        [radius; 4],
        colour,
        alpha,
    );
}

/// Pixels the pointer can land on beyond the black. A layered window passes the pointer
/// through its fully transparent pixels, so the settings button's zone (and the grip's while
/// it is out) carries one 255th of black, which nothing can see. Drawn first: the notch's
/// own black goes over it.
fn draw_handle_zone(canvas: &mut Canvas, frame: Frame, s: f32, grip_out: bool) {
    const PLATE: (u32, f32) = (0x000000, 1.0 / 255.0);
    let (along, depth) = (layout::shape_length() * s, layout::CURL * s);
    let zone = layout::ORB_ZONE * s;
    let (first, last) = ((along - zone, depth - zone), (along + zone, depth + zone));
    fill_frame_rect(canvas, frame, first, last, zone, PLATE);
    if grip_out {
        let half = (layout::GRIP_PITCH + layout::GRIP_DOT / 2.0 + 3.0) * s;
        let start = along + (layout::ORB_RADIUS + layout::GRIP_GAP / 2.0) * s;
        let end = along + layout::ORB_OVERHANG * s;
        let (first, last) = ((start, depth - half), (end, depth + half));
        fill_frame_rect(canvas, frame, first, last, 0.0, PLATE);
    }
}

/// The settings button (the disc the resting arc fills into, with a gear) and the move grip
/// beside it on a black plate. The grip is out whenever the pointer is on either; `part` is
/// the one it is on, drawn brighter.
fn draw_handle(canvas: &mut Canvas, frame: Frame, s: f32, (part, press): (Handle, Option<Press>)) {
    let (along, depth) = (layout::shape_length() * s, layout::CURL * s);
    let centre = frame.at(along, depth);
    let radius = layout::ORB_RADIUS * s;
    // The plate runs from the disc's middle out past the last dots; the disc covers its end.
    let half = (layout::GRIP_PITCH + layout::GRIP_DOT / 2.0 + 1.5) * s;
    let end = along + (layout::GRIP_REACH + layout::GRIP_LENGTH / 2.0 + 2.0) * s;
    let (first, last) = ((along, depth - half), (end, depth + half));
    fill_frame_rect(canvas, frame, first, last, half, (0x000000, 1.0));
    dot(canvas, centre, radius, 0x000000);
    if press == Some(Press::Orb) {
        canvas.fill_round_rect(
            centre.0 - radius,
            centre.1 - radius,
            2.0 * radius,
            2.0 * radius,
            [radius; 4],
            0xFFFFFF,
            HANDLE_PRESS_ALPHA,
        );
    }
    let orb_ink = if part == Handle::Orb { 1.0 } else { 0.7 };
    draw_gear(canvas, centre, radius * 0.52, INK_PRIMARY, orb_ink);
    if press == Some(Press::Grip) {
        fill_frame_rect(
            canvas,
            frame,
            (along + radius, depth - half),
            (end, depth + half),
            half,
            (0xFFFFFF, HANDLE_PRESS_ALPHA),
        );
    }
    let (grip_ink, growth) = if press == Some(Press::Grip) {
        (1.0, 1.45)
    } else if part == Handle::Grip {
        (0.95, 1.3)
    } else {
        (0.55, 1.0)
    };
    let dot_radius = layout::GRIP_DOT / 2.0 * s * growth;
    let grip_along = along + layout::GRIP_REACH * s;
    for line in [-0.5f32, 0.5] {
        for row in [-1.0f32, 0.0, 1.0] {
            let (x, y) = frame.at(
                grip_along + line * layout::GRIP_PITCH * s,
                depth + row * layout::GRIP_PITCH * s,
            );
            canvas.fill_round_rect(
                x - dot_radius,
                y - dot_radius,
                2.0 * dot_radius,
                2.0 * dot_radius,
                [dot_radius; 4],
                INK_PRIMARY,
                grip_ink,
            );
        }
    }
}

/// An eight-toothed gear of outer `radius` centred on `(cx, cy)`, with a hole in the middle.
fn draw_gear(canvas: &mut Canvas, (cx, cy): (f32, f32), radius: f32, colour: u32, alpha: f32) {
    const TEETH: usize = 8;
    const HOLE_POINTS: usize = 24;
    let step = std::f32::consts::TAU / TEETH as f32;
    let root = radius * 0.78;
    let mut rim: Vec<(f32, f32)> = Vec::with_capacity(TEETH * 4);
    for tooth in 0..TEETH {
        let centre = tooth as f32 * step;
        // Each tooth: up from the root circle, across its crest and back down.
        for (offset, reach) in [
            (-0.30f32, root),
            (-0.17, radius),
            (0.17, radius),
            (0.30, root),
        ] {
            let angle = centre + offset;
            rim.push((cx + reach * angle.cos(), cy + reach * angle.sin()));
        }
    }
    let hole: Vec<(f32, f32)> = (0..HOLE_POINTS)
        .map(|point| {
            let angle = point as f32 * std::f32::consts::TAU / HOLE_POINTS as f32;
            (
                cx + radius * 0.36 * angle.cos(),
                cy + radius * 0.36 * angle.sin(),
            )
        })
        .collect();
    canvas.fill_polygon(&[rim, hole], colour, alpha);
}

/// A filled dot centred on `(x, y)`.
fn dot(canvas: &mut Canvas, (x, y): (f32, f32), radius: f32, colour: u32) {
    canvas.fill_round_rect(
        x - radius,
        y - radius,
        2.0 * radius,
        2.0 * radius,
        [radius; 4],
        colour,
        1.0,
    );
}

/// One cell: track ring, arcs and the glyph, the ring centred on `(cx, cy)`. The Mac notch
/// draws no percentage under its rings.
fn draw_cell(canvas: &mut Canvas, view: &CellView, (cx, cy): (f32, f32), s: f32, pressed: bool) {
    let mut dim = if view.stale { STALE_DIM } else { 1.0 };
    if pressed {
        dim *= CELL_PRESS_DIM;
    }
    let radius = (RING / 2.0 - TRACK_STROKE / 2.0) * s;
    canvas.stroke_arc(
        cx,
        cy,
        radius,
        TRACK_STROKE * s,
        1.0,
        0xFFFFFF,
        RING_TRACK_ALPHA * dim,
    );
    // A spent limit reads as exhausted whatever the arc says; otherwise the reading's own
    // band (Memory) or the share's band decides.
    let colour = |fraction: f32| {
        if view.blocked {
            layout::BAND_CRITICAL
        } else {
            band_color(fraction)
        }
    };
    if let Some(main) = view.main {
        let fraction = f32::from(main) / 100.0;
        let ring = match view.band {
            Some(band) if !view.blocked => tint(band),
            _ => colour(fraction),
        };
        canvas.stroke_arc(cx, cy, radius, PROGRESS_STROKE * s, fraction, ring, dim);
    }
    if let Some(inner) = view.inner {
        let fraction = f32::from(inner) / 100.0;
        canvas.stroke_arc(
            cx,
            cy,
            WEEKLY_RADIUS * s,
            WEEKLY_STROKE * s,
            1.0,
            0xFFFFFF,
            RING_TRACK_ALPHA * 0.7 * dim,
        );
        canvas.stroke_arc(
            cx,
            cy,
            WEEKLY_RADIUS * s,
            WEEKLY_STROKE * s,
            fraction,
            colour(fraction),
            0.8 * dim,
        );
    }
    glyphs::draw(
        canvas,
        Glyph::from_key(view.glyph),
        (cx, cy),
        s,
        if view.problem {
            layout::BAND_WATCH
        } else {
            INK_PRIMARY
        },
        if view.blocked {
            dim * BLOCKED_GLYPH
        } else {
            dim
        },
    );
}

/// A single cell on a transparent canvas (no notch body): what the view shots show for a ring.
pub fn render_cell(view: &CellView, dpi: u32, _text: &mut TextPainter) -> Canvas {
    let s = layout::scale(dpi);
    let size = (RING * s).round() as usize;
    let mut canvas = Canvas::new(size, size);
    draw_cell(
        &mut canvas,
        view,
        (size as f32 / 2.0, size as f32 / 2.0),
        s,
        false,
    );
    canvas
}

// ---- the hover card ---------------------------------------------------------------------------
//
// Geometry follows the Mac card (`TooltipCard.swift`, `NotchLayout`): its design frame is
// 44/117 pt per frame pixel and a Mac point is one DIP here, so every figure below is the Mac
// figure at that scale. The Mac sizes the card from font line boxes (`BUDGET`) and lays the
// text out in SwiftUI's tighter ones (`DRAWN`); keeping both reproduces its bottom margin.

/// Line boxes: body line and title line.
#[derive(Clone, Copy)]
struct Metrics {
    line: f32,
    title: f32,
}

/// What the text is laid out in.
const DRAWN: Metrics = Metrics {
    line: 11.0,
    title: 16.0,
};
/// What the card's height is budgeted in.
const BUDGET: Metrics = Metrics {
    line: 12.0,
    title: 17.0,
};

const CARD_WIDTH: f32 = 225.6;
const CARD_PAD: f32 = 12.0;
const CARD_RADIUS: f32 = 18.6;
const TAIL_LENGTH: f32 = 28.2;
const TAIL_HEIGHT: f32 = 32.7;
/// Em sizes in Segoe UI: the title matches the Mac's SF cap height (13.7 pt); the body is
/// sized to the Mac's measured text widths (SF sets about 8% wider at one cap height).
const TITLE_SIZE: f32 = 14.0;
const BODY_SIZE: f32 = 10.5;
/// Segoe UI's ascent in em: cell top to baseline.
const ASCENT: f32 = 1.079;
/// Line top to baseline in the Mac's layout (SF ascender 0.952 em).
const TITLE_BASELINE: f32 = 12.6;
const BODY_BASELINE: f32 = 9.0;
/// Pitch of a wrapped paragraph's lines.
const WRAP_PITCH: f32 = 12.0;
const MARK_SIZE: f32 = 17.3;
const MARK_GAP: f32 = 6.4;
const HEADER_TO_BLOCK: f32 = 7.9;
const BLOCK_GAP: f32 = 7.5;
const LABEL_TO_BAR: f32 = 6.3;
const BAR_TO_USED: f32 = 6.7;
const BAR_HEIGHT: f32 = 3.95;
const STATUS_DOT: f32 = 6.4;
const STATUS_GAP: f32 = 4.1;
const BUTTON_HEIGHT: f32 = 22.0;
const BUTTON_RADIUS: f32 = 6.0;
/// A button plate's side padding, its symbol, and the gap after the symbol or before the detail.
const BUTTON_PAD: f32 = 8.0;
const BUTTON_SYMBOL: f32 = 12.0;
const BUTTON_INNER: f32 = 5.0;
/// Space kept between a label and its value.
const VALUE_GAP: f32 = 7.5;
/// A grouped limits box (the Mac's `TooltipCard` group): title inset, title to box, box
/// padding, corner radius, outline, and the gap above a group that follows other rows.
const GROUP_TITLE_INSET: f32 = 1.5;
const GROUP_TITLE_GAP: f32 = 4.5;
const GROUP_PAD: f32 = 6.0;
const GROUP_RADIUS: f32 = 7.5;
const GROUP_STROKE: f32 = 0.56;
const GROUP_ALPHA: f32 = 0.25;
const GROUP_GAP: f32 = 10.5;
/// White over the black card: bar tracks and button plates (0.176).
const TRACK_ALPHA: f32 = 0.176;

/// The wide card of the disk image and update cards (820 design px).
const WIDE_WIDTH: f32 = 308.3;
/// The banner card's fixed height (262 design px).
const BANNER_HEIGHT: f32 = 98.5;
/// The banner icon's frame (136 design px) and the icon drawn in it.
const LEAD_FRAME: f32 = 51.1;
const LEAD_SIZE: f32 = 43.0;
const LEAD_GAP: f32 = 11.3;
/// Between a banner's title, detail and warning lines.
const BANNER_GAP: f32 = 2.3;
/// Least space between a banner's text and its bottom stack.
const BANNER_SPACER: f32 = 5.3;
const PROGRESS_HEIGHT: f32 = 4.0;
/// The update card's bar: 16 design px tall, a 0.16 track and a 0.9 fill.
const HEAVY_HEIGHT: f32 = 6.0;
const HEAVY_TRACK_ALPHA: f32 = 0.16;
const HEAVY_FILL_ALPHA: f32 = 0.9;
const PROGRESS_ABOVE_BUTTONS: f32 = 5.3;
const PROGRESS_ALONE: f32 = 7.5;
const PROGRESS_TRACK_ALPHA: f32 = 0.2;
/// The fill of an indeterminate bar in a still, and its width while it moves.
const INDETERMINATE: f32 = 0.35;
const STRIPE: f32 = 0.3;
const PILL_HEIGHT: f32 = 24.8;
const PILL_PAD: f32 = 10.5;
const PILL_GAP: f32 = 5.3;
const PILL_INNER: f32 = 3.8;
const PILL_SYMBOL: f32 = 14.0;
const PILL_ALPHA: f32 = 0.14;
/// The cross of a round close button.
const X_SIZE: f32 = 15.0;
const HEAD_HEIGHT: f32 = 28.6;
const HEAD_GAP: f32 = 5.3;
const DISMISS_BOX: f32 = 16.0;
const DEVICE_HEIGHT: f32 = 31.6;
const DEVICE_GAP: f32 = 3.8;
const DEVICE_RADIUS: f32 = 12.0;
const DEVICE_PAD: f32 = 9.8;
const DEVICE_ICON_FRAME: f32 = 21.0;
const DEVICE_ICON: f32 = 14.0;
const DEVICE_TEXT_GAP: f32 = 8.3;
const DEVICE_ALPHA: f32 = 0.10;
/// Below an alert's status line.
const STATUS_TO_NEXT: f32 = 3.0;
const STATUS_TEXT_GAP: f32 = 4.5;
const SPINNER: f32 = 16.5;
const SPINNER_GAP: f32 = 6.0;
const SPINNER_STROKE: f32 = 2.0;
const DISC_ALPHA: f32 = 0.10;
const DISC_SYMBOL: f32 = 26.0;
const DEVICE_NAME_GAP: f32 = 0.8;

/// Width in DIPs a string would occupy.
fn dip_width(text: &mut TextPainter, value: &str, size: f32, bold: bool, s: f32) -> f32 {
    text_width(text, value, size, bold, s) as f32 / s
}

/// `value` cut at the end with an ellipsis to at most `max` DIPs.
fn fit_tail(
    text: &mut TextPainter,
    value: &str,
    (size, bold): (f32, bool),
    max: f32,
    s: f32,
) -> String {
    if dip_width(text, value, size, bold, s) <= max {
        return value.to_string();
    }
    let mut kept: Vec<char> = value.chars().collect();
    while !kept.is_empty() {
        kept.pop();
        let cut = kept.iter().collect::<String>() + "\u{2026}";
        if dip_width(text, &cut, size, bold, s) <= max {
            return cut;
        }
    }
    String::new()
}

/// Length in characters of a trailing "<number> <word> (<size>)" count (or just the
/// parenthesised part when no number precedes it); 0 when `chars` does not end in one.
fn counted_tail(chars: &[char]) -> usize {
    if chars.last() != Some(&')') {
        return 0;
    }
    let Some(open) = chars.iter().rposition(|c| *c == '(') else {
        return 0;
    };
    let mut start = open;
    // Up to two words before the parenthesis, ending at a number.
    for _ in 0..2 {
        let mut word = start;
        while word > 0 && chars[word - 1] == ' ' {
            word -= 1;
        }
        let end = word;
        while word > 0 && chars[word - 1] != ' ' {
            word -= 1;
        }
        if word == end {
            break;
        }
        start = word;
        if chars[word..end].iter().all(|c| c.is_ascii_digit()) {
            break;
        }
    }
    chars.len() - start
}

/// `value` cut in the middle with an ellipsis to at most `max` DIPs, so the start of a name
/// and the count or extension at its end both stay readable.
fn fit_middle(
    text: &mut TextPainter,
    value: &str,
    (size, bold): (f32, bool),
    max: f32,
    s: f32,
) -> String {
    if dip_width(text, value, size, bold, s) <= max {
        return value.to_string();
    }
    let chars: Vec<char> = value.chars().collect();
    // A trailing count such as "2 files (12.4 MB)" stays whole; only the start is given up.
    let kept_tail = counted_tail(&chars);
    if kept_tail > 0 && kept_tail < chars.len() {
        for head in (0..chars.len() - kept_tail).rev() {
            let mut cut: String = chars[..head].iter().collect();
            cut.push('\u{2026}');
            cut.extend(&chars[chars.len() - kept_tail..]);
            if dip_width(text, &cut, size, bold, s) <= max {
                return cut;
            }
        }
    }
    // Otherwise the Mac's split: the end keeps the odd character.
    for keep in (0..chars.len()).rev() {
        let head = keep / 2;
        let tail = keep - head;
        let mut cut: String = chars[..head].iter().collect();
        cut.push('\u{2026}');
        cut.extend(&chars[chars.len() - tail..]);
        if dip_width(text, &cut, size, bold, s) <= max {
            return cut;
        }
    }
    "\u{2026}".to_string()
}

/// Greedy word wrap of `value` at body size to `max` DIPs, at most `lines` lines: what does
/// not fit on the last one is cut with an ellipsis (the Mac's `lineLimit(2)`).
fn wrap_measured(
    text: &mut TextPainter,
    value: &str,
    max: f32,
    lines: usize,
    s: f32,
) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut words = value.split_whitespace();
    while let Some(word) = words.next() {
        let fits = out.last().is_some_and(|line| {
            dip_width(text, &format!("{line} {word}"), BODY_SIZE, false, s) <= max
        });
        if fits {
            if let Some(line) = out.last_mut() {
                line.push(' ');
                line.push_str(word);
            }
        } else if out.len() == lines {
            let mut rest = out.pop().unwrap_or_default();
            rest.push(' ');
            rest.push_str(word);
            for more in words.by_ref() {
                rest.push(' ');
                rest.push_str(more);
            }
            out.push(fit_tail(text, &rest, (BODY_SIZE, false), max, s));
            break;
        } else {
            out.push(word.to_string());
        }
    }
    if out.is_empty() {
        out.push(String::new());
    }
    for line in &mut out {
        if dip_width(text, line, BODY_SIZE, false, s) > max {
            *line = fit_tail(text, line, (BODY_SIZE, false), max, s);
        }
    }
    out
}

/// Where one row sits, in DIPs from the card's top-left, and what its drawing needs measured.
#[derive(Clone, Default)]
struct Placed {
    top: f32,
    height: f32,
    /// The lines of a wrapped text row.
    lines: Vec<String>,
    /// Left edge and width of each button of a button row (the close is the last, a circle).
    spans: Vec<(f32, f32)>,
}

/// A card laid out: its size and where everything goes, from one measuring of the text.
struct Plan {
    width: f32,
    height: f32,
    /// Left edge of a banner's text column.
    text_left: f32,
    /// A banner's wrapped detail lines.
    detail: Vec<String>,
    rows: Vec<Placed>,
    /// Each header button's rectangle `(x, y, w, h)`.
    head: Vec<(f32, f32, f32, f32)>,
}

/// Left edge and width of each pill of a button row from `left`, then the close circle. Pills
/// that together would pass `right` are all narrowed in proportion (their labels then end in
/// an ellipsis), so a row never spills out of its card.
fn button_spans(
    buttons: &[Button],
    close: bool,
    (left, right): (f32, f32),
    text: &mut TextPainter,
    s: f32,
) -> Vec<(f32, f32)> {
    let natural: Vec<f32> = buttons
        .iter()
        .map(|button| {
            let label = dip_width(text, &button.label, BODY_SIZE, true, s);
            let symbol = if button.symbol.is_some() {
                PILL_SYMBOL + PILL_INNER
            } else {
                0.0
            };
            2.0 * PILL_PAD + symbol + label
        })
        .collect();
    let gaps = PILL_GAP * natural.len().saturating_sub(1) as f32;
    let close_room = if close { PILL_GAP + PILL_HEIGHT } else { 0.0 };
    let total: f32 = natural.iter().sum();
    let available = (right - left - gaps - close_room).max(0.0);
    let shrink = if total > available && total > 0.0 {
        available / total
    } else {
        1.0
    };
    let mut spans = Vec::new();
    let mut x = left;
    for width in natural.iter().map(|width| width * shrink) {
        spans.push((x, width));
        x += width + PILL_GAP;
    }
    if close {
        spans.push((x, PILL_HEIGHT));
    }
    spans
}

/// Least gap between the bar's two buttons: the Mac's HStack spacing, spacer and spacing.
const BAR_GAP: f32 = 20.0;
/// Room left on a bar plate beyond its label's measured width, so float rounding between the
/// measured plate and the drawn one can never make "Paste" lose a letter to an ellipsis.
const BAR_SLACK: f32 = 2.0;
/// Off-card span of a bar button that is not there, so no pointer ever lands on it.
const NO_SPAN: (f32, f32) = (-10_000.0, 0.0);

/// Left edge and width of the Send bar's two plates: "Copy last: ..." at `left` (cut to what
/// "Paste" leaves), "Paste" against `right`. A missing button has `NO_SPAN`.
fn bar_spans(
    copy: Option<(&str, &str)>,
    paste: bool,
    (left, right): (f32, f32),
    text: &mut TextPainter,
    s: f32,
) -> Vec<(f32, f32)> {
    let mut plate = |label: &str, detail: &str| {
        let label = dip_width(text, label, BODY_SIZE, false, s);
        let detail = if detail.is_empty() {
            0.0
        } else {
            dip_width(text, detail, BODY_SIZE, false, s) + BUTTON_INNER
        };
        2.0 * BUTTON_PAD + BUTTON_SYMBOL + BUTTON_INNER + label + detail + BAR_SLACK
    };
    let paste_width = paste.then(|| plate("Paste", ""));
    let copy_span = copy.map_or(NO_SPAN, |(label, detail)| {
        let reserved = paste_width.map_or(0.0, |w| w + BAR_GAP);
        (
            left,
            plate(label, detail).min((right - left - reserved).max(0.0)),
        )
    });
    let paste_span = paste_width.map_or(NO_SPAN, |w| (right - w, w));
    vec![copy_span, paste_span]
}

/// Height of one meter: label line, bar (none without a share), summary line.
fn meter_height(fraction: Option<f32>, m: Metrics) -> f32 {
    let bar = if fraction.is_some() {
        LABEL_TO_BAR + BAR_HEIGHT
    } else {
        0.0
    };
    m.line + bar + BAR_TO_USED + m.line
}

/// Where each meter of a group sits inside its box (DIPs from the box's top) and the box's
/// height: padding, the meters a block gap apart, padding.
fn group_tops(rows: &[Row], m: Metrics) -> (Vec<f32>, f32) {
    let mut y = GROUP_PAD;
    let mut tops = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        if index > 0 {
            y += BLOCK_GAP;
        }
        tops.push(y);
        if let Row::Meter { fraction, .. } = row {
            y += meter_height(*fraction, m);
        }
    }
    (tops, y + GROUP_PAD)
}

/// A closed rounded-rectangle outline.
fn round_rect_outline(x: f32, y: f32, w: f32, h: f32, r: f32) -> Vec<(f32, f32)> {
    const STEPS: usize = 8;
    let r = r.min(w / 2.0).min(h / 2.0);
    let corners = [
        (x + w - r, y + r, -std::f32::consts::FRAC_PI_2),
        (x + w - r, y + h - r, 0.0),
        (x + r, y + h - r, std::f32::consts::FRAC_PI_2),
        (x + r, y + r, std::f32::consts::PI),
    ];
    let mut points = Vec::new();
    for (cx, cy, start) in corners {
        for step in 0..=STEPS {
            let angle = start + std::f32::consts::FRAC_PI_2 * step as f32 / STEPS as f32;
            points.push((cx + r * angle.cos(), cy + r * angle.sin()));
        }
    }
    points
}

/// Height in DIPs of the lines of a wrapped paragraph.
fn lines_height(count: usize, m: Metrics) -> f32 {
    (count.max(1) - 1) as f32 * WRAP_PITCH + m.line
}

/// Height, wrapped lines and button spans of `row` in a plain (non-banner) card.
fn flow_row(
    row: &Row,
    m: Metrics,
    room: f32,
    text: &mut TextPainter,
    s: f32,
) -> (f32, Vec<String>, Vec<(f32, f32)>) {
    // Wrapped by measured width, so a line that fits the column stays on one line (the Mac's
    // `Text` only wraps when it does not fit).
    let paragraph = |text: &mut TextPainter, value: &str, room: f32| {
        let lines = wrap_measured(text, value, room, 8, s);
        (lines_height(lines.len(), m), lines, Vec::new())
    };
    match row {
        Row::Button { .. } => (BUTTON_HEIGHT, Vec::new(), Vec::new()),
        Row::Bar {
            copy,
            copy_detail,
            paste,
        } => (
            BUTTON_HEIGHT,
            Vec::new(),
            bar_spans(
                copy.as_deref().map(|label| (label, copy_detail.as_str())),
                *paste,
                (CARD_PAD, CARD_PAD + room),
                text,
                s,
            ),
        ),
        Row::Pair { .. } | Row::Status { .. } => (m.line, Vec::new(), Vec::new()),
        Row::Meter { fraction, .. } => (meter_height(*fraction, m), Vec::new(), Vec::new()),
        Row::Group { rows, .. } => {
            let (_, box_height) = group_tops(rows, m);
            (
                m.line + GROUP_TITLE_GAP + box_height,
                Vec::new(),
                Vec::new(),
            )
        }
        Row::Note(value) | Row::Text(value) | Row::Tinted { text: value, .. } => {
            paragraph(text, value, room)
        }
        Row::Alert(value) => paragraph(text, value, room - STATUS_DOT - STATUS_GAP),
        Row::Buttons { buttons, close } => (
            PILL_HEIGHT,
            Vec::new(),
            button_spans(buttons, *close, (CARD_PAD, CARD_PAD + room), text, s),
        ),
        Row::Progress(_) => (PROGRESS_HEIGHT, Vec::new(), Vec::new()),
        Row::Device { .. } | Row::Waiting(_) => (DEVICE_HEIGHT, Vec::new(), Vec::new()),
    }
}

fn round_head(content: &CardContent) -> bool {
    content.head.iter().any(|head| *head != Head::Dismiss)
}

fn header_height(content: &CardContent, m: Metrics) -> f32 {
    let text = m.title
        + if content.subtitle.is_some() {
            m.line
        } else {
            0.0
        };
    let text = if content.mark == Mark::None {
        text
    } else {
        text.max(MARK_SIZE)
    };
    if round_head(content) {
        text.max(HEAD_HEIGHT)
    } else {
        text
    }
}

/// Each header button's rectangle: round ones centred on the header's height, a bare dismiss
/// on the title line, right to left from the card's padding.
fn head_rects(content: &CardContent, width: f32, m: Metrics) -> Vec<(f32, f32, f32, f32)> {
    let header = header_height(content, m);
    let mut rects = vec![(0.0, 0.0, 0.0, 0.0); content.head.len()];
    let mut right = width - CARD_PAD;
    for (index, head) in content.head.iter().enumerate().rev() {
        let (side, centre) = if *head == Head::Dismiss {
            (DISMISS_BOX, CARD_PAD + m.title / 2.0)
        } else {
            (PILL_HEIGHT, CARD_PAD + header / 2.0)
        };
        rects[index] = (right - side, centre - side / 2.0, side, side);
        right -= side + PILL_GAP;
    }
    rects
}

/// A plain card: header, then rows top to bottom.
fn flow_plan(content: &CardContent, m: Metrics, text: &mut TextPainter, s: f32) -> Plan {
    let width = if content.wide { WIDE_WIDTH } else { CARD_WIDTH };
    let round = round_head(content);
    let mut y = CARD_PAD + header_height(content, m);
    let mut rows = Vec::new();
    let mut previous: Option<&Row> = None;
    for row in &content.rows {
        y += match (previous, row) {
            (None, _) if round => HEAD_GAP,
            (None, _) => HEADER_TO_BLOCK,
            (Some(Row::Status { .. }), _) => STATUS_TO_NEXT,
            (Some(_), Row::Group { .. }) => GROUP_GAP,
            (Some(Row::Device { .. } | Row::Waiting(_)), Row::Device { .. }) => DEVICE_GAP,
            _ => BLOCK_GAP,
        };
        let (height, lines, spans) = flow_row(row, m, width - 2.0 * CARD_PAD, text, s);
        rows.push(Placed {
            top: y,
            height,
            lines,
            spans,
        });
        y += height;
        previous = Some(row);
    }
    Plan {
        width,
        height: content.height.unwrap_or(y + CARD_PAD),
        text_left: CARD_PAD,
        detail: Vec::new(),
        rows,
        head: head_rects(content, width, m),
    }
}

/// A banner card (the Mac's disk image, update and transfer cards): the icon at the left, the
/// title, detail and caveat lines of the text column from the top, the bar and the buttons
/// from the bottom, the card at least as tall as the Mac's fixed height.
fn banner_plan(content: &CardContent, m: Metrics, text: &mut TextPainter, s: f32) -> Plan {
    let width = if content.wide { WIDE_WIDTH } else { CARD_WIDTH };
    let text_left = CARD_PAD + LEAD_FRAME + LEAD_GAP;
    let room = width - CARD_PAD - text_left;
    let detail = content
        .subtitle
        .as_deref()
        .map(|value| wrap_measured(text, value, room, 2, s))
        .unwrap_or_default();
    let mut y = CARD_PAD + m.title;
    if !detail.is_empty() {
        y += BANNER_GAP + lines_height(detail.len(), m);
    }
    let mut rows = Vec::new();
    for row in &content.rows {
        let mut placed = Placed::default();
        if let Row::Tinted { text: value, .. } | Row::Note(value) | Row::Text(value) = row {
            placed.lines = wrap_measured(text, value, room, 2, s);
            y += BANNER_GAP;
            placed.top = y;
            placed.height = lines_height(placed.lines.len(), m);
            y += placed.height;
        }
        rows.push(placed);
    }
    // The bottom stack, measured up from the card's content bottom: (row, top above it).
    let bar = if content.heavy_bar {
        HEAVY_HEIGHT
    } else {
        PROGRESS_HEIGHT
    };
    let mut up = 0.0f32;
    let mut stack: Vec<(usize, f32, f32)> = Vec::new();
    for (index, row) in content.rows.iter().enumerate().rev() {
        match row {
            Row::Buttons { .. } => {
                up += PILL_HEIGHT;
                stack.push((index, up, PILL_HEIGHT));
            }
            Row::Progress(_) => {
                up += if up == 0.0 {
                    PROGRESS_ALONE
                } else {
                    PROGRESS_ABOVE_BUTTONS
                } + bar;
                stack.push((index, up, bar));
            }
            _ => {}
        }
    }
    let needed = y + if up > 0.0 { BANNER_SPACER + up } else { 0.0 } + CARD_PAD;
    let height = content.height.unwrap_or(BANNER_HEIGHT).max(needed);
    for (index, above, row_height) in stack {
        rows[index].top = height - CARD_PAD - above;
        rows[index].height = row_height;
        if let Row::Buttons { buttons, close } = &content.rows[index] {
            let limit = (text_left, width - CARD_PAD);
            rows[index].spans = button_spans(buttons, *close, limit, text, s);
        }
    }
    Plan {
        width,
        height,
        text_left,
        detail,
        rows,
        head: Vec::new(),
    }
}

fn plan(content: &CardContent, m: Metrics, text: &mut TextPainter, s: f32) -> Plan {
    if content.lead.is_some() {
        banner_plan(content, m, text, s)
    } else {
        flow_plan(content, m, text, s)
    }
}

/// The card's rectangle in the window, device pixels: `(x, y, width, height)`; the window is
/// larger by the tail on the side facing the notch.
fn card_rect(content: &CardContent, text: &mut TextPainter, s: f32) -> (i32, i32, i32, i32) {
    let laid = plan(content, BUDGET, text, s);
    let tail = px(TAIL_LENGTH, s);
    let (width, height) = (px(laid.width, s), px(laid.height, s));
    match content.tail.map(|t| t.edge) {
        Some(Edge::Top) => (0, tail, width, height),
        Some(Edge::Left) => (tail, 0, width, height),
        _ => (0, 0, width, height),
    }
}

/// The window around `rect`: the card plus its tail.
fn window_size(content: &CardContent, rect: (i32, i32, i32, i32), s: f32) -> (i32, i32) {
    let (x, y, width, height) = rect;
    let tail = px(TAIL_LENGTH, s);
    match content.tail.map(|t| t.edge) {
        Some(Edge::Left | Edge::Right) => (width + tail, height),
        Some(Edge::Top | Edge::Bottom) => (width, height + tail),
        None => (x + width, y + height),
    }
}

/// Size in device pixels of the card window for `content`: the card plus its tail.
pub fn card_size(content: &CardContent, dpi: u32, text: &mut TextPainter) -> (i32, i32) {
    let s = layout::scale(dpi);
    window_size(content, card_rect(content, text, s), s)
}

/// What is under `(x, y)` (window-local device pixels): a header button, a pill or round
/// button of a button row, or a row that is one thing to press (a text row is not).
pub fn hit_at(
    content: &CardContent,
    dpi: u32,
    text: &mut TextPainter,
    (x, y): (i32, i32),
) -> Option<Hit> {
    let s = layout::scale(dpi);
    let (ox, oy, _, _) = card_rect(content, text, s);
    let (x, y) = ((x - ox) as f32 / s, (y - oy) as f32 / s);
    let laid = plan(content, DRAWN, text, s);
    let inside = |(rx, ry, rw, rh): (f32, f32, f32, f32)| {
        x >= rx - 2.0 && x < rx + rw + 2.0 && y >= ry - 2.0 && y < ry + rh + 2.0
    };
    if let Some(index) = laid.head.iter().position(|rect| inside(*rect)) {
        return Some(Hit::Head(index));
    }
    for (index, (row, placed)) in content.rows.iter().zip(&laid.rows).enumerate() {
        if let Row::Buttons { .. } | Row::Bar { .. } = row {
            let found = placed
                .spans
                .iter()
                .position(|&(bx, bw)| inside((bx, placed.top, bw, placed.height)));
            if let Some(button) = found {
                return Some(Hit::Row(index, button));
            }
            continue;
        }
        let band =
            y >= placed.top - BLOCK_GAP / 2.0 && y < placed.top + placed.height + BLOCK_GAP / 2.0;
        if band && content.lead.is_none() {
            return Some(Hit::Row(index, 0));
        }
    }
    None
}

/// The card moves (a spinner or a travelling bar), so the owner should redraw it on a timer.
pub fn animated(content: &CardContent) -> bool {
    content.head.contains(&Head::Scanning)
        || content
            .rows
            .iter()
            .any(|row| matches!(row, Row::Progress(None) | Row::Waiting(_)))
}

/// The tail's seven control points `(tip, a, b, a shoulder, a tip, b tip, b shoulder)` inside
/// `(x, y, w, h)`: the Mac's `TooltipTail`, whose shoulders leave the card tangent to its
/// edge so card and tail read as one moulded outline.
type TailPoints = [(f32, f32); 7];

fn tail_points(edge: Edge, (x, y, w, h): (f32, f32, f32, f32)) -> TailPoints {
    let (right, bottom, mid_x, mid_y) = (x + w, y + h, x + w / 2.0, y + h / 2.0);
    match edge {
        // Card on the right of the notch, tip to the left.
        Edge::Left => [
            (x, mid_y),
            (right, y),
            (right, bottom),
            (right, y + h * 0.25),
            (x + w * 0.42, mid_y - h * 0.12),
            (x + w * 0.42, mid_y + h * 0.12),
            (right, bottom - h * 0.25),
        ],
        // Card on the left, tip to the right.
        Edge::Right => [
            (right, mid_y),
            (x, y),
            (x, bottom),
            (x, y + h * 0.25),
            (right - w * 0.42, mid_y - h * 0.12),
            (right - w * 0.42, mid_y + h * 0.12),
            (x, bottom - h * 0.25),
        ],
        // Card below the notch, tip upward.
        Edge::Top => [
            (mid_x, y),
            (x, bottom),
            (right, bottom),
            (x + w * 0.25, bottom),
            (mid_x - w * 0.12, y + h * 0.42),
            (mid_x + w * 0.12, y + h * 0.42),
            (right - w * 0.25, bottom),
        ],
        // Card above the notch, tip downward.
        Edge::Bottom => [
            (mid_x, bottom),
            (x, y),
            (right, y),
            (x + w * 0.25, y),
            (mid_x - w * 0.12, bottom - h * 0.42),
            (mid_x + w * 0.12, bottom - h * 0.42),
            (right - w * 0.25, y),
        ],
    }
}

fn cubic(p: [(f32, f32); 4], t: f32) -> (f32, f32) {
    let u = 1.0 - t;
    let weight = [u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t];
    (
        p.iter().zip(weight).map(|(q, k)| q.0 * k).sum(),
        p.iter().zip(weight).map(|(q, k)| q.1 * k).sum(),
    )
}

/// Coverage (0..=1) of the polygon at the pixel centre `(px, py)`.
fn polygon_coverage(poly: &[(f32, f32)], (px, py): (f32, f32)) -> f32 {
    let mut inside = false;
    let mut nearest = f32::MAX;
    for (index, &(ax, ay)) in poly.iter().enumerate() {
        let (bx, by) = poly[(index + 1) % poly.len()];
        if (ay > py) != (by > py) && px < (bx - ax) * (py - ay) / (by - ay) + ax {
            inside = !inside;
        }
        let (dx, dy) = (bx - ax, by - ay);
        let length = dx * dx + dy * dy;
        let t = if length > 0.0 {
            (((px - ax) * dx + (py - ay) * dy) / length).clamp(0.0, 1.0)
        } else {
            0.0
        };
        nearest = nearest.min((px - ax - t * dx).hypot(py - ay - t * dy));
    }
    let distance = if inside { -nearest } else { nearest };
    (0.5 - distance).clamp(0.0, 1.0)
}

/// The card and its tail as one black silhouette.
fn draw_silhouette(canvas: &mut Canvas, content: &CardContent, rect: (i32, i32, i32, i32), s: f32) {
    let (x, y, width, height) = rect;
    canvas.fill_round_rect(
        x as f32,
        y as f32,
        width as f32,
        height as f32,
        [CARD_RADIUS * s; 4],
        0x000000,
        1.0,
    );
    let Some(tail) = content.tail else {
        return;
    };
    let (length, across) = (px(TAIL_LENGTH, s) as f32, TAIL_HEIGHT * s);
    let (x, y, width, height) = (x as f32, y as f32, width as f32, height as f32);
    // Slide the tail along the card to stay on its cell, never onto a corner.
    let slide = |extent: f32| {
        let limit = (extent / 2.0 - CARD_RADIUS * s - across / 2.0).max(0.0);
        (tail.offset as f32).clamp(-limit, limit)
    };
    let rect = match tail.edge {
        Edge::Top => (
            x + width / 2.0 - across / 2.0 + slide(width),
            y - length,
            across,
            length,
        ),
        Edge::Bottom => (
            x + width / 2.0 - across / 2.0 + slide(width),
            y + height,
            across,
            length,
        ),
        Edge::Left => (
            x - length,
            y + height / 2.0 - across / 2.0 + slide(height),
            length,
            across,
        ),
        Edge::Right => (
            x + width,
            y + height / 2.0 - across / 2.0 + slide(height),
            length,
            across,
        ),
    };
    let [tip, a, b, a_shoulder, a_tip, b_tip, b_shoulder] = tail_points(tail.edge, rect);
    let mut outline = vec![a];
    for step in 1..=24 {
        outline.push(cubic([a, a_shoulder, a_tip, tip], step as f32 / 24.0));
    }
    for step in 1..=24 {
        outline.push(cubic([tip, b_tip, b_shoulder, b], step as f32 / 24.0));
    }
    let (x0, y0) = (
        rect.0.floor().max(0.0) as usize,
        rect.1.floor().max(0.0) as usize,
    );
    let x1 = ((rect.0 + rect.2).ceil().max(0.0) as usize).min(canvas.width);
    let y1 = ((rect.1 + rect.3).ceil().max(0.0) as usize).min(canvas.height);
    for py in y0..y1 {
        for px in x0..x1 {
            let coverage = polygon_coverage(&outline, (px as f32 + 0.5, py as f32 + 0.5));
            if coverage > 0.0 {
                // Black source-over: premultiplied colour stays zero, only alpha grows.
                let pixel = &mut canvas.pixels[py * canvas.width + px];
                let alpha = (*pixel >> 24) as f32 / 255.0;
                let alpha = coverage + alpha * (1.0 - coverage);
                *pixel = ((alpha * 255.0).round() as u32) << 24;
            }
        }
    }
}

/// Text drawing for the card, on a baseline in device pixels.
struct Pen<'a> {
    canvas: &'a mut Canvas,
    text: &'a mut TextPainter,
    s: f32,
    /// Where an animation is in its loop (0..1); `None` draws a still.
    phase: Option<f32>,
}

#[derive(Clone, Copy)]
struct Style {
    size: f32,
    bold: bool,
    color: u32,
}

fn body(color: u32) -> Style {
    Style {
        size: BODY_SIZE,
        bold: false,
        color,
    }
}

impl Pen<'_> {
    fn width(&mut self, value: &str, style: Style) -> i32 {
        text_width(self.text, value, style.size, style.bold, self.s)
    }

    /// Draws `value` with its baseline at `baseline`; `x` is its left edge, or its right edge
    /// when `right`. Returns the width.
    fn put(&mut self, value: &str, x: f32, baseline: f32, style: Style, right: bool) -> i32 {
        let em = px(style.size, self.s) as f32;
        let top = (baseline - ASCENT * em).round() as i32;
        let width = self.width(value, style);
        let left = x.round() as i32 - if right { width } else { 0 };
        draw_text(
            self.canvas,
            self.text,
            value,
            (left, top),
            (style.size, style.bold),
            self.s,
            style.color,
            1.0,
        );
        width
    }

    /// `value` cut to `max` pixels with an ellipsis.
    fn fit(&mut self, value: &str, style: Style, max: i32) -> String {
        if self.width(value, style) <= max {
            return value.to_string();
        }
        let mut kept: Vec<char> = value.chars().collect();
        while !kept.is_empty() {
            kept.pop();
            let cut: String = kept.iter().collect::<String>() + "\u{2026}";
            if self.width(&cut, style) <= max {
                return cut;
            }
        }
        String::new()
    }

    fn bar(&mut self, (left, right): (f32, f32), top: f32, fraction: Option<f32>) {
        let height = BAR_HEIGHT * self.s;
        let radius = height / 2.0;
        let width = right - left;
        self.canvas
            .fill_round_rect(left, top, width, height, [radius; 4], 0xFFFFFF, TRACK_ALPHA);
        if let Some(f) = fraction.filter(|f| *f > 0.0) {
            let f = f.clamp(0.0, 1.0);
            self.canvas.fill_round_rect(
                left,
                top,
                (width * f).max(height),
                height,
                [radius; 4],
                band_color(f),
                1.0,
            );
        }
    }

    /// One meter at `y` between `columns`: label and trailing text, the bar, the summary.
    fn meter(&mut self, (left, right): (f32, f32), y: f32, row: &Row) {
        let Row::Meter {
            label,
            trailing,
            fraction,
            summary,
        } = row
        else {
            return;
        };
        let s = self.s;
        let base = |extra: f32| y + (extra + BODY_BASELINE) * s;
        self.put(label, left, base(0.0), body(INK_PRIMARY), false);
        self.put(trailing, right, base(0.0), body(INK_SECONDARY), true);
        let mut next = DRAWN.line;
        if let Some(share) = *fraction {
            self.bar((left, right), y + (next + LABEL_TO_BAR) * s, Some(share));
            next += LABEL_TO_BAR + BAR_HEIGHT;
        }
        next += BAR_TO_USED;
        self.put(summary, left, base(next), body(INK_PRIMARY), false);
    }

    /// The paused-circle mark in front of a limit line.
    fn pause_mark(&mut self, (cx, cy): (f32, f32)) {
        let s = self.s;
        let radius = STATUS_DOT / 2.0 * s;
        self.canvas.fill_round_rect(
            cx - radius,
            cy - radius,
            2.0 * radius,
            2.0 * radius,
            [radius; 4],
            layout::BAND_CRITICAL,
            1.0,
        );
        for dx in [-1.5, 0.5] {
            self.canvas.fill_round_rect(
                cx + dx * s,
                cy - 1.4 * s,
                s.max(1.0),
                2.8 * s,
                [0.0; 4],
                0x000000,
                1.0,
            );
        }
    }

    /// The icon in front of a card's title.
    fn mark(&mut self, mark: Mark, (x, y): (f32, f32)) {
        let glyph = match mark {
            Mark::None => return,
            Mark::Claude => Glyph::Claude,
            Mark::Codex => Glyph::OpenAi,
            Mark::System => Glyph::Chip,
            Mark::Disks => Glyph::Drive,
            Mark::Send => Glyph::Plane,
        };
        let centre = (x + MARK_SIZE * self.s / 2.0, y + MARK_SIZE * self.s / 2.0);
        glyphs::draw(self.canvas, glyph, centre, self.s, INK_PRIMARY, 1.0);
    }

    /// A progress bar: white on a faint track. `None` is indeterminate: a stripe that travels
    /// while the card animates, a fixed share in a still.
    fn progress(
        &mut self,
        (left, right): (f32, f32),
        top: f32,
        fraction: Option<f32>,
        heavy: bool,
    ) {
        let (thick, track, fill_alpha) = if heavy {
            (HEAVY_HEIGHT, HEAVY_TRACK_ALPHA, HEAVY_FILL_ALPHA)
        } else {
            (PROGRESS_HEIGHT, PROGRESS_TRACK_ALPHA, 1.0)
        };
        let height = thick * self.s;
        let radius = height / 2.0;
        let width = right - left;
        self.canvas
            .fill_round_rect(left, top, width, height, [radius; 4], 0xFFFFFF, track);
        let (from, share) = match (fraction, self.phase) {
            (Some(f), _) => (0.0, f.clamp(0.0, 1.0)),
            (None, None) => (0.0, INDETERMINATE),
            (None, Some(phase)) => {
                let start = (1.0 + STRIPE) * phase - STRIPE;
                let end = (start + STRIPE).min(1.0);
                (start.max(0.0), end - start.max(0.0))
            }
        };
        if share > 0.0 {
            let fill = (width * share).max(height).min(width - width * from);
            self.canvas.fill_round_rect(
                left + width * from,
                top,
                fill,
                height,
                [radius; 4],
                INK_PRIMARY,
                fill_alpha,
            );
        }
    }

    /// A quarter-turn gap ring, turning while the card animates.
    fn spinner(&mut self, (cx, cy): (f32, f32), radius: f32) {
        let start = self.phase.unwrap_or(0.0) * 360.0 - 90.0;
        let points: Vec<(f32, f32)> = (0..=18)
            .map(|step| {
                let angle = (start + 270.0 * step as f32 / 18.0).to_radians();
                (cx + radius * angle.cos(), cy + radius * angle.sin())
            })
            .collect();
        self.canvas
            .stroke_polyline(&points, SPINNER_STROKE * self.s, false, INK_PRIMARY, 1.0);
    }

    /// A pill: a faint plate, the symbol and the label in white (dimmed when it does nothing).
    fn pill(&mut self, (x, y, width): (f32, f32, f32), button: &Button, live: bool) {
        let s = self.s;
        let height = PILL_HEIGHT * s;
        self.canvas
            .fill_round_rect(x, y, width, height, [height / 2.0; 4], 0xFFFFFF, PILL_ALPHA);
        let ink = if live { INK_PRIMARY } else { INK_SECONDARY };
        let centre = y + height / 2.0;
        let mut cursor = x + PILL_PAD * s;
        if let Some(symbol) = button.symbol {
            glyphs::symbol(
                self.canvas,
                symbol,
                (cursor + PILL_SYMBOL * s / 2.0, centre),
                PILL_SYMBOL * s,
                ink,
                1.0,
            );
            cursor += (PILL_SYMBOL + PILL_INNER) * s;
        }
        let style = Style {
            size: BODY_SIZE,
            bold: true,
            color: ink,
        };
        let baseline = centre + (BODY_BASELINE - DRAWN.line / 2.0) * s;
        // A label wider than its pill (the pill was cut to fit the card) ends in an ellipsis.
        let room = (x + width - PILL_PAD * s - cursor).max(0.0).round() as i32;
        let label = self.fit(&button.label, style, room);
        self.put(&label, cursor, baseline, style, false);
    }

    /// A full-width button plate (the Mac's "Copy last" and "Paste clipboard" rows): a faint
    /// plate, the symbol and label, and the detail at the right. With no detail the symbol and
    /// label sit in the middle. The detail keeps up to half the plate; whatever does not fit
    /// is cut with an ellipsis. Dimmed when the button does nothing.
    fn button_plate(
        &mut self,
        (left, right, y): (f32, f32, f32),
        (symbol, label, detail): (Option<Symbol>, &str, &str),
        live: bool,
    ) {
        let s = self.s;
        let height = BUTTON_HEIGHT * s;
        self.canvas.fill_round_rect(
            left,
            y,
            right - left,
            height,
            [BUTTON_RADIUS * s; 4],
            0xFFFFFF,
            TRACK_ALPHA,
        );
        let color = if live { INK_PRIMARY } else { INK_SECONDARY };
        let (ink, quiet) = (body(color), body(INK_SECONDARY));
        let pad = BUTTON_PAD * s;
        let icon = if symbol.is_some() {
            (BUTTON_SYMBOL + BUTTON_INNER) * s
        } else {
            0.0
        };
        let mut room = right - left - 2.0 * pad - icon;
        let mut detail_text = String::new();
        if !detail.is_empty() {
            detail_text = self.fit(detail, quiet, (room / 2.0) as i32);
            room -= self.width(&detail_text, quiet) as f32 + BUTTON_INNER * s;
        }
        // Half a pixel of tolerance: the plate's width came from a measured label through
        // DIPs and back, and a truncating cast must not cost a letter.
        let label = self.fit(label, ink, (room + 0.5).max(0.0) as i32);
        let label_width = self.width(&label, ink) as f32;
        let start = if detail.is_empty() {
            (left + right - icon - label_width) / 2.0
        } else {
            left + pad
        };
        if let Some(symbol) = symbol {
            let centre = (start + BUTTON_SYMBOL * s / 2.0, y + height / 2.0);
            glyphs::symbol(self.canvas, symbol, centre, BUTTON_SYMBOL * s, color, 1.0);
        }
        let baseline = y + (BUTTON_HEIGHT - DRAWN.line) / 2.0 * s + BODY_BASELINE * s;
        self.put(&label, start + icon, baseline, ink, false);
        if !detail_text.is_empty() {
            self.put(&detail_text, right - pad, baseline, quiet, true);
        }
    }

    /// A round header or row button: a faint disc with a cross, a refresh arrow or a spinner;
    /// a dismiss is the bare cross.
    fn round(&mut self, (x, y, side): (f32, f32, f32), head: Head, live: bool) {
        let s = self.s;
        let centre = (x + side / 2.0, y + side / 2.0);
        let ink = if live { INK_PRIMARY } else { INK_SECONDARY };
        if head == Head::Dismiss {
            glyphs::symbol(
                self.canvas,
                Symbol::Xmark,
                centre,
                X_SIZE * s,
                INK_SECONDARY,
                1.0,
            );
            return;
        }
        self.canvas
            .fill_round_rect(x, y, side, side, [side / 2.0; 4], 0xFFFFFF, PILL_ALPHA);
        match head {
            Head::Scanning => self.spinner(centre, (SPINNER - SPINNER_STROKE) / 2.0 * s),
            Head::Refresh => {
                glyphs::symbol(
                    self.canvas,
                    Symbol::Refresh,
                    centre,
                    PILL_SYMBOL * s,
                    ink,
                    1.0,
                );
            }
            Head::Close | Head::Dismiss => {
                glyphs::symbol(self.canvas, Symbol::Xmark, centre, X_SIZE * s, ink, 1.0);
            }
            Head::Done => {
                glyphs::symbol(
                    self.canvas,
                    Symbol::Check,
                    centre,
                    PILL_SYMBOL * s,
                    ink,
                    1.0,
                );
            }
            Head::Failed => {
                let red = layout::BAND_CRITICAL;
                glyphs::symbol(
                    self.canvas,
                    Symbol::Warning,
                    centre,
                    PILL_SYMBOL * s,
                    red,
                    1.0,
                );
            }
        }
    }

    /// The banner's large icon centred on `centre`.
    fn lead(&mut self, lead: &Lead, centre: (f32, f32)) {
        let s = self.s;
        match lead {
            Lead::Tile(tile) => glyphs::tile(self.canvas, *tile, centre, LEAD_SIZE * s),
            Lead::File(path) => match fileicon::of(path) {
                Some(icon) => {
                    let side = LEAD_SIZE * s;
                    self.canvas.draw_image(
                        &icon.pixels,
                        (icon.width, icon.height),
                        centre.0 - side / 2.0,
                        centre.1 - side / 2.0,
                        side,
                        side,
                    );
                }
                None => glyphs::tile(self.canvas, Tile::Package, centre, LEAD_SIZE * s),
            },
            Lead::Disc { symbol, problem } => {
                let side = LEAD_FRAME * s;
                self.canvas.fill_round_rect(
                    centre.0 - side / 2.0,
                    centre.1 - side / 2.0,
                    side,
                    side,
                    [side / 2.0; 4],
                    0xFFFFFF,
                    DISC_ALPHA,
                );
                let ink = if *problem {
                    layout::BAND_WATCH
                } else {
                    INK_PRIMARY
                };
                glyphs::symbol(self.canvas, *symbol, centre, DISC_SYMBOL * s, ink, 1.0);
            }
        }
    }

    /// A device row: a faint plate, its symbol, the name in bold with the model beneath, and
    /// the send arrow at the right.
    fn device(
        &mut self,
        (left, right, y): (f32, f32, f32),
        device: (Symbol, &str, &str),
        live: bool,
    ) {
        let (symbol, alias, model) = device;
        let s = self.s;
        let height = DEVICE_HEIGHT * s;
        self.canvas.fill_round_rect(
            left,
            y,
            right - left,
            height,
            [DEVICE_RADIUS * s; 4],
            0xFFFFFF,
            DEVICE_ALPHA,
        );
        let centre = y + height / 2.0;
        let icon = (left + (DEVICE_PAD + DEVICE_ICON_FRAME / 2.0) * s, centre);
        glyphs::symbol(self.canvas, symbol, icon, DEVICE_ICON * s, INK_PRIMARY, 1.0);
        let arrow = (right - (DEVICE_PAD + DEVICE_ICON / 2.0) * s, centre);
        let arrow_alpha = if live { 0.35 } else { 0.2 };
        glyphs::symbol(
            self.canvas,
            Symbol::Plane,
            arrow,
            DEVICE_ICON * s,
            INK_PRIMARY,
            arrow_alpha,
        );
        let x = left + (DEVICE_PAD + DEVICE_ICON_FRAME + DEVICE_TEXT_GAP) * s;
        let room = (arrow.0 - DEVICE_ICON * s - x) / s;
        let name = fit_tail(self.text, alias, (TITLE_SIZE, true), room, s);
        let title = Style {
            size: TITLE_SIZE,
            bold: true,
            color: INK_PRIMARY,
        };
        if model.is_empty() {
            let baseline = centre + (TITLE_BASELINE - DRAWN.title / 2.0) * s;
            self.put(&name, x, baseline, title, false);
            return;
        }
        let block = DRAWN.title + DEVICE_NAME_GAP + DRAWN.line;
        let top = centre - block / 2.0 * s;
        self.put(&name, x, top + TITLE_BASELINE * s, title, false);
        let model = fit_tail(self.text, model, (BODY_SIZE, false), room, s);
        let baseline = top + (DRAWN.title + DEVICE_NAME_GAP + BODY_BASELINE) * s;
        self.put(&model, x, baseline, body(INK_SECONDARY), false);
    }

    /// A plain card: the header (icon, title, plan line, note, round buttons), then rows.
    fn flow(&mut self, content: &CardContent, laid: &Plan, live: &Live, (ox, oy): (f32, f32)) {
        let s = self.s;
        let pad = CARD_PAD * s;
        let columns = (ox + pad, ox + (laid.width - CARD_PAD) * s);
        let (left, right) = columns;
        let top = oy;
        // Header: the icon centred on the title block, the note on the title's line.
        let text_height = DRAWN.title
            + if content.subtitle.is_some() {
                DRAWN.line
            } else {
                0.0
            };
        let header = header_height(content, DRAWN);
        let text_top = top + (CARD_PAD + (header - text_height) / 2.0) * s;
        let title_x = if content.mark == Mark::None {
            left
        } else {
            self.mark(
                content.mark,
                (left, top + (CARD_PAD + (header - MARK_SIZE) / 2.0) * s),
            );
            left + (MARK_SIZE + MARK_GAP) * s
        };
        // The text stops short of the header's buttons.
        let limit = laid
            .head
            .iter()
            .map(|rect| rect.0)
            .fold(laid.width - CARD_PAD, f32::min);
        let text_room = (ox + (limit - VALUE_GAP) * s - title_x) / s;
        let title_baseline = text_top + TITLE_BASELINE * s;
        let title_style = Style {
            size: TITLE_SIZE,
            bold: true,
            color: INK_PRIMARY,
        };
        let title = if laid.head.is_empty() {
            content.title.clone()
        } else {
            fit_tail(self.text, &content.title, (TITLE_SIZE, true), text_room, s)
        };
        self.put(&title, title_x, title_baseline, title_style, false);
        if let Some(note) = &content.accessory {
            self.put(note, right, title_baseline, body(INK_SECONDARY), true);
        }
        if let Some(subtitle) = &content.subtitle {
            let ink = if content.problem {
                layout::BAND_WATCH
            } else {
                INK_SECONDARY
            };
            let subtitle = if laid.head.is_empty() {
                subtitle.clone()
            } else {
                fit_middle(self.text, subtitle, (BODY_SIZE, false), text_room, s)
            };
            self.put(
                &subtitle,
                title_x,
                text_top + (DRAWN.title + BODY_BASELINE) * s,
                body(ink),
                false,
            );
        }
        for (index, (head, rect)) in content.head.iter().zip(&laid.head).enumerate() {
            let on = live.head.get(index) == Some(&true);
            let place = (ox + rect.0 * s, oy + rect.1 * s, rect.2 * s);
            self.round(place, *head, on);
        }
        for (index, (row, placed)) in content.rows.iter().zip(&laid.rows).enumerate() {
            let on = live.row(index, 0);
            let y = top + placed.top * s;
            // Baseline of the line `extra` DIPs down the row.
            let base = |extra: f32| y + (extra + BODY_BASELINE) * s;
            match row {
                Row::Button {
                    symbol,
                    label,
                    detail,
                } => self.button_plate(
                    (left, right, y),
                    (*symbol, label.as_str(), detail.as_str()),
                    on,
                ),
                Row::Bar {
                    copy,
                    copy_detail,
                    paste,
                } => {
                    for (n, (x, width)) in placed.spans.iter().enumerate() {
                        let (label, detail, shown) = match n {
                            0 => (
                                copy.as_deref().unwrap_or(""),
                                copy_detail.as_str(),
                                copy.is_some(),
                            ),
                            _ => ("Paste", "", *paste),
                        };
                        if shown {
                            self.button_plate(
                                (ox + x * s, ox + (x + width) * s, y),
                                (Some(Symbol::Copy), label, detail),
                                live.row(index, n),
                            );
                        }
                    }
                }
                Row::Pair { label, value } => {
                    // Both sides are cut to fit: the value keeps what the label leaves (and
                    // at least half the line when it needs it).
                    let (primary, quiet) = (body(INK_PRIMARY), body(INK_SECONDARY));
                    let total = (right - left) as i32;
                    let gap = (VALUE_GAP * s) as i32;
                    let label_width = self.width(label, primary);
                    let value_room = (total - label_width - gap).max(total / 2);
                    let value = self.fit(value, quiet, value_room);
                    let value_width = self.width(&value, quiet);
                    let label = self.fit(label, primary, total - value_width - gap);
                    self.put(&label, left, base(0.0), primary, false);
                    self.put(&value, right, base(0.0), quiet, true);
                }
                Row::Meter { .. } => self.meter(columns, y, row),
                Row::Group { title, rows } => {
                    let bold = Style {
                        size: BODY_SIZE,
                        bold: true,
                        color: INK_PRIMARY,
                    };
                    self.put(title, left + GROUP_TITLE_INSET * s, base(0.0), bold, false);
                    let box_top = y + (BUDGET.line + GROUP_TITLE_GAP) * s;
                    let (tops, height) = group_tops(rows, BUDGET);
                    let outline = round_rect_outline(
                        left,
                        box_top,
                        right - left,
                        height * s,
                        GROUP_RADIUS * s,
                    );
                    let stroke = GROUP_STROKE * s;
                    self.canvas
                        .stroke_polyline(&outline, stroke, true, 0xFFFFFF, GROUP_ALPHA);
                    let inner = (left + GROUP_PAD * s, right - GROUP_PAD * s);
                    for (meter, top) in rows.iter().zip(tops) {
                        self.meter(inner, box_top + top * s, meter);
                    }
                }
                Row::Note(_) | Row::Text(_) => {
                    let ink = if matches!(row, Row::Note(_)) {
                        INK_SECONDARY
                    } else {
                        INK_PRIMARY
                    };
                    for (n, line) in placed.lines.iter().enumerate() {
                        self.put(line, left, base(n as f32 * WRAP_PITCH), body(ink), false);
                    }
                }
                Row::Tinted { tone, .. } => {
                    for (n, line) in placed.lines.iter().enumerate() {
                        let ink = body(tone_color(*tone));
                        self.put(line, left, base(n as f32 * WRAP_PITCH), ink, false);
                    }
                }
                Row::Alert(_) => {
                    self.pause_mark((left + STATUS_DOT / 2.0 * s, y + DRAWN.line / 2.0 * s));
                    let indent = left + (STATUS_DOT + STATUS_GAP) * s;
                    for (n, line) in placed.lines.iter().enumerate() {
                        let ink = body(layout::BAND_CRITICAL);
                        self.put(line, indent, base(n as f32 * WRAP_PITCH), ink, false);
                    }
                }
                Row::Status { text, tone } => {
                    let colour = tone_color(*tone);
                    let radius = STATUS_DOT / 2.0 * s;
                    let centre = (left + radius, y + DRAWN.line / 2.0 * s);
                    self.canvas.fill_round_rect(
                        centre.0 - radius,
                        centre.1 - radius,
                        2.0 * radius,
                        2.0 * radius,
                        [radius; 4],
                        colour,
                        1.0,
                    );
                    let indent = left + (STATUS_DOT + STATUS_TEXT_GAP) * s;
                    self.put(text, indent, base(0.0), body(colour), false);
                }
                Row::Buttons { buttons, .. } => {
                    self.button_row(buttons, placed, live, (index, ox, y));
                }
                Row::Progress(fraction) => self.progress(columns, y, *fraction, false),
                Row::Device {
                    symbol,
                    alias,
                    model,
                } => self.device(
                    (left, right, y),
                    (*symbol, alias.as_str(), model.as_str()),
                    on,
                ),
                Row::Waiting(label) => {
                    let ink = body(INK_SECONDARY);
                    let label_width = self.width(label, ink) as f32;
                    let total = (SPINNER + SPINNER_GAP) * s + label_width;
                    let x0 = (left + right - total) / 2.0;
                    let centre = y + DEVICE_HEIGHT * s / 2.0;
                    let radius = (SPINNER - SPINNER_STROKE) / 2.0 * s;
                    self.spinner((x0 + SPINNER * s / 2.0, centre), radius);
                    let baseline = centre + (BODY_BASELINE - DRAWN.line / 2.0) * s;
                    self.put(
                        label,
                        x0 + (SPINNER + SPINNER_GAP) * s,
                        baseline,
                        ink,
                        false,
                    );
                }
            }
        }
    }

    /// The pills of a button row then its round close, at the spans the plan measured.
    fn button_row(
        &mut self,
        buttons: &[Button],
        placed: &Placed,
        live: &Live,
        (index, ox, y): (usize, f32, f32),
    ) {
        let s = self.s;
        for (n, (x, width)) in placed.spans.iter().enumerate() {
            let on = live.row(index, n);
            let at = (ox + x * s, y, width * s);
            match buttons.get(n) {
                Some(button) => self.pill(at, button, on),
                None => self.round(at, Head::Close, on),
            }
        }
    }

    /// A banner card: the large icon, the title, the detail and caveat lines, then the bar
    /// and the buttons at the bottom of the text column.
    fn banner(&mut self, content: &CardContent, laid: &Plan, live: &Live, (ox, oy): (f32, f32)) {
        let s = self.s;
        let column = ox + laid.text_left * s;
        let right = ox + (laid.width - CARD_PAD) * s;
        if let Some(lead) = &content.lead {
            let centre = (
                ox + (CARD_PAD + LEAD_FRAME / 2.0) * s,
                oy + laid.height * s / 2.0,
            );
            self.lead(lead, centre);
        }
        let title = fit_middle(
            self.text,
            &content.title,
            (TITLE_SIZE, true),
            (right - column) / s,
            s,
        );
        let title_style = Style {
            size: TITLE_SIZE,
            bold: true,
            color: INK_PRIMARY,
        };
        self.put(
            &title,
            column,
            oy + (CARD_PAD + TITLE_BASELINE) * s,
            title_style,
            false,
        );
        let detail_ink = if content.problem {
            layout::BAND_WATCH
        } else {
            INK_SECONDARY
        };
        let detail_top = CARD_PAD + DRAWN.title + BANNER_GAP;
        for (n, line) in laid.detail.iter().enumerate() {
            let baseline = oy + (detail_top + n as f32 * WRAP_PITCH + BODY_BASELINE) * s;
            self.put(line, column, baseline, body(detail_ink), false);
        }
        for (index, (row, placed)) in content.rows.iter().zip(&laid.rows).enumerate() {
            let y = oy + placed.top * s;
            match row {
                Row::Note(_) | Row::Text(_) | Row::Tinted { .. } => {
                    let ink = match row {
                        Row::Tinted { tone, .. } => tone_color(*tone),
                        Row::Text(_) => INK_PRIMARY,
                        _ => INK_SECONDARY,
                    };
                    for (n, line) in placed.lines.iter().enumerate() {
                        let baseline = y + (n as f32 * WRAP_PITCH + BODY_BASELINE) * s;
                        self.put(line, column, baseline, body(ink), false);
                    }
                }
                Row::Progress(fraction) => {
                    self.progress((column, right), y, *fraction, content.heavy_bar);
                }
                Row::Buttons { buttons, .. } => {
                    self.button_row(buttons, placed, live, (index, ox, y));
                }
                _ => {}
            }
        }
    }
}

/// The colour a tone reads in.
fn tone_color(tone: Tone) -> u32 {
    match tone {
        Tone::Warning => layout::BAND_WATCH,
        Tone::Good => tint(layout::BAND_AMPLE),
        Tone::Critical => layout::BAND_CRITICAL,
    }
}

/// The card: a plain one (header, then rows) or a banner (large icon beside the title block,
/// the buttons below), in a rounded shape with a tail toward the notch. `live` marks which
/// buttons have an action; a dead button reads dimmed. `phase` (0..1, looping) moves a
/// spinner or an indeterminate bar; `None` draws them still.
pub fn render_card(
    content: &CardContent,
    live: &Live,
    dpi: u32,
    text: &mut TextPainter,
    phase: Option<f32>,
) -> Canvas {
    render_card_hover(content, live, dpi, text, phase, None)
}

/// Hover lift over a pressed-able control: white at 8%, as on the Mac.
const HOVER_ALPHA: f32 = 0.16;
/// A pressed pill or round button reads lighter than a hovered one.
const PRESS_ALPHA: f32 = 0.26;
/// How far the hover plate of a text row that acts reaches beyond its text, and its corners.
const ROW_HOVER_X: f32 = 6.0;
const ROW_HOVER_Y: f32 = 3.0;
const ROW_HOVER_RADIUS: f32 = 10.0;
/// The Quit item's plate: the Send bar plate's grey (the 0.176 track) lifted by the same +0.22
/// (hover) and +0.32 (pressed), as white laid over the menu's black.
const MENU_HOVER_ALPHA: f32 = TRACK_ALPHA + 0.22;
const MENU_PRESS_ALPHA: f32 = TRACK_ALPHA + 0.32;
/// Inset of the Quit plate from the menu's edge.
const MENU_PLATE_INSET: f32 = 4.0;
/// The Send bar's plates under the Mac's `CardButtonStyle`: its +0.22 (hover) and +0.32
/// (pressed) brightness on the 0.176 plate, as white laid over that plate.
const BAR_HOVER_ALPHA: f32 = 0.22 / (1.0 - TRACK_ALPHA);
const BAR_PRESS_ALPHA: f32 = 0.32 / (1.0 - TRACK_ALPHA);

/// The primary button is held down over the card (set by the card window, read by every
/// `render_card_hover` so the plate under the pointer reads pressed).
static PRESSED: AtomicBool = AtomicBool::new(false);

/// Records whether the button is held down; true when that changed the picture.
pub fn set_pressed(down: bool) -> bool {
    PRESSED.swap(down, Ordering::Relaxed) != down
}

/// `render_card` with a hover plate over `hover` (a live pill, round button or device row).
pub fn render_card_hover(
    content: &CardContent,
    live: &Live,
    dpi: u32,
    text: &mut TextPainter,
    phase: Option<f32>,
    hover: Option<Hit>,
) -> Canvas {
    let s = layout::scale(dpi);
    let rect = card_rect(content, text, s);
    let (width, height) = window_size(content, rect, s);
    let mut canvas = Canvas::new(width as usize, height as usize);
    draw_silhouette(&mut canvas, content, rect, s);
    let laid = plan(content, DRAWN, text, s);
    let origin = (rect.0 as f32, rect.1 as f32);
    let mut pen = Pen {
        canvas: &mut canvas,
        text,
        s,
        phase,
    };
    if content.lead.is_some() {
        pen.banner(content, &laid, live, origin);
    } else {
        pen.flow(content, &laid, live, origin);
    }
    if let Some((x, y, w, h, radius)) = hover.and_then(|hit| hover_rect(content, &laid, hit)) {
        let pressed = PRESSED.load(Ordering::Relaxed);
        let bar = matches!(
            hover.and_then(|hit| match hit {
                Hit::Row(index, _) => content.rows.get(index),
                Hit::Head(_) => None,
            }),
            Some(Row::Bar { .. } | Row::Button { .. })
        );
        let alpha = match (bar, pressed) {
            (true, true) => BAR_PRESS_ALPHA,
            (true, false) => BAR_HOVER_ALPHA,
            (false, true) => PRESS_ALPHA,
            (false, false) => HOVER_ALPHA,
        };
        canvas.fill_round_rect(
            origin.0 + x * s,
            origin.1 + y * s,
            w * s,
            h * s,
            [radius * s; 4],
            0xFFFFFF,
            alpha,
        );
    }
    canvas
}

/// The plate for `hit` in card DIPs `(x, y, width, height, radius)`: a header button, a pill
/// or round button, or a whole device row.
fn hover_rect(content: &CardContent, laid: &Plan, hit: Hit) -> Option<(f32, f32, f32, f32, f32)> {
    match hit {
        Hit::Head(index) => {
            let (x, y, w, h) = *laid.head.get(index)?;
            Some((x, y, w, h, w.min(h) / 2.0))
        }
        Hit::Row(index, button) => {
            let placed = laid.rows.get(index)?;
            match content.rows.get(index)? {
                Row::Buttons { .. } => {
                    let (x, w) = *placed.spans.get(button)?;
                    Some((x, placed.top, w, placed.height, w.min(placed.height) / 2.0))
                }
                Row::Bar { .. } => {
                    let (x, w) = *placed.spans.get(button)?;
                    Some((x, placed.top, w, placed.height, BUTTON_RADIUS))
                }
                Row::Device { .. } => Some((
                    CARD_PAD,
                    placed.top,
                    laid.width - 2.0 * CARD_PAD,
                    placed.height,
                    DEVICE_RADIUS,
                )),
                // A plain button row (the Send card's Cancel) is a plate over its whole width.
                Row::Button { .. } => Some((
                    CARD_PAD,
                    placed.top,
                    laid.width - 2.0 * CARD_PAD,
                    placed.height,
                    BUTTON_RADIUS,
                )),
                // A nearby device on the Send card's hover card is a text row that acts: the
                // Mac's row hover, a plate a little wider and taller than the text.
                Row::Pair { .. } => {
                    let height = placed.height + 2.0 * ROW_HOVER_Y;
                    Some((
                        CARD_PAD - ROW_HOVER_X,
                        placed.top - ROW_HOVER_Y,
                        laid.width - 2.0 * (CARD_PAD - ROW_HOVER_X),
                        height,
                        ROW_HOVER_RADIUS.min(height / 2.0),
                    ))
                }
                _ => None,
            }
        }
    }
}

fn menu_background(canvas: &mut Canvas, scale: f32) {
    let (w, h) = (canvas.width as f32, canvas.height as f32);
    let r = MENU_RADIUS * scale;
    canvas.fill_round_rect(0.0, 0.0, w, h, [r; 4], BORDER, 1.0);
    let inset = scale.max(1.0).round();
    canvas.fill_round_rect(
        inset,
        inset,
        w - 2.0 * inset,
        h - 2.0 * inset,
        [(r - inset).max(0.0); 4],
        0x000000,
        1.0,
    );
}

/// Menu size in device pixels.
pub fn menu_size(dpi: u32) -> (i32, i32) {
    let s = layout::scale(dpi);
    (px(MENU_WIDTH, s), px(MENU_HEIGHT, s))
}

/// The right-click menu: a single "Quit" item in the card style.
pub fn render_menu(dpi: u32, text: &mut TextPainter) -> Canvas {
    render_menu_hover(dpi, text, false)
}

/// `render_menu` with the Quit plate drawn when the pointer is `over` it (lighter while the
/// primary button is held, as the Send bar's buttons).
pub fn render_menu_hover(dpi: u32, text: &mut TextPainter, over: bool) -> Canvas {
    let s = layout::scale(dpi);
    let (width, height) = menu_size(dpi);
    let mut canvas = Canvas::new(width as usize, height as usize);
    menu_background(&mut canvas, s);
    if over {
        let alpha = if PRESSED.load(Ordering::Relaxed) {
            MENU_PRESS_ALPHA
        } else {
            MENU_HOVER_ALPHA
        };
        let inset = MENU_PLATE_INSET * s;
        canvas.fill_round_rect(
            inset,
            inset,
            width as f32 - 2.0 * inset,
            height as f32 - 2.0 * inset,
            [BUTTON_RADIUS * s; 4],
            0xFFFFFF,
            alpha,
        );
    }
    draw_text(
        &mut canvas,
        text,
        "Quit",
        (px(MENU_PAD, s), px((MENU_HEIGHT - MENU_LINE) / 2.0, s)),
        (MENU_SIZE, false),
        s,
        INK_PRIMARY,
        1.0,
    );
    canvas
}
