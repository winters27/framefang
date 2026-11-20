//! Converting between logical and physical coordinates on high-DPI displays
//! (Retina, fractional scaling, and mixes of both).
//!
//! # Two kinds of coordinate
//!
//! * **Logical** units are what the OS lays the desktop out in and reports
//!   monitor positions in: points on macOS, desktop pixels on Windows and
//!   X11. The platform capture layer reports each monitor's logical
//!   `(x, y, width, height)`.
//! * **Physical** units are real device pixels: what a captured bitmap holds
//!   and what a framebuffer window is measured in. A logical unit is `scale`
//!   physical pixels, and every monitor can have its own `scale`.
//!
//! On a 1x desktop the two are the same and every conversion here returns
//! its input unchanged, so un-scaled setups behave exactly as if this module
//! were not there.
//!
//! # The scale is measured
//!
//! Platforms disagree about what they report. macOS gives monitor bounds in
//! points and a bitmap in device pixels (twice as large on Retina). Windows
//! reports both in device pixels, so the two match even at 150 % DPI. X11
//! divides the rects by `Xft.dpi / 96` but captures at full framebuffer
//! resolution. Dividing the bitmap's size by the reported monitor size gives
//! the factor that really links the two on all of them, with no per-OS code
//! (see [`effective_scale`]). The OS's own factor is only the fallback when
//! the sizes cannot be trusted.
//!
//! # One stitched bitmap
//!
//! [`DisplayMap::from_monitors`] places every monitor in a single bitmap
//! covering the whole virtual desktop. That bitmap uses the *highest* scale
//! of any monitor, so the sharpest screen keeps all its pixels and a lower
//! DPI neighbour is enlarged to fit its slot. The mapping between logical and
//! physical is then one formula for the whole desktop,
//! `physical = (logical - origin) * scale`, which is cheap, reversible, and
//! shared by the overlay selection, the final crop, pin placement and
//! recording regions.

/// A rectangle in logical desktop units.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LogicalRect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl LogicalRect {
    pub fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        LogicalRect { x, y, w, h }
    }

    pub fn right(&self) -> f32 {
        self.w + self.x
    }

    pub fn bottom(&self) -> f32 {
        self.h + self.y
    }

    /// True when the rectangle has no area.
    pub fn is_empty(&self) -> bool {
        self.h <= 0.0 || self.w <= 0.0
    }

    /// True when `p` is inside. The right and bottom edges are left out, so
    /// on a seam between two monitors exactly one of them has the point.
    pub fn contains(&self, p: (f32, f32)) -> bool {
        (self.x..self.right()).contains(&p.0) && (self.y..self.bottom()).contains(&p.1)
    }
}

/// A rectangle in physical device pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PhysicalRect {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

impl PhysicalRect {
    /// The empty rectangle at the origin.
    pub const ZERO: PhysicalRect = PhysicalRect { x: 0, y: 0, w: 0, h: 0 };

    pub fn new(x: i32, y: i32, w: u32, h: u32) -> Self {
        PhysicalRect { x, y, w, h }
    }

    pub fn right(&self) -> i32 {
        self.w as i32 + self.x
    }

    pub fn bottom(&self) -> i32 {
        self.h as i32 + self.y
    }

    /// True when the rectangle has no pixels.
    pub fn is_empty(&self) -> bool {
        self.w.min(self.h) == 0
    }

    /// True when pixel `p` is inside (right and bottom edges excluded).
    pub fn contains(&self, p: (i32, i32)) -> bool {
        (self.x..self.right()).contains(&p.0) && (self.y..self.bottom()).contains(&p.1)
    }
}

/// A monitor as the platform describes it: its logical rectangle and the
/// factor that turns logical units on it into device pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MonitorGeometry {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub scale: f32,
}

impl MonitorGeometry {
    pub fn new(x: i32, y: i32, width: u32, height: u32, scale: f32) -> Self {
        MonitorGeometry { x, y, width, height, scale }
    }

    /// The monitor's rectangle in logical units.
    pub fn logical_rect(&self) -> LogicalRect {
        let (x, y) = (self.x as f32, self.y as f32);
        LogicalRect { x, y, w: self.width as f32, h: self.height as f32 }
    }
}

/// Where one monitor ended up in the stitched bitmap.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MonitorPlacement {
    /// The monitor as the platform reported it, in logical units.
    pub geometry: MonitorGeometry,
    /// The part of the stitched bitmap holding this monitor's pixels.
    pub physical: PhysicalRect,
}

