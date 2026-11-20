//! Text highlights.
//!
//! A highlight lies on the words like ink: on a light page it multiplies
//! (the paper takes the color, the letters stay dark), on a dark page it
//! screens (the page takes the color, light letters stay light). The hand-
//! made wobble comes from noise seeded by the highlight, so it is the same
//! every time it is drawn.

use framefang_core::color::Rgba as FramefangRgba;

use super::steps::sd_round_box;
use super::{blend, in_bounds, rgba_arr, Surface};

/// -1 to 1, the same for the same seed and step.
fn hash(seed: u32, i: i32) -> f32 {
    let mut x = seed ^ (i as u32).wrapping_mul(0x9E37_79B1);
    x ^= x >> 15;
    x = x.wrapping_mul(0x2C1B_3C6D);
    x ^= x >> 12;
    x = x.wrapping_mul(0x297A_2D39);
    x ^= x >> 15;
    (x as f32 / u32::MAX as f32) * 2.0 - 1.0
}

/// Smooth noise along `t`, -1 to 1.
fn wobble(seed: u32, t: f32) -> f32 {
    let i = t.floor();
    let f = t - i;
    let s = f * f * (3.0 - 2.0 * f);
    hash(seed, i as i32) * (1.0 - s) + hash(seed, i as i32 + 1) * s
}

/// Whether the page under `r` is light (dark letters on it), from a sparse
/// look at its pixels.
fn light_under<S: Surface>(s: &S, r: &framefang_core::annotation::Rect) -> bool {
    let (mut sum, mut n) = (0.0f32, 0);
    let (x0, y0) = (r.x.max(0.0) as i32, r.y.max(0.0) as i32);
    let (x1, y1) = (((r.x + r.w) as i32).min(s.width() - 1), ((r.y + r.h) as i32).min(s.height() - 1));
    let step = (((x1 - x0).max(1) * (y1 - y0).max(1)) as f32 / 400.0).sqrt().max(1.0) as usize;
    for y in (y0..=y1).step_by(step) {
        for x in (x0..=x1).step_by(step) {
            if !in_bounds(s, x, y) { continue; }
            let p = s.read(x, y);
            sum += 0.299 * p[0] as f32 + 0.587 * p[1] as f32 + 0.114 * p[2] as f32;
            n += 1;
        }
    }
    n == 0 || sum / n as f32 >= 110.0
}

/// `c` laid on the pixel like ink, `a` strong (0 to 1): multiplied on a
/// light page, screened on a dark one.
fn ink<S: Surface>(s: &mut S, x: i32, y: i32, c: FramefangRgba, a: f32, light: bool) {
    if !in_bounds(s, x, y) || a <= 0.003 { return; }
    let a = a.clamp(0.0, 1.0);
    let d = s.read(x, y);
    let mix = |dc: u8, cc: u8| {
        let (dc, cc) = (dc as f32, cc as f32);
        let on = if light { dc * cc / 255.0 } else { 255.0 - (255.0 - dc) * (255.0 - cc) / 255.0 };
        (dc * (1.0 - a) + on * a).round() as u8
    };
    s.write(x, y, [mix(d[0], c.r), mix(d[1], c.g), mix(d[2], c.b), 255]);
}

/// Coverage of a hand-drawn stroke over a box: the most any stamp gave.
struct Cover { x0: i32, y0: i32, w: i32, h: i32, a: Vec<f32> }

impl Cover {
    fn new((x0, y0, x1, y1): (i32, i32, i32, i32)) -> Cover {
        let (w, h) = ((x1 - x0 + 1).max(1), (y1 - y0 + 1).max(1));
        Cover { x0, y0, w, h, a: vec![0.0; (w * h) as usize] }
    }

