//! The global shortcuts Frame Fang can register, and how they are stored.
//!
//! Region capture always has a shortcut. Every other action (full screen,
//! recording, last region, screen text, QR codes, scrolling capture) only
//! gets one when the user sets it. This module is the logic both the
//! platform layer and the Settings dialog rely on: the list of actions, where
//! each one's binding sits in [`AppSettings`], and spotting two actions set
//! to the same chord.
//!
//! On disk each action is a pair of numbers in settings.json: a Win32
//! modifier mask and a Win32 virtual-key code. A key code of [`UNSET_VK`]
//! means the action has no shortcut. A settings.json from before an action
//! existed has no fields for it, which read as that same zero, so older files
//! load with the newer actions simply unset.

use crate::settings::{AppSettings, Hotkey, Modifiers};

/// The key code that marks an action as having no shortcut. Win32 assigns
/// no key to zero, so a real key press can never be mistaken for it.
pub const UNSET_VK: u32 = 0;

/// An action a global shortcut can start. The variant order is the order
/// Settings lists them in, and the order clashes are reported in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HotkeyAction {
    /// Freeze the screen and let the user pick a region to edit.
    Capture,
    /// Take every screen at once and save it without asking.
    CaptureFullScreen,
    /// Begin a screen recording, or end the one in progress.
    ToggleRecording,
    /// Take the previous capture's rectangle again and treat it as a fresh
    /// drag that was just let go.
    CaptureLastRegion,
    /// Open the picker in TEXT mode, which copies the words in a region.
    CaptureText,
    /// Open the picker in QR mode, which copies what a QR code says.
    CaptureQr,
    /// Open the picker in SCROLL mode, which scrolls a page and joins the
    /// pieces into one picture.
    CaptureScroll,
}

impl HotkeyAction {
    /// All actions, in the order Settings lists them.
    pub const ALL: [HotkeyAction; 7] = [
        Self::Capture, Self::CaptureFullScreen, Self::ToggleRecording,
        Self::CaptureLastRegion, Self::CaptureText, Self::CaptureQr,
        Self::CaptureScroll,
    ];

    /// The action's name as Settings shows it. Clash messages put it in the
    /// middle of a sentence, so it is written to read naturally there.
    pub fn label(self) -> &'static str {
        match self {
            Self::Capture => "Capture region",
            Self::CaptureFullScreen => "Capture full screen",
            Self::ToggleRecording => "Record screen",
            Self::CaptureLastRegion => "Capture last region",
            Self::CaptureText => "Copy screen text",
            Self::CaptureQr => "Read QR code",
            Self::CaptureScroll => "Scrolling capture",
        }
    }

    /// Whether the user may clear this action's shortcut. Region capture
    /// comes with a shortcut for each platform and must keep one; every
    /// other action is something the user opts into.
    pub fn is_optional(self) -> bool {
        self != Self::Capture
    }
}

/// The two settings fields behind an action's binding, as a pair of
/// references `(modifier mask, key code)`. Pass `mut` as the third argument
/// for mutable references. One table serves both reading and writing, so
/// the two can never disagree about which field belongs to which action.
macro_rules! binding_fields {
    ($settings:expr, $action:expr $(, $mutable:tt)?) => {
        match $action {
            HotkeyAction::Capture => (
                &$($mutable)? $settings.hotkey_modifiers,
                &$($mutable)? $settings.hotkey_virtual_key,
            ),
            HotkeyAction::CaptureFullScreen => (
                &$($mutable)? $settings.fullscreen_hotkey_modifiers,
                &$($mutable)? $settings.fullscreen_hotkey_virtual_key,
            ),
            HotkeyAction::ToggleRecording => (
                &$($mutable)? $settings.record_hotkey_modifiers,
                &$($mutable)? $settings.record_hotkey_virtual_key,
            ),
            HotkeyAction::CaptureLastRegion => (
                &$($mutable)? $settings.last_region_hotkey_modifiers,
                &$($mutable)? $settings.last_region_hotkey_virtual_key,
            ),
            HotkeyAction::CaptureText => (
                &$($mutable)? $settings.text_hotkey_modifiers,
                &$($mutable)? $settings.text_hotkey_virtual_key,
            ),
            HotkeyAction::CaptureQr => (
                &$($mutable)? $settings.qr_hotkey_modifiers,
                &$($mutable)? $settings.qr_hotkey_virtual_key,
            ),
            HotkeyAction::CaptureScroll => (
                &$($mutable)? $settings.scroll_hotkey_modifiers,
                &$($mutable)? $settings.scroll_hotkey_virtual_key,
            ),
        }
    };
}

