//! Tray-resident lifecycle.
//!
//! Uses `winit` directly because both `tray-icon` and `global-hotkey` deliver
//! events via channels that need the OS event loop to be pumped on the main
//! thread. We poll those channels each tick and dispatch to the capture
//! pipeline.

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
    tray::{MenuLabels, Tray, TrayEvent},
};

use crate::about_form::{AboutOutcome, AboutView};
use crate::convert_image_form::{ConvertImageOutcome, ConvertImageView};
use crate::convert_video_form::{ConvertVideoOutcome, ConvertVideoView};
use crate::editor::{ChosenRegion, Overlay, OverlayGroup, OverlayOutcome, VideoCommit};
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

pub fn run() -> Result<()> {
    // Must be sampled before `load()` and before anything can call `save()`:
    // the settings file is written on first save/exit, so its absence is the
    // only "this user has never run Framefang" signal we have. A missing config
    // dir (no HOME) reads as "not first run" — better to skip the welcome
    // than to show it on every launch.
    let first_run = AppSettings::settings_path().map(|p| !p.exists()).unwrap_or(false);

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

    // Asked once, at startup, and threaded through the app: the answer can't
    // change without a new login session, and both the startup notice and the
    // Capture row label need it.
    let wayland = framefang_platform::session::is_wayland();

    // Hotkey init is best-effort. If it fails (no display server, no portal,
    // the binding already taken) the app stays running so the user can fix the
    // issue and try again from the menu later. The failure is *kept*, not just
    // logged: on Wayland "the shortcut doesn't work" is the single most likely
    // thing to go wrong, and stderr is not a surface a tray app's user reads.
    // `ensure_tray` turns whatever is here into one toast.
    let mut hotkey_error: Option<String> = None;
    let mut hotkeys = match HotkeyManager::new() {
        Ok(mut hk) => {
            // Only the primary capture binding earns a toast: the two
            // optional actions ship unbound and a chord the desktop already
            // owns costs just that shortcut (logged by `apply_hotkeys`).
            for (action, e) in apply_hotkeys(&mut hk, &settings) {
                if action == HotkeyAction::Capture {
                    hotkey_error = Some(e.to_string());
                }
            }
            // The registration succeeds even when the system takes the key
            // first, so this is the only way to know presses will never arrive.
            if hotkey_error.is_none() && crate::system_shortcuts::system_takes(&settings.hotkey()) {
                hotkey_error = Some(format!(
                    "Windows is using Print Screen for its own capture. In Settings > {}, \
                     turn off \u{201C}{}\u{201D}. Framefang's Settings has a button that opens it.",
                    crate::system_shortcuts::windows_page_path(),
                    crate::system_shortcuts::switch_name()));
            }
            Some(hk)
        }
        Err(e) => {
            eprintln!("hotkey init failed: {e} — use tray menu to capture");
            hotkey_error = Some(e.to_string());
            None
        }
    };

    // Whether presses arrive through a window-system grab or through the
    // desktop's global-shortcuts portal. Only the portal can have the
    // compositor override the requested binding, so only it needs the extra
    // "your desktop decides" wording.
    let portal_hotkey = hotkeys.as_ref()
        .map(|hk| hk.backend().is_portal())
        .unwrap_or(false);

    if hotkey_error.is_some() {
        eprintln!("Framefang is running. The capture shortcut is unavailable — \
                   use the tray menu to capture.");
    } else if portal_hotkey {
        eprintln!("Framefang is running. Wayland session — {} was requested from the \
                   desktop's global-shortcuts portal; your desktop decides the final \
                   binding and lists it under its keyboard settings.",
            settings.hotkey().describe());
    } else {
        eprintln!("Frame Fang is running. Press {} or use the tray menu to capture.",
            settings.hotkey().describe());
    }

    struct TrayApp {
        settings:   AppSettings,
        hotkeys:    Option<HotkeyManager>,
        /// Built on the first event-loop tick, not in `run()` — see
        /// `ensure_tray`. Stays `None` for the whole session if the build
        /// failed, in which case the fallback About window is the only
        /// surface and closing it quits.
        tray:       Option<Tray>,
        /// Whether `ensure_tray` has already run. Distinguishes "not built
        /// yet" from "built and failed", which `tray: None` alone can't.
        tray_built: bool,
        /// Wayland session. Capture and recording both route through
        /// xdg-desktop-portal here rather than through X11.
        wayland:    bool,
        /// Set when hotkey setup failed, so `ensure_tray` can say what went
        /// wrong once instead of leaving a shortcut that never fires
        /// unexplained. Cleared after it has been shown.
        hotkey_error: Option<String>,
        /// Whether presses come from the global-shortcuts portal rather than a
        /// window-system grab. Changes what the tray's Capture row can honestly
        /// promise about the binding.
        portal_hotkey: bool,
        recorder:   Recorder,
        /// Active overlay editor window, if a capture-and-edit is in flight.
        /// Holds the captured screenshot and the user's selection state until
        /// they accept (Enter / right-click) or cancel (Esc).
        overlay:    Option<Overlay>,
        /// Floating "Pin" windows the user has chosen to keep on screen.
        /// Each is its own borderless winit window; the tray app routes
        /// their `WindowEvent`s by `WindowId` and drops them when their
        /// handler signals close.
        pinned:     Vec<PinView>,
        /// The "Copied to clipboard" pill after a copy that closed the
        /// capture window. Dropped by `about_to_wait` once it has faded.
        toast:      Option<crate::toast::Toast>,
        /// The tray icon's menu in the color scheme (Windows), while open.
        tray_menu:  Option<crate::tray_menu::TrayMenu>,
        /// When `tray_menu` last closed. A click on the icon right after it
        /// closed is the click that closed it, so it must not reopen it.
        tray_menu_closed: Option<Instant>,
        /// What `settings.json` looked like when last read or written here,
        /// and when it was last looked at: a hand edit is picked up within
        /// a second (`watch_settings_file`).
        settings_stamp: Option<framefang_core::settings_store::Stamp>,
        settings_checked: Instant,
        /// The version of a hand edit that didn't parse, already reported.
        settings_bad_reported: Option<framefang_core::settings_store::Stamp>,
        /// Changes made while `settings.json` couldn't be read (a hand edit
        /// in progress): in use now, written once the file parses again.
        pending_patch: Option<framefang_core::settings_store::Patch>,
        /// Writing settings.json is failing (said once until it works).
        settings_io_failing: bool,
        /// What the app remembers by itself (`state.json`): the last
        /// captured rectangle.
        app_state: framefang_core::app_state::AppState,
        /// The recording running now ends as a GIF: where its MP4 goes
        /// meanwhile and what the GIF will be called. Decided at start, so
        /// changing the format mid-recording changes nothing.
        gif_plan: Option<GifPlan>,
        /// A finished recording being made into a GIF.
        gif_job: Option<GifJob>,
        /// Text or a QR code being read off a picture, and whether a TEXT or
        /// QR picker waits on it (it closes with the answer) or the editor
        /// asked (it stays), and whether the switch key flipped what a QR
        /// link does. Only the newest request (`read_gen`) counts.
        read_job: Option<(crate::read_text::ReadJob, bool, bool)>,
        read_gen: u64,
        /// The last 10 captures (`recent`): its worker, the list as it last
        /// said, the capture session outputs now belong to (each freeze,
        /// full-screen shot or recent edit starts one), and where the tray was
        /// last clicked (a recent copy's notice goes there).
        recent: crate::recent::Recent,
        recent_list: std::sync::Arc<Vec<crate::recent::Entry>>,
        /// A scrolling capture running, and what shows around its area.
        scroll: Option<crate::scroll::Session>,
        scroll_hud: Option<crate::scroll_hud::ScrollHud>,
        /// The running scrolling capture was picked with the switch key held.
        scroll_flip: bool,
        session_id: u64,
        tray_click_at: Option<(i32, i32)>,
        /// The windows on screen when the current capture froze.
        session_windows: Vec<framefang_platform::windows_list::OnScreenWindow>,
        /// The app and title of the window under the picked region, for
        /// `{app}` and `{title}` in this capture's file names.
        session_facts: Option<(String, String)>,
        /// The editor eyedropper's color-pick session over the frozen
        /// screen, while it is open. Its answer goes back to `overlay`.
        color_pick: Option<Overlay>,
        /// Settings dialog window, present while the user is editing
        /// preferences. Routed by `WindowId` like the overlay + pinned
        /// windows. `None` when no settings dialog is open.
        settings_view: Option<SettingsView>,
        /// Floating recording-control window that appears while a screen
        /// recording is live. Lets the user stop the recording with one
        /// click instead of digging through the tray menu.
        recording_view: Option<RecordingIndicator>,
        /// Themed About modal — replaces the prior rfd MessageDialog.
        about_view:    Option<AboutView>,
        /// Set when the settings file didn't exist at startup; cleared by
        /// `maybe_show_first_run` once the welcome window has had its one
        /// chance to open.
        first_run:     bool,
        /// One-time welcome window. Closing it only drops the view — unlike
        /// the tray-failure About fallback, it never exits the app.
        firstrun_view: Option<FirstRunView>,
        /// Themed Check-for-updates modal — replaces the prior open-url.
        updates_view:  Option<UpdatesView>,
        /// Themed Convert-image dialog.
        convert_image_view: Option<ConvertImageView>,
        /// Themed Convert-video dialog (shells out to bundled ffmpeg).
        convert_video_view: Option<ConvertVideoView>,
        /// In-flight self-update job, if the user clicked Download &
        /// install in the Updates dialog. Polled from `poll()` every
        /// tick — when it finishes we either log the error and carry on
        /// or, on success, the worker will already have spawned the new
        /// framefang.exe and called `exit(0)` so this thread never returns.
        update_job: Option<std::thread::JoinHandle<Result<(), String>>>,
        last_tick:  Instant,
        capturing:  bool,
        /// Path of the most recently saved recording this session — the
        /// target of the tray's "Annotate last recording". Falls back to
        /// the newest .mp4 in the recordings folder when None (fresh app
        /// start).
        last_recording: Option<PathBuf>,
        /// Virtual-desktop bounds of the screenshot the live overlay is
        /// showing. A region the user drags in that overlay is clamped against
        /// exactly this before it reaches the recorder, so the rectangle can
        /// never name a pixel that wasn't on screen. Also where the REC
        /// indicator is placed relative to.
        overlay_desktop: Option<DesktopBounds>,
        /// When this instance came up — see the capture-request poll.
        started_at: Instant,
        /// Audio sources the tray asked for when it opened a record-region
        /// overlay. Taken when that overlay commits; `None` for the Record
        /// button in an ordinary capture session, which records silently — the
        /// same as the plain "Record" tray row.
        pending_region_audio: Option<framefang_platform::recorder::RecordingOptions>,
        /// While `overlay` is a video-annotate session (`new_for_video`),
        /// the clip it belongs to. `Accepted` then carries an annotations-
        /// only transparent overlay that gets burned over this file
        /// instead of being saved as a screenshot.
        video_annotate: Option<PathBuf>,
    }

    impl TrayApp {
        /// Build the tray icon, once, from inside the running event loop.
        ///
        /// `tray-icon` requires the status item to be created after the loop
        /// is up: on macOS an `NSStatusItem` built before `NSApplication`
        /// finishes launching is dropped on the floor and the menu bar stays
        /// empty. Windows and Linux tolerate the early build, but running the
        /// same deferred path everywhere means one code path to reason about
        /// instead of a macOS special case.
        ///
        /// A failure here is not survivable in silence: with no tray there is
        /// no menu, and the tray's Exit item is the app's only exit — the user
        /// would be left with an invisible process they can't quit. So we put
        /// the About window up as a visible, dismissable surface (closing it
        /// exits, see the `AboutOutcome::Closed` arm) and toast the reason.
        fn ensure_tray(&mut self, loop_target: &ActiveEventLoop) {
            if self.tray_built { return; }
            self.tray_built = true;

            match Tray::new(tray_tooltip(&self.settings),
                            &menu_labels(&self.settings, self.portal_hotkey)) {
                Ok(t) => {
                    t.set_gif(self.settings.record_format() == framefang_core::settings::RecordFormat::Gif);
                    t.set_last_region(self.app_state.last_region.is_some());
                    self.tray = Some(t);
                }
                Err(e) => {
                    eprintln!("tray init failed: {e}");
                    notify("Frame Fang: no tray icon",
                        &format!("{e}\n\nThe About window is open instead; closing it quits Frame Fang."),
                        true);
                    self.show_about(loop_target);
                    if self.about_view.is_none() {
                        // No tray and no window either — nothing on screen can
                        // ever reach this process. Quit instead of idling
                        // forever at 30 Hz.
                        eprintln!("No tray and no fallback window — exiting.");
                        loop_target.exit();
                    }
                }
            }

            // One toast, only when there is something the user has to act on.
            // A portal binding that went through needs no announcement — it
            // just works — and the old unconditional "Wayland can't do this"
            // notice is no longer true.
            if let Some(reason) = self.hotkey_error.take() {
                notify("Frame Fang: capture shortcut unavailable", &reason, true);
            }
        }

        fn poll(&mut self, loop_target: &ActiveEventLoop) {
            // An editor change made outside a window event (the overlay's own
            // tick) is saved too.
            self.drain_overlay_patch();
            // Drive any platform-native loop the tray depends on (GTK on
            // Linux). Must run before try_recv so menu-click signals have a
            // chance to land in the channel.
            let (ev, click) = match &self.tray {
                Some(tray) => {
                    tray.pump_events();
                    (tray.try_recv(), tray.take_click())
                }
                None => (TrayEvent::None, None),
            };
            self.run_tray_event(ev, loop_target);
            if let Some(c) = click { self.tray_clicked(c, loop_target); }
            if let Some(m) = self.tray_menu.as_mut() { m.tick(); }
            self.finish_tray_menu(loop_target);
            // The capture dock's animations run on time, not on events.
            if let Some(ov) = self.overlay.as_mut() { ov.tick(); }
            // A full-screen capture window keeps the keyboard, and hears Esc
            // even when Windows gave the keyboard to something else.
            #[cfg(windows)]
            {
                if let Some(ov) = self.overlay.as_mut() { ov.guard_focus(); }
                if let Some(c) = self.color_pick.as_mut() { c.guard_focus(); }
            }
            // A hand edit of settings.json takes effect without a restart.
            self.watch_settings_file();
            // `framefang --settings`, from a second launch or this one.
            if framefang_platform::instance::take_settings_request() {
                self.show_settings(loop_target);
            }
            // An open Settings window follows the FFmpeg download's progress.
            if let Some(v) = self.settings_view.as_mut() { v.tick(); }
            // Collected before dispatch so the manager borrow is done with
            // by the time an action mutates `self`.
            let fired = self.hotkeys.as_ref().map(|hk| hk.drain_pressed()).unwrap_or_default();
            for action in fired {
                match action {
                    HotkeyAction::Capture           => self.capture_from_hotkey(loop_target),
                    HotkeyAction::CaptureFullScreen => self.capture_full_screen(false),
                    HotkeyAction::ToggleRecording   => self.toggle_recording(loop_target),
                    HotkeyAction::CaptureLastRegion => self.capture_last_region(loop_target),
                    HotkeyAction::CaptureText => self.capture_reading(loop_target, crate::read_text::ReadKind::Text, false),
                    HotkeyAction::CaptureQr   => self.capture_reading(loop_target, crate::read_text::ReadKind::Qr, false),
                    HotkeyAction::CaptureScroll => self.capture_scroll(loop_target, false),
                }
            }
            // A second launch of Framefang doesn't open a second tray icon — it
            // leaves a capture request beside the instance lock and exits.
            // Claim it here so re-running the app (double-clicked desktop
            // icon, `framefang` typed again) captures instead of doing nothing.
            // Costs one failing `unlink` per tick when idle. Requests that
            // land in our first seconds of life are dropped unanswered: two
            // launches that close together are a login that started Framefang
            // twice (duplicate autostart entries), not someone asking for a
            // screenshot, and a dimmed full-screen overlay is the wrong
            // thing to greet a fresh desktop with.
            if framefang_platform::instance::take_capture_request()
                && self.started_at.elapsed() >= STARTUP_CAPTURE_GRACE
            {
                self.capture(loop_target);
            }
            // Nothing else watches the encoder between start and stop, so an
            // encoder that dies mid-recording (disk filled, capture device
            // unplugged, OOM kill) would otherwise leave the app flashing REC
            // over a dead process. `poll_health` has already returned the
            // recorder to idle by the time it errs, so we only clean up the UI
            // — calling `stop()` here would just report "not currently
            // recording" over the real reason.
            if let Err(e) = self.recorder.poll_health() {
                eprintln!("Recording stopped unexpectedly: {e}");
                if let Some(t) = &self.tray { t.set_recording(false); }
                self.recording_view = None;
                notify("Frame Fang: recording stopped unexpectedly", &format!("{e}"), true);
                // What a GIF recording got before it stopped still becomes one.
                if let Some(plan) = self.gif_plan.take() {
                    if std::fs::metadata(&plan.work).map(|m| m.len() > 0).unwrap_or(false) {
                        self.start_gif_job(plan);
                    } else {
                        let _ = std::fs::remove_file(&plan.work);
                    }
                }
            }
            self.poll_gif_job();
            self.poll_read_job(loop_target);
            self.poll_recent(loop_target);
            self.poll_scroll(loop_target);
            // Keep the recording indicator's flashing dot animating even
            // when there's no mouse activity over its window.
            if let Some(v) = &self.recording_view { v.tick(); }
            // Pump the updates dialog so its background-fetch result lands
            // and the "checking…" dots keep cycling.
            if let Some(v) = self.updates_view.as_mut() { v.tick(); }
            // Drive the convert-video dialog so its background ffmpeg
            // result lands and the "encoding…" dots keep moving.
            if let Some(v) = self.convert_video_view.as_mut() { v.tick(); }
            // Reap the self-update worker if it's done — surfaces any
            // failure to the user (success exits the process from inside
            // the worker, so we'd never see a successful join here).
            self.poll_update_job();
        }

        /// Do what a tray menu row asks, from the native menu or the app's own.
        fn run_tray_event(&mut self, ev: TrayEvent, loop_target: &ActiveEventLoop) {
            match ev {
                TrayEvent::None                  => {}
                TrayEvent::Capture               => self.capture(loop_target),
                TrayEvent::CaptureFullScreen     => self.capture_full_screen(true),
                TrayEvent::CaptureLastRegion     => {
                    // From a menu: let it close before the shot.
                    #[cfg(target_os = "windows")]
                    { dismiss_tray_flyouts(); std::thread::sleep(TRAY_MENU_SETTLE); }
                    #[cfg(not(target_os = "windows"))]
                    std::thread::sleep(Duration::from_millis(250));
                    self.capture_last_region(loop_target);
                }
                TrayEvent::RecentCopy(i)         => self.open_recent(i, false),
                TrayEvent::RecentEdit(i)         => self.open_recent(i, true),
                TrayEvent::CaptureText           => self.capture_reading(loop_target, crate::read_text::ReadKind::Text, true),
                TrayEvent::CaptureScroll         => self.capture_scroll(loop_target, true),
                TrayEvent::CaptureQr             => self.capture_reading(loop_target, crate::read_text::ReadKind::Qr, true),
                TrayEvent::SetGif(on)            => self.set_record_format(on, loop_target),
                TrayEvent::CancelGif             => self.cancel_gif(),
                TrayEvent::CaptureDelayed(secs)  => self.capture_after(loop_target, Duration::from_secs(secs as u64)),
                TrayEvent::CancelPending         => {} // handled inline by the delay loop
                TrayEvent::StartRecording(opts)  => self.start_recording(opts, loop_target, None),
                TrayEvent::StartRegionRecording(opts) => self.begin_region_recording(opts, loop_target),
                TrayEvent::StopRecording         => self.stop_recording(),
                TrayEvent::AnnotateLastRecording => self.annotate_last_recording(loop_target),
                TrayEvent::OpenSaveFolder        => self.open_save_folder(),
                TrayEvent::OpenRecordingsFolder  => self.open_recordings_folder(),
                TrayEvent::Settings              => self.show_settings(loop_target),
                TrayEvent::About                 => self.show_about(loop_target),
                TrayEvent::CheckForUpdates       => self.show_updates(loop_target),
                TrayEvent::ConvertImage          => self.show_convert_image(loop_target),
                TrayEvent::ConvertVideo          => self.show_convert_video(loop_target),
                TrayEvent::Exit                  => {
                    // Stop any active recording before tearing down so
                    // the MP4 moov atom gets finalized. Best-effort —
                    // if it errors we still exit.
                    let _ = self.recorder.stop();
                    // A GIF recording has no time to become one: keep its MP4.
                    if let Some(plan) = self.gif_plan.take() {
                        let _ = std::fs::create_dir_all(&plan.dir);
                        if keep_gif_mp4(&plan.work, &plan.dir, &plan.pattern, &plan.facts).is_none() {
                            eprintln!("framefang: GIF recording left in {} for the next start", gif_work_dir().display());
                        }
                    }
                    // The last capture's picture finishes writing.
                    self.recent.shutdown();
                    loop_target.exit();
                }
            }
        }

        /// A click on the tray icon (Windows): open the app's menu there, or
        /// close it if it is open, like a native menu.
        fn tray_clicked(&mut self, click: framefang_platform::tray::TrayClick, loop_target: &ActiveEventLoop) {
            if self.tray_menu.is_some() {
                self.tray_menu = None;
                self.tray_menu_closed = Some(Instant::now());
                return;
            }
            // The press on the icon took focus from the open menu, which
            // closed it; this is the same click coming up.
            if self.tray_menu_closed.is_some_and(|t| t.elapsed() < Duration::from_millis(350)) {
                return;
            }
            self.tray_click_at = Some((click.x, click.y));
            let key = |a: HotkeyAction| self.settings.hotkey_for(a).map(|h| h.describe());
            let state = crate::tray_menu::MenuState {
                capture_key: key(HotkeyAction::Capture),
                full_key: key(HotkeyAction::CaptureFullScreen),
                record_key: key(HotkeyAction::ToggleRecording),
                recording: self.recorder.is_recording(),
                sound: sound_from_name(&self.settings.record_sound),
                last_region: self.app_state.last_region.is_some(),
                gif: self.settings.record_format() == framefang_core::settings::RecordFormat::Gif,
                converting: self.gif_job.is_some(),
                ffmpeg: crate::convert_video_form::locate_ffmpeg().is_some(),
            };
            // Keeping off: no RECENT column.
            let recent = self.settings.keep_recent.then(|| self.recent_list.clone());
            match crate::tray_menu::TrayMenu::new(loop_target, (click.x, click.y), state, recent) {
                Ok(m) => self.tray_menu = Some(m),
                Err(e) => {
                    eprintln!("framefang: tray menu: {e}");
                    if let Some(t) = &self.tray { t.show_native_menu(); }
                }
            }
        }

        /// Act on the tray menu's answer once it has one: it closes first, so
        /// it is off the screen before whatever was picked starts.
        fn finish_tray_menu(&mut self, loop_target: &ActiveEventLoop) {
            let Some(out) = self.tray_menu.as_mut().and_then(|m| m.outcome.take()) else { return };
            if let Some(m) = self.tray_menu.take() {
                // The Sound and Format chips' choices are kept in settings.json.
                let name = sound_name(m.sound());
                if self.settings.record_sound != name {
                    let p = patch_of(&self.settings, |s| s.record_sound = name.to_owned());
                    self.store_patch(p);
                }
                let gif_now = self.settings.record_format() == framefang_core::settings::RecordFormat::Gif;
                if m.gif() != gif_now { self.set_record_format(m.gif(), loop_target); }
            }
            self.tray_menu_closed = Some(Instant::now());
            match out {
                crate::tray_menu::MenuOutcome::Run(ev) => self.run_tray_event(ev, loop_target),
                crate::tray_menu::MenuOutcome::Recent { index, edit } => self.open_recent(index, edit),
                crate::tray_menu::MenuOutcome::Closed => {}
            }
        }

        /// A new capture session: what it outputs is one recent item.
        fn new_session(&mut self) { self.session_id += 1; }

        /// Keep `img` as this session's recent capture (unframed).
        fn keep_recent(&self, img: &image::RgbaImage) {
            if self.settings.keep_recent { self.recent.record(self.session_id, img.clone()); }
        }

        /// The tray's RECENT item `index` (newest first): read it back to
        /// copy it or to open it in the editor.
        fn open_recent(&mut self, index: usize, edit: bool) {
            match self.recent_list.get(index) {
                Some(e) => self.recent.load(&e.id, edit),
                None => notify("Frame Fang: that capture is gone", "It was replaced by newer ones.", false),
            }
        }

        /// What the recent worker said: a new list for the menus, or a
        /// picture read back to copy or edit.
        fn poll_recent(&mut self, loop_target: &ActiveEventLoop) {
            use crate::recent::Reply;
            for reply in self.recent.poll() {
                match reply {
                    Reply::Changed(list) => {
                        if let Some(t) = &self.tray {
                            t.set_recent(&list.iter().map(|e| e.caption()).collect::<Vec<_>>());
                        }
                        self.recent_list = std::sync::Arc::new(list);
                    }
                    Reply::Loaded { img, edit: false } => {
                        if self.copy_final(img) {
                            // The notice by the tray, where the click was.
                            let size = crate::toast::size();
                            let spot = self.tray_click_at
                                .map(|(x, y)| (x - size.0 as i32, y - size.1 as i32 - 16));
                            if let Some(spot) = spot {
                                if let Ok(t) = crate::toast::Toast::new(loop_target, spot) { self.toast = Some(t); }
                            }
                        }
                    }
                    Reply::Loaded { img, edit: true } => self.edit_picture(loop_target, img),
                    Reply::Gone => notify("Frame Fang: that capture is gone", "It was replaced by newer ones.", false),
                }
            }
        }

        /// Start following the page under `area` (desktop pixels) and show
        /// the S1 look around it. `flip`: the switch key was held as it was
        /// picked, so the result does the other thing than the setting says.
        fn start_scroll(&mut self, loop_target: &ActiveEventLoop, area: (i32, i32, i32, i32), flip: bool) {
            if self.scroll.is_some() || area.2 < 40 || area.3 < 60 {
                notify("Frame Fang: area too small to scroll", "Drag over more of the page.", false);
                return;
            }
            self.scroll_flip = flip;
            self.scroll = Some(crate::scroll::Session::start(area));
            match crate::scroll_hud::ScrollHud::new(loop_target, area) {
                Ok(h) => self.scroll_hud = Some(h),
                Err(e) => eprintln!("framefang: scroll hud: {e}"),
            }
        }

        /// The scrolling capture's progress, and its result once it ends: in
        /// the editor beside where it was taken when it fits, else copied
        /// and saved (the scrolling editor comes next).
        fn poll_scroll(&mut self, loop_target: &ActiveEventLoop) {
            if let Some(h) = self.scroll_hud.as_mut() { h.tick(); }
            let Some(s) = &self.scroll else { return };
            let area = s.area;
            for ev in s.poll() {
                match ev {
                    crate::scroll::Event::Progress(h) => {
                        if let Some(hud) = self.scroll_hud.as_mut() { hud.set_height(h); }
                    }
                    crate::scroll::Event::Lost(lost) => {
                        if let Some(hud) = self.scroll_hud.as_mut() { hud.set_lost(lost); }
                    }
                    crate::scroll::Event::Done { img, end } => {
                        self.scroll = None;
                        self.scroll_hud = None;
                        if end == crate::scroll::End::Cancelled { return; }
                        if let crate::scroll::End::Failed(_) = end {
                            notify("Frame Fang: scrolling capture failed", &end.notice(0).unwrap_or_default(), true);
                            return;
                        }
                        if let Some(n) = end.notice(img.height()) { notify("Frame Fang: scrolling capture", &n, false); }
                        self.finish_scroll(loop_target, img, area);
                        return;
                    }
                }
            }
        }

        fn finish_scroll(&mut self, loop_target: &ActiveEventLoop, img: image::RgbaImage, area: (i32, i32, i32, i32)) {
            // Where it was taken, as tall as it came out.
            let pic = (area.0, area.1, img.width() as i32, img.height() as i32);
            let monitors = capture_all_screens().map(|s| s.geometry().monitors().to_vec()).unwrap_or_default();
            // After a scrolling capture, its own setting (not a region's).
            let copy_first = self.settings.scroll_copies(self.scroll_flip);
            if self.settings.scroll_opens_editor(self.scroll_flip) {
                if let Some(c) = crate::editor::chosen_for(img.clone(), pic, &monitors) {
                    self.open_chosen(loop_target, c, copy_first);
                    return;
                }
                // Taller than the screen: the scrolling editor.
                let work = crate::tray_menu::work_area(loop_target, (area.0 + area.2 / 2, area.1 + area.3 / 2));
                if let Some(t) = crate::editor::tall_for(img.clone(), area.0, work) {
                    self.open_tall(loop_target, t, copy_first);
                    return;
                }
            }
            // Copy only, or too wide for any editor window: copied and saved.
            self.copy_final(img.clone());
            self.save_final(img);
        }

        /// A picture taller than the screen goes to the scrolling editor;
        /// copied first when `copy_first`, as `open_chosen` does.
        fn open_tall(&mut self, loop_target: &ActiveEventLoop, t: crate::editor::TallChosen, copy_first: bool) {
            if copy_first {
                self.keep_recent(&t.picture);
                self.copy_final(t.picture.clone());
            }
            match Overlay::new_for_tall_editor(loop_target, t, self.settings.clone()) {
                Ok(ov) => self.overlay = Some(ov),
                Err(e) => {
                    eprintln!("Scrolling editor open failed: {e}");
                    notify("Frame Fang: couldn't open the editor", &format!("{e}"), true);
                }
            }
        }

        /// Open `img` (a recent capture) in the editor: in its own window
        /// in the middle of the screen the tray is on, or, when that window
        /// wouldn't fit, over the frozen screen in place. Saving it is a new
        /// recent item.
        fn edit_picture(&mut self, loop_target: &ActiveEventLoop, img: image::RgbaImage) {
            if self.capturing || self.overlay.is_some() {
                notify("Frame Fang: busy", "Finish what is open first, then try again.", false);
                return;
            }
            let Ok(shot) = capture_all_screens() else {
                notify("Frame Fang: couldn't open the editor", "The screen couldn't be read.", true);
                return;
            };
            self.new_session();
            self.session_facts = None;
            let geom = shot.geometry();
            let at = self.tray_click_at.unwrap_or(geom.origin());
            let monitors = geom.monitors();
            let Some(m) = monitors.iter().copied()
                .find(|m| at.0 >= m.x && at.0 < m.x + m.width as i32 && at.1 >= m.y && at.1 < m.y + m.height as i32)
                .or_else(|| monitors.first().copied()) else { return };
            let (w, h) = (img.width() as i32, img.height() as i32);
            let pic = (m.x + (m.width as i32 - w) / 2, m.y + (m.height as i32 - h) / 2, w, h);
            if let Some(c) = crate::editor::chosen_for(img.clone(), pic, monitors) {
                match Overlay::new_for_editor(loop_target, c, self.settings.clone()) {
                    Ok(ov) => self.overlay = Some(ov),
                    Err(e) => notify("Frame Fang: couldn't open the editor", &format!("{e}"), true),
                }
                return;
            }
            // Taller than the screen (a scrolling capture): the scrolling
            // editor.
            if h > m.height as i32 {
                let work = crate::tray_menu::work_area(loop_target, at);
                if let Some(t) = crate::editor::tall_for(img.clone(), pic.0, work) {
                    match Overlay::new_for_tall_editor(loop_target, t, self.settings.clone()) {
                        Ok(ov) => self.overlay = Some(ov),
                        Err(e) => notify("Frame Fang: couldn't open the editor", &format!("{e}"), true),
                    }
                    return;
                }
            }
            // Too big for its own window: over the frozen screen, where it fits.
            let (ox, oy) = geom.origin();
            let bounds = shot.desktop_bounds();
            let map = shot.map;
            let mut bitmap = shot.bitmap;
            let (bw, bh) = (bitmap.width() as i32, bitmap.height() as i32);
            if w > bw || h > bh {
                notify("Frame Fang: too big to edit", "It is bigger than the screens. It was copied instead.", false);
                self.copy_final(img);
                return;
            }
            let at = ((pic.0 - ox).clamp(0, bw - w), (pic.1 - oy).clamp(0, bh - h));
            image::imageops::replace(&mut bitmap, &img, at.0 as i64, at.1 as i64);
            match Overlay::new(loop_target, geom, bitmap, self.settings.clone()) {
                Ok(mut ov) => {
                    ov.set_display_map(map);
                    ov.preselect((at.0 + ox, at.1 + oy, w, h));
                    self.overlay_desktop = Some(bounds);
                    self.pending_region_audio = None;
                    self.overlay = Some(ov);
                }
                Err(e) => notify("Frame Fang: couldn't open the editor", &format!("{e}"), true),
            }
        }

        fn capture(&mut self, loop_target: &ActiveEventLoop) {
            self.capture_after(loop_target, Duration::ZERO);
        }

        /// The picker took `r` (virtual-screen pixels): keep it for "Capture
        /// last region". Written off the event loop; the newest wins.
        fn remember_region(&mut self, r: (i32, i32, i32, i32)) {
            let rect = [r.0, r.1, r.2, r.3];
            if !framefang_core::app_state::valid_rect(rect) || self.app_state.last_region == Some(rect) { return; }
            self.app_state.last_region = Some(rect);
            // Each write takes the newest state under the one lock, so the
            // last written is always the last picked, whatever order the
            // threads run in.
            static LATEST: std::sync::Mutex<Option<framefang_core::app_state::AppState>> = std::sync::Mutex::new(None);
            if let Ok(mut l) = LATEST.lock() { *l = Some(self.app_state.clone()); }
            std::thread::spawn(|| {
                let Ok(l) = LATEST.lock() else { return };
                if let Some(state) = l.as_ref() {
                    if let Err(e) = state.save() { eprintln!("framefang: state.json: {e}"); }
                }
            });
            if let Some(t) = &self.tray { t.set_last_region(true); }
        }

        /// Record as GIF or MP4, from the tray. GIF needs FFmpeg everywhere
        /// (MP4 does too on Windows and Linux); without it, Settings opens
        /// where FFmpeg can be set up and the choice stays as it was.
        fn set_record_format(&mut self, gif: bool, loop_target: &ActiveEventLoop) {
            use framefang_core::settings::RecordFormat;
            let want = if gif { RecordFormat::Gif } else { RecordFormat::Mp4 };
            if gif && !self.have_ffmpeg(loop_target, "Recording a GIF") {
                if let Some(t) = &self.tray { t.set_gif(false); }
                return;
            }
            if self.settings.record_format() == want { return; }
            let p = patch_of(&self.settings, |s| s.record_format = want.name().to_owned());
            self.store_patch(p);
        }

        /// Whether a region capture is on the clipboard before its editor
        /// opens: with auto-copy, or copy-only with the switch key held.
        fn region_copies_first(&self) -> bool { self.settings.auto_copy || !self.settings.open_editor }

        /// A picked region goes to its own editor window, on the clipboard
        /// first when `copy_first`.
        fn open_chosen(&mut self, loop_target: &ActiveEventLoop, c: ChosenRegion, copy_first: bool) {
            if copy_first {
                self.keep_recent(&c.picture);
                self.copy_final(c.picture.clone());
            }
            match Overlay::new_for_editor(loop_target, c, self.settings.clone()) {
                Ok(ov) => self.overlay = Some(ov),
                Err(e) => {
                    eprintln!("Editor open failed: {e}");
                    notify("Frame Fang: capture failed",
                        &format!("The editor window wouldn't open: {e}"), true);
                }
            }
        }

        /// Read `img` for text or QR codes, off the event loop. A TEXT or QR
        /// picker waits for the answer; the editor carries on.
        fn start_read(&mut self, img: image::RgbaImage, kind: crate::read_text::ReadKind, flip: bool) {
            let picker = self.overlay.as_ref().is_some_and(|ov| ov.is_reading());
            self.read_gen += 1;
            self.read_job = Some((crate::read_text::start(img, kind, self.read_gen), picker, flip));
        }

        /// A read finished: its text (or QR codes) goes on the clipboard and
        /// a pill under the picture says what was copied (a link, a Wi-Fi
        /// password...), or that nothing was there. A QR web link opens
        /// instead when Settings says so, or (the switch key held as the read
        /// was let go) when it says copy. A picker that waited closes; the
        /// editor stays. An answer for a session that is gone is dropped.
        fn poll_read_job(&mut self, loop_target: &ActiveEventLoop) {
            use crate::read_text::ReadAnswer;
            use std::sync::mpsc::TryRecvError;
            let Some((job, picker, flip)) = self.read_job.as_ref() else { return };
            let answer = match job.rx.try_recv() {
                Ok(a) => a,
                Err(TryRecvError::Empty) => return,
                Err(TryRecvError::Disconnected) => ReadAnswer::Failed("The reader stopped before it finished.".to_owned()),
            };
            let (kind, gen, picker, flip) = (job.kind, job.gen, *picker, *flip);
            self.read_job = None;
            let alive = self.overlay.as_ref().is_some_and(|ov| ov.is_reading() == picker);
            if gen != self.read_gen || !alive { return; }
            let link = answer.link_to_open(self.settings.open_qr_links != flip);
            let mut text = match &link {
                Some((_, host)) => crate::read_text::opened_notice(host),
                None => answer.notice(kind),
            };
            let spot = self.overlay.as_ref().and_then(|ov| ov.notice_spot(crate::toast::size_for(&text)));
            if picker {
                self.drain_overlay_patch();
                self.overlay = None;
            }
            if let Some((url, _)) = link {
                // Opened, not copied. If the browser can't be started, the
                // link is copied after all, so it isn't lost.
                if open_url(&url) {
                    if let Some(spot) = spot {
                        match crate::toast::Toast::saying(loop_target, spot, &text, true) {
                            Ok(t) => self.toast = Some(t),
                            Err(e) => eprintln!("framefang: read notice: {e}"),
                        }
                    }
                    return;
                }
                notify("Frame Fang: couldn't open the link", "It is on the clipboard instead.", true);
                text = answer.notice(kind);
            }
            let mut done = false;
            if let Some(t) = answer.clipboard_text() {
                if let Err(e) = framefang_platform::copy_text(&t) {
                    notify("Frame Fang: copy failed",
                        &format!("Couldn't put what was read on the clipboard: {e}"), true);
                    return;
                }
                done = true;
            }
            if let ReadAnswer::Failed(why) = &answer {
                notify("Frame Fang: couldn't read it", why, true);
            }
            if let Some(spot) = spot {
                match crate::toast::Toast::saying(loop_target, spot, &text, done) {
                    Ok(t) => self.toast = Some(t),
                    Err(e) => eprintln!("framefang: read notice: {e}"),
                }
            }
        }

        /// Copy, and show the "Copied" pill at `spot` once it worked.
        fn copy_with_toast(&mut self, loop_target: &ActiveEventLoop,
                           img: image::ImageBuffer<image::Rgba<u8>, Vec<u8>>, spot: Option<(i32, i32)>) {
            self.keep_recent(&img);
            if self.copy_final(img) {
                if let Some(spot) = spot {
                    match crate::toast::Toast::new(loop_target, spot) {
                        Ok(t) => self.toast = Some(t),
                        Err(e) => eprintln!("framefang: copied notice: {e}"),
                    }
                }
            }
        }

        /// Take the last captured rectangle again, then do what a released
        /// drag does: copy it, or open it in its own editor window, or (when
        /// the editor wouldn't fit beside it) edit it in place. Decided
        /// before any capture window exists, so nothing flashes up for a
        /// copy.
        fn capture_last_region(&mut self, loop_target: &ActiveEventLoop) {
            if self.capturing || self.overlay.is_some() { return; }
            let Some([x, y, w, h]) = self.app_state.last_region else {
                notify("Frame Fang: no last region yet",
                    "Capture a region once; Capture last region then takes it again.", false);
                return;
            };
            self.capturing = true;
            self.toast = None;
            self.new_session();
            let shot = capture_all_screens();
            let windows = framefang_platform::windows_list::on_screen_windows();
            self.session_facts = Some(facts_at(&windows, (x, y, w, h)));
            self.session_windows = windows;
            #[cfg(target_os = "windows")]
            dismiss_shell_popups();
            match shot {
                Ok(mut shot) => {
                    if self.settings.capture_cursor { framefang_platform::cursor::draw_into(&mut shot); }
                    let geom = shot.geometry();
                    let (ox, oy) = geom.origin();
                    let whole = (0, 0, shot.bitmap.width() as i32, shot.bitmap.height() as i32);
                    match framefang_core::virtual_desktop::intersect_rect((x - ox, y - oy, w, h), whole) {
                        None => {
                            notify("Frame Fang: the last region is off screen now",
                                "Drag a new one.", false);
                            self.capturing = false;
                            self.capture_from_hotkey(loop_target);
                            return;
                        }
                        Some(b) => {
                            let picture = crate::editor::crop(&shot.bitmap, b);
                            let pic = (b.0 + ox, b.1 + oy, b.2, b.3);
                            let monitor = geom.monitors().iter().copied().find(|m| {
                                let (cx, cy) = (pic.0 + pic.2 / 2, pic.1 + pic.3 / 2);
                                cx >= m.x && cx < m.x + m.width as i32 && cy >= m.y && cy < m.y + m.height as i32
                            });
                            if !self.settings.opens_editor(false) {
                                let spot = monitor.map(|m| crate::editor::toast_spot(pic, m, crate::toast::size()));
                                self.copy_with_toast(loop_target, picture, spot);
                            } else if let Some(c) = crate::editor::chosen_for(picture, pic, geom.monitors()) {
                                let copy_first = self.region_copies_first();
                                self.open_chosen(loop_target, c, copy_first);
                            } else {
                                let bounds = shot.desktop_bounds();
                                match Overlay::new(loop_target, geom, shot.bitmap, self.settings.clone()) {
                                    Ok(mut ov) => {
                                        ov.set_display_map(shot.map);
                                        ov.preselect(pic);
                                        self.overlay_desktop = Some(bounds);
                                        self.pending_region_audio = None;
                                        self.overlay = Some(ov);
                                    }
                                    Err(e) => notify("Frame Fang: capture failed",
                                        &format!("The editor window wouldn't open: {e}"), true),
                                }
                            }
                        }
                    }
                }
                Err(e) => notify("Frame Fang: capture failed", &format!("{e}"), true),
            }
            self.capturing = false;
        }

        /// Fire a capture after a user-facing delay, then open the overlay
        /// editor so the user can drag a region. Used by the plain "Capture
        /// Screen" menu/hotkey path (delay = 0) and the "Capture after delay…"
        /// submenu entries (3 / 5 / 10 s).
        ///
        /// During any countdown the tray's GTK main context is still pumped
        /// (on Linux) so the icon stays responsive.
        fn capture_after(&mut self, loop_target: &ActiveEventLoop, user_delay: Duration) {
            self.capture_inner(loop_target, user_delay, true);
        }

        /// The capture hotkey: no tray menu is open, so there is nothing to
        /// wait out and the screen freezes the moment the key goes down.
        fn capture_from_hotkey(&mut self, loop_target: &ActiveEventLoop) {
            self.capture_inner(loop_target, Duration::ZERO, false);
        }

        /// Copy screen text, or Read QR code: the picker opens in that mode.
        /// `settle` for the tray, as `capture_inner`.
        fn capture_reading(&mut self, loop_target: &ActiveEventLoop,
                           kind: crate::read_text::ReadKind, settle: bool) {
            if self.capturing || self.overlay.is_some() { return; }
            self.capture_inner(loop_target, Duration::ZERO, settle);
            if let Some(ov) = self.overlay.as_mut() { ov.open_reading(kind); }
        }

        /// Scrolling capture: the picker opens in SCROLL mode. Its own key
        /// pressed while one runs finishes it.
        fn capture_scroll(&mut self, loop_target: &ActiveEventLoop, settle: bool) {
            if let Some(s) = &self.scroll { s.stop(); return; }
            if self.capturing || self.overlay.is_some() || !crate::scroll::supported() { return; }
            self.capture_inner(loop_target, Duration::ZERO, settle);
            if let Some(ov) = self.overlay.as_mut() { ov.open_scroll(); }
        }

        /// `settle`: wait for the tray menu to finish fading before the shot,
        /// which only a capture started from that menu needs.
        fn capture_inner(&mut self, loop_target: &ActiveEventLoop, user_delay: Duration, settle: bool) {
            // A capture key while a page scrolls ends the scrolling instead.
            if let Some(s) = &self.scroll { s.stop(); return; }
            if self.capturing || self.overlay.is_some() { return; }
            self.capturing = true;
            // A pill still fading from the last copy would sit on top of the
            // new frozen desktop (it is kept out of the capture itself).
            self.toast = None;
            self.new_session();
            // Read now, while the hotkey is still down: holding the switch
            // key with it makes this capture do the other thing.
            let switch_at_press = !settle && user_delay.is_zero()
                && switch_key_down(self.settings.switch_key());

            if !user_delay.is_zero() {
                eprintln!("Capturing in {} second{}…",
                    user_delay.as_secs(),
                    if user_delay.as_secs() == 1 { "" } else { "s" });
                if let Some(t) = &self.tray { t.set_pending(true); }
                let until = Instant::now() + user_delay;
                let mut cancelled = false;
                while Instant::now() < until {
                    if let Some(t) = &self.tray {
                        t.pump_events();
                        // Drain pending tray events — if the user clicked
                        // "Cancel pending capture", abort the countdown.
                        if let TrayEvent::CancelPending = t.try_recv() {
                            eprintln!("Cancel pending capture — aborting countdown.");
                            cancelled = true;
                            break;
                        }
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
                if let Some(t) = &self.tray { t.set_pending(false); }
                if cancelled { self.capturing = false; return; }
            }

            // Brief settle delay so the tray menu / flyout can fully dismiss
            // before xcap shoots the desktop. On top of any user-facing delay.
            //
            // Windows: if the click came through the hidden-icons chevron,
            // the NotifyIconOverflowWindow stays up until we dismiss it, so
            // close it and give it a moment (`TRAY_MENU_SETTLE`).
            //
            // The hotkey skips all of it: no menu, nothing to wait out.
            #[cfg(target_os = "windows")]
            if settle {
                dismiss_tray_flyouts();
                std::thread::sleep(TRAY_MENU_SETTLE);
            }
            #[cfg(not(target_os = "windows"))]
            if settle { std::thread::sleep(Duration::from_millis(250)); }

            // Both failure arms are toasted, not just logged: the user pressed
            // the hotkey and nothing appeared on screen, and stderr is not a
            // surface a tray-resident app's user ever reads.
            match capture_all_screens() {
                Ok(mut shot) => {
                    // The windows as the frozen picture shows them, read
                    // before the capture screen opens on top of them all.
                    let windows = framefang_platform::windows_list::on_screen_windows();
                    self.session_windows = windows.clone();
                    self.session_facts = None;
                    // The Start menu and the other shell popups are in the
                    // picture now; close the real ones, which would sit
                    // above the capture screen.
                    #[cfg(target_os = "windows")]
                    dismiss_shell_popups();
                    if self.settings.capture_cursor { framefang_platform::cursor::draw_into(&mut shot); }
                    // Remember what desktop this selection is being dragged
                    // over: if the user turns the capture into a recording via
                    // the action row's Record button, that's what the rectangle
                    // gets clamped against.
                    self.overlay_desktop = Some(shot.desktop_bounds());
                    self.pending_region_audio = None;
                    // `shot.geometry()` carries the virtual-desktop origin and
                    // the monitor list (in device pixels), so the overlay spans
                    // every screen and maps the selection back to the pixels
                    // the user actually dragged over. The capture's
                    // logical/physical map travels with the frame too: on a
                    // scaled display the bitmap is in device pixels and the
                    // editor needs the mapping to hand a selection back to the
                    // rest of the desktop.
                    match Overlay::new(
                        loop_target, shot.geometry(), shot.bitmap, self.settings.clone()) {
                        Ok(mut ov) => {
                            ov.set_display_map(shot.map);
                            ov.hold_at_press = switch_at_press;
                            ov.set_windows(windows);
                            self.overlay = Some(ov);
                        }
                        Err(e) => {
                            eprintln!("Overlay open failed: {e}");
                            notify("Frame Fang: capture failed",
                                &format!("The editor window wouldn't open: {e}"), true);
                        }
                    }
                }
                Err(e) => {
                    eprintln!("Capture failed: {e}");
                    notify("Frame Fang: capture failed", &format!("{e}"), true);
                }
            }

            self.capturing = false;
        }

        /// Tray -> "Record region": freeze the desktop, open the overlay as a
        /// selection-only picker, and remember the audio sources so the commit
        /// can start the recorder with them. The recording itself starts from
        /// `start_region_from_selection` once the user confirms.
        fn begin_region_recording(&mut self,
                                  opts: framefang_platform::recorder::RecordingOptions,
                                  loop_target: &ActiveEventLoop) {
            if self.refuse_if_recording_busy() { return; }
            if self.capturing || self.overlay.is_some() { return; }
            self.capturing = true;

            // Same settle delay as `capture_after`: the tray menu has to be
            // fully gone before the desktop is frozen, or the user drags their
            // region over a ghost of the menu.
            #[cfg(target_os = "windows")]
            {
                dismiss_tray_flyouts();
                std::thread::sleep(TRAY_MENU_SETTLE);
            }
            #[cfg(not(target_os = "windows"))]
            std::thread::sleep(Duration::from_millis(250));

            match capture_all_screens() {
                Ok(shot) => {
                    let windows = framefang_platform::windows_list::on_screen_windows();
                    #[cfg(target_os = "windows")]
                    dismiss_shell_popups();
                    self.overlay_desktop = Some(shot.desktop_bounds());
                    self.pending_region_audio = Some(opts);
                    match Overlay::new_for_region_record(
                        loop_target, shot.geometry(), shot.bitmap, self.settings.clone())
                    {
                        Ok(mut ov) => {
                            ov.set_display_map(shot.map);
                            ov.set_windows(windows);
                            self.overlay = Some(ov);
                        }
                        Err(e) => {
                            self.pending_region_audio = None;
                            eprintln!("Region selector open failed: {e}");
                            notify("Frame Fang: recording failed",
                                &format!("The region selector wouldn't open: {e}"), true);
                        }
                    }
                }
                Err(e) => {
                    // Without a frozen desktop there is nothing to drag a
                    // rectangle over, so this is a dead end rather than a
                    // reason to fall back to a full-screen recording the user
                    // didn't ask for.
                    eprintln!("Region selector capture failed: {e}");
                    notify("Frame Fang: recording failed",
                        &format!("Couldn't read the screen to pick a region: {e}"), true);
                }
            }

            self.capturing = false;
        }

        /// Turn a confirmed overlay selection into a live region recording.
        ///
        /// The selection is in virtual-screen device pixels (the overlay lifts
        /// it out of frame space) — `record_rect_from_selection` clamps it to the desktop
        /// the shot came from and rounds it to the even dimensions H.264 needs.
        fn start_region_from_selection(&mut self,
                                       selection: (i32, i32, i32, i32),
                                       loop_target: &ActiveEventLoop) {
            let opts = self.pending_region_audio.take()
                .unwrap_or(framefang_platform::recorder::RecordingOptions::NONE);
            let Some(bounds) = self.overlay_desktop else {
                notify("Frame Fang: recording failed",
                    "Lost track of the screen the region was picked on.", true);
                return;
            };
            let Some(rect) = record_rect_from_selection(selection, bounds) else {
                notify("Frame Fang: region too small",
                    "Pick a larger area. A recording needs at least 16x16 pixels.", true);
                return;
            };
            // The overlay window is already dropped, but the compositor still
            // has to repaint what was under it. Starting the encoder in the
            // same breath burns a dimmed frame (and the selection chrome) into
            // the first moments of the video.
            std::thread::sleep(Duration::from_millis(300));
            self.start_recording(opts, loop_target, Some(rect));
        }

        /// One-click full-screen capture: grab the whole desktop and auto-save
        /// it straight to the configured folder — no region selector, no editor.
        /// The user gets a toast with the saved path since there's no overlay
        /// to confirm against.
        /// `settle`: wait out the tray menu's fade first. Only a capture
        /// started from that menu needs it; the hotkey shoots at once.
        fn capture_full_screen(&mut self, settle: bool) {
            if self.capturing || self.overlay.is_some() { return; }
            self.capturing = true;
            self.toast = None;
            // The whole desktop is no one app's.
            self.session_facts = None;

            // Same settle delay as capture_after so the tray menu / flyout fully
            // dismisses before xcap shoots the desktop.
            #[cfg(target_os = "windows")]
            if settle {
                dismiss_tray_flyouts();
                std::thread::sleep(TRAY_MENU_SETTLE);
            }
            #[cfg(not(target_os = "windows"))]
            if settle { std::thread::sleep(Duration::from_millis(250)); }

            match capture_all_screens() {
                Ok(mut shot) => {
                    if self.settings.capture_cursor { framefang_platform::cursor::draw_into(&mut shot); }
                    let img = shot.bitmap;
                    self.new_session();
                    self.keep_recent(&img);
                    if self.settings.auto_copy { self.copy_final(img.clone()); }
                    match self.save_screenshot(&img) {
                        Ok(path) => {
                            eprintln!("Saved full screen {}", path.display());
                            notify("Frame Fang: full screen saved",
                                &format!("{}", path.display()), false);
                        }
                        Err(e) => {
                            eprintln!("Save failed: {e}");
                            notify("Frame Fang: screenshot not saved", &format!("{e}"), true);
                        }
                    }
                }
                Err(e) => {
                    eprintln!("Capture failed: {e}");
                    notify("Frame Fang: capture failed", &format!("{e}"), true);
                }
            }

            self.capturing = false;
        }

        /// Persist a final image (already cropped to the user's selection in
        /// the overlay editor) to the configured save directory.
        ///
        /// Notifies on both outcomes, like `capture_full_screen`: the overlay
        /// has already closed by the time this runs, so the toast naming the
        /// path is the only confirmation the shot landed anywhere — and a
        /// failed write used to look exactly like a successful one.
        /// Save a screenshot into the screenshots folder (named and framed
        /// per Settings). The folder is kept in Settings the first time.
        fn save_screenshot(&mut self, img: &image::ImageBuffer<image::Rgba<u8>, Vec<u8>>) -> Result<PathBuf> {
            let (path, new_folder) = save_capture(&self.settings, img, &self.name_facts(img))?;
            if let Some(dir) = new_folder {
                let p = patch_of(&self.settings, |s| s.save_directory = dir.to_string_lossy().into_owned());
                self.store_patch(p);
            }
            Ok(path)
        }

        /// What this capture's file name is built from: now, its size, and
        /// the app and title of the window it was taken from, if any.
        fn name_facts(&self, bmp: &image::ImageBuffer<image::Rgba<u8>, Vec<u8>>) -> framefang_core::file_name::NameFacts {
            let mut f = name_facts(bmp);
            if let Some((app, title)) = &self.session_facts {
                f.app = app.clone();
                f.title = title.clone();
            }
            f
        }

        fn save_final(&mut self, img: image::ImageBuffer<image::Rgba<u8>, Vec<u8>>) {
            self.keep_recent(&img);
            match self.save_screenshot(&img) {
                Ok(path) => {
                    eprintln!("Saved {}", path.display());
                    notify("Frame Fang: screenshot saved",
                        &format!("{}", path.display()), false);
                }
                Err(e) => {
                    eprintln!("Save failed: {e}");
                    notify("Frame Fang: screenshot not saved", &format!("{e}"), true);
                }
            }
        }

        /// Push the final cropped image into the system clipboard. Uses
        /// arboard, which speaks the right protocol on every platform (X11
        /// selection on Linux, NSPasteboard on macOS, OpenClipboard on
        /// Windows). On Linux arboard runs a background thread to keep the
        /// selection alive while the source process exits — that's fine
        /// because Framefang stays resident. The pasted bitmap is exactly what
        /// `save_final` writes to disk.
        ///
        /// Both failure arms toast. Copy is a dead end when it fails — the
        /// bitmap is dropped here and never reaches the save folder — so a
        /// silent stderr line meant the user pasted stale clipboard contents
        /// and never learned the shot was gone.
        /// `true` when the picture is on the clipboard.
        fn copy_final(&mut self, img: image::ImageBuffer<image::Rgba<u8>, Vec<u8>>) -> bool {
            let img = crate::frame::for_clipboard(&img, self.settings.output_frame());
            let (w, h) = (img.width() as usize, img.height() as usize);
            let bytes  = img.into_raw();
            match arboard::Clipboard::new() {
                Ok(mut clip) => {
                    let data = arboard::ImageData {
                        width:  w,
                        height: h,
                        bytes:  std::borrow::Cow::Owned(bytes),
                    };
                    if let Err(e) = clip.set_image(data) {
                        eprintln!("Clipboard copy failed: {e}");
                        notify("Frame Fang: copy failed",
                            &framefang_core::failure::clipboard_failure(&e.to_string()), true);
                        false
                    } else {
                        eprintln!("Copied {w}×{h} to clipboard");
                        true
                    }
                }
                Err(e) => {
                    eprintln!("Clipboard unavailable: {e}");
                    notify("Frame Fang: copy failed",
                        &framefang_core::failure::clipboard_failure(&e.to_string()), true);
                    false
                }
            }
        }

        /// Save As: the user names the file and picks PNG or JPEG. `true`
        /// once it is written. `false` when the dialog was cancelled or the
        /// write failed, and the editor then comes back with the picture.
        fn save_final_as(&mut self, img: &image::ImageBuffer<image::Rgba<u8>, Vec<u8>>) -> bool {
            let dir = save_directory(&self.settings);
            let name = framefang_core::file_name::render(&self.settings.file_name_pattern, &self.name_facts(img));
            let Some(path) = rfd::FileDialog::new()
                .set_title("Save screenshot as")
                .set_directory(&dir)
                .set_file_name(format!("{name}.png"))
                .add_filter("PNG image", &["png"])
                .add_filter("JPEG image", &["jpg", "jpeg"])
                .save_file()
            else { return false; };
            let framed = crate::frame::apply(img, self.settings.output_frame());
            match write_image(path, &framed) {
                Ok(path) => {
                    self.keep_recent(img);
                    if self.settings.auto_copy { self.copy_final(img.clone()); }
                    eprintln!("Saved {}", path.display());
                    notify("Frame Fang: screenshot saved", &format!("{}", path.display()), false);
                    true
                }
                Err(e) => {
                    eprintln!("Save failed: {e}");
                    notify("Frame Fang: screenshot not saved", &format!("{e}"), true);
                    false
                }
            }
        }

        /// One binding, both ends of a take: start a recording, or stop the
        /// one that's already running.
        ///
        /// Starts with no audio, matching the plain "Record" menu row the
        /// hotkey is advertised on. The three audio variants stay menu-only —
        /// a single toggle has no way to ask which sources the user wants,
        /// and silently picking one would make the same key produce different
        /// files on different days.
        /// Recording, or still making the last recording's GIF.
        fn recording_busy(&self) -> bool {
            self.recorder.is_recording() || self.gif_job.is_some()
        }

        /// Say why a recording can't start now, if it can't. `true` refused.
        fn refuse_if_recording_busy(&self) -> bool {
            if self.recorder.is_recording() {
                notify("Frame Fang: already recording",
                    "Stop the current recording before starting another one.", true);
                true
            } else if self.gif_job.is_some() {
                notify("Frame Fang: making a GIF",
                    "The last recording is still becoming a GIF. It takes a moment.", false);
                true
            } else { false }
        }

        /// Make a finished GIF recording's MP4 into the GIF, on its own
        /// thread. The tray shows it is busy until it is done.
        fn start_gif_job(&mut self, plan: GifPlan) {
            let Some(ffmpeg) = crate::convert_video_form::locate_ffmpeg() else {
                let kept = keep_gif_mp4(&plan.work, &plan.dir, &plan.pattern, &plan.facts);
                notify("Frame Fang: GIF not made", &format!("FFmpeg is missing. The recording is kept as {}.",
                    kept.map(|p| p.display().to_string()).unwrap_or_else(|| "MP4".into())), true);
                return;
            };
            let (tx, rx) = std::sync::mpsc::channel();
            let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let c = cancel.clone();
            let handle = std::thread::spawn(move || {
                let _ = tx.send(gif_worker(ffmpeg, plan, &c));
            });
            self.gif_job = Some(GifJob { rx, cancel, handle });
            self.show_converting(true);
            notify("Frame Fang: making the GIF", "The recording stopped; the GIF follows in a moment.", false);
        }

        /// The tray's look while a GIF is being made.
        fn show_converting(&mut self, on: bool) {
            if let Some(t) = &self.tray {
                let _ = t.set_tooltip(&if on { "Frame Fang: making a GIF...".to_owned() } else { tray_tooltip(&self.settings) });
                t.set_converting(on);
            }
        }

        /// Cancel the GIF being made; its MP4 is kept.
        fn cancel_gif(&mut self) {
            if let Some(job) = &self.gif_job {
                job.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
            }
        }

        /// The GIF job's answer, once it has one.
        fn poll_gif_job(&mut self) {
            let Some(job) = &self.gif_job else { return };
            let result = match job.rx.try_recv() {
                Ok(r) => r,
                Err(std::sync::mpsc::TryRecvError::Empty) => return,
                // The worker died without an answer: its MP4 is in the work
                // folder, swept to recordings on the next start.
                Err(std::sync::mpsc::TryRecvError::Disconnected) =>
                    GifResult::Failed { error: "the GIF worker stopped".into(), kept: None },
            };
            self.gif_job = None;
            self.show_converting(false);
            match result {
                GifResult::Done(path) => {
                    notify("Frame Fang: GIF saved",
                        &format!("{}\n\nTray → \"Annotate last recording\" to draw on it.", path.display()), false);
                    self.last_recording = Some(path);
                }
                GifResult::Failed { error, kept } => {
                    let body = match &kept {
                        Some(p) if error == "cancelled" => format!("Cancelled. The recording is kept as {}.", p.display()),
                        Some(p) => format!("{error}\n\nThe recording is kept as {}.", p.display()),
                        None => format!("{error}\n\nThe recording is kept and moves to your recordings folder on the next start."),
                    };
                    notify("Frame Fang: GIF not made", &body, error != "cancelled");
                    if let Some(p) = kept { self.last_recording = Some(p); }
                }
            }
        }

        fn toggle_recording(&mut self, loop_target: &ActiveEventLoop) {
            if self.recorder.is_recording() {
                self.stop_recording();
            } else if self.gif_job.is_some() {
                self.refuse_if_recording_busy();
            } else {
                self.start_recording(
                    framefang_platform::recorder::RecordingOptions::NONE, loop_target, None);
            }
        }

        /// Start recording. Output lands in the user's Videos directory (or a
        /// fallback) as `framefang_<timestamp>.mp4`. Shows a desktop notification
        /// AND spawns a floating control window with a flashing red dot +
        /// timer + STOP button so the user has a one-click stop without hunting
        /// through the tray menu.
        ///
        /// `region` is `None` for the whole desktop, or the rectangle the user
        /// picked in the overlay. Audio is wired identically either way — the
        /// rectangle only narrows the video input.
        fn start_recording(&mut self,
                           opts: framefang_platform::recorder::RecordingOptions,
                           loop_target: &ActiveEventLoop,
                           region: Option<CaptureRect>) {
            // Reachable from the overlay's Record button, which is offered
            // in every capture session — including one opened while a
            // recording is running — so this has to say so, not just log.
            if self.refuse_if_recording_busy() { return; }
            if !self.have_ffmpeg(loop_target, "Recording") { return; }
            let dir   = recordings_directory_for(&self.settings);
            let (w, h) = region.map(|r| (r.width, r.height)).unwrap_or((0, 0));
            let facts = framefang_core::file_name::NameFacts::at(Local::now(), w, h);
            let gif = self.settings.record_format() == framefang_core::settings::RecordFormat::Gif;
            // A GIF is recorded as MP4 with no sound first, into a work
            // folder, and made into the GIF when the recording stops.
            let (out, opts) = if gif {
                let work = gif_work_dir();
                let _ = std::fs::create_dir_all(&work);
                let name = format!("{}-{}.mp4", Local::now().timestamp_millis(), std::process::id());
                (work.join(name), framefang_platform::recorder::RecordingOptions::NONE)
            } else {
                let _ = std::fs::create_dir_all(&dir);
                // The first free name; the recorder makes the file itself.
                match framefang_core::file_name::claim(&dir, &self.settings.file_name_pattern, &facts, "mp4") {
                    Ok((p, f)) => { drop(f); let _ = std::fs::remove_file(&p); (p, opts) }
                    Err(e) => {
                        notify("Frame Fang: recording failed", &format!("{}: {e}", dir.display()), true);
                        return;
                    }
                }
            };
            self.gif_plan = gif.then(|| GifPlan {
                work: out.clone(), dir: dir.clone(),
                pattern: self.settings.file_name_pattern.clone(), facts,
            });
            match self.recorder.start_region(out.clone(), opts, region) {
                Ok(effective) => {
                    // Describe what's actually in the file, not what the menu
                    // row asked for: audio capture is best-effort, so a box
                    // with no reachable audio server still records video, and
                    // promising a mic track that isn't there is worse than
                    // recording without one.
                    let audio_label = if gif { "GIF, no sound".to_owned() } else { audio_label_for(effective).to_string() };
                    let dropped     = audio_downgrade_note(opts, effective);
                    let saving_to   = if gif { dir.clone() } else { out.clone() };
                    let scope       = match region {
                        Some(r) => format!("{}x{} region", r.width, r.height),
                        None    => "full screen".to_string(),
                    };
                    eprintln!("Recording {scope} ({audio_label}) → {}", out.display());
                    if let Some(t) = &self.tray { t.set_recording(true); }
                    // Float a small flashing control panel so the user can stop
                    // the recording without opening the tray menu. Place it
                    // clear of the recorded rectangle: on Windows / macOS the
                    // panel excludes itself from screen capture anyway, but
                    // keeping it off the region also stops it covering the very
                    // thing being recorded.
                    //
                    // X11 has no capture-exclusion API, so there the panel is
                    // only safe when it lands entirely outside the recorded
                    // rectangle — which a region recording usually allows and a
                    // full-screen one never does. Wayland is worse: the
                    // compositor ignores the position we ask for, so "outside
                    // the rectangle" is a wish, not a fact, and the ScreenCast
                    // path has no exclusion either — no panel there at all.
                    // Best-effort either way: log + carry on if the OS won't
                    // give us another window.
                    let place = indicator_placement(
                        self.indicator_bounds(loop_target, region),
                        region,
                        crate::recording_indicator::SIZE,
                        24,
                    );
                    let capture_safe = cfg!(not(target_os = "linux"))
                        || (region.is_some() && place.clear_of_region
                            && !framefang_platform::session::is_wayland());
                    if capture_safe {
                        match RecordingIndicator::new_at(loop_target, (place.x, place.y)) {
                            Ok(v)  => self.recording_view = Some(v),
                            Err(e) => eprintln!("Recording indicator failed: {e} — stop via tray menu."),
                        }
                    } else if cfg!(target_os = "linux") {
                        eprintln!("Recording indicator skipped on Linux (it would be captured) — stop via tray menu.");
                    }
                    let stop_hint = if self.recording_view.is_some() {
                        "Click the floating STOP button (it won't appear in the video) or use the tray menu to finish."
                    } else {
                        "Use the tray menu to finish."
                    };
                    notify("Frame Fang: recording started",
                        &format!("{scope} - {audio_label}{dropped}\nSaving to {}\n\n{stop_hint}",
                            saving_to.display()),
                        true);
                }
                Err(e) => {
                    eprintln!("Recording failed to start: {e}");
                    self.gif_plan = None;
                    // Nothing recorded: no empty file is left under the name.
                    if std::fs::metadata(&out).map(|m| m.len() == 0).unwrap_or(false) {
                        let _ = std::fs::remove_file(&out);
                    }
                    // Do NOT block the event loop: a synchronous modal here
                    // freezes the capture hotkey + tray until it's dismissed.
                    // On Windows the error modal runs on its own thread
                    // (MessageBoxW is thread-safe and modal only to that
                    // thread), so capture keeps working while it's up; other
                    // platforms (GTK/AppKit dialogs are main-thread-only) get
                    // the non-blocking toast instead.
                    let msg = format!("{e}");
                    #[cfg(target_os = "windows")]
                    {
                        std::thread::spawn(move || {
                            rfd::MessageDialog::new()
                                .set_level(rfd::MessageLevel::Error)
                                .set_title("Frame Fang: recording failed")
                                .set_description(msg)
                                .show();
                        });
                    }
                    #[cfg(not(target_os = "windows"))]
                    {
                        notify("Frame Fang: recording failed", &msg, true);
                    }
                }
            }
        }

        /// The desktop rectangle the REC indicator is positioned within.
        ///
        /// A region was picked on a known desktop, so dodging it has to happen
        /// inside that same desktop. Without a region there is nothing to
        /// dodge, and the panel keeps its long-standing home in the primary
        /// monitor's top-right corner.
        fn indicator_bounds(&self,
                            loop_target: &ActiveEventLoop,
                            region: Option<CaptureRect>) -> DesktopBounds {
            if region.is_some() {
                if let Some(b) = self.overlay_desktop { return b; }
            }
            let size = loop_target.primary_monitor()
                .or_else(|| loop_target.available_monitors().next())
                .map(|m| m.size())
                .unwrap_or(winit::dpi::PhysicalSize::new(1920, 1080));
            DesktopBounds::new(0, 0, size.width, size.height)
        }

        /// Stop the active recording, finalize the file, and tear down the
        /// floating indicator if one is present.
        fn stop_recording(&mut self) {
            // Grabbed before `stop()` consumes it: when finalizing fails the
            // clip is often still on disk and playable, and this is the only
            // handle left to offer it to "Annotate last recording".
            let pending = self.recorder.output_path().map(|p| p.to_path_buf());
            let stopped = self.recorder.stop();
            // A GIF recording: whatever MP4 there is goes on to become the GIF.
            if let Some(plan) = self.gif_plan.take() {
                if let Some(t) = &self.tray { t.set_recording(false); }
                self.recording_view = None;
                let has_video = std::fs::metadata(&plan.work).map(|m| m.len() > 0).unwrap_or(false);
                if has_video {
                    self.start_gif_job(plan);
                } else {
                    let _ = std::fs::remove_file(&plan.work);
                    let why = stopped.err().map(|e| e.to_string()).unwrap_or_else(|| "nothing was recorded".into());
                    notify("Frame Fang: recording not saved", &why, true);
                }
                return;
            }
            match stopped {
                Ok(path) => {
                    eprintln!("Saved recording {}", path.display());
                    if let Some(t) = &self.tray { t.set_recording(false); }
                    notify("Frame Fang: recording saved",
                        &format!("{}\n\nTray → \"Annotate last recording\" to draw on it.", path.display()),
                        false);
                    self.last_recording = Some(path);
                }
                Err(e) => {
                    eprintln!("Stop recording failed: {e}");
                    if let Some(t) = &self.tray { t.set_recording(false); }
                    // A killed encoder still leaves bytes behind, and the error
                    // text now names the file — so make the menu able to reach
                    // it. Anything empty or missing is not offered: those are
                    // the failures where there is genuinely nothing to open.
                    if let Some(p) = pending.filter(|p| {
                        std::fs::metadata(p).map(|m| m.len() > 0).unwrap_or(false)
                    }) {
                        self.last_recording = Some(p);
                    }
                    // Surface it — a recording the user thought was running
                    // produced no usable file. Same non-blocking pattern as
                    // start_recording: threaded modal on Windows, toast elsewhere.
                    let msg = format!("{e}");
                    #[cfg(target_os = "windows")]
                    {
                        std::thread::spawn(move || {
                            rfd::MessageDialog::new()
                                .set_level(rfd::MessageLevel::Error)
                                .set_title("Frame Fang: recording not saved")
                                .set_description(msg)
                                .show();
                        });
                    }
                    #[cfg(not(target_os = "windows"))]
                    {
                        notify("Frame Fang: recording not saved", &msg, true);
                    }
                }
            }
            self.recording_view = None;
        }

        /// Tray → "Annotate last recording": open the most recent clip in
        /// the editor. ffmpeg extracts the first frame at video resolution,
        /// the user draws on it with the normal tools, and `commit` hands
        /// back an annotations-only transparent overlay that
        /// `burn_annotations` composites over the whole clip.
        fn annotate_last_recording(&mut self, loop_target: &ActiveEventLoop) {
            if self.capturing || self.overlay.is_some() { return; }
            if self.recording_busy() {
                notify("Frame Fang: annotate", "Stop the current recording first, and let its GIF finish.", true);
                return;
            }
            let video = match self.last_recording.clone().filter(|p| p.exists())
                .or_else(|| newest_recording(&recordings_directory_for(&self.settings)))
            {
                Some(p) => p,
                None => {
                    notify("Frame Fang: annotate",
                        "No recording found. Record something first.", true);
                    return;
                }
            };
            let frame = match extract_first_frame(&video) {
                Ok(f) => f,
                Err(e) => {
                    notify("Frame Fang: annotate failed", &format!("{e}"), true);
                    return;
                }
            };
            // Clip length feeds the editor's timeline. A parse failure
            // (Duration: N/A) just hides the timeline — annotate still
            // works as the static whole-clip editor.
            let duration = match probe_duration(&video) {
                Ok(d)  => Some(d),
                Err(e) => { eprintln!("framefang: duration probe failed: {e}"); None }
            };
            match Overlay::new_for_video(loop_target, frame, self.settings.clone(),
                                         video.clone(), duration) {
                Ok(ov) => {
                    self.overlay        = Some(ov);
                    self.video_annotate = Some(video);
                }
                Err(e) => notify("Frame Fang: annotate failed", &format!("{e}"), true),
            }
        }

        /// Compose the per-window overlays from the editor's commit payload
        /// and burn them over `video`, each gated to its own time window
        /// (the untimed default is a single whole-clip overlay). Both steps
        /// run on a background thread — composition spawns one ffmpeg seek
        /// per distinct window start and re-encoding a long clip can take a
        /// while; neither may freeze the tray/hotkey loop — and notifies on
        /// completion. The annotated copy lands next to the original as
        /// `<stem>_annotated.mp4`; the original is untouched.
        fn burn_annotations(&mut self, video: PathBuf, commit: VideoCommit) {
            let Some(ffmpeg) = crate::convert_video_form::locate_ffmpeg() else {
                notify("Frame Fang: annotate failed", crate::ffmpeg_fetch::missing_message(), true);
                return;
            };
            // A GIF stays a GIF: the burn makes an MP4 in the work folder,
            // which then becomes `<name>_annotated.gif`. Either way the
            // output name is claimed first, so no file is ever written over.
            let gif = video.extension().is_some_and(|e| e.eq_ignore_ascii_case("gif"));
            let wanted = annotated_output_path(&video);
            let dir = wanted.parent().map(|p| p.to_path_buf()).unwrap_or_else(std::env::temp_dir);
            let stem = wanted.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "annotated".into());
            let facts = framefang_core::file_name::NameFacts::at(Local::now(), 0, 0);
            let out = match framefang_core::file_name::claim(&dir, &stem, &facts, if gif { "gif" } else { "mp4" }) {
                Ok((p, f)) => { drop(f); p }
                Err(e) => { notify("Frame Fang: annotate failed", &format!("{}: {e}", dir.display()), true); return; }
            };
            notify("Frame Fang: annotating recording",
                &format!("Re-encoding with your annotations…\n→ {}", out.display()),
                false);
            std::thread::spawn(move || {
                let overlays = crate::editor::compose_overlay_groups(&commit, &video);
                let result = if gif {
                    let _ = std::fs::create_dir_all(gif_work_dir());
                    let temp = gif_work_dir().join(format!("{}-{}-annotated.mp4",
                        Local::now().timestamp_millis(), std::process::id()));
                    let made = run_annotation_burn(&ffmpeg, &video, &overlays, &temp).map_err(|e| e.to_string())
                        .and_then(|()| crate::gif::convert(&ffmpeg, &temp, &out, crate::gif::RECORDING,
                            &std::sync::atomic::AtomicBool::new(false)).map(|_| out.clone()));
                    let _ = std::fs::remove_file(&temp);
                    made
                } else {
                    run_annotation_burn(&ffmpeg, &video, &overlays, &out).map_err(|e| e.to_string()).map(|()| out.clone())
                };
                // Nothing made: the claimed name goes too.
                if result.is_err() && std::fs::metadata(&out).map(|m| m.len() == 0).unwrap_or(false) {
                    let _ = std::fs::remove_file(&out);
                }
                match result {
                    Ok(path) => notify("Frame Fang: annotated recording saved", &path.display().to_string(), false),
                    Err(e) => notify("Frame Fang: annotate failed", &e, true),
                }
            });
        }

        /// Open the configured screenshot save directory in the user's
        /// default file manager (xdg-open / open / Explorer). Mirrors C#
        /// `TrayContext.OpenSaveFolder` so the tray menu item lands the
        /// user where their screenshots actually live.
        fn open_save_folder(&self) {
            let dir = save_directory(&self.settings);
            open_url(&dir.to_string_lossy());
        }

        fn open_recordings_folder(&self) {
            let dir = recordings_directory_for(&self.settings);
            std::fs::create_dir_all(&dir).ok();
            open_url(&dir.to_string_lossy());
        }

        /// Open the native Settings dialog. Replaces the prior
        /// rfd-MessageDialog kludge with a real custom view that supports
        /// path pickers, toggle pills, hotkey rebinding and a theme cycle
        /// control. The view is
        /// constructed lazily so the GTK/Wayland surface only spins up when
        /// the user actually clicks "Settings…". A second click while the
        /// dialog is open just raises focus to the existing window.
        fn show_settings(&mut self, loop_target: &ActiveEventLoop) {
            self.open_settings(loop_target, false);
        }

        /// Whether FFmpeg is there for `what` ("Recording", "Video
        /// conversion"). When it isn't: on Windows, say so and open Settings
        /// with the FFmpeg row lit and its Download button ready; elsewhere,
        /// say how to install it.
        fn have_ffmpeg(&mut self, loop_target: &ActiveEventLoop, what: &str) -> bool {
            if crate::convert_video_form::locate_ffmpeg().is_some() { return true; }
            if crate::ffmpeg_fetch::offered() {
                notify(&format!("Frame Fang: {what} needs FFmpeg"),
                       "Download it from Settings with one click. If you already have FFmpeg, add it to PATH.",
                       false);
                match self.settings_view.as_mut() {
                    Some(v) => v.draw_attention_to_ffmpeg(),
                    None => self.open_settings(loop_target, true),
                }
            } else {
                notify(&format!("Frame Fang: {what} needs FFmpeg"), crate::ffmpeg_fetch::missing_message(), true);
            }
            false
        }

        fn open_settings(&mut self, loop_target: &ActiveEventLoop, ffmpeg_attention: bool) {
            if self.settings_view.is_some() { return; }
            // Unregister the global hotkey while Settings is open so the
            // rebind capture widget can receive the user's keystrokes
            // (including the current PrintScreen) without triggering a
            // capture. The hotkey is re-registered when the dialog closes,
            // whether the user Saves or Cancels.
            if let Some(hk) = self.hotkeys.as_mut() {
                hk.unregister_all();
            }
            match SettingsView::new(loop_target, self.settings.clone(), ffmpeg_attention) {
                Ok(view) => self.settings_view = Some(view),
                Err(e)   => {
                    eprintln!("Settings dialog failed to open: {e}");
                    // Window failed to open — re-register the hotkeys so
                    // the user isn't stranded with capture disabled.
                    if let Some(hk) = self.hotkeys.as_mut() {
                        apply_hotkeys(hk, &self.settings);
                    }
                }
            }
        }

        /// Called from the window event router after the settings dialog
        /// produces an outcome. `Changed` applies and saves at once (there is
        /// no Save button); `Closed` turns the shortcuts back on; `OpenJson`
        /// opens settings.json and leaves the dialog open. `true` closes it.
        fn apply_settings_outcome(&mut self, outcome: SettingsOutcome) -> bool {
            match outcome {
                SettingsOutcome::Patch(p) => {
                    self.store_patch(p);
                    false
                }
                SettingsOutcome::Replace(s) => {
                    self.store_replace(s);
                    false
                }
                SettingsOutcome::Closed => {
                    // `open_settings` turned every shortcut off so keys could
                    // be rebound; turn on the ones the user ended with.
                    self.register_hotkeys();
                    true
                }
                SettingsOutcome::OpenJson => {
                    if let Some(p) = AppSettings::settings_path() {
                        // Ensure the file exists so the editor has something
                        // to open; create it from current draft if missing.
                        if !p.exists() {
                            self.store_replace(self.settings.clone());
                        }
                        open_url(&p.to_string_lossy());
                    }
                    false
                }
            }
        }

        /// Make `new` the settings in use: the color scheme, the sign-in
        /// entry, the tray's tooltip and menu words, and the shortcuts. The
        /// shortcuts wait while Settings is open (it holds them off so keys
        /// can be rebound) and come on when it closes.
        fn apply_settings(&mut self, new: AppSettings) {
            let old = std::mem::replace(&mut self.settings, new);
            crate::palette::set(crate::palette::Scheme::parse(&self.settings.theme));
            framefang_platform::recorder::set_chosen_ffmpeg(self.settings.chosen_ffmpeg());
            // Turned off (Settings, a hand edit or an import): what was kept
            // is deleted.
            if old.keep_recent != self.settings.keep_recent {
                self.recent.set_enabled(self.settings.keep_recent);
            }
            if old.start_with_windows != self.settings.start_with_windows {
                if let Err(e) = crate::autostart::set(self.settings.start_with_windows) {
                    eprintln!("start with system: {e}");
                    notify("Frame Fang: start with system", &e, true);
                }
            }
            // Every surface that quotes a hotkey (the hover tooltip and the
            // menu rows) is rebuilt in place, so a rebind shows at once.
            if let Some(t) = &self.tray {
                if self.gif_job.is_none() {
                    if let Err(e) = t.set_tooltip(&tray_tooltip(&self.settings)) {
                        eprintln!("Tray tooltip update failed: {e}");
                    }
                }
                t.set_hotkey_labels(&menu_labels(&self.settings, self.portal_hotkey));
                t.set_gif(self.settings.record_format() == framefang_core::settings::RecordFormat::Gif);
            }
            let keys = |s: &AppSettings| (s.hotkey_bindings(), s.switch_key.clone());
            if self.settings_view.is_none() && keys(&old) != keys(&self.settings) {
                self.register_hotkeys();
            }
        }

        /// Register the configured shortcuts. A rebind the user just made is
        /// the one failure they will notice and least expect (on the portal
        /// backend it can be refused outright), so say so where they look.
        fn register_hotkeys(&mut self) {
            if let Some(hk) = self.hotkeys.as_mut() {
                for (action, e) in apply_hotkeys(hk, &self.settings) {
                    notify("Frame Fang: couldn't set that shortcut",
                           &format!("{}: {e}", action.label()), true);
                }
            }
        }

        /// Save a change: `patch` is applied to `settings.json` as it is on
        /// disk now (see `settings_store`), and the result becomes the
        /// settings everywhere (tray, Settings window, open editor). While
        /// the file doesn't parse the change is used but kept back, and
        /// written once the file is fixed.
        fn store_patch(&mut self, patch: framefang_core::settings_store::Patch) {
            use framefang_core::settings_store::{self as store, StoreError};
            let mut all = self.pending_patch.take().unwrap_or_default();
            store::merge(&mut all, patch);
            if all.is_empty() { return; }
            match store::update(&all) {
                Ok((new, refused, stamp)) => {
                    self.settings_stamp = Some(stamp);
                    self.settings_bad_reported = None;
                    self.settings_io_failing = false;
                    if !refused.is_empty() { eprintln!("framefang: settings refused {refused:?}"); }
                    self.adopt_settings(new);
                }
                Err(e) => {
                    let (in_use, _) = store::apply(&self.settings, &all);
                    match &e {
                        StoreError::Unparsable(_) => {
                            self.pending_patch = Some(all);
                            let now = store::path().and_then(|p| store::stamp_of(&p));
                            if self.settings_bad_reported != now {
                                self.settings_bad_reported = now;
                                notify("Frame Fang: settings.json has a mistake",
                                    &format!("{e}. Your change is in use and is saved once the file is fixed."), true);
                            }
                        }
                        StoreError::Io(io) => {
                            // Kept and tried again every second (the file may
                            // only be locked for a moment by a scanner).
                            eprintln!("Failed to persist settings: {io}");
                            if !self.settings_io_failing {
                                self.settings_io_failing = true;
                                notify("Frame Fang: settings not saved yet",
                                    &framefang_core::failure::settings_write_failure(&io.to_string()), true);
                            }
                            self.pending_patch = Some(all);
                        }
                    }
                    self.adopt_settings(in_use);
                }
            }
        }

        /// Import: these settings replace the file.
        fn store_replace(&mut self, s: AppSettings) {
            let mut s = s;
            s.clamp();
            match framefang_core::settings_store::replace(&s) {
                Ok(stamp) => {
                    self.settings_stamp = Some(stamp);
                    self.pending_patch = None;
                }
                Err(e) => {
                    notify("Frame Fang: settings not saved yet",
                        &framefang_core::failure::settings_write_failure(&e.to_string()), true);
                    // Every key, so the retry writes the whole import.
                    self.pending_patch = Some(framefang_core::settings_store::whole(&s));
                }
            }
            self.adopt_settings(s);
        }

        /// Make `new` the settings of every holder: the Settings window (its
        /// draft and the base it diffs from), an open editor (keeping its own
        /// changes not yet sent), and the tray itself.
        fn adopt_settings(&mut self, new: AppSettings) {
            if let Some(v) = self.settings_view.as_mut() { v.replace_draft(new.clone()); }
            if let Some(ov) = self.overlay.as_mut() { ov.adopt_settings(&new); }
            self.apply_settings(new);
        }

        /// Save what the open editor changed (a tool, a color, the capture
        /// mode). Called after every call into it.
        fn drain_overlay_patch(&mut self) {
            if let Some(p) = self.overlay.as_mut().and_then(|o| o.take_patch()) {
                self.store_patch(p);
            }
        }

        /// Pick up a hand edit of `settings.json` (Edit as JSON, or any
        /// editor) within a second: apply it everywhere. A file that doesn't
        /// parse changes nothing, and says what is wrong once per version.
        fn watch_settings_file(&mut self) {
            use framefang_core::settings_store::{self as store, StoreError};
            if self.settings_checked.elapsed() < Duration::from_secs(1) { return; }
            self.settings_checked = Instant::now();
            // A change that couldn't be written (a locked file) is tried again.
            if let Some(p) = self.pending_patch.take() {
                let was_locked = !matches!(store::read(&store::path().unwrap_or_default()), Err(StoreError::Unparsable(_)));
                if was_locked { self.store_patch(p); return; }
                self.pending_patch = Some(p);
            }
            let Some(path) = store::path() else { return };
            let now = store::stamp_of(&path);
            if now.is_none() || now == self.settings_stamp { return; }
            self.settings_stamp = now;
            match store::read(&path) {
                Ok((s, _)) => {
                    self.settings_bad_reported = None;
                    // A change held back while the file was broken goes in now.
                    if let Some(p) = self.pending_patch.take() { self.store_patch(p); return; }
                    if s != self.settings { self.adopt_settings(s); }
                }
                Err(StoreError::Unparsable(e)) => {
                    if self.settings_bad_reported == now { return; }
                    self.settings_bad_reported = now;
                    notify("Frame Fang: settings.json has a mistake",
                        &format!("{e}. Frame Fang keeps your current settings until it's fixed."), true);
                }
                Err(StoreError::Io(_)) => {}
            }
        }

        /// Themed About modal — mirrors the brand chrome of Settings.
        /// Lazily constructed; a second tray-click while the dialog is
        /// open is a no-op.
        fn show_about(&mut self, loop_target: &ActiveEventLoop) {
            if self.about_view.is_some() { return; }
            match AboutView::new(loop_target) {
                Ok(v)  => self.about_view = Some(v),
                Err(e) => eprintln!("About dialog failed to open: {e}"),
            }
        }

        /// Put the welcome window up on the first launch, once.
        ///
        /// Sequenced after `ensure_tray` on purpose: the window's whole job is
        /// to point at the tray icon, so it must not open before the icon
        /// exists. When the tray failed to build, `ensure_tray` has already
        /// raised the About window as the fallback surface and closing that
        /// one quits — stacking a second window on top of it would only give
        /// the user two things to dismiss and one misleading instruction.
        fn maybe_show_first_run(&mut self, loop_target: &ActiveEventLoop) {
            if !self.first_run || !self.tray_built { return; }
            self.first_run = false;
            if self.tray.is_none() { return; }
            let save_dir = save_directory(&self.settings).to_string_lossy().to_string();
            match FirstRunView::new(loop_target,
                                    &self.settings.hotkey().describe(),
                                    &save_dir,
                                    self.wayland) {
                Ok(v)  => self.firstrun_view = Some(v),
                Err(e) => eprintln!("Welcome window failed to open: {e}"),
            }
        }

        /// Themed Check-for-updates modal. Spins up a background curl on
        /// open and shows the installed + latest tag side-by-side once the
        /// fetch resolves. The "Open releases page" button always works
        /// even if the fetch fails.
        fn show_updates(&mut self, loop_target: &ActiveEventLoop) {
            if self.updates_view.is_some() { return; }
            match UpdatesView::new(loop_target) {
                Ok(v)  => self.updates_view = Some(v),
                Err(e) => eprintln!("Updates dialog failed to open: {e}"),
            }
        }

        /// Open the themed image-conversion dialog. Lazy-constructed; a
        /// second click while it's already open is a no-op so the user
        /// can't accidentally stack windows.
        fn show_convert_image(&mut self, loop_target: &ActiveEventLoop) {
            if self.convert_image_view.is_some() { return; }
            match ConvertImageView::new(loop_target) {
                Ok(v)  => self.convert_image_view = Some(v),
                Err(e) => eprintln!("Convert-image dialog failed to open: {e}"),
            }
        }

        fn show_convert_video(&mut self, loop_target: &ActiveEventLoop) {
            if self.convert_video_view.is_some() { return; }
            if !self.have_ffmpeg(loop_target, "Video conversion") { return; }
            match ConvertVideoView::new(loop_target) {
                Ok(v)  => self.convert_video_view = Some(v),
                Err(e) => eprintln!("Convert-video dialog failed to open: {e}"),
            }
        }

        /// Kick off the in-process self-updater. The actual work runs on
        /// a worker thread spawned by `self_updater::download_and_install`;
        /// we just stash the join handle and poll it from `poll()`. On
        /// success the worker calls `exit(0)` so the new binary takes
        /// over — control here only returns on failure.
        ///
        /// While the job runs we close the updates dialog so the user
        /// gets a visible "something is happening" via stderr / the new
        /// binary taking over the tray. A future PR can wire a progress
        /// modal here using the `on_progress` callback.
        fn start_self_update(&mut self, asset_url: String, expected_sha256: Option<String>) {
            if self.update_job.is_some() {
                eprintln!("self-updater: an update is already in progress");
                return;
            }
            // The Updates dialog stays open and shows the download, then the
            // install; Framefang restarts on the new version by itself.

            eprintln!(
                "self-updater: downloading {asset_url} (verify_sha256 = {})",
                if expected_sha256.is_some() { "yes" } else { "no — skipping" }
            );

            let job = self_updater::download_and_install(
                asset_url,
                expected_sha256,
                |downloaded, total| {
                    // Progress callback runs on the worker thread; we
                    // can't touch winit state from here. Cheap stderr
                    // line is enough until we add a transient modal.
                    match total {
                        Some(t) if t > 0 => {
                            eprintln!("self-updater: {downloaded} / {t} bytes");
                        }
                        _ => eprintln!("self-updater: {downloaded} bytes"),
                    }
                },
            );
            self.update_job = Some(job);
        }

        /// Pump the update-job handle. If it's finished, take the result
        /// — a success arm here actually means the worker thread *failed*
        /// after the relaunch, because on a real success the worker
        /// calls `exit(0)` and never lets the JoinHandle resolve.
        fn poll_update_job(&mut self) {
            let finished = self.update_job
                .as_ref()
                .map(|h| h.is_finished())
                .unwrap_or(false);
            if !finished { return; }
            let job = self.update_job.take().unwrap();
            match job.join() {
                Ok(Ok(())) => {
                    // Reachable only if `spawn_and_exit` was prevented from
                    // exiting — shouldn't happen, but log instead of panic.
                    eprintln!("self-updater: job returned Ok without exiting");
                }
                Ok(Err(msg)) => {
                    eprintln!("self-updater: failed: {msg}");
                    // In the dialog that started it, with Try again; a box
                    // of its own only if that dialog was closed meanwhile.
                    match self.updates_view.as_mut() {
                        Some(v) => v.update_failed(msg),
                        None => {
                            rfd::MessageDialog::new()
                                .set_level(rfd::MessageLevel::Error)
                                .set_title("Frame Fang update failed")
                                .set_description(msg)
                                .show();
                        }
                    }
                }
                Err(_) => {
                    eprintln!("self-updater: worker thread panicked");
                }
            }
        }
    }

    impl ApplicationHandler for TrayApp {
        fn resumed(&mut self, loop_target: &ActiveEventLoop) {
            // Earliest point at which the platform event loop is guaranteed to
            // be running, which is what the tray status item needs.
            self.ensure_tray(loop_target);
        }

        fn window_event(&mut self, _loop_target: &ActiveEventLoop, id: WindowId, ev: WindowEvent) {
            // Route the event into the overlay editor if it owns the window.
            // Anything else (close-requested on a phantom window, stray events
            // from a destroyed surface) is silently dropped.
            // Decide the routing target *before* consuming the event so we
            // only dispatch once. Overlay wins if it owns the id; otherwise
            // search the pinned-window list.
            enum Target { Overlay, ColorPick, Pinned(usize), Settings, Recording, About, FirstRun, Updates, ConvertImage, ConvertVideo, Toast, ScrollHud, TrayMenu, Unknown }
            let target = if self.tray_menu.as_ref().is_some_and(|m| m.window_id() == id) {
                Target::TrayMenu
            } else if self.color_pick.as_ref().is_some_and(|c| c.window_id() == id) {
                Target::ColorPick
            } else if self.overlay.as_ref().map(|ov| ov.window_id() == id).unwrap_or(false) {
                Target::Overlay
            } else if self.toast.as_ref().is_some_and(|t| t.window_id() == id) {
                Target::Toast
            } else if self.scroll_hud.as_ref().is_some_and(|h| h.window_id() == id) {
                Target::ScrollHud
            } else if self.settings_view.as_ref().map(|s| s.window_id() == id).unwrap_or(false) {
                Target::Settings
            } else if self.recording_view.as_ref().map(|r| r.window_id() == id).unwrap_or(false) {
                Target::Recording
            } else if self.about_view.as_ref().map(|a| a.window_id() == id).unwrap_or(false) {
                Target::About
            } else if self.firstrun_view.as_ref().map(|f| f.window_id() == id).unwrap_or(false) {
                Target::FirstRun
            } else if self.updates_view.as_ref().map(|u| u.window_id() == id).unwrap_or(false) {
                Target::Updates
            } else if self.convert_image_view.as_ref().map(|c| c.window_id() == id).unwrap_or(false) {
                Target::ConvertImage
            } else if self.convert_video_view.as_ref().map(|c| c.window_id() == id).unwrap_or(false) {
                Target::ConvertVideo
            } else if let Some((i, _)) = self.pinned.iter().enumerate().find(|(_, p)| p.window_id() == id) {
                Target::Pinned(i)
            } else {
                Target::Unknown
            };

            let mut drop_overlay = false;
            let mut accepted:     Option<image::ImageBuffer<image::Rgba<u8>, Vec<u8>>> = None;
            let mut accepted_video: Option<VideoCommit> = None;
            let mut copied:       Option<image::ImageBuffer<image::Rgba<u8>, Vec<u8>>> = None;
            let mut save_as:      Option<image::ImageBuffer<image::Rgba<u8>, Vec<u8>>> = None;
            // Where the "Copied" pill goes, read off the overlay before it is
            // dropped. Only a copy that closes the window gets one.
            let mut toast_spot:   Option<(i32, i32)> = None;
            let mut wants_color_pick = false;
            let mut pinned_payload: Option<(image::ImageBuffer<image::Rgba<u8>, Vec<u8>>, (i32, i32))> = None;
            let mut record_region: Option<(i32, i32, i32, i32)> = None;
            let mut chosen: Option<Box<ChosenRegion>> = None;
            let mut drop_pin: Option<usize> = None;
            let mut settings_outcomes: Vec<SettingsOutcome> = Vec::new();
            let mut picked: Option<(i32, i32, i32, i32)> = None;
            let mut read_request: Option<(image::RgbaImage, crate::read_text::ReadKind, bool)> = None;
            let mut scroll_request: Option<((i32, i32, i32, i32), bool)> = None;

            match target {
                Target::Overlay => {
                    if let Some(ov) = self.overlay.as_mut() {
                        let out = ov.handle_event(ev);
                        // A picture leaving an in-place session: its final
                        // selection is the region to take again.
                        if matches!(out, OverlayOutcome::Copied(_) | OverlayOutcome::CopiedOpen(_)
                            | OverlayOutcome::Accepted(_) | OverlayOutcome::SaveAs(_) | OverlayOutcome::Pinned(..)) {
                            if let Some(r) = ov.picker_rect() { picked = Some(r); }
                        }
                        match out {
                            OverlayOutcome::Continue        => {}
                            OverlayOutcome::Cancelled       => { drop_overlay = true; }
                            OverlayOutcome::Accepted(img)   => { drop_overlay = true; accepted = Some(img); }
                            OverlayOutcome::SaveAs(img)     => { save_as = Some(img); }
                            OverlayOutcome::AcceptedVideo(commit) => { drop_overlay = true; accepted_video = Some(commit); }
                            OverlayOutcome::Copied(img)     => { drop_overlay = true; copied   = Some(img); }
                            OverlayOutcome::CopiedOpen(img) => { copied = Some(img); }
                            OverlayOutcome::Pinned(img, p)  => { drop_overlay = true; pinned_payload = Some((img, p)); }
                            OverlayOutcome::RecordRegion(r) => { drop_overlay = true; record_region = Some(r); }
                            OverlayOutcome::Chosen(c)       => { drop_overlay = true; chosen = Some(c); }
                            OverlayOutcome::PickScreenColor => { wants_color_pick = true; }
                            OverlayOutcome::ReadText(img, kind, flip) => { read_request = Some((img, kind, flip)); }
                            OverlayOutcome::Scroll(area, flip) => { drop_overlay = true; scroll_request = Some((area, flip)); }
                            // Only a color-pick session answers with a color.
                            OverlayOutcome::ColorPicked(_)  => {}
                        }
                        // Read here, on the event that picked it: the overlay
                        // may be dropped below.
                        if let Some(r) = ov.take_picked_rect() { picked = Some(r); }
                    }
                }
                Target::ColorPick => {
                    let answer = self.color_pick.as_mut().map(|c| c.handle_event(ev));
                    match answer {
                        Some(OverlayOutcome::ColorPicked(c)) => {
                            self.color_pick = None;
                            if let Some(ov) = self.overlay.as_mut() { ov.take_screen_color(c); }
                        }
                        Some(OverlayOutcome::Cancelled) => {
                            self.color_pick = None;
                            if let Some(ov) = self.overlay.as_mut() { ov.screen_color_cancelled(); }
                        }
                        _ => {}
                    }
                }
                Target::Settings => {
                    if let Some(view) = self.settings_view.as_mut() {
                        view.handle_event(ev);
                        settings_outcomes.extend(view.take_outcomes());
                    }
                }
                Target::Recording => {
                    let mut stop_now = false;
                    if let Some(view) = self.recording_view.as_mut() {
                        view.handle_event(ev);
                        stop_now = view.stop_requested;
                    }
                    if stop_now { self.stop_recording(); }
                }
                Target::About => {
                    let mut outcome: Option<AboutOutcome> = None;
                    if let Some(view) = self.about_view.as_mut() {
                        view.handle_event(ev);
                        outcome = view.outcome.take();
                    }
                    if let Some(o) = outcome {
                        match o {
                            AboutOutcome::Closed => {
                                self.about_view = None;
                                // When the tray failed to build, this window
                                // was the app's only visible surface and the
                                // tray's Exit item — the sole exit path in the
                                // crate — doesn't exist. Dismissing it has to
                                // quit, or the process lingers unkillable from
                                // the UI.
                                if self.tray_built && self.tray.is_none() {
                                    _loop_target.exit();
                                }
                            }
                            AboutOutcome::OpenProject => { open_url("https://github.com/winters27/framefang"); }
                            AboutOutcome::OpenAuthor  => { open_url("https://github.com/winters27"); }
                            AboutOutcome::OpenUpdates => {
                                self.about_view = None;
                                self.show_updates(_loop_target);
                                // Handing the fallback surface over to the
                                // Updates dialog only works if that dialog
                                // actually opened; if it didn't, nothing is
                                // left on screen to quit from.
                                if self.tray_built && self.tray.is_none()
                                    && self.updates_view.is_none() {
                                    _loop_target.exit();
                                }
                            }
                        }
                    }
                }
                Target::FirstRun => {
                    let mut outcome: Option<FirstRunOutcome> = None;
                    if let Some(view) = self.firstrun_view.as_mut() {
                        view.handle_event(ev);
                        outcome = view.outcome.take();
                    }
                    // Dismissing the welcome window leaves the app running in
                    // the tray — that is the whole point of the window.
                    if matches!(outcome, Some(FirstRunOutcome::Closed)) {
                        self.firstrun_view = None;
                    }
                }
                Target::Updates => {
                    let mut outcome: Option<UpdatesOutcome> = None;
                    if let Some(view) = self.updates_view.as_mut() {
                        view.handle_event(ev);
                        outcome = view.outcome.take();
                    }
                    if let Some(o) = outcome {
                        match o {
                            UpdatesOutcome::Closed => {
                                self.updates_view = None;
                                // Same last-surface rule as the About window:
                                // reaching here from the no-tray fallback (About
                                // -> "Check for updates" closes About) leaves
                                // this dialog as the only way to reach the
                                // process, so dismissing it has to quit.
                                if self.tray_built && self.tray.is_none()
                                    && self.about_view.is_none() {
                                    _loop_target.exit();
                                }
                            }
                            UpdatesOutcome::OpenReleases => { open_url("https://github.com/winters27/framefang/releases"); }
                            UpdatesOutcome::OpenAsset(url) => { open_url(&url); }
                            UpdatesOutcome::DownloadAndInstall { asset_url, expected_sha256 } => {
                                self.start_self_update(asset_url, expected_sha256);
                            }
                            // Package-managed install (Snap, Flatpak, brew,
                            // deb/rpm/AUR, Scoop): the dialog offered the
                            // upgrade command instead of a download, so all
                            // we do is put it on the clipboard.
                            UpdatesOutcome::CopyCommand(cmd) => {
                                match framefang_platform::copy_text(&cmd) {
                                    Ok(()) => notify("Update command copied", &cmd, false),
                                    // The command is the only thing this
                                    // dialog offers on a package-managed
                                    // install, so a failed copy must still
                                    // show it somewhere the user can read.
                                    Err(e) => notify("Frame Fang: copy failed",
                                        &format!("Couldn't copy the update command to the clipboard: {e}\n\nRun this yourself:\n{cmd}"),
                                        true),
                                }
                            }
                        }
                    }
                }
                Target::ConvertImage => {
                    let mut outcome: Option<ConvertImageOutcome> = None;
                    if let Some(view) = self.convert_image_view.as_mut() {
                        view.handle_event(ev);
                        outcome = view.outcome.take();
                    }
                    if matches!(outcome, Some(ConvertImageOutcome::Closed)) {
                        self.convert_image_view = None;
                    }
                }
                Target::ConvertVideo => {
                    let mut outcome: Option<ConvertVideoOutcome> = None;
                    if let Some(view) = self.convert_video_view.as_mut() {
                        view.handle_event(ev);
                        outcome = view.outcome.take();
                    }
                    if matches!(outcome, Some(ConvertVideoOutcome::Closed)) {
                        self.convert_video_view = None;
                    }
                }
                Target::Pinned(i) => {
                    if let Some(p) = self.pinned.get_mut(i) {
                        if p.handle_event(ev) {
                            drop_pin = Some(i);
                        }
                    }
                }
                Target::Toast => {
                    if matches!(ev, WindowEvent::RedrawRequested) {
                        if let Some(t) = self.toast.as_mut() { t.redraw(); }
                    }
                }
                Target::ScrollHud => {
                    if matches!(ev, WindowEvent::RedrawRequested) {
                        if let Some(h) = self.scroll_hud.as_mut() { h.redraw(); }
                    }
                }
                Target::TrayMenu => {
                    if let Some(m) = self.tray_menu.as_mut() { m.handle_event(ev); }
                    self.finish_tray_menu(_loop_target);
                }
                Target::Unknown => {}
            }

            // The editor's eyedropper: freeze the whole screen, editor and
            // all, and let the user pick a color from anywhere on it.
            if wants_color_pick {
                match capture_all_screens() {
                    Ok(shot) => match Overlay::new_for_color_pick(
                        _loop_target, shot.geometry(), shot.bitmap, self.settings.clone())
                    {
                        Ok(mut c) => {
                            c.set_display_map(shot.map);
                            self.color_pick = Some(c);
                        }
                        Err(e) => {
                            eprintln!("framefang: color pick open failed: {e}");
                            if let Some(ov) = self.overlay.as_mut() { ov.screen_color_cancelled(); }
                        }
                    },
                    Err(e) => {
                        eprintln!("framefang: color pick capture failed: {e}");
                        if let Some(ov) = self.overlay.as_mut() { ov.screen_color_cancelled(); }
                    }
                }
            }

            // Save As asks with the overlay out of the way, and brings it back
            // if the user cancels.
            if let Some(img) = save_as {
                if let Some(ov) = self.overlay.as_mut() { ov.set_hidden(true); }
                if self.save_final_as(&img) {
                    drop_overlay = true;
                } else if let Some(ov) = self.overlay.as_mut() {
                    ov.set_hidden(false);
                }
            }

            if drop_overlay && copied.is_some() {
                toast_spot = self.overlay.as_ref().and_then(|ov| ov.notice_spot(crate::toast::size()));
            }

            // Whatever the editor changed in this event (a tool, a knob, the
            // capture mode) is saved before it can be dropped.
            self.drain_overlay_patch();
            if let Some(r) = picked {
                self.session_facts = Some(facts_at(&self.session_windows, r));
                self.remember_region(r);
            }

            if let Some((img, kind, flip)) = read_request { self.start_read(img, kind, flip); }

            if drop_overlay {
                self.overlay = None;
                // A color-pick session has no editor left to answer.
                self.color_pick = None;
                // Nor a read one waiting on its answer.
                self.read_job = None;
                // A cancelled region-selection session must not leave its audio
                // choice armed for whatever overlay commits next.
                if record_region.is_none() { self.pending_region_audio = None; }
                // A video-annotate session ends with its overlay. Only
                // `AcceptedVideo` carries the burn payload (routed below) —
                // Cancelled / Copied / Pinned just end the session.
                if accepted.is_none() && accepted_video.is_none() { self.video_annotate = None; }
            }
            if let Some(i) = drop_pin { self.pinned.swap_remove(i); }
            // The frozen screen is gone: the real page can scroll now.
            if let Some((area, flip)) = scroll_request { self.start_scroll(_loop_target, area, flip); }
            // The picker let go of the desktop; the picture opens in its own
            // editor window where it was on screen.
            if let Some(c) = chosen {
                let copy_first = self.region_copies_first();
                self.open_chosen(_loop_target, *c, copy_first);
            }
            if let Some(commit) = accepted_video {
                if let Some(video) = self.video_annotate.take() {
                    self.burn_annotations(video, commit);
                }
            }
            if let Some(img) = accepted {
                // Screenshot path only — video commits arrive as AcceptedVideo.
                // Auto-copy: the finished picture replaces the raw one copied
                // when the region was picked.
                if self.settings.auto_copy { self.copy_final(img.clone()); }
                self.save_final(img);
            }
            if let Some(img) = copied { self.copy_with_toast(_loop_target, img, toast_spot); }
            if let Some((img, pos)) = pinned_payload {
                self.keep_recent(&img);
                if self.settings.auto_copy { self.copy_final(img.clone()); }
                match PinView::new(_loop_target, img, pos) {
                    Ok(pv)  => self.pinned.push(pv),
                    Err(e)  => {
                        eprintln!("Pin failed: {e}");
                        notify("Frame Fang: pin failed",
                            &framefang_core::failure::pin_failure(&e.to_string()), true);
                    }
                }
            }
            for out in settings_outcomes {
                if self.apply_settings_outcome(out) {
                    self.settings_view = None;
                    break;
                }
            }
            // Last, so the overlay window is already dropped: the encoder must
            // not catch the dimmed selection surface in its first frames.
            if let Some(rect) = record_region {
                self.start_region_from_selection(rect, _loop_target);
            }
        }

        fn about_to_wait(&mut self, loop_target: &ActiveEventLoop) {
            // Backstop for platforms/versions where `resumed` never fires:
            // no-op once the tray has been built.
            self.ensure_tray(loop_target);
            // Always after `ensure_tray`, and only here rather than in
            // `resumed` too, so the welcome window can never be raised on a
            // tick where the tray icon doesn't exist yet.
            self.maybe_show_first_run(loop_target);
            // The "Copied" pill's fade is timed, so every pass steps it.
            if self.toast.as_mut().is_some_and(|t| !t.tick()) { self.toast = None; }
            // Poll roughly 30Hz when idle.
            let now = Instant::now();
            if now.duration_since(self.last_tick) >= Duration::from_millis(33) {
                self.poll(loop_target);
                self.last_tick = now;
            }
            loop_target.set_control_flow(ControlFlow::WaitUntil(now + Duration::from_millis(33)));
        }
    }

    let event_loop = EventLoop::new().map_err(|e| anyhow!("EventLoop::new: {e}"))?;
    event_loop.set_control_flow(ControlFlow::Wait);

    let keep_recent = settings.keep_recent;
    let mut app = TrayApp {
        settings,
        hotkeys: hotkeys.take(),
        tray: None,
        tray_built: false,
        wayland,
        hotkey_error,
        portal_hotkey,
        recorder: Recorder::new(),
        overlay: None,
        pinned:  Vec::new(),
        toast:   None,
        tray_menu: None,
        tray_menu_closed: None,
        settings_stamp: framefang_core::settings_store::path()
            .and_then(|p| framefang_core::settings_store::stamp_of(&p)),
        settings_checked: Instant::now(),
        settings_bad_reported: None,
        pending_patch: None,
        settings_io_failing: false,
        app_state: framefang_core::app_state::AppState::load(),
        gif_plan: None,
        gif_job: None,
        read_job: None,
        read_gen: 0,
        recent: crate::recent::Recent::start(crate::recent::dir(), keep_recent),
        recent_list: Default::default(),
        scroll: None,
        scroll_hud: None,
        scroll_flip: false,
        session_id: 0,
        tray_click_at: None,
        session_windows: Vec::new(),
        session_facts: None,
        color_pick: None,
        settings_view:  None,
        recording_view: None,
        about_view:     None,
        first_run,
        firstrun_view:  None,
        updates_view:   None,
        convert_image_view: None,
        convert_video_view: None,
        update_job: None,
        last_tick: Instant::now(),
        capturing: false,
        last_recording: None,
        overlay_desktop: None,
        started_at: Instant::now(),
        pending_region_audio: None,
        video_annotate: None,
    };

    sweep_gif_work(recordings_directory_for(&app.settings), app.settings.file_name_pattern.clone());
    event_loop.run_app(&mut app).map_err(|e| anyhow!("run_app: {e}"))?;

    // Every change was saved as it was made; only one held back by a
    // settings.json that didn't parse is still waiting.
    if let Some(p) = app.pending_patch.take() {
        let _ = framefang_core::settings_store::update(&p);
    }
    // A GIF still being made stops; its MP4 is kept in the recordings folder
    // (or, if even that can't finish in time, swept there on the next start).
    if let Some(job) = app.gif_job.take() {
        job.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        let until = Instant::now() + Duration::from_secs(2);
        while !job.handle.is_finished() && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    Ok(())
}

// ── helpers ─────────────────────────────────────────────────────────────────

/// A GIF recording in progress: its MP4 work file and how the GIF is named.
struct GifPlan {
    work: PathBuf,
    dir: PathBuf,
    pattern: String,
    facts: framefang_core::file_name::NameFacts,
}

/// A recording being made into a GIF on its own thread.
struct GifJob {
    rx: std::sync::mpsc::Receiver<GifResult>,
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    handle: std::thread::JoinHandle<()>,
}

enum GifResult {
    Done(PathBuf),
    /// The GIF failed or was cancelled; the MP4 was kept here (`None`: left
    /// in the work folder for the next start's sweep).
    Failed { error: String, kept: Option<PathBuf> },
}

/// Where GIF recordings wait as MP4 until they are converted.
fn gif_work_dir() -> PathBuf {
    directories::ProjectDirs::from("org", "framefang", "Framefang")
        .map(|d| d.cache_dir().join("gif-work"))
        .unwrap_or_else(|| std::env::temp_dir().join("framefang-gif-work"))
}

/// Move `from` to `to` (which may exist, empty, as a claimed name), also
/// across drives: a rename, else a copy and a delete.
fn move_file(from: &std::path::Path, to: &std::path::Path) -> std::io::Result<()> {
    if std::fs::rename(from, to).is_ok() { return Ok(()); }
    // Across drives: copy beside the target under a work name and rename it
    // only once whole, so a quit mid-copy never leaves a cut-off file under
    // the real name (the start-up sweep removes the work name).
    let part = part_path(to);
    let copied = std::fs::copy(from, &part).and_then(|_| std::fs::rename(&part, to));
    match copied {
        Ok(()) => { let _ = std::fs::remove_file(from); Ok(()) }
        Err(e) => { let _ = std::fs::remove_file(&part); let _ = std::fs::remove_file(to); Err(e) }
    }
}

/// The work name a file is copied under before it is renamed into place.
fn part_path(to: &std::path::Path) -> PathBuf {
    let mut p = to.as_os_str().to_owned();
    p.push(".framefang-part");
    p.into()
}

/// Keep a GIF recording's MP4 in `dir` under a name of its own.
fn keep_gif_mp4(work: &std::path::Path, dir: &std::path::Path, pattern: &str,
                facts: &framefang_core::file_name::NameFacts) -> Option<PathBuf> {
    let _ = std::fs::create_dir_all(dir);
    let (path, file) = framefang_core::file_name::claim(dir, pattern, facts, "mp4").ok()?;
    drop(file);
    move_file(work, &path).ok().map(|_| path)
}

/// Make `plan`'s MP4 into the GIF, then name it. Runs on its own thread.
fn gif_worker(ffmpeg: PathBuf, plan: GifPlan, cancel: &std::sync::atomic::AtomicBool) -> GifResult {
    let part = plan.work.with_extension("gif.framefang-part");
    let made = crate::gif::convert(&ffmpeg, &plan.work, &part, crate::gif::RECORDING, cancel);
    let fail = |error: String| GifResult::Failed {
        error,
        kept: keep_gif_mp4(&plan.work, &plan.dir, &plan.pattern, &plan.facts),
    };
    if let Err(e) = made { return fail(e); }
    let _ = std::fs::create_dir_all(&plan.dir);
    let claimed = framefang_core::file_name::claim(&plan.dir, &plan.pattern, &plan.facts, "gif");
    match claimed {
        Ok((path, file)) => {
            drop(file);
            match move_file(&part, &path) {
                Ok(()) => { let _ = std::fs::remove_file(&plan.work); GifResult::Done(path) }
                Err(e) => { let _ = std::fs::remove_file(&part); fail(e.to_string()) }
            }
        }
        Err(e) => { let _ = std::fs::remove_file(&part); fail(e.to_string()) }
    }
}

/// At start: GIF recordings a crash or a quit left unconverted go to the
/// recordings folder as MP4, and half-made files are removed. Says so once.
fn sweep_gif_work(recordings: PathBuf, pattern: String) {
    // Listed now, before anything can start recording: only files from
    // earlier runs are swept, never one this run is writing.
    let own = format!("-{}.", std::process::id());
    let found: Vec<PathBuf> = std::fs::read_dir(gif_work_dir()).map(|d| d.flatten().map(|e| e.path())
        .filter(|p| !p.file_name().is_some_and(|n| n.to_string_lossy().contains(&own))).collect())
        .unwrap_or_default();
    // Half-copied files a quit left in the recordings folder.
    if let Ok(d) = std::fs::read_dir(&recordings) {
        for e in d.flatten() {
            if e.file_name().to_string_lossy().ends_with(".framefang-part") { let _ = std::fs::remove_file(e.path()); }
        }
    }
    if found.is_empty() { return; }
    std::thread::spawn(move || {
        let mut kept = 0;
        for p in found {
            let is_mp4 = p.extension().is_some_and(|x| x.eq_ignore_ascii_case("mp4"));
            if is_mp4 && std::fs::metadata(&p).map(|m| m.len() > 0).unwrap_or(false) {
                let facts = framefang_core::file_name::NameFacts::at(Local::now(), 0, 0);
                let _ = std::fs::create_dir_all(&recordings);
                if keep_gif_mp4(&p, &recordings, &pattern, &facts).is_some() { kept += 1; }
            } else {
                let _ = std::fs::remove_file(&p);
            }
        }
        if kept > 0 {
            notify("Frame Fang: unfinished GIF recordings",
                &format!("{kept} recording{} that didn't become a GIF {} kept as MP4 in {}.",
                    if kept == 1 { "" } else { "s" }, if kept == 1 { "was" } else { "were" },
                    recordings.display()), false);
        }
    });
}

/// Save `bmp` (framed per Settings) into the screenshots folder under a new
/// name from the file-name pattern; never over an existing file. Returns the
/// path, and the folder when Settings had none yet (the caller keeps it).
fn save_capture(
    settings: &AppSettings,
    bmp: &image::ImageBuffer<image::Rgba<u8>, Vec<u8>>,
    facts: &framefang_core::file_name::NameFacts,
) -> Result<(PathBuf, Option<PathBuf>)> {
    let dir = save_directory(settings);
    // Both failures below carry the path *and* the OS reason: this error text
    // is what the user sees in the toast, and "save failed" on its own gives
    // them nothing to fix.
    std::fs::create_dir_all(&dir)
        .map_err(|e| anyhow!(framefang_core::failure::save_folder_failure(
            &dir.display().to_string(), &e.to_string())))?;
    let framed = crate::frame::apply(bmp, settings.output_frame());
    let (path, file) = framefang_core::file_name::claim(&dir, &settings.file_name_pattern, facts, "png")
        .map_err(|e| anyhow!(framefang_core::failure::save_write_failure(
            &dir.display().to_string(), &e.to_string())))?;
    // Flushed by hand: dropped writers swallow the last write's error, and a
    // full disk must not read as "saved".
    let mut out = std::io::BufWriter::new(file);
    let written = framed.write_with_encoder(image::codecs::png::PngEncoder::new(&mut out))
        .map_err(|e| e.to_string())
        .and_then(|_| std::io::Write::flush(&mut out).map_err(|e| e.to_string()));
    drop(out);
    if let Err(e) = written {
        // A claimed name with half a picture in it is worse than none.
        let _ = std::fs::remove_file(&path);
        return Err(anyhow!(framefang_core::failure::save_write_failure(
            &path.display().to_string(), &e.to_string())));
    }
    Ok((path, settings.save_directory.is_empty().then_some(dir)))
}

/// The app and title of the frontmost window under the middle of `r`
/// (virtual-screen pixels), or empty ones over the desktop. A rectangle that
/// is a whole screen or more is no one window's.
fn facts_at(windows: &[framefang_platform::windows_list::OnScreenWindow], r: (i32, i32, i32, i32)) -> (String, String) {
    let (cx, cy) = (r.0 + r.2 / 2, r.1 + r.3 / 2);
    let hit = windows.iter().find(|w| {
        let (x, y, ww, wh) = w.rect;
        cx >= x && cx < x + ww && cy >= y && cy < y + wh
    });
    match hit {
        // A region much bigger than the window holds more than that app.
        Some(w) if (r.2 as i64 * r.3 as i64) * 2 <= (w.rect.2 as i64 * w.rect.3 as i64) * 3 =>
            (framefang_platform::windows_list::app_name(w.pid), w.title.clone()),
        _ => (String::new(), String::new()),
    }
}

/// What a screenshot's name is built from, now.
fn name_facts(bmp: &image::ImageBuffer<image::Rgba<u8>, Vec<u8>>) -> framefang_core::file_name::NameFacts {
    framefang_core::file_name::NameFacts::at(Local::now(), bmp.width(), bmp.height())
}

/// Write `bmp` where Save As said, in the format its extension names: JPEG
/// for `.jpg` / `.jpeg`, PNG for `.png` and for a name with no image
/// extension, which gets `.png` added. Returns the path written.
fn write_image(path: PathBuf, bmp: &image::ImageBuffer<image::Rgba<u8>, Vec<u8>>) -> Result<PathBuf> {
    let ext = path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase);
    let fail = |p: &std::path::Path, e: &dyn std::fmt::Display|
        anyhow!(framefang_core::failure::save_write_failure(&p.display().to_string(), &e.to_string()));
    match ext.as_deref() {
        Some("jpg" | "jpeg") => {
            let file = std::fs::File::create(&path).map_err(|e| fail(&path, &e))?;
            // JPEG has no transparency: lay it on white (a shadow's clear
            // margin would otherwise turn black).
            let rgb = image::DynamicImage::ImageRgba8(crate::frame::on_white(bmp)).to_rgb8();
            image::codecs::jpeg::JpegEncoder::new_with_quality(std::io::BufWriter::new(file), 92)
                .encode_image(&rgb)
                .map_err(|e| fail(&path, &e))?;
            Ok(path)
        }
        Some("png") => { bmp.save(&path).map_err(|e| fail(&path, &e))?; Ok(path) }
        _ => {
            let mut named = path.into_os_string();
            named.push(".png");
            let path = PathBuf::from(named);
            bmp.save(&path).map_err(|e| fail(&path, &e))?;
            Ok(path)
        }
    }
}

fn save_directory(s: &AppSettings) -> PathBuf {
    if !s.save_directory.is_empty() {
        let p = PathBuf::from(&s.save_directory);
        if p.is_dir() { return p; }
    }
    if let Some(d) = directories::UserDirs::new().and_then(|u| u.picture_dir().map(|p| p.to_path_buf())) {
        return d;
    }
    std::env::temp_dir()
}

/// Where MP4 recordings land. Honors `settings.recordings_directory` when set
/// and the path is an existing dir; otherwise falls back to XDG Videos, then
/// $HOME, then the OS temp dir. Distinct from `save_directory` because
/// ~/Pictures is the wrong place for video clips on every desktop env.
fn recordings_directory_for(s: &AppSettings) -> PathBuf {
    if !s.recordings_directory.is_empty() {
        let p = PathBuf::from(&s.recordings_directory);
        if p.is_dir() { return p; }
    }
    if let Some(d) = directories::UserDirs::new()
        .and_then(|u| u.video_dir().map(|p| p.to_path_buf())) {
        return d;
    }
    if let Some(home) = directories::UserDirs::new().map(|u| u.home_dir().to_path_buf()) {
        return home;
    }
    std::env::temp_dir()
}

/// Newest `.mp4` in the recordings folder by modification time — the
/// "Annotate last recording" fallback when the app was restarted since the
/// clip was recorded (so `last_recording` is None).
fn newest_recording(dir: &std::path::Path) -> Option<PathBuf> {
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        let video = path.extension().is_some_and(|e| e.eq_ignore_ascii_case("mp4") || e.eq_ignore_ascii_case("gif"));
        let ours = path.file_name().is_some_and(|n| !n.to_string_lossy().contains(".framefang-part"));
        if !video || !ours { continue; }
        let Ok(modified) = entry.metadata().and_then(|m| m.modified()) else { continue };
        if best.as_ref().map(|(t, _)| modified > *t).unwrap_or(true) {
            best = Some((modified, path));
        }
    }
    best.map(|(_, p)| p)
}

