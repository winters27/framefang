//! Overlay editor — region selection + annotation tools.
//!
//! Borderless fullscreen window that composites:
//!
//!   1. blit the captured screenshot at native resolution
//!   2. paint a 45 %-opaque dark dim over the whole surface
//!   3. "punch a hole" by re-blitting the screenshot (full brightness)
//!      inside the active selection rectangle
//!   4. draw any committed annotations *clipped to the selection*
//!   5. draw the in-progress annotation (if the user is mid-drag) on top
//!   6. draw a 1-pixel selection border + 8 corner/edge handles
//!   7. draw the floating tool-picker toolbar at the top of the screen
//!   8. on Enter / right-click: composite annotations onto the cropped
//!      bitmap and return it
//!   9. on Esc: never destroy work on the first press. With ink on the
//!      canvas the first Esc arms a confirmation bar; only a second Esc
//!      (or Enter) goes through with it, and even then the annotations
//!      are pushed onto the undo stack as one `EditOp::Clear`, so a
//!      single Ctrl+Z brings the whole session back — selection included.
//!
//! The window must share the tray's `EventLoop` (winit forbids two), so
//! this exposes an `Overlay` struct rather than running its own event
//! loop. `tray_loop` opens the window inside a `&ActiveEventLoop`, then
//! routes `WindowEvent`s into `Overlay::handle_event` until it returns
//! `Cancelled` or `Accepted(image)`.
//!
//! Stack: winit (window + events) + softbuffer (CPU framebuffer; no GPU)
//! + the in-tree `painter` module for line / rect / ellipse / arrow rasters.
//!
//! What's still queued for later slices: text, step, pixelate, marker,
//! line; undo/redo stack; Save/Copy/Pin choice; magnifier zoom; edge-resize
//! after the selection is committed; real text on the dimension chip.

use std::num::NonZeroU32;
use std::path::PathBuf;
use std::rc::Rc;

use anyhow::{anyhow, Result};
use image::{ImageBuffer, Rgba};
use framefang_core::annotation::{Annotation, Point2, Stroke};
use framefang_core::dpi::DisplayMap;
use framefang_core::edit;
use framefang_core::history::{EditOp, History};
use framefang_core::settings::AppSettings;
use framefang_core::state::{hit_test_edge_scaled, Edge};
use framefang_core::tool::Tool;
use framefang_core::virtual_desktop::{self as vdesk, DesktopGeometry};
use softbuffer::{Context, Surface};
use winit::dpi::PhysicalPosition;
use winit::event::{ElementState, Ime, KeyEvent, MouseButton, WindowEvent};
use winit::event_loop::ActiveEventLoop;
use winit::keyboard::{Key, ModifiersState, NamedKey, SmolStr};
use winit::window::{CursorIcon, Fullscreen, Window, WindowAttributes, WindowId, WindowLevel};

use crate::painter::{self, ImageSurface, U32Surface};

/// What the overlay window should do next after handling an event.
pub enum OverlayOutcome {
    /// Keep the overlay alive — more events expected.
    Continue,
    /// User cancelled (Esc, window close). Caller should drop the Overlay.
    Cancelled,
    /// User accepted a region (Enter / right-click / Ctrl+S). Caller saves
    /// the cropped bitmap to the configured output folder.
    Accepted(ImageBuffer<Rgba<u8>, Vec<u8>>),
    /// Ctrl+Shift+S, or Shift with the Save button: the caller asks where
    /// and in which format, with the overlay hidden meanwhile, and brings it
    /// back (`set_hidden(false)`) if the user cancels.
    SaveAs(ImageBuffer<Rgba<u8>, Vec<u8>>),
    /// User pressed Ctrl+C — caller writes the cropped bitmap to the
    /// system clipboard via arboard instead of saving to disk.
    Copied(ImageBuffer<Rgba<u8>, Vec<u8>>),
    /// Same copy, but the editor stays open (`close_after_copy` is off):
    /// the caller copies and keeps the overlay.
    CopiedOpen(ImageBuffer<Rgba<u8>, Vec<u8>>),
    /// User pressed Ctrl+P — caller floats the bitmap as a pinned, always-
    /// on-top window at the selection's screen position. Carries the (x, y)
    /// of the selection so the pin window opens right where the user
    /// captured. Mirrors `Framefang/PinForm.cs`.
    Pinned(ImageBuffer<Rgba<u8>, Vec<u8>>, (i32, i32)),
    /// Video-annotate commit: the raw annotation list plus the committed
    /// background frame. Composition into per-window overlays is deferred
    /// to the burn worker thread (`compose_overlay_groups`) because each
    /// distinct window start costs a synchronous ffmpeg seek for its
    /// pristine background — that must not stall the event loop. Replaces
    /// `Accepted` for video sessions so the screenshot save path keeps
    /// its single-bitmap payload.
    AcceptedVideo(VideoCommit),
    /// User picked a region to **record** rather than to capture: either the
    /// Record button in the action row of a normal capture session, or Enter /
    /// Record in a dedicated record-mode overlay (`new_for_region_record`).
    /// Carries `(x, y, w, h)` in window pixels — the caller clamps it to the
    /// desktop and hands it to the recorder. No bitmap: nothing is saved.
    RecordRegion((i32, i32, i32, i32)),
    /// The editor's eyedropper wants a color from anywhere on screen, not
    /// just its own picture: the caller freezes the whole screen and opens
    /// `new_for_color_pick` over it, then hands the answer back with
    /// `take_screen_color`.
    PickScreenColor,
    /// A color-pick session's answer: the color clicked.
    ColorPicked(framefang_core::color::Rgba),
    /// The region picker let go of the frozen desktop: the user released a
    /// drag, and the picture now goes to its own editor window
    /// (`Overlay::new_for_editor`) while the rest of the screen comes back.
    Chosen(Box<ChosenRegion>),
}

/// A picked region on its way from the frozen picker to the editor window.
pub struct ChosenRegion {
    pub picture: ImageBuffer<Rgba<u8>, Vec<u8>>,
    pub placement: EditorPlacement,
}

/// Where the editor window opens, in virtual-screen pixels, and where the
/// picture sits inside it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EditorPlacement {
    pub pos: (i32, i32),
    pub size: (u32, u32),
    pub picture_at: (i32, i32),
}

/// Room around the picture in the editor window: a thin edge on the left
/// and top, the tool panel on the right, the action row below.
const EDITOR_EDGE: i32 = 14;
/// The editor canvas outside the picture. The backdrop dims it like the
/// area outside a selection, to `EDITOR_CLEAR` on screen, and on Windows
/// every pixel still that color is cut out of the window (`window_shape`),
/// so the picture and its panels float over the live desktop.
const EDITOR_CANVAS: [u8; 4] = [0x33, 0x33, 0x39, 0xFF];
const EDITOR_CLEAR: u32 = {
    const fn d(c: u8) -> u32 { c as u32 * 55 / 100 }
    (d(EDITOR_CANVAS[0]) << 16) | (d(EDITOR_CANVAS[1]) << 8) | d(EDITOR_CANVAS[2])
};
/// The window's own edge, only where it cannot be cut to shape.
const EDITOR_FRAME: u32 = 0x00_3A_3A_40;
/// The editor's one green (highlights, notices, the action-panel pulse, the
/// video timeline): deep, saturated phosphor green rather than mint.
pub(crate) const NEON_GREEN: u32 = 0x00_00_FF_41;
/// Text on a green notice: near-white with a breath of the same green.
const NEON_GREEN_TEXT: u32 = 0x00_D6_FF_DE;
/// The action-panel pulse: three slow breaths, ending dark.
const PULSE_HZ: f32 = 1.25;
const PULSE_SECS: f32 = 3.0 / PULSE_HZ;

/// The editor window for a picture cut from `pic` (virtual-screen
/// `(x, y, w, h)`) on `monitor`: placed so the picture stays exactly where it
/// was on screen, pushed inward only as far as the monitor edge needs.
/// `None` when the picture plus its tools cannot fit on that monitor; the
/// region is then edited in place on the frozen desktop instead.
///
/// The tools go on the right of the picture. When that would push the
/// picture off its spot (it sits near the monitor's right edge), they go on
/// the left instead: the window leaves no room on the right, so the tool
/// panel and its flyout flip to the left side on their own.
pub fn editor_placement(pic: (i32, i32, i32, i32), monitor: vdesk::MonitorRect) -> Option<EditorPlacement> {
    let (tp_w, tp_h) = tool_panel_dims();
    let (_, ap_h) = action_panel_dims(&VIDEO_ACTION_BUTTONS);
    // The tool column and the flyout that slides out beside it.
    let tools = 5 + tp_w + FLYOUT_GAP + FLYOUT_W + EDITOR_EDGE;
    // The column stands on the picture's bottom edge (`tool_panel_origin`),
    // so a picture shorter than it needs room above for the rest of it.
    let top = EDITOR_EDGE + (tp_h - pic.3).max(0);
    let bottom = 5 + ap_h + EDITOR_EDGE;
    let h = top + pic.3 + bottom;
    let (mw, mh) = (monitor.width as i32, monitor.height as i32);
    if pic.2 <= 0 || pic.3 <= 0 || h > mh { return None; }
    let y = (pic.1 - top).clamp(monitor.y, monitor.y + mh - h);
    let place = |left: i32, right: i32| -> Option<EditorPlacement> {
        let w = left + pic.2 + right;
        if w > mw { return None; }
        let x = (pic.0 - left).clamp(monitor.x, monitor.x + mw - w);
        Some(EditorPlacement { pos: (x, y), size: (w as u32, h as u32), picture_at: (pic.0 - x, pic.1 - y) })
    };
    let right = place(EDITOR_EDGE, tools);
    let stays = |p: &EditorPlacement| p.pos.0 + p.picture_at.0 == pic.0;
    match right {
        Some(p) if stays(&p) => Some(p),
        _ => place(tools, EDITOR_EDGE).filter(stays).or(right),
    }
}

/// One video-burn group: an annotations-only transparent overlay plus its
/// visibility window in clip seconds. `end == None` = until the clip ends.
pub type OverlayGroup = (ImageBuffer<Rgba<u8>, Vec<u8>>, f32, Option<f32>);

/// Deferred video-commit payload carried by `AcceptedVideo`: everything
/// `compose_overlay_groups` needs to build the per-window overlays on the
/// burn worker thread instead of inside `commit`.
pub struct VideoCommit {
    /// Annotations in draw order, each carrying its visibility window.
    pub annotations: Vec<Annotation>,
    /// The background frame displayed at commit — reused as the pristine
    /// frame for groups whose window starts at the committed scrub spot,
    /// and as the fallback when a group's own extraction fails.
    pub frame: ImageBuffer<Rgba<u8>, Vec<u8>>,
    /// Timestamp `frame` was extracted at (the editor's `scrub_frame_t`).
    pub frame_t: f32,
    /// Parsed clip length, for nudging per-group extraction times inside
    /// the clip. `None` when the Duration banner didn't parse.
    pub duration: Option<f32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// Cursor visible, no selection in progress.
    Idle,
    /// Mouse-button down; user is dragging out the rectangle.
    Selecting,
    /// Selection committed (mouse released after a drag). Enter / right-
    /// click accepts; Esc clears it; clicks on the toolbar pick a tool;
    /// clicks inside the selection start a `Drawing`.
    Selected,
    /// Mouse-button is held inside the selection while a tool is active —
    /// `current` holds the in-progress annotation. Mouse-move extends it,
    /// mouse-up commits it to `annotations`.
    Drawing,
    /// User grabbed an edge or corner of the locked-in selection — mouse-
    /// move adjusts the corresponding side of the rect, mouse-up returns
    /// to `Selected`. The grabbed edge lives in `resize_edge`.
    Resizing,
    /// User clicked with `Tool::Text` active and is now typing into a
    /// pending Text annotation. Typed characters extend the buffer in
    /// `current`; Backspace deletes; Enter commits; Esc cancels.
    TextInput,
    /// Select mode: the user grabbed an existing annotation and is
    /// dragging it. Mouse-move shifts the annotation under the cursor,
    /// mouse-up records one undoable `EditOp::Move`.
    MovingAnnotation,
}

/// What a pending Esc confirmation would throw away if the user goes
/// through with it. Set by the first Esc, cleared by any key that isn't
/// Esc / Enter — see `handle_key`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PendingDiscard {
    /// Screenshot mode with a live selection: clearing drops the region
    /// and the ink in it (recoverable with Ctrl+Z).
    ClearSelection,
    /// Closing the overlay outright. Nothing survives this one.
    CloseOverlay,
}

/// Tool / action panel geometry. Mirrors `Framefang/OverlayForm.cs::PositionToolbars`:
/// the tool panel is a vertical column adjacent to the right edge of the
/// selection; the action panel is a horizontal row beneath the selection,
/// right-aligned. Both fall back to the opposite side if they'd clip the
/// screen edge. Free-floating, never covering the whole screen.
const PANEL_BTN:    i32 = 36;
const PANEL_GAP:    i32 = 4;
const PANEL_PAD:    i32 = 5;
const PANEL_RADIUS: i32 = 8;
/// Wide gap between visually distinct groups inside the tool panel.
const PANEL_GROUP_GAP: i32 = 8;
/// Stroke widths the flyout's Size slider covers, in whole pixels. The
/// overlay opens at the width last used (`settings.last_thickness`),
/// clamped by `Knob::clamp`.
const THICKNESS_MIN: f32 = 1.0;
const THICKNESS_MAX: f32 = 24.0;
/// Width a session opens at when the saved one is unusable.
const THICKNESS_DEFAULT: f32 = 4.0;

/// Frame interval for the overlay's self-driven animations (the tool-panel
/// glow and the action-panel attention pulse). Nothing in the event stream
/// drives those, so `redraw` asks for its own next frame — paced to the
/// same ~30 Hz the tray loop polls at, because re-arming unconditionally
/// makes winit hand back the next redraw immediately and spins a core.
const ANIM_FRAME: std::time::Duration = std::time::Duration::from_millis(33);

// ── tool flyout ─────────────────────────────────────────────────────────────
// A tool's settings, slid out beside its button in the tool panel. One row
// per knob: a muted label, a track and a value readout.

/// One setting in a tool's flyout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Knob { Thickness, MarkerOpacity, TextSize, PixelSize }

impl Knob {
    /// The settings a tool has, in the order its flyout shows them.
    fn for_tool(t: Tool) -> &'static [Knob] {
        match t {
            Tool::Pen | Tool::Line | Tool::Arrow | Tool::Rectangle | Tool::Ellipse => &[Knob::Thickness],
            Tool::Marker   => &[Knob::Thickness, Knob::MarkerOpacity],
            Tool::Text     => &[Knob::TextSize],
            Tool::Pixelate => &[Knob::PixelSize],
            Tool::Step     => &[],
        }
    }

    fn label(self) -> &'static str {
        match self {
            Knob::Thickness     => "Size",
            Knob::MarkerOpacity => "Opacity",
            Knob::TextSize      => "Text",
            Knob::PixelSize     => "Blocks",
        }
    }

    /// Whole-number range of the value.
    fn range(self) -> (f32, f32) {
        match self {
            Knob::Thickness     => (THICKNESS_MIN, THICKNESS_MAX),
            // Below about 10 % a highlight is invisible.
            Knob::MarkerOpacity => (26.0, 255.0),
            Knob::TextSize      => (12.0, 96.0),
            Knob::PixelSize     => (4.0, 40.0),
        }
    }

    /// A saved value made usable: rounded, and the default when missing,
    /// NaN or out of range.
    fn clamp(self, v: f32) -> f32 {
        let (lo, hi) = self.range();
        if v.is_finite() && (lo..=hi).contains(&v) { return v.round(); }
        match self {
            Knob::Thickness     => THICKNESS_DEFAULT,
            Knob::MarkerOpacity => 200.0,
            Knob::TextSize      => 28.0,
            Knob::PixelSize     => 10.0,
        }
    }

    fn readout(self, v: f32) -> String {
        match self {
            Knob::MarkerOpacity => format!("{}%", (v * 100.0 / 255.0).round() as i32),
            _ => format!("{} px", v.round() as i32),
        }
    }
}

const FLYOUT_W:       i32 = 214;
const FLYOUT_PAD:     i32 = 8;
const FLYOUT_ROW_H:   i32 = 30;
const FLYOUT_ROW_GAP: i32 = 4;
/// Space between the tool panel and the flyout.
const FLYOUT_GAP:     i32 = 6;
const FLYOUT_LABEL_W: i32 = 48;
const FLYOUT_VALUE_W: i32 = 40;
const FLYOUT_TRACK_H: i32 = 14;
const FLYOUT_KNOB_W:  i32 = 10;
/// How long the slide-out takes.
const FLYOUT_OPEN: std::time::Duration = std::time::Duration::from_millis(170);
/// How long "Copied to clipboard" shows after a Copy that keeps the editor.
const COPIED_NOTICE: std::time::Duration = std::time::Duration::from_millis(1400);

/// The draggable track inside a row, between the label and the readout.
fn flyout_track((rx, ry, rw, rh): (i32, i32, i32, i32)) -> (i32, i32, i32, i32) {
    let tx = rx + FLYOUT_LABEL_W;
    (tx, ry + (rh - FLYOUT_TRACK_H) / 2, rw - FLYOUT_LABEL_W - FLYOUT_VALUE_W, FLYOUT_TRACK_H)
}

/// The tool a session opens on: the one the last session ended on.
fn opening_tool(settings: &AppSettings) -> Tool {
    Tool::ALL.iter().copied().find(|t| t.name() == settings.last_tool).unwrap_or(Tool::Pen)
}

/// `t`'s own color and width, as the stroke it draws with. The color can be
/// off every palette (the spectrum and the eyedropper pick any color); the
/// flyout then rings the current-color chip instead of a swatch.
fn stroke_for(settings: &AppSettings, t: Tool) -> Stroke {
    use framefang_core::color::Rgba;
    let s = settings.tool_style(t);
    let color = Rgba::from_argb(s.color_argb).with_alpha(0xFF);
    // The old pure-red default, saved before there were palettes, opens as
    // the first palette's red, as it always has.
    let color = if color == Rgba::new_opaque(255, 0, 0) {
        framefang_core::annotation::Palettes::ALL[0].colors[0]
    } else { color };
    Stroke { color, thickness: Knob::Thickness.clamp(s.thickness) }
}

/// Share of the spectrum's width given to hues; the strip after it is grays.
const SPECTRUM_HUES: f32 = 0.88;

/// The color at `(u, v)` in the flyout's spectrum, both 0 to 1: hue runs
/// across, from light at the top through full color to dark at the bottom,
/// and the last strip runs from white down to black.
fn spectrum_color(u: f32, v: f32) -> framefang_core::color::Rgba {
    let (u, v) = (u.clamp(0.0, 1.0), v.clamp(0.0, 1.0));
    if u >= SPECTRUM_HUES {
        let g = ((1.0 - v) * 255.0).round() as u8;
        return framefang_core::color::Rgba::new_opaque(g, g, g);
    }
    hsl(u / SPECTRUM_HUES * 360.0, 1.0, 0.92 - v * 0.80)
}

/// HSL (hue in degrees, saturation and lightness 0 to 1) as an opaque color.
fn hsl(h: f32, s: f32, l: f32) -> framefang_core::color::Rgba {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let hp = (h.rem_euclid(360.0)) / 60.0;
    let x = c * (1.0 - (hp % 2.0 - 1.0).abs());
    let (r, g, b) = match hp as u32 {
        0 => (c, x, 0.0), 1 => (x, c, 0.0), 2 => (0.0, c, x),
        3 => (0.0, x, c), 4 => (x, 0.0, c), _ => (c, 0.0, x),
    };
    let m = l - c / 2.0;
    let ch = |v: f32| ((v + m).clamp(0.0, 1.0) * 255.0).round() as u8;
    framefang_core::color::Rgba::new_opaque(ch(r), ch(g), ch(b))
}

/// Every tool but Pixelate has a color (pixelate shows the picture itself).
fn has_colors(t: Tool) -> bool { t != Tool::Pixelate }

// Color section of the flyout: the palette's name between two arrows, then
// its 16 colors in two rows of 8.
const SWATCH:          i32 = 21;
const SWATCH_GAP:      i32 = 4;
const SWATCH_COLS:     i32 = 8;
const PALETTE_ROW_H:   i32 = 22;
/// Space between the size rows and the color section.
const FLYOUT_SECTION_GAP: i32 = 10;
/// The flyout's label and value type, in px.
const FLYOUT_TEXT_PX:  f32 = 13.0;
/// The little pointer from the flyout to its tool's button.
const FLYOUT_POINTER:  i32 = 5;

type Rect4 = (i32, i32, i32, i32);

/// Where everything in a tool's flyout sits, relative to the flyout's top
/// left. One layout feeds drawing and hit-testing, so they always agree.
struct FlyoutLayout {
    w: i32,
    h: i32,
    knobs: Vec<(Knob, Rect4)>,
    /// Previous palette, next palette, the name between them, the swatches.
    colors: Option<(Rect4, Rect4, Rect4, [Rect4; 16])>,
    /// Under the swatches: the eyedropper, the tool's current color below
    /// it, and the spectrum beside both.
    custom: Option<CustomColors>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CustomColors {
    eyedropper: Rect4,
    current: Rect4,
    spectrum: Rect4,
}

fn flyout_layout(t: Tool) -> FlyoutLayout {
    let inner = FLYOUT_W - FLYOUT_PAD * 2;
    let mut y = FLYOUT_PAD;
    let mut knobs = Vec::new();
    for (i, k) in Knob::for_tool(t).iter().enumerate() {
        if i > 0 { y += FLYOUT_ROW_GAP; }
        knobs.push((*k, (FLYOUT_PAD, y, inner, FLYOUT_ROW_H)));
        y += FLYOUT_ROW_H;
    }
    let mut custom = None;
    let colors = has_colors(t).then(|| {
        if !knobs.is_empty() { y += FLYOUT_SECTION_GAP; }
        let arrow = PALETTE_ROW_H;
        let prev = (FLYOUT_PAD, y, arrow, PALETTE_ROW_H);
        let next = (FLYOUT_PAD + inner - arrow, y, arrow, PALETTE_ROW_H);
        let name = (FLYOUT_PAD + arrow, y, inner - arrow * 2, PALETTE_ROW_H);
        y += PALETTE_ROW_H + 6;
        let grid_w = SWATCH_COLS * SWATCH + (SWATCH_COLS - 1) * SWATCH_GAP;
        let gx = FLYOUT_PAD + (inner - grid_w) / 2;
        let swatches: [Rect4; 16] = std::array::from_fn(|i| {
            let (c, r) = (i as i32 % SWATCH_COLS, i as i32 / SWATCH_COLS);
            (gx + c * (SWATCH + SWATCH_GAP), y + r * (SWATCH + SWATCH_GAP), SWATCH, SWATCH)
        });
        y += SWATCH * 2 + SWATCH_GAP;
        // Any other color: two more rows on the same grid. The first column
        // holds the eyedropper over the current color, and the spectrum
        // fills the other seven.
        y += FLYOUT_SECTION_GAP;
        let col = SWATCH + SWATCH_GAP;
        custom = Some(CustomColors {
            eyedropper: (gx, y, SWATCH, SWATCH),
            current: (gx, y + col, SWATCH, SWATCH),
            spectrum: (gx + col, y, grid_w - col, SWATCH * 2 + SWATCH_GAP),
        });
        y += SWATCH * 2 + SWATCH_GAP;
        (prev, next, name, swatches)
    });
    FlyoutLayout { w: FLYOUT_W, h: y + FLYOUT_PAD, knobs, colors, custom }
}

fn offset((x, y, w, h): Rect4, (ox, oy): (i32, i32)) -> Rect4 { (x + ox, y + oy, w, h) }

/// Ease-out cubic: fast out of the panel, settling into place.
fn ease_out(t: f32) -> f32 { 1.0 - (1.0 - t.clamp(0.0, 1.0)).powi(3) }

// ── video timeline bar geometry ─────────────────────────────────────────────
// Bottom-center scrub strip, video mode only. Holds (left→right) a
// "current / total" clock, the seek track with playhead + per-annotation
// start ticks, and the annotation-duration chip. Sized so a 720-px bar
// fits comfortably under a full-frame selection without covering the
// action panel, and shrinks with the window on small screens.
const TIMELINE_MAX_W:   i32 = 720;
const TIMELINE_H:       i32 = 40;
const TIMELINE_MARGIN:  i32 = 16;   // min gap to the screen edges
const TIMELINE_PAD:     i32 = 10;
const TIMELINE_TRACK_H: i32 = 10;
const TIMELINE_KNOB_W:  i32 = 8;
const TIMELINE_CHIP_W:  i32 = 44;
const TIMELINE_CHIP_H:  i32 = 24;

/// Annotation-duration presets the chip cycles through. `None` = visible
/// until the clip ends. The 5×7 bitmap font is ASCII-only (non-ASCII
/// falls back to '?'), so the "infinity" preset is spelled "End".
const DURATION_CHOICES: [Option<f32>; 4] = [None, Some(3.0), Some(5.0), Some(10.0)];

fn duration_chip_label(idx: usize) -> &'static str {
    match idx % DURATION_CHOICES.len() { 0 => "End", 1 => "3s", 2 => "5s", _ => "10s" }
}

/// Tool-panel button identities. The first 9 mirror `Tool::ALL`; the last 2
/// (`Undo`, `Redo`) are actions. Color and size live in each tool's own
/// flyout, so there is no Color or Thickness button any more.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ToolPanelButton { Tool(Tool), Undo, Redo }

const TOOL_PANEL_BUTTONS: [ToolPanelButton; 11] = [
    ToolPanelButton::Tool(Tool::Pen),
    ToolPanelButton::Tool(Tool::Line),
    ToolPanelButton::Tool(Tool::Arrow),
    ToolPanelButton::Tool(Tool::Rectangle),
    ToolPanelButton::Tool(Tool::Ellipse),
    ToolPanelButton::Tool(Tool::Marker),
    ToolPanelButton::Tool(Tool::Text),
    ToolPanelButton::Tool(Tool::Step),
    ToolPanelButton::Tool(Tool::Pixelate),
    // Visual divider sits between index 8 and 9 — see `tool_panel_dims`.
    ToolPanelButton::Undo,
    ToolPanelButton::Redo,
];

/// Action-panel buttons (horizontal row under the selection). Returning
/// outcomes routed through `tray_loop`. `Close` mirrors C# OverlayForm
/// "Close (Esc)" — closes the overlay without saving.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ActionButton { Pin, Copy, Save, Record, Close }

/// Screenshot session: the region the user just dragged can be saved, copied,
/// pinned — or handed straight to the recorder, which is the same rectangle
/// they already picked and saves them dragging it twice.
const ACTION_BUTTONS: [ActionButton; 5] = [
    ActionButton::Pin, ActionButton::Copy, ActionButton::Save,
    ActionButton::Record, ActionButton::Close,
];

/// Video-annotate session: recording a region of a frame that is itself a
/// recording is meaningless, so Record is left out rather than left dead.
const VIDEO_ACTION_BUTTONS: [ActionButton; 4] = [
    ActionButton::Pin, ActionButton::Copy, ActionButton::Save, ActionButton::Close,
];

/// Record-mode session: selection only. No annotation tools, and nothing to
/// save — the rectangle is the entire output of the session.
const RECORD_ACTION_BUTTONS: [ActionButton; 2] = [
    ActionButton::Record, ActionButton::Close,
];

/// Magnifier — small zoomed lens shown near the cursor in Idle / Selecting,
/// so the user can position the selection edge by individual pixels.
const MAG_ZOOM:    i32 = 7;
const MAG_RADIUS:  i32 = 8;          // sample ±8 source pixels around cursor
const MAG_PIXELS:  i32 = MAG_RADIUS * 2 + 1;
const MAG_SIZE:    i32 = MAG_PIXELS * MAG_ZOOM;
const MAG_OFFSET:  i32 = 24;         // pixel offset from cursor to chip corner

