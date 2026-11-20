//! The glyphs on the editor's buttons: the tool panel, the action panel and
//! the color flyout's pipette.
//!
//! One set, drawn alike (Brandon 2026-11-20, "refined line"): every icon is
//! laid out on a 36-unit grid, the size of a panel button, in a single 2 unit
//! line with round ends and joins, its outline kept inside the same 18 unit
//! square so they all read the same size. The grid is scaled to whatever box
//! the icon is given. Rendered with tiny-skia, then blended into the
//! softbuffer frame.

use framefang_core::tool::Tool;
use tiny_skia::{FillRule, LineCap, LineJoin, Paint, Path, PathBuilder, Pixmap, Stroke, Transform};

/// Which glyph to draw.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IconKind {
    /// The tool panel's button for an annotation tool.
    Tool(Tool),
    Undo,
    Redo,
    Pin,
    Copy,
    Save,
    Close,
    /// A ring with a solid dot inside. The ring keeps it looking like a
    /// button at 36 px instead of a lone speck.
    Record,
    /// The flyout's color picker: a pipette, bulb up and right, drawn to
    /// fit a swatch-sized button rather than the panels' 36 px ones.
    Eyedropper,
}

/// The set's line, in grid units.
const LINE: f32 = 2.0;
/// Bezier handle length for a quarter circle.
const KAPPA: f32 = 0.552_284_8;

/// Draw `icon` into the box `x0..x1`, `y0..y1` of the frame, in `fg_rgba`.
/// `accent_rgba` colors Record's dot.
#[allow(clippy::too_many_arguments)]
pub fn render_icon(buf: &mut [u32], stride: usize, height: usize, x0: i32, y0: i32, x1: i32, y1: i32,
                   icon: IconKind, fg_rgba: [u8; 4], accent_rgba: Option<[u8; 4]>) {
    let (box_w, box_h) = ((x1 - x0).max(1) as u32, (y1 - y0).max(1) as u32);
    let Some(mut canvas) = Pixmap::new(box_w, box_h) else { return };
    {
        let mut g = Glyph { pm: &mut canvas, ts: grid_transform(icon, box_w as f32, box_h as f32) };
        let ink = Ink { solid: paint_of(fg_rgba, 255), fg: fg_rgba, line: stroke_of(LINE) };
        match icon {
            IconKind::Tool(tool) => draw_tool(&mut g, &ink, tool),
            other => draw_action(&mut g, &ink, other, accent_rgba),
        }
    }
    composite(buf, stride, height, (x0, y0), &canvas);
}

/// Maps grid units onto the box: scaled to its shorter side and centred.
fn grid_transform(icon: IconKind, w: f32, h: f32) -> Transform {
    // The pipette sits in a swatch button, which is a bit over half a panel
    // button, so it has its own 21 unit grid.
    let units = if icon == IconKind::Eyedropper { 21.0 } else { 36.0 };
    let scale = w.min(h) / units;
    let half_span = units / 2.0 * scale;
    Transform::from_row(scale, 0.0, 0.0, scale, w / 2.0 - half_span, h / 2.0 - half_span)
}

/// The paints and the standard line one glyph is drawn with.
struct Ink {
    solid: Paint<'static>,
    fg: [u8; 4],
    line: Stroke,
}

impl Ink {
    /// The foreground at `alpha` (out of 255) of its own opacity.
    fn faded(&self, alpha: u8) -> Paint<'static> { paint_of(self.fg, alpha) }
}

