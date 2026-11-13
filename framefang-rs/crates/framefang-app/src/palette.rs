//! Framefang's colors, in one place.
//!
//! Cyberpunk and data: one neon accent on near-black, the monospace pixel
//! font and `NAME // PAGE` titles reading like a terminal. The accent and the
//! faint tint in the blacks come from the color scheme the user picks in
//! Settings (Theme): Pink matches the logo and is the default, Green is the
//! laser green Framefang started with, then Cyan and Yellow.
//!
//! The scheme only dresses the dialogs and the title bar. The capture editor
//! keeps its own fixed colors, so a screenshot always looks the same.
//!
//! A color that follows the scheme is a function (`LASER()`); one that never
//! changes is a constant (`TEXT_BRIGHT`). All `0x00RRGGBB`, the softbuffer
//! pixel format.
#![allow(non_snake_case)]

use std::sync::atomic::{AtomicU8, Ordering};

/// A color scheme for the dialogs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scheme {
    Pink,
    Green,
    Cyan,
    Yellow,
}

impl Scheme {
    pub const ALL: [Scheme; 4] = [Scheme::Pink, Scheme::Green, Scheme::Cyan, Scheme::Yellow];

    /// The word shown in Settings and stored in settings.json.
    pub fn name(self) -> &'static str {
        match self {
            Scheme::Pink => "Pink",
            Scheme::Green => "Green",
            Scheme::Cyan => "Cyan",
            Scheme::Yellow => "Yellow",
        }
    }

    /// The scheme a stored value names. Anything else, including the old
    /// "Light" and "Dark" (which never changed how anything looked), is Pink.
    pub fn parse(s: &str) -> Scheme {
        Scheme::ALL.into_iter()
            .find(|c| c.name().eq_ignore_ascii_case(s.trim()))
            .unwrap_or(Scheme::Pink)
    }

    /// This scheme's accent, for drawing its own swatch whatever is current.
    pub fn accent(self) -> u32 { self.colors().accent }

    fn colors(self) -> &'static Colors {
        match self {
            Scheme::Pink => &PINK_SET,
            Scheme::Green => &GREEN_SET,
            Scheme::Cyan => &CYAN_SET,
            Scheme::Yellow => &YELLOW_SET,
        }
    }
}

struct Colors {
    accent: u32,
    /// Lighter, for section headers: in the family, apart from what you press.
    accent_soft: u32,
    /// Hovered borders, the scroll thumb.
    accent_dim: u32,
    toggle_on: u32,
    bg_top: u32,
    bg_body: u32,
    rule: u32,
    field_bg: u32,
    field_border: u32,
    hover: u32,
    text_muted: u32,
    text_dim: u32,
    toggle_off: u32,
    toggle_off_knob: u32,
    scrollbar_bg: u32,
}

/// The logo: hot pink frame, white fangs, black.
const PINK_SET: Colors = Colors {
    accent: 0x00_FC_0C_7A,
    accent_soft: 0x00_FF_6F_AE,
    accent_dim: 0x00_7A_0B_42,
    toggle_on: 0x00_5C_0A_33,
    bg_top: 0x00_0E_0C_10,
    bg_body: 0x00_09_08_0B,
    rule: 0x00_24_1E_28,
    field_bg: 0x00_06_05_08,
    field_border: 0x00_2E_27_33,
    hover: 0x00_1C_15_20,
    text_muted: 0x00_A9_A1_AD,
    text_dim: 0x00_6B_63_71,
    toggle_off: 0x00_17_13_1A,
    toggle_off_knob: 0x00_57_4F_5C,
    scrollbar_bg: 0x00_14_10_17,
};

/// Phosphor green on green-black: deep and saturated, the same green as the
/// editor's highlights (Framefang started on a paler mint, `#00FF95`).
const GREEN_SET: Colors = Colors {
    accent: 0x00_00_FF_41,
    accent_soft: 0x00_8A_FF_A6,
    accent_dim: 0x00_0A_6B_26,
    toggle_on: 0x00_0A_4A_1E,
    bg_top: 0x00_0A_0F_0C,
    bg_body: 0x00_06_0A_08,
    rule: 0x00_1A_2A_22,
    field_bg: 0x00_04_07_05,
    field_border: 0x00_23_36_2C,
    hover: 0x00_0F_1C_16,
    text_muted: 0x00_A0_B4_AA,
    text_dim: 0x00_5E_72_68,
    toggle_off: 0x00_11_19_15,
    toggle_off_knob: 0x00_4C_5E_55,
    scrollbar_bg: 0x00_0C_14_10,
};

/// Ice cyan on blue-black.
const CYAN_SET: Colors = Colors {
    accent: 0x00_22_CE_FC,
    accent_soft: 0x00_8F_E6_FF,
    accent_dim: 0x00_14_6A_86,
    toggle_on: 0x00_0B_4A_60,
    bg_top: 0x00_0A_0E_1A,
    bg_body: 0x00_07_0A_13,
    rule: 0x00_16_22_3A,
    field_bg: 0x00_05_07_0F,
    field_border: 0x00_1C_2A_48,
    hover: 0x00_0E_17_30,
    text_muted: 0x00_8E_A2_C0,
    text_dim: 0x00_56_68_8A,
    toggle_off: 0x00_11_18_27,
    toggle_off_knob: 0x00_45_53_6E,
    scrollbar_bg: 0x00_0B_12_22,
};

