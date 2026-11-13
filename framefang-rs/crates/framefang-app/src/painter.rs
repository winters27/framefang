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

fn line_f<S: Surface>(s: &mut S, a: (f32, f32), b: (f32, f32), thickness: f32, color: FramefangRgba) {
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

/// Numbered step circle: filled disc of `color`, white digits centered inside.
pub fn step_marker<S: Surface>(s: &mut S, center: Point2, number: u32, color: FramefangRgba) {
    const RADIUS: i32 = 14;
    fill_disc(s, center.x as i32, center.y as i32, RADIUS, color);
    // 5×7 font scaled 2× → 10×14 per digit, with 2px gap.
    let scale = 2;
    let n = number.min(999);
    let digits = if n == 0 { 1 } else { (n.ilog10() + 1) as i32 };
    let total_w = digits * 5 * scale + (digits - 1) * scale;
    let total_h = 7 * scale;
    let x = center.x as i32 - total_w / 2;
    let y = center.y as i32 - total_h / 2;
    draw_number(s, x, y, scale, number, FramefangRgba::WHITE);
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
        AnnotationKind::Step     { color, center, number } => {
            step_marker(s, *center, *number, *color);
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use framefang_core::color::Rgba as K;

    fn make_buf(w: i32, h: i32) -> Vec<u32> { vec![0; (w * h) as usize] }

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