/// The range of scale factors we accept, from the OS or from a measurement.
/// Anything outside it means whatever produced it is broken, and 1x (what an
/// un-scaled desktop uses) is the safe substitute.
const SCALE_FLOOR: f32 = 0.1;
const SCALE_CEILING: f32 = 16.0;

/// How close a scale must be to a multiple of 1/8 to be snapped onto it.
const SNAP_TOLERANCE: f32 = 0.02;

/// Make a scale factor safe to use. Values outside the accepted range, and
/// values that are not numbers, become 1.0. Values within 0.02 of a multiple
/// of 1/8 (1.25, 1.5, 1.75, 2 and so on) become exactly that multiple, which
/// cleans up the rounding noise a measured ratio carries.
pub fn sanitize_scale(scale: f32) -> f32 {
    let believable = scale.is_finite() && (SCALE_FLOOR..=SCALE_CEILING).contains(&scale);
    // At 0.1 or more the nearest eighth is at least 0.125, so it is always
    // in range too.
    let nearest_eighth = (scale * 8.0).round() / 8.0;
    match believable {
        false => 1.0,
        true if (scale - nearest_eighth).abs() <= SNAP_TOLERANCE => nearest_eighth,
        true => scale,
    }
}

/// Work out the factor between a monitor's logical size and the size of the
/// bitmap actually captured from it.
///
/// `reported` (the OS's own factor) is used instead whenever the measurement
/// is unreliable: a zero in either size, or a width ratio and a height ratio
/// more than 2 % apart (a rotated monitor, or a backend that swaps width and
/// height).
pub fn effective_scale(logical: (u32, u32), physical: (u32, u32), reported: f32) -> f32 {
    let os_scale = || sanitize_scale(reported);
    if [logical.0, logical.1, physical.0, physical.1].contains(&0) {
        return os_scale();
    }
    let across = physical.0 as f32 / logical.0 as f32;
    let down = physical.1 as f32 / logical.1 as f32;
    let agree = (across - down).abs() <= 0.02 * across.max(down);
    if agree { sanitize_scale((across + down) * 0.5) } else { os_scale() }
}

/// Which source pixel to show at destination pixel `dst` when a run of
/// `src_len` pixels is stretched or squeezed to `dst_len`.
///
/// Nearest neighbour, deliberately. These are screenshots, all sharp edges
/// and text: smoothing them looks like blur, while visible blocks just look
/// like a lower-resolution monitor, which is the truth. The capture stitcher
/// and the pin window both use this so they scale the same way. Either
/// length being zero gives 0, and the result never passes the last source
/// pixel.
pub fn sample_index(dst: u32, dst_len: u32, src_len: u32) -> u32 {
    if dst_len.min(src_len) == 0 {
        return 0;
    }
    // In u64 so the product cannot overflow. When the lengths match this is
    // just `dst`.
    let last = src_len as u64 - 1;
    let src = dst as u64 * src_len as u64 / dst_len as u64;
    src.min(last) as u32
}

/// A region for a screen recorder, in both forms the platforms use.
///
/// `x11grab` (Linux) and `gdigrab` (Windows) take absolute device pixels;
/// macOS `screencapture -R` takes points. Both come from one [`DisplayMap`],
/// so every backend records the same rectangle for the same selection.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GrabRegion {
    /// Absolute device pixels on the virtual desktop, with even width and
    /// height: H.264's half-resolution color planes cannot encode an odd
    /// side, and grabbers asked for one fail.
    pub device: PhysicalRect,
    /// That rectangle again, measured in logical units (points).
    pub points: LogicalRect,
}

/// The authority on logical and physical coordinates: where the virtual
/// desktop starts, the scale the stitched bitmap uses, and where each monitor
/// sits in that bitmap.
#[derive(Debug, Clone, PartialEq)]
pub struct DisplayMap {
    /// Top-left of the virtual desktop, logical units.
    corner: (i32, i32),
    /// Device pixels per logical unit in the stitched bitmap.
    density: f32,
    /// Stitched bitmap size, device pixels.
    bitmap: (u32, u32),
    placed: Vec<MonitorPlacement>,
}

