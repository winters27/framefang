//! The plumbing every themed dialog is built on: About, Welcome, Updates and
//! both Convert dialogs.
//!
//! A [`Shell`] opens a frameless window with the app's title strip and turns
//! raw window events into the handful of things a dialog reacts to
//! ([`Input`]); a [`Page`] is the body under the strip, painted fresh each
//! frame. [`fit_middle`] and [`fit_end`] shorten text to a pixel width.

use std::num::NonZeroU32;
use std::rc::Rc;

use anyhow::{anyhow, Result};
use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::ActiveEventLoop;
use winit::keyboard::{Key, NamedKey};
use winit::window::{CursorIcon, Window, WindowAttributes, WindowId};

use crate::bitmap_font::{self, GLYPH_H, GLYPH_W};
use crate::chrome::{TitleBar, TitlePart, TITLE_H};
use crate::palette::*;

/// `(x, y, width, height)` in a dialog body's pixels.
pub(crate) type Rect = (i32, i32, i32, i32);

/// Whether `p` falls in `r` (left and top edges in, right and bottom out).
pub(crate) fn inside(r: Rect, p: (i32, i32)) -> bool {
    let (x, y, w, h) = r;
    p.0 >= x && p.0 < x + w && p.1 >= y && p.1 < y + h
}

/// One window event, reduced to what a dialog acts on. The shell has
/// already dealt with anything aimed at the title strip.
pub(crate) enum Input {
    /// The OS close, or the strip's own close button.
    Close,
    /// A named key went down.
    Key(NamedKey),
    /// The pointer moved; [`Shell::cursor`] holds where.
    Moved,
    /// Left button down in the body, at [`Shell::cursor`].
    Press,
    /// Left button up.
    Release,
    Wheel(MouseScrollDelta),
    /// Draw again: asked for, or the window changed size.
    Paint,
    Nothing,
}

/// A fixed-size themed dialog window: frameless, with the app's title strip
/// across the top and a software-drawn body under it.
pub(crate) struct Shell {
    window: Rc<Window>,
    // Kept alive for as long as the surface draws through it.
    _context: softbuffer::Context<Rc<Window>>,
    surface: softbuffer::Surface<Rc<Window>, Rc<Window>>,
    strip: TitleBar,
    /// The strip's text.
    heading: &'static str,
    /// Names this dialog in log lines.
    tag: &'static str,
    /// Last frame's body pixels, reused so painting doesn't allocate.
    spare: Vec<u32>,
    /// The pointer in body coordinates: (0, 0) is just under the strip.
    pub(crate) cursor: (i32, i32),
}

impl Shell {
    /// Open a dialog whose body is `body` pixels (the strip is added on
    /// top), centered on the main screen and given focus.
    pub(crate) fn open(
        loop_target: &ActiveEventLoop,
        title: &str,
        heading: &'static str,
        tag: &'static str,
        body: (u32, u32),
    ) -> Result<Shell> {
        let size = PhysicalSize::new(body.0, body.1 + TITLE_H as u32);
        let (x, y) = centred_on_main_screen(loop_target, size);
        let attrs = crate::chrome::frameless(WindowAttributes::default())
            .with_title(title)
            .with_window_icon(crate::brand_icon::shared())
            .with_inner_size(size)
            .with_position(PhysicalPosition::new(x, y))
            .with_resizable(false);
        let window = Rc::new(
            loop_target.create_window(attrs).map_err(|e| anyhow!("{tag}: couldn't open the window: {e}"))?,
        );
        crate::chrome::finish(&window);
        // The app lives in the tray (a menu-bar agent with no Dock icon on
        // macOS), so a dialog that opened behind the front app could never
        // be found. Ask for focus outright; elsewhere it is just a raise.
        window.focus_window();
        window.set_cursor(CursorIcon::Default);
        let context = softbuffer::Context::new(window.clone())
            .map_err(|e| anyhow!("{tag}: no drawing context: {e}"))?;
        let surface = softbuffer::Surface::new(&context, window.clone())
            .map_err(|e| anyhow!("{tag}: no drawing surface: {e}"))?;
        Ok(Shell {
            window,
            _context: context,
            surface,
            strip: TitleBar::default(),
            heading,
            tag,
            spare: Vec::new(),
            cursor: Default::default(),
        })
    }

