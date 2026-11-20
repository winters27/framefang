//! The capture screen and the annotation editor.
//!
//! One `Overlay` type covers every session that shows a frozen picture and
//! takes input on it:
//!
//! - the picker: the whole desktop frozen under a dim, where a region (or a
//!   window, a screen, all screens) is picked, and then edited in place
//!   when it can't have a window of its own;
//! - the editor window: the picked picture in a borderless window of its
//!   own, cut to shape, with the rest of the desktop live again (and the
//!   scrolling version of it for a picture taller than the screen);
//! - a video frame to annotate, with a timeline;
//! - a region to record, which is only the picker's selection;
//! - a color pick over the frozen screen, for the editor's eyedropper.
//!
//! A frame is the capture (dimmed outside the selection), the ink clipped
//! to the picture, the selection's frame and handles, the panels around
//! it, and whatever is open on top. Nothing is ever thrown away on one key:
//! with ink on the picture the first Esc only asks, and even a confirmed
//! clear goes onto the undo log as one step, so Ctrl+Z brings it all back,
//! the selection with it.
//!
//! winit allows one event loop, the tray's, so the overlay has no loop of
//! its own: `tray_loop` opens it inside the running loop and hands it each
//! `WindowEvent` through `Overlay::handle_event` until the answer is
//! anything but `Continue`. Drawing is on the CPU (softbuffer) with the
//! in-tree `painter`.
//!
//! Where things are:
//!
//! | file | what |
//! |------|------|
//! | `open` | opening each kind of session, the window, keyboard focus |
//! | `input` | events, keys, Esc and confirmations, the pointer |
//! | `tools` | picking tools, the flyout's controls, colors, Highlight's words |
//! | `hover` | the picker's highlight, modes and dock; cursors and tips |
//! | `session` | what the tray asks of an overlay, coordinates, finishing |
//! | `tall` | the scrolling editor's page and page map |
//! | `render` | drawing a frame |
//! | `geometry`, `panels`, `flyout` | layout, no window needed |
//! | `paint`, `chrome`, `dock`, `video` | drawing helpers, the timeline |

mod chrome;
mod dock;
mod flyout;
mod geometry;
mod hover;
mod input;
mod open;
mod paint;
mod panels;
mod render;
mod session;
mod tall;
mod tools;
mod video;
#[cfg(test)]
mod previews;

use std::rc::Rc;
use std::time::Instant;

use framefang_core::annotation::{Annotation, Stroke};
use framefang_core::dpi::DisplayMap;
use framefang_core::history::History;
use framefang_core::mark::{MarkStyle, Word};
use framefang_core::settings::AppSettings;
use framefang_core::state::Edge;
use framefang_core::step::{StepLook, StepPart};
use framefang_core::tool::Tool;
use framefang_core::virtual_desktop::DesktopGeometry;
use image::{ImageBuffer, Rgba};
use softbuffer::{Context, Surface};
use winit::keyboard::ModifiersState;
use winit::window::Window;

pub(crate) use geometry::crop;
pub use geometry::{chosen_for, tall_for, toast_spot};
pub(crate) use video::compose_overlay_groups;

use dock::DockFrame;
use flyout::Slider;
use geometry::{CropDrag, Rect4};
use video::Clip;

