//! System-wide hotkeys.
//!
//! [`HotkeyManager`] picks one of two backends when it is created:
//!
//! * **Grab**: the `global-hotkey` crate, which asks the window system for a
//!   key grab (`RegisterHotKey` on Windows, `XGrabKey` on X11,
//!   `RegisterEventHotKey` on macOS). On Windows the same keys are also heard
//!   through raw keyboard input (the `raw_keys` module), for apps that eat
//!   key presses before the hotkey sees them.
//! * **Portal**: `org.freedesktop.portal.GlobalShortcuts`, through
//!   [`crate::hotkey_portal`]. Linux Wayland sessions only.
//!
//! Wayland never falls back to the grab. `global-hotkey` 0.7 has no Wayland
//! support, and its X11 grab is answered by XWayland: registration succeeds
//! and the key then never arrives. A hotkey that quietly never fires looks
//! like a broken app, so on Wayland the portal error is reported as is and the
//! tray menu stays the way in.
//!
//! The API is the same for both: create the manager, hand
//! [`register_all`](HotkeyManager::register_all) the full set of bindings
//! (again whenever they change), and call
//! [`drain_pressed`](HotkeyManager::drain_pressed) from the event loop.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use framefang_core::{hotkeys::HotkeyAction, settings::{Hotkey, Modifiers}};
use global_hotkey::hotkey::{Code, HotKey, Modifiers as GrabModifiers};
#[cfg(target_os = "linux")]
use crate::hotkey_portal::PortalHotkeys;
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};

use crate::{Error, Result};

/// The mechanism delivering presses. The app shows it because the two fail
/// for unrelated reasons and are fixed in unrelated places.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotkeyBackend {
    /// A key grab from the window system: the chord we register is the chord
    /// that fires.
    Grab,
    /// A desktop portal request: the compositor chooses the final binding and
    /// may show it in its own keyboard settings.
    Portal,
}

impl HotkeyBackend {
    pub fn is_portal(self) -> bool {
        self == HotkeyBackend::Portal
    }
}

/// Two presses of the same action closer together than this count as one.
/// It covers a held key repeating and, on Windows, one press arriving through
/// both the hotkey and raw input.
const ONE_PRESS: Duration = Duration::from_millis(400);

pub struct HotkeyManager {
    engine: Engine,
    /// Shared by every path a press can come in on.
    presses: Mutex<PressGate>,
}

/// The backend actually in use.
enum Engine {
    Grab(Grab),
    #[cfg(target_os = "linux")]
    Portal(PortalHotkeys),
}

impl Engine {
    /// The portal on a Linux Wayland session, the key grab everywhere else.
    fn for_this_session() -> Result<Self> {
        #[cfg(target_os = "linux")]
        if crate::is_wayland() {
            return PortalHotkeys::new().map(Engine::Portal);
        }
        Grab::start().map(Engine::Grab)
    }
}

impl HotkeyManager {
    pub fn new() -> Result<Self> {
        let engine = Engine::for_this_session()?;
        Ok(HotkeyManager { engine, presses: Mutex::new(PressGate::default()) })
    }

    /// Which backend this manager is using.
    pub fn backend(&self) -> HotkeyBackend {
        match self.engine {
            Engine::Grab(_) => HotkeyBackend::Grab,
            #[cfg(target_os = "linux")]
            Engine::Portal(_) => HotkeyBackend::Portal,
        }
    }

    /// Keep `action`'s hotkey but stop listening for it through raw input.
    ///
    /// For keys Windows hands to something else first (Print Screen goes to
    /// the Snipping Tool): raw input would still hear the press and both
    /// would open. Does nothing where there is no raw input.
    pub fn without_second_path(&mut self, action: HotkeyAction) {
        // Windows has only the grab backend, so this match has one arm.
        #[cfg(target_os = "windows")]
        match &self.engine {
            Engine::Grab(grab) => grab.stop_raw_for(action),
        }
        #[cfg(not(target_os = "windows"))]
        let _ = action;
    }

