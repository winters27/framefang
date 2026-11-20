//! Opening a session: each kind's constructor, the window they share, and
//! getting the keyboard to it on platforms that are reluctant to give it.

use std::path::PathBuf;
use std::rc::Rc;
use std::time::Instant;

use anyhow::{anyhow, Result};
use framefang_core::annotation::Palettes;
use framefang_core::dpi::DisplayMap;
use framefang_core::history::History;
use framefang_core::settings::AppSettings;
use framefang_core::state::Edge;
use framefang_core::virtual_desktop::DesktopGeometry;
use image::{ImageBuffer, Rgba};
use softbuffer::{Context, Surface};
use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::event_loop::ActiveEventLoop;
use winit::keyboard::ModifiersState;
use winit::window::{CursorIcon, Fullscreen, WindowAttributes, WindowLevel};

use super::dock::DockFrame;
use super::flyout::{opening_tool, step_look, tool_stroke, Slider};
use super::geometry::action_room;
use super::video::Clip;
use super::*;
use crate::pick::PickMode;

type Image = ImageBuffer<Rgba<u8>, Vec<u8>>;

/// An ordinary window's spot and size, for the editor window; `None` for
/// the full-desktop overlay.
type Windowed = Option<((i32, i32), (u32, u32))>;

impl Overlay {
    /// The capture screen over `screenshot`, the desktop as it was when the
    /// capture key went. `desktop` is the geometry it was stitched from
    /// (`Captured::geometry`): it spreads the overlay over every monitor
    /// and maps each selection back to the right pixels. `settings` is the
    /// session's own copy; what changes in it goes back as a patch.
    pub fn new(
        loop_target: &ActiveEventLoop,
        desktop: DesktopGeometry,
        screenshot: ImageBuffer<Rgba<u8>, Vec<u8>>,
        settings: AppSettings,
    ) -> Result<Self> {
        let mut me = Self::build(loop_target, screenshot, desktop, settings, Mode::Picker, None, None)?;
        me.hover.mode = PickMode::parse(&me.settings.capture_mode);
        me.hover.opened_with = me.hover.mode;
        Ok(me)
    }

    /// The frozen screen, undimmed, with the color loupe at the pointer, for
    /// the editor window's eyedropper. A click answers `ColorPicked`; Esc
    /// or a right click answers `Cancelled`.
    pub fn new_for_color_pick(
        loop_target: &ActiveEventLoop,
        desktop: DesktopGeometry,
        screenshot: ImageBuffer<Rgba<u8>, Vec<u8>>,
        settings: AppSettings,
    ) -> Result<Self> {
        let mut me = Self::build(loop_target, screenshot, desktop, settings, Mode::ColorPick, None, None)?;
        me.eyedropper = true;
        Ok(me)
    }

    /// The editor window for a region the picker let go: borderless around
    /// the picture, placed so the picture sits exactly where it was. The
    /// rest of the desktop is live again; only this picture is frozen. Its
    /// selection is locked to the picture, as a video's is to the frame,
    /// and a press on the canvas around it moves the window.
    pub fn new_for_editor(loop_target: &ActiveEventLoop, chosen: ChosenRegion, settings: AppSettings) -> Result<Self> {
        let ChosenRegion { picture, placement } = chosen;
        let canvas = canvas_with(&picture, placement.size, placement.picture_at);
        let mut me = Self::open_editor(loop_target, canvas, &picture, placement.pos, placement.size, placement.picture_at, settings)?;
        // Drawn now: the window starts hidden so it can be cut to shape
        // first, and Windows never asks a hidden window to paint. Waiting
        // for that left the editor invisible for good, and the tray, still
        // holding it, ignored every capture after. This frame cuts the
        // shape and then shows the window.
        me.redraw();
        Ok(me)
    }

    /// The scrolling editor, for a picture taller than the screen: a window
    /// as tall as the work area with the page scrolling inside it and the
    /// page map beside it (`page_map`). Otherwise the editor window.
    pub fn new_for_tall_editor(loop_target: &ActiveEventLoop, chosen: TallChosen, settings: AppSettings) -> Result<Self> {
        let TallChosen { picture, placement } = chosen;
        let (w, view_h) = placement.size;
        let at = placement.picture_at;
        // The page: the picture with the action row's room under it, never
        // shorter than the window.
        let doc_h = (at.1 + picture.height() as i32 + action_room()).max(view_h as i32);
        let canvas = canvas_with(&picture, (w, doc_h as u32), at);
        let mut me = Self::open_editor(loop_target, canvas, &picture, placement.pos, placement.size, at, settings)?;
        let map = crate::page_map::map_rect(w as i32, view_h as i32, placement.map_left);
        me.page = Some(TallPage {
            view: crate::page_map::View { doc_h, view_h: view_h as i32, y: 0 },
            doc: Vec::new(),
            thumb: crate::page_map::thumbnail(&picture, map),
            map_left: placement.map_left,
            dragging: false,
            hovered: false,
            ink_sig: 0,
        });
        // The first frame cuts the shape and shows the window, as in
        // `new_for_editor`.
        me.redraw();
        Ok(me)
    }

