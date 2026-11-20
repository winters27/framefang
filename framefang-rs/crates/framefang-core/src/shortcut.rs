/*!
Turning a [`Hotkey`] into the trigger text the XDG GlobalShortcuts portal
understands.

On Windows and macOS the stored Win32 virtual-key code goes straight to
`global-hotkey`. Wayland has no global key grab, so Frame Fang has to ask the
compositor for the shortcut through `org.freedesktop.portal.GlobalShortcuts`,
and that portal wants a string in the
[shortcuts XDG specification](https://specifications.freedesktop.org/shortcuts-spec/latest/):
modifier names and one XKB keysym name, joined with `+`, such as `Print`,
`CTRL+SHIFT+p` or `SUPER+SHIFT+s`.

Nothing here touches the system, which is why it sits in core instead of
`framefang-platform`: the tests run on every CI host, including the ones that
never see a portal. The keys named here must cover everything `vk_to_code` in
`framefang-platform::hotkey` and `vk_label` in [`crate::settings`] accept, so
any hotkey the rebind widget can produce can also be requested on Wayland. A
test below holds the second half of that.
*/

use crate::settings::{Modifiers, Hotkey};

/// Consecutive virtual-key codes whose keysym names are listed in order:
/// `(first code, names)`.
const KEY_RUNS: [(u32, &[&str]); 4] = [
    // VK_0 to VK_9. Digits are their own keysym names.
    (0x30, &["0", "1", "2", "3", "4", "5", "6", "7", "8", "9"]),
    // VK_A to VK_Z. Win32 names these after the capitals, XKB after the
    // unshifted key, hence lowercase.
    (0x41, &[
        "a", "b", "c", "d", "e", "f", "g", "h", "i",
        "j", "k", "l", "m", "n", "o", "p", "q", "r",
        "s", "t", "u", "v", "w", "x", "y", "z",
    ]),
    // VK_NUMPAD0 to VK_NUMPAD9.
    (0x60, &[
        "KP_0", "KP_1", "KP_2", "KP_3", "KP_4",
        "KP_5", "KP_6", "KP_7", "KP_8", "KP_9",
    ]),
    // VK_F1 to VK_F24.
    (0x70, &[
        "F1",  "F2",  "F3",  "F4",  "F5",  "F6",  "F7",  "F8",
        "F9",  "F10", "F11", "F12", "F13", "F14", "F15", "F16",
        "F17", "F18", "F19", "F20", "F21", "F22", "F23", "F24",
    ]),
];

/// Every other key with a keysym name, sorted by virtual-key code so it can be
/// binary searched (a test checks the order).
const NAMED_KEYS: &[(u32, &str)] = &[
    (0x08, "BackSpace"),
    (0x09, "Tab"),
    (0x0D, "Return"),
    (0x13, "Pause"),
    (0x14, "Caps_Lock"),
    (0x1B, "Escape"),
    (0x20, "space"),
    // X11 kept the old terminal names for the paging keys. "PageUp" is not
    // a keysym, and a portal that checks the string would refuse it.
    (0x21, "Prior"),
    (0x22, "Next"),
    (0x23, "End"),
    (0x24, "Home"),
    (0x25, "Left"),
    (0x26, "Up"),
    (0x27, "Right"),
    (0x28, "Down"),
    (0x2C, "Print"),
    (0x2D, "Insert"),
    (0x2E, "Delete"),
    (0x5D, "Menu"),
    // Media, browser and launcher keys go by their XFree86 names in XKB.
    (0x5F, "XF86Sleep"),
    (0x6A, "KP_Multiply"),
    (0x6B, "KP_Add"),
    (0x6D, "KP_Subtract"),
    (0x6E, "KP_Decimal"),
    (0x6F, "KP_Divide"),
    (0x90, "Num_Lock"),
    (0x91, "Scroll_Lock"),
    (0xA6, "XF86Back"),
    (0xA7, "XF86Forward"),
    (0xA8, "XF86Refresh"),
    (0xA9, "XF86Stop"),
    (0xAA, "XF86Search"),
    (0xAB, "XF86Favorites"),
    (0xAC, "XF86HomePage"),
    (0xAD, "XF86AudioMute"),
    (0xAE, "XF86AudioLowerVolume"),
    (0xAF, "XF86AudioRaiseVolume"),
    (0xB0, "XF86AudioNext"),
    (0xB1, "XF86AudioPrev"),
    (0xB2, "XF86AudioStop"),
    (0xB3, "XF86AudioPlay"),
    (0xB4, "XF86Mail"),
    (0xB5, "XF86AudioMedia"),
    (0xB6, "XF86MyComputer"),
    (0xB7, "XF86Calculator"),
    // Punctuation, named for the unshifted character on a US layout.
    (0xBA, "semicolon"),
    (0xBB, "equal"),
    (0xBC, "comma"),
    (0xBD, "minus"),
    (0xBE, "period"),
    (0xBF, "slash"),
    (0xC0, "grave"),
    (0xDB, "bracketleft"),
    (0xDC, "backslash"),
    (0xDD, "bracketright"),
    (0xDE, "apostrophe"),
    // The extra key next to left Shift on European keyboards.
    (0xE2, "less"),
];

