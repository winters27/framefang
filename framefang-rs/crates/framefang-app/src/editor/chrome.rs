//! Drawing the editor's chrome: the tool column and its glow, the action
//! row and its pulse, a tool's flyout, tips, notice bars, the magnifier,
//! the eyedropper's loupe and the size chip. Layout lives in `panels` and
//! `flyout`; this only paints what they place.

use framefang_core::annotation::{Palettes, Point2, Rect};
use framefang_core::color::Rgba as Color;
use framefang_core::mark::{MarkLook, MarkStyle};
use framefang_core::step::{StepLook, StepPart, StepShape};
use framefang_core::tool::Tool;
use image::{ImageBuffer, Rgba};

use super::flyout::{
    shadow_switch, slider_track, spectrum_color, step_tile_number, to_hsl, FlyoutLayout, Slider,
    FLYOUT_POINTER, FLYOUT_TEXT_PX, KNOB_W,
};
use super::geometry::Rect4;
use super::paint::{color, word, Canvas};
use super::panels::*;
use super::NEON_GREEN;
use crate::icons::{render_icon, IconKind};
use crate::painter;

type Image = ImageBuffer<Rgba<u8>, Vec<u8>>;

/// The panels' surface.
pub(super) const PANEL_BG: u32 = 0x00_22_22_24;
/// A button at rest.
const BUTTON: u32 = 0x00_2E_2E_32;
/// The tool in hand, and the selection's frame.
pub(super) const SELECTION_BLUE: u32 = 0x00_64_95_ED;
/// Icons on the panels.
const ICON_INK: [u8; 4] = [0xE8, 0xE8, 0xEC, 0xFF];
/// Light text on the panels.
const BRIGHT: u32 = 0x00_E8_E8_EC;
/// A hairline around swatches and chips.
const HAIRLINE: u32 = 0x00_3A_3A_40;
/// The dark behind tips, lenses and notices.
const INK_BLACK: u32 = 0x00_10_10_14;

/// The action row's pulse: three slow breaths, ending dark.
pub(super) const PULSE_HZ: f32 = 1.25;
pub(super) const PULSE_SECS: f32 = 3.0 / PULSE_HZ;

/// The magnifier before a region is picked: 17 source pixels across at 7x.
const MAG_ZOOM: i32 = 7;
const MAG_REACH: i32 = 8;
const MAG_PIXELS: i32 = 2 * MAG_REACH + 1;
const MAG_SIZE: i32 = MAG_PIXELS * MAG_ZOOM;
/// From the pointer to the lens's corner.
const MAG_OFFSET: i32 = 24;

/// The eyedropper's loupe: 17 source pixels across at 9x, beside a card.
const LOUPE_PIXELS: i32 = 17;
const LOUPE_ZOOM: i32 = 9;
const LOUPE_LENS: i32 = LOUPE_PIXELS * LOUPE_ZOOM + 2;
const LOUPE_CARD_W: i32 = 184;
const LOUPE_GAP: i32 = 6;

fn icon(c: &mut Canvas, r: Rect4, kind: IconKind, accent: Option<[u8; 4]>) {
    render_icon(c.px, c.w, c.h, r.0, r.1, r.0 + r.2, r.1 + r.3, kind, ICON_INK, accent);
}

/// A hovered button's fill, a step up from `BUTTON`.
const BUTTON_HOVER: u32 = 0x00_40_40_48;
/// The tool in hand, hovered.
const SELECTION_BLUE_HOVER: u32 = 0x00_82_AC_F5;

/// `from` blended toward `to` by `t` (0 to 1), channel by channel.
fn mix(from: u32, to: u32, t: f32) -> u32 {
    let channel = |shift: u32| {
        let (a, b) = (((from >> shift) & 0xFF) as f32, ((to >> shift) & 0xFF) as f32);
        ((a + (b - a) * t.clamp(0.0, 1.0)).round() as u32) << shift
    };
    channel(16) | channel(8) | channel(0)
}

/// One panel button. `hover` (0 at rest, 1 fully hovered, eased by the
/// caller) lifts its fill a step, draws an edge in the color scheme's accent
/// and, past halfway, grows it a pixel each way, so the button the pointer
/// is on is plain at a glance. The icon keeps its box, so it never jumps.
fn draw_button(c: &mut Canvas, r: Rect4, lit: bool, hover: f32, kind: IconKind, accent: Option<[u8; 4]>) {
    let (rest, raised) = if lit { (SELECTION_BLUE, SELECTION_BLUE_HOVER) } else { (BUTTON, BUTTON_HOVER) };
    let grow = i32::from(hover >= 0.5);
    let face = (r.0 - grow, r.1 - grow, r.2 + 2 * grow, r.3 + 2 * grow);
    c.fill_rect(face, mix(rest, raised, hover));
    let edge = (255.0 * hover).round() as u32;
    if edge > 0 {
        c.blend_ring(face.0, face.1, face.0 + face.2 - 1, face.1 + face.3 - 1, crate::palette::LASER(), edge);
    }
    icon(c, r, kind, accent);
}

/// The tool column, the tool in hand lit (none while no tool is).
/// `hover(i)` is how hovered button `i` is.
pub(super) fn draw_tool_panel(c: &mut Canvas, slots: &[ToolSlot], bounds: Rect4, sel: Rect4, active: Option<Tool>,
                              hover: impl Fn(usize) -> f32) {
    const DIVIDER: u32 = 0x00_44_44_48;
    let origin = tool_panel_origin(slots, bounds, sel);
    let (w, h) = tool_panel_dims(slots);
    c.fill_rect((origin.0, origin.1, w, h), PANEL_BG);
    let line = divider_y(slots, origin);
    c.fill(origin.0 + 6, line, origin.0 + w - 6, line + 1, DIVIDER);
    for (i, slot) in slots.iter().enumerate() {
        let lit = matches!(slot, ToolSlot::Tool(t) if active == Some(*t));
        let kind = match slot {
            ToolSlot::Tool(t) => IconKind::Tool(*t),
            ToolSlot::Undo => IconKind::Undo,
            ToolSlot::Redo => IconKind::Redo,
        };
        draw_button(c, slot_rect(slots, origin, i), lit, hover(i), kind, None);
    }
}