    /// Replace the registered hotkeys with exactly `bindings`.
    ///
    /// Each binding stands alone: one the OS refuses (the desktop already
    /// owns it) or whose key cannot be expressed here is left out and
    /// returned in the list, and the others still work. Losing one shortcut
    /// must not cost the user the rest, so the caller logs the list and
    /// carries on.
    ///
    /// For the portal an empty list only means the request was accepted; the
    /// compositor decides whether the keys go live. See
    /// [`crate::hotkey_portal`].
    pub fn register_all(&mut self, bindings: &[(HotkeyAction, Hotkey)])
        -> Vec<(HotkeyAction, Error)>
    {
        match self.engine {
            Engine::Grab(ref mut grab) => grab.replace(bindings),
            #[cfg(target_os = "linux")]
            Engine::Portal(ref mut portal) => portal.register_all(bindings),
        }
    }

    /// Release every hotkey. Settings does this while it is open, so that
    /// the key-capture field receives the keys instead of a capture starting.
    pub fn unregister_all(&mut self) {
        match self.engine {
            Engine::Grab(ref mut grab) => grab.release_all(),
            #[cfg(target_os = "linux")]
            Engine::Portal(ref mut portal) => portal.unregister_all(),
        }
    }

    /// Collect the presses that arrived since the last call, as the actions
    /// they trigger, in arrival order.
    ///
    /// Each action appears at most once per call, since a held key can queue
    /// several presses between two event-loop passes and one press must mean
    /// one capture.
    pub fn drain_pressed(&self) -> Vec<HotkeyAction> {
        let grab = match self.engine {
            Engine::Grab(ref grab) => grab,
            #[cfg(target_os = "linux")]
            Engine::Portal(ref portal) => return portal.drain_pressed(),
        };
        let now = Instant::now();
        let mut new_presses = Vec::new();
        grab.for_each_press(|action| {
            // Recorded with the gate even when it is a repeat in this batch,
            // so a held key keeps renewing its timestamp.
            if self.admit(action, now) && !new_presses.contains(&action) {
                new_presses.push(action);
            }
        });
        new_presses
    }

    fn admit(&self, action: HotkeyAction, now: Instant) -> bool {
        match self.presses.lock() {
            Ok(mut gate) => gate.admit(action, now),
            // A poisoned lock should not swallow hotkeys.
            Err(_) => true,
        }
    }
}

/// Remembers when each action last fired, to fold repeats into one press.
#[derive(Default)]
struct PressGate {
    /// At most one entry per action, so a short list beats a map.
    last_seen: Vec<(HotkeyAction, Instant)>,
}

impl PressGate {
    /// True if a press of `action` at `now` is a new press. The time is
    /// stored either way, so a key held down stays one press for as long as
    /// its repeats keep coming.
    fn admit(&mut self, action: HotkeyAction, now: Instant) -> bool {
        let Some(slot) = self.last_seen.iter_mut().find(|(a, _)| *a == action) else {
            self.last_seen.push((action, now));
            return true;
        };
        let previous = std::mem::replace(&mut slot.1, now);
        now.duration_since(previous) >= ONE_PRESS
    }
}

// ---- the key-grab backend ----

struct Grab {
    manager: GlobalHotKeyManager,
    /// Every chord the OS has accepted, with the action it fires. Three
    /// entries at most, so a linear search per event is the cheap option.
    held: Vec<(HotkeyAction, HotKey)>,
    /// Windows: the raw-input listener, or `None` if it could not start.
    #[cfg(target_os = "windows")]
    raw: Option<RawInput>,
}

#[cfg(target_os = "windows")]
struct RawInput {
    /// The chords raw input should report. Shared with its thread.
    listening: crate::raw_keys::Bindings,
    presses: std::sync::mpsc::Receiver<HotkeyAction>,
}

impl Grab {
    fn start() -> Result<Self> {
        let manager = GlobalHotKeyManager::new().map_err(|e| Error::Hotkey(e.to_string()))?;
        Ok(Grab {
            manager,
            held: Vec::new(),
            #[cfg(target_os = "windows")]
            raw: crate::raw_keys::start()
                .map(|(listening, presses)| RawInput { listening, presses }),
        })
    }

