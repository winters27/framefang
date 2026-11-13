//! Cuts the editor window down to what it draws.
//!
//! The editor window is a rectangle around the picture with room for the
//! tool column and the action row. Everything in it that is still the plain
//! canvas color is cut away, so the picture and the panels float over the
//! live desktop and clicks between them go to whatever is underneath.
//!
//! Windows does it with a window region (`SetWindowRgn`), Linux on X11 with
//! the X Shape extension, for both what is drawn and what takes clicks. The
//! dialogs use the same cut for their rounded corners there.
//!
//! Not on Wayland, which has no way for a client to shape its window
//! without an alpha channel, nor on macOS, where the drawing surface
//! (softbuffer's CoreGraphics backend) ignores alpha. There the canvas
//! simply stays visible, with an edge drawn around it.

/// `(x0, y0, x1, y1)`, exclusive end, in window pixels.
pub type Rect = (i32, i32, i32, i32);

/// The parts of `buf` (`w * h`, `0RGB`) that are not `clear`, as rectangles.
/// Everything inside `keep` counts as drawn whatever its color, so the
/// picture never gets holes where it happens to match the canvas.
///
/// Each row is split into runs of drawn pixels, and a run of rows with the
/// same runs becomes one rectangle per run, so a panel is one rectangle plus
/// a few rows at each rounded corner rather than one per row.
pub fn drawn_rects(buf: &[u32], w: usize, h: usize, keep: Option<Rect>, clear: u32) -> Vec<Rect> {
    let mut out: Vec<Rect> = Vec::new();
    // Rectangles still growing downward, one per run of the previous row.
    let mut open: Vec<Rect> = Vec::new();
    let mut runs: Vec<(i32, i32)> = Vec::new();
    for y in 0..h {
        runs.clear();
        let row = &buf[y * w..(y + 1) * w];
        let (kx0, kx1) = match keep {
            Some((x0, y0, x1, y1)) if (y as i32) >= y0 && (y as i32) < y1 => (x0.max(0) as usize, (x1.max(0) as usize).min(w)),
            _ => (0, 0),
        };
        let mut x = 0;
        while x < w {
            let drawn = |i: usize| (i >= kx0 && i < kx1) || row[i] != clear;
            if !drawn(x) { x += 1; continue; }
            let start = x;
            while x < w && drawn(x) { x += 1; }
            runs.push((start as i32, x as i32));
        }
        let same = open.len() == runs.len()
            && open.iter().zip(&runs).all(|(r, s)| r.0 == s.0 && r.2 == s.1);
        if same {
            for r in &mut open { r.3 = y as i32 + 1; }
        } else {
            out.append(&mut open);
            open.extend(runs.iter().map(|&(a, b)| (a, y as i32, b, y as i32 + 1)));
        }
    }
    out.append(&mut open);
    out
}

/// Applies `rects` as the window's shape. An empty list leaves the window
/// whole rather than invisible.
#[cfg(windows)]
pub fn apply(window: &winit::window::Window, rects: &[Rect]) {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::Graphics::Gdi::{CombineRgn, CreateRectRgn, DeleteObject, SetWindowRgn, HRGN, RGN_OR};
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};

    let Ok(handle) = window.window_handle() else { return };
    let RawWindowHandle::Win32(h) = handle.as_raw() else { return };
    let hwnd = HWND(h.hwnd.get() as *mut core::ffi::c_void);
    unsafe {
        if rects.is_empty() {
            SetWindowRgn(hwnd, None, true);
            return;
        }
        let region: HRGN = CreateRectRgn(0, 0, 0, 0);
        for &(x0, y0, x1, y1) in rects {
            let part = CreateRectRgn(x0, y0, x1, y1);
            CombineRgn(Some(region), Some(region), Some(part), RGN_OR);
            let _ = DeleteObject(part.into());
        }
        // The system owns the region from here; it must not be deleted.
        SetWindowRgn(hwnd, Some(region), true);
    }
}

/// X11: the Shape extension, bounding (what shows) and input (what takes
/// clicks) alike. One connection for the process; the server applies a shape
/// to any client's window.
#[cfg(target_os = "linux")]
pub fn apply(window: &winit::window::Window, rects: &[Rect]) {
    use std::sync::OnceLock;
    use x11rb::connection::Connection;
    use x11rb::protocol::shape::{ConnectionExt as _, SK, SO};
    use x11rb::protocol::xproto::{ClipOrdering, Rectangle};
    use x11rb::rust_connection::RustConnection;
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};

    static CONN: OnceLock<Option<RustConnection>> = OnceLock::new();
    let Some(conn) = CONN.get_or_init(|| x11rb::connect(None).ok().map(|(c, _)| c)) else { return };
    let Ok(handle) = window.window_handle() else { return };
    let xid = match handle.as_raw() {
        RawWindowHandle::Xlib(h) => h.window as u32,
        RawWindowHandle::Xcb(h) => h.window.get(),
        _ => return,
    };
    for kind in [SK::BOUNDING, SK::INPUT] {
        let sent = if rects.is_empty() {
            // No mask: the window is whole again.
            conn.shape_mask(SO::SET, kind, xid, 0, 0, x11rb::NONE).map(|_| ())
        } else {
            let list: Vec<Rectangle> = rects.iter().map(|&(x0, y0, x1, y1)| Rectangle {
                x: x0.clamp(i16::MIN as i32, i16::MAX as i32) as i16,
                y: y0.clamp(i16::MIN as i32, i16::MAX as i32) as i16,
                width: (x1 - x0).clamp(0, u16::MAX as i32) as u16,
                height: (y1 - y0).clamp(0, u16::MAX as i32) as u16,
            }).collect();
            conn.shape_rectangles(SO::SET, kind, ClipOrdering::UNSORTED, xid, 0, 0, &list).map(|_| ())
        };
        if sent.is_err() { return; }
    }
    let _ = conn.flush();
}