/// An orange glow stepping down the column, a second per button, so the
/// palette looks alive while the pointer rests. Each button glows for the
/// first 85 % of its second (a quick rise, a hold, a fade) and rests for
/// the rest, so the step to the next button shows. The glow stays inside
/// the column's padding: in the editor window anything past the column's
/// edge lands on the cut-away canvas, where a glow reads as a dark box.
pub(super) fn draw_tool_panel_glow(c: &mut Canvas, slots: &[ToolSlot], bounds: Rect4, sel: Rect4, secs: f32) {
    const ORANGE: u32 = 0x00_FF_7A_1A;
    let t = secs.rem_euclid(slots.len() as f32);
    let (index, phase) = (t.floor() as usize, t.fract());
    let strength = match phase {
        p if p > 0.85 => return,
        p if p < 0.15 => p / 0.15,
        p if p < 0.55 => 1.0,
        p => ((0.85 - p) / 0.30).max(0.0),
    };
    let (x, y, w, h) = slot_rect(slots, tool_panel_origin(slots, bounds, sel), index);
    let peak = (210.0 * strength).round().clamp(0.0, 255.0) as u32;
    for ring in 0..PANEL_PAD {
        let alpha = peak / (1 + ring as u32 / 2);
        let d = ring + 1;
        c.blend_ring(x - d, y - d, x + w + d - 1, y + h + d - 1, ORANGE, alpha.min(255));
    }
}

/// The action row's brightness `t` seconds after it appeared, 0 to 1: a
/// raised cosine, dark at both ends of each breath and full in its middle.
pub(super) fn pulse_level(t: f32) -> f32 {
    if !(0.0..PULSE_SECS).contains(&t) { return 0.0; }
    (1.0 - (std::f32::consts::TAU * PULSE_HZ * t).cos()) / 2.0
}

/// A green outline breathing around the action row when it first appears,
/// so the eye finds where Save, Copy and Pin went: three slow breaths
/// (Brandon found the old 6.5 Hz strobe far too fast). A 3 px stroke hugs
/// the row; with `halo`, five fainter rings glow outside it. Without
/// (the editor window, where a blend past the row lands on the cut-away
/// canvas and reads as a dark box) the stroke stays solid and dims by
/// brightness, like a neon tube.
pub(super) fn draw_action_panel_pulse(c: &mut Canvas, bounds: Rect4, sel: Rect4, secs: f32, actions: &[Action], halo: bool) {
    if secs >= PULSE_SECS { return; }
    let level = pulse_level(secs);
    let (ox, oy) = action_panel_origin(bounds, sel, actions);
    let (w, h) = action_panel_dims(actions);
    let (green, stroke_alpha) = if halo {
        (NEON_GREEN, (240.0 * level).round() as u32)
    } else {
        // So nearly dark it draws nothing, and the canvas there is cut away
        // again.
        if level < 0.06 { return; }
        let dim = |shift: u32| (((NEON_GREEN >> shift) & 0xFF) as f32 * level).round() as u32;
        ((dim(16) << 16) | (dim(8) << 8) | dim(0), 255)
    };
    let ring = |c: &mut Canvas, d: i32, alpha: u32| c.blend_ring(ox - d, oy - d, ox + w + d, oy + h + d, green, alpha);
    for d in 1..=3 { ring(c, d, stroke_alpha); }
    if !halo { return; }
    let glow = (130.0 * level).round() as u32;
    for i in 0..5 { ring(c, 4 + i, (glow / (1 + i as u32)).min(255)); }
}

/// The action row. `hover(i)` is how hovered button `i` is.
pub(super) fn draw_action_panel(c: &mut Canvas, bounds: Rect4, sel: Rect4, actions: &[Action], hover: impl Fn(usize) -> f32) {
    // The red of the recording indicator's dot, so Record is never taken
    // for Save.
    const RECORD_RED: [u8; 4] = [0xFF, 0x3A, 0x3A, 0xFF];
    let origin = action_panel_origin(bounds, sel, actions);
    let (w, h) = action_panel_dims(actions);
    c.fill_rect((origin.0, origin.1, w, h), PANEL_BG);
    for (i, a) in actions.iter().enumerate() {
        let (kind, accent) = match a {
            Action::Pin => (IconKind::Pin, None),
            Action::Copy => (IconKind::Copy, None),
            Action::Save => (IconKind::Save, None),
            Action::Record => (IconKind::Record, Some(RECORD_RED)),
            Action::Close => (IconKind::Close, None),
        };
        draw_button(c, action_rect(origin, i), false, hover(i), kind, accent);
    }
}

/// Everything the flyout shows this frame.
pub(super) struct FlyoutFrame {
    pub rect: Rect4,
    /// It hangs off the column's right side (else its left).
    pub on_right: bool,
    /// The tool button's middle, down from the flyout's top.
    pub pointer_y: i32,
    pub layout: FlyoutLayout,
    /// One value per slider row.
    pub values: Vec<f32>,
    pub color: Color,
    pub palette: usize,
    /// How far out it has slid, 0 to 1, eased.
    pub open: f32,
    /// The eyedropper is armed.
    pub picking: bool,
    /// Where the color was last taken from the spectrum.
    pub spectrum_at: Option<(f32, f32)>,
    /// Step: its look and the part the colors paint.
    pub step: Option<(StepLook, StepPart)>,
    /// Highlight: the look the next highlight takes.
    pub mark: Option<MarkLook>,
}

