//! CPU rasterizer primitives for the overlay editor.
//!
//! Two surfaces, one API: every routine works against either
//!   - a softbuffer-style `&mut [u32]` (XRGB, used for live preview), or
//!   - an `&mut ImageBuffer<Rgba<u8>, Vec<u8>>` (used at commit time when we
//!     burn annotations into the cropped output PNG).
//!
//! We pick correctness + readability over speed — selections are typically
//! a few hundred pixels per side and we redraw per mouse-move, but a naive
//! Bresenham + circular brush easily holds 60 fps in that regime.

use image::{ImageBuffer, Rgba};
use framefang_core::annotation::{Annotation, AnnotationKind, Point2, Stroke};
use framefang_core::color::Rgba as FramefangRgba;

/// Anything we can stamp pixels into. Implemented for the live softbuffer
/// (`&mut [u32]`) and for the output bitmap (`&mut ImageBuffer<Rgba<u8>>`).
pub trait Surface {
    fn width(&self)  -> i32;
    fn height(&self) -> i32;
    /// Read the destination pixel as RGBA (alpha is opaque for u32 buffers).
    fn read(&self, x: i32, y: i32) -> [u8; 4];
    /// Write a fully-opaque RGB pixel. Alpha is dropped on u32 buffers.
    fn write(&mut self, x: i32, y: i32, rgba: [u8; 4]);
}

pub struct U32Surface<'a> {
    pub buf:    &'a mut [u32],
    pub stride: i32,
    pub height: i32,
}

impl<'a> Surface for U32Surface<'a> {
    fn width(&self)  -> i32 { self.stride }
    fn height(&self) -> i32 { self.height }
    fn read(&self, x: i32, y: i32) -> [u8; 4] {
        let p = self.buf[(y * self.stride + x) as usize];
        [((p >> 16) & 0xFF) as u8, ((p >> 8) & 0xFF) as u8, (p & 0xFF) as u8, 0xFF]
    }
    fn write(&mut self, x: i32, y: i32, rgba: [u8; 4]) {
        self.buf[(y * self.stride + x) as usize] =
            ((rgba[0] as u32) << 16) | ((rgba[1] as u32) << 8) | rgba[2] as u32;
    }
}

pub struct ImageSurface<'a>(pub &'a mut ImageBuffer<Rgba<u8>, Vec<u8>>);

impl<'a> Surface for ImageSurface<'a> {
    fn width(&self)  -> i32 { self.0.width()  as i32 }
    fn height(&self) -> i32 { self.0.height() as i32 }
    fn read(&self, x: i32, y: i32) -> [u8; 4] {
        self.0.get_pixel(x as u32, y as u32).0
    }
    fn write(&mut self, x: i32, y: i32, rgba: [u8; 4]) {
        self.0.put_pixel(x as u32, y as u32, Rgba(rgba));
    }
}

// ── pixel-level ops ─────────────────────────────────────────────────────────

fn in_bounds<S: Surface>(s: &S, x: i32, y: i32) -> bool {
    x >= 0 && x < s.width() && y >= 0 && y < s.height()
}

/// Source-over blend `src` (premultiplied later — we keep it straight here)
/// onto whatever's at (x, y).
fn blend<S: Surface>(s: &mut S, x: i32, y: i32, src: [u8; 4]) {
    if !in_bounds(s, x, y) { return; }
    if src[3] == 0 { return; }
    if src[3] == 255 {
        s.write(x, y, [src[0], src[1], src[2], 255]);
        return;
    }
    let dst = s.read(x, y);
    let a   = src[3] as u32;
    let inv = 255 - a;
    let mix = |sc: u8, dc: u8| (((sc as u32) * a + (dc as u32) * inv + 127) / 255) as u8;
    s.write(x, y, [mix(src[0], dst[0]), mix(src[1], dst[1]), mix(src[2], dst[2]), 255]);
}

fn rgba_arr(c: FramefangRgba) -> [u8; 4] { [c.r, c.g, c.b, c.a] }

// ── primitives ──────────────────────────────────────────────────────────────

/// How a path is painted.
#[derive(Clone, Copy)]
enum Ink {
    /// Stroked at `width`, round ends; corners round or mitered.
    Stroke { width: f32, round_join: bool },
    Fill,
}

/// Paints `parts` anti-aliased, as one shape.
///
/// Every part is rasterized opaque into one scratch pixmap the size of their
/// bounds, and the color's own alpha is applied once at the blend. So a
/// translucent marker reads even where the stroke crosses itself, and an
/// arrow's shaft buried in its head never shows through it.
fn paint<S: Surface>(s: &mut S, parts: &[(tiny_skia::Path, Ink)], color: FramefangRgba) {
    use tiny_skia::{FillRule, LineCap, LineJoin, Paint, Pixmap, Stroke as SkStroke, Transform};
    if color.a == 0 || parts.is_empty() { return; }
    // Union of the parts' bounds, grown by half the widest stroke.
    let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for (path, ink) in parts {
        let grow = match ink { Ink::Stroke { width, .. } => width / 2.0 + 2.0, Ink::Fill => 2.0 };
        let b = path.bounds();
        x0 = x0.min(b.left() - grow);
        y0 = y0.min(b.top() - grow);
        x1 = x1.max(b.right() + grow);
        y1 = y1.max(b.bottom() + grow);
    }
    let x0 = (x0.floor() as i32).max(0);
    let y0 = (y0.floor() as i32).max(0);
    let x1 = (x1.ceil() as i32).min(s.width());
    let y1 = (y1.ceil() as i32).min(s.height());
    if x0 >= x1 || y0 >= y1 { return; }
    let Some(mut pm) = Pixmap::new((x1 - x0) as u32, (y1 - y0) as u32) else { return; };

    let mut paint = Paint::default();
    paint.set_color_rgba8(255, 255, 255, 255);
    paint.anti_alias = true;
    // Annotation coordinates name pixel centers (a 1 px line at y = 4 fills
    // row 4); tiny-skia's name pixel corners. The half-pixel shift lines
    // them up, so a thin stroke lands whole on its row instead of half on
    // two.
    let at = Transform::from_translate(0.5 - x0 as f32, 0.5 - y0 as f32);
    for (path, ink) in parts {
        match *ink {
            Ink::Fill => pm.fill_path(path, &paint, FillRule::Winding, at, None),
            Ink::Stroke { width, round_join } => {
                let st = SkStroke {
                    width: width.max(1.0),
                    line_cap: LineCap::Round,
                    line_join: if round_join { LineJoin::Round } else { LineJoin::Miter },
                    ..SkStroke::default()
                };
                pm.stroke_path(path, &paint, &st, at, None);
            }
        }
    }

    let w = pm.width() as usize;
    for (i, px) in pm.pixels().iter().enumerate() {
        let cov = px.alpha() as u32;
        if cov == 0 { continue; }
        let a = (cov * color.a as u32 + 127) / 255;
        let (x, y) = (x0 + (i % w) as i32, y0 + (i / w) as i32);
        blend(s, x, y, [color.r, color.g, color.b, a as u8]);
    }
}

fn path_of(points: &[(f32, f32)], close: bool) -> Option<tiny_skia::Path> {
    let mut pb = tiny_skia::PathBuilder::new();
    let (first, rest) = points.split_first()?;
    pb.move_to(first.0, first.1);
    for p in rest { pb.line_to(p.0, p.1); }
    if close { pb.close(); }
    pb.finish()
}

/// A dot where a stroke has no length (a click with the pen).
fn dot<S: Surface>(s: &mut S, x: f32, y: f32, thickness: f32, color: FramefangRgba) {
    if let Some(c) = tiny_skia::PathBuilder::from_circle(x, y, (thickness / 2.0).max(0.75)) {
        paint(s, &[(c, Ink::Fill)], color);
    }
}

/// Straight line with round ends.
pub fn line<S: Surface>(s: &mut S, x0: i32, y0: i32, x1: i32, y1: i32, thickness: f32, color: FramefangRgba) {
    line_f(s, (x0 as f32, y0 as f32), (x1 as f32, y1 as f32), thickness, color);
}

pub fn line_f<S: Surface>(s: &mut S, a: (f32, f32), b: (f32, f32), thickness: f32, color: FramefangRgba) {
    if a == b { return dot(s, a.0, a.1, thickness, color); }
    if let Some(p) = path_of(&[a, b], false) {
        paint(s, &[(p, Ink::Stroke { width: thickness, round_join: true })], color);
    }
}

/// Polyline through `points`, round joins. Used by Pen and Marker.
pub fn polyline<S: Surface>(s: &mut S, points: &[Point2], thickness: f32, color: FramefangRgba) {
    let pts: Vec<(f32, f32)> = points.iter().map(|p| (p.x, p.y)).collect();
    match pts.as_slice() {
        [] => {}
        [p] => dot(s, p.0, p.1, thickness, color),
        _ => if let Some(p) = path_of(&pts, false) {
            paint(s, &[(p, Ink::Stroke { width: thickness, round_join: true })], color);
        },
    }
}

/// Rectangle stroke between the two corners (any orientation), square corners.
pub fn stroke_rect<S: Surface>(s: &mut S, a: Point2, b: Point2, thickness: f32, color: FramefangRgba) {
    let (x0, y0, x1, y1) = (a.x.min(b.x), a.y.min(b.y), a.x.max(b.x), a.y.max(b.y));
    if let Some(p) = path_of(&[(x0, y0), (x1, y0), (x1, y1), (x0, y1)], true) {
        paint(s, &[(p, Ink::Stroke { width: thickness, round_join: false })], color);
    }
}

