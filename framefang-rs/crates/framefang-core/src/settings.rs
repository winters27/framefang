//! Persistent app settings. Stored as JSON at the platform's config dir,
//! e.g. `%APPDATA%/Framefang/settings.json` on Windows, `~/.config/Framefang/settings.json`
//! on Linux, `~/Library/Application Support/Framefang/settings.json` on macOS.

use std::fs;
use std::io;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::theme::ThemeName;

bitflags::bitflags! {
    /// Hotkey modifier mask. Numeric values match Win32 `MOD_*` so the same
    /// settings.json works on the C# Windows build and the Rust build.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Modifiers: u32 {
        const ALT     = 0x0001;
        const CONTROL = 0x0002;
        const SHIFT   = 0x0004;
        const SUPER   = 0x0008;
    }
}

impl Serialize for Modifiers {
    fn serialize<S: serde::Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        ser.serialize_u32(self.bits())
    }
}

impl<'de> Deserialize<'de> for Modifiers {
    fn deserialize<D: serde::Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        let bits = u32::deserialize(de)?;
        Ok(Modifiers::from_bits_truncate(bits))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hotkey {
    pub modifiers:   Modifiers,
    /// Win32 virtual-key code. `0x2C` (`VK_SNAPSHOT` / Print Screen) is the
    /// default on Windows and Linux; macOS defaults to `0x37` (`7`) instead,
    /// because Apple keyboards have no Print Screen key at all.
    pub virtual_key: u32,
}

impl Default for Hotkey {
    fn default() -> Self {
        Hotkey {
            modifiers: default_hotkey_modifiers_flags(),
            virtual_key: default_vk(),
        }
    }
}

impl Hotkey {
    /// Human-readable rendering, e.g. `"Ctrl + Shift + P"` or `"Print Screen"`.
    /// Unknown VKs render as `"(0xNN)"` so the user can at least see they have
    /// an unsupported key bound.
    pub fn describe(&self) -> String {
        let key = vk_label(self.virtual_key)
            .map(str::to_owned)
            .unwrap_or_else(|| format!("(0x{:02X})", self.virtual_key));
        let mods = self.modifiers.describe();
        if mods.is_empty() { key } else { format!("{} + {}", mods, key) }
    }
}

impl Modifiers {
    /// Render the active modifier set as `"Ctrl + Shift + Alt + Win"` (in the
    /// order Windows users expect). Returns the empty string when no modifiers
    /// are set.
    ///
    /// macOS uses that platform's names and ordering instead --
    /// `"Cmd + Ctrl + Opt + Shift"`. Names are spelled out rather than drawn
    /// with the Apple modifier glyphs because every string here goes through a
    /// 5x7 ASCII bitmap font.
    #[cfg(not(target_os = "macos"))]
    pub fn describe(&self) -> String {
        let mut parts: Vec<&str> = Vec::new();
        if self.contains(Modifiers::CONTROL) { parts.push("Ctrl"); }
        if self.contains(Modifiers::SHIFT)   { parts.push("Shift"); }
        if self.contains(Modifiers::ALT)     { parts.push("Alt"); }
        if self.contains(Modifiers::SUPER)   { parts.push("Win"); }
        parts.join(" + ")
    }

    #[cfg(target_os = "macos")]
    pub fn describe(&self) -> String {
        let mut parts: Vec<&str> = Vec::new();
        if self.contains(Modifiers::SUPER)   { parts.push("Cmd"); }
        if self.contains(Modifiers::CONTROL) { parts.push("Ctrl"); }
        if self.contains(Modifiers::ALT)     { parts.push("Opt"); }
        if self.contains(Modifiers::SHIFT)   { parts.push("Shift"); }
        parts.join(" + ")
    }
}

/// One key a hotkey can use: its Win32 virtual-key code (the settings.json
/// wire format), the name Settings shows, and whether it may be bound alone.
#[derive(Debug, Clone, Copy)]
pub struct KeyInfo {
    pub vk: u32,
    pub label: &'static str,
    /// A key that is not for typing or moving around (Print Screen, F-keys,
    /// Pause, media keys) works on its own. A typing key (letters, digits,
    /// punctuation, Space, arrows, the numpad) needs Ctrl, Alt, Shift or Win
    /// with it: bound alone, it would stop typing that character everywhere.
    pub alone: bool,
}

const fn k(vk: u32, label: &'static str, alone: bool) -> KeyInfo { KeyInfo { vk, label, alone } }

/// Every key Settings can bind, in one place: the rebind widget, the labels,
/// the "needs a modifier" guard and the platform registration all read it.
/// Labels are ASCII (the 5x7 bitmap font has nothing else).
pub const KEYS: &[KeyInfo] = &[
    k(0x08, "Backspace", false), k(0x09, "Tab", false), k(0x0D, "Enter", false),
    k(0x13, "Pause", true), k(0x14, "Caps Lock", false), k(0x1B, "Esc", false),
    k(0x20, "Space", false),
    k(0x21, "Page Up", false), k(0x22, "Page Down", false), k(0x23, "End", false), k(0x24, "Home", false),
    k(0x25, "Left", false), k(0x26, "Up", false), k(0x27, "Right", false), k(0x28, "Down", false),
    k(0x2C, "Print Screen", true), k(0x2D, "Insert", true), k(0x2E, "Delete", false),
    k(0x30, "0", false), k(0x31, "1", false), k(0x32, "2", false), k(0x33, "3", false), k(0x34, "4", false),
    k(0x35, "5", false), k(0x36, "6", false), k(0x37, "7", false), k(0x38, "8", false), k(0x39, "9", false),
    k(0x41, "A", false), k(0x42, "B", false), k(0x43, "C", false), k(0x44, "D", false), k(0x45, "E", false),
    k(0x46, "F", false), k(0x47, "G", false), k(0x48, "H", false), k(0x49, "I", false), k(0x4A, "J", false),
    k(0x4B, "K", false), k(0x4C, "L", false), k(0x4D, "M", false), k(0x4E, "N", false), k(0x4F, "O", false),
    k(0x50, "P", false), k(0x51, "Q", false), k(0x52, "R", false), k(0x53, "S", false), k(0x54, "T", false),
    k(0x55, "U", false), k(0x56, "V", false), k(0x57, "W", false), k(0x58, "X", false), k(0x59, "Y", false),
    k(0x5A, "Z", false),
    k(0x5D, "Menu", true), k(0x5F, "Sleep", true),
    k(0x60, "Num 0", false), k(0x61, "Num 1", false), k(0x62, "Num 2", false), k(0x63, "Num 3", false),
    k(0x64, "Num 4", false), k(0x65, "Num 5", false), k(0x66, "Num 6", false), k(0x67, "Num 7", false),
    k(0x68, "Num 8", false), k(0x69, "Num 9", false),
    k(0x6A, "Num *", false), k(0x6B, "Num +", false), k(0x6D, "Num -", false), k(0x6E, "Num .", false),
    k(0x6F, "Num /", false),
    k(0x70, "F1", true), k(0x71, "F2", true), k(0x72, "F3", true), k(0x73, "F4", true),
    k(0x74, "F5", true), k(0x75, "F6", true), k(0x76, "F7", true), k(0x77, "F8", true),
    k(0x78, "F9", true), k(0x79, "F10", true), k(0x7A, "F11", true), k(0x7B, "F12", true),
    k(0x7C, "F13", true), k(0x7D, "F14", true), k(0x7E, "F15", true), k(0x7F, "F16", true),
    k(0x80, "F17", true), k(0x81, "F18", true), k(0x82, "F19", true), k(0x83, "F20", true),
    k(0x84, "F21", true), k(0x85, "F22", true), k(0x86, "F23", true), k(0x87, "F24", true),
    k(0x90, "Num Lock", false), k(0x91, "Scroll Lock", true),
    k(0xA6, "Browser Back", true), k(0xA7, "Browser Forward", true), k(0xA8, "Browser Refresh", true),
    k(0xA9, "Browser Stop", true), k(0xAA, "Browser Search", true), k(0xAB, "Favorites", true),
    k(0xAC, "Browser Home", true),
    k(0xAD, "Mute", true), k(0xAE, "Volume Down", true), k(0xAF, "Volume Up", true),
    k(0xB0, "Next Track", true), k(0xB1, "Previous Track", true), k(0xB2, "Stop", true),
    k(0xB3, "Play/Pause", true),
    k(0xB4, "Mail", true), k(0xB5, "Media", true), k(0xB6, "App 1", true), k(0xB7, "App 2", true),
    k(0xBA, ";", false), k(0xBB, "=", false), k(0xBC, ",", false), k(0xBD, "-", false),
    k(0xBE, ".", false), k(0xBF, "/", false), k(0xC0, "`", false),
    k(0xDB, "[", false), k(0xDC, "\\", false), k(0xDD, "]", false), k(0xDE, "'", false),
    k(0xE2, "<>", false),
];

/// The entry for `vk`, if Settings knows the key.
pub fn key_info(vk: u32) -> Option<&'static KeyInfo> {
    KEYS.iter().find(|i| i.vk == vk)
}

/// Map a Win32 virtual-key code to a short, user-facing label, or `None` for
/// a key not in [`KEYS`] (callers show "(0xNN)").
pub fn vk_label(vk: u32) -> Option<&'static str> {
    key_info(vk).map(|i| i.label)
}