/// Round the two ends of a span (already in physical units) to whole pixels
/// and return `(start, length)`.
///
/// With `limit`, both ends are first pulled into `0..=limit`. A span that
/// rounds to nothing is widened to one pixel so a tiny selection still crops
/// something; if that pixel would fall past `limit`, the last pixel before
/// it is used instead.
fn pixel_span(start: f32, end: f32, limit: Option<i32>) -> (i32, u32) {
    let (mut lo, mut hi) = (start.round() as i32, end.round() as i32);
    if let Some(max) = limit {
        lo = lo.clamp(0, max);
        hi = hi.clamp(0, max);
    }
    if hi <= lo {
        hi = lo + 1;
        if let Some(max) = limit.filter(|&max| hi > max) {
            lo = (max - 1).max(0);
            hi = max;
        }
    }
    (lo, (hi - lo).max(0) as u32)
}

impl DisplayMap {
    /// A 1x map over a bitmap of `width` by `height` at the origin, with one
    /// monitor filling it. Every conversion through it changes nothing. Used
    /// whenever the real monitor layout is not known.
    pub fn identity(width: u32, height: u32) -> Self {
        let screen = MonitorGeometry { x: 0, y: 0, width, height, scale: 1.0 };
        let whole = PhysicalRect { x: 0, y: 0, w: width, h: height };
        let only = MonitorPlacement { geometry: screen, physical: whole };
        DisplayMap { corner: (0, 0), density: 1.0, bitmap: (width, height), placed: vec![only] }
    }

    /// Place every monitor in one stitched bitmap.
    ///
    /// The bitmap spans the box around all monitors at the highest scale any
    /// of them has (never below 1x). Each monitor's slot is found by rounding
    /// its logical *edges*, not its size, so two monitors that share an edge
    /// in logical space share it exactly in the bitmap too, with no gap and
    /// no overlap at any scale. An empty list gives the identity map of an
    /// empty bitmap.
    pub fn from_monitors(monitors: &[MonitorGeometry]) -> Self {
        let Some(first) = monitors.first() else {
            return Self::default();
        };

        let far = |m: &MonitorGeometry| (m.x.saturating_add(m.width as i32), m.y.saturating_add(m.height as i32));
        let start = (first.x, first.y, far(first).0, far(first).1);
        let (left, top, right, bottom) = monitors.iter().fold(start, |(l, t, r, b), m| {
            let (mr, mb) = far(m);
            (l.min(m.x), t.min(m.y), r.max(mr), b.max(mb))
        });
        // Never below 1x, so a low-DPI monitor cannot shrink the others.
        let density = monitors.iter().map(|m| sanitize_scale(m.scale)).fold(1.0_f32, f32::max);

        let scaled_extent = |lo: i32, hi: i32| ((hi - lo).max(0) as f32 * density).round().max(0.0) as u32;
        let frame = DisplayMap {
            corner: (left, top),
            density,
            bitmap: (scaled_extent(left, right), scaled_extent(top, bottom)),
            placed: Vec::new(),
        };
        let slot = |geometry: MonitorGeometry| frame.logical_rect_to_physical(geometry.logical_rect());
        let placed = monitors.iter().map(|&geometry| MonitorPlacement { geometry, physical: slot(geometry) }).collect();
        DisplayMap { placed, ..frame }
    }

    /// Top-left corner of the virtual desktop, in logical units.
    pub fn origin(&self) -> (i32, i32) {
        self.corner
    }

    /// Device pixels per logical unit across the stitched bitmap.
    pub fn scale(&self) -> f32 {
        self.density
    }

    /// The stitched bitmap's size in device pixels.
    pub fn physical_size(&self) -> (u32, u32) {
        self.bitmap
    }

    /// The virtual desktop's size in logical units.
    pub fn logical_size(&self) -> (f32, f32) {
        let (w, h) = self.bitmap;
        (w as f32 / self.density, h as f32 / self.density)
    }

    pub fn monitors(&self) -> &[MonitorPlacement] {
        self.placed.as_slice()
    }

    /// The stitched bitmap's top-left corner in absolute device pixels, which
    /// is where a window covering the whole capture has to be placed.
    pub fn physical_origin(&self) -> (i32, i32) {
        let to_device = |v: i32| (v as f32 * self.density).round() as i32;
        (to_device(self.corner.0), to_device(self.corner.1))
    }

    /// The desktop corner as floats, for the point conversions.
    fn corner_f32(&self) -> (f32, f32) {
        (self.corner.0 as f32, self.corner.1 as f32)
    }

    /// A logical point to a position in the stitched bitmap.
    pub fn logical_to_physical(&self, p: (f32, f32)) -> (f32, f32) {
        let (cx, cy) = self.corner_f32();
        ((p.0 - cx) * self.density, (p.1 - cy) * self.density)
    }

