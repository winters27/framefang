//! What the tray asks of an open overlay, the coordinate mapping between
//! the framebuffer, the capture and the desktop, and the ways a session
//! finishes.

use framefang_core::dpi::DisplayMap;
use framefang_core::settings::AppSettings;
use framefang_core::settings_store as store;
use framefang_core::virtual_desktop as vdesk;
use image::{ImageBuffer, Rgba};
use winit::keyboard::ModifiersState;
use winit::window::WindowId;

use super::flyout::{step_look, tool_stroke, Slider};
use super::geometry::{action_room, chosen_for, crop, spot_under, Rect4, EDITOR_EDGE};
use super::panels::{Action, CAPTURE_ACTIONS, LOCKED_ACTIONS, RECORD_ACTIONS};
use super::*;
use crate::painter::{self, ImageSurface};
use crate::pick::PickMode;
use crate::read_text::ReadKind;

type Image = ImageBuffer<Rgba<u8>, Vec<u8>>;

impl Overlay {
    pub fn window_id(&self) -> WindowId { self.window.id() }

    /// The capture's logical to device mapping, handed over right after the
    /// window opens (the constructors' signatures stay as they are). A map
    /// whose device size isn't the bitmap's is ignored: the 1x identity
    /// already in place always agrees with the frame, and a wrong map would
    /// move the crop instead of correcting it.
    pub fn set_display_map(&mut self, map: DisplayMap) {
        if map.physical_size() == self.shot.dimensions() { self.display = map; }
    }

    /// Device pixels per logical unit of the captured frame.
    pub(super) fn display_scale(&self) -> f32 { self.display.scale() }

    /// The windows that were on screen when it froze (frontmost first,
    /// virtual-screen pixels), for the hover highlight.
    pub fn set_windows(&mut self, windows: Vec<framefang_platform::windows_list::OnScreenWindow>) {
        self.hover.handles = windows.iter().map(|w| w.hwnd).collect();
        self.hover.windows = windows.into_iter().map(|w| (w.rect, w.title)).collect();
        self.update_target();
    }

    /// The color a color-pick session answered: the eyedropper's tool takes
    /// it as if it had come from the picture.
    pub fn take_screen_color(&mut self, c: framefang_core::color::Rgba) {
        self.spectrum_at = None;
        self.set_color(c.with_alpha(0xFF));
        self.show_palette_of_color();
        self.save_sliders();
        self.window.focus_window();
    }

    /// The color-pick session was cancelled: the eyedropper goes down.
    pub fn screen_color_cancelled(&mut self) {
        self.stop_picking();
        self.window.focus_window();
    }

    /// The rectangle the picker last took, once (virtual-screen pixels):
    /// the tray keeps it for "Capture last region".
    pub fn take_picked_rect(&mut self) -> Option<(i32, i32, i32, i32)> { self.last_pick.take() }

    /// The selection of a capture being finished in place on the frozen
    /// desktop, in virtual-screen pixels, edits included: what "Capture
    /// last region" takes again. `None` for any other session.
    pub fn picker_rect(&self) -> Option<(i32, i32, i32, i32)> {
        let sel = self.selection.filter(|_| self.is_picker())?;
        Some(vdesk::frame_rect_to_virtual(self.frame_origin, sel))
    }

    /// Open this picker in SCROLL mode, for its own hotkey or tray row.
    pub fn open_scroll(&mut self) {
        if self.is_picker() { self.set_pick_mode(PickMode::Scroll); }
    }

    /// Open this picker in TEXT or QR mode, for its hotkey or tray row. Like
    /// any reading mode it isn't kept for the next capture.
    pub fn open_reading(&mut self, kind: ReadKind) {
        if !self.is_picker() { return; }
        self.set_pick_mode(match kind {
            ReadKind::Text => PickMode::Text,
            ReadKind::Qr => PickMode::Qr,
        });
    }

    /// Select `pic` (virtual-screen pixels) as if it had just been dragged,
    /// for "Capture last region" when the picture is edited in place.
    pub fn preselect(&mut self, pic: (i32, i32, i32, i32)) {
        let (x, y) = vdesk::virtual_to_frame(self.frame_origin, (pic.0, pic.1));
        self.selection = Some((x, y, pic.2, pic.3));
        self.state = State::Ready;
        self.tip = None;
        self.repaint();
    }

    /// TEXT or QR handed its picture over and waits for the answer.
    pub fn is_reading(&self) -> bool { self.reading.is_some() }

