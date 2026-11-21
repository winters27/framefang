//! What the pointer is over and what that shows: before a region exists,
//! the picker's highlight on what a click takes (the window under the
//! pointer, its screen, every screen, or for SCROLL the part that
//! scrolls), the capture modes and the dock; once one exists, the cursor
//! that says what a press does and the tip that names a button. Also the
//! tray's tick, for what animates without events.

use std::time::{Duration, Instant};

use framefang_core::state::Edge;
use framefang_core::virtual_desktop as vdesk;
use winit::window::CursorIcon;

use super::dock::{DockFrame, READ_STEP, SCROLL_HINT};
use super::flyout::{ease_out, flyout_layout, shifted};
use super::geometry::{rect_contains, Rect4};
use super::panels::*;
use super::video::{timeline_bar_rect, timeline_chip_rect};
use super::*;
use crate::pick::{self, Look, PickMode};

/// A panel button eases lit this fast when the pointer lands on it, so it
/// is clear at once what a click would hit...
pub(super) const HOVER_IN: Duration = Duration::from_millis(120);
/// ...and back to rest a little slower when the pointer leaves.
pub(super) const HOVER_OUT: Duration = Duration::from_millis(180);
/// The pointer rests this long before a tip shows, so a passing pointer
/// doesn't flash tips over the panel.
pub(super) const TIP_DELAY: Duration = Duration::from_millis(450);
/// After a tip that was showing goes away, the next one within this shows
/// at once (the pointer is reading its way along the panel).
pub(super) const TIP_WARM: Duration = Duration::from_millis(400);

/// How long after the capture shows before the highlight starts gliding
/// between targets; until then each target lands in place.
const SETTLE: Duration = Duration::from_millis(150);

/// How lit button `b` is, 0 at rest to 1 fully hovered, given the button the
/// pointer is on (and since when) and the one it last left (when, and how
/// lit it was then). A free function so a frame can take a copy of the two
/// before it borrows the window's pixels.
pub(super) fn hover_level_of(hot: Option<(PanelHot, Instant)>, cooling: Option<(PanelHot, Instant, f32)>, b: PanelHot) -> f32 {
    let progress = |since: Instant, span: Duration| (since.elapsed().as_secs_f32() / span.as_secs_f32()).min(1.0);
    match (hot, cooling) {
        (Some((h, since)), _) if h == b => ease_out(progress(since, HOVER_IN)),
        (_, Some((c, since, from))) if c == b => from * (1.0 - ease_out(progress(since, HOVER_OUT))),
        _ => 0.0,
    }
}

/// The resize arrows for a grabbed edge or corner, the crosshair inside.
pub(super) fn edge_cursor(e: Edge) -> CursorIcon {
    match e {
        Edge::Left | Edge::Right => CursorIcon::EwResize,
        Edge::Top | Edge::Bottom => CursorIcon::NsResize,
        Edge::TopLeft | Edge::BottomRight => CursorIcon::NwseResize,
        Edge::TopRight | Edge::BottomLeft => CursorIcon::NeswResize,
        Edge::None => CursorIcon::Crosshair,
    }
}

impl Overlay {
    /// How many screens the capture spans.
    pub(super) fn screens(&self) -> usize { self.desktop.monitors().len().max(1) }

    /// The highlight shows while nothing is picked, and through the first
    /// few pixels of a press, which may still turn out to be a click.
    pub(super) fn hover_showing(&self) -> bool {
        self.mode.hovers() && match self.state {
            State::Waiting => true,
            State::Dragging => self.selection.is_some_and(|r| r.2 < 4 && r.3 < 4),
            _ => false,
        }
    }

    /// What the highlight calls the screen `screen` (frame pixels): its
    /// number when there are several.
    fn screen_name(&self, screen: Rect4) -> String {
        if self.screens() < 2 { return "Screen".to_owned(); }
        let number = self.desktop.monitors().iter()
            .position(|m| vdesk::virtual_to_frame(self.frame_origin, (m.x, m.y)) == (screen.0, screen.1))
            .map_or(1, |i| i + 1);
        format!("Screen {number}")
    }

