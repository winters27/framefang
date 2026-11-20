//! The capture screen before a region exists: the dock that names the
//! capture modes and the keys, the highlight on what a click would take,
//! and the terminal-style look of the reading modes (TEXT and QR, and
//! SCROLL's hint), all in the app's HUD style and Settings' color scheme.

use std::time::Duration;

use framefang_core::color::Rgba as Color;

use super::flyout::FLYOUT_TEXT_PX;
use super::geometry::Rect4;
use super::paint::{color, mix_rgb, Canvas};
use super::chrome::SELECTION_BLUE;
use crate::painter;
use crate::palette::*;
use crate::pick::{Dock, Look, PickMode, DOCK_BAR_W, DOCK_SCALE};

/// The dock's animation state, as far as how it looks goes: the step the
/// sweep and the decode are on, Tab held down, the cursor lit. `tick`
/// redraws only when this changes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct DockFrame {
    pub sweep: Option<u32>,
    pub decode: Option<u32>,
    pub pressed: bool,
    pub cursor_on: bool,
}

/// What the dock draws this frame: how far the opening sweep has gone (0
/// to 1), how long the new mode has been decoding, Tab held down, the
/// cursor lit.
#[derive(Debug, Clone, Copy)]
pub(super) struct DockAnim {
    pub sweep: Option<f32>,
    pub decode: Option<Duration>,
    pub pressed: bool,
    pub cursor_on: bool,
}

fn key(c: &mut Canvas, r: Rect4, name: &str, pressed: bool) {
    crate::keycap::draw_key(c.px, c.w, c.h, r.0, r.1, name, DOCK_SCALE, pressed);
}

fn pixel_text(c: &mut Canvas, x: i32, y: i32, words: &str, rgb: u32) {
    painter::draw_text(&mut c.surface(), x, y, DOCK_SCALE, words, color(rgb));
}

/// The dock (`pick::dock_layout`): black with an accent bar down its left,
/// the Tab key, `//`, a chip per mode (the current one filled with the
/// accent and, right after a switch, decoding into its name), a divider,
/// the finishing keys with their words, and a blinking cursor. As it opens
/// a soft band of the accent sweeps across it once.
pub(super) fn draw_dock(c: &mut Canvas, dock: &Dock, picked: PickMode, hovered: Option<PickMode>, anim: &DockAnim) {
    let (bx, by, bw, bh) = dock.rect;
    let glyph_h = crate::bitmap_font::GLYPH_H * DOCK_SCALE;
    let text_y = dock.text_y;
    c.fill(bx, by, bx + bw, by + bh, 0);
    c.outline(bx, by, bx + bw, by + bh, FIELD_BORDER());
    c.fill(bx, by, bx + DOCK_BAR_W, by + bh, LASER());
    key(c, dock.tab, "TAB", anim.pressed);
    pixel_text(c, dock.slashes_x, text_y, "//", TEXT_DIM());
    for &(mode, (x, y, w, h)) in &dock.chips {
        let current = mode == picked;
        let hot = !current && hovered == Some(mode);
        if current {
            c.fill(x, y, x + w, y + h, LASER());
        } else {
            if hot { c.fill(x, y, x + w, y + h, HOVER_FILL()); }
            c.outline(x, y, x + w, y + h, if hot { LASER_DIM() } else { FIELD_BORDER() });
        }
        let label = match anim.decode.filter(|_| current) {
            Some(since) => crate::pick::decoding(mode.dock_label(), since),
            None => mode.dock_label().to_owned(),
        };
        let ink = if current { 0 } else if hot { TEXT_BRIGHT } else { TEXT_MUTED() };
        let label_w = crate::bitmap_font::measure(&label, DOCK_SCALE);
        pixel_text(c, x + (w - label_w) / 2, y + (h - glyph_h) / 2, &label, ink);
    }
    c.fill(dock.divider_x, text_y - 4, dock.divider_x + 1, text_y + glyph_h + 4, FIELD_BORDER());
    for (name, words, r, words_x) in &dock.keys {
        key(c, *r, name, false);
        pixel_text(c, *words_x, text_y, words, TEXT_MUTED());
    }
    if anim.cursor_on { c.fill_rect(dock.cursor, LASER()); }

    // The opening sweep: a band eased across from left to right, so its
    // travel can be followed, lightening what it passes.
    let Some(k) = anim.sweep else { return };
    let k = k.clamp(0.0, 1.0);
    let center = -0.3 + 1.4 * (k * k * (3.0 - 2.0 * k));
    const HALF: f32 = 0.15;
    let accent = LASER();
    let (x_lo, x_hi) = (bx.max(0), (bx + bw).min(c.w as i32));
    for y in by.max(0)..(by + bh).min(c.h as i32) {
        for x in x_lo..x_hi {
            let along = (x - bx) as f32 / bw.max(1) as f32;
            let strength = 0.55 * (1.0 - (along - center).abs() / HALF);
            if strength > 0.0 {
                let i = y as usize * c.w + x as usize;
                c.px[i] = mix_rgb(c.px[i], accent, strength);
            }
        }
    }
}

