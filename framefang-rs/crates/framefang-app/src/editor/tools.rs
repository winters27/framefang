//! The tools: taking one up and putting it down, its flyout's controls
//! (sliders, palettes, swatches, the spectrum, the eyedropper, Step's and
//! Highlight's looks), what each tool's first press makes, and the words
//! Highlight reads off the picture.
//!
//! Picking a tool slides its settings out beside its button; picking it
//! again tucks them away. Each tool keeps its own color and width
//! (`settings.tool_styles`), and the session saves them back when a
//! control is let go.

use framefang_core::annotation::{Annotation, AnnotationKind, Palettes, Point2, Rect};
use framefang_core::color::Rgba as Color;
use framefang_core::mark::{in_reading_order, span, word_at, MarkLook};
use framefang_core::settings::ToolStyle;
use framefang_core::step::{StepPart, StepShape};

use super::chrome::pixel_at;
use super::flyout::{
    flyout_layout, has_flyout, shifted, slider_track, spectrum_color, tool_stroke, Slider, FLYOUT_GAP,
};
use super::geometry::{crop, rect_contains, Rect4};
use super::panels::{slot_rect, tool_panel_dims, tool_panel_origin, ToolSlot};
use super::*;

/// How far beside a word a Highlight press still takes it.
const MARK_REACH: f32 = 14.0;

impl Overlay {
    /// `t` from its button or its key: its own color and width, its flyout
    /// out (or tucked away when it already is).
    pub(super) fn pick_tool(&mut self, t: Tool) {
        // A tool this session doesn't show can't be reached by its key either.
        if !self.tool_set().has(t) { return; }
        // In the editor window the tools toggle: the one in hand, picked
        // again, goes down, and the picture moves the window again.
        if self.is_editor() && self.armed && t == self.tool {
            self.put_tool_down();
            return;
        }
        self.armed = true;
        let slide_out = has_flyout(t) && self.flyout != Some(t);
        if t != self.tool {
            self.stroke = tool_stroke(&self.settings, t);
            if t == Tool::Step { self.step_part = self.step_look.shape.accent_part(); }
            self.tool = t;
            self.show_palette_of_color();
            // The next capture opens on this tool (never on Crop).
            if t.can_open_with() {
                self.settings.last_tool = t.name().to_owned();
                self.note_settings();
            }
        }
        self.leave_select_mode();
        self.flyout = slide_out.then_some(t);
        if slide_out { self.flyout_since = Instant::now(); }
        self.stop_picking();
        // The pointer hasn't moved, so the tip from before the click would
        // stay up, on top of the flyout.
        self.refresh_tip();
        self.repaint();
    }

    /// No tool in hand (the editor window): the flyout tucks away and a
    /// drag on the picture moves the window.
    pub(super) fn put_tool_down(&mut self) {
        self.armed = false;
        self.flyout = None;
        self.eyedropper = false;
        self.leave_select_mode();
        self.refresh_tip();
        self.update_cursor();
        self.repaint();
    }

    /// Turn the flyout to the palette holding the tool's color, so its
    /// swatch is the one ringed; left as it is when the shown palette has
    /// the color, or none does.
    pub(super) fn show_palette_of_color(&mut self) {
        let palettes = &Palettes::ALL;
        let has_it = |i: usize| palettes[i].colors.contains(&self.stroke.color);
        if has_it(self.palette.min(palettes.len() - 1)) { return; }
        if let Some(i) = (0..palettes.len()).find(|&i| has_it(i)) { self.palette = i; }
    }

    /// The open flyout: its rectangle, whether it hangs off the column's
    /// right (else left), and its tool button's middle as a distance down
    /// from its top, for the pointer. Centered on the button, kept on the
    /// monitor.
    pub(super) fn flyout_rect(&self) -> Option<(Rect4, bool, i32)> {
        let tool = self.flyout?;
        if self.mode == Mode::Record { return None; }
        let sel = self.panel_sel()?;
        let bounds = self.panel_bounds(sel);
        let slots = self.tool_set().slots();
        let origin = tool_panel_origin(slots, bounds, sel);
        let index = slots.iter().position(|s| *s == ToolSlot::Tool(tool))?;
        let (_, button_y, _, button_h) = slot_rect(slots, origin, index);
        let layout = flyout_layout(tool);
        let (w, h) = (layout.w, layout.h);
        let on_right = origin.0 >= sel.0 + sel.2;
        let x = if on_right { origin.0 + tool_panel_dims(slots).0 + FLYOUT_GAP } else { origin.0 - FLYOUT_GAP - w };
        let mid = button_y + button_h / 2;
        let y = (mid - h / 2).clamp(bounds.1, (bounds.1 + bounds.3 - h).max(bounds.1));
        Some(((x, y, w, h), on_right, mid - y))
    }

