//! Frame Fang: a screenshot, annotation and screen recording tool that lives
//! in the system tray.
//!
//! This file only decides whether this process gets to be the running copy.
//! Everything after that (the tray icon, the hotkeys, the capture window and
//! the dialogs) is driven by `tray_loop::run`.
//!
//! Release builds on Windows use the GUI subsystem, so no console window
//! opens next to the tray icon.

#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod about_form;
mod autostart;
mod backdrop;
mod bitmap_font;
mod brand_icon;
mod chrome;
mod convert_image_form;
mod convert_video_form;
mod dialog;
mod editor;
mod ffmpeg_fetch;
mod firstrun_form;
mod frame;
mod gif;
mod hud;
mod icons;
mod install_source;
mod keycap;
mod page_map;
mod painter;
mod palette;
mod pick;
mod pin;
mod read_text;
mod recent;
mod recording_indicator;
mod reveal;
mod scroll;
mod scroll_hud;
mod scroll_probe;
mod self_updater;
mod settings_form;
mod stitch;
mod system_shortcuts;
mod toast;
#[cfg(target_os = "windows")]
mod toast_identity;
mod tray_loop;
mod tray_menu;
mod updates_form;
mod window_shape;

use std::time::Duration;

use anyhow::Result;
use framefang_platform::instance::{self, Instance};

/// The longest a copy started by the self-updater waits for the copy it
/// replaces to give up the single-instance lock. The old copy quits right
/// after starting us, so the wait is normally a few milliseconds; the limit
/// only stops a hung old copy from keeping us waiting for ever.
const UPDATE_HANDOFF_WAIT: Duration = Duration::from_secs(10);

/// What the person asked for on the command line.
#[derive(Clone, Copy, PartialEq, Eq)]
enum LaunchRequest {
    /// Plain launch.
    Default,
    /// `framefang --settings`: show the Settings dialog.
    Settings,
}

impl LaunchRequest {
    fn from_args() -> Self {
        if std::env::args().skip(1).any(|arg| arg == "--settings") {
            LaunchRequest::Settings
        } else {
            LaunchRequest::Default
        }
    }
}

/// Hands this launch's request to the copy that is already running, which
/// watches for these requests, and says on stderr what happened.
fn forward_to_running_copy(request: LaunchRequest) {
    match request {
        LaunchRequest::Settings => {
            instance::request_settings();
            eprintln!("Frame Fang is running already; its Settings window will open.");
        }
        LaunchRequest::Default => {
            instance::request_capture();
            eprintln!(
                "Frame Fang is running already; it will start a capture. \
                 Its tray icon has the full menu."
            );
        }
    }
}

fn main() -> Result<()> {
    // A Windows self-update renames the old executable to `<exe>.old`, which
    // stays locked until the old process is gone. By now it is, so remove
    // it. Does nothing on macOS and Linux.
    self_updater::cleanup_stale_old_binary();

    let request = LaunchRequest::from_args();

    // Only one copy runs per login session: two would mean two tray icons,
    // a hotkey the system refuses the second time, and two recorders
    // writing to the same file names. A copy started by the self-updater is
    // the exception, since its parent is still on the way out, so it waits.
    let handoff_wait = if std::env::var_os(self_updater::RELAUNCH_ENV).is_some() {
        UPDATE_HANDOFF_WAIT
    } else {
        Duration::ZERO
    };

    // The guard holds the lock until the process ends.
    let _single_instance = match instance::acquire_waiting(handoff_wait) {
        Instance::Primary(guard) => Some(guard),
        Instance::AlreadyRunning => {
            // A second launch should still do something useful, so pass
            // the request along instead of quitting silently.
            forward_to_running_copy(request);
            return Ok(());
        }
        // There is no per-user folder to hold the lock. Running without it
        // risks a second tray icon, which beats not starting at all.
        Instance::Unsupported => None,
    };

    // A recorder pid left on disk means an earlier run ended without its
    // cleanup (a crash, a kill, a power cut) and its ffmpeg may still be
    // going with nothing to stop it. With the lock held, no other running
    // copy can own that recorder, so stopping it here is safe.
    if let Some(pid) = framefang_platform::reap_orphaned_recorder() {
        eprintln!("An earlier run left a screen recording going (pid {pid}); it has been stopped.");
    }

    // Taking the lock clears old requests, so a Settings request for this
    // copy is filed only now, for the tray loop to pick up.
    if request == LaunchRequest::Settings {
        instance::request_settings();
    }

    tray_loop::run()
}
