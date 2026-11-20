//! The two panels beside a selection: the tool column (the session's tools,
//! a divider, Undo and Redo) and the action row under it (Pin, Copy, Save,
//! Record, Close, as the session allows). Their layout, hit-testing and the
//! tips their buttons show.
//!
//! The column stands to the right of the selection, bottom-aligned with it,
//! and moves to the left when the screen ends; the row hangs under the
//! selection, right-aligned, and moves above it when the screen ends. Both
//! stay on the selection's own monitor.

use framefang_core::tool::Tool;

use super::geometry::{rect_contains, Rect4};

/// A panel button's side.
pub(super) const PANEL_BTN: i32 = 36;
/// Between neighbouring buttons.
pub(super) const PANEL_GAP: i32 = 4;
/// Between a panel's edge and its buttons.
pub(super) const PANEL_PAD: i32 = 5;
/// Between the selection and a panel.
pub(super) const PANEL_OFFSET: i32 = 5;
/// The corner radius the panels are meant to have (they draw square for
/// now); the flyout's pointer keeps clear of it.
pub(super) const PANEL_RADIUS: i32 = 8;
/// Above and below the divider between the tools and Undo / Redo.
const GROUP_GAP: i32 = 8;
/// The divider's share of the column: a gap, the 1 px line, a gap.
const DIVIDER_ROOM: i32 = 2 * GROUP_GAP + 1;

/// A button in the tool column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ToolSlot { Tool(Tool), Undo, Redo }

/// A column: `tools` in order, then Undo and Redo below the divider.
const fn column<const N: usize>(tools: &[Tool]) -> [ToolSlot; N] {
    assert!(tools.len() + 2 == N, "a column is its tools plus Undo and Redo");
    let mut slots = [ToolSlot::Redo; N];
    let mut i = 0;
    while i < tools.len() {
        slots[i] = ToolSlot::Tool(tools[i]);
        i += 1;
    }
    slots[i] = ToolSlot::Undo;
    slots
}

/// The editor window's column: every tool, Crop included.
pub(super) const EDITOR_SLOTS: [ToolSlot; 15] = column(&[
    Tool::Pen, Tool::Line, Tool::Arrow, Tool::Rectangle, Tool::Ellipse, Tool::Marker,
    Tool::Highlight, Tool::Text, Tool::Step, Tool::Pixelate, Tool::Spotlight, Tool::Magnify, Tool::Crop,
]);

/// A capture edited in place on the frozen desktop: no Crop, since the
/// selection's own handles crop.
const PICKER_SLOTS: [ToolSlot; 14] = column(&[
    Tool::Pen, Tool::Line, Tool::Arrow, Tool::Rectangle, Tool::Ellipse, Tool::Marker,
    Tool::Highlight, Tool::Text, Tool::Step, Tool::Pixelate, Tool::Spotlight, Tool::Magnify,
]);

/// A video frame: only what draws on top of the clip. Spotlight and
/// Magnify would freeze the clip where they dim or enlarge it, Crop can't
/// change a fixed frame, and Highlight reads its words once while a clip
/// moves on.
const VIDEO_SLOTS: [ToolSlot; 11] = column(&[
    Tool::Pen, Tool::Line, Tool::Arrow, Tool::Rectangle, Tool::Ellipse, Tool::Marker,
    Tool::Text, Tool::Step, Tool::Pixelate,
]);

/// Which column a session shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ToolSet {
    /// The frozen desktop, edited in place.
    Picker,
    /// A picture in its own editor window.
    Editor,
    /// A video frame.
    Video,
}

impl ToolSet {
    pub fn slots(self) -> &'static [ToolSlot] {
        match self {
            ToolSet::Picker => &PICKER_SLOTS,
            ToolSet::Editor => &EDITOR_SLOTS,
            ToolSet::Video => &VIDEO_SLOTS,
        }
    }

    /// Whether `t` is in this session's column, so its key works too.
    pub fn has(self, t: Tool) -> bool { self.slots().contains(&ToolSlot::Tool(t)) }
}

/// How many of `slots` are tools; the divider follows them.
pub(super) fn tool_count(slots: &[ToolSlot]) -> usize {
    slots.iter().filter(|s| matches!(s, ToolSlot::Tool(_))).count()
}

/// A button in the action row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Action { Pin, Copy, Save, Record, Close }