/// What the tray does after the overlay handled an event.
pub enum OverlayOutcome {
    /// Nothing yet; more events to come.
    Continue,
    /// Backed out (Esc, a close with nothing to lose). Drop the overlay.
    Cancelled,
    /// The picture, finished (Enter, a right click, Save, Ctrl+S): save it
    /// to the output folder.
    Accepted(ImageBuffer<Rgba<u8>, Vec<u8>>),
    /// Save As (Ctrl+Shift+S, or Save with Shift): ask where and in which
    /// format with the overlay hidden, and bring it back with
    /// `set_hidden(false)` if the dialog is cancelled.
    SaveAs(ImageBuffer<Rgba<u8>, Vec<u8>>),
    /// Copy (Ctrl+C, Copy): put the picture on the clipboard, and close.
    Copied(ImageBuffer<Rgba<u8>, Vec<u8>>),
    /// Copy with `close_after_copy` off: the same, but the editor stays.
    CopiedOpen(ImageBuffer<Rgba<u8>, Vec<u8>>),
    /// Pin (Ctrl+P, Pin): float the picture in an always-on-top window,
    /// its top left at this virtual-screen point, where it was cut from.
    Pinned(ImageBuffer<Rgba<u8>, Vec<u8>>, (i32, i32)),
    /// A video session's finish: the ink and the frame it was drawn on.
    /// The overlays are composed on the burn thread
    /// (`compose_overlay_groups`), since each timed run may need its own
    /// ffmpeg seek and that must not stall the event loop.
    AcceptedVideo(VideoCommit),
    /// Record this region instead of capturing it (the Record button, or
    /// Enter in a record-region session), in virtual-screen pixels. The
    /// tray clamps it to the desktop and starts the recorder; nothing is
    /// saved.
    RecordRegion((i32, i32, i32, i32)),
    /// The editor's eyedropper wants any color on screen, not only one in
    /// its picture: freeze the screen, open `new_for_color_pick` over it,
    /// and hand its answer back with `take_screen_color`.
    PickScreenColor,
    /// A color-pick session's answer: the color clicked.
    ColorPicked(framefang_core::color::Rgba),
    /// The picker let a region go: open it in its own editor window
    /// (`Overlay::new_for_editor`) and give the rest of the screen back.
    Chosen(Box<ChosenRegion>),
    /// TEXT or QR: read this picture. The picker stays frozen, saying it
    /// is reading, until the tray has the answer or Esc cancels. The flag:
    /// the switch key was held as it was let go, so a QR link does the
    /// other thing (opens instead of copying, or the reverse).
    ReadText(ImageBuffer<Rgba<u8>, Vec<u8>>, crate::read_text::ReadKind, bool),
    /// SCROLL: scroll this area (desktop pixels) to its end and stitch it.
    /// The frozen screen goes so the real page can move. The flag: the
    /// switch key was held, so the result does the other thing (the editor
    /// opens on copy only, or the reverse).
    Scroll((i32, i32, i32, i32), bool),
}

/// A picked region on its way from the picker to its editor window.
pub struct ChosenRegion {
    pub picture: ImageBuffer<Rgba<u8>, Vec<u8>>,
    pub placement: EditorPlacement,
}

/// Where an editor window opens, in virtual-screen pixels, and where its
/// picture sits inside it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EditorPlacement {
    pub pos: (i32, i32),
    pub size: (u32, u32),
    pub picture_at: (i32, i32),
}

/// A picture taller than an editor window can be (a scrolling capture) on
/// its way to the scrolling editor (`Overlay::new_for_tall_editor`).
pub struct TallChosen {
    pub picture: ImageBuffer<Rgba<u8>, Vec<u8>>,
    pub placement: crate::page_map::TallPlacement,
}

/// One overlay for the video burn: the ink alone on transparency, and the
/// clip seconds it shows from and until (`None`: to the clip's end).
pub type OverlayGroup = (ImageBuffer<Rgba<u8>, Vec<u8>>, f32, Option<f32>);

/// What `AcceptedVideo` carries for `compose_overlay_groups`.
pub struct VideoCommit {
    /// The ink in draw order, each with its time window.
    pub annotations: Vec<Annotation>,
    /// The frame on screen at the finish: the background for runs that
    /// start at its moment, and the fallback when a run's own seek fails.
    pub frame: ImageBuffer<Rgba<u8>, Vec<u8>>,
    /// When `frame` was taken from.
    pub frame_t: f32,
    /// The clip's length, to keep seeks inside it. `None` when ffmpeg's
    /// banner didn't give one.
    pub duration: Option<f32>,
}