fn draw_tool(g: &mut Glyph, ink: &Ink, tool: Tool) {
    let (paint, line) = (&ink.solid, &ink.line);
    match tool {
        Tool::Pen => {
            // A pen leaning right, with the squiggle it has just drawn
            // running into its tip (Brandon 2026-11-20, option A).
            let tip = (14.5, 21.5);
            if let Some((barrel, nib)) = pen(tip, 45.0, 11.5, 3.1, 4.8) {
                g.stroke(&barrel, paint, line);
                g.fill(&nib, paint);
            }
            g.stroke_opt(pen_band(tip, 45.0, 3.1, 13.0), paint, &stroke_of(1.6));
            g.stroke_opt(curve(&[(6.0, 27.5), (8.5, 24.0), (11.0, 28.0), (13.5, 23.5)]), paint, line);
        }
        Tool::Line => g.stroke_opt(poly(&[(10.0, 26.0), (26.0, 10.0)], false), paint, line),
        Tool::Arrow => {
            g.stroke_opt(poly(&[(10.0, 26.0), (26.0, 10.0)], false), paint, line);
            g.stroke_opt(poly(&[(17.5, 10.0), (26.0, 10.0), (26.0, 18.5)], false), paint, line);
        }
        Tool::Rectangle => g.stroke_opt(round_rect(9.0, 10.5, 18.0, 15.0, 2.5), paint, line),
        Tool::Ellipse => g.stroke_opt(oval(18.0, 18.0, 9.5, 8.0), paint, line),
        Tool::Marker => {
            // A chisel highlighter, wider than the pen, over the broad wash
            // it leaves.
            let tip = (12.5, 21.0);
            if let Some((barrel, nib)) = pen(tip, 45.0, 11.0, 4.3, 4.0) {
                g.stroke(&barrel, paint, line);
                g.fill(&nib, paint);
            }
            g.stroke_opt(poly(&[(8.5, 27.0), (27.5, 27.0)], false), &ink.faded(110), &stroke_of(4.0));
        }
        Tool::Highlight => {
            // Three lines of text, the middle one highlighted.
            g.fill_opt(round_rect(8.0, 14.0, 20.0, 8.0, 2.0), &ink.faded(105));
            g.stroke_opt(poly(&[(10.0, 10.0), (24.0, 10.0)], false), paint, line);
            g.stroke_opt(poly(&[(11.0, 18.0), (25.0, 18.0)], false), paint, line);
            g.stroke_opt(poly(&[(10.0, 26.0), (20.0, 26.0)], false), paint, line);
        }
        Tool::Text => {
            g.stroke_opt(poly(&[(10.5, 11.0), (25.5, 11.0)], false), paint, line);
            g.stroke_opt(poly(&[(18.0, 11.0), (18.0, 26.0)], false), paint, line);
            g.stroke_opt(poly(&[(15.0, 26.0), (21.0, 26.0)], false), paint, line);
        }
        Tool::Step => {
            g.stroke_opt(oval(18.0, 18.0, 9.5, 9.5), paint, line);
            g.stroke_opt(poly(&[(15.8, 15.6), (18.6, 13.4), (18.6, 22.8)], false), paint, line);
        }
        Tool::Pixelate => {
            // A square cut in thirds, two of its cells filled: low-res.
            g.stroke_opt(round_rect(9.0, 9.0, 18.0, 18.0, 2.5), paint, line);
            let (grid_paint, grid_line) = (ink.faded(150), stroke_of(1.4));
            for at in [15.0, 21.0] {
                g.stroke_opt(poly(&[(at, 9.0), (at, 27.0)], false), &grid_paint, &grid_line);
            }
            for at in [15.0, 21.0] {
                g.stroke_opt(poly(&[(9.0, at), (27.0, at)], false), &grid_paint, &grid_line);
            }
            g.fill_opt(rect(15.0, 9.0, 6.0, 6.0), &ink.faded(200));
            g.fill_opt(rect(9.0, 15.0, 6.0, 6.0), &ink.faded(120));
            g.fill_opt(rect(21.0, 21.0, 6.0, 6.0), &ink.faded(200));
        }
        Tool::Spotlight => {
            // A dimmed field, and the lit part of it bright and framed.
            g.fill_opt(round_rect(8.0, 9.0, 20.0, 18.0, 3.0), &ink.faded(70));
            g.fill_opt(round_rect(13.5, 13.5, 9.0, 9.0, 1.5), paint);
        }
        Tool::Magnify => {
            g.stroke_opt(oval(16.0, 16.0, 6.8, 6.8), paint, line);
            g.stroke_opt(poly(&[(21.0, 21.0), (26.5, 26.5)], false), paint, &stroke_of(2.6));
        }
        Tool::Crop => {
            g.stroke_opt(poly(&[(13.0, 8.5), (13.0, 23.0), (27.5, 23.0)], false), paint, line);
            g.stroke_opt(poly(&[(8.5, 13.0), (23.0, 13.0), (23.0, 27.5)], false), paint, line);
        }
    }
}