/// Pull the clip length out of ffmpeg's stderr input banner. The line is
/// `  Duration: 00:00:12.34, start: 0.000000, bitrate: 1234 kb/s` —
/// HH:MM:SS with two centisecond digits, or `N/A` for broken files.
/// Returns `None` rather than erroring so callers can degrade to the
/// static (no-timeline) editor instead of refusing to annotate.
fn parse_ffmpeg_duration(stderr: &str) -> Option<f32> {
    let line  = stderr.lines().find(|l| l.trim_start().starts_with("Duration:"))?;
    let token = line.trim_start().strip_prefix("Duration:")?
        .split(',').next()?.trim();
    if token == "N/A" { return None; }
    let mut parts = token.split(':');
    let h: f32 = parts.next()?.parse().ok()?;
    let m: f32 = parts.next()?.parse().ok()?;
    let s: f32 = parts.next()?.parse().ok()?;
    Some(h * 3600.0 + m * 60.0 + s)
}

/// Clip length in seconds via the ffmpeg input banner. ffprobe is not
/// bundled, but plain `ffmpeg -i <video>` prints the same banner to
/// stderr before bailing with "At least one output file must be
/// specified" — the non-zero exit status is expected and ignored; only
/// the banner matters.
fn probe_duration(video: &std::path::Path) -> Result<f32> {
    let ffmpeg = crate::convert_video_form::locate_ffmpeg()
        .ok_or_else(|| anyhow!(crate::ffmpeg_fetch::missing_message()))?;
    let mut cmd = std::process::Command::new(&ffmpeg);
    cmd.arg("-i").arg(video)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped());
    // CREATE_NO_WINDOW: don't flash a console on Windows (same as every
    // other ffmpeg spawn in the app).
    #[cfg(windows)]
    { use std::os::windows::process::CommandExt; cmd.creation_flags(0x0800_0000); }
    let output = cmd.output().map_err(|e| anyhow!("ffmpeg failed to start: {e}"))?;
    parse_ffmpeg_duration(&String::from_utf8_lossy(&output.stderr))
        .ok_or_else(|| anyhow!("no Duration in ffmpeg banner for {}", video.display()))
}

