//! Rectangles and placement: where the editor window opens, where a toast
//! lands, how a crop or a selection reshapes under the pointer. All pure
//! arithmetic on `(x, y, w, h)` rectangles; nothing here touches a window.

use framefang_core::state::Edge;
use framefang_core::virtual_desktop as vdesk;
use image::{ImageBuffer, Rgba};

use super::flyout::{FLYOUT_GAP, FLYOUT_W};
use super::panels::{action_panel_dims, tool_panel_dims, tooltip_size, EDITOR_SLOTS, LOCKED_ACTIONS, PANEL_OFFSET, TOOLTIP_GAP};
use super::{ChosenRegion, EditorPlacement, TallChosen};

/// `(x, y, w, h)`.
pub(super) type Rect4 = (i32, i32, i32, i32);

/// The picture's margin in the editor window on the sides without panels.
pub(super) const EDITOR_EDGE: i32 = 14;

pub(super) fn rect_contains((x, y, w, h): Rect4, (px, py): (i32, i32)) -> bool {
    (x..x + w).contains(&px) && (y..y + h).contains(&py)
}

/// The rectangle with `a` and `b` as opposite corners.
pub(super) fn rect_from(a: (i32, i32), b: (i32, i32)) -> Rect4 {
    (a.0.min(b.0), a.1.min(b.1), (a.0 - b.0).abs(), (a.1 - b.1).abs())
}

/// Width kept beside the picture for the tool column and the flyout that
/// slides out of it.
fn tool_room() -> i32 {
    let (column_w, _) = tool_panel_dims(&EDITOR_SLOTS);
    PANEL_OFFSET + column_w + FLYOUT_GAP + FLYOUT_W + EDITOR_EDGE
}

/// Height kept under the picture for the action row and the tips that hang
/// under its buttons (Brandon 2026-11-20: "underneath it and not overlapping
/// it"). The scrolling editor keeps the same room at the foot of its page.
pub(super) fn action_room() -> i32 {
    let (_, row_h) = action_panel_dims(&LOCKED_ACTIONS);
    let tip_h = tooltip_size("Copy [Ctrl]+[C]").1;
    PANEL_OFFSET + row_h + TOOLTIP_GAP + tip_h + EDITOR_EDGE
}

/// `picture`, cut from `pic` (virtual-screen pixels), with its editor
/// window placed on the monitor under its center. `None` sends the region
/// back to be edited in place on the frozen desktop: no monitor holds its
/// center, the window would not fit there, or the picture is not the whole
/// region (the capture was cut short).
pub fn chosen_for(
    picture: ImageBuffer<Rgba<u8>, Vec<u8>>,
    pic: Rect4,
    monitors: &[vdesk::MonitorRect],
) -> Option<ChosenRegion> {
    let center = (pic.0 + pic.2 / 2, pic.1 + pic.3 / 2);
    let monitor = monitors.iter().copied()
        .find(|m| rect_contains((m.x, m.y, m.width as i32, m.height as i32), center))?;
    let placement = editor_placement(pic, monitor)?;
    let whole = (picture.width() as i32, picture.height() as i32) == (pic.2, pic.3);
    whole.then_some(ChosenRegion { picture, placement })
}

/// A scrolling capture's `picture`, taken at `pic_x` across, ready for the
/// scrolling editor in the work area `work` (`left, top, right, bottom`).
/// `None` when even that window is too wide for the screen.
pub fn tall_for(picture: ImageBuffer<Rgba<u8>, Vec<u8>>, pic_x: i32, work: Rect4) -> Option<TallChosen> {
    let placement = crate::page_map::tall_placement(pic_x, picture.width() as i32, work, EDITOR_EDGE, tool_room())?;
    Some(TallChosen { picture, placement })
}

