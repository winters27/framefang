//! The running app: one `TrayApp` owns everything Frame Fang has open (the
//! tray icon, the hotkeys, the recorder, every window) and winit hands it
//! each event.
//!
//! Its methods live in the child modules by topic. They are children so
//! that they can reach the private fields declared here:
//! - `driver`: the tray icon, the timed poll, and what the tray's rows do;
//! - `capture`: taking pictures and what becomes of them;
//! - `recording`: screen recording, GIFs and annotating clips;
//! - `prefs`: the Settings window and settings.json;
//! - `dialogs`: About, the welcome window, updates, the converters;
//! - `events`: winit's `ApplicationHandler`, routing window events.

use super::*;
use framefang_core::app_state::AppState;
use framefang_core::settings_store::{Patch, Stamp};
use framefang_core::virtual_desktop::{DesktopGeometry, MonitorRect};
use framefang_platform::recorder::RecordingOptions;

mod capture;
mod dialogs;
mod driver;
mod events;
mod prefs;
mod recording;

/// How long after start-up a capture request from a second launch is
/// ignored. Long enough to cover a login that starts Frame Fang twice (two
/// autostart entries, a launcher that retries); short enough that running
/// it again on purpose, soon after the tray icon appears, still captures.
const STARTUP_CAPTURE_GRACE: Duration = Duration::from_secs(10);

/// Windows: how long a capture started from the tray menu waits before the
/// screen is frozen. The menu itself is our own window, gone as soon as it
/// is clicked and never in a capture, so this is only for the hidden-icons
/// flyout to finish closing. (The old native menu's fade took 700 ms.)
#[cfg(target_os = "windows")]
const TRAY_MENU_SETTLE: Duration = Duration::from_millis(200);

/// Elsewhere: how long the tray menu takes to fade out.
#[cfg(not(target_os = "windows"))]
const TRAY_MENU_SETTLE: Duration = Duration::from_millis(250);

/// Waits until the tray menu (and on Windows the hidden-icons flyout, which
/// is closed first) has left the screen, so a capture started from it does
/// not catch it.
fn wait_for_tray_menu() {
    #[cfg(target_os = "windows")]
    dismiss_tray_flyouts();
    std::thread::sleep(TRAY_MENU_SETTLE);
}

/// What `run` learned before the event loop existed.
pub(super) struct Startup {
    pub(super) settings: AppSettings,
    pub(super) hotkeys: Option<HotkeyManager>,
    pub(super) hotkey_error: Option<String>,
    pub(super) portal_hotkey: bool,
    pub(super) wayland: bool,
    pub(super) first_run: bool,
}

pub(super) struct TrayApp {
    // What the app runs with.
    settings: AppSettings,
    /// What Frame Fang remembers on its own in `state.json` (the last
    /// region picked).
    app_state: AppState,
    hotkeys: Option<HotkeyManager>,
    /// Why the capture shortcut can't work, if it can't; shown once by
    /// `ensure_tray` and then cleared, so a shortcut that never fires is
    /// not left unexplained.
    hotkey_error: Option<String>,
    /// Presses arrive from the desktop's global-shortcuts portal, not a
    /// key grab. The desktop then has the last word on the binding, which
    /// the tray's Capture row has to admit.
    portal_hotkey: bool,
    /// A Wayland session: captures and recordings go through
    /// xdg-desktop-portal instead of X11.
    wayland: bool,
    recorder: Recorder,
    started_at: Instant,
    last_tick: Instant,

    // The tray icon and its menu.
    /// Made by `ensure_tray` on the loop's first pass rather than in `run`.
    /// When making it failed this stays `None`, the About window stands in
    /// for it, and closing that window quits.
    tray: Option<Tray>,
    /// `ensure_tray` has run, which tells "not made yet" from "failed".
    tray_built: bool,
    /// The app's own tray menu (Windows), while it is open.
    tray_menu: Option<crate::tray_menu::TrayMenu>,
    /// When that menu last closed: a click on the icon just after is the
    /// click that closed it, and must not open it again.
    tray_menu_closed: Option<Instant>,
    /// Where the tray icon was last clicked. A copy made from the RECENT
    /// list shows its pill there.
    tray_click_at: Option<(i32, i32)>,

    // settings.json.
    /// The file as it was last read or written here, and when it was last
    /// looked at; a hand edit is noticed within a second.
    settings_stamp: Option<Stamp>,
    settings_checked: Instant,
    /// A broken version of the file that has already been reported.
    settings_bad_reported: Option<Stamp>,
    /// Changes in use but not yet on disk, because the file did not parse
    /// (a hand edit under way) or could not be written.
    pending_patch: Option<Patch>,
    /// Writing the file keeps failing; said once until it works again.
    settings_io_failing: bool,