/// The editor's one green, for highlights, notices, the pulse and the
/// timeline: a deep, saturated phosphor green rather than mint.
pub(crate) const NEON_GREEN: u32 = 0x00_00_FF_41;
/// Words on a green notice: near-white with a breath of the green.
const NEON_GREEN_TEXT: u32 = 0x00_D6_FF_DE;
/// The editor window's canvas around the picture. The backdrop dims it
/// like anything outside a selection, to `EDITOR_CLEAR`, and on Windows
/// every pixel still that color is cut out of the window (`window_shape`),
/// so the picture and its panels float over the live desktop.
const EDITOR_CANVAS: [u8; 4] = [0x33, 0x33, 0x39, 0xFF];
const EDITOR_CLEAR: u32 = {
    // The backdrop's dim keeps 55 % of each channel.
    const fn dim(v: u8) -> u32 { v as u32 * 55 / 100 }
    (dim(EDITOR_CANVAS[0]) << 16) | (dim(EDITOR_CANVAS[1]) << 8) | dim(EDITOR_CANVAS[2])
};
/// The editor window's own edge, drawn only where it can't be cut to shape.
const EDITOR_FRAME: u32 = 0x00_3A_3A_40;

/// Which session an overlay is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// The frozen desktop of a capture.
    Picker,
    /// The frozen desktop, picking a region to record: no tools, nothing to
    /// save.
    Record,
    /// The frozen desktop, undimmed, for one click's color.
    ColorPick,
    /// A picture in its own window.
    Editor,
    /// A video frame.
    Video,
}

impl Mode {
    /// The selection is fixed: the editor's picture, or the video frame.
    fn locked(self) -> bool { matches!(self, Mode::Editor | Mode::Video) }

    /// Sessions that light up what a click would take before a region is
    /// picked.
    fn hovers(self) -> bool { matches!(self, Mode::Picker | Mode::Record) }

    fn tool_set(self) -> panels::ToolSet {
        match self {
            Mode::Editor => panels::ToolSet::Editor,
            Mode::Video => panels::ToolSet::Video,
            Mode::Picker | Mode::Record | Mode::ColorPick => panels::ToolSet::Picker,
        }
    }
}

/// Where the pointer and the selection are in their life.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// No region yet.
    Waiting,
    /// The button is down and a region is being dragged out.
    Dragging,
    /// A region is picked: panels up, tools live.
    Ready,
    /// The button is down with a tool in hand: `draft` follows the pointer
    /// and joins the ink when the button comes up.
    Inking,
    /// An edge or corner of the selection is held (`grabbed_edge`).
    Resizing,
    /// The Text tool's caret is placed; keys type into `draft`.
    Typing,
    /// Select mode: an annotation is held and follows the pointer, one
    /// undo step when the button comes up.
    Moving,
}

/// What an armed Esc confirmation would throw away.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Discard {
    /// The region and its ink (on the undo log, so still recoverable).
    Region,
    /// The whole session. Nothing survives this one.
    Session,
}

/// What the held left button is dragging, besides the selection's states.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Held {
    /// A flyout slider: saved when the button comes up.
    Slider(Slider),
    /// The flyout's spectrum: the color follows, saved when it comes up.
    Spectrum,
    /// The video timeline's playhead.
    Playhead,
}

/// Select mode (S): clicks take hold of existing ink instead of drawing.
#[derive(Debug, Default)]
struct SelectMode {
    on: bool,
    /// The annotation picked, by index. Every undo and redo drops it, since
    /// indices shift under them.
    picked: Option<usize>,
    /// The pointer when the held annotation last moved.
    last: (i32, i32),
    /// How far it has moved since it was taken, for its one undo step.
    moved: (f32, f32),
}

/// A message from the thread reading the picture's words.
type WordsReply = Result<Vec<framefang_platform::ocr::WordBox>, String>;

/// Highlight's words, read once per rectangle on their own thread.
#[derive(Default)]
struct Words {
    /// What was read (canvas pixels, reading order); empty when nothing was.
    found: Option<Vec<Word>>,
    /// The bitmap rectangle `found` came from.
    of: Option<Rect4>,
    /// A reading under way, and where its picture sits on the canvas.
    job: Option<(std::sync::mpsc::Receiver<WordsReply>, (i32, i32))>,
    /// Why the last reading failed.
    error: Option<String>,
}

