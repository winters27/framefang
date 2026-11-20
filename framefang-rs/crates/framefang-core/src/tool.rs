//! Tools the user can pick on the overlay toolbar.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Tool {
    Pen,
    Line,
    Arrow,
    Rectangle,
    Ellipse,
    Marker,
    /// Highlight words the picture's text reading found, snapped to them.
    Highlight,
    Text,
    Step,
    Pixelate,
    /// Dim everything outside a rectangle.
    Spotlight,
    /// A round lens showing what is under it, larger.
    Magnify,
    /// Trim the picture (the editor window only).
    Crop,
}

impl Default for Tool {
    fn default() -> Self {
        Tool::Pen
    }
}

impl Tool {
    /// Single-letter keyboard shortcut.
    pub fn shortcut(self) -> char {
        match self {
            Tool::Pen       => 'p',
            Tool::Line      => 'l',
            Tool::Arrow     => 'a',
            Tool::Rectangle => 'r',
            Tool::Ellipse   => 'e',
            Tool::Marker    => 'm',
            Tool::Highlight => 'h',
            Tool::Text      => 't',
            Tool::Step      => 'n',
            Tool::Pixelate  => 'b',
            Tool::Spotlight => 'f',
            Tool::Magnify   => 'z',
            Tool::Crop      => 'c',
        }
    }

    /// Tooltip + accelerator text, for UI rendering.
    pub fn label(self) -> &'static str {
        match self {
            Tool::Pen       => "Pen (P)",
            Tool::Line      => "Line (L)",
            Tool::Arrow     => "Arrow (A)",
            Tool::Rectangle => "Rectangle (R)",
            Tool::Ellipse   => "Ellipse (E)",
            Tool::Marker    => "Marker (M)",
            Tool::Highlight => "Highlight text (H)",
            Tool::Text      => "Text (T)",
            Tool::Step      => "Numbered step (N)",
            Tool::Pixelate  => "Pixelate / blur (B)",
            Tool::Spotlight => "Spotlight (F)",
            Tool::Magnify   => "Magnify (Z)",
            Tool::Crop      => "Crop (C)",
        }
    }

    /// Stable name, the key a tool's saved style is stored under.
    pub fn name(self) -> &'static str {
        match self {
            Tool::Pen       => "Pen",
            Tool::Line      => "Line",
            Tool::Arrow     => "Arrow",
            Tool::Rectangle => "Rectangle",
            Tool::Ellipse   => "Ellipse",
            Tool::Marker    => "Marker",
            Tool::Highlight => "Highlight",
            Tool::Text      => "Text",
            Tool::Step      => "Step",
            Tool::Pixelate  => "Pixelate",
            Tool::Spotlight => "Spotlight",
            Tool::Magnify   => "Magnify",
            Tool::Crop      => "Crop",
        }
    }

    /// All tools in toolbar order.
    pub const ALL: [Tool; 13] = [
        Tool::Pen,
        Tool::Line,
        Tool::Arrow,
        Tool::Rectangle,
        Tool::Ellipse,
        Tool::Marker,
        Tool::Highlight,
        Tool::Text,
        Tool::Step,
        Tool::Pixelate,
        Tool::Spotlight,
        Tool::Magnify,
        Tool::Crop,
    ];

    /// Whether a session may open with this tool in hand. Crop changes the
    /// picture, not the marks on it, so it is never the tool a session
    /// starts with.
    pub fn can_open_with(self) -> bool { self != Tool::Crop }

    pub fn from_key(c: char) -> Option<Tool> {
        let c = c.to_ascii_lowercase();
        Tool::ALL.iter().copied().find(|t| t.shortcut() == c)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_tool_has_its_own_key_and_name() {
        let mut keys = std::collections::HashSet::new();
        let mut names = std::collections::HashSet::new();
        for t in Tool::ALL {
            assert!(keys.insert(t.shortcut()), "{t:?} shares its key");
            assert!(names.insert(t.name()), "{t:?} shares its name");
            assert_eq!(Tool::from_key(t.shortcut()), Some(t));
            // S is Select; Ctrl+C, Ctrl+Z stay copy and undo (the editor reads
            // those with Ctrl before tool keys).
            assert_ne!(t.shortcut(), 's');
        }
    }
}
