//! Keys drawn as keys, everywhere the app names one (Brandon 2026-11-19:
//! "everywhere else in the app theres key descriptors ... we should show
//! keycaps like we did"): the capture dock, tooltips, notices, the color
//! picker's card, the tray menu, Settings and the welcome window.
//!
//! A key (Brandon picked it from mockups, 2026-11-19): a dark face with its
//! top-left and bottom-right corners cut, outlined in the color scheme's
//! accent with its name in the accent in the pixel font's capitals, on a
//! hard shadow of the same shape offset down and right in the deep accent.
//! Pressed, the face slides into the shadow and fills with the accent.
//!
//! Text that names keys marks each one in brackets, `"Undo [Ctrl]+[Z]"`;
//! [`line_width`] and [`draw_line`] lay such a line out with the words in
//! the pixel font and each key drawn as a key, its face centered on the
//! words' line. Everything draws into any `painter::Surface`; the `buf`
//! versions are for a plain `0x00RRGGBB` buffer.

use crate::bitmap_font::{measure, GLYPH_H};
use crate::painter::{Surface, U32Surface};
use crate::palette::*;

/// The key's face: the name plus room above and below.
pub fn face_h(scale: i32) -> i32 { GLYPH_H * scale + 4 * scale }
/// How far the shadow sits down and right of the face, and so how far a
/// press moves it.
pub fn edge(scale: i32) -> i32 { scale + 1 }
/// A whole key, face and shadow.
pub fn height(scale: i32) -> i32 { face_h(scale) + edge(scale) }
fn pad_x(scale: i32) -> i32 { 4 * scale }
/// How much of the top-left and bottom-right corners is cut away.
fn cut(scale: i32) -> i32 { 2 * scale + 1 }

/// How wide the key named `name` is, shadow included.
pub fn key_w(name: &str, scale: i32) -> i32 {
    face_w(name, scale) + edge(scale)
}

fn face_w(name: &str, scale: i32) -> i32 {
    measure(&name.to_uppercase(), scale) + pad_x(scale) * 2
}

fn rgba(c: u32) -> framefang_core::color::Rgba {
    framefang_core::color::Rgba::new_opaque((c >> 16) as u8, (c >> 8) as u8, c as u8)
}

fn fill<S: Surface>(s: &mut S, x0: i32, y0: i32, x1: i32, y1: i32, c: u32) {
    let px = [(c >> 16) as u8, (c >> 8) as u8, c as u8, 255];
    for y in y0.max(0)..y1.min(s.height()) {
        for x in x0.max(0)..x1.min(s.width()) {
            s.write(x, y, px);
        }
    }
}

/// Row `r` of a `w` x `h` face with `c` cut from two corners: the span
/// `[left, right)` inside it.
fn span(r: i32, w: i32, h: i32, c: i32) -> (i32, i32) {
    ((c - r).max(0), w - (r - (h - 1 - c)).max(0))
}

/// The chamfered shape at `(x, y)`, filled with `inside`, its edge in
/// `outline` when given.
#[allow(clippy::too_many_arguments)]
fn chamfer<S: Surface>(s: &mut S, x: i32, y: i32, w: i32, h: i32, c: i32, inside: u32, outline: Option<u32>) {
    for r in 0..h {
        let (l, rt) = span(r, w, h, c);
        fill(s, x + l, y + r, x + rt, y + r + 1, inside);
        if let Some(o) = outline {
            if r == 0 || r == h - 1 {
                fill(s, x + l, y + r, x + rt, y + r + 1, o);
            } else {
                // One pixel at each end: down the cuts, the rows' ends step
                // one pixel at a time, which draws an unbroken diagonal.
                fill(s, x + l, y + r, x + l + 1, y + r + 1, o);
                fill(s, x + rt - 1, y + r, x + rt, y + r + 1, o);
            }
        }
    }
}

/// Draw the key `name` with its face's top left at `(x, face_y)` (its
/// shadow falls down and right of that). Returns its width, shadow included.
pub fn draw_key_on<S: Surface>(s: &mut S, x: i32, face_y: i32, name: &str, scale: i32, pressed: bool) -> i32 {
    let (fw, fh, e, c) = (face_w(name, scale), face_h(scale), edge(scale), cut(scale));
    let (ink, fx, fy) = if pressed {
        // In the shadow's place, filled: the shadow is under it.
        chamfer(s, x + e, face_y + e, fw, fh, c, LASER(), None);
        (0x00_00_00_00, x + e, face_y + e)
    } else {
        chamfer(s, x + e, face_y + e, fw, fh, c, TOGGLE_ON(), None);
        chamfer(s, x, face_y, fw, fh, c, HOVER_FILL(), Some(LASER()));
        (LASER(), x, face_y)
    };
    let label = name.to_uppercase();
    let tw = measure(&label, scale);
    crate::painter::draw_text(s, fx + (fw - tw) / 2, fy + (fh - GLYPH_H * scale) / 2, scale, &label, rgba(ink));
    fw + e
}

/// [`draw_key_on`] into a plain buffer `w` pixels wide.
#[allow(clippy::too_many_arguments)]
pub fn draw_key(buf: &mut [u32], w: usize, h: usize, x: i32, face_y: i32, name: &str, scale: i32, pressed: bool) -> i32 {
    draw_key_on(&mut U32Surface { buf, stride: w as i32, height: h as i32 }, x, face_y, name, scale, pressed)
}

/// A chord as Settings describes it (`"Ctrl + Shift + R"`) marked up as
/// keys: `"[Ctrl]+[Shift]+[R]"`.
pub fn chord(described: &str) -> String {
    described.split('+').map(str::trim).filter(|k| !k.is_empty())
        .map(|k| format!("[{k}]")).collect::<Vec<_>>().join("+")
}