/// The picker's highlight on what a click takes, its modes and its dock.
struct Hover {
    /// What a click takes before a region is dragged.
    mode: crate::pick::PickMode,
    /// The mode this capture opened in: a reading mode is never kept for
    /// the next capture, this one is.
    opened_with: crate::pick::PickMode,
    /// What a click would take now, in frame pixels, and its name.
    target: Option<(Rect4, String)>,
    /// The highlight glides to a new target from here, since then.
    glide_from: Rect4,
    glide_at: Instant,
    /// A press in Screen or All screens mode, waiting for its release.
    press_pending: bool,
    /// The windows on screen when it froze, frontmost first, as
    /// virtual-screen frames and titles; empty where the platform can't
    /// list them, and the highlight then shows the screen.
    windows: Vec<(Rect4, String)>,
    /// Their handles, in the same order (0 where unknown).
    handles: Vec<isize>,
    /// SCROLL: the helper that finds which part of a window scrolls, the
    /// question last put to it, and the parts found (handle, rectangle).
    probe: Option<crate::scroll_probe::Probe>,
    asked: Option<crate::scroll_probe::Ask>,
    scroll_parts: Vec<(isize, Rect4)>,
    /// Tab was last pressed then (its key shows down for a moment).
    tab_at: Option<Instant>,
    /// The mode last changed then (its chip decodes for a moment).
    decode_at: Option<Instant>,
    /// The dock's look when it was last drawn.
    dock_drawn: DockFrame,
    /// When the dock first reached the screen, which the sweep and the
    /// blink run from. Not from the opening: freezing the screen and the
    /// first frame take a good part of a second (Brandon 2026-11-19: "its
    /// practically done by the time its there").
    dock_since: Option<Instant>,
}

/// TEXT or QR handed its picture over and waits for the answer.
#[derive(Debug, Clone, Copy)]
struct Reading {
    since: Instant,
    /// The switch key was held as it was let go: QR links do the other
    /// thing, and the prompt keeps saying so.
    flip: bool,
}

/// The scrolling editor: a picture taller than the window. The page is
/// drawn whole into `doc` in page pixels and the slice `view.y` down is
/// shown; the pointer is in page pixels too.
struct TallPage {
    view: crate::page_map::View,
    doc: Vec<u32>,
    /// The page map: the page drawn small, on this side of the picture.
    thumb: ImageBuffer<Rgba<u8>, Vec<u8>>,
    map_left: bool,
    /// A press on the map is being dragged, and the pointer is over it.
    dragging: bool,
    hovered: bool,
    /// The ink when the map was last drawn (`tall::ink_signature`).
    ink_sig: u64,
}

pub struct Overlay {
    window: Rc<Window>,
    _softbuffer: Context<Rc<Window>>,
    surface: Surface<Rc<Window>, Rc<Window>>,
    mode: Mode,
    /// The frozen picture: the desktop capture, the editor's canvas with
    /// the picture on it, or the video frame. In captured-bitmap pixels,
    /// which are device pixels like the pointer and the window size, so
    /// nothing inside the overlay converts.
    shot: ImageBuffer<Rgba<u8>, Vec<u8>>,
    /// Logical to device pixels for `shot`: what lifts a rectangle back to
    /// the desktop (a pin, a recording), and how big a device pixel is, so
    /// handles stay the same size on a high-DPI screen. A 1x identity until
    /// the tray hands over the real one.
    display: DisplayMap,
    /// Where the stitched capture sits on the virtual desktop and which
    /// monitors made it. Video and editor sessions carry a monitor-less
    /// geometry, which keeps every mapping an identity.
    desktop: DesktopGeometry,
    /// The virtual-screen point under framebuffer (0, 0): the desktop's
    /// origin when the window landed where it was asked to, read back from
    /// the window before every frame (`sync_frame_origin`).
    frame_origin: (i32, i32),
    /// A window manager placed the overlay off the desktop's origin, and
    /// the log has said so once.
    origin_noted: bool,
    state: State,
    /// The pointer, in page pixels (window pixels except in the scrolling
    /// editor), and in window pixels.
    pointer: (i32, i32),
    pointer_win: (i32, i32),
    /// Where the region drag started.
    anchor: (i32, i32),
    /// `(x, y, w, h)` in frame pixels, never negative.
    selection: Option<Rect4>,
    /// The editor window's whole picture, fixed at open; the selection is
    /// the crop inside it.
    picture: Option<Rect4>,
    crop_drag: Option<CropDrag>,
    /// The selection's edge being dragged while `Resizing`.
    grabbed_edge: Edge,
    /// The selection when the ink was last cleared off it; undoing the
    /// clear puts it back, so a stray Esc costs nothing.
    cleared_selection: Option<Rect4>,