/// The hover highlight: a crisp 2 px frame in the selection's blue (so a
/// click looks like it simply keeps what is lit) with a soft halo outside,
/// and a tab naming what a click takes and how big it is (`size`, the
/// target's own, not the glide's). The tab sits on the frame's top edge,
/// outside it, so it never covers what is captured; a target at the top
/// of the screen has no room there and gets it just inside its corner.
pub(super) fn draw_target(c: &mut Canvas, r: Rect4, label: &str, size: (i32, i32)) {
    const PAD_X: i32 = 10;
    const TAB_H: i32 = 26;
    if r.2 <= 0 || r.3 <= 0 { return; }
    let (x0, y0, x1, y1) = (r.0, r.1, r.0 + r.2, r.1 + r.3);
    for (d, alpha) in [(3, 22), (2, 50), (1, 95)] {
        c.blend_ring(x0 - d, y0 - d, x1 + d, y1 + d, SELECTION_BLUE, alpha);
    }
    c.outline(x0, y0, x1, y1, SELECTION_BLUE);
    c.outline(x0 + 1, y0 + 1, x1 - 1, y1 - 1, SELECTION_BLUE);

    let dims = format!("{} x {}", size.0, size.1);
    let width = |s: &str| framefang_core::text::layout(s, FLYOUT_TEXT_PX).width().ceil() as i32;
    let (name_w, dims_w) = (width(label), width(&dims));
    let tab_w = 2 * PAD_X + name_w + 12 + dims_w;
    let (tx, ty) = if y0 >= TAB_H { (x0, y0 - TAB_H) } else { (x0 + 2, y0 + 2) };
    let tx = tx.clamp(0, (c.w as i32 - tab_w).max(0));
    let ty = ty.clamp(0, (c.h as i32 - TAB_H).max(0));
    c.fill(tx, ty, tx + tab_w, ty + TAB_H, SELECTION_BLUE);
    let line_y = ty as f32 + (TAB_H as f32 - framefang_core::text::line_height(FLYOUT_TEXT_PX)) / 2.0;
    let name_x = (tx + PAD_X) as f32;
    let mut surface = c.surface();
    painter::draw_unicode_text(&mut surface, name_x, line_y, FLYOUT_TEXT_PX, label, Color::new_opaque(0xFF, 0xFF, 0xFF));
    // The size a step quieter than the name.
    painter::draw_unicode_text(&mut surface, name_x + (name_w + 12) as f32, line_y, FLYOUT_TEXT_PX, &dims,
                               Color::new_opaque(0xD8, 0xE3, 0xFA));
}

/// How often the reading look's scan line steps, and how long one pass
/// down the box takes.
pub(super) const READ_STEP: Duration = Duration::from_millis(40);
const READ_SCAN: Duration = Duration::from_millis(900);

/// SCROLL's words beside the highlight: what a click there does (the part
/// that scrolls is lit, or the whole window when nothing does).
pub(super) const SCROLL_HINT: &str = "click, then scroll it yourself";

/// What the reading look draws this frame.
#[derive(Debug, Clone, Copy)]
pub(super) struct ReadLook {
    pub kind: Look,
    /// The prompt's cursor is lit.
    pub cursor_on: bool,
    /// How long the picture has been read, once it is.
    pub reading: Option<Duration>,
    /// QR: `(links open, the setting opens them)`, so the prompt can say
    /// when letting go now does something other than copy.
    pub links: (bool, bool),
}