pub struct Overlay {
    screenshot:  ImageBuffer<Rgba<u8>, Vec<u8>>,
    /// Logical <-> physical mapping for `screenshot`. The overlay works
    /// entirely in captured-bitmap pixels, which are device pixels — the same
    /// units winit reports the cursor and the window size in — so nothing in
    /// here needs converting. The map is what turns a bitmap rect back into a
    /// desktop coordinate for anything outside the overlay (pin placement, a
    /// recording region), and what tells the editor how big a device pixel is
    /// so hit targets stay physically the same size on a high-DPI panel.
    /// Defaults to a 1x identity map, which is the un-scaled desktop.
    display:     DisplayMap,
    /// Virtual-desktop geometry of `screenshot`: where the stitched capture
    /// sits in virtual-screen space and which monitors it was built from.
    /// Video sessions carry a monitor-less `DesktopGeometry::bitmap`, which
    /// keeps every mapping below an identity.
    desktop:     DesktopGeometry,
    /// Virtual-screen coordinates of framebuffer pixel (0, 0). Equal to
    /// `desktop.origin()` when the window landed where we asked; re-derived
    /// from the window itself on every redraw by `sync_frame_origin`.
    frame_origin: (i32, i32),
    /// Set once we've reported a window manager placing the overlay
    /// somewhere other than the virtual-desktop origin — the mapping copes,
    /// but it's worth exactly one line in the log and not one per frame.
    placement_logged: bool,
    window:      Rc<Window>,
    _ctx:        Context<Rc<Window>>,
    surface:     Surface<Rc<Window>, Rc<Window>>,
    state:       State,
    cursor:      (i32, i32),
    anchor:      (i32, i32),
    /// (x, y, w, h) in window-pixel coordinates, normalized so w/h are non-negative.
    selection:   Option<(i32, i32, i32, i32)>,
    tool:        Tool,
    /// Whether `tool` is picked up. The editor window opens with none: a
    /// drag on the picture then moves the window, and picking a tool (its
    /// button or key) arms it. Picking the armed tool again, or Esc, puts it
    /// down. Everywhere else a tool is always armed.
    tool_armed:  bool,
    stroke:      Stroke,
    annotations: Vec<Annotation>,
    /// Undo/redo log over `annotations`. Holds `EditOp`s rather than bare
    /// annotations so moving and deleting existing ink is reversible too;
    /// adding a new annotation clears the redo side, same convention as
    /// `Framefang/OverlayForm.cs`.
    history:     History,
    /// In-progress annotation while `state == Drawing`.
    current:     Option<Annotation>,
    /// Next number assigned by `Tool::Step`. Resets to 1 whenever the user
    /// clears the selection (Esc on `Selected` or starts a fresh drag).
    step_count:  u32,
    /// Live modifier state — winit 0.30 doesn't put modifiers on KeyEvent so
    /// we track them via `WindowEvent::ModifiersChanged` and consult them in
    /// the keyboard handler for Ctrl+Z / Ctrl+Y / Ctrl+S / Ctrl+C.
    mods:        ModifiersState,
    /// Which edge / corner is being dragged while `state == Resizing`.
    resize_edge: Edge,
    /// Palette the flyout's swatches show (`Palettes::ALL`). Stored apart
    /// from `Stroke` so flipping palettes doesn't change the tool's color
    /// until a swatch is picked. Saved as `settings.palette_index`.
    palette_index: usize,
    /// Hovered tool/action/utility button → tooltip label + anchor pixel.
    /// Recomputed on every CursorMoved while in `Selected`. Mirrors the
    /// `tip` arg in C# OverlayForm `MakeButton(tip, …)`.
    hover_tip:     Option<(&'static str, i32, i32)>,
    /// On X11, set true once we've called XSetInputFocus + XGrabKeyboard
    /// against the overlay's window. Done lazily on the first redraw so
    /// the window is already mapped by the time we make the X requests.
    /// Without this Cinnamon never delivers KeyPress to the overlay and
    /// the Text tool sees no characters.
    focus_pushed:  bool,
    /// How many times we've tried to push X11 focus. Capped — see
    /// `push_x11_focus`. Without a cap, WMs that refuse focus entirely
    /// (GNOME focus-stealing prevention; some tiling WMs) make us spin
    /// requesting redraws forever and pin a CPU core.
    focus_attempts: u32,
    /// When the action panel was first shown (transition into `Selected`).
    /// Drives the laser-green attention pulse that fades over 3 s so users
    /// find the panel after it auto-positions for any screen size.
    panel_pulse_started: Option<std::time::Instant>,
    /// Was the previous redraw in `Selected` state? Used to detect the
    /// transition that triggers `panel_pulse_started`.
    last_was_selected: bool,
    /// In-flight IME composition for the pending Text annotation. Shown after
    /// the caret while the user composes but never written into the
    /// annotation — only `Ime::Commit` does that.
    ime_preedit: String,
    /// Wall-clock when the overlay window opened. Drives the slow
    /// sequential orange-neon glow that cycles through every tool-panel
    /// button top-to-bottom (mirrors the website's tool-palette demo).
    opened_at: std::time::Instant,
    /// Live `AppSettings` snapshot for the editor session. Most fields are
    /// read-only from here except for the tool flyout's values
    /// (`marker_opacity`, `last_thickness`, `last_text_size`,
    /// `pixelate_block`), which are persisted when a knob drag ends and read
    /// back by `TrayApp` when the overlay closes.
    settings: AppSettings,
    /// When Copy last ran without closing the editor, for the brief
    /// "Copied to clipboard" notice.
    copied_at: Option<std::time::Instant>,
    /// The tool whose settings flyout is open beside its button.
    flyout: Option<Tool>,
    /// When it opened, for the slide-out.
    flyout_opened: std::time::Instant,
    /// The flyout knob being dragged; it follows CursorMoved until the
    /// button comes up, and the values are saved then.
    dragging_knob: Option<Knob>,
    /// The eyedropper is armed: the next click takes the color under the
    /// cursor, with the magnifier showing which pixel that is.
    picking_color: bool,
    /// The spectrum is being dragged; the color follows the cursor until
    /// the button comes up, and is saved then.
    dragging_spectrum: bool,
    /// Where in the spectrum the color was last picked (0 to 1 each way),
    /// for its marker. Only drawn while the tool still has that color.
    spectrum_pick: Option<(f32, f32)>,
    /// Text tool em-size in px, its own value rather than riding thickness.
    text_size: f32,
    /// Pixelate block size in px.
    pixel_size: u32,
    /// This overlay is the editor window for one picked region
    /// (`new_for_editor`), not the frozen full-desktop picker.
    editor_mode: bool,
    /// The shape last applied to the editor window, so the region is only
    /// rebuilt when what is drawn around the picture changes.
    shape: Vec<crate::window_shape::Rect>,
    /// Whether the window is on screen yet. On Windows every overlay starts
    /// cloaked and is uncloaked after its first frame (`reveal`); on X11 the
    /// editor window starts hidden and is shown once it has been cut to shape.
    shown: bool,
    /// The capture's hold key was down when the hotkey fired, so this
    /// capture does the opposite of the After-a-capture setting.
    pub hold_at_press: bool,
    /// True when annotating a video frame (`new_for_video`): the selection
    /// is locked to the full frame, Esc cancels outright, and `commit`
    /// returns the deferred `VideoCommit` payload instead of a cropped
    /// composite.
    video_mode: bool,
    /// True when the overlay was opened purely to pick a rectangle to record
    /// (`new_for_region_record`): the tool panel, the annotation tools and the
    /// Save / Copy / Pin actions are all absent, Enter or the Record button
    /// confirms, and Esc backs out. Everything else — the drag, the edge
    /// handles, the magnifier, the dimension chip — is the ordinary selector.
    record_mode: bool,
    /// Video-annotate context: the clip path, so the editor itself can
    /// re-extract frames when the user scrubs. `None` in screenshot mode.
    video_path: Option<PathBuf>,
    /// Clip length in seconds from the ffmpeg banner. `None` when parsing
    /// failed (broken file) — the timeline hides entirely and the session
    /// degrades to the pre-timeline static whole-clip editor.
    duration: Option<f32>,
    /// Current scrub position in clip seconds. New annotations stamp it
    /// as their window start; the background frame tracks it.
    scrub_pos: f32,
    /// Timestamp `screenshot` was actually extracted at — skips redundant
    /// ffmpeg spawns when a seek lands on the already-displayed frame.
    scrub_frame_t: f32,
    /// True while the user is left-dragging the timeline playhead. Same
    /// pattern as `dragging_knob`.
    dragging_playhead: bool,
    /// Last mid-drag frame swap. Each swap is a synchronous ffmpeg spawn,
    /// so mid-drag extraction is rate-limited to one per 200 ms; press
    /// and release always extract so the resting frame is exact.
    last_scrub_extract: Option<std::time::Instant>,
    /// Index into `DURATION_CHOICES` for the chip (default 0 = "End").
    duration_choice: usize,
    /// Pass 1: the capture dimmed outside the selection. Built once per
    /// overlay; a drag rewrites only the strips the selection moved across.
    /// `invalidate()` forces a rebuild, which is how `swap_scrub_frame`
    /// swaps the background to a new video frame.
    backdrop: crate::backdrop::Backdrop,
    /// `FRAMEFANG_FRAME_LOG=1`: per-frame redraw times, written to
    /// `%TEMP%\framefang-frames.log` when the overlay closes.
    frame_log: Option<Vec<(f32, f32, f32)>>,
    /// When the last self-driven animation frame was presented. Paces the
    /// redraw re-arm to `ANIM_FRAME`.
    last_anim_frame: Option<std::time::Instant>,
    /// Select mode (S): clicks pick an existing annotation instead of
    /// starting a new one. Any tool key or tool button leaves it.
    select_mode: bool,
    /// Index into `annotations` of the annotation the user selected in
    /// select mode. Cleared by every undo/redo — indices shift under them.
    selected_idx: Option<usize>,
    /// Cursor position the running move drag was last sampled at.
    move_last: (i32, i32),
    /// Total offset the running move drag has accumulated, recorded as one
    /// `EditOp::Move` on mouse-up so undo reverses the whole drag.
    move_total: (f32, f32),
    /// Armed Esc confirmation, if any. Non-`None` means the confirmation
    /// bar is on screen and the next Esc / Enter goes through with it.
    pending_discard: Option<PendingDiscard>,
    /// Selection rect at the moment the canvas was last cleared. Undoing
    /// the clear restores it, so a mis-hit Esc costs nothing at all.
    last_selection: Option<(i32, i32, i32, i32)>,
    /// The windows on screen when it froze, frontmost first, as virtual-
    /// screen frames and titles (`set_windows`). Empty where the platform
    /// cannot list them; the hover then shows the screen.
    windows: Vec<(Rect4, String)>,
    /// What a click takes before a region is dragged (`pick`).
    pick_mode: crate::pick::PickMode,
    /// What a click would take now, in frame pixels, with its label.
    target: Option<(Rect4, String)>,
    /// Where the highlight glides from, and since when.
    glide_from: Rect4,
    glide_at: std::time::Instant,
    /// A press in Screen or All screens mode, waiting for its release.
    pick_pressed: bool,
    /// This overlay is a color-pick session over the frozen screen
    /// (`new_for_color_pick`): no dim, no tools, just the color loupe, and
    /// a click answers with the color under it.
    color_pick: bool,
    /// The editor's eyedropper was just armed and wants the whole screen
    /// to pick from; the press returns `PickScreenColor`.
    wants_screen_color: bool,
}

impl Drop for Overlay {
    fn drop(&mut self) {
        #[cfg(target_os = "linux")]
        release_x11_focus();
        if let Some(log) = self.frame_log.take() {
            write_frame_log(&log);
        }
    }
}

/// Appends one overlay session's frame times to `%TEMP%\framefang-frames.log`:
/// a summary line, then `backdrop chrome total` per frame in ms.
fn write_frame_log(log: &[(f32, f32, f32)]) {
    use std::io::Write;
    if log.is_empty() { return; }
    let mut totals: Vec<f32> = log.iter().map(|f| f.2).collect();
    totals.sort_by(|a, b| a.total_cmp(b));
    let pct = |p: f32| totals[((totals.len() - 1) as f32 * p).round() as usize];
    let mut out = format!(
        "session frames={} total_ms p50={:.2} p95={:.2} max={:.2}\n",
        log.len(), pct(0.5), pct(0.95), pct(1.0),
    );
    for (b, c, t) in log {
        out.push_str(&format!("{b:.2} {c:.2} {t:.2}\n"));
    }
    let path = std::env::temp_dir().join("framefang-frames.log");
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = f.write_all(out.as_bytes());
    }
}

impl Overlay {
    /// Open the fullscreen overlay window for the given screenshot.
    ///
    /// `settings` is taken by value so the editor can mutate its own copy
    /// without racing the tray's `AppSettings`. Per-tool widgets (currently
    /// just the Marker opacity slider) update the local copy live and call
    /// `settings.save()` on commit; the tray reads `marker_opacity()` back
    /// when the overlay closes to keep its in-memory copy in sync for the
    /// next capture.
    ///
    /// `desktop` is the geometry the screenshot was stitched from
    /// (`Captured::geometry`): it places the overlay across the whole
    /// virtual desktop and maps every selection back to the right pixels.
    pub fn new(
        loop_target: &ActiveEventLoop,
        desktop: DesktopGeometry,
        screenshot: ImageBuffer<Rgba<u8>, Vec<u8>>,
        settings: AppSettings,
    ) -> Result<Self> {
        let mut me = Self::build(loop_target, screenshot, desktop, settings, false, None, None, None)?;
        me.pick_mode = crate::pick::PickMode::parse(&me.settings.capture_mode);
        Ok(me)
    }

    /// A color-pick session over the frozen screen, for the editor window's
    /// eyedropper: the whole desktop as it was, undimmed, with the color
    /// loupe at the cursor. A click answers `ColorPicked`; Esc or a right
    /// click answers `Cancelled`.
    pub fn new_for_color_pick(
        loop_target: &ActiveEventLoop,
        desktop: DesktopGeometry,
        screenshot: ImageBuffer<Rgba<u8>, Vec<u8>>,
        settings: AppSettings,
    ) -> Result<Self> {
        let mut me = Self::build(loop_target, screenshot, desktop, settings, false, None, None, None)?;
        me.color_pick = true;
        me.picking_color = true;
        Ok(me)
    }

    /// The color a color-pick session answered with: the eyedropper's tool
    /// takes it, as if picked from the picture.
    pub fn take_screen_color(&mut self, c: framefang_core::color::Rgba) {
        self.spectrum_pick = None;
        self.set_color(c.with_alpha(0xFF));
        self.show_palette_of_color();
        self.save_knobs();
        self.window.focus_window();
    }

    /// The color-pick session was cancelled: the eyedropper goes down.
    pub fn screen_color_cancelled(&mut self) {
        self.stop_picking();
        self.window.focus_window();
    }

    /// The windows that were on screen when it froze (frontmost first, in
    /// virtual-screen pixels), for the hover highlight.
    pub fn set_windows(&mut self, windows: Vec<framefang_platform::windows_list::OnScreenWindow>) {
        self.windows = windows.into_iter().map(|w| (w.rect, w.title)).collect();
        self.update_target();
    }

    /// Open the editor window for a region the picker handed off: a
    /// borderless window around the picture, placed so the picture sits
    /// exactly where it was on screen. Everything else on the desktop is
    /// live again; only this picture is frozen. The selection is locked to
    /// the picture, the way video mode locks it to the frame, and a press on
    /// the canvas around it moves the window.
    pub fn new_for_editor(
        loop_target: &ActiveEventLoop,
        chosen: ChosenRegion,
        settings: AppSettings,
    ) -> Result<Self> {
        let ChosenRegion { picture, placement } = chosen;
        let (w, h) = placement.size;
        let mut canvas = ImageBuffer::from_pixel(w, h, Rgba(EDITOR_CANVAS));
        let (px, py) = placement.picture_at;
        image::imageops::replace(&mut canvas, &picture, px as i64, py as i64);
        let desktop = DesktopGeometry::bitmap(w, h);
        let mut me = Self::build(loop_target, canvas, desktop, settings, false, None, None,
                                 Some((placement.pos, placement.size)))?;
        me.editor_mode = true;
        me.state = State::Selected;
        me.selection = Some((px, py, picture.width() as i32, picture.height() as i32));
        me.window.set_cursor(CursorIcon::Default);
        // No tool in hand yet: the picture is the window's handle until one
        // is picked, and no flyout covers anything.
        me.tool_armed = false;
        me.flyout = None;
        me.show_palette_of_color();
        // Draw the first frame now. The window starts hidden so it can be
        // cut to shape first, and Windows never asks a hidden window to
        // paint: waiting for that request left the editor invisible forever
        // (and the tray, still holding it, ignored every later capture).
        // This frame cuts the shape and then shows the window.
        me.redraw();
        Ok(me)
    }

    /// Whether the switch key flips this capture: held when the hotkey fired,
    /// or held now, as the drag is let go.
    fn switch_held(&self) -> bool {
        use framefang_core::settings::SwitchKey;
        self.hold_at_press || match self.settings.switch_key() {
            SwitchKey::Shift => self.mods.shift_key(),
            SwitchKey::Ctrl  => self.mods.control_key(),
            SwitchKey::Alt   => self.mods.alt_key(),
        }
    }

    /// The selection is fixed: the video frame, or the editor's picture.
    fn locked(&self) -> bool { self.video_mode || self.editor_mode }

    /// The frozen full-desktop picker of a normal capture, the one session
    /// that hands its region off to an editor window.
    fn is_picker(&self) -> bool { !self.video_mode && !self.record_mode && !self.editor_mode && !self.color_pick }

    /// Hand the just-released region to an editor window, or `None` to keep
    /// editing in place (the picture plus its tools would not fit on the
    /// monitor, or the capture is not 1:1 with the screen).
    fn hand_off(&self) -> Option<OverlayOutcome> {
        let sel = self.selection?;
        let pic = vdesk::frame_rect_to_virtual(self.frame_origin, sel);
        let (cx, cy) = (pic.0 + pic.2 / 2, pic.1 + pic.3 / 2);
        let monitor = *self.desktop.monitors().iter().find(|m| {
            cx >= m.x && cx < m.x + m.width as i32 && cy >= m.y && cy < m.y + m.height as i32
        })?;
        let placement = editor_placement(pic, monitor)?;
        let picture = crop(&self.screenshot, self.selection_in_bitmap()?);
        if (picture.width() as i32, picture.height() as i32) != (pic.2, pic.3) { return None; }
        Some(OverlayOutcome::Chosen(Box::new(ChosenRegion { picture, placement })))
    }

    /// Open the editor to annotate a video frame instead of a screenshot.
    /// The selection is pre-locked to the full frame (no cropping, no
    /// re-selecting, no edge-resize) and `commit` returns the annotation
    /// list + committed frame (see `compose_overlay_groups`) that the
    /// caller turns into annotations-only transparent overlays and burns
    /// over the clip with ffmpeg. A bottom-center timeline
    /// bar lets the user scrub the clip and stamp each annotation with a
    /// visibility window (`duration_secs` feeds it; `None` hides the bar
    /// and keeps the static whole-clip behavior). Copy/Pin still work —
    /// they produce an annotated *still* of the frame, which is useful on
    /// its own and keeps the action panel free of dead buttons.
    pub fn new_for_video(
        loop_target: &ActiveEventLoop,
        frame: ImageBuffer<Rgba<u8>, Vec<u8>>,
        settings: AppSettings,
        video: PathBuf,
        duration_secs: Option<f32>,
    ) -> Result<Self> {
        // A video frame is not a screen capture: it is drawn at framebuffer
        // (0, 0) whatever monitor the window ends up on, so it gets a
        // monitor-less geometry and every coordinate mapping stays identity.
        let desktop = DesktopGeometry::bitmap(frame.width(), frame.height());
        Self::build(loop_target, frame, desktop, settings, true, Some(video), duration_secs, None)
    }

    /// Open the overlay as a **recording** region selector.
    ///
    /// Same window, same drag, same edge handles as a capture session — the
    /// user shouldn't have to learn a second selector — but with the drawing
    /// half switched off, because nothing drawn here could ever end up in the
    /// video. Confirming returns `OverlayOutcome::RecordRegion`.
    pub fn new_for_region_record(
        loop_target: &ActiveEventLoop,
        desktop: DesktopGeometry,
        screenshot: ImageBuffer<Rgba<u8>, Vec<u8>>,
        settings: AppSettings,
    ) -> Result<Self> {
        let mut me = Self::build(loop_target, screenshot, desktop, settings, false, None, None, None)?;
        me.record_mode = true;
        me.pick_mode = crate::pick::PickMode::parse(&me.settings.capture_mode);
        Ok(me)
    }

    fn build(
        loop_target: &ActiveEventLoop,
        screenshot: ImageBuffer<Rgba<u8>, Vec<u8>>,
        desktop: DesktopGeometry,
        settings: AppSettings,
        video_mode: bool,
        video_path: Option<PathBuf>,
        duration_secs: Option<f32>,
        // `Some((position, size))`: an ordinary window at that spot instead
        // of the full-desktop overlay (the editor window).
        windowed: Option<((i32, i32), (u32, u32))>,
    ) -> Result<Self> {
        // Plain borderless fullscreen — let the WM manage focus + stacking
        // normally. We tried `override_redirect=true` on X11 to layer above
        // DOCK panels, but it blocked KeyPress delivery to the Text tool on
        // Cinnamon because keyboard focus doesn't propagate to override-
        // redirect windows the same way. Trade-off: dock panels may still
        // be visible at the screen edges, but every annotation tool works
        // including Text. Mirrors C# OverlayForm:
        //   `FormBorderStyle = None; WindowState = Maximized;`
        // `Fullscreen::Borderless(None)` opens at a default size on Cinnamon
        // (~800×600). Setting an explicit inner_size to the primary monitor's
        // physical size + position (0,0) + AlwaysOnTop makes the WM open us
        // at full screen even when fullscreen state isn't honored.
        let monitor_size = loop_target
            .primary_monitor()
            .or_else(|| loop_target.available_monitors().next())
            .map(|m| m.size())
            .unwrap_or(winit::dpi::PhysicalSize::new(
                screenshot.width(),
                screenshot.height(),
            ));
        let primary = loop_target.primary_monitor()
            .or_else(|| loop_target.available_monitors().next());
        let desktop_origin = desktop.origin();
        let desktop_size   = desktop.size();
        let attrs = WindowAttributes::default()
            .with_title(if video_mode { "Frame Fang — annotate recording" } else { "Frame Fang" })
            .with_decorations(false)
            .with_resizable(false)
            .with_window_level(WindowLevel::AlwaysOnTop)
            .with_window_icon(crate::brand_icon::shared());

        // Multi-monitor: the overlay has to cover the *union* of every
        // screen, because a region dragged on a monitor left of / above the
        // primary one starts at a negative virtual-screen coordinate and a
        // primary-monitor-sized window can't even show it.
        //
        // One window spanning the union is the shape winit supports the same
        // way on all three platforms: there is no "fullscreen across all
        // monitors" mode (`Fullscreen::Borderless` takes a single monitor
        // handle), and a window per monitor would mean one softbuffer
        // surface and one event stream each, with a selection drag that
        // starts in one window and ends in another — the cursor leaves the
        // first window, so the drag would need pointer grabs that behave
        // differently on X11, Windows and macOS. A borderless always-on-top
        // window positioned at the union origin is placed natively by
        // Windows and by X11 WMs; macOS honours it too unless "Displays have
        // separate Spaces" is on, in which case the window is clamped to one
        // display — `sync_frame_origin` re-derives the mapping from where
        // the window actually landed, so the crop still matches what the
        // user selected instead of silently sliding to another monitor.
        // Wayland refuses client-side positioning altogether, so the union
        // window would be an undecorated toplevel the compositor parks
        // wherever it likes, and `sync_frame_origin` could not even read
        // where. There the fullscreen-on-primary window is the only shape
        // the compositor will honour, exactly as before this branch.
        let span_union = windowed.is_none() && desktop.spans_multiple_monitors()
            && !framefang_platform::session::is_wayland();
        let attrs = if let Some((pos, size)) = windowed {
            // The editor window: an ordinary stacking window, so the rest of
            // the desktop can come in front of it again.
            // Hidden until the first frame has cut it to shape, so the canvas
            // never flashes up as a dark rectangle.
            attrs
                .with_window_level(WindowLevel::Normal)
                .with_visible(!crate::window_shape::supported())
                .with_inner_size(winit::dpi::PhysicalSize::new(size.0.max(1), size.1.max(1)))
                .with_position(PhysicalPosition::new(pos.0, pos.1))
        } else if span_union {
            attrs
                .with_inner_size(winit::dpi::PhysicalSize::new(
                    desktop_size.0.max(1), desktop_size.1.max(1)))
                .with_position(PhysicalPosition::new(desktop_origin.0, desktop_origin.1))
        } else {
            // Single monitor rooted at (0, 0): unchanged from before —
            // `Fullscreen::Borderless(None)` opens at a default size on
            // Cinnamon (~800×600), so the explicit size + position + fullscreen
            // request together make the WM open us at full screen.
            attrs
                .with_inner_size(monitor_size)
                .with_position(PhysicalPosition::new(0i32, 0i32))
                .with_fullscreen(Some(Fullscreen::Borderless(primary)))
        };
        // Windows: sized and placed while hidden, then shown cloaked, so
        // nothing of the window reaches the screen until its first frame is
        // drawn (`reveal`). Without this it popped up at the default size
        // and grew out from the top left.
        let cloak = crate::reveal::SUPPORTED;
        let attrs = if cloak { attrs.with_visible(false) } else { attrs };

        let window = loop_target
            .create_window(attrs)
            .map(Rc::new)
            .map_err(|e| anyhow!("create_window: {e}"))?;

        // Several WMs apply size/position only once the window is mapped and
        // ignore what the create request asked for. Re-assert both; the
        // request is a no-op when the window already covers the desktop.
        if span_union {
            window.set_outer_position(PhysicalPosition::new(desktop_origin.0, desktop_origin.1));
            let _ = window.request_inner_size(winit::dpi::PhysicalSize::new(
                desktop_size.0.max(1), desktop_size.1.max(1)));
        }
        if cloak {
            crate::reveal::cloak(&window);
            window.set_visible(true);
        }

        window.set_cursor(CursorIcon::Crosshair);
        window.focus_window();

        // `focus_window()` above is the spec-correct ask via _NET_ACTIVE_WINDOW.
        // Cinnamon ignores it fast enough that the overlay never gets KeyPress,
        // so we follow up with a manual XSetInputFocus + XGrabKeyboard from
        // `push_x11_focus` once the window is viewable.

        let ctx = Context::new(window.clone())
            .map_err(|e| anyhow!("softbuffer Context::new: {e}"))?;
        let surface = Surface::new(&ctx, window.clone())
            .map_err(|e| anyhow!("softbuffer Surface::new: {e}"))?;

        // Video mode opens with the whole frame locked in as the selection
        // so every tool is immediately usable and nothing can be cropped.
        let (full_w, full_h) = (screenshot.width() as i32, screenshot.height() as i32);
        Ok(Overlay {
            display: DisplayMap::identity(full_w.max(0) as u32, full_h.max(0) as u32),
            screenshot,
            desktop,
            frame_origin: desktop_origin,
            placement_logged: false,
            window,
            _ctx:        ctx,
            surface,
            state:       if video_mode { State::Selected } else { State::Idle },
            cursor:      (0, 0),
            anchor:      (0, 0),
            selection:   if video_mode { Some((0, 0, full_w, full_h)) } else { None },
            // Opens on the tool the last session ended on, in that tool's
            // own color and width.
            tool:        opening_tool(&settings),
            tool_armed:  true,
            stroke:      stroke_for(&settings, opening_tool(&settings)),
            annotations: Vec::new(),
            history:     History::new(),
            current:     None,
            step_count:  1,
            mods:        ModifiersState::empty(),
            resize_edge: Edge::None,
            palette_index: (settings.palette_index.max(0) as usize)
                .min(framefang_core::annotation::Palettes::ALL.len() - 1),
            hover_tip:     None,
            focus_pushed:  false,
            focus_attempts: 0,
            panel_pulse_started: None,
            last_was_selected:   false,
            ime_preedit:         String::new(),
            opened_at:           std::time::Instant::now(),
            copied_at: None,
            flyout: None,
            flyout_opened: std::time::Instant::now(),
            dragging_knob: None,
            picking_color: false,
            dragging_spectrum: false,
            spectrum_pick: None,
            text_size: Knob::TextSize.clamp(settings.last_text_size),
            pixel_size: Knob::PixelSize.clamp(settings.pixelate_block as f32) as u32,
            settings,
            editor_mode: false,
            shape: Vec::new(),
            shown: !cloak && (windowed.is_none() || !crate::window_shape::supported()),
            hold_at_press: false,
            video_mode,
            record_mode: false,
            video_path,
            duration: duration_secs,
            scrub_pos: 0.0,
            scrub_frame_t: 0.0,
            dragging_playhead: false,
            last_scrub_extract: None,
            duration_choice: 0,
            backdrop: Default::default(),
            frame_log: std::env::var_os("FRAMEFANG_FRAME_LOG").map(|_| Vec::new()),
            last_anim_frame: None,
            select_mode: false,
            selected_idx: None,
            move_last: (0, 0),
            move_total: (0.0, 0.0),
            pending_discard: None,
            last_selection: None,
            windows: Vec::new(),
            pick_mode: crate::pick::PickMode::Auto,
            target: None,
            glide_from: (0, 0, 0, 0),
            glide_at: std::time::Instant::now(),
            pick_pressed: false,
            color_pick: false,
            wants_screen_color: false,
        })
    }

    /// Hand the overlay the capture's logical/physical mapping. Separate from
    /// `new` so the constructor's signature stays put; called right after the
    /// window opens. A map whose physical size doesn't match the bitmap we
    /// were handed is ignored — the 1x identity the constructor installed is
    /// always consistent with the frame, and a mismatched map would move the
    /// crop instead of correcting it.
    pub fn set_display_map(&mut self, map: DisplayMap) {
        if map.physical_size() == (self.screenshot.width(), self.screenshot.height()) {
            self.display = map;
        }
    }

    /// Device pixels per logical desktop unit for the captured frame.
    fn display_scale(&self) -> f32 { self.display.scale() }

    /// Read-only view of the editor's live settings — used by `tray_loop`
    /// after the overlay closes to pull back any per-tool slider values
    /// (currently just `marker_opacity`) the user changed mid-session.
    pub fn settings(&self) -> &AppSettings { &self.settings }

    /// Off screen while a Save As dialog is up, and back with its picture
    /// and marks if the user cancels it.
    pub fn set_hidden(&mut self, hidden: bool) {
        self.window.set_visible(!hidden);
        if !hidden {
            // Keys let go while the dialog had the keyboard never reached
            // this window, so Shift or Ctrl could otherwise read as held.
            self.mods = ModifiersState::empty();
            self.hover_tip = None;
            self.window.focus_window();
            self.window.request_redraw();
        }
    }

    /// Where a notice `size` pixels big goes once this window closes, in
    /// virtual-screen pixels: centered under the selection, or just inside
    /// its bottom edge when the screen ends there, and always on the
    /// selection's monitor.
    pub fn notice_spot(&self, size: (u32, u32)) -> Option<(i32, i32)> {
        const GAP: i32 = 12;
        let sel = self.selection?;
        let (bx, by, bw, bh) = self.panel_bounds(sel);
        let (w, h) = (size.0 as i32, size.1 as i32);
        let x = (sel.0 + (sel.2 - w) / 2).clamp(bx + GAP, (bx + bw - w - GAP).max(bx + GAP));
        let below = sel.1 + sel.3 + GAP;
        let y = if below + h <= by + bh - GAP { below } else { sel.1 + sel.3 - h - GAP };
        let y = y.clamp(by + GAP, (by + bh - h - GAP).max(by + GAP));
        let origin = if self.editor_mode {
            self.window.inner_position().map(|p| (p.x, p.y)).unwrap_or(self.frame_origin)
        } else { self.frame_origin };
        Some(vdesk::frame_to_virtual(origin, (x, y)))
    }

    // ── tool flyout ────────────────────────────────────────────────────
    //
    // Picking a tool slides its settings out beside its button: size (and
    // opacity for Marker, text size for Text, block size for Pixelate) and
    // its own colors. Picking the same tool again tucks them away. Each tool
    // keeps its own color and width (`settings.tool_styles`).

    /// Pick `t` from its button or its key: switch to its own color and
    /// width, and open its flyout, or close it if it is already open.
    fn pick_tool(&mut self, t: Tool) {
        // The editor window's tools are toggles: the armed one, picked again,
        // goes back down and the picture moves the window again.
        if self.editor_mode && self.tool_armed && t == self.tool {
            self.put_tool_down();
            return;
        }
        self.tool_armed = true;
        let reopen = self.flyout != Some(t);
        if t != self.tool {
            self.stroke = stroke_for(&self.settings, t);
            self.tool = t;
            self.show_palette_of_color();
            // The next capture opens on this tool.
            self.settings.last_tool = t.name().to_owned();
            if let Err(e) = self.settings.save() {
                eprintln!("framefang: tool choice save failed: {e}");
            }
        }
        self.leave_select_mode();
        self.flyout = reopen.then_some(t);
        if reopen { self.flyout_opened = std::time::Instant::now(); }
        self.stop_picking();
        // The cursor has not moved, so the tip computed before the click
        // would stay up and sit on top of the flyout.
        self.hover_tip = self.compute_hover_tip();
        self.window.request_redraw();
    }

    /// No tool in hand (editor window): its settings tuck away, and a drag on
    /// the picture moves the window.
    fn put_tool_down(&mut self) {
        self.tool_armed = false;
        self.flyout = None;
        self.picking_color = false;
        self.leave_select_mode();
        self.hover_tip = self.compute_hover_tip();
        self.update_resize_cursor();
        self.window.request_redraw();
    }

    /// Flip the flyout to the palette holding the tool's color, so its swatch
    /// is the one ringed. Stays put when the color is on the shown palette
    /// already, or on none.
    fn show_palette_of_color(&mut self) {
        let pals = &framefang_core::annotation::Palettes::ALL;
        if pals[self.palette_index.min(pals.len() - 1)].colors.contains(&self.stroke.color) { return; }
        if let Some(i) = pals.iter().position(|p| p.colors.contains(&self.stroke.color)) {
            self.palette_index = i;
        }
    }

