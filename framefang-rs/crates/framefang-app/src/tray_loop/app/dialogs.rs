//! The small windows the tray opens (About, the first-run welcome, Check
//! for updates, the two converters) and the self-update job.
//!
//! Each window is built only when asked for, and asking again while it is
//! open does nothing, so no window is ever stacked on its own copy.

use super::*;

impl TrayApp {
    /// About, in the same look as Settings.
    pub(super) fn show_about(&mut self, loop_target: &ActiveEventLoop) {
        if self.about_view.is_some() { return; }
        match AboutView::new(loop_target) {
            Ok(v) => self.about_view = Some(v),
            Err(e) => eprintln!("About window: {e}"),
        }
    }

    /// Put the welcome window up on the first launch, once.
    ///
    /// Sequenced after `ensure_tray` on purpose: the window's whole job is
    /// to point at the tray icon, so it must not open before the icon
    /// exists. When the tray failed to build, `ensure_tray` has already
    /// raised the About window as the fallback surface and closing that
    /// one quits; stacking a second window on top of it would only give
    /// the user two things to dismiss and one misleading instruction.
    pub(super) fn maybe_show_first_run(&mut self, loop_target: &ActiveEventLoop) {
        if !self.first_run || !self.tray_built { return; }
        self.first_run = false;
        if self.tray.is_none() { return; }
        let folder = save_directory(&self.settings).to_string_lossy().into_owned();
        match FirstRunView::new(loop_target, &self.settings.hotkey().describe(), &folder, self.wayland) {
            Ok(v) => self.firstrun_view = Some(v),
            Err(e) => eprintln!("welcome window: {e}"),
        }
    }

    /// Check for updates. The window looks up the latest release in the
    /// background and shows it beside the installed version; its releases
    /// page button works whether or not the lookup does.
    pub(super) fn show_updates(&mut self, loop_target: &ActiveEventLoop) {
        if self.updates_view.is_some() { return; }
        match UpdatesView::new(loop_target) {
            Ok(v) => self.updates_view = Some(v),
            Err(e) => eprintln!("Check for updates window: {e}"),
        }
    }

    /// The image converter.
    pub(super) fn show_convert_image(&mut self, loop_target: &ActiveEventLoop) {
        if self.convert_image_view.is_some() { return; }
        match ConvertImageView::new(loop_target) {
            Ok(v) => self.convert_image_view = Some(v),
            Err(e) => eprintln!("image converter window: {e}"),
        }
    }

    /// The video converter, which needs FFmpeg.
    pub(super) fn show_convert_video(&mut self, loop_target: &ActiveEventLoop) {
        if self.convert_video_view.is_some() { return; }
        if !self.have_ffmpeg(loop_target, "Video conversion") { return; }
        match ConvertVideoView::new(loop_target) {
            Ok(v) => self.convert_video_view = Some(v),
            Err(e) => eprintln!("video converter window: {e}"),
        }
    }

    /// Starts downloading and installing an update on a worker thread
    /// (`self_updater::download_and_install`), checked on from `poll`.
    /// When it works the worker starts the new version and ends this
    /// process, so the job only ever reports back with a failure. The
    /// Updates window stays open meanwhile and shows how far it got.
    pub(super) fn start_self_update(&mut self, asset_url: String, expected_sha256: Option<String>) {
        if self.update_job.is_some() {
            eprintln!("update: one is already being installed");
            return;
        }
        let checked = if expected_sha256.is_some() { "checked against its SHA-256" } else { "not checked, no SHA-256 given" };
        eprintln!("update: fetching {asset_url} ({checked})");
        // This runs on the worker thread, which must not touch any window,
        // so the progress only goes to the log.
        let progress = |downloaded: u64, total: Option<u64>| match total.filter(|&t| t > 0) {
            Some(total) => eprintln!("update: {downloaded} of {total} bytes"),
            None => eprintln!("update: {downloaded} bytes"),
        };
        self.update_job = Some(self_updater::download_and_install(asset_url, expected_sha256, progress));
    }

    /// Collects the update job once its thread has ended. That only happens
    /// when it failed: an update that works ends the process from inside
    /// the worker, so this never sees one.
    pub(super) fn poll_update_job(&mut self) {
        if !self.update_job.as_ref().is_some_and(|h| h.is_finished()) { return; }
        let Some(job) = self.update_job.take() else { return };
        match job.join() {
            // The new version should have taken over before this point.
            Ok(Ok(())) => eprintln!("update: the worker finished without handing over to the new version"),
            Ok(Err(msg)) => {
                eprintln!("update failed: {msg}");
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
            Err(_) => eprintln!("update: the worker thread panicked"),
        }
    }
}
