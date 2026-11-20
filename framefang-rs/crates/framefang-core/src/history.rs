//! The editor's undo and redo.
//!
//! Undo has to cover more than "a mark was drawn": marks can be moved,
//! deleted, wiped all at once, and the picture can be cropped. So instead of
//! snapshots of the mark list, history keeps a log of the changes made to it.
//!
//! The caller changes the mark list first and then calls [`History::record`]
//! with what it did. Undo plays a change backwards and redo plays it forwards
//! again, both against the caller's own list.

use serde::{Deserialize, Serialize};

use crate::{annotation::Annotation, edit};

/// A single change to the mark list that can be played either way.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum EditOp {
    /// `annotation` went into the list at `index` (new marks go on the end).
    Add { index: usize, annotation: Annotation },
    /// `annotation` was taken out of the list from `index`.
    Delete { index: usize, annotation: Annotation },
    /// The mark at `index` was dragged by (dx, dy).
    Move { index: usize, dx: f32, dy: f32 },
    /// Every mark was wiped at once (Esc while a selection is live). The
    /// wiped marks travel with it, so a single undo restores all of them.
    Clear { annotations: Vec<Annotation> },
    /// The picture was cropped from `before` to `after`, each `(x, y, w, h)`
    /// on the editor canvas. The marks are untouched; the editor applies
    /// whichever rectangle undo or redo returns.
    Crop { before: (i32, i32, i32, i32), after: (i32, i32, i32, i32) },
}

/// Which way a change is being played.
#[derive(Clone, Copy, PartialEq)]
enum Way {
    Forward,
    Backward,
}

/// Put `mark` back at `index`, or on the end if the list has since shrunk.
fn reinsert(marks: &mut Vec<Annotation>, index: usize, mark: &Annotation) {
    marks.insert(index.min(marks.len()), mark.clone());
}

/// Take out the mark at `index`; an index that no longer exists is ignored
/// so a stale entry can never bring the editor down.
fn take_out(marks: &mut Vec<Annotation>, index: usize) {
    if index < marks.len() {
        marks.remove(index);
    }
}

impl EditOp {
    /// Apply this change to `marks` in the given direction.
    fn play(&self, marks: &mut Vec<Annotation>, way: Way) {
        let forward = way == Way::Forward;
        match self {
            EditOp::Add { index, annotation } if forward => reinsert(marks, *index, annotation),
            EditOp::Add { index, .. } => take_out(marks, *index),
            EditOp::Delete { index, .. } if forward => take_out(marks, *index),
            EditOp::Delete { index, annotation } => reinsert(marks, *index, annotation),
            EditOp::Move { index, dx, dy } => {
                let sign = if forward { 1.0 } else { -1.0 };
                if let Some(mark) = marks.get_mut(*index) {
                    edit::translate(mark, sign * dx, sign * dy);
                }
            }
            EditOp::Clear { .. } if forward => marks.clear(),
            EditOp::Clear { annotations } => marks.clone_from(annotations),
            EditOp::Crop { .. } => {}
        }
    }

    /// Whether this change, if lost, would lose something the user made.
    fn is_work(&self) -> bool {
        match self {
            EditOp::Clear { annotations } => !annotations.is_empty(),
            _ => true,
        }
    }
}

/// The undo and redo stacks. The mark list itself belongs to the caller and
/// is passed in to each call.
#[derive(Debug, Default, Clone)]
pub struct History {
    /// Changes in the order they were made; undo takes from the end.
    done: Vec<EditOp>,
    /// Changes undone, latest last; redo takes from the end.
    undone: Vec<EditOp>,
}

impl History {
    pub fn new() -> Self { Self::default() }

    /// Log a change the caller has already made. Anything that was undone
    /// can no longer be redone once a fresh change is made.
    pub fn record(&mut self, op: EditOp) {
        self.undone.clear();
        self.done.push(op);
    }

    pub fn can_undo(&self) -> bool { !self.done.is_empty() }
    pub fn can_redo(&self) -> bool { !self.undone.is_empty() }

    /// Whether undo or redo could still bring back something the user made.
    /// The editor asks before closing while this holds, since closing would
    /// throw that work away.
    pub fn holds_recoverable_work(&self) -> bool {
        self.done.iter().chain(&self.undone).any(EditOp::is_work)
    }

    /// Play the latest change backwards and hand it back, so the caller can
    /// renumber steps or drop a selection highlight.
    pub fn undo(&mut self, annotations: &mut Vec<Annotation>) -> Option<EditOp> {
        Self::shift(&mut self.done, &mut self.undone, annotations, Way::Backward)
    }

    /// Play the latest undone change forwards again and hand it back.
    pub fn redo(&mut self, annotations: &mut Vec<Annotation>) -> Option<EditOp> {
        Self::shift(&mut self.undone, &mut self.done, annotations, Way::Forward)
    }

