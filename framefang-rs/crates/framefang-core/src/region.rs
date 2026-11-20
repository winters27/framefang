//! The rectangle a screen recording covers, and where the REC indicator goes
//! so it stays out of that rectangle.
//!
//! This is plain arithmetic shared by the region picker and every recorder
//! backend, so one dragged rectangle means the same pixels to X11
//! `x11grab`, Windows `gdigrab` and macOS `screencapture`, and so no encoder
//! is ever handed a size it rejects.
//!
//! **Units are virtual-desktop pixels**, the space the monitors are stitched
//! into. Its corner ([`DesktopBounds`]'s `x`, `y`) is often not `(0, 0)`: on
//! Windows and macOS a monitor to the left of the primary one has a negative
//! `x`.
//!
//! The backends measure from different corners, so [`CaptureRect`] gives
//! both readings:
//!
//! * absolute, the `x` and `y` fields, measured from the main display's
//!   top-left. macOS `screencapture -R` wants this.
//! * relative to the desktop's corner, [`CaptureRect::offset_x`] and
//!   [`CaptureRect::offset_y`]. X11 `x11grab` (`:0.0+X,Y` from the root
//!   window) and Windows `gdigrab` (`-offset_x` added to
//!   `SM_XVIRTUALSCREEN`) want this.
//!
//! **Width and height are always even.** Every backend encodes H.264 as
//! `yuv420p`, which stores color at half resolution in both directions, so
//! an odd side either fails or silently drops a column. [`even_dimension`]
//! does that rounding, and [`record_rect_from_selection`] applies it after
//! clamping, so no odd rectangle can leave this module.

/// The shortest side we will record, in pixels. Anything smaller is a slipped
/// click rather than a region, and a video that small is no use to anyone.
pub const MIN_RECORD_SIDE: u32 = 16;

/// The box around all monitors, in virtual-desktop pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DesktopBounds {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    /// Device pixels per logical point on the desktop these numbers came
    /// from. It is `1.0` unless the capture was stitched at a higher
    /// density. Backends that take points instead of pixels (macOS
    /// `screencapture -R`) divide by it.
    pub scale: f32,
}

impl DesktopBounds {
    /// Bounds at 1x.
    pub fn new(x: i32, y: i32, width: u32, height: u32) -> Self {
        DesktopBounds { x, y, width, height, scale: 1.0 }
    }

    /// The same bounds on a desktop with `scale` pixels per point. A scale
    /// that is zero, negative or not finite is stored as `1.0`.
    pub fn with_scale(self, scale: f32) -> Self {
        let usable = scale.is_finite() && scale > 0.0;
        DesktopBounds { scale: if usable { scale } else { 1.0 }, ..self }
    }

    /// The column just past the right edge.
    pub fn right(&self) -> i32 {
        far_edge(self.x, self.width)
    }

    /// The row just past the bottom edge.
    pub fn bottom(&self) -> i32 {
        far_edge(self.y, self.height)
    }
}

/// `start + len`, pinned at `i32::MAX` instead of wrapping.
fn far_edge(start: i32, len: u32) -> i32 {
    start.saturating_add(len as i32)
}

/// A rectangle ready to record: inside the desktop, even on both sides and
/// at least [`MIN_RECORD_SIDE`] each way. Only
/// [`record_rect_from_selection`] should build one, since those guarantees
/// are what the type is for.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CaptureRect {
    /// Left edge, absolute virtual-desktop pixels.
    pub x: i32,
    /// Top edge, absolute virtual-desktop pixels.
    pub y: i32,
    /// Even, and never below [`MIN_RECORD_SIDE`].
    pub width: u32,
    /// Even, and never below [`MIN_RECORD_SIDE`].
    pub height: u32,
    /// The desktop's top-left corner when the rectangle was picked. Kept
    /// here so a backend that measures from that corner does not have to
    /// ask for the monitor layout again, and cannot get a different answer
    /// if the user moved a display in the meantime.
    pub desktop_origin: (i32, i32),
    /// Pixels per point on the desktop it was picked on (see
    /// [`DesktopBounds::scale`]).
    pub scale: f32,
}

