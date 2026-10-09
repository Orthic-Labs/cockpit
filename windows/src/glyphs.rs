//! Vector marks for the notch rings, matching the Mac notch: the Claude starburst and the
//! OpenAI knot (outlines traced for the Mac notch, filled even-odd) and hand-drawn stroke
//! equivalents of the SF Symbols the Mac uses for System (`cpu`), Disks (`internaldrive`)
//! and Send (`paperplane`). Coordinates of the strokes are in DIPs from the ring centre.

use crate::canvas::Canvas;
use crate::layout::DESIGN;

/// The glyph frame the Mac draws a mark in (46 design px).
const GLYPH_SIZE: f32 = 46.0 * DESIGN;
/// Stroke weight of the symbol marks, in DIPs.
const STROKE: f32 = 1.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Glyph {
    Claude,
    OpenAi,
    Chip,
    Drive,
    Plane,
}

impl Glyph {
    /// Mark for a `Cell::glyph` key; the Send cell's key is `send::GLYPH`.
    pub fn from_key(key: &str) -> Self {
        match key {
            "claude" => Glyph::Claude,
            "openai" => Glyph::OpenAi,
            "cpu" => Glyph::Chip,
            "disk" => Glyph::Drive,
            _ => Glyph::Plane,
        }
    }
}

/// Draws `glyph` centred on `centre` at `scale` device pixels per DIP.
pub fn draw(
    canvas: &mut Canvas,
    glyph: Glyph,
    centre: (f32, f32),
    scale: f32,
    color: u32,
    alpha: f32,
) {
    match glyph {
        Glyph::Claude => fill(canvas, CLAUDE, 0.97, centre, scale, color, alpha),
        Glyph::OpenAi => fill(canvas, OPENAI, 0.94, centre, scale, color, alpha),
        Glyph::Chip => chip(canvas, centre, scale, color, alpha),
        Glyph::Drive => drive(canvas, centre, scale, color, alpha),
        Glyph::Plane => plane(canvas, centre, scale, color, alpha),
    }
}

fn fill(
    canvas: &mut Canvas,
    outline: &[&[(f32, f32)]],
    optical: f32,
    (cx, cy): (f32, f32),
    scale: f32,
    color: u32,
    alpha: f32,
) {
    let size = GLYPH_SIZE * optical * scale;
    let loops: Vec<Vec<(f32, f32)>> = outline
        .iter()
        .map(|points| {
            points
                .iter()
                .map(|&(x, y)| (cx + (x - 0.5) * size, cy + (y - 0.5) * size))
                .collect()
        })
        .collect();
    canvas.fill_polygon(&loops, color, alpha);
}

/// Rounded rectangle of half-extents `(hx, hy)` and corner radius `r`, as a closed outline
/// around the origin.
fn rounded_rect(hx: f32, hy: f32, r: f32) -> Vec<(f32, f32)> {
    const STEPS: usize = 6;
    let mut points = Vec::new();
    // Corner centres with the angle each arc starts at (clockwise, y down).
    let corners = [
        (hx - r, hy - r, 0.0f32),
        (-hx + r, hy - r, 90.0),
        (-hx + r, -hy + r, 180.0),
        (hx - r, -hy + r, 270.0),
    ];
    for (ox, oy, start) in corners {
        for step in 0..=STEPS {
            let angle = (start + 90.0 * step as f32 / STEPS as f32).to_radians();
            points.push((ox + r * angle.cos(), oy + r * angle.sin()));
        }
    }
    points
}

fn place(points: &[(f32, f32)], (cx, cy): (f32, f32), scale: f32) -> Vec<(f32, f32)> {
    points
        .iter()
        .map(|&(x, y)| (cx + x * scale, cy + y * scale))
        .collect()
}

fn line(
    canvas: &mut Canvas,
    ends: [(f32, f32); 2],
    centre: (f32, f32),
    scale: f32,
    ink: (u32, f32),
) {
    canvas.stroke_polyline(
        &place(&ends, centre, scale),
        STROKE * scale,
        false,
        ink.0,
        ink.1,
    );
}