    /// SCROLL's target: the part of the window under the pointer that
    /// scrolls, once the probe has found it, the whole window until then
    /// (or when nothing in it scrolls), the screen off every window.
    fn scroll_target(&mut self, whole: Rect4, screen: Rect4) -> (Rect4, String) {
        let p = vdesk::frame_to_virtual(self.frame_origin, self.pointer);
        let under = self.hover.windows.iter().position(|(r, _)| rect_contains(*r, p));
        let lit = under.and_then(|i| {
            let window = self.hover.windows[i].0;
            let handle = self.hover.handles.get(i).copied().unwrap_or(0);
            let part = self.hover.scroll_parts.iter()
                .find(|(h, r)| handle != 0 && *h == handle && rect_contains(*r, p))
                .and_then(|(_, r)| pick::clip(*r, window));
            if part.is_none() && handle != 0 {
                let asked = self.hover.asked.is_some_and(|(h, q)| h == handle && crate::scroll_probe::near(q, p));
                if !asked {
                    self.hover.probe.get_or_insert_with(crate::scroll_probe::Probe::start).ask((handle, p));
                    self.hover.asked = Some((handle, p));
                }
            }
            let r = part.unwrap_or(window);
            let (fx, fy) = vdesk::virtual_to_frame(self.frame_origin, (r.0, r.1));
            pick::clip((fx, fy, r.2, r.3), whole)
        });
        (lit.unwrap_or(screen), SCROLL_HINT.to_owned())
    }

    /// Aim the highlight at what is under the pointer now, gliding there
    /// from where it was when that changed.
    pub(super) fn update_target(&mut self) {
        if !self.mode.hovers() { return; }
        let size = self.window.inner_size();
        let whole = (0, 0, size.width as i32, size.height as i32);
        let screen = self.chrome_bounds(self.pointer);
        let next = match self.hover.mode.available(self.screens()) {
            PickMode::AllScreens => (whole, "All screens".to_owned()),
            PickMode::Screen => (screen, self.screen_name(screen)),
            PickMode::Scroll => self.scroll_target(whole, screen),
            PickMode::Auto | PickMode::Text | PickMode::Qr => {
                let p = vdesk::frame_to_virtual(self.frame_origin, self.pointer);
                let window = pick::window_at(&self.hover.windows, p).and_then(|(r, title)| {
                    let (fx, fy) = vdesk::virtual_to_frame(self.frame_origin, (r.0, r.1));
                    let lit = pick::clip((fx, fy, r.2, r.3), whole)?;
                    let title = title.trim();
                    Some((lit, if title.is_empty() { "Window".to_owned() } else { pick::shorten(title, 48) }))
                });
                window.unwrap_or_else(|| (screen, self.screen_name(screen)))
            }
        };
        if self.hover.target.as_ref().map(|t| t.0) != Some(next.0) {
            // Glide only from one target to the next once the capture has
            // been on screen a moment. The first box, and any correction in
            // the first frames (the window's true place is only known once it
            // shows), lands in place: Brandon didn't like the box growing in
            // from the top left as Print Screen opened.
            let settled = self.hover.dock_since.is_some_and(|t| t.elapsed() >= SETTLE);
            let glide = settled && self.hover.target.is_some();
            self.hover.glide_from = if glide { self.shown_target() } else { next.0 };
            self.hover.glide_at = Instant::now();
        }
        self.hover.target = Some(next);
    }

    /// Where the highlight is this frame, part way through its glide.
    pub(super) fn shown_target(&self) -> Rect4 {
        self.hover.target.as_ref().map_or((0, 0, 0, 0), |(r, _)| pick::glide(self.hover.glide_from, *r, self.hover.glide_at.elapsed()))
    }

