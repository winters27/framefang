//! Geometry for editing marks after they are drawn: which mark a click
//! lands on, the box a mark covers, and moving a mark. The Select tool uses
//! all three; saving a crop uses the move to shift marks into the cropped
//! picture's coordinates.
//!
//! It is plain arithmetic, tested without a window. The sizes it works from
//! have to match what the app's painter draws (glyph sizes come from
//! `crate::text`, step markers from `crate::step`); if the painter changes,
//! this must follow, or clicks stop landing where the ink is.

use crate::annotation::{Annotation, AnnotationKind, Point2, Rect};

/// How many pixels outside a mark's ink a click may land and still pick it.
/// Without it a 2 px line would be almost impossible to click.
pub const HIT_SLOP: f32 = 5.0;

/// The width and height a text mark paints. `font_size` is the pixel
/// em-size the Text tool draws with, and the measurement comes from the
/// same text layout that draws it, so the clickable box is the visible ink,
/// lines and all. Empty text is one empty line tall.
pub fn text_extent(text: &str, font_size: f32) -> (f32, f32) {
    match text {
        "" => (0.0, crate::text::line_height(crate::text::clamp_px(font_size))),
        words => crate::text::measure(words, font_size),
    }
}

/// The box a text mark's ink fills, with its top left corner at `position`.
fn text_box(position: Point2, text: &str, font_size: f32) -> Rect {
    let (width, height) = text_extent(text, font_size);
    Rect { x: position.x, y: position.y, w: width, h: height }
}

/// Straight-line distance between two points.
fn dist(a: Point2, b: Point2) -> f32 {
    let (dx, dy) = (a.x - b.x, a.y - b.y);
    (dx * dx + dy * dy).sqrt()
}

/// Distance from `p` to the nearest point of the segment from `a` to `b`.
/// A segment with no length is treated as the point `a`.
fn dist_to_segment(p: Point2, a: Point2, b: Point2) -> f32 {
    let (ux, uy) = (b.x - a.x, b.y - a.y);
    let length_sq = ux * ux + uy * uy;
    if length_sq <= f32::EPSILON {
        return dist(p, a);
    }
    // How far along the segment the closest point sits, 0 at `a`, 1 at `b`.
    let along = (((p.x - a.x) * ux + (p.y - a.y) * uy) / length_sq).clamp(0.0, 1.0);
    dist(p, Point2::new(a.x + ux * along, a.y + uy * along))
}

/// How close to a stroke's centre line a click must be: half the stroke,
/// plus the slop.
fn reach(thickness: f32) -> f32 {
    HIT_SLOP + thickness * 0.5
}

/// Whether `p` is within `within` of a path joining `points` in order. A
/// path of one point is a dot; a path of none is nothing.
fn near_path(points: &[Point2], p: Point2, within: f32) -> bool {
    if let [dot] = points {
        return dist(p, *dot) <= within;
    }
    points.windows(2).any(|pair| dist_to_segment(p, pair[0], pair[1]) <= within)
}

/// Whether `p` is within `within` of the outline of the box with corners
/// `a` and `b`. Only the outline counts: a rectangle mark is a hollow frame,
/// and a click in its middle belongs to whatever lies beneath.
fn near_box_outline(a: Point2, b: Point2, p: Point2, within: f32) -> bool {
    let frame = Rect::from_corners(a, b);
    let (left, top) = (frame.x, frame.y);
    let (right, bottom) = (frame.x + frame.w, frame.y + frame.h);
    let ring = [
        Point2::new(left, top),
        Point2::new(right, top),
        Point2::new(right, bottom),
        Point2::new(left, bottom),
        Point2::new(left, top),
    ];
    near_path(&ring, p, within)
}

/// Whether `p` is within `within` of the outline of the ellipse filling the
/// box with corners `a` and `b`. It compares how far the click is from the
/// centre with how far the curve is from the centre in the same direction:
/// exact along that line, and cheap.
fn near_ellipse_outline(a: Point2, b: Point2, p: Point2, within: f32) -> bool {
    let frame = Rect::from_corners(a, b);
    let (half_w, half_h) = (frame.w / 2.0, frame.h / 2.0);
    // Squashed flat, an ellipse draws as a line; its box outline is then
    // the honest click target.
    if half_w <= within || half_h <= within {
        return near_box_outline(a, b, p, within);
    }
    let centre = Point2::new(frame.x + half_w, frame.y + half_h);
    let from_centre = dist(p, centre);
    if from_centre <= f32::EPSILON {
        return half_w.min(half_h) <= within;
    }
    let (dir_x, dir_y) = ((p.x - centre.x) / from_centre, (p.y - centre.y) / from_centre);
    let inverse_radius = ((dir_x / half_w).powi(2) + (dir_y / half_h).powi(2)).sqrt();
    if inverse_radius <= f32::EPSILON {
        return false;
    }
    (from_centre - 1.0 / inverse_radius).abs() <= within
}