/// The editor window for a picture cut from `pic` (virtual-screen pixels)
/// on `monitor`, placed so the picture stays exactly where it was on
/// screen, pushed inward only as far as the monitor's edge needs.
///
/// The tools go on the picture's right. When that would shift the picture
/// (it sits near the monitor's right edge) they go on its left instead:
/// the window then has no room on the right, and the tool column and its
/// flyout flip over on their own. `None` when the picture with its tools
/// cannot fit on the monitor at all.
pub fn editor_placement(pic: Rect4, monitor: vdesk::MonitorRect) -> Option<EditorPlacement> {
    let (_, column_h) = tool_panel_dims(&EDITOR_SLOTS);
    let (mw, mh) = (monitor.width as i32, monitor.height as i32);
    // The column stands on the picture's bottom edge, so a picture shorter
    // than the column needs headroom for the rest of it.
    let above = EDITOR_EDGE + (column_h - pic.3).max(0);
    let height = above + pic.3 + action_room();
    if pic.2 <= 0 || pic.3 <= 0 || height > mh { return None; }
    let top = (pic.1 - above).clamp(monitor.y, monitor.y + mh - height);
    let fit = |left: i32, right: i32| {
        let width = left + pic.2 + right;
        (width <= mw).then(|| {
            let x = (pic.0 - left).clamp(monitor.x, monitor.x + mw - width);
            EditorPlacement { pos: (x, top), size: (width as u32, height as u32), picture_at: (pic.0 - x, pic.1 - top) }
        })
    };
    let unmoved = |p: &EditorPlacement| p.pos.0 + p.picture_at.0 == pic.0;
    let tools_right = fit(EDITOR_EDGE, tool_room());
    if let Some(p) = tools_right.filter(unmoved) { return Some(p); }
    fit(tool_room(), EDITOR_EDGE).filter(unmoved).or(tools_right)
}

/// Where a toast for a picture at `pic` lands (virtual-screen pixels): under
/// it, else just over its bottom edge, inside `monitor`.
pub fn toast_spot(pic: Rect4, monitor: vdesk::MonitorRect, size: (u32, u32)) -> (i32, i32) {
    spot_under(pic, (monitor.x, monitor.y, monitor.width as i32, monitor.height as i32), size)
}

/// A box `size` big centered under `r`, or tucked just inside `r`'s bottom
/// edge when `screen` ends first, always at least 12 px inside `screen`.
pub(super) fn spot_under(r: Rect4, screen: Rect4, size: (u32, u32)) -> (i32, i32) {
    const MARGIN: i32 = 12;
    let (w, h) = (size.0 as i32, size.1 as i32);
    let (sx, sy, sw, sh) = screen;
    let keep_in = |v: i32, start: i32, span: i32, len: i32| {
        v.clamp(start + MARGIN, (start + span - len - MARGIN).max(start + MARGIN))
    };
    let x = keep_in(r.0 + (r.2 - w) / 2, sx, sw, w);
    let under = r.1 + r.3 + MARGIN;
    let y = if under + h <= sy + sh - MARGIN { under } else { r.1 + r.3 - h - MARGIN };
    (x, keep_in(y, sy, sh, h))
}

/// Which sides of a rectangle an edge or corner grab moves, as
/// `(left, top, right, bottom)`.
fn sides(e: Edge) -> (bool, bool, bool, bool) {
    use Edge::*;
    (
        matches!(e, Left | TopLeft | BottomLeft),
        matches!(e, Top | TopLeft | TopRight),
        matches!(e, Right | TopRight | BottomRight),
        matches!(e, Bottom | BottomLeft | BottomRight),
    )
}

/// The selection `sel` with the grabbed `edge` moved to the pointer `p`.
/// Dragged past the opposite side it turns inside out rather than going
/// negative, and it never gets thinner than 4 px either way.
pub(super) fn resized(sel: Rect4, edge: Edge, p: (i32, i32)) -> Rect4 {
    let (l, t, r, b) = sides(edge);
    let (mut x0, mut y0, mut x1, mut y1) = (sel.0, sel.1, sel.0 + sel.2, sel.1 + sel.3);
    if l { x0 = p.0; }
    if r { x1 = p.0; }
    if t { y0 = p.1; }
    if b { y1 = p.1; }
    let (x, y) = (x0.min(x1), y0.min(y1));
    (x, y, (x1 - x0).abs().max(4), (y1 - y0).abs().max(4))
}

/// A crop being dragged in the editor window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct CropDrag {
    /// The crop when the button went down: Esc returns to it, and the undo
    /// step runs from it.
    pub before: Rect4,
    pub grab: CropGrab,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CropGrab {
    /// A fresh rectangle, from this corner.
    New((i32, i32)),
    /// One of the crop's edges or corners.
    Edge(Edge),
}