/// Whether `vk` with no modifier makes a sensible hotkey. Keys we don't know
/// are treated as special keys and allowed.
pub fn bindable_alone(vk: u32) -> bool {
    key_info(vk).map_or(true, |i| i.alone)
}

/// The modifiers that keep a typing key from typing. Shift doesn't count
/// (Shift+S types a capital S), and on macOS neither does Option (Option+S
/// types a symbol).
pub fn typing_safe_modifiers() -> Modifiers {
    if cfg!(target_os = "macos") {
        Modifiers::SUPER | Modifiers::CONTROL
    } else {
        Modifiers::CONTROL | Modifiers::ALT | Modifiers::SUPER
    }
}

/// Whether `hk` would take a key away from typing: a typing key (see
/// [`KeyInfo::alone`]) with none of [`typing_safe_modifiers`].
pub fn blocks_typing(hk: &Hotkey) -> bool {
    !bindable_alone(hk.virtual_key) && !hk.modifiers.intersects(typing_safe_modifiers())
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppSettings {
    #[serde(rename = "LastTool", default = "default_tool")]
    pub last_tool: String,

    #[serde(rename = "LastColorArgb", default = "default_color_argb")]
    pub last_color_argb: i32,

    #[serde(rename = "LastThickness", default = "default_thickness")]
    pub last_thickness: f32,

    /// Text tool em-size in px, set from the Text tool's flyout.
    #[serde(rename = "LastTextSize", default = "default_text_size")]
    pub last_text_size: f32,

    /// Pixelate block size in px, set from the Pixelate tool's flyout.
    #[serde(rename = "PixelateBlock", default = "default_pixelate_block")]
    pub pixelate_block: u32,

    /// Every screenshot also goes to the clipboard: the region as soon as it
    /// is picked (and again, finished, when it is saved or pinned), and a
    /// full-screen capture as it is saved. Off by default.
    #[serde(rename = "AutoCopy", default)]
    pub auto_copy: bool,

    /// The editor's Copy (button or Ctrl+C) also closes the editor. Off by
    /// default: Copy puts the picture on the clipboard and the editor stays
    /// open for more marks.
    #[serde(rename = "CloseAfterCopy", default)]
    pub close_after_copy: bool,

    /// What a region capture does when the drag is let go: open the editor
    /// (the default), or put the picture on the clipboard and close.
    #[serde(rename = "OpenEditor", default = "default_true")]
    pub open_editor: bool,

    /// What a scrolling capture does when it is finished, apart from a
    /// region capture's: `"Edit"`, `"CopyAndEdit"` (the default) or
    /// `"CopyOnly"` (see [`AfterCapture`]). A long page is worth a look
    /// even when quick captures only copy (Brandon 2026-11-20: "how do we
    /// even edit these pics? no editor comes up").
    #[serde(rename = "ScrollAfterCapture", default = "default_scroll_after_capture")]
    pub scroll_after_capture: String,

    /// The key that, held with the capture key or as the drag is let go,
    /// makes that one capture do the other thing: `"Shift"`, `"Ctrl"` or
    /// `"Alt"` (see [`SwitchKey`]).
    #[serde(rename = "SwitchKey", default = "default_switch_key")]
    pub switch_key: String,

    /// Screenshots show the mouse pointer where it was when they were
    /// taken. Off by default. Windows only for now.
    #[serde(rename = "CaptureCursor", default)]
    pub capture_cursor: bool,

    /// What a click in the capture screen takes, picked in its mode bar and
    /// kept for the next capture: `"Auto"` (drag a region, or click the
    /// window under the cursor), `"Screen"` or `"AllScreens"`.
    #[serde(rename = "CaptureMode", default = "default_capture_mode")]
    pub capture_mode: String,

    /// Each tool's own color and stroke width, keyed by `Tool::name`.
    /// A tool with no entry uses `ToolStyle::default_for`.
    #[serde(rename = "ToolStyles", default)]
    pub tool_styles: std::collections::BTreeMap<String, ToolStyle>,

    #[serde(rename = "SaveDirectory", default)]
    pub save_directory: String,

    #[serde(rename = "RecordingsDirectory", default)]
    pub recordings_directory: String,

    /// The FFmpeg the user picked in Settings. Empty: found by itself (next
    /// to the app, PATH, then the downloaded copy).
    #[serde(rename = "FfmpegPath", default)]
    pub ffmpeg_path: String,

    #[serde(rename = "HotkeyModifiers", default = "default_hotkey_modifiers")]
    pub hotkey_modifiers: u32,

    #[serde(rename = "HotkeyVirtualKey", default = "default_vk")]
    pub hotkey_virtual_key: u32,

    /// Optional second binding: capture the whole desktop straight to disk.
    /// Same Win32 wire encoding as `HotkeyModifiers` / `HotkeyVirtualKey`;
    /// a virtual key of `0` (the default, and what a file written before
    /// these keys existed decodes to) means "not bound". See
    /// `crate::hotkeys` for the accessors.
    #[serde(rename = "FullScreenHotkeyModifiers", default)]
    pub fullscreen_hotkey_modifiers: u32,

    #[serde(rename = "FullScreenHotkeyVirtualKey", default)]
    pub fullscreen_hotkey_virtual_key: u32,

    /// Optional third binding: start the screen recording, or stop the one
    /// that's running. Encoding and "not bound" rule as above.
    #[serde(rename = "RecordHotkeyModifiers", default)]
    pub record_hotkey_modifiers: u32,

    #[serde(rename = "RecordHotkeyVirtualKey", default)]
    pub record_hotkey_virtual_key: u32,

    #[serde(rename = "StartWithWindows", default)]
    pub start_with_windows: bool,

    #[serde(rename = "PaletteIndex", default)]
    pub palette_index: i32,

    #[serde(rename = "Theme", default = "default_theme")]
    pub theme: String,

    /// Alpha (0..=255) applied to the Marker (highlighter) stroke. Default
    /// `0xC8` (200, ≈78 %) preserves the historical look; the editor's
    /// per-tool slider mutates this and persists it on mouseup.
    #[serde(rename = "MarkerOpacity", default = "default_marker_opacity")]
    pub marker_opacity: u8,

    /// The sound a recording started from the tray menu takes: "None",
    /// "Mic", "PC" (what the computer plays) or "Both". Chosen with the
    /// menu's Sound chips.
    #[serde(rename = "RecordSound", default = "default_record_sound")]
    pub record_sound: String,

    /// How saved screenshots and recordings are named: tokens like
    /// `{yyyy}`, `{date}`, `{app}` (see `crate::file_name`). The default
    /// gives the name every version before this one used.
    #[serde(rename = "FileNamePattern", default = "default_file_name_pattern")]
    pub file_name_pattern: String,

    /// The last pattern that was not one of Settings' presets, kept so
    /// picking a preset and then CUSTOM brings it back.
    #[serde(rename = "FileNameCustom", default)]
    pub file_name_custom: String,

    /// What a recording ends as: `"MP4"` or `"GIF"` (see [`RecordFormat`]).
    #[serde(rename = "RecordFormat", default = "default_record_format")]
    pub record_format: String,

    /// A frame added to every saved or copied screenshot: `"None"`,
    /// `"Border"` or `"Shadow"` (see [`OutputFrame`]).
    #[serde(rename = "OutputFrame", default = "default_output_frame")]
    pub output_frame: String,

    /// Optional binding: take the last captured rectangle again. Encoding and
    /// "not bound" rule as the other optional hotkeys.
    #[serde(rename = "LastRegionHotkeyModifiers", default)]
    pub last_region_hotkey_modifiers: u32,

    #[serde(rename = "LastRegionHotkeyVirtualKey", default)]
    pub last_region_hotkey_virtual_key: u32,

    /// Optional binding: open the picker in TEXT mode, to copy the words in
    /// a region.
    #[serde(rename = "TextHotkeyModifiers", default)]
    pub text_hotkey_modifiers: u32,

    #[serde(rename = "TextHotkeyVirtualKey", default)]
    pub text_hotkey_virtual_key: u32,

    /// Optional binding: open the picker in QR mode, to read a QR code.
    #[serde(rename = "QrHotkeyModifiers", default)]
    pub qr_hotkey_modifiers: u32,

    #[serde(rename = "QrHotkeyVirtualKey", default)]
    pub qr_hotkey_virtual_key: u32,

    /// Optional binding: open the picker in SCROLL mode.
    #[serde(rename = "ScrollHotkeyModifiers", default)]
    pub scroll_hotkey_modifiers: u32,

    #[serde(rename = "ScrollHotkeyVirtualKey", default)]
    pub scroll_hotkey_virtual_key: u32,

    /// How much the Magnify lens enlarges: 2, 3 or 4, set from its flyout.
    #[serde(rename = "MagnifyZoom", default = "default_magnify_zoom")]
    pub magnify_zoom: u32,

    /// A QR code that holds a web link opens it in the browser instead of
    /// copying it. Off: every QR code is copied.
    #[serde(rename = "OpenQrLinks", default)]
    pub open_qr_links: bool,

    /// Held as a QR read is let go, a link does the other thing (opens
    /// instead of copying, or the other way round): `"Shift"`, `"Ctrl"` or
    /// `"Alt"`. Its own key, set apart from the capture switch key.
    #[serde(rename = "QrSwitchKey", default = "default_qr_switch_key")]
    pub qr_switch_key: String,

    /// Held as the part to scroll is clicked, a scrolling capture does the
    /// other thing (the editor, or just copying; see `ScrollAfterCapture`).
    /// Its own key, Shift unless set.
    #[serde(rename = "ScrollSwitchKey", default = "default_scroll_switch_key")]
    pub scroll_switch_key: String,

    /// How numbered steps look: shape, each part's color, shadow (see
    /// [`crate::step`]). `None` until one is picked: a plain disc in the
    /// Step tool's own color.
    #[serde(rename = "StepLook", default)]
    pub step_look: Option<crate::step::StepLook>,

    /// The Highlight tool's look (its color is the tool's own).
    #[serde(rename = "HighlightStyle", default)]
    pub highlight_style: crate::mark::MarkStyle,

    /// Keep the last 10 captures (copy-only included) for the tray's RECENT
    /// list. Turning it off deletes them.
    #[serde(rename = "KeepRecentCaptures", default = "default_true")]
    pub keep_recent: bool,
}

fn default_magnify_zoom() -> u32 { 2 }

fn default_record_sound() -> String { "None".to_owned() }
pub fn default_file_name_pattern() -> String { crate::file_name::DEFAULT_PATTERN.to_owned() }
fn default_record_format() -> String { RecordFormat::Mp4.name().to_owned() }
fn default_output_frame() -> String { OutputFrame::None.name().to_owned() }

/// What a recording ends as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordFormat { Mp4, Gif }

impl RecordFormat {
    pub const ALL: [RecordFormat; 2] = [RecordFormat::Mp4, RecordFormat::Gif];

    /// The stored word. Anything unknown reads as MP4.
    pub fn parse(s: &str) -> RecordFormat {
        RecordFormat::ALL.into_iter().find(|f| f.name().eq_ignore_ascii_case(s)).unwrap_or(RecordFormat::Mp4)
    }

    pub fn name(self) -> &'static str {
        match self { RecordFormat::Mp4 => "MP4", RecordFormat::Gif => "GIF" }
    }

    /// The file extension, without the dot.
    pub fn extension(self) -> &'static str {
        match self { RecordFormat::Mp4 => "mp4", RecordFormat::Gif => "gif" }
    }
}