/// Acid yellow on warm black.
const YELLOW_SET: Colors = Colors {
    accent: 0x00_F5_E5_0A,
    accent_soft: 0x00_FF_F2_7A,
    accent_dim: 0x00_6E_66_04,
    toggle_on: 0x00_4A_45_08,
    bg_top: 0x00_0F_0E_08,
    bg_body: 0x00_0A_09_06,
    rule: 0x00_2A_27_16,
    field_bg: 0x00_07_06_04,
    field_border: 0x00_36_32_1C,
    hover: 0x00_1C_1A_0E,
    text_muted: 0x00_B3_AE_92,
    text_dim: 0x00_6E_6A_55,
    toggle_off: 0x00_19_17_0D,
    toggle_off_knob: 0x00_5E_59_43,
    scrollbar_bg: 0x00_14_12_0A,
};

static CURRENT: AtomicU8 = AtomicU8::new(0);

/// Make `s` the scheme every dialog draws with from its next frame.
pub fn set(s: Scheme) {
    let i = Scheme::ALL.iter().position(|c| *c == s).unwrap_or(0);
    CURRENT.store(i as u8, Ordering::Relaxed);
}

/// The scheme in use.
pub fn current() -> Scheme {
    Scheme::ALL.get(CURRENT.load(Ordering::Relaxed) as usize).copied().unwrap_or(Scheme::Pink)
}

fn c() -> &'static Colors { current().colors() }

// ── colors that follow the scheme ───────────────────────────────────────────

/// The accent (named for the laser green it started as).
pub fn LASER() -> u32 { c().accent }
pub fn LASER_DIM() -> u32 { c().accent_dim }
pub fn FIELD_FOCUS() -> u32 { c().accent }
pub fn SECTION_TINT() -> u32 { c().accent_soft }
pub fn TOGGLE_ON() -> u32 { c().toggle_on }
pub fn TOGGLE_KNOB() -> u32 { c().accent }
pub fn SCROLLBAR_FG() -> u32 { c().accent_dim }
pub fn BG_TOP() -> u32 { c().bg_top }
pub fn BG_BODY() -> u32 { c().bg_body }
pub fn HEADER_RULE() -> u32 { c().rule }
pub fn PANEL_BORDER() -> u32 { c().rule }
pub fn PANEL_FILL() -> u32 { c().field_bg }
pub fn FIELD_BG() -> u32 { c().field_bg }
pub fn FIELD_BORDER() -> u32 { c().field_border }
pub fn HOVER_FILL() -> u32 { c().hover }
pub fn TEXT_MUTED() -> u32 { c().text_muted }
pub fn TEXT_DIM() -> u32 { c().text_dim }
pub fn TOGGLE_OFF() -> u32 { c().toggle_off }
pub fn TOGGLE_OFF_K() -> u32 { c().toggle_off_knob }
pub fn SCROLLBAR_BG() -> u32 { c().scrollbar_bg }
/// Behind a primary button on hover, or an armed one (a rebind listening):
/// a faint wash of the accent over the page.
pub fn PRIMARY_FILL() -> u32 {
    let (a, b) = (c().accent, c().bg_body);
    let mix = |s: u32| {
        let (fa, fb) = (((a >> s) & 0xFF) as f32, ((b >> s) & 0xFF) as f32);
        (fb + (fa - fb) * 0.16).round() as u32
    };
    (mix(16) << 16) | (mix(8) << 8) | mix(0)
}

// ── colors that never change ────────────────────────────────────────────────

pub const TEXT_BRIGHT: u32 = 0x00_F2_F2_F4;
/// Errors: orange-red, apart from every scheme's accent.
pub const DANGER: u32 = 0x00_FF_5C_3A;
pub const WARN: u32 = 0x00_FF_B0_20;
pub const OK_TINT: u32 = 0x00_3D_F5_C8;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stored_names_round_trip_and_old_values_fall_back_to_pink() {
        for s in Scheme::ALL {
            assert_eq!(Scheme::parse(s.name()), s);
            assert_eq!(Scheme::parse(&s.name().to_lowercase()), s);
        }
        assert_eq!(Scheme::parse("Light"), Scheme::Pink);
        assert_eq!(Scheme::parse("Dark"), Scheme::Pink);
        assert_eq!(Scheme::parse(""), Scheme::Pink);
    }

    #[test]
    fn errors_read_apart_from_every_accent() {
        // Hue distance, roughly: no accent may sit near DANGER's orange-red.
        let hue = |c: u32| {
            let (r, g, b) = (((c >> 16) & 0xFF) as f32, ((c >> 8) & 0xFF) as f32, (c & 0xFF) as f32);
            let (mx, mn) = (r.max(g).max(b), r.min(g).min(b));
            let d = (mx - mn).max(1.0);
            let h = if mx == r { ((g - b) / d).rem_euclid(6.0) } else if mx == g { (b - r) / d + 2.0 } else { (r - g) / d + 4.0 };
            h * 60.0
        };
        for s in Scheme::ALL {
            let diff = (hue(s.accent()) - hue(DANGER)).abs();
            let diff = diff.min(360.0 - diff);
            assert!(diff > 25.0, "{:?} accent is too close to the error color", s);
        }
    }
}