    /// The slider row under the pointer, when it is on the open flyout.
    pub(super) fn slider_at_pointer(&self) -> Option<(Slider, Rect4)> {
        let (r, ..) = self.flyout_rect()?;
        if !rect_contains(r, self.pointer) { return None; }
        flyout_layout(self.flyout?).sliders.into_iter()
            .map(|(s, row)| (s, shifted(row, (r.0, r.1))))
            .find(|(_, row)| rect_contains(*row, self.pointer))
    }

    /// A press on the flyout's colors: a swatch is the tool's color now, an
    /// arrow turns the palette, the spectrum starts a color drag, the
    /// eyedropper arms or disarms. `true` when it was one of those.
    pub(super) fn press_flyout_colors(&mut self) -> bool {
        let Some(((fx, fy, ..), ..)) = self.flyout_rect() else { return false };
        let Some(block) = self.flyout.and_then(|t| flyout_layout(t).colors) else { return false };
        let p = self.pointer;
        let hit = |r: Rect4| rect_contains(shifted(r, (fx, fy)), p);
        if hit(block.eyedropper) {
            self.eyedropper = !self.eyedropper;
            // The editor window holds only its picture; to pick from the
            // whole screen it asks for a color pick over it.
            self.wants_screen_pick = self.eyedropper && self.is_editor();
            self.tip = None;
            self.update_cursor();
            self.repaint();
            return true;
        }
        if hit(block.spectrum) {
            self.held = Some(Held::Spectrum);
            self.color_from_spectrum();
            return true;
        }
        let count = Palettes::ALL.len();
        if hit(block.prev) {
            self.palette = (self.palette + count - 1) % count;
            self.settings.palette_index = self.palette as i32;
        } else if hit(block.next) {
            self.palette = (self.palette + 1) % count;
            self.settings.palette_index = self.palette as i32;
        } else if let Some(i) = block.swatches.iter().position(|r| hit(*r)) {
            self.set_color(Palettes::get(self.palette).colors[i]);
        } else {
            return false;
        }
        self.save_sliders();
        self.repaint();
        true
    }

    /// A press on Highlight's looks or Step's controls: a look tile is the
    /// next highlight's look; a shape tile is the next marker's shape (in
    /// the same main color), a part chip points the colors at that part,
    /// the switch turns the shadow on or off. `true` when it was one.
    pub(super) fn press_flyout_looks(&mut self) -> bool {
        let Some(((fx, fy, ..), ..)) = self.flyout_rect() else { return false };
        let Some(layout) = self.flyout.map(flyout_layout) else { return false };
        let p = self.pointer;
        let hit = |r: &Rect4| rect_contains(shifted(*r, (fx, fy)), p);
        if let Some(tiles) = layout.looks {
            let Some(i) = tiles.iter().position(hit) else { return false };
            self.mark_style = framefang_core::mark::MarkStyle::ALL[i];
            self.save_sliders();
            self.repaint();
            return true;
        }
        let Some(panel) = layout.step else { return false };
        if let Some(i) = panel.tiles.iter().position(hit) {
            let shape = StepShape::ALL[i];
            if shape != self.step_look.shape { self.step_look = self.step_look.with_shape(shape); }
            self.step_part = shape.accent_part();
        } else if let Some(i) = panel.parts.iter().position(hit) {
            let part = StepPart::ALL[i];
            // A disc has no ring to paint; the press is taken all the same.
            if part == StepPart::Ring && !self.step_look.shape.has_ring() { return true; }
            self.step_part = part;
        } else if hit(&panel.shadow) {
            self.step_look.shadow = !self.step_look.shadow;
        } else {
            return false;
        }
        self.stroke.color = self.step_look.part(self.step_part);
        self.spectrum_at = None;
        self.show_palette_of_color();
        self.stop_picking();
        self.save_sliders();
        self.repaint();
        true
    }

    /// The tool draws in `c` from now on, the text being typed included.
    /// Any color picked puts the eyedropper down.
    pub(super) fn set_color(&mut self, c: Color) {
        self.stroke.color = c;
        // Step: the part being painted.
        if self.tool == Tool::Step { self.step_look.set_part(self.step_part, c); }
        if let Some(Annotation { kind: AnnotationKind::Text { color, .. }, .. }) = self.draft.as_mut() { *color = c; }
        self.stop_picking();
        self.repaint();
    }

    /// The spectrum's color under the pointer, while it is pressed or
    /// dragged; saved when the button comes up.
    pub(super) fn color_from_spectrum(&mut self) {
        let Some(((fx, fy, ..), ..)) = self.flyout_rect() else { return };
        let Some(block) = self.flyout.and_then(|t| flyout_layout(t).colors) else { return };
        let (sx, sy, sw, sh) = shifted(block.spectrum, (fx, fy));
        let along = |p: i32, start: i32, len: i32| ((p - start) as f32 / (len - 1).max(1) as f32).clamp(0.0, 1.0);
        let (u, v) = (along(self.pointer.0, sx, sw), along(self.pointer.1, sy, sh));
        self.spectrum_at = Some((u, v));
        self.set_color(spectrum_color(u, v));
    }