/// A frame around every saved or copied screenshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFrame { None, Border, Shadow }

impl OutputFrame {
    pub const ALL: [OutputFrame; 3] = [OutputFrame::None, OutputFrame::Border, OutputFrame::Shadow];

    /// The stored word. Anything unknown reads as None.
    pub fn parse(s: &str) -> OutputFrame {
        OutputFrame::ALL.into_iter().find(|f| f.name().eq_ignore_ascii_case(s)).unwrap_or(OutputFrame::None)
    }

    pub fn name(self) -> &'static str {
        match self { OutputFrame::None => "None", OutputFrame::Border => "Border", OutputFrame::Shadow => "Shadow" }
    }
}

impl Default for AppSettings {
    fn default() -> Self {
        AppSettings {
            last_tool:           default_tool(),
            last_color_argb:     default_color_argb(),
            last_thickness:      default_thickness(),
            last_text_size:      default_text_size(),
            pixelate_block:      default_pixelate_block(),
            auto_copy:           false,
            close_after_copy:    false,
            open_editor:         true,
            scroll_after_capture: default_scroll_after_capture(),
            switch_key:          default_switch_key(),
            capture_cursor:      false,
            capture_mode:        default_capture_mode(),
            tool_styles:         Default::default(),
            save_directory:      String::new(),
            recordings_directory: String::new(),
            ffmpeg_path:         String::new(),
            hotkey_modifiers:    default_hotkey_modifiers(),
            hotkey_virtual_key:  default_vk(),
            fullscreen_hotkey_modifiers:   0,
            fullscreen_hotkey_virtual_key: 0,
            record_hotkey_modifiers:       0,
            record_hotkey_virtual_key:     0,
            start_with_windows:  false,
            palette_index:       0,
            theme:               default_theme(),
            marker_opacity:      default_marker_opacity(),
            record_sound:        default_record_sound(),
            file_name_pattern:   default_file_name_pattern(),
            file_name_custom:    String::new(),
            record_format:       default_record_format(),
            output_frame:        default_output_frame(),
            last_region_hotkey_modifiers:   0,
            last_region_hotkey_virtual_key: 0,
            text_hotkey_modifiers:          0,
            text_hotkey_virtual_key:        0,
            qr_hotkey_modifiers:            0,
            qr_hotkey_virtual_key:          0,
            scroll_hotkey_modifiers:        0,
            scroll_hotkey_virtual_key:      0,
            magnify_zoom:        default_magnify_zoom(),
            open_qr_links:       false,
            qr_switch_key:       default_qr_switch_key(),
            scroll_switch_key:   default_scroll_switch_key(),
            step_look:           None,
            highlight_style:     Default::default(),
            keep_recent:         true,
        }
    }
}