    /// The open flyout: its rectangle, which side of the tool panel it hangs
    /// off (`true` = right), and its tool button's center as a y offset from
    /// the flyout's top (for the pointer). `None` when none is open.
    fn flyout_rect(&self) -> Option<(Rect4, bool, i32)> {
        let tool = self.flyout?;
        if self.record_mode { return None; }
        let sel = self.selection?;
        let lb = self.panel_bounds(sel);
        let (tp_x, tp_y) = tool_panel_origin(lb, sel);
        let (tp_w, _) = tool_panel_dims();
        let idx = TOOL_PANEL_BUTTONS.iter().position(|b| *b == ToolPanelButton::Tool(tool))? as i32;
        let (_, by0, _, by1) = tool_panel_button_rect((tp_x, tp_y), idx);
        let layout = flyout_layout(tool);
        let (w, h) = (layout.w, layout.h);
        let on_right = tp_x >= sel.0 + sel.2;
        let x = if on_right { tp_x + tp_w + FLYOUT_GAP } else { tp_x - FLYOUT_GAP - w };
        // Centered on its button, kept inside the monitor.
        let (_, ly, _, lh) = lb;
        let mid = (by0 + by1) / 2;
        let y = (mid - h / 2).clamp(ly, (ly + lh - h).max(ly));
        Some(((x, y, w, h), on_right, mid - y))
    }

    /// The knob row under the cursor, if the cursor is on the open flyout.
    fn knob_at_cursor(&self) -> Option<(Knob, Rect4)> {
        let ((fx, fy, fw, fh), _, _) = self.flyout_rect()?;
        if !rect_contains((fx, fy, fw, fh), self.cursor) { return None; }
        flyout_layout(self.flyout?).knobs.into_iter()
            .map(|(k, r)| (k, offset(r, (fx, fy))))
            .find(|(_, r)| rect_contains(*r, self.cursor))
    }

    /// A press on the flyout's color section: a swatch sets the tool's
    /// color, an arrow flips the palette. `true` when it was one of those.
    fn press_flyout_colors(&mut self) -> bool {
        let Some(((fx, fy, _, _), _, _)) = self.flyout_rect() else { return false; };
        let Some(layout) = self.flyout.map(flyout_layout) else { return false; };
        let (Some((prev, next, _, swatches)), Some(custom)) = (layout.colors, layout.custom) else { return false; };
        let n = framefang_core::annotation::Palettes::ALL.len();
        let hit = |r: Rect4| rect_contains(offset(r, (fx, fy)), self.cursor);
        if hit(custom.eyedropper) {
            self.picking_color = !self.picking_color;
            // The editor window holds only its picture; to pick from the
            // whole screen it asks for a color-pick session over it.
            self.wants_screen_color = self.picking_color && self.editor_mode;
            self.hover_tip = None;
            self.update_resize_cursor();
            self.window.request_redraw();
            return true;
        }
        if hit(custom.spectrum) {
            self.dragging_spectrum = true;
            self.set_color_from_spectrum();
            return true;
        }
        if hit(prev) || hit(next) {
            self.palette_index = if hit(prev) { (self.palette_index + n - 1) % n } else { (self.palette_index + 1) % n };
            self.settings.palette_index = self.palette_index as i32;
        } else if let Some(i) = swatches.iter().position(|r| hit(*r)) {
            self.set_color(framefang_core::annotation::Palettes::get(self.palette_index).colors[i]);
        } else {
            return false;
        }
        self.save_knobs();
        self.window.request_redraw();
        true
    }

    /// The tool draws in `c` from now on, the text being typed included.
    /// Picking any color puts the eyedropper back down.
    fn set_color(&mut self, c: framefang_core::color::Rgba) {
        self.stroke.color = c;
        if let Some(a) = self.current.as_mut() {
            if let framefang_core::annotation::AnnotationKind::Text { color, .. } = &mut a.kind {
                *color = c;
            }
        }
        self.stop_picking();
        self.window.request_redraw();
    }

    /// The color under the cursor in the spectrum, while it is pressed or
    /// dragged. Saved when the button comes up.
    fn set_color_from_spectrum(&mut self) {
        let Some(((fx, fy, _, _), _, _)) = self.flyout_rect() else { return; };
        let Some(custom) = self.flyout.and_then(|t| flyout_layout(t).custom) else { return; };
        let (sx, sy, sw, sh) = offset(custom.spectrum, (fx, fy));
        let u = (self.cursor.0 - sx) as f32 / (sw - 1).max(1) as f32;
        let v = (self.cursor.1 - sy) as f32 / (sh - 1).max(1) as f32;
        let (u, v) = (u.clamp(0.0, 1.0), v.clamp(0.0, 1.0));
        self.spectrum_pick = Some((u, v));
        self.set_color(spectrum_color(u, v));
    }

    /// Put the eyedropper down without picking anything.
    fn stop_picking(&mut self) {
        if !self.picking_color { return; }
        self.picking_color = false;
        self.update_resize_cursor();
        self.window.request_redraw();
    }

    /// The eyedropper's click: the tool takes the color of the pixel under
    /// the cursor. In the editor window only the picture counts; the canvas
    /// around it is not part of the capture.
    fn pick_color_at_cursor(&mut self) {
        if self.editor_mode && !self.cursor_in_selection() { return; }
        let Some(c) = self.color_at_cursor() else { return; };
        self.spectrum_pick = None;
        self.set_color(c);
        self.show_palette_of_color();
        self.save_knobs();
    }

    /// The captured pixel under the cursor, opaque.
    fn color_at_cursor(&self) -> Option<framefang_core::color::Rgba> {
        pixel_at(&self.screenshot, self.cursor, self.bitmap_offset())
    }

    fn knob_value(&self, k: Knob) -> f32 {
        match k {
            Knob::Thickness     => self.stroke.thickness,
            Knob::MarkerOpacity => self.settings.marker_opacity as f32,
            Knob::TextSize      => self.text_size,
            Knob::PixelSize     => self.pixel_size as f32,
        }
    }

    /// Set the dragged knob from the cursor's X inside its track.
    fn set_knob_from_cursor(&mut self, k: Knob) {
        let Some(((fx, fy, _, _), _, _)) = self.flyout_rect() else { return; };
        let Some(row) = self.flyout.and_then(|t| flyout_layout(t).knobs.into_iter().find(|(x, _)| *x == k))
            else { return; };
        let (tx, _, tw, _) = flyout_track(offset(row.1, (fx, fy)));
        if tw <= 1 { return; }
        let t = (self.cursor.0 - tx) as f32 / (tw - 1) as f32;
        let t = if t.is_finite() { t.clamp(0.0, 1.0) } else { 0.0 };
        let (lo, hi) = k.range();
        let v = (lo + t * (hi - lo)).round();
        if v == self.knob_value(k) { return; }
        match k {
            Knob::Thickness     => self.stroke.thickness = v,
            Knob::MarkerOpacity => self.settings.marker_opacity = v as u8,
            Knob::TextSize      => {
                self.text_size = v;
                // Resize the text being typed, so the slider previews on it.
                if let Some(Annotation { kind: framefang_core::annotation::AnnotationKind::Text { font_size, .. }, .. })
                    = self.current.as_mut() { *font_size = v; }
            }
            Knob::PixelSize     => self.pixel_size = v as u32,
        }
        self.window.request_redraw();
    }

    /// The current tool's style and every knob into settings, saved to
    /// disk. Called when a knob drag ends or a color is picked, so the next
    /// capture opens with the same values.
    fn save_knobs(&mut self) {
        let style = framefang_core::settings::ToolStyle {
            color_argb: self.stroke.color.to_argb(),
            thickness: self.stroke.thickness,
        };
        self.settings.set_tool_style(self.tool, style);
        self.settings.last_text_size = self.text_size;
        self.settings.pixelate_block = self.pixel_size;
        if let Err(e) = self.settings.save() {
            eprintln!("framefang: tool settings save failed: {e}");
        }
    }

    /// Snap `scrub_pos` to the cursor's X inside the timeline track,
    /// clamped to [0, duration]. `force_extract` bypasses the mid-drag
    /// throttle so press and release always land the background on the
    /// exact frame; mid-drag swaps are rate-limited because each one is
    /// a synchronous ffmpeg spawn.
    fn set_scrub_from_cursor(&mut self, force_extract: bool) {
        let Some(dur) = self.duration else { return; };
        let bar = timeline_bar_rect(self.chrome_bounds(self.cursor));
        let (tx, _ty, tw, _th) = timeline_track(bar, dur);
        if tw <= 1 { return; }
        let mut t = (self.cursor.0 - tx) as f32 / (tw - 1) as f32;
        if !t.is_finite() { t = 0.0; }
        self.scrub_pos = t.clamp(0.0, 1.0) * dur;
        let throttle_ok = self.last_scrub_extract
            .map_or(true, |i| i.elapsed() >= std::time::Duration::from_millis(200));
        if force_extract || throttle_ok {
            self.swap_scrub_frame();
        }
        self.window.request_redraw();
    }

    /// Replace the background with the frame at the current scrub
    /// position. Failures keep the previous frame — a stale background
    /// beats killing the session mid-drag.
    fn swap_scrub_frame(&mut self) {
        let (Some(dur), Some(video)) = (self.duration, self.video_path.as_deref()) else { return; };
        self.last_scrub_extract = Some(std::time::Instant::now());
        let t = clamp_extract_t(self.scrub_pos, dur);
        if t == self.scrub_frame_t { return; }
        match crate::tray_loop::extract_frame_at(video, t) {
            Ok(frame) => {
                self.screenshot    = frame;
                self.scrub_frame_t = t;
                // The dimmed composite is built from the old frame.
                self.backdrop.invalidate();
            }
            Err(e) => eprintln!("framefang: scrub frame extract failed: {e}"),
        }
    }

    pub fn window_id(&self) -> WindowId { self.window.id() }

    // ── virtual-desktop coordinate mapping ─────────────────────────────
    //
    // Mouse events, annotations and every painter call are in *frame*
    // (framebuffer) space. The capture is in *bitmap* space, and the pin
    // window wants *virtual-screen* space. `framefang_core::virtual_desktop`
    // owns the arithmetic; these four helpers feed it the overlay's state.

    /// Re-derive the framebuffer's virtual-screen origin from the window.
    ///
    /// Called at the top of every redraw. When the window manager honours
    /// the requested placement this is a no-op — but when it doesn't (macOS
    /// with separate Spaces per display, a WM that refuses negative
    /// positions), it is what keeps the crop, the pin position and the
    /// magnifier pointed at the pixels the user is actually looking at.
    /// Skipped for non-capture bitmaps (video frames), which are drawn at
    /// framebuffer (0, 0) no matter which monitor the window opens on.
    fn sync_frame_origin(&mut self) {
        if self.desktop.monitors().is_empty() { return; }
        // Wayland refuses to report window position at all; the fallback
        // there is the origin we asked for, same as before.
        let pos = self.window.inner_position().or_else(|_| self.window.outer_position());
        let Ok(p) = pos else { return; };
        if (p.x, p.y) != self.frame_origin {
            self.frame_origin = (p.x, p.y);
            // Pass 1 samples the capture through this offset.
            self.backdrop.invalidate();
        }
        if self.frame_origin != self.desktop.origin() && !self.placement_logged {
            self.placement_logged = true;
            let (ox, oy) = self.desktop.origin();
            eprintln!(
                "framefang: overlay placed at {},{} but the desktop starts at {ox},{oy} — \
                 mapping selections through the difference",
                self.frame_origin.0, self.frame_origin.1
            );
        }
    }

    /// Offset of framebuffer pixel (0, 0) inside the captured bitmap.
    fn bitmap_offset(&self) -> (i32, i32) {
        vdesk::bitmap_offset(self.desktop.origin(), self.frame_origin)
    }

    /// The selection as a crop rect in the captured bitmap.
    fn selection_in_bitmap(&self) -> Option<(i32, i32, i32, i32)> {
        Some(vdesk::frame_rect_to_bitmap(
            self.desktop.origin(), self.frame_origin, self.selection?))
    }

    /// Framebuffer-space rect the floating chrome lays itself out inside:
    /// the monitor under `point`, clipped to the framebuffer. Keeps the
    /// tool / action panels, the magnifier and the hint chip on the monitor
    /// the user is working on instead of letting them flip onto the one
    /// next door, which is where clamping against the whole virtual desktop
    /// would put them.
    fn chrome_bounds(&self, point: (i32, i32)) -> (i32, i32, i32, i32) {
        let size = self.window.inner_size();
        vdesk::monitor_bounds_in_frame(
            self.desktop.monitors(), self.frame_origin, (size.width, size.height), point)
    }

    /// Chrome bounds for the widgets anchored to the selection: the monitor
    /// holding the selection's center.
    fn panel_bounds(&self, sel: (i32, i32, i32, i32)) -> (i32, i32, i32, i32) {
        self.chrome_bounds((sel.0 + sel.2 / 2, sel.1 + sel.3 / 2))
    }

