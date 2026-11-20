//! The frame Settings can add to every saved or copied screenshot: a thin
//! border, or a soft drop shadow on a transparent margin.
//!
//! Applied once, where a picture leaves the app (a save or a copy), never to
//! the picture the editor or a pin shows. PNG keeps the shadow's
//! transparency. JPEG has none and the clipboard is read by apps that drop
//! it, so those get the shadow laid on white instead of black.

use framefang_core::settings::OutputFrame;
use image::{ImageBuffer, Rgba, RgbaImage};

/// Width of the border, in pixels.
pub const BORDER: u32 = 2;
/// The border's color: a mid grey that reads on light and dark pictures.
pub const BORDER_RGB: [u8; 3] = [0x80, 0x80, 0x80];
/// The transparent margin around a shadowed picture.
pub const MARGIN: u32 = 24;
/// How far the shadow falls below the picture.
const DROP: i32 = 6;
/// Box-blur radius; three passes come close to a Gaussian.
const BLUR: i32 = 4;
/// The shadow's darkest alpha (45 %).
const SHADOW_ALPHA: f32 = 0.45;

/// `img` with `style` around it.
pub fn apply(img: &RgbaImage, style: OutputFrame) -> RgbaImage {
    match style {
        OutputFrame::None => img.clone(),
        OutputFrame::Border => border(img),
        OutputFrame::Shadow => shadow(img),
    }
}

/// `img` with `style`, made for the clipboard: a shadow is laid on white.
pub fn for_clipboard(img: &RgbaImage, style: OutputFrame) -> RgbaImage {
    let framed = apply(img, style);
    if style == OutputFrame::Shadow { on_white(&framed) } else { framed }
}

fn border(img: &RgbaImage) -> RgbaImage {
    let (w, h) = (img.width() + 2 * BORDER, img.height() + 2 * BORDER);
    let [r, g, b] = BORDER_RGB;
    let mut out = ImageBuffer::from_pixel(w, h, Rgba([r, g, b, 255]));
    image::imageops::replace(&mut out, img, BORDER as i64, BORDER as i64);
    out
}

fn shadow(img: &RgbaImage) -> RgbaImage {
    let (iw, ih) = (img.width() as i32, img.height() as i32);
    let m = MARGIN as i32;
    let (w, h) = (iw + 2 * m, ih + 2 * m);
    // The picture's rectangle, dropped, as a coverage mask, then blurred.
    let mut mask = vec![0f32; (w * h) as usize];
    for y in (m + DROP).max(0)..(m + DROP + ih).min(h) {
        let row = (y * w) as usize;
        for x in m..m + iw { mask[row + x as usize] = 1.0; }
    }
    for _ in 0..3 {
        blur_rows(&mut mask, w, h, BLUR);
        blur_cols(&mut mask, w, h, BLUR);
    }
    let mut out: RgbaImage = ImageBuffer::from_fn(w as u32, h as u32, |x, y| {
        let a = mask[(y as i32 * w + x as i32) as usize] * SHADOW_ALPHA;
        Rgba([0, 0, 0, (a * 255.0).round().clamp(0.0, 255.0) as u8])
    });
    image::imageops::overlay(&mut out, img, m as i64, m as i64);
    out
}

/// Running-sum box blur along each row.
fn blur_rows(v: &mut [f32], w: i32, h: i32, r: i32) {
    let mut line = vec![0f32; w as usize];
    let n = (2 * r + 1) as f32;
    for y in 0..h {
        let row = (y * w) as usize;
        line.copy_from_slice(&v[row..row + w as usize]);
        let at = |x: i32| if x < 0 || x >= w { 0.0 } else { line[x as usize] };
        let mut sum: f32 = (-r..=r).map(at).sum();
        for x in 0..w {
            v[row + x as usize] = sum / n;
            sum += at(x + r + 1) - at(x - r);
        }
    }
}

/// Running-sum box blur along each column.
fn blur_cols(v: &mut [f32], w: i32, h: i32, r: i32) {
    let mut col = vec![0f32; h as usize];
    let n = (2 * r + 1) as f32;
    for x in 0..w {
        for y in 0..h { col[y as usize] = v[(y * w + x) as usize]; }
        let at = |y: i32| if y < 0 || y >= h { 0.0 } else { col[y as usize] };
        let mut sum: f32 = (-r..=r).map(at).sum();
        for y in 0..h {
            v[(y * w + x) as usize] = sum / n;
            sum += at(y + r + 1) - at(y - r);
        }
    }
}

/// `img` composited over white, fully opaque.
pub fn on_white(img: &RgbaImage) -> RgbaImage {
    ImageBuffer::from_fn(img.width(), img.height(), |x, y| {
        let Rgba([r, g, b, a]) = *img.get_pixel(x, y);
        let a = a as u32;
        let mix = |c: u8| ((c as u32 * a + 255 * (255 - a) + 127) / 255) as u8;
        Rgba([mix(r), mix(g), mix(b), 255])
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pic() -> RgbaImage { ImageBuffer::from_pixel(40, 30, Rgba([200, 30, 30, 255])) }

    #[test]
    fn none_leaves_the_picture_alone() {
        assert_eq!(apply(&pic(), OutputFrame::None), pic());
    }

    #[test]
    fn a_border_adds_two_grey_pixels_all_round() {
        let b = apply(&pic(), OutputFrame::Border);
        assert_eq!(b.dimensions(), (44, 34));
        assert_eq!(*b.get_pixel(0, 0), Rgba([0x80, 0x80, 0x80, 255]));
        assert_eq!(*b.get_pixel(1, 17), Rgba([0x80, 0x80, 0x80, 255]));
        assert_eq!(*b.get_pixel(2, 2), Rgba([200, 30, 30, 255]), "the picture starts inside the border");
    }

    #[test]
    fn a_shadow_adds_a_transparent_margin_darker_below() {
        let s = apply(&pic(), OutputFrame::Shadow);
        assert_eq!(s.dimensions(), (40 + 2 * MARGIN, 30 + 2 * MARGIN));
        assert_eq!(s.get_pixel(0, 0)[3], 0, "the corner is clear");
        assert_eq!(*s.get_pixel(MARGIN + 5, MARGIN + 5), Rgba([200, 30, 30, 255]), "the picture is untouched");
        let below = s.get_pixel(MARGIN + 20, MARGIN + 30 + 3)[3];
        let above = s.get_pixel(MARGIN + 20, MARGIN - 3)[3];
        assert!(below > above && below > 40, "the shadow falls below: {below} vs {above}");
    }

    #[test]
    fn the_clipboard_and_jpeg_get_the_shadow_on_white() {
        let c = for_clipboard(&pic(), OutputFrame::Shadow);
        assert_eq!(*c.get_pixel(0, 0), Rgba([255, 255, 255, 255]), "white, not black, where the margin was clear");
        assert!(c.pixels().all(|p| p[3] == 255));
        assert_eq!(for_clipboard(&pic(), OutputFrame::Border), apply(&pic(), OutputFrame::Border));
    }
}