    pub(crate) fn id(&self) -> WindowId { self.window.id() }

    pub(crate) fn repaint(&self) { self.window.request_redraw(); }

    /// Sort one event: the strip takes its hovers and presses (dragging and
    /// minimizing happen inside [`TitleBar::press`]), the rest is handed on.
    pub(crate) fn input(&mut self, event: WindowEvent) -> Input {
        match event {
            WindowEvent::CloseRequested => Input::Close,
            WindowEvent::KeyboardInput {
                event: KeyEvent { logical_key: Key::Named(key), state: ElementState::Pressed, .. },
                ..
            } => Input::Key(key),
            WindowEvent::CursorLeft { .. } => {
                if self.strip.leave() { self.repaint(); }
                Input::Nothing
            }
            WindowEvent::CursorMoved { position, .. } => {
                let at = (position.x as i32, position.y as i32);
                if self.strip.hover_at(&self.window, at) { self.repaint(); }
                self.cursor = (at.0, at.1 - TITLE_H);
                Input::Moved
            }
            WindowEvent::MouseInput { state, button: MouseButton::Left, .. } => match state {
                ElementState::Released => Input::Release,
                ElementState::Pressed => match self.strip.press(&self.window, self.cursor) {
                    None => Input::Press,
                    Some(TitlePart::Close) => Input::Close,
                    Some(_) => Input::Nothing,
                },
            },
            WindowEvent::MouseWheel { delta, .. } => Input::Wheel(delta),
            WindowEvent::Resized(_) | WindowEvent::RedrawRequested => Input::Paint,
            _ => Input::Nothing,
        }
    }

    /// Store what is under the pointer now in `slot`, show the hand cursor
    /// when it is something clickable, and repaint if it changed.
    pub(crate) fn track_hover<T: PartialEq>(&self, slot: &mut T, now: T, clickable: bool) {
        self.window.set_cursor(if clickable { CursorIcon::Pointer } else { CursorIcon::Default });
        if *slot != now {
            *slot = now;
            self.repaint();
        }
    }

    /// A fresh body to paint on, already filled with the dialog backdrop.
    /// `None` while the window has no area.
    pub(crate) fn page(&mut self) -> Option<Page> {
        let size = self.window.inner_size();
        let frame = (NonZeroU32::new(size.width)?, NonZeroU32::new(size.height)?);
        let w = size.width as usize;
        let h = (size.height as usize).saturating_sub(TITLE_H as usize);
        let mut px = std::mem::take(&mut self.spare);
        px.clear();
        px.resize(w * h, 0);
        // The strip is drawn separately, so the backdrop's header band is
        // empty and its glowing rule runs along the body's top edge.
        crate::hud::backdrop(&mut px, w, h, 0);
        Some(Page { px, w: w as i32, h: h as i32, frame })
    }

    /// Put `page` on screen under the title strip.
    pub(crate) fn show(&mut self, page: Page) {
        let (fw, fh) = page.frame;
        let tag = self.tag;
        match self.surface.resize(fw, fh) {
            Err(e) => eprintln!("{tag}: couldn't size the surface: {e}"),
            Ok(()) => match self.surface.buffer_mut() {
                Err(e) => eprintln!("{tag}: no frame to draw into: {e}"),
                Ok(mut frame) => {
                    self.strip.draw(&mut frame, fw.get() as usize, &page.px, self.heading, BG_TOP());
                    if let Err(e) = frame.present() {
                        eprintln!("{tag}: couldn't show the frame: {e}");
                    }
                }
            },
        }
        self.spare = page.px;
    }
}

/// Where a window of `size` goes to sit in the middle of the main screen
/// (or the first screen, or an assumed 1920x1080 if none is reported).
fn centred_on_main_screen(loop_target: &ActiveEventLoop, size: PhysicalSize<u32>) -> (i32, i32) {
    let screen = loop_target
        .primary_monitor()
        .or_else(|| loop_target.available_monitors().next())
        .map(|m| {
            let (at, extent) = (m.position(), m.size());
            ((at.x, at.y), (extent.width as i32, extent.height as i32))
        });
    let (origin, extent) = screen.unwrap_or(((0, 0), (1920, 1080)));
    centred(origin, extent, (size.width as i32, size.height as i32))
}