    /// The action row for this session. Three shapes, one panel — see the
    /// button-list constants for why each session drops what it drops.
    fn action_buttons(&self) -> &'static [ActionButton] {
        // The editor window has no Record: its picture is a still, and the
        // screen region it came from is live again.
        if self.record_mode      { &RECORD_ACTION_BUTTONS }
        else if self.locked()    { &VIDEO_ACTION_BUTTONS  }
        else                     { &ACTION_BUTTONS        }
    }

    pub fn handle_event(&mut self, event: WindowEvent) -> OverlayOutcome {
        match event {
            WindowEvent::CloseRequested => self.request_close(),

            WindowEvent::ModifiersChanged(m) => {
                self.mods = m.state();
                OverlayOutcome::Continue
            }

            WindowEvent::KeyboardInput {
                event: KeyEvent { logical_key, text, state: ElementState::Pressed, .. }, ..
            } => self.handle_key_with_text(logical_key, text),

            // IME: dead keys, compose sequences, and full input methods all
            // arrive here rather than as `KeyEvent::text`. Only meaningful
            // while typing — `set_text_input_active` turns the IME on with
            // the Text tool and off again, so tool shortcuts stay single
            // keypresses the rest of the time.
            WindowEvent::Ime(ime) => self.handle_ime(ime),

            WindowEvent::CursorMoved { position: PhysicalPosition { x, y }, .. } => {
                self.cursor = (x as i32, y as i32);
                // Marker opacity drag takes priority over per-state cursor
                // updates so the slider tracks the cursor smoothly even
                // when the user drags well outside the slider's own rect.
                if let Some(k) = self.dragging_knob {
                    self.set_knob_from_cursor(k);
                    return OverlayOutcome::Continue;
                }
                if self.dragging_spectrum {
                    self.set_color_from_spectrum();
                    return OverlayOutcome::Continue;
                }
                if self.dragging_playhead {
                    self.set_scrub_from_cursor(false);
                    return OverlayOutcome::Continue;
                }
                match self.state {
                    State::Selecting => {
                        self.selection = Some(rect_from(self.anchor, self.cursor));
                        self.window.request_redraw();
                    }
                    State::Drawing => {
                        if let Some(a) = self.current.as_mut() {
                            a.extend(Point2::new(x as f32, y as f32));
                            self.window.request_redraw();
                        }
                    }
                    State::Resizing => {
                        self.apply_resize();
                        self.window.request_redraw();
                    }
                    State::MovingAnnotation => {
                        if let Some(idx) = self.selected_idx {
                            let dx = (self.cursor.0 - self.move_last.0) as f32;
                            let dy = (self.cursor.1 - self.move_last.1) as f32;
                            if let Some(a) = self.annotations.get_mut(idx) {
                                edit::translate(a, dx, dy);
                            }
                            self.move_total.0 += dx;
                            self.move_total.1 += dy;
                        }
                        self.move_last = self.cursor;
                        self.window.request_redraw();
                    }
                    State::Selected => {
                        // Update the cursor icon based on which edge we're
                        // hovering, matching the C# OverlayForm convention.
                        self.update_resize_cursor();
                        // And recompute the hover tooltip so the user can
                        // tell Pen / Line / Marker apart instantly. Mirrors
                        // C# MakeButton(tip, …) tooltip text.
                        let prev = self.hover_tip;
                        self.hover_tip = self.compute_hover_tip();
                        // The eyedropper's magnifier follows the cursor.
                        if prev != self.hover_tip || self.picking_color { self.window.request_redraw(); }
                    }
                    State::Idle => {
                        // Magnifier follows the cursor in Idle so the user
                        // can place the first selection edge by pixel, and
                        // the highlight follows what a click would take.
                        self.update_target();
                        let on_bar = self.has_hover() && rect_contains(self.mode_bar().0, self.cursor);
                        self.window.set_cursor(if on_bar { CursorIcon::Pointer } else { CursorIcon::Crosshair });
                        self.window.request_redraw();
                    }
                    _ => {}
                }
                OverlayOutcome::Continue
            }

            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. } => {
                self.handle_left_press()
            }

            WindowEvent::MouseInput { state: ElementState::Released, button: MouseButton::Left, .. } => {
                self.handle_left_release()
            }

            // Right-click commits the current selection (matches the
            // OverlayForm gesture for "save this region right now").
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Right, .. } => {
                if self.color_pick { return OverlayOutcome::Cancelled; }
                if self.state == State::Drawing {
                    // Cancel the in-progress annotation, mirroring C# OverlayForm.
                    self.current = None;
                    self.state   = State::Selected;
                    self.window.request_redraw();
                    OverlayOutcome::Continue
                } else {
                    self.commit()
                }
            }

            // The pointer left the window (in the cut-to-shape editor, any
            // move off the picture and panels): a tooltip would otherwise
            // stay up over a button nobody is pointing at.
            WindowEvent::CursorLeft { .. } => {
                if self.hover_tip.take().is_some() { self.window.request_redraw(); }
                OverlayOutcome::Continue
            }

            WindowEvent::Resized(_) => {
                self.window.request_redraw();
                OverlayOutcome::Continue
            }

            WindowEvent::RedrawRequested => {
                self.redraw();
                OverlayOutcome::Continue
            }

            _ => OverlayOutcome::Continue,
        }
    }

    /// Route a keypress, preferring the platform's composed `text` over the
    /// logical key while the Text tool is capturing input.
    ///
    /// `KeyEvent::text` is what the OS decided this keystroke produces after
    /// layout, dead keys and modifiers — "é" from `'` + `e`, "ł" from AltGr+L,
    /// "Ω" from a Greek layout. `logical_key` is only the key's own identity,
    /// which is why the old ASCII path could never see those. Control
    /// characters (Enter, Tab, Backspace, Esc all arrive with a `text` of
    /// their own) are filtered out and fall through to `handle_key`, which
    /// keeps owning every command key.
    fn handle_key_with_text(&mut self, key: Key, text: Option<SmolStr>) -> OverlayOutcome {
        if self.state == State::TextInput && !self.mods.control_key() {
            if let Some(raw) = text.as_deref() {
                let typed = framefang_core::text::sanitize_input(raw);
                // Newlines only ever come from Enter, which `handle_key`
                // treats as "commit"; leave that decision there.
                if !typed.is_empty() && !typed.contains('\n') {
                    self.insert_text(&typed);
                    return OverlayOutcome::Continue;
                }
            }
        }
        self.handle_key(key)
    }

    /// Append user-typed text to the pending Text annotation.
    fn insert_text(&mut self, typed: &str) {
        use framefang_core::annotation::AnnotationKind;
        if typed.is_empty() { return; }
        if let Some(a) = self.current.as_mut() {
            if let AnnotationKind::Text { ref mut text, .. } = a.kind {
                text.push_str(typed);
                self.window.request_redraw();
            }
        }
    }

    /// Turn the window's IME on/off around a text-input session and clear any
    /// half-composed preedit. Enabling it unconditionally would let a CJK IME
    /// swallow the single-letter tool shortcuts, so it is scoped to typing.
    fn set_text_input_active(&mut self, active: bool) {
        self.ime_preedit.clear();
        self.window.set_ime_allowed(active);
    }

    fn handle_ime(&mut self, ime: Ime) -> OverlayOutcome {
        if self.state != State::TextInput {
            return OverlayOutcome::Continue;
        }
        match ime {
            // Committed text is final — append it exactly as the IME built it.
            Ime::Commit(s) => {
                self.ime_preedit.clear();
                let typed = framefang_core::text::sanitize_input(&s);
                self.insert_text(&typed);
            }
            // In-flight composition: shown after the caret, never stored in
            // the annotation, discarded if the user cancels.
            Ime::Preedit(s, _) => {
                self.ime_preedit = framefang_core::text::sanitize_input(&s);
                self.window.request_redraw();
            }
            Ime::Enabled | Ime::Disabled => {
                self.ime_preedit.clear();
                self.window.request_redraw();
            }
        }
        OverlayOutcome::Continue
    }

    fn handle_key(&mut self, key: Key) -> OverlayOutcome {
        eprintln!("framefang: key={:?} state={:?} mods={:?}", key, self.state, self.mods);
        // A color-pick session knows one key: Esc backs out of it.
        if self.color_pick {
            return if key == Key::Named(NamedKey::Escape) { OverlayOutcome::Cancelled } else { OverlayOutcome::Continue };
        }
        // Text-input state owns the keyboard while it's active — typed
        // characters extend the pending annotation; Enter commits, Esc
        // cancels, Backspace pops the last char.
        if self.state == State::TextInput {
            return self.handle_text_key(key);
        }
        // An armed Esc confirmation owns the keyboard: only a second Esc
        // goes through with the discard, anything else calls it off. Enter
        // deliberately does *not* confirm — everywhere else in the overlay
        // Enter means save/commit, so a mis-hit Esc followed by the Enter
        // the user meant all along must never throw their work away. Bare
        // modifier presses are ignored so reaching for Ctrl doesn't count
        // as an answer.
        if self.pending_discard.is_some() && !is_modifier_key(&key) {
            return match key {
                Key::Named(NamedKey::Escape) => self.confirm_discard(),
                _ => {
                    self.pending_discard = None;
                    self.window.request_redraw();
                    OverlayOutcome::Continue
                }
            };
        }
        // A drag in flight has already translated the annotation on screen
        // but not yet logged the `EditOp::Move` (that happens on release).
        // Delete / undo / redo all clear `selected_idx`, which would make the
        // release drop the record and leave the canvas out of step with the
        // undo log — so settle the drag first, as if the button came up.
        if self.state == State::MovingAnnotation
            && (matches!(key, Key::Named(NamedKey::Delete) | Key::Named(NamedKey::Backspace))
                || (matches!(key, Key::Character(_)) && self.mods.control_key()))
        {
            self.finish_annotation_drag();
        }
        match key {
            Key::Named(NamedKey::Escape) => self.handle_escape(),
            Key::Named(NamedKey::Enter) => self.commit(),
            // Tab: the next capture mode, before any region exists.
            Key::Named(NamedKey::Tab) if self.state == State::Idle && self.has_hover() => {
                let next = self.pick_mode.next(self.screens());
                self.set_pick_mode(next);
                OverlayOutcome::Continue
            }
            // Select mode: Delete / Backspace remove the picked annotation.
            // Both are undoable; outside select mode neither key does
            // anything, exactly as before.
            Key::Named(NamedKey::Delete) | Key::Named(NamedKey::Backspace) => {
                self.delete_selected_annotation();
                OverlayOutcome::Continue
            }
            Key::Character(s) => {
                // Ctrl+A picks the whole screen, in record mode too.
                if self.mods.control_key() && s.eq_ignore_ascii_case("a") {
                    return self.select_all();
                }
                // Record mode has no tools to switch to and nothing to save,
                // copy or pin, so every character shortcut would be a lie.
                if self.record_mode {
                    return OverlayOutcome::Continue;
                }
                let c    = match s.chars().next() { Some(c) => c, None => return OverlayOutcome::Continue };
                let ctrl = self.mods.control_key();
                let shift = self.mods.shift_key();
                let lc    = c.to_ascii_lowercase();
                // Undo / redo stay live even with no selection on screen —
                // that is precisely the state a confirmed Esc leaves behind,
                // and Ctrl+Z is the way back from it.
                if ctrl && (lc == 'z' || lc == 'y') {
                    // Ctrl+Z → undo, Ctrl+Y / Ctrl+Shift+Z → redo.
                    if lc == 'y' || shift { self.redo(); } else { self.undo(); }
                    return OverlayOutcome::Continue;
                }
                if self.state != State::Selected {
                    return OverlayOutcome::Continue;
                }
                if ctrl {
                    match lc {
                        // Ctrl+Shift+S → save under a name and format of
                        // the user's choosing
                        's' if shift => return self.commit_as_save_as(),
                        // Ctrl+S → commit-and-save (same as Enter)
                        's' => return self.commit(),
                        // Ctrl+C → commit-and-copy
                        'c' => return self.commit_as_copy(),
                        // Ctrl+P → commit-and-pin (float bitmap on screen)
                        'p' => return self.commit_as_pin(),
                        _ => {}
                    }
                } else if lc == 's' {
                    // S → select mode. No tool letter uses it (see
                    // `Tool::shortcut`), so nothing is shadowed.
                    self.select_mode = !self.select_mode;
                    if !self.select_mode { self.selected_idx = None; }
                    self.window.request_redraw();
                } else if let Some(t) = Tool::from_key(c) {
                    // Same as its button: leaves select mode (otherwise the
                    // next click would move ink instead of drawing) and
                    // slides its settings out.
                    self.pick_tool(t);
                }
                OverlayOutcome::Continue
            }
            _ => OverlayOutcome::Continue,
        }
    }

    /// Esc, outside text input. Ordered cheapest-cancel first: an
    /// in-progress stroke, then the select-mode highlight, then the mode
    /// itself, and only after all of those anything that would cost the
    /// user work — which never happens on this press, it only arms the
    /// confirmation bar.
    fn handle_escape(&mut self) -> OverlayOutcome {
        if self.picking_color {
            self.stop_picking();
            return OverlayOutcome::Continue;
        }
        if self.state == State::Drawing {
            self.current = None;
            self.state   = State::Selected;
            self.window.request_redraw();
            return OverlayOutcome::Continue;
        }
        if self.state == State::MovingAnnotation {
            // Abort the drag by putting the annotation back where it was.
            // Nothing was recorded yet, so there is nothing to undo either.
            if let Some(idx) = self.selected_idx {
                if let Some(a) = self.annotations.get_mut(idx) {
                    edit::translate(a, -self.move_total.0, -self.move_total.1);
                }
            }
            self.move_total = (0.0, 0.0);
            self.state      = State::Selected;
            self.window.request_redraw();
            return OverlayOutcome::Continue;
        }
        if self.selected_idx.is_some() {
            self.selected_idx = None;
            self.window.request_redraw();
            return OverlayOutcome::Continue;
        }
        if self.select_mode {
            self.select_mode = false;
            self.window.request_redraw();
            return OverlayOutcome::Continue;
        }
        // Editor window with a tool in hand: Esc puts it down first.
        if self.editor_mode && self.tool_armed && self.state == State::Selected {
            self.put_tool_down();
            return OverlayOutcome::Continue;
        }
        // Screenshot mode with a live selection: Esc clears the region and
        // the ink drawn in it. Video mode has no "no selection" state to
        // fall back to — the frame IS the selection — so there Esc closes.
        if self.state == State::Selected && !self.locked() {
            if self.annotations.is_empty() {
                // Nothing to lose — clear straight away, as before.
                return self.confirm_discard_kind(PendingDiscard::ClearSelection);
            }
            self.pending_discard = Some(PendingDiscard::ClearSelection);
            self.window.request_redraw();
            return OverlayOutcome::Continue;
        }
        if self.has_unsaved_work() {
            self.pending_discard = Some(PendingDiscard::CloseOverlay);
            self.window.request_redraw();
            return OverlayOutcome::Continue;
        }
        OverlayOutcome::Cancelled
    }

    /// Go through with whatever the confirmation bar is asking about.
    fn confirm_discard(&mut self) -> OverlayOutcome {
        match self.pending_discard.take() {
            Some(kind) => self.confirm_discard_kind(kind),
            None       => OverlayOutcome::Continue,
        }
    }

    fn confirm_discard_kind(&mut self, kind: PendingDiscard) -> OverlayOutcome {
        self.pending_discard = None;
        match kind {
            PendingDiscard::ClearSelection => {
                self.discard_annotations_recoverably();
                self.state        = State::Idle;
                self.selection    = None;
                self.step_count   = 1;
                self.selected_idx = None;
                self.window.request_redraw();
                OverlayOutcome::Continue
            }
            PendingDiscard::CloseOverlay => OverlayOutcome::Cancelled,
        }
    }

    /// Is there annotation work in this session that closing would destroy?
    /// Counts the canvas, an in-progress annotation, and anything sitting
    /// in the undo/redo log — a cleared canvas is still one Ctrl+Z away.
    fn has_unsaved_work(&self) -> bool {
        !self.annotations.is_empty()
            || self.current.is_some()
            || self.history.holds_recoverable_work()
    }

    /// The Close button and the WM's close request both land here: they
    /// ask, they never destroy. With nothing to lose the overlay closes
    /// immediately, exactly as before.
    fn request_close(&mut self) -> OverlayOutcome {
        if self.has_unsaved_work() {
            self.pending_discard = Some(PendingDiscard::CloseOverlay);
            self.window.request_redraw();
            return OverlayOutcome::Continue;
        }
        OverlayOutcome::Cancelled
    }

    /// Leave select mode and drop any annotation highlight.
    fn leave_select_mode(&mut self) {
        self.select_mode  = false;
        self.selected_idx = None;
    }

    /// Index of the topmost annotation under the cursor. Video mode only
    /// considers ink that is actually visible at the scrub position — the
    /// user can't click what they can't see.
    fn hit_annotation_at_cursor(&self) -> Option<usize> {
        let p = Point2::new(self.cursor.0 as f32, self.cursor.1 as f32);
        if self.video_mode {
            self.annotations.iter().enumerate().rev()
                .find(|(_, a)| annotation_visible_at(a, self.scrub_pos) && edit::hit_test(a, p))
                .map(|(i, _)| i)
        } else {
            edit::hit_test_topmost(&self.annotations, p)
        }
    }

    /// End a select-mode drag: one `EditOp::Move` per drag, not per
    /// mouse-move event, so a single Ctrl+Z puts the annotation back where
    /// it started. Called on mouse-up and before any key that would
    /// otherwise clear the selection mid-drag.
    fn finish_annotation_drag(&mut self) {
        self.state = State::Selected;
        if let Some(idx) = self.selected_idx {
            if self.move_total != (0.0, 0.0) {
                self.history.record(EditOp::Move {
                    index: idx,
                    dx:    self.move_total.0,
                    dy:    self.move_total.1,
                });
            }
        }
        self.move_total = (0.0, 0.0);
        self.window.request_redraw();
    }

    /// Remove the annotation picked in select mode, recording it so both
    /// Ctrl+Z and Ctrl+Y replay the removal.
    fn delete_selected_annotation(&mut self) {
        let Some(idx) = self.selected_idx else { return; };
        if idx >= self.annotations.len() { self.selected_idx = None; return; }
        let removed = self.annotations.remove(idx);
        self.history.record(EditOp::Delete { index: idx, annotation: removed });
        self.selected_idx = None;
        self.step_count   = edit::next_step_number(&self.annotations);
        self.window.request_redraw();
    }

    fn handle_text_key(&mut self, key: Key) -> OverlayOutcome {
        use framefang_core::annotation::AnnotationKind;
        eprintln!("framefang: text-input key={:?}", key);
        match key {
            Key::Named(NamedKey::Escape) => {
                self.current = None;
                self.state   = State::Selected;
                self.set_text_input_active(false);
                self.window.request_redraw();
            }
            // Shift+Enter opens a second line; plain Enter commits.
            Key::Named(NamedKey::Enter) if self.mods.shift_key() => {
                self.insert_text("\n");
            }
            Key::Named(NamedKey::Enter) => {
                // Commit only if the user actually typed something.
                if let Some(a) = self.current.take() {
                    if let AnnotationKind::Text { ref text, .. } = a.kind {
                        if !text.trim().is_empty() {
                            self.add_annotation(a);
                        }
                    }
                }
                self.state = State::Selected;
                self.set_text_input_active(false);
                self.window.request_redraw();
            }
            Key::Named(NamedKey::Backspace) => {
                // A half-composed IME preedit is cancelled by the IME itself;
                // here Backspace removes one *visible* character, which on
                // combining or ZWJ sequences is several `char`s at once.
                if let Some(a) = self.current.as_mut() {
                    if let AnnotationKind::Text { ref mut text, .. } = a.kind {
                        framefang_core::text::pop_grapheme(text);
                        self.window.request_redraw();
                    }
                }
            }
            // Fallbacks for platforms/layouts where `KeyEvent::text` is
            // absent — `handle_key_with_text` handles the normal case.
            Key::Named(NamedKey::Space) => self.insert_text(" "),
            Key::Character(s) => {
                // Skip Ctrl-modified characters so Ctrl+Z / Ctrl+S etc. don't
                // get swallowed as plain text input.
                if self.mods.control_key() { return OverlayOutcome::Continue; }
                let typed = framefang_core::text::sanitize_input(&s);
                self.insert_text(&typed);
            }
            _ => {}
        }
        OverlayOutcome::Continue
    }

    fn undo(&mut self) {
        let Some(op) = self.history.undo(&mut self.annotations) else { return; };
        self.after_history_op();
        // Undoing the clear that Esc (or a click outside the region) did
        // brings the selection back with the ink, so the user lands exactly
        // where they were instead of having to re-drag a region first.
        if matches!(op, EditOp::Clear { .. })
            && self.state == State::Idle
            && self.selection.is_none()
            && !self.annotations.is_empty()
        {
            if let Some(rect) = self.last_selection {
                self.selection = Some(rect);
                self.state     = State::Selected;
            }
        }
    }

    fn redo(&mut self) {
        if self.history.redo(&mut self.annotations).is_some() {
            self.after_history_op();
        }
    }

    /// Shared bookkeeping after any undo/redo: step numbering follows the
    /// markers actually on the canvas, and the select-mode highlight is
    /// dropped because indices shift under inserts and removals.
    fn after_history_op(&mut self) {
        self.step_count   = edit::next_step_number(&self.annotations);
        self.selected_idx = None;
        self.window.request_redraw();
    }

    /// Drop the committed annotations off the canvas without destroying
    /// them: both ways of clearing a selection (a confirmed Esc, or
    /// clicking outside the region to start a fresh drag) used to throw the
    /// whole vector away, so one stray keypress could wipe minutes of work
    /// with no way back. The vector moves onto the undo stack as a single
    /// `EditOp::Clear` instead, so one Ctrl+Z restores every annotation —
    /// and `undo` puts the selection rect back with them.
    fn discard_annotations_recoverably(&mut self) {
        if self.annotations.is_empty() { return; }
        if self.selection.is_some() { self.last_selection = self.selection; }
        let cleared = std::mem::take(&mut self.annotations);
        self.history.record(EditOp::Clear { annotations: cleared });
        self.selected_idx = None;
    }

    fn handle_left_press(&mut self) -> OverlayOutcome {
        // A color-pick session answers with the color under the click.
        if self.color_pick {
            return match self.color_at_cursor() {
                Some(c) => OverlayOutcome::ColorPicked(c),
                None => OverlayOutcome::Continue,
            };
        }
        // An armed Esc confirmation owns the click the same way it owns
        // the keyboard: clicking Close again goes through with it, a click
        // anywhere else calls it off and is swallowed so the answer can
        // never double as a drawing gesture.
        if self.pending_discard.is_some() {
            let on_close = self.selection.map_or(false, |sel| {
                let lb = self.panel_bounds(sel);
                let buttons = self.action_buttons();
                action_panel_hit(action_panel_origin(lb, sel, buttons), self.cursor, buttons)
                    == Some(ActionButton::Close)
            });
            if on_close { return self.confirm_discard(); }
            self.pending_discard = None;
            self.window.request_redraw();
            return OverlayOutcome::Continue;
        }
        // The eyedropper's click takes the color under the cursor, anywhere
        // but on the flyout, whose own controls stay live (its eyedropper
        // button puts it back down, a swatch picks that color instead).
        if self.picking_color && !self.flyout_rect().is_some_and(|(r, _, _)| rect_contains(r, self.cursor)) {
            self.pick_color_at_cursor();
            return OverlayOutcome::Continue;
        }
        // The tool flyout comes first, before the typing check below, so the
        // Text size slider resizes the text being typed instead of ending
        // it. A press on a row grabs its knob, one on a swatch or palette
        // arrow takes it; one on the flyout's padding is swallowed rather
        // than drawing under it.
        if let Some((k, _)) = self.knob_at_cursor() {
            self.dragging_knob = Some(k);
            self.set_knob_from_cursor(k);
            return OverlayOutcome::Continue;
        }
        // A color recolors the text being typed too (`set_color`), like the
        // size slider resizes it.
        if self.press_flyout_colors() {
            if std::mem::take(&mut self.wants_screen_color) {
                return OverlayOutcome::PickScreenColor;
            }
            return OverlayOutcome::Continue;
        }
        if self.flyout_rect().is_some_and(|(r, _, _)| rect_contains(r, self.cursor)) {
            return OverlayOutcome::Continue;
        }
        // Click anywhere while typing → commit the pending text and keep
        // going. Mirrors the C# TextBox-loses-focus behaviour.
        if self.state == State::TextInput {
            use framefang_core::annotation::AnnotationKind;
            if let Some(a) = self.current.take() {
                if let AnnotationKind::Text { ref text, .. } = a.kind {
                    if !text.trim().is_empty() {
                        self.add_annotation(a);
                    }
                }
            }
            self.state = State::Selected;
            self.set_text_input_active(false);
            self.window.request_redraw();
            // Fall through so the click can still pick a swatch / start a
            // new text input / drag a region etc.
        }
        // Tool/action panel + popup hit-testing happens BEFORE the
        // edge-resize / draw-start path, so a click on a button always
        // wins over a draw inside the selection. Mirrors the order in
        // C# OverlayForm.OnMouseDown.
        if self.state == State::Selected {
            if let Some(sel) = self.selection {
                let lb = self.panel_bounds(sel);
                let tp_origin = tool_panel_origin(lb, sel);

                // Tool panel — absent in record mode, so it can't take clicks
                // there either. The flyout and the timeline are already
                // unreachable in that mode (no tool letter keys, not a video
                // session).
                if let Some((_, btn)) = tool_panel_hit(tp_origin, self.cursor)
                    .filter(|_| !self.record_mode)
                {
                    match btn {
                        ToolPanelButton::Tool(t) => self.pick_tool(t),
                        ToolPanelButton::Undo => self.undo(),
                        ToolPanelButton::Redo => self.redo(),
                    }
                    self.window.request_redraw();
                    return OverlayOutcome::Continue;
                }

                // Action panel.
                let ap_origin = action_panel_origin(lb, sel, self.action_buttons());
                if let Some(action) = action_panel_hit(ap_origin, self.cursor, self.action_buttons()) {
                    return match action {
                        ActionButton::Pin    => self.commit_as_pin(),
                        ActionButton::Copy   => self.commit_as_copy(),
                        ActionButton::Save if self.mods.shift_key() => self.commit_as_save_as(),
                        ActionButton::Save   => self.commit(),
                        ActionButton::Record => self.commit_as_record(),
                        ActionButton::Close  => self.request_close(),
                    };
                }

                // Video timeline + duration chip. Gated on `video_mode`
                // so the bar can never steal clicks in the screenshot
                // editor, and on a known duration so a broken clip keeps
                // the old static behavior.
                if self.video_mode && self.duration.is_some() {
                    let bar = timeline_bar_rect(lb);
                    if rect_contains(timeline_chip_rect(bar), self.cursor) {
                        self.duration_choice = (self.duration_choice + 1) % DURATION_CHOICES.len();
                        self.window.request_redraw();
                        return OverlayOutcome::Continue;
                    }
                    if rect_contains(bar, self.cursor) {
                        // Click-to-seek + arm drag-tracking until mouseup,
                        // mirroring the marker-opacity slider.
                        self.dragging_playhead = true;
                        self.set_scrub_from_cursor(true);
                        return OverlayOutcome::Continue;
                    }
                }
            }
        }

        match self.state {
            State::Idle => {
                use crate::pick::PickMode;
                // The mode bar: a chip switches what a click takes.
                let (bar, chips) = self.mode_bar();
                if rect_contains(bar, self.cursor) {
                    if let Some((m, _)) = chips.iter().find(|(_, r)| rect_contains(*r, self.cursor)) {
                        self.set_pick_mode(*m);
                    }
                    return OverlayOutcome::Continue;
                }
                self.update_target();
                // Screen and All screens take a click, never a drag.
                if self.pick_mode.available(self.screens()) != PickMode::Auto {
                    self.pick_pressed = true;
                    return OverlayOutcome::Continue;
                }
                self.state     = State::Selecting;
                self.anchor    = self.cursor;
                self.selection = Some((self.cursor.0, self.cursor.1, 0, 0));
                self.window.request_redraw();
            }
            State::Selected => {
                // Edge-resize takes priority over starting a draw — if the
                // cursor is sitting on an edge or corner of the selection,
                // clicking there grabs that edge for resizing. Video mode
                // locks the selection to the full frame: resizing it would
                // desync the annotation coordinates from the video, so the
                // edges are not grabbable there. Select mode gives the edge
                // band back to hit-testing so ink drawn against the border
                // is still reachable.
                if let Some(sel) = self.selection.filter(|_| !self.locked() && !self.select_mode) {
                    let hit = hit_test_edge_scaled(
                        (sel.0 as f32, sel.1 as f32, sel.2 as f32, sel.3 as f32),
                        (self.cursor.0 as f32, self.cursor.1 as f32),
                        self.display_scale(),
                    );
                    if hit.is_some() {
                        self.state       = State::Resizing;
                        self.resize_edge = hit;
                        self.window.request_redraw();
                        return OverlayOutcome::Continue;
                    }
                }
                // Select mode intercepts the click before any drawing
                // starts: hit-test the ink under the cursor, pick the
                // topmost match and arm a move-drag. A click that lands on
                // nothing just drops the highlight — it never starts a new
                // region, so ink can't be lost by a stray click here.
                if self.select_mode {
                    self.selected_idx = self.hit_annotation_at_cursor();
                    if self.selected_idx.is_some() {
                        self.state      = State::MovingAnnotation;
                        self.move_last  = self.cursor;
                        self.move_total = (0.0, 0.0);
                    }
                    self.window.request_redraw();
                    return OverlayOutcome::Continue;
                }
                if self.cursor_in_selection() {
                    // Nothing to draw in record mode — a click inside the
                    // rectangle is a no-op rather than a stray annotation the
                    // video would never carry anyway.
                    if self.record_mode {
                        return OverlayOutcome::Continue;
                    }
                    // Editor window, no tool in hand: the picture is the
                    // handle that moves the window.
                    if self.editor_mode && !self.tool_armed {
                        let _ = self.window.drag_window();
                        return OverlayOutcome::Continue;
                    }
                    // Step is click-to-place — never enters `Drawing`. Drop a
                    // numbered marker right where the user clicked and bump
                    // the counter for the next click.
                    if self.tool == Tool::Step {
                        let p = Point2::new(self.cursor.0 as f32, self.cursor.1 as f32);
                        self.add_annotation(Annotation::step(self.stroke.color, p, self.step_count));
                        self.step_count = self.step_count.saturating_add(1);
                        self.window.request_redraw();
                    } else if self.tool == Tool::Text {
                        // Click-to-place a text caret. Typed characters
                        // extend the annotation; Enter commits, Esc cancels.
                        let p  = Point2::new(self.cursor.0 as f32, self.cursor.1 as f32);
                        // Text has its own size, set in its flyout.
                        let px = self.text_size;
                        self.current = Some(Annotation::text_sized(self.stroke.color, p, "", px));
                        self.state   = State::TextInput;
                        self.set_text_input_active(true);
                        // Park the IME candidate window on the caret so a
                        // CJK/compose popup doesn't cover what's being typed.
                        self.window.set_ime_cursor_area(
                            PhysicalPosition::new(p.x as i32, p.y as i32),
                            winit::dpi::PhysicalSize::new(1u32, px.ceil() as u32),
                        );
                        eprintln!("framefang: entered TextInput at ({}, {})", p.x, p.y);
                        self.window.request_redraw();
                    } else if let Some(a) = self.start_annotation() {
                        self.current = Some(a);
                        self.state   = State::Drawing;
                        self.window.request_redraw();
                    }
                } else if self.editor_mode {
                    // The canvas around the picture is the editor window's
                    // handle: press and drag to move it.
                    let _ = self.window.drag_window();
                } else {
                    // Start a new selection if the click was outside. The
                    // old region's ink is stashed BEFORE the selection is
                    // replaced, so undoing the clear restores the rect the
                    // annotations were drawn in, not the fresh empty one.
                    self.discard_annotations_recoverably();
                    self.state     = State::Selecting;
                    self.anchor    = self.cursor;
                    self.selection = Some((self.cursor.0, self.cursor.1, 0, 0));
                    self.step_count = 1;
                    self.window.request_redraw();
                }
            }
            _ => {}
        }
        OverlayOutcome::Continue
    }

    fn add_annotation(&mut self, mut a: Annotation) {
        // Video mode stamps the visibility window at commit time: start =
        // the current scrub position, end = start + the chip preset. The
        // untouched default (scrub 0 + "End") stays `None`, keeping the
        // zero-interaction session structurally identical to the static
        // whole-clip behavior.
        if self.video_mode {
            a.time = self.annotation_window();
        }
        self.annotations.push(a.clone());
        self.history.record(EditOp::Add { index: self.annotations.len() - 1, annotation: a });
    }

    /// The window a new annotation gets at the current scrub position +
    /// chip preset (see `stamp_window`). `None` = whole clip.
    fn annotation_window(&self) -> Option<(f32, f32)> {
        let dur = self.duration?;
        stamp_window(self.scrub_pos, dur,
                     DURATION_CHOICES[self.duration_choice % DURATION_CHOICES.len()])
    }

    fn handle_left_release(&mut self) -> OverlayOutcome {
        // End a flyout knob drag and persist the values, so the next session
        // opens with them. Best-effort: a save failure is logged, never
        // fatal (same contract as `AppSettings::save` everywhere else).
        if self.dragging_knob.take().is_some() || std::mem::take(&mut self.dragging_spectrum) {
            self.save_knobs();
            return OverlayOutcome::Continue;
        }
        if self.dragging_playhead {
            self.dragging_playhead = false;
            // Final frame swap so the background matches exactly where
            // the drag ended (mid-drag swaps are throttled).
            self.set_scrub_from_cursor(true);
        }
        match self.state {
            State::Idle if std::mem::take(&mut self.pick_pressed) => {
                return self.take_target();
            }
            State::Selecting => {
                let r = rect_from(self.anchor, self.cursor);
                if r.2 < 4 && r.3 < 4 {
                    // A click rather than a drag: what the highlight shows.
                    return self.take_target();
                } else if r.2 < 4 || r.3 < 4 {
                    // A sliver: nothing to take.
                    self.state     = State::Idle;
                    self.selection = None;
                } else {
                    self.selection = Some(r);
                    self.state     = State::Selected;
                    return self.finish_selection(self.switch_held());
                }
                self.window.request_redraw();
            }
            State::Drawing => {
                if let Some(a) = self.current.take() {
                    self.add_annotation(a);
                }
                self.state = State::Selected;
                self.window.request_redraw();
            }
            State::MovingAnnotation => {
                self.finish_annotation_drag();
            }
            State::Resizing => {
                self.state       = State::Selected;
                self.resize_edge = Edge::None;
                self.window.set_cursor(CursorIcon::Crosshair);
                self.window.request_redraw();
            }
            _ => {}
        }
        OverlayOutcome::Continue
    }

    /// A region was just picked (a drag let go, or Ctrl+A). The picker then
    /// copies it and closes (copy only, or the other way round when `switch`
    /// says the switch key is held), hands it to the editor window, or keeps
    /// editing it in place when the editor would not fit.
    fn finish_selection(&mut self, switch: bool) -> OverlayOutcome {
        if self.is_picker() {
            if !self.settings.opens_editor(switch) {
                if let Some(rect) = self.selection_in_bitmap() {
                    return OverlayOutcome::Copied(crop(&self.screenshot, rect));
                }
            }
            if let Some(out) = self.hand_off() { return out; }
        }
        self.window.request_redraw();
        OverlayOutcome::Continue
    }

    /// Ctrl+A: the whole monitor under the cursor, or, pressed on a
    /// selection that already is that monitor, every monitor. Before any
    /// region exists it finishes like a released drag. A locked selection
    /// (the editor window, a video frame) has nothing to select.
    fn select_all(&mut self) -> OverlayOutcome {
        use framefang_core::settings::SwitchKey;
        if self.locked() || !matches!(self.state, State::Idle | State::Selected) {
            return OverlayOutcome::Continue;
        }
        let size = self.window.inner_size();
        let whole = (0, 0, size.width as i32, size.height as i32);
        let screen = self.chrome_bounds(self.cursor);
        let next = if self.selection == Some(screen) { whole } else { screen };
        let was_idle = self.state == State::Idle;
        self.selection = Some(next);
        self.state = State::Selected;
        self.hover_tip = None;
        if was_idle {
            // Ctrl is part of this shortcut, so it only counts as the switch
            // key when it was already held with the capture key.
            let switch = self.hold_at_press
                || (self.settings.switch_key() != SwitchKey::Ctrl && self.switch_held());
            return self.finish_selection(switch);
        }
        self.window.request_redraw();
        OverlayOutcome::Continue
    }

    // ── the hover highlight ───────────────────────────────────────────
    //
    // Before any region exists, the capture screen lights up what a click
    // would take (`pick`): the window under the cursor, its screen, or
    // every screen. A drag still draws a region in the first mode.

    /// The capture screens that have a hover: the picker and record mode.
    fn has_hover(&self) -> bool { !self.locked() && !self.color_pick }

    /// How many screens the capture spans.
    fn screens(&self) -> usize { self.desktop.monitors().len().max(1) }

    /// The highlight shows while nothing is selected, and through the first
    /// few pixels of a press, which may still turn out to be a click.
    fn hover_showing(&self) -> bool {
        self.has_hover() && match self.state {
            State::Idle => true,
            State::Selecting => self.selection.is_some_and(|r| r.2 < 4 && r.3 < 4),
            _ => false,
        }
    }

    /// Re-aim the highlight at what is under the cursor now, gliding there
    /// from where it is if that changed.
    fn update_target(&mut self) {
        use crate::pick::PickMode;
        if !self.has_hover() { return; }
        let size = self.window.inner_size();
        let whole = (0, 0, size.width as i32, size.height as i32);
        let screen = self.chrome_bounds(self.cursor);
        let screens = self.screens();
        let screen_label = || if screens > 1 {
            let n = self.desktop.monitors().iter().position(|m| {
                let (mx, my) = vdesk::virtual_to_frame(self.frame_origin, (m.x, m.y));
                (mx, my) == (screen.0, screen.1)
            }).map_or(1, |i| i + 1);
            format!("Screen {n}")
        } else { "Screen".to_owned() };
        let next = match self.pick_mode.available(screens) {
            PickMode::AllScreens => Some((whole, "All screens".to_owned())),
            PickMode::Screen => Some((screen, screen_label())),
            PickMode::Auto => {
                let p = vdesk::frame_to_virtual(self.frame_origin, self.cursor);
                let hit = crate::pick::window_at(&self.windows, p).and_then(|(r, title)| {
                    let (fx, fy) = vdesk::virtual_to_frame(self.frame_origin, (r.0, r.1));
                    let r = crate::pick::clip((fx, fy, r.2, r.3), whole)?;
                    let name = if title.trim().is_empty() { "Window".to_owned() }
                               else { crate::pick::shorten(title.trim(), 48) };
                    Some((r, name))
                });
                hit.or_else(|| Some((screen, screen_label())))
            }
        };
        if next.as_ref().map(|t| t.0) != self.target.as_ref().map(|t| t.0) {
            self.glide_from = match &self.target {
                Some(_) => self.shown_target(),
                None => next.as_ref().map_or((0, 0, 0, 0), |t| crate::pick::arrival(t.0)),
            };
            self.glide_at = std::time::Instant::now();
        }
        self.target = next;
    }

    /// Where the highlight is drawn this frame, part way through its glide.
    fn shown_target(&self) -> Rect4 {
        match &self.target {
            Some((r, _)) => crate::pick::glide(self.glide_from, *r, self.glide_at.elapsed()),
            None => (0, 0, 0, 0),
        }
    }

    /// Switch what a click takes, and keep it for the next capture.
    fn set_pick_mode(&mut self, mode: crate::pick::PickMode) {
        if mode == self.pick_mode { return; }
        self.pick_mode = mode;
        self.settings.capture_mode = mode.name().to_owned();
        if let Err(e) = self.settings.save() {
            eprintln!("framefang: capture mode save failed: {e}");
        }
        self.update_target();
        self.window.request_redraw();
    }

    /// The mode bar and its chips, on the screen under the cursor.
    fn mode_bar(&self) -> (Rect4, Vec<(crate::pick::PickMode, Rect4)>) {
        let modes = crate::pick::PickMode::offered(self.screens());
        crate::pick::bar_layout(self.chrome_bounds(self.cursor), modes,
            |s| framefang_core::text::layout(s, FLYOUT_TEXT_PX).width().ceil() as i32)
    }

    /// A click (not a drag): take what the highlight shows.
    fn take_target(&mut self) -> OverlayOutcome {
        self.update_target();
        let Some((r, _)) = self.target.clone() else {
            self.state = State::Idle;
            self.selection = None;
            self.window.request_redraw();
            return OverlayOutcome::Continue;
        };
        self.selection = Some(r);
        self.state = State::Selected;
        self.hover_tip = None;
        self.finish_selection(self.switch_held())
    }

    /// Mutate the selection rect according to the edge being dragged.
    fn apply_resize(&mut self) {
        let Some((mut x, mut y, mut w, mut h)) = self.selection else { return; };
        let (cx, cy) = self.cursor;
        match self.resize_edge {
            Edge::Left       => { let dx = cx - x; w -= dx; x  = cx; }
            Edge::Right      => { w = cx - x; }
            Edge::Top        => { let dy = cy - y; h -= dy; y  = cy; }
            Edge::Bottom     => { h = cy - y; }
            Edge::TopLeft    => { let dx = cx - x; let dy = cy - y; w -= dx; h -= dy; x = cx; y = cy; }
            Edge::TopRight   => { let dy = cy - y; w = cx - x; h -= dy; y = cy; }
            Edge::BottomLeft => { let dx = cx - x; w -= dx; h = cy - y; x = cx; }
            Edge::BottomRight=> { w = cx - x; h = cy - y; }
            Edge::None       => {}
        }
        // Clamp to non-negative width/height — flip the rect if the user
        // dragged past the opposite edge.
        if w < 0 { x += w; w = -w; }
        if h < 0 { y += h; h = -h; }
        if w < 4 { w = 4; }
        if h < 4 { h = 4; }
        self.selection = Some((x, y, w, h));
    }

    fn compute_hover_tip(&self) -> Option<(&'static str, i32, i32)> {
        let sel = self.selection?;
        let lb  = self.panel_bounds(sel);
        // Tool panel — never drawn in record mode, so never hoverable either.
        let tp = tool_panel_origin(lb, sel);
        if let Some((idx, btn)) = tool_panel_hit(tp, self.cursor).filter(|_| !self.record_mode) {
            let (_, _, x1, y1) = tool_panel_button_rect(tp, idx as i32);
            let label = match btn {
                ToolPanelButton::Tool(Tool::Pen)        => "Pen (P)",
                ToolPanelButton::Tool(Tool::Line)       => "Line (L)",
                ToolPanelButton::Tool(Tool::Arrow)      => "Arrow (A)",
                ToolPanelButton::Tool(Tool::Rectangle)  => "Rectangle (R)",
                ToolPanelButton::Tool(Tool::Ellipse)    => "Ellipse (E)",
                ToolPanelButton::Tool(Tool::Marker)     => "Marker (M)",
                ToolPanelButton::Tool(Tool::Text)       => "Text (T)",
                ToolPanelButton::Tool(Tool::Step)       => "Step (N)",
                ToolPanelButton::Tool(Tool::Pixelate)   => "Pixelate (B)",
                ToolPanelButton::Undo                   => "Undo (Ctrl+Z)",
                ToolPanelButton::Redo                   => "Redo (Ctrl+Y)",
            };
            // The tool whose settings are slid out beside it needs no tip:
            // it would land on top of them.
            if matches!(btn, ToolPanelButton::Tool(t) if self.flyout == Some(t)) {
                return None;
            }
            return Some((label, x1 + 6, y1 - 14));
        }
        // Action panel.
        let ap = action_panel_origin(lb, sel, self.action_buttons());
        if let Some(btn) = action_panel_hit(ap, self.cursor, self.action_buttons()) {
            let label = match btn {
                ActionButton::Pin    => "Pin to screen",
                ActionButton::Copy   => "Copy (Ctrl+C)",
                ActionButton::Save if self.video_mode => "Save (Ctrl+S)",
                ActionButton::Save   => "Save (Ctrl+S)  -  Shift: Save as",
                ActionButton::Record => if self.record_mode { "Record (Enter)" } else { "Record this region" },
                ActionButton::Close  => "Close (Esc)",
            };
            return Some((label, self.cursor.0 + 14, self.cursor.1 + 14));
        }
        // The flyout labels its own rows; no tooltip over it, but for the
        // eyedropper, which is only an icon.
        if let Some(((fx, fy, fw, fh), _, _)) = self.flyout_rect() {
            if rect_contains((fx, fy, fw, fh), self.cursor) {
                let on_dropper = self.flyout.and_then(|t| flyout_layout(t).custom)
                    .is_some_and(|c| rect_contains(offset(c.eyedropper, (fx, fy)), self.cursor));
                return (on_dropper && !self.picking_color)
                    .then_some(("Pick a color from the picture", self.cursor.0 + 14, self.cursor.1 + 14));
            }
        }
        // Video timeline — same gates as the click handler so the tip can
        // only ever appear where a click would actually land.
        if self.video_mode && self.duration.is_some() {
            let bar = timeline_bar_rect(lb);
            if rect_contains(timeline_chip_rect(bar), self.cursor) {
                return Some(("Annotation duration", self.cursor.0 + 14, self.cursor.1 - 24));
            }
            if rect_contains(bar, self.cursor) {
                return Some(("Seek", self.cursor.0 + 14, self.cursor.1 - 24));
            }
        }
        None
    }

    /// The editor window's cursor, saying what a press would do there: a
    /// hand on a button, an arrow on the flyout, the move arrows wherever a
    /// drag moves the window (the picture with no tool in hand, the panels'
    /// edges), the crosshair where a drag draws. `None` leaves it to select
    /// mode's own cursor.
    fn editor_cursor(&self) -> Option<CursorIcon> {
        let sel = self.selection?;
        let lb = self.panel_bounds(sel);
        let on_tool = tool_panel_hit(tool_panel_origin(lb, sel), self.cursor).is_some();
        let actions = self.action_buttons();
        let on_action = action_panel_hit(action_panel_origin(lb, sel, actions), self.cursor, actions).is_some();
        if on_tool || on_action {
            return Some(CursorIcon::Pointer);
        }
        if self.flyout_rect().is_some_and(|(r, _, _)| rect_contains(r, self.cursor)) {
            return Some(CursorIcon::Default);
        }
        if self.select_mode { return None; }
        Some(if self.tool_armed && self.cursor_in_selection() { CursorIcon::Crosshair } else { CursorIcon::Move })
    }

    fn update_resize_cursor(&self) {
        // The eyedropper aims with the crosshair. Over the flyout, whose
        // controls still work, it is the arrow.
        if self.picking_color {
            let on_flyout = self.flyout_rect().is_some_and(|(r, _, _)| rect_contains(r, self.cursor));
            self.window.set_cursor(if on_flyout { CursorIcon::Default } else { CursorIcon::Crosshair });
            return;
        }
        if self.editor_mode {
            if let Some(icon) = self.editor_cursor() {
                self.window.set_cursor(icon);
                return;
            }
        }
        // Select mode has no edge-resize: the cursor reports whether there
        // is ink under it to grab instead.
        if self.select_mode {
            let icon = if self.hit_annotation_at_cursor().is_some() {
                CursorIcon::Move
            } else {
                CursorIcon::Default
            };
            self.window.set_cursor(icon);
            return;
        }
        let Some(sel) = self.selection else { return; };
        // A locked selection has no edges to grab. In the editor window the
        // picture takes the crosshair and the canvas around it moves the
        // window.
        if self.locked() {
            let icon = if !self.editor_mode || self.cursor_in_selection() { CursorIcon::Crosshair }
                       else { CursorIcon::Move };
            self.window.set_cursor(icon);
            return;
        }
        let hit = hit_test_edge_scaled(
            (sel.0 as f32, sel.1 as f32, sel.2 as f32, sel.3 as f32),
            (self.cursor.0 as f32, self.cursor.1 as f32),
            self.display_scale(),
        );
        let icon = match hit {
            Edge::Left | Edge::Right                  => CursorIcon::EwResize,
            Edge::Top  | Edge::Bottom                 => CursorIcon::NsResize,
            Edge::TopLeft | Edge::BottomRight         => CursorIcon::NwseResize,
            Edge::TopRight | Edge::BottomLeft         => CursorIcon::NeswResize,
            Edge::None                                => CursorIcon::Crosshair,
        };
        self.window.set_cursor(icon);
    }

    fn cursor_in_selection(&self) -> bool {
        if let Some((x, y, w, h)) = self.selection {
            let (cx, cy) = self.cursor;
            cx >= x && cx < x + w && cy >= y && cy < y + h
        } else { false }
    }

    fn start_annotation(&self) -> Option<Annotation> {
        let p = Point2::new(self.cursor.0 as f32, self.cursor.1 as f32);
        Some(match self.tool {
            Tool::Pen       => Annotation::pen(self.stroke, p),
            Tool::Arrow     => Annotation::arrow(self.stroke, p),
            Tool::Rectangle => Annotation::rectangle(self.stroke, p),
            Tool::Ellipse   => Annotation::ellipse(self.stroke, p),
            Tool::Line      => Annotation::line(self.stroke, p),
            Tool::Marker    => Annotation::marker(self.stroke, p, self.settings.marker_opacity),
            Tool::Pixelate  => {
                let mut a = Annotation::pixelate(p);
                if let framefang_core::annotation::AnnotationKind::Pixelate { block_size, .. } = &mut a.kind {
                    *block_size = self.pixel_size;
                }
                a
            }
            // Step is handled inline at click site (no `Drawing` state).
            Tool::Step      => return None,
            // Text enters its own `TextInput` state instead of `Drawing` —
            // also handled inline.
            Tool::Text      => return None,
        })
    }

    fn commit(&mut self) -> OverlayOutcome {
        // Record mode: Enter starts a recording of the rectangle instead of
        // saving a screenshot of it.
        if self.record_mode {
            return self.commit_as_record();
        }
        // Video mode: AcceptedVideo carries the raw annotation list plus
        // the committed background frame. The tray loop's burn worker
        // composes the per-window overlays from it (`compose_overlay_
        // groups`) — each distinct window start spawns ffmpeg for its
        // pristine frame, which would freeze the event loop here.
        if self.video_mode {
            if self.state != State::Selected { return OverlayOutcome::Continue; }
            return OverlayOutcome::AcceptedVideo(VideoCommit {
                annotations: self.annotations.clone(),
                frame:       self.screenshot.clone(),
                frame_t:     self.scrub_frame_t,
                duration:    self.duration,
            });
        }
        match self.compose_final() {
            Some(img) => OverlayOutcome::Accepted(img),
            None      => OverlayOutcome::Continue,
        }
    }

    fn commit_as_copy(&mut self) -> OverlayOutcome {
        match self.compose_final() {
            Some(img) if self.settings.close_after_copy => OverlayOutcome::Copied(img),
            // Copy and keep editing: say it worked, since nothing closes.
            Some(img) => {
                self.copied_at = Some(std::time::Instant::now());
                self.window.request_redraw();
                OverlayOutcome::CopiedOpen(img)
            }
            None => OverlayOutcome::Continue,
        }
    }

    /// Save As. A video session's Save burns the clip, which has no format
    /// to choose, so there it is the ordinary Save.
    fn commit_as_save_as(&mut self) -> OverlayOutcome {
        if self.video_mode || self.record_mode { return self.commit(); }
        match self.compose_final() {
            Some(img) => OverlayOutcome::SaveAs(img),
            None      => OverlayOutcome::Continue,
        }
    }

    /// Hand the locked-in rectangle back as a recording region.
    ///
    /// Unlike every other commit path this composes no bitmap: the output of
    /// the session is the rectangle itself. Virtual-screen (device) pixels,
    /// lifted out of frame space the same way the pin position is — the
    /// caller clamps them to the desktop and rounds them for the encoder.
    fn commit_as_record(&mut self) -> OverlayOutcome {
        if self.state != State::Selected { return OverlayOutcome::Continue; }
        match self.selection {
            Some(rect) => OverlayOutcome::RecordRegion(
                vdesk::frame_rect_to_virtual(self.frame_origin, rect)),
            None       => OverlayOutcome::Continue,
        }
    }

    fn commit_as_pin(&mut self) -> OverlayOutcome {
        // `PinView` positions its window in absolute device pixels, which the
        // selection is only trivially equal to on a single 1x screen at the
        // virtual origin. `frame_origin` is the framebuffer's device-pixel
        // origin (the display map's physical origin, re-derived from the
        // window), so lifting the selection through it lands the pin over
        // the region it was cut from on a scaled or offset desktop — on a
        // monitor left of / above the primary one that is a negative position.
        // `PinView` positions its window in virtual-screen coordinates, so
        // the selection has to be lifted out of frame space — on a monitor
        // left of / above the primary one that is a negative position.
        // The editor window may have been moved, so its pin opens over the
        // picture where the window is now.
        let origin = if self.editor_mode {
            self.window.inner_position().map(|p| (p.x, p.y)).unwrap_or(self.frame_origin)
        } else { self.frame_origin };
        let pos = match self.selection {
            Some((x, y, _, _)) => vdesk::frame_to_virtual(origin, (x, y)),
            None               => return OverlayOutcome::Continue,
        };
        match self.compose_final() {
            Some(img) => OverlayOutcome::Pinned(img, pos),
            None      => OverlayOutcome::Continue,
        }
    }

    /// Crop + composite annotations into the output bitmap. Shared between
    /// the save and copy commit paths so they're guaranteed to produce the
    /// same pixels — only what the caller does with the bitmap differs.
    fn compose_final(&self) -> Option<ImageBuffer<Rgba<u8>, Vec<u8>>> {
        if self.state != State::Selected { return None; }
        let rect = self.selection?;
        // The selection is in frame space; the capture is in bitmap space.
        // On a multi-monitor desktop those differ by the virtual-desktop
        // origin, which is what used to make a region picked on a monitor
        // left of / above the primary one crop the wrong pixels.
        let mut img = crop(&self.screenshot, self.selection_in_bitmap()?);
        // Snapshot the un-annotated crop FIRST so pixelate's source-sampling
        // stays idempotent under draw-order: pixelate must always sample the
        // original screenshot, never something we already painted on.
        // Mirrors C# `PixelateAnnotation`.
        let pristine = img.clone();
        let dx = -rect.0 as f32;
        let dy = -rect.1 as f32;
        let mut surf = ImageSurface(&mut img);
        for a in &self.annotations {
            // Video-mode Copy/Pin produce a still of exactly what is on
            // screen: only ink visible at the scrub position is burned.
            // Screenshot mode carries no windows so this never skips.
            if self.video_mode && !annotation_visible_at(a, self.scrub_pos) { continue; }
            let translated = translate_annotation(a, dx, dy);
            painter::render_annotation(&mut surf, &translated, Some(&pristine));
        }
        Some(img)
    }

    /// Force keyboard focus + grab to our window via raw X11 calls. Retries
    /// on every redraw until SetInputFocus stops returning BadMatch — the
    /// window may still be in `IsUnviewable` state on the first frame and
    /// X11 rejects focus changes against unviewable windows. Cinnamon
    /// usually maps the window within ~2-3 redraw cycles after which the
    /// focus push succeeds.
    #[cfg(target_os = "linux")]
    fn push_x11_focus(&mut self) {
        if self.focus_pushed { return; }
        const MAX_FOCUS_ATTEMPTS: u32 = 60;
        self.focus_attempts = self.focus_attempts.saturating_add(1);
        use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
        let xid = match self.window.window_handle() {
            Ok(h) => match h.as_raw() {
                RawWindowHandle::Xlib(x) => x.window as u32,
                _ => { self.focus_pushed = true; return; }
            }
            Err(_) => { self.focus_pushed = true; return; }
        };
        match force_x11_focus(xid) {
            Ok(()) => {
                eprintln!("framefang: x11 focus + grab pushed for window 0x{xid:x}");
                self.focus_pushed = true;
            }
            Err(e) => {
                if self.focus_attempts >= MAX_FOCUS_ATTEMPTS {
                    eprintln!(
                        "framefang: gave up pushing x11 focus after {} attempts ({e}); \
                         Text tool may not receive keypresses on this WM",
                        self.focus_attempts
                    );
                    self.focus_pushed = true;
                } else {
                    eprintln!("framefang: x11 focus retry pending ({e})");
                }
            }
        }
    }

    fn redraw(&mut self) {
        // Where the window actually landed decides every coordinate mapping
        // below, so re-read it before anything is drawn or sampled.
        self.sync_frame_origin();
        #[cfg(target_os = "linux")]
        {
            self.push_x11_focus();
            // Keep cycling redraws until focus is grabbed. After that it
            // settles and we redraw only on actual events.
            if !self.focus_pushed {
                self.window.request_redraw();
            }
        }
        // Bound before the framebuffer is borrowed: the list is `'static`, but
        // reading it off `self` mid-draw would alias the surface borrow.
        let actions = self.action_buttons();
        let phys = self.window.inner_size();
        let (Some(w), Some(h)) = (NonZeroU32::new(phys.width), NonZeroU32::new(phys.height)) else { return; };
        // Chrome bounds have to be resolved before the framebuffer is
        // borrowed — they read `self`, the buffer borrows `self.surface`.
        let cursor_bounds = self.chrome_bounds(self.cursor);
        let sel_bounds    = self.selection.map(|sel| self.panel_bounds(sel));
        // The tool flyout: where, which side, each row's value, how far out.
        let flyout_draw = self.flyout_rect().and_then(|(rect, on_right, pointer_y)| {
            let layout = flyout_layout(self.flyout?);
            let values: Vec<f32> = layout.knobs.iter().map(|(k, _)| self.knob_value(*k)).collect();
            let t = self.flyout_opened.elapsed().as_secs_f32() / FLYOUT_OPEN.as_secs_f32();
            Some(FlyoutFrame {
                rect, on_right, pointer_y, layout, values,
                color: self.stroke.color,
                palette: self.palette_index,
                open: ease_out(t),
                picking: self.picking_color,
                spectrum_pick: self.spectrum_pick,
            })
        });
        let locked = self.locked();
        // Where the eyedropper would take a color from.
        let picking_here = self.picking_color
            && !flyout_draw.as_ref().is_some_and(|f| rect_contains(f.rect, self.cursor))
            && (!self.editor_mode || self.cursor_in_selection());
        // The hover highlight, part way through its glide, and its label.
        let hover = self.hover_showing()
            .then(|| self.target.as_ref().map(|(r, label)| (self.shown_target(), label.clone(), (r.2, r.3))))
            .flatten();
        let gliding = hover.is_some() && self.glide_at.elapsed() < crate::pick::GLIDE;
        let pick_mode = self.pick_mode.available(self.screens());
        let mode_bar = (self.state == State::Idle && self.has_hover()).then(|| self.mode_bar());
        // Offset of framebuffer (0, 0) inside the capture: zero whenever the
        // overlay covers the virtual desktop as asked.
        let shot_off      = self.bitmap_offset();
        if let Err(e) = self.surface.resize(w, h) {
            eprintln!("overlay: surface.resize: {e}"); return;
        }
        let mut buf = match self.surface.buffer_mut() {
            Ok(b) => b,
            Err(e) => { eprintln!("overlay: buffer_mut: {e}"); return; }
        };

        let win_w  = w.get() as usize;
        let win_h  = h.get() as usize;
        let shot_w = self.screenshot.width()  as usize;
        let shot_h = self.screenshot.height() as usize;
        let shot   = self.screenshot.as_raw();

        // While the highlight shows, a press that has not moved yet is no
        // selection to draw.
        let sel_rect = self.selection.filter(|_| hover.is_none()).map(|(x, y, w, h)| (x, y, x + w, y + h));

        // Pass 1: screenshot, dimmed outside the selection (or, before
        // there is one, outside the highlight). Off-capture pixels (a gap
        // between differently-sized monitors, or a window the WM pushed past
        // the capture) paint black.
        let t_start = std::time::Instant::now();
        let backdrop_shot = crate::backdrop::Shot { rgba: shot, width: shot_w, height: shot_h, offset: shot_off };
        // A color-pick session shows the screen as it is: nothing dimmed.
        let lit = if self.color_pick { Some((0, 0, win_w as i32, win_h as i32)) }
                  else { hover.as_ref().map(|((x, y, w, h), _, _)| (*x, *y, x + w, y + h)).or(sel_rect) };
        buf.copy_from_slice(self.backdrop.frame(win_w, win_h, &backdrop_shot, lit));
        let t_backdrop = t_start.elapsed();

        // Pass 2: annotations, clipped to the selection. We render into the
        // shared u32 buffer through `U32Surface`. Bounds-clipping happens at
        // the per-pixel level inside the painter so we don't have to manage
        // a scissor here, but we still skip when there's no selection.
        let mut surf = U32Surface { buf: &mut buf, stride: win_w as i32, height: win_h as i32 };
        for a in &self.annotations {
            // Timeline preview: only ink whose window contains the scrub
            // position is visible. Screenshot mode carries no windows.
            if self.video_mode && !annotation_visible_at(a, self.scrub_pos) { continue; }
            painter::render_annotation_offset(&mut surf, a, Some(&self.screenshot), shot_off);
        }
        if let Some(a) = self.current.as_ref() {
            painter::render_annotation_offset(&mut surf, a, Some(&self.screenshot), shot_off);
        }

        // While typing — show a subtle dashed rectangle around the text
        // bounds so the user can see where text will appear. Drawn only
        // in the live preview; `compose_final` never renders it, so the
        // saved/copied bitmap shows just the text without any frame.
        if self.state == State::TextInput {
            if let Some(a) = self.current.as_ref() {
                if let framefang_core::annotation::AnnotationKind::Text { color, position, text, font_size } = &a.kind {
                    // Frame + caret are measured with the same rasterizer that
                    // paints the glyphs, so they track accents, wide scripts
                    // and multi-line text exactly.
                    let px = *font_size;
                    // The preedit is drawn but not stored, so it has to be
                    // measured as if it were already appended.
                    let composed = if self.ime_preedit.is_empty() {
                        None
                    } else {
                        Some(format!("{text}{}", self.ime_preedit))
                    };
                    let shown: &str = composed.as_deref().unwrap_or(text);
                    let block = framefang_core::text::layout(shown, px);
                    if !self.ime_preedit.is_empty() {
                        // Composition is provisional — draw it dimmed after
                        // the committed text so the user can tell them apart.
                        let mut surf = U32Surface { buf: &mut buf, stride: win_w as i32, height: win_h as i32 };
                        let preedit_color = color.with_alpha(0x99);
                        painter::blit_text_block(&mut surf, position.x as i32, position.y as i32,
                                                 &block, preedit_color);
                    }
                    let (caret_x_f, caret_y_f) = block.caret();
                    let text_w = block.width().ceil() as i32;
                    let text_h = block.height().ceil() as i32;
                    let pad = 4;
                    let x0 = position.x as i32 - pad;
                    let y0 = position.y as i32 - pad;
                    let x1 = position.x as i32 + text_w.max(2) + pad;
                    let y1 = position.y as i32 + text_h + pad;
                    draw_dashed_border(&mut buf, win_w, win_h, x0, y0, x1, y1, 0x00_88_88_8C);
                    // Solid 1-px caret on the last line, at the trailing edge.
                    let caret_x  = position.x as i32 + caret_x_f.round() as i32 + 1;
                    let caret_y0 = position.y as i32 + caret_y_f.round() as i32;
                    let caret_y1 = caret_y0 + block.line_height.round() as i32;
                    for cy in caret_y0..caret_y1 {
                        if caret_x >= 0 && (caret_x as usize) < win_w && cy >= 0 && (cy as usize) < win_h {
                            buf[cy as usize * win_w + caret_x as usize] = 0x00_E8_E8_EC;
                        }
                    }
                }
            }
        }

        // Pass 2.5: select-mode highlight — a dashed laser-green box around
        // the picked annotation's painted bounds, with solid corner ticks so
        // it reads as "grabbed" rather than as another region rectangle.
        // Preview only: `compose_final` never draws it, so the saved bitmap
        // carries the ink alone.
        if let Some(idx) = self.selected_idx {
            if let Some(a) = self.annotations.get(idx) {
                const HILITE: u32 = NEON_GREEN;
                let b   = framefang_core::edit::bounds(a);
                let pad = 5;
                let x0  = b.x as i32 - pad;
                let y0  = b.y as i32 - pad;
                let x1  = (b.x + b.w) as i32 + pad;
                let y1  = (b.y + b.h) as i32 + pad;
                draw_dashed_border(&mut buf, win_w, win_h, x0, y0, x1, y1, HILITE);
                let tick = 4;
                for &(hx, hy) in &[(x0, y0), (x1 - tick, y0), (x0, y1 - tick), (x1 - tick, y1 - tick)] {
                    draw_filled_rect(&mut buf, win_w, win_h, hx, hy, hx + tick, hy + tick, HILITE);
                }
            }
        }

        // Pass 3: selection border + 8 handles.
        if let Some((x0, y0, x1, y1)) = sel_rect {
            const BLUE:  u32 = 0x00_64_95_ED;
            const WHITE: u32 = 0x00_FF_FF_FF;
            draw_rect_border(&mut buf, win_w, win_h, x0, y0, x1, y1, BLUE);

            let xm = (x0 + x1) / 2;
            let ym = (y0 + y1) / 2;
            // Handles say "drag me"; a locked selection has nothing to drag.
            let handles: &[(i32, i32)] = if locked { &[] } else { &[
                (x0, y0), (xm, y0), (x1.saturating_sub(1), y0),
                (x0, ym),                     (x1.saturating_sub(1), ym),
                (x0, y1.saturating_sub(1)), (xm, y1.saturating_sub(1)), (x1.saturating_sub(1), y1.saturating_sub(1)),
            ] };
            for &(hx, hy) in handles {
                draw_filled_rect(&mut buf, win_w, win_h, hx - 3, hy - 3, hx + 3, hy + 3, WHITE);
                draw_rect_border(&mut buf, win_w, win_h, hx - 3, hy - 3, hx + 3, hy + 3, BLUE);
            }
        }
        // Pass 3.5: the hover highlight's frame and label.
        if let Some((r, label, size)) = &hover {
            draw_target(&mut buf, win_w, win_h, *r, label, *size);
        }
        // Where the window cannot be cut to shape, an edge so it at least
        // reads as a window over the live desktop.
        if self.editor_mode && !crate::window_shape::supported() {
            draw_rect_border(&mut buf, win_w, win_h, 0, 0, win_w as i32, win_h as i32, EDITOR_FRAME);
        }

        // Pass 4: dimension chip — small dark pill at bottom-right of the
        // selection showing the locked-in width × height. Visible whenever
        // there's a selection (including mid-drag), matches C# OverlayForm.
        // Not in the editor window: the size was settled in the picker.
        if let Some((x, y, w, h)) = self.selection.filter(|_| !self.editor_mode) {
            if w > 8 && h > 8 {
                let lb = sel_bounds.unwrap_or((0, 0, win_w as i32, win_h as i32));
                draw_dimension_chip(&mut buf, win_w, win_h, lb, x + w, y + h, w as u32, h as u32);
            }
        }

        // Pass 5: tool panel + action panel + (optional) color popup —
        // floating around the selection, never spanning the screen.
        let is_selected_now = matches!(self.state, State::Selected);
        if is_selected_now && !self.last_was_selected {
            self.panel_pulse_started = Some(std::time::Instant::now());
        }
        self.last_was_selected = is_selected_now;

        if matches!(self.state,
                    State::Selected | State::Drawing | State::Resizing
                    | State::TextInput | State::MovingAnnotation) {
            if let Some(sel) = self.selection {
                let lb = sel_bounds.unwrap_or((0, 0, win_w as i32, win_h as i32));
                // Record mode is selection-only: no tool column, no glow, no
                // palette, no per-tool widgets. Just the rectangle and the
                // two-button action row.
                if !self.record_mode {
                    draw_tool_panel(&mut buf, win_w, win_h, lb, sel, self.tool_armed.then_some(self.tool));
                    // Sequential orange-neon halo cycles through every button
                    // (top→bottom, 1 per slot). Mirrors the website's tool-palette
                    // demo so the in-app palette feels alive even when the user
                    // is just hovering. Painted after the panel so the halo sits
                    // on top of the button background but under the icon glyph.
                    let elapsed = self.opened_at.elapsed().as_secs_f32();
                    draw_tool_panel_glow(&mut buf, win_w, win_h, lb, sel, elapsed);
                }
                draw_action_panel(&mut buf, win_w, win_h, lb, sel, actions);
                // Attention pulse on the action panel when it appears (three
                // slow breaths) — the panel auto-positions based on selection +
                // screen, so first-time users can lose track of where Save /
                // Copy / Pin landed.
                if matches!(self.state, State::Selected) {
                    if let Some(start) = self.panel_pulse_started {
                        let elapsed = start.elapsed().as_secs_f32();
                        if elapsed < PULSE_SECS {
                            // The editor window gets the frame without the
                            // outer haze: past the panel it would blend over
                            // the cut-away canvas and read as a dark box.
                            draw_action_panel_pulse(&mut buf, win_w, win_h, lb, sel, elapsed, actions,
                                                    !self.editor_mode);
                        } else {
                            self.panel_pulse_started = None;
                        }
                    }
                }
                if let Some(f) = &flyout_draw {
                    draw_flyout(&mut buf, win_w, win_h, f);
                }
            }
        }

        // Pass 5.5: idle hint — before any selection exists the overlay is
        // just a dimmed desktop with a lens on it, which says nothing about
        // dragging a region or about the way back out. One dim line spells
        // both out; it disappears the moment the drag starts.
        if matches!(self.state, State::Idle) && !self.color_pick {
            draw_idle_hint(&mut buf, win_w, win_h, cursor_bounds,
                           if self.record_mode { RECORD_HINT } else { pick_mode.hint() });
        }
        if let Some((bar, chips)) = &mode_bar {
            let hovered = chips.iter().find(|(_, r)| rect_contains(*r, self.cursor)).map(|(m, _)| *m);
            draw_mode_bar(&mut buf, win_w, win_h, *bar, chips, pick_mode, hovered);
        }

        // Pass 6: magnifier — only useful when the user is positioning the
        // selection edge by individual pixels. Once a region is locked in,
        // the toolbar+palette take over and the lens just gets in the way.
        // Drawn after the hint so the lens is never covered by it.
        // The eyedropper brings it back over the picture, to show the pixel
        // a click would take.
        // Only where a drag can start: Screen and All screens take a click.
        let can_drag = locked || pick_mode == crate::pick::PickMode::Auto;
        let on_bar = mode_bar.as_ref().is_some_and(|(b, _)| rect_contains(*b, self.cursor));
        if picking_here {
            // The eyedropper's own loupe: the lens and the color's card. Over
            // the capture screen's panels it opens up and to the left, away
            // from the tool panel and its flyout.
            draw_color_loupe(&mut buf, win_w, win_h, cursor_bounds,
                             &self.screenshot, self.cursor, shot_off, !self.color_pick, self.color_pick);
        } else if matches!(self.state, State::Idle | State::Selecting) && can_drag && !on_bar {
            draw_magnifier(&mut buf, win_w, win_h, cursor_bounds,
                           &self.screenshot, self.cursor, shot_off, false);
        }

        // Pass 6.5: video timeline — video mode only, and only when the
        // clip length is known (a broken Duration banner hides the bar
        // and the session behaves like the static whole-clip editor).
        if self.video_mode {
            if let Some(dur) = self.duration {
                let ticks: Vec<f32> = self.annotations.iter()
                    .filter_map(|a| a.time.map(|(s, _)| s))
                    .collect();
                draw_timeline_bar(&mut buf, win_w, win_h, cursor_bounds, self.scrub_pos, dur,
                                  duration_chip_label(self.duration_choice), &ticks);
            }
        }

        // Pass 7: tooltip chip — only when the user is hovering a button
        // in `Selected`. Mirrors C# `MakeButton(tip, ...)` behaviour.
        if let Some((label, x, y)) = self.hover_tip {
            if matches!(self.state, State::Selected) {
                draw_tooltip(&mut buf, win_w, win_h, cursor_bounds, label, x, y);
            }
        }

        // Pass 8: the notice bar — the Esc confirmation, or the select-mode
        // cue when nothing needs confirming. Painted last so it is never
        // covered by a panel, a popup or the timeline.
        if let Some(kind) = self.pending_discard {
            let headline = match kind {
                PendingDiscard::ClearSelection =>
                    format!("Discard {} annotation{}?", self.annotations.len(),
                            if self.annotations.len() == 1 { "" } else { "s" }),
                PendingDiscard::CloseOverlay => "Close without saving?".to_string(),
            };
            draw_notice_bar(
                &mut buf, win_w, win_h, cursor_bounds,
                &[&headline, "Esc again to discard  -  any other key to keep editing"],
                0x00_FF_B0_20, 0x00_FF_D8_8A,
            );
        } else if self.select_mode {
            // Once something is picked the cue shrinks to a single line —
            // the user is working under it and doesn't need the recipe any
            // more.
            let lines: &[&str] = if self.selected_idx.is_some() {
                &["Select mode", "drag to move  -  Del removes"]
            } else {
                &["Select mode", "click ink to move it  -  Del removes  -  S or a tool key exits"]
            };
            draw_notice_bar(&mut buf, win_w, win_h, cursor_bounds, lines, NEON_GREEN, NEON_GREEN_TEXT);
        } else if self.copied_at.is_some_and(|t| t.elapsed() < COPIED_NOTICE) {
            // Copy that kept the editor open: nothing closed, so say it worked.
            draw_notice_bar(&mut buf, win_w, win_h, cursor_bounds, &["Copied to clipboard"],
                            NEON_GREEN, NEON_GREEN_TEXT);
        }

        let t_chrome = t_start.elapsed();
        // Editor window: cut away every pixel that is still bare canvas.
        if self.editor_mode && crate::window_shape::supported() {
            let rects = crate::window_shape::drawn_rects(&buf, win_w, win_h, sel_rect, EDITOR_CLEAR);
            if rects != self.shape {
                crate::window_shape::apply(&self.window, &rects);
                self.shape = rects;
            }
        }
        if let Err(e) = buf.present() {
            eprintln!("overlay: buf.present: {e}");
        }
        if !self.shown {
            self.shown = true;
            if crate::reveal::SUPPORTED {
                crate::reveal::uncloak(&self.window);
            } else {
                self.window.set_visible(true);
            }
            self.window.focus_window();
        }
        if let Some(log) = self.frame_log.as_mut() {
            let ms = |d: std::time::Duration| d.as_secs_f32() * 1000.0;
            log.push((ms(t_backdrop), ms(t_chrome), ms(t_start.elapsed())));
        }

        // The tool-panel glow and the action-panel pulse have no event
        // behind them, so this frame has to ask for the next one. Only the
        // resting `Selected` state self-drives: while the user is drawing,
        // resizing or typing, their input already produces frames, and
        // pacing those would just add latency. Sleeping out the remainder
        // of the interval is what keeps this from becoming a hot loop —
        // winit hands back a redraw requested here the instant we return,
        // so without the wait the overlay renders as fast as the CPU
        // allows for an animation that only steps 30 times a second.
        // The flyout's slide-out runs at 60 frames a second; the glow and
        // pulse only need 30.
        let sliding = self.flyout.is_some() && self.flyout_opened.elapsed() < FLYOUT_OPEN;
        if gliding {
            // The highlight's glide, at 60 frames a second until it lands.
            let since = self.last_anim_frame.map_or(ANIM_FRAME, |t| t.elapsed());
            if since < ANIM_FRAME / 2 { std::thread::sleep(ANIM_FRAME / 2 - since); }
            self.last_anim_frame = Some(std::time::Instant::now());
            self.window.request_redraw();
        } else if is_selected_now && self.selection.is_some() {
            let frame = if sliding { ANIM_FRAME / 2 } else { ANIM_FRAME };
            let since = self.last_anim_frame.map_or(frame, |t| t.elapsed());
            if since < frame {
                std::thread::sleep(frame - since);
            }
            self.last_anim_frame = Some(std::time::Instant::now());
            self.window.request_redraw();
        }
    }

}

