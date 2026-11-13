//! The overlay's backdrop: the frozen capture, dimmed outside the selection.
//!
//! The dimmed layer depends only on the window size, the bitmap offset and
//! the captured frame, so it is built once per overlay. The composite (dimmed
//! everywhere, bright inside the selection) is kept between frames and only
//! the rows and columns the selection moved across are rewritten. Before this,
//! the cache was keyed on the selection itself, so every pointer move of a
//! drag re-shaded the whole virtual desktop pixel by pixel.

/// Selection rectangle in frame space, `(x0, y0, x1, y1)`, exclusive end.
pub type SelRect = (i32, i32, i32, i32);

/// Dim factor for pixels outside the selection: `c * DIM_NUM / DIM_DENOM`.
const DIM_NUM: u32 = 55;
const DIM_DENOM: u32 = 100;

#[derive(Default)]
pub struct Backdrop {
    /// `(win_w, win_h, bitmap_offset)` the layers were built for.
    key: Option<(usize, usize, (i32, i32))>,
    /// The whole frame, dimmed.
    dimmed: Vec<u32>,
    /// What the frame shows: `dimmed` with the selection bright.
    composed: Vec<u32>,
    /// The selection `composed` currently shows, clamped to the frame.
    shown: Option<SelRect>,
}

/// The capture the backdrop samples, in bitmap space (RGBA8).
pub struct Shot<'a> {
    pub rgba: &'a [u8],
    pub width: usize,
    pub height: usize,
    /// Offset of frame (0, 0) inside the capture.
    pub offset: (i32, i32),
}

impl Shot<'_> {
    /// Frame columns `[a, b)` of frame row `y` that land on the capture, and
    /// the capture index of column `a`. Everything else on the row is black.
    fn row_span(&self, y: usize, win_w: usize) -> Option<(usize, usize, usize)> {
        let sy = y as i64 + self.offset.1 as i64;
        if sy < 0 || sy >= self.height as i64 {
            return None;
        }
        let ox = self.offset.0 as i64;
        let a = (-ox).clamp(0, win_w as i64) as usize;
        let b = (self.width as i64 - ox).clamp(0, win_w as i64) as usize;
        if a >= b {
            return None;
        }
        let src = (sy as usize * self.width + (a as i64 + ox) as usize) * 4;
        Some((a, b, src))
    }
}

#[inline]
fn pack(px: &[u8]) -> u32 {
    ((px[0] as u32) << 16) | ((px[1] as u32) << 8) | px[2] as u32
}

#[inline]
fn pack_dim(px: &[u8]) -> u32 {
    let d = |c: u8| c as u32 * DIM_NUM / DIM_DENOM;
    (d(px[0]) << 16) | (d(px[1]) << 8) | d(px[2])
}

impl Backdrop {
    /// Drops the layers so the next frame rebuilds them (a new background
    /// frame, or the window moved).
    pub fn invalidate(&mut self) {
        self.key = None;
    }

    /// The backdrop for this frame, `win_w * win_h` pixels as `0RGB`.
    pub fn frame(&mut self, win_w: usize, win_h: usize, shot: &Shot, sel: Option<SelRect>) -> &[u32] {
        let key = (win_w, win_h, shot.offset);
        let sel = sel.and_then(|r| clamp(r, win_w, win_h));
        if self.key != Some(key) || self.dimmed.len() != win_w * win_h {
            self.build(win_w, win_h, shot);
            self.key = Some(key);
            self.shown = None;
            if let Some(r) = sel {
                self.paint_bright(win_w, shot, r);
            }
            self.shown = sel;
        } else if self.shown != sel {
            // Restore what the old selection brightened, then brighten the
            // new one. Both are confined to the two rectangles.
            if let Some(old) = self.shown {
                self.restore(win_w, old, sel);
            }
            if let Some(r) = sel {
                self.paint_bright(win_w, shot, r);
            }
            self.shown = sel;
        }
        &self.composed
    }

    fn build(&mut self, win_w: usize, win_h: usize, shot: &Shot) {
        self.dimmed.clear();
        self.dimmed.resize(win_w * win_h, 0);
        for y in 0..win_h {
            if let Some((a, b, src)) = shot.row_span(y, win_w) {
                let row = &mut self.dimmed[y * win_w + a..y * win_w + b];
                let px = &shot.rgba[src..src + (b - a) * 4];
                for (d, p) in row.iter_mut().zip(px.chunks_exact(4)) {
                    *d = pack_dim(p);
                }
            }
        }
        self.composed.clear();
        self.composed.extend_from_slice(&self.dimmed);
    }

    /// Puts the dimmed pixels back where `old` was bright, skipping the part
    /// `keep` is about to brighten again.
    fn restore(&mut self, win_w: usize, old: SelRect, keep: Option<SelRect>) {
        let (x0, y0, x1, y1) = old;
        for y in y0..y1 {
            let row = y as usize * win_w;
            let spans: [(i32, i32); 2] = match keep {
                Some((kx0, ky0, kx1, ky1)) if y >= ky0 && y < ky1 => {
                    [(x0, x1.min(kx0).max(x0)), (x0.max(kx1).min(x1), x1)]
                }
                _ => [(x0, x1), (x1, x1)],
            };
            for (a, b) in spans {
                if a < b {
                    let (a, b) = (row + a as usize, row + b as usize);
                    self.composed[a..b].copy_from_slice(&self.dimmed[a..b]);
                }
            }
        }
    }