/// `r` with `by` added on every side.
fn grow(r: Rect, by: f32) -> Rect {
    Rect { x: r.x - by, y: r.y - by, w: r.w + by * 2.0, h: r.h + by * 2.0 }
}

/// The box a step marker covers, worked out from its look.
fn step_box(center: Point2, number: u32, look: &crate::step::StepLook) -> Rect {
    let (left, top, right, bottom) = crate::step::extent(number, look);
    Rect { x: center.x + left, y: center.y + top, w: right - left, h: bottom - top }
}

/// How far an arrow's head reaches out from its tip: twice the stroke, never
/// less than 5 px.
fn arrow_head_reach(thickness: f32) -> f32 {
    (thickness * 2.0).max(5.0)
}

/// Whether a click at `p` lands on the ink of `a`.
pub fn hit_test(a: &Annotation, p: Point2) -> bool {
    use AnnotationKind as K;
    match &a.kind {
        K::Pen { stroke, points } | K::Marker { stroke, points } => {
            near_path(points, p, reach(stroke.thickness))
        }
        K::Line { stroke, start, end } => near_path(&[*start, *end], p, reach(stroke.thickness)),
        K::Arrow { stroke, start, end } => {
            // The head is wider than the shaft, so near the tip the head's
            // own reach applies.
            dist_to_segment(p, *start, *end) <= reach(stroke.thickness)
                || dist(p, *end) <= arrow_head_reach(stroke.thickness) + HIT_SLOP
        }
        K::Rectangle { stroke, start, end } => {
            near_box_outline(*start, *end, p, reach(stroke.thickness))
        }
        K::Ellipse { stroke, start, end } => {
            near_ellipse_outline(*start, *end, p, reach(stroke.thickness))
        }
        K::Text { position, text, font_size, .. } => {
            grow(text_box(*position, text, *font_size), HIT_SLOP).contains(p)
        }
        K::Step { center, number, look } => grow(step_box(*center, *number, look), HIT_SLOP).contains(p),
        K::TextMark { rects, .. } => rects.iter().any(|line| grow(*line, HIT_SLOP).contains(p)),
        // Blur and spotlight fill their whole box.
        K::Pixelate { start, end, .. } | K::Spotlight { start, end } => {
            grow(Rect::from_corners(*start, *end), HIT_SLOP).contains(p)
        }
        K::Magnify { center, edge, .. } => {
            dist(p, *center) <= crate::annotation::magnify_radius(*center, *edge) + HIT_SLOP
        }
    }
}

/// The index of the mark a click at `p` picks: the last drawn one under it.
/// Spotlights are painted beneath all other marks wherever they sit in the
/// list, so any other mark under the click wins, and a spotlight is picked
/// only where nothing else is.
pub fn hit_test_topmost(annotations: &[Annotation], p: Point2) -> Option<usize> {
    let topmost = |want_spotlight: bool| {
        annotations.iter().rposition(|a| {
            matches!(a.kind, AnnotationKind::Spotlight { .. }) == want_spotlight && hit_test(a, p)
        })
    };
    topmost(false).or_else(|| topmost(true))
}

/// The smallest box around `points`, with `pad` added on every side. No
/// points give an empty box at the origin.
fn span(points: &[Point2], pad: f32) -> Rect {
    let Some(first) = points.first() else {
        return Rect { x: 0.0, y: 0.0, w: 0.0, h: 0.0 };
    };
    let (mut low, mut high) = (*first, *first);
    for q in points {
        low = Point2::new(low.x.min(q.x), low.y.min(q.y));
        high = Point2::new(high.x.max(q.x), high.y.max(q.y));
    }
    Rect {
        x: low.x - pad,
        y: low.y - pad,
        w: (high.x - low.x) + pad * 2.0,
        h: (high.y - low.y) + pad * 2.0,
    }
}