// ── tool/action panel layout (mirrors C# OverlayForm.PositionToolbars) ─────

/// Outer width × height of the vertical tool panel (13 buttons + 1 divider).
fn tool_panel_dims() -> (i32, i32) {
    let n   = TOOL_PANEL_BUTTONS.len() as i32;
    let h_buttons = n * PANEL_BTN + (n - 1) * PANEL_GAP;
    // A 1-px divider sits between the 9 tool buttons and the 4 utility
    // buttons (color / thickness / undo / redo). Two extra group-gaps add
    // breathing room above and below the divider line.
    let extra = PANEL_GROUP_GAP * 2 + 1;
    let w = PANEL_BTN + PANEL_PAD * 2;
    let h = h_buttons + extra + PANEL_PAD * 2;
    (w, h)
}

fn action_panel_dims(buttons: &[ActionButton]) -> (i32, i32) {
    let n = buttons.len() as i32;
    let w = n * PANEL_BTN + (n - 1) * PANEL_GAP + PANEL_PAD * 2;
    let h = PANEL_BTN + PANEL_PAD * 2;
    (w, h)
}

/// Frame-space origin of the tool panel. Right of selection by default;
/// flips to the left if the right edge would clip; rounds inward to keep
/// the panel fully inside `bounds`.
///
/// `bounds` is the monitor the selection sits on (see `Overlay::panel_bounds`),
/// not the whole framebuffer — on a multi-monitor overlay clamping against
/// the framebuffer would let the panel drift onto the monitor next door
/// instead of flipping to the other side of the selection.
fn tool_panel_origin(bounds: (i32, i32, i32, i32), sel: (i32, i32, i32, i32)) -> (i32, i32) {
    let (bx, by, bw, bh) = bounds;
    let (sx, sy, sw, sh) = sel;
    let (pw, ph) = tool_panel_dims();
    let mut tx = sx + sw + 5;
    // Bottom-aligned with the selection, beside the action row: on a short
    // capture the column grows upward past it rather than hanging far
    // below it.
    let mut ty = sy + sh - ph;
    if tx + pw > bx + bw { tx = sx - pw - 5; }
    if ty + ph > by + bh { ty = by + bh - ph; }
    (tx.max(bx), ty.max(by))
}

// ── video timeline bar layout (video mode only) ────────────────────────────

/// Point-in-rect test shared by the timeline bar + duration chip.
fn rect_contains((x, y, w, h): (i32, i32, i32, i32), (cx, cy): (i32, i32)) -> bool {
    cx >= x && cx < x + w && cy >= y && cy < y + h
}

/// Bottom-center timeline bar, video mode only. Centered inside `bounds`
/// so it lands on one monitor rather than straddling the seam between two.
fn timeline_bar_rect(bounds: (i32, i32, i32, i32)) -> (i32, i32, i32, i32) {
    let (bx, by, bw, bh) = bounds;
    let w = TIMELINE_MAX_W.min(bw - TIMELINE_MARGIN * 2).max(120);
    let x = bx + (bw - w) / 2;
    let y = (by + bh - TIMELINE_MARGIN - TIMELINE_H).max(by);
    (x, y, w, TIMELINE_H)
}

/// Duration chip — right end of the bar, inside it, so it can never clip
/// the screen edge regardless of window size.
fn timeline_chip_rect(bar: (i32, i32, i32, i32)) -> (i32, i32, i32, i32) {
    let (bx, by, bw, bh) = bar;
    (bx + bw - TIMELINE_PAD - TIMELINE_CHIP_W,
     by + (bh - TIMELINE_CHIP_H) / 2,
     TIMELINE_CHIP_W, TIMELINE_CHIP_H)
}

/// Seek track between the clock readout and the chip. The readout slot is
/// measured from the worst-case "total / total" label so the track does
/// not jitter as the current-time digits change while scrubbing.
fn timeline_track(bar: (i32, i32, i32, i32), total_secs: f32) -> (i32, i32, i32, i32) {
    let (bx, by, _bw, bh) = bar;
    let label   = format!("{0} / {0}", format_timecode(total_secs));
    let label_w = crate::bitmap_font::measure(&label, 2);
    let tx   = bx + TIMELINE_PAD + label_w + 10;
    let chip = timeline_chip_rect(bar);
    let tw   = (chip.0 - 8 - tx).max(40);
    let ty   = by + (bh - TIMELINE_TRACK_H) / 2;
    (tx, ty, tw, TIMELINE_TRACK_H)
}

/// "M:SS" clock readout (minutes unpadded; hours roll into minutes —
/// recordings are short).
fn format_timecode(secs: f32) -> String {
    let s = secs.max(0.0).round() as u64;
    format!("{}:{:02}", s / 60, s % 60)
}

/// `[start, end)` containment for the timeline preview. `None` = whole
/// clip; an `f32::INFINITY` end means "until the clip ends".
fn annotation_visible_at(a: &Annotation, t: f32) -> bool {
    match a.time { None => true, Some((s, e)) => t >= s && t < e }
}

/// Seeking at (or past) the clip's end yields no frame from ffmpeg, so
/// extraction times are nudged just inside the clip.
fn clamp_extract_t(t: f32, dur: f32) -> f32 { t.min((dur - 0.05).max(0.0)) }