/// Ellipse outline inside the bounding box (a, b).
pub fn stroke_ellipse<S: Surface>(s: &mut S, a: Point2, b: Point2, thickness: f32, color: FramefangRgba) {
    let (x0, y0) = (a.x.min(b.x), a.y.min(b.y));
    let (w, h) = ((a.x - b.x).abs(), (a.y - b.y).abs());
    if w < 1.0 || h < 1.0 { return; }
    let oval = tiny_skia::Rect::from_xywh(x0, y0, w, h).and_then(tiny_skia::PathBuilder::from_oval);
    if let Some(p) = oval {
        paint(s, &[(p, Ink::Stroke { width: thickness, round_join: true })], color);
    }
}

/// Head length as a multiple of the stroke, so a head looks the same at every
/// weight. Never under `ARROW_MIN_REACH` px, so a hairline still points.
const ARROW_REACH: f32 = 6.0;
const ARROW_MIN_REACH: f32 = 12.0;
/// How far the head opens either side of the shaft.
const ARROW_SPREAD: f32 = std::f32::consts::PI / 7.0;
/// How far the back edge is swept in toward the tip, as a fraction of the reach.
const ARROW_NOTCH: f32 = 0.58;

/// The barbed head: tip, left barb, notch, right barb. The shaft ends at the
/// notch, buried in the widest part of the head, so neither its end nor its
/// round cap shows either side of the point.
pub fn arrow_head(from: (f32, f32), to: (f32, f32), thickness: f32) -> [(f32, f32); 4] {
    let size = (thickness * ARROW_REACH).max(ARROW_MIN_REACH);
    let angle = (to.1 - from.1).atan2(to.0 - from.0);
    let barb = |turn: f32| (to.0 - size * (angle + turn).cos(), to.1 - size * (angle + turn).sin());
    // The notch never reaches back past the tail of a short arrow.
    let span = ((to.0 - from.0).powi(2) + (to.1 - from.1).powi(2)).sqrt();
    let back = (size * ARROW_NOTCH).min(span);
    let notch = if span > 0.0 {
        (to.0 - back * (to.0 - from.0) / span, to.1 - back * (to.1 - from.1) / span)
    } else { to };
    [to, barb(-ARROW_SPREAD), notch, barb(ARROW_SPREAD)]
}

/// Arrow: a round-ended shaft into a swept, notched head, painted as one shape.
pub fn arrow<S: Surface>(s: &mut S, a: Point2, b: Point2, thickness: f32, color: FramefangRgba) {
    let (from, to) = ((a.x, a.y), (b.x, b.y));
    if from == to { return dot(s, to.0, to.1, thickness, color); }
    let head = arrow_head(from, to, thickness);
    let mut parts = Vec::with_capacity(2);
    if head[2] != from {
        if let Some(p) = path_of(&[from, head[2]], false) {
            parts.push((p, Ink::Stroke { width: thickness, round_join: true }));
        }
    }
    if let Some(p) = path_of(&head, true) {
        parts.push((p, Ink::Fill));
    }
    paint(s, &parts, color);
}

// ── step number rendering (5×7 bitmap font, 0–9 only) ──────────────────────

/// Each row is 5 bits, bit 4 = leftmost. 7 rows.
const DIGITS: [[u8; 7]; 10] = [
    // 0
    [0b01110, 0b10001, 0b10011, 0b10101, 0b11001, 0b10001, 0b01110],
    // 1
    [0b00100, 0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110],
    // 2
    [0b01110, 0b10001, 0b00001, 0b00010, 0b00100, 0b01000, 0b11111],
    // 3
    [0b11110, 0b00001, 0b00001, 0b01110, 0b00001, 0b00001, 0b11110],
    // 4
    [0b00010, 0b00110, 0b01010, 0b10010, 0b11111, 0b00010, 0b00010],
    // 5
    [0b11111, 0b10000, 0b11110, 0b00001, 0b00001, 0b10001, 0b01110],
    // 6
    [0b00110, 0b01000, 0b10000, 0b11110, 0b10001, 0b10001, 0b01110],
    // 7
    [0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b01000, 0b01000],
    // 8
    [0b01110, 0b10001, 0b10001, 0b01110, 0b10001, 0b10001, 0b01110],
    // 9
    [0b01110, 0b10001, 0b10001, 0b01111, 0b00001, 0b00010, 0b01100],
];

/// Render `n` (0–999) at the given top-left position, scaled by `scale`.
pub fn draw_number<S: Surface>(s: &mut S, x: i32, y: i32, scale: i32, n: u32, color: FramefangRgba) {
    let mut digits = Vec::new();
    let mut v = n.min(999);
    if v == 0 { digits.push(0); } else {
        while v > 0 { digits.push((v % 10) as usize); v /= 10; }
    }
    digits.reverse();
    let glyph_w = 5 * scale;
    let gap     = scale;
    let mut cx  = x;
    for d in digits {
        let glyph = &DIGITS[d];
        for (row, bits) in glyph.iter().enumerate() {
            for col in 0..5 {
                let on = (bits >> (4 - col)) & 1 == 1;
                if on {
                    for sy in 0..scale {
                        for sx in 0..scale {
                            blend(s, cx + col * scale + sx, y + row as i32 * scale + sy, rgba_arr(color));
                        }
                    }
                }
            }
        }
        cx += glyph_w + gap;
    }
}

/// Filled disc in alpha-blended source-over.
pub fn fill_disc<S: Surface>(s: &mut S, cx: i32, cy: i32, r: i32, color: FramefangRgba) {
    if r <= 0 { return; }
    let r2 = r * r;
    let c = rgba_arr(color);
    for dy in -r..=r {
        for dx in -r..=r {
            if dx * dx + dy * dy <= r2 {
                blend(s, cx + dx, cy + dy, c);
            }
        }
    }
}

/// Render a string starting at (x, y) using the in-tree 5×7 bitmap font.
/// Same scale convention as `draw_number`: `scale=2` → 10×14 per glyph
/// with a 2px column gap.
pub fn draw_text<S: Surface>(s: &mut S, x: i32, y: i32, scale: i32, text: &str, color: FramefangRgba) {
    let mut cx = x;
    let glyph_w = crate::bitmap_font::GLYPH_W * scale;
    let gap     = scale;
    for ch in text.chars() {
        let g = crate::bitmap_font::glyph(ch);
        for (row, bits) in g.iter().enumerate() {
            for col in 0..crate::bitmap_font::GLYPH_W {
                let on = (bits >> (4 - col)) & 1 == 1;
                if on {
                    for sy in 0..scale {
                        for sx in 0..scale {
                            blend(s, cx + col * scale + sx, y + row as i32 * scale + sy, rgba_arr(color));
                        }
                    }
                }
            }
        }
        cx += glyph_w + gap;
    }
}

// ── Unicode text (Text annotation) ─────────────────────────────────────────

/// Render `text` with the bundled OFL typeface, anti-aliased, with the block's
/// top-left corner at (x, y).
///
/// This is the Text *annotation* path — what the user types. `draw_text`
/// above stays the app's own chrome font (button labels, the dimension chip)
/// where a crisp ASCII grid is the intended look. `px` is a pixel em-size.
///
/// Glyph coverage arrives as 8-bit alpha from `framefang_core::text` and is
/// multiplied into the annotation color's own alpha, so it composites through
/// the same `blend` every other primitive uses — including the video-burn
/// `DiffSurface`, which therefore records exactly the pixels the editor drew.
pub fn draw_unicode_text<S: Surface>(s: &mut S, x: f32, y: f32, px: f32, text: &str, color: FramefangRgba) {
    if text.is_empty() { return; }
    let block = framefang_core::text::layout(text, px);
    blit_text_block(s, x.round() as i32, y.round() as i32, &block, color);
}

/// Blit an already-laid-out block. Split out so a caller that already has the
/// layout (the editor measures one for its typing frame) doesn't rasterize twice.
pub fn blit_text_block<S: Surface>(
    s:     &mut S,
    ox:    i32,
    oy:    i32,
    block: &framefang_core::text::TextBlock,
    color: FramefangRgba,
) {
    let base = rgba_arr(color);
    if base[3] == 0 { return; }
    // Only glyphs that can land on the surface at all — the per-pixel `blend`
    // still bounds-checks, this just skips whole glyphs cheaply.
    let clip = framefang_core::text::TextRect::new(
        -ox as f32, -oy as f32, s.width() as f32, s.height() as f32,
    );
    for g in block.visible_glyphs(clip) {
        for row in 0..g.h {
            let gy = oy + g.y + row as i32;
            for col in 0..g.w {
                let cov = g.coverage[row * g.w + col];
                if cov == 0 { continue; }
                let a = ((base[3] as u32 * cov as u32 + 127) / 255) as u8;
                blend(s, ox + g.x + col as i32, gy, [base[0], base[1], base[2], a]);
            }
        }
    }
}

// ── text highlights ────────────────────────────────────────────────────────
//
// A highlight lies on the words like ink: on a light page it multiplies
// (the paper takes the color, the letters stay dark), on a dark page it
// screens (the page takes the color, light letters stay light). The hand-
// made wobble comes from noise seeded by the highlight, so it is the same
// every time it is drawn.

/// -1 to 1, the same for the same seed and step.
fn hash(seed: u32, i: i32) -> f32 {
    let mut x = seed ^ (i as u32).wrapping_mul(0x9E37_79B1);
    x ^= x >> 15;
    x = x.wrapping_mul(0x2C1B_3C6D);
    x ^= x >> 12;
    x = x.wrapping_mul(0x297A_2D39);
    x ^= x >> 15;
    (x as f32 / u32::MAX as f32) * 2.0 - 1.0
}