    /// A round dab of radius `r` at (cx, cy), smooth at its edge.
    fn dab(&mut self, cx: f32, cy: f32, r: f32) {
        let (xa, xb) = ((cx - r - 1.0).floor() as i32, (cx + r + 1.0).ceil() as i32);
        let (ya, yb) = ((cy - r - 1.0).floor() as i32, (cy + r + 1.0).ceil() as i32);
        for y in ya.max(self.y0)..=yb.min(self.y0 + self.h - 1) {
            for x in xa.max(self.x0)..=xb.min(self.x0 + self.w - 1) {
                let d = ((x as f32 + 0.5 - cx).powi(2) + (y as f32 + 0.5 - cy).powi(2)).sqrt();
                let v = (r + 0.5 - d).clamp(0.0, 1.0);
                let i = ((y - self.y0) * self.w + (x - self.x0)) as usize;
                if v > self.a[i] { self.a[i] = v; }
            }
        }
    }

    /// A stroke along `pts`, its radius at each point from `radius(t)`
    /// (0 at the start, 1 at the end), dabbed every half pixel.
    fn trace(&mut self, pts: &[(f32, f32)], radius: &dyn Fn(f32) -> f32) {
        let total: f32 = pts.windows(2).map(|w| ((w[1].0 - w[0].0).powi(2) + (w[1].1 - w[0].1).powi(2)).sqrt()).sum();
        let mut gone = 0.0;
        for w in pts.windows(2) {
            let len = ((w[1].0 - w[0].0).powi(2) + (w[1].1 - w[0].1).powi(2)).sqrt();
            let n = (len / 0.5).ceil().max(1.0) as i32;
            for i in 0..=n {
                let f = i as f32 / n as f32;
                let t = if total > 0.0 { (gone + len * f) / total } else { 0.0 };
                self.dab(w[0].0 + (w[1].0 - w[0].0) * f, w[0].1 + (w[1].1 - w[0].1) * f, radius(t));
            }
            gone += len;
        }
    }

    fn paint<S: Surface>(&self, s: &mut S, c: FramefangRgba, alpha: f32) {
        let k = rgba_arr(c);
        for (i, v) in self.a.iter().enumerate() {
            if *v <= 0.0 { continue; }
            let (x, y) = (self.x0 + i as i32 % self.w, self.y0 + i as i32 / self.w);
            blend(s, x, y, [k[0], k[1], k[2], (k[3] as f32 * alpha * v).round() as u8]);
        }
    }
}

/// Words highlighted in `look`: one rectangle per line.
pub fn text_mark<S: Surface>(s: &mut S, rects: &[framefang_core::annotation::Rect], look: &framefang_core::mark::MarkLook) {
    use framefang_core::mark::MarkStyle;
    let c = look.color.with_alpha(255);
    if look.style == MarkStyle::Circle {
        circle_mark(s, rects, c, look.seed);
        return;
    }
    for (li, r) in rects.iter().enumerate() {
        let seed = look.seed.wrapping_add(li as u32 * 7919);
        let light = light_under(s, r);
        match look.style {
            MarkStyle::Marker => marker_band(s, r, c, seed, light),
            MarkStyle::Clean => {
                let d = sd_round_box(r.x + r.w / 2.0, r.y + r.h / 2.0, r.w / 2.0 + 2.5, r.h / 2.0 + 1.5, 3.5);
                sdf_ink(s, r, 5, &d, &|_, _| 0.5, c, light);
            }
            MarkStyle::Lower => {
                // Under the letters' bodies, along the line they sit on.
                let (top, bottom) = (r.y + r.h * 0.62, r.y + r.h + 2.0);
                let mid = (top + bottom) / 2.0;
                let half = (bottom - top) / 2.0;
                let d = move |x: f32, y: f32| {
                    let edge = wobble(seed ^ 0xA5, x / 19.0) * 0.6;
                    sd_round_box(r.x + r.w / 2.0, mid + edge, r.w / 2.0 + 2.5, half, half * 0.9)(x, y)
                };
                sdf_ink(s, r, 5, &d, &|_, _| 0.58, c, light);
            }
            MarkStyle::Glow => {
                let core = sd_round_box(r.x + r.w / 2.0, r.y + r.h / 2.0, r.w / 2.0 + 3.0, r.h / 2.0 + 2.0, 5.0);
                // The soft light around it first, then the wash.
                let (x0, y0) = ((r.x - 12.0) as i32, (r.y - 12.0) as i32);
                let (x1, y1) = ((r.x + r.w + 12.0) as i32, (r.y + r.h + 12.0) as i32);
                for y in y0..=y1 {
                    for x in x0..=x1 {
                        let d = core(x as f32 + 0.5, y as f32 + 0.5);
                        if d <= 0.0 { continue; }
                        let a = 0.42 * (-d / 3.5).exp();
                        if a > 0.01 { ink(s, x, y, c, a, light); }
                    }
                }
                sdf_ink(s, r, 5, &core, &|_, _| 0.5, c, light);
            }
            MarkStyle::Underline => {
                let y = r.y + r.h + 3.0;
                let (x0, x1) = (r.x - 2.0, r.x + r.w + 2.0);
                let n = ((x1 - x0) / 6.0).ceil().max(2.0) as usize;
                let pts: Vec<(f32, f32)> = (0..=n).map(|i| {
                    let x = x0 + (x1 - x0) * i as f32 / n as f32;
                    // A slight sag and a little wave, as a hand pulls a line.
                    let t = i as f32 / n as f32;
                    (x, y + wobble(seed, x / 23.0) * 0.9 + (t - 0.5).powi(2) * -2.0 + 0.5)
                }).collect();
                let mut cov = Cover::new(((x0 - 4.0) as i32, (y - 5.0) as i32, (x1 + 4.0) as i32, (y + 5.0) as i32));
                cov.trace(&pts, &|t| 1.45 - 0.45 * ((t - 0.5).abs() * 2.0).powi(3));
                cov.paint(s, c, 0.92);
            }
            MarkStyle::Circle => unreachable!(),
        }
    }
}

