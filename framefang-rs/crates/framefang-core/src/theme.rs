//! Light and dark palettes for Frame Fang's own windows (Settings, About and
//! the other dialogs). The capture overlay has its own look and does not read
//! these.

use serde::{Deserialize, Serialize};

use crate::color::Rgba;

/// Which palette the dialogs use. Settings stores this as text, see
/// [`ThemeName::parse`] and [`ThemeName::as_str`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ThemeName {
    #[default]
    Light,
    Dark,
}

impl ThemeName {
    /// Read a stored theme name. Only "dark" (any letter case) selects the
    /// dark palette; anything else, including an empty or garbled value,
    /// falls back to light.
    pub fn parse(s: &str) -> ThemeName {
        match s.eq_ignore_ascii_case("dark") {
            true => ThemeName::Dark,
            false => ThemeName::Light,
        }
    }

    /// The spelling written back to settings.
    pub fn as_str(self) -> &'static str {
        if self == ThemeName::Dark { "Dark" } else { "Light" }
    }
}

/// Every color a dialog paints with, by role.
#[derive(Debug, Clone, Copy)]
pub struct ThemeColors {
    pub background: Rgba,
    pub surface: Rgba,
    pub surface_alt: Rgba,
    pub border: Rgba,
    pub text: Rgba,
    pub text_muted: Rgba,
    pub accent: Rgba,
    pub button_bg: Rgba,
    pub button_hover: Rgba,
}

/// Shorthand for the opaque entries below.
const fn rgb(r: u8, g: u8, b: u8) -> Rgba {
    Rgba::new_opaque(r, g, b)
}

impl ThemeColors {
    pub const LIGHT: ThemeColors = ThemeColors {
        background: rgb(245, 245, 247),
        surface: rgb(255, 255, 255),
        surface_alt: rgb(235, 235, 240),
        border: rgb(210, 210, 215),
        text: rgb(30, 30, 30),
        text_muted: rgb(110, 110, 120),
        accent: rgb(88, 86, 214),
        button_bg: rgb(228, 228, 232),
        button_hover: rgb(215, 215, 220),
    };

    pub const DARK: ThemeColors = ThemeColors {
        background: rgb(32, 32, 36),
        surface: rgb(45, 45, 50),
        surface_alt: rgb(60, 60, 66),
        border: rgb(70, 70, 76),
        text: rgb(235, 235, 238),
        text_muted: rgb(160, 160, 168),
        accent: rgb(120, 118, 240),
        button_bg: rgb(70, 70, 76),
        button_hover: rgb(90, 90, 96),
    };

    /// The palette for a theme.
    pub const fn for_theme(name: ThemeName) -> ThemeColors {
        if let ThemeName::Dark = name { Self::DARK } else { Self::LIGHT }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_dark_in_any_case_picks_the_dark_theme() {
        for s in ["dark", "Dark", "DARK", "dArK"] {
            assert_eq!(ThemeName::parse(s), ThemeName::Dark, "{s}");
        }
        for s in ["", "light", "Light", "darker", " dark", "midnight"] {
            assert_eq!(ThemeName::parse(s), ThemeName::Light, "{s}");
        }
    }

    #[test]
    fn stored_names_parse_back_to_themselves() {
        for t in [ThemeName::Light, ThemeName::Dark] {
            assert_eq!(ThemeName::parse(t.as_str()), t);
        }
        assert_eq!(ThemeName::default(), ThemeName::Light);
    }

    #[test]
    fn each_theme_gets_its_own_palette() {
        assert_eq!(ThemeColors::for_theme(ThemeName::Light).background, ThemeColors::LIGHT.background);
        assert_eq!(ThemeColors::for_theme(ThemeName::Dark).background, ThemeColors::DARK.background);
        // Text must contrast with the page in both.
        for p in [ThemeColors::LIGHT, ThemeColors::DARK] {
            assert_ne!(p.text, p.background);
            assert_eq!(p.accent.a, 255);
        }
    }
}