/// Top-left corner that centers `size` on a screen at `origin` of `extent`,
/// never above or left of the screen when the window is the bigger one.
fn centred(origin: (i32, i32), extent: (i32, i32), size: (i32, i32)) -> (i32, i32) {
    let x = origin.0 + (extent.0 - size.0) / 2;
    let y = origin.1 + (extent.1 - size.1) / 2;
    (x.max(origin.0), y.max(origin.1))
}

/// A dialog body being painted: `0x00RRGGBB` pixels, `w` by `h`.
pub(crate) struct Page {
    px: Vec<u32>,
    w: i32,
    h: i32,
    /// The whole window, strip included.
    frame: (NonZeroU32, NonZeroU32),
}

/// A palette color as the text painter's RGBA.
fn rgba(color: u32) -> framefang_core::color::Rgba {
    framefang_core::color::Rgba::new((color >> 16) as u8, (color >> 8) as u8, color as u8, 255)
}

impl Page {
    /// Fill `r` (clipped to the page) with `color`.
    pub(crate) fn fill(&mut self, r: Rect, color: u32) {
        let (x0, y0) = (r.0.max(0), r.1.max(0));
        let (x1, y1) = ((r.0 + r.2).min(self.w), (r.1 + r.3).min(self.h));
        if x0 >= x1 { return; }
        for y in y0..y1 {
            let row = (y * self.w) as usize;
            self.px[row + x0 as usize..row + x1 as usize].fill(color & 0x00FF_FFFF);
        }
    }

    /// A one-pixel border just inside `r`.
    pub(crate) fn outline(&mut self, r: Rect, color: u32) {
        let (x, y, w, h) = r;
        self.fill((x, y, w, 1), color);
        self.fill((x, y + h - 1, w, 1), color);
        self.fill((x, y, 1, h), color);
        self.fill((x + w - 1, y, 1, h), color);
    }

    /// `s` in the pixel font with its top-left corner at `(x, y)`.
    pub(crate) fn text(&mut self, x: i32, y: i32, s: &str, color: u32) {
        self.text_at(x, y, 1, s, color);
    }

    pub(crate) fn text_at(&mut self, x: i32, y: i32, scale: i32, s: &str, color: u32) {
        crate::painter::draw_text(self, x, y, scale, s, rgba(color));
    }

    /// `s` centered in `r`.
    pub(crate) fn centered(&mut self, r: Rect, s: &str, scale: i32, color: u32) {
        let x = r.0 + (r.2 - bitmap_font::measure(s, scale)) / 2;
        let y = r.1 + (r.3 - GLYPH_H * scale) / 2;
        self.text_at(x, y, scale, s, color);
    }

    /// A dialog button. The primary one (the dialog's main action) is
    /// outlined and lettered in the accent; the others are quiet until
    /// hovered.
    pub(crate) fn button(&mut self, r: Rect, caption: &str, primary: bool, hovered: bool) {
        let (edge, ink) = match (primary, hovered) {
            (true, _) => (LASER(), LASER()),
            (false, true) => (LASER_DIM(), TEXT_BRIGHT),
            (false, false) => (PANEL_BORDER(), TEXT_BRIGHT),
        };
        if hovered {
            self.fill(r, if primary { PRIMARY_FILL() } else { HOVER_FILL() });
        }
        self.outline(r, edge);
        self.centered(r, caption, 1, ink);
    }

    /// A numbered section label (`hud::section`) from `x` with its rule out
    /// to `right`.
    pub(crate) fn section(&mut self, x: i32, right: i32, y: i32, n: usize, name: &str) {
        crate::hud::section(&mut self.px, self.w as usize, self.h as usize, x, right, y, n, name);
    }
}

impl crate::painter::Surface for Page {
    fn width(&self) -> i32 { self.w }
    fn height(&self) -> i32 { self.h }
    fn read(&self, x: i32, y: i32) -> [u8; 4] {
        if !inside((0, 0, self.w, self.h), (x, y)) { return [0, 0, 0, 255]; }
        let c = self.px[(y * self.w + x) as usize];
        [(c >> 16) as u8, (c >> 8) as u8, c as u8, 255]
    }
    fn write(&mut self, x: i32, y: i32, rgba: [u8; 4]) {
        if !inside((0, 0, self.w, self.h), (x, y)) { return; }
        let [r, g, b, _] = rgba;
        self.px[(y * self.w + x) as usize] = u32::from(r) << 16 | u32::from(g) << 8 | u32::from(b);
    }
}

