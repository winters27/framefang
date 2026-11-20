//! Multi-monitor geometry: the box around every monitor, and the conversions
//! between the overlay window, the stitched screenshot and the OS's virtual
//! screen.
//!
//! With two or more monitors there are three coordinate spaces, and using the
//! wrong one is how a region picked on a secondary monitor ends up cropping
//! someone else's pixels:
//!
//! | Space | Its `(0, 0)` | Who uses it |
//! | --- | --- | --- |
//! | virtual screen | the primary monitor's top-left; monitors left of or above it have negative coordinates | monitor rects, window positions (winit), the pin window |
//! | bitmap | the top-left of the stitched capture, which is the corner of the box around all monitors | the captured bitmap, the crop rectangle |
//! | frame | the top-left of the overlay window's client area | mouse events, annotations, all painting |
//!
//! When the overlay covers the whole virtual desktop, frame and bitmap are
//! the same space, offset from virtual-screen space by the box's corner. The
//! overlay reads its real position back from the window on every redraw, so
//! a window manager that ignores the requested placement shifts these
//! mappings instead of quietly cropping the wrong pixels.
//!
//! None of this cares about units. Use logical or physical pixels, just the
//! same kind everywhere. DPI scaling is layered on top by the caller.

/// A monitor's rectangle in virtual-screen coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MonitorRect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

impl MonitorRect {
    pub const fn new(x: i32, y: i32, width: u32, height: u32) -> Self {
        MonitorRect { x, y, width, height }
    }

    /// The first column past the monitor.
    pub const fn right(&self) -> i32 {
        self.width as i32 + self.x
    }

    /// The first row past the monitor.
    pub const fn bottom(&self) -> i32 {
        self.height as i32 + self.y
    }

    /// True when the point is on this monitor. The right and bottom edges
    /// belong to the neighbour, so a point on a seam is on exactly one
    /// monitor.
    pub const fn contains(&self, (px, py): (i32, i32)) -> bool {
        self.x <= px && px < self.right() && self.y <= py && py < self.bottom()
    }
}

/// The box around every monitor, as `(top-left corner, (width, height))` in
/// virtual-screen coordinates. `None` for an empty list, since no monitors
/// means no desktop.
pub fn union_bounds(monitors: &[MonitorRect]) -> Option<((i32, i32), (u32, u32))> {
    let (first, rest) = monitors.split_first()?;
    let start = (first.x, first.y, first.right(), first.bottom());
    let (left, top, right, bottom) = rest.iter().fold(start, |(l, t, r, b), m| {
        (l.min(m.x), t.min(m.y), r.max(m.right()), b.max(m.bottom()))
    });
    let extent = |lo: i32, hi: i32| (hi - lo).max(0) as u32;
    Some(((left, top), (extent(left, right), extent(top, bottom))))
}

/// The virtual desktop as the overlay sees it: where the stitched capture
/// starts in virtual-screen space, its size, and the monitors in it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DesktopGeometry {
    /// Virtual-screen position of the bitmap's top-left pixel.
    corner: (i32, i32),
    extent: (u32, u32),
    screens: Vec<MonitorRect>,
}

impl DesktopGeometry {
    /// The desktop made of these monitors, or `None` when there are none.
    pub fn from_monitors(monitors: Vec<MonitorRect>) -> Option<Self> {
        let bounds = union_bounds(&monitors);
        bounds.map(|(corner, extent)| DesktopGeometry { corner, extent, screens: monitors })
    }

    /// For an image that did not come from the screen (a video frame, for
    /// instance): anchored at `(0, 0)` and with no monitors, so the chrome
    /// lays out against the whole image instead of one monitor.
    pub fn bitmap(width: u32, height: u32) -> Self {
        DesktopGeometry { extent: (width, height), ..Default::default() }
    }

    /// Where the bitmap's top-left pixel is in virtual-screen coordinates.
    pub fn origin(&self) -> (i32, i32) {
        self.corner
    }

    pub fn size(&self) -> (u32, u32) {
        self.extent
    }

    pub fn monitors(&self) -> &[MonitorRect] {
        self.screens.as_slice()
    }