    /// Put the eyedropper down, nothing picked.
    pub(super) fn stop_picking(&mut self) {
        if !std::mem::take(&mut self.eyedropper) { return; }
        self.update_cursor();
        self.repaint();
    }

    /// The eyedropper's click: the tool takes the color of the pixel under
    /// it. In the editor window only the picture counts; the canvas around
    /// it is no part of the capture.
    pub(super) fn pick_color_at_pointer(&mut self) {
        if self.is_editor() && !self.pointer_in_selection() { return; }
        let Some(c) = self.color_at_pointer() else { return };
        self.spectrum_at = None;
        self.set_color(c);
        self.show_palette_of_color();
        self.save_sliders();
    }

    /// The captured pixel under the pointer, opaque.
    pub(super) fn color_at_pointer(&self) -> Option<Color> { pixel_at(&self.shot, self.pointer, self.bitmap_offset()) }

    pub(super) fn slider_value(&self, s: Slider) -> f32 {
        match s {
            Slider::Width => self.stroke.thickness,
            Slider::Opacity => f32::from(self.settings.marker_opacity),
            Slider::TextSize => self.text_px,
            Slider::BlockSize => self.block_px as f32,
            Slider::Zoom => self.settings.magnify_zoom as f32,
        }
    }

    /// Set the held slider from the pointer's place along its track.
    pub(super) fn slider_to_pointer(&mut self, s: Slider) {
        let Some(((fx, fy, ..), ..)) = self.flyout_rect() else { return };
        let Some((_, row)) = self.flyout.and_then(|t| flyout_layout(t).sliders.into_iter().find(|(x, _)| *x == s)) else { return };
        let (tx, _, tw, _) = slider_track(shifted(row, (fx, fy)));
        if tw < 2 { return; }
        let share = (self.pointer.0 - tx) as f32 / (tw - 1) as f32;
        let share = if share.is_finite() { share.clamp(0.0, 1.0) } else { 0.0 };
        let (lo, hi) = s.range();
        let value = (lo + share * (hi - lo)).round();
        if value == self.slider_value(s) { return; }
        match s {
            Slider::Width => self.stroke.thickness = value,
            Slider::Opacity => self.settings.marker_opacity = value as u8,
            Slider::TextSize => {
                self.text_px = value;
                // The text being typed takes the size, so the slider
                // previews on it.
                if let Some(Annotation { kind: AnnotationKind::Text { font_size, .. }, .. }) = self.draft.as_mut() { *font_size = value; }
            }
            Slider::BlockSize => self.block_px = value as u32,
            // Saved with the rest when the drag ends.
            Slider::Zoom => self.settings.magnify_zoom = value as u32,
        }
        self.repaint();
    }

    /// The tool's style and every slider into the settings, to be saved, so
    /// the next capture opens with them. When a control is let go or a
    /// color is picked.
    pub(super) fn save_sliders(&mut self) {
        // Step's own color is its body's; the rest of its look goes whole.
        let color = if self.tool == Tool::Step {
            self.settings.step_look = Some(self.step_look);
            self.step_look.body.with_alpha(0xFF)
        } else {
            self.stroke.color
        };
        if self.tool == Tool::Highlight { self.settings.highlight_style = self.mark_style; }
        self.settings.set_tool_style(self.tool, ToolStyle { color_argb: color.to_argb(), thickness: self.stroke.thickness });
        self.settings.last_text_size = self.text_px;
        self.settings.pixelate_block = self.block_px;
        self.note_settings();
    }

    /// What a press with the tool in hand starts drawing. Step, Text,
    /// Highlight and Crop draw nothing that way: Step places on the click,
    /// Text types, Highlight snaps to words and Crop changes the picture.
    pub(super) fn start_annotation(&self) -> Option<Annotation> {
        let at = Point2::new(self.pointer.0 as f32, self.pointer.1 as f32);
        let s = self.stroke;
        Some(match self.tool {
            Tool::Pen => Annotation::pen(s, at),
            Tool::Line => Annotation::line(s, at),
            Tool::Arrow => Annotation::arrow(s, at),
            Tool::Rectangle => Annotation::rectangle(s, at),
            Tool::Ellipse => Annotation::ellipse(s, at),
            Tool::Marker => Annotation::marker(s, at, self.settings.marker_opacity),
            Tool::Spotlight => Annotation::spotlight(at),
            Tool::Magnify => Annotation::magnify(s, at, self.settings.magnify_zoom),
            Tool::Pixelate => {
                let mut blur = Annotation::pixelate(at);
                if let AnnotationKind::Pixelate { block_size, .. } = &mut blur.kind { *block_size = self.block_px; }
                blur
            }
            Tool::Step | Tool::Text | Tool::Highlight | Tool::Crop => return None,
        })
    }