/// Smooth noise along `t`, -1 to 1.
fn wobble(seed: u32, t: f32) -> f32 {
    let i = t.floor();
    let f = t - i;
    let s = f * f * (3.0 - 2.0 * f);
    hash(seed, i as i32) * (1.0 - s) + hash(seed, i as i32 + 1) * s
}

/// Whether the page under `r` is light (dark letters on it), from a sparse
/// look at its pixels.
fn light_under<S: Surface>(s: &S, r: &framefang_core::annotation::Rect) -> bool {
    let (mut sum, mut n) = (0.0f32, 0);
    let (x0, y0) = (r.x.max(0.0) as i32, r.y.max(0.0) as i32);
    let (x1, y1) = (((r.x + r.w) as i32).min(s.width() - 1), ((r.y + r.h) as i32).min(s.height() - 1));
    let step = (((x1 - x0).max(1) * (y1 - y0).max(1)) as f32 / 400.0).sqrt().max(1.0) as usize;
    for y in (y0..=y1).step_by(step) {
        for x in (x0..=x1).step_by(step) {
            if !in_bounds(s, x, y) { continue; }
            let p = s.read(x, y);
            sum += 0.299 * p[0] as f32 + 0.587 * p[1] as f32 + 0.114 * p[2] as f32;
            n += 1;
        }
    }
    n == 0 || sum / n as f32 >= 110.0
}

/// `c` laid on the pixel like ink, `a` strong (0 to 1): multiplied on a
/// light page, screened on a dark one.
fn ink<S: Surface>(s: &mut S, x: i32, y: i32, c: FramefangRgba, a: f32, light: bool) {
    if !in_bounds(s, x, y) || a <= 0.003 { return; }
    let a = a.clamp(0.0, 1.0);
    let d = s.read(x, y);
    let mix = |dc: u8, cc: u8| {
        let (dc, cc) = (dc as f32, cc as f32);
        let on = if light { dc * cc / 255.0 } else { 255.0 - (255.0 - dc) * (255.0 - cc) / 255.0 };
        (dc * (1.0 - a) + on * a).round() as u8
    };
    s.write(x, y, [mix(d[0], c.r), mix(d[1], c.g), mix(d[2], c.b), 255]);
}

/// Coverage of a hand-drawn stroke over a box: the most any stamp gave.
struct Cover { x0: i32, y0: i32, w: i32, h: i32, a: Vec<f32> }

impl Cover {
    fn new((x0, y0, x1, y1): (i32, i32, i32, i32)) -> Cover {
        let (w, h) = ((x1 - x0 + 1).max(1), (y1 - y0 + 1).max(1));
        Cover { x0, y0, w, h, a: vec![0.0; (w * h) as usize] }
    }

    /// A round dab of radius `r` at (cx, cy), smooth at its edge.
    fn dab(&mut self, cx: f32, cy: f32, r: f32) {
        let (xa, xb) = ((cx - r - 1.0).floor() as i32, (cx + r + 1.0).ceil() as i32);
        let (ya, yb) = ((cy - r - 1.0).floor() as i32, (cy + r + 1.0).ceil() as i32);
        for y in ya.max(self.y0)..=yb.min(self.y0 + self.h - 1) {
            for x in xa.max(self.x0)..=xb.min(self.x0 + self.w - 1) {
                let d = ((x as f32 + 0.5 - cx).powi(2) + (y as f32 + 0.5 - cy).powi(2)).sqrt();
                let v = (r + 0.5 - d).clamp(0.0, 1.0);
                let i = ((y - self.y0) * self.w + (x - self.x0)) as usize;
                if v > self.a[i] { self.a[i] = v; }
            }
        }
    }

    /// A stroke along `pts`, its radius at each point from `radius(t)`
    /// (0 at the start, 1 at the end), dabbed every half pixel.
    fn trace(&mut self, pts: &[(f32, f32)], radius: &dyn Fn(f32) -> f32) {
        let total: f32 = pts.windows(2).map(|w| ((w[1].0 - w[0].0).powi(2) + (w[1].1 - w[0].1).powi(2)).sqrt()).sum();
        let mut gone = 0.0;
        for w in pts.windows(2) {
            let len = ((w[1].0 - w[0].0).powi(2) + (w[1].1 - w[0].1).powi(2)).sqrt();
            let n = (len / 0.5).ceil().max(1.0) as i32;
            for i in 0..=n {
                let f = i as f32 / n as f32;
                let t = if total > 0.0 { (gone + len * f) / total } else { 0.0 };
                self.dab(w[0].0 + (w[1].0 - w[0].0) * f, w[0].1 + (w[1].1 - w[0].1) * f, radius(t));
            }
            gone += len;
        }
    }

    fn paint<S: Surface>(&self, s: &mut S, c: FramefangRgba, alpha: f32) {
        let k = rgba_arr(c);
        for (i, v) in self.a.iter().enumerate() {
            if *v <= 0.0 { continue; }
            let (x, y) = (self.x0 + i as i32 % self.w, self.y0 + i as i32 / self.w);
            blend(s, x, y, [k[0], k[1], k[2], (k[3] as f32 * alpha * v).round() as u8]);
        }
    }
}

/// Words highlighted in `look`: one rectangle per line.
pub fn text_mark<S: Surface>(s: &mut S, rects: &[framefang_core::annotation::Rect], look: &framefang_core::mark::MarkLook) {
    use framefang_core::mark::MarkStyle;
    let c = look.color.with_alpha(255);
    if look.style == MarkStyle::Circle {
        circle_mark(s, rects, c, look.seed);
        return;
    }
    for (li, r) in rects.iter().enumerate() {
        let seed = look.seed.wrapping_add(li as u32 * 7919);
        let light = light_under(s, r);
        match look.style {
            MarkStyle::Marker => marker_band(s, r, c, seed, light),
            MarkStyle::Clean => {
                let d = sd_round_box(r.x + r.w / 2.0, r.y + r.h / 2.0, r.w / 2.0 + 2.5, r.h / 2.0 + 1.5, 3.5);
                sdf_ink(s, r, 5, &d, &|_, _| 0.5, c, light);
            }
            MarkStyle::Lower => {
                // Under the letters' bodies, along the line they sit on.
                let (top, bottom) = (r.y + r.h * 0.62, r.y + r.h + 2.0);
                let mid = (top + bottom) / 2.0;
                let half = (bottom - top) / 2.0;
                let d = move |x: f32, y: f32| {
                    let edge = wobble(seed ^ 0xA5, x / 19.0) * 0.6;
                    sd_round_box(r.x + r.w / 2.0, mid + edge, r.w / 2.0 + 2.5, half, half * 0.9)(x, y)
                };
                sdf_ink(s, r, 5, &d, &|_, _| 0.58, c, light);
            }
            MarkStyle::Glow => {
                let core = sd_round_box(r.x + r.w / 2.0, r.y + r.h / 2.0, r.w / 2.0 + 3.0, r.h / 2.0 + 2.0, 5.0);
                // The soft light around it first, then the wash.
                let (x0, y0) = ((r.x - 12.0) as i32, (r.y - 12.0) as i32);
                let (x1, y1) = ((r.x + r.w + 12.0) as i32, (r.y + r.h + 12.0) as i32);
                for y in y0..=y1 {
                    for x in x0..=x1 {
                        let d = core(x as f32 + 0.5, y as f32 + 0.5);
                        if d <= 0.0 { continue; }
                        let a = 0.42 * (-d / 3.5).exp();
                        if a > 0.01 { ink(s, x, y, c, a, light); }
                    }
                }
                sdf_ink(s, r, 5, &core, &|_, _| 0.5, c, light);
            }
            MarkStyle::Underline => {
                let y = r.y + r.h + 3.0;
                let (x0, x1) = (r.x - 2.0, r.x + r.w + 2.0);
                let n = ((x1 - x0) / 6.0).ceil().max(2.0) as usize;
                let pts: Vec<(f32, f32)> = (0..=n).map(|i| {
                    let x = x0 + (x1 - x0) * i as f32 / n as f32;
                    // A slight sag and a little wave, as a hand pulls a line.
                    let t = i as f32 / n as f32;
                    (x, y + wobble(seed, x / 23.0) * 0.9 + (t - 0.5).powi(2) * -2.0 + 0.5)
                }).collect();
                let mut cov = Cover::new(((x0 - 4.0) as i32, (y - 5.0) as i32, (x1 + 4.0) as i32, (y + 5.0) as i32));
                cov.trace(&pts, &|t| 1.45 - 0.45 * ((t - 0.5).abs() * 2.0).powi(3));
                cov.paint(s, c, 0.92);
            }
            MarkStyle::Circle => unreachable!(),
        }
    }
}

/// Fill the shape `d` around `r` (out to `m` past it) with ink, `alpha` at
/// each point.
#[allow(clippy::too_many_arguments)]
fn sdf_ink<S: Surface>(s: &mut S, r: &framefang_core::annotation::Rect, m: i32, d: &dyn Fn(f32, f32) -> f32,
                       alpha: &dyn Fn(f32, f32) -> f32, c: FramefangRgba, light: bool) {
    for y in (r.y as i32 - m)..=((r.y + r.h) as i32 + m) {
        for x in (r.x as i32 - m)..=((r.x + r.w) as i32 + m) {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            let cov = (0.5 - d(fx, fy)).clamp(0.0, 1.0);
            if cov > 0.0 { ink(s, x, y, c, alpha(fx, fy) * cov, light); }
        }
    }
}