    /// What both editor windows share: the canvas in a window at `pos`, the
    /// picture locked in as the selection, and no tool in hand, so the
    /// picture is the window's handle until one is picked and no flyout
    /// covers anything.
    fn open_editor(
        loop_target: &ActiveEventLoop,
        canvas: Image,
        picture: &Image,
        pos: (i32, i32),
        size: (u32, u32),
        at: (i32, i32),
        settings: AppSettings,
    ) -> Result<Self> {
        let desktop = DesktopGeometry::bitmap(canvas.width(), canvas.height());
        let mut me = Self::build(loop_target, canvas, desktop, settings, Mode::Editor, None, Some((pos, size)))?;
        me.state = State::Ready;
        me.selection = Some((at.0, at.1, picture.width() as i32, picture.height() as i32));
        me.picture = me.selection;
        me.window.set_cursor(CursorIcon::Default);
        me.armed = false;
        me.flyout = None;
        me.show_palette_of_color();
        Ok(me)
    }

    /// A video frame to annotate. The whole frame is the selection, locked
    /// (no crop, no new region, no resize), and the finish hands back the
    /// ink and the frame (`compose_overlay_groups`) for the tray to turn
    /// into overlays and burn over the clip with ffmpeg. With a known
    /// `duration_secs` a timeline lets the clip be scrubbed and each new
    /// annotation stamped with how long it shows; without one the bar
    /// stays hidden and all ink spans the clip. Copy and Pin still work and
    /// make a still of the frame as it looks, so the row has no dead buttons.
    pub fn new_for_video(
        loop_target: &ActiveEventLoop,
        frame: ImageBuffer<Rgba<u8>, Vec<u8>>,
        settings: AppSettings,
        video: PathBuf,
        duration_secs: Option<f32>,
    ) -> Result<Self> {
        // Not a screen capture: drawn at framebuffer (0, 0) whatever monitor
        // the window lands on, so a monitor-less geometry keeps every
        // mapping an identity.
        let desktop = DesktopGeometry::bitmap(frame.width(), frame.height());
        Self::build(loop_target, frame, desktop, settings, Mode::Video, Some(Clip::new(video, duration_secs)), None)
    }

    /// The capture screen as a selector for a region to record: the same
    /// drag, handles, magnifier and size chip, so there is no second
    /// selector to learn, but nothing to draw with, since nothing drawn
    /// here could reach the video. Confirming answers `RecordRegion`.
    pub fn new_for_region_record(
        loop_target: &ActiveEventLoop,
        desktop: DesktopGeometry,
        screenshot: ImageBuffer<Rgba<u8>, Vec<u8>>,
        settings: AppSettings,
    ) -> Result<Self> {
        let mut me = Self::build(loop_target, screenshot, desktop, settings, Mode::Record, None, None)?;
        me.hover.mode = PickMode::parse(&me.settings.capture_mode);
        Ok(me)
    }

