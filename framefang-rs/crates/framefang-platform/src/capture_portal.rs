//! Screen capture through the desktop's Screenshot portal
//! (`org.freedesktop.portal.Screenshot`), used only when the normal path
//! cannot run.
//!
//! The normal path in [`crate::capture`] uses `xcap`, and `xcap` is happy to
//! grab pixels on Wayland (it tries GNOME Shell, then this portal, then
//! wlroots screencopy). Its weak spot is listing the monitors: that always
//! goes through X11 RandR, so on a session with no XWayland at all the list
//! fails and the whole capture fails with it, even though the compositor
//! would gladly hand over a picture.
//!
//! For that case we ask the portal for one picture of the whole screen and
//! present it as a single monitor at the origin with no scaling. That loses
//! the per-monitor layout, which is why it is a fallback and not the default.

use std::path::{Path, PathBuf};

use ashpd::desktop::screenshot;
use framefang_core::dpi::{DisplayMap as Map, PhysicalRect as Rect};
use image::RgbaImage;

use crate::capture::{Captured, MonitorFrame};
use crate::Error;

/// Take a full-screen shot through the portal and wrap it as a capture.
///
/// The request is non-interactive: Frame Fang draws its own selection overlay
/// straight after, and showing the portal's region picker first would mean
/// two pickers in a row. The portal may still ask the user for permission the
/// first time, which is its call to make.
pub fn capture_via_portal() -> crate::Result<Captured> {
    let location = pollster::block_on(request_screenshot())?;
    // The portal writes a PNG somewhere it chooses and hands back a file
    // URI. Nobody else cleans that file up, so it is deleted once read.
    let file = local_file(&location)?;
    as_single_monitor(read_and_discard(&file)?)
}

/// Shorthand for the capture error every failure here becomes.
fn fail(message: String) -> Error {
    Error::Capture(message)
}

/// Send the portal request and wait for the URI it answers with.
async fn request_screenshot() -> crate::Result<ashpd::url::Url> {
    let pending = screenshot::Screenshot::request()
        .interactive(false)
        .modal(true)
        .send()
        .await
        .map_err(|e| portal_failure("sending the request", &e))?;
    match pending.response() {
        Ok(answer) => Ok(answer.uri().clone()),
        Err(e) => Err(portal_failure("waiting for its answer", &e)),
    }
}

/// The path behind a `file://` URI. Anything else (a remote or virtual
/// location) cannot be opened here.
fn local_file(location: &ashpd::url::Url) -> crate::Result<PathBuf> {
    location.to_file_path().map_err(|()| fail(format!(
        "the desktop saved its screenshot at {location}, which is not a file on this \
         computer, so Framefang has no way to open it."
    )))
}

/// Decode the PNG at `path`, then remove the file whether or not decoding
/// worked, so a bad file is not left behind either.
fn read_and_discard(path: &Path) -> crate::Result<RgbaImage> {
    let decoded = image::open(path);
    let _ = std::fs::remove_file(path);
    decoded.map(|picture| picture.into_rgba8()).map_err(|why| fail(format!(
        "the desktop saved a screenshot to {}, but it could not be decoded: {why}",
        path.display()
    )))
}

/// Present one bitmap as a capture of a single 1x monitor at `(0, 0)`.
///
/// The portal says nothing about where its picture sits in a wider virtual
/// desktop, and for "the screen" the origin is the honest answer. Its pixels
/// are device pixels already, so an identity map keeps every later
/// coordinate conversion a no-op.
fn as_single_monitor(bitmap: RgbaImage) -> crate::Result<Captured> {
    let (w, h) = bitmap.dimensions();
    if w.min(h) == 0 {
        return Err(fail("the desktop's screenshot came back with no pixels in it".into()));
    }
    let whole_screen = MonitorFrame {
        x: 0,
        y: 0,
        width: w,
        height: h,
        name: String::from("portal"),
        scale_factor: 1.0,
        effective_scale: 1.0,
        physical: Rect::new(0, 0, w, h),
    };
    Ok(Captured { map: Map::identity(w, h), virtual_origin: (0, 0), monitors: vec![whole_screen], bitmap })
}

/// The error shown when the portal itself fails, with a hint about which
/// packages provide it.
fn portal_failure(step: &str, cause: &ashpd::Error) -> Error {
    fail(format!(
        "the desktop's screenshot service failed while {step} ({cause}). It is \
         provided by xdg-desktop-portal together with your desktop's own backend \
         (such as xdg-desktop-portal-gnome, -kde or -wlr); make sure both are \
         installed and running."
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_portal_shot_becomes_one_unscaled_monitor() {
        let shot = as_single_monitor(RgbaImage::new(640, 360)).unwrap();
        assert_eq!(shot.virtual_origin, (0, 0));
        assert_eq!(shot.bitmap.dimensions(), (640, 360));
        assert_eq!(shot.monitors.len(), 1);

        let m = &shot.monitors[0];
        assert_eq!((m.x, m.y, m.width, m.height), (0, 0, 640, 360));
        assert_eq!(m.name, "portal");
        assert_eq!(m.scale_factor, 1.0);
        assert_eq!(m.effective_scale, 1.0);
        assert_eq!(m.physical, Rect::new(0, 0, 640, 360));
    }

    #[test]
    fn an_empty_portal_shot_is_an_error() {
        for (w, h) in [(0, 0), (0, 10), (10, 0)] {
            let err = as_single_monitor(RgbaImage::new(w, h)).err().unwrap();
            assert!(err.to_string().contains("no pixels"), "{w}x{h}: {err}");
        }
    }

    #[test]
    fn an_unreadable_file_is_reported_and_removed() {
        let path = std::env::temp_dir()
            .join(format!("framefang-portal-test-{}.png", std::process::id()));
        std::fs::write(&path, b"not a png").unwrap();

        let err = read_and_discard(&path).err().unwrap();
        assert!(err.to_string().contains("could not be decoded"), "{err}");
        assert!(!path.exists(), "the portal's file should be deleted even on failure");
    }
}