/// Monotonic suffix for ffmpeg temp files — pid alone collides when one
/// session extracts many scrub frames, or when a burn is still encoding
/// on its worker thread while a newer session commits its own.
static FRAME_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// How long after start-up a second-launch capture request is ignored. Long
/// enough to cover a login session starting Framefang twice (two autostart
/// entries, a launcher that retries), short enough that a deliberate
/// re-launch a moment after the tray icon appeared still works.
const STARTUP_CAPTURE_GRACE: Duration = Duration::from_secs(10);

/// Windows: how long a capture started from the tray menu waits before it
/// freezes the screen. The menu is the app's own window, gone the moment it
/// is clicked and kept out of captures anyway, so this only covers the
/// hidden-icons flyout closing. The native menu's fade needed 700 ms.
#[cfg(target_os = "windows")]
const TRAY_MENU_SETTLE: Duration = Duration::from_millis(200);

/// Extract the frame at `secs` (clip seconds) at native resolution via
/// ffmpeg. The PNG round-trips through the OS temp dir; the editor gets
/// it as an RGBA buffer, exactly like a screenshot.
pub(crate) fn extract_frame_at(
    video: &std::path::Path,
    secs:  f32,
) -> Result<image::ImageBuffer<image::Rgba<u8>, Vec<u8>>> {
    let ffmpeg = crate::convert_video_form::locate_ffmpeg()
        .ok_or_else(|| anyhow!(crate::ffmpeg_fetch::missing_message()))?;
    let seq = FRAME_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let frame_png = std::env::temp_dir()
        .join(format!("framefang_annotate_frame_{}_{seq}.png", std::process::id()));
    let mut cmd = std::process::Command::new(&ffmpeg);
    cmd.arg("-y");
    // `-ss` before `-i` is the fast input seek (keyframe jump + decode to
    // the exact timestamp). Omitted at 0.0 so the default first-frame
    // path issues the exact argv it always has.
    if secs > 0.0 { cmd.args(["-ss", &format!("{secs:.3}")]); }
    cmd.arg("-i").arg(video)
        .args(["-frames:v", "1", "-update", "1"])
        .arg(&frame_png)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped());
    // CREATE_NO_WINDOW: don't flash a console on Windows (same as every
    // other ffmpeg spawn in the app).
    #[cfg(windows)]
    { use std::os::windows::process::CommandExt; cmd.creation_flags(0x0800_0000); }
    let output = cmd.output()
        .map_err(|e| anyhow!("ffmpeg failed to start: {e}"))?;
    if !output.status.success() {
        let tail = stderr_tail(&String::from_utf8_lossy(&output.stderr));
        let _ = std::fs::remove_file(&frame_png);
        return Err(anyhow!("could not read a frame from {}: {tail}", video.display()));
    }
    let img = image::open(&frame_png)
        .map_err(|e| anyhow!("decoding extracted frame: {e}"))?
        .into_rgba8();
    let _ = std::fs::remove_file(&frame_png);
    Ok(img)
}

