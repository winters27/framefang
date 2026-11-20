//! Keys for the rebind boxes: winit's key names to the Win32 virtual-key
//! numbers `settings.json` stores, and the modifier mask that goes with them.
//!
//! A binding is stored as a modifier mask (the Win32 `MOD_*` bits) plus a
//! virtual-key number, 0 meaning unbound. Those numbers are fixed by Windows,
//! so the tables here are facts; only how they are written down is ours.

use winit::keyboard::{KeyCode, ModifiersState, PhysicalKey};

use framefang_core::settings::vk_label;
use framefang_core::Modifiers as HkMods;

/// Keys that come in one unbroken run of virtual-key numbers, in the order
/// the run counts up. `keycode_to_vk` finds a key's place in its run and adds
/// that to the run's first number.
const LETTER_RUN: (u32, [KeyCode; 26]) = (0x41, [
    KeyCode::KeyA, KeyCode::KeyB, KeyCode::KeyC, KeyCode::KeyD, KeyCode::KeyE, KeyCode::KeyF,
    KeyCode::KeyG, KeyCode::KeyH, KeyCode::KeyI, KeyCode::KeyJ, KeyCode::KeyK, KeyCode::KeyL,
    KeyCode::KeyM, KeyCode::KeyN, KeyCode::KeyO, KeyCode::KeyP, KeyCode::KeyQ, KeyCode::KeyR,
    KeyCode::KeyS, KeyCode::KeyT, KeyCode::KeyU, KeyCode::KeyV, KeyCode::KeyW, KeyCode::KeyX,
    KeyCode::KeyY, KeyCode::KeyZ,
]);

/// The top row of digits. The numpad has its own run below: Windows numbers
/// the two apart.
const DIGIT_RUN: (u32, [KeyCode; 10]) = (0x30, [
    KeyCode::Digit0, KeyCode::Digit1, KeyCode::Digit2, KeyCode::Digit3, KeyCode::Digit4,
    KeyCode::Digit5, KeyCode::Digit6, KeyCode::Digit7, KeyCode::Digit8, KeyCode::Digit9,
]);

/// F1 to F24 run on without a gap.
const FUNCTION_RUN: (u32, [KeyCode; 24]) = (0x70, [
    KeyCode::F1, KeyCode::F2, KeyCode::F3, KeyCode::F4, KeyCode::F5, KeyCode::F6,
    KeyCode::F7, KeyCode::F8, KeyCode::F9, KeyCode::F10, KeyCode::F11, KeyCode::F12,
    KeyCode::F13, KeyCode::F14, KeyCode::F15, KeyCode::F16, KeyCode::F17, KeyCode::F18,
    KeyCode::F19, KeyCode::F20, KeyCode::F21, KeyCode::F22, KeyCode::F23, KeyCode::F24,
]);

const NUMPAD_RUN: (u32, [KeyCode; 10]) = (0x60, [
    KeyCode::Numpad0, KeyCode::Numpad1, KeyCode::Numpad2, KeyCode::Numpad3, KeyCode::Numpad4,
    KeyCode::Numpad5, KeyCode::Numpad6, KeyCode::Numpad7, KeyCode::Numpad8, KeyCode::Numpad9,
]);

