//! winit's side: the loop's callbacks, and handing each window event to
//! the window it belongs to.

use super::*;

/// How often the timed poll runs while nothing else wakes the loop (about
/// 30 times a second).
const TICK: Duration = Duration::from_millis(33);

/// Which of the app's windows an event is for.
#[derive(Clone, Copy)]
enum Owner {
    TrayMenu,
    ColorPick,
    Editor,
    Toast,
    ScrollHud,
    Settings,
    RecPanel,
    About,
    Welcome,
    Updates,
    ConvertImage,
    ConvertVideo,
    Pin(usize),
}

/// Outcomes that hand a finished picture out of an in-place capture. The
/// selection they came from is then the region "Capture last region"
/// takes again.
fn hands_out_picture(out: &OverlayOutcome) -> bool {
    matches!(out, OverlayOutcome::Copied(_) | OverlayOutcome::CopiedOpen(_)
        | OverlayOutcome::Accepted(_) | OverlayOutcome::SaveAs(_) | OverlayOutcome::Pinned(..))
}

/// Outcomes after which the capture window or editor closes. (Save As
/// closes it too, but only once the file is written.)
fn closes_editor(out: &OverlayOutcome) -> bool {
    matches!(out, OverlayOutcome::Cancelled | OverlayOutcome::Accepted(_) | OverlayOutcome::AcceptedVideo(_)
        | OverlayOutcome::Copied(_) | OverlayOutcome::Pinned(..) | OverlayOutcome::RecordRegion(_)
        | OverlayOutcome::Chosen(_) | OverlayOutcome::Scroll(..))
}

impl TrayApp {
    /// The window `id` belongs to, if it is one of ours. Events for any
    /// other id (a window already closed, say) are ignored.
    fn owner_of(&self, id: WindowId) -> Option<Owner> {
        let windows = [
            (self.tray_menu.as_ref().map(|w| w.window_id()), Owner::TrayMenu),
            (self.color_pick.as_ref().map(|w| w.window_id()), Owner::ColorPick),
            (self.overlay.as_ref().map(|w| w.window_id()), Owner::Editor),
            (self.toast.as_ref().map(|w| w.window_id()), Owner::Toast),
            (self.scroll_hud.as_ref().map(|w| w.window_id()), Owner::ScrollHud),
            (self.settings_view.as_ref().map(|w| w.window_id()), Owner::Settings),
            (self.recording_view.as_ref().map(|w| w.window_id()), Owner::RecPanel),
            (self.about_view.as_ref().map(|w| w.window_id()), Owner::About),
            (self.firstrun_view.as_ref().map(|w| w.window_id()), Owner::Welcome),
            (self.updates_view.as_ref().map(|w| w.window_id()), Owner::Updates),
            (self.convert_image_view.as_ref().map(|w| w.window_id()), Owner::ConvertImage),
            (self.convert_video_view.as_ref().map(|w| w.window_id()), Owner::ConvertVideo),
        ];
        windows.into_iter().find(|(w, _)| *w == Some(id)).map(|(_, owner)| owner)
            .or_else(|| self.pinned.iter().position(|p| p.window_id() == id).map(Owner::Pin))
    }

    /// Without a tray icon, the window being closed may be the last way to
    /// reach the app, and closing it has to quit.
    fn running_without_tray(&self) -> bool {
        self.tray_built && self.tray.is_none()
    }

