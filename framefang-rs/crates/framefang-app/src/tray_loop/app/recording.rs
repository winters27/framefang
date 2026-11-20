//! Screen recording: starting and stopping the recorder, the floating REC
//! panel, turning a finished recording into a GIF, and drawing annotations
//! into a copy of a clip.

use super::*;

/// The body of a "saved" notice for a clip, with a pointer to where it can
/// be drawn on.
fn saved_clip_notice(path: &std::path::Path) -> String {
    format!("{}\n\nTo draw on it, pick \"Annotate last recording\" in the tray menu.", path.display())
}

/// Whether `path` is a file with something in it.
fn has_bytes(path: &std::path::Path) -> bool {
    std::fs::metadata(path).is_ok_and(|m| m.len() > 0)
}

/// Whether `path` is a file with nothing in it.
fn is_empty_file(path: &std::path::Path) -> bool {
    std::fs::metadata(path).is_ok_and(|m| m.len() == 0)
}

/// Tells the person a recording went wrong without holding up the event
/// loop. On Windows that is a message box on a thread of its own (a
/// MessageBoxW is modal only to its own thread, so the hotkeys and tray
/// keep working). Elsewhere the system's dialogs only run on the main
/// thread, so it is a notification instead.
fn report_recording_problem(title: &'static str, msg: String) {
    #[cfg(target_os = "windows")]
    std::thread::spawn(move || {
        rfd::MessageDialog::new()
            .set_level(rfd::MessageLevel::Error)
            .set_title(title)
            .set_description(msg)
            .show();
    });
    #[cfg(not(target_os = "windows"))]
    notify(title, &msg, true);
}

/// What a recording covers, as the start notice and the log put it.
fn scope_words(region: Option<CaptureRect>) -> String {
    match region {
        Some(r) => format!("Region {}x{}", r.width, r.height),
        None => "Whole screen".to_owned(),
    }
}