fn default_true()        -> bool    { true }
fn default_capture_mode() -> String { "Auto".to_owned() }
fn default_switch_key()  -> String  { SwitchKey::Shift.name() }

/// A key held as a capture is let go to do the other thing: the capture
/// switch key (editor or just copy) and QR's key (open a link or copy it).
/// A modifier, or any other key the user binds (Brandon 2026-11-19: "ppl
/// should be able to bind their own"), stored by its Win32 virtual-key code
/// like the hotkeys.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwitchKey { Shift, Ctrl, Alt, Key(u32) }

impl SwitchKey {
    /// The modifiers, the usual picks.
    pub const ALL: [SwitchKey; 3] = [SwitchKey::Shift, SwitchKey::Ctrl, SwitchKey::Alt];

    /// The stored word. Anything unknown reads as Shift.
    pub fn parse(s: &str) -> SwitchKey { SwitchKey::parse_or(s, SwitchKey::Shift) }

    /// The stored word: `Shift`, `Ctrl`, `Alt` or `Key:0x51`; anything
    /// unknown, or a key that can't be held for this, reads as `fallback`.
    pub fn parse_or(s: &str, fallback: SwitchKey) -> SwitchKey {
        if let Some(k) = SwitchKey::ALL.into_iter().find(|k| k.name().eq_ignore_ascii_case(s)) { return k; }
        let hex = s.trim().strip_prefix("Key:").and_then(|h| h.strip_prefix("0x").or(h.strip_prefix("0X")));
        match hex.and_then(|h| u32::from_str_radix(h, 16).ok()) {
            Some(vk) => SwitchKey::from_vk(vk).unwrap_or(fallback),
            None => fallback,
        }
    }

    /// The key a press of `vk` binds: a modifier (either side) as itself,
    /// another key as itself. `None` for a key the capture screen or the
    /// system already gives a meaning (Tab, Enter, Esc, the Windows keys).
    pub fn from_vk(vk: u32) -> Option<SwitchKey> {
        match vk {
            0x10 | 0xA0 | 0xA1 => Some(SwitchKey::Shift),
            0x11 | 0xA2 | 0xA3 => Some(SwitchKey::Ctrl),
            0x12 | 0xA4 | 0xA5 => Some(SwitchKey::Alt),
            0 | 0x01..=0x07 | 0x09 | 0x0D | 0x1B | 0x5B | 0x5C => None,
            v if v > 0xFE => None,
            v => Some(SwitchKey::Key(v)),
        }
    }

    pub fn name(self) -> String {
        match self {
            SwitchKey::Shift => "Shift".to_owned(),
            SwitchKey::Ctrl => "Ctrl".to_owned(),
            SwitchKey::Alt => "Alt".to_owned(),
            SwitchKey::Key(vk) => format!("Key:0x{vk:02X}"),
        }
    }

    /// The key's name as this platform's keyboards print it (Option on a Mac).
    pub fn label(self) -> String {
        match self {
            SwitchKey::Alt if cfg!(target_os = "macos") => "Opt".to_owned(),
            SwitchKey::Key(vk) => vk_label(vk).map(str::to_owned).unwrap_or_else(|| format!("Key 0x{vk:02X}")),
            k => k.name(),
        }
    }

    /// Its Win32 virtual-key code, to ask whether it is down.
    pub fn vk(self) -> u32 {
        match self { SwitchKey::Shift => 0x10, SwitchKey::Ctrl => 0x11, SwitchKey::Alt => 0x12, SwitchKey::Key(v) => v }
    }

    /// The same key as a hotkey modifier bit; `None` for a key that isn't one.
    pub fn modifier(self) -> Option<Modifiers> {
        match self {
            SwitchKey::Shift => Some(Modifiers::SHIFT),
            SwitchKey::Ctrl => Some(Modifiers::CONTROL),
            SwitchKey::Alt => Some(Modifiers::ALT),
            SwitchKey::Key(_) => None,
        }
    }
}

fn default_qr_switch_key() -> String { SwitchKey::Ctrl.name() }
fn default_scroll_switch_key() -> String { SwitchKey::Shift.name() }

impl AppSettings {
    pub fn switch_key(&self) -> SwitchKey { SwitchKey::parse(&self.switch_key) }

    /// The key that flips what a QR link does. Anything unknown reads as
    /// its default, Ctrl.
    pub fn qr_switch_key(&self) -> SwitchKey { SwitchKey::parse_or(&self.qr_switch_key, SwitchKey::Ctrl) }

    pub fn scroll_switch_key(&self) -> SwitchKey { SwitchKey::parse_or(&self.scroll_switch_key, SwitchKey::Shift) }

    pub fn record_format(&self) -> RecordFormat { RecordFormat::parse(&self.record_format) }

    pub fn output_frame(&self) -> OutputFrame { OutputFrame::parse(&self.output_frame) }

