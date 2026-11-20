//! Text and the other small round things.
//!
//! There are two kinds of text. The chrome (button labels, the size chip,
//! the Terminal step style) uses the 5x7 pixel font in `bitmap_font`, drawn
//! as hard squares on purpose. Text annotations, which hold whatever the
//! person typed in any script, use the bundled typeface laid out by
//! `framefang_core::text` and drawn with smooth edges.

use framefang_core::color::Rgba as FramefangRgba;
use framefang_core::text::{TextBlock, TextRect};

use super::{blend, rgba_arr, Surface};
use crate::bitmap_font::{self, GLYPH_W};

/// Draws `n` with the pixel font, its top left at (x, y). Numbers above
/// 999 are drawn as 999, since nothing that shows one has room for more.
///
/// The pixel font's digits are the ones this used to carry in its own
/// table, so this is the same as [`draw_text`] on the decimal string.
pub fn draw_number<S: Surface>(s: &mut S, x: i32, y: i32, scale: i32, n: u32, color: FramefangRgba) {
    draw_text(s, x, y, scale, &n.min(999).to_string(), color);
}

/// A solid disc of radius `r` around (cx, cy): every pixel whose offset
/// from the centre is at most `r` away. Nothing for `r <= 0`.
pub fn fill_disc<S: Surface>(s: &mut S, cx: i32, cy: i32, r: i32, color: FramefangRgba) {
    if r <= 0 { return; }
    let rgba = rgba_arr(color);
    for dy in -r..=r {
        let reach = int_sqrt(r * r - dy * dy);
        for dx in -reach..=reach {
            blend(s, cx + dx, cy + dy, rgba);
        }
    }
}

/// The largest `w` with `w * w <= v`, for `v >= 0`.
fn int_sqrt(v: i32) -> i32 {
    let mut w = (v as f64).sqrt() as i32;
    while w * w > v { w -= 1; }
    while (w + 1) * (w + 1) <= v { w += 1; }
    w
}

/// Draws `text` with the 5x7 pixel font, its top left at (x, y), every
/// font pixel a `scale` by `scale` square. Characters sit `GLYPH_W + 1`
/// font pixels apart, so at scale 2 each is 10 by 14 with a 2 pixel gap.
/// Characters the font lacks come out as `?`.
pub fn draw_text<S: Surface>(s: &mut S, x: i32, y: i32, scale: i32, text: &str, color: FramefangRgba) {
    let rgba = rgba_arr(color);
    let advance = (GLYPH_W + 1) * scale;
    for (i, ch) in text.chars().enumerate() {
        let left = x + i as i32 * advance;
        for (row, bits) in bitmap_font::glyph(ch).iter().enumerate() {
            let top = y + row as i32 * scale;
            // Bit 4 is the leftmost column.
            for col in (0..GLYPH_W).filter(|col| bits & (0b10000 >> col) != 0) {
                fill_square(s, left + col * scale, top, scale, rgba);
            }
        }
    }
}

/// One pixel of the pixel font, `size` real pixels on a side.
fn fill_square<S: Surface>(s: &mut S, x: i32, y: i32, size: i32, rgba: [u8; 4]) {
    for py in y..y + size {
        for px in x..x + size {
            blend(s, px, py, rgba);
        }
    }
}

/// Draws a Text annotation's `text` in the bundled typeface at `px` pixels
/// to the em, the block's top left at (x, y) rounded to whole pixels.
///
/// Each glyph pixel's coverage scales the color's own alpha and goes
/// through `blend` like every other shape, so the video burn-in's
/// recording surface sees exactly the pixels the editor showed.
pub fn draw_unicode_text<S: Surface>(s: &mut S, x: f32, y: f32, px: f32, text: &str, color: FramefangRgba) {
    if text.is_empty() { return; }
    let block = framefang_core::text::layout(text, px);
    blit_text_block(s, x.round() as i32, y.round() as i32, &block, color);
}

