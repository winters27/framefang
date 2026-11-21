//! What a click in the capture screen takes, before any region is dragged.
//!
//! The capture screen highlights what a click would take: the window under
//! the cursor, the screen it is on, or every screen, by the mode picked in
//! the bar at its top. A drag still draws a region in the first mode. The
//! highlight glides from one target to the next rather than jumping.
//!
//! Only the rules live here (modes, which window is hit, the glide, the
//! bar's layout); the editor draws them.

use std::time::Duration;

type Rect4 = (i32, i32, i32, i32);

/// How long the highlight takes to glide to a new target.
pub const GLIDE: Duration = Duration::from_millis(140);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickMode {
    /// Drag a region, or click the window under the cursor (the screen when
    /// it is over the desktop).
    Auto,
    /// Click the screen under the cursor.
    Screen,
    /// One click takes every screen.
    AllScreens,
    /// Drag over words (or click a window): their text goes to the clipboard.
    Text,
    /// Drag over a QR code (or click a window): what it says goes to the
    /// clipboard.
    Qr,
    /// Drag over a scrolling page (or click a window): it is scrolled to its
    /// end and stitched into one tall picture.
    Scroll,
}

/// How a one-shot mode draws its selection (the terminal look, Brandon
/// 2026-11-19): what its prompt says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Look { Text, Qr, Scroll }

impl PickMode {
    /// Every mode, in Tab order.
    pub const ALL: [PickMode; 6] = [PickMode::Auto, PickMode::Screen, PickMode::AllScreens, PickMode::Text,
                                    PickMode::Qr, PickMode::Scroll];
    /// The modes that take a picture. Only these are kept for the next
    /// capture: one that reads text is chosen for one capture at a time.
    pub const CAPTURE: [PickMode; 3] = [PickMode::Auto, PickMode::Screen, PickMode::AllScreens];

    /// The stored word. Anything unknown, and a reading mode, reads as Auto.
    pub fn parse(s: &str) -> PickMode {
        PickMode::CAPTURE.into_iter().find(|m| m.name() == s).unwrap_or(PickMode::Auto)
    }

    pub fn name(self) -> &'static str {
        match self {
            PickMode::Auto => "Auto", PickMode::Screen => "Screen", PickMode::AllScreens => "AllScreens",
            PickMode::Text => "Text", PickMode::Qr => "Qr", PickMode::Scroll => "Scroll",
        }
    }

    /// The terminal look this mode draws its selection in, if it is a
    /// one-shot mode.
    pub fn look(self) -> Option<Look> {
        match self {
            PickMode::Text => Some(Look::Text),
            PickMode::Qr => Some(Look::Qr),
            PickMode::Scroll => Some(Look::Scroll),
            _ => None,
        }
    }

    /// What this mode reads, if it reads rather than captures.
    pub fn reads(self) -> Option<crate::read_text::ReadKind> {
        match self {
            PickMode::Text => Some(crate::read_text::ReadKind::Text),
            PickMode::Qr => Some(crate::read_text::ReadKind::Qr),
            _ => None,
        }
    }

    /// The mode the next capture opens in, after this one: a one-shot mode
    /// (TEXT, QR, SCROLL) is never kept.
    pub fn remembered(self) -> Option<PickMode> {
        self.look().is_none().then_some(self)
    }

    /// The mode the next capture opens in once this one is picked, in a
    /// capture that opened in `opened`: a capture mode is kept; a reading
    /// mode keeps `opened`, so the capture modes Tab passed on the way to
    /// TEXT or QR are not.
    pub fn kept(self, opened: PickMode) -> PickMode {
        self.remembered().or(opened.remembered()).unwrap_or(PickMode::Auto)
    }

    /// Its chip in the mode bar.
    #[allow(dead_code)]
    pub fn label(self) -> &'static str {
        match self {
            PickMode::Auto => "Region or window",
            PickMode::Screen => "Screen",
            PickMode::AllScreens => "All screens",
            PickMode::Text => "Text",
            PickMode::Qr => "QR code",
            PickMode::Scroll => "Scrolling page",
        }
    }

    /// The modes the bar offers: All screens only with more than one; the
    /// one-shot modes only where a capture can use them (`readers`), not
    /// where a region is picked to record; SCROLL only where Frame Fang can
    /// scroll other windows.
    pub fn offered(screens: usize, readers: bool) -> Vec<PickMode> {
        PickMode::ALL.into_iter()
            .filter(|m| *m != PickMode::AllScreens || screens > 1)
            .filter(|m| readers || m.look().is_none())
            .filter(|m| *m != PickMode::Scroll || crate::scroll::supported())
            .collect()
    }

    /// This mode where only `offered` exist: All screens on one screen is
    /// that screen.
    pub fn available(self, screens: usize) -> PickMode {
        if self == PickMode::AllScreens && screens <= 1 { PickMode::Screen } else { self }
    }

    /// The next one Tab goes to.
    pub fn next(self, screens: usize, readers: bool) -> PickMode {
        let offered = PickMode::offered(screens, readers);
        let i = offered.iter().position(|m| *m == self.available(screens)).unwrap_or(0);
        offered[(i + 1) % offered.len()]
    }

    /// The mode's chip in the capture dock, in the dock's terminal capitals.
    pub fn dock_label(self) -> &'static str {
        match self {
            PickMode::Auto => "REGION",
            PickMode::Screen => "SCREEN",
            PickMode::AllScreens => "ALL SCREENS",
            PickMode::Text => "TEXT",
            PickMode::Qr => "QR",
            PickMode::Scroll => "SCROLL",
        }
    }
}

