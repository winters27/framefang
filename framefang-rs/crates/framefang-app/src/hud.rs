//! The dialogs' design language: a retro-futuristic terminal, the feel of a
//! cyberpunk HUD, carried by a few restrained touches rather than props. One
//! accent (the color scheme's) on near-black, the pixel font for every system
//! label, and:
//!
//! - **Scanlines:** a faint accent line every third row of the background, so
//!   the surfaces read as a phosphor screen. Backgrounds only; text stays
//!   crisp.
//! - **Glowing headings:** each window's title with a soft blurred halo of
//!   its own color behind it, like neon. The rule under the header glows too.
//! - **Terminal section labels:** `01 // CAPTURES`.
//!
//! Everything here draws into a `0x00RRGGBB` buffer `w` pixels wide.

use crate::palette::*;

// ── pixels ──────────────────────────────────────────────────────────────────

fn mix(dst: u32, src: u32, a: f32) -> u32 {
    let a = a.clamp(0.0, 1.0);
    let ch = |s: u32| {
        let (d, c) = (((dst >> s) & 0xFF) as f32, ((src >> s) & 0xFF) as f32);
        (d + (c - d) * a).round() as u32
    };
    (ch(16) << 16) | (ch(8) << 8) | ch(0)
}

/// Lighten `dst` by `src * a` (an additive glow, clamped).
fn add(dst: u32, src: u32, a: f32) -> u32 {
    let ch = |s: u32| {
        let (d, c) = (((dst >> s) & 0xFF) as f32, ((src >> s) & 0xFF) as f32);
        (d + c * a.max(0.0)).min(255.0).round() as u32
    };
    (ch(16) << 16) | (ch(8) << 8) | ch(0)
}

fn blend_px(buf: &mut [u32], w: usize, h: usize, x: i32, y: i32, c: u32, a: f32) {
    if x < 0 || y < 0 || x as usize >= w || y as usize >= h || a <= 0.0 { return; }
    let i = y as usize * w + x as usize;
    buf[i] = mix(buf[i], c, a);
}

// ── backgrounds ─────────────────────────────────────────────────────────────

/// A dialog's background: the header band and the body, both with faint
/// scanlines, and the glowing rule under the header.
pub fn backdrop(buf: &mut [u32], w: usize, h: usize, header_h: i32) {
    let (top, body, accent) = (BG_TOP(), BG_BODY(), LASER());
    let (top_line, body_line) = (mix(top, accent, 0.035), mix(body, accent, 0.03));
    for y in 0..h {
        let line = y % 3 == 0;
        let c = match ((y as i32) < header_h, line) {
            (true, false) => top,
            (true, true) => top_line,
            (false, false) => body,
            (false, true) => body_line,
        };
        buf[y * w..(y + 1) * w].fill(c);
    }
    header_rule(buf, w, h, header_h);
}

/// The rule under a header: a 1 px accent line faded at both ends, with a
/// faint glow row on either side.
pub fn header_rule(buf: &mut [u32], w: usize, h: usize, y: i32) {
    let accent = LASER();
    for x in 0..w {
        let t = x as f32 / (w.max(2) - 1) as f32;
        let edge = (t.min(1.0 - t) / 0.12).min(1.0);
        blend_px(buf, w, h, x as i32, y, accent, 0.85 * edge);
        blend_px(buf, w, h, x as i32, y - 1, accent, 0.18 * edge);
        blend_px(buf, w, h, x as i32, y + 1, accent, 0.18 * edge);
    }
}

// ── headings ────────────────────────────────────────────────────────────────

/// A window's heading at `(x, y)` in the scale-2 pixel font, in the accent,
/// with a soft neon glow of the same color behind it.
pub fn heading(buf: &mut [u32], w: usize, h: usize, x: i32, y: i32, text: &str) {
    const SCALE: i32 = 2;
    const R: i32 = 5; // glow radius
    let accent = LASER();
    let tw = crate::bitmap_font::measure(text, SCALE);
    let th = crate::bitmap_font::GLYPH_H * SCALE;
    // The text as a mask, with room for the glow around it.
    let (mw, mh) = ((tw + 2 * R) as usize, (th + 2 * R) as usize);
    let mut mask = vec![0u32; mw * mh];
    {
        let mut s = crate::painter::U32Surface { buf: &mut mask, stride: mw as i32, height: mh as i32 };
        crate::painter::draw_text(&mut s, R, R, SCALE, text, framefang_core::color::Rgba::new(255, 255, 255, 255));
    }
    let mut a: Vec<f32> = mask.iter().map(|&p| (p & 0xFF) as f32 / 255.0).collect();
    // Two box blurs make a soft round falloff.
    for _ in 0..2 { box_blur(&mut a, mw, mh, R / 2 + 1); }
    for my in 0..mh {
        for mx in 0..mw {
            let g = a[my * mw + mx];
            if g <= 0.004 { continue; }
            let (bx, by) = (x - R + mx as i32, y - R + my as i32);
            if bx < 0 || by < 0 || bx as usize >= w || by as usize >= h { continue; }
            let i = by as usize * w + bx as usize;
            buf[i] = add(buf[i], accent, g * 0.7);
        }
    }
    let mut s = crate::painter::U32Surface { buf, stride: w as i32, height: h as i32 };
    let c = framefang_core::color::Rgba::new((accent >> 16) as u8, (accent >> 8) as u8, accent as u8, 255);
    crate::painter::draw_text(&mut s, x, y, SCALE, text, c);
}