/// The crop for `drag` with the pointer at `p`: kept inside the picture
/// `pic`, an edge never closer than 8 px to the one opposite, and at least
/// 1 px each way (a sliver is undone when the button comes up).
pub(super) fn crop_rect(drag: CropDrag, p: (i32, i32), pic: Rect4) -> Rect4 {
    const NARROWEST: i32 = 8;
    let inside = |(x, y): (i32, i32)| (x.clamp(pic.0, pic.0 + pic.2), y.clamp(pic.1, pic.1 + pic.3));
    let (px, py) = inside(p);
    let b = drag.before;
    let [mut x0, mut y0, mut x1, mut y1] = [b.0, b.1, b.0 + b.2, b.1 + b.3];
    match drag.grab {
        CropGrab::New(start) => {
            let (ax, ay) = inside(start);
            (x0, y0, x1, y1) = (ax.min(px), ay.min(py), ax.max(px), ay.max(py));
        }
        CropGrab::Edge(e) => {
            let (l, t, r, btm) = sides(e);
            if l { x0 = px.min(x1 - NARROWEST); }
            if r { x1 = px.max(x0 + NARROWEST); }
            if t { y0 = py.min(y1 - NARROWEST); }
            if btm { y1 = py.max(y0 + NARROWEST); }
        }
    }
    let ((x0, y0), (x1, y1)) = (inside((x0, y0)), inside((x1, y1)));
    (x0, y0, (x1 - x0).max(1), (y1 - y0).max(1))
}