/// The prompt naming a reading mode. QR adds a flag, as on a command line,
/// when its links won't just be copied: `--open` while they open, `--copy`
/// while the switch key turns an Open setting into a copy.
fn read_prompt(kind: Look, (open, setting_opens): (bool, bool)) -> &'static str {
    match (kind, open, setting_opens) {
        (Look::Text, ..) => "> OCR",
        (Look::Qr, true, _) => "> QR --open",
        (Look::Qr, false, true) => "> QR --copy",
        (Look::Qr, false, false) => "> QR",
        (Look::Scroll, ..) => "> SCROLL",
    }
}

/// A black HUD tag with its top left at `(x, y)`, kept on the window:
/// `prompt` in the accent in the pixel font, a block cursor after it when
/// `cursor`, then `name` a step quieter in the smooth face.
fn draw_prompt_tag(c: &mut Canvas, x: i32, y: i32, prompt: &str, cursor: bool, name: Option<&str>) {
    const SCALE: i32 = 2;
    const PAD_X: i32 = 8;
    const PAD_Y: i32 = 6;
    // The dock's gap before its cursor, so it reads as a cursor and not as
    // part of the last letter.
    const CURSOR_GAP: i32 = 8;
    let glyph_h = crate::bitmap_font::GLYPH_H * SCALE;
    let cursor_w = crate::bitmap_font::GLYPH_W * SCALE;
    let prompt_w = crate::bitmap_font::measure(prompt, SCALE);
    let name = name.filter(|n| !n.is_empty());
    let name_room = name.map_or(0, |n| 12 + framefang_core::text::layout(n, FLYOUT_TEXT_PX).width().ceil() as i32);
    let (w, h) = (2 * PAD_X + prompt_w + CURSOR_GAP + cursor_w + name_room, glyph_h + 2 * PAD_Y);
    let x = x.clamp(0, (c.w as i32 - w).max(0));
    let y = y.clamp(0, (c.h as i32 - h).max(0));
    c.fill(x, y, x + w, y + h, 0);
    c.outline(x, y, x + w, y + h, LASER_DIM());
    let (tx, ty) = (x + PAD_X, y + PAD_Y);
    painter::draw_text(&mut c.surface(), tx, ty, SCALE, prompt, color(LASER()));
    let cursor_x = tx + prompt_w + CURSOR_GAP;
    if cursor { c.fill(cursor_x, ty, cursor_x + cursor_w, ty + glyph_h, LASER()); }
    if let Some(n) = name {
        let ny = y as f32 + (h as f32 - framefang_core::text::line_height(FLYOUT_TEXT_PX)) / 2.0;
        painter::draw_unicode_text(&mut c.surface(), (cursor_x + cursor_w + 12) as f32, ny, FLYOUT_TEXT_PX, n, color(TEXT_MUTED()));
    }
}