    /// One event for the capture window or editor, and everything its
    /// answer leads to, in this order: the eyedropper or Save As, the
    /// "Copied" pill's spot, saving the editor's own setting changes, the
    /// remembered region, closing the window, and then the picture's way
    /// out (saved, copied, pinned, recorded, read...). Recording starts
    /// last of all, once the window is gone, so its first frames do not
    /// show the dimmed picker.
    fn editor_event(&mut self, loop_target: &ActiveEventLoop, ev: WindowEvent) {
        let Some(ov) = self.overlay.as_mut() else { return self.drain_overlay_patch() };
        let out = ov.handle_event(ev);
        let mut picked = if hands_out_picture(&out) { ov.picker_rect() } else { None };
        // Read on the event that picked it: the window may close below.
        if let Some(r) = ov.take_picked_rect() { picked = Some(r); }
        let mut closes = closes_editor(&out);
        match &out {
            OverlayOutcome::PickScreenColor => self.open_color_pick(loop_target),
            OverlayOutcome::SaveAs(img) => closes = self.save_as_from_editor(img),
            _ => {}
        }
        // Only a copy that closes the window leaves a pill, placed by the
        // window before it goes.
        let pill_at = if closes && matches!(out, OverlayOutcome::Copied(_)) {
            self.overlay.as_ref().and_then(|ov| ov.notice_spot(crate::toast::size()))
        } else { None };
        // A tool, knob or mode the editor changed is saved before it closes.
        self.drain_overlay_patch();
        if let Some(r) = picked {
            self.session_facts = Some(facts_at(&self.session_windows, r));
            self.remember_region(r);
        }
        if closes { self.close_editor(&out); }
        match out {
            OverlayOutcome::ReadText(img, kind, flip) => self.start_read(img, kind, flip),
            // The frozen screen is gone, so the real page can scroll now.
            OverlayOutcome::Scroll(area, flip) => self.start_scroll(loop_target, area, flip),
            // The picker let go of the desktop: the picture opens in its own
            // editor window, where it was on screen.
            OverlayOutcome::Chosen(c) => {
                let copy_first = self.region_copies_first();
                self.open_chosen(loop_target, *c, copy_first);
            }
            OverlayOutcome::AcceptedVideo(commit) => {
                if let Some(video) = self.video_annotate.take() { self.burn_annotations(video, commit); }
            }
            OverlayOutcome::Accepted(img) => {
                // With auto-copy the finished picture replaces the raw one
                // copied when the region was picked.
                if self.settings.auto_copy { self.copy_final(img.clone()); }
                self.save_final(img);
            }
            OverlayOutcome::Copied(img) | OverlayOutcome::CopiedOpen(img) => self.copy_with_toast(loop_target, img, pill_at),
            OverlayOutcome::Pinned(img, at) => self.pin(loop_target, img, at),
            OverlayOutcome::RecordRegion(r) => self.start_region_from_selection(r, loop_target),
            // Continue, Cancelled and SaveAs need nothing more; PickScreenColor
            // was handled above, and only an eyedropper session answers
            // with ColorPicked.
            OverlayOutcome::Continue | OverlayOutcome::Cancelled | OverlayOutcome::SaveAs(_)
            | OverlayOutcome::PickScreenColor | OverlayOutcome::ColorPicked(_) => {}
        }
    }

    /// Everything that ends with the capture window or editor.
    fn close_editor(&mut self, out: &OverlayOutcome) {
        self.overlay = None;
        // An eyedropper session has no editor left to answer, nor does a
        // read that waited on it.
        self.color_pick = None;
        self.read_job = None;
        // A cancelled region pick must not leave its sound choice set for
        // the next capture window that commits.
        if !matches!(out, OverlayOutcome::RecordRegion(_)) { self.pending_region_audio = None; }
        // Annotating a clip ends with its window; only AcceptedVideo carries
        // something to draw into it.
        if !matches!(out, OverlayOutcome::Accepted(_) | OverlayOutcome::AcceptedVideo(_)) { self.video_annotate = None; }
    }

    /// Save As with the editor hidden, so the file dialog has the screen.
    /// `true` when the file was written and the editor can close; on a
    /// cancel or a failed write it comes back.
    fn save_as_from_editor(&mut self, img: &image::RgbaImage) -> bool {
        if let Some(ov) = self.overlay.as_mut() { ov.set_hidden(true); }
        let written = self.save_final_as(img);
        if !written {
            if let Some(ov) = self.overlay.as_mut() { ov.set_hidden(false); }
        }
        written
    }

