//! Numbered step markers.
//!
//! Every shape is a signed distance (pixels, negative inside), so edges are
//! smooth, a ring is the same shape grown, and a shadow is the shape blurred.
//! The sizes come from `framefang_core::step`, which hit-testing uses too.

use framefang_core::annotation::Point2;
use framefang_core::color::Rgba as FramefangRgba;

use super::{blend, draw_number, rgba_arr, Surface};

/// A box around (cx, cy) reaching `l, t, r, b` out, in whole pixels.
fn sdf_box(cx: f32, cy: f32, (l, t, r, b): (f32, f32, f32, f32)) -> (i32, i32, i32, i32) {
    ((cx + l).floor() as i32 - 1, (cy + t).floor() as i32 - 1, (cx + r).ceil() as i32 + 1, (cy + b).ceil() as i32 + 1)
}

/// Paint `color` where `d` is inside, smoothing the edge over one pixel.
fn fill_sdf<S: Surface>(s: &mut S, (x0, y0, x1, y1): (i32, i32, i32, i32), color: FramefangRgba, d: &dyn Fn(f32, f32) -> f32) {
    let c = rgba_arr(color);
    if c[3] == 0 { return; }
    for y in y0..=y1 {
        for x in x0..=x1 {
            let cov = (0.5 - d(x as f32 + 0.5, y as f32 + 0.5)).clamp(0.0, 1.0);
            if cov <= 0.0 { continue; }
            blend(s, x, y, [c[0], c[1], c[2], (c[3] as f32 * cov).round() as u8]);
        }
    }
}

/// A soft shadow of the shape `d`, a little below it.
fn shadow_sdf<S: Surface>(s: &mut S, (x0, y0, x1, y1): (i32, i32, i32, i32), d: &dyn Fn(f32, f32) -> f32) {
    const DROP: f32 = 1.5;
    const SOFT: f32 = 3.0;
    const DARK: f32 = 0.42;
    for y in y0 - 1..=y1 + (SOFT + DROP) as i32 + 1 {
        for x in x0 - 2..=x1 + 2 {
            let t = ((SOFT - d(x as f32 + 0.5, y as f32 + 0.5 - DROP)) / (2.0 * SOFT)).clamp(0.0, 1.0);
            let a = DARK * t * t * (3.0 - 2.0 * t);
            if a <= 0.004 { continue; }
            blend(s, x, y, [0, 0, 0, (a * 255.0).round() as u8]);
        }
    }
}

fn sd_circle(cx: f32, cy: f32, r: f32) -> impl Fn(f32, f32) -> f32 {
    move |x, y| ((x - cx).powi(2) + (y - cy).powi(2)).sqrt() - r
}