/// A capture: the region can be pinned, copied, saved, or handed straight
/// to the recorder (the rectangle is already picked, so no second drag).
pub(super) const CAPTURE_ACTIONS: [Action; 5] = [Action::Pin, Action::Copy, Action::Save, Action::Record, Action::Close];
/// A locked picture (the editor window, a video frame): recording a still
/// or a frame of a recording means nothing, so Record is left out rather
/// than left dead.
pub(super) const LOCKED_ACTIONS: [Action; 4] = [Action::Pin, Action::Copy, Action::Save, Action::Close];
/// Picking a region to record: the rectangle is all the session makes.
pub(super) const RECORD_ACTIONS: [Action; 2] = [Action::Record, Action::Close];

/// The tool column's outer width and height.
pub(super) fn tool_panel_dims(slots: &[ToolSlot]) -> (i32, i32) {
    let n = slots.len() as i32;
    let buttons = n * PANEL_BTN + (n - 1) * PANEL_GAP;
    (PANEL_BTN + 2 * PANEL_PAD, buttons + DIVIDER_ROOM + 2 * PANEL_PAD)
}

/// The action row's outer width and height.
pub(super) fn action_panel_dims(actions: &[Action]) -> (i32, i32) {
    let n = actions.len() as i32;
    (n * PANEL_BTN + (n - 1) * PANEL_GAP + 2 * PANEL_PAD, PANEL_BTN + 2 * PANEL_PAD)
}

/// The tool column's top left for a selection `sel` on the monitor
/// `bounds`. Right of the selection unless that runs off the monitor, then
/// left of it. Its foot is level with the selection's, beside the action
/// row, so on a short capture the column grows upward instead of hanging
/// far below; a monitor's edges hold it on screen. Clamping to the monitor
/// rather than the whole frame is what keeps it from drifting onto the
/// screen next door instead of flipping sides.
pub(super) fn tool_panel_origin(slots: &[ToolSlot], bounds: Rect4, sel: Rect4) -> (i32, i32) {
    let (bx, by, bw, bh) = bounds;
    let (w, h) = tool_panel_dims(slots);
    let right = sel.0 + sel.2 + PANEL_OFFSET;
    let x = if right + w > bx + bw { sel.0 - w - PANEL_OFFSET } else { right };
    let y = (sel.1 + sel.3 - h).min(by + bh - h);
    (x.max(bx), y.max(by))
}

/// The action row's top left: under the selection, right-aligned with it;
/// above it when the monitor ends below, left-aligned when the row would
/// run off the monitor's left.
pub(super) fn action_panel_origin(bounds: Rect4, sel: Rect4, actions: &[Action]) -> (i32, i32) {
    let (bx, by, _, bh) = bounds;
    let (w, h) = action_panel_dims(actions);
    let under = sel.1 + sel.3 + PANEL_OFFSET;
    let y = if under + h > by + bh { sel.1 - h - PANEL_OFFSET } else { under };
    let x = if sel.0 + sel.2 - w < bx { sel.0 } else { sel.0 + sel.2 - w };
    (x.max(bx), y.max(by))
}

/// The `i`th button of a column whose top left is `origin`. Undo and Redo
/// sit one divider further down than the tools.
pub(super) fn slot_rect(slots: &[ToolSlot], origin: (i32, i32), i: usize) -> Rect4 {
    let below_divider = if i < tool_count(slots) { 0 } else { DIVIDER_ROOM };
    let y = origin.1 + PANEL_PAD + i as i32 * (PANEL_BTN + PANEL_GAP) + below_divider;
    (origin.0 + PANEL_PAD, y, PANEL_BTN, PANEL_BTN)
}

/// The divider's row in a column whose top left is `origin`.
pub(super) fn divider_y(slots: &[ToolSlot], origin: (i32, i32)) -> i32 {
    origin.1 + PANEL_PAD + tool_count(slots) as i32 * (PANEL_BTN + PANEL_GAP) + GROUP_GAP
}

/// The `i`th button of an action row whose top left is `origin`.
pub(super) fn action_rect(origin: (i32, i32), i: usize) -> Rect4 {
    (origin.0 + PANEL_PAD + i as i32 * (PANEL_BTN + PANEL_GAP), origin.1 + PANEL_PAD, PANEL_BTN, PANEL_BTN)
}

/// The column button under `p`, with its index.
pub(super) fn slot_at(slots: &[ToolSlot], origin: (i32, i32), p: (i32, i32)) -> Option<(usize, ToolSlot)> {
    (0..slots.len()).find(|&i| rect_contains(slot_rect(slots, origin, i), p)).map(|i| (i, slots[i]))
}

/// The action button under `p`, with its index.
pub(super) fn action_at(origin: (i32, i32), p: (i32, i32), actions: &[Action]) -> Option<(usize, Action)> {
    (0..actions.len()).find(|&i| rect_contains(action_rect(origin, i), p)).map(|i| (i, actions[i]))
}

