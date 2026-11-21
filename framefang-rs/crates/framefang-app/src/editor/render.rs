//! Drawing a frame, back to front: the capture dimmed outside the selection
//! (or the highlight), the ink, the typing frame and caret, Highlight's
//! aim, the select-mode pick, the selection's frame and handles, the hover
//! highlight, the size chip, the panels and the flyout, the dock, the
//! magnifier or loupe, the timeline, the tip, and last the notice bar, so
//! nothing ever covers it. The scrolling editor draws its whole page and
//! shows a slice; the editor window is then cut to shape.

use std::num::NonZeroU32;
use std::time::Duration;

use framefang_core::annotation::AnnotationKind;
use framefang_core::mark::MarkLook;

use super::chrome::*;
use super::dock::{draw_dock, draw_reading_frame, draw_target, DockAnim, ReadLook};
use super::flyout::{ease_out, flyout_layout, FLYOUT_OPEN};
use super::geometry::rect_contains;
use super::paint::{word, Canvas};
use super::panels::PanelHot;
use super::video::{draw_timeline_bar, visible_at, DURATIONS};
use super::*;
use crate::painter::{self, U32Surface};
use crate::pick::{self, PickMode};

/// The pace of the animations nothing in the event stream drives (the
/// column's glow, the row's pulse): about the tray's 30 Hz. Asking for the
/// next frame without waiting makes winit hand it straight back and spins
/// a core.
const ANIM_FRAME: Duration = Duration::from_millis(33);
/// How long "Copied to clipboard" stays after a Copy that keeps the editor.
const COPIED_NOTICE: Duration = Duration::from_millis(1400);

/// The dashed frame around text being typed.
const TYPING_FRAME: u32 = 0x00_88_88_8C;
const CARET: u32 = 0x00_E8_E8_EC;
/// The Esc confirmation's amber, and its quieter words.
const ASK_AMBER: u32 = 0x00_FF_B0_20;
const ASK_AMBER_TEXT: u32 = 0x00_FF_D8_8A;

impl Overlay {
    /// The open flyout as it draws this frame.
    fn flyout_frame(&self) -> Option<FlyoutFrame> {
        let (rect, on_right, pointer_y) = self.flyout_rect()?;
        let tool = self.flyout?;
        let layout = flyout_layout(tool);
        let values = layout.sliders.iter().map(|(s, _)| self.slider_value(*s)).collect();
        Some(FlyoutFrame {
            rect,
            on_right,
            pointer_y,
            layout,
            values,
            color: self.stroke.color,
            palette: self.palette,
            open: ease_out(self.flyout_since.elapsed().as_secs_f32() / FLYOUT_OPEN.as_secs_f32()),
            picking: self.eyedropper,
            spectrum_at: self.spectrum_at,
            step: (tool == Tool::Step).then_some((self.step_look, self.step_part)),
            mark: (tool == Tool::Highlight).then_some(MarkLook { style: self.mark_style, color: self.stroke.color, seed: 11 }),
        })
    }

    /// The dock's animations for this frame, noted so `tick` knows it is
    /// drawn. Before the window is on screen (the first frame is drawn
    /// cloaked) the sweep waits at its start.
    fn dock_anim(&mut self) -> DockAnim {
        if self.shown && self.hover.dock_since.is_none() { self.hover.dock_since = Some(Instant::now()); }
        let since = self.hover.dock_since.map_or(Duration::ZERO, |t| t.elapsed());
        self.hover.dock_drawn = self.dock_frame();
        DockAnim {
            sweep: (since < pick::SWEEP).then(|| since.as_secs_f32() / pick::SWEEP.as_secs_f32()),
            decode: self.hover.decode_at.map(|t| t.elapsed()).filter(|d| *d < pick::DECODE),
            pressed: self.hover.dock_drawn.pressed,
            cursor_on: self.hover.dock_drawn.cursor_on,
        }
    }

    /// Ask for the next frame no sooner than `frame` after the last one
    /// asked for this way. Waiting out the rest of the interval is what
    /// keeps it from becoming a hot loop.
    fn pace(&mut self, frame: Duration) {
        let since = self.anim_at.map_or(frame, |t| t.elapsed());
        if since < frame { std::thread::sleep(frame - since); }
        self.anim_at = Some(Instant::now());
        self.repaint();
    }