    /// Move the top change of `from` onto `to`, playing it on the way.
    fn shift(
        from: &mut Vec<EditOp>,
        to: &mut Vec<EditOp>,
        marks: &mut Vec<Annotation>,
        way: Way,
    ) -> Option<EditOp> {
        let op = from.pop()?;
        op.play(marks, way);
        to.push(op.clone());
        Some(op)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{annotation::{Point2, Stroke}, color::Rgba};

    fn at(x: f32, y: f32) -> Point2 { Point2::new(x, y) }

    fn scribble(x: f32) -> Annotation { Annotation::pen(Stroke::default(), at(x, x)) }

    fn label(words: &str) -> Annotation { Annotation::text(Rgba::RED, at(0.0, 0.0), words) }

    /// Draw `mark` the way the editor does: append it, then log it.
    fn draw(log: &mut History, marks: &mut Vec<Annotation>, mark: Annotation) {
        marks.push(mark.clone());
        log.record(EditOp::Add { index: marks.len() - 1, annotation: mark });
    }

    #[test]
    fn drawn_marks_undo_and_redo_in_order() {
        let mut log = History::new();
        let mut marks = Vec::new();
        draw(&mut log, &mut marks, scribble(1.0));
        draw(&mut log, &mut marks, Annotation::step(Rgba::RED, at(5.0, 5.0), 1));
        assert!(log.undo(&mut marks).is_some());
        assert_eq!(marks, vec![scribble(1.0)]);
        assert!(log.undo(&mut marks).is_some());
        assert!(marks.is_empty());
        assert_eq!(log.undo(&mut marks), None, "nothing left to undo");
        log.redo(&mut marks);
        log.redo(&mut marks);
        assert_eq!(marks.len(), 2);
        assert_eq!(log.redo(&mut marks), None, "nothing left to redo");
        assert!(log.can_undo() && !log.can_redo());
    }

    #[test]
    fn an_undone_delete_returns_to_its_old_slot() {
        let mut log = History::new();
        let mut marks = Vec::new();
        for words in ["one", "two", "three"] {
            draw(&mut log, &mut marks, label(words));
        }
        let gone = marks.remove(1);
        log.record(EditOp::Delete { index: 1, annotation: gone.clone() });
        log.undo(&mut marks);
        assert_eq!(marks, vec![label("one"), gone, label("three")]);
        log.redo(&mut marks);
        assert_eq!(marks, vec![label("one"), label("three")]);
    }

    #[test]
    fn a_drag_undoes_back_to_the_start_and_redoes_to_the_end() {
        let mut log = History::new();
        let mut marks = Vec::new();
        draw(&mut log, &mut marks, Annotation::step(Rgba::RED, at(10.0, 10.0), 1));
        let start = marks[0].clone();
        edit::translate(&mut marks[0], 30.0, -12.0);
        let end = marks[0].clone();
        log.record(EditOp::Move { index: 0, dx: 30.0, dy: -12.0 });
        log.undo(&mut marks);
        assert_eq!(marks[0], start);
        log.redo(&mut marks);
        assert_eq!(marks[0], end);
    }

    #[test]
    fn one_undo_restores_everything_a_clear_wiped() {
        let mut log = History::new();
        let mut marks = Vec::new();
        draw(&mut log, &mut marks, scribble(1.0));
        draw(&mut log, &mut marks, scribble(2.0));
        let wiped = std::mem::take(&mut marks);
        log.record(EditOp::Clear { annotations: wiped.clone() });
        log.undo(&mut marks);
        assert_eq!(marks, wiped);
        log.redo(&mut marks);
        assert!(marks.is_empty());
    }

    #[test]
    fn a_new_change_throws_away_the_redo_stack() {
        let mut log = History::new();
        let mut marks = Vec::new();
        draw(&mut log, &mut marks, scribble(1.0));
        log.undo(&mut marks);
        assert!(log.can_redo());
        draw(&mut log, &mut marks, scribble(9.0));
        assert!(!log.can_redo());
        assert_eq!(marks, vec![scribble(9.0)]);
    }

    #[test]
    fn work_counts_on_either_stack() {
        let mut log = History::new();
        let mut marks = Vec::new();
        assert!(!log.holds_recoverable_work(), "nothing made yet");
        draw(&mut log, &mut marks, scribble(1.0));
        assert!(log.holds_recoverable_work());
        log.undo(&mut marks);
        assert!(marks.is_empty());
        assert!(log.holds_recoverable_work(), "redo can still bring it back");
    }

    #[test]
    fn clearing_an_empty_canvas_is_not_work() {
        let mut log = History::new();
        log.record(EditOp::Clear { annotations: Vec::new() });
        assert!(!log.holds_recoverable_work());
        log.record(EditOp::Move { index: 0, dx: 1.0, dy: 0.0 });
        assert!(log.holds_recoverable_work());
    }

    #[test]
    fn a_crop_counts_as_work_and_returns_its_rectangles() {
        let mut log = History::new();
        let mut marks: Vec<Annotation> = Vec::new();
        let crop = EditOp::Crop { before: (0, 0, 100, 100), after: (10, 10, 50, 50) };
        log.record(crop.clone());
        assert!(log.holds_recoverable_work());
        assert_eq!(log.undo(&mut marks).as_ref(), Some(&crop));
        assert_eq!(log.redo(&mut marks).as_ref(), Some(&crop));
        assert!(marks.is_empty());
    }

    #[test]
    fn out_of_range_indices_are_skipped_without_panicking() {
        let mut log = History::new();
        let mut marks: Vec<Annotation> = Vec::new();
        log.record(EditOp::Add { index: 7, annotation: scribble(0.0) });
        log.undo(&mut marks);
        assert!(marks.is_empty());
        log.record(EditOp::Move { index: 42, dx: 1.0, dy: 1.0 });
        log.undo(&mut marks);
        log.redo(&mut marks);
        assert!(marks.is_empty());
    }
}