/// What a column button's tip says. A `[Key]` is drawn as a key.
pub(super) fn slot_tip(slot: ToolSlot) -> &'static str {
    let t = match slot {
        ToolSlot::Undo => return "Undo [Ctrl]+[Z]",
        ToolSlot::Redo => return "Redo [Ctrl]+[Y]",
        ToolSlot::Tool(t) => t,
    };
    match t {
        Tool::Pen => "Pen [P]",
        Tool::Line => "Line [L]",
        Tool::Arrow => "Arrow [A]",
        Tool::Rectangle => "Rectangle [R]",
        Tool::Ellipse => "Ellipse [E]",
        Tool::Marker => "Marker [M]",
        Tool::Highlight => "Highlight text [H]",
        Tool::Text => "Text [T]",
        Tool::Step => "Step [N]",
        Tool::Pixelate => "Pixelate [B]",
        Tool::Spotlight => "Spotlight [F]",
        Tool::Magnify => "Magnify [Z]",
        Tool::Crop => "Crop [C]",
    }
}

/// What an action button's tip says. A video's Save burns the clip, which
/// has no Save As; in record mode Enter is what Record does.
pub(super) fn action_tip(a: Action, video: bool, record: bool) -> &'static str {
    match a {
        Action::Pin => "Pin over the screen",
        Action::Copy => "Copy [Ctrl]+[C]  // hold [Shift] to copy its text",
        Action::Save if video => "Save [Ctrl]+[S]",
        Action::Save => "Save [Ctrl]+[S]  // hold [Shift] to Save as",
        Action::Record if record => "Record [Enter]",
        Action::Record => "Record this region",
        Action::Close => "Close [Esc]",
    }
}

/// Tips are in the pixel font at this scale.
pub(super) const TOOLTIP_SCALE: i32 = 2;
/// The accent bar down a tip's left side.
pub(super) const TOOLTIP_BAR_W: i32 = 3;
pub(super) const TOOLTIP_PAD_X: i32 = 10;
const TOOLTIP_PAD_Y: i32 = 6;
/// Between a tip and the panel it belongs to.
pub(super) const TOOLTIP_GAP: i32 = 4;

/// A tip chip's size for `label`.
pub(super) fn tooltip_size(label: &str) -> (i32, i32) {
    let words = crate::keycap::line_width(label, TOOLTIP_SCALE);
    let lines = crate::keycap::line_height(label, TOOLTIP_SCALE);
    (TOOLTIP_BAR_W + words + 2 * TOOLTIP_PAD_X, lines + 2 * TOOLTIP_PAD_Y)
}

/// Where a column button's tip goes: level with the button and clear of
/// the whole column, on its right, or on its left when the right has no
/// room (and the right again when neither does).
pub(super) fn tool_tip_spot(button: Rect4, panel: Rect4, label: &str, bounds: Rect4) -> (i32, i32) {
    let (w, h) = tooltip_size(label);
    let (bx, by, bw, bh) = bounds;
    let y = (button.1 + button.3 / 2 - h / 2).clamp(by + 4, (by + bh - h - 4).max(by + 4));
    let right = panel.0 + panel.2 + TOOLTIP_GAP;
    let left = panel.0 - TOOLTIP_GAP - w;
    let x = if right + w > bx + bw - 4 && left >= bx + 4 { left } else { right };
    (x, y)
}

