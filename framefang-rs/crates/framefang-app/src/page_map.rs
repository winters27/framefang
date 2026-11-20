//! The scrolling editor (look E2, Brandon 2026-11-19): a picture taller
//! than the screen, such as a scrolling capture, opens in a full-height
//! editor window. The picture scrolls inside it with the wheel, a page map
//! beside it shows the whole page with the visible part boxed (click or drag
//! it to jump), and the tools stay put.
//!
//! The editor draws the whole page into a page-sized buffer in page
//! coordinates, as it draws any picture, then shows the slice `View::y`
//! down. Everything here is that slice's arithmetic and the map's drawing;
//! `editor.rs` wires it in.

use image::{ImageBuffer, Rgba};

/// The map column's width, and the room either side of it.
pub const MAP_W: i32 = 72;
pub const MAP_GAP: i32 = 10;
/// How far one wheel notch scrolls the page.
pub const NOTCH_PX: i32 = 120;

/// While a stroke, a move or a crop is dragged, the pointer this close to
/// the window's top or bottom scrolls the page along with it.
pub const EDGE_ZONE: i32 = 56;

/// How far one tick (about 30 a second) scrolls with the pointer at `y`
/// (window pixels) in a window `view_h` tall while dragging: up near the
/// top, down near the bottom, faster the deeper in (and past the edge).
pub fn edge_push(y: i32, view_h: i32) -> i32 {
    let speed = |depth: i32| (depth.clamp(0, 2 * EDGE_ZONE) * 32 / (2 * EDGE_ZONE)).max(4);
    if y < EDGE_ZONE { -speed(EDGE_ZONE - y) }
    else if y > view_h - EDGE_ZONE { speed(y - (view_h - EDGE_ZONE)) }
    else { 0 }
}

/// The visible slice of a page `doc_h` tall in a window `view_h` tall, its
/// top `y` down the page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct View {
    pub doc_h: i32,
    pub view_h: i32,
    pub y: i32,
}

impl View {
    pub fn max_y(&self) -> i32 { (self.doc_h - self.view_h).max(0) }

    /// Scroll to `y`, kept on the page. `true` when it moved.
    pub fn set_y(&mut self, y: i32) -> bool {
        let y = y.clamp(0, self.max_y());
        let moved = y != self.y;
        self.y = y;
        moved
    }

    #[cfg(test)]
    pub fn scroll_by(&mut self, dy: i32) -> bool { self.set_y(self.y + dy) }
}

/// The scrolling editor window on the work area `work` (`left, top, right,
/// bottom`) for a picture `pic_w` x `pic_h` taken at `pic_x`: as tall as the
/// work area, the picture where it was across, the map on the side the
/// tools are not. `left` and `right` are the room `editor_placement` keeps
/// either side of a picture for the edge and the tools; the map adds to the
/// left (`map_left`), or to the right when the tools had to go left.
/// Returns the window's position and size, the picture's place in the
/// page, and whether the map is on the left; `None` when it doesn't fit.
pub struct TallPlacement {
    pub pos: (i32, i32),
    pub size: (u32, u32),
    pub picture_at: (i32, i32),
    pub map_left: bool,
}

pub fn tall_placement(pic_x: i32, pic_w: i32, work: (i32, i32, i32, i32), edge: i32, tools: i32)
    -> Option<TallPlacement> {
    let (l, t, r, b) = work;
    let (ww, wh) = (r - l, b - t);
    let map = MAP_GAP + MAP_W + MAP_GAP;
    let place = |left: i32, right: i32, map_left: bool| -> Option<TallPlacement> {
        let w = left + pic_w + right;
        if w > ww || pic_w <= 0 || wh <= 0 { return None; }
        let x = (pic_x - left).clamp(l, r - w);
        Some(TallPlacement { pos: (x, t), size: (w as u32, wh as u32), picture_at: (left, edge), map_left })
    };
    // Tools on the right, the map on the left; else the other way round.
    place(map.max(edge), tools, true).or_else(|| place(tools, map.max(edge), false))
}

/// Where the map goes in the window (`win_w` x `view_h`): its column
/// beside the picture, as tall as the window less its margins.
pub fn map_rect(win_w: i32, view_h: i32, map_left: bool) -> (i32, i32, i32, i32) {
    let x = if map_left { MAP_GAP } else { win_w - MAP_GAP - MAP_W };
    (x, MAP_GAP, MAP_W, (view_h - 2 * MAP_GAP).max(1))
}