/// Window stamped on a new annotation: start = the scrub position, end =
/// start + the chip preset. `None` = whole clip. The start is nudged
/// inside the clip with `clamp_extract_t` because the seek track clamps
/// to [0, dur] — a stamp at exactly `dur` would gate the burn on
/// gte(t, dur), which no frame ever reaches (the last frame's timestamp
/// is one frame-duration short of the container length), silently
/// dropping ink the preview showed on the displayed (already-nudged)
/// final frame. A preset that runs past the clip's end means the same as
/// "End" — open-ended, so the burn emits gte() instead of trusting the
/// parsed duration to the centisecond.
fn stamp_window(scrub_pos: f32, dur: f32, preset: Option<f32>) -> Option<(f32, f32)> {
    let start = clamp_extract_t(scrub_pos, dur);
    let end = match preset {
        Some(d) if start + d < dur => start + d,
        _ => f32::INFINITY,
    };
    if start == 0.0 && end == f32::INFINITY { return None; }
    Some((start, end))
}

/// Group annotations into runs of identical visibility windows —
/// consecutive only, so the chained overlays composite in exactly the
/// editor's draw order. Merging across runs would re-order ink: with
/// A(whole-clip) B(timed) C(whole-clip), folding C into A's group puts
/// B's overlay above C in the burn while the preview painted C on top.
/// Interleaved sequences cost one extra overlay input per run; the
/// zero-interaction default is still a single run, so the byte-identical
/// pre-timeline filtergraph is preserved.
fn group_by_window(annotations: &[Annotation]) -> Vec<(Option<(f32, f32)>, Vec<&Annotation>)> {
    let mut groups: Vec<(Option<(f32, f32)>, Vec<&Annotation>)> = Vec::new();
    for a in annotations {
        match groups.last_mut() {
            Some((k, list)) if *k == a.time => list.push(a),
            _ => groups.push((a.time, vec![a])),
        }
    }
    groups
}

/// Video-mode counterpart of `compose_final`: render the annotations into
/// fully-transparent RGBA buffers the size of the frame, leaving untouched
/// pixels transparent — one buffer per run of identical (start, end)
/// visibility windows. ffmpeg then overlays each buffer on the clip, gated
/// to its window with `enable=`.
///
/// The painter's `blend` pre-composites semi-transparent strokes against
/// whatever `read()` returns, so we can't render straight onto
/// transparency — Marker would come out as an opaque dark band. Instead
/// `DiffSurface` reads from (and writes to) a live copy of the frame — so
/// blending and Pixelate sampling behave exactly like the screenshot
/// editor — while mirroring every touched pixel, opaque, into the
/// transparent out-buffer. WYSIWYG with one caveat: Marker/Pixelate
/// pixels carry frame content frozen from the annotated frame.
///
/// Runs on the burn worker thread (see `burn_annotations`) — each group
/// whose start differs from the committed scrub frame costs a synchronous
/// ffmpeg seek for its pristine background, which must not stall the
/// event loop. Extractions are deduplicated by start time so runs sharing
/// a position pay for one seek, not one per run.
pub(crate) fn compose_overlay_groups(commit: &VideoCommit, video: &std::path::Path) -> Vec<OverlayGroup> {
    let (w, h) = (commit.frame.width(), commit.frame.height());
    let mut groups = group_by_window(&commit.annotations);
    // No annotations → keep today's behavior: one fully-transparent
    // whole-clip overlay; the burn still produces the _annotated copy.
    if groups.is_empty() { groups.push((None, Vec::new())); }

    let mut extracted: Vec<(f32, ImageBuffer<Rgba<u8>, Vec<u8>>)> = Vec::new();
    let mut out_groups = Vec::with_capacity(groups.len());
    for (window, list) in groups {
        let start = window.map(|(s, _)| s).unwrap_or(0.0);
        // Pristine background = the frame at the group's START, so
        // Marker blending + Pixelate sampling are faithful to the
        // moment the ink first appears. The committed scrub frame is
        // reused when it already matches; extraction failure falls
        // back to it — losing blend fidelity for one group beats
        // failing the whole burn.
        let pristine = if start == commit.frame_t {
            commit.frame.clone()
        } else if let Some((_, f)) = extracted.iter().find(|(t, _)| *t == start) {
            f.clone()
        } else {
            let t = commit.duration.map(|d| clamp_extract_t(start, d)).unwrap_or(start);
            match crate::tray_loop::extract_frame_at(video, t) {
                Ok(frame) => {
                    extracted.push((start, frame.clone()));
                    frame
                }
                Err(e) => {
                    eprintln!("framefang: group frame extract failed: {e}");
                    commit.frame.clone()
                }
            }
        };
        let mut frame = pristine.clone();
        let mut out   = ImageBuffer::from_pixel(w, h, Rgba([0, 0, 0, 0]));
        let mut surf  = DiffSurface { frame: &mut frame, out: &mut out };
        for a in list {
            painter::render_annotation(&mut surf, a, Some(&pristine));
        }
        let end = window.and_then(|(_, e)| e.is_finite().then_some(e));
        out_groups.push((out, start, end));
    }
    out_groups
}

fn action_panel_origin(
    bounds: (i32, i32, i32, i32),
    sel: (i32, i32, i32, i32),
    buttons: &[ActionButton],
) -> (i32, i32) {
    let (bx, by, _bw, bh) = bounds;
    let (sx, sy, sw, sh) = sel;
    let (pw, ph) = action_panel_dims(buttons);
    let mut ax = sx + sw - pw;
    let mut ay = sy + sh + 5;
    if ay + ph > by + bh { ay = sy - ph - 5; }
    if ax < bx { ax = sx; }
    (ax.max(bx), ay.max(by))
}

/// Rectangle for the i-th tool-panel button. Index above the divider
/// position skips one slot to leave room for the line.
fn tool_panel_button_rect(panel_origin: (i32, i32), idx: i32) -> (i32, i32, i32, i32) {
    let (ox, oy) = panel_origin;
    let x = ox + PANEL_PAD;
    // Indices 0..9 are the 9 tools. Index 9..13 are utility buttons that
    // sit *below* the divider (extra group gap + 1px line + group gap).
    let above_divider = idx < 9;
    let extra = if above_divider { 0 } else { PANEL_GROUP_GAP * 2 + 1 };
    let y = oy + PANEL_PAD + idx * (PANEL_BTN + PANEL_GAP) + extra;
    (x, y, x + PANEL_BTN, y + PANEL_BTN)
}

fn action_panel_button_rect(panel_origin: (i32, i32), idx: i32) -> (i32, i32, i32, i32) {
    let (ox, oy) = panel_origin;
    let x = ox + PANEL_PAD + idx * (PANEL_BTN + PANEL_GAP);
    let y = oy + PANEL_PAD;
    (x, y, x + PANEL_BTN, y + PANEL_BTN)
}

fn tool_panel_hit(panel_origin: (i32, i32), (cx, cy): (i32, i32)) -> Option<(usize, ToolPanelButton)> {
    for (i, b) in TOOL_PANEL_BUTTONS.iter().enumerate() {
        let (x0, y0, x1, y1) = tool_panel_button_rect(panel_origin, i as i32);
        if cx >= x0 && cx < x1 && cy >= y0 && cy < y1 {
            return Some((i, *b));
        }
    }
    None
}

fn action_panel_hit(
    panel_origin: (i32, i32),
    (cx, cy): (i32, i32),
    buttons: &[ActionButton],
) -> Option<ActionButton> {
    for (i, b) in buttons.iter().enumerate() {
        let (x0, y0, x1, y1) = action_panel_button_rect(panel_origin, i as i32);
        if cx >= x0 && cx < x1 && cy >= y0 && cy < y1 {
            return Some(*b);
        }
    }
    None
}

// ── drawing ─────────────────────────────────────────────────────────────────

fn draw_tool_panel(
    buf:        &mut [u32],
    win_w:      usize,
    win_h:      usize,
    bounds:     (i32, i32, i32, i32),
    sel:        (i32, i32, i32, i32),
    // The tool in hand, if any: no button is lit while none is.
    active:     Option<Tool>,
) {
    const BG:         u32 = 0x00_22_22_24;
    const BTN:        u32 = 0x00_2E_2E_32;
    const BTN_ACTIVE: u32 = 0x00_64_95_ED;
    const TEXT:       u32 = 0x00_E8_E8_EC;
    const DIVIDER:    u32 = 0x00_44_44_48;

    let (ox, oy) = tool_panel_origin(bounds, sel);
    let (pw, ph) = tool_panel_dims();
    draw_rounded_rect(buf, win_w, win_h, ox, oy, ox + pw, oy + ph, PANEL_RADIUS, BG);

    // Divider sits between buttons 8 (Pixelate) and 9 (Undo).
    let div_y = oy + PANEL_PAD + 9 * (PANEL_BTN + PANEL_GAP) + PANEL_GROUP_GAP;
    draw_filled_rect(buf, win_w, win_h, ox + 6, div_y, ox + pw - 6, div_y + 1, DIVIDER);

    for (i, b) in TOOL_PANEL_BUTTONS.iter().enumerate() {
        let (x0, y0, x1, y1) = tool_panel_button_rect((ox, oy), i as i32);
        let highlight = match b {
            ToolPanelButton::Tool(t) => active == Some(*t),
            _ => false,
        };
        let bg = if highlight { BTN_ACTIVE } else { BTN };
        draw_rounded_rect(buf, win_w, win_h, x0, y0, x1, y1, 6, bg);
        match b {
            ToolPanelButton::Tool(t)    => crate::icons::render_icon(buf, win_w, win_h, x0, y0, x1, y1, crate::icons::IconKind::Tool(*t), [232,232,236,255], None),
            ToolPanelButton::Undo       => crate::icons::render_icon(buf, win_w, win_h, x0, y0, x1, y1, crate::icons::IconKind::Undo, [232,232,236,255], None),
            ToolPanelButton::Redo       => crate::icons::render_icon(buf, win_w, win_h, x0, y0, x1, y1, crate::icons::IconKind::Redo, [232,232,236,255], None),
        }
    }
}

/// Sequential orange-neon halo around the tool-panel buttons, cycling
/// top-to-bottom one button per second. Each button gets a brief glow
/// window of ~0.85 s out of every (N × 1 s) cycle, where N is the number
/// of buttons in `TOOL_PANEL_BUTTONS`. Halo is a 4-ring outer glow that
/// fades with distance so the icon glyph stays readable through it.
///
/// Visual mirror of the website's `.tool-glow` CSS animation — same
/// orange palette, same ~1 s active window, same slow loop.
fn draw_tool_panel_glow(
    buf:     &mut [u32],
    win_w:   usize,
    win_h:   usize,
    bounds:  (i32, i32, i32, i32),
    sel:     (i32, i32, i32, i32),
    elapsed: f32,
) {
    let n = TOOL_PANEL_BUTTONS.len() as f32;       // 13
    let slot = 1.0_f32;                            // 1 second per button
    let cycle = n * slot;                          // 13 s total
    let t = elapsed.rem_euclid(cycle);
    let active_idx = (t / slot).floor() as i32;
    // Fraction-through-slot, 0..1.
    let phase = (t - active_idx as f32 * slot) / slot;
    // On for the first 85 % of the slot, off for the trailing 15 % so the
    // glow has a visible "step" between buttons.
    if phase > 0.85 { return; }
    // Triangular envelope inside the on-window: rises fast, peaks ~mid,
    // fades. Keeps the transition smooth at slot boundaries.
    let env = if phase < 0.15 { phase / 0.15 }
              else if phase < 0.55 { 1.0 }
              else { ((0.85 - phase) / 0.30).max(0.0) };

    let panel_origin = tool_panel_origin(bounds, sel);
    let (x0, y0, x1, y1) = tool_panel_button_rect(panel_origin, active_idx);

    // Orange-neon palette. Outer rings are progressively fainter so the halo
    // reads as a soft glow rather than a hard frame around the button.
    let orange_r: u32 = 0xFF;
    let orange_g: u32 = 0x7A;
    let orange_b: u32 = 0x1A;
    let base_alpha = (210.0 * env).round().clamp(0.0, 255.0) as u32;
    // The halo stays inside the panel's own padding: in the editor window
    // anything past the panel's edge would land on the cut-away canvas, and
    // a glow blended over that reads as a dark box, not light.
    for ring in 0..PANEL_PAD {
        let inset    = 1 + ring;
        let layer_a  = (base_alpha / (1 + ring as u32 / 2)).min(255);
        if layer_a == 0 { continue; }
        let rx0 = x0 - inset;
        let ry0 = y0 - inset;
        let rx1 = x1 + inset - 1;
        let ry1 = y1 + inset - 1;
        blend_rect_outline(buf, win_w, win_h, rx0, ry0, rx1, ry1,
                           orange_r, orange_g, orange_b, layer_a);
    }
}

/// A neon outline breathing around the action panel when it first appears,
/// so the eye finds where Save / Copy / Pin landed: three slow, smooth
/// breaths (`PULSE_HZ`), each rising from dark to full and back, so it ends
/// dark rather than cutting off. Brandon found the old 6.5 Hz strobe far too
/// fast.
///
/// Layout: a 3-pixel stroke hugging the panel, plus 5 fainter outer rings for
/// the glow where they can blend over the frozen screen.
fn draw_action_panel_pulse(
    buf:    &mut [u32],
    win_w:  usize,
    win_h:  usize,
    bounds: (i32, i32, i32, i32),
    sel:    (i32, i32, i32, i32),
    elapsed: f32,
    buttons: &[ActionButton],
    // The outer rings, and see-through dimming of the stroke. Off in the
    // editor window, where anything blended past the panel lands on the
    // cut-away canvas and reads as a dark box; there the stroke stays solid
    // and dims by brightness instead, like a neon tube.
    halo: bool,
) {
    if elapsed >= PULSE_SECS { return; }
    let level = pulse_level(elapsed);
    let origin = action_panel_origin(bounds, sel, buttons);
    let (pw, ph) = action_panel_dims(buttons);

    let (gr, gg, gb) = ((NEON_GREEN >> 16) & 0xFF, (NEON_GREEN >> 8) & 0xFF, NEON_GREEN & 0xFF);
    let (laser_r, laser_g, laser_b, stroke_a) = if halo {
        (gr, gg, gb, (240.0 * level).round() as u32)
    } else {
        // Nearly dark: draw nothing, so the canvas there is cut away again.
        if level < 0.06 { return; }
        let dim = |c: u32| (c as f32 * level).round() as u32;
        (dim(gr), dim(gg), dim(gb), 255)
    };
    let halo_a = (130.0 * level).round() as u32;

    // 3-px stroke directly hugging the panel.
    for inset in 1..=3 {
        let x0 = origin.0 - inset;
        let y0 = origin.1 - inset;
        let x1 = origin.0 + pw + inset;
        let y1 = origin.1 + ph + inset;
        blend_rect_outline(buf, win_w, win_h, x0, y0, x1, y1,
                           laser_r, laser_g, laser_b, stroke_a);
    }
    // 5-ring outer halo — each ring is fainter than the last.
    if !halo { return; }
    for ring in 0..5 {
        let inset    = 4 + ring;
        let layer_a  = (halo_a / (1 + ring as u32)).min(255);
        let x0       = origin.0 - inset;
        let y0       = origin.1 - inset;
        let x1       = origin.0 + pw + inset;
        let y1       = origin.1 + ph + inset;
        blend_rect_outline(buf, win_w, win_h, x0, y0, x1, y1,
                           laser_r, laser_g, laser_b, layer_a);
    }
}

/// The pulse's brightness at `t` seconds, 0 to 1: a raised cosine, dark at
/// the start and the end of every breath, full at the middle.
fn pulse_level(t: f32) -> f32 {
    if !(0.0..PULSE_SECS).contains(&t) { return 0.0; }
    0.5 - 0.5 * (std::f32::consts::TAU * PULSE_HZ * t).cos()
}

/// Source-over blend a 1-px rectangular outline of `(r,g,b,a)` onto the
/// XRGB softbuffer. Clipped at the buffer bounds.
fn blend_rect_outline(
    buf: &mut [u32], win_w: usize, win_h: usize,
    x0: i32, y0: i32, x1: i32, y1: i32,
    r: u32, g: u32, b: u32, a: u32,
) {
    if a == 0 { return; }
    let inv = 255 - a;
    let plot = |buf: &mut [u32], x: i32, y: i32| {
        if x < 0 || y < 0 { return; }
        let (xu, yu) = (x as usize, y as usize);
        if xu >= win_w || yu >= win_h { return; }
        let idx = yu * win_w + xu;
        let cur = buf[idx];
        let dr  = (cur >> 16) & 0xFF;
        let dg  = (cur >>  8) & 0xFF;
        let db  =  cur        & 0xFF;
        let nr  = ((r * a + dr * inv + 127) / 255).min(255);
        let ng  = ((g * a + dg * inv + 127) / 255).min(255);
        let nb  = ((b * a + db * inv + 127) / 255).min(255);
        buf[idx] = (nr << 16) | (ng << 8) | nb;
    };
    for x in x0..=x1 { plot(buf, x, y0); plot(buf, x, y1); }
    for y in y0..=y1 { plot(buf, x0, y); plot(buf, x1, y); }
}

fn draw_action_panel(
    buf:    &mut [u32],
    win_w:  usize,
    win_h:  usize,
    bounds: (i32, i32, i32, i32),
    sel:    (i32, i32, i32, i32),
    buttons: &[ActionButton],
) {
    const BG:   u32 = 0x00_22_22_24;
    const BTN:  u32 = 0x00_2E_2E_32;
    const TEXT: u32 = 0x00_E8_E8_EC;
    // Record red, the same hue as the REC dot in `recording_indicator`, so the
    // one destructive-ish action in the row is never confused for Save.
    const REC_DOT: [u8; 4] = [0xFF, 0x3A, 0x3A, 0xFF];

    let origin = action_panel_origin(bounds, sel, buttons);
    let (pw, ph) = action_panel_dims(buttons);
    draw_rounded_rect(buf, win_w, win_h, origin.0, origin.1, origin.0 + pw, origin.1 + ph, PANEL_RADIUS, BG);

    for (i, b) in buttons.iter().enumerate() {
        let (x0, y0, x1, y1) = action_panel_button_rect(origin, i as i32);
        draw_rounded_rect(buf, win_w, win_h, x0, y0, x1, y1, 6, BTN);
        match b {
            ActionButton::Pin    => crate::icons::render_icon(buf, win_w, win_h, x0, y0, x1, y1, crate::icons::IconKind::Pin,    [232,232,236,255], None),
            ActionButton::Copy   => crate::icons::render_icon(buf, win_w, win_h, x0, y0, x1, y1, crate::icons::IconKind::Copy,   [232,232,236,255], None),
            ActionButton::Save   => crate::icons::render_icon(buf, win_w, win_h, x0, y0, x1, y1, crate::icons::IconKind::Save,   [232,232,236,255], None),
            ActionButton::Record => crate::icons::render_icon(buf, win_w, win_h, x0, y0, x1, y1, crate::icons::IconKind::Record, [232,232,236,255], Some(REC_DOT)),
            ActionButton::Close  => crate::icons::render_icon(buf, win_w, win_h, x0, y0, x1, y1, crate::icons::IconKind::Close,  [232,232,236,255], None),
        }
    }
}

/// The tool flyout, sliding out from behind the tool panel.
///
/// Drawn whole into a scratch buffer, then `open` (0 → 1, eased) of it is
/// copied out next to the panel, the edge nearest the panel last, so it
/// reads as a drawer pulled out from under the column rather than a box
/// that fades in. Each size row: a muted label, a track, the value.
/// Thickness gets a wedge track that widens to the right; the rest a plain
/// bar. The part up to the knob is in the tool's color. Below them, the
/// palette's name between two arrows and its 16 colors, the tool's own
/// color ringed.
struct FlyoutFrame {
    rect: Rect4,
    on_right: bool,
    /// The tool button's center, as a y offset from the flyout's top.
    pointer_y: i32,
    layout: FlyoutLayout,
    /// One value per `layout.knobs` row.
    values: Vec<f32>,
    color: framefang_core::color::Rgba,
    palette: usize,
    /// 0 → 1, eased.
    open: f32,
    /// The eyedropper is armed: its button shows it held.
    picking: bool,
    /// Where the color was last picked in the spectrum, for its marker.
    spectrum_pick: Option<(f32, f32)>,
}

fn draw_flyout(buf: &mut [u32], win_w: usize, win_h: usize, f: &FlyoutFrame) {
    const BG:      u32 = 0x00_22_22_24;   // the tool panel's own surface
    const BTN:     u32 = 0x00_2E_2E_32;   // the tool panel's buttons
    const BTN_ARMED: u32 = 0x00_64_95_ED; // the tool panel's picked tool
    const GROOVE:  u32 = 0x00_14_14_17;
    const REST:    u32 = 0x00_44_44_4A;
    const EDGE:    u32 = 0x00_3A_3A_40;
    const LABEL:   u32 = 0x00_9C_9C_A4;
    const VALUE:   u32 = 0x00_E8_E8_EC;
    const KNOB:    u32 = 0x00_E8_E8_EC;
    /// Not a color: marks the scratch pixels outside the rounded corners.
    const NONE:    u32 = 0xFF00_0000;

    let (fx, fy, fw, fh) = f.rect;
    if fw <= 0 || fh <= 0 { return; }
    let (sw, sh) = (fw as usize, fh as usize);
    let mut s = vec![NONE; sw * sh];
    draw_rounded_rect(&mut s, sw, sh, 0, 0, fw, fh, PANEL_RADIUS, BG);
    let rgb = |c: framefang_core::color::Rgba| ((c.r as u32) << 16) | ((c.g as u32) << 8) | c.b as u32;
    let ink = rgb(f.color);
    let text = |c: u32| framefang_core::color::Rgba::new((c >> 16) as u8, (c >> 8) as u8, c as u8, 0xFF);
    // Text in the flyout's own type, vertically centered in `r`.
    let write = |s: &mut [u32], r: Rect4, x: f32, words: &str, c: u32| {
        let mut surf = crate::painter::U32Surface { buf: s, stride: fw, height: fh };
        let y = r.1 as f32 + (r.3 as f32 - framefang_core::text::line_height(FLYOUT_TEXT_PX)) / 2.0;
        crate::painter::draw_unicode_text(&mut surf, x, y, FLYOUT_TEXT_PX, words, text(c));
    };
    let width = |words: &str| framefang_core::text::layout(words, FLYOUT_TEXT_PX).width();

    if let Some((prev, next, name, swatches)) = &f.layout.colors {
        for (r, left) in [(prev, true), (next, false)] {
            draw_rounded_rect(&mut s, sw, sh, r.0, r.1, r.0 + r.2, r.1 + r.3, 5, BTN);
            draw_chevron(&mut s, sw, sh, r.0, r.1, r.0 + r.2, r.1 + r.3, left);
        }
        let pal = framefang_core::annotation::Palettes::get(f.palette);
        write(&mut s, *name, name.0 as f32 + (name.2 as f32 - width(pal.name)) / 2.0, pal.name, LABEL);
        let mut chosen = None;
        for (i, r) in swatches.iter().enumerate() {
            let c = pal.colors[i];
            // A hairline edge so black and near-panel colors still read.
            draw_rounded_rect(&mut s, sw, sh, r.0 - 1, r.1 - 1, r.0 + r.2 + 1, r.1 + r.3 + 1, 5, EDGE);
            draw_rounded_rect(&mut s, sw, sh, r.0, r.1, r.0 + r.2, r.1 + r.3, 4, rgb(c));
            if c == f.color { chosen = Some(*r); }
        }
        // Any other color. The eyedropper, lit while it is armed.
        if let Some(c) = &f.layout.custom {
            let r = c.eyedropper;
            draw_rounded_rect(&mut s, sw, sh, r.0, r.1, r.0 + r.2, r.1 + r.3, 5, if f.picking { BTN_ARMED } else { BTN });
            crate::icons::render_icon(&mut s, sw, sh, r.0, r.1, r.0 + r.2, r.1 + r.3,
                                      crate::icons::IconKind::Eyedropper, [0xE8, 0xE8, 0xEC, 0xFF], None);
            // The tool's color below it, framed like a swatch.
            let r = c.current;
            draw_rounded_rect(&mut s, sw, sh, r.0 - 1, r.1 - 1, r.0 + r.2 + 1, r.1 + r.3 + 1, 5, EDGE);
            draw_rounded_rect(&mut s, sw, sh, r.0, r.1, r.0 + r.2, r.1 + r.3, 4, ink);
            // The spectrum, framed with the swatches' hairline.
            let (x0, y0, w, h) = c.spectrum;
            draw_rounded_rect(&mut s, sw, sh, x0 - 1, y0 - 1, x0 + w + 1, y0 + h + 1, 5, EDGE);
            for y in 0..h {
                for x in 0..w {
                    let p = spectrum_color(x as f32 / (w - 1).max(1) as f32, y as f32 / (h - 1).max(1) as f32);
                    let (px, py) = ((x0 + x) as usize, (y0 + y) as usize);
                    if px < sw && py < sh { s[py * sw + px] = rgb(p); }
                }
            }
            // Its marker, a ringed dot of the color, only while the tool
            // still draws in it.
            if let Some((u, v)) = f.spectrum_pick.filter(|&(u, v)| spectrum_color(u, v) == f.color) {
                let mx = x0 + ((w - 1) as f32 * u).round() as i32;
                let my = y0 + ((h - 1) as f32 * v).round() as i32;
                let mut surf = crate::painter::U32Surface { buf: &mut s, stride: fw, height: fh };
                crate::painter::fill_disc(&mut surf, mx, my, 7, text(GROOVE));
                crate::painter::fill_disc(&mut surf, mx, my, 6, text(VALUE));
                crate::painter::fill_disc(&mut surf, mx, my, 4, f.color);
            }
        }
        // The tool's own color: a white ring with a gap of panel between,
        // on its swatch, or on the current-color chip when it is on none.
        if let Some(r) = chosen.or(f.layout.custom.map(|c| c.current)) {
            draw_rounded_rect(&mut s, sw, sh, r.0 - 3, r.1 - 3, r.0 + r.2 + 3, r.1 + r.3 + 3, 7, VALUE);
            draw_rounded_rect(&mut s, sw, sh, r.0 - 1, r.1 - 1, r.0 + r.2 + 1, r.1 + r.3 + 1, 5, BG);
            draw_rounded_rect(&mut s, sw, sh, r.0, r.1, r.0 + r.2, r.1 + r.3, 4, ink);
        }
    }

    for (&(knob, row), v) in f.layout.knobs.iter().zip(&f.values) {
        let (rx, _, rw, _) = row;
        let (tx, ty, tw, th) = flyout_track(row);
        let (lo, hi) = knob.range();
        let t = ((v - lo) / (hi - lo)).clamp(0.0, 1.0);
        let knob_x = tx + ((tw - 1) as f32 * t).round() as i32;
        let mid = ty as f32 + th as f32 / 2.0;
        for x in tx..tx + tw {
            let f = (x - tx) as f32 / (tw - 1).max(1) as f32;
            // The wedge goes from a hairline to the track's height; a bar
            // is a constant 4 px.
            let h = if knob == Knob::Thickness { 1.0 + f * (th as f32 - 3.0) } else { 4.0 };
            let (y0, y1) = ((mid - h / 2.0).round() as i32, (mid + h / 2.0).round() as i32);
            let c = if x <= knob_x { ink } else if knob == Knob::Thickness { REST } else { GROOVE };
            draw_filled_rect(&mut s, sw, sh, x, y0, x + 1, y1.max(y0 + 1), c);
        }
        let kx = (knob_x - FLYOUT_KNOB_W / 2).clamp(tx, tx + tw - FLYOUT_KNOB_W);
        draw_rounded_rect(&mut s, sw, sh, kx, ty - 3, kx + FLYOUT_KNOB_W, ty + th + 3, 3, KNOB);

        // Smooth type, the face the Text tool draws with, rather than the
        // 5x7 pixel font.
        write(&mut s, row, (rx + 2) as f32, knob.label(), LABEL);
        let value = knob.readout(*v);
        write(&mut s, row, (rx + rw) as f32 - width(&value), &value, VALUE);
    }

    // Copy out the revealed part: the far edge first, sliding out from
    // under the panel.
    let shown = ((fw as f32) * f.open.clamp(0.0, 1.0)).round() as i32;
    if shown <= 0 { return; }
    let (src_x0, dst_x0) = if f.on_right { (fw - shown, fx) } else { (0, fx + fw - shown) };
    for y in 0..fh {
        let dy = fy + y;
        if dy < 0 || dy as usize >= win_h { continue; }
        for x in 0..shown {
            let dx = dst_x0 + x;
            if dx < 0 || dx as usize >= win_w { continue; }
            let p = s[y as usize * sw + (src_x0 + x) as usize];
            if p != NONE { buf[dy as usize * win_w + dx as usize] = p; }
        }
    }

    // Once it is out, a small pointer across the gap, level with the tool's
    // button, so the flyout reads as belonging to that button.
    if f.open < 0.98 { return; }
    let cy = fy + f.pointer_y.clamp(PANEL_RADIUS + FLYOUT_POINTER, fh - PANEL_RADIUS - FLYOUT_POINTER);
    for d in 0..FLYOUT_POINTER {
        let x = if f.on_right { fx - 1 - d } else { fx + fw + d };
        let half = FLYOUT_POINTER - 1 - d;
        draw_filled_rect(buf, win_w, win_h, x, cy - half, x + 1, cy + half + 1, BG);
    }
}