    fn build(
        loop_target: &ActiveEventLoop,
        shot: Image,
        desktop: DesktopGeometry,
        settings: AppSettings,
        mode: Mode,
        clip: Option<Clip>,
        windowed: Windowed,
    ) -> Result<Self> {
        let (window, cloaked) = open_window(loop_target, &shot, &desktop, mode, windowed)?;
        let _softbuffer = Context::new(window.clone()).map_err(|e| anyhow!("no software framebuffer for the capture window: {e}"))?;
        let surface = Surface::new(&_softbuffer, window.clone()).map_err(|e| anyhow!("no drawing surface for the capture window: {e}"))?;

        let (w, h) = (shot.width(), shot.height());
        // A video frame opens with all of it as the selection, so every
        // tool works at once and nothing can be cropped.
        let video = mode == Mode::Video;
        let tool = opening_tool(&settings, mode.tool_set());
        let look = step_look(&settings);
        let frame_origin = desktop.origin();
        Ok(Overlay {
            window,
            _softbuffer,
            surface,
            mode,
            display: DisplayMap::identity(w, h),
            shot,
            desktop,
            frame_origin,
            origin_noted: false,
            state: if video { State::Ready } else { State::Waiting },
            pointer: (0, 0),
            pointer_win: (0, 0),
            anchor: (0, 0),
            selection: video.then_some((0, 0, w as i32, h as i32)),
            picture: None,
            crop_drag: None,
            grabbed_edge: Edge::None,
            cleared_selection: None,
            tool,
            armed: true,
            stroke: tool_stroke(&settings, tool),
            ink: Vec::new(),
            history: History::new(),
            draft: None,
            preedit: String::new(),
            next_step: 1,
            palette: (settings.palette_index.max(0) as usize).min(Palettes::ALL.len() - 1),
            text_px: Slider::TextSize.usable(settings.last_text_size),
            block_px: Slider::BlockSize.usable(settings.pixelate_block as f32) as u32,
            step_look: look,
            step_part: look.shape.accent_part(),
            mark_style: settings.highlight_style,
            mark_from: None,
            spectrum_at: None,
            words: Words::default(),
            select: SelectMode::default(),
            flyout: None,
            flyout_since: Instant::now(),
            held: None,
            eyedropper: false,
            wants_screen_pick: false,
            mods: ModifiersState::empty(),
            #[cfg(not(windows))]
            keys_down: Default::default(),
            #[cfg(windows)]
            esc_was_down: false,
            settings_base: settings.clone(),
            settings,
            unsent: Default::default(),
            tip: None,
            confirm: None,
            copied_at: None,
            opened_at: Instant::now(),
            pulse_from: None,
            was_ready: false,
            anim_at: None,
            hover: Hover {
                mode: PickMode::Auto,
                opened_with: PickMode::Auto,
                target: None,
                glide_from: (0, 0, 0, 0),
                glide_at: Instant::now(),
                press_pending: false,
                windows: Vec::new(),
                handles: Vec::new(),
                probe: None,
                asked: None,
                scroll_parts: Vec::new(),
                tab_at: None,
                decode_at: None,
                dock_drawn: DockFrame::default(),
                dock_since: None,
            },
            reading: None,
            read_drawn: None,
            last_pick: None,
            queued: None,
            hold_at_press: false,
            clip,
            page: None,
            backdrop: Default::default(),
            cut: Vec::new(),
            shown: !cloaked && (windowed.is_none() || !crate::window_shape::supported()),
            frame_times: std::env::var_os("FRAMEFANG_FRAME_LOG").map(|_| Vec::new()),
            #[cfg(target_os = "linux")]
            x11_focus: (false, 0),
        })
    }

    /// Windows: keep the keyboard on a full-screen capture window (the
    /// picker, the record picker, the color pick); the tray calls this every
    /// tick. Windows may refuse focus to a window a background app opens,
    /// and Esc then went to whatever was behind the frozen screen, which
    /// stayed up (Brandon 2026-11-19). So while another window has the
    /// keyboard, the overlay claims it back for its first 2 s, and an Esc
    /// pressed anywhere is answered as if it had arrived. The editor window
    /// is an ordinary window someone may well leave, so it is left alone.
    #[cfg(windows)]
    pub fn guard_focus(&mut self) {
        use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_ESCAPE};
        use windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow;
        use winit::keyboard::{Key, NamedKey};
        if self.is_editor() || self.queued.is_some() { return; }
        let esc = unsafe { GetAsyncKeyState(VK_ESCAPE.0 as i32) as u16 & 0x8000 != 0 };
        let pressed = esc && !self.esc_was_down;
        self.esc_was_down = esc;
        let Some(hwnd) = hwnd_of(&self.window) else { return };
        if unsafe { GetForegroundWindow() } == hwnd { return; }
        if self.opened_at.elapsed() < std::time::Duration::from_secs(2) { force_foreground(hwnd); }
        if !pressed { return; }
        let answer = match self.reading {
            Some(_) => OverlayOutcome::Cancelled,
            None => self.handle_key(Key::Named(NamedKey::Escape)),
        };
        if !matches!(answer, OverlayOutcome::Continue) { self.queued = Some(answer); }
        self.repaint();
    }

    /// X11: take keyboard focus and grab the keyboard by hand. Tried before
    /// every frame until it works, since the first frames may come before
    /// the window is viewable and X refuses focus to an unviewable window;
    /// Cinnamon maps it within two or three. Given up after 60 tries,
    /// because some window managers never allow it (GNOME's
    /// focus-stealing prevention, some tiling ones), and retrying forever
    /// redrew without end and held a core.
    #[cfg(target_os = "linux")]
    pub(super) fn push_x11_focus(&mut self) {
        use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
        const TRIES: u32 = 60;
        if self.x11_focus.0 { return; }
        self.x11_focus.1 = self.x11_focus.1.saturating_add(1);
        let xid = match self.window.window_handle().map(|h| h.as_raw()) {
            Ok(RawWindowHandle::Xlib(x)) => x.window as u32,
            // Not X11 (Wayland), so nothing to do.
            _ => { self.x11_focus.0 = true; return; }
        };
        match force_x11_focus(xid) {
            Ok(()) => {
                eprintln!("framefang: took X11 keyboard focus for window 0x{xid:x}");
                self.x11_focus.0 = true;
            }
            Err(e) if self.x11_focus.1 >= TRIES => {
                eprintln!("framefang: X11 focus still refused after {} tries ({e}); typed text may not reach the Text tool under this window manager", self.x11_focus.1);
                self.x11_focus.0 = true;
            }
            Err(e) => eprintln!("framefang: X11 focus refused for now, trying again next frame ({e})"),
        }
    }
}