    pub(super) fn redraw(&mut self) {
        // Where the window really is decides every mapping below.
        self.sync_frame_origin();
        self.refresh_map();
        #[cfg(target_os = "linux")]
        {
            self.push_x11_focus();
            // Frames keep coming until the focus is taken; after that only
            // events bring them.
            if !self.x11_focus.0 { self.repaint(); }
        }
        let size = self.window.inner_size();
        let (Some(w), Some(h)) = (NonZeroU32::new(size.width), NonZeroU32::new(size.height)) else { return };

        // Everything that reads `self` through a method is settled before the
        // framebuffer borrows the surface.
        let actions = self.actions();
        let slots = self.tool_set().slots();
        let editor = self.is_editor();
        let locked = self.mode.locked();
        let near_pointer = self.chrome_bounds(self.pointer);
        let around_selection = self.selection.map(|s| self.panel_bounds(s));
        let flyout = self.flyout_frame();
        let (aimed_word, highlight_note) = self.highlight_hint();
        let pointer = self.pointer;
        let in_selection = self.pointer_in_selection();
        // Crop in hand: the crop's handles show, since they drag now.
        let crop_in_hand = editor && self.armed && self.tool == Tool::Crop && !self.select.on;
        // Where the eyedropper would take its color.
        let picking_here = self.eyedropper
            && !flyout.as_ref().is_some_and(|f| rect_contains(f.rect, pointer))
            && (!editor || in_selection);
        let hover = self.hover_showing()
            .then(|| self.hover.target.as_ref().map(|(r, name)| (self.shown_target(), name.clone(), (r.2, r.3))))
            .flatten();
        let gliding = hover.is_some() && self.hover.glide_at.elapsed() < pick::GLIDE;
        let pick_mode = self.hover.mode.available(self.screens());
        let dock = (self.state == State::Waiting && self.mode.hovers()).then(|| self.mode_bar());
        let dock_anim = dock.as_ref().map(|_| self.dock_anim());
        self.read_drawn = self.read_frame();
        let read_look = self.reading_kind().map(|kind| ReadLook {
            kind,
            // `read_frame` is the blink step (0 is lit) doubled, plus a flag.
            cursor_on: self.read_drawn.is_some_and(|f| f / 2 == 0),
            reading: self.reading.map(|r| r.since.elapsed()),
            links: (self.links_open(), self.settings.open_qr_links),
        });
        let offset = self.bitmap_offset();
        let picture_bounds = self.picture_bounds();
        let panel_sel = self.panel_sel();
        let playhead = self.clip.as_ref().map(|c| c.at);
        // Copies, taken before the window's pixels are borrowed below.
        let (hot, cooling, tip_up) = (self.hot, self.cooling, self.tip_shown());

        if let Err(e) = self.surface.resize(w, h) {
            eprintln!("framefang: the overlay's framebuffer could not be resized: {e}");
            return;
        }
        let mut out = match self.surface.buffer_mut() {
            Ok(buffer) => buffer,
            Err(e) => { eprintln!("framefang: the overlay's framebuffer is unavailable: {e}"); return; }
        };
        let (win_w, shown_h) = (w.get() as usize, h.get() as usize);
        // The scrolling editor paints its whole page, in page pixels like any
        // picture, and copies the slice on screen at the end.
        let view = self.page.as_ref().map(|p| p.view);
        let mut doc = self.page.as_mut().map(|p| std::mem::take(&mut p.doc));
        let win_h = view.map_or(shown_h, |v| v.doc_h as usize);
        let px: &mut [u32] = match doc.as_mut() {
            Some(page) => { page.resize(win_w * win_h, 0); page }
            None => &mut out[..],
        };
        let whole = (0, 0, win_w as i32, win_h as i32);
        let mut c = Canvas::new(px, win_w, win_h);
        // While the highlight shows, a press that hasn't moved yet is no
        // selection to draw.
        let sel_corners = self.selection.filter(|_| hover.is_none()).map(|(x, y, w, h)| (x, y, x + w, y + h));

        // The capture, dimmed outside the selection, or before one exists
        // outside the highlight; a color pick dims nothing. Pixels off the
        // capture (a gap between monitors of different sizes, a window the
        // WM pushed past it) are black.
        let started = Instant::now();
        let shot = crate::backdrop::Shot { rgba: self.shot.as_raw(), width: self.shot.width() as usize, height: self.shot.height() as usize, offset };
        let lit = if self.mode == Mode::ColorPick {
            Some((0, 0, win_w as i32, win_h as i32))
        } else {
            hover.as_ref().map(|((x, y, w, h), ..)| (*x, *y, x + w, y + h)).or(sel_corners)
        };
        c.px.copy_from_slice(self.backdrop.frame(win_w, win_h, &shot, lit));
        let backdrop_time = started.elapsed();

        // The ink, clipped to the picture; in a video only what shows at the
        // playhead.
        let ink = self.ink.iter().filter(move |a| playhead.map_or(true, |t| visible_at(a, t))).chain(self.draft.iter());
        let pass = painter::Pass { source: Some(&self.shot), src_offset: offset, bounds: picture_bounds.unwrap_or(whole) };
        painter::render_all(&mut c.surface(), ink, &pass);

        // Typing: a dashed frame around the text and a caret. Neither is
        // ever in a finished picture.
        if self.state == State::Typing {
            if let Some(AnnotationKind::Text { color, position, text, font_size }) = self.draft.as_ref().map(|a| &a.kind) {
                draw_typing(&mut c, (position.x as i32, position.y as i32), text, &self.preedit, *font_size, *color);
            }
        }

        // Highlight in hand: the word a press takes, outlined in its color,
        // or a word on what the reading found.
        if let Some(r) = aimed_word {
            let (x0, y0) = (r.x.floor() as i32 - 3, r.y.floor() as i32 - 2);
            c.dashed(x0, y0, (r.x + r.w).ceil() as i32 + 3, (r.y + r.h).ceil() as i32 + 2, word(self.stroke.color));
        }
        if let Some(note) = &highlight_note { draw_tooltip(&mut c, near_pointer, note, pointer.0 + 16, pointer.1 + 18); }

        // Select mode's pick: a dashed green box with solid corners, so it
        // reads as held rather than as another region.
        if let Some(a) = self.select.picked.and_then(|i| self.ink.get(i)) {
            const TICK: i32 = 4;
            let b = framefang_core::edit::bounds(a);
            let (x0, y0) = (b.x as i32 - 5, b.y as i32 - 5);
            let (x1, y1) = ((b.x + b.w) as i32 + 5, (b.y + b.h) as i32 + 5);
            c.dashed(x0, y0, x1, y1, NEON_GREEN);
            for (x, y) in [(x0, y0), (x1 - TICK, y0), (x0, y1 - TICK), (x1 - TICK, y1 - TICK)] {
                c.fill(x, y, x + TICK, y + TICK, NEON_GREEN);
            }
        }

        // The selection: the reading look in TEXT and QR, else the blue frame
        // and, where it can be dragged, its eight handles.
        if let Some((x0, y0, x1, y1)) = sel_corners {
            match &read_look {
                Some(look) => draw_reading_frame(&mut c, (x0, y0, x1 - x0, y1 - y0), look, None),
                None => {
                    // Handles only where they can be grabbed: a settled box
                    // that can still be pulled (record region, the in-place
                    // editor, Crop). Mid-drag the button is already down,
                    // so there they would only be clutter.
                    let grabbable = (!locked || crop_in_hand) && self.state != State::Dragging;
                    draw_selection_frame(&mut c, (x0, y0, x1, y1), grabbable);
                }
            }
        }
        if let Some((r, name, size)) = &hover {
            match &read_look {
                Some(look) => draw_reading_frame(&mut c, *r, look, Some(name)),
                None => draw_target(&mut c, *r, name, *size),
            }
        }
        // Where the editor window can't be cut to shape, an edge so it reads
        // as a window over the live desktop.
        if editor && !crate::window_shape::supported() {
            c.outline(0, 0, win_w as i32, win_h as i32, EDITOR_FRAME);
        }

        // The size chip, whenever there is a selection, mid-drag too. Not in
        // the editor window, whose size was settled in the picker, except
        // while a crop is dragged (people crop to exact sizes); not in TEXT
        // and QR either, where what is read matters, not how big.
        let wants_size = read_look.is_none() && (!editor || self.crop_drag.is_some());
        if let Some(sel) = self.selection.filter(|r| wants_size && r.2 > 8 && r.3 > 8) {
            draw_dimension_chip(&mut c, around_selection.unwrap_or(whole), sel, sel.2 as u32, sel.3 as u32);
        }

        // The panels, from the moment a region is picked; the pulse starts
        // when they first appear. TEXT and QR read and close, so they show
        // none.
        let ready = self.state == State::Ready;
        if ready && !self.was_ready { self.pulse_from = Some(Instant::now()); }
        self.was_ready = ready;
        let editing = matches!(self.state, State::Ready | State::Inking | State::Resizing | State::Typing | State::Moving);
        if let Some(sel) = panel_sel.filter(|_| editing && read_look.is_none()) {
            let bounds = around_selection.unwrap_or(whole);
            // Record mode is the selection alone: no column, just the row.
            let lit = |b: PanelHot| super::hover::hover_level_of(hot, cooling, b);
            if self.mode != Mode::Record {
                draw_tool_panel(&mut c, slots, bounds, sel, self.armed.then_some(self.tool), |i| lit(PanelHot::Slot(i)));
                draw_tool_panel_glow(&mut c, slots, bounds, sel, self.opened_at.elapsed().as_secs_f32());
            }
            draw_action_panel(&mut c, bounds, sel, actions, |i| lit(PanelHot::Action(i)));
            if let Some(since) = self.pulse_from.filter(|_| ready).map(|t| t.elapsed().as_secs_f32()) {
                if since < PULSE_SECS {
                    // The editor window's pulse has no outer haze: past the
                    // row it would blend over the cut-away canvas.
                    draw_action_panel_pulse(&mut c, bounds, sel, since, actions, !editor);
                } else {
                    self.pulse_from = None;
                }
            }
            if let Some(f) = &flyout { draw_flyout(&mut c, f); }
        }

        // The dock. Before a region exists the overlay is a dimmed desktop
        // that would say nothing about the modes or the way out; the dock
        // does, and goes as soon as a drag starts.
        if let (Some(dock), Some(anim)) = (&dock, &dock_anim) {
            let over = dock.chips.iter().find(|(_, r)| rect_contains(*r, pointer)).map(|(m, _)| *m);
            draw_dock(&mut c, dock, pick_mode, over, anim);
        }

        // The eyedropper's loupe; else the magnifier, while an edge is being
        // placed by the pixel (where a drag can start: Screen and All screens
        // take a click) and off the dock. Once a region is picked the lens
        // would only be in the way.
        let on_dock = dock.as_ref().is_some_and(|d| rect_contains(d.rect, pointer));
        let can_drag = locked || pick_mode == PickMode::Auto;
        if picking_here {
            // Over the panels it opens up and left, away from the column and
            // its flyout.
            let color_pick = self.mode == Mode::ColorPick;
            draw_color_loupe(&mut c, near_pointer, &self.shot, pointer, offset, !color_pick, color_pick);
        } else if matches!(self.state, State::Waiting | State::Dragging) && can_drag && !on_dock {
            draw_magnifier(&mut c, near_pointer, &self.shot, pointer, offset);
        }

        // The timeline, only for a video whose length is known.
        if let Some((clip, total)) = self.clip.as_ref().and_then(|clip| Some((clip, clip.duration?))) {
            let starts: Vec<f32> = self.ink.iter().filter_map(|a| a.time.map(|(start, _)| start)).collect();
            draw_timeline_bar(&mut c, near_pointer, clip.at, total, DURATIONS[clip.choice % DURATIONS.len()].1, &starts);
        }

        // Only once the pointer has rested: the button's own hover already
        // said what a click would hit.
        if let Some((label, x, y)) = self.tip.filter(|_| ready && tip_up) {
            draw_tooltip(&mut c, near_pointer, label, x, y);
        }

        // The notice bar: the Esc confirmation, else select mode's cue, else
        // the word that a copy worked.
        if let Some(what) = self.confirm {
            let headline = match what {
                Discard::Region => {
                    let n = self.ink.len();
                    format!("Throw away {n} annotation{}?", if n == 1 { "" } else { "s" })
                }
                Discard::Session => "Close without saving your marks?".to_owned(),
            };
            let lines = [headline.as_str(), "[Esc] again to discard  //  any other key keeps editing"];
            draw_notice_bar(&mut c, near_pointer, &lines, ASK_AMBER, ASK_AMBER_TEXT);
        } else if self.select.on {
            // With something picked the cue shrinks: the recipe is known by
            // then.
            let how = if self.select.picked.is_some() {
                "drag to move  //  [Del] removes"
            } else {
                "click ink to move it  //  [Del] removes  //  [S] or a tool key exits"
            };
            draw_notice_bar(&mut c, near_pointer, &["Select mode", how], NEON_GREEN, NEON_GREEN_TEXT);
        } else if self.copied_at.is_some_and(|t| t.elapsed() < COPIED_NOTICE) {
            draw_notice_bar(&mut c, near_pointer, &["Copied to clipboard"], NEON_GREEN, NEON_GREEN_TEXT);
        }
        let chrome_time = started.elapsed();

        // The scrolling editor: the slice of the page on screen, then the
        // page map over it, in window pixels.
        let scroll_y = view.map_or(0, |v| v.y);
        if let (Some(v), Some(page_px)) = (view, doc.as_ref()) {
            let from = (v.y as usize * win_w).min(page_px.len());
            let n = (shown_h * win_w).min(page_px.len() - from).min(out.len());
            out[..n].copy_from_slice(&page_px[from..from + n]);
            if let (Some(page), Some((_, pic_y, _, pic_h))) = (self.page.as_ref(), self.picture) {
                let column = crate::page_map::map_rect(win_w as i32, v.view_h, page.map_left);
                crate::page_map::draw(&mut out, win_w, shown_h, column, &page.thumb, v, pic_y, pic_h,
                                      crate::palette::LASER(), page.hovered || page.dragging);
            }
        }
        // The editor window: cut away every pixel still bare canvas. The whole
        // picture is kept, cropped-off part included: its dimmed pixels can
        // match the canvas and would be cut into holes.
        if editor && crate::window_shape::supported() {
            let keep = self.picture
                .map(|(x, y, w, h)| (x, (y - scroll_y).max(0), x + w, (y + h - scroll_y).min(shown_h as i32)))
                .or(sel_corners);
            let rects = crate::window_shape::drawn_rects(&out, win_w, shown_h, keep, EDITOR_CLEAR);
            if rects != self.cut {
                crate::window_shape::apply(&self.window, &rects);
                self.cut = rects;
            }
        }
        if let Err(e) = out.present() { eprintln!("framefang: the overlay's frame could not be shown: {e}"); }
        if let (Some(page), Some(page_px)) = (self.page.as_mut(), doc) { page.doc = page_px; }

        if !self.shown {
            self.shown = true;
            if crate::reveal::SUPPORTED { crate::reveal::uncloak(&self.window); } else { self.window.set_visible(true); }
            self.window.focus_window();
            // On screen from now: the dock's sweep starts here.
            self.hover.dock_since = Some(Instant::now());
        }
        if let Some(times) = self.frame_times.as_mut() {
            let ms = |d: Duration| d.as_secs_f32() * 1000.0;
            times.push((ms(backdrop_time), ms(chrome_time), ms(started.elapsed())));
        }

        // The glow, the pulse and the highlight's glide have no events
        // behind them, so this frame asks for the next. Only at rest in
        // `Ready`: while drawing, resizing or typing, input brings frames
        // already, and pacing those would only add lag. The glide and the
        // flyout's slide run at 60 frames a second, the glow and pulse at 30.
        // A button easing lit or back is paced like the slide, so the ease
        // reads smooth rather than in two or three steps.
        let sliding = (self.flyout.is_some() && self.flyout_since.elapsed() < FLYOUT_OPEN) || self.hover_easing();
        if gliding {
            let since = self.anim_at.map_or(ANIM_FRAME, |t| t.elapsed());
            if since < ANIM_FRAME / 2 { std::thread::sleep(ANIM_FRAME / 2 - since); }
            self.anim_at = Some(Instant::now());
            self.repaint();
        } else if ready && self.selection.is_some() {
            self.pace(if sliding { ANIM_FRAME / 2 } else { ANIM_FRAME });
        }
    }
}

