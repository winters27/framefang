//! The scrolling editor: a picture taller than the screen, drawn whole into
//! a page that scrolls inside the window, with the page map (the page drawn
//! small) beside it to see where the window is and to jump.

use std::hash::{Hash, Hasher};

use framefang_core::annotation::Annotation;
use winit::dpi::PhysicalPosition;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};

use super::geometry::{rect_contains, Rect4};
use super::*;
use crate::page_map;

/// A fingerprint of the ink: how many, and where each one's bounds are.
/// It changes when one is added, moved, deleted or undone.
pub(super) fn ink_signature(ink: &[Annotation]) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    ink.len().hash(&mut h);
    for b in ink.iter().map(framefang_core::edit::bounds) {
        [b.x, b.y, b.w, b.h].map(f32::to_bits).hash(&mut h);
    }
    h.finish()
}

impl Overlay {
    /// A window point as a page point: down by the scroll in the scrolling
    /// editor, unchanged anywhere else.
    pub(super) fn page_point(&self, p: (i32, i32)) -> (i32, i32) {
        self.page.as_ref().map_or(p, |page| (p.0, p.1 + page.view.y))
    }

    /// Scroll the page to `y`. `true` when it moved.
    fn scroll_page_to(&mut self, y: i32) -> bool {
        let Some(page) = self.page.as_mut() else { return false };
        if !page.view.set_y(y) { return false; }
        // The pointer stayed; the page under it moved.
        self.pointer = self.page_point(self.pointer_win);
        self.repaint();
        true
    }

    /// Draw the map's page again once the ink changed (added, moved,
    /// deleted, undone), not while something is still being drawn.
    pub(super) fn refresh_map(&mut self) {
        if matches!(self.state, State::Inking | State::Moving | State::Typing) { return; }
        let sig = ink_signature(&self.ink);
        let Some(page) = self.page.as_mut() else { return };
        if sig == page.ink_sig { return; }
        page.ink_sig = sig;
        // The whole picture with its ink, uncropped.
        let Some(img) = self.picture.map(|base| self.inked(base)) else { return };
        let width = self.window.inner_size().width as i32;
        if let Some(page) = self.page.as_mut() {
            page.thumb = page_map::thumbnail(&img, page_map::map_rect(width, page.view.view_h, page.map_left));
        }
    }

    /// While ink, a move or a crop is dragged near the window's top or
    /// foot, scroll the page on and carry the drag along, as if the pointer
    /// had moved.
    pub(super) fn edge_scroll(&mut self) {
        let Some(page) = &self.page else { return };
        let dragging = matches!(self.state, State::Inking | State::Moving) || self.crop_drag.is_some();
        if !dragging || page.dragging { return; }
        let (view_y, view_h) = (page.view.y, page.view.view_h);
        let dy = page_map::edge_push(self.pointer_win.1, view_h);
        if dy != 0 && self.scroll_page_to(view_y + dy) {
            let _ = self.pointer_moved(self.pointer_win);
        }
    }

    /// The map's column and the page's picture inside it, in window pixels.
    pub(super) fn map_rects(&self) -> Option<(Rect4, Rect4)> {
        let page = self.page.as_ref()?;
        let column = page_map::map_rect(self.window.inner_size().width as i32, page.view.view_h, page.map_left);
        Some((column, page_map::thumb_rect(column, page.thumb.dimensions())))
    }

    /// The scrolling editor's own input, ahead of everything else: the
    /// wheel scrolls the page, a press or drag on the map jumps there.
    /// `None` passes the event on.
    pub(super) fn page_map_event(&mut self, event: &WindowEvent) -> Option<OverlayOutcome> {
        let view = self.page.as_ref()?.view;
        let (column, thumb) = self.map_rects()?;
        let (_, pic_y, _, pic_h) = self.picture?;
        let jump = |me: &mut Self| {
            let y = page_map::y_for_map_point(view, thumb, pic_y, pic_h, me.pointer_win.1);
            me.scroll_page_to(y);
        };
        let page = self.page.as_ref()?;
        let (dragging, hovered) = (page.dragging, page.hovered);
        let on_map = rect_contains(column, self.pointer_win);
        match event {
            WindowEvent::MouseWheel { delta, .. } => {
                let dy = match delta {
                    MouseScrollDelta::LineDelta(_, lines) => (-lines * page_map::NOTCH_PX as f32).round() as i32,
                    MouseScrollDelta::PixelDelta(px) => -px.y.round() as i32,
                };
                self.scroll_page_to(view.y + dy);
            }
            WindowEvent::CursorMoved { position: PhysicalPosition { x, y }, .. } => {
                self.pointer_win = (*x as i32, *y as i32);
                if !dragging {
                    let over = rect_contains(column, self.pointer_win);
                    if over != hovered {
                        if let Some(page) = self.page.as_mut() { page.hovered = over; }
                        self.repaint();
                    }
                    return None;
                }
                jump(self);
            }
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. } if on_map => {
                if let Some(page) = self.page.as_mut() { page.dragging = true; }
                jump(self);
            }
            WindowEvent::MouseInput { state: ElementState::Released, button: MouseButton::Left, .. } if dragging => {
                if let Some(page) = self.page.as_mut() { page.dragging = false; }
            }
            _ => return None,
        }
        Some(OverlayOutcome::Continue)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use framefang_core::annotation::{Point2, Stroke};

    #[test]
    fn the_signature_follows_the_ink_and_nothing_else() {
        let mut a = Annotation::rectangle(Stroke::default(), Point2::new(10.0, 10.0));
        a.extend(Point2::new(40.0, 30.0));
        let one = vec![a.clone()];
        assert_eq!(ink_signature(&one), ink_signature(&[a.clone()]), "the same ink, the same signature");
        assert_ne!(ink_signature(&one), ink_signature(&[]), "added or removed");
        let moved = vec![framefang_core::edit::translated(&a, 5.0, 0.0)];
        assert_ne!(ink_signature(&one), ink_signature(&moved), "moved");
    }
}