/// Modifier names in the order the specification lists them.
const MODIFIER_NAMES: [(Modifiers, &str); 4] = [
    (Modifiers::CONTROL, "CTRL"),
    (Modifiers::ALT, "ALT"),
    (Modifiers::SHIFT, "SHIFT"),
    (Modifiers::SUPER, "SUPER"),
];

/// The XKB keysym name the shortcuts specification expects for a Win32
/// virtual-key code, or `None` when the code cannot be the key half of a
/// trigger: either we have no name for it (`settings.json` can be
/// hand-edited) or it is a bare modifier (`VK_SHIFT`, `VK_CONTROL`,
/// `VK_MENU`). `vk_to_code` does accept those, but a shortcut whose key is a
/// modifier is not a shortcut, and a portal would reject or quietly drop it,
/// so they are simply absent from the tables above.
pub fn keysym_name(vk: u32) -> Option<&'static str> {
    let from_run = KEY_RUNS.iter().find_map(|&(first, names)| {
        let offset = vk.checked_sub(first)?;
        names.get(usize::try_from(offset).ok()?).copied()
    });
    from_run.or_else(|| {
        NAMED_KEYS
            .binary_search_by_key(&vk, |&(code, _)| code)
            .ok()
            .map(|at| NAMED_KEYS[at].1)
    })
}

/// The trigger string for `hk`, or `None` when its key has no keysym name
/// (see [`keysym_name`]). Modifiers come out in the specification's order
/// (CTRL, ALT, SHIFT, SUPER), not the order [`Modifiers::describe`] uses for
/// Windows: nothing parses the string back, but compositors show it in their
/// shortcut settings, and the spec order makes it look like its neighbours
/// there. No modifiers at all is fine (`Print` is the Linux default) and
/// gives just the key name.
pub fn portal_trigger(hk: &Hotkey) -> Option<String> {
    let key_name = keysym_name(hk.virtual_key)?;
    let mut trigger = String::with_capacity(24);
    for (flag, name) in MODIFIER_NAMES {
        if hk.modifiers.contains(flag) {
            trigger.push_str(name);
            trigger.push('+');
        }
    }
    trigger.push_str(key_name);
    Some(trigger)
}

#[cfg(test)]
mod checks {
    use super::{keysym_name, portal_trigger, Hotkey, Modifiers, KEY_RUNS, NAMED_KEYS};
    use crate::settings::vk_label;

    /// `VK_SHIFT`, `VK_CONTROL` and `VK_MENU`.
    const BARE_MODIFIERS: [u32; 3] = [0x10, 0x11, 0x12];

    fn trigger(modifiers: Modifiers, virtual_key: u32) -> Option<String> {
        portal_trigger(&Hotkey { modifiers, virtual_key })
    }

    #[test]
    fn print_screen_alone_is_a_valid_trigger() {
        // The Linux and Windows default is Print Screen with no modifiers.
        assert_eq!(trigger(Modifiers::empty(), 0x2C).as_deref(), Some("Print"));
    }