/// An upright box around everything `a` paints, stroke width and arrow
/// head included. The selection highlight is drawn around it.
pub fn bounds(a: &Annotation) -> Rect {
    use AnnotationKind as K;
    match &a.kind {
        K::Pen { stroke, points } | K::Marker { stroke, points } => span(points, stroke.thickness / 2.0),
        K::Line { stroke, start, end }
        | K::Rectangle { stroke, start, end }
        | K::Ellipse { stroke, start, end } => span(&[*start, *end], stroke.thickness / 2.0),
        K::Arrow { stroke, start, end } => span(&[*start, *end], arrow_head_reach(stroke.thickness)),
        K::Pixelate { start, end, .. } | K::Spotlight { start, end } => span(&[*start, *end], 0.0),
        K::Magnify { stroke, center, edge, .. } => {
            let outer = crate::annotation::magnify_radius(*center, *edge) + stroke.thickness / 2.0;
            grow(Rect { x: center.x, y: center.y, w: 0.0, h: 0.0 }, outer)
        }
        K::Text { position, text, font_size, .. } => text_box(*position, text, *font_size),
        K::Step { center, number, look } => step_box(*center, *number, look),
        K::TextMark { rects, look } => crate::mark::bounds(rects, look.style),
    }
}

/// Move every point of `a` by (dx, dy), changing it in place.
pub fn translate(a: &mut Annotation, dx: f32, dy: f32) {
    use AnnotationKind as K;
    let nudge = |q: &mut Point2| *q = Point2::new(q.x + dx, q.y + dy);
    match &mut a.kind {
        K::Pen { points, .. } | K::Marker { points, .. } => points.iter_mut().for_each(nudge),
        K::Line { start, end, .. }
        | K::Arrow { start, end, .. }
        | K::Rectangle { start, end, .. }
        | K::Ellipse { start, end, .. }
        | K::Pixelate { start, end, .. }
        | K::Spotlight { start, end }
        | K::Magnify { center: start, edge: end, .. } => {
            nudge(start);
            nudge(end);
        }
        K::Text { position: anchor, .. } | K::Step { center: anchor, .. } => nudge(anchor),
        K::TextMark { rects, .. } => {
            for line in rects.iter_mut() {
                line.x += dx;
                line.y += dy;
            }
        }
    }
}

/// A moved copy of `a`, leaving `a` alone. Saving uses it to carry marks
/// from window coordinates into the cropped picture's own.
pub fn translated(a: &Annotation, dx: f32, dy: f32) -> Annotation {
    let mut copy = a.clone();
    translate(&mut copy, dx, dy);
    copy
}