/// Extract the first frame of `video` at native resolution via ffmpeg.
/// The PNG round-trips through the OS temp dir; the editor gets it as an
/// RGBA buffer, exactly like a screenshot.
fn extract_first_frame(
    video: &std::path::Path,
) -> Result<image::ImageBuffer<image::Rgba<u8>, Vec<u8>>> {
    extract_frame_at(video, 0.0)
}

/// `<dir>/<stem>_annotated.mp4`, next to the original clip.
fn annotated_output_path(video: &std::path::Path) -> PathBuf {
    let stem = video.file_stem().map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "framefang".to_owned());
    video.parent().map(|p| p.to_path_buf())
        .unwrap_or_else(std::env::temp_dir)
        .join(format!("{stem}_annotated.mp4"))
}

/// The actual burn: one transparent overlay PNG per (start, end) group,
/// each composited over the clip through its own `overlay` filter. Timed
/// groups are gated with `enable=`; the whole-clip group (start 0, open
/// end) gets a plain `overlay=0:0`, so the zero-interaction default
/// produces the exact pre-timeline filtergraph. Audio copied through
/// untouched. Encoder settings mirror the recorder (libx264 ultrafast,
/// yuv420p for player compatibility). Runs on a worker thread — see
/// `burn_annotations`.
fn run_annotation_burn(
    ffmpeg:   &std::path::Path,
    video:    &std::path::Path,
    overlays: &[OverlayGroup],
    out:      &std::path::Path,
) -> Result<()> {
    if overlays.is_empty() {
        return Err(anyhow!("nothing to burn: no overlay groups."));
    }
    // One temp PNG per group. pid + group index alone is NOT unique:
    // burns run on detached worker threads, so committing a second
    // annotate session while an earlier clip is still encoding reuses
    // the same pid and indices — each burn also takes a FRAME_SEQ
    // ticket, the same disambiguation scrub-frame extraction uses.
    let seq = FRAME_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut pngs: Vec<PathBuf> = Vec::with_capacity(overlays.len());
    for (i, (overlay, _, _)) in overlays.iter().enumerate() {
        let png = std::env::temp_dir()
            .join(format!("framefang_annotate_overlay_{}_{seq}_{i}.png", std::process::id()));
        if let Err(e) = overlay.save(&png) {
            for p in &pngs { let _ = std::fs::remove_file(p); }
            return Err(anyhow!("writing overlay png: {e}"));
        }
        pngs.push(png);
    }
    let windows: Vec<(f32, Option<f32>)> =
        overlays.iter().map(|(_, s, e)| (*s, *e)).collect();

    let mut cmd = std::process::Command::new(ffmpeg);
    cmd.args(["-y", "-i"]).arg(video);
    for png in &pngs { cmd.arg("-i").arg(png); }
    cmd.arg("-filter_complex").arg(overlay_filtergraph(&windows))
        // `[0:a?]` — map audio only if the clip has any (video-only
        // recordings produce no audio stream).
        .args(["-map", "[v]", "-map", "0:a?",
               "-c:v", "libx264", "-preset", "ultrafast",
               "-pix_fmt", "yuv420p",
               "-c:a", "copy",
               "-movflags", "+faststart"])
        .arg(out)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped());
    #[cfg(windows)]
    { use std::os::windows::process::CommandExt; cmd.creation_flags(0x0800_0000); }
    let output = cmd.output()
        .map_err(|e| anyhow!("ffmpeg failed to start: {e}"));
    for png in &pngs { let _ = std::fs::remove_file(png); }
    let output = output?;
    if !output.status.success() {
        let tail = stderr_tail(&String::from_utf8_lossy(&output.stderr));
        return Err(anyhow!("ffmpeg exited with {}: {tail}", output.status));
    }
    match std::fs::metadata(out) {
        Ok(m) if m.len() > 0 => Ok(()),
        _ => Err(anyhow!("ffmpeg reported success but {} is missing or empty.", out.display())),
    }
}

