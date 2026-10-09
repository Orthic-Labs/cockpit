//! Draws the notch body, the hover card and the Quit menu into canvases (premultiplied BGRA)
//! using the software rasteriser and GDI text. Dark solid style, like the Mac notch's solid
//! surface. Pure of window state: callers pass data and get pixels.

use crate::canvas::{Canvas, Mask};
use crate::card::{CardContent, Dot, Mark, Row};
use crate::glyphs::{self, Glyph};
use crate::layout::{
    self, Activity, Badges, CellView, Edge, INK_PRIMARY, INK_SECONDARY, PROGRESS_STROKE, RING,
    RING_TRACK_ALPHA, TRACK_STROKE, WEEKLY_RADIUS, WEEKLY_STROKE,
};
use crate::surface::TextPainter;

const STALE_DIM: f32 = 0.45;
/// Glyph opacity while a limit is spent (the Mac dims it to 0.35).
const BLOCKED_GLYPH: f32 = 0.35;
/// Permission dot on the folded pill (the Mac's four-point amber dot), in DIPs.
const PILL_DOT: f32 = 4.0;
const BORDER: u32 = 0x2A2A2A;

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
/// of the settings arc); otherwise a pending update draws as a red one on the arc.
pub fn render_notch(
    views: &[CellView],
    edge: Edge,
    folded: bool,
    badges: Badges,
    dpi: u32,
    text: &mut TextPainter,
) -> Canvas {
    draw_notch(views, edge, folded, badges, dpi, text, false)
}

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
    draw_notch(views, edge, folded, badges, dpi, text, true)
}