    /// Note what changed in `settings` since the last time, for the tray to
    /// save (`take_patch`). The session never writes settings.json itself:
    /// its copy dates from when it opened, and saving all of it undid
    /// changes made elsewhere meanwhile.
    pub(super) fn note_settings(&mut self) {
        let changed = store::diff(&self.settings_base, &self.settings);
        if changed.is_empty() { return; }
        store::merge(&mut self.unsent, changed);
        self.settings_base = self.settings.clone();
    }

    /// What this session changed at its save points (a slider let go, a
    /// tool or color picked, a mode chosen) since the tray last asked. A
    /// slider still being dragged isn't in it: one save when it is let go,
    /// not one per move.
    pub fn take_patch(&mut self) -> Option<framefang_core::settings_store::Patch> {
        (!self.unsent.is_empty()).then(|| std::mem::take(&mut self.unsent))
    }

    /// The settings as saved now become this session's copy, with its own
    /// changes not yet handed over laid on top. The values it keeps outside
    /// `settings` (sizes, the stroke) are read again, so its next save
    /// can't write old ones back.
    pub fn adopt_settings(&mut self, saved: &AppSettings) {
        let unsent = std::mem::take(&mut self.unsent);
        self.settings = store::apply(saved, &unsent).0;
        self.settings_base = saved.clone();
        self.unsent = unsent;
        self.text_px = Slider::TextSize.usable(self.settings.last_text_size);
        self.block_px = Slider::BlockSize.usable(self.settings.pixelate_block as f32) as u32;
        self.stroke = tool_stroke(&self.settings, self.tool);
        self.step_look = step_look(&self.settings);
        if self.tool == Tool::Step { self.stroke.color = self.step_look.part(self.step_part); }
    }

    /// Off screen while a Save As dialog is up, and back with the picture
    /// and its ink when the dialog is cancelled.
    pub fn set_hidden(&mut self, hidden: bool) {
        self.window.set_visible(!hidden);
        if hidden { return; }
        // Keys let go while the dialog had the keyboard never reached this
        // window; Shift or Ctrl would otherwise still read as held.
        self.mods = ModifiersState::empty();
        self.tip = None;
        self.window.focus_window();
        self.repaint();
    }

    /// Where a notice `size` big goes once this window closes, in
    /// virtual-screen pixels: centered under the selection, or just inside
    /// its foot when the screen ends there, on the selection's monitor.
    pub fn notice_spot(&self, size: (u32, u32)) -> Option<(i32, i32)> {
        let sel = self.panel_sel()?;
        let spot = spot_under(sel, self.panel_bounds(sel), size);
        Some(vdesk::frame_to_virtual(self.window_origin(), spot))
    }

    /// The virtual-screen point under framebuffer (0, 0) now. The editor
    /// window may have been dragged since it opened, so it asks the window.
    fn window_origin(&self) -> (i32, i32) {
        if !self.is_editor() { return self.frame_origin; }
        self.window.inner_position().map_or(self.frame_origin, |p| (p.x, p.y))
    }

    // Pointer, ink and painter are in frame (framebuffer) pixels; the
    // capture is in bitmap pixels; a pin wants virtual-screen pixels.
    // `virtual_desktop` does the arithmetic, these feed it.

    /// Read where the window really is before a frame. Nothing changes when
    /// the window manager honoured the placement; when it didn't (macOS
    /// with separate Spaces, a WM refusing negative positions) this keeps
    /// the crop, the pin and the magnifier on the pixels being looked at.
    /// A bitmap with no monitors (a video frame, the editor canvas) is
    /// drawn at framebuffer (0, 0) anyway and is skipped.
    pub(super) fn sync_frame_origin(&mut self) {
        if self.desktop.monitors().is_empty() { return; }
        // Wayland tells no position; the origin asked for stands there.
        let Ok(at) = self.window.inner_position().or_else(|_| self.window.outer_position()) else { return };
        if (at.x, at.y) != self.frame_origin {
            self.frame_origin = (at.x, at.y);
            // The dim samples the capture through this offset.
            self.backdrop.invalidate();
        }
        if self.frame_origin != self.desktop.origin() && !self.origin_noted {
            self.origin_noted = true;
            let (dx, dy) = self.desktop.origin();
            let (fx, fy) = self.frame_origin;
            eprintln!("framefang: the window manager put the overlay at {fx},{fy}, not the desktop's {dx},{dy}; selections are mapped through the difference");
        }
    }

    /// Where framebuffer (0, 0) is in the captured bitmap.
    pub(super) fn bitmap_offset(&self) -> (i32, i32) { vdesk::bitmap_offset(self.desktop.origin(), self.frame_origin) }

    /// The selection as a rectangle of the captured bitmap.
    pub(super) fn selection_in_bitmap(&self) -> Option<Rect4> {
        Some(vdesk::frame_rect_to_bitmap(self.desktop.origin(), self.frame_origin, self.selection?))
    }