/// Build the `-filter_complex` graph chaining one `overlay` per group:
/// `[0:v][1:v]overlay=0:0:enable='gte(t,S0)*lt(t,E0)'[v0];[v0][2:v]…[v]`,
/// last link always labelled `[v]` for `-map [v]`.
///
/// Quoting: this string is passed as a single Command argv entry (no
/// shell), so the only parser involved is ffmpeg's filtergraph parser.
/// There an unescaped `,` ends the filter's option list, so the commas
/// inside `gte(t,…)` / `lt(t,…)` must be protected — single quotes at
/// the filtergraph level, which the parser strips before the expression
/// evaluator runs. Backslash-escaping each comma would also work; the
/// quote form matches the ffmpeg documentation examples, while the
/// unquoted form is rejected up front with `No such filter: '2.000'` —
/// the comma is taken as a chain separator.
///
/// Window forms:
///   (0.0, None)  → plain `overlay=0:0` — byte-identical to the
///                  pre-timeline single-overlay graph.
///   (s,   None)  → `enable='gte(t,S)'` — "until clip end" without
///                  trusting the parsed duration to the centisecond.
///   (s, Some(e)) → `enable='gte(t,S)*lt(t,E)'` — half-open [S, E),
///                  matching `annotation_visible_at` exactly. ffmpeg's
///                  `between()` is inclusive at BOTH ends, and a frame
///                  landing exactly on E is the common case, not a
///                  corner: the chip presets (3/5/10 s) sit right on the
///                  30 fps frame grid of framefang's own recordings, so the
///                  inclusive form burned one frame the preview said was
///                  clean.
fn overlay_filtergraph(windows: &[(f32, Option<f32>)]) -> String {
    let mut chains = Vec::with_capacity(windows.len());
    let mut prev = "0:v".to_owned();
    for (i, (start, end)) in windows.iter().enumerate() {
        let label = if i + 1 == windows.len() { "v".to_owned() } else { format!("v{i}") };
        let gate = match end {
            None if *start == 0.0 => String::new(),
            None                  => format!(":enable='gte(t,{start:.3})'"),
            Some(e)               => format!(":enable='gte(t,{start:.3})*lt(t,{e:.3})'"),
        };
        chains.push(format!("[{prev}][{}:v]overlay=0:0{gate}[{label}]", i + 1));
        prev = label;
    }
    chains.join(";")
}

