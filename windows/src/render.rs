//! Draws the notch body, the hover card and the Quit menu into canvases (premultiplied BGRA)
//! using the software rasteriser and GDI text. Dark solid style, like the Mac notch's solid
//! surface. Pure of window state: callers pass data and get pixels.

use crate::canvas::{Canvas, Mask};
use crate::card::{CardContent, Row};
use crate::layout::{
    self, BODY_CORNER, CellView, INK_PRIMARY, INK_SECONDARY, LABEL_GAP, PAD_TOP, PROGRESS_STROKE,
    RING, RING_TRACK_ALPHA, TRACK_STROKE, WEEKLY_RADIUS, WEEKLY_STROKE,
};
use crate::surface::TextPainter;

const STALE_DIM: f32 = 0.45;
const BORDER: u32 = 0x2A2A2A;
/// Side margin around a lone ring in `render_cell`.
const CELL_MARGIN: f32 = 8.0;

const CARD_WIDTH: f32 = 260.0;
const CARD_PAD: f32 = 14.0;
const CARD_RADIUS: f32 = 12.0;
const TITLE_SIZE: f32 = 13.0;
const TITLE_HEIGHT: f32 = 20.0;
const ROW_SIZE: f32 = 11.5;
const NOTE_SIZE: f32 = 10.5;
const LINE_HEIGHT: f32 = 18.0;
const NOTE_HEIGHT: f32 = 16.0;
const BAR_HEIGHT: f32 = 4.0;
const BAR_ROW_EXTRA: f32 = 8.0;
const ROW_GAP: f32 = 4.0;
const BAR_TRACK_ALPHA: f32 = 0.176;
const BUTTON_PLATE_ALPHA: f32 = 0.10;

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

/// Centres `value` horizontally on `centre_x` with its top at `top`.
#[allow(clippy::too_many_arguments)]
fn draw_centered(
    canvas: &mut Canvas,
    text: &mut TextPainter,
    value: &str,
    (centre_x, top): (f32, f32),
    (size_dip, bold): (f32, bool),
    scale: f32,
    color: u32,
    alpha: f32,
) {
    let Some(mask) = text.render(value, px(size_dip, scale), bold) else {
        return;
    };
    let x = (centre_x - mask.width as f32 / 2.0).round() as i32;
    canvas.draw_mask(&mask, x, top.round() as i32, color, alpha);
}

/// The notch body with its six cells.
pub fn render_panel(views: &[CellView], dpi: u32, text: &mut TextPainter) -> Canvas {
    let s = layout::scale(dpi);
    let (width, height) = layout::body_size(dpi);
    let mut canvas = Canvas::new(width as usize, height as usize);
    // Flush with the screen's top edge: only the bottom corners are rounded.
    let corner = BODY_CORNER * s;
    canvas.fill_round_rect(
        0.0,
        0.0,
        width as f32,
        height as f32,
        [0.0, 0.0, corner, corner],
        0x000000,
        1.0,
    );
    for (index, view) in views.iter().enumerate() {
        let cx = layout::cell_left(index, dpi) + RING * s / 2.0;
        draw_cell(&mut canvas, view, cx, s, text);
    }
    canvas
}

/// One cell: track ring, arcs, glyph mark and the label under it, the ring centred on `cx`.
fn draw_cell(canvas: &mut Canvas, view: &CellView, cx: f32, s: f32, text: &mut TextPainter) {
    let dim = if view.stale { STALE_DIM } else { 1.0 };
    let cy = (PAD_TOP + RING / 2.0) * s;
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
    if let Some(main) = view.main {
        let fraction = f32::from(main) / 100.0;
        canvas.stroke_arc(
            cx,
            cy,
            radius,
            PROGRESS_STROKE * s,
            fraction,
            layout::band_color(fraction),
            dim,
        );
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
            layout::band_color(fraction),
            0.8 * dim,
        );
    }
    // Glyph mark centred in the ring (font cells are taller than their capitals, so
    // nudge up by a tenth of the cell).
    let glyph_size = 8.5;
    draw_centered(
        canvas,
        text,
        view.glyph,
        (cx, cy - glyph_size * s * 0.6),
        (glyph_size, true),
        s,
        if view.problem {
            layout::BAND_WATCH
        } else {
            INK_PRIMARY
        },
        dim,
    );
    let known = view.main.is_some();
    draw_centered(
        canvas,
        text,
        &view.label,
        (cx, (PAD_TOP + RING + LABEL_GAP) * s),
        (11.0, false),
        s,
        if known { INK_PRIMARY } else { INK_SECONDARY },
        1.0,
    );
}

/// A single cell on a transparent canvas (no notch body): what the view shots show for a ring.
pub fn render_cell(view: &CellView, dpi: u32, text: &mut TextPainter) -> Canvas {
    let s = layout::scale(dpi);
    let (_, height) = layout::body_size(dpi);
    let width = ((RING + 2.0 * CELL_MARGIN) * s).round() as usize;
    let mut canvas = Canvas::new(width, height as usize);
    draw_cell(&mut canvas, view, width as f32 / 2.0, s, text);
    canvas
}

fn row_height(row: &Row) -> f32 {
    match row {
        Row::Pair { .. } => LINE_HEIGHT,
        Row::Bar { .. } => LINE_HEIGHT + BAR_ROW_EXTRA,
        Row::Note(_) => NOTE_HEIGHT,
    }
}

