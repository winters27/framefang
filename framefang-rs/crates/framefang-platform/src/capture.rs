//! Taking a picture of every screen at once.
//!
//! The per-OS work of reading pixels belongs to `xcap` (Windows Graphics
//! Capture, XCB or a Wayland protocol, CoreGraphics). What this module adds
//! is the part every screenshot tool gets wrong at least once: putting the
//! monitors side by side in one bitmap without losing pixels on a high-DPI
//! screen.
//!
//! # Points and pixels
//!
//! The rectangle a monitor reports and the bitmap it captures are not always
//! in the same unit. macOS reports points and captures device pixels (twice
//! as many on Retina), X11 shrinks its rectangles by `Xft.dpi/96`, and
//! Windows uses device pixels for both. So the scale is measured, monitor by
//! monitor, from the two sizes we actually hold, and `framefang_core::dpi`'s
//! [`DisplayMap`] lays the monitors out at the sharpest scale among them. The
//! overlay, the final crop, pins and recording regions all convert through
//! that one map, and at 1x it changes nothing.

use crate::{Error, Result};
use framefang_core::dpi::{effective_scale, sample_index, DisplayMap, MonitorGeometry, PhysicalRect};
use framefang_core::region::DesktopBounds;
use framefang_core::virtual_desktop::{DesktopGeometry, MonitorRect};
use image::{ImageBuffer, Rgba, RgbaImage};

/// One capture of the whole desktop.
#[derive(Debug, Clone)]
pub struct Captured {
    /// Every monitor drawn into one RGBA8 bitmap, in device pixels, covering
    /// the bounding box of the layout.
    pub bitmap: ImageBuffer<Rgba<u8>, Vec<u8>>,
    /// The bitmap's top-left corner on the virtual screen, in the logical
    /// units the OS reports. `map.physical_origin()` gives the same corner in
    /// device pixels.
    pub virtual_origin: (i32, i32),
    /// How screen coordinates and bitmap pixels relate. Any conversion
    /// between the two goes through here.
    pub map: DisplayMap,
    /// The monitors the bitmap was built from, in the order the OS listed
    /// them, for callers that need to know where one screen ends.
    pub monitors: Vec<MonitorFrame>,
}

/// One monitor's part of a [`Captured`].
#[derive(Debug, Clone)]
pub struct MonitorFrame {
    /// Position and size as the OS reports them (logical units).
    pub x: i32,
    pub y: i32,
    pub width:  u32,
    pub height: u32,
    pub name:   String,
    /// The OS's own scale factor. Kept for information only: some systems
    /// report one that doesn't match the pixels (see `effective_scale`).
    pub scale_factor: f32,
    /// Device pixels per logical unit, worked out from the bitmap this
    /// monitor really returned.
    pub effective_scale: f32,
    /// This monitor's slot inside `Captured::bitmap`.
    pub physical: PhysicalRect,
}

impl Captured {
    /// The desktop rectangle this capture shows, for clamping a recording
    /// region.
    ///
    /// It is measured in device pixels, the same as [`Captured::geometry`]:
    /// the corner is the map's physical origin and the size is the bitmap's,
    /// so a selection dragged over the overlay is clamped against exactly the
    /// pixels it was dragged over, even if the monitors change before the
    /// recording starts. The scale goes along for backends that address the
    /// screen in points (macOS `screencapture`).
    pub fn desktop_bounds(&self) -> DesktopBounds {
        let (x, y) = self.map.physical_origin();
        DesktopBounds::new(x, y, self.bitmap.width(), self.bitmap.height()).with_scale(self.map.scale())
    }

    /// Where the bitmap sits on the virtual desktop and which monitor slots
    /// it is made of, all in device pixels: the unit the bitmap, the overlay
    /// window and winit's pointer share.
    ///
    /// The overlay places itself with this and maps selections back through
    /// it, which is what lets a region on a monitor left of or above the
    /// primary one crop the pixels the user saw. If the slots don't add up to
    /// the bitmap's size, the bitmap alone is the answer.
    pub fn geometry(&self) -> DesktopGeometry {
        let (w, h) = (self.bitmap.width(), self.bitmap.height());
        let (x, y) = self.map.physical_origin();
        let slots = self.monitors.iter()
            .map(|m| MonitorRect::new(x + m.physical.x, y + m.physical.y, m.physical.w, m.physical.h))
            .collect();
        match DesktopGeometry::from_monitors(slots) {
            Some(g) if g.size() == (w, h) => g,
            _ => DesktopGeometry::bitmap(w, h),
        }
    }
}