/// The reading modes' look, on the selection or the hover highlight: a
/// dashed accent outline with solid corner brackets, a prompt naming the
/// mode over its top left ("> OCR" with a blinking cursor, then the
/// window's name on a highlight), a reticle in the middle for QR, and
/// while the picture is read a scan line passing down it and "reading..."
/// under it.
pub(super) fn draw_reading_frame(c: &mut Canvas, r: Rect4, look: &ReadLook, name: Option<&str>) {
    if r.2 <= 0 || r.3 <= 0 { return; }
    let (x0, y0, x1, y1) = (r.0, r.1, r.0 + r.2, r.1 + r.3);
    let accent = LASER();
    // The scan line with a soft tail above it, under the outline.
    if let Some(t) = look.reading {
        const TAIL: i32 = 22;
        let pass = (t.as_millis() % READ_SCAN.as_millis()) as f32 / READ_SCAN.as_millis() as f32;
        let line_y = y0 + (pass * r.3 as f32) as i32;
        let (x_lo, x_hi) = (x0.max(0), x1.min(c.w as i32));
        for y in (line_y - TAIL).max(y0).max(0)..=line_y.min(y1 - 1).min(c.h as i32 - 1) {
            let fade = 1.0 - (line_y - y) as f32 / TAIL as f32;
            let strength = if y == line_y { 0.85 } else { 0.5 * fade * fade };
            for x in x_lo..x_hi {
                let i = y as usize * c.w + x as usize;
                c.px[i] = mix_rgb(c.px[i], accent, strength);
            }
        }
    }
    c.dashed(x0, y0, x1, y1, accent);
    // Corner brackets, 2 px thick, so where the box ends reads on any
    // picture.
    let arm = 12.min(r.2 / 2).min(r.3 / 2);
    for (cx, cy, right, down) in [(x0, y0, true, true), (x1 - 1, y0, false, true), (x0, y1 - 1, true, false), (x1 - 1, y1 - 1, false, false)] {
        let (along_x0, along_x1) = if right { (cx, cx + arm) } else { (cx - arm + 1, cx + 1) };
        let (along_y0, along_y1) = if down { (cy, cy + arm) } else { (cy - arm + 1, cy + 1) };
        let (thick_y0, thick_y1) = if down { (cy, cy + 2) } else { (cy - 1, cy + 1) };
        let (thick_x0, thick_x1) = if right { (cx, cx + 2) } else { (cx - 1, cx + 1) };
        c.fill(along_x0, thick_y0, along_x1, thick_y1, accent);
        c.fill(thick_x0, along_y0, thick_x1, along_y1, accent);
    }
    // QR: a reticle in the middle, open in its center so it hides nothing
    // of the code.
    if look.kind == Look::Qr && r.2 > 60 && r.3 > 60 {
        let (mx, my, quiet) = (x0 + r.2 / 2, y0 + r.3 / 2, LASER_DIM());
        for (a, b) in [(-16, -5), (5, 16)] {
            c.fill(mx + a, my, mx + b, my + 1, quiet);
            c.fill(mx, my + a, mx + 1, my + b, quiet);
        }
    }
    let tag_h = crate::bitmap_font::GLYPH_H * 2 + 12;
    let room_above = y0 - tag_h - 4 >= 0;
    let (tag_x, tag_y) = if room_above { (x0, y0 - tag_h - 4) } else { (x0 + 4, y0 + 4) };
    let blink = look.cursor_on && look.reading.is_none();
    draw_prompt_tag(c, tag_x, tag_y, read_prompt(look.kind, look.links), blink, name);
    if let Some(t) = look.reading {
        // Padded, so the tag keeps its width while the dots count.
        let dots = (t.as_millis() / 300) as usize % 4;
        let words = format!("reading{:<3}", ".".repeat(dots));
        let below = y1 + 4 + tag_h <= c.h as i32;
        let (sx, sy) = if below { (x0, y1 + 4) } else { (x0 + 4, y1 - tag_h - 4) };
        draw_prompt_tag(c, sx, sy, &words, false, None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qr_says_when_a_link_will_not_just_be_copied() {
        assert_eq!(read_prompt(Look::Qr, (false, false)), "> QR");
        assert_eq!(read_prompt(Look::Qr, (true, false)), "> QR --open");
        assert_eq!(read_prompt(Look::Qr, (true, true)), "> QR --open");
        assert_eq!(read_prompt(Look::Qr, (false, true)), "> QR --copy");
        assert_eq!(read_prompt(Look::Text, (true, true)), "> OCR");
        assert_eq!(read_prompt(Look::Scroll, (false, false)), "> SCROLL");
    }

    #[test]
    fn the_target_tab_stays_on_screen_at_the_top() {
        let (w, h) = (400usize, 300usize);
        let mut px = vec![0u32; w * h];
        draw_target(&mut Canvas::new(&mut px, w, h), (0, 0, 400, 300), "Screen", (400, 300));
        // No room above: the tab hangs inside the top left corner.
        assert_eq!(px[5 * w + 5], SELECTION_BLUE);
        let mut px = vec![0u32; w * h];
        draw_target(&mut Canvas::new(&mut px, w, h), (50, 0, 0, 10), "Nothing", (0, 10));
        assert!(px.iter().all(|&p| p == 0), "an empty target draws nothing");
    }
}