    /// True when a window sized to the primary monitor would not cover the
    /// whole desktop: there is more than one monitor, or the desktop does
    /// not start at the origin. In that case the overlay has to be placed
    /// explicitly rather than simply made fullscreen.
    pub fn spans_multiple_monitors(&self) -> bool {
        self.corner != (0, 0) || self.screens.len() > 1
    }
}

/// `p` moved by `by`.
fn shifted((x, y): (i32, i32), by: (i32, i32)) -> (i32, i32) {
    (x + by.0, y + by.1)
}

/// `p` moved back by `by`.
fn unshifted((x, y): (i32, i32), by: (i32, i32)) -> (i32, i32) {
    (x - by.0, y - by.1)
}

/// A point in the overlay window to the same point on the virtual screen.
pub fn frame_to_virtual(frame_origin: (i32, i32), p: (i32, i32)) -> (i32, i32) {
    shifted(p, frame_origin)
}

/// A point on the virtual screen to the same point in the overlay window.
pub fn virtual_to_frame(frame_origin: (i32, i32), p: (i32, i32)) -> (i32, i32) {
    unshifted(p, frame_origin)
}

/// A rectangle in the overlay window to virtual-screen coordinates. Only the
/// corner moves; the size is the same in both spaces.
pub fn frame_rect_to_virtual(frame_origin: (i32, i32), (x, y, w, h): (i32, i32, i32, i32)) -> (i32, i32, i32, i32) {
    let moved = shifted((x, y), frame_origin);
    (moved.0, moved.1, w, h)
}

/// A point in the overlay window to a pixel in the stitched capture.
///
/// `bitmap_origin` is where the capture starts on the virtual screen (the
/// corner of the box around all monitors, negative when a monitor sits left
/// of or above the primary). `frame_origin` is where the overlay window
/// actually is. The two match when the overlay covers the desktop as
/// requested; any difference between them is precisely the correction a
/// misplaced overlay needs.
pub fn frame_to_bitmap(bitmap_origin: (i32, i32), frame_origin: (i32, i32), p: (i32, i32)) -> (i32, i32) {
    unshifted(shifted(p, frame_origin), bitmap_origin)
}

/// A rectangle in the overlay window to a rectangle in the stitched capture.
/// The crop goes through this: a selection on a monitor left of the primary
/// has negative virtual-screen coordinates but a positive place in the
/// bitmap.
pub fn frame_rect_to_bitmap(
    bitmap_origin: (i32, i32), frame_origin: (i32, i32), (x, y, w, h): (i32, i32, i32, i32),
) -> (i32, i32, i32, i32) {
    let in_bitmap = frame_to_bitmap(bitmap_origin, frame_origin, (x, y));
    (in_bitmap.0, in_bitmap.1, w, h)
}

/// Where the overlay window's top-left pixel falls in the stitched capture.
/// Blits that walk the window add this to every coordinate.
pub fn bitmap_offset(bitmap_origin: (i32, i32), frame_origin: (i32, i32)) -> (i32, i32) {
    // The window's own (0, 0), moved onto the screen and then into the bitmap.
    unshifted(frame_origin, bitmap_origin)
}

/// The overlap of two `(x, y, w, h)` rectangles, or `None` when they share
/// no area (which includes either one being empty).
pub fn intersect_rect(a: (i32, i32, i32, i32), b: (i32, i32, i32, i32)) -> Option<(i32, i32, i32, i32)> {
    let left = a.0.max(b.0);
    let top = a.1.max(b.1);
    let right = (a.0 + a.2).min(b.0 + b.2);
    let bottom = (a.1 + a.3).min(b.1 + b.3);
    let has_area = right > left && bottom > top;
    has_area.then(|| (left, top, right - left, bottom - top))
}

