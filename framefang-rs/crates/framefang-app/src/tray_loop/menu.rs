//! The tray's own text and settings glue: tooltip, menu row labels, hotkey
//! registration, the sound choice, and the wording about recorded audio.

use framefang_core::hotkeys::HotkeyAction;
use framefang_core::settings_store::{self, Patch};
use framefang_core::AppSettings;
use framefang_platform::hotkey::HotkeyManager;
use framefang_platform::recorder::RecordingOptions;
use framefang_platform::tray::MenuLabels;

/// What changing `s` with `change` would amount to, as a settings patch.
pub(super) fn patch_of(s: &AppSettings, change: impl FnOnce(&mut AppSettings)) -> Patch {
    let mut edited = s.clone();
    change(&mut edited);
    settings_store::diff(s, &edited)
}

/// The sound choice as `settings.json` stores it (`RecordSound`). These words
/// are on disk, so they never change.
pub(super) fn sound_name(o: RecordingOptions) -> &'static str {
    match (o.mic, o.system_audio) {
        (false, false) => "None",
        (true, false) => "Mic",
        (false, true) => "PC",
        (true, true) => "Both",
    }
}

/// The stored sound word back as recorder options. Case and surrounding
/// spaces are forgiven (people edit this file by hand); anything unknown
/// means no sound.
pub(super) fn sound_from_name(s: &str) -> RecordingOptions {
    match s.trim().to_ascii_lowercase().as_str() {
        "mic" => RecordingOptions::MIC_ONLY,
        "pc" | "system" => RecordingOptions::SYSTEM_ONLY,
        "both" => RecordingOptions::MIC_AND_SYS,
        _ => RecordingOptions::NONE,
    }
}

/// The tray icon's hover text: the app name, then the capture key as Settings
/// has it, on two lines. Built again whenever Settings is saved.
pub(super) fn tray_tooltip(s: &AppSettings) -> String {
    format!("Frame Fang\nPress {} to capture", s.hotkey().describe())
}

/// Register every configured binding and return the ones the OS turned down.
///
/// A chord another program already owns is the usual cause, and it costs the
/// user only that shortcut (the tray menu still reaches every action), so each
/// failure is logged and the call carries on.
pub(super) fn apply_hotkeys(
    hk: &mut HotkeyManager,
    s: &AppSettings,
) -> Vec<(HotkeyAction, framefang_platform::Error)> {
    let bindings = s.hotkey_bindings();
    // On Windows the capture key can also be pressed with the switch key held,
    // so the switch key may already be down when the capture key lands (the
    // overlay asks `switch_key_down`). Elsewhere the switch key is read only
    // as the drag is let go.
    let switch_chord = if cfg!(windows) { s.capture_switch_chord() } else { None };
    let has_switch_chord = switch_chord.is_some();
    let mut wanted = bindings.clone();
    wanted.extend(switch_chord.map(|chord| (HotkeyAction::Capture, chord)));
    let mut failed = hk.register_all(&wanted);
    // If another program owns the switch chord, that must not cost the capture
    // key itself: register again without it, so only real failures come back.
    if has_switch_chord && failed.iter().any(|(action, _)| *action == HotkeyAction::Capture) {
        eprintln!("the capture shortcut with the switch key is taken; releasing the switch key as the drag ends still works");
        failed = hk.register_all(&bindings);
    }
    for (action, e) in &failed {
        eprintln!("could not register the {} hotkey: {e} (the tray menu still has it)", action.label());
    }
    // While Windows hands Print Screen to the Snipping Tool, the raw-input path
    // would still hear it and open Frame Fang on top. That key stays with the
    // system until the user hands it over (Settings explains how).
    if crate::system_shortcuts::system_takes(&s.hotkey()) {
        hk.without_second_path(HotkeyAction::Capture);
    }
    failed
}

/// The text of the tray rows that quote a hotkey.
pub(super) fn menu_labels(s: &AppSettings, portal_hotkey: bool) -> MenuLabels {
    let label = |action, base| hotkey_label(s, action, base, portal_hotkey);
    MenuLabels {
        capture: label(HotkeyAction::Capture, "Capture"),
        capture_full: label(HotkeyAction::CaptureFullScreen, "Capture Full Screen"),
        record: label(HotkeyAction::ToggleRecording, "Record"),
    }
}

/// `"Record (Ctrl + Shift + R)"`, or plain `"Record"` when the action has no
/// binding. The binding rides in parentheses so people find it from the menu
/// they are already in. Through the desktop's shortcut portal the compositor
/// decides the real key (we can only ask), so every row that names one says
/// it is the desktop's choice.
fn hotkey_label(s: &AppSettings, action: HotkeyAction, base: &str, portal_hotkey: bool) -> String {
    let Some(binding) = s.hotkey_for(action) else { return base.to_owned() };
    let key = binding.describe();
    if portal_hotkey {
        format!("{base} ({key} - set by your desktop)")
    } else {
        format!("{base} ({key})")
    }
}