/// Every glyph that is not a tool: undo and redo, the action panel's
/// buttons and the pipette.
fn draw_action(g: &mut Glyph, ink: &Ink, icon: IconKind, accent: Option<[u8; 4]>) {
    let (paint, line) = (&ink.solid, &ink.line);
    match icon {
        IconKind::Tool(tool) => draw_tool(g, ink, tool),
        IconKind::Undo => return_arrow(g, ink, false),
        IconKind::Redo => return_arrow(g, ink, true),
        IconKind::Pin => {
            // A map pin, outlined, its center marked.
            let (hx, hy, r, tip) = (18.0, 15.5, 6.5, 28.0);
            let mut body = PathBuilder::new();
            body.move_to(hx, tip);
            body.cubic_to(hx - 2.0, tip - 3.5, hx - r, hy + r * 0.9, hx - r, hy);
            body.cubic_to(hx - r, hy - r * KAPPA, hx - r * KAPPA, hy - r, hx, hy - r);
            body.cubic_to(hx + r * KAPPA, hy - r, hx + r, hy - r * KAPPA, hx + r, hy);
            body.cubic_to(hx + r, hy + r * 0.9, hx + 2.0, tip - 3.5, hx, tip);
            body.close();
            g.stroke_opt(body.finish(), paint, line);
            g.fill_opt(oval(hx, hy, 2.2, 2.2), paint);
        }
        IconKind::Copy => {
            // The front sheet whole; of the one behind, only what shows.
            g.stroke_opt(round_rect(9.0, 9.0, 13.5, 13.5, 2.5), paint, line);
            let mut back = PathBuilder::new();
            back.move_to(22.5, 13.5);
            back.line_to(24.5, 13.5);
            back.cubic_to(25.9, 13.5, 27.0, 14.6, 27.0, 16.0);
            back.line_to(27.0, 24.5);
            back.cubic_to(27.0, 25.9, 25.9, 27.0, 24.5, 27.0);
            back.line_to(16.0, 27.0);
            back.cubic_to(14.6, 27.0, 13.5, 25.9, 13.5, 24.5);
            back.line_to(13.5, 22.5);
            g.stroke_opt(back.finish(), paint, line);
        }
        IconKind::Save => {
            // A floppy disk: a rounded body with its corner cut, the shutter
            // at the top and the label at the bottom.
            let mut disk = PathBuilder::new();
            disk.move_to(11.5, 9.0);
            disk.line_to(23.0, 9.0);
            disk.line_to(27.0, 13.0);
            disk.line_to(27.0, 24.5);
            disk.cubic_to(27.0, 25.9, 25.9, 27.0, 24.5, 27.0);
            disk.line_to(11.5, 27.0);
            disk.cubic_to(10.1, 27.0, 9.0, 25.9, 9.0, 24.5);
            disk.line_to(9.0, 11.5);
            disk.cubic_to(9.0, 10.1, 10.1, 9.0, 11.5, 9.0);
            disk.close();
            g.stroke_opt(disk.finish(), paint, line);
            g.stroke_opt(poly(&[(13.0, 9.0), (13.0, 13.5), (21.0, 13.5), (21.0, 9.0)], false), paint, line);
            g.stroke_opt(poly(&[(12.5, 27.0), (12.5, 20.0), (23.5, 20.0), (23.5, 27.0)], false), paint, line);
        }
        IconKind::Close => {
            g.stroke_opt(poly(&[(11.0, 11.0), (25.0, 25.0)], false), paint, line);
            g.stroke_opt(poly(&[(25.0, 11.0), (11.0, 25.0)], false), paint, line);
        }
        IconKind::Record => {
            g.stroke_opt(oval(18.0, 18.0, 9.5, 9.5), paint, line);
            g.fill_opt(oval(18.0, 18.0, 5.0, 5.0), &paint_of(accent.unwrap_or(ink.fg), 255));
        }
        IconKind::Eyedropper => {
            // On its own 21 unit grid: the glass tube with its tip at the
            // lower left, the collar across it, the bulb.
            let thin = stroke_of(1.8);
            g.stroke_opt(poly(&[(5.0, 16.0), (12.5, 8.5)], false), paint, &thin);
            g.stroke_opt(poly(&[(10.0, 7.0), (14.0, 11.0)], false), paint, &thin);
            g.fill_opt(oval(14.5, 6.5, 2.8, 2.8), paint);
        }
    }
}