/// Capture every monitor into one bitmap.
///
/// Bitmap pixel `(px, py)` is the screen point `map.physical_to_logical((px,
/// py))`, and `map.logical_to_physical` goes the other way; at 1x that is
/// just `virtual_origin` plus the pixel.
///
/// Under Wayland `xcap` still reads the pixels (GNOME Shell's screenshot
/// call, the screenshot portal or wlroots screencopy, whichever answers),
/// but it can only list monitors over XCB/RandR. When that listing fails,
/// [`fallback_capture`] gets a second try.
pub fn capture_all_screens() -> Result<Captured> {
    capture_with_xcap().or_else(fallback_capture)
}

/// The pixels of one rectangle `(x, y, w, h)` in desktop pixels, from the
/// monitor under its centre and cut to that monitor's edges. Fast enough on
/// Windows (a few milliseconds) to take one between every scroll step.
pub fn capture_area(x: i32, y: i32, w: u32, h: u32) -> Result<image::RgbaImage> {
    let failed = |e: xcap::XCapError| Error::Capture(format!("reading part of the screen: {e}"));
    let screen = xcap::Monitor::from_point(x + w as i32 / 2, y + h as i32 / 2).map_err(failed)?;
    let left = screen.x().map_err(failed)?;
    let top = screen.y().map_err(failed)?;
    let screen_w = screen.width().map_err(failed)?;
    let screen_h = screen.height().map_err(failed)?;
    // Relative to the monitor, never left of or above it, never past it.
    let dx = (x - left).max(0) as u32;
    let dy = (y - top).max(0) as u32;
    let cut_w = w.min(screen_w.saturating_sub(dx));
    let cut_h = h.min(screen_h.saturating_sub(dy));
    screen.capture_region(dx, dy, cut_w, cut_h).map_err(failed)
}

/// What to do when `xcap` can't produce a capture.
///
/// Only a Wayland session has a second way in: with no XWayland the monitor
/// listing fails outright, but the screenshot portal can still hand over the
/// screen. On X11 the first error stands, since nothing else would do better.
#[cfg(target_os = "linux")]
fn fallback_capture(first: Error) -> Result<Captured> {
    match crate::session::is_wayland() {
        true => crate::capture_portal::capture_via_portal().map_err(|second| Error::Capture(format!(
            "this Wayland session wouldn't give up the screen. Reading it directly failed \
             ({first}), and so did the desktop's screenshot portal ({second})"
        ))),
        false => Err(first),
    }
}

/// Elsewhere there is no other way in, so the first error is the answer.
#[cfg(not(target_os = "linux"))]
fn fallback_capture(first: Error) -> Result<Captured> {
    Err(first)
}

/// One monitor as `xcap` gave it to us: its frame (slot not placed yet), its
/// layout entry and its pixels.
struct Shot {
    frame: MonitorFrame,
    layout: MonitorGeometry,
    pixels: RgbaImage,
}

fn shoot(monitor: &xcap::Monitor) -> Result<Shot> {
    let fail = |what: &str, e: xcap::XCapError| Error::Capture(format!("reading the monitor's {what}: {e}"));
    let x = monitor.x().map_err(|e| fail("position", e))?;
    let y = monitor.y().map_err(|e| fail("position", e))?;
    let width = monitor.width().map_err(|e| fail("size", e))?;
    let height = monitor.height().map_err(|e| fail("size", e))?;
    let name = monitor.name().map_err(|e| fail("name", e))?;
    let scale_factor = monitor.scale_factor().map_err(|e| fail("scale", e))?;
    let pixels = monitor.capture_image()
        .map_err(|e| Error::Capture(format!("reading the pixels of {name}: {e}")))?;

    // Only now, holding both sizes, can the real scale be known.
    let measured = effective_scale((width, height), pixels.dimensions(), scale_factor);
    Ok(Shot {
        layout: MonitorGeometry::new(x, y, width, height, measured),
        frame: MonitorFrame {
            x, y, width, height, name, scale_factor,
            effective_scale: measured,
            physical: PhysicalRect::ZERO,
        },
        pixels,
    })
}

