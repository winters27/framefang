//! Window chrome for the dialogs (Settings, About, Updates, First run, the
//! two converters).
//!
//! Each dialog draws its own header band with its title and its Close / Save
//! / Cancel buttons, and above it a title strip drawn here, so the dialogs
//! look like one app on every platform while still behaving like that
//! platform's windows:
//!
//! - **Windows:** frameless, with the strip carrying icon, name, minimize and
//!   close; the system keeps the drop shadow and gives Windows 11's rounded
//!   corners and a hairline border (DWM).
//! - **macOS:** the native frame stays (rounded corners, shadow, the red /
//!   yellow / green buttons at the left), with its title bar made
//!   transparent so the strip shows through; the strip centers the name and
//!   leaves the buttons' corner to the system.
//! - **Linux:** frameless like Windows. On X11 the window is cut to rounded
//!   corners (Shape extension); everywhere a 1 px edge is drawn, since there
//!   is no system border to separate the window from what is behind it.

use winit::window::{Window, WindowAttributes};

/// `attrs` set up for a dialog's own chrome on this platform.
pub fn frameless(attrs: WindowAttributes) -> WindowAttributes {
    #[cfg(windows)]
    {
        use winit::platform::windows::WindowAttributesExtWindows;
        attrs.with_decorations(false).with_undecorated_shadow(true)
    }
    #[cfg(target_os = "macos")]
    {
        use winit::platform::macos::WindowAttributesExtMacOS;
        attrs
            .with_titlebar_transparent(true)
            .with_title_hidden(true)
            .with_fullsize_content_view(true)
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        attrs.with_decorations(false)
    }
}

/// Round the window's corners and give its edge a quiet dark hairline, the
/// way Windows 11 draws its own windows. A frameless window gets square
/// corners and a light system border unless it asks. No-op on Windows 10,
/// which has neither attribute.
#[cfg(windows)]
pub fn finish(window: &Window) {
    use windows::Win32::Foundation::{COLORREF, HWND};
    use windows::Win32::Graphics::Dwm::{
        DwmSetWindowAttribute, DWMWA_BORDER_COLOR, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND,
    };
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};

    let Ok(handle) = window.window_handle() else { return };
    let RawWindowHandle::Win32(h) = handle.as_raw() else { return };
    let hwnd = HWND(h.hwnd.get() as *mut core::ffi::c_void);
    let corners = DWMWCP_ROUND;
    // The palette's hairline, flipped to Windows' 0x00BBGGRR.
    let rule = crate::palette::FIELD_BORDER();
    let border = COLORREF(((rule & 0xFF) << 16) | (rule & 0xFF00) | ((rule >> 16) & 0xFF));
    unsafe {
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            std::ptr::from_ref(&corners).cast(),
            std::mem::size_of_val(&corners) as u32,
        );
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_BORDER_COLOR,
            std::ptr::from_ref(&border).cast(),
            std::mem::size_of_val(&border) as u32,
        );
    }
}

/// Linux: on X11, cut the window to rounded corners. The dialogs are a fixed
/// size, so once is enough.
#[cfg(target_os = "linux")]
pub fn finish(window: &Window) {
    let r = corner_radius();
    if r == 0 { return; }
    let size = window.inner_size();
    crate::window_shape::apply(window, &crate::window_shape::rounded(size.width as i32, size.height as i32, r));
}

/// macOS keeps its native frame; nothing to add.
#[cfg(not(any(windows, target_os = "linux")))]
pub fn finish(_window: &Window) {}

/// Corner radius this app cuts dialogs to itself: Linux on X11 only (Windows
/// and macOS round their own; Wayland can't be cut without alpha).
pub fn corner_radius() -> i32 {
    if cfg!(target_os = "linux") && crate::window_shape::supported() { 8 } else { 0 }
}

/// A press at `y` that hit no control: start moving the window when it is
/// on the header band, which reads as part of the title bar.
pub fn press_off_controls(window: &Window, y: i32, header_h: i32) {
    if y >= 0 && y < header_h {
        let _ = window.drag_window();
    }
}

// ── the title bar ───────────────────────────────────────────────────────────
//
// A strip across the top of every dialog. The dialog's content is drawn into
// its own buffer and placed below the strip, so no dialog's layout changes;
// its window is `TITLE_H` taller and its cursor is handed over shifted up by
// `TITLE_H`.