/// SF Symbol `cpu`: a chip with a square die and three pins on each side.
fn chip(canvas: &mut Canvas, centre: (f32, f32), scale: f32, color: u32, alpha: f32) {
    let width = STROKE * scale;
    canvas.stroke_polyline(
        &place(&rounded_rect(5.7, 5.7, 1.0), centre, scale),
        width,
        true,
        color,
        alpha,
    );
    canvas.stroke_polyline(
        &place(&rounded_rect(3.0, 3.0, 0.4), centre, scale),
        width,
        true,
        color,
        alpha,
    );
    for offset in [-2.55f32, 0.0, 2.55] {
        for sign in [-1.0f32, 1.0] {
            let ink = (color, alpha);
            line(
                canvas,
                [(offset, sign * 5.7), (offset, sign * 7.5)],
                centre,
                scale,
                ink,
            );
            line(
                canvas,
                [(sign * 5.7, offset), (sign * 7.5, offset)],
                centre,
                scale,
                ink,
            );
        }
    }
}

/// SF Symbol `internaldrive`: a trapezoid lid over a rounded body with five vent ticks.
fn drive(canvas: &mut Canvas, centre: (f32, f32), scale: f32, color: u32, alpha: f32) {
    let width = STROKE * scale;
    let lid = [
        (-6.7, 0.5),
        (-4.3, -4.6),
        (-3.6, -5.15),
        (3.6, -5.15),
        (4.3, -4.6),
        (6.7, 0.5),
    ];
    canvas.stroke_polyline(&place(&lid, centre, scale), width, false, color, alpha);
    let body: Vec<(f32, f32)> = rounded_rect(6.9, 2.45, 2.2)
        .into_iter()
        .map(|(x, y)| (x, y + 2.95))
        .collect();
    canvas.stroke_polyline(&place(&body, centre, scale), width, true, color, alpha);
    for x in [-1.6f32, -0.2, 1.3, 2.9, 4.4] {
        canvas.stroke_polyline(
            &place(&[(x, 1.9), (x, 3.6)], centre, scale),
            0.8 * scale,
            false,
            color,
            alpha * 0.8,
        );
    }
}

/// SF Symbol `paperplane`: the outline of the plane and its fold line.
fn plane(canvas: &mut Canvas, centre: (f32, f32), scale: f32, color: u32, alpha: f32) {
    let width = 1.05 * scale;
    let outline = [(6.4, -6.9), (-6.9, -1.5), (-0.6, 0.5), (1.3, 6.4)];
    canvas.stroke_polyline(&place(&outline, centre, scale), width, true, color, alpha);
    canvas.stroke_polyline(
        &place(&[(6.4, -6.9), (-0.6, 0.5)], centre, scale),
        width,
        false,
        color,
        alpha,
    );
}

// ---- card symbols -------------------------------------------------------------------------------
//
// The SF Symbols the Mac cards use (pills, device rows, transfer discs), redrawn as strokes in a
// unit box: `-1.0..=1.0` spans the symbol's frame.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Symbol {
    Xmark,
    Refresh,
    Stop,
    Clock,
    DownCircle,
    DownApp,
    Cycle,
    Undo,
    Box,
    Folder,
    Copy,
    Compass,
    Plane,
    Phone,
    Laptop,
    Desktop,
    Globe,
    Terminal,
    Server,
    Check,
    Hand,
    Warning,
}

impl Symbol {
    /// The symbol for an SF Symbol name in a view fixture; the display for any other.
    pub fn from_name(name: &str) -> Self {
        match name {
            "iphone" => Symbol::Phone,
            "laptopcomputer" => Symbol::Laptop,
            "globe" => Symbol::Globe,
            "terminal" => Symbol::Terminal,
            "server.rack" => Symbol::Server,
            "checkmark.circle" => Symbol::Check,
            "hand.raised" => Symbol::Hand,
            "exclamationmark.triangle" => Symbol::Warning,
            _ => Symbol::Desktop,
        }
    }
}