/// Last few non-empty stderr lines — enough to say *why* ffmpeg bailed
/// without dumping its whole banner into a notification.
fn stderr_tail(stderr: &str) -> String {
    let lines: Vec<&str> = stderr.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    let n = lines.len();
    lines[n.saturating_sub(4)..].join(" | ")
}

/// The difference `change` makes to `s`, as a patch.
fn patch_of(s: &AppSettings, change: impl FnOnce(&mut AppSettings)) -> framefang_core::settings_store::Patch {
    let mut next = s.clone();
    change(&mut next);
    framefang_core::settings_store::diff(s, &next)
}

/// The tray menu's Sound choice as `settings.json` stores it (`RecordSound`).
fn sound_name(o: framefang_platform::recorder::RecordingOptions) -> &'static str {
    match (o.mic, o.system_audio) {
        (false, false) => "None",
        (true, false)  => "Mic",
        (false, true)  => "PC",
        (true, true)   => "Both",
    }
}

/// `RecordSound` back to the recorder's options; anything unknown is None.
fn sound_from_name(s: &str) -> framefang_platform::recorder::RecordingOptions {
    use framefang_platform::recorder::RecordingOptions as O;
    match s.trim().to_ascii_lowercase().as_str() {
        "mic" => O::MIC_ONLY,
        "pc" | "system" => O::SYSTEM_ONLY,
        "both" => O::MIC_AND_SYS,
        _ => O::NONE,
    }
}