impl CaptureRect {
    /// `(x, y, width, height)` in logical points: each value divided by the
    /// scale and rounded to the nearest point, with width and height kept at
    /// one point or more. At 1x this is the pixel rectangle itself. Only for
    /// backends that take points (macOS `screencapture -R`); the pixel
    /// fields are the truth everywhere else.
    pub fn points(&self) -> (i32, i32, u32, u32) {
        let to_points = |v: f32| (v / self.scale).round();
        let edge = |v: i32| to_points(v as f32) as i32;
        let side = |v: u32| to_points(v as f32).max(1.0) as u32;
        (edge(self.x), edge(self.y), side(self.width), side(self.height))
    }

    /// The column just past the right edge, absolute.
    pub fn right(&self) -> i32 {
        far_edge(self.x, self.width)
    }

    /// The row just past the bottom edge, absolute.
    pub fn bottom(&self) -> i32 {
        far_edge(self.y, self.height)
    }

    /// Left edge counted from the desktop's corner, the number `x11grab`'s
    /// `+X,Y` and `gdigrab`'s `-offset_x` take.
    pub fn offset_x(&self) -> i32 {
        self.x.saturating_sub(self.desktop_origin.0)
    }

    /// Top edge counted from the desktop's corner.
    pub fn offset_y(&self) -> i32 {
        self.y.saturating_sub(self.desktop_origin.1)
    }

    /// The size as ffmpeg's `-video_size` spells it, `WIDTHxHEIGHT`.
    pub fn video_size(&self) -> String {
        format!("{}x{}", self.width, self.height)
    }

    /// True when the rectangle `(x, y, w, h)` covers at least one pixel of
    /// this one. Rectangles that only touch along an edge do not overlap.
    pub fn overlaps(&self, x: i32, y: i32, w: u32, h: u32) -> bool {
        let across = x < self.right() && far_edge(x, w) > self.x;
        let down = y < self.bottom() && far_edge(y, h) > self.y;
        across && down
    }
}

/// Round a side length down to an even number of pixels.
///
/// Down, not up: rounding up would either reach past the desktop's edge or
/// record a column the user did not pick. Losing one column is the smaller
/// harm.
pub fn even_dimension(v: u32) -> u32 {
    v - v % 2
}

/// Order the two ends of a span given as a start and a signed length, so a
/// drag in either direction gives `(low, high)`.
fn ordered_span(start: i32, len: i32) -> (i32, i32) {
    let end = start.saturating_add(len);
    (start.min(end), start.max(end))
}

/// Turn the rectangle the user dragged into one every backend can record.
///
/// `selection` is `(x, y, w, h)` in virtual-desktop pixels; `w` and `h` are
/// negative when the drag went left or up. The result is cut to `bounds` and
/// rounded down to even sides. `None` means nothing usable is left: the
/// selection lies off the desktop, or a side ends up under
/// [`MIN_RECORD_SIDE`].
pub fn record_rect_from_selection(selection: (i32, i32, i32, i32), bounds: DesktopBounds) -> Option<CaptureRect> {
    let (left, right) = ordered_span(selection.0, selection.2);
    let (top, bottom) = ordered_span(selection.1, selection.3);

    let left = left.max(bounds.x);
    let top = top.max(bounds.y);
    let right = right.min(bounds.right());
    let bottom = bottom.min(bounds.bottom());
    if right <= left || bottom <= top {
        return None;
    }

    let width = even_dimension(right.abs_diff(left));
    let height = even_dimension(bottom.abs_diff(top));
    if width.min(height) < MIN_RECORD_SIDE {
        return None;
    }

    let desktop_origin = (bounds.x, bounds.y);
    Some(CaptureRect { x: left, y: top, width, height, desktop_origin, scale: bounds.scale })
}

/// Where the floating REC indicator should sit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IndicatorPlacement {
    pub x: i32,
    pub y: i32,
    /// True when the panel is completely outside the recorded rectangle.
    /// Where the OS cannot hide a window from the capture (X11), false means
    /// the panel would be burned into the video, so callers have to check
    /// this rather than assume it.
    pub clear_of_region: bool,
}