    /// Change what a click takes, and keep it for the next capture. A
    /// reading mode is for this capture only: reaching one keeps the mode
    /// this capture opened in.
    pub(super) fn set_pick_mode(&mut self, mode: PickMode) {
        if mode == self.hover.mode { return; }
        self.hover.mode = mode;
        self.hover.decode_at = Some(Instant::now());
        let kept = mode.kept(self.hover.opened_with).name();
        if self.settings.capture_mode != kept {
            self.settings.capture_mode = kept.to_owned();
            self.note_settings();
        }
        self.update_target();
        self.repaint();
    }

    /// The capture dock, on the screen under the pointer, above its taskbar.
    pub(super) fn mode_bar(&self) -> pick::Dock {
        let modes = PickMode::offered(self.screens(), self.is_picker());
        pick::dock_layout(self.usable(self.chrome_bounds(self.pointer)), &modes, self.mode == Mode::Record)
    }

    /// The screen `b` (frame pixels) less what the taskbar, or another bar
    /// docked to an edge, takes from it, on Windows. The frozen picture
    /// covers the taskbar too, so without this the dock sat partly behind
    /// the real one (Brandon 2026-11-19).
    fn usable(&self, b: Rect4) -> Rect4 {
        #[cfg(windows)]
        {
            use windows::Win32::Foundation::POINT;
            use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTONEAREST};
            let (x, y) = vdesk::frame_to_virtual(self.frame_origin, (b.0 + b.2 / 2, b.1 + b.3 / 2));
            let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
            let found = unsafe { GetMonitorInfoW(MonitorFromPoint(POINT { x, y }, MONITOR_DEFAULTTONEAREST), &mut info).as_bool() };
            if found {
                let (all, work) = (info.rcMonitor, info.rcWork);
                let (left, top) = (work.left - all.left, work.top - all.top);
                let (right, bottom) = (all.right - work.right, all.bottom - work.bottom);
                return (b.0 + left, b.1 + top, b.2 - left - right, b.3 - top - bottom);
            }
        }
        b
    }

    /// The dock's animation step now.
    pub(super) fn dock_frame(&self) -> DockFrame {
        let since = self.hover.dock_since.map_or(Duration::ZERO, |t| t.elapsed());
        let steps = |d: Duration, step: Duration| (d.as_millis() / step.as_millis()) as u32;
        DockFrame {
            sweep: (since < pick::SWEEP).then(|| steps(since, Duration::from_millis(16))),
            decode: self.hover.decode_at.map(|t| t.elapsed()).filter(|d| *d < pick::DECODE).map(|d| steps(d, pick::DECODE_STEP)),
            pressed: self.hover.tab_at.is_some_and(|t| t.elapsed() < pick::KEY_PRESS),
            cursor_on: steps(since, pick::BLINK) % 2 == 0,
        }
    }

    /// The reading look TEXT, QR or SCROLL draw in, in a picker.
    pub(super) fn reading_kind(&self) -> Option<Look> { self.hover.mode.look().filter(|_| self.is_picker()) }

    /// The reading look's animation step: the scan's step while reading,
    /// else the prompt cursor's blink, with whether links open folded in
    /// (the switch key going down or up changes QR's flag). `None` outside
    /// the reading modes.
    pub(super) fn read_frame(&self) -> Option<u32> {
        self.reading_kind()?;
        let step = match self.reading {
            Some(r) => 1_000 + (r.since.elapsed().as_millis() / READ_STEP.as_millis()) as u32,
            None => (self.opened_at.elapsed().as_millis() / pick::BLINK.as_millis()) as u32 % 2,
        };
        Some(step * 2 + u32::from(self.links_open()))
    }

    /// QR: whether a link in the code will open rather than be copied: the
    /// setting, turned around while the switch key is held (or was, as the
    /// read was let go).
    pub(super) fn links_open(&self) -> bool {
        let flipped = self.reading.map_or_else(|| self.qr_key_down(), |r| r.flip);
        self.settings.open_qr_links != flipped
    }

    /// The tray's tick: what moves without events (the dock's sweep,
    /// decode, key and cursor; the reading look; the scrolling editor's
    /// drag at an edge; Highlight's reading; SCROLL's probe) redraws when
    /// its look changed.
    pub fn tick(&mut self) {
        self.edge_scroll();
        if self.tool == Tool::Highlight && self.armed { self.read_words(); }
        let found = self.hover.probe.as_ref().map(|p| p.answers()).unwrap_or_default();
        if !found.is_empty() {
            for ((handle, _), part) in found {
                let Some(r) = part else { continue };
                if !self.hover.scroll_parts.contains(&(handle, r)) { self.hover.scroll_parts.push((handle, r)); }
            }
            // The highlight moves onto a part that scrolls.
            self.update_target();
            self.repaint();
        }
        if self.read_frame() != self.read_drawn {
            self.repaint();
        } else if self.state == State::Waiting && self.mode.hovers() && self.dock_frame() != self.hover.dock_drawn {
            self.repaint();
        }
    }

    /// A click, not a drag: take what the highlight shows.
    pub(super) fn take_target(&mut self) -> OverlayOutcome {
        self.update_target();
        let Some((r, _)) = self.hover.target.clone() else {
            self.state = State::Waiting;
            self.selection = None;
            self.repaint();
            return OverlayOutcome::Continue;
        };
        self.selection = Some(r);
        self.state = State::Ready;
        self.tip = None;
        let switch = self.switch_held();
        self.finish_selection(switch)
    }

    /// The panel button under the pointer, if any. The column isn't drawn in
    /// record mode, so only the row counts there.
    pub(super) fn hot_button(&self) -> Option<PanelHot> {
        if self.state != State::Ready { return None; }
        let sel = self.panel_sel()?;
        let bounds = self.panel_bounds(sel);
        let slots = self.tool_set().slots();
        let column = tool_panel_origin(slots, bounds, sel);
        if let Some((i, _)) = slot_at(slots, column, self.pointer).filter(|_| self.mode != Mode::Record) {
            return Some(PanelHot::Slot(i));
        }
        let actions = self.actions();
        action_at(action_panel_origin(bounds, sel, actions), self.pointer, actions).map(|(i, _)| PanelHot::Action(i))
    }

    /// Follow the pointer onto and off the panel buttons: the one it lands
    /// on starts easing lit, the one it left eases back from however lit it
    /// had got. True when that changed what is drawn.
    pub(super) fn refresh_hot(&mut self) -> bool {
        let now = self.hot_button();
        if now == self.hot.map(|(b, _)| b) { return false; }
        if let Some((left, _)) = self.hot {
            self.cooling = Some((left, Instant::now(), self.hover_level(left)));
        }
        self.hot = now.map(|b| (b, Instant::now()));
        true
    }

    /// How lit button `b` is now, 0 at rest to 1 fully hovered.
    pub(super) fn hover_level(&self, b: PanelHot) -> f32 {
        hover_level_of(self.hot, self.cooling, b)
    }

    /// A button is still easing lit or back, so frames should come fast.
    pub(super) fn hover_easing(&self) -> bool {
        self.hot.is_some_and(|(_, since)| since.elapsed() < HOVER_IN)
            || self.cooling.is_some_and(|(_, since, _)| since.elapsed() < HOVER_OUT)
    }

    /// Recompute the tip for where the pointer is. A new tip waits until the
    /// pointer has rested `TIP_DELAY` on what it names; one that follows a
    /// tip that was showing (moving along the column) shows at once. True
    /// when the tip changed.
    pub(super) fn refresh_tip(&mut self) -> bool {
        let next = self.compute_tip();
        let named = |t: Option<(&'static str, i32, i32)>| t.map(|(label, ..)| label);
        if named(next) != named(self.tip) {
            if self.tip_shown() { self.tip_warm = Some(Instant::now()); }
            let warm = self.tip_warm.is_some_and(|t| t.elapsed() < TIP_WARM);
            let now = Instant::now();
            self.tip_since = if warm { now.checked_sub(TIP_DELAY).unwrap_or(now) } else { now };
        }
        let changed = next != self.tip;
        self.tip = next;
        changed
    }

    /// The tip is up: the pointer has rested on its subject long enough.
    pub(super) fn tip_shown(&self) -> bool {
        self.tip.is_some() && self.tip_since.elapsed() >= TIP_DELAY
    }

    /// The tip for what the pointer is over, and where it goes.
    pub(super) fn compute_tip(&self) -> Option<(&'static str, i32, i32)> {
        let sel = self.panel_sel()?;
        let bounds = self.panel_bounds(sel);
        let screen = self.chrome_bounds(self.pointer);
        let p = self.pointer;
        // The column isn't drawn in record mode, so nothing there to name.
        let slots = self.tool_set().slots();
        let column = tool_panel_origin(slots, bounds, sel);
        if let Some((i, slot)) = slot_at(slots, column, p).filter(|_| self.mode != Mode::Record) {
            // The tool whose flyout is out needs no tip; it would cover it.
            if matches!(slot, ToolSlot::Tool(t) if self.flyout == Some(t)) { return None; }
            let label = slot_tip(slot);
            let (w, h) = tool_panel_dims(slots);
            let (x, y) = tool_tip_spot(slot_rect(slots, column, i), (column.0, column.1, w, h), label, screen);
            return Some((label, x, y));
        }
        let actions = self.actions();
        let row = action_panel_origin(bounds, sel, actions);
        if let Some((i, action)) = action_at(row, p, actions) {
            let label = action_tip(action, self.mode == Mode::Video, self.mode == Mode::Record);
            let (w, h) = action_panel_dims(actions);
            let (x, y) = action_tip_spot(action_rect(row, i), (row.0, row.1, w, h), label, screen);
            return Some((label, x, y));
        }
        // The flyout labels its own rows; only the eyedropper, an icon, has
        // a tip there.
        if let Some((r, ..)) = self.flyout_rect().filter(|(r, ..)| rect_contains(*r, p)) {
            let on_dropper = self.flyout.and_then(|t| flyout_layout(t).colors)
                .is_some_and(|c| rect_contains(shifted(c.eyedropper, (r.0, r.1)), p));
            return (on_dropper && !self.eyedropper).then_some(("Pick a color from the picture", p.0 + 14, p.1 + 14));
        }
        // The timeline, where a click would land.
        self.timeline_len()?;
        let bar = timeline_bar_rect(bounds);
        if rect_contains(timeline_chip_rect(bar), p) { return Some(("How long new ink shows", p.0 + 14, p.1 - 24)); }
        rect_contains(bar, p).then_some(("Scrub through the clip", p.0 + 14, p.1 - 24))
    }

    /// The editor window's cursor, saying what a press does: a hand on a
    /// button, the arrow on the flyout, the move arrows where a drag moves
    /// the window (the picture with no tool in hand, the canvas), the
    /// crosshair where a drag draws. `None` leaves it to select mode.
    fn editor_cursor(&self) -> Option<CursorIcon> {
        let sel = self.panel_sel()?;
        let bounds = self.panel_bounds(sel);
        let slots = self.tool_set().slots();
        let actions = self.actions();
        let on_button = slot_at(slots, tool_panel_origin(slots, bounds, sel), self.pointer).is_some()
            || action_at(action_panel_origin(bounds, sel, actions), self.pointer, actions).is_some();
        if on_button { return Some(CursorIcon::Pointer); }
        if self.flyout_rect().is_some_and(|(r, ..)| rect_contains(r, self.pointer)) { return Some(CursorIcon::Default); }
        if self.select.on { return None; }
        // Crop in hand: the crop's edges resize, the rest of the picture
        // starts a new crop.
        if self.armed && self.tool == Tool::Crop {
            if let (Some(pic), Some(crop)) = (self.picture, self.selection) {
                if rect_contains(pic, self.pointer) { return Some(edge_cursor(self.edge_at(crop))); }
            }
        }
        Some(if self.armed && self.pointer_in_selection() { CursorIcon::Crosshair } else { CursorIcon::Move })
    }

    /// Set the cursor for what a press does where the pointer is.
    pub(super) fn update_cursor(&self) {
        let icon = if self.eyedropper {
            // It aims with the crosshair; over the flyout, whose controls
            // still work, it is the arrow.
            let on_flyout = self.flyout_rect().is_some_and(|(r, ..)| rect_contains(r, self.pointer));
            if on_flyout { CursorIcon::Default } else { CursorIcon::Crosshair }
        } else if let Some(icon) = self.editor_cursor().filter(|_| self.is_editor()) {
            icon
        } else if self.select.on {
            // No resizing in select mode: the cursor says whether there is
            // ink under it to take.
            if self.ink_at_pointer().is_some() { CursorIcon::Move } else { CursorIcon::Default }
        } else {
            let Some(sel) = self.selection else { return };
            if !self.mode.locked() {
                edge_cursor(self.edge_at(sel))
            } else if !self.is_editor() || self.pointer_in_selection() {
                // A locked selection has no edges to take. In the editor
                // window the canvas around the picture moves the window.
                CursorIcon::Crosshair
            } else {
                CursorIcon::Move
            }
        };
        self.window.set_cursor(icon);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hovered_button_eases_lit_and_back() {
        let (a, b) = (PanelHot::Slot(2), PanelHot::Action(1));
        let ago = |ms: u64| Instant::now().checked_sub(Duration::from_millis(ms)).unwrap();
        // Just arrived: barely lit. A while later: fully lit.
        assert!(hover_level_of(Some((a, Instant::now())), None, a) < 0.2);
        assert_eq!(hover_level_of(Some((a, ago(1000))), None, a), 1.0);
        // Halfway through its ease it is well past half, easing out.
        let half = hover_level_of(Some((a, ago(HOVER_IN.as_millis() as u64 / 2))), None, a);
        assert!(half > 0.6 && half < 1.0, "{half}");
        // Just left from 0.8: still about 0.8. Long gone: dark.
        let leaving = hover_level_of(None, Some((a, Instant::now(), 0.8)), a);
        assert!(leaving > 0.7 && leaving <= 0.8, "{leaving}");
        assert_eq!(hover_level_of(None, Some((a, ago(1000), 0.8)), a), 0.0);
        // Other buttons stay at rest.
        assert_eq!(hover_level_of(Some((a, ago(1000))), Some((a, ago(10), 1.0)), b), 0.0);
        // The pointer back on the button it was leaving: the hover wins.
        assert_eq!(hover_level_of(Some((a, ago(1000))), Some((a, Instant::now(), 0.3)), a), 1.0);
    }

    #[test]
    fn tips_wait_longer_than_the_hover_takes() {
        // The button answers first; the tip only after a real rest.
        assert!(TIP_DELAY > HOVER_IN * 3);
        assert!(TIP_WARM < TIP_DELAY);
    }

    #[test]
    fn each_edge_shows_the_arrows_it_moves_along() {
        assert_eq!(edge_cursor(Edge::Left), CursorIcon::EwResize);
        assert_eq!(edge_cursor(Edge::Bottom), CursorIcon::NsResize);
        assert_eq!(edge_cursor(Edge::TopLeft), CursorIcon::NwseResize);
        assert_eq!(edge_cursor(Edge::BottomLeft), CursorIcon::NeswResize);
        assert_eq!(edge_cursor(Edge::None), CursorIcon::Crosshair);
    }
}
