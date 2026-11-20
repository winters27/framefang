//! Software drawing for everything Frame Fang paints itself: annotations,
//! the editor's chrome, the dialogs.
//!
//! All of it goes through one small trait, [`Surface`], so the same code
//! draws into the window's `u32` frame while someone is editing and into the
//! RGBA image that gets saved, copied or burned into a video. Nothing here
//! talks to the GPU: the areas involved are small enough that drawing them
//! on the CPU every frame keeps up with the mouse.
//!
//! The parts:
//! - `shapes`: lines, pen and marker strokes, boxes, ellipses and arrows,
//!   anti-aliased through tiny-skia;
//! - `glyphs`: the 5x7 pixel font for chrome and the bundled typeface for
//!   Text annotations;
//! - `marks`: highlights laid over words the text reader found;
//! - `steps`: the numbered step markers;
//! - `effects`: Pixelate, Spotlight and Magnify, which read the picture
//!   instead of only adding ink to it;
//! - `render`: turning an `Annotation` (or a list of them) into calls to
//!   the above.

use framefang_core::color::Rgba as FramefangRgba;
use image::{ImageBuffer, Rgba};

mod effects;
mod glyphs;
mod marks;
mod render;
mod shapes;
mod steps;

pub use effects::*;
pub use glyphs::*;
pub use marks::*;
pub use render::*;
pub use shapes::*;
pub use steps::*;

/// A grid of pixels the drawing code can read and write.
///
/// Callers only ever pass coordinates inside `0..width()` and `0..height()`
/// (the helpers below check), so implementations may index directly.
pub trait Surface {
    fn width(&self) -> i32;
    fn height(&self) -> i32;
    /// The pixel at (x, y) as RGBA. Surfaces without alpha report 255.
    fn read(&self, x: i32, y: i32) -> [u8; 4];
    /// Replaces the pixel at (x, y). Surfaces without alpha keep only the
    /// color channels.
    fn write(&mut self, x: i32, y: i32, rgba: [u8; 4]);
}

/// A window frame as softbuffer hands it out: one `0x00RRGGBB` word per
/// pixel, `stride` words per row.
pub struct U32Surface<'a> {
    pub buf: &'a mut [u32],
    pub stride: i32,
    pub height: i32,
}

impl U32Surface<'_> {
    fn index(&self, x: i32, y: i32) -> usize {
        (y * self.stride + x) as usize
    }
}

impl Surface for U32Surface<'_> {
    fn width(&self) -> i32 {
        self.stride
    }

    fn height(&self) -> i32 {
        self.height
    }

    fn read(&self, x: i32, y: i32) -> [u8; 4] {
        let [_, r, g, b] = self.buf[self.index(x, y)].to_be_bytes();
        [r, g, b, 255]
    }

    fn write(&mut self, x: i32, y: i32, rgba: [u8; 4]) {
        let at = self.index(x, y);
        self.buf[at] = u32::from_be_bytes([0, rgba[0], rgba[1], rgba[2]]);
    }
}

/// An RGBA image, such as the picture that is about to be saved.
pub struct ImageSurface<'a>(pub &'a mut ImageBuffer<Rgba<u8>, Vec<u8>>);

impl Surface for ImageSurface<'_> {
    fn width(&self) -> i32 {
        self.0.width() as i32
    }

    fn height(&self) -> i32 {
        self.0.height() as i32
    }

    fn read(&self, x: i32, y: i32) -> [u8; 4] {
        self.0[(x as u32, y as u32)].0
    }

    fn write(&mut self, x: i32, y: i32, rgba: [u8; 4]) {
        self.0[(x as u32, y as u32)] = Rgba(rgba);
    }
}

/// Whether (x, y) is a pixel of `s`.
fn in_bounds<S: Surface>(s: &S, x: i32, y: i32) -> bool {
    (0..s.width()).contains(&x) && (0..s.height()).contains(&y)
}

/// Lays `src` (straight, not premultiplied, alpha) over the pixel at (x, y)
/// and leaves the result opaque. Points off the surface are ignored, so
/// shapes may run past the edges freely.
fn blend<S: Surface>(s: &mut S, x: i32, y: i32, src: [u8; 4]) {
    let alpha = src[3];
    if alpha == 0 || !in_bounds(s, x, y) {
        return;
    }
    if alpha == 255 {
        s.write(x, y, [src[0], src[1], src[2], 255]);
        return;
    }
    let under = s.read(x, y);
    let (a, rest) = (alpha as u32, 255 - alpha as u32);
    // Rounded to nearest, so a 50 % blend of 0 and 255 lands on 128.
    let channel = |i: usize| ((src[i] as u32 * a + under[i] as u32 * rest + 127) / 255) as u8;
    s.write(x, y, [channel(0), channel(1), channel(2), 255]);
}

/// A color as the `[r, g, b, a]` that [`blend`] takes.
fn rgba_arr(c: FramefangRgba) -> [u8; 4] {
    [c.r, c.g, c.b, c.a]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_u32_frame_round_trips_its_color_channels_and_reads_opaque() {
        let mut words = vec![0u32; 6];
        let mut frame = U32Surface { buf: &mut words, stride: 3, height: 2 };
        frame.write(2, 1, [0x12, 0x34, 0x56, 0x00]);
        assert_eq!(frame.read(2, 1), [0x12, 0x34, 0x56, 0xFF]);
        assert_eq!(words[5], 0x0012_3456, "row 1, column 2 of a 3 wide frame");
    }

    #[test]
    fn blending_rounds_to_nearest_and_skips_what_is_off_the_surface() {
        let mut words = vec![0u32; 4];
        let mut frame = U32Surface { buf: &mut words, stride: 2, height: 2 };
        // Half-transparent white over black: 255 * 128 / 255 rounds to 128.
        blend(&mut frame, 0, 0, [255, 255, 255, 128]);
        assert_eq!(frame.read(0, 0), [128, 128, 128, 255]);
        // Opaque replaces; fully transparent leaves the pixel alone.
        blend(&mut frame, 1, 0, [9, 8, 7, 255]);
        blend(&mut frame, 1, 0, [200, 200, 200, 0]);
        assert_eq!(frame.read(1, 0), [9, 8, 7, 255]);
        // Off the frame on every side: nothing happens, nothing panics.
        for (x, y) in [(-1, 0), (2, 0), (0, -1), (0, 2)] {
            blend(&mut frame, x, y, [255, 0, 0, 255]);
        }
        assert_eq!(&words[2..], &[0, 0]);
    }
}