/// Every other bindable key, one per line, grouped by what it is.
const SINGLES: &[(KeyCode, u32)] = &[
    // Punctuation, named for a US layout; the number follows the key's place.
    (KeyCode::Semicolon, 0xBA),
    (KeyCode::Equal, 0xBB),
    (KeyCode::Comma, 0xBC),
    (KeyCode::Minus, 0xBD),
    (KeyCode::Period, 0xBE),
    (KeyCode::Slash, 0xBF),
    (KeyCode::Backquote, 0xC0),
    (KeyCode::BracketLeft, 0xDB),
    (KeyCode::Backslash, 0xDC),
    (KeyCode::BracketRight, 0xDD),
    (KeyCode::Quote, 0xDE),
    (KeyCode::IntlBackslash, 0xE2),
    // Whitespace and editing keys (they need a modifier to be bound).
    (KeyCode::Space, 0x20),
    (KeyCode::Tab, 0x09),
    (KeyCode::Enter, 0x0D),
    (KeyCode::Backspace, 0x08),
    // The rest of the numpad.
    (KeyCode::NumpadMultiply, 0x6A),
    (KeyCode::NumpadAdd, 0x6B),
    (KeyCode::NumpadSubtract, 0x6D),
    (KeyCode::NumpadDecimal, 0x6E),
    (KeyCode::NumpadDivide, 0x6F),
    (KeyCode::NumpadEnter, 0x0D),
    // Menu and sleep.
    (KeyCode::ContextMenu, 0x5D),
    (KeyCode::Sleep, 0x5F),
    // Browser keys.
    (KeyCode::BrowserBack, 0xA6),
    (KeyCode::BrowserForward, 0xA7),
    (KeyCode::BrowserRefresh, 0xA8),
    (KeyCode::BrowserStop, 0xA9),
    (KeyCode::BrowserSearch, 0xAA),
    (KeyCode::BrowserFavorites, 0xAB),
    (KeyCode::BrowserHome, 0xAC),
    // Volume and media.
    (KeyCode::AudioVolumeMute, 0xAD),
    (KeyCode::AudioVolumeDown, 0xAE),
    (KeyCode::AudioVolumeUp, 0xAF),
    (KeyCode::MediaTrackNext, 0xB0),
    (KeyCode::MediaTrackPrevious, 0xB1),
    (KeyCode::MediaStop, 0xB2),
    (KeyCode::MediaPlayPause, 0xB3),
    // Launchers.
    (KeyCode::LaunchMail, 0xB4),
    (KeyCode::MediaSelect, 0xB5),
    (KeyCode::LaunchApp1, 0xB6),
    (KeyCode::LaunchApp2, 0xB7),
    // The block above the arrows, and the arrows.
    (KeyCode::PrintScreen, 0x2C),
    (KeyCode::ScrollLock, 0x91),
    (KeyCode::Pause, 0x13),
    (KeyCode::Insert, 0x2D),
    (KeyCode::Delete, 0x2E),
    (KeyCode::Home, 0x24),
    (KeyCode::End, 0x23),
    (KeyCode::PageUp, 0x21),
    (KeyCode::PageDown, 0x22),
    (KeyCode::ArrowLeft, 0x25),
    (KeyCode::ArrowUp, 0x26),
    (KeyCode::ArrowRight, 0x27),
    (KeyCode::ArrowDown, 0x28),
];

/// Where `code` falls in `run`, as the virtual-key number it counts to.
fn in_run<const N: usize>(run: &(u32, [KeyCode; N]), code: KeyCode) -> Option<u32> {
    run.1.iter().position(|k| *k == code).map(|i| run.0 + i as u32)
}

/// The virtual-key number a winit key is stored as, for the keys a hotkey
/// may use (every entry of `framefang_core::settings::KEYS` except Esc, which
/// cancels the rebind, and Caps Lock). A key winit can't name is read from
/// its scancode instead (`native_vk`).
pub(super) fn keycode_to_vk(code: KeyCode) -> Option<u32> {
    in_run(&LETTER_RUN, code)
        .or_else(|| in_run(&DIGIT_RUN, code))
        .or_else(|| in_run(&FUNCTION_RUN, code))
        .or_else(|| in_run(&NUMPAD_RUN, code))
        .or_else(|| SINGLES.iter().find(|(k, _)| *k == code).map(|&(_, vk)| vk))
}

/// A bare modifier or lock key. A rebind box skips these, because a hotkey of
/// "Ctrl" alone would fire on every press of it.
pub(super) fn is_modifier_code(code: KeyCode) -> bool {
    use KeyCode::*;
    matches!(
        code,
        ControlLeft | ControlRight | ShiftLeft | ShiftRight | AltLeft | AltRight
            | SuperLeft | SuperRight | Meta | Hyper | Fn | FnLock | CapsLock | NumLock
    )
}