    /// Whether a key `global-hotkey` cannot name can still be heard.
    fn hears_unnamed_keys(&self) -> bool {
        #[cfg(target_os = "windows")]
        return self.raw.is_some();
        #[cfg(not(target_os = "windows"))]
        return false;
    }

    fn replace(&mut self, bindings: &[(HotkeyAction, Hotkey)]) -> Vec<(HotkeyAction, Error)> {
        self.unregister_held();

        let mut refused = Vec::new();
        // What raw input should listen for: exactly what took effect, so a
        // chord some other program owns stays theirs.
        let mut live = Vec::with_capacity(bindings.len());
        for &(action, hotkey) in bindings {
            // Keys with no `Code` (Menu, browser and launch keys) cannot be
            // grabbed, but raw input hears every key, so they become raw-only
            // bindings instead of failures. Raw input only listens, so the
            // key still does its usual thing as well.
            if self.hears_unnamed_keys() && vk_to_code(hotkey.virtual_key).is_none() {
                live.push((action, hotkey));
                continue;
            }
            match register_chord(&self.manager, hotkey) {
                Ok(grabbed) => {
                    self.held.push((action, grabbed));
                    live.push((action, hotkey));
                }
                Err(e) => refused.push((action, e)),
            }
        }
        self.set_raw_listening(live);
        refused
    }

    fn release_all(&mut self) {
        self.unregister_held();
        self.set_raw_listening(Vec::new());
    }

    fn unregister_held(&mut self) {
        for (_, grabbed) in self.held.drain(..) {
            let _ = self.manager.unregister(grabbed);
        }
    }

    #[cfg(target_os = "windows")]
    fn set_raw_listening(&self, chords: Vec<(HotkeyAction, Hotkey)>) {
        if let Some(raw) = &self.raw {
            if let Ok(mut listening) = raw.listening.lock() {
                *listening = chords;
            }
        }
    }

    #[cfg(not(target_os = "windows"))]
    fn set_raw_listening(&self, _chords: Vec<(HotkeyAction, Hotkey)>) {}

    #[cfg(target_os = "windows")]
    fn stop_raw_for(&self, action: HotkeyAction) {
        if let Some(raw) = &self.raw {
            if let Ok(mut listening) = raw.listening.lock() {
                listening.retain(|(a, _)| *a != action);
            }
        }
    }

    /// Feed `on_press` every press waiting since the last drain: first the
    /// hotkey events, then (on Windows) what raw input heard.
    fn for_each_press(&self, mut on_press: impl FnMut(HotkeyAction)) {
        while let Ok(event) = GlobalHotKeyEvent::receiver().try_recv() {
            if event.state != HotKeyState::Pressed {
                continue;
            }
            if let Some(&(action, _)) = self.held.iter().find(|(_, h)| h.id() == event.id) {
                on_press(action);
            }
        }
        #[cfg(target_os = "windows")]
        if let Some(raw) = &self.raw {
            while let Ok(action) = raw.presses.try_recv() {
                on_press(action);
            }
        }
    }
}

/// Ask the window system for one chord.
fn register_chord(manager: &GlobalHotKeyManager, hotkey: Hotkey) -> Result<HotKey> {
    let code = vk_to_code(hotkey.virtual_key)
        .ok_or_else(|| Error::Hotkey(format!("unknown vk 0x{:X}", hotkey.virtual_key)))?;
    let chord = HotKey::new(Some(grab_modifiers(hotkey.modifiers)), code);
    manager.register(chord).map_err(|e| Error::Hotkey(e.to_string()))?;
    Ok(chord)
}

fn grab_modifiers(mods: Modifiers) -> GrabModifiers {
    [
        (Modifiers::ALT, GrabModifiers::ALT),
        (Modifiers::CONTROL, GrabModifiers::CONTROL),
        (Modifiers::SHIFT, GrabModifiers::SHIFT),
        (Modifiers::SUPER, GrabModifiers::SUPER),
    ]
    .into_iter()
    .filter(|(ours, _)| mods.contains(*ours))
    .fold(GrabModifiers::empty(), |acc, (_, theirs)| acc | theirs)
}