/// Find a spot for the REC indicator, of `size`, that keeps it out of the
/// recording.
///
/// Spots are tried in this order: under the region, over it, right of it,
/// left of it, and finally the desktop's top-right corner. The first one
/// that is fully on the desktop and does not touch the region wins.
/// Recording the whole desktop leaves no such spot; then the corner comes
/// back with `clear_of_region: false` and the caller chooses between a
/// visible panel and a clean video. Without a region the corner is used and
/// counts as clear.
///
/// `margin` is the gap kept between the panel and what it avoids.
pub fn indicator_placement(bounds: DesktopBounds, region: Option<CaptureRect>, size: (u32, u32), margin: i32) -> IndicatorPlacement {
    let (panel_w, panel_h) = (size.0 as i32, size.1 as i32);

    // One margin in from the desktop's top-right corner (never past its left
    // edge on a desktop narrower than the panel). The indicator's usual home.
    let corner_x = (bounds.right() - panel_w - margin).max(bounds.x + margin);
    let corner = (corner_x, margin + bounds.y);
    let at = |(x, y): (i32, i32), clear_of_region: bool| IndicatorPlacement { x, y, clear_of_region };

    let Some(rec) = region else {
        return at(corner, true);
    };

    let on_desktop = |(x, y): (i32, i32)| {
        x >= bounds.x
            && y >= bounds.y
            && x.saturating_add(panel_w) <= bounds.right()
            && y.saturating_add(panel_h) <= bounds.bottom()
    };
    let clear = |(x, y): (i32, i32)| !rec.overlaps(x, y, size.0, size.1);

    let under = (rec.right() - panel_w, rec.bottom() + margin);
    let over = (rec.right() - panel_w, rec.y - margin - panel_h);
    let beside_right = (rec.right() + margin, rec.y);
    let beside_left = (rec.x - margin - panel_w, rec.y);

    [under, over, beside_right, beside_left, corner]
        .into_iter()
        .find(|&spot| on_desktop(spot) && clear(spot))
        .map_or_else(|| at(corner, false), |spot| at(spot, true))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A single 1080p monitor at the origin.
    fn hd() -> DesktopBounds {
        DesktopBounds::new(0, 0, 1920, 1080)
    }

    fn pick(sel: (i32, i32, i32, i32), bounds: DesktopBounds) -> CaptureRect {
        record_rect_from_selection(sel, bounds).expect("selection should be recordable")
    }

    fn xywh(r: &CaptureRect) -> (i32, i32, u32, u32) {
        (r.x, r.y, r.width, r.height)
    }

    // Scale and points.

    #[test]
    fn points_halve_everything_on_a_2x_desktop() {
        let retina = DesktopBounds::new(0, 0, 2880, 1800).with_scale(2.0);
        let r = pick((300, 160, 640, 480), retina);
        assert_eq!(xywh(&r), (300, 160, 640, 480), "pixel fields stay in pixels");
        assert_eq!(r.points(), (150, 80, 320, 240));
    }

    #[test]
    fn points_equal_pixels_at_1x() {
        let r = pick((124, 46, 678, 90), hd());
        assert_eq!((r.scale, r.points()), (1.0, (124, 46, 678, 90)));
    }

    #[test]
    fn points_round_to_nearest_and_keep_at_least_one() {
        let r = CaptureRect { x: 3, y: -3, width: 16, height: 16, desktop_origin: (0, 0), scale: 2.0 };
        // 1.5 and -1.5 round away from zero.
        assert_eq!(r.points(), (2, -2, 8, 8));
        let dense = CaptureRect { scale: 40.0, ..r };
        assert_eq!(dense.points().2, 1, "a side never collapses to zero points");
        assert_eq!(dense.points().3, 1);
    }

    #[test]
    fn with_scale_ignores_unusable_values() {
        let base = DesktopBounds::new(0, 0, 10, 10);
        assert_eq!(base.with_scale(0.0).scale, 1.0);
        assert_eq!(base.with_scale(-2.0).scale, 1.0);
        assert_eq!(base.with_scale(f32::NAN).scale, 1.0);
        assert_eq!(base.with_scale(f32::INFINITY).scale, 1.0);
        assert_eq!(base.with_scale(1.5).scale, 1.5);
        assert_eq!(base.with_scale(1.5).width, 10, "only the scale changes");
    }

    // Even sides.

    #[test]
    fn even_dimension_only_ever_rounds_down() {
        let cases = [(0, 0), (1, 0), (2, 2), (3, 2), (1919, 1918), (1920, 1920), (u32::MAX, u32::MAX - 1)];
        for (input, want) in cases {
            assert_eq!(even_dimension(input), want, "{input}");
        }
    }

    #[test]
    fn an_odd_drag_records_even_sides() {
        // A typical hand-drawn 401 x 303 drag, which yuv420p cannot encode.
        let r = pick((10, 20, 401, 303), hd());
        assert_eq!(xywh(&r), (10, 20, 400, 302));
    }

    // Clamping.

    #[test]
    fn a_drag_past_the_right_and_bottom_stops_at_the_edge() {
        let r = pick((1800, 1000, 500, 500), hd());
        assert_eq!((r.x, r.y), (1800, 1000));
        assert_eq!((r.right(), r.bottom()), (1920, 1080));
    }

    #[test]
    fn a_drag_starting_off_screen_is_cut_back_in() {
        let r = pick((-200, -100, 600, 400), hd());
        assert_eq!(xywh(&r), (0, 0, 400, 300));
    }

    #[test]
    fn dragging_up_and_left_gives_the_same_rectangle() {
        let backwards = pick((500, 400, -300, -200), hd());
        let forwards = pick((200, 200, 300, 200), hd());
        assert_eq!(backwards, forwards);
        assert_eq!(xywh(&backwards), (200, 200, 300, 200));
    }

    #[test]
    fn a_selection_off_the_desktop_is_refused() {
        assert!(record_rect_from_selection((3000, 3000, 100, 100), hd()).is_none());
        assert!(record_rect_from_selection((-500, 10, 100, 100), hd()).is_none());
        // Touching the edge from outside is still nothing.
        assert!(record_rect_from_selection((1920, 0, 100, 100), hd()).is_none());
    }

    #[test]
    fn slivers_are_refused() {
        // A twitch during a click.
        assert!(record_rect_from_selection((10, 10, 6, 6), hd()).is_none());
        // Wide enough but one side short is still refused: a 200 x 14
        // strip is not a watchable video.
        assert!(record_rect_from_selection((10, 10, 200, 14), hd()).is_none());
        assert!(record_rect_from_selection((10, 10, 0, 200), hd()).is_none());
    }

    #[test]
    fn rounding_down_never_dips_below_the_minimum() {
        // 17 becomes 16, exactly the minimum, which is allowed.
        assert_eq!(xywh(&pick((0, 0, 17, 17), hd())), (0, 0, 16, 16));
        // 15 would become 14, which is not.
        assert!(record_rect_from_selection((0, 0, 15, 200), hd()).is_none());
    }

    #[test]
    fn extreme_selections_saturate_instead_of_wrapping() {
        // Both the selection's far edge and the desktop's right edge would
        // pass i32::MAX; they stop there instead.
        let near_max = DesktopBounds::new(i32::MAX - 100, 0, 1000, 100);
        assert_eq!(near_max.right(), i32::MAX);
        let r = pick((i32::MAX - 51, 0, i32::MAX, 100), near_max);
        assert_eq!(xywh(&r), (i32::MAX - 51, 0, 50, 100));
        assert_eq!(r.right(), i32::MAX - 1);
    }

    // Absolute versus desktop-relative.

    #[test]
    fn offsets_count_from_a_negative_desktop_corner() {
        // A monitor left of the primary puts the corner at x = -1920. This is
        // the layout where gdigrab's -offset_x and screencapture's -R part
        // ways.
        let wide = DesktopBounds::new(-1920, 0, 3840, 1080);
        let r = pick((-1000, 100, 800, 600), wide);
        assert_eq!((r.x, r.y), (-1000, 100));
        assert_eq!((r.offset_x(), r.offset_y()), (920, 100));
        assert_eq!(r.desktop_origin, (-1920, 0));
        assert_eq!(r.video_size(), "800x600");
    }

    #[test]
    fn offsets_and_absolute_agree_when_the_corner_is_the_origin() {
        let r = pick((320, 240, 800, 600), hd());
        assert_eq!((r.offset_x(), r.offset_y()), (r.x, r.y));
    }

    // Overlap.

    #[test]
    fn overlap_needs_a_shared_pixel() {
        let r = pick((100, 100, 200, 200), hd());
        assert!(r.overlaps(150, 150, 10, 10), "fully inside");
        assert!(r.overlaps(90, 90, 20, 20), "over the top-left corner");
        assert!(r.overlaps(0, 0, 1000, 1000), "covering it entirely");
        assert!(!r.overlaps(300, 100, 50, 50), "touching the right side");
        assert!(!r.overlaps(100, 300, 50, 50), "touching the bottom");
        assert!(!r.overlaps(0, 0, 100, 100), "touching the top-left corner");
    }

    // REC indicator.

    const PANEL_W: i32 = 240;
    const PANEL_H: i32 = 60;
    const PANEL: (u32, u32) = (PANEL_W as u32, PANEL_H as u32);
    const GAP: i32 = 24;
    /// Where the panel sits in [`hd`]'s top-right corner.
    const HD_CORNER: (i32, i32) = (1920 - PANEL_W - GAP, GAP);

    fn assert_clear_and_on_screen(p: IndicatorPlacement, r: &CaptureRect, bounds: DesktopBounds) {
        assert!(p.clear_of_region, "no clear spot was found");
        assert!(!r.overlaps(p.x, p.y, PANEL.0, PANEL.1), "panel covers the recording");
        let panel_box = (p.x, p.y, p.x + PANEL_W, p.y + PANEL_H);
        assert!(panel_box.0 >= bounds.x && panel_box.1 >= bounds.y, "{panel_box:?} starts off the desktop");
        assert!(panel_box.2 <= bounds.right() && panel_box.3 <= bounds.bottom(), "{panel_box:?} ends off it");
    }

    #[test]
    fn with_no_region_the_indicator_uses_the_corner() {
        let p = indicator_placement(hd(), None, PANEL, GAP);
        assert_eq!(p, IndicatorPlacement { x: HD_CORNER.0, y: HD_CORNER.1, clear_of_region: true });
    }

    #[test]
    fn the_indicator_goes_under_a_region_first() {
        let r = pick((200, 100, 800, 600), hd());
        let p = indicator_placement(hd(), Some(r), PANEL, GAP);
        assert_clear_and_on_screen(p, &r, hd());
        assert_eq!((p.x, p.y), (r.right() - PANEL_W, r.bottom() + GAP), "right-aligned under it");
    }

    #[test]
    fn a_region_on_the_bottom_edge_pushes_it_above() {
        let r = pick((200, 300, 800, 780), hd());
        assert_eq!(r.bottom(), hd().bottom());
        let p = indicator_placement(hd(), Some(r), PANEL, GAP);
        assert_clear_and_on_screen(p, &r, hd());
        assert_eq!(p.y, r.y - GAP - PANEL_H);
    }

    #[test]
    fn a_full_height_region_puts_it_alongside() {
        let r = pick((0, 0, 1000, 1080), hd());
        let p = indicator_placement(hd(), Some(r), PANEL, GAP);
        assert_clear_and_on_screen(p, &r, hd());
        assert_eq!((p.x, p.y), (r.right() + GAP, r.y));
    }

    #[test]
    fn a_full_height_region_on_the_right_puts_it_to_the_left() {
        let r = pick((900, 0, 1020, 1080), hd());
        let p = indicator_placement(hd(), Some(r), PANEL, GAP);
        assert_clear_and_on_screen(p, &r, hd());
        assert_eq!((p.x, p.y), (900 - GAP - PANEL_W, 0));
    }

    #[test]
    fn recording_the_whole_desktop_leaves_no_clear_spot() {
        let r = pick((0, 0, 1920, 1080), hd());
        let p = indicator_placement(hd(), Some(r), PANEL, GAP);
        assert!(!p.clear_of_region, "every spot on this desktop is recorded");
        // The corner is still a real on-screen position, for callers that
        // choose to show the panel anyway.
        assert_eq!((p.x, p.y), HD_CORNER);
    }

    #[test]
    fn the_indicator_stays_on_a_desktop_with_a_negative_corner() {
        let tall_left = DesktopBounds::new(-2560, -300, 4480, 1440);
        let r = pick((-2400, -200, 600, 400), tall_left);
        let p = indicator_placement(tall_left, Some(r), PANEL, GAP);
        assert_clear_and_on_screen(p, &r, tall_left);
    }
}