fn draw_notch(
    views: &[CellView],
    edge: Edge,
    folded: bool,
    badges: Badges,
    dpi: u32,
    _text: &mut TextPainter,
    keep_band: bool,
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
        (layout::DEPTH * s, layout::SHAPE_LENGTH * s)
    };
    let outline: Vec<(f32, f32)> = shape_outline(length, depth, s)
        .into_iter()
        .map(|(along, down)| frame.at(along, down - band))
        .collect();
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
    canvas.stroke_polyline(&arc, layout::ORB_STROKE * s, false, 0x000000, 1.0);
    for (index, view) in views.iter().enumerate() {
        let centre = frame.shift(layout::ring_center(edge, index, dpi));
        draw_cell(&mut canvas, view, centre, s);
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
fn draw_cell(canvas: &mut Canvas, view: &CellView, (cx, cy): (f32, f32), s: f32) {
    let dim = if view.stale { STALE_DIM } else { 1.0 };
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
            layout::band_color(fraction)
        }
    };
    if let Some(main) = view.main {
        let fraction = f32::from(main) / 100.0;
        let ring = match view.band {
            Some(band) if !view.blocked => band,
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
    // Waiting on a question / just completed: a full ring inside the track, at full strength
    // even when the usage reading is stale (it is first-hand).
    let activity = match view.activity {
        Activity::None => None,
        Activity::Waiting => Some(layout::BAND_WATCH),
        Activity::Success => Some(layout::BAND_AMPLE),
    };
    if let Some(activity) = activity {
        canvas.stroke_arc(
            cx,
            cy,
            layout::ACTIVITY_RADIUS * s,
            layout::ACTIVITY_STROKE * s,
            1.0,
            activity,
            1.0,
        );
    }
}

/// A single cell on a transparent canvas (no notch body): what the view shots show for a ring.
pub fn render_cell(view: &CellView, dpi: u32, _text: &mut TextPainter) -> Canvas {
    let s = layout::scale(dpi);
    let size = (RING * s).round() as usize;
    let mut canvas = Canvas::new(size, size);
    draw_cell(&mut canvas, view, (size as f32 / 2.0, size as f32 / 2.0), s);
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
/// Em sizes in Segoe UI that match the Mac's SF cap heights (9.48 pt body, 13.7 pt title).
const TITLE_SIZE: f32 = 14.0;
const BODY_SIZE: f32 = 9.7;
/// Segoe UI's ascent in em: cell top to baseline.
const ASCENT: f32 = 1.079;
/// Line top to baseline in the Mac's layout (SF ascender 0.952 em).
const TITLE_BASELINE: f32 = 13.0;
const BODY_BASELINE: f32 = 9.0;
/// Pitch of a wrapped paragraph's lines.
const WRAP_PITCH: f32 = 12.0;
/// Characters a paragraph line holds before it wraps (the card's text column at body size).
const WRAP_CHARS: usize = 38;
const MARK_SIZE: f32 = 17.3;
const MARK_GAP: f32 = 6.4;
const HEADER_TO_BLOCK: f32 = 7.9;
const BLOCK_GAP: f32 = 7.5;
const LABEL_TO_BAR: f32 = 6.3;
const BAR_TO_USED: f32 = 6.7;
const BAR_HEIGHT: f32 = 3.95;
const SESSION_GAP: f32 = 3.8;
const HAIRLINE: f32 = 0.94;
const STATUS_DOT: f32 = 6.4;
const STATUS_STROKE: f32 = 1.28;
const STATUS_GAP: f32 = 4.1;
const BUTTON_HEIGHT: f32 = 22.0;
const BUTTON_RADIUS: f32 = 6.0;
/// Space kept between a label and its value.
const VALUE_GAP: f32 = 7.5;
/// White over the black card: bar tracks and button plates (0.176), the rule (0.188).
const TRACK_ALPHA: f32 = 0.176;
const RULE_ALPHA: f32 = 0.188;

/// Height in DIPs of `row` under `metrics`.
fn row_height(row: &Row, m: Metrics) -> f32 {
    let wrapped = |text: &str, width: usize| {
        (wrap(text, width).len().max(1) - 1) as f32 * WRAP_PITCH + m.line
    };
    match row {
        Row::Pair { value, .. } if value.is_empty() => BUTTON_HEIGHT,
        Row::Pair { .. } => m.line,
        Row::Bar { .. } => m.line + LABEL_TO_BAR + BAR_HEIGHT,
        Row::Meter { fraction, .. } => {
            let bar = if fraction.is_some() {
                LABEL_TO_BAR + BAR_HEIGHT
            } else {
                0.0
            };
            m.line + bar + BAR_TO_USED + m.line
        }
        Row::Note(text) | Row::Text(text) => wrapped(text, WRAP_CHARS),
        Row::Alert(text) => wrapped(text, WRAP_CHARS - 4),
        Row::Rule => HAIRLINE,
        Row::Session { .. } => 2.0 * m.line + SESSION_GAP,
    }
}

/// Greedy word wrap at `width` characters; a longer word keeps its own line.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for word in text.split_whitespace() {
        match lines.last_mut() {
            Some(line) if line.chars().count() + 1 + word.chars().count() <= width => {
                line.push(' ');
                line.push_str(word);
            }
            _ => lines.push(word.to_string()),
        }
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

fn header_height(content: &CardContent, m: Metrics) -> f32 {
    let text = m.title
        + if content.subtitle.is_some() {
            m.line
        } else {
            0.0
        };
    if content.mark == Mark::None {
        text
    } else {
        text.max(MARK_SIZE)
    }
}

/// Top and height of each row, in DIPs from the card's top edge.
fn row_tops(content: &CardContent, m: Metrics) -> Vec<(f32, f32)> {
    let mut y = CARD_PAD + header_height(content, m);
    content
        .rows
        .iter()
        .enumerate()
        .map(|(index, row)| {
            y += if index == 0 {
                HEADER_TO_BLOCK
            } else {
                BLOCK_GAP
            };
            let top = y;
            let height = row_height(row, m);
            y += height;
            (top, height)
        })
        .collect()
}

fn card_height(content: &CardContent) -> f32 {
    let header = CARD_PAD + header_height(content, BUDGET);
    let bottom = row_tops(content, BUDGET)
        .last()
        .map_or(header, |(top, height)| top + height);
    bottom + CARD_PAD
}

/// The card's rectangle in the window, device pixels: `(x, y, width, height)`; the window is
/// larger by the tail on the side facing the notch.
fn card_rect(content: &CardContent, s: f32) -> (i32, i32, i32, i32) {
    let tail = px(TAIL_LENGTH, s);
    let (width, height) = (px(CARD_WIDTH, s), px(card_height(content), s));
    match content.tail.map(|t| t.edge) {
        Some(Edge::Top) => (0, tail, width, height),
        Some(Edge::Left) => (tail, 0, width, height),
        _ => (0, 0, width, height),
    }
}

/// Index of the row under `y` (window-local device pixels), if any.
pub fn row_at(content: &CardContent, dpi: u32, y: i32) -> Option<usize> {
    let s = layout::scale(dpi);
    let y = (y - card_rect(content, s).1) as f32 / s;
    row_tops(content, DRAWN)
        .into_iter()
        .position(|(top, height)| y >= top - BLOCK_GAP / 2.0 && y < top + height + BLOCK_GAP / 2.0)
}

/// Size in device pixels of the card window for `content`: the card plus its tail.
pub fn card_size(content: &CardContent, dpi: u32) -> (i32, i32) {
    let s = layout::scale(dpi);
    let (x, y, width, height) = card_rect(content, s);
    let tail = px(TAIL_LENGTH, s);
    match content.tail.map(|t| t.edge) {
        Some(Edge::Left | Edge::Right) => (width + tail, height),
        Some(Edge::Top | Edge::Bottom) => (width, height + tail),
        None => (x + width, y + height),
    }
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
fn draw_silhouette(canvas: &mut Canvas, content: &CardContent, s: f32) {
    let (x, y, width, height) = card_rect(content, s);
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
                layout::band_color(f),
                1.0,
            );
        }
    }

    /// The ring beside a session's status: turning (three quarters) while busy, half a ring
    /// when blocked, whole otherwise.
    fn status_ring(&mut self, (cx, cy): (f32, f32), dot: Dot) {
        let (fraction, color) = match dot {
            Dot::Busy => (0.75, INK_PRIMARY),
            Dot::Waiting => (0.5, layout::BAND_WATCH),
            Dot::Success => (1.0, layout::BAND_AMPLE),
            Dot::Idle => (1.0, INK_SECONDARY),
        };
        let radius = (STATUS_DOT - STATUS_STROKE) / 2.0 * self.s;
        self.canvas
            .stroke_arc(cx, cy, radius, STATUS_STROKE * self.s, fraction, color, 1.0);
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
        // HOOK(glyphs.rs): the vector icons (Claude, Codex, chip, drive, send) are drawn here
        // once that module lands, into the `MARK_SIZE` square at `(x, y)`. Until then the
        // glyph's text abbreviation stands in.
        let abbreviation = match mark {
            Mark::None => return,
            Mark::Claude => "Cl",
            Mark::Codex => "Cx",
            Mark::System => "CPU",
            Mark::Disks => "DSK",
            Mark::Send => "Snd",
        };
        let style = Style {
            size: 8.0,
            bold: true,
            color: INK_PRIMARY,
        };
        let width = self.width(abbreviation, style) as f32;
        let centre = (x + MARK_SIZE * self.s / 2.0, y + MARK_SIZE * self.s / 2.0);
        self.put(
            abbreviation,
            centre.0 - width / 2.0,
            centre.1 + 4.0 * self.s,
            style,
            false,
        );
    }
}