/// The large colour icons a card leads with when there is no real file icon to show.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tile {
    Folder,
    Message,
    Package,
    App,
}

/// Strokes in unit coordinates around a centre.
struct Brush<'a> {
    canvas: &'a mut Canvas,
    centre: (f32, f32),
    half: f32,
    width: f32,
    color: u32,
    alpha: f32,
}

type Point = (f32, f32);

/// Points of a circular arc, angles in degrees clockwise from 3 o'clock (y points down).
fn arc(centre: Point, radius: f32, from: f32, to: f32) -> Vec<Point> {
    let steps = ((to - from).abs() / 10.0).ceil().max(1.0) as usize;
    (0..=steps)
        .map(|step| {
            let angle = (from + (to - from) * step as f32 / steps as f32).to_radians();
            (
                centre.0 + radius * angle.cos(),
                centre.1 + radius * angle.sin(),
            )
        })
        .collect()
}

/// A closed ellipse as an outline.
fn ellipse(centre: Point, rx: f32, ry: f32) -> Vec<Point> {
    arc((0.0, 0.0), 1.0, 0.0, 360.0)
        .into_iter()
        .map(|(x, y)| (centre.0 + x * rx, centre.1 + y * ry))
        .collect()
}

fn shifted(points: Vec<Point>, (dx, dy): Point) -> Vec<Point> {
    points.into_iter().map(|(x, y)| (x + dx, y + dy)).collect()
}

impl Brush<'_> {
    fn at(&self, (x, y): Point) -> Point {
        (self.centre.0 + x * self.half, self.centre.1 + y * self.half)
    }

    fn path(&mut self, points: &[Point], closed: bool) {
        let placed: Vec<Point> = points.iter().map(|&p| self.at(p)).collect();
        self.canvas
            .stroke_polyline(&placed, self.width, closed, self.color, self.alpha);
    }

    fn circle(&mut self, centre: Point, radius: f32) {
        let (x, y) = self.at(centre);
        self.canvas.stroke_arc(
            x,
            y,
            radius * self.half,
            self.width,
            1.0,
            self.color,
            self.alpha,
        );
    }

    fn fill(&mut self, points: &[Point]) {
        let placed: Vec<Point> = points.iter().map(|&p| self.at(p)).collect();
        self.canvas.fill_polygon(&[placed], self.color, self.alpha);
    }

    fn rect(&mut self, (hx, hy): Point, radius: f32, centre: Point) {
        self.path(&shifted(rounded_rect(hx, hy, radius), centre), true);
    }

    /// An arrow head: a chevron with its tip at `tip`, pointing along `dir`.
    fn head(&mut self, tip: Point, dir: Point, len: f32) {
        let side = (-dir.1, dir.0);
        let back = (tip.0 - dir.0 * len, tip.1 - dir.1 * len);
        let wing = len * 0.85;
        self.path(
            &[
                (back.0 + side.0 * wing, back.1 + side.1 * wing),
                tip,
                (back.0 - side.0 * wing, back.1 - side.1 * wing),
            ],
            false,
        );
    }

    /// The tangent of a clockwise circle at `angle` degrees.
    fn tangent(angle: f32) -> Point {
        let a = angle.to_radians();
        (-a.sin(), a.cos())
    }
}

