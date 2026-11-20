//! Input: the event router, the pointer, the keyboard and typing, Esc and
//! the confirmations it arms, and undo.
//!
//! Esc never costs work on its first press. It cancels the cheapest thing
//! first (the eyedropper, a crop being dragged, ink being drawn, a held
//! annotation, the select-mode pick, select mode itself, the tool in the
//! editor window's hand) and only then, with ink on the picture, arms a
//! confirmation; a second Esc goes through with it, and even that clear is
//! one undo step.

use framefang_core::annotation::{Annotation, AnnotationKind, Point2};
use framefang_core::edit;
use framefang_core::history::EditOp;
use framefang_core::settings::SwitchKey;
use framefang_core::state::{hit_test_edge_scaled, Edge};
use framefang_core::text::{pop_grapheme, sanitize_input};
use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, Ime, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey, SmolStr};
use winit::window::CursorIcon;

use super::geometry::{crop_rect, rect_contains, rect_from, resized, CropDrag, CropGrab, Rect4};
use super::panels::{action_at, action_panel_origin, slot_at, tool_panel_origin, Action, ToolSlot};
use super::video::{timeline_bar_rect, timeline_chip_rect, visible_at, DURATIONS};
use super::*;
use crate::pick::PickMode;

type Out = OverlayOutcome;