/// The frontmost window whose frame holds `p`. `windows` is frontmost
/// first, in the same coordinates as `p`.
pub fn window_at<'a>(windows: &'a [(Rect4, String)], p: (i32, i32)) -> Option<&'a (Rect4, String)> {
    windows.iter().find(|((x, y, w, h), _)| p.0 >= *x && p.0 < x + w && p.1 >= *y && p.1 < y + h)
}

/// `r` cut to `bounds`, or `None` when nothing of it is left.
pub fn clip(r: Rect4, bounds: Rect4) -> Option<Rect4> {
    let (x0, y0) = (r.0.max(bounds.0), r.1.max(bounds.1));
    let (x1, y1) = ((r.0 + r.2).min(bounds.0 + bounds.2), (r.1 + r.3).min(bounds.1 + bounds.3));
    (x1 > x0 && y1 > y0).then_some((x0, y0, x1 - x0, y1 - y0))
}

/// Where the highlight is `t` into a glide from `from` to `to`: ease-out,
/// so it leaves quickly and settles softly.
pub fn glide(from: Rect4, to: Rect4, t: Duration) -> Rect4 {
    let k = (t.as_secs_f32() / GLIDE.as_secs_f32()).clamp(0.0, 1.0);
    let k = 1.0 - (1.0 - k).powi(3);
    let mix = |a: i32, b: i32| (a as f32 + (b - a) as f32 * k).round() as i32;
    (mix(from.0, to.0), mix(from.1, to.1), mix(from.2, to.2), mix(from.3, to.3))
}

/// `s` cut to at most `max` characters, with `...` when it was longer.
pub fn shorten(s: &str, max: usize) -> String {
    if s.chars().count() <= max { return s.to_owned(); }
    let mut out: String = s.chars().take(max.saturating_sub(3)).collect();
    out.push_str("...");
    out
}

// ── the capture dock ────────────────────────────────────────────────────────
//
// One line at the bottom of the capture screen, in the app's HUD look
// (Brandon 2026-11-19, from mockups): the Tab key drawn as a key, `//`, one
// chip per mode, a divider, the keys that finish (Enter to start a
// recording, Esc to close) each with its word, and a blinking cursor. The
// pixel font at `DOCK_SCALE`; its letters are all one width, so a label
// decoding through random characters never changes size.

pub const DOCK_SCALE: i32 = 2;
/// Tall enough for a key and a few pixels around it (Brandon 2026-11-19:
/// 46 was "a little too tall"). Odd, so a key with its shadow (25 at scale
/// 2) leaves the same room above and below.
pub const DOCK_H: i32 = 35;
/// The accent bar down the dock's left edge.
pub const DOCK_BAR_W: i32 = 3;
pub const DOCK_PAD_X: i32 = 12;
pub const DOCK_GAP: i32 = 10;
/// From the bottom of the screen's usable area (above the taskbar).
pub const DOCK_MARGIN: i32 = 24;
/// A chip is as tall as a key's face, so their words share a line.
pub const CHIP_H: i32 = 22;
pub const CHIP_PAD_X: i32 = 10;
pub const CHIP_GAP: i32 = 6;