/// Draws `symbol` in a square of `size` device pixels centred on `centre`.
pub fn symbol(
    canvas: &mut Canvas,
    symbol: Symbol,
    centre: (f32, f32),
    size: f32,
    color: u32,
    alpha: f32,
) {
    if symbol == Symbol::Plane {
        plane(canvas, centre, size / 15.0, color, alpha);
        return;
    }
    let mut b = Brush {
        canvas,
        centre,
        half: size / 2.0,
        width: (size * 0.085).max(1.0),
        color,
        alpha,
    };
    match symbol {
        Symbol::Xmark => {
            b.width = (size * 0.14).max(1.2);
            b.path(&[(-0.55, -0.55), (0.55, 0.55)], false);
            b.path(&[(0.55, -0.55), (-0.55, 0.55)], false);
        }
        Symbol::Refresh => {
            let end = 290.0;
            b.path(&arc((0.0, 0.0), 0.72, -10.0, end), false);
            let a = end.to_radians();
            b.head((0.72 * a.cos(), 0.72 * a.sin()), Brush::tangent(end), 0.32);
        }
        Symbol::Stop => {
            b.circle((0.0, 0.0), 0.86);
            b.fill(&rounded_rect(0.3, 0.3, 0.08));
        }
        Symbol::Clock => {
            b.circle((0.0, 0.0), 0.86);
            b.path(&[(0.0, -0.5), (0.0, 0.0), (0.34, 0.34)], false);
        }
        Symbol::DownCircle => {
            b.circle((0.0, 0.0), 0.86);
            b.path(&[(0.0, -0.42), (0.0, 0.4)], false);
            b.path(&[(-0.3, 0.1), (0.0, 0.42), (0.3, 0.1)], false);
        }
        Symbol::DownApp => {
            b.rect((0.82, 0.82), 0.3, (0.0, 0.0));
            b.path(&[(0.0, -0.4), (0.0, 0.34)], false);
            b.path(&[(-0.28, 0.06), (0.0, 0.36), (0.28, 0.06)], false);
        }
        Symbol::Cycle => {
            b.path(&arc((0.0, 0.0), 0.68, -150.0, -30.0), false);
            b.head(
                (
                    0.68 * (-30f32).to_radians().cos(),
                    0.68 * (-30f32).to_radians().sin(),
                ),
                Brush::tangent(-30.0),
                0.3,
            );
            b.path(&arc((0.0, 0.0), 0.68, 30.0, 150.0), false);
            b.head(
                (
                    0.68 * 150f32.to_radians().cos(),
                    0.68 * 150f32.to_radians().sin(),
                ),
                Brush::tangent(150.0),
                0.3,
            );
        }
        Symbol::Undo => {
            let mut turn = vec![(-0.55, -0.45), (0.2, -0.45)];
            turn.extend(arc((0.2, 0.0), 0.45, -90.0, 90.0));
            turn.push((-0.4, 0.45));
            b.path(&turn, false);
            b.head((-0.62, -0.45), (-1.0, 0.0), 0.34);
        }
        Symbol::Box => {
            b.path(
                &[(0.0, -0.88), (0.8, -0.44), (0.0, 0.0), (-0.8, -0.44)],
                true,
            );
            b.path(
                &[
                    (-0.8, -0.44),
                    (-0.8, 0.46),
                    (0.0, 0.9),
                    (0.8, 0.46),
                    (0.8, -0.44),
                ],
                false,
            );
            b.path(&[(0.0, 0.0), (0.0, 0.9)], false);
        }
        Symbol::Folder => {
            b.path(
                &[
                    (-0.88, 0.66),
                    (-0.88, -0.66),
                    (-0.2, -0.66),
                    (0.06, -0.38),
                    (0.88, -0.38),
                    (0.88, 0.66),
                ],
                true,
            );
        }
        Symbol::Copy => {
            b.rect((0.46, 0.62), 0.12, (0.22, 0.2));
            b.path(
                &[
                    (-0.24, 0.42),
                    (-0.66, 0.42),
                    (-0.66, -0.82),
                    (0.2, -0.82),
                    (0.2, -0.42),
                ],
                false,
            );
        }
        Symbol::Compass => {
            b.circle((0.0, 0.0), 0.86);
            b.path(
                &[(0.38, -0.38), (0.12, 0.12), (-0.38, 0.38), (-0.12, -0.12)],
                true,
            );
        }
        Symbol::Phone => {
            b.rect((0.38, 0.86), 0.2, (0.0, 0.0));
            b.path(&[(-0.14, 0.66), (0.14, 0.66)], false);
        }
        Symbol::Laptop => {
            b.rect((0.7, 0.4), 0.1, (0.0, -0.22));
            b.path(&[(-0.95, 0.52), (0.95, 0.52)], false);
        }
        Symbol::Desktop => {
            b.rect((0.86, 0.5), 0.12, (0.0, -0.22));
            b.path(&[(0.0, 0.28), (0.0, 0.62)], false);
            b.path(&[(-0.36, 0.7), (0.36, 0.7)], false);
        }
        Symbol::Globe => {
            b.circle((0.0, 0.0), 0.86);
            b.path(&ellipse((0.0, 0.0), 0.38, 0.86), true);
            b.path(&[(-0.86, 0.0), (0.86, 0.0)], false);
        }
        Symbol::Terminal => {
            b.rect((0.88, 0.68), 0.14, (0.0, 0.0));
            b.path(&[(-0.5, -0.25), (-0.15, 0.0), (-0.5, 0.25)], false);
            b.path(&[(0.0, 0.28), (0.45, 0.28)], false);
        }
        Symbol::Server => {
            b.rect((0.82, 0.3), 0.1, (0.0, -0.5));
            b.rect((0.82, 0.3), 0.1, (0.0, 0.5));
            b.path(&[(-0.5, -0.5), (-0.4, -0.5)], false);
            b.path(&[(-0.5, 0.5), (-0.4, 0.5)], false);
        }
        Symbol::Check => {
            b.circle((0.0, 0.0), 0.86);
            b.path(&[(-0.4, 0.02), (-0.1, 0.32), (0.42, -0.3)], false);
        }
        Symbol::Hand => {
            b.rect((0.5, 0.42), 0.3, (0.0, 0.42));
            for (x, top) in [(-0.38, -0.5), (-0.13, -0.8), (0.13, -0.85), (0.38, -0.55)] {
                let half = (0.1, (0.1 - top) / 2.0);
                b.rect(half, 0.1, (x, (top + 0.1) / 2.0));
            }
        }
        Symbol::Warning => {
            b.path(&[(0.0, -0.8), (0.9, 0.74), (-0.9, 0.74)], true);
            b.path(&[(0.0, -0.22), (0.0, 0.26)], false);
            b.path(&[(0.0, 0.5), (0.0, 0.5)], false);
        }
        Symbol::Plane => {}
    }
}