    // Taking pictures.
    /// A capture is being taken right now, so another must wait.
    capturing: bool,
    /// The capture window or editor, while one is open.
    overlay: Option<Overlay>,
    /// The desktop the open capture window froze, in virtual-desktop
    /// pixels. A region picked in it for recording is kept inside it, so
    /// the recorder is never asked for pixels that were not on screen, and
    /// the REC panel is placed against it.
    overlay_desktop: Option<DesktopBounds>,
    /// The eyedropper's full-screen session, while the editor has one
    /// open; the color it picks goes back to `overlay`.
    color_pick: Option<Overlay>,
    /// Pictures pinned on top of everything, each in its own window.
    pinned: Vec<PinView>,
    /// The "Copied" pill left after a copy closed the capture window,
    /// until its fade ends.
    toast: Option<crate::toast::Toast>,
    /// Each capture (a freeze, a full-screen shot, a recent picture edited)
    /// is one session, and everything it outputs is one recent item.
    session_id: u64,
    /// The windows on screen when this session's capture froze.
    session_windows: Vec<framefang_platform::windows_list::OnScreenWindow>,
    /// The app and title of the window under the picked region, for the
    /// `{app}` and `{title}` parts of file names.
    session_facts: Option<(String, String)>,
    /// The last 10 captures: the worker that keeps them and the list as
    /// it last reported it.
    recent: crate::recent::Recent,
    recent_list: std::sync::Arc<Vec<crate::recent::Entry>>,
    /// Text or QR codes being read off a picture, whether a TEXT or QR
    /// picker waits on it (and closes with the answer) rather than the
    /// editor, and whether the switch key flipped what a QR link does.
    /// Only the newest request, `read_gen`, counts.
    read_job: Option<(crate::read_text::ReadJob, bool, bool)>,
    read_gen: u64,
    /// A scrolling capture under way, and the frame shown around it.
    scroll: Option<crate::scroll::Session>,
    scroll_hud: Option<crate::scroll_hud::ScrollHud>,
    /// That scrolling capture was picked with the switch key held.
    scroll_flip: bool,

    // Recording.
    /// The REC panel with its STOP button, while recording.
    recording_view: Option<RecordingIndicator>,
    /// The sound sources the tray's Record region asked for, used once the
    /// region is confirmed. `None` for the capture window's own Record
    /// button, which records without sound like the plain Record row.
    pending_region_audio: Option<RecordingOptions>,
    /// A recording that ends as a GIF: where its MP4 goes meanwhile and
    /// what the GIF will be called. Fixed when it starts, so switching the
    /// format halfway changes nothing.
    gif_plan: Option<GifPlan>,
    /// A finished recording being turned into a GIF.
    gif_job: Option<GifJob>,
    /// The newest recording saved in this run, for "Annotate last
    /// recording"; before there is one, the newest file in the recordings
    /// folder is used.
    last_recording: Option<PathBuf>,
    /// While the editor is annotating a clip, that clip. The editor's
    /// `AcceptedVideo` is then drawn over this file instead of saved as a
    /// screenshot.
    video_annotate: Option<PathBuf>,

    // Dialogs.
    settings_view: Option<SettingsView>,
    about_view: Option<AboutView>,
    /// Settings did not exist at start-up, so this is a first run; cleared
    /// once the welcome window has had its one chance.
    first_run: bool,
    /// The welcome window. Closing it only closes it; it never quits, not
    /// even when the tray is missing.
    firstrun_view: Option<FirstRunView>,
    updates_view: Option<UpdatesView>,
    convert_image_view: Option<ConvertImageView>,
    convert_video_view: Option<ConvertVideoView>,
    /// The self-update worker, checked each tick. When the update works the
    /// worker has already started the new version and ended this process,
    /// so a result only ever comes back for a failure.
    update_job: Option<std::thread::JoinHandle<Result<(), String>>>,
}

impl TrayApp {
    pub(super) fn new(start: Startup) -> TrayApp {
        let now = Instant::now();
        let keep_recent = start.settings.keep_recent;
        TrayApp {
            settings: start.settings,
            app_state: AppState::load(),
            hotkeys: start.hotkeys,
            hotkey_error: start.hotkey_error,
            portal_hotkey: start.portal_hotkey,
            wayland: start.wayland,
            recorder: Recorder::new(),
            started_at: now,
            last_tick: now,
            tray: None,
            tray_built: false,
            tray_menu: None,
            tray_menu_closed: None,
            tray_click_at: None,
            settings_stamp: framefang_core::settings_store::path()
                .and_then(|p| framefang_core::settings_store::stamp_of(&p)),
            settings_checked: now,
            settings_bad_reported: None,
            pending_patch: None,
            settings_io_failing: false,
            capturing: false,
            overlay: None,
            overlay_desktop: None,
            color_pick: None,
            pinned: Vec::new(),
            toast: None,
            session_id: 0,
            session_windows: Vec::new(),
            session_facts: None,
            recent: crate::recent::Recent::start(crate::recent::dir(), keep_recent),
            recent_list: Default::default(),
            read_job: None,
            read_gen: 0,
            scroll: None,
            scroll_hud: None,
            scroll_flip: false,
            recording_view: None,
            pending_region_audio: None,
            gif_plan: None,
            gif_job: None,
            last_recording: None,
            video_annotate: None,
            settings_view: None,
            about_view: None,
            first_run: start.first_run,
            firstrun_view: None,
            updates_view: None,
            convert_image_view: None,
            convert_video_view: None,
            update_job: None,
        }
    }

    /// Recordings left in the GIF work folder by an earlier run go to the
    /// recordings folder (as MP4s).
    pub(super) fn sweep_old_gif_work(&self) {
        sweep_gif_work(recordings_directory_for(&self.settings), self.settings.file_name_pattern.clone());
    }

    /// Called once the event loop has returned.
    pub(super) fn finish(&mut self) {
        // Changes are written as they are made, so the only one left is one
        // held back by a settings.json that did not parse.
        if let Some(p) = self.pending_patch.take() {
            let _ = framefang_core::settings_store::update(&p);
        }
        // A GIF still being made is cancelled, which keeps its MP4 in the
        // recordings folder; if even that does not finish in time, the
        // next start sweeps it there.
        if let Some(job) = self.gif_job.take() {
            job.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
            let give_up = Instant::now() + Duration::from_secs(2);
            while !job.handle.is_finished() && Instant::now() < give_up {
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }
}