/// How far the pixel font moves right per character at scale 1: the glyph
/// and the one-pixel gap after it. The last glyph has no gap, so `n`
/// characters are `n * ADVANCE - 1` wide.
const ADVANCE: i32 = GLYPH_W + 1;

/// How many characters fit in `px` at scale 1.
fn chars_in(px: i32) -> i32 { (px + 1) / ADVANCE }

/// `s`, shortened to fit `max_px` at scale 1 by cutting out its middle and
/// putting `..` in the gap. Two thirds of what is kept come from the end:
/// these strings are mostly paths, and the file name is what matters.
pub(crate) fn fit_middle(s: &str, max_px: i32) -> String {
    if bitmap_font::measure(s, 1) <= max_px { return s.to_owned(); }
    const GAP: &str = "..";
    let room = chars_in(max_px);
    if room <= GAP.len() as i32 { return GAP[..room.max(0) as usize].to_owned(); }
    let kept = (room - GAP.len() as i32) as usize;
    let from_end = (kept * 2 / 3).max(1);
    let from_start = kept - from_end;
    // `s` is wider than `room` characters, so the two pieces can't overlap.
    let total = s.chars().count();
    let byte_at = |nth: usize| s.char_indices().nth(nth).map_or(s.len(), |(i, _)| i);
    format!("{}{GAP}{}", &s[..byte_at(from_start)], &s[byte_at(total - from_end)..])
}

/// `s`, cut short with `...` to fit `max_px` at scale 1.
pub(crate) fn fit_end(s: &str, max_px: i32) -> String {
    if bitmap_font::measure(s, 1) <= max_px { return s.to_owned(); }
    let kept = (chars_in(max_px) - 3).max(0) as usize;
    let mut out: String = s.chars().take(kept).collect();
    out.push_str("...");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_window_is_centred_and_kept_on_screen() {
        assert_eq!(centred((0, 0), (1920, 1080), (480, 312)), (720, 384));
        // A second screen to the left of the main one.
        assert_eq!(centred((-1280, 0), (1280, 1024), (280, 200)), (-780, 412));
        // Bigger than the screen: pinned to its top-left corner.
        assert_eq!(centred((100, 50), (400, 300), (800, 600)), (100, 50));
    }

    #[test]
    fn inside_counts_the_top_left_edges_only() {
        let r = (10, 20, 5, 4);
        assert!(inside(r, (10, 20)));
        assert!(inside(r, (14, 23)));
        assert!(!inside(r, (15, 20)));
        assert!(!inside(r, (10, 24)));
        assert!(!inside(r, (9, 21)));
    }

    #[test]
    fn fit_middle_keeps_short_text_and_the_end_of_long_paths() {
        assert_eq!(fit_middle("C:\\shots", 200), "C:\\shots");
        // 10 characters of room: 2 dots, 3 from the start, 5 from the end.
        let max = 10 * ADVANCE - 1;
        let out = fit_middle("C:\\Users\\someone\\Pictures\\shot.png", max);
        assert_eq!(out, "C:\\..t.png");
        assert!(bitmap_font::measure(&out, 1) <= max);
        // Room for the dots and nothing else, or less.
        assert_eq!(fit_middle("abcdefghij", 2 * ADVANCE - 1), "..");
        assert_eq!(fit_middle("abcdefghij", ADVANCE - 1), ".");
        assert_eq!(fit_middle("abcdefghij", -5), "");
        // Three characters of room: one from the end, none from the start.
        assert_eq!(fit_middle("abcdefghij", 3 * ADVANCE - 1), "..j");
    }

    #[test]
    fn fit_middle_cuts_on_characters_not_bytes() {
        let out = fit_middle("\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}x", 6 * ADVANCE - 1);
        assert_eq!(out.chars().count(), 6);
        assert!(out.ends_with('x') && out.contains(".."), "{out}");
    }

    #[test]
    fn fit_end_trails_off_with_three_dots() {
        assert_eq!(fit_end("short", 100), "short");
        let max = 8 * ADVANCE - 1;
        let out = fit_end("checksum mismatch for the file", max);
        assert_eq!(out, "check...");
        assert!(bitmap_font::measure(&out, 1) <= max);
        assert_eq!(fit_end("anything", 2 * ADVANCE), "...");
    }
}