/// A chisel highlighter's pass over one line: its edges a little uneven,
/// its ends slanted the way the tip meets the page, the ink heavier where
/// the stroke starts and stops and along its edges, faint streaks along it.
fn marker_band<S: Surface>(s: &mut S, r: &framefang_core::annotation::Rect, c: FramefangRgba, seed: u32, light: bool) {
    const SLANT: f32 = 3.5;
    let (x0, x1) = (r.x - 3.0, r.x + r.w + 3.0);
    let (y0, y1) = (r.y - 1.5, r.y + r.h + 1.0);
    let h = y1 - y0;
    let tilt = hash(seed, 99) * 1.1;
    let top = |x: f32| y0 + wobble(seed, x / 21.0) * 0.8 + tilt * (x - x0) / (x1 - x0).max(1.0);
    let bottom = |x: f32| y1 + wobble(seed ^ 0x5bd1, x / 17.0) * 0.8 + tilt * (x - x0) / (x1 - x0).max(1.0);
    for y in (y0 - 3.0) as i32..=(y1 + 3.0) as i32 {
        for x in (x0 - 1.0) as i32..=(x1 + 1.0) as i32 {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            let (t, b) = (top(fx), bottom(fx));
            let v = ((fy - t) / h.max(1.0)).clamp(0.0, 1.0);
            // The tip's slant: the stroke's ends lean.
            let (l, rr) = (x0 + SLANT * (1.0 - v), x1 - SLANT * v);
            let cov = (fy - t + 0.5).clamp(0.0, 1.0) * (b - fy + 0.5).clamp(0.0, 1.0)
                * (fx - l + 0.5).clamp(0.0, 1.0) * (rr - fx + 0.5).clamp(0.0, 1.0);
            if cov <= 0.0 { continue; }
            let from_end = (fx - l).min(rr - fx).max(0.0);
            let from_edge = (fy - t).min(b - fy).max(0.0);
            let pool = 0.2 * (1.0 - (from_end / 6.0).min(1.0)).powi(2);
            let rim = 0.08 * (1.0 - (from_edge / 1.6).min(1.0));
            let streak = 1.0 + 0.07 * wobble(seed ^ 0x2f, fy * 0.85) + 0.03 * wobble(seed ^ 0x77, fx / 9.0);
            ink(s, x, y, c, ((0.5 + pool + rim) * streak) * cov, light);
        }
    }
}

/// A loose loop around all the lines, drawn in one go with a pen: a rounded
/// box that hugs the words (an ellipse cuts their corners), its line
/// wandering a little, going a bit more than once round so its ends cross.
fn circle_mark<S: Surface>(s: &mut S, rects: &[framefang_core::annotation::Rect], c: FramefangRgba, seed: u32) {
    let b = framefang_core::mark::bounds(rects, framefang_core::mark::MarkStyle::Clean);
    if b.w <= 0.0 { return; }
    let (hw, hh) = (b.w / 2.0 + 5.0, b.h / 2.0 + 4.0);
    let (cx, cy) = (b.x + b.w / 2.0, b.y + b.h / 2.0);
    let r = hh.min(22.0);
    // The perimeter as a function of distance along it, clockwise from the
    // top-left corner's end.
    let (sx, sy) = (2.0 * (hw - r), 2.0 * (hh - r));
    let arc = std::f32::consts::FRAC_PI_2 * r;
    let total = 2.0 * (sx + sy) + 4.0 * arc;
    let at = |d: f32| -> ((f32, f32), (f32, f32)) {
        let d = d.rem_euclid(total);
        // Each piece: (length, point at f along it, outward normal).
        let corner = |k: usize, f: f32| {
            let (ccx, ccy, a0) = match k {
                0 => (cx + hw - r, cy - hh + r, -std::f32::consts::FRAC_PI_2),
                1 => (cx + hw - r, cy + hh - r, 0.0),
                2 => (cx - hw + r, cy + hh - r, std::f32::consts::FRAC_PI_2),
                _ => (cx - hw + r, cy - hh + r, std::f32::consts::PI),
            };
            let a = a0 + f * std::f32::consts::FRAC_PI_2;
            ((ccx + r * a.cos(), ccy + r * a.sin()), (a.cos(), a.sin()))
        };
        let mut d = d;
        let sides = [(sx, (cx - hw + r, cy - hh), (1.0, 0.0), (0.0, -1.0)),
                     (sy, (cx + hw, cy - hh + r), (0.0, 1.0), (1.0, 0.0)),
                     (sx, (cx + hw - r, cy + hh), (-1.0, 0.0), (0.0, 1.0)),
                     (sy, (cx - hw, cy + hh - r), (0.0, -1.0), (-1.0, 0.0))];
        for (k, (len, from, dir, normal)) in sides.into_iter().enumerate() {
            if d <= len { return ((from.0 + dir.0 * d, from.1 + dir.1 * d), normal); }
            d -= len;
            if d <= arc { return corner(k, d / arc.max(1e-3)); }
            d -= arc;
        }
        corner(3, 1.0)
    };
    // Start a little way along the top, go round once and a short way more.
    let start = total * (0.18 + 0.08 * hash(seed, 5));
    let length = total * 1.035;
    let n = (length / 4.0).ceil().max(40.0) as usize;
    let pts: Vec<(f32, f32)> = (0..=n).map(|i| {
        let t = i as f32 / n as f32;
        let ((x, y), (nx, ny)) = at(start + length * t);
        // It wanders in and out, and ends a touch further out than it began.
        let out = wobble(seed, t * 7.0) * 1.4 + 2.2 * t - 0.6;
        (x + nx * out, y + ny * out)
    }).collect();
    let mut cov = Cover::new(((cx - hw - 10.0) as i32, (cy - hh - 10.0) as i32, (cx + hw + 10.0) as i32, (cy + hh + 10.0) as i32));
    cov.trace(&pts, &|t| 1.35 - 0.5 * ((t - 0.5).abs() * 2.0).powi(4));
    cov.paint(s, c, 0.92);
}

// ── numbered steps ─────────────────────────────────────────────────────────
//
// Every shape is a signed distance (pixels, negative inside), so edges are
// smooth, a ring is the same shape grown, and a shadow is the shape blurred.
// The sizes come from `framefang_core::step`, which hit-testing uses too.

/// A box around (cx, cy) reaching `l, t, r, b` out, in whole pixels.
fn sdf_box(cx: f32, cy: f32, (l, t, r, b): (f32, f32, f32, f32)) -> (i32, i32, i32, i32) {
    ((cx + l).floor() as i32 - 1, (cy + t).floor() as i32 - 1, (cx + r).ceil() as i32 + 1, (cy + b).ceil() as i32 + 1)
}

/// Paint `color` where `d` is inside, smoothing the edge over one pixel.
fn fill_sdf<S: Surface>(s: &mut S, (x0, y0, x1, y1): (i32, i32, i32, i32), color: FramefangRgba, d: &dyn Fn(f32, f32) -> f32) {
    let c = rgba_arr(color);
    if c[3] == 0 { return; }
    for y in y0..=y1 {
        for x in x0..=x1 {
            let cov = (0.5 - d(x as f32 + 0.5, y as f32 + 0.5)).clamp(0.0, 1.0);
            if cov <= 0.0 { continue; }
            blend(s, x, y, [c[0], c[1], c[2], (c[3] as f32 * cov).round() as u8]);
        }
    }
}

/// A soft shadow of the shape `d`, a little below it.
fn shadow_sdf<S: Surface>(s: &mut S, (x0, y0, x1, y1): (i32, i32, i32, i32), d: &dyn Fn(f32, f32) -> f32) {
    const DROP: f32 = 1.5;
    const SOFT: f32 = 3.0;
    const DARK: f32 = 0.42;
    for y in y0 - 1..=y1 + (SOFT + DROP) as i32 + 1 {
        for x in x0 - 2..=x1 + 2 {
            let t = ((SOFT - d(x as f32 + 0.5, y as f32 + 0.5 - DROP)) / (2.0 * SOFT)).clamp(0.0, 1.0);
            let a = DARK * t * t * (3.0 - 2.0 * t);
            if a <= 0.004 { continue; }
            blend(s, x, y, [0, 0, 0, (a * 255.0).round() as u8]);
        }
    }
}

fn sd_circle(cx: f32, cy: f32, r: f32) -> impl Fn(f32, f32) -> f32 {
    move |x, y| ((x - cx).powi(2) + (y - cy).powi(2)).sqrt() - r
}