    /// Values a hand edit or an old file can get wrong, put back in range.
    /// Run on every way settings come in (load, import, a patch).
    pub fn clamp(&mut self) {
        if self.file_name_pattern.trim().is_empty() { self.file_name_pattern = default_file_name_pattern(); }
        self.record_format = self.record_format().name().to_owned();
        self.output_frame = self.output_frame().name().to_owned();
        self.switch_key = self.switch_key().name();
        self.qr_switch_key = self.qr_switch_key().name();
        self.scroll_switch_key = self.scroll_switch_key().name();
        self.scroll_after_capture = self.scroll_after().name().to_owned();
        if !self.last_thickness.is_finite() || self.last_thickness <= 0.0 { self.last_thickness = default_thickness(); }
        if !self.last_text_size.is_finite() || self.last_text_size <= 0.0 { self.last_text_size = default_text_size(); }
        if self.pixelate_block < 2 { self.pixelate_block = default_pixelate_block(); }
        self.magnify_zoom = self.magnify_zoom.clamp(2, 4);
        // Copy only copies everything (full-screen shots, and a capture
        // flipped into the editor), as Settings shows it.
        if !self.open_editor { self.auto_copy = true; }
        // Crop is never the tool an editor opens with.
        if crate::tool::Tool::ALL.iter().any(|t| t.name() == self.last_tool && !t.can_open_with()) {
            self.last_tool = default_tool();
        }
    }

    /// Whether a region capture opens the editor, given whether the switch
    /// key was held for it.
    pub fn opens_editor(&self, switch_held: bool) -> bool { self.open_editor != switch_held }

    /// The FFmpeg picked in Settings, if any (blank reads as none).
    pub fn chosen_ffmpeg(&self) -> Option<std::path::PathBuf> {
        let p = self.ffmpeg_path.trim();
        (!p.is_empty()).then(|| std::path::PathBuf::from(p))
    }

    /// What a capture does, as the one choice Settings offers.
    pub fn after_capture(&self) -> AfterCapture {
        if !self.open_editor { AfterCapture::CopyOnly }
        else if self.auto_copy { AfterCapture::CopyAndEdit }
        else { AfterCapture::Edit }
    }

    pub fn set_after_capture(&mut self, a: AfterCapture) {
        self.open_editor = a != AfterCapture::CopyOnly;
        self.auto_copy = a != AfterCapture::Edit;
    }

    /// What a scrolling capture does, set apart from [`Self::after_capture`].
    pub fn scroll_after(&self) -> AfterCapture { AfterCapture::parse(&self.scroll_after_capture) }

    pub fn set_scroll_after(&mut self, a: AfterCapture) { self.scroll_after_capture = a.name().to_owned(); }

    /// Whether a scrolling capture opens the editor, given whether the
    /// switch key was held as it was picked.
    pub fn scroll_opens_editor(&self, switch_held: bool) -> bool {
        (self.scroll_after() != AfterCapture::CopyOnly) != switch_held
    }

    /// Whether a scrolling capture is copied: always, except Edit opening
    /// the editor (flipped into Copy only, it is copied).
    pub fn scroll_copies(&self, switch_held: bool) -> bool {
        self.scroll_after() != AfterCapture::Edit || !self.scroll_opens_editor(switch_held)
    }
}

/// What a capture does when the drag ends: open the editor; open it with the
/// picture already copied (and copy again on save or pin, and full-screen
/// shots too); or only copy. Stored as `OpenEditor` plus `AutoCopy`
/// (Brandon 2026-11-20: the separate auto-copy switch overlapped Copy only).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AfterCapture { Edit, CopyAndEdit, CopyOnly }

impl AfterCapture {
    pub const ALL: [AfterCapture; 3] = [AfterCapture::Edit, AfterCapture::CopyAndEdit, AfterCapture::CopyOnly];

    pub fn label(self) -> &'static str {
        match self {
            AfterCapture::Edit        => "Edit",
            AfterCapture::CopyAndEdit => "Copy + edit",
            AfterCapture::CopyOnly    => "Copy only",
        }
    }

    /// How it is stored (`ScrollAfterCapture`).
    pub fn name(self) -> &'static str {
        match self {
            AfterCapture::Edit        => "Edit",
            AfterCapture::CopyAndEdit => "CopyAndEdit",
            AfterCapture::CopyOnly    => "CopyOnly",
        }
    }

    /// A stored name back; anything else is the default, Copy + edit.
    pub fn parse(s: &str) -> AfterCapture {
        AfterCapture::ALL.into_iter().find(|a| a.name().eq_ignore_ascii_case(s.trim())).unwrap_or(AfterCapture::CopyAndEdit)
    }
}

fn default_scroll_after_capture() -> String { AfterCapture::CopyAndEdit.name().to_owned() }

fn default_tool()        -> String  { "Pen".to_owned() }
fn default_color_argb()  -> i32     { 0xFFFF_0000_u32 as i32 }
fn default_thickness()   -> f32     { 3.0 }
fn default_text_size()   -> f32     { crate::text::DEFAULT_PX }
fn default_pixelate_block() -> u32  { 10 }
/// Default capture key, as a Win32 VK.
///
/// Windows and Linux use Print Screen. Apple keyboards have no such key, and
/// `global-hotkey` would translate `0x2C` to Carbon keycode `0x46`, which no
/// Mac keyboard can produce -- so a Print Screen default there is a hotkey
/// that can never fire. macOS uses `7` instead, with Cmd+Shift as modifiers
/// (Cmd+Shift+5 already belongs to the system Screenshot app).
#[cfg(not(target_os = "macos"))]
fn default_vk()          -> u32     { 0x2C }
#[cfg(target_os = "macos")]
fn default_vk()          -> u32     { 0x37 }

/// Modifiers paired with [`default_vk`], as a Win32 `MOD_*` mask.
#[cfg(not(target_os = "macos"))]
fn default_hotkey_modifiers() -> u32 { Modifiers::empty().bits() }
#[cfg(target_os = "macos")]
fn default_hotkey_modifiers() -> u32 { (Modifiers::SUPER | Modifiers::SHIFT).bits() }

fn default_hotkey_modifiers_flags() -> Modifiers {
    Modifiers::from_bits_truncate(default_hotkey_modifiers())
}

/// The dialogs' color scheme by name (Pink, Green, Cyan, Yellow, Orange). Older
/// files hold "Light" or "Dark", which the app reads as Pink.
fn default_theme()       -> String  { "Pink".to_owned() }
fn default_marker_opacity() -> u8 { 0xC8 }

/// One tool's own color and stroke width, so changing one tool never changes
/// another: a yellow highlighter and a red arrow stay that way.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ToolStyle {
    /// ARGB, the same packing as `LastColorArgb`.
    #[serde(rename = "Color")]
    pub color_argb: i32,
    #[serde(rename = "Thickness")]
    pub thickness: f32,
}

impl ToolStyle {
    /// A tool that has never been styled: the Marker is a thick yellow
    /// highlighter; everything else starts from the last global color and
    /// width, which is what every tool shared before tools had their own.
    /// Both colors are palette colors, so a swatch shows as chosen; the
    /// factory global color (pure red) is on no palette, so it becomes the
    /// Vivid palette's red.
    pub fn default_for(tool: crate::tool::Tool, s: &AppSettings) -> ToolStyle {
        let vivid = &crate::annotation::Palettes::ALL[0].colors;
        match tool {
            crate::tool::Tool::Marker => ToolStyle { color_argb: vivid[3].to_argb(), thickness: 16.0 },
            // A highlighter's yellow.
            crate::tool::Tool::Highlight => ToolStyle {
                color_argb: crate::mark::MarkLook::default().color.to_argb(), thickness: s.last_thickness,
            },
            _ => ToolStyle {
                color_argb: if s.last_color_argb == default_color_argb() { vivid[0].to_argb() } else { s.last_color_argb },
                thickness: s.last_thickness,
            },
        }
    }
}

