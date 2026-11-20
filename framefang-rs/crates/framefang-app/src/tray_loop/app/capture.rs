//! Taking pictures: the capture window, full-screen shots, "Capture last
//! region", scrolling captures, reading text and QR codes, and what happens
//! to a finished picture (copied, saved, kept as a recent capture).

use super::*;

/// The monitor holding the point `(x, y)`, in virtual-screen pixels.
fn monitor_under(monitors: &[MonitorRect], (x, y): (i32, i32)) -> Option<MonitorRect> {
    monitors.iter().copied().find(|m| {
        (m.x..m.x + m.width as i32).contains(&x) && (m.y..m.y + m.height as i32).contains(&y)
    })
}

/// The middle of a rectangle given as (x, y, width, height).
fn middle_of(r: (i32, i32, i32, i32)) -> (i32, i32) {
    (r.0 + r.2 / 2, r.1 + r.3 / 2)
}

/// Logs and announces a screenshot that reached the disk.
fn tell_saved(title: &str, path: &std::path::Path) {
    eprintln!("screenshot written to {}", path.display());
    notify(title, &path.display().to_string(), false);
}

/// Logs and announces a screenshot that did not.
fn tell_not_saved(e: &dyn std::fmt::Display) {
    eprintln!("screenshot not written: {e}");
    notify("Frame Fang: screenshot not saved", &e.to_string(), true);
}

const CAPTURE_FAILED: &str = "Frame Fang: capture failed";
const EDITOR_FAILED: &str = "Frame Fang: couldn't open the editor";
const CAPTURE_GONE: (&str, &str) = ("Frame Fang: that capture is gone", "It was replaced by newer ones.");

impl TrayApp {
    /// A new capture session: what it outputs is one recent item.
    pub(super) fn new_session(&mut self) { self.session_id += 1; }

    /// Keep `img` as this session's recent capture (unframed).
    pub(super) fn keep_recent(&self, img: &image::RgbaImage) {
        if self.settings.keep_recent { self.recent.record(self.session_id, img.clone()); }
    }

    /// The tray's RECENT item `index` (newest first): read it back to
    /// copy it or to open it in the editor.
    pub(super) fn open_recent(&mut self, index: usize, edit: bool) {
        match self.recent_list.get(index) {
            Some(e) => self.recent.load(&e.id, edit),
            None => notify(CAPTURE_GONE.0, CAPTURE_GONE.1, false),
        }
    }