/// A box `hw` by `hh` from its middle, corners rounded by `rad`.
fn sd_round_box(cx: f32, cy: f32, hw: f32, hh: f32, rad: f32) -> impl Fn(f32, f32) -> f32 {
    move |x, y| {
        let qx = (x - cx).abs() - hw + rad;
        let qy = (y - cy).abs() - hh + rad;
        (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt() + qx.max(qy).min(0.0) - rad
    }
}

/// A map pin: a round head of radius `r` at (cx, hy), drawn down to a
/// point at (cx, ty) along the lines that just touch the head.
fn sd_pin(cx: f32, hy: f32, r: f32, ty: f32) -> impl Fn(f32, f32) -> f32 {
    let dist = (ty - hy).max(r + 1.0);
    // Where the lines from the tip touch the head.
    let py = r * r / dist;
    let px = (r * r - py * py).max(0.0).sqrt();
    let (tip, left, right) = ((cx, ty), (cx - px, hy + py), (cx + px, hy + py));
    // Signed distance to the triangle tip-left-right (edges outward).
    let edge = |a: (f32, f32), b: (f32, f32), x: f32, y: f32| {
        let (ex, ey) = (b.0 - a.0, b.1 - a.1);
        let len = (ex * ex + ey * ey).sqrt().max(1e-3);
        ((x - a.0) * ey - (y - a.1) * ex) / len
    };
    move |x, y| {
        let tri = edge(tip, left, x, y).max(edge(left, right, x, y)).max(edge(right, tip, x, y));
        let head = ((x - cx).powi(2) + (y - hy).powi(2)).sqrt() - r;
        head.min(tri)
    }
}

/// The digits, bold and smooth, their ink centered on (cx, cy).
fn step_digits<S: Surface>(s: &mut S, cx: f32, cy: f32, number: u32, px: f32, color: FramefangRgba) {
    let text = number.min(999).to_string();
    let block = framefang_core::text::layout(&text, px);
    let clip = framefang_core::text::TextRect::new(-10_000.0, -10_000.0, 20_000.0, 20_000.0);
    let glyphs: Vec<_> = block.visible_glyphs(clip).collect();
    if glyphs.is_empty() { return; }
    let gx0 = glyphs.iter().map(|g| g.x).min().unwrap_or(0);
    let gy0 = glyphs.iter().map(|g| g.y).min().unwrap_or(0);
    let gx1 = glyphs.iter().map(|g| g.x + g.w as i32).max().unwrap_or(0);
    let gy1 = glyphs.iter().map(|g| g.y + g.h as i32).max().unwrap_or(0);
    // One pixel wider for the bolding.
    let (w, h) = ((gx1 - gx0 + 1).max(1) as usize, (gy1 - gy0).max(1) as usize);
    let mut cov = vec![0u8; w * h];
    for g in glyphs {
        for row in 0..g.h {
            for col in 0..g.w {
                let i = (g.y - gy0) as usize + row;
                let j = (g.x - gx0) as usize + col;
                let v = g.coverage[row * g.w + col];
                if v > cov[i * w + j] { cov[i * w + j] = v; }
            }
        }
    }
    // Bold: each pixel also takes most of its left neighbour's ink.
    for i in 0..h {
        for j in (1..w).rev() {
            let left = (cov[i * w + j - 1] as f32 * 0.9) as u8;
            if left > cov[i * w + j] { cov[i * w + j] = left; }
        }
    }
    // Center the ink itself, not the text box.
    let inked = |v: &u8| *v > 60;
    let rows: Vec<usize> = (0..h).filter(|&i| cov[i * w..(i + 1) * w].iter().any(inked)).collect();
    let cols: Vec<usize> = (0..w).filter(|&j| (0..h).any(|i| inked(&cov[i * w + j]))).collect();
    let (Some(&r0), Some(&r1), Some(&c0), Some(&c1)) = (rows.first(), rows.last(), cols.first(), cols.last()) else { return; };
    let ox = (cx - (c0 + c1 + 1) as f32 / 2.0).round() as i32;
    let oy = (cy - (r0 + r1 + 1) as f32 / 2.0).round() as i32;
    let c = rgba_arr(color);
    for i in 0..h {
        for j in 0..w {
            let v = cov[i * w + j];
            if v == 0 { continue; }
            let a = ((c[3] as u32 * v as u32 + 127) / 255) as u8;
            blend(s, ox + j as i32, oy + i as i32, [c[0], c[1], c[2], a]);
        }
    }
}

/// A numbered step marker in `look`, placed by its anchor (its middle, or a
/// pin's tip).
pub fn step_marker<S: Surface>(s: &mut S, center: Point2, number: u32, look: &framefang_core::step::StepLook) {
    use framefang_core::step::{self as st, StepShape};
    let (cx, cy) = (center.x, center.y);
    let r = st::RADIUS;
    let area = sdf_box(cx, cy, st::extent(number, look));
    let digits = number.min(999).to_string().len();
    let px = st::digit_px(digits, look.shape);
    // Where the digits go: a pin's head.
    let mut middle = (cx, cy);
    match look.shape {
        StepShape::Disc => {
            let body = sd_circle(cx, cy, r);
            if look.shadow { shadow_sdf(s, area, &body); }
            fill_sdf(s, area, look.body, &body);
        }
        StepShape::Ringed => {
            let ring = sd_circle(cx, cy, r - 1.0 + st::RING);
            if look.shadow { shadow_sdf(s, area, &ring); }
            fill_sdf(s, area, look.ring, &ring);
            fill_sdf(s, area, look.body, &sd_circle(cx, cy, r - 1.0));
        }
        StepShape::Outlined => {
            let ring = sd_circle(cx, cy, r + 1.0);
            if look.shadow { shadow_sdf(s, area, &ring); }
            fill_sdf(s, area, look.ring, &ring);
            fill_sdf(s, area, look.body, &sd_circle(cx, cy, r + 1.0 - st::THICK_RING));
        }
        StepShape::Square => {
            let half = r - 1.0;
            let ring = sd_round_box(cx, cy, half + st::RING, half + st::RING, 7.0 + st::RING);
            if look.shadow { shadow_sdf(s, area, &ring); }
            fill_sdf(s, area, look.ring, &ring);
            fill_sdf(s, area, look.body, &sd_round_box(cx, cy, half, half, 7.0));
        }
        StepShape::Halo => {
            let body = sd_circle(cx, cy, r);
            let faint = look.ring.with_alpha((look.ring.a as f32 * 0.32).round() as u8);
            fill_sdf(s, area, faint, &sd_circle(cx, cy, r + st::HALO));
            if look.shadow { shadow_sdf(s, area, &body); }
            fill_sdf(s, area, look.body, &body);
        }
        StepShape::Pin => {
            let head = r - 1.0;
            let hy = cy - st::PIN_RISE;
            let ring = sd_pin(cx, hy, head + st::RING, cy);
            if look.shadow { shadow_sdf(s, area, &ring); }
            fill_sdf(s, area, look.ring, &ring);
            fill_sdf(s, area, look.body, &sd_pin(cx, hy, head, cy - st::RING * 1.6));
            middle = (cx, hy);
        }
        StepShape::Hollow => {
            let body = sd_circle(cx, cy, r);
            if look.shadow { shadow_sdf(s, area, &body); }
            fill_sdf(s, area, look.body, &body);
            let (mid, half) = (r - 1.5, 1.5);
            fill_sdf(s, area, look.ring, &move |x: f32, y: f32| (((x - cx).powi(2) + (y - cy).powi(2)).sqrt() - mid).abs() - half);
        }
        StepShape::Pill => {
            let (hw, hh) = (st::pill_half_width(number), r - 1.0);
            let ring = sd_round_box(cx, cy, hw + st::RING, hh + st::RING, hh + st::RING);
            if look.shadow { shadow_sdf(s, area, &ring); }
            fill_sdf(s, area, look.ring, &ring);
            fill_sdf(s, area, look.body, &sd_round_box(cx, cy, hw, hh, hh));
        }
        StepShape::Terminal => {
            let half = r + 1.0;
            let body = sd_round_box(cx, cy, half, half, 1.0);
            if look.shadow { shadow_sdf(s, area, &body); }
            fill_sdf(s, area, look.body, &body);
            // Corner brackets: two arms at each corner, along the edges.
            // Whole pixels thick, crisp like the pixel digits.
            let (arm, t) = (9.0, 2.0);
            for (sx, sy) in [(-1.0f32, -1.0f32), (1.0, -1.0), (-1.0, 1.0), (1.0, 1.0)] {
                let (ex, ey) = (cx + sx * half, cy + sy * half);
                let along_x = sd_round_box(ex - sx * arm / 2.0, ey - sy * t / 2.0, arm / 2.0, t / 2.0, 0.0);
                let along_y = sd_round_box(ex - sx * t / 2.0, ey - sy * arm / 2.0, t / 2.0, arm / 2.0, 0.0);
                fill_sdf(s, area, look.ring, &along_x);
                fill_sdf(s, area, look.ring, &along_y);
            }
            // The app's own pixel digits.
            let scale = if digits <= 2 { 2 } else { 1 };
            let n = digits as i32;
            let tw = n * 5 * scale + (n - 1) * scale;
            let th = 7 * scale;
            draw_number(s, (cx - tw as f32 / 2.0).round() as i32, (cy - th as f32 / 2.0).round() as i32,
                        scale, number, look.number);
            return;
        }
    }
    step_digits(s, middle.0, middle.1, number, px, look.number);
}

// ── pixelate ────────────────────────────────────────────────────────────────

/// Average-then-fill: replace every `block_size×block_size` block in the dest
/// rect with the average color of the corresponding block in `src`. The source
/// is expected to be the *original* screenshot — never a downstream surface
/// that may already have annotations baked in. That keeps pixelate idempotent
/// under draw-order changes, matching `Framefang/Annotations.cs::PixelateAnnotation`.
///
/// `src_offset` is where the destination surface's origin sits inside `src`.
/// It is `(0, 0)` whenever the two share a coordinate space; the overlay
/// passes a real offset when its window doesn't line up with the capture it
/// is showing (see `framefang_core::virtual_desktop`).
pub fn pixelate_rect<S: Surface>(
    s:          &mut S,
    src:        &ImageBuffer<Rgba<u8>, Vec<u8>>,
    a:          Point2,
    b:          Point2,
    block_size: u32,
    src_offset: (i32, i32),
) {
    let block = block_size.max(2) as i32;
    let x0 = a.x.min(b.x) as i32;
    let y0 = a.y.min(b.y) as i32;
    let x1 = a.x.max(b.x) as i32;
    let y1 = a.y.max(b.y) as i32;
    let src_w = src.width()  as i32;
    let src_h = src.height() as i32;

    let mut by = y0;
    while by < y1 {
        let mut bx = x0;
        while bx < x1 {
            // Average source pixels inside this block.
            let bx2 = (bx + block).min(x1);
            let by2 = (by + block).min(y1);
            let (mut rs, mut gs, mut bs, mut count) = (0u32, 0u32, 0u32, 0u32);
            for sy in by..by2 {
                for sx in bx..bx2 {
                    let (sx, sy) = (sx + src_offset.0, sy + src_offset.1);
                    if sx >= 0 && sx < src_w && sy >= 0 && sy < src_h {
                        let p = src.get_pixel(sx as u32, sy as u32).0;
                        rs += p[0] as u32; gs += p[1] as u32; bs += p[2] as u32; count += 1;
                    }
                }
            }
            if count > 0 {
                let avg = [
                    (rs / count) as u8, (gs / count) as u8, (bs / count) as u8, 255,
                ];
                for sy in by..by2 {
                    for sx in bx..bx2 {
                        if in_bounds(s, sx, sy) {
                            s.write(sx, sy, avg);
                        }
                    }
                }
            }
            bx += block;
        }
        by += block;
    }
}

// ── spotlight and magnify ───────────────────────────────────────────────────

/// How much of a pixel's color is left outside every spotlight (55 % dimmer).
const SPOT_KEEP: u32 = 115;

/// The lit stretch of row `y` inside one spotlight, a rounded rectangle
/// (corner radius 8, less for a small one), or `None` on a row it misses.
fn spot_span(a: Point2, b: Point2, y: i32) -> Option<(i32, i32)> {
    let (x0, x1) = (a.x.min(b.x), a.x.max(b.x));
    let (y0, y1) = (a.y.min(b.y), a.y.max(b.y));
    let yc = y as f32 + 0.5;
    if yc < y0 || yc >= y1 { return None; }
    let r = 8f32.min((x1 - x0) / 2.0).min((y1 - y0) / 2.0).max(0.0);
    // How far the corner curve pulls this row in.
    let dy = if yc < y0 + r { y0 + r - yc } else if yc > y1 - r { yc - (y1 - r) } else { 0.0 };
    let inset = if dy > 0.0 { r - (r * r - dy * dy).max(0.0).sqrt() } else { 0.0 };
    let (l, rr) = ((x0 + inset).round() as i32, (x1 - inset).round() as i32);
    (rr > l).then_some((l, rr))
}

/// Dim every pixel inside `bounds` (`x, y, w, h`) that no spotlight lights.
/// Row by row: each spotlight lights one stretch of a row, so only those
/// stretches are skipped and the cost is the area, not area x spotlights.
pub fn dim_outside_spots<S: Surface>(s: &mut S, spots: &[(Point2, Point2)], bounds: (i32, i32, i32, i32)) {
    if spots.is_empty() { return; }
    let (bx0, by0) = (bounds.0.max(0), bounds.1.max(0));
    let (bx1, by1) = ((bounds.0 + bounds.2).min(s.width()), (bounds.1 + bounds.3).min(s.height()));
    let mut lit: Vec<(i32, i32)> = Vec::with_capacity(spots.len());
    for y in by0..by1 {
        lit.clear();
        lit.extend(spots.iter().filter_map(|(a, b)| spot_span(*a, *b, y)));
        lit.sort_unstable();
        let mut x = bx0;
        let mut i = 0;
        while x < bx1 {
            // Skip a lit stretch under x, else dim up to the next one.
            while i < lit.len() && lit[i].1 <= x { i += 1; }
            if i < lit.len() && lit[i].0 <= x {
                x = lit[i].1;
                continue;
            }
            let stop = if i < lit.len() { lit[i].0.min(bx1) } else { bx1 };
            for xx in x..stop {
                let p = s.read(xx, y);
                let k = |c: u8| ((c as u32 * SPOT_KEEP + 127) / 255) as u8;
                s.write(xx, y, [k(p[0]), k(p[1]), k(p[2]), p[3]]);
            }
            x = stop;
        }
    }
}

/// A round lens at `center` out to `edge`: what is already drawn under it,
/// `zoom` times larger (sampled only inside `bounds`, the picture), ringed in
/// the stroke.
pub fn magnify<S: Surface>(s: &mut S, stroke: Stroke, center: Point2, edge: Point2, zoom: u32,
                           bounds: (i32, i32, i32, i32)) {
    let r = framefang_core::annotation::magnify_radius(center, edge);
    if r < 1.0 { return; }
    let z = zoom.clamp(2, 4) as f32;
    let (bx0, by0) = (bounds.0.max(0), bounds.1.max(0));
    let (bx1, by1) = ((bounds.0 + bounds.2).min(s.width()) - 1, (bounds.1 + bounds.3).min(s.height()) - 1);
    if bx1 < bx0 || by1 < by0 { return; }
    // What the lens shows, read before anything is written over it.
    let reach = r / z + 2.0;
    let (sx0, sy0) = (((center.x - reach).floor() as i32).clamp(bx0, bx1), ((center.y - reach).floor() as i32).clamp(by0, by1));
    let (sx1, sy1) = (((center.x + reach).ceil() as i32).clamp(bx0, bx1), ((center.y + reach).ceil() as i32).clamp(by0, by1));
    let sw = (sx1 - sx0 + 1) as usize;
    let mut snap = Vec::with_capacity(sw * (sy1 - sy0 + 1) as usize);
    for y in sy0..=sy1 { for x in sx0..=sx1 { snap.push(s.read(x, y)); } }
    let at = |x: i32, y: i32| snap[(y.clamp(sy0, sy1) - sy0) as usize * sw + (x.clamp(sx0, sx1) - sx0) as usize];
    let (dx0, dy0) = (((center.x - r).floor() as i32).max(bx0), ((center.y - r).floor() as i32).max(by0));
    let (dx1, dy1) = (((center.x + r).ceil() as i32).min(bx1), ((center.y + r).ceil() as i32).min(by1));
    for y in dy0..=dy1 {
        for x in dx0..=dx1 {
            let (px, py) = (x as f32 + 0.5 - center.x, y as f32 + 0.5 - center.y);
            if px * px + py * py > r * r { continue; }
            // Bilinear sample of the spot this pixel shows.
            let (fx, fy) = (center.x + px / z - 0.5, center.y + py / z - 0.5);
            let (ix, iy) = (fx.floor() as i32, fy.floor() as i32);
            let (tx, ty) = (fx - ix as f32, fy - iy as f32);
            let (p00, p10, p01, p11) = (at(ix, iy), at(ix + 1, iy), at(ix, iy + 1), at(ix + 1, iy + 1));
            let mut out = [0u8; 4];
            for c in 0..4 {
                let top = p00[c] as f32 * (1.0 - tx) + p10[c] as f32 * tx;
                let bot = p01[c] as f32 * (1.0 - tx) + p11[c] as f32 * tx;
                out[c] = (top * (1.0 - ty) + bot * ty).round() as u8;
            }
            s.write(x, y, out);
        }
    }
    // The ring's inner edge meets the lens.
    let ring = r + stroke.thickness / 2.0;
    stroke_ellipse(s, Point2::new(center.x - ring, center.y - ring), Point2::new(center.x + ring, center.y + ring),
                   stroke.thickness, stroke.color);
}

/// Where a pass of annotations draws: the picture they sit on, and what
/// their effects read.
pub struct Pass<'a> {
    /// The untouched capture Pixelate averages, and where the surface's
    /// origin sits in it.
    pub source: Option<&'a ImageBuffer<Rgba<u8>, Vec<u8>>>,
    pub src_offset: (i32, i32),
    /// The picture's rectangle on the surface: spotlights dim inside it,
    /// lenses read only from it.
    pub bounds: (i32, i32, i32, i32),
}