/// A large colour icon filling a square of `size` device pixels centred on `centre`.
pub fn tile(canvas: &mut Canvas, tile: Tile, (cx, cy): (f32, f32), size: f32) {
    let half = size / 2.0;
    let rect = |canvas: &mut Canvas, (x0, y0, x1, y1): (f32, f32, f32, f32), r: f32, color: u32| {
        canvas.fill_round_rect(
            cx + x0 * half,
            cy + y0 * half,
            (x1 - x0) * half,
            (y1 - y0) * half,
            [r * half; 4],
            color,
            1.0,
        );
    };
    match tile {
        Tile::Folder => {
            rect(canvas, (-0.9, -0.72, 0.9, 0.78), 0.14, 0x4BA3E6);
            rect(canvas, (-0.9, -0.82, -0.1, -0.5), 0.1, 0x4BA3E6);
            rect(canvas, (-0.9, -0.42, 0.9, 0.78), 0.14, 0x78C5F6);
        }
        Tile::Message => {
            rect(canvas, (-0.92, -0.92, 0.92, 0.92), 0.4, 0x5CB85C);
            let bubble: Vec<Point> = ellipse((0.0, -0.08), 0.58, 0.44)
                .into_iter()
                .map(|(x, y)| (cx + x * half, cy + y * half))
                .collect();
            canvas.fill_polygon(&[bubble], 0xFFFFFF, 1.0);
            let tail: Vec<Point> = [(-0.38, 0.26), (-0.62, 0.6), (-0.08, 0.36)]
                .iter()
                .map(|&(x, y)| (cx + x * half, cy + y * half))
                .collect();
            canvas.fill_polygon(&[tail], 0xFFFFFF, 1.0);
        }
        Tile::Package => {
            rect(canvas, (-0.92, -0.92, 0.92, 0.92), 0.4, 0x4B5567);
            symbol(canvas, Symbol::Box, (cx, cy), size * 0.56, 0xFFFFFF, 1.0);
        }
        Tile::App => {
            rect(canvas, (-0.92, -0.92, 0.92, 0.92), 0.4, 0x14141A);
            canvas.stroke_arc(cx, cy, 0.38 * half, 0.13 * half, 0.78, 0xFFFFFF, 1.0);
        }
    }
}