/// Video-mode timeline bar: "current / total" clock, seek track with
/// elapsed fill + playhead, laser-green ticks at each timed annotation's
/// start, and the annotation-duration chip. Chrome matches the marker
/// opacity slider so the two widgets read as one family.
fn draw_timeline_bar(
    buf:        &mut [u32],
    win_w:      usize,
    win_h:      usize,
    bounds:     (i32, i32, i32, i32),
    scrub:      f32,
    total:      f32,
    chip_label: &str,
    tick_times: &[f32],
) {
    const BG:       u32 = 0x00_22_22_24;
    const BTN:      u32 = 0x00_2E_2E_32;
    const TRACK_BG: u32 = 0x00_0A_0A_0E;     // void-black groove
    const BORDER:   u32 = NEON_GREEN;
    const FILL:     u32 = 0x00_64_95_ED;     // elapsed portion, selection blue
    const KNOB:     u32 = 0x00_FF_FF_FF;
    const TICK:     u32 = NEON_GREEN;        // annotation starts
    const TEXT:     u32 = 0x00_E8_E8_EC;

    let bar = timeline_bar_rect(bounds);
    let (bx, by, bw, bh) = bar;
    draw_rounded_rect(buf, win_w, win_h, bx, by, bx + bw, by + bh, PANEL_RADIUS, BG);
    draw_rect_border(buf, win_w, win_h, bx, by, bx + bw, by + bh, BORDER);

    // Clock readout, left slot.
    let label = format!("{} / {}", format_timecode(scrub), format_timecode(total));
    let ly = by + (bh - crate::bitmap_font::GLYPH_H * 2) / 2;
    {
        let mut surf = crate::painter::U32Surface { buf, stride: win_w as i32, height: win_h as i32 };
        crate::painter::draw_text(&mut surf, bx + TIMELINE_PAD, ly, 2, &label,
            framefang_core::color::Rgba::new(
                ((TEXT >> 16) & 0xFF) as u8,
                ((TEXT >>  8) & 0xFF) as u8,
                ( TEXT        & 0xFF) as u8,
                0xFF,
            ));
    }

    // Seek track: groove, elapsed fill, frame.
    let (tx, ty, tw, th) = timeline_track(bar, total);
    draw_filled_rect(buf, win_w, win_h, tx, ty, tx + tw, ty + th, TRACK_BG);
    let frac   = if total > 0.0 { (scrub / total).clamp(0.0, 1.0) } else { 0.0 };
    let head_x = tx + ((tw - 1) as f32 * frac).round() as i32;
    draw_filled_rect(buf, win_w, win_h, tx, ty, head_x, ty + th, FILL);
    draw_rect_border(buf, win_w, win_h, tx, ty, tx + tw, ty + th, BORDER);

    // Annotation start ticks — full-height lines through the groove so
    // the user can find their timed ink without scrubbing for it.
    for &t in tick_times {
        let frac = if total > 0.0 { (t / total).clamp(0.0, 1.0) } else { 0.0 };
        let xn = tx + ((tw - 1) as f32 * frac).round() as i32;
        for y in (ty + 1)..(ty + th - 1) {
            if xn >= 0 && (xn as usize) < win_w && y >= 0 && (y as usize) < win_h {
                buf[y as usize * win_w + xn as usize] = TICK;
            }
        }
    }

    // Playhead knob — taller than the groove like the marker knob.
    let kx = (head_x - TIMELINE_KNOB_W / 2).clamp(tx - 1, tx + tw - TIMELINE_KNOB_W + 1);
    let knob_h = th + 6;
    let ky = ty + (th - knob_h) / 2;
    draw_filled_rect(buf, win_w, win_h, kx, ky, kx + TIMELINE_KNOB_W, ky + knob_h, KNOB);
    draw_rect_border(buf, win_w, win_h, kx, ky, kx + TIMELINE_KNOB_W, ky + knob_h, BORDER);

    // Duration chip, right slot.
    let (cx, cy, cw, ch) = timeline_chip_rect(bar);
    draw_rounded_rect(buf, win_w, win_h, cx, cy, cx + cw, cy + ch, 6, BTN);
    draw_rect_border(buf, win_w, win_h, cx, cy, cx + cw, cy + ch, BORDER);
    let text_w = crate::bitmap_font::measure(chip_label, 2);
    let text_x = cx + (cw - text_w) / 2;
    let text_y = cy + (ch - crate::bitmap_font::GLYPH_H * 2) / 2;
    let mut surf = crate::painter::U32Surface { buf, stride: win_w as i32, height: win_h as i32 };
    crate::painter::draw_text(&mut surf, text_x, text_y, 2, chip_label, framefang_core::color::Rgba::WHITE);
}

fn draw_chevron(
    buf: &mut [u32], stride: usize, height: usize,
    x0: i32, y0: i32, x1: i32, y1: i32, left: bool,
) {
    let cx = (x0 + x1) / 2;
    let cy = (y0 + y1) / 2;
    let line = |buf: &mut [u32], mut sx0: i32, mut sy0: i32, ex: i32, ey: i32| {
        let dx =  (ex - sx0).abs();
        let dy = -(ey - sy0).abs();
        let stepx = if sx0 < ex { 1 } else { -1 };
        let stepy = if sy0 < ey { 1 } else { -1 };
        let mut err = dx + dy;
        loop {
            if sx0 >= 0 && (sx0 as usize) < stride && sy0 >= 0 && (sy0 as usize) < height {
                buf[sy0 as usize * stride + sx0 as usize] = 0x00_E8_E8_EC;
            }
            if sx0 == ex && sy0 == ey { break; }
            let e2 = 2 * err;
            if e2 >= dy { err += dy; sx0 += stepx; }
            if e2 <= dx { err += dx; sy0 += stepy; }
        }
    };
    if left {
        line(buf, cx + 4, cy - 6, cx - 4, cy);
        line(buf, cx - 4, cy,     cx + 4, cy + 6);
    } else {
        line(buf, cx - 4, cy - 6, cx + 4, cy);
        line(buf, cx + 4, cy,     cx - 4, cy + 6);
    }
}

/// Returns Some(true) if the prev-arrow button was hit, Some(false) if next.
/// Force focus + keyboard grab onto the given X11 window XID. Without this
/// Cinnamon ignores winit's `_NET_ACTIVE_WINDOW` request and never routes
/// KeyPress events to us, so the Text tool sees nothing.
#[cfg(target_os = "linux")]
fn force_x11_focus(xid: u32) -> anyhow::Result<()> {
    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::{ConnectionExt, GrabMode, InputFocus};
    let (conn, _screen) = x11rb::connect(None)
        .map_err(|e| anyhow!("x11rb::connect: {e}"))?;
    conn.set_input_focus(InputFocus::PARENT, xid, x11rb::CURRENT_TIME)
        .map_err(|e| anyhow!("set_input_focus: {e}"))?
        .check()
        .map_err(|e| anyhow!("set_input_focus check: {e}"))?;
    conn.grab_keyboard(false, xid, x11rb::CURRENT_TIME, GrabMode::ASYNC, GrabMode::ASYNC)
        .map_err(|e| anyhow!("grab_keyboard: {e}"))?
        .reply()
        .map_err(|e| anyhow!("grab_keyboard reply: {e}"))?;
    conn.flush().map_err(|e| anyhow!("flush: {e}"))?;
    Ok(())
}

/// Drop the X11 keyboard grab when the overlay closes so the next focused
/// app gets typed input back.
///
/// No-op on Wayland: `push_x11_focus` never took a grab there (the window
/// handle isn't an Xlib one), so there is nothing to release and an X11
/// connection attempt per closed overlay would only be noise.
#[cfg(target_os = "linux")]
fn release_x11_focus() {
    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::ConnectionExt;
    if framefang_platform::session::is_wayland() { return; }
    let Ok((conn, _)) = x11rb::connect(None) else { return; };
    let _ = conn.ungrab_keyboard(x11rb::CURRENT_TIME);
    let _ = conn.flush();
}

/// Small dark pill carrying a button label, drawn near where the user's
/// cursor is hovering. Uses the in-tree 5×7 bitmap font at scale 2 so it
/// stays consistent with the dimension chip and palette header.
fn draw_tooltip(
    buf:    &mut [u32],
    win_w:  usize,
    win_h:  usize,
    bounds: (i32, i32, i32, i32),
    label:  &str,
    x:      i32,
    y:      i32,
) {
    let scale = 2;
    let text_w = crate::bitmap_font::measure(label, scale);
    let text_h = crate::bitmap_font::GLYPH_H * scale;
    let pad_x  = 6;
    let pad_y  = 4;
    let chip_w = text_w + pad_x * 2;
    let chip_h = text_h + pad_y * 2;
    // Auto-flip so the chip stays on the monitor it belongs to.
    let (bx, by, bw, bh) = bounds;
    let mut x0 = x;
    let mut y0 = y;
    if x0 + chip_w > bx + bw { x0 = bx + bw - chip_w - 4; }
    if y0 + chip_h > by + bh { y0 = by + bh - chip_h - 4; }
    if x0 < bx { x0 = bx + 4; }
    if y0 < by { y0 = by + 4; }
    let x1 = x0 + chip_w;
    let y1 = y0 + chip_h;
    draw_filled_rect(buf, win_w, win_h, x0, y0, x1, y1, 0x00_10_10_14);
    draw_rect_border(buf, win_w, win_h, x0, y0, x1, y1, 0x00_4A_4A_50);
    let mut surf = crate::painter::U32Surface { buf, stride: win_w as i32, height: win_h as i32 };
    crate::painter::draw_text(&mut surf, x0 + pad_x, y0 + pad_y, scale, label, framefang_core::color::Rgba::WHITE);
}

/// The one-line hint shown while nothing is selected yet. Plain ASCII —
/// the 5x7 bitmap font substitutes '?' for anything outside 0x20..=0x7E,
/// so no dashes or arrows beyond the keyboard set.
/// The capture screen's own line is its mode's (`PickMode::hint`).
///
/// Record-mode variant: names the confirm key, because there is no Save button
/// to fall back on and the rectangle alone doesn't say how to start.
const RECORD_HINT: &str = "pick the area to record  -  Tab for modes  -  Enter to start  -  Esc to close";

/// Bottom-center hint chip, painted only in `State::Idle`. Same dark fill +
/// hairline border as the tooltip chip so it reads as overlay chrome, but
/// in a muted grey: it's a nudge for first-time users, not something that
/// should pull attention away from the region they're about to pick.
fn draw_idle_hint(
    buf:    &mut [u32],
    win_w:  usize,
    win_h:  usize,
    bounds: (i32, i32, i32, i32),
    hint:   &str,
) {
    const SCALE:  i32 = 2;
    const PAD_X:  i32 = 12;
    const PAD_Y:  i32 = 8;
    const MARGIN: i32 = 48;          // gap from the bottom screen edge

    let (bx, by, bw, bh) = bounds;
    let chip_w = crate::bitmap_font::measure(hint, SCALE) + PAD_X * 2;
    let chip_h = crate::bitmap_font::GLYPH_H * SCALE + PAD_Y * 2;
    // Centered on the monitor the cursor is on, not across the seam of a
    // multi-monitor overlay.
    let x0 = bx + (bw - chip_w) / 2;
    let y0 = by + bh - chip_h - MARGIN;
    // Screens too small to hold the chip get no hint rather than a clipped
    // one — it would land on top of the user's content either way.
    if x0 < bx || y0 < by { return; }
    let x1 = x0 + chip_w;
    let y1 = y0 + chip_h;
    draw_filled_rect(buf, win_w, win_h, x0, y0, x1, y1, 0x00_10_10_14);
    draw_rect_border(buf, win_w, win_h, x0, y0, x1, y1, 0x00_3A_3A_40);
    let mut surf = crate::painter::U32Surface { buf, stride: win_w as i32, height: win_h as i32 };
    crate::painter::draw_text(
        &mut surf, x0 + PAD_X, y0 + PAD_Y, SCALE, hint,
        framefang_core::color::Rgba::new_opaque(0x9A, 0x9A, 0xA2),
    );
}

/// The selection's blue, which the hover highlight shares so a click looks
/// like it simply keeps what was lit.
const SELECTION_BLUE: u32 = 0x00_64_95_ED;

/// The hover highlight: a crisp 2 px frame in the selection's blue with a
/// soft halo outside it, and a label naming what a click would take and how
/// big it is (`size`, the target's own, not the glide's).
fn draw_target(buf: &mut [u32], win_w: usize, win_h: usize, r: Rect4, label: &str, size: (i32, i32)) {
    let (x0, y0, x1, y1) = (r.0, r.1, r.0 + r.2, r.1 + r.3);
    if r.2 <= 0 || r.3 <= 0 { return; }
    let (br, bg, bb) = ((SELECTION_BLUE >> 16) & 0xFF, (SELECTION_BLUE >> 8) & 0xFF, SELECTION_BLUE & 0xFF);
    for (d, a) in [(3, 22), (2, 50), (1, 95)] {
        blend_rect_outline(buf, win_w, win_h, x0 - d, y0 - d, x1 + d, y1 + d, br, bg, bb, a);
    }
    draw_rect_border(buf, win_w, win_h, x0, y0, x1, y1, SELECTION_BLUE);
    draw_rect_border(buf, win_w, win_h, x0 + 1, y0 + 1, x1 - 1, y1 - 1, SELECTION_BLUE);

    // The label: a tab in the frame's blue, sitting on the frame's top edge
    // outside it, so it never covers what is being captured. A target at
    // the top of the screen has no room above; the tab then hangs just
    // inside its top-left corner. The name is white, the size a step
    // quieter.
    const PAD_X: i32 = 10;
    const CHIP_H: i32 = 26;
    let dims = format!("{} x {}", size.0, size.1);
    let tw = |s: &str| framefang_core::text::layout(s, FLYOUT_TEXT_PX).width().ceil() as i32;
    let (name_w, dims_w) = (tw(label), tw(&dims));
    let chip_w = PAD_X * 2 + name_w + 12 + dims_w;
    let (cx, cy) = if y0 - CHIP_H >= 0 { (x0, y0 - CHIP_H) } else { (x0 + 2, y0 + 2) };
    let cx = cx.clamp(0, (win_w as i32 - chip_w).max(0));
    let cy = cy.clamp(0, (win_h as i32 - CHIP_H).max(0));
    draw_filled_rect(buf, win_w, win_h, cx, cy, cx + chip_w, cy + CHIP_H, SELECTION_BLUE);
    let ty = cy as f32 + (CHIP_H as f32 - framefang_core::text::line_height(FLYOUT_TEXT_PX)) / 2.0;
    let mut surf = crate::painter::U32Surface { buf, stride: win_w as i32, height: win_h as i32 };
    let tx = (cx + PAD_X) as f32;
    crate::painter::draw_unicode_text(&mut surf, tx, ty, FLYOUT_TEXT_PX, label,
                                      framefang_core::color::Rgba::new_opaque(0xFF, 0xFF, 0xFF));
    crate::painter::draw_unicode_text(&mut surf, tx + (name_w + 12) as f32, ty, FLYOUT_TEXT_PX, &dims,
                                      framefang_core::color::Rgba::new_opaque(0xD8, 0xE3, 0xFA));
}

/// The capture screen's mode bar: one chip per mode, the current one in the
/// selection's blue, and a quiet `Tab` after them for the key that cycles.
fn draw_mode_bar(
    buf: &mut [u32], win_w: usize, win_h: usize,
    bar: Rect4, chips: &[(crate::pick::PickMode, Rect4)],
    current: crate::pick::PickMode, hovered: Option<crate::pick::PickMode>,
) {
    let (bx, by, bw, bh) = bar;
    draw_filled_rect(buf, win_w, win_h, bx, by, bx + bw, by + bh, 0x00_22_22_24);
    draw_rect_border(buf, win_w, win_h, bx, by, bx + bw, by + bh, 0x00_3A_3A_40);
    let line_h = framefang_core::text::line_height(FLYOUT_TEXT_PX);
    for (m, (x, y, w, h)) in chips {
        let (fill, ink) = if *m == current {
            (SELECTION_BLUE, framefang_core::color::Rgba::new_opaque(0xFF, 0xFF, 0xFF))
        } else if hovered == Some(*m) {
            (0x00_38_38_3E, framefang_core::color::Rgba::new_opaque(0xE8, 0xE8, 0xEC))
        } else {
            (0x00_2E_2E_32, framefang_core::color::Rgba::new_opaque(0xC0, 0xC0, 0xC6))
        };
        draw_filled_rect(buf, win_w, win_h, *x, *y, x + w, y + h, fill);
        let mut surf = crate::painter::U32Surface { buf: &mut *buf, stride: win_w as i32, height: win_h as i32 };
        let tw = framefang_core::text::layout(m.label(), FLYOUT_TEXT_PX).width();
        crate::painter::draw_unicode_text(&mut surf, *x as f32 + (*w as f32 - tw) / 2.0,
                                          *y as f32 + (*h as f32 - line_h) / 2.0, FLYOUT_TEXT_PX, m.label(), ink);
    }
    let key_x = bx + bw - crate::pick::BAR_KEY_W;
    let tw = framefang_core::text::layout("Tab", FLYOUT_TEXT_PX).width();
    let mut surf = crate::painter::U32Surface { buf, stride: win_w as i32, height: win_h as i32 };
    crate::painter::draw_unicode_text(&mut surf, key_x as f32 + (crate::pick::BAR_KEY_W as f32 - tw) / 2.0,
                                      by as f32 + (bh as f32 - line_h) / 2.0, FLYOUT_TEXT_PX, "Tab",
                                      framefang_core::color::Rgba::new_opaque(0x7A, 0x7A, 0x82));
}

/// Bare modifier presses — winit delivers these as ordinary key events, and
/// they must not count as "any other key" while a confirmation is armed:
/// reaching for Ctrl+Z would otherwise silently dismiss the prompt.
fn is_modifier_key(key: &Key) -> bool {
    matches!(
        key,
        Key::Named(
            NamedKey::Shift
                | NamedKey::Control
                | NamedKey::Alt
                | NamedKey::Super
                | NamedKey::Meta
                | NamedKey::Hyper
                | NamedKey::AltGraph
                | NamedKey::CapsLock
                | NamedKey::NumLock
                | NamedKey::ScrollLock
                | NamedKey::Fn
                | NamedKey::FnLock
                | NamedKey::Symbol
                | NamedKey::SymbolLock
        )
    )
}

/// Two-line notice chip pinned to the top center of the overlay. Same dark
/// fill as the tooltip and idle-hint chrome, with a colored hairline so the
/// Esc confirmation (amber) and the select-mode cue (laser green) are
/// distinguishable at a glance. ASCII only — the 5x7 font substitutes '?'
/// for anything outside 0x20..=0x7E. `bounds` is the monitor the chip is
/// centred in (the one under the cursor), so on a multi-monitor desktop it
/// never straddles a seam.
fn draw_notice_bar(
    buf:    &mut [u32],
    win_w:  usize,
    win_h:  usize,
    bounds: (i32, i32, i32, i32),
    lines:  &[&str],
    accent: u32,
    text:   u32,
) {
    const SCALE:  i32 = 2;
    const PAD_X:  i32 = 14;
    const PAD_Y:  i32 = 10;
    const LINE_GAP: i32 = 6;
    const MARGIN: i32 = 36;          // gap from the top screen edge

    if lines.is_empty() { return; }
    let line_h  = crate::bitmap_font::GLYPH_H * SCALE;
    let text_w  = lines.iter()
        .map(|l| crate::bitmap_font::measure(l, SCALE))
        .max()
        .unwrap_or(0);
    let chip_w = text_w + PAD_X * 2;
    let chip_h = line_h * lines.len() as i32
        + LINE_GAP * (lines.len() as i32 - 1)
        + PAD_Y * 2;
    let (bx, by, bw, bh) = bounds;
    let x0 = bx + (bw - chip_w) / 2;
    let y0 = by + MARGIN;
    // A screen too small to hold the chip gets none rather than a clipped
    // one — same rule as the idle hint.
    if x0 < bx || y0 + chip_h > by + bh
        || x0 < 0 || y0 < 0 || x0 + chip_w > win_w as i32 || y0 + chip_h > win_h as i32 { return; }
    draw_filled_rect(buf, win_w, win_h, x0, y0, x0 + chip_w, y0 + chip_h, 0x00_10_10_14);
    draw_rect_border(buf, win_w, win_h, x0, y0, x0 + chip_w, y0 + chip_h, accent);
    let text_rgb = framefang_core::color::Rgba::new_opaque(
        ((text >> 16) & 0xFF) as u8, ((text >> 8) & 0xFF) as u8, (text & 0xFF) as u8,
    );
    let accent_rgb = framefang_core::color::Rgba::new_opaque(
        ((accent >> 16) & 0xFF) as u8, ((accent >> 8) & 0xFF) as u8, (accent & 0xFF) as u8,
    );
    let mut surf = crate::painter::U32Surface { buf, stride: win_w as i32, height: win_h as i32 };
    for (i, line) in lines.iter().enumerate() {
        let lw = crate::bitmap_font::measure(line, SCALE);
        let lx = x0 + (chip_w - lw) / 2;
        let ly = y0 + PAD_Y + i as i32 * (line_h + LINE_GAP);
        // First line is the headline and takes the accent color; the rest
        // is the quieter instruction text.
        let color = if i == 0 { accent_rgb } else { text_rgb };
        crate::painter::draw_text(&mut surf, lx, ly, SCALE, line, color);
    }
}

/// Magnifier lens. Samples the original capture in a (2·R+1)² window around
/// the cursor and draws each source pixel as a `MAG_ZOOM`-sized square. Adds
/// a 1-px border + crosshair through the center pixel. Auto-flips position
/// so it never falls off the edge of the monitor it is on.
///
/// `shot_off` maps the cursor from frame space into the capture: on a
/// multi-monitor desktop the two differ by the virtual-desktop origin, and
/// without it the lens shows pixels from somewhere else entirely.
fn draw_magnifier(
    buf:      &mut [u32],
    win_w:    usize,
    win_h:    usize,
    bounds:   (i32, i32, i32, i32),
    shot:     &image::ImageBuffer<image::Rgba<u8>, Vec<u8>>,
    cursor:   (i32, i32),
    shot_off: (i32, i32),
    // Open up and to the left of the cursor first, rather than down and
    // to the right.
    up_left:  bool,
) {
    let (bx, by, bw, bh) = bounds;
    let chip = MAG_SIZE + 4;          // includes border
    let (right, below) = (cursor.0 + MAG_OFFSET, cursor.1 + MAG_OFFSET);
    let (left, above) = (cursor.0 - MAG_OFFSET - chip, cursor.1 - MAG_OFFSET - chip);
    let x0 = if up_left {
        if left >= bx { left } else { right }
    } else if right + chip > bx + bw { left } else { right };
    let y0 = if up_left {
        if above >= by { above } else { below }
    } else if below + chip > by + bh { above } else { below };
    if x0 < bx || y0 < by || x0 + chip > bx + bw || y0 + chip > by + bh { return; } // no room either way

    let shot_w = shot.width()  as i32;
    let shot_h = shot.height() as i32;

    // Background fill (kept opaque so the lens stays readable on dark
    // shots) + 1-px white border.
    draw_filled_rect(buf, win_w, win_h, x0, y0, x0 + chip, y0 + chip, 0x00_10_10_14);
    draw_rect_border(buf, win_w, win_h, x0, y0, x0 + chip, y0 + chip, 0x00_FF_FF_FF);

    let inner_x = x0 + 2;
    let inner_y = y0 + 2;
    for sy in 0..MAG_PIXELS {
        for sx in 0..MAG_PIXELS {
            let src_x = cursor.0 + shot_off.0 + sx - MAG_RADIUS;
            let src_y = cursor.1 + shot_off.1 + sy - MAG_RADIUS;
            let px = if src_x >= 0 && src_x < shot_w && src_y >= 0 && src_y < shot_h {
                let p = shot.get_pixel(src_x as u32, src_y as u32).0;
                ((p[0] as u32) << 16) | ((p[1] as u32) << 8) | p[2] as u32
            } else { 0x00_00_00_00 };
            let dx = inner_x + sx * MAG_ZOOM;
            let dy = inner_y + sy * MAG_ZOOM;
            draw_filled_rect(buf, win_w, win_h, dx, dy, dx + MAG_ZOOM, dy + MAG_ZOOM, px);
        }
    }

    // Crosshair through the center pixel — a 1-px red plus inside the lens
    // makes the exact source pixel obvious.
    let cx = inner_x + MAG_RADIUS * MAG_ZOOM;
    let cy = inner_y + MAG_RADIUS * MAG_ZOOM;
    let red = 0x00_DC_26_26;
    draw_filled_rect(buf, win_w, win_h, inner_x, cy, inner_x + MAG_SIZE, cy + 1, red);
    draw_filled_rect(buf, win_w, win_h, cx, inner_y, cx + 1, inner_y + MAG_SIZE, red);
}

/// The captured pixel at frame point `p`, opaque. `None` off the capture.
fn pixel_at(shot: &ImageBuffer<Rgba<u8>, Vec<u8>>, p: (i32, i32), shot_off: (i32, i32))
    -> Option<framefang_core::color::Rgba>
{
    let (x, y) = (p.0 + shot_off.0, p.1 + shot_off.1);
    if x < 0 || y < 0 || x >= shot.width() as i32 || y >= shot.height() as i32 { return None; }
    let px = shot.get_pixel(x as u32, y as u32).0;
    Some(framefang_core::color::Rgba::new_opaque(px[0], px[1], px[2]))
}

/// `c` as hue in whole degrees and saturation and lightness in whole
/// percent.
fn to_hsl(c: framefang_core::color::Rgba) -> (u32, u32, u32) {
    let (r, g, b) = (c.r as f32 / 255.0, c.g as f32 / 255.0, c.b as f32 / 255.0);
    let (max, min) = (r.max(g).max(b), r.min(g).min(b));
    let l = (max + min) / 2.0;
    let d = max - min;
    if d < 1e-6 { return (0, 0, (l * 100.0).round() as u32); }
    let s = d / (1.0 - (2.0 * l - 1.0).abs());
    let h = if max == r { ((g - b) / d).rem_euclid(6.0) } else if max == g { (b - r) / d + 2.0 } else { (r - g) / d + 4.0 };
    (((h * 60.0).round() as u32) % 360, (s * 100.0).round().min(100.0) as u32, (l * 100.0).round() as u32)
}

/// The color loupe's lens: 17 pixels across at 9x.
const LOUPE_PIXELS: i32 = 17;
const LOUPE_ZOOM: i32 = 9;
const LOUPE_LENS: i32 = LOUPE_PIXELS * LOUPE_ZOOM + 2;
const LOUPE_CARD_W: i32 = 184;
const LOUPE_GAP: i32 = 6;

/// The eyedropper's loupe: a lens on the pixels around the cursor with the
/// one a click takes outlined, and beside it a card with that color as a
/// large swatch, its hex in big type, and its RGB and HSL. The pair sits
/// below and right of the cursor (`up_left`: above and left), flipping to
/// stay on the screen in `bounds`. `hint` adds how to pick and back out.
#[allow(clippy::too_many_arguments)]
fn draw_color_loupe(
    buf: &mut [u32], win_w: usize, win_h: usize,
    bounds: Rect4,
    shot: &ImageBuffer<Rgba<u8>, Vec<u8>>,
    cursor: (i32, i32), shot_off: (i32, i32),
    up_left: bool, hint: bool,
) {
    use framefang_core::color::Rgba as C;
    const SURFACE: u32 = 0x00_1A_1A_1D;
    const EDGE: u32 = 0x00_3A_3A_40;
    const OFFSET: i32 = 22;
    let Some(color) = pixel_at(shot, cursor, shot_off) else { return; };
    let (bx, by, bw, bh) = bounds;
    let (unit_w, unit_h) = (LOUPE_LENS + LOUPE_GAP + LOUPE_CARD_W, LOUPE_LENS);
    let (right, below) = (cursor.0 + OFFSET, cursor.1 + OFFSET);
    let (left, above) = (cursor.0 - OFFSET - unit_w, cursor.1 - OFFSET - unit_h);
    let fits_x = |x: i32| x >= bx && x + unit_w <= bx + bw;
    let fits_y = |y: i32| y >= by && y + unit_h <= by + bh;
    let x0 = if up_left { if fits_x(left) { left } else { right } } else if fits_x(right) { right } else { left };
    let y0 = if up_left { if fits_y(above) { above } else { below } } else if fits_y(below) { below } else { above };
    if !fits_x(x0) || !fits_y(y0) { return; }

    // The lens. Each captured pixel a 9 px square; the center one framed
    // dark then light, so it reads on any color.
    draw_filled_rect(buf, win_w, win_h, x0, y0, x0 + LOUPE_LENS, y0 + LOUPE_LENS, SURFACE);
    draw_rect_border(buf, win_w, win_h, x0, y0, x0 + LOUPE_LENS, y0 + LOUPE_LENS, EDGE);
    let r = LOUPE_PIXELS / 2;
    for sy in 0..LOUPE_PIXELS {
        for sx in 0..LOUPE_PIXELS {
            let p = pixel_at(shot, (cursor.0 + sx - r, cursor.1 + sy - r), shot_off)
                .map_or(0, |c| ((c.r as u32) << 16) | ((c.g as u32) << 8) | c.b as u32);
            let (dx, dy) = (x0 + 1 + sx * LOUPE_ZOOM, y0 + 1 + sy * LOUPE_ZOOM);
            draw_filled_rect(buf, win_w, win_h, dx, dy, dx + LOUPE_ZOOM, dy + LOUPE_ZOOM, p);
        }
    }
    let (cx, cy) = (x0 + 1 + r * LOUPE_ZOOM, y0 + 1 + r * LOUPE_ZOOM);
    draw_rect_border(buf, win_w, win_h, cx - 1, cy - 1, cx + LOUPE_ZOOM + 1, cy + LOUPE_ZOOM + 1, 0x00_00_00_00);
    draw_rect_border(buf, win_w, win_h, cx - 2, cy - 2, cx + LOUPE_ZOOM + 2, cy + LOUPE_ZOOM + 2, 0x00_FF_FF_FF);

    // The card.
    let (kx, ky) = (x0 + LOUPE_LENS + LOUPE_GAP, y0);
    draw_filled_rect(buf, win_w, win_h, kx, ky, kx + LOUPE_CARD_W, ky + unit_h, SURFACE);
    draw_rect_border(buf, win_w, win_h, kx, ky, kx + LOUPE_CARD_W, ky + unit_h, EDGE);
    const PAD: i32 = 14;
    const SW: i32 = 44;
    let ink = ((color.r as u32) << 16) | ((color.g as u32) << 8) | color.b as u32;
    draw_filled_rect(buf, win_w, win_h, kx + PAD - 1, ky + PAD - 1, kx + PAD + SW + 1, ky + PAD + SW + 1, EDGE);
    draw_filled_rect(buf, win_w, win_h, kx + PAD, ky + PAD, kx + PAD + SW, ky + PAD + SW, ink);
    let hex = format!("#{:02X}{:02X}{:02X}", color.r, color.g, color.b);
    let (h, s, l) = to_hsl(color);
    let rows = [("RGB", format!("{}, {}, {}", color.r, color.g, color.b)), ("HSL", format!("{h}, {s}%, {l}%"))];
    let mut surf = crate::painter::U32Surface { buf, stride: win_w as i32, height: win_h as i32 };
    const HEX_PX: f32 = 20.0;
    let hex_y = (ky + PAD) as f32 + (SW as f32 - framefang_core::text::line_height(HEX_PX)) / 2.0;
    crate::painter::draw_unicode_text(&mut surf, (kx + PAD + SW + 12) as f32, hex_y, HEX_PX, &hex,
                                      C::new_opaque(0xF4, 0xF4, 0xF7));
    let row_px = FLYOUT_TEXT_PX;
    let row_h = framefang_core::text::line_height(row_px) + 4.0;
    let mut y = (ky + PAD + SW + 12) as f32;
    for (label, value) in rows {
        crate::painter::draw_unicode_text(&mut surf, (kx + PAD) as f32, y, row_px, label, C::new_opaque(0x7A, 0x7A, 0x84));
        crate::painter::draw_unicode_text(&mut surf, (kx + PAD + 40) as f32, y, row_px, &value, C::new_opaque(0xDC, 0xDC, 0xE2));
        y += row_h;
    }
    if hint {
        let hint_y = (ky + unit_h - PAD) as f32 - framefang_core::text::line_height(11.5);
        crate::painter::draw_unicode_text(&mut surf, (kx + PAD) as f32, hint_y, 11.5,
                                          "Click to pick  -  Esc to cancel", C::new_opaque(0x6E, 0x6E, 0x78));
    }
}

// ── helpers ──────────────────────────────────────────────────────────────

/// Paint surface for `compose_video_overlay`. Reads (and writes) a live
/// copy of the video frame so alpha blending + Pixelate source-sampling
/// match the screenshot editor pixel-for-pixel, while mirroring every
/// touched pixel — opaque — into a transparent out-buffer that becomes the
/// ffmpeg overlay. Pixels no annotation touches stay (0,0,0,0).
struct DiffSurface<'a> {
    frame: &'a mut ImageBuffer<Rgba<u8>, Vec<u8>>,
    out:   &'a mut ImageBuffer<Rgba<u8>, Vec<u8>>,
}