/// `picture` on a canvas `size` big, at `at`; the rest is canvas color.
fn canvas_with(picture: &Image, size: (u32, u32), at: (i32, i32)) -> Image {
    let mut canvas = ImageBuffer::from_pixel(size.0, size.1, Rgba(EDITOR_CANVAS));
    image::imageops::replace(&mut canvas, picture, i64::from(at.0), i64::from(at.1));
    canvas
}

/// The session's window, and whether it starts cloaked.
///
/// The editor window is an ordinary stacking window, so the rest of the
/// desktop can come in front of it again, hidden until its first frame
/// has cut it to shape so the canvas never flashes up as a dark box.
///
/// Every other session covers the desktop, borderless and always on top.
/// Across several monitors it has to cover their union: a region on a
/// monitor left of or above the primary one starts at a negative
/// coordinate, which a window the size of the primary can't even show.
/// One window over the union is the shape all three platforms support the
/// same way (borderless fullscreen takes a single monitor, and a window per
/// monitor would need a pointer grab for a drag from one to the next, which
/// behaves differently everywhere). Windows and X11 place it natively;
/// macOS does too unless "Displays have separate Spaces" is on and clamps
/// it to one display, which `sync_frame_origin` copes with by reading where
/// it really is. Wayland lets no client place a window at all, so there the
/// fullscreen window on the primary monitor is the only shape it honours.
///
/// With the plain fullscreen window the WM manages focus and stacking as
/// usual. (Override-redirect would sit above dock panels on X11, but on
/// Cinnamon keyboard focus then never reaches the window and the Text tool
/// gets no keys, so dock panels may show at the edges instead.) The
/// explicit size, position and fullscreen request together are what make
/// Cinnamon open it full screen rather than at about 800 x 600.
fn open_window(
    loop_target: &ActiveEventLoop,
    shot: &Image,
    desktop: &DesktopGeometry,
    mode: Mode,
    windowed: Windowed,
) -> Result<(Rc<Window>, bool)> {
    let primary = loop_target.primary_monitor().or_else(|| loop_target.available_monitors().next());
    let screen_size = primary.as_ref().map_or(PhysicalSize::new(shot.width(), shot.height()), |m| m.size());
    let (origin, (dw, dh)) = (desktop.origin(), desktop.size());
    let union_size = PhysicalSize::new(dw.max(1), dh.max(1));
    let title = if mode == Mode::Video { "Frame Fang: annotate recording" } else { "Frame Fang" };
    let base = WindowAttributes::default()
        .with_title(title)
        .with_decorations(false)
        .with_resizable(false)
        .with_window_level(WindowLevel::AlwaysOnTop)
        .with_window_icon(crate::brand_icon::shared());
    let spans_union = windowed.is_none() && desktop.spans_multiple_monitors() && !framefang_platform::session::is_wayland();
    let attrs = match windowed {
        Some((pos, size)) => base
            .with_window_level(WindowLevel::Normal)
            .with_visible(!crate::window_shape::supported())
            .with_inner_size(PhysicalSize::new(size.0.max(1), size.1.max(1)))
            .with_position(PhysicalPosition::new(pos.0, pos.1)),
        None if spans_union => base
            .with_inner_size(union_size)
            .with_position(PhysicalPosition::new(origin.0, origin.1)),
        None => base
            .with_inner_size(screen_size)
            .with_position(PhysicalPosition::new(0i32, 0i32))
            .with_fullscreen(Some(Fullscreen::Borderless(primary))),
    };
    // Windows: sized and placed while hidden, then shown cloaked, so nothing
    // reaches the screen before the first frame (`reveal`). Without it the
    // window popped up small and grew out from the top left.
    let cloaked = crate::reveal::SUPPORTED;
    let attrs = if cloaked { attrs.with_visible(false) } else { attrs };
    let window = Rc::new(loop_target.create_window(attrs).map_err(|e| anyhow!("could not open the capture window: {e}"))?);
    // Several WMs only apply size and position once the window is mapped,
    // whatever the request asked; ask again (a no-op when it already fits).
    if spans_union {
        window.set_outer_position(PhysicalPosition::new(origin.0, origin.1));
        let _ = window.request_inner_size(union_size);
    }
    if cloaked {
        crate::reveal::cloak(&window);
        window.set_visible(true);
    }
    window.set_cursor(CursorIcon::Crosshair);
    // The standard request for focus. Cinnamon ignores it, so on X11
    // `push_x11_focus` follows up once the window can be focused.
    window.focus_window();
    Ok((window, cloaked))
}

