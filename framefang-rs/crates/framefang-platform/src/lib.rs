//! The parts of Frame Fang that have to talk to the operating system.
//!
//! Grabbing the screen, owning a global hotkey, sitting in the tray, putting
//! things on the clipboard and driving a screen recorder all work differently
//! on Windows, macOS, X11 and Wayland. Each module here gives the app one
//! small, OS-neutral API for one of those jobs and keeps the per-OS crates and
//! system calls out of sight.

// Screens, windows and the pointer.
pub mod capture;
/// Wayland only: asks the desktop's screenshot portal for the screen when
/// `xcap` can't list the monitors itself.
#[cfg(target_os = "linux")]
pub mod capture_portal;
/// Draws the mouse pointer into a capture, when Settings wants it there.
pub mod cursor;
/// What is on screen when a capture starts, for the hover highlight.
pub mod windows_list;
pub mod scroll_parts;
/// Text recognition, through the OCR engine the OS already has.
pub mod ocr;

// Keys, tray and clipboard.
pub mod hotkey;
/// Wayland only: the GlobalShortcuts portal, which `hotkey::HotkeyManager`
/// switches to because a Wayland client can't grab keys.
#[cfg(target_os = "linux")]
pub mod hotkey_portal;
/// Windows only: a second route for hotkeys, through raw keyboard input,
/// for apps that eat a key before `RegisterHotKey` gets it.
#[cfg(target_os = "windows")]
mod raw_keys;
pub mod tray;
pub mod clipboard;

/// The running programs, to spot another tool that wants Print Screen.
pub mod processes;

// Recording and the process around it.
pub mod recorder;
pub mod child_guard;
pub mod instance;
pub mod session;

pub use capture::{capture_all_screens, Captured, MonitorFrame};
pub use child_guard::reap_orphaned_recorder;
pub use clipboard::{copy_image_png, copy_text};
pub use hotkey::{HotkeyBackend, HotkeyManager};
pub use instance::{Instance, InstanceLock};
pub use recorder::Recorder;
pub use session::is_wayland;
pub use tray::{MenuLabels, Tray, TrayEvent};

/// Every failure this crate reports. The text inside each variant is written
/// for the person using the app, so callers can show it as it is.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Reading the screen didn't work.
    #[error("couldn't capture the screen: {0}")]
    Capture(String),

    /// A global shortcut couldn't be set up, or the system refused it.
    #[error("shortcut problem: {0}")]
    Hotkey(String),

    /// The clipboard wouldn't take or give data.
    #[error("clipboard problem: {0}")]
    Clipboard(String),

    /// The tray icon or its menu couldn't be built or changed.
    #[error("tray icon problem: {0}")]
    Tray(String),

    /// Starting, watching or stopping a screen recording went wrong.
    #[error("recording problem: {0}")]
    Recording(String),

    /// A file or pipe operation failed.
    #[error("file or pipe failure: {0}")]
    Io(#[from] std::io::Error),
}

/// This crate's result type.
pub type Result<T> = std::result::Result<T, Error>;