/// Where the strip's controls go.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layout {
    /// Icon and name at the left, minimize and close drawn at the right
    /// (Windows' proportions, 46 x 32). Windows and Linux.
    Windows,
    /// The system's own buttons at the left, the name centered. macOS.
    Mac,
}

/// This platform's layout.
pub const LAYOUT: Layout = if cfg!(target_os = "macos") { Layout::Mac } else { Layout::Windows };

/// Height of the title strip: Windows' 32, or macOS's 28 so the system's
/// buttons sit centered in it.
pub const TITLE_H: i32 = if cfg!(target_os = "macos") { 28 } else { 32 };
const BTN_W: i32 = 46;
/// The corner macOS's red / yellow / green buttons occupy.
const MAC_BUTTONS_W: i32 = 78;
/// The title: the pixel font at this scale, and (Windows / Linux) where it
/// starts, past the icon.
const TITLE_SCALE: i32 = 2;
const TITLE_X: i32 = 36;
const GLYPH: u32 = crate::palette::TEXT_BRIGHT;
/// Windows' own close-hover red.
const CLOSE_HOVER: u32 = 0x00_C4_2B_1C;

/// What part of the title strip a point is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TitlePart { Drag, Minimize, Close }

/// Which part of the strip of a `win_w`-wide window `(x, y)` (window
/// coordinates) is on, if any.
pub fn title_part(win_w: i32, pos: (i32, i32)) -> Option<TitlePart> {
    title_part_in(LAYOUT, TITLE_H, win_w, pos)
}

fn title_part_in(layout: Layout, strip_h: i32, win_w: i32, (x, y): (i32, i32)) -> Option<TitlePart> {
    if y < 0 || y >= strip_h || x < 0 || x >= win_w { return None; }
    match layout {
        // The system's buttons take their own clicks.
        Layout::Mac if x < MAC_BUTTONS_W => None,
        Layout::Mac => Some(TitlePart::Drag),
        Layout::Windows => Some(if x >= win_w - BTN_W {
            TitlePart::Close
        } else if x >= win_w - 2 * BTN_W {
            TitlePart::Minimize
        } else {
            TitlePart::Drag
        }),
    }
}

/// A dialog's title bar: which button the pointer is over.
#[derive(Default)]
pub struct TitleBar {
    hover: Option<TitlePart>,
}

impl TitleBar {
    /// Track the pointer at window position `raw`. True when the hover
    /// changed and the strip needs redrawing.
    pub fn hover_at(&mut self, window: &Window, raw: (i32, i32)) -> bool {
        let part = title_part(window.inner_size().width as i32, raw)
            .filter(|p| *p != TitlePart::Drag);
        let changed = part != self.hover;
        self.hover = part;
        changed
    }

    /// The pointer left the window.
    pub fn leave(&mut self) -> bool {
        self.hover.take().is_some()
    }

    /// A left press at `content` (the dialog's own coordinates, so the strip
    /// is at negative y). Moves or minimizes the window itself and returns
    /// what was hit; `Close` is the caller's to act on, as its own close.
    /// `None` when the press is below the strip.
    pub fn press(&self, window: &Window, content: (i32, i32)) -> Option<TitlePart> {
        let raw = (content.0, content.1 + TITLE_H);
        // On a Mac the system's buttons corner is `None` here: theirs.
        let part = title_part(window.inner_size().width as i32, raw)?;
        match part {
            TitlePart::Drag => { let _ = window.drag_window(); }
            TitlePart::Minimize => window.set_minimized(true),
            TitlePart::Close => {}
        }
        Some(part)
    }