fn capture_with_xcap() -> Result<Captured> {
    let screens = xcap::Monitor::all().map_err(|e| Error::Capture(format!("listing the monitors: {e}")))?;
    if screens.is_empty() {
        return Err(Error::Capture("the system reports no monitors".into()));
    }

    let mut frames = Vec::with_capacity(screens.len());
    let mut panels = Vec::with_capacity(screens.len());
    for monitor in &screens {
        let shot = shoot(monitor)?;
        frames.push(shot.frame);
        panels.push((shot.layout, shot.pixels));
    }

    let (bitmap, map) = stitch(&panels)?;
    for (frame, placed) in frames.iter_mut().zip(map.monitors()) {
        frame.physical = placed.physical;
    }
    Ok(Captured { bitmap, virtual_origin: map.origin(), map, monitors: frames })
}

/// Lay each monitor's pixels into one desktop-sized bitmap.
///
/// Kept apart from the capture itself because all of the high-DPI risk is in
/// this arithmetic, and here it can be tested with no screen attached.
fn stitch(panels: &[(MonitorGeometry, RgbaImage)]) -> Result<(RgbaImage, DisplayMap)> {
    let layout: Vec<MonitorGeometry> = panels.iter().map(|(g, _)| *g).collect();
    let map = DisplayMap::from_monitors(&layout);
    let (w, h) = map.physical_size();
    if w == 0 || h == 0 {
        return Err(Error::Capture("the monitor layout covers no pixels".into()));
    }

    // Opaque black wherever no monitor reaches (beside a shorter screen).
    let mut canvas = ImageBuffer::from_pixel(w, h, Rgba([0, 0, 0, 255]));
    for ((_, pixels), placed) in panels.iter().zip(map.monitors()) {
        blit(&mut canvas, w, h, placed.physical, pixels);
    }
    Ok((canvas, map))
}