/// The held modifiers as `framefang-core`'s flag set, whose bits are the Win32
/// `MOD_*` values `settings.json` stores.
pub(super) fn mods_state_to_hk(state: ModifiersState) -> HkMods {
    [
        (state.control_key(), HkMods::CONTROL),
        (state.shift_key(), HkMods::SHIFT),
        (state.alt_key(), HkMods::ALT),
        (state.super_key(), HkMods::SUPER),
    ]
    .into_iter()
    .filter(|(held, _)| *held)
    .fold(HkMods::empty(), |acc, (_, flag)| acc | flag)
}

/// What the listening box says when a typing key was pressed without a
/// modifier that stops it typing: what to hold, in one instruction.
pub(super) fn typing_key_hint(vk: u32) -> String {
    let key = vk_label(vk).map(str::to_owned).unwrap_or_else(|| format!("(0x{vk:02X})"));
    let hold = if cfg!(target_os = "macos") { "Cmd or Ctrl" } else { "Ctrl, Alt or Win" };
    format!("Hold {hold} with {key}")
}

/// The Win32 virtual key of a key winit couldn't name, from the scancode
/// Windows reported for it. `None` elsewhere, or when Windows has no VK for
/// that scancode either.
pub(super) fn native_vk(key: PhysicalKey) -> Option<u32> {
    #[cfg(windows)]
    {
        use winit::keyboard::NativeKeyCode;
        use windows::Win32::UI::Input::KeyboardAndMouse::{MapVirtualKeyW, MAPVK_VSC_TO_VK_EX};
        if let PhysicalKey::Unidentified(NativeKeyCode::Windows(scancode)) = key {
            let vk = unsafe { MapVirtualKeyW(scancode as u32, MAPVK_VSC_TO_VK_EX) };
            // Esc stays the way out of the rebind.
            return (vk != 0 && vk != 0x1B).then_some(vk);
        }
        None
    }
    #[cfg(not(windows))]
    {
        let _ = key;
        None
    }
}

