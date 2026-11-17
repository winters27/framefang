//! Opening the capture windows in one piece.
//!
//! winit shows a new window before it sizes and places it, so a capture
//! overlay used to pop up at Windows' default size and then visibly grow and
//! slide out from the top left to cover the desktop, with Windows' own
//! open animation on top. On Windows the overlay and the editor are now
//! created hidden, **cloaked** (DWM keeps composing the window but puts
//! nothing on screen) with the system's open and close transitions turned
//! off, shown while still cloaked, and uncloaked only once the first frame
//! is drawn. The frozen desktop then appears whole, in a single frame.
//!
//! Cloaking rather than staying hidden: a hidden window gets no paint
//! requests and GDI drawing into it is thrown away, so the first visible
//! frame would be blank. A cloaked window is visible to everything but the
//! screen, so it paints and keeps what it drew.

use winit::window::Window;

/// Whether this platform opens the capture windows cloaked.
pub const SUPPORTED: bool = cfg!(windows);

/// Cloak `window` and turn off its open and close transitions. Call on a
/// window created hidden, then show it; [`uncloak`] after the first frame.
#[cfg(windows)]
pub fn cloak(window: &Window) {
    use windows::core::BOOL;
    use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_CLOAK, DWMWA_TRANSITIONS_FORCEDISABLED};
    let Some(hwnd) = hwnd(window) else { return };
    let on = BOOL::from(true);
    unsafe {
        for attr in [DWMWA_TRANSITIONS_FORCEDISABLED, DWMWA_CLOAK] {
            let _ = DwmSetWindowAttribute(hwnd, attr, std::ptr::from_ref(&on).cast(),
                                          std::mem::size_of_val(&on) as u32);
        }
    }
}

/// Put a cloaked window on screen, exactly as it was last drawn.
#[cfg(windows)]
pub fn uncloak(window: &Window) {
    use windows::core::BOOL;
    use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_CLOAK};
    let Some(hwnd) = hwnd(window) else { return };
    let off = BOOL::from(false);
    unsafe {
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_CLOAK, std::ptr::from_ref(&off).cast(),
                                      std::mem::size_of_val(&off) as u32);
    }
}

#[cfg(windows)]
fn hwnd(window: &Window) -> Option<windows::Win32::Foundation::HWND> {
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let handle = window.window_handle().ok()?;
    let RawWindowHandle::Win32(h) = handle.as_raw() else { return None };
    Some(windows::Win32::Foundation::HWND(h.hwnd.get() as *mut core::ffi::c_void))
}

#[cfg(not(windows))]
pub fn cloak(_window: &Window) {}

#[cfg(not(windows))]
pub fn uncloak(_window: &Window) {}