    /// What the recent worker said: a new list for the menus, or a
    /// picture read back to copy or edit.
    pub(super) fn poll_recent(&mut self, loop_target: &ActiveEventLoop) {
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
                    // Copied from the tray: the "Copied" pill appears beside
                    // the icon, just up and left of where it was clicked.
                    if self.copy_final(img) {
                        let (pill_w, pill_h) = crate::toast::size();
                        let spot = self.tray_click_at.map(|(x, y)| (x - pill_w as i32, y - pill_h as i32 - 16));
                        if let Some(Ok(t)) = spot.map(|s| crate::toast::Toast::new(loop_target, s)) {
                            self.toast = Some(t);
                        }
                    }
                }
                Reply::Loaded { img, edit: true } => self.edit_picture(loop_target, img),
                Reply::Gone => notify(CAPTURE_GONE.0, CAPTURE_GONE.1, false),
            }
        }
    }

    /// Start following the page under `area` (desktop pixels) and show
    /// the S1 look around it. `flip`: the switch key was held as it was
    /// picked, so the result does the other thing than the setting says.
    pub(super) fn start_scroll(&mut self, loop_target: &ActiveEventLoop, area: (i32, i32, i32, i32), flip: bool) {
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
    pub(super) fn poll_scroll(&mut self, loop_target: &ActiveEventLoop) {
        use crate::scroll::{End, Event};
        if let Some(h) = self.scroll_hud.as_mut() { h.tick(); }
        let Some(session) = &self.scroll else { return };
        let area = session.area;
        for ev in session.poll() {
            match ev {
                Event::Progress(h) => {
                    if let Some(hud) = self.scroll_hud.as_mut() { hud.set_height(h); }
                }
                Event::Lost(lost) => {
                    if let Some(hud) = self.scroll_hud.as_mut() { hud.set_lost(lost); }
                }
                Event::Done { img, end } => {
                    self.scroll = None;
                    self.scroll_hud = None;
                    match end {
                        End::Cancelled => {}
                        End::Failed(_) => notify("Frame Fang: scrolling capture failed",
                                                 &end.notice(0).unwrap_or_default(), true),
                        _ => {
                            if let Some(n) = end.notice(img.height()) { notify("Frame Fang: scrolling capture", &n, false); }
                            self.finish_scroll(loop_target, img, area);
                        }
                    }
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
            let work = crate::tray_menu::work_area(loop_target, middle_of(area));
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
    pub(super) fn open_tall(&mut self, loop_target: &ActiveEventLoop, t: crate::editor::TallChosen, copy_first: bool) {
        if copy_first {
            self.keep_recent(&t.picture);
            self.copy_final(t.picture.clone());
        }
        match Overlay::new_for_tall_editor(loop_target, t, self.settings.clone()) {
            Ok(ov) => self.overlay = Some(ov),
            Err(e) => {
                eprintln!("scrolling editor: {e}");
                notify(EDITOR_FAILED, &e.to_string(), true);
            }
        }
    }

    /// Opens the full capture window over a frozen desktop with `pic`
    /// (virtual-screen pixels) already selected, as a drag that has just
    /// ended would leave it.
    fn open_in_place(&mut self, loop_target: &ActiveEventLoop, geom: DesktopGeometry,
                     bitmap: image::RgbaImage, map: framefang_core::dpi::DisplayMap,
                     bounds: DesktopBounds, pic: (i32, i32, i32, i32)) -> Result<()> {
        let mut ov = Overlay::new(loop_target, geom, bitmap, self.settings.clone())?;
        ov.set_display_map(map);
        ov.preselect(pic);
        self.overlay_desktop = Some(bounds);
        self.pending_region_audio = None;
        self.overlay = Some(ov);
        Ok(())
    }

    /// Open `img` (a recent capture) in the editor: in its own window
    /// in the middle of the screen the tray is on, or, when that window
    /// wouldn't fit, over the frozen screen in place. Saving it is a new
    /// recent item.
    pub(super) fn edit_picture(&mut self, loop_target: &ActiveEventLoop, img: image::RgbaImage) {
        if self.capturing || self.overlay.is_some() {
            notify("Frame Fang: busy", "Finish what is open first, then try again.", false);
            return;
        }
        let Ok(shot) = capture_all_screens() else {
            notify(EDITOR_FAILED, "The screen couldn't be read.", true);
            return;
        };
        self.new_session();
        self.session_facts = None;
        let geom = shot.geometry();
        let at = self.tray_click_at.unwrap_or(geom.origin());
        let monitors = geom.monitors();
        let Some(m) = monitor_under(monitors, at).or_else(|| monitors.first().copied()) else { return };
        let (w, h) = (img.width() as i32, img.height() as i32);
        let pic = (m.x + (m.width as i32 - w) / 2, m.y + (m.height as i32 - h) / 2, w, h);
        if let Some(c) = crate::editor::chosen_for(img.clone(), pic, monitors) {
            if let Err(e) = Overlay::new_for_editor(loop_target, c, self.settings.clone()).map(|ov| self.overlay = Some(ov)) {
                notify(EDITOR_FAILED, &e.to_string(), true);
            }
            return;
        }
        // Taller than the screen (a scrolling capture): the scrolling
        // editor.
        if h > m.height as i32 {
            let work = crate::tray_menu::work_area(loop_target, at);
            if let Some(t) = crate::editor::tall_for(img.clone(), pic.0, work) {
                if let Err(e) = Overlay::new_for_tall_editor(loop_target, t, self.settings.clone()).map(|ov| self.overlay = Some(ov)) {
                    notify(EDITOR_FAILED, &e.to_string(), true);
                }
                return;
            }
        }
        // Too big for its own window: over the frozen screen, where it fits.
        let (ox, oy) = geom.origin();
        let bounds = shot.desktop_bounds();
        let mut desktop = shot.bitmap;
        let (dw, dh) = (desktop.width() as i32, desktop.height() as i32);
        if w > dw || h > dh {
            notify("Frame Fang: too big to edit", "It is bigger than the screens. It was copied instead.", false);
            self.copy_final(img);
            return;
        }
        let (px, py) = ((pic.0 - ox).clamp(0, dw - w), (pic.1 - oy).clamp(0, dh - h));
        image::imageops::replace(&mut desktop, &img, px as i64, py as i64);
        if let Err(e) = self.open_in_place(loop_target, geom, desktop, shot.map, bounds, (px + ox, py + oy, w, h)) {
            notify(EDITOR_FAILED, &e.to_string(), true);
        }
    }

    pub(super) fn capture(&mut self, loop_target: &ActiveEventLoop) {
        self.capture_after(loop_target, Duration::ZERO);
    }

    /// The picker took `r` (virtual-screen pixels): keep it for "Capture
    /// last region". Written off the event loop; the newest wins.
    pub(super) fn remember_region(&mut self, r: (i32, i32, i32, i32)) {
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

    /// Whether a region capture is on the clipboard before its editor
    /// opens: with auto-copy, or copy-only with the switch key held.
    pub(super) fn region_copies_first(&self) -> bool { self.settings.auto_copy || !self.settings.open_editor }

    /// A picked region goes to its own editor window, on the clipboard
    /// first when `copy_first`.
    pub(super) fn open_chosen(&mut self, loop_target: &ActiveEventLoop, c: ChosenRegion, copy_first: bool) {
        if copy_first {
            self.keep_recent(&c.picture);
            self.copy_final(c.picture.clone());
        }
        match Overlay::new_for_editor(loop_target, c, self.settings.clone()) {
            Ok(ov) => self.overlay = Some(ov),
            Err(e) => {
                eprintln!("editor window: {e}");
                notify(CAPTURE_FAILED, &format!("Couldn't open the editor window: {e}"), true);
            }
        }
    }

    /// Read `img` for text or QR codes, off the event loop. A TEXT or QR
    /// picker waits for the answer; the editor carries on.
    pub(super) fn start_read(&mut self, img: image::RgbaImage, kind: crate::read_text::ReadKind, flip: bool) {
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
    pub(super) fn poll_read_job(&mut self, loop_target: &ActiveEventLoop) {
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
                self.show_read_notice(loop_target, spot, &text, true);
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
        self.show_read_notice(loop_target, spot, &text, done);
    }

    /// The pill under a read picture, when there is somewhere to put it.
    fn show_read_notice(&mut self, loop_target: &ActiveEventLoop, spot: Option<(i32, i32)>, text: &str, done: bool) {
        let Some(spot) = spot else { return };
        match crate::toast::Toast::saying(loop_target, spot, text, done) {
            Ok(t) => self.toast = Some(t),
            Err(e) => eprintln!("framefang: read notice: {e}"),
        }
    }

    /// Copy, and show the "Copied" pill at `spot` once it worked.
    pub(super) fn copy_with_toast(&mut self, loop_target: &ActiveEventLoop,
                                  img: image::ImageBuffer<image::Rgba<u8>, Vec<u8>>, spot: Option<(i32, i32)>) {
        self.keep_recent(&img);
        if !self.copy_final(img) { return; }
        let Some(spot) = spot else { return };
        match crate::toast::Toast::new(loop_target, spot) {
            Ok(t) => self.toast = Some(t),
            Err(e) => eprintln!("framefang: copied notice: {e}"),
        }
    }

    /// Take the last captured rectangle again, then do what a released
    /// drag does: copy it, or open it in its own editor window, or (when
    /// the editor wouldn't fit beside it) edit it in place. Decided
    /// before any capture window exists, so nothing flashes up for a
    /// copy.
    pub(super) fn capture_last_region(&mut self, loop_target: &ActiveEventLoop) {
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
            Ok(shot) => {
                if self.take_region_from(loop_target, shot, (x, y, w, h)).is_none() {
                    notify("Frame Fang: the last region is off screen now", "Drag a new one.", false);
                    self.capturing = false;
                    self.capture_from_hotkey(loop_target);
                    return;
                }
            }
            Err(e) => notify(CAPTURE_FAILED, &e.to_string(), true),
        }
        self.capturing = false;
    }

    /// The second half of "Capture last region": `region` cut out of a
    /// fresh shot and sent where a finished drag would go. `None` when no
    /// part of the region is on any screen any more.
    fn take_region_from(&mut self, loop_target: &ActiveEventLoop, mut shot: framefang_platform::capture::Captured,
                        region: (i32, i32, i32, i32)) -> Option<()> {
        if self.settings.capture_cursor { framefang_platform::cursor::draw_into(&mut shot); }
        let geom = shot.geometry();
        let (ox, oy) = geom.origin();
        let whole = (0, 0, shot.bitmap.width() as i32, shot.bitmap.height() as i32);
        let local = (region.0 - ox, region.1 - oy, region.2, region.3);
        let b = framefang_core::virtual_desktop::intersect_rect(local, whole)?;
        let picture = crate::editor::crop(&shot.bitmap, b);
        let pic = (b.0 + ox, b.1 + oy, b.2, b.3);
        if !self.settings.opens_editor(false) {
            let spot = monitor_under(geom.monitors(), middle_of(pic))
                .map(|m| crate::editor::toast_spot(pic, m, crate::toast::size()));
            self.copy_with_toast(loop_target, picture, spot);
        } else if let Some(c) = crate::editor::chosen_for(picture, pic, geom.monitors()) {
            let copy_first = self.region_copies_first();
            self.open_chosen(loop_target, c, copy_first);
        } else {
            let bounds = shot.desktop_bounds();
            if let Err(e) = self.open_in_place(loop_target, geom, shot.bitmap, shot.map, bounds, pic) {
                notify(CAPTURE_FAILED, &format!("Couldn't open the editor window: {e}"), true);
            }
        }
        Some(())
    }

    /// The tray's Capture and its delayed variants (3, 5 or 10 s): wait
    /// `user_delay`, then open the capture window to pick a region in.
    /// During a countdown the tray keeps being serviced, so its icon (and
    /// its "Cancel pending capture" row) still answer.
    pub(super) fn capture_after(&mut self, loop_target: &ActiveEventLoop, user_delay: Duration) {
        self.capture_inner(loop_target, user_delay, true);
    }

    /// The capture hotkey: no tray menu is open, so there is nothing to
    /// wait out and the screen freezes the moment the key goes down.
    pub(super) fn capture_from_hotkey(&mut self, loop_target: &ActiveEventLoop) {
        self.capture_inner(loop_target, Duration::ZERO, false);
    }

    /// Copy screen text, or Read QR code: the picker opens in that mode.
    /// `settle` for the tray, as `capture_inner`.
    pub(super) fn capture_reading(&mut self, loop_target: &ActiveEventLoop,
                                  kind: crate::read_text::ReadKind, settle: bool) {
        if self.capturing || self.overlay.is_some() { return; }
        self.capture_inner(loop_target, Duration::ZERO, settle);
        if let Some(ov) = self.overlay.as_mut() { ov.open_reading(kind); }
    }

    /// Scrolling capture: the picker opens in SCROLL mode. Its own key
    /// pressed while one runs finishes it.
    pub(super) fn capture_scroll(&mut self, loop_target: &ActiveEventLoop, settle: bool) {
        if let Some(s) = &self.scroll { s.stop(); return; }
        if self.capturing || self.overlay.is_some() || !crate::scroll::supported() { return; }
        self.capture_inner(loop_target, Duration::ZERO, settle);
        if let Some(ov) = self.overlay.as_mut() { ov.open_scroll(); }
    }

    /// Freeze the desktop and open the capture window over it, after
    /// `user_delay`. `settle` is for captures started from the tray menu,
    /// which has to be off the screen before the shot.
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
        let go_ahead = user_delay.is_zero() || self.count_down(user_delay);
        if go_ahead {
            if settle { wait_for_tray_menu(); }
            // A failure is announced, not only logged: whoever pressed the
            // key saw nothing happen, and nobody reads a tray app's stderr.
            match capture_all_screens() {
                Ok(shot) => self.open_capture_window(loop_target, shot, switch_at_press),
                Err(e) => {
                    eprintln!("screen capture: {e}");
                    notify(CAPTURE_FAILED, &e.to_string(), true);
                }
            }
        }
        self.capturing = false;
    }

    /// Waits out a delayed capture's countdown while servicing the tray.
    /// `false` when the person cancelled it from the tray menu. Any other
    /// tray row picked meanwhile is dropped.
    fn count_down(&mut self, delay: Duration) -> bool {
        let secs = delay.as_secs();
        eprintln!("Capture in {secs} s.");
        if let Some(t) = &self.tray { t.set_pending(true); }
        let deadline = Instant::now() + delay;
        let mut cancelled = false;
        while !cancelled && Instant::now() < deadline {
            if let Some(t) = &self.tray {
                t.pump_events();
                cancelled = matches!(t.try_recv(), TrayEvent::CancelPending);
            }
            if cancelled {
                eprintln!("Delayed capture cancelled.");
            } else {
                std::thread::sleep(Duration::from_millis(50));
            }
        }
        if let Some(t) = &self.tray { t.set_pending(false); }
        !cancelled
    }

    /// The capture window over a fresh shot of every screen.
    fn open_capture_window(&mut self, loop_target: &ActiveEventLoop, mut shot: framefang_platform::capture::Captured,
                           switch_at_press: bool) {
        // The windows as the frozen picture shows them, read before the
        // capture window opens on top of them all.
        let windows = framefang_platform::windows_list::on_screen_windows();
        self.session_windows = windows.clone();
        self.session_facts = None;
        // The Start menu and the other shell popups are in the picture now;
        // close the real ones, which would sit above the capture window.
        #[cfg(target_os = "windows")]
        dismiss_shell_popups();
        if self.settings.capture_cursor { framefang_platform::cursor::draw_into(&mut shot); }
        // Should the selection become a recording (the Record button), the
        // rectangle is kept inside this desktop.
        self.overlay_desktop = Some(shot.desktop_bounds());
        self.pending_region_audio = None;
        // The geometry gives the window its place over every monitor and
        // turns a selection back into desktop pixels; the display map does
        // the same between logical and device pixels on scaled screens.
        let geom = shot.geometry();
        match Overlay::new(loop_target, geom, shot.bitmap, self.settings.clone()) {
            Ok(mut ov) => {
                ov.set_display_map(shot.map);
                ov.hold_at_press = switch_at_press;
                ov.set_windows(windows);
                self.overlay = Some(ov);
            }
            Err(e) => {
                eprintln!("capture window: {e}");
                notify(CAPTURE_FAILED, &format!("Couldn't open the editor window: {e}"), true);
            }
        }
    }

    /// Capture Full Screen: every screen, straight to the screenshots
    /// folder (and the clipboard with auto-copy), with no window to pick
    /// or edit in. The notice naming the file is the only sign it worked.
    /// `settle` waits for the tray menu to leave first; the hotkey does not.
    pub(super) fn capture_full_screen(&mut self, settle: bool) {
        if self.capturing || self.overlay.is_some() { return; }
        self.capturing = true;
        self.toast = None;
        // The whole desktop is no one app's.
        self.session_facts = None;
        if settle { wait_for_tray_menu(); }
        match capture_all_screens() {
            Ok(mut shot) => {
                if self.settings.capture_cursor { framefang_platform::cursor::draw_into(&mut shot); }
                let img = shot.bitmap;
                self.new_session();
                self.keep_recent(&img);
                if self.settings.auto_copy { self.copy_final(img.clone()); }
                match self.save_screenshot(&img) {
                    Ok(path) => tell_saved("Frame Fang: full screen saved", &path),
                    Err(e) => tell_not_saved(&e),
                }
            }
            Err(e) => {
                eprintln!("screen capture: {e}");
                notify(CAPTURE_FAILED, &e.to_string(), true);
            }
        }
        self.capturing = false;
    }

    /// Save a screenshot into the screenshots folder (named and framed
    /// per Settings). The folder is kept in Settings the first time.
    pub(super) fn save_screenshot(&mut self, img: &image::ImageBuffer<image::Rgba<u8>, Vec<u8>>) -> Result<PathBuf> {
        let (path, new_folder) = save_capture(&self.settings, img, &self.name_facts(img))?;
        if let Some(dir) = new_folder {
            let p = patch_of(&self.settings, |s| s.save_directory = dir.to_string_lossy().into_owned());
            self.store_patch(p);
        }
        Ok(path)
    }

    /// What this capture's file name is built from: now, its size, and
    /// the app and title of the window it was taken from, if any.
    pub(super) fn name_facts(&self, bmp: &image::ImageBuffer<image::Rgba<u8>, Vec<u8>>) -> framefang_core::file_name::NameFacts {
        let mut facts = name_facts(bmp);
        if let Some((app, title)) = &self.session_facts {
            facts.app = app.clone();
            facts.title = title.clone();
        }
        facts
    }

    /// Saves a finished picture to the screenshots folder and says where.
    /// The capture window is gone by now, so the notice (or the failure
    /// notice) is the only word the person gets.
    pub(super) fn save_final(&mut self, img: image::ImageBuffer<image::Rgba<u8>, Vec<u8>>) {
        self.keep_recent(&img);
        match self.save_screenshot(&img) {
            Ok(path) => tell_saved("Frame Fang: screenshot saved", &path),
            Err(e) => tell_not_saved(&e),
        }
    }

    /// Puts a finished picture on the clipboard, framed as Settings say,
    /// the same pixels `save_final` would write. arboard handles each
    /// system's clipboard; on Linux it serves the picture from its own
    /// thread, which works because Frame Fang keeps running.
    ///
    /// A failed copy is announced: the picture goes nowhere else, and
    /// without the notice the person would paste whatever was there
    /// before. `true` when the picture is on the clipboard.
    pub(super) fn copy_final(&mut self, img: image::ImageBuffer<image::Rgba<u8>, Vec<u8>>) -> bool {
        let framed = crate::frame::for_clipboard(&img, self.settings.output_frame());
        let (width, height) = (framed.width() as usize, framed.height() as usize);
        let data = arboard::ImageData { width, height, bytes: std::borrow::Cow::Owned(framed.into_raw()) };
        match arboard::Clipboard::new().and_then(|mut clip| clip.set_image(data)) {
            Ok(()) => {
                eprintln!("{width}x{height} picture on the clipboard");
                true
            }
            Err(e) => {
                eprintln!("clipboard: {e}");
                notify("Frame Fang: copy failed", &framefang_core::failure::clipboard_failure(&e.to_string()), true);
                false
            }
        }
    }

    /// Save As: the user names the file and picks PNG or JPEG. `true`
    /// once it is written. `false` when the dialog was cancelled or the
    /// write failed, and the editor then comes back with the picture.
    pub(super) fn save_final_as(&mut self, img: &image::ImageBuffer<image::Rgba<u8>, Vec<u8>>) -> bool {
        let suggested = framefang_core::file_name::render(&self.settings.file_name_pattern, &self.name_facts(img));
        let chosen = rfd::FileDialog::new()
            .set_title("Save screenshot as")
            .set_directory(save_directory(&self.settings))
            .set_file_name(format!("{suggested}.png"))
            .add_filter("PNG image", &["png"])
            .add_filter("JPEG image", &["jpg", "jpeg"])
            .save_file();
        let Some(path) = chosen else { return false };
        match write_image(path, &crate::frame::apply(img, self.settings.output_frame())) {
            Ok(path) => {
                self.keep_recent(img);
                if self.settings.auto_copy { self.copy_final(img.clone()); }
                tell_saved("Frame Fang: screenshot saved", &path);
                true
            }
            Err(e) => {
                tell_not_saved(&e);
                false
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_monitor_under_a_point_counts_its_left_and_top_edges_only() {
        let screens = [MonitorRect::new(0, 0, 1920, 1080), MonitorRect::new(1920, -200, 1280, 1024)];
        assert_eq!(monitor_under(&screens, (0, 0)), Some(screens[0]));
        assert_eq!(monitor_under(&screens, (1919, 1079)), Some(screens[0]));
        assert_eq!(monitor_under(&screens, (1920, 0)), Some(screens[1]), "a shared edge belongs to the right-hand one");
        assert_eq!(monitor_under(&screens, (2000, -200)), Some(screens[1]));
        assert_eq!(monitor_under(&screens, (2000, 900)), None, "below the second screen");
        assert_eq!(monitor_under(&screens, (-1, 10)), None);
    }

    #[test]
    fn the_middle_of_a_rectangle_rounds_toward_its_top_left() {
        assert_eq!(middle_of((10, 20, 100, 50)), (60, 45));
        assert_eq!(middle_of((0, 0, 5, 3)), (2, 1));
        assert_eq!(middle_of((-10, -10, 4, 4)), (-8, -8));
    }
}