/// Fill the shape `d` around `r` (out to `m` past it) with ink, `alpha` at
/// each point.
#[allow(clippy::too_many_arguments)]
fn sdf_ink<S: Surface>(s: &mut S, r: &framefang_core::annotation::Rect, m: i32, d: &dyn Fn(f32, f32) -> f32,
                       alpha: &dyn Fn(f32, f32) -> f32, c: FramefangRgba, light: bool) {
    for y in (r.y as i32 - m)..=((r.y + r.h) as i32 + m) {
        for x in (r.x as i32 - m)..=((r.x + r.w) as i32 + m) {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            let cov = (0.5 - d(fx, fy)).clamp(0.0, 1.0);
            if cov > 0.0 { ink(s, x, y, c, alpha(fx, fy) * cov, light); }
        }
    }
}

/// A chisel highlighter's pass over one line: its edges a little uneven,
/// its ends slanted the way the tip meets the page, the ink heavier where
/// the stroke starts and stops and along its edges, faint streaks along it.
fn marker_band<S: Surface>(s: &mut S, r: &framefang_core::annotation::Rect, c: FramefangRgba, seed: u32, light: bool) {
    const SLANT: f32 = 3.5;
    let (x0, x1) = (r.x - 3.0, r.x + r.w + 3.0);
    let (y0, y1) = (r.y - 1.5, r.y + r.h + 1.0);
    let h = y1 - y0;
    let tilt = hash(seed, 99) * 1.1;
    let top = |x: f32| y0 + wobble(seed, x / 21.0) * 0.8 + tilt * (x - x0) / (x1 - x0).max(1.0);
    let bottom = |x: f32| y1 + wobble(seed ^ 0x5bd1, x / 17.0) * 0.8 + tilt * (x - x0) / (x1 - x0).max(1.0);
    for y in (y0 - 3.0) as i32..=(y1 + 3.0) as i32 {
        for x in (x0 - 1.0) as i32..=(x1 + 1.0) as i32 {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            let (t, b) = (top(fx), bottom(fx));
            let v = ((fy - t) / h.max(1.0)).clamp(0.0, 1.0);
            // The tip's slant: the stroke's ends lean.
            let (l, rr) = (x0 + SLANT * (1.0 - v), x1 - SLANT * v);
            let cov = (fy - t + 0.5).clamp(0.0, 1.0) * (b - fy + 0.5).clamp(0.0, 1.0)
                * (fx - l + 0.5).clamp(0.0, 1.0) * (rr - fx + 0.5).clamp(0.0, 1.0);
            if cov <= 0.0 { continue; }
            let from_end = (fx - l).min(rr - fx).max(0.0);
            let from_edge = (fy - t).min(b - fy).max(0.0);
            let pool = 0.2 * (1.0 - (from_end / 6.0).min(1.0)).powi(2);
            let rim = 0.08 * (1.0 - (from_edge / 1.6).min(1.0));
            let streak = 1.0 + 0.07 * wobble(seed ^ 0x2f, fy * 0.85) + 0.03 * wobble(seed ^ 0x77, fx / 9.0);
            ink(s, x, y, c, ((0.5 + pool + rim) * streak) * cov, light);
        }
    }
}