#[derive(Debug, PartialEq, Eq)]
enum Piece<'a> { Text(&'a str), Key(&'a str) }

fn pieces(text: &str) -> Vec<Piece<'_>> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(open) = rest.find('[') {
        let Some(close) = rest[open..].find(']').map(|c| open + c) else { break };
        if open > 0 { out.push(Piece::Text(&rest[..open])); }
        out.push(Piece::Key(&rest[open + 1..close]));
        rest = &rest[close + 1..];
    }
    if !rest.is_empty() { out.push(Piece::Text(rest)); }
    out
}

/// Room between a key and what is next to it.
fn gap(scale: i32) -> i32 { 2 * scale }

fn piece_w(p: &Piece, scale: i32) -> i32 {
    match p {
        Piece::Text(t) => measure(t, scale) + scale,
        Piece::Key(k) => key_w(k, scale) + gap(scale),
    }
}

fn trail(ps: &[Piece], scale: i32) -> i32 {
    match ps.last() { Some(Piece::Text(_)) => scale, Some(Piece::Key(_)) => gap(scale), None => 0 }
}

/// How wide `text` is with its `[Key]`s drawn as keys.
pub fn line_width(text: &str, scale: i32) -> i32 {
    let ps = pieces(text);
    ps.iter().map(|p| piece_w(p, scale)).sum::<i32>() - trail(&ps, scale)
}

/// Whether `text` names a key, so its line needs a key's height.
pub fn has_keys(text: &str) -> bool { pieces(text).iter().any(|p| matches!(p, Piece::Key(_))) }

/// How tall a line of `text` is: a key's height when it has one.
pub fn line_height(text: &str, scale: i32) -> i32 {
    if has_keys(text) { height(scale) } else { GLYPH_H * scale }
}

/// Draw `text` with its words' top at `text_y`, the words in `ink` and each
/// `[Key]` as a key, its face centered on the words. Returns the width, as
/// [`line_width`] measures it.
#[allow(clippy::too_many_arguments)]
pub fn draw_line_on<S: Surface>(s: &mut S, x: i32, text_y: i32, text: &str, scale: i32, ink: u32) -> i32 {
    let face_y = text_y - (face_h(scale) - GLYPH_H * scale) / 2;
    let ps = pieces(text);
    let mut cx = x;
    for p in &ps {
        match p {
            Piece::Text(t) => crate::painter::draw_text(s, cx, text_y, scale, t, rgba(ink)),
            Piece::Key(k) => { draw_key_on(s, cx + gap(scale) / 2, face_y, k, scale, false); }
        }
        cx += piece_w(p, scale);
    }
    cx - x - trail(&ps, scale)
}

/// [`draw_line_on`] into a plain buffer `w` pixels wide.
#[allow(clippy::too_many_arguments)]
pub fn draw_line(buf: &mut [u32], w: usize, h: usize, x: i32, text_y: i32, text: &str, scale: i32, ink: u32) -> i32 {
    draw_line_on(&mut U32Surface { buf, stride: w as i32, height: h as i32 }, x, text_y, text, scale, ink)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn brackets_mark_keys_and_the_rest_is_words() {
        assert_eq!(pieces("Undo [Ctrl]+[Z]"),
                   vec![Piece::Text("Undo "), Piece::Key("Ctrl"), Piece::Text("+"), Piece::Key("Z")]);
        assert_eq!(pieces("[Esc] closes"), vec![Piece::Key("Esc"), Piece::Text(" closes")]);
        assert_eq!(pieces("no keys"), vec![Piece::Text("no keys")]);
        assert_eq!(pieces("open [bracket"), vec![Piece::Text("open [bracket")], "a lone [ is just text");
    }

    #[test]
    fn a_described_chord_becomes_keys() {
        assert_eq!(chord("Ctrl + Shift + R"), "[Ctrl]+[Shift]+[R]");
        assert_eq!(chord("Print Screen"), "[Print Screen]");
    }

    #[test]
    fn a_line_is_as_wide_as_what_it_draws_and_stays_inside() {
        for scale in [1, 2] {
            for text in ["Undo [Ctrl]+[Z]", "[Esc] closes", "plain words", "Hold [Shift] to copy only"] {
                let (w, h) = (600usize, 60usize);
                let mut buf = vec![0u32; w * h];
                let drawn = draw_line(&mut buf, w, h, 10, 20, text, scale, 0x00_FF_FF_FF);
                assert_eq!(drawn, line_width(text, scale), "{text} at {scale}");
                let last_lit = (0..w).rev().find(|&x| (0..h).any(|y| buf[y * w + x] != 0)).unwrap() as i32;
                assert!(last_lit < 10 + drawn + gap(scale), "{text} stays inside its width");
            }
        }
    }

    #[test]
    fn a_key_has_its_corners_cut_and_its_shadow_down_and_right() {
        let (w, h) = (80usize, 40usize);
        let mut buf = vec![0u32; w * h];
        let kw = draw_key(&mut buf, w, h, 5, 5, "Esc", 2, false);
        let at = |x: i32, y: i32| buf[y as usize * w + x as usize];
        assert_eq!(at(5, 5), 0, "the top-left corner is cut");
        assert_ne!(at(5 + cut(2) + 1, 5), 0, "the top edge starts past the cut");
        let (fw, fh) = (kw - edge(2), face_h(2));
        assert_ne!(at(5 + fw, 5 + fh / 2), 0, "the shadow shows right of the face");
        assert_ne!(at(5 + fw / 2, 5 + fh), 0, "and under it");
    }
}