/// The tray icon's hover text: the name, then the capture key as set in
/// Settings (rebuilt whenever Settings saves), on two lines.
fn tray_tooltip(s: &AppSettings) -> String {
    format!("Frame Fang\nPress {} to capture", s.hotkey().describe())
}

/// Register every configured binding, reporting the ones the OS refused.
///
/// A chord another application already owns is the common case here, and it
/// costs the user only that one shortcut — the tray menu still reaches every
/// action — so this logs and moves on rather than failing the call.
fn apply_hotkeys(hk: &mut HotkeyManager, s: &AppSettings) -> Vec<(HotkeyAction, framefang_platform::Error)> {
    let bindings = s.hotkey_bindings();
    // Windows: the capture key with the switch key held is a capture too, so
    // the switch key can be held with it (`switch_key_down` reads which).
    // Other platforms read the switch key only as the drag is let go.
    let switch = if cfg!(windows) { s.capture_switch_chord() } else { None };
    let mut failed = match switch {
        Some(chord) => {
            let mut with = bindings.clone();
            with.push((HotkeyAction::Capture, chord));
            hk.register_all(&with)
        }
        None => hk.register_all(&bindings),
    };
    // A refused switch chord (another program owns it) must not cost the
    // capture key itself: register without it, which reports only real
    // failures.
    if switch.is_some() && failed.iter().any(|(a, _)| *a == HotkeyAction::Capture) {
        eprintln!("switch-key capture chord unavailable; holding it as the drag ends still works");
        failed = hk.register_all(&bindings);
    }
    for (action, e) in &failed {
        eprintln!("hotkey register failed for {}: {e} — use the tray menu instead",
                  action.label());
    }
    // While Windows sends Print Screen to the Snipping Tool, the raw-input
    // path would still hear it and open Framefang on top. Leave that key to
    // the system until the user hands it over (Settings says how).
    if crate::system_shortcuts::system_takes(&s.hotkey()) {
        hk.without_second_path(HotkeyAction::Capture);
    }
    failed
}