/// The flyout, sliding out from under the tool column: drawn whole off to
/// the side, then the revealed share copied next to the column, the edge
/// nearest the column last, so it reads as a drawer pulled out rather than
/// a box fading in.
pub(super) fn draw_flyout(out: &mut Canvas, f: &FlyoutFrame) {
    let (fx, fy, fw, fh) = f.rect;
    if fw <= 0 || fh <= 0 { return; }
    let (sw, sh) = (fw as usize, fh as usize);
    let mut scratch = vec![PANEL_BG; sw * sh];
    paint_flyout(&mut Canvas::new(&mut scratch, sw, sh), f);

    let shown = (fw as f32 * f.open.clamp(0.0, 1.0)).round() as i32;
    if shown <= 0 { return; }
    let (from_x, to_x) = if f.on_right { (fw - shown, fx) } else { (0, fx + fw - shown) };
    // Columns of the revealed share that land on the window.
    let first = (-to_x).max(0);
    let last = shown.min(out.w as i32 - to_x);
    if first < last {
        for y in 0..fh {
            let dy = fy + y;
            if dy < 0 || dy as usize >= out.h { continue; }
            let src = y as usize * sw + (from_x + first) as usize;
            let dst = dy as usize * out.w + (to_x + first) as usize;
            let n = (last - first) as usize;
            out.px[dst..dst + n].copy_from_slice(&scratch[src..src + n]);
        }
    }

    // Once fully out, a small pointer across the gap, level with the
    // tool's button, says whose flyout it is.
    if f.open < 0.98 { return; }
    let mid = fy + f.pointer_y.clamp(PANEL_RADIUS + FLYOUT_POINTER, fh - PANEL_RADIUS - FLYOUT_POINTER);
    for step in 0..FLYOUT_POINTER {
        let x = if f.on_right { fx - 1 - step } else { fx + fw + step };
        let reach = FLYOUT_POINTER - 1 - step;
        out.fill(x, mid - reach, x + 1, mid + reach + 1, PANEL_BG);
    }
}

/// The flyout's insides, on a canvas exactly its size.
fn paint_flyout(s: &mut Canvas, f: &FlyoutFrame) {
    const ARMED: u32 = SELECTION_BLUE;
    const GROOVE: u32 = 0x00_14_14_17;
    const REST: u32 = 0x00_44_44_4A;
    const MUTED: u32 = 0x00_9C_9C_A4;
    let ink = word(f.color);
    let text_w = |words: &str| framefang_core::text::layout(words, FLYOUT_TEXT_PX).width();
    // Words in the flyout's own type, centered down `r`, from `x` across.
    let write = |s: &mut Canvas, r: Rect4, x: f32, words: &str, rgb: u32| {
        let y = r.1 as f32 + (r.3 as f32 - framefang_core::text::line_height(FLYOUT_TEXT_PX)) / 2.0;
        painter::draw_unicode_text(&mut s.surface(), x, y, FLYOUT_TEXT_PX, words, color(rgb));
    };
    // A white ring around the picked tile, with a gap of panel inside it.
    let ring_pick = |s: &mut Canvas, r: Rect4| {
        s.fill(r.0 - 2, r.1 - 2, r.0 + r.2 + 2, r.1 + r.3 + 2, BRIGHT);
        s.fill_rect(r, PANEL_BG);
    };

    if let (Some(tiles), Some(look)) = (&f.layout.looks, f.mark) {
        for (i, &r) in tiles.iter().enumerate() {
            let style = MarkStyle::ALL[i];
            let inset = if style == look.style { ring_pick(s, r); 1 } else { 0 };
            let tile = (r.0 + inset, r.1 + inset, r.2 - 2 * inset, r.3 - 2 * inset);
            draw_mark_tile(s, tile, &MarkLook { style, seed: 3 + i as u32, ..look });
        }
    }

    if let (Some(panel), Some((look, part))) = (&f.layout.step, f.step) {
        const TILE: u32 = 0x00_1A_1A_1D;
        // Each shape as it would come out; the picked one ringed like a
        // picked swatch.
        for (i, &r) in panel.tiles.iter().enumerate() {
            let shape = StepShape::ALL[i];
            let picked = shape == look.shape;
            if picked {
                ring_pick(s, r);
                s.fill(r.0 + 1, r.1 + 1, r.0 + r.2 - 1, r.1 + r.3 - 1, TILE);
            } else {
                s.fill_rect(r, TILE);
            }
            let shown = if picked { look } else { look.with_shape(shape) };
            let n = step_tile_number(i);
            let (left, top, right, bottom) = framefang_core::step::extent(n, &shown);
            let cx = r.0 as f32 + r.2 as f32 / 2.0 - (left + right) / 2.0;
            let cy = r.1 as f32 + r.3 as f32 / 2.0 - (top + bottom) / 2.0;
            painter::step_marker(&mut s.surface(), Point2::new(cx.round(), cy.round()), n, &shown);
        }
        // The parts, each with a dot of its color. A disc has no ring.
        for (i, &r) in panel.parts.iter().enumerate() {
            let p = StepPart::ALL[i];
            let usable = p != StepPart::Ring || look.shape.has_ring();
            s.fill_rect(r, if usable && p == part { ARMED } else { BUTTON });
            const DOT: i32 = 10;
            let x = r.0 + (r.2 - (DOT + 6 + text_w(p.label()).round() as i32)) / 2;
            if usable {
                let y = r.1 + (r.3 - DOT) / 2;
                s.fill(x - 1, y - 1, x + DOT + 1, y + DOT + 1, HAIRLINE);
                s.fill(x, y, x + DOT, y + DOT, word(look.part(p)));
            }
            write(s, r, (x + DOT + 6) as f32, p.label(), if usable { BRIGHT } else { REST });
        }
        let row = panel.shadow;
        write(s, row, (row.0 + 2) as f32, "Shadow", MUTED);
        let (px, py, pw, ph) = shadow_switch(row);
        s.fill(px, py, px + pw, py + ph, if look.shadow { ARMED } else { GROOVE });
        let knob = ph - 6;
        let kx = if look.shadow { px + pw - 3 - knob } else { px + 3 };
        s.fill(kx, py + 3, kx + knob, py + 3 + knob, if look.shadow { BRIGHT } else { REST });
    }

    if let Some(block) = &f.layout.colors {
        for (r, left) in [(block.prev, true), (block.next, false)] {
            s.fill_rect(r, BUTTON);
            s.chevron(r.0, r.1, r.0 + r.2, r.1 + r.3, left);
        }
        let palette = Palettes::get(f.palette);
        let name = block.name;
        write(s, name, name.0 as f32 + (name.2 as f32 - text_w(palette.name)) / 2.0, palette.name, MUTED);
        let framed = |s: &mut Canvas, r: Rect4, rgb: u32| {
            // A hairline edge, so black and colors near the panel's still read.
            s.fill(r.0 - 1, r.1 - 1, r.0 + r.2 + 1, r.1 + r.3 + 1, HAIRLINE);
            s.fill_rect(r, rgb);
        };
        let mut picked = None;
        for (&r, &swatch) in block.swatches.iter().zip(palette.colors.iter()) {
            framed(s, r, word(swatch));
            if swatch == f.color { picked = Some(r); }
        }
        s.fill_rect(block.eyedropper, if f.picking { ARMED } else { BUTTON });
        icon(s, block.eyedropper, IconKind::Eyedropper, None);
        framed(s, block.current, ink);
        let (x0, y0, w, h) = block.spectrum;
        s.fill(x0 - 1, y0 - 1, x0 + w + 1, y0 + h + 1, HAIRLINE);
        let (du, dv) = ((w - 1).max(1) as f32, (h - 1).max(1) as f32);
        for y in 0..h {
            for x in 0..w {
                s.set(x0 + x, y0 + y, word(spectrum_color(x as f32 / du, y as f32 / dv)));
            }
        }
        // Where the color came from, while the tool still draws in it.
        if let Some((u, v)) = f.spectrum_at.filter(|&(u, v)| spectrum_color(u, v) == f.color) {
            let mx = x0 + ((w - 1) as f32 * u).round() as i32;
            let my = y0 + ((h - 1) as f32 * v).round() as i32;
            let mut surface = s.surface();
            painter::fill_disc(&mut surface, mx, my, 7, color(GROOVE));
            painter::fill_disc(&mut surface, mx, my, 6, color(BRIGHT));
            painter::fill_disc(&mut surface, mx, my, 4, f.color);
        }
        // The tool's own color ringed: its swatch, or the current-color chip
        // when it is on no swatch.
        let r = picked.unwrap_or(block.current);
        s.fill(r.0 - 3, r.1 - 3, r.0 + r.2 + 3, r.1 + r.3 + 3, BRIGHT);
        s.fill(r.0 - 1, r.1 - 1, r.0 + r.2 + 1, r.1 + r.3 + 1, PANEL_BG);
        s.fill_rect(r, ink);
    }

    for (&(slider, row), &value) in f.layout.sliders.iter().zip(&f.values) {
        let (tx, ty, tw, th) = slider_track(row);
        let (lo, hi) = slider.range();
        let share = ((value - lo) / (hi - lo)).clamp(0.0, 1.0);
        let grip = tx + ((tw - 1) as f32 * share).round() as i32;
        let mid = ty as f32 + th as f32 / 2.0;
        let wedge = slider == Slider::Width;
        for x in tx..tx + tw {
            // Size is a wedge from a hairline up to the track's height, the
            // others a plain 4 px bar; up to the grip it is the tool's color.
            let along = (x - tx) as f32 / (tw - 1).max(1) as f32;
            let thick = if wedge { 1.0 + along * (th as f32 - 3.0) } else { 4.0 };
            let top = (mid - thick / 2.0).round() as i32;
            let bottom = ((mid + thick / 2.0).round() as i32).max(top + 1);
            let rgb = if x <= grip { ink } else if wedge { REST } else { GROOVE };
            s.fill(x, top, x + 1, bottom, rgb);
        }
        let kx = (grip - KNOB_W / 2).clamp(tx, tx + tw - KNOB_W);
        s.fill(kx, ty - 3, kx + KNOB_W, ty + th + 3, BRIGHT);
        // The Text tool's smooth face, not the pixel font.
        write(s, row, (row.0 + 2) as f32, slider.label(), MUTED);
        let readout = slider.readout(value);
        write(s, row, (row.0 + row.2) as f32 - text_w(&readout), &readout, BRIGHT);
    }
}

