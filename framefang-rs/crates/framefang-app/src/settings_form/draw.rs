//! Pixels for the settings window: the little drawing kit (rectangles, text,
//! the pixel buffer) and one painter per kind of row. Where things go is
//! decided in `build_rows`; this file only says how each one looks.
//!
//! Colors are the palette's `0x00RRGGBB` numbers throughout; they only become
//! `Rgba` at the moment text reaches the painter.

use framefang_core::color::Rgba;

use super::*;

/// x, y, width, height.
pub(super) type Rect = (i32, i32, i32, i32);

/// Whether the point is inside `rect` (left and top edges in, right and
/// bottom edges out).
pub(super) fn rect_has(rect: Rect, x: i32, y: i32) -> bool {
    let (rx, ry, rw, rh) = rect;
    (rx..rx + rw).contains(&x) && (ry..ry + rh).contains(&y)
}

// ── drawing kit ─────────────────────────────────────────────────────────────

/// The window body as a plain `0x00RRGGBB` pixel buffer the painter can draw
/// into. Reads outside it are opaque black; writes outside it are dropped.
pub(super) struct PixelBuf<'a> {
    pub(super) px: &'a mut [u32],
    pub(super) wide: i32,
    pub(super) tall: i32,
}

impl PixelBuf<'_> {
    fn slot(&self, x: i32, y: i32) -> Option<usize> {
        if x < 0 || y < 0 || x >= self.wide || y >= self.tall { return None; }
        Some(y as usize * self.wide as usize + x as usize)
    }
}

impl painter::Surface for PixelBuf<'_> {
    fn width(&self) -> i32 { self.wide }
    fn height(&self) -> i32 { self.tall }

    fn read(&self, x: i32, y: i32) -> [u8; 4] {
        match self.slot(x, y) {
            Some(i) => {
                let [_, r, g, b] = self.px[i].to_be_bytes();
                [r, g, b, 0xFF]
            }
            None => [0, 0, 0, 0xFF],
        }
    }

    fn write(&mut self, x: i32, y: i32, rgba: [u8; 4]) {
        if let Some(i) = self.slot(x, y) {
            self.px[i] = u32::from_be_bytes([0, rgba[0], rgba[1], rgba[2]]);
        }
    }
}

/// A palette color as the painter's opaque `Rgba`.
pub(super) fn opaque(argb: u32) -> Rgba {
    let [_, r, g, b] = argb.to_be_bytes();
    Rgba { r, g, b, a: 255 }
}

pub(super) fn fill_box<S: painter::Surface>(s: &mut S, x: i32, y: i32, w: i32, h: i32, argb: u32) {
    let px = opaque(argb);
    let rgba = [px.r, px.g, px.b, px.a];
    for row in y..y + h {
        for col in x..x + w { s.write(col, row, rgba); }
    }
}

/// A one pixel outline just inside the rectangle.
pub(super) fn frame_box<S: painter::Surface>(s: &mut S, x: i32, y: i32, w: i32, h: i32, argb: u32) {
    let px = opaque(argb);
    let rgba = [px.r, px.g, px.b, px.a];
    for col in x..x + w {
        s.write(col, y, rgba);
        s.write(col, y + h - 1, rgba);
    }
    for row in y..y + h {
        s.write(x, row, rgba);
        s.write(x + w - 1, row, rgba);
    }
}

/// Bitmap-font text at scale 1.
pub(super) fn put_text<S: painter::Surface>(s: &mut S, x: i32, y: i32, words: &str, argb: u32) {
    painter::draw_text(s, x, y, 1, words, opaque(argb));
}

/// The top of a line of text centred in a band of `h` starting at `y`.
fn mid_y(y: i32, h: i32) -> i32 { y + (h - bitmap_font::GLYPH_H) / 2 }

/// `words` centred in `rect`, both ways.
fn put_centered<S: painter::Surface>(s: &mut S, rect: Rect, words: &str, argb: u32) {
    let (x, y, w, h) = rect;
    put_text(s, x + (w - bitmap_font::measure(words, 1)) / 2, mid_y(y, h), words, argb);
}

/// An outlined button, filled only when `fill` says so, its label centred.
fn button<S: painter::Surface>(s: &mut S, rect: Rect, edge: u32, fill: Option<u32>, label: &str, label_color: u32) {
    let (x, y, w, h) = rect;
    if let Some(f) = fill { fill_box(s, x, y, w, h, f); }
    frame_box(s, x, y, w, h, edge);
    put_centered(s, rect, label, label_color);
}