/// Whether `key` is held right now, anywhere on the desktop. Windows only;
/// elsewhere the overlay reads it as the drag is let go.
fn switch_key_down(key: framefang_core::settings::SwitchKey) -> bool {
    #[cfg(windows)]
    {
        use windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
        // High bit: down at this moment.
        unsafe { (GetAsyncKeyState(key.vk() as i32) as u16 & 0x8000) != 0 }
    }
    #[cfg(not(windows))]
    { let _ = key; false }
}

/// Text of the tray rows that quote a hotkey. The binding rides along in
/// parentheses so it is discoverable from the menu the user is already in,
/// instead of only from the Settings dialog; an unbound optional action gets
/// its plain label back.
///
/// On the portal backend the compositor has the last word on what the shortcut
/// actually is — we can only ask (see `framefang_platform::hotkey_portal`) — so
/// the labels mark it as requested rather than stating it as fact.
fn menu_labels(s: &AppSettings, portal_hotkey: bool) -> MenuLabels {
    MenuLabels {
        capture:      hotkey_label(s, HotkeyAction::Capture, "Capture", portal_hotkey),
        capture_full: hotkey_label(s, HotkeyAction::CaptureFullScreen, "Capture Full Screen", portal_hotkey),
        record:       hotkey_label(s, HotkeyAction::ToggleRecording, "Record", portal_hotkey),
    }
}

/// `"Record (Ctrl + Shift + R)"`, or bare `"Record"` when the action has no
/// binding. The portal suffix is appended per row because every row that
/// names a key is equally at the compositor's mercy in that session.
fn hotkey_label(s: &AppSettings, action: HotkeyAction, base: &str, portal_hotkey: bool) -> String {
    match s.hotkey_for(action) {
        None     => base.to_owned(),
        Some(hk) => {
            let key = hk.describe();
            if portal_hotkey {
                format!("{base} ({key} - set by your desktop)")
            } else {
                format!("{base} ({key})")
            }
        }
    }
}

/// One-phrase summary of the audio tracks a recording actually carries.
fn audio_label_for(o: framefang_platform::recorder::RecordingOptions) -> &'static str {
    match (o.mic, o.system_audio) {
        (false, false) => "video only",
        (true,  false) => "with microphone",
        (false, true)  => "with system audio",
        (true,  true)  => "with mic + system audio",
    }
}

/// Names the audio sources the user asked for that the recorder couldn't
/// actually open, for appending to the "recording started" toast. Empty
/// string when the recording is exactly what was requested.
fn audio_downgrade_note(requested: framefang_platform::recorder::RecordingOptions,
                        effective: framefang_platform::recorder::RecordingOptions) -> String {
    let lost_mic = requested.mic          && !effective.mic;
    let lost_sys = requested.system_audio && !effective.system_audio;
    match (lost_mic, lost_sys) {
        (false, false) => String::new(),
        (true,  true)  => "\n(no audio - neither source could be opened)".to_owned(),
        (true,  false) => "\n(no microphone - the input device could not be opened)".to_owned(),
        (false, true)  => "\n(no system audio - the audio server was not reachable)".to_owned(),
    }
}

/// Open `url` in the user's default browser. Best-effort — failures are
/// logged but the dialog already gave them the URL as plain text, so they
/// can copy it manually.
/// Send a desktop notification via `notify-send` on Linux, `osascript` on
/// macOS, `powershell BurntToast` on Windows. Best-effort — silent failure
/// if the binary isn't available. `urgent=true` keeps the toast on screen
/// 5 s instead of 3 s.
fn notify(title: &str, body: &str, urgent: bool) {
    let timeout = if urgent { "5000" } else { "3000" };
    #[cfg(target_os = "linux")]
    {
        let _ = std::process::Command::new("notify-send")
            .args(["-a", "Frame Fang", "-t", timeout, title, body])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
        return;
    }
    #[cfg(target_os = "macos")]
    {
        let script = format!(
            "display notification \"{}\" with title \"{}\"",
            body.replace('"', "\\\""),
            title.replace('"', "\\\""),
        );
        let _ = std::process::Command::new("osascript")
            .args(["-e", &script])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
        return;
    }
    #[cfg(target_os = "windows")]
    {
        // PowerShell toast — no extra dep on the user side.
        let script = format!(
            "[Windows.UI.Notifications.ToastNotificationManager,Windows.UI.Notifications,ContentType=WindowsRuntime] | Out-Null; \
             [Windows.Data.Xml.Dom.XmlDocument,Windows.Data.Xml.Dom.XmlDocument,ContentType=WindowsRuntime] | Out-Null; \
             $template = [Windows.UI.Notifications.ToastNotificationManager]::GetTemplateContent([Windows.UI.Notifications.ToastTemplateType]::ToastText02); \
             $template.GetElementsByTagName('text').Item(0).InnerText = '{}'; \
             $template.GetElementsByTagName('text').Item(1).InnerText = '{}'; \
             [Windows.UI.Notifications.ToastNotificationManager]::CreateToastNotifier('Framefang').Show($template);",
            title.replace('\'', "''"),
            body.replace('\'', "''"),
        );
        // CREATE_NO_WINDOW: without it, every toast flashes a black powershell
        // console — including the ones fired on record start/stop.
        use std::os::windows::process::CommandExt;
        let _ = std::process::Command::new("powershell")
            .args(["-NoProfile", "-Command", &script])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .creation_flags(0x0800_0000)
            .spawn();
        let _ = timeout;
    }
}

/// Tell the tray's open popup menu and hidden-icons flyout to close *now*,
/// so the screenshot we're about to take doesn't capture them. The settle
/// sleep that follows gives Windows' menu-fade animation time to actually
/// finish painting.
#[cfg(target_os = "windows")]
fn dismiss_tray_flyouts() {
    use windows::core::w;
    use windows::Win32::UI::WindowsAndMessaging::{
        EndMenu, FindWindowW, ShowWindow, SW_HIDE,
    };
    unsafe {
        // Closes any popup menu owned by the calling thread's foreground —
        // this is what the tray menu actually is once we get the click event.
        let _ = EndMenu();
        // Hide the "show hidden icons" chevron flyout if the user opened it
        // to reach our tray icon. Safe no-op if the window isn't present.
        if let Ok(hwnd) = FindWindowW(w!("NotifyIconOverflowWindow"), None) {
            if !hwnd.is_invalid() {
                let _ = ShowWindow(hwnd, SW_HIDE);
            }
        }
    }
}

/// The processes behind Windows' shell popups: Start, search, the
/// notification center and quick settings. Lowercase file names.
#[cfg(target_os = "windows")]
const SHELL_POPUP_HOSTS: [&str; 5] = [
    "startmenuexperiencehost.exe", "searchhost.exe", "searchapp.exe",
    "shellexperiencehost.exe", "shellhost.exe",
];

/// Close the Start menu (or search, the notification center, quick
/// settings) if it has the keyboard. Windows draws these in a layer above
/// every always-on-top window, so the capture screen opened underneath them
/// (Brandon 2026-11-19: "the screenshot only works underneath OS stuff like
/// the start menu"). Called after the screen is frozen, so they are already
/// in the picture; each closes on Esc. Nothing is sent to any other window.
#[cfg(target_os = "windows")]
fn dismiss_shell_popups() {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, VK_ESCAPE,
    };
    use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};
    let name = unsafe {
        let fg = GetForegroundWindow();
        if fg.is_invalid() { return; }
        let mut pid = 0u32;
        GetWindowThreadProcessId(fg, Some(&mut pid));
        let Ok(h) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else { return };
        let mut buf = [0u16; 260];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, windows::core::PWSTR(buf.as_mut_ptr()), &mut len).is_ok();
        let _ = CloseHandle(h);
        if !ok { return; }
        String::from_utf16_lossy(&buf[..len as usize])
    };
    let file = name.rsplit('\\').next().unwrap_or("").to_ascii_lowercase();
    if !SHELL_POPUP_HOSTS.contains(&file.as_str()) { return; }
    let key = |flags| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: VK_ESCAPE, wScan: 0, dwFlags: flags, time: 0, dwExtraInfo: 0 } },
    };
    let inputs = [key(Default::default()), key(KEYEVENTF_KEYUP)];
    unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32); }
}

/// Open a folder, file or web link the way a double-click would: the folder
/// in Explorer (Finder, the file manager), the link in the browser.
///
/// On Windows this is ShellExecute, never `cmd /c start`: a tray app that
/// starts cmd flashes a console window on every click of Open shots.
/// Open `url` in the default browser. `false` when it couldn't be.
fn open_url(url: &str) -> bool {
    #[cfg(target_os = "windows")]
    {
        use windows::core::{w, HSTRING, PCWSTR};
        use windows::Win32::UI::Shell::ShellExecuteW;
        use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
        let target = HSTRING::from(url);
        let h = unsafe {
            ShellExecuteW(None, w!("open"), PCWSTR(target.as_ptr()), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL)
        };
        // ShellExecute reports success as a value above 32.
        if h.0 as isize <= 32 {
            eprintln!("Couldn't open {url}: ShellExecute returned {}", h.0 as isize);
            return false;
        }
        true
    }
    #[cfg(not(target_os = "windows"))]
    {
        let opener = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
        match std::process::Command::new(opener).arg(url).spawn() {
            Ok(_) => true,
            Err(e) => { eprintln!("Couldn't open {url}: {e}"); false }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sound_choice_round_trips_through_settings_json() {
        use framefang_platform::recorder::RecordingOptions as O;
        for o in [O::NONE, O::MIC_ONLY, O::SYSTEM_ONLY, O::MIC_AND_SYS] {
            assert_eq!(sound_from_name(sound_name(o)), o);
        }
        assert_eq!(sound_from_name(" both "), O::MIC_AND_SYS, "hand edits are forgiven case and spaces");
        assert_eq!(sound_from_name("loud"), O::NONE);
    }

    #[test]
    fn annotated_path_lands_next_to_source() {
        let p = annotated_output_path(std::path::Path::new("/tmp/vids/framefang_20260606_101010.mp4"));
        assert_eq!(p, PathBuf::from("/tmp/vids/framefang_20260606_101010_annotated.mp4"));
    }

    #[test]
    fn stderr_tail_keeps_last_lines_only() {
        let s = "banner\n\nline1\nline2\nline3\nline4\nline5\n";
        let tail = stderr_tail(s);
        assert_eq!(tail, "line2 | line3 | line4 | line5");
        assert_eq!(stderr_tail(""), "");
    }

    #[test]
    fn ffmpeg_banner_duration_parses() {
        // Real banner shape: 2-space indent, comma-terminated field.
        let stderr = "ffmpeg version 6.1.1 Copyright (c) 2000-2023 the FFmpeg developers\n\
            Input #0, mov,mp4,m4a,3gp,3g2,mj2, from 'framefang_20260606_101010.mp4':\n\
            \x20 Metadata:\n\
            \x20   major_brand     : isom\n\
            \x20 Duration: 00:00:12.34, start: 0.000000, bitrate: 1234 kb/s\n\
            \x20 Stream #0:0[0x1](und): Video: h264 (High), yuv420p, 1920x1080\n";
        let d = parse_ffmpeg_duration(stderr).unwrap();
        assert!((d - 12.34).abs() < 0.01, "got {d}");
        let hour = "  Duration: 01:02:03.45, start: 0.000000, bitrate: 5 kb/s\n";
        let d = parse_ffmpeg_duration(hour).unwrap();
        assert!((d - 3723.45).abs() < 0.01, "got {d}");
    }

    #[test]
    fn ffmpeg_banner_duration_rejects_na_and_absent() {
        assert_eq!(parse_ffmpeg_duration("  Duration: N/A, start: 0.000000, bitrate: N/A\n"), None);
        assert_eq!(parse_ffmpeg_duration("no banner here\n"), None);
        assert_eq!(parse_ffmpeg_duration(""), None);
    }

    #[test]
    fn filtergraph_single_whole_clip_matches_pre_timeline_graph() {
        // The zero-interaction default must produce the exact filter the
        // static burn always used.
        assert_eq!(overlay_filtergraph(&[(0.0, None)]), "[0:v][1:v]overlay=0:0[v]");
    }

    #[test]
    fn filtergraph_chains_enable_windows() {
        let graph = overlay_filtergraph(&[
            (0.0, None),
            (1.5, Some(4.5)),
            (2.0, None),
        ]);
        assert_eq!(graph,
            "[0:v][1:v]overlay=0:0[v0];\
             [v0][2:v]overlay=0:0:enable='gte(t,1.500)*lt(t,4.500)'[v1];\
             [v1][3:v]overlay=0:0:enable='gte(t,2.000)'[v]");
    }

    #[test]
    fn a_region_is_named_for_the_window_under_it_but_a_screen_is_not() {
        use framefang_platform::windows_list::OnScreenWindow;
        let windows = vec![
            OnScreenWindow { rect: (100, 100, 800, 600), title: "Notes".into(), pid: 0, hwnd: 0 },
            OnScreenWindow { rect: (0, 0, 1920, 1080), title: "Back".into(), pid: 0, hwnd: 0 },
        ];
        assert_eq!(facts_at(&windows, (200, 200, 300, 200)).1, "Notes", "the frontmost window under the middle");
        assert_eq!(facts_at(&windows, (1500, 900, 100, 100)).1, "Back");
        assert_eq!(facts_at(&windows, (-500, -500, 50, 50)), (String::new(), String::new()), "over nothing");
        // Centred inside "Back", but half again its size: no one window's.
        assert_eq!(facts_at(&windows, (0, 0, 3600, 1000)).1, "", "a region much bigger than the window");
        assert_eq!(facts_at(&windows, (0, 0, 1900, 1000)).1, "Back");
    }

    #[test]
    fn a_gif_that_fails_keeps_the_recording_as_mp4() {
        let dir = std::env::temp_dir().join(format!("ff-gifjob-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let work = dir.join("123-4.mp4");
        std::fs::write(&work, b"video").unwrap();
        let recordings = dir.join("rec");
        let plan = GifPlan {
            work: work.clone(), dir: recordings.clone(), pattern: "clip".into(),
            facts: framefang_core::file_name::NameFacts::at(Local::now(), 0, 0),
        };
        // An FFmpeg that isn't there fails like one that exits 1.
        let r = gif_worker(dir.join("no-ffmpeg.exe"), plan, &std::sync::atomic::AtomicBool::new(false));
        match r {
            GifResult::Failed { kept: Some(p), .. } => {
                assert_eq!(p, recordings.join("clip.mp4"));
                assert_eq!(std::fs::read(&p).unwrap(), b"video");
                assert!(!work.exists(), "moved, not copied");
            }
            _ => panic!("expected the MP4 kept"),
        }
        assert!(!recordings.join("clip.gif").exists(), "no empty GIF left under the name");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_copied_pill_sits_under_the_picture_and_on_its_screen() {
        let m = framefang_core::virtual_desktop::MonitorRect { x: 0, y: 0, width: 1920, height: 1080 };
        let (x, y) = crate::editor::toast_spot((100, 100, 400, 300), m, (180, 32));
        assert_eq!((x, y), (100 + (400 - 180) / 2, 100 + 300 + 12));
        // At the bottom edge it goes up over the picture, still on screen.
        let (_, y) = crate::editor::toast_spot((100, 900, 400, 170), m, (180, 32));
        assert!(y + 32 <= 1080 - 12 && y < 900 + 170);
    }

    #[test]
    fn jpeg_lays_transparency_on_white() {
        let dir = std::env::temp_dir().join(format!("ff-jpeg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let clear = image::ImageBuffer::from_pixel(8, 8, image::Rgba([0u8, 0, 0, 0]));
        let path = write_image(dir.join("x.jpg"), &clear).unwrap();
        let back = image::open(&path).unwrap().to_rgb8();
        assert!(back.get_pixel(4, 4)[0] > 240, "white, not black: {:?}", back.get_pixel(4, 4));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn newest_recording_picks_latest_mp4() {
        let dir = std::env::temp_dir().join(format!("framefang_test_newest_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let old = dir.join("a.mp4");
        let new = dir.join("b.mp4");
        let png = dir.join("c.png");
        std::fs::write(&old, b"x").unwrap();
        std::fs::write(&png, b"x").unwrap();
        // Ensure a strictly newer mtime for b.mp4.
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&new, b"x").unwrap();
        assert_eq!(newest_recording(&dir), Some(new));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