impl AppSettings {
    /// The shortcut set for `action`, or `None` if it has none.
    pub fn hotkey_for(&self, action: HotkeyAction) -> Option<Hotkey> {
        let (mask, key) = binding_fields!(self, action);
        (*key != UNSET_VK).then(|| Hotkey {
            modifiers: Modifiers::from_bits_truncate(*mask),
            virtual_key: *key,
        })
    }

    /// Set `action`'s shortcut, or clear it with `None`. A cleared action
    /// also gets an empty modifier mask, so the file never shows leftover
    /// modifiers beside a key that is not set.
    pub fn set_hotkey_for(&mut self, action: HotkeyAction, hk: Option<Hotkey>) {
        let (mask, key) = binding_fields!(self, action, mut);
        (*mask, *key) = hk.map_or((0, UNSET_VK), |chord| (chord.modifiers.bits(), chord.virtual_key));
    }

    /// The actions that have a shortcut, each with it, in Settings order.
    /// This list is what the platform layer registers with the OS.
    pub fn hotkey_bindings(&self) -> Vec<(HotkeyAction, Hotkey)> {
        HotkeyAction::ALL
            .into_iter()
            .filter_map(|action| Some((action, self.hotkey_for(action)?)))
            .collect()
    }

    /// The capture shortcut with the switch key held as well. Pressing it
    /// still captures, but that one capture does the other thing (see
    /// `opens_editor`). There is none when the switch key is not a modifier
    /// (such a key is only read as the drag is let go), when the capture
    /// shortcut already includes it (the release reads it there), or when
    /// another action already owns the combined chord.
    pub fn capture_switch_chord(&self) -> Option<Hotkey> {
        let capture = self.hotkey_for(HotkeyAction::Capture)?;
        let extra = self.switch_key().modifier()?;
        if capture.modifiers.contains(extra) {
            return None;
        }
        let chord = Hotkey { modifiers: capture.modifiers | extra, ..capture };
        let unclaimed = self.hotkey_bindings().into_iter().all(|(_, bound)| bound != chord);
        unclaimed.then_some(chord)
    }

    /// The first two actions, in Settings order, set to the same chord.
    ///
    /// The OS has no way to share one chord between two actions: Windows
    /// refuses the second registration outright, and on X11 both grabs land
    /// on the same key and whichever is matched first wins. Settings will not
    /// save while this returns a pair, so the user never meets that.
    pub fn hotkey_conflict(&self) -> Option<(HotkeyAction, HotkeyAction)> {
        let bound = self.hotkey_bindings();
        bound.iter().enumerate().find_map(|(i, &(first, chord))| {
            bound[i + 1..]
                .iter()
                .find(|(_, other)| *other == chord)
                .map(|&(second, _)| (first, second))
        })
    }
}

/// The line Settings shows under the form when two actions clash.
///
/// The footer draws it as one line of the 5x7 bitmap font, which has room
/// for 99 characters. The longest possible message (the two longest action
/// names with every modifier and the longest key name) stays within that.
pub fn conflict_message(a: HotkeyAction, b: HotkeyAction, hk: Hotkey) -> String {
    format!("{} and {} both use {}.", a.label(), b.label(), hk.describe())
}

#[cfg(test)]
mod tests {
    use super::*;

    const CTRL: Modifiers = Modifiers::CONTROL;
    const CTRL_SHIFT: Modifiers = Modifiers::CONTROL.union(Modifiers::SHIFT);

    fn chord(modifiers: Modifiers, virtual_key: u32) -> Hotkey {
        Hotkey { modifiers, virtual_key }
    }

    /// Settings as a new install has them.
    fn fresh() -> AppSettings { AppSettings::default() }

    fn through_json(s: &AppSettings) -> AppSettings {
        serde_json::from_str(&serde_json::to_string_pretty(s).unwrap()).unwrap()
    }

    #[test]
    fn a_fresh_install_binds_region_capture_and_nothing_else() {
        let s = fresh();
        assert!(s.hotkey_for(HotkeyAction::Capture).is_some());
        for action in HotkeyAction::ALL.into_iter().skip(1) {
            assert_eq!(s.hotkey_for(action), None, "{action:?}");
        }
        assert_eq!(s.hotkey_bindings().len(), 1);
    }

    #[test]
    fn region_capture_uses_the_platform_default_chord() {
        let s = fresh();
        assert_eq!(s.hotkey(), Hotkey::default());
        assert_eq!(s.hotkey_for(HotkeyAction::Capture), Some(s.hotkey()));
    }