    /// Fill `frame` (the whole window, `win_w` wide): the strip on `bg`,
    /// then `content` below it, then (Linux) the window's edge.
    pub fn draw(&self, frame: &mut [u32], win_w: usize, content: &[u32], title: &str, bg: u32) {
        let edge = if cfg!(target_os = "linux") { Some(corner_radius()) } else { None };
        self.draw_in(LAYOUT, TITLE_H, edge, frame, win_w, content, title, bg);
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_in(&self, layout: Layout, strip_h: i32, edge: Option<i32>,
               frame: &mut [u32], win_w: usize, content: &[u32], title: &str, bg: u32) {
        let strip = strip_h as usize * win_w;
        if frame.len() < strip || win_w == 0 { return; }
        frame[..strip].fill(bg);
        let n = content.len().min(frame.len() - strip);
        frame[strip..strip + n].copy_from_slice(&content[..n]);

        let w = win_w as i32;
        let rgba = |c: u32| framefang_core::color::Rgba::new((c >> 16) as u8, (c >> 8) as u8, c as u8, 255);
        // The window's name is its heading (Brandon 2026-11-20: the strip
        // and a big "FRAME FANG // ..." band under it said the same thing):
        // the pixel font in the accent with its glow, "// SETTINGS".
        let tw = crate::bitmap_font::measure(title, TITLE_SCALE);
        let ty = (strip_h - crate::bitmap_font::GLYPH_H * TITLE_SCALE) / 2;
        let tx = match layout {
            // Centered, as macOS titles are, clear of the system's buttons.
            Layout::Mac => ((w - tw) / 2).max(MAC_BUTTONS_W),
            Layout::Windows => TITLE_X,
        };
        crate::hud::glow_text(&mut frame[..strip], win_w, strip_h as usize, tx, ty, TITLE_SCALE, 4, title);
        {
            let mut s = crate::painter::U32Surface { buf: &mut frame[..strip], stride: w, height: strip_h };
            match layout {
                Layout::Mac => {}
                Layout::Windows => {
                    icon_16(&mut s, 10, (strip_h - 16) / 2);
                    // Minimize and close, with Windows' hover fills.
                    for (part, x0) in [(TitlePart::Minimize, w - 2 * BTN_W), (TitlePart::Close, w - BTN_W)] {
                        let hovered = self.hover == Some(part);
                        if hovered {
                            let fill = if part == TitlePart::Close { CLOSE_HOVER } else { crate::palette::HOVER_FILL() };
                            for y in 0..strip_h as usize {
                                let row = y * win_w;
                                s.buf[row + x0.max(0) as usize..row + (x0 + BTN_W).min(w) as usize].fill(fill);
                            }
                        }
                        let ink = if hovered && part == TitlePart::Close { rgba(0x00FF_FFFF) } else { rgba(GLYPH) };
                        let (cx, cy) = (x0 + BTN_W / 2, strip_h / 2);
                        match part {
                            TitlePart::Minimize => crate::painter::line(&mut s, cx - 5, cy, cx + 5, cy, 1.0, ink),
                            _ => {
                                crate::painter::line(&mut s, cx - 5, cy - 5, cx + 5, cy + 5, 1.0, ink);
                                crate::painter::line(&mut s, cx + 5, cy - 5, cx - 5, cy + 5, 1.0, ink);
                            }
                        }
                    }
                }
            }
        }
        if let Some(r) = edge {
            draw_edge(frame, win_w, frame.len() / win_w, r, crate::palette::FIELD_BORDER());
        }
    }
}

/// A 1 px line just inside the window's (rounded) outline: every pixel that
/// is inside and touches the outside.
fn draw_edge(frame: &mut [u32], w: usize, h: usize, r: i32, color: u32) {
    let (wi, hi) = (w as i32, h as i32);
    let band = r.max(1);
    let inside = |x: i32, y: i32| crate::window_shape::inside_rounded(x, y, wi, hi, r);
    for y in 0..hi {
        let near_y = y < band || y >= hi - band;
        for x in 0..wi {
            if !near_y && x >= band && x < wi - band { continue; }
            if !inside(x, y) { continue; }
            if !inside(x - 1, y) || !inside(x + 1, y) || !inside(x, y - 1) || !inside(x, y + 1) {
                frame[y as usize * w + x as usize] = color;
            }
        }
    }
}

/// The 16 px app icon, blended at `(x, y)`.
fn icon_16(s: &mut crate::painter::U32Surface, x: i32, y: i32) {
    use std::sync::OnceLock;
    static ICON: OnceLock<Option<image::RgbaImage>> = OnceLock::new();
    let icon = ICON.get_or_init(|| {
        image::load_from_memory(include_bytes!("../../../../icons/linux_hicolor/16x16/apps/framefang.png"))
            .ok()
            .map(|i| i.into_rgba8())
    });
    let Some(icon) = icon else { return };
    for (px, py, p) in icon.enumerate_pixels() {
        let a = p.0[3] as u32;
        if a == 0 { continue; }
        let (dx, dy) = (x + px as i32, y + py as i32);
        if dx < 0 || dy < 0 || dx >= s.stride || dy >= s.height { continue; }
        let i = (dy * s.stride + dx) as usize;
        let d = s.buf[i];
        let mix = |src: u8, shift: u32| (src as u32 * a + ((d >> shift) & 0xFF) * (255 - a)) / 255;
        s.buf[i] = (mix(p.0[0], 16) << 16) | (mix(p.0[1], 8) << 8) | mix(p.0[2], 0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_windows_strip_splits_into_drag_minimize_and_close() {
        let (w, h) = (640, 32);
        let part = |p| title_part_in(Layout::Windows, h, w, p);
        assert_eq!(part((10, 5)), Some(TitlePart::Drag));
        assert_eq!(part((w - 1, 0)), Some(TitlePart::Close));
        assert_eq!(part((w - BTN_W, h - 1)), Some(TitlePart::Close));
        assert_eq!(part((w - BTN_W - 1, 10)), Some(TitlePart::Minimize));
        assert_eq!(part((w - 2 * BTN_W - 1, 10)), Some(TitlePart::Drag));
        // Below the strip, or outside the window, is not the title bar.
        assert_eq!(part((10, h)), None);
        assert_eq!(part((10, -1)), None);
    }

    #[test]
    fn the_mac_strip_leaves_the_system_buttons_alone_and_drags_elsewhere() {
        let (w, h) = (640, 28);
        let part = |p| title_part_in(Layout::Mac, h, w, p);
        assert_eq!(part((10, 10)), None, "the red / yellow / green corner is the system's");
        assert_eq!(part((MAC_BUTTONS_W, 10)), Some(TitlePart::Drag));
        assert_eq!(part((w - 1, 10)), Some(TitlePart::Drag), "no drawn close at the right on a Mac");
        assert_eq!(part((300, h)), None);
    }

    #[test]
    fn content_lands_below_the_strip() {
        let (w, content_h) = (100usize, 10usize);
        for (layout, strip_h) in [(Layout::Windows, 32), (Layout::Mac, 28)] {
            let mut frame = vec![0u32; w * (strip_h as usize + content_h)];
            let content = vec![0x00AB_CDEF; w * content_h];
            TitleBar::default().draw_in(layout, strip_h, None, &mut frame, w, &content, "// SET", 0x0008_0C0A);
            assert_eq!(frame[strip_h as usize * w], 0x00AB_CDEF);
            assert_eq!(*frame.last().unwrap(), 0x00AB_CDEF);
            assert_eq!(frame[w + 99], 0x0008_0C0A, "strip background");
        }
    }

    #[test]
    fn every_dialog_title_fits_its_strip_beside_the_buttons() {
        // The pixel-font titles: past the icon, clear of minimize, and
        // inside the strip with their glow.
        for t in ["// SETTINGS", "// ABOUT", "// UPDATES", "// WELCOME", "// CONVERT IMAGE", "// CONVERT VIDEO"] {
            let tw = crate::bitmap_font::measure(t, TITLE_SCALE);
            assert!(TITLE_X + tw + 16 <= 480 - 2 * BTN_W, "{t}");
            assert!(crate::bitmap_font::GLYPH_H * TITLE_SCALE + 8 <= TITLE_H.min(28), "{t} is too tall");
        }
        assert!(TITLE_X >= 10 + 16 + 8, "past the icon");
    }

    #[test]
    fn the_linux_edge_traces_the_rounded_outline() {
        let (w, h, r) = (60usize, 40usize, 8);
        let mut frame = vec![0u32; w * h];
        draw_edge(&mut frame, w, h, r, 0xEE);
        assert_eq!(frame[w / 2], 0xEE, "top edge");
        assert_eq!(frame[(h / 2) * w], 0xEE, "left edge");
        assert_eq!(frame[(h - 1) * w + w / 2], 0xEE, "bottom edge");
        assert_eq!(frame[0], 0, "the cut corner is left alone");
        assert_eq!(frame[(h / 2) * w + w / 2], 0, "the middle is untouched");
        // Square on Wayland: the very corner pixel is edge.
        let mut sq = vec![0u32; w * h];
        draw_edge(&mut sq, w, h, 0, 0xEE);
        assert_eq!(sq[0], 0xEE);
    }
}
