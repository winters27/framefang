//! What shows while a scrolling capture runs (look S1, Brandon 2026-11-19):
//! the area in the terminal look of TEXT and QR, a dashed accent frame with
//! solid corners, a `> SCROLL  4,210 px` tag counting up above it with a
//! blinking cursor, and under it how to finish (or, after a scroll too fast
//! to follow, to scroll back a little).
//!
//! One window around the area, cut to the frame and the two tags so the page
//! itself shows through and gets the wheel; clicks go through it, it never
//! takes focus, and it is kept out of captures (on Windows), so the frames
//! being stitched never show it.

use std::num::NonZeroU32;
use std::rc::Rc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use softbuffer::{Context, Surface};
use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::event_loop::ActiveEventLoop;
use winit::window::{Window, WindowAttributes, WindowId, WindowLevel};

use crate::palette::*;

const SCALE: i32 = 2;
/// A tag's height, and the room between it and the frame.
const TAG_H: i32 = crate::bitmap_font::GLYPH_H * SCALE + 12;
const GAP: i32 = 6;
/// How far the frame sits outside the area, and its brackets' length.
const OUT: i32 = 3;
const ARM: i32 = 14;
const BLINK: Duration = Duration::from_millis(500);
const HINT: &str = "Scroll the page   [Enter] or click: done   [Esc] cancel";
const LOST_HINT: &str = "Too fast to follow: scroll back a little";

/// How wide the lower tag is: room for either line.
fn hint_w() -> i32 {
    crate::keycap::line_width(HINT, SCALE).max(crate::keycap::line_width(LOST_HINT, SCALE)) + 20
}

/// The window's place on the desktop, the area's place inside it, and
/// whether the tags go above and below the area (`false`: both below, when
/// there is no room above).
fn geometry(area: (i32, i32, i32, i32)) -> ((i32, i32, i32, i32), (i32, i32), bool) {
    let (x, y, w, h) = area;
    let above = y - (TAG_H + GAP + OUT) >= 0;
    let top = if above { TAG_H + GAP + OUT } else { OUT };
    let below = if above { OUT + GAP + TAG_H } else { OUT + 2 * (GAP + TAG_H) };
    let left = OUT;
    let ww = (w + 2 * OUT + 360).max(hint_w() + OUT);
    ((x - left, y - top, ww, top + h + below), (left, top), above)
}

pub struct ScrollHud {
    window: Rc<Window>,
    _ctx: Context<Rc<Window>>,
    surface: Surface<Rc<Window>, Rc<Window>>,
    area: (i32, i32, i32, i32),
    height: u32,
    /// The last scroll was too fast to follow.
    lost: bool,
    opened: Instant,
    drawn_cursor: bool,
}

impl ScrollHud {
    pub fn new(loop_target: &ActiveEventLoop, area: (i32, i32, i32, i32)) -> Result<Self> {
        let ((wx, wy, ww, wh), _, _) = geometry(area);
        let attrs = WindowAttributes::default()
            .with_title("Frame Fang")
            .with_decorations(false)
            .with_resizable(false)
            .with_active(false)
            .with_visible(false)
            .with_inner_size(PhysicalSize::new(ww as u32, wh as u32))
            .with_position(PhysicalPosition::new(wx, wy))
            .with_window_level(WindowLevel::AlwaysOnTop);
        #[cfg(windows)]
        let attrs = {
            use winit::platform::windows::WindowAttributesExtWindows;
            attrs.with_skip_taskbar(true)
        };
        let window = loop_target.create_window(attrs).map(Rc::new)
            .map_err(|e| anyhow!("create_window (scroll hud): {e}"))?;
        let _ = window.set_cursor_hittest(false);
        // Click-through makes it a layered window, which shows nothing
        // until it is given an opacity.
        #[cfg(windows)]
        crate::toast::set_opacity(&window, 255);
        #[cfg(windows)]
        let _ = crate::recording_indicator::exclude_from_capture(&window);
        crate::window_shape::apply(&window, &shape(area));
        crate::reveal::cloak(&window);
        window.set_visible(true);
        let ctx = Context::new(window.clone()).map_err(|e| anyhow!("softbuffer Context::new (scroll hud): {e}"))?;
        let surface = Surface::new(&ctx, window.clone()).map_err(|e| anyhow!("softbuffer Surface::new (scroll hud): {e}"))?;
        let mut me = ScrollHud { window, _ctx: ctx, surface, area, height: area.3 as u32, lost: false, opened: Instant::now(), drawn_cursor: true };
        me.redraw();
        crate::reveal::uncloak(&me.window);
        Ok(me)
    }