impl painter::Surface for DiffSurface<'_> {
    fn width(&self)  -> i32 { self.frame.width()  as i32 }
    fn height(&self) -> i32 { self.frame.height() as i32 }
    fn read(&self, x: i32, y: i32) -> [u8; 4] {
        self.frame.get_pixel(x as u32, y as u32).0
    }
    fn write(&mut self, x: i32, y: i32, rgba: [u8; 4]) {
        self.frame.put_pixel(x as u32, y as u32, Rgba(rgba));
        self.out.put_pixel(x as u32, y as u32, Rgba([rgba[0], rgba[1], rgba[2], 255]));
    }
}

fn rect_from(a: (i32, i32), b: (i32, i32)) -> (i32, i32, i32, i32) {
    let x = a.0.min(b.0);
    let y = a.1.min(b.1);
    let w = (a.0 - b.0).abs();
    let h = (a.1 - b.1).abs();
    (x, y, w, h)
}

fn crop(
    src: &ImageBuffer<Rgba<u8>, Vec<u8>>,
    (x, y, w, h): (i32, i32, i32, i32),
) -> ImageBuffer<Rgba<u8>, Vec<u8>> {
    let img_w = src.width()  as i32;
    let img_h = src.height() as i32;
    let x0 = x.max(0).min(img_w);
    let y0 = y.max(0).min(img_h);
    let x1 = (x + w).max(0).min(img_w);
    let y1 = (y + h).max(0).min(img_h);
    let cw = (x1 - x0).max(1) as u32;
    let ch = (y1 - y0).max(1) as u32;
    let mut out = ImageBuffer::<Rgba<u8>, Vec<u8>>::new(cw, ch);
    for j in 0..ch {
        for i in 0..cw {
            out.put_pixel(i, j, *src.get_pixel(x0 as u32 + i, y0 as u32 + j));
        }
    }
    out
}

/// Translate an annotation by (dx, dy) — used to move window-space coords
/// into the cropped output's local space when burning into the saved PNG.
/// The transform itself lives in `framefang-core` so the Select tool's move
/// and this crop-space shift can never drift apart.
fn translate_annotation(a: &Annotation, dx: f32, dy: f32) -> Annotation {
    edit::translated(a, dx, dy)
}

/// Width × height chip rendered just outside the bottom-right corner of the
/// selection. Uses the existing 5×7 digit font (via `painter::draw_number`)
/// and a tiny hand-drawn `×` glyph between the two numbers. Background is a
/// 75 %-opaque dark fill so it stays legible on light or dark screenshots.
fn draw_dimension_chip(
    buf: &mut [u32], stride: usize, height: usize,
    bounds: (i32, i32, i32, i32),
    anchor_x: i32, anchor_y: i32, w: u32, h: u32,
) {
    const SCALE:  i32 = 2;
    const PAD_X:  i32 = 6;
    const PAD_Y:  i32 = 4;
    const X_GLYPH_W: i32 = 5 * SCALE;
    const GAP:    i32 = SCALE * 2;

    let glyph_h    = 7 * SCALE;
    let digits_w_w = digit_count(w) as i32 * 5 * SCALE + (digit_count(w) as i32 - 1).max(0) * SCALE;
    let digits_h_w = digit_count(h) as i32 * 5 * SCALE + (digit_count(h) as i32 - 1).max(0) * SCALE;
    let inner_w    = digits_w_w + GAP + X_GLYPH_W + GAP + digits_h_w;
    let chip_w     = inner_w + PAD_X * 2;
    let chip_h     = glyph_h + PAD_Y * 2;

    // Place chip just inside the selection's bottom-right corner. Flip
    // outward if it would clip the screen edge.
    let (bx, by, _bw, _bh) = bounds;
    let mut x0 = anchor_x - chip_w - 4;
    let mut y0 = anchor_y - chip_h - 4;
    if x0 < bx { x0 = anchor_x + 4; }
    if y0 < by { y0 = anchor_y + 4; }
    let x1 = x0 + chip_w;
    let y1 = y0 + chip_h;

    // 75 %-opaque dark fill — no real alpha blend on the u32 buffer, so just
    // mix toward the existing pixel by 1/4. This also keeps the chip from
    // wiping out the screenshot underneath.
    let xa = x0.max(0) as usize;
    let xb = (x1.min(stride as i32) as usize).max(xa);
    let ya = y0.max(0) as usize;
    let yb = (y1.min(height as i32) as usize).max(ya);
    for y in ya..yb.min(height) {
        for x in xa..xb.min(stride) {
            let dst = buf[y * stride + x];
            let dr = (dst >> 16) & 0xFF;
            let dg = (dst >> 8)  & 0xFF;
            let db =  dst        & 0xFF;
            // src = 0x16191D, weight 192/256.
            let r = (dr * 64 + 0x16 * 192) / 256;
            let g = (dg * 64 + 0x19 * 192) / 256;
            let b = (db * 64 + 0x1D * 192) / 256;
            buf[y * stride + x] = (r << 16) | (g << 8) | b;
        }
    }

    // Render the digits + 'x' separator using the painter via a tiny inline
    // U32Surface (the painter alpha-blends, which leaves the chip background
    // visible underneath the strokes — that's intentional).
    let mut surf = crate::painter::U32Surface { buf, stride: stride as i32, height: height as i32 };
    let text_y = y0 + PAD_Y;
    let mut cx = x0 + PAD_X;
    crate::painter::draw_number(&mut surf, cx, text_y, SCALE, w, framefang_core::color::Rgba::WHITE);
    cx += digits_w_w + GAP;
    draw_x_glyph(&mut surf, cx, text_y, SCALE);
    cx += X_GLYPH_W + GAP;
    crate::painter::draw_number(&mut surf, cx, text_y, SCALE, h, framefang_core::color::Rgba::WHITE);
}

fn digit_count(mut n: u32) -> u32 {
    if n == 0 { return 1; }
    let mut c = 0u32; while n > 0 { c += 1; n /= 10; } c
}

/// Tiny `×` drawn as two diagonal lines through a 5-wide × 7-tall cell. Same
/// scale convention as `draw_number`.
fn draw_x_glyph(surf: &mut crate::painter::U32Surface, x: i32, y: i32, scale: i32) {
    use framefang_core::color::Rgba;
    let w = 5 * scale;
    let h = 7 * scale;
    crate::painter::line(surf, x, y + scale, x + w - 1, y + h - scale - 1, scale as f32, Rgba::WHITE);
    crate::painter::line(surf, x + w - 1, y + scale, x, y + h - scale - 1, scale as f32, Rgba::WHITE);
}

/// 1-px dashed rectangle border. Used as a subtle text-area indicator
/// while the Text tool is in TextInput state — vanishes the moment the
/// user commits because compose_final / save / copy / pin all snapshot
/// the bitmap without re-running this render pass.
fn draw_dashed_border(
    buf: &mut [u32], stride: usize, height: usize,
    x0: i32, y0: i32, x1: i32, y1: i32, rgb: u32,
) {
    let xa = x0.max(0) as usize;
    let xb = (x1.min(stride as i32) as usize).max(xa);
    let ya = y0.max(0) as usize;
    let yb = (y1.min(height as i32) as usize).max(ya);
    if xa >= stride || ya >= height || xa == xb || ya == yb { return; }
    // 3-on-3-off dash pattern.
    let dash = |i: usize| (i / 3) & 1 == 0;
    for x in xa..xb.min(stride) {
        if dash(x) {
            buf[ya * stride + x] = rgb;
            let by = (yb - 1).min(height - 1);
            buf[by * stride + x] = rgb;
        }
    }
    for y in ya..yb.min(height) {
        if dash(y) {
            buf[y * stride + xa] = rgb;
            let bx = (xb - 1).min(stride - 1);
            buf[y * stride + bx] = rgb;
        }
    }
}

fn draw_rect_border(
    buf: &mut [u32], stride: usize, height: usize,
    x0: i32, y0: i32, x1: i32, y1: i32, rgb: u32,
) {
    let xa = x0.max(0) as usize;
    let xb = (x1.min(stride as i32) as usize).max(xa);
    let ya = y0.max(0) as usize;
    let yb = (y1.min(height as i32) as usize).max(ya);
    if xa >= stride || ya >= height || xa == xb || ya == yb { return; }
    for x in xa..xb.min(stride) {
        buf[ya * stride + x] = rgb;
        let by = (yb - 1).min(height - 1);
        buf[by * stride + x] = rgb;
    }
    for y in ya..yb.min(height) {
        buf[y * stride + xa] = rgb;
        let bx = (xb - 1).min(stride - 1);
        buf[y * stride + bx] = rgb;
    }
}

fn draw_filled_rect(
    buf: &mut [u32], stride: usize, height: usize,
    x0: i32, y0: i32, x1: i32, y1: i32, rgb: u32,
) {
    let xa = x0.max(0) as usize;
    let xb = (x1.min(stride as i32) as usize).max(xa);
    let ya = y0.max(0) as usize;
    let yb = (y1.min(height as i32) as usize).max(ya);
    for y in ya..yb.min(height) {
        for x in xa..xb.min(stride) {
            buf[y * stride + x] = rgb;
        }
    }
}

/// Filled rect — radius is reserved for a later AA pass; sharp corners
/// are good enough for the slice-1 toolbar chrome.
fn draw_rounded_rect(
    buf: &mut [u32], stride: usize, height: usize,
    x0: i32, y0: i32, x1: i32, y1: i32, _radius: i32, rgb: u32,
) {
    draw_filled_rect(buf, stride, height, x0, y0, x1, y1, rgb);
}

/// Draw a thin diagonal stripe across the rect to indicate "disabled".
fn draw_diagonal_stripe(
    buf: &mut [u32], stride: usize, height: usize,
    x0: i32, y0: i32, x1: i32, y1: i32, rgb: u32,
) {
    let w = x1 - x0;
    let h = y1 - y0;
    let steps = w.max(h);
    for i in 0..steps {
        let x = x0 + i * w / steps;
        let y = y0 + i * h / steps;
        for dy in -1..=1 {
            let yy = (y + dy).max(0) as usize;
            if x >= 0 && (x as usize) < stride && yy < height {
                buf[yy * stride + x as usize] = rgb;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pulse_breathes_three_times_slowly_and_ends_dark() {
        assert_eq!(pulse_level(0.0), 0.0, "starts dark");
        assert!(pulse_level(PULSE_SECS - 0.001) < 0.01, "ends dark, no hard cut");
        assert_eq!(pulse_level(PULSE_SECS), 0.0);
        let breath = 1.0 / PULSE_HZ;
        for k in 0..3 {
            let mid = (k as f32 + 0.5) * breath;
            assert!(pulse_level(mid) > 0.99, "full at the middle of breath {k}");
        }
        // Slow: a full breath takes most of a second, not the old 150 ms.
        assert!(breath >= 0.6);
    }

    #[test]
    fn no_tool_in_hand_lights_no_tool_button() {
        let (w, h) = (900usize, 900usize);
        let (bounds, sel) = ((0, 0, w as i32, h as i32), (300, 150, 300, 300));
        let lit = |active: Option<Tool>| {
            let mut buf = vec![0u32; w * h];
            draw_tool_panel(&mut buf, w, h, bounds, sel, active);
            buf.iter().filter(|&&p| p == 0x00_64_95_ED).count()
        };
        assert_eq!(lit(None), 0);
        assert!(lit(Some(Tool::Pen)) > 0);
    }

    #[test]
    fn the_cut_out_color_is_what_the_backdrop_turns_the_canvas_into() {
        let px = EDITOR_CANVAS;
        let shot = crate::backdrop::Shot { rgba: &px, width: 1, height: 1, offset: (0, 0) };
        let mut b = crate::backdrop::Backdrop::default();
        assert_eq!(b.frame(1, 1, &shot, None), &[EDITOR_CLEAR]);
    }

    fn monitor(x: i32, y: i32, w: u32, h: u32) -> vdesk::MonitorRect {
        vdesk::MonitorRect { x, y, width: w, height: h }
    }

    #[test]
    fn the_editor_opens_with_the_picture_where_it_was() {
        let m = monitor(0, 0, 2560, 1440);
        let p = editor_placement((600, 350, 900, 600), m).unwrap();
        let on_screen = (p.pos.0 + p.picture_at.0, p.pos.1 + p.picture_at.1);
        assert_eq!(on_screen, (600, 350));
        assert_eq!(p.picture_at, (EDITOR_EDGE, EDITOR_EDGE));
        // Room for the tool column on the right and the action row below.
        let (tp_w, _) = tool_panel_dims();
        assert!(p.size.0 as i32 >= EDITOR_EDGE + 900 + 5 + tp_w);
    }

    #[test]
    fn the_editor_window_stays_on_its_monitor() {
        // Against the right and bottom edges, on a monitor left of and above
        // the primary one: the window moves in, the picture moves with it.
        let m = monitor(-1080, -801, 1080, 1920);
        let p = editor_placement((-400, 900, 390, 200), m).unwrap();
        assert!(p.pos.0 + p.size.0 as i32 <= -1080 + 1080);
        assert!(p.pos.0 >= -1080 && p.pos.1 >= -801);
        assert!(p.pos.1 + p.size.1 as i32 <= -801 + 1920);
        // A short picture still gets a window tall enough for the tool column.
        let (_, tp_h) = tool_panel_dims();
        assert!(p.size.1 as i32 >= tp_h + EDITOR_EDGE * 2);
    }

    #[test]
    fn the_tools_stand_on_the_pictures_bottom_right_corner() {
        let (_, tp_h) = tool_panel_dims();
        // A short picture: the column ends level with its bottom and the
        // window makes room above for the rest of it.
        let m = monitor(0, 0, 2560, 1440);
        let p = editor_placement((600, 700, 300, 120), m).unwrap();
        assert_eq!((p.pos.0 + p.picture_at.0, p.pos.1 + p.picture_at.1), (600, 700), "the picture stays put");
        let sel = (p.picture_at.0, p.picture_at.1, 300, 120);
        let (_, ty) = tool_panel_origin((0, 0, p.size.0 as i32, p.size.1 as i32), sel);
        assert_eq!(ty + tp_h, sel.1 + sel.3, "bottom-aligned with the picture");
        assert!(ty >= 0, "inside the window");
        // A tall selection on the frozen desktop: the same, not its top.
        let (_, ty) = tool_panel_origin((0, 0, 2560, 1440), (100, 100, 900, 1000));
        assert_eq!(ty + tp_h, 1100);
        // Near the top of the screen the column is held on it and runs down.
        let (_, ty) = tool_panel_origin((0, 0, 2560, 1440), (100, 10, 400, 80));
        assert_eq!(ty, 0);
    }

    #[test]
    fn a_picture_too_big_for_a_window_is_edited_in_place() {
        let m = monitor(0, 0, 2560, 1440);
        assert_eq!(editor_placement((0, 0, 2560, 1440), m), None);
        assert_eq!(editor_placement((0, 0, 0, 10), m), None);
    }

    #[test]
    fn a_session_opens_at_the_last_usable_values() {
        assert_eq!(Knob::Thickness.clamp(9.0), 9.0);
        assert_eq!(Knob::Thickness.clamp(6.4), 6.0);
        assert_eq!(Knob::Thickness.clamp(THICKNESS_MAX), THICKNESS_MAX);
        assert_eq!(Knob::Thickness.clamp(0.0), THICKNESS_DEFAULT);
        assert_eq!(Knob::Thickness.clamp(99.0), THICKNESS_DEFAULT);
        assert_eq!(Knob::Thickness.clamp(f32::NAN), THICKNESS_DEFAULT);
        assert_eq!(Knob::TextSize.clamp(500.0), 28.0);
        assert_eq!(Knob::PixelSize.clamp(0.0), 10.0);
        // Every default sits inside its own range.
        for k in [Knob::Thickness, Knob::MarkerOpacity, Knob::TextSize, Knob::PixelSize] {
            let (lo, hi) = k.range();
            assert!((lo..=hi).contains(&k.clamp(f32::NAN)), "{k:?}");
        }
    }

    #[test]
    fn every_tool_has_a_flyout_with_what_it_uses() {
        use framefang_core::tool::Tool as T;
        for t in framefang_core::tool::Tool::ALL {
            let l = flyout_layout(t);
            assert!(!l.knobs.is_empty() || l.colors.is_some(), "{t:?} has an empty flyout");
            // Everything sits inside the flyout.
            let mut rects: Vec<Rect4> = l.knobs.iter().map(|(_, r)| *r).collect();
            if let Some((p, n, name, sw)) = l.colors { rects.extend([p, n, name]); rects.extend(sw); }
            // Any-color row: with the palette, under its swatches, and none
            // of its parts overlapping.
            assert_eq!(l.colors.is_some(), l.custom.is_some(), "{t:?}");
            if let (Some(c), Some((_, _, _, sw))) = (l.custom, l.colors) {
                let last_swatch = sw.iter().map(|r| r.1 + r.3).max().unwrap();
                assert!(c.eyedropper.1 > last_swatch, "{t:?} under the swatches");
                assert!(c.current.1 >= c.eyedropper.1 + c.eyedropper.3, "{t:?} current under the eyedropper");
                assert!(c.spectrum.0 >= c.eyedropper.0 + c.eyedropper.2, "{t:?} spectrum beside them");
                assert_eq!(c.spectrum.0 + c.spectrum.2, sw[7].0 + sw[7].2, "{t:?} spectrum ends with the grid");
                rects.extend([c.eyedropper, c.current, c.spectrum]);
            }
            for r in rects {
                assert!(r.0 >= 0 && r.1 >= 0 && r.0 + r.2 <= l.w && r.1 + r.3 <= l.h, "{t:?} {r:?}");
            }
        }
        // Step is colors only; Pixelate has no color; Marker has two sliders.
        assert!(flyout_layout(T::Step).knobs.is_empty() && flyout_layout(T::Step).colors.is_some());
        assert!(flyout_layout(T::Pixelate).colors.is_none());
        assert_eq!(Knob::for_tool(T::Marker), &[Knob::Thickness, Knob::MarkerOpacity]);
    }

    #[test]
    fn a_tool_opens_in_its_own_color_and_width() {
        use framefang_core::tool::Tool as T;
        let mut s = AppSettings::default();
        let blue = framefang_core::color::Rgba::new_opaque(0, 100, 255);
        s.set_tool_style(T::Arrow, framefang_core::settings::ToolStyle { color_argb: blue.to_argb(), thickness: 7.0 });
        assert_eq!(stroke_for(&s, T::Arrow).color, blue);
        assert_eq!(stroke_for(&s, T::Arrow).thickness, 7.0);
        // The marker's own default, untouched by the arrow.
        assert_eq!(stroke_for(&s, T::Marker).thickness, 16.0);
        // The old pure-red default, saved before palettes were the only
        // way to pick, opens as the Vivid palette's red.
        let pure_red = framefang_core::color::Rgba::new_opaque(255, 0, 0);
        s.set_tool_style(T::Pen, framefang_core::settings::ToolStyle { color_argb: pure_red.to_argb(), thickness: 3.0 });
        assert_eq!(stroke_for(&s, T::Pen).color, framefang_core::annotation::Palettes::ALL[0].colors[0]);
        s.last_tool = "Marker".into();
        assert_eq!(opening_tool(&s), T::Marker);
        s.last_tool = "Nonsense".into();
        assert_eq!(opening_tool(&s), T::Pen);
    }

    #[test]
    fn the_tools_go_left_when_the_picture_is_near_the_right_edge() {
        let m = monitor(0, 0, 2560, 1440);
        // 40 px from the right edge: no room for the tools on that side.
        let p = editor_placement((1720, 300, 800, 500), m).unwrap();
        assert_eq!(p.pos.0 + p.picture_at.0, 1720, "the picture stays put");
        assert!(p.picture_at.0 > EDITOR_EDGE, "the tools are on its left");
        assert!(p.pos.0 + p.size.0 as i32 <= 2560);
    }

    #[test]
    fn the_spectrum_runs_through_the_hues_and_ends_in_grays() {
        use framefang_core::color::Rgba as C;
        // Full red sits halfway down the left edge.
        let mid = (0.92 - 0.5) / 0.80;
        assert_eq!(spectrum_color(0.0, mid), C::new_opaque(255, 0, 0));
        // A third of the way along the hues is green, two thirds blue.
        let g = spectrum_color(SPECTRUM_HUES / 3.0, mid);
        assert!(g.g > 250 && g.r < 5 && g.b < 5, "{g:?}");
        let b = spectrum_color(SPECTRUM_HUES * 2.0 / 3.0, mid);
        assert!(b.b > 250 && b.r < 5 && b.g < 5, "{b:?}");
        // Light at the top, dark at the bottom.
        let (top, bottom) = (spectrum_color(0.0, 0.0), spectrum_color(0.0, 1.0));
        assert!(top.g > 200 && bottom.r < 70, "{top:?} {bottom:?}");
        // The gray strip: white to black.
        assert_eq!(spectrum_color(1.0, 0.0), C::new_opaque(255, 255, 255));
        assert_eq!(spectrum_color(1.0, 1.0), C::new_opaque(0, 0, 0));
        // Out-of-range input is clamped, never a panic or a wrapped hue.
        assert_eq!(spectrum_color(-1.0, 5.0), spectrum_color(0.0, 1.0));
    }

    #[test]
    fn the_loupe_reads_colors_out_in_hsl() {
        use framefang_core::color::Rgba as C;
        assert_eq!(to_hsl(C::new_opaque(255, 0, 0)), (0, 100, 50));
        assert_eq!(to_hsl(C::new_opaque(0, 255, 0)), (120, 100, 50));
        assert_eq!(to_hsl(C::new_opaque(0, 0, 255)), (240, 100, 50));
        assert_eq!(to_hsl(C::new_opaque(128, 128, 128)), (0, 0, 50));
        assert_eq!(to_hsl(C::new_opaque(255, 255, 255)), (0, 0, 100));
        // A blue between: #3A7BD5.
        let (h, s, l) = to_hsl(C::new_opaque(0x3A, 0x7B, 0xD5));
        assert!((214..=215).contains(&h) && (64..=66).contains(&s) && (52..=54).contains(&l), "{h} {s} {l}");
    }

    #[test]
    fn the_eyedropper_reads_the_capture_through_its_offset() {
        let mut shot = frame(4, 4, [0, 0, 0, 255]);
        shot.put_pixel(3, 2, Rgba([10, 20, 30, 7]));
        // Frame (1, 1) is capture (3, 2) when the frame starts at (2, 1).
        assert_eq!(pixel_at(&shot, (1, 1), (2, 1)), Some(framefang_core::color::Rgba::new_opaque(10, 20, 30)));
        assert_eq!(pixel_at(&shot, (5, 0), (0, 0)), None);
    }

    #[test]
    fn a_picked_color_survives_the_next_session() {
        use framefang_core::tool::Tool as T;
        let mut s = AppSettings::default();
        let teal = framefang_core::color::Rgba::new_opaque(18, 140, 126);
        s.set_tool_style(T::Pen, framefang_core::settings::ToolStyle { color_argb: teal.to_argb(), thickness: 3.0 });
        assert_eq!(stroke_for(&s, T::Pen).color, teal, "off every palette, and kept");
    }

    #[test]
    fn the_flyout_opens_fully_and_starts_fast() {
        assert_eq!(ease_out(0.0), 0.0);
        assert_eq!(ease_out(1.0), 1.0);
        assert_eq!(ease_out(2.0), 1.0);
        assert!(ease_out(0.25) > 0.5, "most of the way out in the first quarter");
    }
    use framefang_core::annotation::{Annotation, Point2, Stroke};
    use framefang_core::color::Rgba as KRgba;

    fn frame(w: u32, h: u32, px: [u8; 4]) -> ImageBuffer<Rgba<u8>, Vec<u8>> {
        ImageBuffer::from_pixel(w, h, Rgba(px))
    }

    /// Untouched pixels of the video-annotate overlay must stay fully
    /// transparent so ffmpeg shows the clip through them.
    #[test]
    fn diff_surface_untouched_pixels_stay_transparent() {
        let mut f   = frame(32, 32, [50, 60, 70, 255]);
        let mut out = frame(32, 32, [0, 0, 0, 0]);
        let mut surf = DiffSurface { frame: &mut f, out: &mut out };

        let mut pen = Annotation::pen(
            Stroke { color: KRgba { r: 255, g: 0, b: 0, a: 255 }, thickness: 1.0 },
            Point2::new(4.0, 4.0),
        );
        pen.extend(Point2::new(10.0, 4.0));
        painter::render_annotation(&mut surf, &pen, None);

        assert_eq!(out.get_pixel(4, 4).0[3], 255, "stroked pixel must be opaque");
        // Mid-stroke: the round end at (4, 4) is anti-aliased, so its color
        // is partly the frame's.
        assert_eq!(out.get_pixel(7, 4).0[0], 255, "stroked pixel keeps stroke color");
        assert_eq!(out.get_pixel(20, 20).0, [0, 0, 0, 0], "untouched pixel stays transparent");
    }

    /// Semi-transparent strokes (Marker) must blend against the FRAME, not
    /// against transparency — the regression this surface exists to stop:
    /// blending against (0,0,0,0) renders an opaque near-black band.
    #[test]
    fn diff_surface_marker_blends_against_frame() {
        let mut f   = frame(32, 32, [200, 200, 200, 255]);
        let mut out = frame(32, 32, [0, 0, 0, 0]);
        let mut surf = DiffSurface { frame: &mut f, out: &mut out };

        // 50%-alpha yellow marker over light grey.
        let mut marker = Annotation::marker(
            Stroke { color: KRgba { r: 255, g: 255, b: 0, a: 255 }, thickness: 4.0 },
            Point2::new(8.0, 8.0),
            128,
        );
        marker.extend(Point2::new(16.0, 8.0));
        painter::render_annotation(&mut surf, &marker, None);

        let p = out.get_pixel(10, 8).0;
        assert_eq!(p[3], 255, "marker pixel is opaque in the overlay");
        // Yellow@50% over grey(200) ≈ (227, 227, 100) — must NOT be the
        // near-black (127, 127, 0) that blending against transparency gives.
        assert!(p[0] > 180 && p[1] > 180,
            "marker must blend against the frame, got {p:?}");
        assert!(p[2] < 160, "blue channel should drop under a yellow marker, got {p:?}");
    }

    #[test]
    fn time_window_filter_is_half_open() {
        let mut a = Annotation::pen(Stroke::default(), Point2::new(0.0, 0.0));
        assert!(annotation_visible_at(&a, 0.0), "None = whole clip");
        assert!(annotation_visible_at(&a, 99.0));
        a.time = Some((1.0, 4.0));
        assert!(!annotation_visible_at(&a, 0.999));
        assert!( annotation_visible_at(&a, 1.0), "start is inclusive");
        assert!( annotation_visible_at(&a, 3.999));
        assert!(!annotation_visible_at(&a, 4.0), "end is exclusive");
        a.time = Some((2.0, f32::INFINITY));
        assert!(annotation_visible_at(&a, 9999.0), "open end means until clip end");
    }

    #[test]
    fn group_by_window_groups_consecutive_runs_only() {
        let p = Point2::new(0.0, 0.0);
        let a  = Annotation::pen(Stroke::default(), p);               // None
        let a2 = Annotation::pen(Stroke::default(), p);               // None — same run
        let mut b = Annotation::pen(Stroke::default(), p);
        b.time = Some((1.0, 4.0));
        let c = Annotation::pen(Stroke::default(), p);                // None again — new run
        let all = [a, a2, b, c];
        let groups = group_by_window(&all);
        assert_eq!(groups.len(), 3,
            "the trailing whole-clip stroke must start a new run — folding \
             it into the first would burn the timed ink on top of it");
        assert_eq!(groups[0].0, None);
        assert_eq!(groups[0].1.len(), 2, "consecutive whole-clip strokes share one group");
        assert_eq!(groups[1].0, Some((1.0, 4.0)));
        assert_eq!(groups[2].0, None);
        assert_eq!(groups[2].1.len(), 1);
    }

    #[test]
    fn stamp_window_at_clip_end_stays_inside_the_clip() {
        // Dragging the playhead fully right puts scrub_pos exactly at the
        // duration. A raw stamp would gate the burn on gte(t, dur), which
        // no frame ever reaches — frame timestamps stop one frame short
        // of the container length — so the ink the preview showed would
        // silently vanish from the burned video.
        let w = stamp_window(10.0, 10.0, None).expect("end-of-clip stamp is a window");
        assert!(w.0 < 10.0, "start must sit strictly inside the clip, got {}", w.0);
        assert_eq!(w.0, 10.0 - 0.05);
        assert_eq!(w.1, f32::INFINITY);
        // Presets that run past the end collapse to open-ended too.
        assert_eq!(stamp_window(10.0, 10.0, Some(3.0)), Some((10.0 - 0.05, f32::INFINITY)));
    }

    #[test]
    fn stamp_window_default_collapses_to_whole_clip() {
        assert_eq!(stamp_window(0.0, 10.0, None), None, "scrub 0 + \"End\" = no window");
        assert_eq!(stamp_window(0.0, 10.0, Some(3.0)), Some((0.0, 3.0)));
        assert_eq!(stamp_window(2.0, 10.0, Some(3.0)), Some((2.0, 5.0)));
        assert_eq!(stamp_window(2.0, 10.0, None), Some((2.0, f32::INFINITY)));
    }

    #[test]
    fn modifier_keys_do_not_answer_a_confirmation() {
        // "any other key cancels" must not include the modifier half of a
        // Ctrl+Z the user is reaching for.
        assert!(is_modifier_key(&Key::Named(NamedKey::Shift)));
        assert!(is_modifier_key(&Key::Named(NamedKey::Control)));
        assert!(is_modifier_key(&Key::Named(NamedKey::Alt)));
        assert!(!is_modifier_key(&Key::Named(NamedKey::Escape)));
        assert!(!is_modifier_key(&Key::Named(NamedKey::Enter)));
        assert!(!is_modifier_key(&Key::Character("p".into())));
    }

    #[test]
    fn notice_bar_skips_screens_too_small_to_hold_it() {
        // Must not panic or write out of bounds on a tiny surface.
        let mut buf = vec![0u32; 40 * 20];
        draw_notice_bar(&mut buf, 40, 20, (0, 0, 40, 20), &["Discard 2 annotations?", "Esc or Enter"], 0x00_FF_B0_20, 0x00_FF_D8_8A);
        assert!(buf.iter().all(|&px| px == 0), "no room for the chip means no chip");
    }

    #[test]
    fn notice_bar_paints_inside_a_roomy_surface() {
        let (w, h) = (600usize, 300usize);
        let mut buf = vec![0u32; w * h];
        draw_notice_bar(&mut buf, w, h, (0, 0, w as i32, h as i32), &["Select mode", "click ink to move it"], 0x00_00_FF_95, 0x00_C8_FF_E4);
        assert!(buf.iter().any(|&px| px != 0), "the chip should have been drawn");
        // Bottom rows stay untouched — the bar is pinned to the top.
        assert!(buf[(h - 1) * w..].iter().all(|&px| px == 0));
    }

    #[test]
    fn timecode_formats_minutes_and_seconds() {
        assert_eq!(format_timecode(0.0),    "0:00");
        assert_eq!(format_timecode(12.34),  "0:12");
        assert_eq!(format_timecode(75.0),   "1:15");
        assert_eq!(format_timecode(3601.0), "60:01");
    }
}