/// Draw `anns` in order. Spotlights first, as one dim under all the ink;
/// a Pixelate block landing outside them is dimmed the same, so it never
/// shows bright in the dark; a lens magnifies what is drawn before it.
pub fn render_all<'b, S: Surface>(s: &mut S, anns: impl Iterator<Item = &'b Annotation> + Clone, pass: &Pass) {
    let spots: Vec<(Point2, Point2)> = anns.clone().filter_map(|a| match a.kind {
        AnnotationKind::Spotlight { start, end } => Some((start, end)),
        _ => None,
    }).collect();
    dim_outside_spots(s, &spots, pass.bounds);
    for a in anns {
        match &a.kind {
            AnnotationKind::Spotlight { .. } => {}
            AnnotationKind::Pixelate { start, end, .. } => {
                render_annotation_offset(s, a, pass.source, pass.src_offset);
                let (x0, y0) = (start.x.min(end.x) as i32, start.y.min(end.y) as i32);
                let (x1, y1) = (start.x.max(end.x) as i32, start.y.max(end.y) as i32);
                if let Some(r) = intersect((x0, y0, x1 - x0, y1 - y0), pass.bounds) {
                    dim_outside_spots(s, &spots, r);
                }
            }
            AnnotationKind::Magnify { stroke, center, edge, zoom } =>
                magnify(s, *stroke, *center, *edge, *zoom, pass.bounds),
            _ => render_annotation_offset(s, a, pass.source, pass.src_offset),
        }
    }
}

fn intersect(a: (i32, i32, i32, i32), b: (i32, i32, i32, i32)) -> Option<(i32, i32, i32, i32)> {
    let (x0, y0) = (a.0.max(b.0), a.1.max(b.1));
    let (x1, y1) = ((a.0 + a.2).min(b.0 + b.2), (a.1 + a.3).min(b.1 + b.3));
    (x1 > x0 && y1 > y0).then_some((x0, y0, x1 - x0, y1 - y0))
}

// ── annotation dispatch ─────────────────────────────────────────────────────

/// Draw one annotation, sampling the pixelate source in the destination
/// surface's own coordinate space.
pub fn render_annotation<S: Surface>(
    s:      &mut S,
    a:      &Annotation,
    source: Option<&ImageBuffer<Rgba<u8>, Vec<u8>>>,
) {
    render_annotation_offset(s, a, source, (0, 0));
}

