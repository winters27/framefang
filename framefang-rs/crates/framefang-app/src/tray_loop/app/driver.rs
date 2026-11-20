//! Keeping the app going between window events: the tray icon, the timed
//! poll that services everything without a window of its own, and what
//! each tray menu row does.

use super::*;

/// Within this long after the tray menu closed, a click on the icon is the
/// same click that closed it.
const MENU_REOPEN_GUARD: Duration = Duration::from_millis(350);

impl TrayApp {
    /// Make the tray icon, once, from inside the running event loop.
    ///
    /// macOS needs this: a status item created before NSApplication has
    /// finished launching never shows up in the menu bar. Windows and Linux
    /// would accept it earlier, but one path for everyone is simpler.
    ///
    /// Failing here can't be quiet. With no icon there is no menu and so no
    /// Exit, and the app would be an invisible process nobody can close. So
    /// the About window opens in its place (closing it quits, see the
    /// `AboutOutcome::Closed` handling) and a notice says why.
    pub(super) fn ensure_tray(&mut self, loop_target: &ActiveEventLoop) {
        if self.tray_built { return; }
        self.tray_built = true;
        match Tray::new(tray_tooltip(&self.settings), &menu_labels(&self.settings, self.portal_hotkey)) {
            Ok(t) => {
                t.set_gif(self.settings.record_format() == framefang_core::settings::RecordFormat::Gif);
                t.set_last_region(self.app_state.last_region.is_some());
                self.tray = Some(t);
            }
            Err(e) => {
                eprintln!("tray icon: {e}");
                notify("Frame Fang: no tray icon",
                    &format!("{e}\n\nThe About window is open instead; closing it quits Frame Fang."),
                    true);
                self.show_about(loop_target);
                if self.about_view.is_none() {
                    // Neither an icon nor a window: nothing on screen leads
                    // to this process any more, so end it rather than tick
                    // away unseen.
                    eprintln!("No tray icon and no window to show instead; quitting.");
                    loop_target.exit();
                }
            }
        }
        // A notice only when the person has something to fix. A portal
        // shortcut that registered simply works and needs no word.
        if let Some(reason) = self.hotkey_error.take() {
            notify("Frame Fang: capture shortcut unavailable", &reason, true);
        }
    }

    /// The timed pass, about 30 times a second, over everything that has no
    /// window event to wake it: the tray, the hotkeys, background jobs and
    /// the animations of the small windows. The order is kept as it is on
    /// purpose (the tray's own loop runs before its events are read, for
    /// instance).
    pub(super) fn poll(&mut self, loop_target: &ActiveEventLoop) {
        // A change the editor made on its own tick is saved too.
        self.drain_overlay_patch();
        self.poll_tray(loop_target);
        // The capture dock animates on time, not on events.
        if let Some(ov) = self.overlay.as_mut() { ov.tick(); }
        // A full-screen capture window holds on to the keyboard, so it still
        // hears Esc when Windows hands the keyboard to something else.
        #[cfg(windows)]
        {
            if let Some(ov) = self.overlay.as_mut() { ov.guard_focus(); }
            if let Some(c) = self.color_pick.as_mut() { c.guard_focus(); }
        }
        // settings.json edited by hand takes effect without a restart.
        self.watch_settings_file();
        // `framefang --settings`, from this launch or a later one.
        if framefang_platform::instance::take_settings_request() {
            self.show_settings(loop_target);
        }
        // An open Settings window shows the FFmpeg download as it goes.
        if let Some(v) = self.settings_view.as_mut() { v.tick(); }
        self.poll_hotkeys(loop_target);
        self.poll_second_launch(loop_target);
        self.poll_recorder_health();
        self.poll_gif_job();
        self.poll_read_job(loop_target);
        self.poll_recent(loop_target);
        self.poll_scroll(loop_target);
        // These animate even with the mouse elsewhere: the REC dot blinks,
        // and the update check and the video converter take in their
        // workers' answers and keep their "working" dots moving.
        if let Some(v) = &self.recording_view { v.tick(); }
        if let Some(v) = self.updates_view.as_mut() { v.tick(); }
        if let Some(v) = self.convert_video_view.as_mut() { v.tick(); }
        // A finished self-update job means it failed; say so.
        self.poll_update_job();
    }