    tool: Tool,
    /// Whether `tool` is in hand. The editor window opens with none, when a
    /// drag on the picture moves the window; picking a tool takes it up,
    /// picking it again or Esc puts it down. Everywhere else one is always
    /// in hand.
    armed: bool,
    stroke: Stroke,
    ink: Vec<Annotation>,
    /// Undo and redo over `ink`, as edits, so moving and deleting are
    /// undoable too; anything new clears the redo side.
    history: History,
    /// The annotation being drawn or typed.
    draft: Option<Annotation>,
    /// An input method's composition, drawn after the caret but never in
    /// the text: only its commit is.
    preedit: String,
    /// The next Step number. Back to 1 when the region is cleared.
    next_step: u32,
    /// The palette the flyout shows (`Palettes::ALL`), kept apart from the
    /// stroke so leafing through palettes changes no color until a swatch
    /// is picked.
    palette: usize,
    /// Text's own size and Pixelate's block size.
    text_px: f32,
    block_px: u32,
    /// The next step marker's look, and which part of it the colors paint.
    step_look: StepLook,
    step_part: StepPart,
    /// The look the next highlight takes, and the word a highlight drag
    /// began on.
    mark_style: MarkStyle,
    mark_from: Option<usize>,
    /// Where in the spectrum the color was last taken (0 to 1 each way),
    /// for its marker while the tool still has that color.
    spectrum_at: Option<(f32, f32)>,
    words: Words,
    select: SelectMode,

    /// The tool whose flyout is out, and since when (for the slide).
    flyout: Option<Tool>,
    flyout_since: Instant,
    held: Option<Held>,
    /// The eyedropper is armed: the next click takes the color under it.
    eyedropper: bool,
    /// It was armed in the editor window, which wants the whole screen to
    /// pick from: the press answers `PickScreenColor`.
    wants_screen_pick: bool,

    /// winit puts no modifiers on key events; `ModifiersChanged` keeps this.
    mods: ModifiersState,
    /// Keys this window heard go down and not up, by virtual-key code, for
    /// a hold key that is no modifier. Windows asks the system instead.
    #[cfg(not(windows))]
    keys_down: std::collections::HashSet<u32>,
    /// Esc was down at the last `guard_focus`, so one press counts once.
    #[cfg(windows)]
    esc_was_down: bool,

    /// The session's copy of the settings. The tray gets what changed as a
    /// patch (`take_patch`); the editor never writes the file itself.
    settings: AppSettings,
    /// `settings` as last handed over or adopted: changes are relative to it.
    settings_base: AppSettings,
    /// Changes the tray hasn't taken yet.
    unsent: framefang_core::settings_store::Patch,