    /// A position in the stitched bitmap to a logical point.
    pub fn physical_to_logical(&self, p: (f32, f32)) -> (f32, f32) {
        let (cx, cy) = self.corner_f32();
        (p.0 / self.density + cx, p.1 / self.density + cy)
    }

    /// A logical rectangle to bitmap pixels, kept inside the bitmap. The
    /// final crop uses this.
    pub fn logical_rect_to_physical(&self, r: LogicalRect) -> PhysicalRect {
        self.to_pixels(r, true)
    }

    /// A logical rectangle to *absolute* device pixels on the virtual
    /// desktop: not relative to the bitmap and not cut to it. Screen grabbers
    /// want this, since `x11grab` and `gdigrab` offsets and window positions
    /// are all absolute device pixels.
    pub fn logical_rect_to_device(&self, r: LogicalRect) -> PhysicalRect {
        self.physical_rect_to_device(self.to_pixels(r, false))
    }

    /// Bitmap pixels to absolute device pixels.
    pub fn physical_rect_to_device(&self, r: PhysicalRect) -> PhysicalRect {
        let (dx, dy) = self.physical_origin();
        PhysicalRect { x: r.x + dx, y: r.y + dy, ..r }
    }

    /// Bitmap pixels to logical units. Used to line a window (a pin, say) up
    /// with the region it was cut from, and for backends that take logical
    /// units, like macOS `screencapture -R`, which takes points.
    pub fn physical_rect_to_logical(&self, r: PhysicalRect) -> LogicalRect {
        let corner = self.physical_to_logical((r.x as f32, r.y as f32));
        let in_units = |pixels: u32| pixels as f32 / self.density;
        LogicalRect { x: corner.0, y: corner.1, w: in_units(r.w), h: in_units(r.h) }
    }

    /// The first monitor placement that passes `test`.
    fn first_placement(&self, test: impl Fn(&MonitorPlacement) -> bool) -> Option<&MonitorPlacement> {
        self.placed.iter().find(|placement| test(placement))
    }

    /// The monitor under a logical point, if there is one.
    pub fn monitor_at_logical(&self, p: (f32, f32)) -> Option<&MonitorPlacement> {
        self.first_placement(|placement| placement.geometry.logical_rect().contains(p))
    }

    /// The monitor under a bitmap pixel, if there is one.
    pub fn monitor_at_physical(&self, p: (i32, i32)) -> Option<&MonitorPlacement> {
        self.first_placement(|placement| placement.physical.contains(p))
    }

    /// The scale of the monitor under a logical point. Points under no
    /// monitor (the empty corners of an L-shaped layout) get the bitmap's
    /// scale.
    pub fn scale_at_logical(&self, p: (f32, f32)) -> f32 {
        self.own_scale(self.monitor_at_logical(p))
    }

    /// The scale of the monitor under a bitmap pixel, with the same fallback.
    pub fn scale_at_physical(&self, p: (i32, i32)) -> f32 {
        self.own_scale(self.monitor_at_physical(p))
    }

    fn own_scale(&self, monitor: Option<&MonitorPlacement>) -> f32 {
        monitor.map_or(self.density, |m| sanitize_scale(m.geometry.scale))
    }

    /// The recording region for a selection given in bitmap pixels.
    ///
    /// The recorder does not take a region from here today (it records the
    /// whole desktop), but any future caller should come through this so the
    /// recorded rectangle and the screenshot crop cannot disagree. Odd sides
    /// are rounded down to even, and the points rectangle describes that
    /// trimmed size.
    pub fn grab_region(&self, selection: PhysicalRect) -> GrabRegion {
        let even = |v: u32| v & !1;
        let trimmed = PhysicalRect { w: even(selection.w), h: even(selection.h), ..selection };
        let device = self.physical_rect_to_device(trimmed);
        let points = self.physical_rect_to_logical(trimmed);
        GrabRegion { device, points }
    }

    /// Shared body of the logical-to-pixel conversions. `clamp_to_bitmap`
    /// keeps the result inside the stitched bitmap. An empty input gives
    /// [`PhysicalRect::ZERO`].
    fn to_pixels(&self, r: LogicalRect, clamp_to_bitmap: bool) -> PhysicalRect {
        let limit = |n: u32| clamp_to_bitmap.then_some(n as i32);
        let (x0, y0) = self.logical_to_physical((r.x, r.y));
        let (x1, y1) = self.logical_to_physical((r.right(), r.bottom()));
        if r.is_empty() {
            PhysicalRect::ZERO
        } else {
            let (x, w) = pixel_span(x0, x1, limit(self.bitmap.0));
            let (y, h) = pixel_span(y0, y1, limit(self.bitmap.1));
            PhysicalRect { x, y, w, h }
        }
    }
}