/// Undo's glyph, a return arrow: out along the top, round the turn, back
/// along the bottom. `mirrored` flips it left to right for Redo.
fn return_arrow(g: &mut Glyph, ink: &Ink, mirrored: bool) {
    let x = |v: f32| if mirrored { 36.0 - v } else { v };
    let bend = 5.5 * KAPPA;
    let mut hook = PathBuilder::new();
    hook.move_to(x(10.0), 14.0);
    hook.line_to(x(21.0), 14.0);
    hook.cubic_to(x(21.0 + bend), 14.0, x(26.5), 19.5 - bend, x(26.5), 19.5);
    hook.cubic_to(x(26.5), 19.5 + bend, x(21.0 + bend), 25.0, x(21.0), 25.0);
    hook.line_to(x(15.0), 25.0);
    g.stroke_opt(hook.finish(), &ink.solid, &ink.line);
    g.stroke_opt(poly(&[(x(14.5), 9.5), (x(10.0), 14.0), (x(14.5), 18.5)], false), &ink.solid, &ink.line);
}

/// A pixmap and the grid's transform onto it.
struct Glyph<'a> {
    pm: &'a mut Pixmap,
    ts: Transform,
}

impl Glyph<'_> {
    fn stroke(&mut self, p: &Path, paint: &Paint, s: &Stroke) { self.pm.stroke_path(p, paint, s, self.ts, None); }
    fn fill(&mut self, p: &Path, paint: &Paint) { self.pm.fill_path(p, paint, FillRule::Winding, self.ts, None); }
    fn stroke_opt(&mut self, p: Option<Path>, paint: &Paint, s: &Stroke) { if let Some(p) = p { self.stroke(&p, paint, s); } }
    fn fill_opt(&mut self, p: Option<Path>, paint: &Paint) { if let Some(p) = p { self.fill(&p, paint); } }
}

fn paint_of(c: [u8; 4], alpha: u8) -> Paint<'static> {
    let mut p = Paint::default();
    p.set_color_rgba8(c[0], c[1], c[2], ((c[3] as u16 * alpha as u16) / 255) as u8);
    p.anti_alias = true;
    p
}

fn stroke_of(width: f32) -> Stroke {
    Stroke { width, line_cap: LineCap::Round, line_join: LineJoin::Round, ..Default::default() }
}

/// Straight segments through `pts`, closed back to the first when `closed`.
fn poly(pts: &[(f32, f32)], closed: bool) -> Option<Path> {
    let mut pb = PathBuilder::new();
    let (first, rest) = pts.split_first()?;
    pb.move_to(first.0, first.1);
    for p in rest { pb.line_to(p.0, p.1); }
    if closed { pb.close(); }
    pb.finish()
}

fn rect(x: f32, y: f32, w: f32, h: f32) -> Option<Path> {
    poly(&[(x, y), (x + w, y), (x + w, y + h), (x, y + h)], true)
}

