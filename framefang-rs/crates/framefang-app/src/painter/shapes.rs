//! Stroked and filled shapes: Pen, Marker, Line, Arrow, Rectangle and
//! Ellipse, plus the plain lines the chrome uses. All of them go through
//! tiny-skia for anti-aliasing and land on the surface through `blend`.

use framefang_core::annotation::Point2;
use framefang_core::color::Rgba as FramefangRgba;

use super::{blend, Surface};

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::painter::U32Surface;
    use framefang_core::color::Rgba as K;

    fn blank(w: i32, h: i32) -> Vec<u32> { vec![0; (w * h) as usize] }

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
        let mut data = blank(60, 60);
        let mut s = U32Surface { buf: &mut data, stride: 60, height: 60 };
        let half = K { r: 255, g: 255, b: 255, a: 128 };
        let pts = [Point2::new(5.0, 30.0), Point2::new(55.0, 30.0), Point2::new(30.0, 5.0), Point2::new(30.0, 55.0)];
        polyline(&mut s, &pts, 8.0, half);
        // The crossing at (30, 30) and a single-coverage point on one leg.
        assert_eq!(data[30 * 60 + 30], data[30 * 60 + 12]);
        assert_ne!(data[30 * 60 + 12], 0);
    }

    #[test]
    fn a_thin_line_reaches_both_of_its_end_pixels() {
        let mut px = blank(20, 20);
        line(&mut U32Surface { buf: &mut px, stride: 20, height: 20 }, 2, 2, 17, 17, 1.0, K::WHITE);
        for (x, y) in [(2, 2), (17, 17)] {
            assert_ne!(px[y * 20 + x], 0, "nothing at ({x}, {y})");
        }
    }

    #[test]
    fn a_rectangle_outline_reaches_every_corner() {
        let mut px = blank(20, 20);
        let corners = (Point2::new(15.0, 15.0), Point2::new(3.0, 3.0));
        // Given bottom-right first: the order of the corners does not matter.
        stroke_rect(&mut U32Surface { buf: &mut px, stride: 20, height: 20 }, corners.0, corners.1, 1.0, K::WHITE);
        for (x, y) in [(3, 3), (15, 3), (3, 15), (15, 15)] {
            assert_ne!(px[y * 20 + x], 0, "corner ({x}, {y}) is empty");
        }
        assert_eq!(px[9 * 20 + 9], 0, "an outline, not a filled box");
    }

    #[test]
    fn an_arrow_inks_its_point() {
        let mut px = blank(40, 40);
        arrow(&mut U32Surface { buf: &mut px, stride: 40, height: 40 },
              Point2::new(5.0, 5.0), Point2::new(30.0, 30.0), 2.0, K::RED);
        assert_ne!(px[30 * 40 + 30], 0);
    }
}