/// Where an action button's tip goes: centered on the button, just under
/// the whole row, or just over it when the screen ends below (Brandon
/// 2026-11-20: tips at the pointer covered the buttons, so "underneath it
/// and not overlapping it").
pub(super) fn action_tip_spot(button: Rect4, panel: Rect4, label: &str, bounds: Rect4) -> (i32, i32) {
    let (w, h) = tooltip_size(label);
    let (bx, by, bw, bh) = bounds;
    let x = (button.0 + button.2 / 2 - w / 2).clamp(bx + 4, (bx + bw - w - 4).max(bx + 4));
    let under = panel.1 + panel.3 + TOOLTIP_GAP;
    let over = panel.1 - TOOLTIP_GAP - h;
    let y = if under + h <= by + bh {
        under
    } else if over >= by {
        over
    } else {
        under.min(by + bh - h)
    };
    (x, y)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_session_shows_its_tools_with_undo_and_redo_last() {
        for set in [ToolSet::Picker, ToolSet::Editor, ToolSet::Video] {
            let slots = set.slots();
            let tools = tool_count(slots);
            assert_eq!(&slots[tools..], &[ToolSlot::Undo, ToolSlot::Redo], "{set:?}");
            // Every button's middle hit-tests back to it.
            for i in 0..slots.len() {
                let (x, y, w, h) = slot_rect(slots, (0, 0), i);
                assert_eq!(slot_at(slots, (0, 0), (x + w / 2, y + h / 2)).map(|(j, _)| j), Some(i));
            }
            // The divider sits in the gap between the last tool and Undo.
            let last_tool = slot_rect(slots, (0, 0), tools - 1);
            let undo = slot_rect(slots, (0, 0), tools);
            let line = divider_y(slots, (0, 0));
            assert!(line >= last_tool.1 + last_tool.3 && line < undo.1, "{set:?}");
        }
        assert!(ToolSet::Editor.has(Tool::Crop) && !ToolSet::Picker.has(Tool::Crop));
        for t in [Tool::Spotlight, Tool::Magnify, Tool::Crop, Tool::Highlight] {
            assert!(!ToolSet::Video.has(t), "{t:?}");
        }
        assert_eq!(tool_count(&EDITOR_SLOTS), Tool::ALL.len());
    }

    #[test]
    fn every_tool_tip_names_the_key_that_picks_it() {
        for t in Tool::ALL {
            let tip = slot_tip(ToolSlot::Tool(t));
            let key = tip.rsplit('[').next().and_then(|k| k.chars().next()).unwrap();
            assert_eq!(Tool::from_key(key), Some(t), "{tip}");
        }
    }

    #[test]
    fn the_action_row_hangs_under_the_selection_and_flips_at_the_foot() {
        let screen = (0, 0, 1920, 1080);
        let (w, h) = action_panel_dims(&CAPTURE_ACTIONS);
        assert_eq!(action_panel_origin(screen, (200, 100, 600, 400), &CAPTURE_ACTIONS), (800 - w, 505));
        assert_eq!(action_panel_origin(screen, (200, 600, 600, 470), &CAPTURE_ACTIONS).1, 600 - h - PANEL_OFFSET);
        // Narrower than the row: it starts at the selection's left instead.
        assert_eq!(action_panel_origin(screen, (10, 100, 20, 20), &CAPTURE_ACTIONS).0, 10);
        for i in 0..CAPTURE_ACTIONS.len() {
            let (x, y, ..) = action_rect((0, 0), i);
            assert_eq!(action_at((0, 0), (x + 1, y + 1), &CAPTURE_ACTIONS), Some((i, CAPTURE_ACTIONS[i])));
        }
    }

    #[test]
    fn tips_sit_clear_of_their_panel_wherever_it_is() {
        let overlap = |a: Rect4, b: Rect4| a.0 < b.0 + b.2 && b.0 < a.0 + a.2 && a.1 < b.1 + b.3 && b.1 < a.1 + a.3;
        let screen = (0, 0, 1920, 1080);
        let label = action_tip(Action::Copy, false, false);
        let (w, h) = tooltip_size(label);
        let (row_w, row_h) = action_panel_dims(&CAPTURE_ACTIONS);
        for row_y in [900, 2, 1080 - row_h] {
            let row = (800, row_y, row_w, row_h);
            for i in 0..CAPTURE_ACTIONS.len() {
                let button = action_rect((row.0, row.1), i);
                let (x, y) = action_tip_spot(button, row, label, screen);
                assert!(!overlap((x, y, w, h), row), "row at {row_y}, button {i}");
                assert!((x + w / 2 - (button.0 + button.2 / 2)).abs() <= 1, "centered on button {i}");
                if row_y + row_h + TOOLTIP_GAP + h <= 1080 {
                    assert_eq!(y, row_y + row_h + TOOLTIP_GAP, "just under the row");
                } else {
                    assert_eq!(y + h + TOOLTIP_GAP, row_y, "just over it at the screen's foot");
                }
                assert!(x >= 0 && y >= 0 && x + w <= 1920 && y + h <= 1080);
            }
        }
        // Against the right edge the column's tip goes to its left.
        let label = slot_tip(ToolSlot::Tool(Tool::Rectangle));
        let (w, h) = tooltip_size(label);
        for column_x in [100, 1920 - 46] {
            let column = (column_x, 300, 46, 500);
            let (x, y) = tool_tip_spot((column_x + 5, 400, PANEL_BTN, PANEL_BTN), column, label, screen);
            assert!(!overlap((x, y, w, h), column), "column at {column_x}");
            assert_eq!(y + h / 2, 400 + PANEL_BTN / 2, "level with its button");
        }
    }
}