impl AppSettings {
    /// `tool`'s saved style, or its default.
    pub fn tool_style(&self, tool: crate::tool::Tool) -> ToolStyle {
        self.tool_styles.get(tool.name()).copied()
            .unwrap_or_else(|| ToolStyle::default_for(tool, self))
    }

    pub fn set_tool_style(&mut self, tool: crate::tool::Tool, style: ToolStyle) {
        self.tool_styles.insert(tool.name().to_owned(), style);
    }

    /// `~/.../Framefang/`, created if it doesn't exist.
    pub fn config_dir() -> Option<PathBuf> {
        let dirs = directories::ProjectDirs::from("org", "framefang", "Framefang")?;
        Some(dirs.config_dir().to_path_buf())
    }

    pub fn settings_path() -> Option<PathBuf> {
        Self::config_dir().map(|d| d.join("settings.json"))
    }

    /// Load settings; a missing file gives `Default`. See [`Self::load_reporting`]
    /// for a file that doesn't parse.
    pub fn load() -> AppSettings {
        Self::load_reporting().0
    }

    /// Load settings, and say what went wrong when `settings.json` exists
    /// but doesn't parse (a hand edit with a typo). Such a file is copied to
    /// `settings.broken.json` first, so the next save can't overwrite the
    /// only copy of the user's text, and the app runs on defaults.
    ///
    /// The first run after the rename finds no file here; it reads the one
    /// the app kept under its old name instead (`org/kashot/Kashot`) and
    /// saves a copy here, so hotkeys, folders and tool styles carry over.
    /// The old file is left in place.
    pub fn load_reporting() -> (AppSettings, Option<String>) {
        let Some(path) = Self::settings_path() else { return (AppSettings::default(), None); };
        match fs::read_to_string(&path) {
            Ok(s) => match Self::parse(&s) {
                Ok(settings) => (settings, None),
                Err(e) => {
                    let kept = path.with_file_name("settings.broken.json");
                    let note = match fs::copy(&path, &kept) {
                        Ok(_) => format!("settings.json couldn't be read ({e}). Frame Fang started with \
                                          the defaults and kept your file as settings.broken.json."),
                        Err(_) => format!("settings.json couldn't be read ({e}). Frame Fang started with \
                                           the defaults."),
                    };
                    (AppSettings::default(), Some(note))
                }
            },
            Err(_) => match Self::pre_rename_settings() {
                Some(s) => {
                    let _ = s.save();
                    (s, None)
                }
                None => (AppSettings::default(), None),
            },
        }
    }

    /// Settings from JSON text. Missing fields take their defaults; anything
    /// that isn't JSON, or a field of the wrong kind, is an error naming the
    /// line and column.
    pub fn parse(text: &str) -> Result<AppSettings, String> {
        let mut s: AppSettings = serde_json::from_str(text).map_err(|e| e.to_string())?;
        s.clamp();
        Ok(s)
    }

    /// Settings read from a file the user picked (Import in Settings).
    pub fn read_from(path: &std::path::Path) -> Result<AppSettings, String> {
        let text = fs::read_to_string(path).map_err(|e| e.to_string())?;
        if text.trim().is_empty() { return Err("the file is empty".to_owned()); }
        Self::parse(&text)
    }

    /// These settings written to a file the user picked (Export in Settings),
    /// in the same form as `settings.json`.
    pub fn write_to(&self, path: &std::path::Path) -> io::Result<()> {
        let json = serde_json::to_string_pretty(self).map_err(io::Error::other)?;
        crate::atomic_file::write_atomic(path, json.as_bytes())
    }

    /// The settings file from before the app was renamed, if one exists and
    /// parses.
    fn pre_rename_settings() -> Option<AppSettings> {
        let dirs = directories::ProjectDirs::from("org", "kashot", "Kashot")?;
        let text = fs::read_to_string(dirs.config_dir().join("settings.json")).ok()?;
        serde_json::from_str(&text).ok()
    }

    /// Persist settings to `settings.json`.
    ///
    /// The write is atomic (see [`crate::atomic_file::write_atomic`]): the
    /// JSON goes to a temp file alongside the destination and is renamed over
    /// it. A crash mid-write therefore leaves the *previous* settings intact
    /// instead of a truncated document that `load` can only answer with
    /// `Default` — which reads to the user as "it forgot my save folder,
    /// my hotkey and my tools".
    ///
    /// Errors are returned rather than swallowed so the caller can tell the
    /// user their change won't survive a restart.
    pub fn save(&self) -> io::Result<()> {
        let dir  = Self::config_dir().ok_or_else(|| io::Error::other("no config dir"))?;
        fs::create_dir_all(&dir)?;
        let path = dir.join("settings.json");
        let json = serde_json::to_string_pretty(self).map_err(io::Error::other)?;
        crate::atomic_file::write_atomic(&path, json.as_bytes())
    }

    pub fn theme(&self) -> ThemeName {
        ThemeName::parse(&self.theme)
    }

    pub fn hotkey(&self) -> Hotkey {
        Hotkey {
            modifiers:   Modifiers::from_bits_truncate(self.hotkey_modifiers),
            virtual_key: self.hotkey_virtual_key,
        }
    }