// ---- virtual-key table ----

/// F1 through F24, which sit on consecutive codes from `VK_F1` (0x70).
const FUNCTION_KEYS: [Code; 24] = [
    Code::F1, Code::F2, Code::F3, Code::F4, Code::F5, Code::F6,
    Code::F7, Code::F8, Code::F9, Code::F10, Code::F11, Code::F12,
    Code::F13, Code::F14, Code::F15, Code::F16, Code::F17, Code::F18,
    Code::F19, Code::F20, Code::F21, Code::F22, Code::F23, Code::F24,
];

/// The top-row digits, from `'0'` (0x30).
const DIGIT_KEYS: [Code; 10] = [
    Code::Digit0, Code::Digit1, Code::Digit2, Code::Digit3, Code::Digit4,
    Code::Digit5, Code::Digit6, Code::Digit7, Code::Digit8, Code::Digit9,
];

/// The keypad digits, from `VK_NUMPAD0` (0x60).
const NUMPAD_DIGIT_KEYS: [Code; 10] = [
    Code::Numpad0, Code::Numpad1, Code::Numpad2, Code::Numpad3, Code::Numpad4,
    Code::Numpad5, Code::Numpad6, Code::Numpad7, Code::Numpad8, Code::Numpad9,
];

/// The letters, from `'A'` (0x41).
const LETTER_KEYS: [Code; 26] = [
    Code::KeyA, Code::KeyB, Code::KeyC, Code::KeyD, Code::KeyE, Code::KeyF,
    Code::KeyG, Code::KeyH, Code::KeyI, Code::KeyJ, Code::KeyK, Code::KeyL,
    Code::KeyM, Code::KeyN, Code::KeyO, Code::KeyP, Code::KeyQ, Code::KeyR,
    Code::KeyS, Code::KeyT, Code::KeyU, Code::KeyV, Code::KeyW, Code::KeyX,
    Code::KeyY, Code::KeyZ,
];

/// Translate a Windows virtual-key code into the `Code` that `global-hotkey`
/// registers.
///
/// Settings store keys as Windows virtual-key codes on every platform, so
/// the same settings file means the same keys everywhere. Elsewhere
/// `global-hotkey` maps the `Code` on to the native key code itself, purely
/// by position. That is why the macOS default is Cmd+Shift+7 (`0x37`) rather
/// than Print Screen: `0x2C` lands on a Carbon key code no Apple keyboard has.
///
/// Keys missing here on purpose, which raw input carries on Windows: Menu
/// (0x5D), Sleep (0x5F), the browser and launch keys (0xA6 to 0xAC and 0xB4
/// to 0xB7) and the extra key on European keyboards (0xE2).
fn vk_to_code(vk: u32) -> Option<Code> {
    let from = |table: &[Code], first: u32| table.get((vk - first) as usize).copied();
    match vk {
        0x30..=0x39 => from(&DIGIT_KEYS, 0x30),
        0x41..=0x5A => from(&LETTER_KEYS, 0x41),
        0x60..=0x69 => from(&NUMPAD_DIGIT_KEYS, 0x60),
        0x70..=0x87 => from(&FUNCTION_KEYS, 0x70),
        _ => NAMED_KEYS.iter().find(|(code, _)| *code == vk).map(|&(_, key)| key),
    }
}

