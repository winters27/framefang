//! Pixel primitives for the editor's chrome, on a 0RGB `u32` framebuffer.
//!
//! Every shape is clipped to the buffer. Rectangles take their corners as
//! `x0, y0` (inclusive) to `x1, y1` (exclusive) unless a function says
//! otherwise; an outline whose rectangle runs off the buffer is drawn along
//! the buffer's edge instead, so a frame cut by the screen still reads as a
//! frame.

use framefang_core::color::Rgba as Color;

use crate::painter::U32Surface;

/// A mutable view of a framebuffer: its pixels, row length and row count.
pub(super) struct Canvas<'a> {
    pub px: &'a mut [u32],
    pub w: usize,
    pub h: usize,
}

impl<'a> Canvas<'a> {
    pub fn new(px: &'a mut [u32], w: usize, h: usize) -> Self { Canvas { px, w, h } }

    /// The painter's view of these pixels, for text, numbers and shapes.
    pub fn surface(&mut self) -> U32Surface<'_> {
        U32Surface { buf: &mut *self.px, stride: self.w as i32, height: self.h as i32 }
    }

    fn index(&self, x: i32, y: i32) -> Option<usize> {
        let inside = x >= 0 && y >= 0 && (x as usize) < self.w && (y as usize) < self.h;
        inside.then(|| y as usize * self.w + x as usize)
    }

    /// One pixel, ignored off the buffer.
    pub fn set(&mut self, x: i32, y: i32, rgb: u32) {
        if let Some(i) = self.index(x, y) { self.px[i] = rgb; }
    }

    /// The part of a rectangle on the buffer, as corners, or `None` when no
    /// pixel of it is.
    fn visible(&self, x0: i32, y0: i32, x1: i32, y1: i32) -> Option<(usize, usize, usize, usize)> {
        let (w, h) = (self.w as i32, self.h as i32);
        let (xa, xb) = (x0.clamp(0, w), x1.clamp(0, w));
        let (ya, yb) = (y0.clamp(0, h), y1.clamp(0, h));
        (xb > xa && yb > ya).then_some((xa as usize, ya as usize, xb as usize, yb as usize))
    }

    pub fn fill(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, rgb: u32) {
        let Some((xa, ya, xb, yb)) = self.visible(x0, y0, x1, y1) else { return };
        for row in self.px.chunks_exact_mut(self.w).take(yb).skip(ya) {
            row[xa..xb].fill(rgb);
        }
    }

    /// `fill` for an `(x, y, w, h)` rectangle.
    pub fn fill_rect(&mut self, (x, y, w, h): (i32, i32, i32, i32), rgb: u32) {
        self.fill(x, y, x + w, y + h, rgb);
    }

    /// A 1 px frame just inside the visible part of the rectangle.
    pub fn outline(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, rgb: u32) {
        self.frame(x0, y0, x1, y1, rgb, |_| true);
    }

    /// `outline`, three pixels on and three off, counted along the buffer
    /// so neighbouring dashed frames line up.
    pub fn dashed(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, rgb: u32) {
        self.frame(x0, y0, x1, y1, rgb, |i| (i / 3) % 2 == 0);
    }

    fn frame(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, rgb: u32, on: impl Fn(usize) -> bool) {
        let Some((xa, ya, xb, yb)) = self.visible(x0, y0, x1, y1) else { return };
        let w = self.w;
        for x in (xa..xb).filter(|&x| on(x)) {
            self.px[ya * w + x] = rgb;
            self.px[(yb - 1) * w + x] = rgb;
        }
        for y in (ya..yb).filter(|&y| on(y)) {
            self.px[y * w + xa] = rgb;
            self.px[y * w + xb - 1] = rgb;
        }
    }

    /// Lay `rgb` at opacity `a` (0 to 255) over one pixel.
    pub fn blend(&mut self, x: i32, y: i32, rgb: u32, a: u32) {
        let Some(i) = self.index(x, y) else { return };
        let under = self.px[i];
        let mix = |shift: u32| {
            let (top, bottom) = ((rgb >> shift) & 0xFF, (under >> shift) & 0xFF);
            ((top * a + bottom * (255 - a) + 127) / 255).min(255) << shift
        };
        self.px[i] = mix(16) | mix(8) | mix(0);
    }

    /// A 1 px ring laid over the picture at opacity `a`, corners included
    /// (`x1`, `y1` inclusive). The rows and the columns are blended in two
    /// passes, so each corner pixel takes the color twice and glows a touch
    /// brighter, which is how the glow and the pulse have always looked.
    pub fn blend_ring(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, rgb: u32, a: u32) {
        if a == 0 { return; }
        for x in x0..=x1 {
            self.blend(x, y0, rgb, a);
            self.blend(x, y1, rgb, a);
        }
        for y in y0..=y1 {
            self.blend(x0, y, rgb, a);
            self.blend(x1, y, rgb, a);
        }
    }

