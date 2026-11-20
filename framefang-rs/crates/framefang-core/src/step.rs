//! How a numbered step marker looks (Brandon 2026-11-20: "our steps are
//! ugly"): one of nine shapes, the colors of its three parts, and a soft
//! shadow under it or not.
//!
//! The sizes here are the ones the painter draws (`framefang-app`'s
//! `painter::step_marker`), so a click lands on what shows. A marker is
//! placed by its anchor, where the user clicked: its middle, or a pin's tip.

use serde::{Deserialize, Serialize};

use crate::color::Rgba;

/// A step's shape. `Disc` is the default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum StepShape {
    /// A solid disc, no ring.
    #[default]
    Disc,
    /// A disc inside a thin ring.
    Ringed,
    /// A light disc inside a thick colored ring.
    Outlined,
    /// A rounded square inside a thin ring.
    Square,
    /// A disc inside a faint wide ring of its color.
    Halo,
    /// A map pin: the tip is where it was placed.
    Pin,
    /// A dark see-through disc inside a colored ring.
    Hollow,
    /// Round for one digit, a pill for more.
    Pill,
    /// A dark square with corner brackets and pixel digits, the app's look.
    Terminal,
}

impl StepShape {
    /// In the order the step panel shows them.
    pub const ALL: [StepShape; 9] = [
        StepShape::Disc, StepShape::Ringed, StepShape::Outlined,
        StepShape::Square, StepShape::Halo, StepShape::Pin,
        StepShape::Hollow, StepShape::Pill, StepShape::Terminal,
    ];

    pub fn label(self) -> &'static str {
        match self {
            StepShape::Disc => "Disc",
            StepShape::Ringed => "Ring",
            StepShape::Outlined => "Outline",
            StepShape::Square => "Square",
            StepShape::Halo => "Halo",
            StepShape::Pin => "Pin",
            StepShape::Hollow => "Hollow",
            StepShape::Pill => "Pill",
            StepShape::Terminal => "Terminal",
        }
    }

    /// The part that wears the main color: the ring where the middle is
    /// light or dark, else the body.
    pub fn accent_part(self) -> StepPart {
        match self {
            StepShape::Outlined | StepShape::Hollow | StepShape::Terminal => StepPart::Ring,
            _ => StepPart::Body,
        }
    }

    /// Whether it has a ring (a halo or brackets count) to color.
    pub fn has_ring(self) -> bool { self != StepShape::Disc }
}

/// A part of a step marker that has its own color.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StepPart { Body, Ring, Number }

impl StepPart {
    pub const ALL: [StepPart; 3] = [StepPart::Body, StepPart::Ring, StepPart::Number];

    pub fn label(self) -> &'static str {
        match self { StepPart::Body => "Body", StepPart::Ring => "Ring", StepPart::Number => "Digits" }
    }
}

/// A marker's whole look.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct StepLook {
    #[serde(default)]
    pub shape: StepShape,
    pub body: Rgba,
    pub ring: Rgba,
    pub number: Rgba,
    #[serde(default = "yes")]
    pub shadow: bool,
}

fn yes() -> bool { true }

impl Default for StepLook {
    fn default() -> Self { StepLook::recipe(StepShape::Disc, Rgba::RED) }
}

/// White or near-black, whichever reads on `c`.
pub fn contrast(c: Rgba) -> Rgba {
    let l = 0.2126 * c.r as f32 + 0.7152 * c.g as f32 + 0.0722 * c.b as f32;
    if l > 165.0 { Rgba::new_opaque(24, 24, 28) } else { Rgba::WHITE }
}

impl StepLook {
    /// `shape` as it comes, its main color `accent`: what picking a shape
    /// in the step panel gives.
    pub fn recipe(shape: StepShape, accent: Rgba) -> StepLook {
        let dark = Rgba::new_opaque(11, 9, 12);
        let (body, ring, number) = match shape {
            StepShape::Disc | StepShape::Halo => (accent, accent, contrast(accent)),
            StepShape::Ringed | StepShape::Square | StepShape::Pin | StepShape::Pill =>
                (accent, Rgba::WHITE, contrast(accent)),
            // A light main color can't be read on the white middle: dark digits.
            StepShape::Outlined => (Rgba::WHITE, accent,
                                    if contrast(accent) == Rgba::WHITE { accent } else { contrast(accent) }),
            StepShape::Hollow => (Rgba::new(0, 0, 0, 150), accent, Rgba::WHITE),
            StepShape::Terminal => (dark, accent, accent),
        };
        StepLook { shape, body, ring, number, shadow: true }
    }

    /// The main color: the accent part's.
    pub fn accent(&self) -> Rgba { self.part(self.shape.accent_part()) }

    /// The same look in another shape, keeping its main color.
    pub fn with_shape(&self, shape: StepShape) -> StepLook {
        StepLook { shadow: self.shadow, ..StepLook::recipe(shape, self.accent().with_alpha(0xFF)) }
    }

    pub fn part(&self, p: StepPart) -> Rgba {
        match p { StepPart::Body => self.body, StepPart::Ring => self.ring, StepPart::Number => self.number }
    }