/// The window's Win32 handle.
#[cfg(windows)]
pub(super) fn hwnd_of(window: &Window) -> Option<windows::Win32::Foundation::HWND> {
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let RawWindowHandle::Win32(h) = window.window_handle().ok()?.as_raw() else { return None };
    Some(windows::Win32::Foundation::HWND(h.hwnd.get() as *mut core::ffi::c_void))
}

/// Give `hwnd` the keyboard. Windows lets the owner of the foreground
/// window hand focus over, so for the call this thread shares that
/// window's input state (`AttachThreadInput`); a background app's plain
/// `SetForegroundWindow` is often refused without it.
#[cfg(windows)]
fn force_foreground(hwnd: windows::Win32::Foundation::HWND) {
    use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
    use windows::Win32::UI::Input::KeyboardAndMouse::SetFocus;
    use windows::Win32::UI::WindowsAndMessaging::{BringWindowToTop, GetForegroundWindow, GetWindowThreadProcessId, SetForegroundWindow};
    unsafe {
        let front = GetForegroundWindow();
        let owner = if front.is_invalid() { 0 } else { GetWindowThreadProcessId(front, None) };
        let me = GetCurrentThreadId();
        let joined = owner != 0 && owner != me && AttachThreadInput(me, owner, true).as_bool();
        let _ = BringWindowToTop(hwnd);
        let _ = SetForegroundWindow(hwnd);
        let _ = SetFocus(Some(hwnd));
        if joined { let _ = AttachThreadInput(me, owner, false); }
    }
}

/// Set X11 input focus to `xid` and grab the keyboard for it. Cinnamon
/// ignores winit's `_NET_ACTIVE_WINDOW` request and never routes key
/// presses to the overlay otherwise, so the Text tool would get nothing.
#[cfg(target_os = "linux")]
fn force_x11_focus(xid: u32) -> Result<()> {
    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::{ConnectionExt, GrabMode, InputFocus};
    let (conn, _) = x11rb::connect(None).map_err(|e| anyhow!("no X11 connection: {e}"))?;
    let time = x11rb::CURRENT_TIME;
    conn.set_input_focus(InputFocus::PARENT, xid, time)
        .map_err(|e| anyhow!("asking X11 for focus: {e}"))?
        .check()
        .map_err(|e| anyhow!("X11 refused focus: {e}"))?;
    conn.grab_keyboard(false, xid, time, GrabMode::ASYNC, GrabMode::ASYNC)
        .map_err(|e| anyhow!("asking X11 for the keyboard: {e}"))?
        .reply()
        .map_err(|e| anyhow!("X11 refused the keyboard: {e}"))?;
    conn.flush().map_err(|e| anyhow!("sending to X11: {e}"))?;
    Ok(())
}

/// Let go of the X11 keyboard grab when the overlay closes, so the next
/// window gets typing back. Nothing on Wayland, where no grab was taken
/// and an X11 connection per closed overlay would only be noise.
#[cfg(target_os = "linux")]
pub(super) fn release_x11_focus() {
    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::ConnectionExt;
    if framefang_platform::session::is_wayland() { return; }
    if let Ok((conn, _)) = x11rb::connect(None) {
        let _ = conn.ungrab_keyboard(x11rb::CURRENT_TIME);
        let _ = conn.flush();
    }
}