impl Default for DisplayMap {
    /// The identity map of an empty bitmap.
    fn default() -> Self {
        Self::identity(0, 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn near(a: f32, b: f32) {
        assert!((a - b).abs() < 1e-3, "{a} is not {b}");
    }

    fn mon(x: i32, y: i32, w: u32, h: u32, scale: f32) -> MonitorGeometry {
        MonitorGeometry::new(x, y, w, h, scale)
    }

    fn stitch(monitors: &[MonitorGeometry]) -> DisplayMap {
        DisplayMap::from_monitors(monitors)
    }

    fn one(x: i32, y: i32, w: u32, h: u32, scale: f32) -> DisplayMap {
        stitch(&[mon(x, y, w, h, scale)])
    }

    /// [`effective_scale`] with the four sizes spelled out.
    fn measured(lw: u32, lh: u32, pw: u32, ph: u32, os: f32) -> f32 {
        effective_scale((lw, lh), (pw, ph), os)
    }

    /// A 1440 x 900 Retina laptop with a 1080p 1x display to its right.
    fn laptop_and_external() -> DisplayMap {
        stitch(&[mon(0, 0, 1440, 900, 2.0), mon(1440, 0, 1920, 1080, 1.0)])
    }

    fn lrect(x: f32, y: f32, w: f32, h: f32) -> LogicalRect {
        LogicalRect::new(x, y, w, h)
    }

    fn prect(x: i32, y: i32, w: u32, h: u32) -> PhysicalRect {
        PhysicalRect::new(x, y, w, h)
    }

    // At 1x nothing moves.

    #[test]
    fn a_1x_monitor_changes_nothing() {
        let map = one(0, 0, 1920, 1080, 1.0);
        assert_eq!((map.scale(), map.physical_size(), map.origin()), (1.0, (1920, 1080), (0, 0)));
        let p = (640.0, 480.0);
        assert_eq!((map.logical_to_physical(p), map.physical_to_logical(p)), (p, p));
        assert_eq!(map.logical_rect_to_physical(lrect(10.0, 20.0, 300.0, 400.0)), prect(10, 20, 300, 400));
        assert_eq!(map.monitors()[0].physical, prect(0, 0, 1920, 1080));
        assert_eq!(map.logical_size(), (1920.0, 1080.0));
    }

    #[test]
    fn the_identity_map_covers_its_bitmap() {
        let plain = DisplayMap::identity(1366, 768);
        assert_eq!((plain.scale(), plain.physical_size()), (1.0, (1366, 768)));
        assert_eq!(plain.scale_at_logical((683.0, 384.0)), 1.0);
        assert_eq!(plain.monitors().len(), 1);
        assert_eq!(plain.logical_rect_to_device(lrect(0.0, 0.0, 120.0, 70.0)), prect(0, 0, 120, 70));
        assert_eq!(DisplayMap::default(), DisplayMap::identity(0, 0));
    }

    // 2x.

    #[test]
    fn retina_doubles_everything() {
        let map = one(0, 0, 1440, 900, 2.0);
        assert_eq!((map.scale(), map.physical_size()), (2.0, (2880, 1800)));
        let (pt, px) = ((100.0, 50.0), (200.0, 100.0));
        assert_eq!(map.logical_to_physical(pt), px);
        assert_eq!(map.physical_to_logical(px), pt);
        assert_eq!(map.logical_rect_to_physical(lrect(100.0, 50.0, 400.0, 300.0)), prect(200, 100, 800, 600));
        assert_eq!(map.logical_size(), (1440.0, 900.0));
    }

    #[test]
    fn a_retina_crop_maps_back_to_points() {
        let map = one(0, 0, 1440, 900, 2.0);
        assert_eq!(map.physical_rect_to_logical(prect(200, 100, 800, 600)), lrect(100.0, 50.0, 400.0, 300.0));
    }

    // 1.5x.

    #[test]
    fn fractional_scales_round_edges_not_sizes() {
        let map = one(0, 0, 1920, 1080, 1.5);
        assert_eq!((map.scale(), map.physical_size()), (1.5, (2880, 1620)));
        // Edges 10 and 43 land on 15 and 64.5, which rounds to 65: 50 wide.
        assert_eq!(map.logical_rect_to_physical(lrect(10.0, 10.0, 33.0, 33.0)), prect(15, 15, 50, 50));
        // Move the same 33-unit box by one: edges 16.5 and 66 round to 17
        // and 66, so it is 49 wide. Rounding the size alone (49.5) would
        // have said 50 both times and opened a gap or overlap at seams.
        assert_eq!(map.logical_rect_to_physical(lrect(11.0, 11.0, 33.0, 33.0)), prect(17, 17, 49, 49));
    }

    #[test]
    fn fractional_neighbours_meet_without_a_seam() {
        // Two 1.5x monitors side by side. Any gap between their slots would
        // show up as a black line in the screenshot.
        let map = stitch(&[mon(0, 0, 1707, 960, 1.5), mon(1707, 0, 1707, 960, 1.5)]);
        let (left, right) = (map.monitors()[0].physical, map.monitors()[1].physical);
        assert_eq!(left.x, 0);
        assert_eq!(left.right(), right.x);
        assert_eq!(right.right() as u32, map.physical_size().0);
    }

    // Mixed scales.

    #[test]
    fn mixed_layouts_are_stitched_at_the_sharpest_scale() {
        let map = laptop_and_external();
        assert_eq!((map.scale(), map.physical_size()), (2.0, (6720, 2160)));
        assert_eq!(map.monitors()[0].physical, prect(0, 0, 2880, 1800));
        assert_eq!(map.monitors()[1].physical, prect(2880, 0, 3840, 2160));
        // Each monitor still reports its own real scale, and the empty strip
        // below the shorter laptop (under no monitor) gets the bitmap's.
        for (point, pixel, want) in [((50.0, 60.0), (50, 60), 2.0), ((2000.0, 60.0), (4000, 60), 1.0)] {
            assert_eq!((map.scale_at_logical(point), map.scale_at_physical(pixel)), (want, want));
        }
        let (gap_point, gap_pixel) = ((50.0, 1500.0), (50, 2000));
        assert!(map.monitor_at_logical(gap_point).is_none());
        assert!(map.monitor_at_physical(gap_pixel).is_none());
        assert_eq!((map.scale_at_logical(gap_point), map.scale_at_physical(gap_pixel)), (2.0, 2.0));
    }

    #[test]
    fn the_seam_belongs_to_the_right_hand_monitor() {
        let map = laptop_and_external();
        let at_seam = map.monitor_at_logical((1440.0, 10.0)).unwrap();
        assert_eq!(at_seam.geometry.x, 1440);
        let at_pixel_seam = map.monitor_at_physical((2880, 10)).unwrap();
        assert_eq!(at_pixel_seam.geometry.x, 1440);
    }

    #[test]
    fn a_selection_across_both_monitors_maps_as_one_rectangle() {
        let map = laptop_and_external();
        assert_eq!(map.logical_rect_to_physical(lrect(1400.0, 100.0, 200.0, 100.0)), prect(2800, 200, 400, 200));
    }

    #[test]
    fn a_low_dpi_monitor_never_shrinks_the_bitmap_below_1x() {
        let map = one(0, 0, 1000, 500, 0.5);
        assert_eq!(map.scale(), 1.0);
        assert_eq!(map.physical_size(), (1000, 500));
        // The monitor's own scale is still reported as it is.
        assert_eq!(map.scale_at_logical((10.0, 10.0)), 0.5);
    }

    #[test]
    fn a_monitor_up_and_left_puts_the_origin_negative() {
        let map = stitch(&[mon(0, 0, 1920, 1080, 2.0), mon(-1280, -200, 1280, 1024, 2.0)]);
        assert_eq!(
            (map.origin(), map.physical_size(), map.physical_origin()),
            ((-1280, -200), (6400, 2560), (-2560, -400))
        );
        assert_eq!(map.monitors()[1].physical, prect(0, 0, 2560, 2048));
        assert_eq!(map.monitors()[0].physical, prect(2560, 400, 3840, 2160));
        // The desktop's corner is the bitmap's first pixel.
        assert_eq!(map.logical_to_physical((-1280.0, -200.0)), (0.0, 0.0));
        // Grabbers get absolute device pixels, not bitmap offsets.
        assert_eq!(map.logical_rect_to_device(lrect(-1280.0, -200.0, 100.0, 50.0)), prect(-2560, -400, 200, 100));
        assert_eq!(map.physical_rect_to_device(prect(2560, 400, 10, 10)), prect(0, 0, 10, 10));
    }

    // Clamping and odd input.

    #[test]
    fn crops_stay_inside_the_bitmap() {
        let map = one(0, 0, 800, 600, 2.0);
        assert_eq!(map.logical_rect_to_physical(lrect(-100.0, -100.0, 2000.0, 2000.0)), prect(0, 0, 1600, 1200));
        assert_eq!(map.logical_rect_to_physical(lrect(790.0, 590.0, 100.0, 100.0)), prect(1580, 1180, 20, 20));
    }

    #[test]
    fn device_rects_are_not_clamped() {
        let map = one(0, 0, 800, 600, 2.0);
        assert_eq!(map.logical_rect_to_device(lrect(-100.0, 700.0, 50.0, 50.0)), prect(-200, 1400, 100, 100));
    }

    #[test]
    fn a_selection_smaller_than_a_pixel_still_crops_one() {
        let map = one(0, 0, 800, 600, 1.0);
        assert_eq!(map.logical_rect_to_physical(lrect(10.0, 10.0, 0.2, 0.2)), prect(10, 10, 1, 1));
        // At the far corner the pixel is taken from just inside the bitmap.
        assert_eq!(map.logical_rect_to_physical(lrect(800.0, 600.0, 0.2, 0.2)), prect(799, 599, 1, 1));
        // Unclamped, it simply sits where it is.
        assert_eq!(map.logical_rect_to_device(lrect(800.0, 600.0, 0.2, 0.2)), prect(800, 600, 1, 1));
        // A rectangle with no area maps to nothing.
        assert_eq!(map.logical_rect_to_physical(lrect(10.0, 10.0, 0.0, 50.0)), PhysicalRect::ZERO);
        assert!(map.logical_rect_to_physical(lrect(10.0, 10.0, 50.0, -1.0)).is_empty());
    }

    #[test]
    fn an_empty_bitmap_has_no_pixels_to_crop() {
        let map = stitch(&[]);
        assert_eq!((map.scale(), map.physical_size()), (1.0, (0, 0)));
        assert_eq!(map.logical_rect_to_physical(lrect(0.0, 0.0, 0.5, 0.5)), prect(0, 0, 0, 0));
    }

    #[test]
    fn rectangles_exclude_their_far_edges() {
        let l = lrect(0.0, 0.0, 10.0, 10.0);
        assert!(l.contains((0.0, 0.0)) && l.contains((9.99, 9.99)));
        assert!(!l.contains((10.0, 5.0)) && !l.contains((5.0, 10.0)));
        let p = prect(-5, -5, 10, 10);
        assert_eq!((p.right(), p.bottom()), (5, 5));
        assert!(p.contains((-5, -5)) && p.contains((4, 4)));
        assert!(!p.contains((5, 0)) && !p.contains((0, 5)));
        assert!(PhysicalRect::ZERO.is_empty() && lrect(1.0, 1.0, 0.0, 1.0).is_empty());
        assert_eq!(mon(-3, 4, 5, 6, 2.0).logical_rect(), lrect(-3.0, 4.0, 5.0, 6.0));
    }

    // Measuring the scale.

    #[test]
    fn retina_measures_as_2x() {
        // macOS: rect in points, bitmap in pixels.
        assert_eq!(measured(1440, 900, 2880, 1800, 2.0), 2.0);
        // The OS's figure does not matter when the measurement is good.
        assert_eq!(measured(1440, 900, 2880, 1800, 1.0), 2.0);
    }

    #[test]
    fn matching_sizes_measure_as_1x_whatever_the_os_says() {
        // Windows at 150 % still reports the rect in device pixels; the
        // bitmap matches it and must not be rescaled.
        assert_eq!(measured(2560, 1440, 2560, 1440, 1.5), 1.0);
    }

    #[test]
    fn truncated_x11_rects_snap_to_the_real_fraction() {
        // Xft.dpi 144 reports floor(2560 / 1.5) = 1706 and captures 2560.
        assert_eq!(measured(1706, 960, 2560, 1440, 1.5), 1.5);
    }

    #[test]
    fn disagreeing_axes_fall_back_to_the_os() {
        // Width and height swapped: the ratios differ wildly.
        assert_eq!(measured(1080, 1920, 3840, 2160, 2.0), 2.0);
        // A 1.5 % disagreement is inside the 2 % tolerance and still
        // measured (then snapped to 2).
        assert_eq!(measured(1000, 1000, 2000, 2030, 3.0), 2.0);
    }

    #[test]
    fn missing_sizes_fall_back_to_a_sane_os_scale() {
        // (logical w, logical h, bitmap w, bitmap h, OS scale, expected)
        let cases = [
            (0, 0, 2880, 1800, 2.0, 2.0),
            (1440, 900, 0, 900, 2.0, 2.0),
            (1440, 900, 0, 0, f32::NAN, 1.0),
            (1440, 900, 0, 0, 0.0, 1.0),
            (1440, 900, 0, 0, 1e9, 1.0),
        ];
        for (lw, lh, pw, ph, os, want) in cases {
            assert_eq!(measured(lw, lh, pw, ph, os), want, "{lw}x{lh} vs {pw}x{ph}, OS {os}");
        }
    }

    #[test]
    fn scales_snap_to_eighths_and_reject_junk() {
        near(sanitize_scale(1.4999), 1.5);
        near(sanitize_scale(2.0001), 2.0);
        near(sanitize_scale(1.26), 1.25);
        // Too far from any eighth to snap.
        near(sanitize_scale(1.3333), 1.3333);
        // The accepted range is inclusive at both ends.
        near(sanitize_scale(0.1), 0.1);
        near(sanitize_scale(16.0), 16.0);
        for junk in [-1.0, 0.0, 0.09, 16.5, f32::INFINITY, f32::NEG_INFINITY, f32::NAN] {
            assert_eq!(sanitize_scale(junk), 1.0, "{junk}");
        }
    }

    #[test]
    fn points_and_pixels_convert_back_and_forth_at_any_scale() {
        for scale in [3.0_f32, 2.0, 1.5, 1.0] {
            let map = one(-100, 40, 1000, 800, scale);
            // The corner, a point inside, and the last whole unit.
            for p in [(-100.0_f32, 40.0), (0.0, 100.0), (899.0, 839.0)] {
                let (x, y) = map.physical_to_logical(map.logical_to_physical(p));
                near(x, p.0);
                near(y, p.1);
            }
        }
    }

    // Recording regions.

    #[test]
    fn a_grab_region_has_both_pixels_and_points() {
        let map = one(0, 0, 1440, 900, 2.0);
        let region = map.grab_region(prect(200, 100, 800, 600));
        assert_eq!(region.device, prect(200, 100, 800, 600));
        assert_eq!(region.points, lrect(100.0, 50.0, 400.0, 300.0));
    }

    #[test]
    fn grab_regions_drop_an_odd_column_and_row() {
        let region = DisplayMap::identity(1600, 900).grab_region(prect(11, 13, 101, 67));
        assert_eq!(region.device, prect(11, 13, 100, 66));
        assert_eq!(region.points, lrect(11.0, 13.0, 100.0, 66.0));
    }

    #[test]
    fn grab_regions_are_absolute_on_the_desktop() {
        let map = stitch(&[mon(0, 0, 1920, 1080, 1.0), mon(-1920, 0, 1920, 1080, 1.0)]);
        // Bitmap column 1920 is where the primary monitor starts.
        let region = map.grab_region(prect(1920, 0, 640, 480));
        assert_eq!(region.device, prect(0, 0, 640, 480));
        assert_eq!(region.points, lrect(0.0, 0.0, 640.0, 480.0));
    }

    // Resampling.

    #[test]
    fn equal_lengths_sample_one_to_one() {
        assert!((0..4).all(|i| sample_index(i, 4, 4) == i));
        assert!((0..1000).all(|i| sample_index(i, 1000, 1000) == i));
    }

    #[test]
    fn stretching_repeats_and_squeezing_skips() {
        // Two source pixels over eight: four copies of each.
        let stretched: Vec<u32> = (0..8).map(|i| sample_index(i, 8, 2)).collect();
        assert_eq!(stretched, vec![0, 0, 0, 0, 1, 1, 1, 1]);
        // Eight source pixels into two: every fourth.
        assert_eq!((sample_index(0, 2, 8), sample_index(1, 2, 8)), (0, 4));
        // Uneven ratio: three into two.
        assert_eq!((sample_index(0, 2, 3), sample_index(1, 2, 3)), (0, 1));
    }

    #[test]
    fn sampling_never_leaves_the_source() {
        // (dst, dst_len, src_len, expected)
        let cases = [
            (99, 8, 2, 1),
            (99, 4, 4, 3),
            (0, 0, 4, 0),
            (0, 4, 0, 0),
            (u32::MAX, u32::MAX, u32::MAX, u32::MAX - 1),
        ];
        for (dst, dst_len, src_len, want) in cases {
            assert_eq!(sample_index(dst, dst_len, src_len), want, "{dst} of {dst_len} from {src_len}");
        }
    }
}