    pub fn set_part(&mut self, p: StepPart, c: Rgba) {
        match p { StepPart::Body => self.body = c, StepPart::Ring => self.ring = c, StepPart::Number => self.number = c }
    }
}

// ── sizes, in pixels ──────────────────────────────────────────────────────

/// A disc's radius; the other shapes are sized around it.
pub const RADIUS: f32 = 15.0;
/// A thin ring's width, outside the body.
pub const RING: f32 = 2.5;
/// Outlined's ring, inside its edge.
pub const THICK_RING: f32 = 3.5;
/// How far a halo reaches past the body.
pub const HALO: f32 = 6.0;
/// A pin's head sits this far above its tip.
pub const PIN_RISE: f32 = 25.0;
/// Bold digits' size for one, two and three digits in a round shape.
pub fn digit_px(digits: usize, shape: StepShape) -> f32 {
    if shape == StepShape::Pill { return 17.0; }
    match digits { 0 | 1 => 17.0, 2 => 15.0, _ => 12.0 }
}

/// How wide a pill is for `number`: round up to two digits' worth, then
/// long enough for the digits with room at both ends.
pub fn pill_half_width(number: u32) -> f32 {
    let text = number.min(999).to_string();
    let (w, _) = crate::text::measure(&text, digit_px(text.len(), StepShape::Pill));
    ((w + 1.0) / 2.0 + 9.0).max(RADIUS - 1.0)
}

/// What a marker covers, around its anchor: (left, top, right, bottom)
/// offsets, ring and halo in, shadow out.
pub fn extent(number: u32, look: &StepLook) -> (f32, f32, f32, f32) {
    let r = RADIUS;
    let round = |half: f32| (-half, -half, half, half);
    match look.shape {
        StepShape::Disc | StepShape::Hollow => round(r),
        StepShape::Ringed => round(r - 1.0 + RING),
        StepShape::Outlined => round(r + 1.0),
        StepShape::Square => round(r - 1.0 + RING),
        StepShape::Halo => round(r + HALO),
        StepShape::Terminal => round(r + 1.0),
        StepShape::Pill => {
            let hw = pill_half_width(number) + RING;
            (-hw, -(r - 1.0 + RING), hw, r - 1.0 + RING)
        }
        StepShape::Pin => {
            let head = r - 1.0 + RING;
            (-head, -PIN_RISE - head, head, RING)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_is_a_plain_disc_with_a_shadow_and_readable_digits() {
        let l = StepLook::default();
        assert_eq!(l.shape, StepShape::Disc);
        assert!(l.shadow);
        assert_eq!(l.number, Rgba::WHITE, "white on red");
        assert_ne!(contrast(Rgba::new_opaque(255, 230, 0)), Rgba::WHITE, "dark on yellow");
        let yellow = StepLook::recipe(StepShape::Outlined, Rgba::new_opaque(255, 230, 0));
        assert_eq!(yellow.number, contrast(yellow.ring), "dark digits, readable on its white middle");
    }

    #[test]
    fn another_shape_keeps_the_main_color_on_its_main_part() {
        let blue = Rgba::new_opaque(30, 100, 220);
        let disc = StepLook::recipe(StepShape::Disc, blue);
        let outlined = disc.with_shape(StepShape::Outlined);
        assert_eq!(outlined.ring, blue, "the ring wears it");
        assert_eq!(outlined.body, Rgba::WHITE);
        assert_eq!(outlined.with_shape(StepShape::Ringed).body, blue, "and back on the body");
        let mut quiet = disc;
        quiet.shadow = false;
        assert!(!quiet.with_shape(StepShape::Pin).shadow, "the shadow choice stays");
    }

    #[test]
    fn every_part_can_be_set_and_read() {
        let mut l = StepLook::default();
        for (i, p) in StepPart::ALL.into_iter().enumerate() {
            let c = Rgba::new_opaque(i as u8 * 50, 7, 9);
            l.set_part(p, c);
            assert_eq!(l.part(p), c);
        }
    }

    #[test]
    fn a_pill_grows_with_its_number_and_a_pin_stands_above_its_tip() {
        let pill = StepLook::recipe(StepShape::Pill, Rgba::RED);
        let (l1, _, r1, _) = extent(3, &pill);
        let (l2, _, r2, _) = extent(123, &pill);
        assert!(r2 - l2 > r1 - l1 + 10.0, "three digits make it longer");
        assert!((r1 - l1) >= 2.0 * RADIUS, "one digit is still round");
        let (_, top, _, bottom) = extent(1, &StepLook::recipe(StepShape::Pin, Rgba::RED));
        assert!(bottom <= RING + 0.1 && top < -PIN_RISE, "the tip is at the anchor");
    }

    #[test]
    fn a_saved_look_without_a_shape_or_shadow_reads_as_the_defaults() {
        let l: StepLook = serde_json::from_str(
            r#"{"body":{"r":1,"g":2,"b":3,"a":255},"ring":{"r":1,"g":2,"b":3,"a":255},"number":{"r":1,"g":2,"b":3,"a":255}}"#).unwrap();
        assert_eq!((l.shape, l.shadow), (StepShape::Disc, true));
    }
}