/// A rectangle with its corners rounded by `r`.
fn round_rect(x: f32, y: f32, w: f32, h: f32, r: f32) -> Option<Path> {
    let r = r.min(w / 2.0).min(h / 2.0);
    let c = r * KAPPA;
    let (x1, y1) = (x + w, y + h);
    let mut pb = PathBuilder::new();
    pb.move_to(x + r, y);
    pb.line_to(x1 - r, y);
    pb.cubic_to(x1 - r + c, y, x1, y + r - c, x1, y + r);
    pb.line_to(x1, y1 - r);
    pb.cubic_to(x1, y1 - r + c, x1 - r + c, y1, x1 - r, y1);
    pb.line_to(x + r, y1);
    pb.cubic_to(x + r - c, y1, x, y1 - r + c, x, y1 - r);
    pb.line_to(x, y + r);
    pb.cubic_to(x, y + r - c, x + r - c, y, x + r, y);
    pb.close();
    pb.finish()
}

/// An ellipse around (cx, cy).
fn oval(cx: f32, cy: f32, rx: f32, ry: f32) -> Option<Path> {
    let (kx, ky) = (rx * KAPPA, ry * KAPPA);
    let mut pb = PathBuilder::new();
    pb.move_to(cx + rx, cy);
    pb.cubic_to(cx + rx, cy + ky, cx + kx, cy + ry, cx, cy + ry);
    pb.cubic_to(cx - kx, cy + ry, cx - rx, cy + ky, cx - rx, cy);
    pb.cubic_to(cx - rx, cy - ky, cx - kx, cy - ry, cx, cy - ry);
    pb.cubic_to(cx + kx, cy - ry, cx + rx, cy - ky, cx + rx, cy);
    pb.close();
    pb.finish()
}

/// A pen leaning `angle` degrees right of upright, its tip at `tip`: the
/// barrel `len` long and `half` wide each side, the cone `cone` long.
/// (barrel outline, nib outline).
fn pen(tip: (f32, f32), angle: f32, len: f32, half: f32, cone: f32) -> Option<(Path, Path)> {
    let a = angle.to_radians();
    let (axis, perp) = ((a.sin(), -a.cos()), (a.cos(), a.sin()));
    let at = |v: f32, u: f32| (tip.0 + v * perp.0 + u * axis.0, tip.1 + v * perp.1 + u * axis.1);
    let barrel = poly(&[at(-half, cone), at(-half, cone + len), at(half, cone + len), at(half, cone)], true)?;
    let nib = poly(&[at(-half, cone), tip, at(half, cone)], true)?;
    Some((barrel, nib))
}

/// The band across a pen's barrel `u` along it from the tip.
fn pen_band(tip: (f32, f32), angle: f32, half: f32, u: f32) -> Option<Path> {
    let a = angle.to_radians();
    let (ax, ay, px, py) = (a.sin(), -a.cos(), a.cos(), a.sin());
    poly(&[(tip.0 - half * px + u * ax, tip.1 - half * py + u * ay),
           (tip.0 + half * px + u * ax, tip.1 + half * py + u * ay)], false)
}

/// A smooth curve through the points (Catmull-Rom, as cubics).
fn curve(pts: &[(f32, f32)]) -> Option<Path> {
    let mut pb = PathBuilder::new();
    pb.move_to(pts.first()?.0, pts.first()?.1);
    for i in 0..pts.len() - 1 {
        let p0 = pts[i.saturating_sub(1)];
        let (p1, p2) = (pts[i], pts[i + 1]);
        let p3 = pts[(i + 2).min(pts.len() - 1)];
        pb.cubic_to(p1.0 + (p2.0 - p0.0) / 6.0, p1.1 + (p2.1 - p0.1) / 6.0,
                    p2.0 - (p3.0 - p1.0) / 6.0, p2.1 - (p3.1 - p1.1) / 6.0, p2.0, p2.1);
    }
    pb.finish()
}

