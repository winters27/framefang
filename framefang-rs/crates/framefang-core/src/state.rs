//! What the capture overlay is doing right now, and which part of the
//! selection rectangle the pointer is over.

use serde::{Deserialize, Serialize};

/// The overlay's interaction mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum State {
    #[default]
    Idle,
    Selecting,
    Selected,
    Drawing,
    TextInput,
    Resizing,
    Moving,
}

/// A grab handle on the selection: one of its four sides, one of its four
/// corners, or `None` when the pointer is on neither.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    None,
    Left,
    Right,
    Top,
    Bottom,
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

impl Edge {
    /// How far (in pixels at 1x) the pointer may be from a side and still
    /// grab it.
    pub const HIT_THRESHOLD: f32 = 8.0;

    /// The resize cursor that belongs on this handle.
    pub fn cursor_size(self) -> CursorHint {
        match self {
            Self::None => CursorHint::Default,
            Self::Left | Self::Right => CursorHint::SizeWE,
            Self::Top | Self::Bottom => CursorHint::SizeNS,
            Self::TopLeft | Self::BottomRight => CursorHint::SizeNWSE,
            Self::TopRight | Self::BottomLeft => CursorHint::SizeNESW,
        }
    }

    /// True for every handle, false for `Edge::None`.
    pub fn is_some(self) -> bool {
        self != Edge::None
    }
}

/// A platform-neutral pointer shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorHint {
    Default,
    Crosshair,
    SizeWE,
    SizeNS,
    SizeNWSE,
    SizeNESW,
}

/// Which handle of `selection` (`x, y, w, h`) the point `p` is on, with the
/// 1x grab distance.
pub fn hit_test_edge(selection: (f32, f32, f32, f32), p: (f32, f32)) -> Edge {
    hit_test_edge_scaled(selection, p, UNSCALED)
}

/// The display scale of a plain, non-HiDPI screen.
const UNSCALED: f32 = 1.0;

/// Which side of a span a coordinate is near.
#[derive(Clone, Copy)]
enum Near {
    Start,
    End,
    Neither,
}

/// Classify `v` against the span `[start, end]` with grab distance `reach`.
/// When the span is so short that both ends are in reach, the start wins.
fn near_end_of(v: f32, start: f32, end: f32, reach: f32) -> Near {
    if (v - start).abs() <= reach {
        Near::Start
    } else if (v - end).abs() <= reach {
        Near::End
    } else {
        Near::Neither
    }
}