/// Draws a block that has already been laid out, its top left at
/// (ox, oy). The editor lays out the text it is editing once to size the
/// typing frame and hands the same block here rather than laying it out
/// again.
pub fn blit_text_block<S: Surface>(s: &mut S, ox: i32, oy: i32, block: &TextBlock, color: FramefangRgba) {
    let [r, g, b, alpha] = rgba_arr(color);
    if alpha == 0 { return; }
    // The surface, in the block's own coordinates. Glyphs wholly outside it
    // are skipped as a whole; `blend` still clips the ones that straddle.
    let on_surface = TextRect::new(-ox as f32, -oy as f32, s.width() as f32, s.height() as f32);
    for glyph in block.visible_glyphs(on_surface) {
        if glyph.w == 0 { continue; }
        let rows = glyph.coverage.chunks_exact(glyph.w).take(glyph.h);
        for (row, line) in rows.enumerate() {
            let y = oy + glyph.y + row as i32;
            for (col, &cover) in line.iter().enumerate().filter(|(_, c)| **c != 0) {
                let a = ((alpha as u32 * cover as u32 + 127) / 255) as u8;
                blend(s, ox + glyph.x + col as i32, y, [r, g, b, a]);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::painter::U32Surface;
    use framefang_core::color::Rgba as K;

    fn frame(w: i32, h: i32) -> Vec<u32> { vec![0; (w * h) as usize] }

    #[test]
    fn numbers_draw_exactly_like_their_digits_as_text() {
        for (n, shown) in [(0, "0"), (407, "407"), (12_345, "999")] {
            let (mut a, mut b) = (frame(80, 20), frame(80, 20));
            draw_number(&mut U32Surface { buf: &mut a, stride: 80, height: 20 }, 1, 2, 2, n, K::WHITE);
            draw_text(&mut U32Surface { buf: &mut b, stride: 80, height: 20 }, 1, 2, 2, shown, K::WHITE);
            assert_eq!(a, b, "{n}");
            assert!(a.iter().any(|&p| p != 0));
        }
    }

    #[test]
    fn pixel_text_places_each_character_one_cell_apart() {
        // "1" at scale 1: its stem is column 2 of the cell, so in a two
        // character string the second stem sits GLYPH_W + 1 pixels later.
        let mut px = frame(20, 8);
        draw_text(&mut U32Surface { buf: &mut px, stride: 20, height: 8 }, 0, 0, 1, "11", K::WHITE);
        let row3: Vec<usize> = (0..20).filter(|&x| px[3 * 20 + x] != 0).collect();
        assert_eq!(row3, vec![2, 2 + (GLYPH_W as usize + 1)]);
    }

    #[test]
    fn a_disc_covers_exactly_the_pixels_within_its_radius() {
        let mut px = frame(21, 21);
        fill_disc(&mut U32Surface { buf: &mut px, stride: 21, height: 21 }, 10, 10, 6, K::WHITE);
        for y in 0..21i32 {
            for x in 0..21i32 {
                let inside = (x - 10).pow(2) + (y - 10).pow(2) <= 36;
                assert_eq!(px[(y * 21 + x) as usize] != 0, inside, "({x}, {y})");
            }
        }
        // Radius 0 draws nothing at all.
        let mut none = frame(3, 3);
        fill_disc(&mut U32Surface { buf: &mut none, stride: 3, height: 3 }, 1, 1, 0, K::WHITE);
        assert!(none.iter().all(|&p| p == 0));
    }

    #[test]
    fn typed_text_draws_scripts_beyond_ascii() {
        // The pixel font would have turned each of these into `?` boxes.
        for sample in ["Grüße", "Привет", "50 % ± ½"] {
            let mut px = frame(400, 120);
            draw_unicode_text(&mut U32Surface { buf: &mut px, stride: 400, height: 120 }, 10.0, 10.0, 32.0, sample, K::WHITE);
            assert!(px.iter().any(|&p| p != 0), "nothing drawn for {sample}");
        }
    }

    #[test]
    fn typed_text_has_soft_edges() {
        let mut px = frame(300, 100);
        draw_unicode_text(&mut U32Surface { buf: &mut px, stride: 300, height: 100 }, 10.0, 10.0, 48.0, "Sé", K::WHITE);
        let in_between = px.iter().filter(|&&p| p != 0 && p != 0x00FF_FFFF).count();
        assert!(in_between > 0, "every pixel was either fully on or off");
    }

    #[test]
    fn typed_text_off_the_surface_is_skipped_and_text_across_its_edge_is_cut() {
        let mut px = frame(64, 64);
        for at in [-500.0, 500.0] {
            draw_unicode_text(&mut U32Surface { buf: &mut px, stride: 64, height: 64 }, at, at, 32.0, "clipped", K::WHITE);
        }
        assert!(px.iter().all(|&p| p == 0), "text far outside left a mark");
        draw_unicode_text(&mut U32Surface { buf: &mut px, stride: 64, height: 64 }, -10.0, 20.0, 32.0, "clipped", K::WHITE);
        assert!(px.iter().any(|&p| p != 0), "the part inside was not drawn");
    }
}