/// A Highlight look's tile: a scrap of paper with two lines of pretend
/// words, the first line highlighted in `look`.
pub(super) fn draw_mark_tile(c: &mut Canvas, r: Rect4, look: &MarkLook) {
    const PAPER: u32 = 0x00_F4_F3_EF;
    const WORDS: u32 = 0x00_4A_4A_50;
    // Each line's words as (start, width); the lines are 42 px wide.
    const LINES: [[(i32, i32); 3]; 2] = [[(0, 14), (17, 9), (29, 16)], [(0, 10), (13, 18), (34, 8)]];
    c.fill_rect(r, PAPER);
    let left = r.0 + (r.2 - 42) / 2;
    for (row, line) in LINES.iter().enumerate() {
        let y = r.1 + 12 + 15 * row as i32;
        for &(x, w) in line { c.fill(left + x, y, left + x + w, y + 4, WORDS); }
    }
    let (end_x, end_w) = LINES[0][2];
    let first = Rect { x: left as f32, y: (r.1 + 12) as f32 - 3.0, w: (end_x + end_w) as f32, h: 10.0 };
    painter::text_mark(&mut c.surface(), &[first], look);
}

/// A button's tip at `(x, y)`, in the capture dock's look: black, framed a
/// step quieter than the accent, an accent bar down its left, the words in
/// the pixel font with any `[Key]` drawn as a key. Moved back inside
/// `bounds` (the monitor it belongs to) when it would leave it.
pub(super) fn draw_tooltip(c: &mut Canvas, bounds: Rect4, label: &str, x: i32, y: i32) {
    use crate::palette::{LASER, LASER_DIM, TEXT_BRIGHT};
    let (w, h) = tooltip_size(label);
    let (bx, by, bw, bh) = bounds;
    let x = if x + w > bx + bw { bx + bw - w - 4 } else { x };
    let y = if y + h > by + bh { by + bh - h - 4 } else { y };
    let (x, y) = (if x < bx { bx + 4 } else { x }, if y < by { by + 4 } else { y });
    c.fill(x, y, x + w, y + h, 0);
    c.outline(x, y, x + w, y + h, LASER_DIM());
    c.fill(x, y, x + TOOLTIP_BAR_W, y + h, LASER());
    let text_y = y + (h - crate::bitmap_font::GLYPH_H * TOOLTIP_SCALE) / 2;
    crate::keycap::draw_line(c.px, c.w, c.h, x + TOOLTIP_BAR_W + TOOLTIP_PAD_X, text_y, label, TOOLTIP_SCALE, TEXT_BRIGHT);
}