    pub fn window_id(&self) -> WindowId { self.window.id() }

    /// The stitched picture is this tall now.
    pub fn set_height(&mut self, h: u32) {
        if h != self.height { self.height = h; self.window.request_redraw(); }
    }

    /// Too fast to follow (`true`), or following again.
    pub fn set_lost(&mut self, lost: bool) {
        if lost != self.lost { self.lost = lost; self.window.request_redraw(); }
    }

    /// The cursor blinks.
    pub fn tick(&mut self) {
        if self.cursor_on() != self.drawn_cursor { self.window.request_redraw(); }
    }

    fn cursor_on(&self) -> bool { (self.opened.elapsed().as_millis() / BLINK.as_millis()) % 2 == 0 }

    pub fn redraw(&mut self) {
        let phys = self.window.inner_size();
        let (Some(w), Some(h)) = (NonZeroU32::new(phys.width), NonZeroU32::new(phys.height)) else { return };
        if self.surface.resize(w, h).is_err() { return; }
        self.drawn_cursor = self.cursor_on();
        let Ok(mut buf) = self.surface.buffer_mut() else { return };
        draw(&mut buf, w.get() as usize, h.get() as usize, self.area, self.height, self.lost, self.drawn_cursor);
        let _ = buf.present();
    }
}

/// The prompt: what is happening and how tall it is so far.
pub fn prompt(height: u32) -> String {
    let n = height.to_string();
    let mut grouped = String::new();
    for (i, c) in n.chars().enumerate() {
        if i > 0 && (n.len() - i) % 3 == 0 { grouped.push(','); }
        grouped.push(c);
    }
    format!("> SCROLL  {grouped} px")
}

/// Where the two tags go in the window: (x, y, w) each.
fn tags(area: (i32, i32, i32, i32), height: u32) -> ((i32, i32, i32), (i32, i32, i32)) {
    let (_, (ax, ay), above) = geometry(area);
    let (_, _, _, h) = area;
    let cursor_w = crate::bitmap_font::GLYPH_W * SCALE;
    let top_w = crate::bitmap_font::measure(&prompt(height.max(99_999)), SCALE) + 8 + cursor_w + 20;
    let below_y = ay + h + OUT + GAP;
    if above {
        ((ax - OUT, 0, top_w), (ax - OUT, below_y, hint_w()))
    } else {
        ((ax - OUT, below_y, top_w), (ax - OUT, below_y + TAG_H + GAP, hint_w()))
    }
}

/// The window's shape: the frame around the area (not the area), and the
/// tags.
fn shape(area: (i32, i32, i32, i32)) -> Vec<crate::window_shape::Rect> {
    let (_, (ax, ay), _) = geometry(area);
    let (_, _, w, h) = area;
    let (t, b) = tags(area, area.3 as u32);
    let (fx, fy, fw, fh) = (ax - OUT, ay - OUT, w + 2 * OUT, h + 2 * OUT);
    // `window_shape` takes corners, `(x0, y0, x1, y1)`; these are laid out
    // as `(x, y, w, h)` (as `(x, y, w, h)` they cut the window to slivers
    // and nothing showed while scrolling, 2026-11-20).
    let corners = |(x, y, w, h): (i32, i32, i32, i32)| (x, y, x + w, y + h);
    [
        (fx, fy, fw, OUT), (fx, fy + fh - OUT, fw, OUT),
        (fx, fy, OUT, fh), (fx + fw - OUT, fy, OUT, fh),
        (t.0, t.1, t.2, TAG_H), (b.0, b.1, b.2, TAG_H),
    ].into_iter().map(corners).collect()
}