    /// The tray's own loop (GTK on Linux), then whatever it delivered: a
    /// menu row picked, or a click on the icon, and the app menu's state.
    fn poll_tray(&mut self, loop_target: &ActiveEventLoop) {
        let (picked, clicked) = match &self.tray {
            Some(tray) => {
                // Run first, so menu clicks have reached the channel.
                tray.pump_events();
                (tray.try_recv(), tray.take_click())
            }
            None => (TrayEvent::None, None),
        };
        self.run_tray_event(picked, loop_target);
        if let Some(c) = clicked { self.tray_clicked(c, loop_target); }
        if let Some(m) = self.tray_menu.as_mut() { m.tick(); }
        self.finish_tray_menu(loop_target);
    }

    /// The hotkeys pressed since the last pass, each run in turn.
    fn poll_hotkeys(&mut self, loop_target: &ActiveEventLoop) {
        use crate::read_text::ReadKind;
        // Taken out first: the manager is not borrowed while the actions
        // change `self`.
        let pressed = self.hotkeys.as_ref().map(|hk| hk.drain_pressed()).unwrap_or_default();
        for action in pressed {
            match action {
                HotkeyAction::Capture => self.capture_from_hotkey(loop_target),
                HotkeyAction::CaptureFullScreen => self.capture_full_screen(false),
                HotkeyAction::ToggleRecording => self.toggle_recording(loop_target),
                HotkeyAction::CaptureLastRegion => self.capture_last_region(loop_target),
                HotkeyAction::CaptureText => self.capture_reading(loop_target, ReadKind::Text, false),
                HotkeyAction::CaptureQr => self.capture_reading(loop_target, ReadKind::Qr, false),
                HotkeyAction::CaptureScroll => self.capture_scroll(loop_target, false),
            }
        }
    }

    /// Launching Frame Fang again while it runs leaves a capture request
    /// next to the instance lock and exits, so that starting it again
    /// (its icon double-clicked, `framefang` typed) captures. Checking
    /// costs one failed unlink per pass. Requests in the first seconds
    /// after start-up are dropped: two launches that close together are a
    /// login starting the app twice, not a wish for a screenshot, and a
    /// dimmed full-screen capture is no way to greet a new desktop.
    fn poll_second_launch(&mut self, loop_target: &ActiveEventLoop) {
        if framefang_platform::instance::take_capture_request()
            && self.started_at.elapsed() >= STARTUP_CAPTURE_GRACE
        {
            self.capture(loop_target);
        }
    }

    /// Notices a recorder that died mid-recording (a full disk, an unplugged
    /// device, killed for memory). Nothing else looks at it between start
    /// and stop, so without this the app would show REC over a dead process.
    /// `poll_health` has already put the recorder back to idle when it
    /// reports this, so only the UI is cleaned up here; calling `stop()`
    /// would only add "not recording" on top of the real reason.
    fn poll_recorder_health(&mut self) {
        let Err(e) = self.recorder.poll_health() else { return };
        eprintln!("The recording ended by itself: {e}");
        if let Some(t) = &self.tray { t.set_recording(false); }
        self.recording_view = None;
        notify("Frame Fang: recording stopped unexpectedly", &e.to_string(), true);
        // What a GIF recording got before it stopped still becomes one.
        if let Some(plan) = self.gif_plan.take() {
            if std::fs::metadata(&plan.work).is_ok_and(|m| m.len() > 0) {
                self.start_gif_job(plan);
            } else {
                let _ = std::fs::remove_file(&plan.work);
            }
        }
    }

