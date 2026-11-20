//! The annotations that change the picture rather than draw on it:
//! Pixelate, Spotlight and Magnify.

use framefang_core::annotation::{Point2, Stroke};
use image::{ImageBuffer, Rgba};

use super::{stroke_ellipse, Surface};

/// A half-open pixel rectangle: `x0..x1` by `y0..y1`.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Span {
    x0: i32,
    y0: i32,
    x1: i32,
    y1: i32,
}

impl Span {
    fn clipped_to(self, w: i32, h: i32) -> Span {
        Span { x0: self.x0.max(0), y0: self.y0.max(0), x1: self.x1.min(w), y1: self.y1.min(h) }
    }

    fn is_empty(self) -> bool {
        self.x0 >= self.x1 || self.y0 >= self.y1
    }
}

/// Pixelates the rectangle with corners `a` and `b`: it is cut into
/// `block_size` square cells (at least 2 px, counted from its top left
/// corner, the last row and column cut short), and each cell is filled with
/// the average color of the same cell in `src`.
///
/// `src` should be the capture as taken, without any annotations. Reading
/// the clean picture means a Pixelate looks the same whatever was drawn
/// before or after it, and can never smear another annotation's ink.
///
/// `src_offset` is where this surface's (0, 0) falls in `src`. It is
/// `(0, 0)` when both share coordinates; the editor passes a real offset
/// when its window does not sit exactly on the capture it shows (see
/// `framefang_core::virtual_desktop`). Cells with no source pixels under
/// them are left alone.
pub fn pixelate_rect<S: Surface>(s: &mut S, src: &ImageBuffer<Rgba<u8>, Vec<u8>>, a: Point2, b: Point2,
                                 block_size: u32, src_offset: (i32, i32)) {
    let cell = block_size.max(2) as i32;
    let area = Span {
        x0: a.x.min(b.x) as i32,
        y0: a.y.min(b.y) as i32,
        x1: a.x.max(b.x) as i32,
        y1: a.y.max(b.y) as i32,
    };
    for y in (area.y0..area.y1).step_by(cell as usize) {
        for x in (area.x0..area.x1).step_by(cell as usize) {
            let here = Span { x0: x, y0: y, x1: (x + cell).min(area.x1), y1: (y + cell).min(area.y1) };
            if let Some(color) = average_under(src, here, src_offset) {
                fill_span(s, here, color);
            }
        }
    }
}

/// The mean color (each channel rounded down, alpha 255) of the source
/// pixels under `cell`, or `None` when the cell lies wholly off the source.
fn average_under(src: &ImageBuffer<Rgba<u8>, Vec<u8>>, cell: Span, (dx, dy): (i32, i32)) -> Option<[u8; 4]> {
    let shifted = Span { x0: cell.x0 + dx, y0: cell.y0 + dy, x1: cell.x1 + dx, y1: cell.y1 + dy };
    let on_src = shifted.clipped_to(src.width() as i32, src.height() as i32);
    if on_src.is_empty() { return None; }
    let mut sum = [0u32; 3];
    for y in on_src.y0..on_src.y1 {
        for x in on_src.x0..on_src.x1 {
            let px = src[(x as u32, y as u32)].0;
            for (total, &c) in sum.iter_mut().zip(&px[..3]) {
                *total += c as u32;
            }
        }
    }
    let n = ((on_src.x1 - on_src.x0) * (on_src.y1 - on_src.y0)) as u32;
    Some([(sum[0] / n) as u8, (sum[1] / n) as u8, (sum[2] / n) as u8, 255])
}

/// Paints the part of `area` that is on the surface solid `color`.
fn fill_span<S: Surface>(s: &mut S, area: Span, color: [u8; 4]) {
    let on_surface = area.clipped_to(s.width(), s.height());
    for y in on_surface.y0..on_surface.y1 {
        for x in on_surface.x0..on_surface.x1 {
            s.write(x, y, color);
        }
    }
}

/// How much of a pixel's color is left outside every spotlight (55 % dimmer).
const SPOT_KEEP: u32 = 115;