    pub fn set_hotkey(&mut self, hk: Hotkey) {
        self.hotkey_modifiers   = hk.modifiers.bits();
        self.hotkey_virtual_key = hk.virtual_key;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_round_trips_through_json() {
        let s = AppSettings::default();
        let txt = serde_json::to_string_pretty(&s).unwrap();
        let s2: AppSettings = serde_json::from_str(&txt).unwrap();
        assert_eq!(s.last_tool, s2.last_tool);
        assert_eq!(s.last_color_argb, s2.last_color_argb);
        assert_eq!(s.hotkey_virtual_key, s2.hotkey_virtual_key);
        assert_eq!(s, s2, "every field survives the trip");
    }

    #[test]
    fn a_hand_edit_with_a_typo_is_an_error_that_names_the_line() {
        let e = AppSettings::parse("{\n  \"Theme\": \"Orange\",\n  \"AutoCopy\": nope,\n  \"OpenEditor\": true\n}").unwrap_err();
        assert!(e.contains("line 3"), "{e}");
        let e = AppSettings::parse("{ \"AutoCopy\": \"yes\" }").unwrap_err();
        assert!(e.contains("line 1"), "a field of the wrong kind is refused: {e}");
    }

    #[test]
    fn a_partial_file_fills_the_rest_with_defaults() {
        let s = AppSettings::parse("{ \"Theme\": \"Orange\" }").unwrap();
        assert_eq!(s.theme, "Orange");
        assert_eq!(AppSettings { theme: "Pink".into(), ..s }, AppSettings::default());
    }

    #[test]
    fn export_then_import_gives_the_same_settings() {
        let dir = std::env::temp_dir().join(format!("ff-settings-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("exported.json");
        let mut s = AppSettings::default();
        s.theme = "Orange".into();
        s.auto_copy = true;
        s.record_sound = "Both".into();
        s.write_to(&path).unwrap();
        assert_eq!(AppSettings::read_from(&path).unwrap(), s);
        std::fs::write(&path, "").unwrap();
        assert!(AppSettings::read_from(&path).is_err(), "an empty file is refused");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_key_table_is_unique_ascii_and_guards_typing_keys() {
        let mut seen = std::collections::HashSet::new();
        for i in KEYS {
            assert!(seen.insert(i.vk), "vk 0x{:02X} listed twice", i.vk);
            assert!(i.label.bytes().all(|b| (0x20..=0x7E).contains(&b)), "{} is not ASCII", i.label);
        }
        for vk in [0x2C, 0x13, 0x7C, 0x87, 0x5D, 0xB3, 0x70] {
            assert!(bindable_alone(vk), "0x{vk:02X} should work alone");
        }
        for vk in [0x41, 0x31, 0x20, 0x25, 0x0D, 0x60, 0xBA, 0x08] {
            assert!(!bindable_alone(vk), "0x{vk:02X} is a typing key");
        }
        assert!(bindable_alone(0xE8), "unknown keys are special keys");
        assert_eq!(vk_label(0x7C), Some("F13"));
    }

    #[test]
    fn a_typing_key_needs_a_modifier_that_stops_it_typing() {
        let hk = |m: Modifiers, vk: u32| Hotkey { modifiers: m, virtual_key: vk };
        assert!(blocks_typing(&hk(Modifiers::empty(), 0x53)), "S alone types S");
        assert!(blocks_typing(&hk(Modifiers::SHIFT, 0x53)), "Shift+S types a capital S");
        assert!(!blocks_typing(&hk(Modifiers::CONTROL | Modifiers::SHIFT, 0x53)));
        assert!(!blocks_typing(&hk(Modifiers::empty(), 0x2C)), "Print Screen types nothing");
        assert!(!blocks_typing(&hk(Modifiers::SHIFT, 0x7C)), "Shift+F13 is fine");
    }

    #[test]
    fn missing_keys_are_filled_with_defaults() {
        let s: AppSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(s.last_tool, "Pen");
        assert_eq!(s.theme, "Pink");
        assert_eq!(s.hotkey_virtual_key, default_vk());
        assert_eq!(s.hotkey(), Hotkey::default());
    }

    #[test]
    fn modifiers_serialize_as_u32() {
        let m = Modifiers::CONTROL | Modifiers::SHIFT;
        let bits: u32 = serde_json::from_str(&serde_json::to_string(&m).unwrap()).unwrap();
        assert_eq!(bits, 0x0002 | 0x0004);
    }

    #[test]
    fn hotkey_ctrl_shift_p_round_trips() {
        let hk = Hotkey {
            modifiers:   Modifiers::CONTROL | Modifiers::SHIFT,
            virtual_key: 0x50,
        };
        let mut s = AppSettings::default();
        s.set_hotkey(hk);
        let txt = serde_json::to_string_pretty(&s).unwrap();
        let s2: AppSettings = serde_json::from_str(&txt).unwrap();
        let hk2 = s2.hotkey();
        assert_eq!(hk, hk2);
        assert_eq!(hk2.modifiers.bits(), 0x0002 | 0x0004);
        assert_eq!(hk2.virtual_key, 0x50);
        assert_eq!(hk2.describe(), "Ctrl + Shift + P");
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn default_hotkey_is_printscreen() {
        let s = AppSettings::default();
        let hk = s.hotkey();
        assert_eq!(hk.modifiers, Modifiers::empty());
        assert_eq!(hk.virtual_key, 0x2C);
        assert_eq!(hk.describe(), "Print Screen");
        assert_eq!(Hotkey::default(), hk);
    }

    /// Apple keyboards have no Print Screen key, so the macOS default is
    /// Cmd+Shift+7 (Cmd+Shift+5 is the system Screenshot app's).
    #[cfg(target_os = "macos")]
    #[test]
    fn default_hotkey_is_cmd_shift_7() {
        let s = AppSettings::default();
        let hk = s.hotkey();
        assert_eq!(hk.modifiers, Modifiers::SUPER | Modifiers::SHIFT);
        assert_eq!(hk.virtual_key, 0x37);
        assert_eq!(hk.describe(), "Cmd + Shift + 7");
        assert_eq!(Hotkey::default(), hk);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_modifier_labels_are_ascii_mac_names() {
        let all = Modifiers::SUPER | Modifiers::CONTROL | Modifiers::ALT | Modifiers::SHIFT;
        assert_eq!(all.describe(), "Cmd + Ctrl + Opt + Shift");
        assert!(all.describe().is_ascii());
    }

    #[test]
    fn default_hotkey_label_is_ascii() {
        assert!(Hotkey::default().describe().is_ascii());
    }

    #[test]
    fn default_vk_has_a_label() {
        assert!(vk_label(default_vk()).is_some());
    }

    #[test]
    fn vk_label_covers_common_keys() {
        assert_eq!(vk_label(0x2C), Some("Print Screen"));
        assert_eq!(vk_label(0x50), Some("P"));
        assert_eq!(vk_label(0x70), Some("F1"));
        assert_eq!(vk_label(0x7B), Some("F12"));
        assert_eq!(vk_label(0x25), Some("Left"));
        assert_eq!(vk_label(0xABCD), None);
    }

    #[test]
    fn marker_opacity_default_preserves_legacy_alpha() {
        assert_eq!(AppSettings::default().marker_opacity, 0xC8);
    }

    #[test]
    fn marker_opacity_round_trips_through_json() {
        let mut s = AppSettings::default();
        s.marker_opacity = 0x40;
        let txt = serde_json::to_string(&s).unwrap();
        assert!(txt.contains("\"MarkerOpacity\""), "JSON key should be MarkerOpacity: {txt}");
        let s2: AppSettings = serde_json::from_str(&txt).unwrap();
        assert_eq!(s2.marker_opacity, 0x40);
    }

    #[test]
    fn each_tool_keeps_its_own_style() {
        use crate::tool::Tool;
        let mut s = AppSettings::default();
        // Unstyled: the marker is yellow and thick, the rest follow the
        // legacy global width, in a color that is on a palette.
        let on_a_palette = |argb: i32| crate::annotation::Palettes::ALL.iter()
            .any(|p| p.colors.iter().any(|c| c.to_argb() == argb));
        assert_eq!(s.tool_style(Tool::Marker).thickness, 16.0);
        assert!(on_a_palette(s.tool_style(Tool::Marker).color_argb));
        assert!(on_a_palette(s.tool_style(Tool::Arrow).color_argb));
        // A global color someone picked before tools had their own is kept.
        let mut picked = AppSettings::default();
        picked.last_color_argb = crate::color::Rgba::new_opaque(0, 180, 80).to_argb();
        assert_eq!(picked.tool_style(Tool::Arrow).color_argb, picked.last_color_argb);
        let blue = crate::color::Rgba::new_opaque(0, 100, 255).to_argb();
        s.set_tool_style(Tool::Arrow, ToolStyle { color_argb: blue, thickness: 6.0 });
        assert_eq!(s.tool_style(Tool::Arrow).color_argb, blue);
        assert_ne!(s.tool_style(Tool::Pen).color_argb, blue, "other tools untouched");
        let back: AppSettings = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(back.tool_style(Tool::Arrow), ToolStyle { color_argb: blue, thickness: 6.0 });
    }

    #[test]
    fn tool_flyout_values_round_trip_and_default_when_missing() {
        let mut s = AppSettings::default();
        s.last_text_size = 40.0;
        s.pixelate_block = 22;
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.contains("\"LastTextSize\"") && json.contains("\"PixelateBlock\""));
        let back: AppSettings = serde_json::from_str(&json).unwrap();
        assert_eq!((back.last_text_size, back.pixelate_block), (40.0, 22));
        // A settings file from before these existed still loads.
        let old: AppSettings = serde_json::from_str("{}").unwrap();
        assert_eq!((old.last_text_size, old.pixelate_block), (crate::text::DEFAULT_PX, 10));
        assert!(!old.auto_copy, "auto-copy starts off");
        assert!(!old.close_after_copy, "Copy keeps the editor open unless asked");
        let on: AppSettings = serde_json::from_str("{\"AutoCopy\":true}").unwrap();
        assert!(on.auto_copy);
    }

    #[test]
    fn a_capture_opens_the_editor_unless_set_or_switched_otherwise() {
        // A file from before the setting existed keeps opening the editor.
        let old: AppSettings = serde_json::from_str("{}").unwrap();
        assert!(old.open_editor);
        assert_eq!(old.switch_key(), SwitchKey::Shift);
        assert!(old.opens_editor(false));
        assert!(!old.opens_editor(true), "holding the key skips the editor");
        // Copy-only: holding the key is how to get the editor.
        let copy: AppSettings = serde_json::from_str("{\"OpenEditor\":false,\"SwitchKey\":\"Alt\"}").unwrap();
        assert!(!copy.opens_editor(false));
        assert!(copy.opens_editor(true));
        assert_eq!(copy.switch_key(), SwitchKey::Alt);
        let back: AppSettings = serde_json::from_str(&serde_json::to_string(&copy).unwrap()).unwrap();
        assert_eq!((back.open_editor, back.switch_key()), (false, SwitchKey::Alt));
        // Unknown words fall back to Shift rather than to no key.
        assert_eq!(SwitchKey::parse("Hyper"), SwitchKey::Shift);
        for k in SwitchKey::ALL { assert_eq!(SwitchKey::parse(&k.name()), k); }
    }

    #[test]
    fn after_a_capture_is_one_choice_of_three_over_the_two_stored_fields() {
        // Every file reads as one of the three, and each choice round-trips.
        let read = |json: &str| { let mut s: AppSettings = serde_json::from_str(json).unwrap(); s.clamp(); s };
        assert_eq!(read("{}").after_capture(), AfterCapture::Edit, "the editor, nothing copied, by default");
        assert_eq!(read("{\"AutoCopy\":true}").after_capture(), AfterCapture::CopyAndEdit);
        assert_eq!(read("{\"OpenEditor\":false,\"AutoCopy\":true}").after_capture(), AfterCapture::CopyOnly);
        // An old copy-only file with auto-copy off now copies everything,
        // full-screen shots too, as Copy only says.
        let old = read("{\"OpenEditor\":false}");
        assert_eq!(old.after_capture(), AfterCapture::CopyOnly);
        assert!(old.auto_copy);
        for a in AfterCapture::ALL {
            let mut s = AppSettings::default();
            s.set_after_capture(a);
            s.clamp();
            assert_eq!(s.after_capture(), a);
            assert_eq!(s.opens_editor(false), a != AfterCapture::CopyOnly);
        }
    }

    #[test]
    fn a_scrolling_capture_has_its_own_after_choice_that_opens_the_editor_by_default() {
        let read = |json: &str| { let mut s: AppSettings = serde_json::from_str(json).unwrap(); s.clamp(); s };
        // A copy-only file from before still opens long pages in the editor.
        let old = read("{\"OpenEditor\":false,\"AutoCopy\":true}");
        assert_eq!(old.scroll_after(), AfterCapture::CopyAndEdit);
        assert!(old.scroll_opens_editor(false) && old.scroll_copies(false));
        assert!(!old.scroll_opens_editor(true), "the switch key flips it to copy only");
        assert_eq!(read("{\"ScrollAfterCapture\":\"nonsense\"}").scroll_after_capture, "CopyAndEdit");
        for a in AfterCapture::ALL {
            let mut s = AppSettings::default();
            s.set_scroll_after(a);
            s.clamp();
            assert_eq!(s.scroll_after(), a, "round trip");
            assert_eq!(s.after_capture(), AfterCapture::Edit, "region captures untouched");
            assert_eq!(s.scroll_opens_editor(false), a != AfterCapture::CopyOnly);
            assert_eq!(s.scroll_copies(false), a != AfterCapture::Edit);
            // Flipped, it always ends up copied one way or the other.
            assert!(s.scroll_copies(true) || s.scroll_opens_editor(true));
        }
        assert_eq!(AppSettings::default().scroll_switch_key(), SwitchKey::Shift, "its own key, Shift unless set");
        assert_eq!(read("{\"ScrollSwitchKey\":\"Alt\"}").scroll_switch_key(), SwitchKey::Alt);
        let mut edit = AppSettings::default();
        edit.set_scroll_after(AfterCapture::Edit);
        assert!(!edit.scroll_opens_editor(true) && edit.scroll_copies(true), "Edit flipped: copied only");
    }

    #[test]
    fn any_key_can_be_the_hold_key_but_the_capture_screens_own() {
        // A bound key is stored by its code and read back.
        let q = SwitchKey::Key(0x51);
        assert_eq!(q.name(), "Key:0x51");
        assert_eq!(SwitchKey::parse(&q.name()), q);
        assert_eq!(q.label(), "Q");
        assert_eq!(q.modifier(), None);
        // Either side of a modifier is the modifier.
        assert_eq!(SwitchKey::from_vk(0xA1), Some(SwitchKey::Shift));
        assert_eq!(SwitchKey::from_vk(0xA3), Some(SwitchKey::Ctrl));
        assert_eq!(SwitchKey::from_vk(0x20), Some(SwitchKey::Key(0x20)));
        // Tab, Enter, Esc and the Windows keys already mean something.
        for vk in [0x09, 0x0D, 0x1B, 0x5B, 0x5C, 0] { assert_eq!(SwitchKey::from_vk(vk), None, "{vk:#x}"); }
        assert_eq!(SwitchKey::parse_or("Key:0x09", SwitchKey::Ctrl), SwitchKey::Ctrl, "a hand-edited Tab falls back");
        // QR's key: Ctrl unless set; anything stored is kept through clamp.
        let mut s = AppSettings::default();
        assert_eq!(s.qr_switch_key(), SwitchKey::Ctrl);
        s.qr_switch_key = "Key:0x20".into();
        s.clamp();
        assert_eq!(s.qr_switch_key(), SwitchKey::Key(0x20));
    }

    #[test]
    fn marker_opacity_missing_key_falls_back_to_default() {
        let s: AppSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(s.marker_opacity, 0xC8);
    }
}