/// A loose loop around all the lines, drawn in one go with a pen: a rounded
/// box that hugs the words (an ellipse cuts their corners), its line
/// wandering a little, going a bit more than once round so its ends cross.
fn circle_mark<S: Surface>(s: &mut S, rects: &[framefang_core::annotation::Rect], c: FramefangRgba, seed: u32) {
    let b = framefang_core::mark::bounds(rects, framefang_core::mark::MarkStyle::Clean);
    if b.w <= 0.0 { return; }
    let (hw, hh) = (b.w / 2.0 + 5.0, b.h / 2.0 + 4.0);
    let (cx, cy) = (b.x + b.w / 2.0, b.y + b.h / 2.0);
    let r = hh.min(22.0);
    // The perimeter as a function of distance along it, clockwise from the
    // top-left corner's end.
    let (sx, sy) = (2.0 * (hw - r), 2.0 * (hh - r));
    let arc = std::f32::consts::FRAC_PI_2 * r;
    let total = 2.0 * (sx + sy) + 4.0 * arc;
    let at = |d: f32| -> ((f32, f32), (f32, f32)) {
        let d = d.rem_euclid(total);
        // Each piece: (length, point at f along it, outward normal).
        let corner = |k: usize, f: f32| {
            let (ccx, ccy, a0) = match k {
                0 => (cx + hw - r, cy - hh + r, -std::f32::consts::FRAC_PI_2),
                1 => (cx + hw - r, cy + hh - r, 0.0),
                2 => (cx - hw + r, cy + hh - r, std::f32::consts::FRAC_PI_2),
                _ => (cx - hw + r, cy - hh + r, std::f32::consts::PI),
            };
            let a = a0 + f * std::f32::consts::FRAC_PI_2;
            ((ccx + r * a.cos(), ccy + r * a.sin()), (a.cos(), a.sin()))
        };
        let mut d = d;
        let sides = [(sx, (cx - hw + r, cy - hh), (1.0, 0.0), (0.0, -1.0)),
                     (sy, (cx + hw, cy - hh + r), (0.0, 1.0), (1.0, 0.0)),
                     (sx, (cx + hw - r, cy + hh), (-1.0, 0.0), (0.0, 1.0)),
                     (sy, (cx - hw, cy + hh - r), (0.0, -1.0), (-1.0, 0.0))];
        for (k, (len, from, dir, normal)) in sides.into_iter().enumerate() {
            if d <= len { return ((from.0 + dir.0 * d, from.1 + dir.1 * d), normal); }
            d -= len;
            if d <= arc { return corner(k, d / arc.max(1e-3)); }
            d -= arc;
        }
        corner(3, 1.0)
    };
    // Start a little way along the top, go round once and a short way more.
    let start = total * (0.18 + 0.08 * hash(seed, 5));
    let length = total * 1.035;
    let n = (length / 4.0).ceil().max(40.0) as usize;
    let pts: Vec<(f32, f32)> = (0..=n).map(|i| {
        let t = i as f32 / n as f32;
        let ((x, y), (nx, ny)) = at(start + length * t);
        // It wanders in and out, and ends a touch further out than it began.
        let out = wobble(seed, t * 7.0) * 1.4 + 2.2 * t - 0.6;
        (x + nx * out, y + ny * out)
    }).collect();
    let mut cov = Cover::new(((cx - hw - 10.0) as i32, (cy - hh - 10.0) as i32, (cx + hw + 10.0) as i32, (cy + hh + 10.0) as i32));
    cov.trace(&pts, &|t| 1.35 - 0.5 * ((t - 0.5).abs() * 2.0).powi(4));
    cov.paint(s, c, 0.92);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::painter::{draw_unicode_text, ImageSurface};
    use framefang_core::color::Rgba as K;

    /// A light or dark page with two lines of text, and the lines' boxes.
    fn text_page(light: bool) -> (image::RgbaImage, Vec<framefang_core::annotation::Rect>) {
        let (bg, fg) = if light { ([250u8, 250, 247], K::new_opaque(28, 28, 30)) } else { ([30u8, 31, 36], K::new_opaque(228, 228, 232)) };
        let mut img = image::RgbaImage::from_pixel(360, 90, image::Rgba([bg[0], bg[1], bg[2], 255]));
        let mut rects = Vec::new();
        for (i, words) in ["Highlights that sit right on", "the words, line by line."].iter().enumerate() {
            let (x, y, px) = (16.0, 14.0 + i as f32 * 34.0, 20.0);
            draw_unicode_text(&mut ImageSurface(&mut img), x, y, px, words, fg);
            let (w, h) = framefang_core::text::measure(words, px);
            rects.push(framefang_core::annotation::Rect { x, y: y + 2.0, w, h: h - 4.0 });
        }
        (img, rects)
    }

    #[test]
    fn a_highlight_keeps_the_letters_readable_on_light_and_dark_pages() {
        use framefang_core::mark::{MarkLook, MarkStyle};
        for light in [true, false] {
            let (mut img, rects) = text_page(light);
            let before = img.clone();
            let look = MarkLook { style: MarkStyle::Marker, color: K::new_opaque(255, 226, 52), seed: 1 };
            text_mark(&mut ImageSurface(&mut img), &rects, &look);
            // The page under the words took the color...
            let (bx, by) = (rects[0].x as u32 + 1, rects[0].y as u32 + 1);
            assert_ne!(img.get_pixel(bx, by), before.get_pixel(bx, by), "light {light}: the page is colored");
            // ...and the letters still stand apart from it as much as before, nearly.
            let contrast = |im: &image::RgbaImage| {
                let (mut lo, mut hi) = (255u8, 0u8);
                for x in rects[0].x as u32..(rects[0].x + rects[0].w) as u32 {
                    for y in rects[0].y as u32..(rects[0].y + rects[0].h) as u32 {
                        let p = im.get_pixel(x, y);
                        let l = ((p[0] as u32 * 3 + p[1] as u32 * 6 + p[2] as u32) / 10) as u8;
                        lo = lo.min(l); hi = hi.max(l);
                    }
                }
                hi - lo
            };
            assert!(contrast(&img) as f32 >= contrast(&before) as f32 * 0.6, "light {light}: the words still read");
        }
    }

    /// Writes target/text-marks.png: every highlight look on a light and a
    /// dark page.
    #[test]
    #[ignore]
    fn render_text_marks_to_png() {
        use framefang_core::mark::{MarkLook, MarkStyle};
        let colors = [K::new_opaque(255, 226, 52), K::new_opaque(120, 230, 130), K::new_opaque(255, 120, 190),
                      K::new_opaque(90, 200, 255), K::new_opaque(255, 150, 60), K::new_opaque(190, 140, 255)];
        let (pw, ph) = (360u32, 90u32);
        let mut out = image::RgbaImage::new(pw * 2, ph * MarkStyle::ALL.len() as u32);
        for (i, style) in MarkStyle::ALL.into_iter().enumerate() {
            for (j, light) in [true, false].into_iter().enumerate() {
                let (mut img, rects) = text_page(light);
                let look = MarkLook { style, color: colors[i], seed: 7 + i as u32 };
                text_mark(&mut ImageSurface(&mut img), &rects, &look);
                image::imageops::overlay(&mut out, &img, (j as u32 * pw) as i64, (i as u32 * ph) as i64);
            }
        }
        let big = image::imageops::resize(&out, out.width() * 2, out.height() * 2, image::imageops::FilterType::Nearest);
        big.save(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/text-marks.png")).unwrap();
    }
}