    /// The editor's eyedropper: the whole screen frozen again, editor and
    /// all, to pick a color from anywhere. If that can't open, the editor
    /// is told the pick is off.
    fn open_color_pick(&mut self, loop_target: &ActiveEventLoop) {
        let opened = capture_all_screens()
            .map_err(|e| format!("screen capture: {e}"))
            .and_then(|shot| {
                let geom = shot.geometry();
                let mut pick = Overlay::new_for_color_pick(loop_target, geom, shot.bitmap, self.settings.clone())
                    .map_err(|e| format!("window: {e}"))?;
                pick.set_display_map(shot.map);
                Ok(pick)
            });
        match opened {
            Ok(pick) => self.color_pick = Some(pick),
            Err(why) => {
                eprintln!("framefang: eyedropper {why}");
                if let Some(ov) = self.overlay.as_mut() { ov.screen_color_cancelled(); }
            }
        }
    }

    /// An event for the eyedropper's window. Its color, or its cancel, goes
    /// back to the editor.
    fn color_pick_event(&mut self, ev: WindowEvent) {
        let Some(pick) = self.color_pick.as_mut() else { return };
        match pick.handle_event(ev) {
            OverlayOutcome::ColorPicked(c) => {
                self.color_pick = None;
                if let Some(ov) = self.overlay.as_mut() { ov.take_screen_color(c); }
            }
            OverlayOutcome::Cancelled => {
                self.color_pick = None;
                if let Some(ov) = self.overlay.as_mut() { ov.screen_color_cancelled(); }
            }
            _ => {}
        }
    }

    /// Pins a picture on top of everything at `at`.
    fn pin(&mut self, loop_target: &ActiveEventLoop, img: image::RgbaImage, at: (i32, i32)) {
        self.keep_recent(&img);
        if self.settings.auto_copy { self.copy_final(img.clone()); }
        match PinView::new(loop_target, img, at) {
            Ok(pv) => self.pinned.push(pv),
            Err(e) => {
                eprintln!("pin window: {e}");
                notify("Frame Fang: pin failed", &framefang_core::failure::pin_failure(&e.to_string()), true);
            }
        }
    }

    fn about_answer(&mut self, loop_target: &ActiveEventLoop, answer: AboutOutcome) {
        match answer {
            AboutOutcome::Closed => {
                self.about_view = None;
                // With no tray this window was all there was, and the
                // tray's Exit, the only other way out, does not exist.
                if self.running_without_tray() { loop_target.exit(); }
            }
            AboutOutcome::OpenProject => { open_url("https://github.com/winters27/framefang"); }
            AboutOutcome::OpenAuthor => { open_url("https://github.com/winters27"); }
            AboutOutcome::OpenUpdates => {
                self.about_view = None;
                self.show_updates(loop_target);
                // The Updates window takes over as the last way in, but
                // only if it actually opened.
                if self.running_without_tray() && self.updates_view.is_none() { loop_target.exit(); }
            }
        }
    }

    fn updates_answer(&mut self, loop_target: &ActiveEventLoop, answer: UpdatesOutcome) {
        match answer {
            UpdatesOutcome::Closed => {
                self.updates_view = None;
                // Reached from the no-tray About window (its Check for
                // updates closes it), this was the last way in.
                if self.running_without_tray() && self.about_view.is_none() { loop_target.exit(); }
            }
            UpdatesOutcome::OpenReleases => { open_url("https://github.com/winters27/framefang/releases"); }
            UpdatesOutcome::OpenAsset(url) => { open_url(&url); }
            UpdatesOutcome::DownloadAndInstall { asset_url, expected_sha256 } => {
                self.start_self_update(asset_url, expected_sha256);
            }
            // Installed by a package manager (Snap, Flatpak, Homebrew,
            // deb/rpm/AUR, Scoop): the window offers the upgrade command,
            // and the button puts it on the clipboard. If that fails the
            // command is shown in the notice, since it is all there is.
            UpdatesOutcome::CopyCommand(cmd) => match framefang_platform::copy_text(&cmd) {
                Ok(()) => notify("Frame Fang: update command copied", &cmd, false),
                Err(e) => notify("Frame Fang: copy failed",
                    &format!("The update command couldn't be put on the clipboard ({e}). Run it yourself:\n{cmd}"),
                    true),
            },
        }
    }
}