/// [`render_annotation`], with the pixelate source sampled through
/// `src_offset` — the destination surface's origin inside `source`. Only the
/// live overlay preview needs this, and only when its window doesn't sit
/// exactly on the capture it is showing.
pub fn render_annotation_offset<S: Surface>(
    s:          &mut S,
    a:          &Annotation,
    source:     Option<&ImageBuffer<Rgba<u8>, Vec<u8>>>,
    src_offset: (i32, i32),
) {
    match &a.kind {
        AnnotationKind::Pen      { stroke: Stroke { color, thickness }, points } |
        AnnotationKind::Marker   { stroke: Stroke { color, thickness }, points } => {
            polyline(s, points, *thickness, *color);
        }
        AnnotationKind::Line     { stroke: Stroke { color, thickness }, start, end } => {
            line(s, start.x as i32, start.y as i32, end.x as i32, end.y as i32, *thickness, *color);
        }
        AnnotationKind::Arrow    { stroke: Stroke { color, thickness }, start, end } => {
            arrow(s, *start, *end, *thickness, *color);
        }
        AnnotationKind::Rectangle{ stroke: Stroke { color, thickness }, start, end } => {
            stroke_rect(s, *start, *end, *thickness, *color);
        }
        AnnotationKind::Ellipse  { stroke: Stroke { color, thickness }, start, end } => {
            stroke_ellipse(s, *start, *end, *thickness, *color);
        }
        AnnotationKind::Step     { center, number, look } => {
            step_marker(s, *center, *number, look);
        }
        AnnotationKind::TextMark { rects, look } => {
            text_mark(s, rects, look);
        }
        AnnotationKind::Pixelate { start, end, block_size } => {
            if let Some(src) = source {
                pixelate_rect(s, src, *start, *end, *block_size, src_offset);
            }
        }
        AnnotationKind::Text { color, position, text, font_size } => {
            // `font_size` is a pixel em-size for the bundled typeface, and
            // `position` is the top-left of the text block (unchanged from the
            // bitmap-font era, so existing placement behaviour still holds).
            draw_unicode_text(s, position.x, position.y, *font_size, text, *color);
        }
        // Drawn by `render_all`, which knows the picture's bounds: a lone
        // spotlight dims the whole surface it is given, a lone lens reads it.
        AnnotationKind::Spotlight { start, end } => {
            let all = (0, 0, s.width(), s.height());
            dim_outside_spots(s, &[(*start, *end)], all);
        }
        AnnotationKind::Magnify { stroke, center, edge, zoom } => {
            let all = (0, 0, s.width(), s.height());
            magnify(s, *stroke, *center, *edge, *zoom, all);
        }
    }
}

#[cfg(test)]
mod effect_tests {
    use super::*;
    use framefang_core::annotation::Annotation;

    fn grey(w: u32, h: u32) -> ImageBuffer<Rgba<u8>, Vec<u8>> {
        ImageBuffer::from_fn(w, h, |x, _| Rgba([(x * 2) as u8, 100, 100, 255]))
    }

    #[test]
    fn a_spotlight_dims_only_outside_itself_and_only_the_picture() {
        let mut img = ImageBuffer::from_pixel(100, 100, Rgba([200u8, 200, 200, 255]));
        let mut spot = Annotation::spotlight(Point2::new(20.0, 20.0));
        spot.extend(Point2::new(60.0, 60.0));
        let mut arrow = Annotation::arrow(Stroke { color: FramefangRgba::RED, thickness: 4.0 }, Point2::new(70.0, 80.0));
        arrow.extend(Point2::new(95.0, 80.0));
        // The arrow comes first in the list; it is still drawn over the dim.
        let list = [arrow, spot];
        render_all(&mut ImageSurface(&mut img), list.iter(), &Pass { source: None, src_offset: (0, 0), bounds: (0, 0, 90, 100) });
        assert_eq!(img.get_pixel(40, 40).0, [200, 200, 200, 255], "inside stays as captured");
        assert_eq!(img.get_pixel(5, 5).0[0], ((200 * SPOT_KEEP + 127) / 255) as u8, "outside is dimmed");
        assert_eq!(img.get_pixel(95, 5).0, [200, 200, 200, 255], "past the picture's bounds nothing changes");
        assert!(img.get_pixel(80, 80).0[0] > 200 && img.get_pixel(80, 80).0[1] < 60, "the arrow is undimmed red");
        // Rounded corners: the very corner of the spotlight is not lit.
        assert!(img.get_pixel(20, 20).0[0] < 200);
    }

    #[test]
    fn a_pixelate_under_a_spotlights_dark_is_as_dark_as_the_rest() {
        let src = grey(100, 100);
        let mut img = src.clone();
        let mut spot = Annotation::spotlight(Point2::new(0.0, 0.0));
        spot.extend(Point2::new(30.0, 30.0));
        let mut blur = Annotation::pixelate(Point2::new(50.0, 50.0));
        blur.extend(Point2::new(90.0, 90.0));
        let list = [spot, blur];
        render_all(&mut ImageSurface(&mut img), list.iter(), &Pass { source: Some(&src), src_offset: (0, 0), bounds: (0, 0, 100, 100) });
        assert!(img.get_pixel(60, 60).0[1] < 60, "the block is dimmed with its surroundings: {:?}", img.get_pixel(60, 60));
    }