/// A short phrase for the audio a recording really carries.
pub(super) fn audio_label_for(o: RecordingOptions) -> &'static str {
    match (o.mic, o.system_audio) {
        (false, false) => "no audio",
        (true, false) => "microphone only",
        (false, true) => "system audio only",
        (true, true) => "microphone and system audio",
    }
}

/// A line naming the audio sources that were asked for but could not be
/// opened, to append to the "recording started" toast. Empty when the
/// recording has everything that was requested.
pub(super) fn audio_downgrade_note(requested: RecordingOptions, effective: RecordingOptions) -> String {
    let lost_mic = requested.mic && !effective.mic;
    let lost_system = requested.system_audio && !effective.system_audio;
    let detail = match (lost_mic, lost_system) {
        (false, false) => return String::new(),
        (true, true) => "no audio: neither source could be opened",
        (true, false) => "no microphone: the input device could not be opened",
        (false, true) => "no system audio: the audio server could not be reached",
    };
    format!("\n({detail})")
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL_SOUNDS: [RecordingOptions; 4] = [
        RecordingOptions::NONE,
        RecordingOptions::MIC_ONLY,
        RecordingOptions::SYSTEM_ONLY,
        RecordingOptions::MIC_AND_SYS,
    ];

    #[test]
    fn the_sound_choice_round_trips_through_settings_json() {
        for o in ALL_SOUNDS {
            assert_eq!(sound_from_name(sound_name(o)), o);
        }
        assert_eq!(sound_from_name(" both "), RecordingOptions::MIC_AND_SYS, "hand edits are forgiven");
        assert_eq!(sound_from_name("loud"), RecordingOptions::NONE);
    }

    #[test]
    fn the_stored_sound_words_are_the_ones_on_disk() {
        let words: Vec<&str> = ALL_SOUNDS.iter().map(|&o| sound_name(o)).collect();
        assert_eq!(words, ["None", "Mic", "PC", "Both"]);
        assert_eq!(sound_from_name("SYSTEM"), RecordingOptions::SYSTEM_ONLY, "the long spelling is read too");
        assert_eq!(sound_from_name(""), RecordingOptions::NONE);
    }

    #[test]
    fn each_sound_choice_has_its_own_label() {
        let labels: Vec<&str> = ALL_SOUNDS.iter().map(|&o| audio_label_for(o)).collect();
        for (i, a) in labels.iter().enumerate() {
            assert!(!a.is_empty());
            assert!(labels[i + 1..].iter().all(|b| b != a), "{a} repeats");
        }
    }

    #[test]
    fn the_downgrade_note_names_only_what_was_lost() {
        let both = RecordingOptions::MIC_AND_SYS;
        let none = RecordingOptions::NONE;
        assert_eq!(audio_downgrade_note(both, both), "");
        assert_eq!(audio_downgrade_note(none, none), "");
        assert!(audio_downgrade_note(both, none).contains("neither source"));
        assert!(audio_downgrade_note(both, RecordingOptions::SYSTEM_ONLY).contains("no microphone"));
        assert!(audio_downgrade_note(both, RecordingOptions::MIC_ONLY).contains("no system audio"));
        // Asking for less than you get is not a loss.
        assert_eq!(audio_downgrade_note(RecordingOptions::MIC_ONLY, both), "");
        let note = audio_downgrade_note(both, none);
        assert!(note.starts_with("\n(") && note.ends_with(')'));
    }

    #[test]
    fn a_patch_holds_only_what_the_change_touched() {
        let s = AppSettings::default();
        assert!(patch_of(&s, |_| {}).is_empty());
        let changed = patch_of(&s, |next| {
            next.record_sound = if next.record_sound == "Mic" { "PC" } else { "Mic" }.to_owned();
        });
        assert_eq!(changed.len(), 1);
    }

    #[test]
    fn menu_rows_quote_the_binding_and_say_when_the_desktop_chose_it() {
        let s = AppSettings::default();
        let plain = menu_labels(&s, false);
        let ported = menu_labels(&s, true);
        let key = s.hotkey().describe();
        assert_eq!(plain.capture, format!("Capture ({key})"));
        assert_eq!(ported.capture, format!("Capture ({key} - set by your desktop)"));
        assert!(plain.record.starts_with("Record"));
    }

    #[test]
    fn the_tooltip_names_the_capture_key() {
        let s = AppSettings::default();
        let tip = tray_tooltip(&s);
        assert!(tip.starts_with("Frame Fang\n"));
        assert!(tip.contains(&s.hotkey().describe()));
    }
}