/// The lit stretch of row `y` inside one spotlight, a rounded rectangle
/// (corner radius 8, less for a small one), or `None` on a row it misses.
fn spot_span(a: Point2, b: Point2, y: i32) -> Option<(i32, i32)> {
    let (x0, x1) = (a.x.min(b.x), a.x.max(b.x));
    let (y0, y1) = (a.y.min(b.y), a.y.max(b.y));
    let yc = y as f32 + 0.5;
    if yc < y0 || yc >= y1 { return None; }
    let r = 8f32.min((x1 - x0) / 2.0).min((y1 - y0) / 2.0).max(0.0);
    // How far the corner curve pulls this row in.
    let dy = if yc < y0 + r { y0 + r - yc } else if yc > y1 - r { yc - (y1 - r) } else { 0.0 };
    let inset = if dy > 0.0 { r - (r * r - dy * dy).max(0.0).sqrt() } else { 0.0 };
    let (l, rr) = ((x0 + inset).round() as i32, (x1 - inset).round() as i32);
    (rr > l).then_some((l, rr))
}

/// Dim every pixel inside `bounds` (`x, y, w, h`) that no spotlight lights.
/// Row by row: each spotlight lights one stretch of a row, so only those
/// stretches are skipped and the cost is the area, not area x spotlights.
pub fn dim_outside_spots<S: Surface>(s: &mut S, spots: &[(Point2, Point2)], bounds: (i32, i32, i32, i32)) {
    if spots.is_empty() { return; }
    let (bx0, by0) = (bounds.0.max(0), bounds.1.max(0));
    let (bx1, by1) = ((bounds.0 + bounds.2).min(s.width()), (bounds.1 + bounds.3).min(s.height()));
    let mut lit: Vec<(i32, i32)> = Vec::with_capacity(spots.len());
    for y in by0..by1 {
        lit.clear();
        lit.extend(spots.iter().filter_map(|(a, b)| spot_span(*a, *b, y)));
        lit.sort_unstable();
        let mut x = bx0;
        let mut i = 0;
        while x < bx1 {
            // Skip a lit stretch under x, else dim up to the next one.
            while i < lit.len() && lit[i].1 <= x { i += 1; }
            if i < lit.len() && lit[i].0 <= x {
                x = lit[i].1;
                continue;
            }
            let stop = if i < lit.len() { lit[i].0.min(bx1) } else { bx1 };
            for xx in x..stop {
                let p = s.read(xx, y);
                let k = |c: u8| ((c as u32 * SPOT_KEEP + 127) / 255) as u8;
                s.write(xx, y, [k(p[0]), k(p[1]), k(p[2]), p[3]]);
            }
            x = stop;
        }
    }
}

