//! Putting things on the system clipboard.
//!
//! Both helpers go through `arboard`, which already talks to each OS in its
//! own terms (OLE on Windows, X11 and Wayland selections on Linux, the
//! pasteboard on macOS), so nothing here is per-platform.

use std::borrow::Cow;

use image::{ImageBuffer, Rgba};

use crate::{Error, Result};

/// Turn any `arboard` failure into the crate's clipboard error.
fn clipboard_err(e: arboard::Error) -> Error {
    Error::Clipboard(e.to_string())
}

/// A fresh handle to the clipboard. Opening one is cheap and some platforms
/// prefer short-lived handles, so it is never cached.
fn open() -> Result<arboard::Clipboard> {
    arboard::Clipboard::new().map_err(clipboard_err)
}

/// Place an RGBA bitmap on the clipboard as an image.
///
/// The pixel buffer is lent to `arboard` rather than copied; it does any
/// encoding the target platform wants on its side.
pub fn copy_image_png(bitmap: &ImageBuffer<Rgba<u8>, Vec<u8>>) -> Result<()> {
    let (w, h) = bitmap.dimensions();
    let image = arboard::ImageData { width: w as usize, height: h as usize, bytes: Cow::Borrowed(bitmap.as_raw()) };
    open()?.set_image(image).map_err(clipboard_err)
}

/// Place plain text on the clipboard.
///
/// The Updates dialog uses this to hand over the package-manager command for
/// installs that the built-in updater leaves alone.
pub fn copy_text(text: &str) -> Result<()> {
    open()?.set_text(text.to_owned()).map_err(clipboard_err)
}