/// A box `hw` by `hh` from its middle, corners rounded by `rad`.
pub(super) fn sd_round_box(cx: f32, cy: f32, hw: f32, hh: f32, rad: f32) -> impl Fn(f32, f32) -> f32 {
    move |x, y| {
        let qx = (x - cx).abs() - hw + rad;
        let qy = (y - cy).abs() - hh + rad;
        (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt() + qx.max(qy).min(0.0) - rad
    }
}

/// A map pin: a round head of radius `r` at (cx, hy), drawn down to a
/// point at (cx, ty) along the lines that just touch the head.
fn sd_pin(cx: f32, hy: f32, r: f32, ty: f32) -> impl Fn(f32, f32) -> f32 {
    let dist = (ty - hy).max(r + 1.0);
    // Where the lines from the tip touch the head.
    let py = r * r / dist;
    let px = (r * r - py * py).max(0.0).sqrt();
    let (tip, left, right) = ((cx, ty), (cx - px, hy + py), (cx + px, hy + py));
    // Signed distance to the triangle tip-left-right (edges outward).
    let edge = |a: (f32, f32), b: (f32, f32), x: f32, y: f32| {
        let (ex, ey) = (b.0 - a.0, b.1 - a.1);
        let len = (ex * ex + ey * ey).sqrt().max(1e-3);
        ((x - a.0) * ey - (y - a.1) * ex) / len
    };
    move |x, y| {
        let tri = edge(tip, left, x, y).max(edge(left, right, x, y)).max(edge(right, tip, x, y));
        let head = ((x - cx).powi(2) + (y - hy).powi(2)).sqrt() - r;
        head.min(tri)
    }
}

/// The digits, bold and smooth, their ink centered on (cx, cy).
fn step_digits<S: Surface>(s: &mut S, cx: f32, cy: f32, number: u32, px: f32, color: FramefangRgba) {
    let text = number.min(999).to_string();
    let block = framefang_core::text::layout(&text, px);
    let clip = framefang_core::text::TextRect::new(-10_000.0, -10_000.0, 20_000.0, 20_000.0);
    let glyphs: Vec<_> = block.visible_glyphs(clip).collect();
    if glyphs.is_empty() { return; }
    let gx0 = glyphs.iter().map(|g| g.x).min().unwrap_or(0);
    let gy0 = glyphs.iter().map(|g| g.y).min().unwrap_or(0);
    let gx1 = glyphs.iter().map(|g| g.x + g.w as i32).max().unwrap_or(0);
    let gy1 = glyphs.iter().map(|g| g.y + g.h as i32).max().unwrap_or(0);
    // One pixel wider for the bolding.
    let (w, h) = ((gx1 - gx0 + 1).max(1) as usize, (gy1 - gy0).max(1) as usize);
    let mut cov = vec![0u8; w * h];
    for g in glyphs {
        for row in 0..g.h {
            for col in 0..g.w {
                let i = (g.y - gy0) as usize + row;
                let j = (g.x - gx0) as usize + col;
                let v = g.coverage[row * g.w + col];
                if v > cov[i * w + j] { cov[i * w + j] = v; }
            }
        }
    }
    // Bold: each pixel also takes most of its left neighbour's ink.
    for i in 0..h {
        for j in (1..w).rev() {
            let left = (cov[i * w + j - 1] as f32 * 0.9) as u8;
            if left > cov[i * w + j] { cov[i * w + j] = left; }
        }
    }
    // Center the ink itself, not the text box.
    let inked = |v: &u8| *v > 60;
    let rows: Vec<usize> = (0..h).filter(|&i| cov[i * w..(i + 1) * w].iter().any(inked)).collect();
    let cols: Vec<usize> = (0..w).filter(|&j| (0..h).any(|i| inked(&cov[i * w + j]))).collect();
    let (Some(&r0), Some(&r1), Some(&c0), Some(&c1)) = (rows.first(), rows.last(), cols.first(), cols.last()) else { return; };
    let ox = (cx - (c0 + c1 + 1) as f32 / 2.0).round() as i32;
    let oy = (cy - (r0 + r1 + 1) as f32 / 2.0).round() as i32;
    let c = rgba_arr(color);
    for i in 0..h {
        for j in 0..w {
            let v = cov[i * w + j];
            if v == 0 { continue; }
            let a = ((c[3] as u32 * v as u32 + 127) / 255) as u8;
            blend(s, ox + j as i32, oy + i as i32, [c[0], c[1], c[2], a]);
        }
    }
}

/// A numbered step marker in `look`, placed by its anchor (its middle, or a
/// pin's tip).
pub fn step_marker<S: Surface>(s: &mut S, center: Point2, number: u32, look: &framefang_core::step::StepLook) {
    use framefang_core::step::{self as st, StepShape};
    let (cx, cy) = (center.x, center.y);
    let r = st::RADIUS;
    let area = sdf_box(cx, cy, st::extent(number, look));
    let digits = number.min(999).to_string().len();
    let px = st::digit_px(digits, look.shape);
    // Where the digits go: a pin's head.
    let mut middle = (cx, cy);
    match look.shape {
        StepShape::Disc => {
            let body = sd_circle(cx, cy, r);
            if look.shadow { shadow_sdf(s, area, &body); }
            fill_sdf(s, area, look.body, &body);
        }
        StepShape::Ringed => {
            let ring = sd_circle(cx, cy, r - 1.0 + st::RING);
            if look.shadow { shadow_sdf(s, area, &ring); }
            fill_sdf(s, area, look.ring, &ring);
            fill_sdf(s, area, look.body, &sd_circle(cx, cy, r - 1.0));
        }
        StepShape::Outlined => {
            let ring = sd_circle(cx, cy, r + 1.0);
            if look.shadow { shadow_sdf(s, area, &ring); }
            fill_sdf(s, area, look.ring, &ring);
            fill_sdf(s, area, look.body, &sd_circle(cx, cy, r + 1.0 - st::THICK_RING));
        }
        StepShape::Square => {
            let half = r - 1.0;
            let ring = sd_round_box(cx, cy, half + st::RING, half + st::RING, 7.0 + st::RING);
            if look.shadow { shadow_sdf(s, area, &ring); }
            fill_sdf(s, area, look.ring, &ring);
            fill_sdf(s, area, look.body, &sd_round_box(cx, cy, half, half, 7.0));
        }
        StepShape::Halo => {
            let body = sd_circle(cx, cy, r);
            let faint = look.ring.with_alpha((look.ring.a as f32 * 0.32).round() as u8);
            fill_sdf(s, area, faint, &sd_circle(cx, cy, r + st::HALO));
            if look.shadow { shadow_sdf(s, area, &body); }
            fill_sdf(s, area, look.body, &body);
        }
        StepShape::Pin => {
            let head = r - 1.0;
            let hy = cy - st::PIN_RISE;
            let ring = sd_pin(cx, hy, head + st::RING, cy);
            if look.shadow { shadow_sdf(s, area, &ring); }
            fill_sdf(s, area, look.ring, &ring);
            fill_sdf(s, area, look.body, &sd_pin(cx, hy, head, cy - st::RING * 1.6));
            middle = (cx, hy);
        }
        StepShape::Hollow => {
            let body = sd_circle(cx, cy, r);
            if look.shadow { shadow_sdf(s, area, &body); }
            fill_sdf(s, area, look.body, &body);
            let (mid, half) = (r - 1.5, 1.5);
            fill_sdf(s, area, look.ring, &move |x: f32, y: f32| (((x - cx).powi(2) + (y - cy).powi(2)).sqrt() - mid).abs() - half);
        }
        StepShape::Pill => {
            let (hw, hh) = (st::pill_half_width(number), r - 1.0);
            let ring = sd_round_box(cx, cy, hw + st::RING, hh + st::RING, hh + st::RING);
            if look.shadow { shadow_sdf(s, area, &ring); }
            fill_sdf(s, area, look.ring, &ring);
            fill_sdf(s, area, look.body, &sd_round_box(cx, cy, hw, hh, hh));
        }
        StepShape::Terminal => {
            let half = r + 1.0;
            let body = sd_round_box(cx, cy, half, half, 1.0);
            if look.shadow { shadow_sdf(s, area, &body); }
            fill_sdf(s, area, look.body, &body);
            // Corner brackets: two arms at each corner, along the edges.
            // Whole pixels thick, crisp like the pixel digits.
            let (arm, t) = (9.0, 2.0);
            for (sx, sy) in [(-1.0f32, -1.0f32), (1.0, -1.0), (-1.0, 1.0), (1.0, 1.0)] {
                let (ex, ey) = (cx + sx * half, cy + sy * half);
                let along_x = sd_round_box(ex - sx * arm / 2.0, ey - sy * t / 2.0, arm / 2.0, t / 2.0, 0.0);
                let along_y = sd_round_box(ex - sx * t / 2.0, ey - sy * arm / 2.0, t / 2.0, arm / 2.0, 0.0);
                fill_sdf(s, area, look.ring, &along_x);
                fill_sdf(s, area, look.ring, &along_y);
            }
            // The app's own pixel digits.
            let scale = if digits <= 2 { 2 } else { 1 };
            let n = digits as i32;
            let tw = n * 5 * scale + (n - 1) * scale;
            let th = 7 * scale;
            draw_number(s, (cx - tw as f32 / 2.0).round() as i32, (cy - th as f32 / 2.0).round() as i32,
                        scale, number, look.number);
            return;
        }
    }
    step_digits(s, middle.0, middle.1, number, px, look.number);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::painter::{ImageSurface, U32Surface};
    use framefang_core::color::Rgba as K;

    #[test]
    fn every_step_style_stays_inside_its_extent_and_shows_its_number() {
        use framefang_core::step::{extent, StepLook, StepShape};
        for shape in StepShape::ALL {
            for (number, look) in [(3, StepLook::recipe(shape, K::RED)), (12, StepLook::recipe(shape, K::new_opaque(255, 230, 0)))] {
                let (w, h) = (160, 160);
                let mut buf = vec![0u32; (w * h) as usize];
                let (cx, cy) = (80.0, 90.0);
                let mut look = look;
                look.shadow = false;
                {
                    let mut s = U32Surface { buf: &mut buf, stride: w, height: h };
                    step_marker(&mut s, Point2::new(cx, cy), number, &look);
                }
                let (l, t, r, b) = extent(number, &look);
                let mut lit = 0;
                for y in 0..h { for x in 0..w {
                    if buf[(y * w + x) as usize] == 0 { continue; }
                    lit += 1;
                    let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
                    assert!(fx >= cx + l - 1.0 && fx <= cx + r + 1.0 && fy >= cy + t - 1.0 && fy <= cy + b + 1.0,
                            "{shape:?} {number}: ink at ({x}, {y}) outside {:?}", (l, t, r, b));
                } }
                assert!(lit > 300, "{shape:?} is drawn");
                // The number's color is somewhere in it.
                let n = look.number;
                let want = ((n.r as u32) << 16) | ((n.g as u32) << 8) | n.b as u32;
                assert!(buf.contains(&want), "{shape:?} {number}: no digits in {:06X}", want);
            }
        }
    }

    /// Writes target/step-styles.png: every style, on light and dark, with
    /// one and two digits, in red and yellow.
    #[test]
    #[ignore]
    fn render_step_styles_to_png() {
        use framefang_core::step::{StepLook, StepShape};
        let (cell, rows) = (64u32, 4u32);
        let w = cell * StepShape::ALL.len() as u32;
        let mut img = image::RgbaImage::from_fn(w, cell * rows, |_, y| {
            if (y / cell) % 2 == 0 { image::Rgba([244, 244, 242, 255]) } else { image::Rgba([29, 31, 36, 255]) }
        });
        for (i, shape) in StepShape::ALL.into_iter().enumerate() {
            for row in 0..rows {
                let accent = if row < 2 { K::new_opaque(220, 38, 38) } else { K::new_opaque(255, 200, 0) };
                let number = if row % 2 == 0 { 3 } else { 12 };
                let cx = (i as u32 * cell + cell / 2) as f32;
                let cy = (row * cell + cell / 2) as f32 + if shape == StepShape::Pin { 14.0 } else { 0.0 };
                let mut s = ImageSurface(&mut img);
                step_marker(&mut s, Point2::new(cx, cy), number, &StepLook::recipe(shape, accent));
            }
        }
        let big = image::imageops::resize(&img, w * 2, cell * rows * 2, image::imageops::FilterType::Nearest);
        big.save(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/step-styles.png")).unwrap();
    }
}
