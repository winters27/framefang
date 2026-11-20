//! Settings: the Settings window, putting new settings into effect, and
//! keeping `settings.json` and the running app in step (changes written as
//! patches, hand edits picked up while running).

use super::*;
use framefang_core::settings_store::{self as store, Patch, StoreError};

impl TrayApp {
    /// The tray's Settings row. The window is built the first time it is
    /// asked for, so nothing of it exists until then; asking again while it
    /// is open does nothing.
    pub(super) fn show_settings(&mut self, loop_target: &ActiveEventLoop) {
        self.open_settings(loop_target, false);
    }

    /// Whether FFmpeg is there for `what` ("Recording", "Video
    /// conversion"). When it isn't: on Windows, say so and open Settings
    /// with the FFmpeg row lit and its Download button ready; elsewhere,
    /// say how to install it.
    pub(super) fn have_ffmpeg(&mut self, loop_target: &ActiveEventLoop, what: &str) -> bool {
        if crate::convert_video_form::locate_ffmpeg().is_some() { return true; }
        let title = format!("Frame Fang: {what} needs FFmpeg");
        if crate::ffmpeg_fetch::offered() {
            notify(&title, "Download it from Settings with one click. If you already have FFmpeg, add it to PATH.", false);
            match self.settings_view.as_mut() {
                Some(v) => v.draw_attention_to_ffmpeg(),
                None => self.open_settings(loop_target, true),
            }
        } else {
            notify(&title, crate::ffmpeg_fetch::missing_message(), true);
        }
        false
    }

    /// Opens the Settings window, with the FFmpeg row called out when
    /// `ffmpeg_attention`.
    ///
    /// Every global shortcut is off while it is open: rebinding one means
    /// pressing it, and that press (even Print Screen) must reach the
    /// window instead of starting a capture. They come back when it closes.
    pub(super) fn open_settings(&mut self, loop_target: &ActiveEventLoop, ffmpeg_attention: bool) {
        if self.settings_view.is_some() { return; }
        if let Some(hk) = self.hotkeys.as_mut() { hk.unregister_all(); }
        match SettingsView::new(loop_target, self.settings.clone(), ffmpeg_attention) {
            Ok(view) => self.settings_view = Some(view),
            Err(e) => {
                eprintln!("Settings window: {e}");
                // No window after all, so the shortcuts go straight back on
                // rather than leaving capture without its key.
                if let Some(hk) = self.hotkeys.as_mut() { apply_hotkeys(hk, &self.settings); }
            }
        }
    }

    /// Called from the window event router after the settings dialog
    /// produces an outcome. `Changed` applies and saves at once (there is
    /// no Save button); `Closed` turns the shortcuts back on; `OpenJson`
    /// opens settings.json and leaves the dialog open. `true` closes it.
    pub(super) fn apply_settings_outcome(&mut self, outcome: SettingsOutcome) -> bool {
        match outcome {
            SettingsOutcome::Patch(p) => self.store_patch(p),
            SettingsOutcome::Replace(s) => self.store_replace(s),
            SettingsOutcome::Closed => {
                // `open_settings` turned every shortcut off so keys could
                // be rebound; turn on the ones the user ended with.
                self.register_hotkeys();
                return true;
            }
            SettingsOutcome::OpenJson => self.open_settings_file(),
        }
        false
    }

    /// Edit as JSON: settings.json in the system's editor for it, written
    /// out first if it does not exist yet so there is a file to open.
    fn open_settings_file(&mut self) {
        let Some(path) = AppSettings::settings_path() else { return };
        if !path.exists() {
            self.store_replace(self.settings.clone());
        }
        open_url(&path.to_string_lossy());
    }

    /// Make `new` the settings in use: the color scheme, the sign-in
    /// entry, the tray's tooltip and menu words, and the shortcuts. The
    /// shortcuts wait while Settings is open (it holds them off so keys
    /// can be rebound) and come on when it closes.
    pub(super) fn apply_settings(&mut self, new: AppSettings) {
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
                    eprintln!("tray tooltip: {e}");
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
    pub(super) fn register_hotkeys(&mut self) {
        let Some(hk) = self.hotkeys.as_mut() else { return };
        for (action, e) in apply_hotkeys(hk, &self.settings) {
            notify("Frame Fang: couldn't set that shortcut", &format!("{}: {e}", action.label()), true);
        }
    }

    /// Save a change: `patch` is applied to `settings.json` as it is on
    /// disk now (see `settings_store`), and the result becomes the
    /// settings everywhere (tray, Settings window, open editor). While
    /// the file doesn't parse the change is used but kept back, and
    /// written once the file is fixed.
    pub(super) fn store_patch(&mut self, patch: Patch) {
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
                        eprintln!("settings.json not written: {io}");
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
    pub(super) fn store_replace(&mut self, mut s: AppSettings) {
        s.clamp();
        match store::replace(&s) {
            Ok(stamp) => {
                self.settings_stamp = Some(stamp);
                self.pending_patch = None;
            }
            Err(e) => {
                notify("Frame Fang: settings not saved yet",
                    &framefang_core::failure::settings_write_failure(&e.to_string()), true);
                // Every key, so the retry writes the whole import.
                self.pending_patch = Some(store::whole(&s));
            }
        }
        self.adopt_settings(s);
    }

    /// Make `new` the settings of every holder: the Settings window (its
    /// draft and the base it diffs from), an open editor (keeping its own
    /// changes not yet sent), and the tray itself.
    pub(super) fn adopt_settings(&mut self, new: AppSettings) {
        if let Some(v) = self.settings_view.as_mut() { v.replace_draft(new.clone()); }
        if let Some(ov) = self.overlay.as_mut() { ov.adopt_settings(&new); }
        self.apply_settings(new);
    }

    /// Save what the open editor changed (a tool, a color, the capture
    /// mode). Called after every call into it.
    pub(super) fn drain_overlay_patch(&mut self) {
        if let Some(p) = self.overlay.as_mut().and_then(|o| o.take_patch()) {
            self.store_patch(p);
        }
    }

    /// Pick up a hand edit of `settings.json` (Edit as JSON, or any
    /// editor) within a second: apply it everywhere. A file that doesn't
    /// parse changes nothing, and says what is wrong once per version.
    pub(super) fn watch_settings_file(&mut self) {
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
}
