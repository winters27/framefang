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
}

impl PickMode {
    pub const ALL: [PickMode; 3] = [PickMode::Auto, PickMode::Screen, PickMode::AllScreens];

    /// The stored word. Anything unknown reads as Auto.
    pub fn parse(s: &str) -> PickMode {
        PickMode::ALL.into_iter().find(|m| m.name() == s).unwrap_or(PickMode::Auto)
    }

    pub fn name(self) -> &'static str {
        match self { PickMode::Auto => "Auto", PickMode::Screen => "Screen", PickMode::AllScreens => "AllScreens" }
    }

    /// Its chip in the mode bar.
    pub fn label(self) -> &'static str {
        match self {
            PickMode::Auto => "Region or window",
            PickMode::Screen => "Screen",
            PickMode::AllScreens => "All screens",
        }
    }

    /// The modes the bar offers: All screens only with more than one.
    pub fn offered(screens: usize) -> &'static [PickMode] {
        if screens > 1 { &PickMode::ALL } else { &PickMode::ALL[..2] }
    }

    /// This mode where only `offered` exist: All screens on one screen is
    /// that screen.
    pub fn available(self, screens: usize) -> PickMode {
        if self == PickMode::AllScreens && screens <= 1 { PickMode::Screen } else { self }
    }

    /// The next one Tab goes to.
    pub fn next(self, screens: usize) -> PickMode {
        let offered = PickMode::offered(screens);
        let i = offered.iter().position(|m| *m == self.available(screens)).unwrap_or(0);
        offered[(i + 1) % offered.len()]
    }

    /// The line at the bottom of the capture screen.
    pub fn hint(self) -> &'static str {
        match self {
            PickMode::Auto => "drag a region or click a window  -  Tab for modes  -  Esc to close",
            PickMode::Screen => "click a screen  -  Tab for modes  -  Esc to close",
            PickMode::AllScreens => "click to take every screen  -  Tab for modes  -  Esc to close",
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

/// Where a highlight with nothing to glide from starts: a touch larger than
/// its target, so it settles onto it.
pub fn arrival(to: Rect4) -> Rect4 {
    let grow = (to.2.min(to.3) / 40).clamp(4, 14);
    (to.0 - grow, to.1 - grow, to.2 + grow * 2, to.3 + grow * 2)
}

/// `s` cut to at most `max` characters, with `...` when it was longer.
pub fn shorten(s: &str, max: usize) -> String {
    if s.chars().count() <= max { return s.to_owned(); }
    let mut out: String = s.chars().take(max.saturating_sub(3)).collect();
    out.push_str("...");
    out
}

pub const BAR_H: i32 = 34;
pub const BAR_PAD: i32 = 4;
pub const BAR_GAP: i32 = 4;
pub const CHIP_PAD_X: i32 = 14;
/// Room after the chips for the quiet `Tab` that says how to switch.
pub const BAR_KEY_W: i32 = 40;
/// The bar's distance from the top of its screen.
pub const BAR_TOP: i32 = 20;

/// The mode bar and its chips, centered at the top of `bounds`. `width`
/// measures a chip's label.
pub fn bar_layout(bounds: Rect4, modes: &[PickMode], width: impl Fn(&str) -> i32) -> (Rect4, Vec<(PickMode, Rect4)>) {
    let chip_h = BAR_H - BAR_PAD * 2;
    let widths: Vec<i32> = modes.iter().map(|m| width(m.label()) + CHIP_PAD_X * 2).collect();
    let inner: i32 = widths.iter().sum::<i32>() + BAR_GAP * (modes.len() as i32 - 1).max(0);
    let bar_w = BAR_PAD * 2 + inner + BAR_KEY_W;
    let bar = (bounds.0 + (bounds.2 - bar_w) / 2, bounds.1 + BAR_TOP, bar_w, BAR_H);
    let mut x = bar.0 + BAR_PAD;
    let chips = modes.iter().zip(&widths).map(|(m, w)| {
        let r = (x, bar.1 + BAR_PAD, *w, chip_h);
        x += w + BAR_GAP;
        (*m, r)
    }).collect();
    (bar, chips)
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
        // Arriving from nothing it settles inward onto the target.
        let start = arrival(b);
        assert!(start.0 < b.0 && start.2 > b.2);
    }

    #[test]
    fn tab_cycles_the_modes_on_offer() {
        assert_eq!(PickMode::Auto.next(2), PickMode::Screen);
        assert_eq!(PickMode::Screen.next(2), PickMode::AllScreens);
        assert_eq!(PickMode::AllScreens.next(2), PickMode::Auto);
        // One screen: no All screens to go to, or to stay in.
        assert_eq!(PickMode::Screen.next(1), PickMode::Auto);
        assert_eq!(PickMode::AllScreens.available(1), PickMode::Screen);
        assert_eq!(PickMode::parse("nonsense"), PickMode::Auto);
        for m in PickMode::ALL { assert_eq!(PickMode::parse(m.name()), m); }
    }

    #[test]
    fn the_bar_is_centered_with_its_chips_inside_in_order() {
        let (bar, chips) = bar_layout((100, 0, 1000, 800), &PickMode::ALL, |s| s.len() as i32 * 7);
        let (left, right) = (bar.0 - 100, 1000 - (bar.0 - 100 + bar.2));
        assert!((left - right).abs() <= 1, "centered: {left} vs {right}");
        assert_eq!(bar.1, BAR_TOP);
        for w in chips.windows(2) { assert!(w[0].1 .0 + w[0].1 .2 < w[1].1 .0, "in order, apart"); }
        let last = chips.last().unwrap().1;
        assert!(last.0 + last.2 + BAR_KEY_W <= bar.0 + bar.2, "room for the Tab hint");
        for (_, r) in &chips { assert!(r.1 >= bar.1 && r.1 + r.3 <= bar.1 + bar.3); }
    }

    #[test]
    fn long_titles_are_shortened() {
        assert_eq!(shorten("abc", 10), "abc");
        assert_eq!(shorten("abcdefghijkl", 8), "abcde...");
    }
}