/// Shrink `s` until it fits in `max_px`, eliding the middle: these fields
/// mostly hold filesystem paths, and the leaf name at the end is the part
/// worth reading.
pub(super) fn truncate_for(s: &str, max_px: i32) -> String {
    if bitmap_font::measure(s, 1) <= max_px { return s.to_owned(); }
    const DOTS: &str = "..";
    // Each glyph is GLYPH_W wide plus a one pixel gap, and `measure` leaves
    // off the gap after the last one: n glyphs are n * pitch - 1 pixels.
    let pitch = bitmap_font::GLYPH_W + 1;
    let room = (max_px + 1) / pitch;
    let dots = DOTS.len() as i32;
    if room <= dots { return DOTS[..room.max(0) as usize].to_owned(); }
    // Two thirds of what is left goes to the end of the text.
    let budget = (room - dots) as usize;
    let tail = (budget * 2 / 3).max(1);
    let head = budget - tail;
    // `room` is below the character count here, so the two ends can't meet.
    let chars: Vec<char> = s.chars().collect();
    let mut out: String = chars[..head].iter().collect();
    out.push_str(DOTS);
    out.extend(&chars[chars.len() - tail..]);
    out
}

/// What a folder field shows: the folder picked, or else where the system
/// keeps that kind of file (`fallback` when it has no such place), so people
/// see where output goes before they choose anything.
fn shown_folder(
    picked: &str,
    system_dir: fn(&directories::UserDirs) -> Option<&std::path::Path>,
    fallback: &str,
) -> String {
    if !picked.is_empty() { return picked.to_owned(); }
    directories::UserDirs::new()
        .and_then(|u| system_dir(&u).map(|p| p.to_string_lossy().into_owned()))
        .unwrap_or_else(|| fallback.to_owned())
}

// ── the line under HOTKEYS ──────────────────────────────────────────────────

/// What the button is for on the left, the button on the right. When the
/// system is taking the capture binding the line says so in the warning tint
/// and the button lights up as the thing to press.
pub(super) fn render_system_line<S: painter::Surface>(
    surf: &mut S, row: &Row, hovered: bool,
    page: Option<crate::system_shortcuts::Page>, takes: bool,
) {
    let Some(page) = page else { return };
    let (words, words_color) = if takes {
        ("Print Screen opens the Snipping Tool, not Frame Fang", WARN())
    } else {
        (page.line, TEXT_MUTED())
    };
    put_text(surf, PAD + 6, mid_y(row.rect.1, row.rect.3), words, words_color);
    let edge = if takes { WARN() } else if hovered { LASER_DIM() } else { FIELD_BORDER() };
    let label_color = if takes { WARN() } else { TEXT_BRIGHT };
    button(surf, row.rect, edge, hovered.then(HOVER_FILL), row.label, label_color);
}

// ── FFmpeg row ──────────────────────────────────────────────────────────────