    #[test]
    fn modifier_order_comes_from_the_spec_not_the_caller() {
        let every = Modifiers::SUPER | Modifiers::SHIFT | Modifiers::ALT | Modifiers::CONTROL;
        assert_eq!(trigger(every, 0x50).as_deref(), Some("CTRL+ALT+SHIFT+SUPER+p"));
        assert_eq!(
            trigger(Modifiers::SUPER | Modifiers::SHIFT, 0x53).as_deref(),
            Some("SHIFT+SUPER+s")
        );
    }

    #[test]
    fn letter_keys_use_lowercase_keysyms() {
        assert_eq!(trigger(Modifiers::CONTROL, 0x41).as_deref(), Some("CTRL+a"));
        assert_eq!(trigger(Modifiers::CONTROL, 0x5A).as_deref(), Some("CTRL+z"));
    }

    #[test]
    fn each_run_covers_its_whole_range_and_stops() {
        let edges = [
            (0x30, Some("0")),
            (0x39, Some("9")),
            (0x3A, None),
            (0x5B, None),
            (0x60, Some("KP_0")),
            (0x69, Some("KP_9")),
            (0x70, Some("F1")),
            (0x7B, Some("F12")),
            (0x87, Some("F24")),
            (0x88, None),
        ];
        for (vk, name) in edges {
            assert_eq!(keysym_name(vk), name, "vk 0x{vk:02X}");
        }
    }

    #[test]
    fn paging_keys_get_their_x11_names() {
        // The Windows labels are PageUp and PageDown, which are not keysyms.
        assert_eq!([keysym_name(0x21), keysym_name(0x22)], [Some("Prior"), Some("Next")]);
    }

    #[test]
    fn modifier_keys_cannot_be_the_trigger_key() {
        // `vk_to_code` maps these, so this is what stops a hand-edited
        // settings.json from asking the portal for Shift on its own.
        for vk in BARE_MODIFIERS {
            assert_eq!(keysym_name(vk), None, "vk 0x{vk:02X}");
            assert_eq!(trigger(Modifiers::CONTROL, vk), None, "vk 0x{vk:02X}");
        }
    }

    #[test]
    fn unnamed_codes_give_no_trigger() {
        // The caller reports an error instead of sending an invented string.
        assert_eq!(trigger(Modifiers::CONTROL, 0xFF), None);
        assert_eq!(trigger(Modifiers::empty(), 0), None);
        assert_eq!(keysym_name(u32::MAX), None);
    }

    #[test]
    fn named_key_table_is_sorted_without_duplicates() {
        for pair in NAMED_KEYS.windows(2) {
            assert!(pair[0].0 < pair[1].0, "0x{:02X} is out of order", pair[1].0);
        }
        // And no code is named twice across the tables.
        for &(first, names) in &KEY_RUNS {
            let last = first + names.len() as u32 - 1;
            assert!(
                NAMED_KEYS.iter().all(|&(code, _)| code < first || code > last),
                "run at 0x{first:02X} overlaps the named keys"
            );
        }
    }

    #[test]
    fn a_sample_of_named_keys() {
        let expected = [
            (0x08, "BackSpace"),
            (0x20, "space"),
            (0x5D, "Menu"),
            (0x6F, "KP_Divide"),
            (0x91, "Scroll_Lock"),
            (0xAD, "XF86AudioMute"),
            (0xC0, "grave"),
            (0xE2, "less"),
        ];
        for (vk, name) in expected {
            assert_eq!(keysym_name(vk), Some(name), "vk 0x{vk:02X}");
        }
    }

    #[test]
    fn every_key_the_ui_can_label_has_a_keysym() {
        // The rebind widget only offers keys `vk_label` can name. A gap here
        // would be a hotkey that works on X11 but cannot be asked for on
        // Wayland.
        let labelled = (0u32..=0xFF).filter(|vk| !BARE_MODIFIERS.contains(vk) && vk_label(*vk).is_some());
        for vk in labelled {
            assert!(keysym_name(vk).is_some(), "vk 0x{vk:02X} has a label but no keysym");
        }
    }
}