    #[test]
    fn three_bindings_survive_a_trip_through_json() {
        let mut s = fresh();
        s.set_hotkey_for(HotkeyAction::Capture, Some(chord(CTRL, 0x2C)));
        s.set_hotkey_for(HotkeyAction::CaptureFullScreen, Some(chord(CTRL_SHIFT, 0x46)));
        s.set_hotkey_for(HotkeyAction::ToggleRecording, Some(chord(Modifiers::ALT | Modifiers::SUPER, 0x52)));
        let text = serde_json::to_string_pretty(&s).unwrap();
        for field in ["FullScreenHotkeyModifiers", "FullScreenHotkeyVirtualKey",
                      "RecordHotkeyModifiers", "RecordHotkeyVirtualKey"] {
            assert!(text.contains(&format!("\"{field}\"")), "{field} not written: {text}");
        }
        let back: AppSettings = serde_json::from_str(&text).unwrap();
        assert_eq!(back.hotkey_bindings(), vec![
            (HotkeyAction::Capture, chord(CTRL, 0x2C)),
            (HotkeyAction::CaptureFullScreen, chord(CTRL_SHIFT, 0x46)),
            (HotkeyAction::ToggleRecording, chord(Modifiers::ALT | Modifiers::SUPER, 0x52)),
        ]);
    }

    #[test]
    fn clearing_writes_zeros_and_stays_cleared_after_reload() {
        let mut s = fresh();
        s.set_hotkey_for(HotkeyAction::ToggleRecording, Some(chord(Modifiers::ALT, 0x52)));
        s.set_hotkey_for(HotkeyAction::ToggleRecording, None);
        assert_eq!((s.record_hotkey_modifiers, s.record_hotkey_virtual_key), (0, UNSET_VK));
        assert_eq!(through_json(&s).hotkey_for(HotkeyAction::ToggleRecording), None);
    }

    /// A file from before there was more than one shortcut keeps its capture
    /// chord and its other settings, with no new action bound.
    #[test]
    fn a_single_hotkey_file_still_loads() {
        // Frame Fang has no watermark settings; those fields are ignored.
        let old = concat!(
            r#"{"LastTool": "Arrow", "LastColorArgb": -65536, "LastThickness": 3.0,"#,
            r#" "SaveDirectory": "", "RecordingsDirectory": "","#,
            r#" "HotkeyModifiers": 6, "HotkeyVirtualKey": 80, "StartWithWindows": false,"#,
            r#" "WatermarkEnabled": true, "WatermarkText": "Framefang", "WatermarkOpacity": 0.85,"#,
            r#" "WatermarkPosition": "BottomRight", "PaletteIndex": 0, "Theme": "Dark","#,
            r#" "MarkerOpacity": 200}"#,
        );
        let s: AppSettings = serde_json::from_str(old).unwrap();
        assert_eq!(s.hotkey_bindings(), vec![(HotkeyAction::Capture, chord(CTRL_SHIFT, 0x50))]);
        assert_eq!((s.last_tool.as_str(), s.theme.as_str(), s.marker_opacity), ("Arrow", "Dark", 200));
    }

    /// `{}` is the other shape old files take: only the default capture.
    #[test]
    fn an_empty_object_binds_only_the_default_capture() {
        let s: AppSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(s.hotkey_bindings(), vec![(HotkeyAction::Capture, Hotkey::default())]);
        assert!(s.hotkey_conflict().is_none());
    }

    #[test]
    fn two_actions_on_one_chord_clash() {
        let mut s = fresh();
        let shared = chord(CTRL_SHIFT, 0x52);
        s.set_hotkey_for(HotkeyAction::CaptureFullScreen, Some(shared));
        s.set_hotkey_for(HotkeyAction::ToggleRecording, Some(shared));
        let expected = (HotkeyAction::CaptureFullScreen, HotkeyAction::ToggleRecording);
        assert_eq!(s.hotkey_conflict(), Some(expected));
    }

    #[test]
    fn the_reported_clash_is_the_earliest_pair_in_settings_order() {
        let mut s = fresh();
        let capture = s.hotkey();
        // Set out of order on purpose: the report follows the list, not the
        // order the user typed them in.
        s.set_hotkey_for(HotkeyAction::ToggleRecording, Some(capture));
        s.set_hotkey_for(HotkeyAction::CaptureFullScreen, Some(capture));
        assert_eq!(s.hotkey_conflict(), Some((HotkeyAction::Capture, HotkeyAction::CaptureFullScreen)));
    }

    #[test]
    fn one_key_under_different_modifiers_does_not_clash() {
        let mut s = fresh();
        s.set_hotkey_for(HotkeyAction::Capture, Some(chord(CTRL, 0x52)));
        s.set_hotkey_for(HotkeyAction::CaptureFullScreen, Some(chord(CTRL_SHIFT, 0x52)));
        s.set_hotkey_for(HotkeyAction::ToggleRecording, Some(chord(Modifiers::ALT, 0x52)));
        assert!(s.hotkey_conflict().is_none());
    }

