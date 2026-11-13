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
    /// Human-readable rendering, e.g. `"Ctrl + Shift + P"` or `"PrintScreen"`.
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

/// Map a Win32 virtual-key code to a short, user-facing label.
///
/// Canonical source for VK → label rendering shared by the Settings dialog,
/// the tray tooltip, and any future UI surface. Returns `None` for codes the
/// rebind widget doesn't know how to display so callers can render an
/// "(0xNN)" fallback. Keep this list in sync with `vk_to_code` in
/// `framefang-platform::hotkey` — anything `vk_to_code` accepts should have a
/// label here.
pub fn vk_label(vk: u32) -> Option<&'static str> {
    Some(match vk {
        0x08 => "Backspace",
        0x09 => "Tab",
        0x0D => "Enter",
        0x14 => "CapsLock",
        0x1B => "Esc",
        0x20 => "Space",
        0x21 => "PageUp",
        0x22 => "PageDown",
        0x23 => "End",
        0x24 => "Home",
        0x25 => "Left",
        0x26 => "Up",
        0x27 => "Right",
        0x28 => "Down",
        0x2C => "PrintScreen",
        0x2D => "Insert",
        0x2E => "Delete",
        0x91 => "ScrollLock",
        0x13 => "Pause",
        0x30 => "0", 0x31 => "1", 0x32 => "2", 0x33 => "3", 0x34 => "4",
        0x35 => "5", 0x36 => "6", 0x37 => "7", 0x38 => "8", 0x39 => "9",
        0x41 => "A", 0x42 => "B", 0x43 => "C", 0x44 => "D", 0x45 => "E",
        0x46 => "F", 0x47 => "G", 0x48 => "H", 0x49 => "I", 0x4A => "J",
        0x4B => "K", 0x4C => "L", 0x4D => "M", 0x4E => "N", 0x4F => "O",
        0x50 => "P", 0x51 => "Q", 0x52 => "R", 0x53 => "S", 0x54 => "T",
        0x55 => "U", 0x56 => "V", 0x57 => "W", 0x58 => "X", 0x59 => "Y",
        0x5A => "Z",
        0x70 => "F1",  0x71 => "F2",  0x72 => "F3",  0x73 => "F4",
        0x74 => "F5",  0x75 => "F6",  0x76 => "F7",  0x77 => "F8",
        0x78 => "F9",  0x79 => "F10", 0x7A => "F11", 0x7B => "F12",
        _ => return None,
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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

    /// Each tool's own color and stroke width, keyed by `Tool::name`.
    /// A tool with no entry uses `ToolStyle::default_for`.
    #[serde(rename = "ToolStyles", default)]
    pub tool_styles: std::collections::BTreeMap<String, ToolStyle>,

    #[serde(rename = "SaveDirectory", default)]
    pub save_directory: String,

    #[serde(rename = "RecordingsDirectory", default)]
    pub recordings_directory: String,

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
            tool_styles:         Default::default(),
            save_directory:      String::new(),
            recordings_directory: String::new(),
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
        }
    }
}

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

/// The dialogs' color scheme by name (Pink, Green, Cyan, Yellow). Older
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

    /// Load settings; missing or malformed file silently returns `Default`.
    ///
    /// The first run after the rename finds no file here; it reads the one
    /// the app kept under its old name instead (`org/kashot/Kashot`) and
    /// saves a copy here, so hotkeys, folders and tool styles carry over.
    /// The old file is left in place.
    pub fn load() -> AppSettings {
        let Some(path) = Self::settings_path() else { return AppSettings::default(); };
        match fs::read_to_string(&path) {
            Ok(s) => serde_json::from_str(&s).unwrap_or_default(),
            Err(_) => match Self::pre_rename_settings() {
                Some(s) => {
                    let _ = s.save();
                    s
                }
                None => AppSettings::default(),
            },
        }
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
        assert_eq!(hk.describe(), "PrintScreen");
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
        assert_eq!(vk_label(0x2C), Some("PrintScreen"));
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
    fn marker_opacity_missing_key_falls_back_to_default() {
        let s: AppSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(s.marker_opacity, 0xC8);
    }
}