    /// A 1 px line from `a` to `b`, both ends included (Bresenham).
    pub fn line(&mut self, a: (i32, i32), b: (i32, i32), rgb: u32) {
        let (mut x, mut y) = a;
        let (run, rise) = ((b.0 - x).abs(), -(b.1 - y).abs());
        let (sx, sy) = (if b.0 > x { 1 } else { -1 }, if b.1 > y { 1 } else { -1 });
        let mut err = run + rise;
        loop {
            self.set(x, y, rgb);
            if (x, y) == b { break; }
            let twice = 2 * err;
            if twice >= rise { err += rise; x += sx; }
            if twice <= run { err += run; y += sy; }
        }
    }

    /// `>` (or `<` when `left`) centered in the rectangle: the palette arrows.
    pub fn chevron(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, left: bool) {
        const INK: u32 = 0x00_E8_E8_EC;
        let (cx, cy) = ((x0 + x1) / 2, (y0 + y1) / 2);
        let (tail, tip) = if left { (cx + 4, cx - 4) } else { (cx - 4, cx + 4) };
        self.line((tail, cy - 6), (tip, cy), INK);
        self.line((tip, cy), (tail, cy + 6), INK);
    }
}

/// `dst` moved toward `src` by `a` (0 to 1), channel by channel.
pub(super) fn mix_rgb(dst: u32, src: u32, a: f32) -> u32 {
    let a = a.clamp(0.0, 1.0);
    [16u32, 8, 0].into_iter().fold(0, |out, shift| {
        let (from, to) = (((dst >> shift) & 0xFF) as f32, ((src >> shift) & 0xFF) as f32);
        out | (((from + (to - from) * a).round() as u32) << shift)
    })
}

/// A color as the framebuffer's 0RGB word, alpha dropped.
pub(super) fn word(c: Color) -> u32 { (u32::from(c.r) << 16) | (u32::from(c.g) << 8) | u32::from(c.b) }

/// A framebuffer word as an opaque color.
pub(super) fn color(rgb: u32) -> Color { Color::new_opaque((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8) }

#[cfg(test)]
mod tests {
    use super::*;

    fn blank(w: usize, h: usize) -> Vec<u32> { vec![0; w * h] }

    #[test]
    fn a_frame_cut_by_the_buffer_runs_along_its_edge() {
        let mut px = blank(10, 10);
        Canvas::new(&mut px, 10, 10).outline(-5, 2, 4, 6, 7);
        // The left side falls off the buffer, so column 0 carries it.
        assert_eq!(px[2 * 10], 7);
        assert_eq!(px[5 * 10 + 3], 7);
        assert_eq!(px[3 * 10 + 2], 0, "the inside stays clear");
        // Nothing at all for a rectangle entirely off the buffer.
        let mut px = blank(10, 10);
        Canvas::new(&mut px, 10, 10).outline(12, 0, 20, 5, 7);
        assert!(px.iter().all(|&p| p == 0));
    }

    #[test]
    fn fills_clip_and_inverted_rectangles_draw_nothing() {
        let mut px = blank(6, 4);
        let mut c = Canvas::new(&mut px, 6, 4);
        c.fill(4, 2, 99, 99, 1);
        c.fill(3, 3, 1, 1, 2);
        assert_eq!(px.iter().filter(|&&p| p == 1).count(), 4);
        assert!(!px.contains(&2));
    }

    #[test]
    fn dashes_go_three_on_three_off() {
        let mut px = blank(12, 3);
        Canvas::new(&mut px, 12, 3).dashed(0, 0, 12, 3, 9);
        let top: Vec<bool> = px[..12].iter().map(|&p| p == 9).collect();
        assert_eq!(&top[..6], &[true, true, true, false, false, false]);
    }

    #[test]
    fn a_ring_brightens_its_corners_twice() {
        let mut px = blank(5, 5);
        Canvas::new(&mut px, 5, 5).blend_ring(1, 1, 3, 3, 0x00_FF_FF_FF, 128);
        let (corner, side) = (px[5 + 1] & 0xFF, px[5 + 2] & 0xFF);
        assert_eq!(side, 128);
        assert!(corner > side, "corner {corner} side {side}");
    }

    #[test]
    fn lines_include_both_ends() {
        let mut px = blank(9, 9);
        Canvas::new(&mut px, 9, 9).line((1, 7), (7, 1), 5);
        assert_eq!(px[7 * 9 + 1], 5);
        assert_eq!(px[9 + 7], 5);
        assert_eq!(px.iter().filter(|&&p| p == 5).count(), 7);
    }

    #[test]
    fn colors_round_trip_through_the_framebuffer_word() {
        let c = Color::new_opaque(0x12, 0x34, 0x56);
        assert_eq!(word(c), 0x00_12_34_56);
        assert_eq!(color(word(c)), c);
        assert_eq!(mix_rgb(0, 0x00_FF_FF_FF, 0.5), 0x00_80_80_80);
        assert_eq!(mix_rgb(0x00_10_20_30, 0, 2.0), 0);
    }
}
