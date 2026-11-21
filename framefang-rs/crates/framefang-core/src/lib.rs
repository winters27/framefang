//! The part of Frame Fang that needs no window system.
//!
//! Everything here is plain data and logic: the drawing tools and the marks
//! they make, editing and undo, settings and where they live on disk,
//! geometry for monitors and capture regions, colours and themes, and the
//! wording of user-facing messages. Keeping it free of any OS or GUI crate
//! means it builds and tests the same way on every platform.

// Marks on a picture, and editing them.
pub mod annotation;
pub mod edit;
pub mod history;
pub mod mark;
pub mod step;
pub mod text;
pub mod tool;

// Settings, saved state and files on disk.
pub mod app_state;
pub mod atomic_file;
pub mod file_name;
pub mod settings;
pub mod settings_store;

// Keyboard shortcuts, and other tools that want the same key.
pub mod hotkeys;
pub mod rivals;
pub mod shortcut;

// Screens, regions and the capture state machine.
pub mod dpi;
pub mod region;
pub mod state;
pub mod virtual_desktop;

// Look, install channel and messages.
pub mod color;
pub mod failure;
pub mod install_channel;
pub mod theme;

pub use crate::{
    annotation::{Annotation, AnnotationKind, ColorPalette, Palettes},
    color::Rgba,
    dpi::{DisplayMap, GrabRegion, LogicalRect, MonitorGeometry, MonitorPlacement, PhysicalRect},
    history::{EditOp, History},
    hotkeys::HotkeyAction,
    install_channel::{detect_action, HostOs, InstallChannel, InstallProbe, UpdateAction},
    region::{CaptureRect, DesktopBounds},
    settings::{AppSettings, Hotkey, Modifiers},
    shortcut::portal_trigger,
    state::{Edge, State},
    text::{PlacedGlyph, TextBlock, TextRect},
    theme::{ThemeColors, ThemeName},
    tool::Tool,
    virtual_desktop::{DesktopGeometry, MonitorRect},
};