/// A round lens at `center` out to `edge`: what is already drawn under it,
/// `zoom` times larger (sampled only inside `bounds`, the picture), ringed in
/// the stroke.
pub fn magnify<S: Surface>(s: &mut S, stroke: Stroke, center: Point2, edge: Point2, zoom: u32,
                           bounds: (i32, i32, i32, i32)) {
    let r = framefang_core::annotation::magnify_radius(center, edge);
    if r < 1.0 { return; }
    let z = zoom.clamp(2, 4) as f32;
    let (bx0, by0) = (bounds.0.max(0), bounds.1.max(0));
    let (bx1, by1) = ((bounds.0 + bounds.2).min(s.width()) - 1, (bounds.1 + bounds.3).min(s.height()) - 1);
    if bx1 < bx0 || by1 < by0 { return; }
    // What the lens shows, read before anything is written over it.
    let reach = r / z + 2.0;
    let (sx0, sy0) = (((center.x - reach).floor() as i32).clamp(bx0, bx1), ((center.y - reach).floor() as i32).clamp(by0, by1));
    let (sx1, sy1) = (((center.x + reach).ceil() as i32).clamp(bx0, bx1), ((center.y + reach).ceil() as i32).clamp(by0, by1));
    let sw = (sx1 - sx0 + 1) as usize;
    let mut snap = Vec::with_capacity(sw * (sy1 - sy0 + 1) as usize);
    for y in sy0..=sy1 { for x in sx0..=sx1 { snap.push(s.read(x, y)); } }
    let at = |x: i32, y: i32| snap[(y.clamp(sy0, sy1) - sy0) as usize * sw + (x.clamp(sx0, sx1) - sx0) as usize];
    let (dx0, dy0) = (((center.x - r).floor() as i32).max(bx0), ((center.y - r).floor() as i32).max(by0));
    let (dx1, dy1) = (((center.x + r).ceil() as i32).min(bx1), ((center.y + r).ceil() as i32).min(by1));
    for y in dy0..=dy1 {
        for x in dx0..=dx1 {
            let (px, py) = (x as f32 + 0.5 - center.x, y as f32 + 0.5 - center.y);
            if px * px + py * py > r * r { continue; }
            // Bilinear sample of the spot this pixel shows.
            let (fx, fy) = (center.x + px / z - 0.5, center.y + py / z - 0.5);
            let (ix, iy) = (fx.floor() as i32, fy.floor() as i32);
            let (tx, ty) = (fx - ix as f32, fy - iy as f32);
            let (p00, p10, p01, p11) = (at(ix, iy), at(ix + 1, iy), at(ix, iy + 1), at(ix + 1, iy + 1));
            let mut out = [0u8; 4];
            for c in 0..4 {
                let top = p00[c] as f32 * (1.0 - tx) + p10[c] as f32 * tx;
                let bot = p01[c] as f32 * (1.0 - tx) + p11[c] as f32 * tx;
                out[c] = (top * (1.0 - ty) + bot * ty).round() as u8;
            }
            s.write(x, y, out);
        }
    }
    // The ring's inner edge meets the lens.
    let ring = r + stroke.thickness / 2.0;
    stroke_ellipse(s, Point2::new(center.x - ring, center.y - ring), Point2::new(center.x + ring, center.y + ring),
                   stroke.thickness, stroke.color);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::painter::{render_all, ImageSurface, Pass};
    use framefang_core::annotation::Annotation;
    use framefang_core::color::Rgba as FramefangRgba;

    fn grey(w: u32, h: u32) -> ImageBuffer<Rgba<u8>, Vec<u8>> {
        ImageBuffer::from_fn(w, h, |x, _| Rgba([(x * 2) as u8, 100, 100, 255]))
    }

    #[test]
    fn pixelate_fills_each_cell_with_its_source_average_rounded_down() {
        // Red rises by one per column: a 4 wide cell over columns 0..4 holds
        // 0, 1, 2, 3, which average to 1.5 and are stored as 1.
        let src = ImageBuffer::from_fn(8, 4, |x, _| Rgba([x as u8, 50, 60, 255]));
        let mut out = ImageBuffer::from_pixel(8, 4, Rgba([0u8, 0, 0, 0]));
        pixelate_rect(&mut ImageSurface(&mut out), &src, Point2::new(0.0, 0.0), Point2::new(8.0, 4.0), 4, (0, 0));
        assert_eq!(out.get_pixel(0, 0).0, [1, 50, 60, 255]);
        assert_eq!(out.get_pixel(3, 3).0, [1, 50, 60, 255]);
        assert_eq!(out.get_pixel(4, 0).0, [5, 50, 60, 255], "the second cell averages 4..8");
        // The corners may be given either way round.
        let mut flipped = ImageBuffer::from_pixel(8, 4, Rgba([0u8, 0, 0, 0]));
        pixelate_rect(&mut ImageSurface(&mut flipped), &src, Point2::new(8.0, 4.0), Point2::new(0.0, 0.0), 4, (0, 0));
        assert_eq!(out, flipped);
    }

    #[test]
    fn pixelate_reads_through_the_offset_and_skips_cells_off_the_source() {
        let src = ImageBuffer::from_fn(4, 4, |x, y| Rgba([(x * 10) as u8, (y * 10) as u8, 0, 255]));
        let mut out = ImageBuffer::from_pixel(8, 4, Rgba([9u8, 9, 9, 9]));
        // Surface column 2 is source column 0. Cells of 2: surface 2..4
        // reads source 0..2, surface 4..6 reads 2..4, surface 6..8 is off it.
        pixelate_rect(&mut ImageSurface(&mut out), &src, Point2::new(2.0, 0.0), Point2::new(8.0, 2.0), 1, (-2, 0));
        assert_eq!(out.get_pixel(2, 0).0, [5, 5, 0, 255], "block size 1 is treated as 2");
        assert_eq!(out.get_pixel(4, 1).0, [25, 5, 0, 255]);
        assert_eq!(out.get_pixel(6, 0).0, [9, 9, 9, 9], "no source under it, left alone");
        assert_eq!(out.get_pixel(0, 0).0, [9, 9, 9, 9], "outside the rectangle, left alone");
    }

    #[test]
    fn a_spotlight_dims_only_outside_itself_and_only_the_picture() {
        let mut img = ImageBuffer::from_pixel(100, 100, Rgba([200u8, 200, 200, 255]));
        let mut spot = Annotation::spotlight(Point2::new(20.0, 20.0));
        spot.extend(Point2::new(60.0, 60.0));
        let mut arrow = Annotation::arrow(Stroke { color: FramefangRgba::RED, thickness: 4.0 }, Point2::new(70.0, 80.0));
        arrow.extend(Point2::new(95.0, 80.0));
        // The arrow comes first in the list; it is still drawn over the dim.
        let list = [arrow, spot];
        render_all(&mut ImageSurface(&mut img), list.iter(), &Pass { source: None, src_offset: (0, 0), bounds: (0, 0, 90, 100) });
        assert_eq!(img.get_pixel(40, 40).0, [200, 200, 200, 255], "inside stays as captured");
        assert_eq!(img.get_pixel(5, 5).0[0], ((200 * SPOT_KEEP + 127) / 255) as u8, "outside is dimmed");
        assert_eq!(img.get_pixel(95, 5).0, [200, 200, 200, 255], "past the picture's bounds nothing changes");
        assert!(img.get_pixel(80, 80).0[0] > 200 && img.get_pixel(80, 80).0[1] < 60, "the arrow is undimmed red");
        // Rounded corners: the very corner of the spotlight is not lit.
        assert!(img.get_pixel(20, 20).0[0] < 200);
    }

    #[test]
    fn a_pixelate_under_a_spotlights_dark_is_as_dark_as_the_rest() {
        let src = grey(100, 100);
        let mut img = src.clone();
        let mut spot = Annotation::spotlight(Point2::new(0.0, 0.0));
        spot.extend(Point2::new(30.0, 30.0));
        let mut blur = Annotation::pixelate(Point2::new(50.0, 50.0));
        blur.extend(Point2::new(90.0, 90.0));
        let list = [spot, blur];
        render_all(&mut ImageSurface(&mut img), list.iter(), &Pass { source: Some(&src), src_offset: (0, 0), bounds: (0, 0, 100, 100) });
        assert!(img.get_pixel(60, 60).0[1] < 60, "the block is dimmed with its surroundings: {:?}", img.get_pixel(60, 60));
    }

    #[test]
    fn a_lens_shows_what_is_under_it_larger_and_only_the_picture() {
        // Left half black, right half white: a 2x lens on the edge widens it.
        let mut img = ImageBuffer::from_fn(200, 100, |x, _| if x < 100 { Rgba([0u8, 0, 0, 255]) } else { Rgba([255u8, 255, 255, 255]) });
        let stroke = Stroke { color: FramefangRgba::RED, thickness: 2.0 };
        let mut lens = Annotation::magnify(stroke, Point2::new(100.0, 50.0), 2);
        lens.extend(Point2::new(140.0, 50.0));
        render_all(&mut ImageSurface(&mut img), [lens].iter(), &Pass { source: None, src_offset: (0, 0), bounds: (0, 0, 200, 100) });
        // 20 px left of center shows what was 10 px left: still black; 20
        // px right shows white; the pixels at the very edge of the old line
        // are now 2x further out.
        assert_eq!(img.get_pixel(80, 50).0[0], 0);
        assert_eq!(img.get_pixel(120, 50).0[0], 255);
        // The ring sits just outside.
        assert!(img.get_pixel(141, 50).0[0] > 150 && img.get_pixel(141, 50).0[1] < 100, "{:?}", img.get_pixel(141, 50));
    }
}