/// The number the Step tool's next click gives: one more than the highest
/// step on the canvas, or 1 when there is none. Reading it from the canvas
/// keeps the count right after undo, redo, or deleting a step.
pub fn next_step_number(annotations: &[Annotation]) -> u32 {
    let highest = annotations.iter().filter_map(|a| match a.kind {
        AnnotationKind::Step { number, .. } => Some(number),
        _ => None,
    });
    highest.max().map_or(1, |n| n.saturating_add(1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{annotation::Stroke, color::Rgba};

    fn pt(x: f32, y: f32) -> Point2 { Point2::new(x, y) }

    /// A 4 px red stroke, so the reach is 2 + 5 = 7 px from the centre line.
    fn ink() -> Stroke { Stroke { color: Rgba::RED, thickness: 4.0 } }

    /// `start` dragged to `end` with `make`, the way the editor builds one.
    fn dragged(make: fn(Stroke, Point2) -> Annotation, start: Point2, end: Point2) -> Annotation {
        let mut mark = make(ink(), start);
        mark.extend(end);
        mark
    }

    #[test]
    fn a_pen_path_is_hit_along_each_segment_only() {
        let mut path = Annotation::pen(ink(), pt(10.0, 10.0));
        path.extend(pt(50.0, 10.0));
        path.extend(pt(50.0, 40.0));
        assert!(hit_test(&path, pt(30.0, 12.0)), "first leg");
        assert!(hit_test(&path, pt(48.0, 30.0)), "second leg");
        assert!(!hit_test(&path, pt(30.0, 35.0)), "inside the corner, away from ink");
        assert!(!hit_test(&path, pt(200.0, 200.0)));
    }

    #[test]
    fn a_one_point_pen_path_is_a_dot() {
        let dot = Annotation::pen(ink(), pt(10.0, 10.0));
        assert!(hit_test(&dot, pt(14.0, 14.0)));
        assert!(!hit_test(&dot, pt(40.0, 40.0)));
    }

    #[test]
    fn a_line_is_hit_on_the_segment_not_beyond_it() {
        let line = dragged(Annotation::line, pt(0.0, 0.0), pt(100.0, 100.0));
        assert!(hit_test(&line, pt(51.0, 49.0)));
        assert!(!hit_test(&line, pt(50.0, 70.0)));
        assert!(!hit_test(&line, pt(-40.0, -40.0)), "on the line's extension, past its end");
    }

    #[test]
    fn an_arrow_head_reaches_further_than_its_shaft() {
        let arrow = dragged(Annotation::arrow, pt(0.0, 0.0), pt(100.0, 0.0));
        assert!(hit_test(&arrow, pt(50.0, 6.0)), "shaft");
        assert!(!hit_test(&arrow, pt(50.0, 10.0)), "10 px off the shaft is a miss");
        assert!(hit_test(&arrow, pt(100.0, 10.0)), "10 px off the tip is the head");
        assert!(!hit_test(&arrow, pt(50.0, 30.0)));
    }

    #[test]
    fn a_rectangle_is_a_hollow_frame() {
        let frame = dragged(Annotation::rectangle, pt(10.0, 10.0), pt(110.0, 60.0));
        assert!(hit_test(&frame, pt(60.0, 12.0)), "top");
        assert!(hit_test(&frame, pt(108.0, 35.0)), "right");
        assert!(hit_test(&frame, pt(60.0, 60.0)), "bottom");
        assert!(!hit_test(&frame, pt(60.0, 35.0)), "the middle is see-through");
    }

    #[test]
    fn an_ellipse_is_a_hollow_ring() {
        let ring = dragged(Annotation::ellipse, pt(0.0, 0.0), pt(200.0, 100.0));
        assert!(hit_test(&ring, pt(100.0, 2.0)), "top");
        assert!(hit_test(&ring, pt(198.0, 50.0)), "right");
        assert!(!hit_test(&ring, pt(100.0, 50.0)), "the middle is see-through");
        assert!(!hit_test(&ring, pt(0.0, 0.0)), "its box corner is off the curve");
    }

    #[test]
    fn a_flattened_ellipse_is_hit_like_a_line() {
        let flat = dragged(Annotation::ellipse, pt(0.0, 50.0), pt(100.0, 50.0));
        assert!(hit_test(&flat, pt(50.0, 52.0)));
        assert!(!hit_test(&flat, pt(50.0, 70.0)));
    }

    #[test]
    fn the_marker_is_six_times_wider_to_click() {
        let mut swipe = Annotation::marker(ink(), pt(0.0, 0.0), 0xC8);
        swipe.extend(pt(100.0, 0.0));
        // 4 px becomes 24 px: a reach of 12 + 5.
        assert!(hit_test(&swipe, pt(50.0, 16.0)));
        assert!(!hit_test(&swipe, pt(50.0, 18.0)));
    }

    #[test]
    fn text_is_hit_inside_its_measured_box_plus_slop() {
        let at = pt(20.0, 30.0);
        let label = Annotation::text(Rgba::RED, at, "hi");
        let (w, h) = text_extent("hi", crate::text::DEFAULT_PX);
        assert!(w > 0.0 && h > 0.0);
        assert!(hit_test(&label, pt(at.x + 2.0, at.y + 2.0)));
        assert!(hit_test(&label, pt(at.x + w + HIT_SLOP - 1.0, at.y + h + HIT_SLOP - 1.0)), "corner, within slop");
        assert!(!hit_test(&label, pt(at.x + w + HIT_SLOP + 1.0, at.y + 2.0)), "right of it");
        assert!(!hit_test(&label, pt(at.x + 2.0, at.y + h + HIT_SLOP + 1.0)), "below it");
    }

    #[test]
    fn empty_text_is_one_line_tall_and_no_width() {
        let (w, h) = text_extent("", 30.0);
        assert_eq!(w, 0.0);
        assert_eq!(h, crate::text::line_height(crate::text::clamp_px(30.0)));
    }

    #[test]
    fn a_step_disc_is_hit_across_its_face() {
        let step = Annotation::step(Rgba::RED, pt(100.0, 100.0), 3);
        assert!(hit_test(&step, pt(100.0, 100.0)));
        assert!(hit_test(&step, pt(112.0, 100.0)));
        assert!(!hit_test(&step, pt(130.0, 100.0)), "past the disc and the slop");
    }

    #[test]
    fn a_blur_is_hit_anywhere_inside() {
        let mut blur = Annotation::pixelate(pt(10.0, 10.0));
        blur.extend(pt(60.0, 40.0));
        assert!(hit_test(&blur, pt(35.0, 25.0)));
        assert!(hit_test(&blur, pt(64.0, 25.0)), "within slop of its edge");
        assert!(!hit_test(&blur, pt(90.0, 25.0)));
    }

    #[test]
    fn the_last_drawn_mark_wins_a_shared_spot() {
        let frame = dragged(Annotation::rectangle, pt(0.0, 0.0), pt(100.0, 100.0));
        let step = Annotation::step(Rgba::RED, pt(0.0, 0.0), 1);
        let marks = [frame, step];
        assert_eq!(hit_test_topmost(&marks, pt(0.0, 0.0)), Some(1));
        assert_eq!(hit_test_topmost(&marks, pt(100.0, 50.0)), Some(0));
        assert_eq!(hit_test_topmost(&marks, pt(400.0, 400.0)), None);
    }

    #[test]
    fn a_spotlight_loses_to_any_ink_over_it() {
        let arrow = dragged(Annotation::arrow, pt(0.0, 50.0), pt(100.0, 50.0));
        let mut spot = Annotation::spotlight(pt(0.0, 0.0));
        spot.extend(pt(200.0, 200.0));
        // Later in the list, yet painted beneath the arrow.
        let marks = [arrow, spot];
        assert_eq!(hit_test_topmost(&marks, pt(50.0, 50.0)), Some(0));
        assert_eq!(hit_test_topmost(&marks, pt(150.0, 150.0)), Some(1));
    }

    #[test]
    fn a_lens_is_a_solid_disc_with_a_capped_size() {
        let mut lens = Annotation::magnify(ink(), pt(100.0, 100.0), 2);
        assert!(lens.is_degenerate(), "a bare click makes no lens");
        lens.extend(pt(140.0, 100.0));
        assert!(!lens.is_degenerate());
        assert!(hit_test(&lens, pt(100.0, 100.0)) && hit_test(&lens, pt(130.0, 110.0)));
        assert!(!hit_test(&lens, pt(200.0, 100.0)));
        let mut huge = Annotation::magnify(ink(), pt(0.0, 0.0), 9);
        huge.extend(pt(5000.0, 0.0));
        let widest = (crate::annotation::MAGNIFY_MAX_RADIUS + 2.0) * 2.0;
        assert_eq!(bounds(&huge).w, widest, "radius capped, plus half the stroke");
        let AnnotationKind::Magnify { zoom, .. } = huge.kind else { unreachable!() };
        assert_eq!(zoom, 4, "zoom is held to 2 through 4");
        let mut sliver = Annotation::spotlight(pt(0.0, 0.0));
        sliver.extend(pt(300.0, 2.0));
        assert!(sliver.is_degenerate(), "a sliver spotlights nothing");
    }

    #[test]
    fn nothing_is_hit_on_an_empty_canvas() {
        assert_eq!(hit_test_topmost(&[], pt(1.0, 1.0)), None);
    }

    /// One of every kind of mark, each with some size to it.
    fn one_of_each() -> Vec<Annotation> {
        let (from, to) = (pt(1.0, 2.0), pt(3.0, 4.0));
        let mut pen = Annotation::pen(ink(), from);
        pen.extend(to);
        let mut swipe = Annotation::marker(ink(), from, 0xC8);
        swipe.extend(to);
        let mut blur = Annotation::pixelate(from);
        blur.extend(to);
        let mut spot = Annotation::spotlight(from);
        spot.extend(pt(30.0, 40.0));
        let mut lens = Annotation::magnify(ink(), pt(50.0, 50.0), 2);
        lens.extend(pt(80.0, 50.0));
        vec![
            pen,
            swipe,
            dragged(Annotation::line, from, to),
            dragged(Annotation::arrow, from, to),
            dragged(Annotation::rectangle, from, to),
            dragged(Annotation::ellipse, from, to),
            blur,
            spot,
            lens,
            Annotation::text(Rgba::RED, from, "abc"),
            Annotation::step(Rgba::RED, from, 7),
        ]
    }

    fn close(a: f32, b: f32) -> bool { (a - b).abs() < 0.001 }

    #[test]
    fn moving_shifts_the_box_of_every_kind_without_resizing_it() {
        for mark in one_of_each() {
            let was = bounds(&mark);
            let now = bounds(&translated(&mark, 25.0, -13.0));
            assert!(close(now.x, was.x + 25.0) && close(now.y, was.y - 13.0), "{:?}", mark.kind);
            assert!(close(now.w, was.w) && close(now.h, was.h), "{:?} changed size", mark.kind);
        }
    }

    #[test]
    fn moving_there_and_back_restores_every_kind_exactly() {
        for mark in one_of_each() {
            let mut moved = mark.clone();
            translate(&mut moved, 37.5, -64.0);
            translate(&mut moved, -37.5, 64.0);
            assert_eq!(moved, mark);
        }
    }

    #[test]
    fn a_moved_mark_no_longer_answers_where_it_was() {
        for mark in one_of_each() {
            let b = bounds(&mark);
            let old_middle = pt(b.x + b.w / 2.0, b.y + b.h / 2.0);
            let moved = translated(&mark, 500.0, 500.0);
            assert!(!hit_test(&moved, old_middle), "{:?}", mark.kind);
            assert_eq!(std::mem::discriminant(&mark.kind), std::mem::discriminant(&moved.kind));
            assert_eq!(moved.time, mark.time);
        }
    }

    #[test]
    fn moving_keeps_a_video_time_window() {
        let mut pen = Annotation::pen(ink(), pt(0.0, 0.0));
        pen.time = Some((1.0, 4.0));
        assert_eq!(translated(&pen, 5.0, 5.0).time, Some((1.0, 4.0)));
    }

    #[test]
    fn a_highlight_moves_every_line_box() {
        let mut mark = Annotation {
            kind: AnnotationKind::TextMark {
                rects: vec![Rect { x: 1.0, y: 2.0, w: 30.0, h: 10.0 }, Rect { x: 1.0, y: 14.0, w: 20.0, h: 10.0 }],
                look: Default::default(),
            },
            time: None,
        };
        translate(&mut mark, 10.0, 5.0);
        let AnnotationKind::TextMark { rects, .. } = &mark.kind else { unreachable!() };
        assert_eq!((rects[0].x, rects[0].y, rects[1].x, rects[1].y), (11.0, 7.0, 11.0, 19.0));
        assert_eq!((rects[0].w, rects[1].w), (30.0, 20.0), "sizes untouched");
    }

    #[test]
    fn a_step_box_covers_its_shape() {
        let disc = Annotation::step(Rgba::RED, pt(100.0, 100.0), 1);
        let b = bounds(&disc);
        assert_eq!((b.x, b.y, b.w, b.h), (85.0, 85.0, 30.0, 30.0), "a disc of radius 15");
        // A pin rises above the point it marks.
        use crate::step::{StepLook, StepShape};
        let pin = Annotation::step_styled(pt(100.0, 100.0), 1, StepLook::recipe(StepShape::Pin, Rgba::RED));
        let b = bounds(&pin);
        assert!(b.y < 60.0 && b.y + b.h <= 103.0, "{b:?}");
        assert!(hit_test(&pin, pt(100.0, 75.0)), "the head");
        assert!(!hit_test(&pin, pt(100.0, 115.0)), "under the point");
    }

    #[test]
    fn a_path_with_no_points_has_an_empty_box() {
        let bare = Annotation { kind: AnnotationKind::Pen { stroke: ink(), points: Vec::new() }, time: None };
        assert!(bounds(&bare).is_empty());
        assert!(!hit_test(&bare, pt(0.0, 0.0)));
    }

    #[test]
    fn the_next_step_follows_the_highest_on_the_canvas() {
        assert_eq!(next_step_number(&Vec::new()), 1, "the first step is 1");
        let marks = [
            Annotation::step(Rgba::RED, pt(0.0, 0.0), 1),
            Annotation::pen(ink(), pt(0.0, 0.0)),
            Annotation::step(Rgba::RED, pt(0.0, 0.0), 2),
        ];
        assert_eq!(next_step_number(&marks), 3);
        assert_eq!(next_step_number(&marks[..1]), 2);
        assert_eq!(next_step_number(&[Annotation::step(Rgba::RED, pt(0.0, 0.0), u32::MAX)]), u32::MAX);
    }
}