#[cfg(not(any(windows, target_os = "linux")))]
pub fn apply(_window: &winit::window::Window, _rects: &[Rect]) {}

/// Whether windows can be cut to shape here: Windows, and Linux on X11.
pub fn supported() -> bool {
    cfg!(windows) || (cfg!(target_os = "linux") && !framefang_platform::session::is_wayland())
}

/// Whether pixel `(x, y)` is inside a `w` x `h` rectangle with corners of
/// radius `r` (pixel centers tested, so the edge is symmetric).
pub fn inside_rounded(x: i32, y: i32, w: i32, h: i32, r: i32) -> bool {
    if x < 0 || y < 0 || x >= w || y >= h { return false; }
    let r = r.min(w / 2).min(h / 2).max(0);
    if r == 0 { return true; }
    let (cx, cy) = (x as f32 + 0.5, y as f32 + 0.5);
    let ox = if cx < r as f32 { r as f32 } else if cx > (w - r) as f32 { (w - r) as f32 } else { return true };
    let oy = if cy < r as f32 { r as f32 } else if cy > (h - r) as f32 { (h - r) as f32 } else { return true };
    let (dx, dy) = (cx - ox, cy - oy);
    dx * dx + dy * dy <= (r * r) as f32
}

/// A `w` x `h` rounded rectangle as window-shape rectangles: one per corner
/// row, one for the straight middle. Used by the Linux dialogs.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn rounded(w: i32, h: i32, r: i32) -> Vec<Rect> {
    let mut out: Vec<Rect> = Vec::new();
    for y in 0..h {
        let x0 = (0..w).find(|&x| inside_rounded(x, y, w, h, r)).unwrap_or(w);
        let x1 = (0..w).rev().find(|&x| inside_rounded(x, y, w, h, r)).map_or(0, |x| x + 1);
        if x0 >= x1 { continue; }
        match out.last_mut() {
            Some(last) if last.0 == x0 && last.2 == x1 && last.3 == y => last.3 = y + 1,
            _ => out.push((x0, y, x1, y + 1)),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const C: u32 = 0x1C1C1F;

    fn canvas(w: usize, h: usize) -> Vec<u32> { vec![C; w * h] }

    fn fill(buf: &mut [u32], w: usize, (x0, y0, x1, y1): Rect, v: u32) {
        for y in y0..y1 { for x in x0..x1 { buf[y as usize * w + x as usize] = v; } }
    }

    #[test]
    fn a_rounded_rectangle_cuts_only_its_corners() {
        let (w, h, r) = (60, 40, 8);
        let rects = rounded(w, h, r);
        let covered = |x: i32, y: i32| rects.iter().any(|&(x0, y0, x1, y1)| x >= x0 && x < x1 && y >= y0 && y < y1);
        assert!(!covered(0, 0) && !covered(w - 1, 0) && !covered(0, h - 1) && !covered(w - 1, h - 1));
        assert!(covered(w / 2, 0) && covered(0, h / 2) && covered(w - 1, h / 2) && covered(w / 2, h - 1));
        for y in 0..h { for x in 0..w { assert_eq!(covered(x, y), inside_rounded(x, y, w, h, r), "({x}, {y})"); } }
        // The straight middle is a single rectangle.
        assert!(rects.iter().any(|&(x0, _, x1, _)| x0 == 0 && x1 == w));
        assert!(rects.len() < 2 * r as usize + 2);
        // No radius: the whole window.
        assert_eq!(rounded(10, 5, 0), vec![(0, 0, 10, 5)]);
    }

    #[test]
    fn a_panel_becomes_one_rectangle() {
        let mut b = canvas(40, 30);
        fill(&mut b, 40, (25, 5, 35, 25), 0x222224);
        assert_eq!(drawn_rects(&b, 40, 30, None, C), vec![(25, 5, 35, 25)]);
    }

    #[test]
    fn the_picture_is_kept_whole_even_where_it_matches_the_canvas() {
        let mut b = canvas(40, 30);
        fill(&mut b, 40, (2, 2, 20, 20), 0xFFFFFF);
        fill(&mut b, 40, (5, 5, 8, 8), C); // a canvas-colored patch in the picture
        assert_eq!(drawn_rects(&b, 40, 30, Some((2, 2, 20, 20)), C), vec![(2, 2, 20, 20)]);
    }

    #[test]
    fn side_by_side_parts_and_corners_stay_apart() {
        let mut b = canvas(40, 30);
        fill(&mut b, 40, (2, 2, 20, 20), 0xFFFFFF);
        fill(&mut b, 40, (25, 4, 35, 10), 0x222224);
        // A rounded corner: the first row of the panel is narrower.
        fill(&mut b, 40, (26, 3, 34, 4), 0x222224);
        let r = drawn_rects(&b, 40, 30, Some((2, 2, 20, 20)), C);
        let area: i32 = r.iter().map(|(x0, y0, x1, y1)| (x1 - x0) * (y1 - y0)).sum();
        assert_eq!(area, 18 * 18 + 10 * 6 + 8);
        // Every drawn pixel is covered and no canvas pixel is.
        for y in 0..30 {
            for x in 0..40 {
                let inside = r.iter().any(|&(x0, y0, x1, y1)| x >= x0 && x < x1 && y >= y0 && y < y1);
                let in_pic = (2..20).contains(&x) && (2..20).contains(&y);
                assert_eq!(inside, in_pic || b[y as usize * 40 + x as usize] != C, "({x}, {y})");
            }
        }
    }
}