#[rustfmt::skip]
const CLAUDE: &[&[(f32, f32)]] = &[
    &[
        (0.2879, 0.0108), (0.2667, 0.0223), (0.2423, 0.0516), (0.2427, 0.0873),
        (0.2611, 0.1275), (0.3425, 0.2606), (0.3879, 0.3474), (0.3888, 0.3670),
        (0.3695, 0.3643), (0.2014, 0.2351), (0.1878, 0.2200), (0.1552, 0.1998),
        (0.1253, 0.1950), (0.1111, 0.1995), (0.0877, 0.2258), (0.0887, 0.2565),
        (0.0953, 0.2714), (0.1180, 0.2961), (0.1661, 0.3284), (0.1791, 0.3426),
        (0.2965, 0.4155), (0.3095, 0.4297), (0.3940, 0.4803), (0.3974, 0.4946),
        (0.3767, 0.5004), (0.2177, 0.4830), (0.0404, 0.4741), (0.0186, 0.4808),
        (0.0129, 0.4990), (0.0276, 0.5245), (0.0649, 0.5375), (0.3801, 0.5449),
        (0.3943, 0.5488), (0.3979, 0.5574), (0.3773, 0.5800), (0.3130, 0.6116),
        (0.2589, 0.6470), (0.2341, 0.6564), (0.1360, 0.7209), (0.1142, 0.7485),
        (0.1149, 0.7681), (0.1416, 0.7866), (0.1912, 0.7787), (0.4120, 0.6320),
        (0.4270, 0.6288), (0.4321, 0.6332), (0.4292, 0.6454), (0.3940, 0.6799),
        (0.3428, 0.7530), (0.2416, 0.8771), (0.2334, 0.9073), (0.2403, 0.9269),
        (0.2558, 0.9336), (0.2734, 0.9308), (0.3431, 0.8618), (0.4700, 0.6899),
        (0.4807, 0.6667), (0.4915, 0.6611), (0.5001, 0.6669), (0.4995, 0.6906),
        (0.4474, 0.9569), (0.4618, 0.9946), (0.4901, 1.0070), (0.5162, 0.9945),
        (0.5228, 0.9833), (0.5369, 0.9018), (0.5520, 0.7109), (0.5593, 0.6981),
        (0.5814, 0.7096), (0.6328, 0.7971), (0.7227, 0.9242), (0.7395, 0.9333),
        (0.7662, 0.9315), (0.7775, 0.9241), (0.7830, 0.9106), (0.7785, 0.8597),
        (0.6882, 0.7238), (0.6753, 0.7115), (0.6765, 0.6956), (0.6875, 0.6957),
        (0.7252, 0.7339), (0.8721, 0.8489), (0.8842, 0.8515), (0.8959, 0.8468),
        (0.9038, 0.8373), (0.9046, 0.8256), (0.8530, 0.7662), (0.6984, 0.6269),
        (0.6775, 0.6016), (0.6778, 0.5933), (0.6885, 0.5908), (0.8106, 0.6247),
        (0.9440, 0.6533), (0.9782, 0.6467), (1.0094, 0.6185), (0.9968, 0.5927),
        (0.9637, 0.5647), (0.8743, 0.5599), (0.8332, 0.5528), (0.7469, 0.5529),
        (0.7252, 0.5475), (0.7138, 0.5382), (0.7174, 0.5299), (0.7308, 0.5244),
        (0.9772, 0.4740), (0.9904, 0.4655), (0.9985, 0.4514), (1.0037, 0.4324),
        (1.0001, 0.4183), (0.9889, 0.4107), (0.9616, 0.4064), (0.8604, 0.4200),
        (0.7578, 0.4394), (0.7164, 0.4523), (0.7010, 0.4468), (0.7391, 0.3769),
        (0.8619, 0.2207), (0.8733, 0.1749), (0.8678, 0.1520), (0.8528, 0.1331),
        (0.8338, 0.1217), (0.8169, 0.1215), (0.7888, 0.1313), (0.7199, 0.2007),
        (0.6224, 0.3290), (0.6034, 0.3517), (0.5941, 0.3541), (0.5878, 0.3448),
        (0.5875, 0.3285), (0.6249, 0.1713), (0.6378, 0.0744), (0.6253, 0.0430),
        (0.6043, 0.0257), (0.5890, 0.0265), (0.5661, 0.0463), (0.5471, 0.0805),
        (0.5369, 0.2279), (0.5259, 0.2877), (0.5223, 0.3461), (0.5165, 0.3730),
        (0.5080, 0.3786), (0.4728, 0.2909), (0.3941, 0.1409), (0.3647, 0.0645),
        (0.3439, 0.0282), (0.3305, 0.0183), (0.3013, 0.0095), (0.2880, 0.0108),
    ],
];

