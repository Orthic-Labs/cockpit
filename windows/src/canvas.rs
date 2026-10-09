//! Software canvas for the notch and its cards: anti-aliased rounded rectangles, ring arcs
//! and text masks composited into a premultiplied `0xAARRGGBB` buffer, ready for
//! `UpdateLayeredWindow`. Pure code (no Win32), so the output can be reasoned about on its
//! own. Colours are `0xRRGGBB`; alpha and coverage are `0.0..=1.0`.

use std::f32::consts::TAU;

pub type Rgb = u32;

/// Grey coverage of rendered text (one byte per pixel).
pub struct Mask {
    pub width: usize,
    pub height: usize,
    pub coverage: Vec<u8>,
}

pub struct Canvas {
    pub width: usize,
    pub height: usize,
    /// Premultiplied `0xAARRGGBB`, row-major, top row first.
    pub pixels: Vec<u32>,
}

fn channels(color: Rgb) -> (f32, f32, f32) {
    (
        ((color >> 16) & 0xFF) as f32,
        ((color >> 8) & 0xFF) as f32,
        (color & 0xFF) as f32,
    )
}

impl Canvas {
    pub fn new(width: usize, height: usize) -> Self {
        Self {
            width,
            height,
            pixels: vec![0; width * height],
        }
    }

    /// Source-over blend of `color` at `coverage` onto the premultiplied pixel.
    fn blend(&mut self, x: usize, y: usize, color: Rgb, coverage: f32) {
        let a = coverage.clamp(0.0, 1.0);
        if a <= 0.0 || x >= self.width || y >= self.height {
            return;
        }
        let index = y * self.width + x;
        let dst = self.pixels[index];
        let inv = 1.0 - a;
        let (sr, sg, sb) = channels(color);
        let mix = |source: f32, shift: u32| -> u32 {
            let d = ((dst >> shift) & 0xFF) as f32;
            (source * a + d * inv).round().clamp(0.0, 255.0) as u32
        };
        let out_a = (255.0 * a + ((dst >> 24) & 0xFF) as f32 * inv)
            .round()
            .clamp(0.0, 255.0) as u32;
        self.pixels[index] = (out_a << 24) | (mix(sr, 16) << 16) | (mix(sg, 8) << 8) | mix(sb, 0);
    }

    /// Rounded rectangle with per-corner radii `[top-left, top-right, bottom-right,
    /// bottom-left]`.
    #[allow(clippy::too_many_arguments)]
    pub fn fill_round_rect(
        &mut self,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        radii: [f32; 4],
        color: Rgb,
        alpha: f32,
    ) {
        let x0 = x.floor().max(0.0) as usize;
        let y0 = y.floor().max(0.0) as usize;
        let x1 = ((x + w).ceil().max(0.0) as usize).min(self.width);
        let y1 = ((y + h).ceil().max(0.0) as usize).min(self.height);
        let (cx, cy) = (x + w / 2.0, y + h / 2.0);
        for py in y0..y1 {
            for px in x0..x1 {
                let (fx, fy) = (px as f32 + 0.5, py as f32 + 0.5);
                let radius = match (fx < cx, fy < cy) {
                    (true, true) => radii[0],
                    (false, true) => radii[1],
                    (false, false) => radii[2],
                    (true, false) => radii[3],
                }
                .min(w / 2.0)
                .min(h / 2.0)
                .max(0.0);
                let qx = (fx - cx).abs() - w / 2.0 + radius;
                let qy = (fy - cy).abs() - h / 2.0 + radius;
                let outside = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt();
                let distance = outside + qx.max(qy).min(0.0) - radius;
                self.blend(px, py, color, (0.5 - distance) * alpha);
            }
        }
    }

    /// Ring stroke centred on `radius`. `fraction` sweeps clockwise from 12 o'clock with
    /// round caps; `1.0` is a closed ring; `0.0` draws nothing.
    #[allow(clippy::too_many_arguments)]
    pub fn stroke_arc(
        &mut self,
        cx: f32,
        cy: f32,
        radius: f32,
        width: f32,
        fraction: f32,
        color: Rgb,
        alpha: f32,
    ) {
        let sweep = fraction.clamp(0.0, 1.0);
        if sweep <= 0.0 {
            return;
        }
        let half = width / 2.0;
        let closed = sweep >= 0.999;
        let end_angle = sweep * TAU;
        let start_cap = (cx, cy - radius);
        let end_cap = (cx + radius * end_angle.sin(), cy - radius * end_angle.cos());
        let reach = radius + half + 1.0;
        let x0 = (cx - reach).floor().max(0.0) as usize;
        let y0 = (cy - reach).floor().max(0.0) as usize;
        let x1 = ((cx + reach).ceil().max(0.0) as usize).min(self.width);
        let y1 = ((cy + reach).ceil().max(0.0) as usize).min(self.height);
        for py in y0..y1 {
            for px in x0..x1 {
                let (fx, fy) = (px as f32 + 0.5, py as f32 + 0.5);
                let (dx, dy) = (fx - cx, fy - cy);
                let radial = (0.5 - ((dx.hypot(dy) - radius).abs() - half)).clamp(0.0, 1.0);
                let coverage = if closed {
                    radial
                } else {
                    let mut theta = dx.atan2(-dy);
                    if theta < 0.0 {
                        theta += TAU;
                    }
                    let along_start = theta * radius;
                    let along_end = (end_angle - theta) * radius;
                    let body = (0.5 + along_start.min(along_end)).clamp(0.0, 1.0) * radial;
                    let cap = |c: (f32, f32)| (0.5 - ((fx - c.0).hypot(fy - c.1) - half)).clamp(0.0, 1.0);
                    body.max(cap(start_cap)).max(cap(end_cap))
                };
                self.blend(px, py, color, coverage * alpha);
            }
        }
    }

    /// Composites a text mask with its top-left corner at `(x, y)`.
    pub fn draw_mask(&mut self, mask: &Mask, x: i32, y: i32, color: Rgb, alpha: f32) {
        for row in 0..mask.height {
            let py = y + row as i32;
            if py < 0 {
                continue;
            }
            for column in 0..mask.width {
                let px = x + column as i32;
                if px < 0 {
                    continue;
                }
                let level = mask.coverage[row * mask.width + column];
                if level != 0 {
                    self.blend(px as usize, py as usize, color, f32::from(level) / 255.0 * alpha);
                }
            }
        }
    }
}