    /// The hovered button's tip and where it goes.
    tip: Option<(&'static str, i32, i32)>,
    /// An Esc confirmation on screen: the next Esc goes through with it.
    confirm: Option<Discard>,
    /// A Copy that kept the editor open, for "Copied to clipboard".
    copied_at: Option<Instant>,
    opened_at: Instant,
    /// The action row first appeared then, for its pulse.
    pulse_from: Option<Instant>,
    was_ready: bool,
    /// The last frame an animation asked for itself, to pace the next.
    anim_at: Option<Instant>,

    hover: Hover,
    reading: Option<Reading>,
    /// The reading look's frame last drawn (`read_frame`).
    read_drawn: Option<u32>,
    /// The rectangle the picker last took (virtual-screen pixels), for
    /// "Capture last region".
    last_pick: Option<Rect4>,
    /// An answer reached outside an event (`guard_focus`), handed back by
    /// the next `handle_event` so the tray closes it the usual way.
    queued: Option<OverlayOutcome>,
    /// The capture's hold key was down when the hotkey fired, so this
    /// capture does the opposite of the After-a-capture setting.
    pub hold_at_press: bool,

    clip: Option<Clip>,
    page: Option<TallPage>,

    /// The capture dimmed outside the selection, kept between frames; a
    /// drag rewrites only the strips the selection crossed.
    backdrop: crate::backdrop::Backdrop,
    /// The shape last cut for the editor window, so it is cut again only
    /// when what surrounds the picture changes.
    cut: Vec<crate::window_shape::Rect>,
    /// The window is on screen. Windows starts every overlay cloaked until
    /// its first frame; X11 shows the editor window once it is cut.
    shown: bool,
    /// `FRAMEFANG_FRAME_LOG=1`: each frame's times, written out at close.
    frame_times: Option<Vec<(f32, f32, f32)>>,
    /// X11: keyboard focus taken by hand, and how often it was tried.
    #[cfg(target_os = "linux")]
    x11_focus: (bool, u32),
}

impl Drop for Overlay {
    fn drop(&mut self) {
        #[cfg(target_os = "linux")]
        open::release_x11_focus();
        if let Some(times) = self.frame_times.take() { write_frame_times(&times); }
    }
}

/// Add one session's frame times to `framefang-frames.log` in the temp
/// folder: a summary line, then `backdrop chrome total` in ms per frame.
fn write_frame_times(times: &[(f32, f32, f32)]) {
    use std::fmt::Write as _;
    use std::io::Write as _;
    if times.is_empty() { return; }
    let mut totals: Vec<f32> = times.iter().map(|t| t.2).collect();
    totals.sort_by(f32::total_cmp);
    let at = |share: f32| totals[((totals.len() - 1) as f32 * share).round() as usize];
    let mut text = format!("session frames={} total_ms p50={:.2} p95={:.2} max={:.2}\n", times.len(), at(0.5), at(0.95), at(1.0));
    for (backdrop, chrome, total) in times {
        let _ = writeln!(text, "{backdrop:.2} {chrome:.2} {total:.2}");
    }
    let path = std::env::temp_dir().join("framefang-frames.log");
    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = file.write_all(text.as_bytes());
    }
}

impl Overlay {
    /// Ask for a frame.
    fn repaint(&self) { self.window.request_redraw(); }

    /// The column this session shows.
    fn tool_set(&self) -> panels::ToolSet { self.mode.tool_set() }

    /// The frozen desktop of an ordinary capture: the one session that
    /// hands its region on to an editor window.
    fn is_picker(&self) -> bool { self.mode == Mode::Picker }

    fn is_editor(&self) -> bool { self.mode == Mode::Editor }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cut_out_color_is_what_the_backdrop_makes_of_the_canvas() {
        let px = EDITOR_CANVAS;
        let shot = crate::backdrop::Shot { rgba: &px, width: 1, height: 1, offset: (0, 0) };
        let mut backdrop = crate::backdrop::Backdrop::default();
        assert_eq!(backdrop.frame(1, 1, &shot, None), &[EDITOR_CLEAR]);
    }

    #[test]
    fn sessions_know_what_they_lock_and_show() {
        use panels::ToolSet;
        assert!(Mode::Editor.locked() && Mode::Video.locked() && !Mode::Picker.locked());
        assert!(Mode::Picker.hovers() && Mode::Record.hovers());
        assert!(!Mode::ColorPick.hovers() && !Mode::Editor.hovers() && !Mode::Video.hovers());
        assert_eq!(Mode::Record.tool_set(), ToolSet::Picker);
        assert_eq!(Mode::Video.tool_set(), ToolSet::Video);
        assert_eq!(Mode::Editor.tool_set(), ToolSet::Editor);
    }
}