impl Overlay {
    pub fn handle_event(&mut self, event: WindowEvent) -> OverlayOutcome {
        if let Some(answer) = self.queued.take() { return answer; }
        #[cfg(not(windows))]
        if let WindowEvent::KeyboardInput { event: KeyEvent { physical_key, state, .. }, .. } = &event {
            if let Some(vk) = crate::settings_form::vk_of(*physical_key) {
                match state {
                    ElementState::Pressed => { self.keys_down.insert(vk); }
                    ElementState::Released => { self.keys_down.remove(&vk); }
                }
            }
        }
        // Reading: the screen holds still; only Esc and closing count.
        if self.reading.is_some() {
            return match event {
                WindowEvent::CloseRequested => Out::Cancelled,
                WindowEvent::KeyboardInput { event: KeyEvent { logical_key: Key::Named(NamedKey::Escape), state: ElementState::Pressed, .. }, .. } => Out::Cancelled,
                WindowEvent::RedrawRequested | WindowEvent::Resized(_) => { self.redraw(); Out::Continue }
                _ => Out::Continue,
            };
        }
        // The scrolling editor's wheel and page map come first.
        if self.page.is_some() {
            if let Some(answer) = self.page_map_event(&event) { return answer; }
        }
        match event {
            WindowEvent::CloseRequested => self.request_close(),
            WindowEvent::ModifiersChanged(m) => { self.mods = m.state(); Out::Continue }
            WindowEvent::KeyboardInput { event: KeyEvent { logical_key, text, state: ElementState::Pressed, .. }, .. } => {
                self.key_with_text(logical_key, text)
            }
            // Dead keys, compose sequences and input methods arrive here,
            // not as a key's text. Only while typing: `set_typing` turns the
            // input method on for the Text tool and off after, so tool
            // letters stay one key press the rest of the time.
            WindowEvent::Ime(ime) => self.on_ime(ime),
            WindowEvent::CursorMoved { position, .. } => self.pointer_moved((position.x as i32, position.y as i32)),
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. } => self.left_down(),
            WindowEvent::MouseInput { state: ElementState::Released, button: MouseButton::Left, .. } => self.left_up(),
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Right, .. } => self.right_down(),
            // The pointer left (in the cut-out editor window, any move off the
            // picture and panels): a tip would stay over a button no one
            // points at.
            WindowEvent::CursorLeft { .. } => {
                if self.tip.take().is_some() { self.repaint(); }
                Out::Continue
            }
            WindowEvent::Resized(_) => { self.repaint(); Out::Continue }
            WindowEvent::RedrawRequested => { self.redraw(); Out::Continue }
            _ => Out::Continue,
        }
    }

    /// The pointer moved to window point `at`.
    pub(super) fn pointer_moved(&mut self, at: (i32, i32)) -> OverlayOutcome {
        self.pointer_win = at;
        self.pointer = self.page_point(at);
        // A held control follows the pointer wherever it goes, even far off
        // the control itself.
        match self.held {
            Some(Held::Slider(s)) => { self.slider_to_pointer(s); return Out::Continue; }
            Some(Held::Spectrum) => { self.color_from_spectrum(); return Out::Continue; }
            Some(Held::Playhead) => { self.scrub_to_pointer(false); return Out::Continue; }
            None => {}
        }
        if let (Some(drag), Some(pic)) = (self.crop_drag, self.picture) {
            self.selection = Some(crop_rect(drag, self.pointer, pic));
            self.repaint();
            return Out::Continue;
        }
        let p = self.pointer;
        match self.state {
            State::Dragging => {
                self.selection = Some(rect_from(self.anchor, p));
                self.repaint();
            }
            State::Inking if self.tool == Tool::Highlight => self.drag_highlight(),
            State::Inking => {
                if let Some(a) = self.draft.as_mut() {
                    a.extend(Point2::new(p.0 as f32, p.1 as f32));
                    self.repaint();
                }
            }
            State::Resizing => {
                if let Some(sel) = self.selection { self.selection = Some(resized(sel, self.grabbed_edge, p)); }
                self.repaint();
            }
            State::Moving => {
                if let Some(a) = self.select.picked.and_then(|i| self.ink.get_mut(i)) {
                    let (dx, dy) = ((p.0 - self.select.last.0) as f32, (p.1 - self.select.last.1) as f32);
                    edit::translate(a, dx, dy);
                    self.select.moved.0 += dx;
                    self.select.moved.1 += dy;
                }
                self.select.last = p;
                self.repaint();
            }
            State::Ready => {
                // The cursor says what a press does here, and a tip names
                // the button under it. The eyedropper's lens follows the
                // pointer, and so does Highlight's outline of a word.
                self.update_cursor();
                let before = self.tip;
                self.tip = self.compute_tip();
                let follows = self.eyedropper || (self.tool == Tool::Highlight && self.armed);
                if before != self.tip || follows { self.repaint(); }
            }
            State::Waiting => {
                // The magnifier and the highlight follow the pointer, so the
                // first edge can be placed to the pixel.
                self.update_target();
                let on_dock = self.mode.hovers() && rect_contains(self.mode_bar().rect, p);
                self.window.set_cursor(if on_dock { CursorIcon::Pointer } else { CursorIcon::Crosshair });
                self.repaint();
            }
            State::Typing => {}
        }
        Out::Continue
    }

    /// The edge or corner of `r` under the pointer, at a size that stays the
    /// same on a high-DPI screen.
    pub(super) fn edge_at(&self, r: Rect4) -> Edge {
        let as_f = |(x, y, w, h): Rect4| (x as f32, y as f32, w as f32, h as f32);
        hit_test_edge_scaled(as_f(r), (self.pointer.0 as f32, self.pointer.1 as f32), self.display_scale())
    }

    pub(super) fn pointer_in_selection(&self) -> bool { self.selection.is_some_and(|r| rect_contains(r, self.pointer)) }

    fn start_region(&mut self) {
        self.state = State::Dragging;
        self.anchor = self.pointer;
        self.selection = Some((self.pointer.0, self.pointer.1, 0, 0));
        self.repaint();
    }

    fn left_down(&mut self) -> OverlayOutcome {
        if self.mode == Mode::ColorPick {
            return self.color_at_pointer().map_or(Out::Continue, Out::ColorPicked);
        }
        // An armed confirmation owns the click as it owns the keys: Close
        // again goes through with it; anywhere else calls it off, and the
        // click is swallowed so an answer never doubles as a stroke.
        if self.confirm.is_some() {
            let on_close = self.selection.is_some_and(|sel| {
                let actions = self.actions();
                let origin = action_panel_origin(self.panel_bounds(sel), sel, actions);
                matches!(action_at(origin, self.pointer, actions), Some((_, Action::Close)))
            });
            if on_close { return self.confirm_discard(); }
            self.confirm = None;
            self.repaint();
            return Out::Continue;
        }
        let on_flyout = self.flyout_rect().is_some_and(|(r, ..)| rect_contains(r, self.pointer));
        // The eyedropper takes the color under the click anywhere but on the
        // flyout, whose controls stay live (its own button puts it down, a
        // swatch picks that color instead).
        if self.eyedropper && !on_flyout {
            self.pick_color_at_pointer();
            return Out::Continue;
        }
        // The flyout before the typing check below, so Text's size slider
        // and colors change the words being typed instead of ending them. A
        // press on its padding is swallowed rather than drawing under it.
        if let Some((s, _)) = self.slider_at_pointer() {
            self.held = Some(Held::Slider(s));
            self.slider_to_pointer(s);
            return Out::Continue;
        }
        if self.press_flyout_looks() { return Out::Continue; }
        if self.press_flyout_colors() {
            let screen = std::mem::take(&mut self.wants_screen_pick);
            return if screen { Out::PickScreenColor } else { Out::Continue };
        }
        if on_flyout { return Out::Continue; }
        // A click anywhere while typing keeps what was typed, then goes on
        // to do what it would have done.
        if self.state == State::Typing { self.finish_text(); }
        // The panels before anything that draws, so a button always wins
        // over a stroke starting inside the selection.
        if self.state == State::Ready {
            if let Some(answer) = self.press_panels() { return answer; }
        }
        match self.state {
            State::Waiting => {
                let dock = self.mode_bar();
                if rect_contains(dock.rect, self.pointer) {
                    if let Some(&(mode, _)) = dock.chips.iter().find(|(_, r)| rect_contains(*r, self.pointer)) {
                        self.set_pick_mode(mode);
                    }
                    return Out::Continue;
                }
                self.update_target();
                // Screen and All screens take a click, never a drag; the
                // reading modes drag like Region.
                if matches!(self.hover.mode.available(self.screens()), PickMode::Screen | PickMode::AllScreens) {
                    self.hover.press_pending = true;
                } else {
                    self.start_region();
                }
            }
            State::Ready => self.press_on_picture(),
            _ => {}
        }
        Out::Continue
    }

    /// A press on the tool column, the action row or the timeline, with
    /// what it answers. `None` when it was on none of them.
    fn press_panels(&mut self) -> Option<OverlayOutcome> {
        let sel = self.panel_sel()?;
        let bounds = self.panel_bounds(sel);
        let slots = self.tool_set().slots();
        // Record mode shows no column, so its buttons take no clicks there.
        let slot = slot_at(slots, tool_panel_origin(slots, bounds, sel), self.pointer).filter(|_| self.mode != Mode::Record);
        if let Some((_, slot)) = slot {
            match slot {
                ToolSlot::Tool(t) => self.pick_tool(t),
                ToolSlot::Undo => self.undo(),
                ToolSlot::Redo => self.redo(),
            }
            self.repaint();
            return Some(Out::Continue);
        }
        let actions = self.actions();
        if let Some((_, action)) = action_at(action_panel_origin(bounds, sel, actions), self.pointer, actions) {
            let shift = self.mods.shift_key();
            return Some(match action {
                Action::Pin => self.commit_as_pin(),
                Action::Copy if shift => self.commit_as_text(),
                Action::Copy => self.commit_as_copy(),
                Action::Save if shift => self.commit_as_save_as(),
                Action::Save => self.commit(),
                Action::Record => self.commit_as_record(),
                Action::Close => self.request_close(),
            });
        }
        // The timeline exists only in a video with a known length, so it can
        // never take a click anywhere else.
        self.timeline_len()?;
        let bar = timeline_bar_rect(bounds);
        if rect_contains(timeline_chip_rect(bar), self.pointer) {
            if let Some(clip) = self.clip.as_mut() { clip.choice = (clip.choice + 1) % DURATIONS.len(); }
            self.repaint();
            return Some(Out::Continue);
        }
        if rect_contains(bar, self.pointer) {
            // Seek to the press, then follow the drag until the release.
            self.held = Some(Held::Playhead);
            self.scrub_to_pointer(true);
            return Some(Out::Continue);
        }
        None
    }

    /// A press with a region picked, off the panels.
    fn press_on_picture(&mut self) {
        // An edge or corner wins over drawing: it resizes. Not on a locked
        // selection, whose picture or frame the ink is pinned to, and not in
        // select mode, which gives the edge band back to ink drawn against
        // the border.
        if let Some(sel) = self.selection.filter(|_| !self.mode.locked() && !self.select.on) {
            let edge = self.edge_at(sel);
            if edge != Edge::None {
                self.state = State::Resizing;
                self.grabbed_edge = edge;
                self.repaint();
                return;
            }
        }
        // Select mode takes the topmost ink under the press and holds it. A
        // press on nothing only drops the pick: it never starts a region, so
        // no stray click here loses ink.
        if self.select.on {
            self.select.picked = self.ink_at_pointer();
            if self.select.picked.is_some() {
                self.state = State::Moving;
                self.select.last = self.pointer;
                self.select.moved = (0.0, 0.0);
            }
            self.repaint();
            return;
        }
        // Crop in hand: a press on the picture takes the crop's edge or
        // corner under it, or starts a new crop.
        if self.is_editor() && self.armed && self.tool == Tool::Crop {
            if let (Some(pic), Some(sel)) = (self.picture, self.selection) {
                if rect_contains(pic, self.pointer) {
                    let grab = match self.edge_at(sel) {
                        Edge::None => CropGrab::New(self.pointer),
                        edge => CropGrab::Edge(edge),
                    };
                    self.crop_drag = Some(CropDrag { before: sel, grab });
                    self.repaint();
                    return;
                }
            }
        }
        if !self.pointer_in_selection() {
            if self.is_editor() {
                // The canvas around the picture is the window's handle.
                let _ = self.window.drag_window();
            } else {
                // A press outside starts a new region. The old ink goes onto
                // the undo log first, so undoing brings it back with the
                // region it was drawn in, not this fresh one.
                self.clear_ink_recoverably();
                self.start_region();
                self.next_step = 1;
            }
            return;
        }
        // Nothing drawn here could reach a recording.
        if self.mode == Mode::Record { return; }
        // No tool in hand: the picture moves the editor window.
        if self.is_editor() && !self.armed {
            let _ = self.window.drag_window();
            return;
        }
        let at = Point2::new(self.pointer.0 as f32, self.pointer.1 as f32);
        match self.tool {
            // Step places a numbered marker where it is clicked; there is no
            // drag.
            Tool::Step => {
                self.add_ink(Annotation::step_styled(at, self.next_step, self.step_look));
                self.next_step = self.next_step.saturating_add(1);
                self.repaint();
            }
            Tool::Text => self.start_typing(at),
            Tool::Highlight => self.press_highlight(),
            _ => {
                if let Some(a) = self.start_annotation() {
                    self.draft = Some(a);
                    self.state = State::Inking;
                    self.repaint();
                }
            }
        }
    }

    /// Place the Text caret at `at`, in Text's own size.
    fn start_typing(&mut self, at: Point2) {
        let px = self.text_px;
        self.draft = Some(Annotation::text_sized(self.stroke.color, at, "", px));
        self.state = State::Typing;
        self.set_typing(true);
        // An input method's candidate window goes by the caret, so it never
        // covers what is being typed.
        self.window.set_ime_cursor_area(PhysicalPosition::new(at.x as i32, at.y as i32), PhysicalSize::new(1u32, px.ceil() as u32));
        eprintln!("framefang: typing from ({}, {})", at.x, at.y);
        self.repaint();
    }

    /// `a` joins the ink as one undo step. In a video it is stamped with its
    /// window then (`ink_window`); untouched (playhead at 0, chip at "End")
    /// that is none, so such a session burns exactly as one without a
    /// timeline.
    pub(super) fn add_ink(&mut self, mut a: Annotation) {
        if self.clip.is_some() { a.time = self.ink_window(); }
        self.ink.push(a.clone());
        self.history.record(EditOp::Add { index: self.ink.len() - 1, annotation: a });
    }

    fn left_up(&mut self) -> OverlayOutcome {
        match self.held {
            // A slider or the spectrum let go: its value is saved, so the
            // next session opens with it.
            Some(Held::Slider(_) | Held::Spectrum) => {
                self.held = None;
                self.save_sliders();
                return Out::Continue;
            }
            // The playhead let go: one exact frame where it stopped, since
            // the drag only swapped frames now and then.
            Some(Held::Playhead) => {
                self.held = None;
                self.scrub_to_pointer(true);
            }
            None => {}
        }
        if self.crop_drag.is_some() {
            self.finish_crop();
            return Out::Continue;
        }
        match self.state {
            State::Waiting if std::mem::take(&mut self.hover.press_pending) => return self.take_target(),
            State::Dragging => {
                let r = rect_from(self.anchor, self.pointer);
                match (r.2 < 4, r.3 < 4) {
                    // A click, not a drag: take what the highlight shows.
                    (true, true) => return self.take_target(),
                    (false, false) => {
                        self.selection = Some(r);
                        self.state = State::Ready;
                        let switch = self.switch_held();
                        return self.finish_selection(switch);
                    }
                    // A sliver: nothing to take.
                    _ => {
                        self.state = State::Waiting;
                        self.selection = None;
                    }
                }
                self.repaint();
            }
            State::Inking => {
                // A click with Spotlight or Magnify made nothing worth keeping
                // (it would dim everything, or be a dot).
                if let Some(a) = self.draft.take().filter(|a| !a.is_degenerate()) { self.add_ink(a); }
                self.state = State::Ready;
                self.repaint();
            }
            State::Moving => self.finish_move(),
            State::Resizing => {
                self.state = State::Ready;
                self.grabbed_edge = Edge::None;
                self.window.set_cursor(CursorIcon::Crosshair);
                self.repaint();
            }
            _ => {}
        }
        Out::Continue
    }

    /// A right click finishes the capture, or drops the stroke being drawn.
    /// It backs out of a color pick.
    fn right_down(&mut self) -> OverlayOutcome {
        if self.mode == Mode::ColorPick { return Out::Cancelled; }
        if self.state != State::Inking { return self.commit(); }
        self.draft = None;
        self.state = State::Ready;
        self.repaint();
        Out::Continue
    }

    /// Ctrl+A: the whole monitor under the pointer, or every monitor when the
    /// selection already is that monitor. Before any region exists it
    /// finishes like a drag let go. A locked selection has nothing to pick.
    fn select_all(&mut self) -> OverlayOutcome {
        if self.mode.locked() || !matches!(self.state, State::Waiting | State::Ready) { return Out::Continue; }
        let size = self.window.inner_size();
        let screen = self.chrome_bounds(self.pointer);
        let next = if self.selection == Some(screen) { (0, 0, size.width as i32, size.height as i32) } else { screen };
        let first = self.state == State::Waiting;
        self.selection = Some(next);
        self.state = State::Ready;
        self.tip = None;
        if first {
            // Ctrl is part of this shortcut, so it is the switch key only when
            // it was already held as the capture key went.
            let switch = self.hold_at_press || (self.settings.switch_key() != SwitchKey::Ctrl && self.switch_held());
            return self.finish_selection(switch);
        }
        self.repaint();
        Out::Continue
    }

    /// A key, preferring the text the platform made of it while typing.
    /// That text is what the keystroke produces after the layout, dead keys
    /// and modifiers ("é" from ' then e, "ł" from AltGr+L, "Ω" on a Greek
    /// layout), which the key's own identity can't say. Control characters
    /// (Enter, Tab, Backspace and Esc come with text of their own) drop
    /// through to `handle_key`, which owns every command key.
    fn key_with_text(&mut self, key: Key, text: Option<SmolStr>) -> OverlayOutcome {
        if self.state == State::Typing && !self.mods.control_key() {
            let typed = text.as_deref().map(sanitize_input).unwrap_or_default();
            // A new line only ever comes from Enter, which `handle_key`
            // treats as finishing; that decision stays there.
            if !typed.is_empty() && !typed.contains('\n') {
                self.insert_text(&typed);
                return Out::Continue;
            }
        }
        self.handle_key(key)
    }

    fn insert_text(&mut self, typed: &str) {
        if typed.is_empty() { return; }
        if let Some(Annotation { kind: AnnotationKind::Text { text, .. }, .. }) = self.draft.as_mut() {
            text.push_str(typed);
            self.repaint();
        }
    }

    /// The input method on for typing and off after, any half-composed text
    /// dropped. Always on, an East Asian input method would swallow the
    /// one-letter tool keys.
    fn set_typing(&mut self, on: bool) {
        self.preedit.clear();
        self.window.set_ime_allowed(on);
    }

    fn on_ime(&mut self, ime: Ime) -> OverlayOutcome {
        if self.state != State::Typing { return Out::Continue; }
        match ime {
            // Final: in it goes, exactly as the input method built it.
            Ime::Commit(s) => {
                self.preedit.clear();
                self.insert_text(&sanitize_input(&s));
            }
            // Still composing: shown after the caret, never stored, gone if
            // the typing is cancelled.
            Ime::Preedit(s, _) => {
                self.preedit = sanitize_input(&s);
                self.repaint();
            }
            Ime::Enabled | Ime::Disabled => {
                self.preedit.clear();
                self.repaint();
            }
        }
        Out::Continue
    }

    /// Keep what was typed, if anything was, and stop typing.
    fn finish_text(&mut self) {
        if let Some(a) = self.draft.take() {
            if matches!(&a.kind, AnnotationKind::Text { text, .. } if !text.trim().is_empty()) { self.add_ink(a); }
        }
        self.state = State::Ready;
        self.set_typing(false);
        self.repaint();
    }

    /// The keys while typing: Esc drops the text, Enter keeps it (Shift+Enter
    /// starts a new line), Backspace takes back one visible character.
    fn typing_key(&mut self, key: Key) -> OverlayOutcome {
        eprintln!("framefang: typing key {key:?}");
        match key {
            Key::Named(NamedKey::Escape) => {
                self.draft = None;
                self.state = State::Ready;
                self.set_typing(false);
                self.repaint();
            }
            Key::Named(NamedKey::Enter) if self.mods.shift_key() => self.insert_text("\n"),
            Key::Named(NamedKey::Enter) => self.finish_text(),
            Key::Named(NamedKey::Backspace) => {
                // The input method cancels its own half-composed text; this
                // removes one character as seen, which for combining marks
                // or joined emoji is several code points.
                if let Some(Annotation { kind: AnnotationKind::Text { text, .. }, .. }) = self.draft.as_mut() {
                    pop_grapheme(text);
                    self.repaint();
                }
            }
            // For layouts and platforms that give a key no text;
            // `key_with_text` covers the usual case.
            Key::Named(NamedKey::Space) => self.insert_text(" "),
            // Ctrl with a letter is a command (Ctrl+Z, Ctrl+S), not text.
            Key::Character(s) if !self.mods.control_key() => self.insert_text(&sanitize_input(&s)),
            _ => {}
        }
        Out::Continue
    }

    pub(super) fn handle_key(&mut self, key: Key) -> OverlayOutcome {
        eprintln!("framefang: key {:?} while {:?} with {:?}", key, self.state, self.mods);
        // A color pick knows one key: Esc backs out.
        if self.mode == Mode::ColorPick {
            return if key == Key::Named(NamedKey::Escape) { Out::Cancelled } else { Out::Continue };
        }
        if self.state == State::Typing { return self.typing_key(key); }
        // An armed confirmation owns the keyboard: Esc again goes through
        // with it, any other key calls it off. Enter deliberately does not
        // confirm: everywhere else Enter saves, and a stray Esc followed by
        // the Enter meant all along must never lose work. A bare modifier
        // isn't an answer, so reaching for Ctrl+Z doesn't count.
        if self.confirm.is_some() && !is_modifier_key(&key) {
            if key == Key::Named(NamedKey::Escape) { return self.confirm_discard(); }
            self.confirm = None;
            self.repaint();
            return Out::Continue;
        }
        // A held annotation has moved on screen but isn't on the undo log
        // until it is let go. Delete, undo and redo all drop the pick, which
        // would lose that step and leave the picture out of step with the
        // log, so the move is settled first, as if the button came up.
        let edits = matches!(key, Key::Named(NamedKey::Delete | NamedKey::Backspace))
            || (matches!(key, Key::Character(_)) && self.mods.control_key());
        if self.state == State::Moving && edits { self.finish_move(); }
        match key {
            Key::Named(NamedKey::Escape) => self.escape(),
            Key::Named(NamedKey::Enter) => self.commit(),
            // Tab: the next capture mode, before any region exists.
            Key::Named(NamedKey::Tab) if self.state == State::Waiting && self.mode.hovers() => {
                self.hover.tab_at = Some(Instant::now());
                let next = self.hover.mode.next(self.screens(), self.is_picker());
                self.set_pick_mode(next);
                Out::Continue
            }
            // Delete and Backspace remove the annotation picked in select
            // mode (undoably), and do nothing without one.
            Key::Named(NamedKey::Delete | NamedKey::Backspace) => {
                self.delete_picked();
                Out::Continue
            }
            Key::Character(s) => self.letter_key(&s),
            _ => Out::Continue,
        }
    }

    /// A character key outside typing: shortcuts.
    fn letter_key(&mut self, s: &str) -> OverlayOutcome {
        let (ctrl, shift) = (self.mods.control_key(), self.mods.shift_key());
        // Ctrl+A picks the whole screen, in record mode too.
        if ctrl && s.eq_ignore_ascii_case("a") { return self.select_all(); }
        // Record mode has no tools and nothing to save, copy or pin; every
        // other letter would promise something it can't do.
        if self.mode == Mode::Record { return Out::Continue; }
        let Some(c) = s.chars().next() else { return Out::Continue };
        let letter = c.to_ascii_lowercase();
        // Undo and redo work with no region on screen too: that is exactly
        // what a confirmed Esc leaves, and Ctrl+Z is the way back.
        if ctrl && matches!(letter, 'z' | 'y') {
            if letter == 'y' || shift { self.redo() } else { self.undo() }
            return Out::Continue;
        }
        if self.state != State::Ready { return Out::Continue; }
        if ctrl {
            return match (letter, shift) {
                ('s', true) => self.commit_as_save_as(),
                ('s', false) => self.commit(),
                // Ctrl+Shift+C copies the words in the picture.
                ('c', true) => self.commit_as_text(),
                ('c', false) => self.commit_as_copy(),
                ('p', _) => self.commit_as_pin(),
                _ => Out::Continue,
            };
        }
        if letter == 's' {
            // S: select mode. No tool's key is S, so nothing is hidden.
            self.select.on = !self.select.on;
            if !self.select.on { self.select.picked = None; }
            self.repaint();
        } else if let Some(t) = Tool::from_key(c) {
            // As its button: out of select mode (or the next click would move
            // ink instead of drawing) and its flyout out.
            self.pick_tool(t);
        }
        Out::Continue
    }

    fn escape(&mut self) -> OverlayOutcome {
        if self.eyedropper {
            self.stop_picking();
            return Out::Continue;
        }
        // A crop being dragged goes back to how it was at the press.
        if let Some(drag) = self.crop_drag.take() {
            self.selection = Some(drag.before);
        } else if self.state == State::Inking {
            self.draft = None;
            self.state = State::Ready;
        } else if self.state == State::Moving {
            // Put it back where it was. Nothing was logged, so nothing to undo.
            let (dx, dy) = self.select.moved;
            if let Some(a) = self.select.picked.and_then(|i| self.ink.get_mut(i)) { edit::translate(a, -dx, -dy); }
            self.select.moved = (0.0, 0.0);
            self.state = State::Ready;
        } else if self.select.picked.is_some() {
            self.select.picked = None;
        } else if self.select.on {
            self.select.on = false;
        } else if self.is_editor() && self.armed && self.state == State::Ready {
            // The editor window with a tool in hand puts it down first.
            self.put_tool_down();
            return Out::Continue;
        } else if self.state == State::Ready && !self.mode.locked() {
            // A live region on the frozen desktop: clear it and its ink.
            // (A locked selection has no "no region" to fall back to, so
            // there Esc closes instead.) With nothing to lose it clears at
            // once.
            if self.ink.is_empty() { return self.discard(Discard::Region); }
            self.confirm = Some(Discard::Region);
        } else if self.has_unsaved_work() {
            self.confirm = Some(Discard::Session);
        } else {
            return Out::Cancelled;
        }
        self.repaint();
        Out::Continue
    }

    /// Go through with what the confirmation asks about.
    fn confirm_discard(&mut self) -> OverlayOutcome {
        match self.confirm.take() {
            Some(what) => self.discard(what),
            None => Out::Continue,
        }
    }

    fn discard(&mut self, what: Discard) -> OverlayOutcome {
        self.confirm = None;
        if what == Discard::Session { return Out::Cancelled; }
        self.clear_ink_recoverably();
        self.state = State::Waiting;
        self.selection = None;
        self.next_step = 1;
        self.select.picked = None;
        self.repaint();
        Out::Continue
    }

    /// Whether closing would lose work: ink on the picture, ink being drawn,
    /// or anything the undo log could still bring back (a cleared picture
    /// is one Ctrl+Z away).
    fn has_unsaved_work(&self) -> bool {
        !self.ink.is_empty() || self.draft.is_some() || self.history.holds_recoverable_work()
    }

    /// The Close button and the window manager's close: they ask, never
    /// destroy. With nothing to lose the overlay simply closes.
    fn request_close(&mut self) -> OverlayOutcome {
        if !self.has_unsaved_work() { return Out::Cancelled; }
        self.confirm = Some(Discard::Session);
        self.repaint();
        Out::Continue
    }

    pub(super) fn leave_select_mode(&mut self) {
        self.select.on = false;
        self.select.picked = None;
    }

    /// The topmost ink under the pointer. In a video only what shows at the
    /// playhead counts: what can't be seen can't be clicked.
    pub(super) fn ink_at_pointer(&self) -> Option<usize> {
        let p = Point2::new(self.pointer.0 as f32, self.pointer.1 as f32);
        match &self.clip {
            Some(clip) => self.ink.iter().rposition(|a| visible_at(a, clip.at) && edit::hit_test(a, p)),
            None => edit::hit_test_topmost(&self.ink, p),
        }
    }

    /// A held annotation let go: its whole drag is one undo step, so one
    /// Ctrl+Z puts it back where it started. Also before any key that would
    /// drop the pick mid-drag.
    fn finish_move(&mut self) {
        self.state = State::Ready;
        let (dx, dy) = std::mem::take(&mut self.select.moved);
        if let Some(index) = self.select.picked.filter(|_| (dx, dy) != (0.0, 0.0)) {
            self.history.record(EditOp::Move { index, dx, dy });
        }
        self.repaint();
    }

    /// Remove the annotation picked in select mode, logged so undo and redo
    /// both replay it.
    fn delete_picked(&mut self) {
        let Some(index) = self.select.picked.take() else { return };
        if index >= self.ink.len() { return; }
        let annotation = self.ink.remove(index);
        self.history.record(EditOp::Delete { index, annotation });
        self.next_step = edit::next_step_number(&self.ink);
        self.repaint();
    }

    pub(super) fn undo(&mut self) {
        // A crop still being dragged is let go first, so it is what's undone.
        if self.crop_drag.is_some() { self.finish_crop(); }
        let Some(op) = self.history.undo(&mut self.ink) else { return };
        if let EditOp::Crop { before, .. } = op { self.selection = Some(before); }
        self.after_history();
        // Undoing the clear that Esc (or a press outside the region) made
        // brings the region back with the ink, right where it was, rather
        // than leaving a region to drag again first.
        let cleared = matches!(op, EditOp::Clear { .. });
        if cleared && self.state == State::Waiting && self.selection.is_none() && !self.ink.is_empty() {
            if let Some(r) = self.cleared_selection {
                self.selection = Some(r);
                self.state = State::Ready;
            }
        }
    }

    pub(super) fn redo(&mut self) {
        if self.crop_drag.is_some() { self.finish_crop(); }
        let Some(op) = self.history.redo(&mut self.ink) else { return };
        if let EditOp::Crop { after, .. } = op { self.selection = Some(after); }
        self.after_history();
    }

    /// A crop let go: a click (no rectangle to speak of) leaves the crop as
    /// it was; anything else is one undo step.
    fn finish_crop(&mut self) {
        let Some(drag) = self.crop_drag.take() else { return };
        let now = self.selection.unwrap_or(drag.before);
        if now.2 < 4 || now.3 < 4 {
            self.selection = Some(drag.before);
        } else if now != drag.before {
            self.history.record(EditOp::Crop { before: drag.before, after: now });
        }
        self.repaint();
    }

    /// After any undo or redo: the Step numbers follow the markers left, and
    /// the select-mode pick goes, since indices shift.
    fn after_history(&mut self) {
        self.next_step = edit::next_step_number(&self.ink);
        self.select.picked = None;
        self.repaint();
    }

    /// Take the ink off the picture without destroying it. Both ways of
    /// clearing a region (a confirmed Esc, a press outside it) once threw
    /// the ink away, so one stray key could wipe minutes of work. Now it
    /// goes onto the undo log as one step, and `undo` restores the region
    /// with it.
    fn clear_ink_recoverably(&mut self) {
        if self.ink.is_empty() { return; }
        if self.selection.is_some() { self.cleared_selection = self.selection; }
        let annotations = std::mem::take(&mut self.ink);
        self.history.record(EditOp::Clear { annotations });
        self.select.picked = None;
    }

    /// Whether the switch key flips this capture: held as the hotkey went,
    /// or held now, as the region is let go.
    pub(super) fn switch_held(&self) -> bool { self.hold_at_press || self.key_down(self.settings.switch_key()) }

    /// Whether QR's own key is down now, flipping what a link does. Only
    /// now: a key held as the capture key went was about the capture.
    pub(super) fn qr_key_down(&self) -> bool { self.key_down(self.settings.qr_switch_key()) }

    pub(super) fn key_down(&self, key: SwitchKey) -> bool {
        match key {
            SwitchKey::Shift => self.mods.shift_key(),
            SwitchKey::Ctrl => self.mods.control_key(),
            SwitchKey::Alt => self.mods.alt_key(),
            // Any other key: Windows says whether it is down even when the
            // keyboard went elsewhere; other platforms go by what this
            // window heard.
            #[cfg(windows)]
            SwitchKey::Key(vk) => unsafe {
                windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState(vk as i32) as u16 & 0x8000 != 0
            },
            #[cfg(not(windows))]
            SwitchKey::Key(vk) => self.keys_down.contains(&vk),
        }
    }
}

/// A bare modifier press. winit sends these as ordinary keys, and they
/// must not count as "any other key" while a confirmation is up: reaching
/// for Ctrl+Z would quietly dismiss it.
pub(super) fn is_modifier_key(key: &Key) -> bool {
    use NamedKey::*;
    matches!(key, Key::Named(
        Shift | Control | Alt | AltGraph | Super | Meta | Hyper | Fn | FnLock
            | CapsLock | NumLock | ScrollLock | Symbol | SymbolLock
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bare_modifier_is_no_answer_to_a_confirmation() {
        for k in [NamedKey::Shift, NamedKey::Control, NamedKey::Alt, NamedKey::AltGraph, NamedKey::CapsLock] {
            assert!(is_modifier_key(&Key::Named(k)), "{k:?}");
        }
        for k in [NamedKey::Escape, NamedKey::Enter, NamedKey::Tab, NamedKey::Delete] {
            assert!(!is_modifier_key(&Key::Named(k)), "{k:?}");
        }
        assert!(!is_modifier_key(&Key::Character("p".into())));
    }
}
