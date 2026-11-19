//! The "Copied to clipboard" pill shown after a copy that closes the
//! capture window: copy only, or the editor's Copy with "Close the editor
//! after copying" on. Nothing else on screen says the copy worked then.
//!
//! A small window under where the picture was, in the editor's notice style.
//! It never takes focus or clicks, stays out of screenshots and recordings
//! (Windows), fades in and out (Windows) and closes itself.

use std::num::NonZeroU32;
use std::rc::Rc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use softbuffer::{Context, Surface};
use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::event_loop::ActiveEventLoop;
use winit::window::{Window, WindowAttributes, WindowId, WindowLevel};

const TEXT: &str = "Copied to clipboard";
const SCALE: i32 = 2;
const PAD_X: i32 = 14;
const PAD_Y: i32 = 10;
const FILL: u32 = 0x00_10_10_14;

const FADE_IN: Duration = Duration::from_millis(120);
const HOLD: Duration = Duration::from_millis(1000);
const FADE_OUT: Duration = Duration::from_millis(300);

/// The pill's size in device pixels.
pub fn size() -> (u32, u32) {
    let w = crate::bitmap_font::measure(TEXT, SCALE) + PAD_X * 2;
    let h = crate::bitmap_font::GLYPH_H * SCALE + PAD_Y * 2;
    (w as u32, h as u32)
}

/// How opaque the pill is `t` after it opened, 0 to 1: up quickly, held,
/// then down slowly. `None` once it is over.
fn opacity(t: Duration) -> Option<f32> {
    if t < FADE_IN { return Some(t.as_secs_f32() / FADE_IN.as_secs_f32()); }
    if t < FADE_IN + HOLD { return Some(1.0); }
    let out = t.checked_sub(FADE_IN + HOLD).filter(|o| *o < FADE_OUT)?;
    Some(1.0 - out.as_secs_f32() / FADE_OUT.as_secs_f32())
}

pub struct Toast {
    window: Rc<Window>,
    _ctx: Context<Rc<Window>>,
    surface: Surface<Rc<Window>, Rc<Window>>,
    opened: Instant,
    /// The opacity last applied, so a tick that would not change it is free.
    alpha: u8,
}

impl Toast {
    /// Show the pill with its top left at `pos`, in virtual-screen pixels.
    pub fn new(loop_target: &ActiveEventLoop, pos: (i32, i32)) -> Result<Self> {
        let (w, h) = size();
        let attrs = WindowAttributes::default()
            .with_title("Framefang")
            .with_decorations(false)
            .with_resizable(false)
            .with_active(false)
            .with_visible(false)
            .with_inner_size(PhysicalSize::new(w, h))
            .with_position(PhysicalPosition::new(pos.0, pos.1))
            .with_window_level(WindowLevel::AlwaysOnTop);
        #[cfg(windows)]
        let attrs = {
            use winit::platform::windows::WindowAttributesExtWindows;
            attrs.with_skip_taskbar(true)
        };
        let window = loop_target.create_window(attrs).map(Rc::new)
            .map_err(|e| anyhow!("create_window (toast): {e}"))?;
        // Clicks go through to whatever is under it.
        let _ = window.set_cursor_hittest(false);
        #[cfg(windows)]
        {
            let _ = crate::recording_indicator::exclude_from_capture(&window);
            set_opacity(&window, 0);
        }
        crate::reveal::cloak(&window);
        window.set_visible(true);
        let ctx = Context::new(window.clone()).map_err(|e| anyhow!("softbuffer Context::new (toast): {e}"))?;
        let surface = Surface::new(&ctx, window.clone()).map_err(|e| anyhow!("softbuffer Surface::new (toast): {e}"))?;
        let mut me = Toast { window, _ctx: ctx, surface, opened: Instant::now(), alpha: 0 };
        me.redraw();
        crate::reveal::uncloak(&me.window);
        Ok(me)
    }

    pub fn window_id(&self) -> WindowId { self.window.id() }

    pub fn redraw(&mut self) {
        let phys = self.window.inner_size();
        let (Some(w), Some(h)) = (NonZeroU32::new(phys.width), NonZeroU32::new(phys.height)) else { return; };
        if self.surface.resize(w, h).is_err() { return; }
        let Ok(mut buf) = self.surface.buffer_mut() else { return; };
        let (bw, bh) = (w.get() as usize, h.get() as usize);
        buf.fill(FILL);
        let green = crate::editor::NEON_GREEN;
        // The 1 px frame of the editor's notices.
        for x in 0..bw {
            buf[x] = green;
            buf[(bh - 1) * bw + x] = green;
        }
        for y in 0..bh {
            buf[y * bw] = green;
            buf[y * bw + bw - 1] = green;
        }
        let tw = crate::bitmap_font::measure(TEXT, SCALE);
        let th = crate::bitmap_font::GLYPH_H * SCALE;
        let mut surf = crate::painter::U32Surface { buf: &mut buf, stride: bw as i32, height: bh as i32 };
        let c = framefang_core::color::Rgba::new_opaque((green >> 16) as u8, (green >> 8) as u8, green as u8);
        crate::painter::draw_text(&mut surf, (bw as i32 - tw) / 2, (bh as i32 - th) / 2, SCALE, TEXT, c);
        let _ = buf.present();
    }

    /// Step the fade. `false` once the pill has run its course and should
    /// be dropped.
    pub fn tick(&mut self) -> bool {
        let Some(o) = opacity(self.opened.elapsed()) else { return false; };
        // Without a fade the pill shows whole until it closes.
        let a = if cfg!(windows) { (o * 255.0).round() as u8 } else { 255 };
        if a != self.alpha {
            self.alpha = a;
            #[cfg(windows)]
            set_opacity(&self.window, a);
        }
        true
    }
}

/// The whole window at `alpha` (0 to 255): a layered window, which DWM
/// blends over what is behind it.
#[cfg(windows)]
fn set_opacity(window: &Window, alpha: u8) {
    use windows::Win32::Foundation::{COLORREF, HWND};
    use windows::Win32::UI::WindowsAndMessaging::{
        GetWindowLongPtrW, SetLayeredWindowAttributes, SetWindowLongPtrW, GWL_EXSTYLE, LWA_ALPHA,
        WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TRANSPARENT,
    };
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let Ok(handle) = window.window_handle() else { return };
    let RawWindowHandle::Win32(h) = handle.as_raw() else { return };
    let hwnd = HWND(h.hwnd.get() as *mut core::ffi::c_void);
    unsafe {
        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        let want = ex | (WS_EX_LAYERED.0 | WS_EX_TRANSPARENT.0 | WS_EX_NOACTIVATE.0) as isize;
        if want != ex { SetWindowLongPtrW(hwnd, GWL_EXSTYLE, want); }
        let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), alpha, LWA_ALPHA);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pill_fades_in_quickly_holds_and_fades_out() {
        assert_eq!(opacity(Duration::ZERO), Some(0.0));
        assert_eq!(opacity(FADE_IN), Some(1.0));
        assert_eq!(opacity(FADE_IN + HOLD / 2), Some(1.0));
        let half_out = opacity(FADE_IN + HOLD + FADE_OUT / 2).unwrap();
        assert!((half_out - 0.5).abs() < 0.01);
        assert_eq!(opacity(FADE_IN + HOLD + FADE_OUT), None);
    }

    #[test]
    fn the_words_fit_with_room_around_them() {
        let (w, h) = size();
        assert!(w as i32 >= crate::bitmap_font::measure(TEXT, SCALE) + 2 * PAD_X);
        assert!(h as i32 > crate::bitmap_font::GLYPH_H * SCALE);
    }
}