    /// Do what a tray menu row asks, from the native menu or the app's own.
    pub(super) fn run_tray_event(&mut self, ev: TrayEvent, loop_target: &ActiveEventLoop) {
        use crate::read_text::ReadKind;
        match ev {
            // Delays are handled by the countdown loop itself.
            TrayEvent::None | TrayEvent::CancelPending => {}
            TrayEvent::Capture => self.capture(loop_target),
            TrayEvent::CaptureFullScreen => self.capture_full_screen(true),
            TrayEvent::CaptureLastRegion => {
                // Picked from a menu, which must be gone before the shot.
                wait_for_tray_menu();
                self.capture_last_region(loop_target);
            }
            TrayEvent::CaptureText => self.capture_reading(loop_target, ReadKind::Text, true),
            TrayEvent::CaptureQr => self.capture_reading(loop_target, ReadKind::Qr, true),
            TrayEvent::CaptureScroll => self.capture_scroll(loop_target, true),
            TrayEvent::CaptureDelayed(secs) => self.capture_after(loop_target, Duration::from_secs(secs as u64)),
            TrayEvent::RecentCopy(i) => self.open_recent(i, false),
            TrayEvent::RecentEdit(i) => self.open_recent(i, true),
            TrayEvent::SetGif(on) => self.set_record_format(on, loop_target),
            TrayEvent::CancelGif => self.cancel_gif(),
            TrayEvent::StartRecording(opts) => self.start_recording(opts, loop_target, None),
            TrayEvent::StartRegionRecording(opts) => self.begin_region_recording(opts, loop_target),
            TrayEvent::StopRecording => self.stop_recording(),
            TrayEvent::AnnotateLastRecording => self.annotate_last_recording(loop_target),
            TrayEvent::OpenSaveFolder => self.open_save_folder(),
            TrayEvent::OpenRecordingsFolder => self.open_recordings_folder(),
            TrayEvent::Settings => self.show_settings(loop_target),
            TrayEvent::About => self.show_about(loop_target),
            TrayEvent::CheckForUpdates => self.show_updates(loop_target),
            TrayEvent::ConvertImage => self.show_convert_image(loop_target),
            TrayEvent::ConvertVideo => self.show_convert_video(loop_target),
            TrayEvent::Exit => self.quit(loop_target),
        }
    }

    /// The tray's Exit.
    fn quit(&mut self, loop_target: &ActiveEventLoop) {
        // A running recording is stopped first so its MP4 gets finished
        // properly. If that fails, Frame Fang quits all the same.
        let _ = self.recorder.stop();
        // There is no time to make a GIF now, so its MP4 is kept instead.
        if let Some(plan) = self.gif_plan.take() {
            let _ = std::fs::create_dir_all(&plan.dir);
            if keep_gif_mp4(&plan.work, &plan.dir, &plan.pattern, &plan.facts).is_none() {
                eprintln!("framefang: the GIF recording stays in {} until the next start", gif_work_dir().display());
            }
        }
        // The last capture's picture finishes writing.
        self.recent.shutdown();
        loop_target.exit();
    }

    /// A click on the tray icon (Windows): open the app's menu there, or
    /// close it if it is open, like a native menu.
    pub(super) fn tray_clicked(&mut self, click: framefang_platform::tray::TrayClick, loop_target: &ActiveEventLoop) {
        if self.tray_menu.take().is_some() {
            self.tray_menu_closed = Some(Instant::now());
            return;
        }
        // The press on the icon took focus from the open menu, which
        // closed it; this is the same click coming up.
        if self.tray_menu_closed.is_some_and(|t| t.elapsed() < MENU_REOPEN_GUARD) {
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
    pub(super) fn finish_tray_menu(&mut self, loop_target: &ActiveEventLoop) {
        use crate::tray_menu::MenuOutcome;
        let Some(out) = self.tray_menu.as_mut().and_then(|m| m.outcome.take()) else { return };
        if let Some(menu) = self.tray_menu.take() {
            // The Sound and Format chips' choices are kept in settings.json.
            let name = sound_name(menu.sound());
            if self.settings.record_sound != name {
                let p = patch_of(&self.settings, |s| s.record_sound = name.to_owned());
                self.store_patch(p);
            }
            let gif_now = self.settings.record_format() == framefang_core::settings::RecordFormat::Gif;
            if menu.gif() != gif_now { self.set_record_format(menu.gif(), loop_target); }
        }
        self.tray_menu_closed = Some(Instant::now());
        match out {
            MenuOutcome::Run(ev) => self.run_tray_event(ev, loop_target),
            MenuOutcome::Recent { index, edit } => self.open_recent(index, edit),
            MenuOutcome::Closed => {}
        }
    }
}