impl TrayApp {
    /// Record as GIF or MP4, from the tray. GIF needs FFmpeg everywhere
    /// (MP4 does too on Windows and Linux); without it, Settings opens
    /// where FFmpeg can be set up and the choice stays as it was.
    pub(super) fn set_record_format(&mut self, gif: bool, loop_target: &ActiveEventLoop) {
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

    /// The tray's Record region: freeze the desktop and open the capture
    /// window as a picker that only selects. The audio choice waits in
    /// `pending_region_audio` until the pick is confirmed, and
    /// `start_region_from_selection` starts the recording then.
    pub(super) fn begin_region_recording(&mut self, opts: RecordingOptions, loop_target: &ActiveEventLoop) {
        if self.refuse_if_recording_busy() { return; }
        if self.capturing || self.overlay.is_some() { return; }
        self.capturing = true;
        // The menu must be gone first, or the region gets dragged over a
        // picture of it.
        wait_for_tray_menu();
        match capture_all_screens() {
            Ok(shot) => {
                let windows = framefang_platform::windows_list::on_screen_windows();
                #[cfg(target_os = "windows")]
                dismiss_shell_popups();
                self.overlay_desktop = Some(shot.desktop_bounds());
                self.pending_region_audio = Some(opts);
                let geom = shot.geometry();
                match Overlay::new_for_region_record(loop_target, geom, shot.bitmap, self.settings.clone()) {
                    Ok(mut ov) => {
                        ov.set_display_map(shot.map);
                        ov.set_windows(windows);
                        self.overlay = Some(ov);
                    }
                    Err(e) => {
                        self.pending_region_audio = None;
                        eprintln!("region picker: {e}");
                        notify("Frame Fang: recording failed", &format!("Couldn't open the region picker: {e}"), true);
                    }
                }
            }
            Err(e) => {
                // No frozen desktop means nothing to pick a region on. Falling
                // back to recording the whole screen would record something
                // nobody asked for, so this stops here.
                eprintln!("region picker, screen capture: {e}");
                notify("Frame Fang: recording failed",
                    &format!("The screen couldn't be read, so there was nothing to pick a region on: {e}"), true);
            }
        }
        self.capturing = false;
    }

    /// Starts the region recording for a confirmed selection, given in
    /// virtual-screen device pixels. `record_rect_from_selection` keeps it
    /// inside the desktop it was picked on and evens its size for H.264.
    pub(super) fn start_region_from_selection(&mut self, selection: (i32, i32, i32, i32), loop_target: &ActiveEventLoop) {
        let opts = self.pending_region_audio.take().unwrap_or(RecordingOptions::NONE);
        let Some(bounds) = self.overlay_desktop else {
            notify("Frame Fang: recording failed", "The screen the region was picked on is no longer known.", true);
            return;
        };
        let Some(rect) = record_rect_from_selection(selection, bounds) else {
            notify("Frame Fang: region too small",
                "Pick a larger area. A recording needs at least 16x16 pixels.", true);
            return;
        };
        // The picker has closed, but what was under it still has to be
        // repainted; recording at once would put its dimmed frame into the
        // first moments of the clip.
        std::thread::sleep(Duration::from_millis(300));
        self.start_recording(opts, loop_target, Some(rect));
    }

    /// Recording, or still making the last recording's GIF.
    pub(super) fn recording_busy(&self) -> bool {
        self.recorder.is_recording() || self.gif_job.is_some()
    }

    /// Say why a recording can't start now, if it can't. `true` refused.
    pub(super) fn refuse_if_recording_busy(&self) -> bool {
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
    pub(super) fn start_gif_job(&mut self, plan: GifPlan) {
        let Some(ffmpeg) = crate::convert_video_form::locate_ffmpeg() else {
            let kept = keep_gif_mp4(&plan.work, &plan.dir, &plan.pattern, &plan.facts);
            let kept = kept.map(|p| p.display().to_string()).unwrap_or_else(|| "MP4".into());
            notify("Frame Fang: GIF not made", &format!("FFmpeg is missing. The recording is kept as {kept}."), true);
            return;
        };
        let (tx, rx) = std::sync::mpsc::channel();
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let worker_cancel = cancel.clone();
        let handle = std::thread::spawn(move || {
            let _ = tx.send(gif_worker(ffmpeg, plan, &worker_cancel));
        });
        self.gif_job = Some(GifJob { rx, cancel, handle });
        self.show_converting(true);
        notify("Frame Fang: making the GIF", "The recording stopped; the GIF follows in a moment.", false);
    }

    /// The tray's look while a GIF is being made.
    fn show_converting(&mut self, on: bool) {
        let Some(t) = &self.tray else { return };
        let tip = if on { "Frame Fang: making a GIF...".to_owned() } else { tray_tooltip(&self.settings) };
        let _ = t.set_tooltip(&tip);
        t.set_converting(on);
    }

    /// Cancel the GIF being made; its MP4 is kept.
    pub(super) fn cancel_gif(&mut self) {
        if let Some(job) = &self.gif_job {
            job.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }

    /// The GIF job's answer, once it has one.
    pub(super) fn poll_gif_job(&mut self) {
        use std::sync::mpsc::TryRecvError;
        let Some(job) = &self.gif_job else { return };
        let result = match job.rx.try_recv() {
            Ok(r) => r,
            Err(TryRecvError::Empty) => return,
            // The worker died without an answer: its MP4 is in the work
            // folder, swept to recordings on the next start.
            Err(TryRecvError::Disconnected) =>
                GifResult::Failed { error: "the GIF worker stopped".into(), kept: None },
        };
        self.gif_job = None;
        self.show_converting(false);
        match result {
            GifResult::Done(path) => {
                notify("Frame Fang: GIF saved", &saved_clip_notice(&path), false);
                self.last_recording = Some(path);
            }
            GifResult::Failed { error, kept } => {
                let cancelled = error == "cancelled";
                let body = match &kept {
                    Some(p) if cancelled => format!("Cancelled. The recording is kept as {}.", p.display()),
                    Some(p) => format!("{error}\n\nThe recording is kept as {}.", p.display()),
                    None => format!("{error}\n\nThe recording is kept and moves to your recordings folder on the next start."),
                };
                notify("Frame Fang: GIF not made", &body, !cancelled);
                if let Some(p) = kept { self.last_recording = Some(p); }
            }
        }
    }

    /// The recording hotkey: start a recording, or stop the one running.
    ///
    /// It records without sound, like the plain Record row of the tray
    /// menu it is shown on. One key has no way to ask which sources to
    /// use, and guessing would make the same key give different files on
    /// different days, so the sound choices stay in the menu.
    pub(super) fn toggle_recording(&mut self, loop_target: &ActiveEventLoop) {
        if self.recorder.is_recording() {
            self.stop_recording();
        } else if self.gif_job.is_some() {
            self.refuse_if_recording_busy();
        } else {
            self.start_recording(RecordingOptions::NONE, loop_target, None);
        }
    }

    /// Starts recording `region` (or every screen when `None`) with the
    /// sound sources in `opts`, into the recordings folder under the file
    /// name pattern. A GIF recording goes to a work folder as a silent MP4
    /// and becomes the GIF when it stops. While it runs, a small floating
    /// panel with a blinking dot, the time and a STOP button gives a
    /// one-click way to end it.
    pub(super) fn start_recording(&mut self, opts: RecordingOptions, loop_target: &ActiveEventLoop,
                                  region: Option<CaptureRect>) {
        // The capture window's Record button can be pressed while another
        // recording runs, so a refusal has to be said, not just logged.
        if self.refuse_if_recording_busy() { return; }
        if !self.have_ffmpeg(loop_target, "Recording") { return; }
        let dir = recordings_directory_for(&self.settings);
        let (w, h) = region.map(|r| (r.width, r.height)).unwrap_or((0, 0));
        let facts = framefang_core::file_name::NameFacts::at(Local::now(), w, h);
        let gif = self.settings.record_format() == framefang_core::settings::RecordFormat::Gif;
        let target = if gif { Some(self.gif_work_file()) } else { self.claim_recording_name(&dir, &facts) };
        let Some(out) = target else { return };
        // A GIF has no sound.
        let opts = if gif { RecordingOptions::NONE } else { opts };
        self.gif_plan = gif.then(|| GifPlan {
            work: out.clone(), dir: dir.clone(),
            pattern: self.settings.file_name_pattern.clone(), facts,
        });
        match self.recorder.start_region(out.clone(), opts, region) {
            Ok(effective) => self.recording_started(loop_target, region, opts, effective, &out, if gif { &dir } else { &out }),
            Err(e) => {
                eprintln!("recorder did not start: {e}");
                self.gif_plan = None;
                // Nothing was recorded, so the claimed name is given back.
                if is_empty_file(&out) {
                    let _ = std::fs::remove_file(&out);
                }
                report_recording_problem("Frame Fang: recording failed", e.to_string());
            }
        }
    }

    /// Where a GIF recording's MP4 goes while it runs.
    fn gif_work_file(&self) -> PathBuf {
        let work = gif_work_dir();
        let _ = std::fs::create_dir_all(&work);
        work.join(format!("{}-{}.mp4", Local::now().timestamp_millis(), std::process::id()))
    }

    /// The first free recording name in `dir`. The recorder creates the
    /// file itself, so the placeholder the claim made is removed again.
    /// `None` (already announced) when the folder can't be written.
    fn claim_recording_name(&self, dir: &std::path::Path, facts: &framefang_core::file_name::NameFacts) -> Option<PathBuf> {
        let _ = std::fs::create_dir_all(dir);
        match framefang_core::file_name::claim(dir, &self.settings.file_name_pattern, facts, "mp4") {
            Ok((path, file)) => {
                drop(file);
                let _ = std::fs::remove_file(&path);
                Some(path)
            }
            Err(e) => {
                notify("Frame Fang: recording failed", &format!("{}: {e}", dir.display()), true);
                None
            }
        }
    }

    /// The recorder is running: mark the tray, float the REC panel where
    /// it is safe, and say what is being recorded and how to stop it.
    fn recording_started(&mut self, loop_target: &ActiveEventLoop, region: Option<CaptureRect>,
                         asked: RecordingOptions, effective: RecordingOptions,
                         out: &std::path::Path, saving_to: &std::path::Path) {
        // The notice describes the file as it will be: sound is best-effort,
        // and a recording without a reachable audio server still gets its
        // video, so promising a track that is not there would mislead.
        let sound = if self.gif_plan.is_some() { "GIF, no sound".to_owned() } else { audio_label_for(effective).to_string() };
        let dropped = audio_downgrade_note(asked, effective);
        let scope = scope_words(region);
        eprintln!("recording {scope}, {sound}, to {}", out.display());
        if let Some(t) = &self.tray { t.set_recording(true); }
        self.show_rec_panel(loop_target, region);
        let how_to_stop = if self.recording_view.is_some() {
            "Stop it with the floating STOP button, which stays out of the video, or from the tray menu."
        } else {
            "Stop it from the tray menu."
        };
        notify("Frame Fang: recording started",
            &format!("{scope} - {sound}{dropped}\nSaving to {}\n\n{how_to_stop}", saving_to.display()),
            true);
    }

    /// Floats the REC panel clear of the recorded area, where it can stay
    /// out of the video.
    ///
    /// Windows and macOS keep the panel out of screen recordings by
    /// themselves; placing it off the region also keeps it from covering
    /// what is being recorded. X11 has no such exclusion, so there the
    /// panel only appears when it fits wholly outside a region (never for
    /// the whole screen). On Wayland the compositor places windows as it
    /// likes and the ScreenCast stream excludes nothing, so there is no
    /// panel at all. If the system refuses the window, the tray menu is
    /// still there to stop the recording.
    fn show_rec_panel(&mut self, loop_target: &ActiveEventLoop, region: Option<CaptureRect>) {
        let spot = indicator_placement(
            self.indicator_bounds(loop_target, region),
            region,
            crate::recording_indicator::SIZE,
            24,
        );
        let stays_out_of_video = cfg!(not(target_os = "linux"))
            || (region.is_some() && spot.clear_of_region && !framefang_platform::session::is_wayland());
        if stays_out_of_video {
            match RecordingIndicator::new_at(loop_target, (spot.x, spot.y)) {
                Ok(v) => self.recording_view = Some(v),
                Err(e) => eprintln!("REC panel not shown ({e}); the tray menu stops the recording."),
            }
        } else if cfg!(target_os = "linux") {
            eprintln!("REC panel left out: on this Linux desktop it would be in the video. The tray menu stops the recording.");
        }
    }

    /// The desktop area the REC panel is placed in. With a region, that is
    /// the desktop the region was picked on, so the panel can keep clear
    /// of it. Without one there is nothing to keep clear of, and the panel
    /// goes to the top right of the primary monitor as it always has.
    pub(super) fn indicator_bounds(&self, loop_target: &ActiveEventLoop, region: Option<CaptureRect>) -> DesktopBounds {
        if let (Some(_), Some(picked_on)) = (region, self.overlay_desktop) {
            return picked_on;
        }
        let monitor = loop_target.primary_monitor().or_else(|| loop_target.available_monitors().next());
        let size = monitor.map_or(winit::dpi::PhysicalSize::new(1920, 1080), |m| m.size());
        DesktopBounds::new(0, 0, size.width, size.height)
    }

    /// Ends the recording, finishes its file and closes the REC panel.
    pub(super) fn stop_recording(&mut self) {
        // `stop()` gives up the recorder's path, and a recording that
        // fails to finish often still leaves a playable file there, which
        // "Annotate last recording" should still be able to reach.
        let pending = self.recorder.output_path().map(|p| p.to_path_buf());
        let stopped = self.recorder.stop();
        if let Some(t) = &self.tray { t.set_recording(false); }
        // A GIF recording: whatever MP4 there is goes on to become the GIF.
        if let Some(plan) = self.gif_plan.take() {
            self.recording_view = None;
            if has_bytes(&plan.work) {
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
                eprintln!("recording saved to {}", path.display());
                notify("Frame Fang: recording saved", &saved_clip_notice(&path), false);
                self.last_recording = Some(path);
            }
            Err(e) => {
                eprintln!("stopping the recorder: {e}");
                // A killed encoder still leaves its bytes, and the error names
                // the file, so the menu should be able to reach it. An empty
                // or missing file is not offered: there is nothing to open.
                if let Some(p) = pending.filter(|p| has_bytes(p)) {
                    self.last_recording = Some(p);
                }
                // The person thought this was recording, and there is no
                // usable file, so they hear about it.
                report_recording_problem("Frame Fang: recording not saved", e.to_string());
            }
        }
        self.recording_view = None;
    }

    /// The tray's Annotate last recording: the newest clip opens in the
    /// editor on its first frame (taken by ffmpeg at the clip's own size),
    /// to be drawn on with the usual tools. The editor's commit hands back
    /// transparent overlays that `burn_annotations` lays over the clip.
    pub(super) fn annotate_last_recording(&mut self, loop_target: &ActiveEventLoop) {
        if self.capturing || self.overlay.is_some() { return; }
        if self.recording_busy() {
            notify("Frame Fang: annotate", "Stop the current recording first, and let its GIF finish.", true);
            return;
        }
        let latest = self.last_recording.clone().filter(|p| p.exists())
            .or_else(|| newest_recording(&recordings_directory_for(&self.settings)));
        let Some(video) = latest else {
            notify("Frame Fang: annotate", "No recording found. Record something first.", true);
            return;
        };
        let frame = match extract_first_frame(&video) {
            Ok(f) => f,
            Err(e) => return notify("Frame Fang: annotate failed", &e.to_string(), true),
        };
        // The length drives the editor's timeline. When it can't be read
        // (ffmpeg says N/A) the timeline is hidden and the whole clip is
        // annotated as one.
        let duration = probe_duration(&video)
            .map_err(|e| eprintln!("framefang: couldn't read the clip's length: {e}"))
            .ok();
        match Overlay::new_for_video(loop_target, frame, self.settings.clone(), video.clone(), duration) {
            Ok(ov) => {
                self.overlay = Some(ov);
                self.video_annotate = Some(video);
            }
            Err(e) => notify("Frame Fang: annotate failed", &e.to_string(), true),
        }
    }

    /// Draws the editor's annotations into a copy of `video`, each during
    /// its own stretch of time (untimed ones over the whole clip). Making
    /// the overlays takes an ffmpeg seek per start time and re-encoding a
    /// long clip takes a while, so both happen on a worker thread and the
    /// tray stays responsive; a notice follows when it is done. The copy is
    /// named after the original (`<stem>_annotated`), which stays as it is.
    pub(super) fn burn_annotations(&mut self, video: PathBuf, commit: VideoCommit) {
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
            &format!("Drawing your annotations into a copy of the clip.\nIt will be saved as {}", out.display()),
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
            if result.is_err() && is_empty_file(&out) {
                let _ = std::fs::remove_file(&out);
            }
            match result {
                Ok(path) => notify("Frame Fang: annotated recording saved", &path.display().to_string(), false),
                Err(e) => notify("Frame Fang: annotate failed", &e, true),
            }
        });
    }

    /// The tray's Open screenshots folder: the folder captures are saved
    /// to, in the system's file manager.
    pub(super) fn open_save_folder(&self) {
        open_url(&save_directory(&self.settings).to_string_lossy());
    }

    /// The tray's Open recordings folder, made first if it is not there.
    pub(super) fn open_recordings_folder(&self) {
        let dir = recordings_directory_for(&self.settings);
        let _ = std::fs::create_dir_all(&dir);
        open_url(&dir.to_string_lossy());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_recording_says_whether_it_covers_a_region_or_the_whole_screen() {
        let desktop = DesktopBounds::new(0, 0, 1920, 1080);
        let r = record_rect_from_selection((4, 8, 640, 360), desktop).expect("a valid region");
        assert_eq!(scope_words(Some(r)), format!("Region {}x{}", r.width, r.height));
        assert_eq!((r.width, r.height), (640, 360));
        assert_eq!(scope_words(None), "Whole screen");
    }

    #[test]
    fn a_saved_clip_notice_names_the_file_then_the_menu_row() {
        let text = saved_clip_notice(std::path::Path::new("clips/take.mp4"));
        assert!(text.starts_with("clips"), "{text}");
        assert!(text.contains("take.mp4\n\n"), "{text}");
        assert!(text.contains("\"Annotate last recording\""), "{text}");
    }

    #[test]
    fn empty_and_filled_files_are_told_apart() {
        let dir = std::env::temp_dir().join(format!("framefang-rec-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let (empty, full, missing) = (dir.join("empty.mp4"), dir.join("full.mp4"), dir.join("missing.mp4"));
        std::fs::write(&empty, b"").unwrap();
        std::fs::write(&full, b"moov").unwrap();
        assert!(is_empty_file(&empty) && !has_bytes(&empty));
        assert!(has_bytes(&full) && !is_empty_file(&full));
        assert!(!has_bytes(&missing) && !is_empty_file(&missing), "a missing file is neither");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