/// The hover card: a header (icon, title, plan line, note), then rows, in a rounded card with
/// a tail toward the notch. `clickable[i]` marks row `i` as live; a button row that is not
/// reads dimmed.
pub fn render_card(
    content: &CardContent,
    clickable: &[bool],
    dpi: u32,
    text: &mut TextPainter,
) -> Canvas {
    let s = layout::scale(dpi);
    let (width, height) = card_size(content, dpi);
    let mut canvas = Canvas::new(width as usize, height as usize);
    draw_silhouette(&mut canvas, content, s);
    let (ox, oy, card_width, _) = card_rect(content, s);
    let pad = CARD_PAD * s;
    let columns = (ox as f32 + pad, (ox + card_width) as f32 - pad);
    let (left, right) = columns;
    let top = oy as f32;
    let mut pen = Pen {
        canvas: &mut canvas,
        text,
        s,
    };

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
        pen.mark(
            content.mark,
            (left, top + (CARD_PAD + (header - MARK_SIZE) / 2.0) * s),
        );
        left + (MARK_SIZE + MARK_GAP) * s
    };
    let title_baseline = text_top + TITLE_BASELINE * s;
    pen.put(
        &content.title,
        title_x,
        title_baseline,
        Style {
            size: TITLE_SIZE,
            bold: true,
            color: INK_PRIMARY,
        },
        false,
    );
    if let Some(note) = &content.accessory {
        pen.put(note, right, title_baseline, body(INK_SECONDARY), true);
    }
    if let Some(subtitle) = &content.subtitle {
        pen.put(
            subtitle,
            title_x,
            text_top + (DRAWN.title + BODY_BASELINE) * s,
            body(INK_SECONDARY),
            false,
        );
    }

    let tops = row_tops(content, DRAWN);
    for (index, (row, (row_top, _))) in content.rows.iter().zip(tops).enumerate() {
        let live = clickable.get(index) == Some(&true);
        let y = top + row_top * s;
        // Baseline of the line `extra` DIPs down the row.
        let base = |extra: f32| y + (extra + BODY_BASELINE) * s;
        match row {
            Row::Pair { label, value } if value.is_empty() => {
                let height = BUTTON_HEIGHT * s;
                pen.canvas.fill_round_rect(
                    left,
                    y,
                    right - left,
                    height,
                    [BUTTON_RADIUS * s; 4],
                    0xFFFFFF,
                    TRACK_ALPHA,
                );
                let ink = body(if live { INK_PRIMARY } else { INK_SECONDARY });
                let label_width = pen.width(label, ink) as f32;
                pen.put(
                    label,
                    (left + right - label_width) / 2.0,
                    y + (BUTTON_HEIGHT - DRAWN.line) / 2.0 * s + BODY_BASELINE * s,
                    ink,
                    false,
                );
            }
            Row::Pair { label, value } => {
                let label_width = pen.put(label, left, base(0.0), body(INK_PRIMARY), false);
                let room = (right - left) as i32 - label_width - (VALUE_GAP * s) as i32;
                let value = pen.fit(value, body(INK_SECONDARY), room);
                pen.put(&value, right, base(0.0), body(INK_SECONDARY), true);
            }
            Row::Bar {
                label,
                value,
                fraction,
            } => {
                pen.put(label, left, base(0.0), body(INK_PRIMARY), false);
                pen.put(value, right, base(0.0), body(INK_SECONDARY), true);
                pen.bar(columns, y + (DRAWN.line + LABEL_TO_BAR) * s, *fraction);
            }
            Row::Meter {
                label,
                trailing,
                fraction,
                summary,
            } => {
                pen.put(label, left, base(0.0), body(INK_PRIMARY), false);
                pen.put(trailing, right, base(0.0), body(INK_SECONDARY), true);
                let mut next = DRAWN.line;
                if let Some(share) = fraction {
                    pen.bar(columns, y + (next + LABEL_TO_BAR) * s, Some(*share));
                    next += LABEL_TO_BAR + BAR_HEIGHT;
                }
                next += BAR_TO_USED;
                pen.put(summary, left, base(next), body(INK_PRIMARY), false);
            }
            Row::Note(note) | Row::Text(note) => {
                let ink = if matches!(row, Row::Note(_)) {
                    INK_SECONDARY
                } else {
                    INK_PRIMARY
                };
                for (n, line) in wrap(note, WRAP_CHARS).iter().enumerate() {
                    pen.put(line, left, base(n as f32 * WRAP_PITCH), body(ink), false);
                }
            }
            Row::Alert(alert) => {
                pen.pause_mark((left + STATUS_DOT / 2.0 * s, y + DRAWN.line / 2.0 * s));
                let indent = left + (STATUS_DOT + STATUS_GAP) * s;
                for (n, line) in wrap(alert, WRAP_CHARS - 4).iter().enumerate() {
                    let ink = body(layout::BAND_CRITICAL);
                    pen.put(line, indent, base(n as f32 * WRAP_PITCH), ink, false);
                }
            }
            Row::Rule => pen.canvas.fill_round_rect(
                left,
                y,
                right - left,
                (HAIRLINE * s).max(1.0),
                [0.0; 4],
                0xFFFFFF,
                RULE_ALPHA,
            ),
            Row::Session {
                name,
                dot,
                word,
                detail,
                age,
            } => {
                let ink = match dot {
                    Dot::Busy => INK_PRIMARY,
                    Dot::Waiting => layout::BAND_WATCH,
                    Dot::Success => layout::BAND_AMPLE,
                    Dot::Idle => INK_SECONDARY,
                };
                let word_width = pen.put(word, right, base(0.0), body(ink), true) as f32;
                let centre_x = right - word_width - (STATUS_GAP + STATUS_DOT / 2.0) * s;
                pen.status_ring((centre_x, y + DRAWN.line / 2.0 * s), *dot);
                let room = centre_x - left - (STATUS_DOT / 2.0 + VALUE_GAP) * s;
                let name = pen.fit(name, body(INK_PRIMARY), room as i32);
                pen.put(&name, left, base(0.0), body(INK_PRIMARY), false);
                let second = DRAWN.line + SESSION_GAP;
                let age_width = pen.put(age, right, base(second), body(INK_SECONDARY), true) as f32;
                let room = right - left - age_width - VALUE_GAP * s;
                let detail = pen.fit(detail, body(INK_SECONDARY), room as i32);
                pen.put(&detail, left, base(second), body(INK_SECONDARY), false);
            }
        }
    }
    canvas
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
    let s = layout::scale(dpi);
    let (width, height) = menu_size(dpi);
    let mut canvas = Canvas::new(width as usize, height as usize);
    menu_background(&mut canvas, s);
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