/// A notice pinned near the top middle of `bounds` (the monitor under the
/// pointer, so it never straddles two): the first line in `accent`, the
/// rest in `text`, framed in `accent`. Amber asks before work is lost,
/// green tells about a mode. A screen too small for it gets none rather
/// than a clipped one. The pixel font is ASCII only.
pub(super) fn draw_notice_bar(c: &mut Canvas, bounds: Rect4, lines: &[&str], accent: u32, text: u32) {
    const SCALE: i32 = 2;
    const PAD_X: i32 = 14;
    const PAD_Y: i32 = 10;
    const LINE_GAP: i32 = 6;
    /// Down from the screen's top.
    const DROP: i32 = 36;
    if lines.is_empty() { return; }
    // A line naming a key is a key tall.
    let heights: Vec<i32> = lines.iter().map(|l| crate::keycap::line_height(l, SCALE)).collect();
    let widest = lines.iter().map(|l| crate::keycap::line_width(l, SCALE)).max().unwrap_or(0);
    let (w, h) = (widest + 2 * PAD_X, heights.iter().sum::<i32>() + LINE_GAP * (lines.len() as i32 - 1) + 2 * PAD_Y);
    let (bx, by, bw, bh) = bounds;
    let (x, y) = (bx + (bw - w) / 2, by + DROP);
    let fits_screen = x >= bx && y + h <= by + bh;
    let fits_window = x >= 0 && y >= 0 && x + w <= c.w as i32 && y + h <= c.h as i32;
    if !fits_screen || !fits_window { return; }
    c.fill(x, y, x + w, y + h, INK_BLACK);
    c.outline(x, y, x + w, y + h, accent);
    let mut top = y + PAD_Y;
    for (i, (line, &line_h)) in lines.iter().zip(&heights).enumerate() {
        let lx = x + (w - crate::keycap::line_width(line, SCALE)) / 2;
        let ly = top + (line_h - crate::bitmap_font::GLYPH_H * SCALE) / 2;
        crate::keycap::draw_line(c.px, c.w, c.h, lx, ly, line, SCALE, if i == 0 { accent } else { text });
        top += line_h + LINE_GAP;
    }
}

/// The captured pixel under frame point `p`, opaque, read through
/// `offset` (where the frame's origin sits in the capture). `None` off it.
pub(super) fn pixel_at(shot: &Image, p: (i32, i32), offset: (i32, i32)) -> Option<Color> {
    let (x, y) = (p.0 + offset.0, p.1 + offset.1);
    let on = x >= 0 && y >= 0 && (x as u32) < shot.width() && (y as u32) < shot.height();
    on.then(|| {
        let [r, g, b, _] = shot.get_pixel(x as u32, y as u32).0;
        Color::new_opaque(r, g, b)
    })
}

/// The lens that helps place a selection's edge to the pixel: the capture
/// around the pointer, each pixel a 7 px square, a red cross through the
/// one under it. Down and right of the pointer, flipped either way to stay
/// on the monitor `bounds`, and left out when it fits neither way. The
/// capture is read through `offset`, without which a multi-monitor desktop
/// showed pixels from somewhere else entirely.
pub(super) fn draw_magnifier(c: &mut Canvas, bounds: Rect4, shot: &Image, pointer: (i32, i32), offset: (i32, i32)) {
    const CROSS: u32 = 0x00_DC_26_26;
    let side = MAG_SIZE + 4;
    let (bx, by, bw, bh) = bounds;
    let pick = |p: i32, lo: i32, span: i32| {
        let after = p + MAG_OFFSET;
        if after + side > lo + span { p - MAG_OFFSET - side } else { after }
    };
    let (x, y) = (pick(pointer.0, bx, bw), pick(pointer.1, by, bh));
    if x < bx || y < by || x + side > bx + bw || y + side > by + bh { return; }
    c.fill(x, y, x + side, y + side, INK_BLACK);
    c.outline(x, y, x + side, y + side, 0x00_FF_FF_FF);
    let (ix, iy) = (x + 2, y + 2);
    for sy in 0..MAG_PIXELS {
        for sx in 0..MAG_PIXELS {
            let at = (pointer.0 + sx - MAG_REACH, pointer.1 + sy - MAG_REACH);
            let rgb = pixel_at(shot, at, offset).map_or(0, word);
            let (dx, dy) = (ix + sx * MAG_ZOOM, iy + sy * MAG_ZOOM);
            c.fill(dx, dy, dx + MAG_ZOOM, dy + MAG_ZOOM, rgb);
        }
    }
    let middle = MAG_REACH * MAG_ZOOM;
    c.fill(ix, iy + middle, ix + MAG_SIZE, iy + middle + 1, CROSS);
    c.fill(ix + middle, iy, ix + middle + 1, iy + MAG_SIZE, CROSS);
}