/// Index of the row under `y` (card-local device pixels), if any.
pub fn row_at(content: &CardContent, dpi: u32, y: i32) -> Option<usize> {
    let s = layout::scale(dpi);
    let y = y as f32;
    let mut top = CARD_PAD + TITLE_HEIGHT + 6.0;
    for (index, row) in content.rows.iter().enumerate() {
        let height = row_height(row);
        if y >= (top - ROW_GAP / 2.0) * s && y < (top + height + ROW_GAP / 2.0) * s {
            return Some(index);
        }
        top += height + ROW_GAP;
    }
    None
}

/// Size in device pixels of the card for `content`.
pub fn card_size(content: &CardContent, dpi: u32) -> (i32, i32) {
    let s = layout::scale(dpi);
    let rows: f32 = content.rows.iter().map(|r| row_height(r) + ROW_GAP).sum();
    let height = CARD_PAD + TITLE_HEIGHT + 6.0 + rows + CARD_PAD - ROW_GAP;
    (px(CARD_WIDTH, s), px(height, s))
}

fn card_background(canvas: &mut Canvas, scale: f32) {
    let (w, h) = (canvas.width as f32, canvas.height as f32);
    let r = CARD_RADIUS * scale;
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

/// The hover card: title (with optional plan on the right), then rows. `clickable[i]` marks
/// row `i` as a button (primary ink on a faint plate); missing entries are plain text.
pub fn render_card(
    content: &CardContent,
    clickable: &[bool],
    dpi: u32,
    text: &mut TextPainter,
) -> Canvas {
    let s = layout::scale(dpi);
    let (width, height) = card_size(content, dpi);
    let mut canvas = Canvas::new(width as usize, height as usize);
    card_background(&mut canvas, s);
    let left = px(CARD_PAD, s);
    let right = width - left;
    let mut y = CARD_PAD;
    draw_text(
        &mut canvas,
        text,
        &content.title,
        (left, px(y, s)),
        (TITLE_SIZE, true),
        s,
        INK_PRIMARY,
        1.0,
    );
    if let Some(accessory) = &content.accessory {
        let w = text_width(text, accessory, ROW_SIZE, false, s);
        draw_text(
            &mut canvas,
            text,
            accessory,
            (right - w, px(y + 1.0, s)),
            (ROW_SIZE, false),
            s,
            INK_SECONDARY,
            1.0,
        );
    }
    y += TITLE_HEIGHT + 6.0;
    for (index, row) in content.rows.iter().enumerate() {
        let button = clickable.get(index) == Some(&true);
        if button {
            canvas.fill_round_rect(
                left as f32 - 6.0 * s,
                (y - 1.0) * s,
                (right - left) as f32 + 12.0 * s,
                LINE_HEIGHT * s,
                [4.0 * s; 4],
                0xFFFFFF,
                BUTTON_PLATE_ALPHA,
            );
        }
        let label_ink = if button { INK_PRIMARY } else { INK_SECONDARY };
        match row {
            Row::Pair { label, value } => {
                draw_pair(
                    &mut canvas,
                    text,
                    (label, label_ink),
                    value,
                    (left, right),
                    y,
                    s,
                );
            }
            Row::Bar {
                label,
                value,
                fraction,
            } => {
                draw_pair(
                    &mut canvas,
                    text,
                    (label, label_ink),
                    value,
                    (left, right),
                    y,
                    s,
                );
                let bar_top = (y + LINE_HEIGHT + 1.0) * s;
                let bar_width = (right - left) as f32;
                let bar_height = BAR_HEIGHT * s;
                let radius = bar_height / 2.0;
                canvas.fill_round_rect(
                    left as f32,
                    bar_top,
                    bar_width,
                    bar_height,
                    [radius; 4],
                    0xFFFFFF,
                    BAR_TRACK_ALPHA,
                );
                if let Some(f) = fraction.filter(|f| *f > 0.0) {
                    let f = f.clamp(0.0, 1.0);
                    canvas.fill_round_rect(
                        left as f32,
                        bar_top,
                        (bar_width * f).max(bar_height),
                        bar_height,
                        [radius; 4],
                        layout::band_color(f),
                        1.0,
                    );
                }
            }
            Row::Note(note) => {
                draw_text(
                    &mut canvas,
                    text,
                    note,
                    (left, px(y, s)),
                    (NOTE_SIZE, false),
                    s,
                    INK_SECONDARY,
                    1.0,
                );
            }
        }
        y += row_height(row) + ROW_GAP;
    }
    canvas
}

/// Label on the left in secondary ink, value right-aligned in primary ink.
fn draw_pair(
    canvas: &mut Canvas,
    text: &mut TextPainter,
    (label, label_ink): (&str, u32),
    value: &str,
    (left, right): (i32, i32),
    y: f32,
    scale: f32,
) {
    draw_text(
        canvas,
        text,
        label,
        (left, px(y, scale)),
        (ROW_SIZE, false),
        scale,
        label_ink,
        1.0,
    );
    let w = text_width(text, value, ROW_SIZE, false, scale);
    draw_text(
        canvas,
        text,
        value,
        (right - w, px(y, scale)),
        (ROW_SIZE, false),
        scale,
        INK_PRIMARY,
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
    card_background(&mut canvas, s);
    draw_text(
        &mut canvas,
        text,
        "Quit",
        (px(CARD_PAD, s), px((MENU_HEIGHT - LINE_HEIGHT) / 2.0, s)),
        (ROW_SIZE + 0.5, false),
        s,
        INK_PRIMARY,
        1.0,
    );
    canvas
}