/// Where everything in the dock goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dock {
    pub rect: Rect4,
    pub tab: Rect4,
    /// Left edge of the `//` after the Tab key.
    pub slashes_x: i32,
    pub chips: Vec<(PickMode, Rect4)>,
    pub divider_x: i32,
    /// The finishing keys: name, word, the key's box, the word's left edge.
    pub keys: Vec<(&'static str, &'static str, Rect4, i32)>,
    /// The blinking cursor's block.
    pub cursor: Rect4,
    /// The top of the words' line (`//` and the keys' words), the same line
    /// as the names on the keys and chips.
    pub text_y: i32,
}

fn text_w(s: &str) -> i32 { crate::bitmap_font::measure(s, DOCK_SCALE) }

/// A key's width (`keycap`).
pub fn key_w(name: &str) -> i32 { crate::keycap::key_w(name, DOCK_SCALE) }

/// The dock for `modes`, centered at the bottom of `bounds`. A record-area
/// picker (`record`) also offers Enter to start.
pub fn dock_layout(bounds: Rect4, modes: &[PickMode], record: bool) -> Dock {
    let finish: &[(&'static str, &'static str)] =
        if record { &[("ENTER", "START"), ("ESC", "CLOSE")] } else { &[("ESC", "CLOSE")] };
    // The whole key, shadow included, is centered: centering only its face
    // left the shadow hanging low and the key looking off center (Brandon
    // 2026-11-19). The chips and the words then sit on the faces' line, so
    // every label in the dock shares one middle.
    let key_h = crate::keycap::height(DOCK_SCALE);
    let key_y = (DOCK_H - key_h) / 2;
    let chip_y = key_y + (crate::keycap::face_h(DOCK_SCALE) - CHIP_H) / 2;
    let text_y = key_y + (crate::keycap::face_h(DOCK_SCALE) - crate::bitmap_font::GLYPH_H * DOCK_SCALE) / 2;
    // Laid out from 0, then moved to its place.
    let mut x = DOCK_BAR_W + DOCK_PAD_X;
    let tab = (x, key_y, key_w("TAB"), key_h);
    x += tab.2 + DOCK_GAP;
    let slashes_x = x;
    x += text_w("//") + DOCK_GAP;
    let mut chips = Vec::new();
    for m in modes {
        let w = text_w(m.dock_label()) + CHIP_PAD_X * 2;
        chips.push((*m, (x, chip_y, w, CHIP_H)));
        x += w + CHIP_GAP;
    }
    x += DOCK_GAP - CHIP_GAP;
    let divider_x = x;
    x += 1 + DOCK_GAP;
    let mut keys = Vec::new();
    for &(name, word) in finish {
        let kr = (x, key_y, key_w(name), key_h);
        let word_x = x + kr.2 + 8;
        keys.push((name, word, kr, word_x));
        x = word_x + text_w(word) + DOCK_GAP;
    }
    let glyph_h = crate::bitmap_font::GLYPH_H * DOCK_SCALE;
    let cursor_x = x - DOCK_GAP + 8;
    let cursor = (cursor_x, text_y, crate::bitmap_font::GLYPH_W * DOCK_SCALE, glyph_h);
    let w = cursor_x + cursor.2 + DOCK_PAD_X;

    let (dx, dy) = (bounds.0 + (bounds.2 - w) / 2, bounds.1 + bounds.3 - DOCK_H - DOCK_MARGIN);
    let mv = |r: Rect4| (r.0 + dx, r.1 + dy, r.2, r.3);
    Dock {
        rect: (dx, dy, w, DOCK_H),
        tab: mv(tab),
        slashes_x: slashes_x + dx,
        chips: chips.into_iter().map(|(m, r)| (m, mv(r))).collect(),
        divider_x: divider_x + dx,
        keys: keys.into_iter().map(|(n, wd, r, wx)| (n, wd, mv(r), wx + dx)).collect(),
        cursor: mv(cursor),
        text_y: text_y + dy,
    }
}

/// How long the scanner sweep crosses the dock as it opens, the decode of a
/// newly picked mode, its steps, and how long the Tab key stays down.
pub const SWEEP: Duration = Duration::from_millis(700);
pub const DECODE: Duration = Duration::from_millis(480);
pub const DECODE_STEP: Duration = Duration::from_millis(45);
pub const KEY_PRESS: Duration = Duration::from_millis(110);
/// The cursor is on for this long, then off for as long.
pub const BLINK: Duration = Duration::from_millis(500);