/// The eyedropper's loupe: a lens on the pixels around the pointer with
/// the one a click takes framed, and beside it a card with that color as a
/// big swatch, its hex in large type, and its RGB and HSL. Down and right
/// of the pointer (`up_left`: up and left first), flipped to stay inside
/// `bounds`. `hint` adds how to pick and how to back out.
pub(super) fn draw_color_loupe(
    c: &mut Canvas, bounds: Rect4, shot: &Image, pointer: (i32, i32), offset: (i32, i32), up_left: bool, hint: bool,
) {
    const CARD: u32 = 0x00_1A_1A_1D;
    const AWAY: i32 = 22;
    const PAD: i32 = 14;
    const SWATCH: i32 = 44;
    const HEX_PX: f32 = 20.0;
    let Some(picked) = pixel_at(shot, pointer, offset) else { return };
    let (bx, by, bw, bh) = bounds;
    let (w, h) = (LOUPE_LENS + LOUPE_GAP + LOUPE_CARD_W, LOUPE_LENS);
    let place = |p: i32, lo: i32, span: i32, len: i32| {
        let (after, before) = (p + AWAY, p - AWAY - len);
        let fits = |v: i32| v >= lo && v + len <= lo + span;
        let first_choice = if up_left { before } else { after };
        if fits(first_choice) { Some(first_choice) } else { Some(if up_left { after } else { before }).filter(|&v| fits(v)) }
    };
    let (Some(x), Some(y)) = (place(pointer.0, bx, bw, w), place(pointer.1, by, bh, h)) else { return };

    c.fill(x, y, x + LOUPE_LENS, y + LOUPE_LENS, CARD);
    c.outline(x, y, x + LOUPE_LENS, y + LOUPE_LENS, HAIRLINE);
    let reach = LOUPE_PIXELS / 2;
    for sy in 0..LOUPE_PIXELS {
        for sx in 0..LOUPE_PIXELS {
            let rgb = pixel_at(shot, (pointer.0 + sx - reach, pointer.1 + sy - reach), offset).map_or(0, word);
            let (dx, dy) = (x + 1 + sx * LOUPE_ZOOM, y + 1 + sy * LOUPE_ZOOM);
            c.fill(dx, dy, dx + LOUPE_ZOOM, dy + LOUPE_ZOOM, rgb);
        }
    }
    // The pixel a click takes: framed dark, then light, to read on any color.
    let (mx, my) = (x + 1 + reach * LOUPE_ZOOM, y + 1 + reach * LOUPE_ZOOM);
    c.outline(mx - 1, my - 1, mx + LOUPE_ZOOM + 1, my + LOUPE_ZOOM + 1, 0);
    c.outline(mx - 2, my - 2, mx + LOUPE_ZOOM + 2, my + LOUPE_ZOOM + 2, 0x00_FF_FF_FF);

    let (kx, ky) = (x + LOUPE_LENS + LOUPE_GAP, y);
    c.fill(kx, ky, kx + LOUPE_CARD_W, ky + h, CARD);
    c.outline(kx, ky, kx + LOUPE_CARD_W, ky + h, HAIRLINE);
    c.fill(kx + PAD - 1, ky + PAD - 1, kx + PAD + SWATCH + 1, ky + PAD + SWATCH + 1, HAIRLINE);
    c.fill(kx + PAD, ky + PAD, kx + PAD + SWATCH, ky + PAD + SWATCH, word(picked));
    let Color { r, g, b, .. } = picked;
    let (hue, sat, light) = to_hsl(picked);
    {
        let mut surface = c.surface();
        let hex_y = (ky + PAD) as f32 + (SWATCH as f32 - framefang_core::text::line_height(HEX_PX)) / 2.0;
        painter::draw_unicode_text(&mut surface, (kx + PAD + SWATCH + 12) as f32, hex_y, HEX_PX,
                                   &format!("#{r:02X}{g:02X}{b:02X}"), Color::new_opaque(0xF4, 0xF4, 0xF7));
        let step = framefang_core::text::line_height(FLYOUT_TEXT_PX) + 4.0;
        let rows = [("RGB", format!("{r}, {g}, {b}")), ("HSL", format!("{hue}, {sat}%, {light}%"))];
        for (i, (name, value)) in rows.iter().enumerate() {
            let row_y = (ky + PAD + SWATCH + 12) as f32 + step * i as f32;
            painter::draw_unicode_text(&mut surface, (kx + PAD) as f32, row_y, FLYOUT_TEXT_PX, name, Color::new_opaque(0x7A, 0x7A, 0x84));
            painter::draw_unicode_text(&mut surface, (kx + PAD + 40) as f32, row_y, FLYOUT_TEXT_PX, value, Color::new_opaque(0xDC, 0xDC, 0xE2));
        }
    }
    if hint {
        // A key's height up from the card's bottom padding.
        let line_y = ky + h - PAD - crate::keycap::height(1) + (crate::keycap::face_h(1) - crate::bitmap_font::GLYPH_H) / 2;
        crate::keycap::draw_line(c.px, c.w, c.h, kx + PAD, line_y, "Click to pick  [Esc] cancels", 1, 0x00_8A_8A_94);
    }
}

/// How many decimal digits `n` has.
fn digits(n: u32) -> i32 { n.checked_ilog10().map_or(1, |d| d as i32 + 1) }

/// The selection handles' fill.
const HANDLE_FILL: u32 = 0x00_FF_FF_FF;
/// Half a handle's side: 4 px squares, a white dot in a blue edge (6 px
/// until 1.7.1, which read heavy on the 1 px frame).
const HANDLE_REACH: i32 = 2;

/// The selection: a 1 px blue frame through the corners `(x0, y0)` and
/// `(x1, y1)` (exclusive), and, where the box can be pulled, small marks at
/// its corners and the middles of its sides. What grabs an edge is
/// `Edge::HIT_THRESHOLD` around it, not the drawn square, so small marks are
/// no harder to hit.
pub(super) fn draw_selection_frame(c: &mut Canvas, (x0, y0, x1, y1): (i32, i32, i32, i32), handles: bool) {
    c.outline(x0, y0, x1, y1, SELECTION_BLUE);
    if !handles { return; }
    let (xm, ym, xe, ye) = ((x0 + x1) / 2, (y0 + y1) / 2, x1 - 1, y1 - 1);
    for (x, y) in [(x0, y0), (xm, y0), (xe, y0), (x0, ym), (xe, ym), (x0, ye), (xm, ye), (xe, ye)] {
        let (a, b) = ((x - HANDLE_REACH, y - HANDLE_REACH), (x + HANDLE_REACH, y + HANDLE_REACH));
        c.fill(a.0, a.1, b.0, b.1, HANDLE_FILL);
        c.outline(a.0, a.1, b.0, b.1, SELECTION_BLUE);
    }
}