/// The page drawn small enough to fit the map column whole: as wide as the
/// column, or shorter when the page is very long.
pub fn thumbnail(picture: &ImageBuffer<Rgba<u8>, Vec<u8>>, map: (i32, i32, i32, i32)) -> ImageBuffer<Rgba<u8>, Vec<u8>> {
    let (pw, ph) = (picture.width().max(1) as f32, picture.height().max(1) as f32);
    let k = ((map.2 - 4) as f32 / pw).min((map.3 - 4) as f32 / ph);
    let (tw, th) = ((pw * k).round().max(1.0) as u32, (ph * k).round().max(1.0) as u32);
    image::imageops::resize(picture, tw, th, image::imageops::FilterType::Triangle)
}

/// Where the thumbnail sits in the map: centered across, from the top.
pub fn thumb_rect(map: (i32, i32, i32, i32), thumb: (u32, u32)) -> (i32, i32, i32, i32) {
    let (tw, th) = (thumb.0 as i32, thumb.1 as i32);
    (map.0 + (map.2 - tw) / 2, map.1 + 2, tw, th)
}

/// The box on the thumbnail that is the visible slice. The page has the
/// picture `picture_y` down it, as tall as `pic_h`.
pub fn view_box(view: View, thumb: (i32, i32, i32, i32), picture_y: i32, pic_h: i32) -> (i32, i32, i32, i32) {
    let k = thumb.3 as f32 / pic_h.max(1) as f32;
    let top = ((view.y - picture_y) as f32 * k).round() as i32;
    let h = (view.view_h as f32 * k).round().max(4.0) as i32;
    let y0 = (thumb.1 + top).clamp(thumb.1, thumb.1 + thumb.3 - h.min(thumb.3));
    (thumb.0 - 2, y0, thumb.2 + 4, h.min(thumb.3))
}

/// The page position for a press `py` (window pixels) on the map: the
/// visible slice centered on the part of the page under it.
pub fn y_for_map_point(view: View, thumb: (i32, i32, i32, i32), picture_y: i32, pic_h: i32, py: i32) -> i32 {
    let k = pic_h.max(1) as f32 / thumb.3.max(1) as f32;
    let page_y = picture_y + ((py - thumb.1) as f32 * k).round() as i32;
    page_y - view.view_h / 2
}

/// Draw the map into `buf` (the window, `w` x `h`): a dark column, the
/// thumbnail, the page outside the visible slice dimmed, and the slice
/// boxed in the accent.
pub fn draw(buf: &mut [u32], w: usize, h: usize, map: (i32, i32, i32, i32),
            thumb: &ImageBuffer<Rgba<u8>, Vec<u8>>, view: View, picture_y: i32, pic_h: i32,
            accent: u32, hovered: bool) {
    let fill = |buf: &mut [u32], (x, y, rw, rh): (i32, i32, i32, i32), c: u32| {
        for yy in y.max(0)..(y + rh).min(h as i32) {
            for xx in x.max(0)..(x + rw).min(w as i32) { buf[yy as usize * w + xx as usize] = c; }
        }
    };
    // The column, as tall as the page drawn in it: a step off black so it
    // never matches the editor's cut-away canvas.
    let tr = thumb_rect(map, thumb.dimensions());
    fill(buf, (map.0 - 4, map.1 - 4, map.2 + 8, (tr.1 + tr.3 + 6) - (map.1 - 4)), 0x00_14_14_18);
    let vb = view_box(view, tr, picture_y, pic_h);
    for ty in 0..tr.3 {
        let y = tr.1 + ty;
        if y < 0 || y >= h as i32 { continue; }
        let lit = y >= vb.1 && y < vb.1 + vb.3;
        for tx in 0..tr.2 {
            let x = tr.0 + tx;
            if x < 0 || x >= w as i32 { continue; }
            let p = thumb.get_pixel(tx as u32, ty as u32).0;
            // Outside the slice: half as bright.
            let k = if lit { 1.0 } else { 0.45 };
            let c = |v: u8| (v as f32 * k) as u32;
            buf[y as usize * w + x as usize] = (c(p[0]) << 16) | (c(p[1]) << 8) | c(p[2]);
        }
    }
    // The slice's box, brighter under the pointer.
    let edge = if hovered { accent } else { dim(accent) };
    let (bx, by, bw, bh) = vb;
    fill(buf, (bx, by, bw, 2), edge);
    fill(buf, (bx, by + bh - 2, bw, 2), edge);
    fill(buf, (bx, by, 2, bh), edge);
    fill(buf, (bx + bw - 2, by, 2, bh), edge);
}