/// The virtual-key number of any key, modifiers included (told apart by
/// side): for binding a hold key, and for the capture window's held keys off
/// Windows.
pub(crate) fn vk_of(key: PhysicalKey) -> Option<u32> {
    if let PhysicalKey::Code(code) = key {
        let by_side = match code {
            KeyCode::ShiftLeft => Some(0xA0),
            KeyCode::ShiftRight => Some(0xA1),
            KeyCode::ControlLeft => Some(0xA2),
            KeyCode::ControlRight => Some(0xA3),
            KeyCode::AltLeft => Some(0xA4),
            KeyCode::AltRight => Some(0xA5),
            KeyCode::SuperLeft => Some(0x5B),
            KeyCode::SuperRight => Some(0x5C),
            KeyCode::Escape => Some(0x1B),
            _ => keycode_to_vk(code),
        };
        if by_side.is_some() { return by_side; }
    }
    native_vk(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runs_count_up_from_their_first_key() {
        assert_eq!(keycode_to_vk(KeyCode::KeyA), Some(0x41));
        assert_eq!(keycode_to_vk(KeyCode::KeyP), Some(0x50));
        assert_eq!(keycode_to_vk(KeyCode::KeyZ), Some(0x5A));
        assert_eq!(keycode_to_vk(KeyCode::Digit0), Some(0x30));
        assert_eq!(keycode_to_vk(KeyCode::Digit9), Some(0x39));
        assert_eq!(keycode_to_vk(KeyCode::F1), Some(0x70));
        assert_eq!(keycode_to_vk(KeyCode::F12), Some(0x7B));
        assert_eq!(keycode_to_vk(KeyCode::F13), Some(0x7C));
        assert_eq!(keycode_to_vk(KeyCode::F24), Some(0x87));
        assert_eq!(keycode_to_vk(KeyCode::Numpad0), Some(0x60));
        assert_eq!(keycode_to_vk(KeyCode::Numpad9), Some(0x69));
    }

    #[test]
    fn single_keys_keep_their_windows_numbers() {
        for (code, vk) in [
            (KeyCode::PrintScreen, 0x2C), (KeyCode::ScrollLock, 0x91), (KeyCode::Pause, 0x13),
            (KeyCode::Insert, 0x2D), (KeyCode::Delete, 0x2E), (KeyCode::Home, 0x24),
            (KeyCode::End, 0x23), (KeyCode::PageUp, 0x21), (KeyCode::PageDown, 0x22),
            (KeyCode::ArrowLeft, 0x25), (KeyCode::ArrowUp, 0x26), (KeyCode::ArrowRight, 0x27),
            (KeyCode::ArrowDown, 0x28), (KeyCode::Space, 0x20), (KeyCode::Tab, 0x09),
            (KeyCode::Enter, 0x0D), (KeyCode::NumpadEnter, 0x0D), (KeyCode::Backspace, 0x08),
            (KeyCode::Semicolon, 0xBA), (KeyCode::IntlBackslash, 0xE2), (KeyCode::Quote, 0xDE),
            (KeyCode::NumpadMultiply, 0x6A), (KeyCode::NumpadDivide, 0x6F),
            (KeyCode::AudioVolumeUp, 0xAF), (KeyCode::LaunchApp2, 0xB7),
            (KeyCode::ContextMenu, 0x5D), (KeyCode::Sleep, 0x5F),
        ] {
            assert_eq!(keycode_to_vk(code), Some(vk), "{code:?}");
        }
    }

    #[test]
    fn keys_with_no_number_stay_unmapped() {
        for code in [KeyCode::Escape, KeyCode::CapsLock, KeyCode::ShiftLeft, KeyCode::Fn, KeyCode::NumLock] {
            assert_eq!(keycode_to_vk(code), None, "{code:?}");
        }
    }

    #[test]
    fn no_two_tables_claim_the_same_key_twice() {
        let mut seen = std::collections::HashSet::new();
        let runs = LETTER_RUN.1.iter().chain(&DIGIT_RUN.1).chain(&FUNCTION_RUN.1).chain(&NUMPAD_RUN.1);
        for code in runs.chain(SINGLES.iter().map(|(k, _)| k)) {
            assert!(seen.insert(*code), "{code:?} is listed twice");
        }
    }

    #[test]
    fn bare_modifiers_and_locks_are_not_bindable_keys() {
        assert!(is_modifier_code(KeyCode::ControlRight));
        assert!(is_modifier_code(KeyCode::SuperLeft));
        assert!(is_modifier_code(KeyCode::CapsLock));
        assert!(!is_modifier_code(KeyCode::KeyA));
        assert!(!is_modifier_code(KeyCode::PrintScreen));
    }

    #[test]
    fn held_modifiers_become_the_win32_mask() {
        assert!(mods_state_to_hk(ModifiersState::empty()).is_empty());
        let both = ModifiersState::CONTROL | ModifiersState::SHIFT;
        assert_eq!(mods_state_to_hk(both), HkMods::CONTROL | HkMods::SHIFT);
        let all = both | ModifiersState::ALT | ModifiersState::SUPER;
        let want = HkMods::CONTROL | HkMods::SHIFT | HkMods::ALT | HkMods::SUPER;
        assert_eq!(mods_state_to_hk(all), want);
    }

    #[test]
    fn sides_and_escape_have_numbers_for_hold_keys() {
        assert_eq!(vk_of(PhysicalKey::Code(KeyCode::ShiftLeft)), Some(0xA0));
        assert_eq!(vk_of(PhysicalKey::Code(KeyCode::ControlRight)), Some(0xA3));
        assert_eq!(vk_of(PhysicalKey::Code(KeyCode::SuperRight)), Some(0x5C));
        assert_eq!(vk_of(PhysicalKey::Code(KeyCode::Escape)), Some(0x1B));
        assert_eq!(vk_of(PhysicalKey::Code(KeyCode::KeyQ)), Some(0x51));
        assert_eq!(vk_of(PhysicalKey::Code(KeyCode::CapsLock)), None);
    }

    #[test]
    fn the_typing_hint_names_the_key() {
        let hint = typing_key_hint(0x41);
        assert!(hint.starts_with("Hold ") && hint.ends_with(" with A"), "{hint}");
        assert!(typing_key_hint(0xFE).ends_with("(0xFE)"));
    }
}