    fn paint_bright(&mut self, win_w: usize, shot: &Shot, r: SelRect) {
        let (x0, y0, x1, y1) = r;
        for y in y0 as usize..y1 as usize {
            let row = y * win_w;
            let out = &mut self.composed[row + x0 as usize..row + x1 as usize];
            out.fill(0);
            let Some((a0, b0, src)) = shot.row_span(y, win_w) else { continue };
            let (a, b) = (a0.max(x0 as usize), b0.min(x1 as usize));
            if a < b {
                let start = src + (a - a0) * 4;
                let px = &shot.rgba[start..start + (b - a) * 4];
                let dst = &mut out[a - x0 as usize..b - x0 as usize];
                for (d, p) in dst.iter_mut().zip(px.chunks_exact(4)) {
                    *d = pack(p);
                }
            }
        }
    }
}

fn clamp(r: SelRect, w: usize, h: usize) -> Option<SelRect> {
    let (x0, y0, x1, y1) = r;
    let c = (x0.clamp(0, w as i32), y0.clamp(0, h as i32), x1.clamp(0, w as i32), y1.clamp(0, h as i32));
    (c.0 < c.2 && c.1 < c.3).then_some(c)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The original pass 1, kept as the reference the new one must match.
    fn reference(win_w: usize, win_h: usize, shot: &Shot, sel: Option<SelRect>) -> Vec<u32> {
        let mut out = vec![0; win_w * win_h];
        for y in 0..win_h {
            for x in 0..win_w {
                let sx = x as i32 + shot.offset.0;
                let sy = y as i32 + shot.offset.1;
                let (r, g, b) = if sx >= 0 && sy >= 0 && (sx as usize) < shot.width && (sy as usize) < shot.height {
                    let s = (sy as usize * shot.width + sx as usize) * 4;
                    (shot.rgba[s] as u32, shot.rgba[s + 1] as u32, shot.rgba[s + 2] as u32)
                } else {
                    (0, 0, 0)
                };
                let inside = sel.is_some_and(|(x0, y0, x1, y1)| {
                    (x as i32) >= x0 && (x as i32) < x1 && (y as i32) >= y0 && (y as i32) < y1
                });
                let (r, g, b) = if inside { (r, g, b) } else { (r * 55 / 100, g * 55 / 100, b * 55 / 100) };
                out[y * win_w + x] = (r << 16) | (g << 8) | b;
            }
        }
        out
    }

    fn noise(w: usize, h: usize) -> Vec<u8> {
        let mut s: u32 = 0x9E37_79B9;
        (0..w * h * 4)
            .map(|_| {
                s ^= s << 13;
                s ^= s >> 17;
                s ^= s << 5;
                s as u8
            })
            .collect()
    }

    #[test]
    fn matches_the_original_through_a_drag() {
        let (sw, sh) = (61, 47);
        let rgba = noise(sw, sh);
        // Offsets cover a frame inside the capture, hanging off every edge,
        // and a gap the capture does not reach.
        for offset in [(0, 0), (5, 3), (-4, -6), (9, -2)] {
            let shot = Shot { rgba: &rgba, width: sw, height: sh, offset };
            let (ww, wh) = (58, 50);
            let mut b = Backdrop::default();
            let drag = [
                None,
                Some((10, 10, 11, 11)),
                Some((10, 10, 30, 25)),
                Some((10, 10, 20, 40)),
                Some((5, 2, 57, 49)),
                Some((40, 30, 70, 90)), // past the frame edge, clamped
                Some((-8, -8, 3, 3)),
                Some((20, 20, 20, 30)), // empty
                Some((12, 14, 33, 29)),
                None,
            ];
            for sel in drag {
                assert_eq!(b.frame(ww, wh, &shot, sel), &reference(ww, wh, &shot, sel)[..], "offset {offset:?} sel {sel:?}");
            }
            b.invalidate();
            assert_eq!(b.frame(ww, wh, &shot, Some((1, 1, 9, 9))), &reference(ww, wh, &shot, Some((1, 1, 9, 9)))[..]);
        }
    }

    /// `cargo test --release -p framefang-app backdrop::tests::drag_cost -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn drag_cost() {
        let (w, h) = (3640, 2241);
        let rgba = noise(w, h);
        let shot = Shot { rgba: &rgba, width: w, height: h, offset: (0, 0) };
        let steps: Vec<SelRect> = (1..=120).map(|i| (400, 300, 400 + i * 20, 300 + i * 12)).collect();

        let t = std::time::Instant::now();
        for &s in &steps {
            std::hint::black_box(reference(w, h, &shot, Some(s)));
        }
        let old = t.elapsed().as_secs_f64() * 1000.0 / steps.len() as f64;

        let mut b = Backdrop::default();
        b.frame(w, h, &shot, None);
        let t = std::time::Instant::now();
        for &s in &steps {
            std::hint::black_box(b.frame(w, h, &shot, Some(s)));
        }
        let new = t.elapsed().as_secs_f64() * 1000.0 / steps.len() as f64;
        println!("backdrop per drag frame at {w}x{h}: before {old:.2} ms, after {new:.3} ms");
    }
}