/// The keys that are not part of a numbered run, as (virtual key, `Code`)
/// pairs. Looked up only when hotkeys are registered, so a scan is plenty.
const NAMED_KEYS: [(u32, Code); 46] = [
    // Editing and navigation.
    (0x08, Code::Backspace), (0x09, Code::Tab), (0x0D, Code::Enter),
    (0x1B, Code::Escape), (0x20, Code::Space),
    (0x21, Code::PageUp), (0x22, Code::PageDown), (0x23, Code::End), (0x24, Code::Home),
    (0x25, Code::ArrowLeft), (0x26, Code::ArrowUp), (0x27, Code::ArrowRight), (0x28, Code::ArrowDown),
    (0x2D, Code::Insert), (0x2E, Code::Delete),
    // Modifiers and locks, as keys in their own right.
    (0x10, Code::ShiftLeft), (0x11, Code::ControlLeft), (0x12, Code::AltLeft),
    (0x14, Code::CapsLock), (0x90, Code::NumLock), (0x91, Code::ScrollLock),
    // System keys.
    (0x13, Code::Pause), (0x2C, Code::PrintScreen),
    // Keypad operators.
    (0x6A, Code::NumpadMultiply), (0x6B, Code::NumpadAdd), (0x6D, Code::NumpadSubtract),
    (0x6E, Code::NumpadDecimal), (0x6F, Code::NumpadDivide),
    // Punctuation, at their US layout positions.
    (0xBA, Code::Semicolon), (0xBB, Code::Equal), (0xBC, Code::Comma), (0xBD, Code::Minus),
    (0xBE, Code::Period), (0xBF, Code::Slash), (0xC0, Code::Backquote),
    (0xDB, Code::BracketLeft), (0xDC, Code::Backslash), (0xDD, Code::BracketRight), (0xDE, Code::Quote),
    // Volume and media.
    (0xAD, Code::AudioVolumeMute), (0xAE, Code::AudioVolumeDown), (0xAF, Code::AudioVolumeUp),
    (0xB0, Code::MediaTrackNext), (0xB1, Code::MediaTrackPrevious),
    (0xB2, Code::MediaStop), (0xB3, Code::MediaPlayPause),
];

#[cfg(test)]
mod tests {
    use super::*;

    /// Keys `global-hotkey` 0.7 cannot name; on Windows raw input carries
    /// them instead.
    const RAW_INPUT_ONLY: [u32; 14] =
        [0x5D, 0x5F, 0xA6, 0xA7, 0xA8, 0xA9, 0xAA, 0xAB, 0xAC, 0xB4, 0xB5, 0xB6, 0xB7, 0xE2];

    /// Every key Settings lets the user pick is either grabbable or one of
    /// the known raw-input-only keys, never both and never neither.
    #[test]
    fn every_settings_key_is_grabbable_or_raw_only() {
        for key in framefang_core::settings::KEYS {
            let grabbable = vk_to_code(key.vk).is_some();
            let raw_only = RAW_INPUT_ONLY.contains(&key.vk);
            assert!(grabbable != raw_only,
                "{} (0x{:02X}): grabbable={grabbable}, raw-only={raw_only}", key.label, key.vk);
        }
    }

    #[test]
    fn numbered_runs_line_up() {
        assert_eq!(vk_to_code(0x30), Some(Code::Digit0));
        assert_eq!(vk_to_code(0x37), Some(Code::Digit7));
        assert_eq!(vk_to_code(0x39), Some(Code::Digit9));
        assert_eq!(vk_to_code(0x41), Some(Code::KeyA));
        assert_eq!(vk_to_code(0x53), Some(Code::KeyS));
        assert_eq!(vk_to_code(0x5A), Some(Code::KeyZ));
        assert_eq!(vk_to_code(0x60), Some(Code::Numpad0));
        assert_eq!(vk_to_code(0x69), Some(Code::Numpad9));
        assert_eq!(vk_to_code(0x70), Some(Code::F1));
        assert_eq!(vk_to_code(0x7B), Some(Code::F12));
        assert_eq!(vk_to_code(0x7C), Some(Code::F13));
        assert_eq!(vk_to_code(0x87), Some(Code::F24));
    }

    #[test]
    fn gaps_between_runs_are_unmapped() {
        // 0x3A to 0x40 sit between the digits and letters; 0x6C is the
        // keypad separator; 0x88 is just past F24.
        for vk in [0x3A, 0x40, 0x5B, 0x6C, 0x88, 0x00, 0xFF, 0x1_0000] {
            assert_eq!(vk_to_code(vk), None, "0x{vk:X} should be unmapped");
        }
        for vk in RAW_INPUT_ONLY {
            assert_eq!(vk_to_code(vk), None, "0x{vk:X} is meant to be raw-only");
        }
    }