/// `label` `t` into its decode: letters settle left to right, the rest are
/// noise that changes every `DECODE_STEP`. Same length as `label`, and
/// spaces stay spaces.
pub fn decoding(label: &str, t: Duration) -> String {
    const NOISE: &[u8] = b"#%&@$*+=<>/";
    let k = (t.as_secs_f32() / DECODE.as_secs_f32()).clamp(0.0, 1.0);
    let n = label.chars().count();
    let settled = (k * n as f32).floor() as usize;
    let step = (t.as_millis() / DECODE_STEP.as_millis()) as u32;
    label.chars().enumerate().map(|(i, c)| {
        if c == ' ' || i < settled { return c; }
        let h = step.wrapping_mul(2_654_435_761) ^ (i as u32).wrapping_mul(40_503).rotate_left(11);
        NOISE[(h >> 7) as usize % NOISE.len()] as char
    }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_frontmost_window_under_the_cursor_wins() {
        let wins = vec![((10, 10, 50, 50), "front".to_owned()), ((0, 0, 200, 200), "back".to_owned())];
        assert_eq!(window_at(&wins, (20, 20)).unwrap().1, "front");
        assert_eq!(window_at(&wins, (100, 100)).unwrap().1, "back");
        assert!(window_at(&wins, (300, 300)).is_none());
        // The right and bottom edges are outside.
        assert_eq!(window_at(&wins, (60, 20)).unwrap().1, "back");
    }

    #[test]
    fn a_window_off_the_desktop_is_cut_to_it() {
        assert_eq!(clip((-20, 10, 100, 50), (0, 0, 1920, 1080)), Some((0, 10, 80, 50)));
        assert_eq!(clip((2000, 0, 50, 50), (0, 0, 1920, 1080)), None);
    }

    #[test]
    fn the_glide_leaves_fast_and_lands_exactly() {
        let (a, b) = ((0, 0, 100, 100), (200, 100, 300, 50));
        assert_eq!(glide(a, b, Duration::ZERO), a);
        assert_eq!(glide(a, b, GLIDE), b);
        assert_eq!(glide(a, b, GLIDE * 3), b);
        let quarter = glide(a, b, GLIDE / 4);
        assert!(quarter.0 > 100, "past halfway a quarter of the way in: {quarter:?}");
        // Gliding from where it already is goes nowhere: how the first box
        // of a capture lands in place.
        assert_eq!(glide(b, b, Duration::ZERO), b);
    }

    #[test]
    fn tab_cycles_the_modes_on_offer() {
        assert_eq!(PickMode::Auto.next(2, false), PickMode::Screen);
        assert_eq!(PickMode::Screen.next(2, false), PickMode::AllScreens);
        assert_eq!(PickMode::AllScreens.next(2, false), PickMode::Auto);
        // One screen: no All screens to go to, or to stay in.
        assert_eq!(PickMode::Screen.next(1, false), PickMode::Auto);
        assert_eq!(PickMode::AllScreens.available(1), PickMode::Screen);
        assert_eq!(PickMode::parse("nonsense"), PickMode::Auto);
        for m in PickMode::CAPTURE { assert_eq!(PickMode::parse(m.name()), m); }
    }

    #[test]
    fn the_reading_modes_follow_the_capture_ones_and_are_never_kept() {
        let scroll: &[PickMode] = if crate::scroll::supported() { &[PickMode::Scroll] } else { &[] };
        assert_eq!(PickMode::offered(2, true),
                   [&[PickMode::Auto, PickMode::Screen, PickMode::AllScreens, PickMode::Text, PickMode::Qr][..], scroll].concat());
        assert_eq!(PickMode::offered(1, true),
                   [&[PickMode::Auto, PickMode::Screen, PickMode::Text, PickMode::Qr][..], scroll].concat());
        assert_eq!(PickMode::offered(2, false), PickMode::CAPTURE.to_vec(), "a record picker reads nothing");
        assert_eq!(PickMode::AllScreens.next(2, true), PickMode::Text);
        let last = *PickMode::offered(2, true).last().unwrap();
        assert_eq!(last.next(2, true), PickMode::Auto, "round again");
        // The next capture never opens in a one-shot mode.
        assert_eq!(PickMode::Text.remembered(), None);
        assert_eq!(PickMode::Scroll.remembered(), None);
        assert_eq!(PickMode::parse("Scroll"), PickMode::Auto);
        assert_eq!(PickMode::Screen.remembered(), Some(PickMode::Screen));
        assert_eq!(PickMode::parse("Text"), PickMode::Auto, "a stored reading mode reads as Auto");
        assert_eq!(PickMode::parse("Qr"), PickMode::Auto);
    }

    #[test]
    fn tabbing_on_to_text_keeps_the_mode_the_capture_opened_in() {
        // Opened in Region, Tab passes Screen and All screens to reach TEXT:
        // the next capture opens in Region again.
        let opened = PickMode::Auto;
        let mut kept = opened;
        for m in [PickMode::Screen, PickMode::AllScreens, PickMode::Text] { kept = m.kept(opened); }
        assert_eq!(kept, PickMode::Auto);
        assert_eq!(PickMode::Qr.kept(PickMode::Screen), PickMode::Screen);
        // Round again to a capture mode: that one is kept.
        assert_eq!(PickMode::Screen.kept(PickMode::Auto), PickMode::Screen);
    }

    #[test]
    fn the_dock_with_every_mode_fits_a_small_screen() {
        let d = dock_layout((0, 0, 1280, 720), &PickMode::offered(2, true), false);
        assert!(d.rect.0 >= 0 && d.rect.0 + d.rect.2 <= 1280, "{:?}", d.rect);
    }

    #[test]
    fn the_dock_is_centered_at_the_bottom_in_order_and_inside() {
        for record in [false, true] {
            let d = dock_layout((100, 0, 1400, 800), &PickMode::offered(2, !record), record);
            let (left, right) = (d.rect.0 - 100, 1400 - (d.rect.0 - 100 + d.rect.2));
            assert!((left - right).abs() <= 1, "centered: {left} vs {right}");
            assert_eq!(d.rect.1 + d.rect.3, 800 - DOCK_MARGIN, "on the bottom of the screen");
            // Left to right: Tab, //, the chips, the divider, the keys, the cursor.
            assert!(d.tab.0 + d.tab.2 < d.slashes_x);
            assert!(d.slashes_x < d.chips[0].1 .0);
            for w in d.chips.windows(2) { assert!(w[0].1 .0 + w[0].1 .2 < w[1].1 .0, "in order, apart"); }
            let last_chip = d.chips.last().unwrap().1;
            assert!(last_chip.0 + last_chip.2 < d.divider_x && d.divider_x < d.keys[0].2 .0);
            let (_, word, _, wx) = *d.keys.last().unwrap();
            assert!(wx + text_w(word) < d.cursor.0);
            assert!(d.cursor.0 + d.cursor.2 <= d.rect.0 + d.rect.2 - DOCK_PAD_X);
            for r in d.chips.iter().map(|c| c.1).chain([d.tab]).chain(d.keys.iter().map(|k| k.2)) {
                assert!(r.1 >= d.rect.1 && r.1 + r.3 <= d.rect.1 + d.rect.3, "{r:?} inside the dock");
            }
            // A key, shadow and all, has as much room above it as below.
            let (above, below) = (d.tab.1 - d.rect.1, d.rect.1 + d.rect.3 - (d.tab.1 + d.tab.3));
            assert_eq!(above, below, "the keys are centered in the dock");
            // Every label shares one middle: the chips' words, the keys' names, the rest.
            let face_mid = d.tab.1 + crate::keycap::face_h(DOCK_SCALE) / 2;
            let chip_mid = d.chips[0].1 .1 + CHIP_H / 2;
            let text_mid = d.text_y + crate::bitmap_font::GLYPH_H * DOCK_SCALE / 2;
            assert_eq!((face_mid, chip_mid), (text_mid, text_mid));
            assert_eq!(d.keys.len(), if record { 2 } else { 1 }, "Enter only where it starts something");
        }
    }

    #[test]
    fn a_decode_keeps_the_label_length_and_settles_on_it() {
        for label in ["REGION", "ALL SCREENS"] {
            for ms in [0u64, 45, 200, 470] {
                let s = decoding(label, Duration::from_millis(ms));
                assert_eq!(s.chars().count(), label.chars().count(), "same width at {ms} ms");
                assert_eq!(s.find(' '), label.find(' '), "spaces stay put");
            }
            assert_eq!(decoding(label, DECODE), label, "settled at the end");
            assert_ne!(decoding(label, Duration::ZERO), label, "noise at the start");
        }
    }

    #[test]
    fn long_titles_are_shortened() {
        assert_eq!(shorten("abc", 10), "abc");
        assert_eq!(shorten("abcdefghijkl", 8), "abcde...");
    }
}