    #[test]
    fn a_lens_shows_what_is_under_it_larger_and_only_the_picture() {
        // Left half black, right half white: a 2x lens on the edge widens it.
        let mut img = ImageBuffer::from_fn(200, 100, |x, _| if x < 100 { Rgba([0u8, 0, 0, 255]) } else { Rgba([255u8, 255, 255, 255]) });
        let stroke = Stroke { color: FramefangRgba::RED, thickness: 2.0 };
        let mut lens = Annotation::magnify(stroke, Point2::new(100.0, 50.0), 2);
        lens.extend(Point2::new(140.0, 50.0));
        render_all(&mut ImageSurface(&mut img), [lens].iter(), &Pass { source: None, src_offset: (0, 0), bounds: (0, 0, 200, 100) });
        // 20 px left of center shows what was 10 px left: still black; 20
        // px right shows white; the pixels at the very edge of the old line
        // are now 2x further out.
        assert_eq!(img.get_pixel(80, 50).0[0], 0);
        assert_eq!(img.get_pixel(120, 50).0[0], 255);
        // The ring sits just outside.
        assert!(img.get_pixel(141, 50).0[0] > 150 && img.get_pixel(141, 50).0[1] < 100, "{:?}", img.get_pixel(141, 50));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use framefang_core::color::Rgba as K;

    fn make_buf(w: i32, h: i32) -> Vec<u32> { vec![0; (w * h) as usize] }

    /// A light or dark page with two lines of text, and the lines' boxes.
    fn text_page(light: bool) -> (image::RgbaImage, Vec<framefang_core::annotation::Rect>) {
        let (bg, fg) = if light { ([250u8, 250, 247], K::new_opaque(28, 28, 30)) } else { ([30u8, 31, 36], K::new_opaque(228, 228, 232)) };
        let mut img = image::RgbaImage::from_pixel(360, 90, image::Rgba([bg[0], bg[1], bg[2], 255]));
        let mut rects = Vec::new();
        for (i, words) in ["Highlights that sit right on", "the words, line by line."].iter().enumerate() {
            let (x, y, px) = (16.0, 14.0 + i as f32 * 34.0, 20.0);
            draw_unicode_text(&mut ImageSurface(&mut img), x, y, px, words, fg);
            let (w, h) = framefang_core::text::measure(words, px);
            rects.push(framefang_core::annotation::Rect { x, y: y + 2.0, w, h: h - 4.0 });
        }
        (img, rects)
    }

    #[test]
    fn a_highlight_keeps_the_letters_readable_on_light_and_dark_pages() {
        use framefang_core::mark::{MarkLook, MarkStyle};
        for light in [true, false] {
            let (mut img, rects) = text_page(light);
            let before = img.clone();
            let look = MarkLook { style: MarkStyle::Marker, color: K::new_opaque(255, 226, 52), seed: 1 };
            text_mark(&mut ImageSurface(&mut img), &rects, &look);
            // The page under the words took the color...
            let (bx, by) = (rects[0].x as u32 + 1, rects[0].y as u32 + 1);
            assert_ne!(img.get_pixel(bx, by), before.get_pixel(bx, by), "light {light}: the page is colored");
            // ...and the letters still stand apart from it as much as before, nearly.
            let contrast = |im: &image::RgbaImage| {
                let (mut lo, mut hi) = (255u8, 0u8);
                for x in rects[0].x as u32..(rects[0].x + rects[0].w) as u32 {
                    for y in rects[0].y as u32..(rects[0].y + rects[0].h) as u32 {
                        let p = im.get_pixel(x, y);
                        let l = ((p[0] as u32 * 3 + p[1] as u32 * 6 + p[2] as u32) / 10) as u8;
                        lo = lo.min(l); hi = hi.max(l);
                    }
                }
                hi - lo
            };
            assert!(contrast(&img) as f32 >= contrast(&before) as f32 * 0.6, "light {light}: the words still read");
        }
    }

    /// Writes target/text-marks.png: every highlight look on a light and a
    /// dark page.
    #[test]
    #[ignore]
    fn render_text_marks_to_png() {
        use framefang_core::mark::{MarkLook, MarkStyle};
        let colors = [K::new_opaque(255, 226, 52), K::new_opaque(120, 230, 130), K::new_opaque(255, 120, 190),
                      K::new_opaque(90, 200, 255), K::new_opaque(255, 150, 60), K::new_opaque(190, 140, 255)];
        let (pw, ph) = (360u32, 90u32);
        let mut out = image::RgbaImage::new(pw * 2, ph * MarkStyle::ALL.len() as u32);
        for (i, style) in MarkStyle::ALL.into_iter().enumerate() {
            for (j, light) in [true, false].into_iter().enumerate() {
                let (mut img, rects) = text_page(light);
                let look = MarkLook { style, color: colors[i], seed: 7 + i as u32 };
                text_mark(&mut ImageSurface(&mut img), &rects, &look);
                image::imageops::overlay(&mut out, &img, (j as u32 * pw) as i64, (i as u32 * ph) as i64);
            }
        }
        let big = image::imageops::resize(&out, out.width() * 2, out.height() * 2, image::imageops::FilterType::Nearest);
        big.save(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/text-marks.png")).unwrap();
    }

    #[test]
    fn every_step_style_stays_inside_its_extent_and_shows_its_number() {
        use framefang_core::step::{extent, StepLook, StepShape};
        for shape in StepShape::ALL {
            for (number, look) in [(3, StepLook::recipe(shape, K::RED)), (12, StepLook::recipe(shape, K::new_opaque(255, 230, 0)))] {
                let (w, h) = (160, 160);
                let mut buf = make_buf(w, h);
                let (cx, cy) = (80.0, 90.0);
                let mut look = look;
                look.shadow = false;
                {
                    let mut s = U32Surface { buf: &mut buf, stride: w, height: h };
                    step_marker(&mut s, Point2::new(cx, cy), number, &look);
                }
                let (l, t, r, b) = extent(number, &look);
                let mut lit = 0;
                for y in 0..h { for x in 0..w {
                    if buf[(y * w + x) as usize] == 0 { continue; }
                    lit += 1;
                    let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
                    assert!(fx >= cx + l - 1.0 && fx <= cx + r + 1.0 && fy >= cy + t - 1.0 && fy <= cy + b + 1.0,
                            "{shape:?} {number}: ink at ({x}, {y}) outside {:?}", (l, t, r, b));
                } }
                assert!(lit > 300, "{shape:?} is drawn");
                // The number's color is somewhere in it.
                let n = look.number;
                let want = ((n.r as u32) << 16) | ((n.g as u32) << 8) | n.b as u32;
                assert!(buf.contains(&want), "{shape:?} {number}: no digits in {:06X}", want);
            }
        }
    }

    /// Writes target/step-styles.png: every style, on light and dark, with
    /// one and two digits, in red and yellow.
    #[test]
    #[ignore]
    fn render_step_styles_to_png() {
        use framefang_core::step::{StepLook, StepShape};
        let (cell, rows) = (64u32, 4u32);
        let w = cell * StepShape::ALL.len() as u32;
        let mut img = image::RgbaImage::from_fn(w, cell * rows, |_, y| {
            if (y / cell) % 2 == 0 { image::Rgba([244, 244, 242, 255]) } else { image::Rgba([29, 31, 36, 255]) }
        });
        for (i, shape) in StepShape::ALL.into_iter().enumerate() {
            for row in 0..rows {
                let accent = if row < 2 { K::new_opaque(220, 38, 38) } else { K::new_opaque(255, 200, 0) };
                let number = if row % 2 == 0 { 3 } else { 12 };
                let cx = (i as u32 * cell + cell / 2) as f32;
                let cy = (row * cell + cell / 2) as f32 + if shape == StepShape::Pin { 14.0 } else { 0.0 };
                let mut s = ImageSurface(&mut img);
                step_marker(&mut s, Point2::new(cx, cy), number, &StepLook::recipe(shape, accent));
            }
        }
        let big = image::imageops::resize(&img, w * 2, cell * rows * 2, image::imageops::FilterType::Nearest);
        big.save(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/step-styles.png")).unwrap();
    }

    #[test]
    fn arrow_head_turns_with_the_line_and_buries_the_shaft() {
        for (from, to) in [((0.0, 0.0), (100.0, 0.0)), ((50.0, 50.0), (10.0, 90.0)), ((5.0, 80.0), (5.0, 2.0))] {
            let [tip, left, notch, right] = arrow_head(from, to, 4.0);
            assert_eq!(tip, to);
            let d = |p: (f32, f32)| ((p.0 - to.0).powi(2) + (p.1 - to.1).powi(2)).sqrt();
            // Both barbs the same distance back, the notch between them and
            // nearer the tip, so the head is symmetric and swept.
            assert!((d(left) - d(right)).abs() < 0.01);
            assert!(d(notch) < d(left));
            // The notch lies on the shaft's own line.
            let cross = (to.0 - from.0) * (notch.1 - from.1) - (to.1 - from.1) * (notch.0 - from.0);
            assert!(cross.abs() < 0.5, "{from:?}->{to:?}");
        }
        // An arrow shorter than its head never reaches back past its tail.
        let [_, _, notch, _] = arrow_head((0.0, 0.0), (3.0, 0.0), 8.0);
        assert!(notch.0 >= -0.001);
    }

    #[test]
    fn a_translucent_stroke_does_not_darken_where_it_crosses_itself() {
        let mut data = make_buf(60, 60);
        let mut s = U32Surface { buf: &mut data, stride: 60, height: 60 };
        let half = K { r: 255, g: 255, b: 255, a: 128 };
        let pts = [Point2::new(5.0, 30.0), Point2::new(55.0, 30.0), Point2::new(30.0, 5.0), Point2::new(30.0, 55.0)];
        polyline(&mut s, &pts, 8.0, half);
        // The crossing at (30, 30) and a single-coverage point on one leg.
        assert_eq!(data[30 * 60 + 30], data[30 * 60 + 12]);
        assert_ne!(data[30 * 60 + 12], 0);
    }

    #[test]
    fn line_stamps_at_least_one_pixel_per_endpoint() {
        let mut data = make_buf(20, 20);
        let mut s = U32Surface { buf: &mut data, stride: 20, height: 20 };
        line(&mut s, 2, 2, 17, 17, 1.0, K::WHITE);
        assert_ne!(data[2 * 20 + 2], 0, "start pixel was not written");
        assert_ne!(data[17 * 20 + 17], 0, "end pixel was not written");
    }

    #[test]
    fn stroke_rect_paints_all_four_edges() {
        let mut data = make_buf(20, 20);
        let mut s = U32Surface { buf: &mut data, stride: 20, height: 20 };
        stroke_rect(&mut s, Point2::new(3.0, 3.0), Point2::new(15.0, 15.0), 1.0, K::WHITE);
        assert_ne!(data[3 * 20 + 3],  0); // top-left
        assert_ne!(data[3 * 20 + 15], 0); // top-right
        assert_ne!(data[15 * 20 + 3], 0); // bottom-left
        assert_ne!(data[15 * 20 + 15],0); // bottom-right
    }

    #[test]
    fn unicode_text_paints_accented_and_cyrillic_glyphs() {
        // The old 5×7 path turned every one of these into a '?' box.
        for sample in ["Grüße", "Привет", "50 % ± ½"] {
            let mut data = make_buf(400, 120);
            let mut s = U32Surface { buf: &mut data, stride: 400, height: 120 };
            draw_unicode_text(&mut s, 10.0, 10.0, 32.0, sample, K::WHITE);
            assert!(data.iter().any(|&p| p != 0), "{sample} painted nothing");
        }
    }

    #[test]
    fn unicode_text_is_antialiased() {
        let mut data = make_buf(300, 100);
        let mut s = U32Surface { buf: &mut data, stride: 300, height: 100 };
        draw_unicode_text(&mut s, 10.0, 10.0, 48.0, "Sé", K::WHITE);
        // Anti-aliasing means partially-lit pixels, not just black/white.
        let partial = data.iter().filter(|&&p| p != 0 && p != 0x00FF_FFFF).count();
        assert!(partial > 0, "expected soft edges from the rasterizer");
    }

    #[test]
    fn unicode_text_clips_at_the_surface_edge() {
        // Drawing far outside the surface must be a no-op, not a panic.
        let mut data = make_buf(64, 64);
        {
            let mut s = U32Surface { buf: &mut data, stride: 64, height: 64 };
            draw_unicode_text(&mut s, -500.0, -500.0, 32.0, "clipped", K::WHITE);
            draw_unicode_text(&mut s, 500.0, 500.0, 32.0, "clipped", K::WHITE);
        }
        assert!(data.iter().all(|&p| p == 0));
        // Straddling the edge paints only what fits.
        {
            let mut s = U32Surface { buf: &mut data, stride: 64, height: 64 };
            draw_unicode_text(&mut s, -10.0, 20.0, 32.0, "clipped", K::WHITE);
        }
        assert!(data.iter().any(|&p| p != 0));
    }

    #[test]
    fn text_annotation_renders_multiline() {
        use framefang_core::annotation::Annotation;
        let mut data = make_buf(300, 200);
        let mut s = U32Surface { buf: &mut data, stride: 300, height: 200 };
        let a = Annotation::text_sized(K::WHITE, Point2::new(10.0, 10.0), "one\ntwo", 28.0);
        render_annotation(&mut s, &a, None);
        let lit_rows: Vec<usize> = (0..200)
            .filter(|y| data[y * 300..(y + 1) * 300].iter().any(|&p| p != 0))
            .collect();
        assert!(!lit_rows.is_empty());
        // Two lines → a vertical gap between the inked bands.
        let span = lit_rows.last().unwrap() - lit_rows.first().unwrap();
        assert!(span as f32 > framefang_core::text::line_height(28.0),
                "second line should sit a line below the first");
    }

    #[test]
    fn arrow_writes_pixels_at_tip() {
        let mut data = make_buf(40, 40);
        let mut s = U32Surface { buf: &mut data, stride: 40, height: 40 };
        arrow(&mut s, Point2::new(5.0, 5.0), Point2::new(30.0, 30.0), 2.0, K::RED);
        assert_ne!(data[30 * 40 + 30], 0, "arrow tip should be painted");
    }
}