    /// Highlight's words: take a reading that finished, and start one on
    /// the selection's picture when this rectangle isn't read (or being
    /// read) yet. The reading runs on its own thread.
    pub(super) fn read_words(&mut self) {
        let reply = self.words.job.as_ref().map(|(rx, at)| (rx.try_recv(), *at));
        if let Some((reply, (ox, oy))) = reply {
            use std::sync::mpsc::TryRecvError;
            let (ox, oy) = (ox as f32, oy as f32);
            match reply {
                Ok(Ok(boxes)) => {
                    let words = boxes.into_iter().map(|b| framefang_core::mark::Word {
                        rect: Rect { x: ox + b.rect.0, y: oy + b.rect.1, w: b.rect.2, h: b.rect.3 },
                        line: b.line,
                    }).collect();
                    self.words.found = Some(in_reading_order(words));
                    self.words.job = None;
                    self.repaint();
                }
                Ok(Err(why)) => {
                    self.words.error = Some(why);
                    self.words.found = Some(Vec::new());
                    self.words.job = None;
                    self.repaint();
                }
                Err(TryRecvError::Disconnected) => {
                    self.words.found = Some(Vec::new());
                    self.words.job = None;
                }
                Err(TryRecvError::Empty) => {}
            }
        }
        if self.state == State::Dragging || matches!(self.mode, Mode::Video | Mode::Record) { return; }
        let (Some(r), Some(sel)) = (self.selection_in_bitmap(), self.selection) else { return };
        if r.2 < 4 || r.3 < 4 || self.words.of == Some(r) { return; }
        self.words = Words { of: Some(r), ..Words::default() };
        let picture = crop(&self.shot, r);
        let (tx, rx) = std::sync::mpsc::channel();
        let reader = move || { let _ = tx.send(framefang_platform::ocr::read_words(&picture).map_err(|e| e.to_string())); };
        let _ = std::thread::Builder::new().name("read words".into()).spawn(reader);
        self.words.job = Some((rx, (sel.0, sel.1)));
        self.repaint();
    }

    /// Highlight: a press on a word, or just beside one, starts a highlight
    /// there.
    pub(super) fn press_highlight(&mut self) {
        let at = Point2::new(self.pointer.0 as f32, self.pointer.1 as f32);
        let Some(words) = self.words.found.as_ref() else { return };
        let Some(i) = word_at(words, at, MARK_REACH) else { return };
        // Each highlight wobbles its own way.
        let seed = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
            .map_or(1, |d| d.subsec_nanos() ^ d.as_secs() as u32);
        let look = MarkLook { style: self.mark_style, color: self.stroke.color, seed };
        self.draft = Some(Annotation { kind: AnnotationKind::TextMark { rects: span(words, i, i), look }, time: None });
        self.mark_from = Some(i);
        self.state = State::Inking;
        self.repaint();
    }

    /// Highlight while dragging: every word from where it began to the one
    /// nearest the pointer, in reading order.
    pub(super) fn drag_highlight(&mut self) {
        let (Some(words), Some(from)) = (self.words.found.as_ref(), self.mark_from) else { return };
        let at = Point2::new(self.pointer.0 as f32, self.pointer.1 as f32);
        let Some(to) = word_at(words, at, f32::MAX) else { return };
        let Some(Annotation { kind: AnnotationKind::TextMark { rects, .. }, .. }) = self.draft.as_mut() else { return };
        *rects = span(words, from, to);
        self.repaint();
    }

    /// Highlight in hand and not dragging: the word a press would take, to
    /// outline, or what to say instead (reading, nothing found, can't read).
    pub(super) fn highlight_hint(&self) -> (Option<Rect>, Option<String>) {
        let aiming = self.tool == Tool::Highlight && self.armed && self.state != State::Inking && self.pointer_in_selection();
        if !aiming { return (None, None); }
        if self.words.job.is_some() { return (None, Some("Reading the text...".into())); }
        let Some(words) = &self.words.found else { return (None, None) };
        if let Some(why) = &self.words.error {
            // The first sentence of the reason, short enough for one line.
            let first = why.split(". ").next().unwrap_or(why).trim_end_matches('.');
            return (None, Some(format!("Can't read text: {}", first.chars().take(70).collect::<String>())));
        }
        if words.is_empty() { return (None, Some("No text found here".into())); }
        let at = Point2::new(self.pointer.0 as f32, self.pointer.1 as f32);
        (word_at(words, at, MARK_REACH).and_then(|i| span(words, i, i).into_iter().next()), None)
    }
}
