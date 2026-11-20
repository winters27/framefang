//! The one color type Frame Fang passes around: four 8-bit channels, straight
//! (not premultiplied) alpha.
//!
//! The serde shape is a plain `{"r":..,"g":..,"b":..,"a":..}` object and the
//! packed form is the Win32 `0xAARRGGBB` integer. Settings files already on
//! users' disks hold both, so neither may change.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Rgba {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

/// Blend one channel `t` of the way from `from` to `to`, rounding to the
/// nearest step. Written as a weighted sum of both ends (rather than
/// `from + (to - from) * t`) so `t == 0` and `t == 1` land exactly on the
/// endpoints.
fn blend_channel(from: u8, to: u8, t: f32) -> u8 {
    (from as f32 * (1.0 - t) + to as f32 * t).round() as u8
}

impl Rgba {
    /// The default annotation red.
    pub const RED: Rgba = Rgba::new_opaque(220, 38, 38);
    pub const WHITE: Rgba = Rgba::new_opaque(255, 255, 255);
    pub const BLACK: Rgba = Rgba::new_opaque(0, 0, 0);

    pub const fn new(r: u8, g: u8, b: u8, a: u8) -> Self {
        Rgba { r, g, b, a }
    }

    /// A fully opaque color.
    pub const fn new_opaque(r: u8, g: u8, b: u8) -> Self {
        Rgba::new(r, g, b, u8::MAX)
    }

    /// Pack into the signed `0xAARRGGBB` integer Win32 and the settings file
    /// use. Opaque colors come out negative, because alpha owns the sign bit.
    pub fn to_argb(self) -> i32 {
        let packed = (self.a as u32) << 24
            | (self.r as u32) << 16
            | (self.g as u32) << 8
            | self.b as u32;
        packed as i32
    }

    /// Inverse of [`Rgba::to_argb`].
    pub fn from_argb(v: i32) -> Self {
        let bits = v as u32;
        let byte = |shift: u32| (bits >> shift) as u8;
        Rgba::new(byte(16), byte(8), byte(0), byte(24))
    }

    /// The same color with its alpha replaced.
    pub fn with_alpha(self, alpha: u8) -> Self {
        Rgba { a: alpha, ..self }
    }

    /// Mix toward `other`. `t` is clamped into `[0, 1]`: 0 keeps `self`,
    /// 1 gives `other`, and every channel (alpha included) moves together.
    pub fn lerp(self, other: Rgba, t: f32) -> Rgba {
        let share = t.clamp(0.0, 1.0);
        let mix = |from: u8, to: u8| blend_channel(from, to, share);
        Rgba::new(mix(self.r, other.r), mix(self.g, other.g), mix(self.b, other.b), mix(self.a, other.a))
    }

    /// Channels as `[r, g, b, a]` in `0.0..=1.0`, the form GPU-style APIs
    /// take.
    pub fn to_rgba_f32(self) -> [f32; 4] {
        [self.r, self.g, self.b, self.a].map(|c| c as f32 / 255.0)
    }
}

impl Default for Rgba {
    /// Red, because that is what a new annotation is drawn in.
    fn default() -> Self {
        Self::RED
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn argb_packs_alpha_into_the_top_byte() {
        let c = Rgba::new(0x12, 0x34, 0x56, 0x78);
        assert_eq!(c.to_argb(), 0x7812_3456);
        // Opaque white is all ones, which as a signed value is -1.
        assert_eq!(Rgba::WHITE.to_argb(), -1);
        assert_eq!(Rgba::new(0, 0, 0, 0).to_argb(), 0);
    }

    #[test]
    fn argb_round_trips_every_channel() {
        for c in [Rgba::RED, Rgba::WHITE, Rgba::BLACK, Rgba::new(1, 2, 3, 4), Rgba::new(200, 0, 255, 128)] {
            assert_eq!(Rgba::from_argb(c.to_argb()), c);
        }
        assert_eq!(Rgba::from_argb(0xFFDC_2626_u32 as i32), Rgba::RED);
    }

    #[test]
    fn with_alpha_only_touches_alpha() {
        let c = Rgba::RED.with_alpha(40);
        assert_eq!((c.r, c.g, c.b, c.a), (220, 38, 38, 40));
    }

    #[test]
    fn lerp_hits_both_ends_and_rounds_the_middle() {
        let a = Rgba::new(0, 100, 255, 0);
        let b = Rgba::new(255, 100, 0, 255);
        assert_eq!(a.lerp(b, 0.0), a);
        assert_eq!(a.lerp(b, 1.0), b);
        // 127.5 rounds away from zero.
        assert_eq!(a.lerp(b, 0.5), Rgba::new(128, 100, 128, 128));
        // Out-of-range t is clamped rather than overshooting.
        assert_eq!(a.lerp(b, -3.0), a);
        assert_eq!(a.lerp(b, 7.0), b);
    }

    #[test]
    fn float_channels_span_zero_to_one() {
        assert_eq!(Rgba::WHITE.to_rgba_f32(), [1.0; 4]);
        assert_eq!(Rgba::new(0, 0, 0, 0).to_rgba_f32(), [0.0; 4]);
        let [r, _, _, a] = Rgba::new(51, 0, 0, 255).to_rgba_f32();
        assert!((r - 0.2).abs() < 1e-6);
        assert_eq!(a, 1.0);
    }

    #[test]
    fn default_is_the_annotation_red() {
        assert_eq!(Rgba::default(), Rgba::new(220, 38, 38, 255));
        assert_eq!(Rgba::BLACK, Rgba::new(0, 0, 0, 255));
    }

    #[test]
    fn json_shape_is_a_named_channel_object() {
        // Settings files on disk depend on exactly this spelling.
        let json = serde_json::to_string(&Rgba::new(1, 2, 3, 4)).unwrap();
        assert_eq!(json, r#"{"r":1,"g":2,"b":3,"a":4}"#);
        let back: Rgba = serde_json::from_str(r#"{"a":255,"b":38,"g":38,"r":220}"#).unwrap();
        assert_eq!(back, Rgba::RED);
    }
}