fn dim(c: u32) -> u32 {
    let f = |s: u32| ((c >> s) & 0xFF) * 3 / 4;
    (f(16) << 16) | (f(8) << 8) | f(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_slice_stays_on_the_page() {
        let mut v = View { doc_h: 5000, view_h: 1300, y: 0 };
        assert!(!v.scroll_by(-120), "already at the top");
        assert!(v.scroll_by(NOTCH_PX));
        assert_eq!(v.y, 120);
        v.set_y(99_999);
        assert_eq!(v.y, 3700, "the bottom of the page at the bottom of the window");
        // A page shorter than the window never scrolls.
        let mut short = View { doc_h: 800, view_h: 1300, y: 0 };
        assert!(!short.scroll_by(500));
    }

    #[test]
    fn dragging_near_an_edge_scrolls_that_way_faster_the_deeper_in() {
        let h = 1392;
        assert_eq!(edge_push(700, h), 0, "the middle stays put");
        assert_eq!(edge_push(EDGE_ZONE, h), 0);
        assert!(edge_push(EDGE_ZONE - 1, h) < 0, "near the top: up");
        assert!(edge_push(h - EDGE_ZONE + 1, h) > 0, "near the bottom: down");
        assert!(edge_push(0, h).abs() > edge_push(EDGE_ZONE - 10, h).abs(), "deeper is faster");
        assert_eq!(edge_push(-500, h), -32, "past the edge: the top speed");
        assert_eq!(edge_push(h + 500, h), 32);
    }

    #[test]
    fn the_window_is_as_tall_as_the_work_area_with_the_map_beside_the_picture() {
        let work = (0, 0, 2560, 1392);
        let p = tall_placement(600, 1064, work, 14, 200).unwrap();
        assert_eq!(p.size.1, 1392);
        assert!(p.map_left);
        assert_eq!(p.pos.0 + p.picture_at.0, 600, "the picture stays where it was across");
        assert!(p.picture_at.0 >= MAP_GAP + MAP_W + MAP_GAP, "the map fits left of it");
        // Near the right edge the window is pushed in, still whole.
        let q = tall_placement(2400, 1064, work, 14, 200).unwrap();
        assert!(q.pos.0 + q.size.0 as i32 <= 2560);
        // Too wide for the screen: none.
        assert!(tall_placement(0, 2500, work, 14, 200).is_none());
    }

    #[test]
    fn the_map_box_follows_the_slice_and_a_press_centers_it() {
        let view = View { doc_h: 14 + 10845 + 80, view_h: 1392, y: 0 };
        let thumb = (20, 12, 40, 1300);
        let top = view_box(view, thumb, 14, 10845);
        assert_eq!(top.1, thumb.1, "at the top of the page, the box is at the top");
        let mut bottom = view;
        bottom.set_y(bottom.max_y());
        let b = view_box(bottom, thumb, 14, 10845);
        assert_eq!(b.1 + b.3, thumb.1 + thumb.3, "at the bottom, at the bottom");
        assert!(b.3 >= 4 && b.3 < 300, "a slice of a long page is a short box: {}", b.3);
        // A press halfway down the thumbnail shows the middle of the page.
        let y = y_for_map_point(view, thumb, 14, 10845, thumb.1 + 650);
        let mid = y + view.view_h / 2;
        assert!((mid - (14 + 10845 / 2)).abs() < 20, "{mid}");
    }

    #[test]
    fn a_long_page_is_drawn_whole_in_the_map() {
        let pic = ImageBuffer::from_pixel(1064, 10845, Rgba([200, 200, 200, 255]));
        let map = map_rect(1300, 1392, true);
        let t = thumbnail(&pic, map);
        assert!(t.height() as i32 <= map.3 - 4 && t.width() as i32 <= map.2 - 4);
        // As big as the column allows: its full width, or its full height
        // for a page longer still.
        assert!(t.width() as i32 >= map.2 - 5 || t.height() as i32 >= map.3 - 5, "{:?}", t.dimensions());
        let very_long = ImageBuffer::from_pixel(1000, 40_000, Rgba([0, 0, 0, 255]));
        assert!(thumbnail(&very_long, map).height() as i32 >= map.3 - 5);
        let mut buf = vec![0u32; 1300 * 1392];
        let view = View { doc_h: 11000, view_h: 1392, y: 3000 };
        draw(&mut buf, 1300, 1392, map, &t, view, 14, 10845, 0x00_FF_00_80, false);
        assert!(buf.iter().filter(|&&p| p != 0).count() > 1000);
        // The column ends just under the page drawn in it.
        let tr = thumb_rect(map, t.dimensions());
        let below = (tr.1 + tr.3 + 20) as usize * 1300 + (map.0 + 2) as usize;
        assert_eq!(buf[below], 0, "nothing drawn below the page's thumbnail");
    }
}