/// The part of `src` under `r`, clamped to the image and never smaller
/// than 1 px each way.
pub(crate) fn crop(src: &ImageBuffer<Rgba<u8>, Vec<u8>>, r: Rect4) -> ImageBuffer<Rgba<u8>, Vec<u8>> {
    let span = |start: i32, len: i32, size: u32| {
        let size = size as i32;
        let from = start.clamp(0, size);
        let to = (start + len).clamp(0, size);
        (from as u32, (to - from).max(1) as u32)
    };
    let (x, w) = span(r.0, r.2, src.width());
    let (y, h) = span(r.1, r.3, src.height());
    ImageBuffer::from_fn(w, h, |i, j| *src.get_pixel(x + i, y + j))
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::panels::tool_panel_origin;

    fn monitor(x: i32, y: i32, w: u32, h: u32) -> vdesk::MonitorRect {
        vdesk::MonitorRect { x, y, width: w, height: h }
    }

    /// Where the picture lands on screen once its window is placed.
    fn on_screen(p: &EditorPlacement) -> (i32, i32) { (p.pos.0 + p.picture_at.0, p.pos.1 + p.picture_at.1) }

    #[test]
    fn the_picture_opens_exactly_where_it_was_cut() {
        let screen = monitor(0, 0, 2560, 1440);
        let (column_w, column_h) = tool_panel_dims(&EDITOR_SLOTS);
        // Taller than the tool column: an even margin above and left.
        let p = editor_placement((600, 350, 900, column_h + 80), screen).unwrap();
        assert_eq!(on_screen(&p), (600, 350));
        assert_eq!(p.picture_at, (EDITOR_EDGE, EDITOR_EDGE));
        assert!(p.size.0 as i32 >= EDITOR_EDGE + 900 + PANEL_OFFSET + column_w, "room for the column");
        // Shorter: the window grows upward for the column, the picture stays.
        let p = editor_placement((600, 800, 900, 200), screen).unwrap();
        assert_eq!(on_screen(&p), (600, 800));
        assert_eq!(p.picture_at.1, EDITOR_EDGE + column_h - 200);
    }

    #[test]
    fn the_window_is_pushed_back_onto_its_monitor() {
        // A monitor left of and above the primary one, the picture against
        // its right and bottom edges.
        let m = monitor(-1080, -801, 1080, 1920);
        let p = editor_placement((-400, 900, 390, 200), m).unwrap();
        assert!(p.pos.0 >= m.x && p.pos.0 + p.size.0 as i32 <= m.x + m.width as i32);
        assert!(p.pos.1 >= m.y && p.pos.1 + p.size.1 as i32 <= m.y + m.height as i32);
        let (_, column_h) = tool_panel_dims(&EDITOR_SLOTS);
        assert!(p.size.1 as i32 >= column_h + 2 * EDITOR_EDGE, "tall enough for the column");
    }

    #[test]
    fn the_column_stands_on_the_bottom_right_corner() {
        let slots = &EDITOR_SLOTS;
        let (_, column_h) = tool_panel_dims(slots);
        let p = editor_placement((600, 700, 300, 120), monitor(0, 0, 2560, 1440)).unwrap();
        assert_eq!(on_screen(&p), (600, 700));
        let window = (0, 0, p.size.0 as i32, p.size.1 as i32);
        let pic = (p.picture_at.0, p.picture_at.1, 300, 120);
        let (_, top) = tool_panel_origin(slots, window, pic);
        assert_eq!(top + column_h, pic.1 + pic.3, "level with the picture's foot");
        assert!(top >= 0);
        // On the frozen desktop too, for a tall selection.
        let desk = (0, 0, 2560, 1440);
        assert_eq!(tool_panel_origin(slots, desk, (100, 100, 900, 1000)).1 + column_h, 1100);
        // Near the top of the screen it is held on screen and runs down.
        assert_eq!(tool_panel_origin(slots, desk, (100, 10, 400, 80)).1, 0);
    }

    #[test]
    fn too_big_or_empty_pictures_stay_on_the_frozen_desktop() {
        let m = monitor(0, 0, 2560, 1440);
        assert_eq!(editor_placement((0, 0, 2560, 1440), m), None);
        assert_eq!(editor_placement((0, 0, 0, 10), m), None);
    }

    #[test]
    fn near_the_right_edge_the_tools_move_to_the_left() {
        let p = editor_placement((1720, 300, 800, 500), monitor(0, 0, 2560, 1440)).unwrap();
        assert_eq!(on_screen(&p).0, 1720);
        assert!(p.picture_at.0 > EDITOR_EDGE, "the tools are on its left");
        assert!(p.pos.0 + p.size.0 as i32 <= 2560);
    }

    #[test]
    fn a_picture_cut_short_or_off_every_monitor_is_not_handed_off() {
        let screens = [monitor(0, 0, 1920, 1080)];
        let picture = |w, h| ImageBuffer::from_pixel(w, h, Rgba([1, 2, 3, 255]));
        assert!(chosen_for(picture(300, 200), (100, 100, 300, 200), &screens).is_some());
        assert!(chosen_for(picture(299, 200), (100, 100, 300, 200), &screens).is_none());
        assert!(chosen_for(picture(300, 200), (5000, 100, 300, 200), &screens).is_none());
    }

    #[test]
    fn toasts_go_under_the_picture_or_over_its_foot() {
        let m = monitor(0, 0, 1920, 1080);
        assert_eq!(toast_spot((100, 100, 400, 300), m, (180, 32)), (210, 412));
        let (_, y) = toast_spot((100, 900, 400, 170), m, (180, 32));
        assert_eq!(y, 900 + 170 - 32 - 12);
        // Never past the monitor's margin, even for a picture off its left.
        assert_eq!(toast_spot((-500, 100, 100, 100), m, (180, 32)).0, 12);
    }

    #[test]
    fn a_crop_stays_inside_the_picture_and_its_edges_move() {
        let pic = (10, 10, 400, 300);
        let fresh = CropDrag { before: pic, grab: CropGrab::New((50, 60)) };
        assert_eq!(crop_rect(fresh, (900, 900), pic), (50, 60, 360, 250), "stops at the picture's edge");
        assert_eq!(crop_rect(fresh, (20, 20), pic), (20, 20, 30, 40), "up and left of the start");
        let right = CropDrag { before: (50, 60, 200, 100), grab: CropGrab::Edge(Edge::Right) };
        assert_eq!(crop_rect(right, (150, 999), pic), (50, 60, 100, 100));
        assert_eq!(crop_rect(right, (0, 0), pic).2, 8, "an edge can't pass the other one");
        let corner = CropDrag { before: (50, 60, 200, 100), grab: CropGrab::Edge(Edge::TopLeft) };
        assert_eq!(crop_rect(corner, (30, 40), pic), (30, 40, 220, 120));
    }

    #[test]
    fn a_dragged_edge_turns_the_selection_inside_out_not_negative() {
        let sel = (100, 100, 200, 100);
        assert_eq!(resized(sel, Edge::Right, (250, 0)), (100, 100, 150, 100));
        assert_eq!(resized(sel, Edge::Left, (350, 0)), (300, 100, 50, 100), "past the right side");
        assert_eq!(resized(sel, Edge::BottomRight, (101, 101)), (100, 100, 4, 4), "never thinner than 4 px");
        assert_eq!(resized(sel, Edge::None, (0, 0)), sel);
        assert_eq!(resized(sel, Edge::TopLeft, (90, 80)), (90, 80, 210, 120));
    }

    #[test]
    fn crops_are_clamped_to_the_image() {
        let img = ImageBuffer::from_fn(10, 8, |x, y| Rgba([x as u8, y as u8, 0, 255]));
        let c = crop(&img, (-3, 2, 6, 100));
        assert_eq!(c.dimensions(), (3, 6));
        assert_eq!(c.get_pixel(0, 0).0, [0, 2, 0, 255]);
        // An empty rectangle still gives one pixel.
        assert_eq!(crop(&img, (4, 4, 0, -2)).dimensions(), (1, 1));
    }

    #[test]
    fn rectangles_are_half_open() {
        assert!(rect_contains((0, 0, 4, 4), (3, 3)));
        assert!(!rect_contains((0, 0, 4, 4), (4, 0)));
        assert_eq!(rect_from((10, 2), (4, 8)), (4, 2, 6, 6));
    }
}