fn box_blur(a: &mut [f32], w: usize, h: usize, r: i32) {
    let mut tmp = vec![0.0f32; a.len()];
    let r = r.max(1) as isize;
    let n = (2 * r + 1) as f32;
    for y in 0..h {
        for x in 0..w {
            let mut s = 0.0;
            for d in -r..=r {
                let xx = x as isize + d;
                if xx >= 0 && (xx as usize) < w { s += a[y * w + xx as usize]; }
            }
            tmp[y * w + x] = s / n;
        }
    }
    for y in 0..h {
        for x in 0..w {
            let mut s = 0.0;
            for d in -r..=r {
                let yy = y as isize + d;
                if yy >= 0 && (yy as usize) < h { s += tmp[yy as usize * w + x]; }
            }
            a[y * w + x] = s / n;
        }
    }
}

// ── section labels ──────────────────────────────────────────────────────────

/// A section label at `(x, y)`: `01 // NAME`, the number and slashes dim, the
/// name in the section tint, then a quiet rule out to `right`.
pub fn section(buf: &mut [u32], w: usize, h: usize, x: i32, right: i32, y: i32, n: usize, name: &str) {
    let rgba = |c: u32| framefang_core::color::Rgba::new((c >> 16) as u8, (c >> 8) as u8, c as u8, 255);
    let prefix = format!("{n:02} // ");
    // `measure` leaves off the gap after the last glyph; add it back.
    let pw = crate::bitmap_font::measure(&prefix, 1) + 1;
    {
        let mut s = crate::painter::U32Surface { buf, stride: w as i32, height: h as i32 };
        crate::painter::draw_text(&mut s, x, y, 1, &prefix, rgba(TEXT_DIM()));
        crate::painter::draw_text(&mut s, x + pw, y, 1, name, rgba(SECTION_TINT()));
    }
    let start = x + pw + crate::bitmap_font::measure(name, 1) + 10;
    let rule_y = y + crate::bitmap_font::GLYPH_H / 2;
    if rule_y < 0 || rule_y as usize >= h { return; }
    let rule = HEADER_RULE();
    for xx in start.max(0)..right.min(w as i32) {
        buf[rule_y as usize * w + xx as usize] = rule;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_header_rule_fades_out_at_both_ends() {
        let w = 200;
        let bg = BG_TOP();
        let mut buf = vec![bg; w * 5];
        header_rule(&mut buf, w, 5, 2);
        assert_eq!(buf[2 * w], bg, "left end is the background");
        assert_eq!(buf[3 * w - 1], bg, "right end is the background");
        assert_ne!(buf[2 * w + w / 2], bg, "the middle is lit");
        assert_ne!(buf[w + w / 2], bg, "with a glow above it");
        assert_eq!(buf[w / 2], bg, "two rows away is untouched");
    }

    #[test]
    fn a_heading_glows_around_its_letters() {
        let (w, h) = (300usize, 40usize);
        let mut b = vec![0u32; w * h];
        heading(&mut b, w, h, 10, 10, "FRAME FANG");
        assert!(b[5 * w + 30] != 0, "light spills above the letters");
        assert_eq!(b[39 * w + 299], 0, "far away stays dark");
    }

    #[test]
    fn scanlines_are_faint_and_only_every_third_row() {
        let (w, h) = (10usize, 12usize);
        let mut b = vec![0u32; w * h];
        backdrop(&mut b, w, h, 4);
        assert_ne!(b[6 * w], b[7 * w], "a scanline row differs from its neighbor");
        assert_eq!(b[7 * w], b[8 * w], "rows between scanlines match");
        let lum = |p: u32| ((p >> 16) & 0xFF) + ((p >> 8) & 0xFF) + (p & 0xFF);
        assert!(lum(b[6 * w]).abs_diff(lum(b[7 * w])) < 30, "faint");
    }
}
