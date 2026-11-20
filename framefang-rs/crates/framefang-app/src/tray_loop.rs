//! Frame Fang's life in the system tray.
//!
//! `run` gets everything ready that does not need the event loop (the
//! settings, the global shortcuts), then hands control to winit. From then
//! on `app::TrayApp` owns the tray icon, the windows and the recorder. The
//! tray and hotkey libraries deliver their events on channels that need the
//! main thread's event loop to keep turning, so the app polls them on a
//! steady tick rather than blocking on them.
//!
//! The helpers that do not need the app's state live beside it:
//! `desktop` (notifications, opening links, Windows shell popups), `files`
//! (where captures and recordings go), `gif_jobs`, `media` (ffmpeg) and
//! `menu` (tray wording, hotkey registration, sound names).

use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use chrono::Local;
use framefang_core::hotkeys::HotkeyAction;
use framefang_core::region::{
    indicator_placement, record_rect_from_selection, CaptureRect, DesktopBounds,
};
use framefang_core::AppSettings;
use framefang_platform::{
    capture::capture_all_screens,
    hotkey::HotkeyManager,
    recorder::Recorder,
    tray::{Tray, TrayEvent},
};

use crate::about_form::{AboutOutcome, AboutView};
use crate::convert_image_form::{ConvertImageOutcome, ConvertImageView};
use crate::convert_video_form::{ConvertVideoOutcome, ConvertVideoView};
use crate::editor::{ChosenRegion, Overlay, OverlayOutcome, VideoCommit};
use crate::firstrun_form::{FirstRunOutcome, FirstRunView};
use crate::pin::PinView;
use crate::recording_indicator::RecordingIndicator;
use crate::self_updater;
use crate::settings_form::{SettingsOutcome, SettingsView};
use crate::updates_form::{UpdatesOutcome, UpdatesView};
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::WindowId;

mod desktop;
mod files;
mod gif_jobs;
mod media;
mod menu;

pub(crate) use self::media::extract_frame_at;
use self::desktop::*;
use self::files::*;
use self::gif_jobs::{
    gif_work_dir, gif_worker, keep_gif_mp4, sweep_gif_work, GifJob, GifPlan, GifResult,
};
use self::media::{annotated_output_path, extract_first_frame, probe_duration, run_annotation_burn};
use self::menu::*;

mod app;

use self::app::{Startup, TrayApp};

pub fn run() -> Result<()> {
    // settings.json is first written when something is saved, so its
    // absence now is the only sign that Frame Fang has never run for this
    // person. This has to be checked before anything can save. With no
    // config folder at all (no HOME) it counts as not a first run: missing
    // the welcome once is better than seeing it on every launch.
    let first_run = AppSettings::settings_path().is_some_and(|p| !p.exists());

    let (settings, settings_error) = AppSettings::load_reporting();
    crate::palette::set(crate::palette::Scheme::parse(&settings.theme));
    framefang_platform::recorder::set_chosen_ffmpeg(settings.chosen_ffmpeg());
    if let Some(e) = &settings_error {
        eprintln!("{e}");
        notify("Frame Fang: settings file had a mistake", e, true);
    }
    // "Start with system" on, but the sign-in entry is gone (removed from the
    // system's startup list, or the setting was edited by hand): put it back.
    if let Err(e) = crate::autostart::ensure(settings.start_with_windows) {
        eprintln!("start with system: {e}");
    }

    // A Wayland session stays one until the person logs out, so this is
    // asked once and passed along; both the start-up notice and the tray's
    // Capture row depend on it.
    let wayland = framefang_platform::session::is_wayland();

    let (hotkeys, hotkey_error) = start_hotkeys(&settings);
    // Presses come either from a key grab or from the desktop's
    // global-shortcuts portal. Only through the portal can the desktop pick
    // a different key than the one asked for, which the wording must allow.
    let portal_hotkey = hotkeys.as_ref().is_some_and(|hk| hk.backend().is_portal());
    log_how_to_capture(&settings, hotkey_error.is_some(), portal_hotkey);

    let event_loop = EventLoop::new().map_err(|e| anyhow!("event loop: {e}"))?;
    event_loop.set_control_flow(ControlFlow::Wait);

    let mut app = TrayApp::new(Startup { settings, hotkeys, hotkey_error, portal_hotkey, wayland, first_run });
    app.sweep_old_gif_work();
    event_loop.run_app(&mut app).map_err(|e| anyhow!("event loop stopped with an error: {e}"))?;
    app.finish();
    Ok(())
}

/// Sets up the global shortcuts, and says why the capture shortcut won't
/// work if it won't.
///
/// None of this stops the app: without shortcuts the tray menu still
/// captures, and the problem can be fixed and the shortcut set again from
/// Settings. The reason is kept rather than only logged, because on
/// Wayland a dead shortcut is the likeliest thing to go wrong and a tray
/// app's stderr is read by nobody; `ensure_tray` shows it once.
fn start_hotkeys(settings: &AppSettings) -> (Option<HotkeyManager>, Option<String>) {
    let mut hk = match HotkeyManager::new() {
        Ok(hk) => hk,
        Err(e) => {
            eprintln!("Global shortcuts unavailable ({e}); capture from the tray menu.");
            return (None, Some(e.to_string()));
        }
    };
    // Only the main capture shortcut is worth a notice. The optional ones
    // start unbound, and one the desktop already owns costs only that
    // shortcut (`apply_hotkeys` logs it).
    let mut problem = apply_hotkeys(&mut hk, settings).into_iter()
        .filter(|(action, _)| *action == HotkeyAction::Capture)
        .map(|(_, e)| e.to_string())
        .last();
    // Registering succeeds even when the system grabs the key first, so
    // this check is the only way to learn its presses will never come.
    if problem.is_none() && crate::system_shortcuts::system_takes(&settings.hotkey()) {
        problem = Some(format!(
            "Windows is using Print Screen for its own capture. In Settings > {}, \
             turn off \u{201C}{}\u{201D}. Framefang's Settings has a button that opens it.",
            crate::system_shortcuts::windows_page_path(),
            crate::system_shortcuts::switch_name()));
    }
    (Some(hk), problem)
}

/// One line on stderr saying how to take a capture now that the app is up.
fn log_how_to_capture(settings: &AppSettings, shortcut_broken: bool, portal_hotkey: bool) {
    let key = settings.hotkey().describe();
    if shortcut_broken {
        eprintln!("Frame Fang is running. Its capture shortcut isn't available, so capture from the tray menu.");
    } else if portal_hotkey {
        eprintln!("Frame Fang is running. This is a Wayland session, so {key} was asked of the desktop's \
                   global-shortcuts portal; the desktop has the final say on the key and lists it in its \
                   keyboard settings.");
    } else {
        eprintln!("Frame Fang is running. Press {key} or use the tray menu to capture.");
    }
}
