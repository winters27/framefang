/*!
The window icon every Frame Fang window sets on itself (editor overlay, pins,
the settings, about, updates and convert dialogs, the recording indicator).

It is decoded from the same 256 by 256 brand PNG the tray icon, the Linux
launcher entry, the Windows `.ico` and the macOS `.icns` are made from, so the
taskbar, Dock and Alt-Tab pictures match the tray and the installer.
*/

use {std::sync::OnceLock, winit::window::Icon};

/// The brand PNG, compiled in. 256 pixels downsamples cleanly to any size an
/// OS asks for while staying only a few kilobytes.
pub(crate) const BRAND_PNG: &[u8] =
    include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../../icons/linux_hicolor/256x256/apps/framefang.png"));

/// Decoded once, since a normal session opens several windows and each
/// would otherwise decode the PNG again.
static DECODED: OnceLock<Option<Icon>> = OnceLock::new();

/// The brand icon, or `None` if it could not be built. That should not
/// happen with a compiled-in PNG, and it is not an error worth surfacing: a
/// window without an icon works fine and the OS shows its generic one.
pub fn shared() -> Option<Icon> {
    DECODED.get_or_init(decode).clone()
}

fn decode() -> Option<Icon> {
    let rgba = image::load_from_memory(BRAND_PNG).ok()?.into_rgba8();
    let (width, height) = rgba.dimensions();
    Icon::from_rgba(rgba.into_raw(), width, height).ok()
}