/// The area, in overlay-window coordinates, that floating chrome (tool
/// panel, action panel, magnifier, hint chip) should keep itself inside:
/// the monitor under `point`, trimmed to the window.
///
/// When no monitor is under the point (a gap in an L-shaped layout, or an
/// empty monitor list for a non-screen image) the whole window is used.
pub fn monitor_bounds_in_frame(
    monitors: &[MonitorRect], frame_origin: (i32, i32), frame_size: (u32, u32), point: (i32, i32),
) -> (i32, i32, i32, i32) {
    let window = (0, 0, frame_size.0 as i32, frame_size.1 as i32);
    let on_screen = frame_to_virtual(frame_origin, point);
    monitors
        .iter()
        .find(|m| m.contains(on_screen))
        .and_then(|m| {
            let (fx, fy) = virtual_to_frame(frame_origin, (m.x, m.y));
            intersect_rect((fx, fy, m.width as i32, m.height as i32), window)
        })
        .unwrap_or(window)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The layout that used to crop the wrong pixels: a 1600 x 900 monitor
    /// left of and slightly above a 1080p primary, so the desktop starts at
    /// negative coordinates.
    fn laptop_left_of_primary() -> DesktopGeometry {
        desktop(&[PRIMARY, LAPTOP])
    }

    const PRIMARY: MonitorRect = MonitorRect::new(0, 0, 1920, 1080);
    const LAPTOP: MonitorRect = MonitorRect::new(-1600, -200, 1600, 900);

    fn desktop(monitors: &[MonitorRect]) -> DesktopGeometry {
        DesktopGeometry::from_monitors(monitors.to_vec()).expect("at least one monitor")
    }

    #[test]
    fn one_monitor_is_its_own_desktop() {
        let single = desktop(&[PRIMARY]);
        assert_eq!((single.origin(), single.size()), ((0, 0), (1920, 1080)));
        assert!(!single.spans_multiple_monitors(), "one monitor at the origin");
    }

    #[test]
    fn the_desktop_box_reaches_into_negative_coordinates() {
        let g = laptop_left_of_primary();
        assert_eq!((g.origin(), g.size()), ((-1600, -200), (3520, 1280)));
        assert_eq!(g.monitors(), &[PRIMARY, LAPTOP]);
        assert!(g.spans_multiple_monitors(), "two monitors");
    }

    #[test]
    fn no_monitors_means_no_desktop() {
        assert_eq!(DesktopGeometry::from_monitors(vec![]), None);
        assert_eq!(union_bounds(&[]), None);
    }

    #[test]
    fn the_box_covers_gaps_between_monitors() {
        let gapped = [MonitorRect::new(0, 0, 100, 100), MonitorRect::new(300, 50, 100, 100)];
        assert_eq!(union_bounds(&gapped), Some(((0, 0), (400, 150))));
    }

    #[test]
    fn a_lone_monitor_off_the_origin_still_needs_explicit_placement() {
        assert!(desktop(&[MonitorRect::new(-1920, 0, 1920, 1080)]).spans_multiple_monitors());
    }

    #[test]
    fn a_plain_bitmap_has_no_monitors() {
        let video = DesktopGeometry::bitmap(1280, 720);
        assert_eq!((video.origin(), video.size(), video.monitors().len()), ((0, 0), (1280, 720), 0));
        assert!(!video.spans_multiple_monitors(), "nothing to span");
        assert_eq!(DesktopGeometry::default(), DesktopGeometry::bitmap(0, 0));
    }

    #[test]
    fn monitor_edges_are_half_open() {
        let m = MonitorRect::new(-100, -50, 100, 50);
        assert_eq!((m.right(), m.bottom()), (0, 0));
        assert!(m.contains((-100, -50)));
        assert!(m.contains((-1, -1)));
        assert!(!m.contains((0, -1)), "the right edge belongs to the next monitor");
        assert!(!m.contains((-1, 0)), "so does the bottom edge");
    }

    #[test]
    fn frame_and_virtual_convert_both_ways() {
        let window_at = laptop_left_of_primary().origin();
        for p in [(0, 0), (7, 31), (1600, 200), (3519, 1279)] {
            let on_screen = frame_to_virtual(window_at, p);
            assert_eq!(virtual_to_frame(window_at, on_screen), p);
        }
        // The window's corner is the desktop's corner, and the primary
        // monitor's corner is the virtual-screen origin.
        assert_eq!(frame_to_virtual(window_at, (0, 0)), window_at);
        assert_eq!(virtual_to_frame(window_at, (0, 0)), (1600, 200));
    }

    /// A selection on the left monitor sits at a positive offset in the
    /// bitmap but a negative position on the virtual screen (where the pin
    /// window goes).
    #[test]
    fn a_selection_on_the_left_monitor_lands_inside_the_bitmap() {
        // The overlay covers the whole desktop, so the window and the bitmap
        // start at the same place.
        let corner = laptop_left_of_primary().origin();
        let picked = (100, 50, 400, 300);
        assert_eq!(frame_rect_to_bitmap(corner, corner, picked), picked);
        assert_eq!(frame_rect_to_virtual(corner, picked), (-1600 + 100, -200 + 50, 400, 300));
    }

    /// The window manager put the overlay at (0, 0) instead of where it was
    /// asked to go. The crop must move by the difference, or it takes pixels
    /// from the other monitor.
    #[test]
    fn a_misplaced_overlay_moves_the_crop() {
        let capture_at = LAPTOP_CORNER;
        let window_at = (0, 0);
        let picked = (100, 50, 400, 300);
        assert_eq!(bitmap_offset(capture_at, window_at), (1600, 200));
        assert_eq!(frame_rect_to_bitmap(capture_at, window_at, picked), (1600 + 100, 200 + 50, 400, 300));
        // On the virtual screen it is wherever the window really is.
        assert_eq!(frame_rect_to_virtual(window_at, picked), picked);
    }

    const LAPTOP_CORNER: (i32, i32) = (LAPTOP.x, LAPTOP.y);

    #[test]
    fn a_correctly_placed_overlay_has_no_offset() {
        for corner in [LAPTOP_CORNER, (0, 0), (5, 5)] {
            assert_eq!(bitmap_offset(corner, corner), (0, 0), "{corner:?}");
            assert_eq!(frame_to_bitmap(corner, corner, (7, 9)), (7, 9));
        }
    }

    #[test]
    fn intersection_trims_and_refuses_empty_results() {
        let unit = (0, 0, 100, 100);
        let cases = [
            ((50, 50, 100, 100), Some((50, 50, 50, 50))),
            ((100, 0, 10, 10), None),        // touching the right edge only
            ((-40, -40, 80, 80), Some((0, 0, 40, 40))),
            ((20, 30, 10, 10), Some((20, 30, 10, 10))),
            ((0, 0, 100, 100), Some(unit)),
        ];
        for (other, want) in cases {
            assert_eq!(intersect_rect(unit, other), want, "{other:?}");
            assert_eq!(intersect_rect(other, unit), want, "order must not matter");
        }
        assert_eq!(intersect_rect((0, 0, 0, 100), (0, 0, 10, 10)), None, "empty input");
    }

    /// Chrome for a selection on the left monitor has to stay on that
    /// monitor, not spread over the 3520-pixel-wide desktop and end up on
    /// the next screen.
    #[test]
    fn chrome_stays_on_the_monitor_under_the_point() {
        let g = laptop_left_of_primary();
        let area_at = |p| monitor_bounds_in_frame(g.monitors(), g.origin(), g.size(), p);
        // On the laptop, which fills the window's top-left.
        assert_eq!(area_at((10, 10)), (0, 0, 1600, 900));
        // On the primary: its corner in the window is the desktop origin
        // negated, (1600, 200).
        let primary_in_window = (1600, 200, 1920, 1080);
        assert_eq!(area_at((1700, 300)), primary_in_window);
        // Exactly on the seam between them belongs to the primary.
        assert_eq!(area_at((1600, 300)), primary_in_window);
    }

    #[test]
    fn chrome_uses_the_whole_window_off_every_monitor() {
        let g = laptop_left_of_primary();
        let whole_window = (0, 0, g.size().0 as i32, g.size().1 as i32);
        // Inside the desktop box but under no monitor.
        assert_eq!(monitor_bounds_in_frame(g.monitors(), g.origin(), g.size(), (10, 1200)), whole_window);
        // A video frame has no monitors at all.
        assert_eq!(monitor_bounds_in_frame(&[], (0, 0), (1280, 720), (5, 5)), (0, 0, 1280, 720));
    }

    /// A window smaller than its monitor only offers the part it covers.
    #[test]
    fn chrome_is_trimmed_to_the_window() {
        assert_eq!(monitor_bounds_in_frame(&[PRIMARY], (0, 0), (1280, 720), (5, 5)), (0, 0, 1280, 720));
        // A window starting 1000 px across the monitor only has the last
        // 920 px of it to offer.
        assert_eq!(monitor_bounds_in_frame(&[PRIMARY], (1000, 0), (1280, 720), (5, 5)), (0, 0, 920, 720));
    }
}