/// What the FFmpeg row shows.
pub(super) struct FfmpegRow<'a> {
    /// The FFmpeg that will be used.
    pub(super) found: Option<&'a std::path::Path>,
    /// The one picked in Settings, whether or not it still exists.
    pub(super) chosen: Option<&'a std::path::Path>,
    pub(super) fetch: &'a crate::ffmpeg_fetch::Fetch,
    /// The Download button and whether it is hovered, when it can be pressed.
    pub(super) button: Option<(&'a Row, bool)>,
    /// The Auto button, while an FFmpeg is picked.
    pub(super) auto: Option<(&'a Row, bool)>,
    pub(super) browse: Option<(&'a Row, bool)>,
    /// Lit because recording or conversion just needed FFmpeg.
    pub(super) attention: bool,
}

/// What the FFmpeg field says, and in which color, outside a download: the
/// pick in bright text, one found by itself muted (like the folders'
/// defaults), and a pick that is gone in the warning color with what is
/// used instead.
pub(super) fn ffmpeg_field_text(found: Option<&std::path::Path>, chosen: Option<&std::path::Path>) -> Option<(String, u32)> {
    match (chosen, found) {
        (Some(c), Some(f)) if c == f => Some((f.display().to_string(), TEXT_BRIGHT)),
        (Some(_), Some(f)) => Some((format!("Picked FFmpeg is gone, using {}", f.display()), WARN())),
        (Some(c), None) => Some((format!("Not found: {}", c.display()), WARN())),
        (None, Some(f)) => Some((f.display().to_string(), TEXT_MUTED())),
        (None, None) => None,
    }
}

/// The FFmpeg row: its label, a read-out field, then Download (when there
/// is nothing to use) or Auto (while one is picked), and Browse... While
/// downloading, the field fills from the left like a progress bar.
pub(super) fn render_ffmpeg_row<S: painter::Surface>(surf: &mut S, row: &Row, ff: &FfmpegRow) {
    use crate::ffmpeg_fetch::Fetch;
    let (rx, ry, rw, rh) = row.rect;
    let label_color = if ff.attention { LASER() } else { TEXT_BRIGHT };
    put_text(surf, rx + 6, mid_y(ry, rh), row.label, label_color);

    let val_x = rx + LABEL_W;
    let (val_y, val_h) = (ry + 4, rh - 8);
    let right = ff.button.or(ff.auto).or(ff.browse).map_or(rx + rw - 4, |(b, _)| b.rect.0 - 8);
    let val_w = right - val_x;

    let edge = if ff.attention { LASER() } else { FIELD_BORDER() };
    fill_box(surf, val_x, val_y, val_w, val_h, FIELD_BG());
    let ff_text = ffmpeg_field_text(ff.found, ff.chosen);
    let (text, color) = match (ff.found, ff.fetch) {
        (_, Fetch::Downloading { got, total }) => {
            // The field fills as the download comes in.
            if let Some(t) = total.filter(|t| *t > 0) {
                let filled = ((val_w as f64) * (*got as f64 / t as f64)).clamp(0.0, val_w as f64) as i32;
                fill_box(surf, val_x, val_y, filled, val_h, PRIMARY_FILL());
            }
            (format!("Downloading FFmpeg  {}", crate::ffmpeg_fetch::progress_text(*got, *total)), TEXT_BRIGHT)
        }
        (_, Fetch::Installing) => ("Checking and installing...".to_owned(), TEXT_BRIGHT),
        _ if ff_text.is_some() => ff_text.clone().unwrap_or_default(),
        // Covered by `ff_text` above; kept so the match is complete.
        (Some(path), _) => (path.display().to_string(), TEXT_MUTED()),
        (None, Fetch::Failed(e)) => (format!("Download failed: {e}"), DANGER()),
        (None, _) if crate::ffmpeg_fetch::offered() =>
            ("Not installed. Recording and video conversion need it.".to_owned(), TEXT_MUTED()),
        (None, _) => (crate::ffmpeg_fetch::missing_message().to_owned(), TEXT_MUTED()),
    };
    frame_box(surf, val_x, val_y, val_w, val_h, edge);
    put_text(surf, val_x + 8, mid_y(val_y, val_h), &truncate_for(&text, val_w - 16), color);

    if let Some((b, hovered)) = ff.button {
        let label = if matches!(ff.fetch, Fetch::Failed(_)) { "RETRY" } else { b.label };
        let fill = (hovered || ff.attention).then(PRIMARY_FILL);
        button(surf, b.rect, LASER(), fill, label, LASER());
    }
    // Auto and Browse...: the folder rows' quiet outlined buttons.
    for (b, hovered) in ff.auto.into_iter().chain(ff.browse) {
        let edge = if hovered { LASER_DIM() } else { FIELD_BORDER() };
        button(surf, b.rect, edge, hovered.then(HOVER_FILL), b.label, TEXT_BRIGHT);
    }
}

// ── Windows Print Screen card ───────────────────────────────────────────────

/// The status line, the page line and the color of the card's edge for `g`.
/// Both say plainly that the switch is about using Print Screen here.
pub(super) fn guide_text(g: &WindowsGuide) -> (String, String, u32) {
    if g.takes {
        (
            "Windows is using Print Screen for its own capture".to_owned(),
            format!("To use Print Screen here, turn this off in {}:", g.page),
            WARN(),
        )
    } else if let Some(rival) = g.rival {
        // Windows' switch is fine, but another tool wants the key too.
        (
            format!("{rival} is also using Print Screen; turn it off there"),
            format!("Keep this off in {} for Print Screen to work:", g.page),
            WARN(),
        )
    } else {
        (
            "Print Screen goes straight to Frame Fang".to_owned(),
            format!("Keep this off in {} for Print Screen to work:", g.page),
            LASER(),
        )
    }
}

const MINI_SWITCH_W: i32 = 26;

/// A small on/off switch in the app's square style: filled with the knob at
/// the right when on, an outline with the knob at the left when off.
fn mini_switch<S: painter::Surface>(surf: &mut S, x: i32, y: i32, on: bool, color: u32) {
    let h = 14;
    let knob = h - 6;
    if on {
        fill_box(surf, x, y, MINI_SWITCH_W, h, color);
        fill_box(surf, x + MINI_SWITCH_W - 3 - knob, y + 3, knob, knob, BG_BODY());
    } else {
        frame_box(surf, x, y, MINI_SWITCH_W, h, color);
        fill_box(surf, x + 3, y + 3, knob, knob, color);
    }
}

/// Windows' Print Screen guide: a card a step above the page. Its edge and
/// dot say the state at a glance; the first line says it in words beside
/// the button; the second names the Settings page; the third shows the
/// switch itself (a small on/off drawn like Windows' own, then its exact
/// name in the smooth font, since those are Windows' words, not ours).
pub(super) fn render_windows_card<S: painter::Surface>(surf: &mut S, btn: &Row, hovered: bool, g: &WindowsGuide) {
    let (_, by, _, bh) = btn.rect;
    let (cx, cy) = (PAD, by - CARD_PAD);
    let cw = WIN_W as i32 - 2 * PAD;
    let (headline, page_line, status) = guide_text(g);

    // Surface, hairline, and a status edge down the left.
    fill_box(surf, cx, cy, cw, CARD_H, BG_TOP());
    frame_box(surf, cx, cy, cw, CARD_H, FIELD_BORDER());
    fill_box(surf, cx, cy, 3, CARD_H, status);

    // Line 1: a status dot and the state, level with the button.
    let tx = cx + 16;
    let line1 = mid_y(by, bh);
    fill_box(surf, tx, line1 + 1, 5, 5, status);
    put_text(surf, tx + 12, line1, &headline, TEXT_BRIGHT);

    // The button: the thing to press while Windows has the key.
    let edge = if g.takes { LASER() } else if hovered { LASER_DIM() } else { FIELD_BORDER() };
    let label_color = if g.takes { LASER() } else { TEXT_BRIGHT };
    button(surf, btn.rect, edge, hovered.then(HOVER_FILL), btn.label, label_color);

    // Line 2: where the switch is.
    let line2 = by + bh + 10;
    put_text(surf, tx + 12, line2, &page_line, TEXT_MUTED());

    // Line 3: the switch, drawn, then its name.
    let row3 = line2 + bitmap_font::GLYPH_H + 8;
    let mut x = tx + 12;
    if g.takes {
        // On now, off is the goal.
        mini_switch(surf, x, row3 + 1, true, status);
        x += MINI_SWITCH_W + 6;
        put_text(surf, x, row3 + 4, "->", TEXT_MUTED());
        x += bitmap_font::measure("->", 1) + 6;
        mini_switch(surf, x, row3 + 1, false, TEXT_BRIGHT);
    } else {
        // Off, as it should stay: fine even when another tool is the
        // problem (the edge says that).
        mini_switch(surf, x, row3 + 1, false, LASER());
    }
    x += MINI_SWITCH_W + 10;
    let name = format!("\u{201C}{}\u{201D}", g.switch);
    // Windows' wording in another language can run long: shrink to fit.
    let room = (cx + cw - CARD_PAD - x) as f32;
    let mut px = 13.0_f32;
    let w = framefang_core::text::layout(&name, px).width();
    if w > room { px = (px * room / w).max(9.0); }
    let ty = row3 as f32 + (16.0 - framefang_core::text::line_height(px)) / 2.0;
    painter::draw_unicode_text(surf, x as f32, ty, px, &name, opaque(TEXT_BRIGHT));
}

// ── setting rows ────────────────────────────────────────────────────────────

/// Draw one row that has no painter of its own in `redraw`: the settings
/// file's buttons, the hotkey boxes and their buttons, the folder fields and
/// the on/off rows.
pub(super) fn render_row<S: painter::Surface>(
    surf: &mut S, row: &Row,
    hovered: bool,
    rebinding: Option<HotkeyAction>,
    // What the listening box says instead of "[ PRESS A KEY ]", if anything.
    capture_hint: Option<&str>,
    draft: &AppSettings,
) {
    let (rx, ry, rw, rh) = row.rect;

    match row.kind {
        // Plain outlined buttons; none of them commits anything.
        WidgetKind::ExportBtn | WidgetKind::ImportBtn | WidgetKind::OpenJson => {
            let edge = if hovered { LASER_DIM() } else { PANEL_BORDER() };
            button(surf, row.rect, edge, hovered.then(HOVER_FILL), row.label, TEXT_BRIGHT);
            return;
        }
        // Unbinds an optional action. Dim while there is nothing to clear, so
        // the row reads as "already unbound", not as a button that did nothing.
        WidgetKind::HotkeyClear(action) => {
            let live = draft.hotkey_for(action).is_some();
            let lit = hovered && live;
            let edge = if lit { LASER_DIM() } else { FIELD_BORDER() };
            let label_color = if live { TEXT_BRIGHT } else { TEXT_DIM() };
            button(surf, row.rect, edge, lit.then(HOVER_FILL), row.label, label_color);
            return;
        }
        WidgetKind::HotkeyRebind(action) => {
            render_rebind_button(surf, row, rebinding == Some(action), hovered);
            return;
        }
        _ => {}
    }

    if hovered { fill_box(surf, rx, ry, rw, rh, HOVER_FILL()); }

    // A refining row sits indented under its parent, label a step quieter.
    let (indent, label_color) = match row.kind {
        WidgetKind::CloseAfterCopy => (SUB_INDENT, TEXT_MUTED()),
        _ => (0, TEXT_BRIGHT),
    };
    put_text(surf, rx + 6 + indent, mid_y(ry, rh), row.label, label_color);

    // The value half starts right of the label column and ends at the row's
    // edge, inset a little top and bottom.
    let val_x = rx + LABEL_W;
    let val_w = rw - LABEL_W - 4;
    let (val_y, val_h) = (ry + 4, rh - 8);

    match row.kind {
        WidgetKind::SaveFolder | WidgetKind::RecordingsFolder => {
            let path_w = val_w - BROWSE_W - 8;
            // An empty setting shows where the folder really is, dimmed so
            // it reads as not yet customised.
            let (value, picked) = if row.kind == WidgetKind::SaveFolder {
                let set = &draft.save_directory;
                (shown_folder(set, directories::UserDirs::picture_dir, "~/Pictures"), !set.is_empty())
            } else {
                let set = &draft.recordings_directory;
                (shown_folder(set, directories::UserDirs::video_dir, "~/Videos"), !set.is_empty())
            };
            fill_box(surf, val_x, val_y, path_w, val_h, FIELD_BG());
            frame_box(surf, val_x, val_y, path_w, val_h, FIELD_BORDER());
            let color = if picked { TEXT_BRIGHT } else { TEXT_MUTED() };
            put_text(surf, val_x + 8, mid_y(val_y, val_h), &truncate_for(&value, path_w - 16), color);
            let edge = if hovered { LASER_DIM() } else { FIELD_BORDER() };
            button(surf, (val_x + path_w + 8, val_y, BROWSE_W, val_h), edge, None, "Browse...", TEXT_BRIGHT);
        }
        WidgetKind::HotkeyDisplay(action) => {
            // The box stops short of REBIND (and of CLEAR where there is one),
            // which are rows laid over this one at its right end, a gutter
            // apart.
            let buttons = REBIND_W + 8 + if action.is_optional() { CLEAR_W + 8 } else { 0 };
            let box_w = val_w - buttons;
            let listening = rebinding == Some(action);
            // The border lights up while it waits for a key.
            let edge = if listening { FIELD_FOCUS() } else if hovered { LASER_DIM() } else { FIELD_BORDER() };
            fill_box(surf, val_x, val_y, box_w, val_h, FIELD_BG());
            frame_box(surf, val_x, val_y, box_w, val_h, edge);
            let binding = draft.hotkey_for(action);
            let (words, color) = match (listening, capture_hint, binding) {
                (true, Some(hint), _) => (hint.to_owned(), WARN()),
                (true, None, _) => ("[ PRESS A KEY ]".to_owned(), LASER()),
                (false, _, Some(hk)) => (hk.describe(), TEXT_BRIGHT),
                // An optional action starts unbound; say so rather than
                // leaving an empty box that looks like a drawing bug.
                (false, _, None) => ("(not set)".to_owned(), TEXT_DIM()),
            };
            let text_y = mid_y(val_y, val_h);
            let keys = crate::keycap::chord(&words);
            if !listening && binding.is_some() && crate::keycap::line_width(&keys, 1) <= box_w - 16 {
                // The binding as the keys themselves (`keycap`).
                crate::keycap::draw_line_on(surf, val_x + 8, text_y, &keys, 1, color);
            } else {
                put_text(surf, val_x + 8, text_y, &truncate_for(&words, box_w - 16), color);
            }
        }
        WidgetKind::StartWithOs | WidgetKind::CloseAfterCopy | WidgetKind::CaptureCursor
        | WidgetKind::KeepRecent => {
            let on = match row.kind {
                WidgetKind::KeepRecent => draft.keep_recent,
                WidgetKind::CloseAfterCopy => draft.close_after_copy,
                WidgetKind::CaptureCursor => draft.capture_cursor,
                _ => draft.start_with_windows,
            };
            // At the row's right edge, where the other controls end.
            draw_toggle(surf, rx + rw - 4 - TOGGLE_W, val_y, val_h, on);
        }
        _ => {}
    }
}

/// A group's tree line: down from its setting's row to the middle of its
/// last sub-setting, with a tick out to each sub-setting's words, so they
/// read as hanging off the setting above.
pub(super) fn render_tree<S: painter::Surface>(surf: &mut S, group: &Row, sub: i32, rows: &[Row]) {
    let (gx, gy, _, gh) = group.rect;
    let ticks: Vec<i32> = rows.iter()
        .filter(|r| matches!(r.kind, WidgetKind::HoldLine(_) | WidgetKind::CloseAfterCopy)
            && (sub..gy + gh).contains(&r.rect.1))
        .map(|r| r.rect.1 + r.rect.3 / 2)
        .collect();
    let Some(&lowest) = ticks.last() else { return };
    let color = FIELD_BORDER();
    let trunk_x = gx + TREE_X;
    fill_box(surf, trunk_x, sub - 6, 1, lowest - sub + 7, color);
    for t in ticks { fill_box(surf, trunk_x, t, TREE_TICK - TREE_X, 1, color); }
}

/// A REBIND button: an outlined button that fills while it waits for a
/// key, when it reads LISTENING (Hotkeys page and hold lines alike).
pub(super) fn render_rebind_button<S: painter::Surface>(surf: &mut S, row: &Row, active: bool, hovered: bool) {
    let (edge, fill) = if active {
        (LASER(), Some(PRIMARY_FILL()))
    } else if hovered {
        (LASER_DIM(), Some(HOVER_FILL()))
    } else {
        (FIELD_BORDER(), None)
    };
    let (label, color) = if active { ("LISTENING", LASER()) } else { (row.label, TEXT_BRIGHT) };
    button(surf, row.rect, edge, fill, label, color);
}

/// A hold line: "Hold", the key as a keycap, then what holding it does,
/// hanging off the choice above like any sub-setting, with REBIND at its
/// right end. The key sinks under the pointer, like a press; waiting for a
/// key it says so, and the words after it say how to give up (or why a key
/// was refused).
pub(super) fn render_hold_line<S: painter::Surface>(surf: &mut S, line: &Row, hold: Hold, draft: &AppSettings,
                                                    listening: bool, key_hovered: bool, refused: Option<&str>) {
    let name = hold_key_name(hold, draft, listening);
    let (kx, ky, kw, _) = hold_key_rect(line, &name);
    let text_y = mid_y(line.rect.1, line.rect.3);
    put_text(surf, line.rect.0 + 6 + SUB_INDENT, text_y, HOLD_WORD, TEXT_MUTED());
    crate::keycap::draw_key_on(surf, kx, ky, &name, 1, key_hovered && !listening);
    let (tail, color) = match (listening, refused) {
        (true, Some(why)) => (why, WARN()),
        (true, None) => ("Esc cancels", LASER()),
        _ => (hold.tail(draft), TEXT_MUTED()),
    };
    put_text(surf, kx + kw + HOLD_GAP, text_y, tail, color);
}

/// One tab in the strip. The one shown is a raised tab on the page's
/// surface with the accent along its top; the rest sit back.
pub(super) fn render_tab<S: painter::Surface>(surf: &mut S, row: &Row, shown: bool, hovered: bool) {
    let (rx, ry, rw, rh) = row.rect;
    if shown {
        fill_box(surf, rx, ry, rw, rh, BG_TOP());
        fill_box(surf, rx, ry, rw, 2, LASER());
        fill_box(surf, rx, ry, 1, rh, HEADER_RULE());
        fill_box(surf, rx + rw - 1, ry, 1, rh, HEADER_RULE());
    } else if hovered {
        fill_box(surf, rx, ry + 2, rw, rh - 3, HOVER_FILL());
    }
    let color = if shown || hovered { TEXT_BRIGHT } else { TEXT_MUTED() };
    put_text(surf, rx + TAB_PAD, mid_y(ry, rh) + 1, row.label, color);
}

/// A row's label with one quiet line under it (the next file name, or why a
/// choice is greyed).
pub(super) fn render_label_with_caption<S: painter::Surface>(surf: &mut S, row: &Row, caption: &str) {
    let (rx, ry, _, rh) = row.rect;
    let slack = (rh - 2 * bitmap_font::GLYPH_H) / 3;
    let top = ry + slack;
    put_text(surf, rx + 6, top, row.label, TEXT_BRIGHT);
    put_text(surf, rx + 6, top + bitmap_font::GLYPH_H + slack, caption, TEXT_MUTED());
}

/// A chip in a row of choices. The picked one is filled and framed.
/// `unavailable`: dim, though it still answers (GIF without
/// FFmpeg points at where FFmpeg is set up).
pub(super) fn render_choice_chip<S: painter::Surface>(surf: &mut S, row: &Row, hovered: bool, draft: &AppSettings,
                                                      unavailable: bool) {
    let chosen = match row.kind {
        WidgetKind::AfterCaptureChoice(after) => draft.after_capture() == after,
        WidgetKind::ScrollAfterChoice(after) => draft.scroll_after() == after,
        WidgetKind::FileNameChoice(choice) => framefang_core::file_name::preset_index(&draft.file_name_pattern) == choice,
        WidgetKind::RecordFormatChoice(gif) =>
            (draft.record_format() == framefang_core::settings::RecordFormat::Gif) == gif,
        WidgetKind::FrameChoice(f) => draft.output_frame() == f,
        WidgetKind::QrLinksChoice(open) => draft.open_qr_links == open,
        _ => return,
    };
    let edge = if chosen { LASER() } else if hovered { LASER_DIM() } else { FIELD_BORDER() };
    let label_color = if unavailable { TEXT_DIM() } else if chosen { TEXT_BRIGHT } else { TEXT_MUTED() };
    button(surf, row.rect, edge, (chosen || hovered).then(HOVER_FILL), row.label, label_color);
}

/// A color-scheme chip: the scheme's own accent as a swatch, then its name.
/// The chosen one is framed in its accent; the rest stay quiet until hovered.
pub(super) fn render_theme_chip<S: painter::Surface>(
    surf: &mut S, row: &Row, scheme: crate::palette::Scheme, hovered: bool, draft: &AppSettings,
) {
    const SWATCH: i32 = 8;
    const SWATCH_GAP: i32 = 6;
    let (rx, ry, rw, rh) = row.rect;
    let chosen = crate::palette::Scheme::parse(&draft.theme) == scheme;
    let edge = if chosen { scheme.accent() } else if hovered { LASER_DIM() } else { FIELD_BORDER() };
    if chosen || hovered { fill_box(surf, rx, ry, rw, rh, HOVER_FILL()); }
    frame_box(surf, rx, ry, rw, rh, edge);

    let group_w = SWATCH + SWATCH_GAP + bitmap_font::measure(row.label, 1);
    let left = rx + (rw - group_w) / 2;
    fill_box(surf, left, ry + (rh - SWATCH) / 2, SWATCH, SWATCH, scheme.accent());
    let color = if chosen { TEXT_BRIGHT } else { TEXT_MUTED() };
    put_text(surf, left + SWATCH + SWATCH_GAP, mid_y(ry, rh), row.label, color);
}

/// The width of an ON/OFF switch (its height comes from the row).
const TOGGLE_W: i32 = 64;

/// An ON/OFF switch anchored at (x, y) and `h` tall: a square knob on the
/// right when on, left when off, with the word on the other side.
fn draw_toggle<S: painter::Surface>(s: &mut S, x: i32, y: i32, h: i32, on: bool) {
    let w = TOGGLE_W;
    let (body, edge, knob_color, word_color) = if on {
        (TOGGLE_ON(), LASER_DIM(), TOGGLE_KNOB(), TEXT_BRIGHT)
    } else {
        (TOGGLE_OFF(), FIELD_BORDER(), TOGGLE_OFF_K(), TEXT_MUTED())
    };
    fill_box(s, x, y, w, h, body);
    frame_box(s, x, y, w, h, edge);
    let knob = h - 8;
    let knob_x = if on { x + w - knob - 4 } else { x + 4 };
    fill_box(s, knob_x, y + 4, knob, knob, knob_color);
    let word = if on { "ON" } else { "OFF" };
    let word_x = if on { x + 8 } else { x + w - bitmap_font::measure(word, 1) - 8 };
    put_text(s, word_x, mid_y(y, h), word, word_color);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buf(px: &mut Vec<u32>, w: i32, h: i32) -> PixelBuf<'_> {
        *px = vec![0; (w * h) as usize];
        PixelBuf { px, wide: w, tall: h }
    }

    #[test]
    fn a_palette_color_becomes_an_opaque_rgba() {
        let c = opaque(0x00_12_34_56);
        assert_eq!((c.r, c.g, c.b, c.a), (0x12, 0x34, 0x56, 255));
        // Whatever sits in the top byte is ignored.
        assert_eq!(opaque(0xFF_AB_CD_EF).r, 0xAB);
    }

    #[test]
    fn the_pixel_buffer_round_trips_and_clips() {
        let mut store = Vec::new();
        let mut b = buf(&mut store, 4, 3);
        painter::Surface::write(&mut b, 2, 1, [0x10, 0x20, 0x30, 0x80]);
        assert_eq!(painter::Surface::read(&b, 2, 1), [0x10, 0x20, 0x30, 0xFF]);
        assert_eq!(b.px[6], 0x00_10_20_30);
        // Off the edge: reads are black, writes vanish.
        painter::Surface::write(&mut b, -1, 0, [9, 9, 9, 255]);
        painter::Surface::write(&mut b, 4, 0, [9, 9, 9, 255]);
        painter::Surface::write(&mut b, 0, 3, [9, 9, 9, 255]);
        assert_eq!(painter::Surface::read(&b, 4, 0), [0, 0, 0, 0xFF]);
        assert_eq!(b.px.iter().filter(|p| **p != 0).count(), 1);
    }

    #[test]
    fn a_filled_box_covers_its_area_and_a_frame_only_the_edge() {
        let mut store = Vec::new();
        let mut b = buf(&mut store, 6, 6);
        fill_box(&mut b, 1, 1, 3, 2, 0x00_FF_00_00);
        assert_eq!(b.px.iter().filter(|p| **p == 0x00_FF_00_00).count(), 6);
        let mut store2 = Vec::new();
        let mut f = buf(&mut store2, 6, 6);
        frame_box(&mut f, 1, 1, 4, 4, 0x00_00_FF_00);
        // 4 x 4 outline: twelve edge pixels, the middle 2 x 2 untouched.
        assert_eq!(f.px.iter().filter(|p| **p == 0x00_00_FF_00).count(), 12);
        assert_eq!(f.px[2 * 6 + 2], 0);
        assert_eq!(f.px[1 * 6 + 1], 0x00_00_FF_00);
    }

    #[test]
    fn a_point_hits_a_rect_on_its_left_and_top_edges_only() {
        let r = (10, 20, 30, 8);
        assert!(rect_has(r, 10, 20));
        assert!(rect_has(r, 39, 27));
        assert!(!rect_has(r, 40, 27), "right edge is out");
        assert!(!rect_has(r, 39, 28), "bottom edge is out");
        assert!(!rect_has(r, 9, 20));
        assert!(!rect_has(r, 10, 19));
    }

    #[test]
    fn short_text_is_left_alone_and_long_text_loses_its_middle() {
        assert_eq!(truncate_for("short", 400), "short");
        let path = "C:/Users/Someone/Pictures/Screenshots/2026/November/capture-final.png";
        for max_px in [60, 120, 200] {
            let cut = truncate_for(path, max_px);
            assert!(bitmap_font::measure(&cut, 1) <= max_px, "{max_px}: {cut}");
            assert!(cut.contains(".."), "{cut}");
            assert!(cut.starts_with("C:"), "{cut}");
            assert!(cut.ends_with(".png"), "{cut}");
        }
    }

    #[test]
    fn a_very_narrow_field_shows_only_dots_or_nothing() {
        assert_eq!(truncate_for("abcdefghij", 0), "");
        let one = truncate_for("abcdefghij", bitmap_font::GLYPH_W);
        assert!(one == "." || one.is_empty(), "{one:?}");
        let few = truncate_for("abcdefghij", 2 * (bitmap_font::GLYPH_W + 1));
        assert!(few.chars().all(|c| c == '.') && !few.is_empty(), "{few:?}");
    }

    #[test]
    fn a_folder_field_shows_the_pick_or_a_fallback() {
        assert_eq!(shown_folder("D:/Shots", directories::UserDirs::picture_dir, "~/Pictures"), "D:/Shots");
        let blank = shown_folder("", directories::UserDirs::picture_dir, "~/Pictures");
        assert!(!blank.is_empty());
    }
}