/// [`hit_test_edge`] for a display at `scale`.
///
/// Selection and pointer are both in captured-bitmap pixels, so on a 2x
/// screen a fixed 8-pixel reach would be half as wide under the finger as
/// on a 1x one. Growing the reach with the scale keeps handles equally easy
/// to grab everywhere. A scale that is zero, negative or not finite counts
/// as 1x.
pub fn hit_test_edge_scaled(selection: (f32, f32, f32, f32), p: (f32, f32), scale: f32) -> Edge {
    let (left, top, width, height) = selection;
    let factor = if scale.is_finite() && scale > 0.0 { scale } else { UNSCALED };
    let reach = Edge::HIT_THRESHOLD * factor;

    // An empty selection has no handles. Otherwise the pointer must be inside
    // the selection grown by the reach on every side; beyond that nothing is
    // grabbable.
    let empty = width <= 0.0 || height <= 0.0;
    let (right, bottom) = (left + width, top + height);
    let inside_x = p.0 >= left - reach && p.0 <= right + reach;
    let inside_y = p.1 >= top - reach && p.1 <= bottom + reach;
    if empty || !(inside_x && inside_y) {
        return Edge::None;
    }

    match (near_end_of(p.0, left, right, reach), near_end_of(p.1, top, bottom, reach)) {
        (Near::Start, Near::Start) => Edge::TopLeft,
        (Near::End, Near::Start) => Edge::TopRight,
        (Near::Start, Near::End) => Edge::BottomLeft,
        (Near::End, Near::End) => Edge::BottomRight,
        (Near::Start, Near::Neither) => Edge::Left,
        (Near::End, Near::Neither) => Edge::Right,
        (Near::Neither, Near::Start) => Edge::Top,
        (Near::Neither, Near::End) => Edge::Bottom,
        (Near::Neither, Near::Neither) => Edge::None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BOX: (f32, f32, f32, f32) = (100.0, 100.0, 200.0, 200.0);

    #[test]
    fn each_corner_is_its_own_handle() {
        assert_eq!(hit_test_edge(BOX, (100.0, 100.0)), Edge::TopLeft);
        assert_eq!(hit_test_edge(BOX, (300.0, 100.0)), Edge::TopRight);
        assert_eq!(hit_test_edge(BOX, (100.0, 300.0)), Edge::BottomLeft);
        assert_eq!(hit_test_edge(BOX, (300.0, 300.0)), Edge::BottomRight);
    }

    #[test]
    fn each_side_is_its_own_handle() {
        let square = (10.0, 10.0, 80.0, 80.0);
        let sides = [((10.0, 50.0), Edge::Left), ((90.0, 50.0), Edge::Right), ((50.0, 10.0), Edge::Top), ((50.0, 90.0), Edge::Bottom)];
        for (point, side) in sides {
            assert_eq!(hit_test_edge(square, point), side, "{point:?}");
        }
    }

    #[test]
    fn reach_grows_with_the_display_scale() {
        // 12 px in from the left side: too far at 1x, close enough at 2x.
        assert_eq!(hit_test_edge_scaled(BOX, (112.0, 200.0), 1.0), Edge::None);
        assert_eq!(hit_test_edge_scaled(BOX, (112.0, 200.0), 2.0), Edge::Left);
        // Junk scales behave as 1x instead of shrinking or exploding the reach.
        assert_eq!(hit_test_edge_scaled(BOX, (112.0, 200.0), 0.0), Edge::None);
        assert_eq!(hit_test_edge_scaled(BOX, (112.0, 200.0), f32::NAN), Edge::None);
        assert_eq!(hit_test_edge_scaled(BOX, (112.0, 200.0), -2.0), Edge::None);
        assert_eq!(hit_test_edge_scaled(BOX, (112.0, 200.0), f32::INFINITY), Edge::None);
        assert_eq!(hit_test_edge_scaled(BOX, (100.0, 100.0), 1.0), Edge::TopLeft);
    }

    #[test]
    fn the_middle_of_the_selection_grabs_nothing() {
        // Deep inside: well over 8 px from every side.
        assert_eq!(hit_test_edge((20.0, 20.0, 160.0, 160.0), (100.0, 100.0)), Edge::None);
        assert_eq!(hit_test_edge(BOX, (130.0, 270.0)), Edge::None);
    }

    #[test]
    fn handles_reach_exactly_the_threshold_and_no_further() {
        // Exactly 8 px outside still counts; 8.5 does not.
        assert_eq!(hit_test_edge(BOX, (92.0, 200.0)), Edge::Left);
        assert_eq!(hit_test_edge(BOX, (91.5, 200.0)), Edge::None);
        assert_eq!(hit_test_edge(BOX, (200.0, 308.0)), Edge::Bottom);
        // Near a side's line but past the end of it is outside the grab zone.
        assert_eq!(hit_test_edge(BOX, (100.0, 400.0)), Edge::None);
    }

    #[test]
    fn a_tiny_selection_prefers_the_top_left() {
        // Every side is within reach, so the start of each axis wins.
        let sel = (50.0, 50.0, 4.0, 4.0);
        assert_eq!(hit_test_edge(sel, (52.0, 52.0)), Edge::TopLeft);
        // Far enough right that only the right side is in reach.
        assert_eq!(hit_test_edge(sel, (61.0, 52.0)), Edge::TopRight);
    }

    #[test]
    fn an_empty_selection_has_no_handles() {
        assert_eq!(hit_test_edge((10.0, 10.0, 0.0, 50.0), (10.0, 10.0)), Edge::None);
        assert_eq!(hit_test_edge((10.0, 10.0, 50.0, -5.0), (10.0, 10.0)), Edge::None);
    }

    #[test]
    fn handles_map_to_matching_cursors() {
        assert_eq!(Edge::None.cursor_size(), CursorHint::Default);
        assert_eq!(Edge::Left.cursor_size(), CursorHint::SizeWE);
        assert_eq!(Edge::Bottom.cursor_size(), CursorHint::SizeNS);
        assert_eq!(Edge::BottomRight.cursor_size(), CursorHint::SizeNWSE);
        assert_eq!(Edge::TopRight.cursor_size(), CursorHint::SizeNESW);
        assert!(Edge::Top.is_some());
        assert!(!Edge::None.is_some());
        assert_eq!(State::default(), State::Idle);
    }
}