/// Between the selection's edge and the size chip.
const CHIP_GAP: i32 = 4;

/// Where the size chip goes for selection `sel` on monitor `bounds`:
/// outside the box, on its top edge and flush with its left side, so it
/// never covers what is being captured (the right side and the foot are
/// where the tool column and the action row go). A box against the top of
/// the monitor has no room there, so the chip sits just inside its top left
/// corner instead. Kept on the monitor sideways.
fn dimension_chip_spot(bounds: Rect4, sel: Rect4, chip: (i32, i32)) -> (i32, i32) {
    let (bx, by, bw, _) = bounds;
    let above = sel.1 - CHIP_GAP - chip.1;
    let (x, y) = if above >= by { (sel.0, above) } else { (sel.0 + CHIP_GAP, sel.1 + CHIP_GAP) };
    (x.clamp(bx, (bx + bw - chip.0).max(bx)), y)
}

/// The selection's size, `w x h` in the pixel font on a dark chip outside
/// the box (`dimension_chip_spot`). The chip darkens what is under it to a
/// quarter rather than hiding it.
pub(super) fn draw_dimension_chip(c: &mut Canvas, bounds: Rect4, sel: Rect4, w: u32, h: u32) {
    const SCALE: i32 = 2;
    const PAD_X: i32 = 6;
    const PAD_Y: i32 = 4;
    const CROSS_W: i32 = 5 * SCALE;
    const SPACE: i32 = 2 * SCALE;
    let number_w = |n: u32| digits(n) * 5 * SCALE + (digits(n) - 1) * SCALE;
    let chip_w = number_w(w) + SPACE + CROSS_W + SPACE + number_w(h) + 2 * PAD_X;
    let chip_h = 7 * SCALE + 2 * PAD_Y;
    let (x, y) = dimension_chip_spot(bounds, sel, (chip_w, chip_h));
    // Three quarters of 0x16191D over the picture.
    let (xa, xb) = (x.clamp(0, c.w as i32) as usize, (x + chip_w).clamp(0, c.w as i32) as usize);
    let (ya, yb) = (y.clamp(0, c.h as i32) as usize, (y + chip_h).clamp(0, c.h as i32) as usize);
    for row in ya..yb {
        for p in &mut c.px[row * c.w + xa..row * c.w + xb.max(xa)] {
            let under = *p;
            let tint = |shift: u32, toward: u32| (((under >> shift) & 0xFF) * 64 + toward * 192) / 256 << shift;
            *p = tint(16, 0x16) | tint(8, 0x19) | tint(0, 0x1D);
        }
    }
    let mut surface = c.surface();
    let (mut pen, top) = (x + PAD_X, y + PAD_Y);
    painter::draw_number(&mut surface, pen, top, SCALE, w, Color::WHITE);
    pen += number_w(w) + SPACE;
    // The `x`: two diagonals across a 5 by 7 cell.
    let (cw, ch) = (CROSS_W, 7 * SCALE);
    painter::line(&mut surface, pen, top + SCALE, pen + cw - 1, top + ch - SCALE - 1, SCALE as f32, Color::WHITE);
    painter::line(&mut surface, pen + cw - 1, top + SCALE, pen, top + ch - SCALE - 1, SCALE as f32, Color::WHITE);
    pen += CROSS_W + SPACE;
    painter::draw_number(&mut surface, pen, top, SCALE, h, Color::WHITE);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_size_chip_sits_outside_the_box_unless_the_screen_ends() {
        let screen = (0, 0, 1920, 1080);
        let chip = (90, 22);
        let inside = |p: (i32, i32), r: Rect4| p.0 >= r.0 && p.0 < r.0 + r.2 && p.1 >= r.1 && p.1 < r.1 + r.3;
        let overlaps = |(x, y): (i32, i32), r: Rect4| {
            x < r.0 + r.2 && r.0 < x + chip.0 && y < r.1 + r.3 && r.1 < y + chip.1
        };
        // Room above: on the top edge, outside, flush with the left side.
        let sel = (400, 300, 500, 200);
        let spot = dimension_chip_spot(screen, sel, chip);
        assert_eq!(spot, (400, 300 - CHIP_GAP - chip.1));
        assert!(!overlaps(spot, sel), "it must not cover the picture");
        // Against the top of the screen: just inside the top left corner.
        let top = (400, 10, 500, 200);
        assert_eq!(dimension_chip_spot(screen, top, chip), (400 + CHIP_GAP, 10 + CHIP_GAP));
        assert!(inside(dimension_chip_spot(screen, top, chip), top));
        // A box at the right edge keeps the chip on the screen.
        let right = (1880, 300, 40, 40);
        assert_eq!(dimension_chip_spot(screen, right, chip).0, 1920 - chip.0);
        // On a second monitor its own top counts.
        let second = (1920, 0, 1920, 1080);
        assert_eq!(dimension_chip_spot(second, (2000, 20, 300, 300), chip), (2000 + CHIP_GAP, 20 + CHIP_GAP));
    }

    #[test]
    fn a_hovered_button_lifts_grows_and_takes_the_accent_edge() {
        let (w, h) = (900usize, 900usize);
        let (screen, sel) = ((0, 0, 900, 900), (300, 150, 300, 300));
        let draw = |hover: f32| {
            let mut px = vec![0u32; w * h];
            draw_tool_panel(&mut Canvas::new(&mut px, w, h), &EDITOR_SLOTS, screen, sel, None, |i| if i == 2 { hover } else { 0.0 });
            px
        };
        let (rest, hot) = (draw(0.0), draw(1.0));
        let r = slot_rect(&EDITOR_SLOTS, tool_panel_origin(&EDITOR_SLOTS, screen, sel), 2);
        let at = |px: &[u32], x: i32, y: i32| px[y as usize * w + x as usize];
        // At rest the pixel just outside the button is panel; hovered, the
        // button has grown over it and its edge carries the accent.
        let (out_x, mid_y) = (r.0 - 1, r.1 + r.3 / 2);
        assert_eq!(at(&rest, out_x, mid_y), PANEL_BG);
        assert_eq!(at(&hot, out_x, mid_y), crate::palette::LASER());
        // The face is lighter than at rest, away from the icon's ink.
        assert_eq!(at(&rest, r.0 + 1, r.1 + 1), BUTTON);
        assert_eq!(at(&hot, r.0 + 1, r.1 + 1), BUTTON_HOVER);
        // Other buttons are untouched.
        let other = slot_rect(&EDITOR_SLOTS, tool_panel_origin(&EDITOR_SLOTS, screen, sel), 7);
        assert_eq!(at(&rest, other.0 + 1, other.1 + 1), at(&hot, other.0 + 1, other.1 + 1));
        // Mixing lands on both ends exactly.
        assert_eq!((mix(BUTTON, BUTTON_HOVER, 0.0), mix(BUTTON, BUTTON_HOVER, 1.0)), (BUTTON, BUTTON_HOVER));
    }

    #[test]
    fn the_pulse_breathes_three_times_slowly_and_ends_dark() {
        assert_eq!(pulse_level(0.0), 0.0, "starts dark");
        assert!(pulse_level(PULSE_SECS - 0.001) < 0.01, "fades out, no hard cut");
        assert_eq!(pulse_level(PULSE_SECS), 0.0);
        let breath = 1.0 / PULSE_HZ;
        for k in 0..3 {
            assert!(pulse_level((k as f32 + 0.5) * breath) > 0.99, "full in the middle of breath {k}");
        }
        assert!(breath >= 0.6, "most of a second a breath, not a strobe");
    }

    #[test]
    fn with_no_tool_in_hand_no_button_is_lit() {
        let (w, h) = (900usize, 900usize);
        let lit = |active: Option<Tool>| {
            let mut px = vec![0u32; w * h];
            draw_tool_panel(&mut Canvas::new(&mut px, w, h), &EDITOR_SLOTS, (0, 0, 900, 900), (300, 150, 300, 300), active, |_| 0.0);
            px.iter().filter(|&&p| p == SELECTION_BLUE).count()
        };
        assert_eq!(lit(None), 0);
        assert!(lit(Some(Tool::Pen)) > 0);
    }

    #[test]
    fn the_eyedropper_reads_the_capture_through_its_offset() {
        let mut shot = ImageBuffer::from_pixel(4, 4, Rgba([0, 0, 0, 255]));
        shot.put_pixel(3, 2, Rgba([10, 20, 30, 7]));
        // Frame (1, 1) is capture (3, 2) when the frame starts at (2, 1).
        assert_eq!(pixel_at(&shot, (1, 1), (2, 1)), Some(Color::new_opaque(10, 20, 30)));
        assert_eq!(pixel_at(&shot, (5, 0), (0, 0)), None);
        assert_eq!(pixel_at(&shot, (-1, 0), (0, 0)), None);
    }

    #[test]
    fn a_notice_too_big_for_its_screen_is_left_out() {
        let mut px = vec![0u32; 40 * 20];
        draw_notice_bar(&mut Canvas::new(&mut px, 40, 20), (0, 0, 40, 20), &["Throw away 2 annotations?", "Esc"], 0x00_FF_B0_20, 0x00_FF_D8_8A);
        assert!(px.iter().all(|&p| p == 0), "nothing at all, not a clipped chip");
    }

    #[test]
    fn a_notice_sits_at_the_top_of_a_roomy_screen() {
        let (w, h) = (600usize, 300usize);
        let mut px = vec![0u32; w * h];
        draw_notice_bar(&mut Canvas::new(&mut px, w, h), (0, 0, 600, 300), &["Select mode", "click ink to move it"], NEON_GREEN, 0x00_C8_FF_E4);
        assert!(px.iter().any(|&p| p != 0));
        assert!(px[(h - 1) * w..].iter().all(|&p| p == 0), "nothing at the foot");
    }

    #[test]
    fn digits_are_counted_from_one() {
        assert_eq!([0, 9, 10, 1920, u32::MAX].map(digits), [1, 1, 2, 4, 10]);
    }

    #[test]
    fn the_size_chip_darkens_what_is_under_it_and_stays_on_screen() {
        let (w, h) = (200usize, 120usize);
        let mut px = vec![0x00_FF_FF_FF; w * h];
        // A box near the screen's top: no room above, so the chip sits just
        // inside its top left corner.
        draw_dimension_chip(&mut Canvas::new(&mut px, w, h), (0, 0, 200, 120), (10, 10, 150, 90), 640, 480);
        assert_ne!(px[20 * w + 20], 0x00_FF_FF_FF, "inside the chip is darker");
        assert_eq!(px[5 * w + 5], 0x00_FF_FF_FF, "outside the box stays clear");
    }

    #[test]
    fn the_flyout_slides_out_from_the_column_side() {
        let layout = super::super::flyout::flyout_layout(Tool::Pen);
        let (lw, lh) = (layout.w, layout.h);
        let frame = |open: f32, on_right: bool| FlyoutFrame {
            rect: (20, 0, lw, lh), on_right, pointer_y: 40, layout: super::super::flyout::flyout_layout(Tool::Pen),
            values: vec![4.0], color: Color::new_opaque(255, 0, 0), palette: 0, open, picking: false,
            spectrum_at: None, step: None, mark: None,
        };
        let (w, h) = ((lw + 40) as usize, lh as usize);
        let drawn = |f: &FlyoutFrame| {
            let mut px = vec![0u32; w * h];
            draw_flyout(&mut Canvas::new(&mut px, w, h), f);
            (0..w).filter(|&x| px[10 * w + x] != 0).collect::<Vec<_>>()
        };
        // Half out on the right of the column: the half nearest it shows.
        let cols = drawn(&frame(0.5, true));
        assert_eq!((cols[0], cols.len()), (20, (lw as f32 * 0.5).round() as usize));
        let cols = drawn(&frame(0.5, false));
        assert_eq!(*cols.last().unwrap(), (20 + lw - 1) as usize);
        assert!(drawn(&frame(0.0, true)).is_empty());
    }
}