    /// Every unset action shares the same "unset" value, which is not a clash.
    #[test]
    fn unset_actions_do_not_clash_with_each_other() {
        let mut s = fresh();
        for action in HotkeyAction::ALL.into_iter().skip(1) {
            s.set_hotkey_for(action, None);
        }
        assert!(s.hotkey_conflict().is_none());
    }

    #[test]
    fn names_and_clash_text_are_plain_ascii() {
        for action in HotkeyAction::ALL {
            assert!(action.label().is_ascii(), "{action:?}");
        }
        let text = conflict_message(HotkeyAction::Capture, HotkeyAction::ToggleRecording, Hotkey::default());
        assert!(text.is_ascii(), "{text}");
        assert!(text.contains("Capture region") && text.contains("Record screen"), "{text}");
    }

    /// The footer is a 640 px window less 22 px padding on each side, 596 px.
    /// A glyph is 5 px plus a 1 px gap (the last gap trimmed), so 99 fit.
    #[test]
    fn the_longest_clash_text_fits_on_the_footer_line() {
        let every = Modifiers::all();
        let mut longest_names = HotkeyAction::ALL.map(HotkeyAction::label);
        longest_names.sort_by_key(|name| std::cmp::Reverse(name.len()));
        let widest_chord = (1..=0xFE)
            .map(|vk| chord(every, vk))
            .max_by_key(|c| c.describe().len())
            .unwrap();
        let first = HotkeyAction::ALL.into_iter().find(|a| a.label() == longest_names[0]).unwrap();
        let second = HotkeyAction::ALL.into_iter().rev().find(|a| a.label() == longest_names[1]).unwrap();
        let text = conflict_message(first, second, widest_chord);
        assert!(text.len() <= 99, "{} characters: {text}", text.len());
    }

    #[test]
    fn the_switch_chord_adds_the_switch_modifier_when_it_is_free() {
        let mut s = fresh();
        s.set_hotkey_for(HotkeyAction::Capture, Some(chord(Modifiers::empty(), 0x2C)));
        assert_eq!(s.capture_switch_chord(), Some(chord(Modifiers::SHIFT, 0x2C)));
        s.switch_key = "Ctrl".into();
        assert_eq!(s.capture_switch_chord(), Some(chord(CTRL, 0x2C)));
        // Ctrl+Print Screen given to full screen stays with full screen.
        s.set_hotkey_for(HotkeyAction::CaptureFullScreen, Some(chord(CTRL, 0x2C)));
        assert_eq!(s.capture_switch_chord(), None);
        // A capture chord already holding Ctrl needs no extra chord.
        s.set_hotkey_for(HotkeyAction::Capture, Some(chord(CTRL_SHIFT, 0x50)));
        assert_eq!(s.capture_switch_chord(), None);
        // A switch key that is not a modifier cannot join a chord.
        let mut plain = fresh();
        plain.switch_key = "Key:0x51".into();
        assert_eq!(plain.capture_switch_chord(), None);
    }

    #[test]
    fn region_capture_is_the_only_required_shortcut() {
        let required: Vec<_> = HotkeyAction::ALL.into_iter().filter(|a| !a.is_optional()).collect();
        assert_eq!(required, vec![HotkeyAction::Capture]);
    }

    #[test]
    fn the_newer_actions_start_unset_and_survive_reload() {
        let mut s = fresh();
        let picks = [
            (HotkeyAction::CaptureLastRegion, chord(CTRL_SHIFT, 0x4C), "LastRegionHotkeyVirtualKey"),
            (HotkeyAction::CaptureText, chord(CTRL_SHIFT, 0x54), "TextHotkeyVirtualKey"),
            (HotkeyAction::CaptureQr, chord(CTRL_SHIFT, 0x51), "QrHotkeyVirtualKey"),
            (HotkeyAction::CaptureScroll, chord(CTRL_SHIFT, 0x53), "ScrollHotkeyVirtualKey"),
        ];
        for (action, keys, _) in picks {
            assert_eq!(s.hotkey_for(action), None, "{action:?}");
            s.set_hotkey_for(action, Some(keys));
        }
        let text = serde_json::to_string(&s).unwrap();
        let back: AppSettings = serde_json::from_str(&text).unwrap();
        for (action, keys, field) in picks {
            assert!(text.contains(field), "{field} not written");
            assert_eq!(back.hotkey_for(action), Some(keys), "{action:?}");
        }
    }
}
