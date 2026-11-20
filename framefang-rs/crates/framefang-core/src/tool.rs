//! The drawing tools on the overlay and editor toolbars.

use serde::{Deserialize, Serialize};

/// A drawing tool. The variant names are what settings.json stores, so they
/// never change; the order of [`Tool::ALL`] is the toolbar order.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Tool {
    #[default]
    Pen,
    Line,
    Arrow,
    Rectangle,
    Ellipse,
    Marker,
    /// Marks words found by reading the picture's text, snapping to them.
    Highlight,
    Text,
    Step,
    Pixelate,
    /// Darkens the picture outside a chosen rectangle.
    Spotlight,
    /// A round loupe that enlarges what sits under it.
    Magnify,
    /// Cuts the picture down (only offered in the editor window).
    Crop,
}

impl Tool {
    /// Every tool, left to right as the toolbar shows them.
    pub const ALL: [Tool; 13] = [
        Tool::Pen, Tool::Line, Tool::Arrow, Tool::Rectangle, Tool::Ellipse,
        Tool::Marker, Tool::Highlight, Tool::Text, Tool::Step, Tool::Pixelate,
        Tool::Spotlight, Tool::Magnify, Tool::Crop,
    ];

    /// The fixed facts about a tool, kept together so a new tool is one
    /// line here: (stored name, key letter, tooltip).
    fn facts(self) -> (&'static str, char, &'static str) {
        match self {
            Tool::Pen => ("Pen", 'p', "Pen (P)"),
            Tool::Line => ("Line", 'l', "Line (L)"),
            Tool::Arrow => ("Arrow", 'a', "Arrow (A)"),
            Tool::Rectangle => ("Rectangle", 'r', "Rectangle (R)"),
            Tool::Ellipse => ("Ellipse", 'e', "Ellipse (E)"),
            Tool::Marker => ("Marker", 'm', "Marker (M)"),
            Tool::Highlight => ("Highlight", 'h', "Highlight text (H)"),
            Tool::Text => ("Text", 't', "Text (T)"),
            Tool::Step => ("Step", 'n', "Numbered step (N)"),
            Tool::Pixelate => ("Pixelate", 'b', "Pixelate / blur (B)"),
            Tool::Spotlight => ("Spotlight", 'f', "Spotlight (F)"),
            Tool::Magnify => ("Magnify", 'z', "Magnify (Z)"),
            Tool::Crop => ("Crop", 'c', "Crop (C)"),
        }
    }

    /// The lowercase letter that picks this tool from the keyboard.
    pub fn shortcut(self) -> char { self.facts().1 }

    /// The tooltip text, ending with the key letter in brackets.
    pub fn label(self) -> &'static str { self.facts().2 }

    /// A name that never changes, used as the key for the tool's saved style.
    pub fn name(self) -> &'static str { self.facts().0 }

    /// Whether a capture may start with this tool selected. Crop reshapes
    /// the picture itself rather than drawing on it, so it never is.
    pub fn can_open_with(self) -> bool { self != Tool::Crop }

    /// The tool a key press picks, ignoring case.
    pub fn from_key(c: char) -> Option<Tool> {
        let wanted = c.to_ascii_lowercase();
        Self::ALL.into_iter().find(|tool| tool.shortcut() == wanted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn keys_and_names_are_unique_and_round_trip() {
        let mut seen_keys = HashSet::new();
        let mut seen_names = HashSet::new();
        for tool in Tool::ALL {
            assert!(seen_keys.insert(tool.shortcut()), "{tool:?} reuses a key");
            assert!(seen_names.insert(tool.name()), "{tool:?} reuses a name");
            assert_eq!(Tool::from_key(tool.shortcut()), Some(tool));
            // S belongs to Select, and the editor checks Ctrl+C and Ctrl+Z
            // before tool letters, so C and Z can still be tools.
            assert_ne!(tool.shortcut(), 's', "{tool:?} took Select's key");
        }
    }

    #[test]
    fn upper_case_keys_pick_the_same_tool_and_tooltips_show_the_key() {
        for tool in Tool::ALL {
            let upper = tool.shortcut().to_ascii_uppercase();
            assert_eq!(Tool::from_key(upper), Some(tool));
            assert!(tool.label().ends_with(&format!("({upper})")), "{}", tool.label());
        }
        assert_eq!(Tool::from_key('s'), None);
        assert_eq!(Tool::from_key('1'), None);
    }

    #[test]
    fn pen_is_the_default_and_crop_is_never_a_starting_tool() {
        assert_eq!(Tool::default(), Tool::Pen);
        assert!(!Tool::Crop.can_open_with());
        assert_eq!(Tool::ALL.iter().filter(|t| !t.can_open_with()).count(), 1);
    }
}