impl ApplicationHandler for TrayApp {
    fn resumed(&mut self, loop_target: &ActiveEventLoop) {
        // The first moment the system's event loop is sure to be running,
        // which the tray icon needs.
        self.ensure_tray(loop_target);
    }

    fn window_event(&mut self, loop_target: &ActiveEventLoop, id: WindowId, ev: WindowEvent) {
        let mut from_settings = Vec::new();
        match self.owner_of(id) {
            // Runs its own follow-up, including saving the editor's changes.
            Some(Owner::Editor) => return self.editor_event(loop_target, ev),
            Some(Owner::ColorPick) => self.color_pick_event(ev),
            Some(Owner::Settings) => {
                if let Some(view) = self.settings_view.as_mut() {
                    view.handle_event(ev);
                    from_settings = view.take_outcomes();
                }
            }
            Some(Owner::RecPanel) => {
                let stop = self.recording_view.as_mut().is_some_and(|v| {
                    v.handle_event(ev);
                    v.stop_requested
                });
                if stop { self.stop_recording(); }
            }
            Some(Owner::About) => {
                let answer = self.about_view.as_mut().and_then(|v| { v.handle_event(ev); v.outcome.take() });
                if let Some(a) = answer { self.about_answer(loop_target, a); }
            }
            Some(Owner::Welcome) => {
                let answer = self.firstrun_view.as_mut().and_then(|v| { v.handle_event(ev); v.outcome.take() });
                // Closing the welcome window leaves Frame Fang in the tray,
                // which is what the window is there to show.
                if matches!(answer, Some(FirstRunOutcome::Closed)) { self.firstrun_view = None; }
            }
            Some(Owner::Updates) => {
                let answer = self.updates_view.as_mut().and_then(|v| { v.handle_event(ev); v.outcome.take() });
                if let Some(a) = answer { self.updates_answer(loop_target, a); }
            }
            Some(Owner::ConvertImage) => {
                let answer = self.convert_image_view.as_mut().and_then(|v| { v.handle_event(ev); v.outcome.take() });
                if matches!(answer, Some(ConvertImageOutcome::Closed)) { self.convert_image_view = None; }
            }
            Some(Owner::ConvertVideo) => {
                let answer = self.convert_video_view.as_mut().and_then(|v| { v.handle_event(ev); v.outcome.take() });
                if matches!(answer, Some(ConvertVideoOutcome::Closed)) { self.convert_video_view = None; }
            }
            Some(Owner::Pin(i)) => {
                let closed = self.pinned.get_mut(i).is_some_and(|p| p.handle_event(ev));
                if closed { self.pinned.swap_remove(i); }
            }
            Some(Owner::Toast) => {
                if matches!(ev, WindowEvent::RedrawRequested) {
                    if let Some(t) = self.toast.as_mut() { t.redraw(); }
                }
            }
            Some(Owner::ScrollHud) => {
                if matches!(ev, WindowEvent::RedrawRequested) {
                    if let Some(h) = self.scroll_hud.as_mut() { h.redraw(); }
                }
            }
            Some(Owner::TrayMenu) => {
                if let Some(m) = self.tray_menu.as_mut() { m.handle_event(ev); }
                self.finish_tray_menu(loop_target);
            }
            None => {}
        }
        self.drain_overlay_patch();
        for out in from_settings {
            if self.apply_settings_outcome(out) {
                self.settings_view = None;
                break;
            }
        }
    }

    fn about_to_wait(&mut self, loop_target: &ActiveEventLoop) {
        // In case `resumed` never comes on some system; does nothing once
        // the icon exists.
        self.ensure_tray(loop_target);
        // Here and not in `resumed`, and after `ensure_tray`, so the welcome
        // window can never open before the icon it points at.
        self.maybe_show_first_run(loop_target);
        // The "Copied" pill fades on a timer, so each pass moves it on.
        if self.toast.as_mut().is_some_and(|t| !t.tick()) { self.toast = None; }
        let now = Instant::now();
        if now.duration_since(self.last_tick) >= TICK {
            self.poll(loop_target);
            self.last_tick = now;
        }
        loop_target.set_control_flow(ControlFlow::WaitUntil(now + TICK));
    }
}