    #[test]
    fn print_screen_and_media_keys_map() {
        assert_eq!(vk_to_code(0x2C), Some(Code::PrintScreen));
        assert_eq!(vk_to_code(0x13), Some(Code::Pause));
        assert_eq!(vk_to_code(0xB3), Some(Code::MediaPlayPause));
        assert_eq!(vk_to_code(0xDE), Some(Code::Quote));
    }

    #[test]
    fn named_keys_are_unique_and_outside_the_runs() {
        let in_a_run = |vk: u32| matches!(vk, 0x30..=0x39 | 0x41..=0x5A | 0x60..=0x69 | 0x70..=0x87);
        for (i, &(vk, code)) in NAMED_KEYS.iter().enumerate() {
            assert!(!in_a_run(vk), "0x{vk:X} would be shadowed by a numbered run");
            assert_eq!(vk_to_code(vk), Some(code), "0x{vk:X}");
            assert!(NAMED_KEYS[i + 1..].iter().all(|&(other, _)| other != vk), "0x{vk:X} listed twice");
        }
    }

    #[test]
    fn modifiers_carry_over_one_for_one() {
        assert_eq!(grab_modifiers(Modifiers::empty()), GrabModifiers::empty());
        assert_eq!(grab_modifiers(Modifiers::ALT), GrabModifiers::ALT);
        assert_eq!(grab_modifiers(Modifiers::CONTROL | Modifiers::SHIFT),
            GrabModifiers::CONTROL | GrabModifiers::SHIFT);
        assert_eq!(grab_modifiers(Modifiers::all()),
            GrabModifiers::ALT | GrabModifiers::CONTROL | GrabModifiers::SHIFT | GrabModifiers::SUPER);
    }

    #[test]
    fn a_first_press_counts() {
        let mut gate = PressGate::default();
        assert!(gate.admit(HotkeyAction::Capture, Instant::now()));
    }

    #[test]
    fn a_quick_repeat_is_the_same_press() {
        let mut gate = PressGate::default();
        let t0 = Instant::now();
        assert!(gate.admit(HotkeyAction::Capture, t0));
        assert!(!gate.admit(HotkeyAction::Capture, t0 + Duration::from_millis(50)));
        assert!(!gate.admit(HotkeyAction::Capture, t0 + Duration::from_millis(399)));
    }

    #[test]
    fn a_held_key_stays_one_press() {
        // Each repeat renews the timestamp, so repeats 300 ms apart never
        // add up to a new press even long after the first.
        let mut gate = PressGate::default();
        let t0 = Instant::now();
        assert!(gate.admit(HotkeyAction::Capture, t0));
        for step in 1..10 {
            let at = t0 + Duration::from_millis(300 * step);
            assert!(!gate.admit(HotkeyAction::Capture, at), "repeat {step}");
        }
    }

    #[test]
    fn a_press_after_the_window_is_new() {
        let mut gate = PressGate::default();
        let t0 = Instant::now();
        assert!(gate.admit(HotkeyAction::Capture, t0));
        assert!(gate.admit(HotkeyAction::Capture, t0 + ONE_PRESS));
    }

    #[test]
    fn actions_are_gated_separately() {
        let mut gate = PressGate::default();
        let t0 = Instant::now();
        assert!(gate.admit(HotkeyAction::Capture, t0));
        assert!(gate.admit(HotkeyAction::ToggleRecording, t0));
        assert!(gate.admit(HotkeyAction::CaptureFullScreen, t0 + Duration::from_millis(10)));
        assert!(!gate.admit(HotkeyAction::ToggleRecording, t0 + Duration::from_millis(20)));
    }

    #[test]
    fn only_the_portal_backend_says_portal() {
        assert!(HotkeyBackend::Portal.is_portal());
        assert!(!HotkeyBackend::Grab.is_portal());
    }
}