#[rustfmt::skip]
// Traced outline coordinates; values near pi/4 are coincidence, not the constant.
#[allow(clippy::approx_constant)]
const OPENAI: &[&[(f32, f32)]] = &[
    &[
        (0.4234, -0.0272), (0.3541, -0.0187), (0.3013, 0.0040), (0.2422, 0.0495),
        (0.2012, 0.1069), (0.1823, 0.1455), (0.1435, 0.1661), (0.1104, 0.1756),
        (0.0468, 0.2232), (0.0013, 0.2863), (-0.0216, 0.3430), (-0.0275, 0.4147),
        (-0.0212, 0.4800), (0.0021, 0.5354), (0.0380, 0.5897), (0.0271, 0.6302),
        (0.0242, 0.6634), (0.0281, 0.7123), (0.0453, 0.7686), (0.0889, 0.8376),
        (0.1131, 0.8638), (0.1823, 0.9074), (0.2413, 0.9266), (0.3060, 0.9300),
        (0.3346, 0.9264), (0.3516, 0.9307), (0.4021, 0.9710), (0.4298, 0.9808),
        (0.4562, 0.9973), (0.5234, 1.0093), (0.5785, 1.0094), (0.6468, 0.9924),
        (0.7148, 0.9513), (0.7772, 0.8832), (0.8042, 0.8303), (0.8365, 0.8205),
        (0.8821, 0.7975), (0.9458, 0.7449), (0.9740, 0.7075), (0.9957, 0.6627),
        (1.0095, 0.5968), (1.0090, 0.5479), (0.9945, 0.4764), (0.9451, 0.3970),
        (0.9551, 0.3353), (0.9516, 0.2607), (0.9295, 0.1997), (0.8836, 0.1329),
        (0.8279, 0.0900), (0.7854, 0.0679), (0.7109, 0.0532), (0.6410, 0.0590),
        (0.6009, 0.0243), (0.5658, 0.0021), (0.5325, -0.0075), (0.5138, -0.0192),
        (0.4237, -0.0273),
    ],
    &[
        (0.5915, 0.6193), (0.6003, 0.6198), (0.6051, 0.6294), (0.6065, 0.6987),
        (0.6030, 0.7094), (0.5791, 0.7311), (0.5493, 0.7434), (0.4991, 0.7768),
        (0.4755, 0.7856), (0.3803, 0.8423), (0.3101, 0.8617), (0.2639, 0.8607),
        (0.2101, 0.8447), (0.1602, 0.8126), (0.1332, 0.7824), (0.1099, 0.7456),
        (0.0966, 0.6858), (0.0971, 0.6532), (0.1028, 0.6439), (0.1253, 0.6483),
        (0.1529, 0.6679), (0.1798, 0.6772), (0.2300, 0.7111), (0.2579, 0.7211),
        (0.3071, 0.7554), (0.3359, 0.7601), (0.3530, 0.7554), (0.5913, 0.6194),
    ],
    &[
        (0.1549, 0.2337), (0.1661, 0.2362), (0.1713, 0.2520), (0.1740, 0.4890),
        (0.2443, 0.5383), (0.3815, 0.6098), (0.3978, 0.6261), (0.4244, 0.6356),
        (0.4284, 0.6478), (0.4203, 0.6616), (0.3691, 0.6910), (0.3509, 0.6950),
        (0.3314, 0.6910), (0.3149, 0.6770), (0.2011, 0.6118), (0.1787, 0.6039),
        (0.1148, 0.5618), (0.0696, 0.5126), (0.0463, 0.4673), (0.0413, 0.4222),
        (0.0428, 0.3713), (0.0661, 0.3121), (0.1072, 0.2634), (0.1340, 0.2422),
        (0.1549, 0.2337),
    ],
    &[
        (0.6595, 0.4590), (0.6735, 0.4607), (0.7335, 0.5002), (0.7409, 0.5112),
        (0.7407, 0.7673), (0.7335, 0.8119), (0.6907, 0.8831), (0.6628, 0.9073),
        (0.6193, 0.9293), (0.5683, 0.9410), (0.4861, 0.9339), (0.4346, 0.9106),
        (0.4283, 0.8998), (0.4330, 0.8914), (0.4606, 0.8724), (0.5577, 0.8212),
        (0.5741, 0.8071), (0.5975, 0.7986), (0.6123, 0.7854), (0.6347, 0.7751),
        (0.6475, 0.7604), (0.6525, 0.7361), (0.6513, 0.4738), (0.6592, 0.4591),
    ],
    &[
        (0.4201, 0.0407), (0.4901, 0.0454), (0.5415, 0.0677), (0.5485, 0.0825),
        (0.5360, 0.1008), (0.5091, 0.1112), (0.4589, 0.1456), (0.3563, 0.1987),
        (0.3290, 0.2258), (0.3275, 0.5024), (0.3233, 0.5161), (0.3135, 0.5208),
        (0.3039, 0.5183), (0.2881, 0.5027), (0.2618, 0.4935), (0.2379, 0.4671),
        (0.2377, 0.2259), (0.2419, 0.1878), (0.2623, 0.1376), (0.3041, 0.0883),
        (0.3313, 0.0668), (0.3713, 0.0482), (0.4197, 0.0407),
    ],
    &[
        (0.6793, 0.1223), (0.7157, 0.1231), (0.7641, 0.1328), (0.8047, 0.1561),
        (0.8276, 0.1768), (0.8625, 0.2233), (0.8807, 0.2686), (0.8876, 0.3190),
        (0.8831, 0.3376), (0.8696, 0.3399), (0.8290, 0.3206), (0.7808, 0.2871),
        (0.7555, 0.2771), (0.7028, 0.2432), (0.6491, 0.2198), (0.6010, 0.2420),
        (0.3950, 0.3609), (0.3770, 0.3587), (0.3753, 0.2836), (0.3855, 0.2637),
        (0.6109, 0.1354), (0.6793, 0.1223),
    ],
    &[
        (0.6168, 0.2853), (0.6369, 0.2862), (0.8593, 0.4128), (0.9080, 0.4616),
        (0.9290, 0.4972), (0.9405, 0.5690), (0.9306, 0.6410), (0.9050, 0.6876),
        (0.8550, 0.7354), (0.8203, 0.7467), (0.8106, 0.7381), (0.8105, 0.5166),
        (0.8003, 0.4834), (0.7040, 0.4293), (0.6883, 0.4155), (0.6661, 0.4077),
        (0.5469, 0.3401), (0.5506, 0.3260), (0.6001, 0.2999), (0.6168, 0.2853),
    ],
    &[
        (0.4779, 0.3668), (0.5139, 0.3713), (0.5650, 0.4061), (0.5917, 0.4164),
        (0.6023, 0.4290), (0.6061, 0.4482), (0.6032, 0.5543), (0.5631, 0.5821),
        (0.4922, 0.6155), (0.4806, 0.6145), (0.4134, 0.5801), (0.3827, 0.5553),
        (0.3749, 0.5304), (0.3747, 0.4480), (0.3821, 0.4243), (0.4778, 0.3668),
    ],
];