/// Text being typed at `at`: a composition in progress drawn dimmed after
/// what is typed (it isn't stored, so it is measured as if it were), a
/// dashed frame around it all and a 1 px caret at the end of the last
/// line. Measured with the rasterizer that paints the glyphs, so it fits
/// accents, wide scripts and several lines.
fn draw_typing(c: &mut Canvas, at: (i32, i32), text: &str, preedit: &str, px: f32, color: framefang_core::color::Rgba) {
    const PAD: i32 = 4;
    let composed = (!preedit.is_empty()).then(|| [text, preedit].concat());
    let block = framefang_core::text::layout(composed.as_deref().unwrap_or(text), px);
    if composed.is_some() {
        let mut surface = U32Surface { buf: &mut *c.px, stride: c.w as i32, height: c.h as i32 };
        painter::blit_text_block(&mut surface, at.0, at.1, &block, color.with_alpha(0x99));
    }
    let (w, h) = (block.width().ceil() as i32, block.height().ceil() as i32);
    c.dashed(at.0 - PAD, at.1 - PAD, at.0 + w.max(2) + PAD, at.1 + h + PAD, TYPING_FRAME);
    let (caret_x, caret_y) = block.caret();
    let x = at.0 + caret_x.round() as i32 + 1;
    let top = at.1 + caret_y.round() as i32;
    for y in top..top + block.line_height.round() as i32 { c.set(x, y, CARET); }
}