/// Copy one monitor's pixels into its slot on the canvas.
///
/// When the source and the slot are the same size (every monitor at one
/// scale, the usual case) each row is a single slice copy. Sizes within two
/// pixels of each other count as the same: that is rounding left by a
/// fractional scale, and not worth resampling a whole screen for. Otherwise
/// the source is stretched with nearest-neighbour sampling, because blurring
/// the hard edges of text and UI looks worse than the blockiness.
fn blit(canvas: &mut RgbaImage, canvas_w: u32, canvas_h: u32, slot: PhysicalRect, src: &RgbaImage) {
    let (sw, sh) = src.dimensions();
    if slot.is_empty() || sw == 0 || sh == 0 {
        return;
    }
    // The layout always fits the canvas, but clipping here turns any mistake
    // into a wrong pixel instead of an out-of-bounds panic.
    let (left, top) = (slot.x.max(0), slot.y.max(0));
    let right = slot.right().min(canvas_w as i32);
    let bottom = slot.bottom().min(canvas_h as i32);
    if right <= left || bottom <= top {
        return;
    }

    let close = |a: u32, b: u32| a.abs_diff(b) <= 2;
    let same_size = close(sw, slot.w) && close(sh, slot.h);
    let src_row_len = sw as usize * 4;
    let dst_row_len = canvas_w as usize * 4;
    let skip = (left - slot.x) as usize;
    let src_px = src.as_raw();
    let dst_px: &mut [u8] = canvas;

    // Each canvas row paired with the source row it shows.
    let pick = |dy: u32| if same_size { dy } else { sample_index(dy, slot.h, sh) };
    let rows = (top..bottom).map(|y| (y, pick((y - slot.y) as u32))).filter(|&(_, sy)| sy < sh);

    for (y, sy) in rows {
        let from = &src_px[sy as usize * src_row_len..][..src_row_len];
        let into = &mut dst_px[y as usize * dst_row_len..][..dst_row_len];
        if same_size {
            let n = ((right - left) as usize).min((sw as usize).saturating_sub(skip));
            let at = left as usize * 4;
            into[at..at + n * 4].copy_from_slice(&from[skip * 4..(skip + n) * 4]);
        } else {
            for x in left..right {
                let sx = sample_index((x - slot.x) as u32, slot.w, sw) as usize;
                if sx < sw as usize {
                    into[x as usize * 4..][..4].copy_from_slice(&from[sx * 4..][..4]);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use framefang_core::dpi::LogicalRect;

    const BLACK: Rgba<u8> = Rgba([0, 0, 0, 255]);
    const TOP_LEFT: Rgba<u8> = Rgba([1, 1, 1, 255]);
    const BOTTOM_RIGHT: Rgba<u8> = Rgba([2, 2, 2, 255]);

    fn flat(w: u32, h: u32, rgba: [u8; 4]) -> RgbaImage {
        ImageBuffer::from_pixel(w, h, Rgba(rgba))
    }

    /// A fake monitor: `(x, y, lw, lh)` logical at `scale`, giving a `pw` x
    /// `ph` bitmap of `fill` whose two corners are marked, so a shifted or
    /// mirrored copy shows.
    fn screen(at: (i32, i32, u32, u32), scale: f32, (pw, ph): (u32, u32), fill: [u8; 4])
        -> (MonitorGeometry, RgbaImage)
    {
        let mut img = flat(pw, ph, fill);
        img.put_pixel(0, 0, TOP_LEFT);
        img.put_pixel(pw - 1, ph - 1, BOTTOM_RIGHT);
        (MonitorGeometry::new(at.0, at.1, at.2, at.3, scale), img)
    }

    /// Build a `Captured` from fake monitors the way the real capture does.
    fn captured(panels: &[(MonitorGeometry, RgbaImage)]) -> Captured {
        let (bitmap, map) = stitch(panels).unwrap();
        let monitors = map.monitors().iter().zip(panels).map(|(p, (g, img))| MonitorFrame {
            x: g.x, y: g.y, width: g.width, height: g.height, name: "fake".into(),
            scale_factor: 1.0, effective_scale: img.width() as f32 / g.width as f32,
            physical: p.physical,
        }).collect();
        Captured { bitmap, virtual_origin: map.origin(), map, monitors }
    }

    #[test]
    fn two_monitors_at_1x_sit_side_by_side() {
        let (canvas, map) = stitch(&[
            screen((0, 0, 40, 30), 1.0, (40, 30), [10, 0, 0, 255]),
            screen((40, 0, 20, 30), 1.0, (20, 30), [0, 10, 0, 255]),
        ]).unwrap();
        assert_eq!(1.0, map.scale(), "nothing to scale");
        assert_eq!(canvas.dimensions(), (60, 30));
        for (x, y, want) in [(0, 0, TOP_LEFT), (39, 29, BOTTOM_RIGHT), (40, 0, TOP_LEFT), (59, 29, BOTTOM_RIGHT)] {
            assert_eq!(*canvas.get_pixel(x, y), want, "at ({x}, {y})");
        }
        assert_eq!(*canvas.get_pixel(5, 5), Rgba([10, 0, 0, 255]));
        assert_eq!(*canvas.get_pixel(45, 5), Rgba([0, 10, 0, 255]));
    }

    #[test]
    fn a_retina_screen_keeps_all_of_its_pixels() {
        // Points reported, device pixels captured: the canvas has to be the
        // device size, not the point size.
        let (canvas, map) = stitch(&[screen((0, 0, 40, 30), 2.0, (80, 60), [7, 7, 7, 255])]).unwrap();
        assert_eq!(2.0, map.scale(), "stitched at the sharpest scale");
        assert_eq!(canvas.dimensions(), (80, 60));
        assert_eq!(*canvas.get_pixel(0, 0), TOP_LEFT);
        assert_eq!(*canvas.get_pixel(79, 59), BOTTOM_RIGHT);
        // A logical selection lands on the doubled pixels.
        assert_eq!(map.logical_rect_to_physical(LogicalRect::new(10.0, 10.0, 20.0, 10.0)),
                   PhysicalRect::new(20, 20, 40, 20));
    }

    #[test]
    fn a_one_and_a_half_scale_screen_fills_the_canvas() {
        let (canvas, _) = stitch(&[screen((0, 0, 40, 30), 1.5, (60, 45), [3, 3, 3, 255])]).unwrap();
        assert_eq!(canvas.dimensions(), (60, 45));
        assert_eq!(*canvas.get_pixel(59, 44), BOTTOM_RIGHT);
        assert!(canvas.pixels().all(|p| *p != BLACK), "no gap left at the background colour");
    }

    #[test]
    fn mixed_scales_stretch_only_the_lower_dpi_screen() {
        // A 2x laptop with a 1x external monitor to its right.
        let (canvas, map) = stitch(&[
            screen((0, 0, 40, 30), 2.0, (80, 60), [5, 0, 0, 255]),
            screen((40, 0, 40, 20), 1.0, (40, 20), [0, 5, 0, 255]),
        ]).unwrap();
        assert_eq!(2.0, map.scale(), "stitched at the sharpest scale");
        assert_eq!(canvas.dimensions(), (160, 60));
        // The laptop is copied exactly.
        assert_eq!(*canvas.get_pixel(0, 0), TOP_LEFT);
        assert_eq!(*canvas.get_pixel(79, 59), BOTTOM_RIGHT);
        // The external screen is doubled into an 80x40 slot.
        assert_eq!(*canvas.get_pixel(80, 0), TOP_LEFT);
        assert_eq!(*canvas.get_pixel(81, 1), TOP_LEFT);
        assert_eq!(*canvas.get_pixel(159, 39), BOTTOM_RIGHT);
        assert_eq!(*canvas.get_pixel(100, 10), Rgba([0, 5, 0, 255]));
        // Under the shorter screen there is nothing but background.
        assert_eq!(*canvas.get_pixel(100, 50), BLACK);
    }

    #[test]
    fn no_monitors_is_an_error() {
        assert!(stitch(&[]).is_err());
    }

    #[test]
    fn equal_sizes_copy_without_resampling() {
        let mut canvas = flat(10, 5, [0, 0, 0, 255]);
        let mut src = flat(5, 5, [40, 50, 60, 255]);
        src.put_pixel(0, 0, Rgba([11, 12, 13, 255]));
        blit(&mut canvas, 10, 5, PhysicalRect::new(5, 0, 5, 5), &src);
        assert_eq!(*canvas.get_pixel(5, 0), Rgba([11, 12, 13, 255]));
        assert_eq!(*canvas.get_pixel(6, 0), Rgba([40, 50, 60, 255]));
        assert_eq!(*canvas.get_pixel(9, 4), Rgba([40, 50, 60, 255]));
        assert_eq!(*canvas.get_pixel(4, 4), BLACK, "the left half is untouched");
    }

    #[test]
    fn a_small_source_is_stretched_nearest_neighbour() {
        const RED: Rgba<u8> = Rgba([200, 0, 0, 255]);
        const BLUE: Rgba<u8> = Rgba([0, 0, 200, 255]);
        let mut canvas = flat(8, 8, [0, 0, 0, 255]);
        let mut src = flat(2, 2, [0, 0, 0, 255]);
        src.put_pixel(0, 0, RED);
        src.put_pixel(1, 1, BLUE);
        blit(&mut canvas, 8, 8, PhysicalRect::new(0, 0, 8, 8), &src);
        // Each source pixel becomes a 4x4 block.
        for (x, y, want) in [(0, 0, RED), (3, 3, RED), (4, 4, BLUE), (7, 7, BLUE), (4, 0, BLACK)] {
            assert_eq!(*canvas.get_pixel(x, y), want, "at ({x}, {y})");
        }
    }

    #[test]
    fn a_slot_past_the_canvas_edge_is_clipped() {
        let mut canvas = flat(4, 4, [0, 0, 0, 255]);
        blit(&mut canvas, 4, 4, PhysicalRect::new(2, 2, 4, 4), &flat(4, 4, [9, 9, 9, 255]));
        assert_eq!(*canvas.get_pixel(3, 3), Rgba([9, 9, 9, 255]));
        assert_eq!(*canvas.get_pixel(1, 1), BLACK);
    }

    #[test]
    fn a_two_pixel_mismatch_still_copies_straight_across() {
        // A 1.25x screen can round to a slot a pixel or two off the bitmap.
        let mut canvas = flat(10, 10, [0, 0, 0, 255]);
        let mut src = flat(9, 9, [4, 4, 4, 255]);
        src.put_pixel(8, 0, Rgba([8, 8, 8, 255]));
        blit(&mut canvas, 10, 10, PhysicalRect::new(0, 0, 10, 10), &src);
        assert_eq!(*canvas.get_pixel(8, 0), Rgba([8, 8, 8, 255]), "copied, not resampled");
        assert_eq!(*canvas.get_pixel(9, 0), BLACK, "the extra column stays background");
    }

    #[test]
    fn geometry_and_bounds_follow_the_stitched_bitmap() {
        // A 1x monitor to the left of the primary one.
        let shot = captured(&[
            screen((-20, 0, 20, 10), 1.0, (20, 10), [1, 0, 0, 255]),
            screen((0, 0, 30, 10), 1.0, (30, 10), [0, 1, 0, 255]),
        ]);
        let g = shot.geometry();
        assert_eq!(g.origin(), (-20, 0));
        assert_eq!(g.size(), (50, 10));
        assert_eq!(g.monitors().len(), 2);
        let b = shot.desktop_bounds();
        assert_eq!((b.x, b.y, b.width, b.height), (-20, 0, 50, 10));
        assert_eq!(b.scale, 1.0);
    }
}