/// Lays the drawn glyph over the 0RGB frame with its top left corner at
/// `origin`, clipped to the frame.
///
/// tiny-skia keeps its colors premultiplied by alpha, so "over" is simply
/// the glyph's channel plus what shows through of the frame:
/// `out = src + dst * (255 - alpha) / 255`, rounded to nearest.
fn composite(frame: &mut [u32], stride: usize, height: usize, origin: (i32, i32), glyph: &Pixmap) {
    let gw = glyph.width() as usize;
    // Glyph columns and rows that land inside the frame.
    let col_start = (-origin.0).max(0) as usize;
    let row_start = (-origin.1).max(0) as usize;
    let col_end = (stride as i64 - origin.0 as i64).clamp(0, gw as i64) as usize;
    let row_end = (height as i64 - origin.1 as i64).clamp(0, glyph.height() as i64) as usize;
    if col_start >= col_end { return; }

    let over = |src: u8, dst: u32, keep: u32| (src as u32 + (dst * keep + 127) / 255).min(255);
    for row in row_start..row_end {
        let src_row = &glyph.pixels()[row * gw..(row + 1) * gw];
        let dst_base = (origin.1 as i64 + row as i64) as usize * stride;
        for col in col_start..col_end {
            let px = src_row[col];
            if px.alpha() == 0 { continue; }
            let at = dst_base + (origin.0 as i64 + col as i64) as usize;
            let old = frame[at];
            let keep = 255 - px.alpha() as u32;
            let red = over(px.red(), (old >> 16) & 0xFF, keep);
            let green = over(px.green(), (old >> 8) & 0xFF, keep);
            let blue = over(px.blue(), old & 0xFF, keep);
            frame[at] = (red << 16) | (green << 8) | blue;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [IconKind; 21] = [
        IconKind::Tool(Tool::Pen), IconKind::Tool(Tool::Line), IconKind::Tool(Tool::Arrow),
        IconKind::Tool(Tool::Rectangle), IconKind::Tool(Tool::Ellipse), IconKind::Tool(Tool::Marker),
        IconKind::Tool(Tool::Highlight),
        IconKind::Tool(Tool::Text), IconKind::Tool(Tool::Step), IconKind::Tool(Tool::Pixelate),
        IconKind::Tool(Tool::Spotlight), IconKind::Tool(Tool::Magnify), IconKind::Tool(Tool::Crop),
        IconKind::Undo, IconKind::Redo, IconKind::Pin, IconKind::Copy, IconKind::Save,
        IconKind::Close, IconKind::Record, IconKind::Eyedropper,
    ];

    /// Every icon drawn in a 36 px box, as (lit pixels, ink box).
    fn ink(icon: IconKind, side: i32) -> (usize, (i32, i32, i32, i32)) {
        let mut buf = vec![0u32; (side * side) as usize];
        render_icon(&mut buf, side as usize, side as usize, 0, 0, side, side, icon, [255, 255, 255, 255], None);
        let mut lit = 0;
        let (mut x0, mut y0, mut x1, mut y1) = (side, side, 0, 0);
        for y in 0..side { for x in 0..side {
            if buf[(y * side + x) as usize] & 0xFF > 60 {
                lit += 1;
                x0 = x0.min(x); y0 = y0.min(y); x1 = x1.max(x); y1 = y1.max(y);
            }
        } }
        (lit, (x0, y0, x1, y1))
    }

    #[test]
    fn every_icon_draws_inside_its_box_with_a_margin_and_reads_about_the_same_size() {
        for icon in ALL {
            let (lit, (x0, y0, x1, y1)) = ink(icon, 36);
            assert!(lit > 25, "{icon:?} is drawn");
            assert!(x0 >= 4 && y0 >= 4 && x1 <= 31 && y1 <= 31, "{icon:?} keeps its margin: {:?}", (x0, y0, x1, y1));
            if icon == IconKind::Eyedropper { continue; }
            // The same optical size: none tiny, none filling the button.
            let (w, h) = (x1 - x0 + 1, y1 - y0 + 1);
            assert!(w.max(h) >= 14 && w.max(h) <= 24, "{icon:?} is {w}x{h}");
        }
        // Scaled with its box: twice the box, about twice the ink across.
        let (_, (a0, _, a1, _)) = ink(IconKind::Tool(Tool::Rectangle), 36);
        let (_, (b0, _, b1, _)) = ink(IconKind::Tool(Tool::Rectangle), 72);
        assert!(((b1 - b0) - 2 * (a1 - a0)).abs() <= 3);
    }

    #[test]
    fn undo_and_redo_mirror_each_other() {
        let side = 36;
        let draw = |icon| {
            let mut buf = vec![0u32; (side * side) as usize];
            render_icon(&mut buf, side as usize, side as usize, 0, 0, side, side, icon, [255, 255, 255, 255], None);
            buf
        };
        let (u, r) = (draw(IconKind::Undo), draw(IconKind::Redo));
        let mut off = 0;
        for y in 0..side { for x in 0..side {
            let a = u[(y * side + x) as usize] & 0xFF;
            let b = r[(y * side + side - 1 - x) as usize] & 0xFF;
            if a.abs_diff(b) > 90 { off += 1; }
        } }
        assert!(off < 12, "{off} pixels differ from the mirror");
    }

    #[test]
    fn a_glyph_hanging_off_the_frame_is_clipped_not_wrapped() {
        // A 10x10 frame and a 36 px icon placed so most of it falls off the
        // left and top edges: nothing may panic, and the right-hand column
        // of the frame must not pick up ink that belongs off the left edge.
        let mut frame = vec![0u32; 100];
        render_icon(&mut frame, 10, 10, -20, -20, 16, 16, IconKind::Close, [255, 255, 255, 255], None);
        assert!(frame.iter().any(|&p| p != 0), "the on-frame corner of the cross is drawn");
        for y in 0..10 {
            assert_eq!(frame[y * 10 + 9], 0, "row {y} wrapped onto the right edge");
        }
        // Wholly outside: untouched.
        let mut frame = vec![0x00_10_20_30u32; 100];
        render_icon(&mut frame, 10, 10, 40, 40, 76, 76, IconKind::Save, [255, 255, 255, 255], None);
        assert!(frame.iter().all(|&p| p == 0x00_10_20_30));
    }

    #[test]
    fn half_covered_pixels_blend_with_the_frame_below() {
        // White glyph over pure blue: where the line's edge is partly
        // covered, the result mixes the two rather than replacing blue.
        let side = 36;
        let mut frame = vec![0x00_00_00_FFu32; (side * side) as usize];
        render_icon(&mut frame, side as usize, side as usize, 0, 0, side, side,
                    IconKind::Tool(Tool::Line), [255, 255, 255, 255], None);
        let mixed = frame.iter().any(|&p| {
            let (r, b) = ((p >> 16) & 0xFF, p & 0xFF);
            r > 20 && r < 235 && b == 255
        });
        assert!(mixed, "no partly covered pixel kept the blue underneath");
    }

    /// Writes target/tool-icons.png: every icon, on the tool panel's colors,
    /// at 3x.
    #[test]
    #[ignore]
    fn render_tool_icons_to_png() {
        const B: i32 = 36;
        let cols = 13;
        let rows = ALL.len().div_ceil(cols) as i32;
        let (w, h) = (cols as i32 * (B + 6) + 6, rows * (B + 6) + 6);
        let mut buf = vec![0x00_22_22_24u32; (w * h) as usize];
        for (i, icon) in ALL.iter().enumerate() {
            let (x, y) = (6 + (i % cols) as i32 * (B + 6), 6 + (i / cols) as i32 * (B + 6));
            for yy in y..y + B { for xx in x..x + B { buf[(yy * w + xx) as usize] = 0x00_2E_2E_32; } }
            let side = if *icon == IconKind::Eyedropper { 21 } else { B };
            let off = (B - side) / 2;
            render_icon(&mut buf, w as usize, h as usize, x + off, y + off, x + off + side, y + off + side,
                        *icon, [0xE8, 0xE8, 0xEC, 0xFF], Some([0xE5, 0x38, 0x3B, 0xFF]));
        }
        let img = image::RgbImage::from_fn(w as u32, h as u32, |x, y| {
            let p = buf[(y * w as u32 + x) as usize];
            image::Rgb([(p >> 16) as u8, (p >> 8) as u8, p as u8])
        });
        let big = image::imageops::resize(&img, w as u32 * 3, h as u32 * 3, image::imageops::FilterType::Nearest);
        big.save(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/tool-icons.png")).unwrap();
    }
}