fn fill(buf: &mut [u32], w: usize, h: usize, (x, y, rw, rh): (i32, i32, i32, i32), c: u32) {
    for yy in y.max(0)..(y + rh).min(h as i32) {
        for xx in x.max(0)..(x + rw).min(w as i32) { buf[yy as usize * w + xx as usize] = c; }
    }
}

#[allow(clippy::too_many_arguments)]
fn draw(buf: &mut [u32], w: usize, h: usize, area: (i32, i32, i32, i32), height: u32, lost: bool, cursor_on: bool) {
    buf.fill(0);
    let (_, (ax, ay), _) = geometry(area);
    let (_, _, aw, ah) = area;
    let accent = LASER();
    // The frame just outside the area: dashes, solid corner brackets.
    let (fx, fy, fw, fh) = (ax - 1, ay - 1, aw + 2, ah + 2);
    for i in 0..fw {
        if (i / 4) % 2 == 0 { fill(buf, w, h, (fx + i, fy, 1, 1), accent); fill(buf, w, h, (fx + i, fy + fh - 1, 1, 1), accent); }
    }
    for i in 0..fh {
        if (i / 4) % 2 == 0 { fill(buf, w, h, (fx, fy + i, 1, 1), accent); fill(buf, w, h, (fx + fw - 1, fy + i, 1, 1), accent); }
    }
    let arm = ARM.min(aw / 2).min(ah / 2);
    for (cx, cy, dx, dy) in [(fx, fy, 1, 1), (fx + fw - 1, fy, -1, 1), (fx, fy + fh - 1, 1, -1), (fx + fw - 1, fy + fh - 1, -1, -1)] {
        let hx = if dx > 0 { cx - 2 } else { cx - arm + 1 };
        let vy = if dy > 0 { cy - 2 } else { cy - arm + 1 };
        let ty = if dy > 0 { cy - 2 } else { cy };
        let lx = if dx > 0 { cx - 2 } else { cx };
        fill(buf, w, h, (hx, ty, arm + 2, 3), accent);
        fill(buf, w, h, (lx, vy, 3, arm + 2), accent);
    }
    // The tags.
    let (t, b) = tags(area, height);
    let text_y = |y: i32| y + (TAG_H - crate::bitmap_font::GLYPH_H * SCALE) / 2;
    for (x, y, tw) in [t, b] {
        fill(buf, w, h, (x, y, tw, TAG_H), 0);
        let c = LASER_DIM();
        fill(buf, w, h, (x, y, tw, 1), c);
        fill(buf, w, h, (x, y + TAG_H - 1, tw, 1), c);
        fill(buf, w, h, (x, y, 1, TAG_H), c);
        fill(buf, w, h, (x + tw - 1, y, 1, TAG_H), c);
    }
    let p = prompt(height);
    let px = t.0 + 10;
    {
        let mut s = crate::painter::U32Surface { buf: &mut *buf, stride: w as i32, height: h as i32 };
        let c = framefang_core::color::Rgba::new_opaque((accent >> 16) as u8, (accent >> 8) as u8, accent as u8);
        crate::painter::draw_text(&mut s, px, text_y(t.1), SCALE, &p, c);
    }
    if cursor_on {
        let cx = px + crate::bitmap_font::measure(&p, SCALE) + 8;
        fill(buf, w, h, (cx, text_y(t.1), crate::bitmap_font::GLYPH_W * SCALE, crate::bitmap_font::GLYPH_H * SCALE), accent);
    }
    let (hint, ink) = if lost { (LOST_HINT, WARN()) } else { (HINT, TEXT_MUTED()) };
    crate::keycap::draw_line(buf, w, h, b.0 + 10, text_y(b.1), hint, SCALE, ink);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_prompt_groups_thousands() {
        assert_eq!(prompt(480), "> SCROLL  480 px");
        assert_eq!(prompt(10_791), "> SCROLL  10,791 px");
    }

    #[test]
    fn the_window_is_wide_enough_for_the_hint_over_a_narrow_area() {
        let area = (300, 300, 60, 200);
        let ((_, _, ww, _), _, _) = geometry(area);
        let (_, (bx, _, bw)) = tags(area, 200);
        assert!(bx + bw <= ww, "the lower tag ends inside the window: {} > {ww}", bx + bw);
    }

    #[test]
    fn the_window_leaves_the_area_itself_uncovered() {
        for area in [(500, 300, 800, 600), (100, 5, 400, 300)] {
            let ((wx, wy, _, _), _, _) = geometry(area);
            for (x0, y0, x1, y1) in shape(area) {
                assert!(x1 > x0 && y1 > y0, "a shape rectangle is corners, not a size: {:?}", (x0, y0, x1, y1));
                // In desktop pixels, no shape rectangle reaches into the area.
                let (dx0, dy0, dx1, dy1) = (wx + x0, wy + y0, wx + x1, wy + y1);
                let inside_x = dx0 < area.0 + area.2 && dx1 > area.0;
                let inside_y = dy0 < area.1 + area.3 && dy1 > area.1;
                assert!(!(inside_x && inside_y), "{:?} covers part of {area:?}", (dx0, dy0, dx1, dy1));
            }
        }
    }

    #[test]
    fn everything_drawn_is_inside_the_window_shape() {
        // Nothing showed while scrolling when the shape was cut wrong
        // (2026-11-20): every lit pixel must be one the shape keeps.
        for area in [(500, 300, 800, 600), (100, 5, 400, 300)] {
            let ((_, _, ww, wh), _, _) = geometry(area);
            let (w, h) = (ww as usize, wh as usize);
            let mut buf = vec![0u32; w * h];
            draw(&mut buf, w, h, area, 12_345, false, true);
            let shaped = shape(area);
            let mut lit = 0;
            for y in 0..h { for x in 0..w {
                if buf[y * w + x] == 0 { continue; }
                lit += 1;
                let (x, y) = (x as i32, y as i32);
                assert!(shaped.iter().any(|(x0, y0, x1, y1)| x >= *x0 && x < *x1 && y >= *y0 && y < *y1),
                        "{area:?}: drawn pixel ({x}, {y}) is cut away");
            } }
            assert!(lit > 1000, "the frame and both tags are drawn");
        }
    }

    /// Writes target/scroll-hud.png: the look around a sample area.
    #[test]
    #[ignore]
    fn render_scroll_hud_to_png() {
        let area = (0, 0, 520, 300);
        let ((_, _, ww, wh), (ax, ay), _) = geometry((200, 200, 520, 300));
        let (w, h) = (ww as usize, wh as usize);
        let mut buf = vec![0u32; w * h];
        draw(&mut buf, w, h, (200, 200, area.2, area.3), 4210, std::env::var("FF_LOST").is_ok(), true);
        // The page showing through the hole.
        for y in 0..area.3 { for x in 0..area.2 {
            let v = if (y / 22) % 2 == 0 { 0x00_F4_F4_F6 } else { 0x00_FF_FF_FF };
            buf[(ay + y) as usize * w + (ax + x) as usize] = v;
        } }
        let shaped = shape((200, 200, area.2, area.3));
        let img = image::RgbImage::from_fn(w as u32, h as u32, |x, y| {
            let inside = shaped.iter().any(|(x0, y0, x1, y1)| (x as i32) >= *x0 && (x as i32) < *x1 && (y as i32) >= *y0 && (y as i32) < *y1);
            let in_area = (x as i32) >= ax && (x as i32) < ax + area.2 && (y as i32) >= ay && (y as i32) < ay + area.3;
            let p = buf[y as usize * w + x as usize];
            if inside || in_area { image::Rgb([(p >> 16) as u8, (p >> 8) as u8, p as u8]) } else { image::Rgb([40, 44, 52]) }
        });
        img.save(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/scroll-hud.png")).unwrap();
    }
}