    /// The frame rectangle the chrome near `point` lays itself out in: that
    /// point's monitor, clipped to the framebuffer, so panels, the
    /// magnifier and tips stay on the screen being worked on instead of
    /// flipping onto the next one. In the scrolling editor, the slice of
    /// the page on screen.
    pub(super) fn chrome_bounds(&self, point: (i32, i32)) -> Rect4 {
        let size = self.window.inner_size();
        if let Some(page) = &self.page { return (0, page.view.y, size.width as i32, page.view.view_h); }
        vdesk::monitor_bounds_in_frame(self.desktop.monitors(), self.frame_origin, (size.width, size.height), point)
    }

    /// The chrome bounds for what hangs off the selection: the monitor
    /// holding its center.
    pub(super) fn panel_bounds(&self, sel: Rect4) -> Rect4 { self.chrome_bounds((sel.0 + sel.2 / 2, sel.1 + sel.3 / 2)) }

    /// This session's action row (each list says why it lacks what it lacks).
    pub(super) fn actions(&self) -> &'static [Action] {
        match self.mode {
            Mode::Record => &RECORD_ACTIONS,
            m if m.locked() => &LOCKED_ACTIONS,
            _ => &CAPTURE_ACTIONS,
        }
    }

    /// The picture's rectangle: in the editor window the whole picture (a
    /// crop dims part of it but keeps it), elsewhere the selection.
    pub(super) fn picture_bounds(&self) -> Option<Rect4> {
        if self.is_editor() { self.picture.or(self.selection) } else { self.selection }
    }

    /// What the panels stand beside: the picture bounds, so a crop never
    /// moves them onto the picture. In the scrolling editor only the part
    /// of the picture on screen, less the action row's room at its foot, so
    /// the panels stay in view as the page scrolls.
    pub(super) fn panel_sel(&self) -> Option<Rect4> {
        let pic = self.picture_bounds()?;
        let Some(page) = &self.page else { return Some(pic) };
        let v = page.view;
        let top = (v.y + EDITOR_EDGE).max(pic.1);
        let bottom = (v.y + v.view_h - action_room()).min(pic.1 + pic.3);
        Some((pic.0, top, pic.2, (bottom - top).max(1)))
    }

    /// Hand the region just let go to an editor window, or `None` to keep
    /// editing it here (it would not fit with its tools on the monitor, or
    /// the capture is not 1:1 with the screen).
    fn hand_off(&self) -> Option<OverlayOutcome> {
        let pic = vdesk::frame_rect_to_virtual(self.frame_origin, self.selection?);
        let picture = crop(&self.shot, self.selection_in_bitmap()?);
        Some(OverlayOutcome::Chosen(Box::new(chosen_for(picture, pic, self.desktop.monitors())?)))
    }

    /// A region was just picked (a drag let go, a click, Ctrl+A). SCROLL
    /// follows and stitches it, TEXT and QR read it; otherwise the picker
    /// copies it and closes (copy only, or the reverse when `switch` says
    /// the switch key is held), hands it to an editor window, or keeps
    /// editing it in place when no editor window would fit.
    pub(super) fn finish_selection(&mut self, switch: bool) -> OverlayOutcome {
        let picker = self.is_picker();
        if picker && self.hover.mode == PickMode::Scroll {
            if let Some(sel) = self.selection {
                // SCROLL's own key, held as the part is clicked, flips what
                // follows.
                let flip = self.key_down(self.settings.scroll_switch_key());
                return OverlayOutcome::Scroll(vdesk::frame_rect_to_virtual(self.frame_origin, sel), flip);
            }
        }
        if let (Some(kind), true) = (self.hover.mode.reads(), picker) {
            if let Some(r) = self.selection_in_bitmap() {
                let flip = self.qr_key_down();
                self.reading = Some(Reading { since: Instant::now(), flip });
                self.repaint();
                return OverlayOutcome::ReadText(crop(&self.shot, r), kind, flip);
            }
        }
        if picker {
            if let Some(sel) = self.selection { self.last_pick = Some(vdesk::frame_rect_to_virtual(self.frame_origin, sel)); }
            if !self.settings.opens_editor(switch) {
                if let Some(r) = self.selection_in_bitmap() { return OverlayOutcome::Copied(crop(&self.shot, r)); }
            }
            if let Some(handed) = self.hand_off() { return handed; }
        }
        self.repaint();
        OverlayOutcome::Continue
    }

    /// `base` (frame pixels) cut from the capture with the ink on screen
    /// painted over it, in its own pixels. Pixelate samples a copy taken
    /// before any ink, so the order things are drawn in never changes what
    /// it shows.
    pub(super) fn inked(&self, base: Rect4) -> Image {
        let mut img = crop(&self.shot, vdesk::frame_rect_to_bitmap(self.desktop.origin(), self.frame_origin, base));
        let pristine = img.clone();
        let (dx, dy) = (-base.0 as f32, -base.1 as f32);
        let moved: Vec<_> = self.ink.iter().filter(|a| self.shows(a)).map(|a| framefang_core::edit::translated(a, dx, dy)).collect();
        let bounds = (0, 0, img.width() as i32, img.height() as i32);
        painter::render_all(&mut ImageSurface(&mut img), moved.iter(),
                            &painter::Pass { source: Some(&pristine), src_offset: (0, 0), bounds });
        img
    }

    /// The finished picture: the selection with its ink, the same pixels
    /// for every way of finishing. The ink is painted on the whole picture
    /// (past a crop, in the editor window) and cut to the selection last,
    /// so a lens or a pixelation near the crop's edge reads what it showed
    /// on screen. In a video only the ink showing at the playhead goes in.
    fn compose_final(&self) -> Option<Image> {
        if self.state != State::Ready { return None; }
        let sel = self.selection?;
        let base = self.picture_bounds().unwrap_or(sel);
        let img = self.inked(base);
        Some(if base == sel { img } else { crop(&img, (sel.0 - base.0, sel.1 - base.1, sel.2, sel.3)) })
    }

    /// Enter, Save, Ctrl+S, a right click. Record mode records instead. A
    /// video hands back its ink and frame; the burn thread builds the
    /// overlays (`compose_overlay_groups`), since each timed run may need
    /// an ffmpeg seek that would freeze the event loop here.
    pub(super) fn commit(&mut self) -> OverlayOutcome {
        if self.mode == Mode::Record { return self.commit_as_record(); }
        if let Some(clip) = &self.clip {
            if self.state != State::Ready { return OverlayOutcome::Continue; }
            return OverlayOutcome::AcceptedVideo(VideoCommit {
                annotations: self.ink.clone(),
                frame: self.shot.clone(),
                frame_t: clip.frame_t,
                duration: clip.duration,
            });
        }
        self.compose_final().map_or(OverlayOutcome::Continue, OverlayOutcome::Accepted)
    }

    pub(super) fn commit_as_copy(&mut self) -> OverlayOutcome {
        let Some(img) = self.compose_final() else { return OverlayOutcome::Continue };
        if self.settings.close_after_copy { return OverlayOutcome::Copied(img); }
        // Nothing closes, so the editor says it worked.
        self.copied_at = Some(Instant::now());
        self.repaint();
        OverlayOutcome::CopiedOpen(img)
    }

    /// Copy the words in the picture as it looks now (a blurred name stays
    /// unread). The editor stays; the tray says what was copied.
    pub(super) fn commit_as_text(&mut self) -> OverlayOutcome {
        self.compose_final().map_or(OverlayOutcome::Continue, |img| OverlayOutcome::ReadText(img, ReadKind::Text, false))
    }

    /// Save As. A video's Save burns the clip, which has no format to pick,
    /// and record mode saves nothing: there it is the plain finish.
    pub(super) fn commit_as_save_as(&mut self) -> OverlayOutcome {
        if matches!(self.mode, Mode::Video | Mode::Record) { return self.commit(); }
        self.compose_final().map_or(OverlayOutcome::Continue, OverlayOutcome::SaveAs)
    }

    /// The selection itself as the region to record, lifted to
    /// virtual-screen pixels like a pin's spot. No picture: the rectangle
    /// is all this session makes; the tray clamps and rounds it.
    pub(super) fn commit_as_record(&mut self) -> OverlayOutcome {
        match self.selection.filter(|_| self.state == State::Ready) {
            Some(r) => OverlayOutcome::RecordRegion(vdesk::frame_rect_to_virtual(self.frame_origin, r)),
            None => OverlayOutcome::Continue,
        }
    }

    /// The pin opens over the region it was cut from. Its window is placed
    /// in virtual-screen pixels, which the selection only equals on one
    /// unscaled screen at the origin, so the selection is lifted through
    /// the framebuffer's origin (negative on a monitor left of or above the
    /// primary), or through where the editor window is now.
    pub(super) fn commit_as_pin(&mut self) -> OverlayOutcome {
        let Some((x, y, _, _)) = self.selection else { return OverlayOutcome::Continue };
        let at = vdesk::frame_to_virtual(self.window_origin(), (x, y));
        self.compose_final().map_or(OverlayOutcome::Continue, |img| OverlayOutcome::Pinned(img, at))
    }
}
