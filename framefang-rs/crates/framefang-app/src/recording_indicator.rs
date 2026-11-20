/*!
The floating recording panel: a small always-on-top window shown while a
screen recording runs, so stopping it is one click instead of a trip to the
tray menu.

The panel is 220 by 56 pixels with no frame:

```text
+--------------------------------------+
|  (o) REC  00:12             [ STOP ] |
+--------------------------------------+
```

The red dot blinks once a second (lit for the first 60% of each second) and
the clock counts wall time since the recording began. Dragging anywhere but
the button moves the panel; clicking STOP ends the recording.

`TrayApp` holds an `Option<RecordingIndicator>`, forwards the panel's
`WindowEvent`s to it by `WindowId`, and checks `stop_requested` after each one
to learn that STOP was pressed.

The panel must never end up in the video it controls, so `new_at` hides the
window from screen capture before handing it back:

- Windows: `SetWindowDisplayAffinity(WDA_EXCLUDEFROMCAPTURE)`, which makes DWM
  leave the window out of the BitBlt and DXGI reads gdigrab performs.
- macOS: `NSWindow.sharingType = NSWindowSharingNone`, which `screencapture`
  honours.
- Linux on X11 has nothing comparable (x11grab reads the composited root
  window). For a full-screen recording the tray loop never opens the panel
  there (see `start_recording` in tray_loop.rs). For a region recording the
  panel only matters if it overlaps the region, so the tray loop asks
  `framefang_core::region::indicator_placement` for a spot outside it, and
  that placement is sized from [`SIZE`].

If hiding fails (Windows 10 before version 2004, for one), `new_at` returns an
error and the recording goes ahead without a panel: better no STOP button than
one recorded into the user's video.

This module also holds the few window helpers the small floating windows
(this panel and pinned images) share: their window attributes, the drawing
surface, and the resize, draw and present cycle.
*/

use {
    crate::{
        bitmap_font::{measure, GLYPH_H},
        painter::{self, Surface as _},
    },
    anyhow::{anyhow, bail, Result},
    framefang_core::color::Rgba,
    std::{
        num::NonZeroU32,
        rc::Rc,
        time::{Duration, Instant},
    },
    winit::{
        dpi::{PhysicalPosition, PhysicalSize},
        event::{ElementState, MouseButton, WindowEvent},
        event_loop::ActiveEventLoop,
        window::{CursorIcon, Window, WindowAttributes, WindowId, WindowLevel},
    },
};

/// The drawing surface a floating window paints into.
pub(crate) type SoftSurface = softbuffer::Surface<Rc<Window>, Rc<Window>>;
/// The softbuffer context a [`SoftSurface`] needs kept alive.
pub(crate) type SoftContext = softbuffer::Context<Rc<Window>>;

const PANEL_W: i32 = 220;
const PANEL_H: i32 = 56;

/// The panel's outer size in pixels, for placing it around a recorded
/// region. `new_at` opens the window at exactly this size.
pub const SIZE: (u32, u32) = (PANEL_W as u32, PANEL_H as u32);

// Panel colours as 0x00RRGGBB, from the app's black, green and red palette.
const PANEL_FILL:   u32 = 0x0010_1614;
const PANEL_EDGE:   u32 = 0x0028_3a30;
const LABEL_INK:    u32 = 0x00e8_ffe8;
const CLOCK_INK:    u32 = 0x008a_9c92;
const DOT_LIT:      u32 = 0x00ff_3a3a;
const DOT_DARK:     u32 = 0x0044_1414;
const BUTTON_FILL:  u32 = 0x00ff_3a3a;
const BUTTON_HOVER: u32 = 0x00ff_6a6a;
const BUTTON_EDGE:  u32 = 0x00ff_6a6a;
const BUTTON_INK:   u32 = 0x000a_0606;

/// Centre of the record dot.
const DOT_CENTRE: (i32, i32) = (14, PANEL_H / 2);
/// Radius of the dot itself; while lit, two fainter rings of glow are drawn
/// over it, given here as `(radius, opacity)`.
const DOT_RADIUS: i32 = 6;
const GLOW: [(i32, u8); 2] = [(10, 70), (14, 30)];

/// The STOP button: 60 by 24, 14 pixels in from the right edge, centred
/// vertically.
const BUTTON: Rect = {
    let (w, h) = (60, 24);
    Rect { x: PANEL_W - w - 14, y: (PANEL_H - h) / 2, w, h }
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Rect {
    x: i32,
    y: i32,
    w: i32,
    h: i32,
}

impl Rect {
    fn contains(self, (px, py): (i32, i32)) -> bool {
        (self.x..self.x + self.w).contains(&px) && (self.y..self.y + self.h).contains(&py)
    }
}

pub struct RecordingIndicator {
    window: Rc<Window>,
    // Never read, but the surface is only valid while its context lives.
    _context: SoftContext,
    surface: SoftSurface,
    started: Instant,
    pointer: (i32, i32),
    over_button: bool,
    /// Becomes `true` when the user presses STOP (or closes the window).
    /// The tray loop checks it after every event, and when it flips drops the
    /// panel and stops the recording.
    pub stop_requested: bool,
}

impl RecordingIndicator {
    /// Open the panel with its top-left corner at `origin`. Where it goes
    /// matters: on X11 a panel inside the recorded rectangle ends up in the
    /// video, so `tray_loop` chooses `origin` with
    /// `framefang_core::region::indicator_placement`.
    pub fn new_at(
        loop_target: &ActiveEventLoop,
        origin: (i32, i32),
    ) -> Result<Self> {
        let attrs = floating_attrs("Frame Fang: recording", SIZE, origin);
        // Refuse to carry on if the panel would be recorded.
        let panel = open_window(loop_target, attrs, "recording indicator")
            .and_then(|panel| exclude_from_capture(&panel).map(|()| panel))?;
        panel.set_cursor(CursorIcon::Default);
        let (context, surface) = drawing_surface(&panel, "recording indicator")?;

        let mut indicator = Self {
            window: panel, _context: context, surface, started: Instant::now(),
            pointer: (0, 0), over_button: false, stop_requested: false,
        };
        indicator.repaint_now();
        Ok(indicator)
    }

    pub fn window_id(&self) -> WindowId {
        self.window.id()
    }

    /// Called on every tray-loop poll so the dot keeps blinking and the clock
    /// keeps counting even when nothing else happens to the window.
    pub fn tick(&self) {
        self.window.request_redraw()
    }

    pub fn handle_event(&mut self, event: WindowEvent) {
        use WindowEvent as Ev;
        match &event {
            Ev::CloseRequested => self.stop_requested = true,
            Ev::CursorMoved { position: at, .. } => self.pointer_moved((at.x as i32, at.y as i32)),
            Ev::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. } => self.left_pressed(),
            Ev::Resized(_) | Ev::RedrawRequested => self.repaint_now(),
            _ => (),
        }
    }

    fn pointer_moved(&mut self, at: (i32, i32)) {
        self.pointer = at;
        let over = BUTTON.contains(at);
        self.window.set_cursor(if over { CursorIcon::Pointer } else { CursorIcon::Move });
        if over != self.over_button {
            self.over_button = over;
            self.window.request_redraw();
        }
    }

    fn left_pressed(&mut self) {
        if BUTTON.contains(self.pointer) {
            self.stop_requested = true;
            return;
        }
        // Anywhere else is a handle for moving the panel. The window manager
        // runs the drag, as it does for pins; if it declines, nothing moves.
        self.window.drag_window().ok();
    }

    fn repaint_now(&mut self) {
        let (elapsed, over_button) = (self.started.elapsed(), self.over_button);
        repaint(&self.window, &mut self.surface, "recording indicator", |px, w, h| {
            let mut canvas = Canvas { px, w: w as i32, h: h as i32 };
            paint(&mut canvas, elapsed, over_button);
        });
    }
}

// Shared by the small floating windows (this panel and pinned images).

/// Window attributes for a small frameless window that floats above
/// everything: `size` and `at` are in physical pixels.
pub(crate) fn floating_attrs(title: &str, size: (u32, u32), at: (i32, i32)) -> WindowAttributes {
    let icon = crate::brand_icon::shared();
    WindowAttributes::default()
        .with_title(title)
        .with_decorations(false).with_resizable(false)
        .with_inner_size(PhysicalSize::new(size.0, size.1))
        .with_position(PhysicalPosition::new(at.0, at.1))
        .with_window_icon(icon).with_window_level(WindowLevel::AlwaysOnTop)
}

/// Create the window, naming `what` it was for if the OS refuses.
pub(crate) fn open_window(loop_target: &ActiveEventLoop, attrs: WindowAttributes, what: &str) -> Result<Rc<Window>> {
    match loop_target.create_window(attrs) {
        Ok(window) => Ok(Rc::new(window)),
        Err(e) => bail!("couldn't open the {what} window: {e}"),
    }
}

/// The softbuffer context and surface for drawing into `window`.
pub(crate) fn drawing_surface(window: &Rc<Window>, what: &str) -> Result<(SoftContext, SoftSurface)> {
    let context = SoftContext::new(Rc::clone(window))
        .map_err(|e| anyhow!("couldn't set up drawing for the {what} window: {e}"))?;
    let surface = SoftSurface::new(&context, Rc::clone(window))
        .map_err(|e| anyhow!("couldn't create the {what} window's drawing surface: {e}"))?;
    Ok((context, surface))
}

/// Size `surface` to `window`, let `draw` fill the 0RGB pixels (given with
/// the width and height), and show the result. A zero-sized window (such as
/// a minimised one) is skipped. Failures are logged under `what`, since a
/// missed frame is not worth more than that.
pub(crate) fn repaint(
    window: &Window,
    surface: &mut SoftSurface,
    what: &str,
    draw: impl FnOnce(&mut [u32], usize, usize),
) {
    let inner = window.inner_size();
    let (Some(width), Some(height)) = (NonZeroU32::new(inner.width), NonZeroU32::new(inner.height)) else {
        return;
    };
    let cycle = || -> Result<(), String> {
        surface.resize(width, height).map_err(|e| format!("couldn't resize the surface: {e}"))?;
        let mut frame = surface.buffer_mut().map_err(|e| format!("couldn't get a frame to draw in: {e}"))?;
        draw(&mut *frame, width.get() as usize, height.get() as usize);
        frame.present().map_err(|e| format!("couldn't show the frame: {e}"))
    };
    if let Err(why) = cycle() {
        eprintln!("{what}: {why}");
    }
}

// Drawing the panel

/// Whether the dot is lit `elapsed` into the recording: on for the first
/// 600 ms of every second.
fn dot_lit(elapsed: Duration) -> bool {
    elapsed.subsec_millis() < 600
}

/// The clock text, minutes and seconds with two digits each. Minutes keep
/// counting past 99 instead of wrapping.
fn clock_text(elapsed: Duration) -> String {
    let (minutes, seconds) = (elapsed.as_secs() / 60, elapsed.as_secs() % 60);
    format!("{minutes:02}:{seconds:02}")
}

/// Draw the whole panel. Positions come from the panel's design size, not
/// the window's, so a window the system resized just shows more or less of
/// the same layout.
fn paint(canvas: &mut Canvas, elapsed: Duration, over_button: bool) {
    let whole = Rect { x: 0, y: 0, w: canvas.w, h: canvas.h };
    fill(canvas, whole, PANEL_FILL);
    outline(canvas, whole, PANEL_EDGE);

    let (cx, cy) = DOT_CENTRE;
    let lit = dot_lit(elapsed);
    disc(canvas, DOT_CENTRE, DOT_RADIUS, if lit { DOT_LIT } else { DOT_DARK }, 255);
    if lit {
        for (radius, opacity) in GLOW {
            disc(canvas, DOT_CENTRE, radius, DOT_LIT, opacity);
        }
    }

    let label = "REC";
    let label_x = cx + 12;
    let text_y = cy - GLYPH_H / 2;
    painter::draw_text(canvas, label_x, text_y, 1, label, ink(LABEL_INK));
    let clock_x = label_x + measure(label, 1) + 12;
    painter::draw_text(canvas, clock_x, text_y, 1, &clock_text(elapsed), ink(CLOCK_INK));

    fill(canvas, BUTTON, if over_button { BUTTON_HOVER } else { BUTTON_FILL });
    outline(canvas, BUTTON, BUTTON_EDGE);
    let caption = "STOP";
    painter::draw_text(
        canvas,
        BUTTON.x + (BUTTON.w - measure(caption, 1)) / 2,
        BUTTON.y + (BUTTON.h - GLYPH_H) / 2,
        1,
        caption,
        ink(BUTTON_INK),
    );
}

fn ink(rgb: u32) -> Rgba {
    let [_, r, g, b] = rgb.to_be_bytes();
    Rgba { r, g, b, a: 255 }
}

/// A view of a 0RGB pixel buffer as a `painter::Surface`, dropping writes
/// that fall outside it (a resized window can be smaller than the layout).
struct Canvas<'a> {
    px: &'a mut [u32],
    w: i32,
    h: i32,
}

impl Canvas<'_> {
    fn slot(&self, x: i32, y: i32) -> Option<usize> {
        let inside = (0..self.w).contains(&x) && (0..self.h).contains(&y);
        inside.then(|| y as usize * self.w as usize + x as usize)
    }
}

impl painter::Surface for Canvas<'_> {
    fn width(&self) -> i32 { self.w }
    fn height(&self) -> i32 { self.h }

    fn read(&self, x: i32, y: i32) -> [u8; 4] {
        let [_, r, g, b] = self.slot(x, y).map_or(0, |i| self.px[i]).to_be_bytes();
        [r, g, b, 0xFF]
    }

    fn write(&mut self, x: i32, y: i32, rgba: [u8; 4]) {
        if let Some(i) = self.slot(x, y) {
            let [r, g, b, _] = rgba;
            self.px[i] = u32::from_be_bytes([0, r, g, b]);
        }
    }
}

fn fill(canvas: &mut Canvas, r: Rect, rgb: u32) {
    let colour = rgb_bytes(rgb);
    for y in r.y..r.y + r.h {
        for x in r.x..r.x + r.w {
            canvas.write(x, y, colour);
        }
    }
}

/// A one pixel border just inside `r`.
fn outline(canvas: &mut Canvas, r: Rect, rgb: u32) {
    let colour = rgb_bytes(rgb);
    let (right, bottom) = (r.x + r.w - 1, r.y + r.h - 1);
    for x in r.x..r.x + r.w {
        canvas.write(x, r.y, colour);
        canvas.write(x, bottom, colour);
    }
    for y in r.y..r.y + r.h {
        canvas.write(r.x, y, colour);
        canvas.write(right, y, colour);
    }
}

/// A filled circle mixed over what is there at `opacity` out of 255 (255
/// paints it solid), rounding to the nearest value.
fn disc(canvas: &mut Canvas, (cx, cy): (i32, i32), radius: i32, rgb: u32, opacity: u8) {
    let colour = rgb_bytes(rgb);
    let keep = 255 - u32::from(opacity);
    let mix = |over: u8, under: u8| {
        ((u32::from(over) * u32::from(opacity) + u32::from(under) * keep + 127) / 255) as u8
    };
    for dy in -radius..=radius {
        for dx in -radius..=radius {
            if dx * dx + dy * dy > radius * radius {
                continue;
            }
            let (x, y) = (cx + dx, cy + dy);
            let under = canvas.read(x, y);
            let mixed = [0, 1, 2].map(|c| mix(colour[c], under[c]));
            canvas.write(x, y, [mixed[0], mixed[1], mixed[2], 255]);
        }
    }
}

fn rgb_bytes(rgb: u32) -> [u8; 4] {
    let [_, r, g, b] = rgb.to_be_bytes();
    [r, g, b, 255]
}

// Keeping the panel out of recordings

/// The window's native handle, for the platform calls below.
#[cfg(any(windows, target_os = "macos"))]
fn native_handle(window: &Window) -> Result<winit::raw_window_handle::RawWindowHandle> {
    use winit::raw_window_handle::HasWindowHandle as _;
    match window.window_handle() {
        Ok(handle) => Ok(handle.as_raw()),
        Err(e) => bail!("couldn't get the native handle of a floating window: {e}"),
    }
}

/// Hide `window` from screen capture so it never appears in a recording.
/// From Windows 10 version 2004 (build 19041) `WDA_EXCLUDEFROMCAPTURE` makes
/// DWM drop the window from BitBlt and DXGI screen reads, which is what
/// ffmpeg's gdigrab does (OBS and KeePassXC rely on the same flag). The build
/// has to be checked first, because on older builds the call still succeeds
/// but behaves like `WDA_MONITOR`: the panel would record as a solid black
/// box, which is worse than recording it. Any failure comes back as an error,
/// and the tray loop records without the panel (the tray menu can still stop
/// it).
#[cfg(windows)]
pub(crate) fn exclude_from_capture(window: &Rc<Window>) -> Result<()> {
    use {
        windows::Win32::{Foundation::HWND, UI::WindowsAndMessaging::{SetWindowDisplayAffinity, WDA_EXCLUDEFROMCAPTURE}},
        winit::raw_window_handle::RawWindowHandle,
    };

    /// Windows 10 version 2004, where the flag starts meaning "leave out".
    const FIRST_EXCLUDING_BUILD: u32 = 19041;

    let found = windows_build_number();
    if found < FIRST_EXCLUDING_BUILD {
        bail!(
            "this is Windows build {found}, and hiding windows from capture needs build \
             {FIRST_EXCLUDING_BUILD} or later (older ones record a black box instead), so the \
             recording runs without the floating panel"
        );
    }
    let RawWindowHandle::Win32(win32) = native_handle(window)? else {
        bail!("the recording panel's handle is not a Win32 window");
    };
    let hwnd = HWND(win32.hwnd.get() as *mut core::ffi::c_void);
    // SAFETY: `hwnd` is the live window winit just created on this thread.
    let excluded = unsafe { SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE) };
    excluded.map_err(|e| anyhow!("Windows refused to hide the recording panel from capture: {e}"))
}

/// The Windows build number, read from `CurrentBuildNumber` in the registry
/// because `GetVersionEx` reports an old version to apps without a
/// compatibility manifest. 0 when it cannot be read, which callers treat as
/// too old for anything.
#[cfg(windows)]
pub(crate) fn windows_build_number() -> u32 {
    use windows::{
        core::{w, PCWSTR},
        Win32::System::Registry::{RegGetValueW, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ},
    };

    const VERSION_KEY: PCWSTR = w!(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion");
    const BUILD_VALUE: PCWSTR = w!("CurrentBuildNumber");

    let mut text = [0u16; 32];
    let mut bytes = std::mem::size_of_val(&text) as u32;
    // SAFETY: `text` and `bytes` outlive the call, and `bytes` tells the API
    // how much room `text` has.
    let read = unsafe {
        RegGetValueW(HKEY_LOCAL_MACHINE, VERSION_KEY, BUILD_VALUE, RRF_RT_REG_SZ, None,
                     Some(text.as_mut_ptr().cast()), Some(&mut bytes))
    };
    read.ok().map_or(0, |()| build_from_reg_sz(&text, bytes))
}

/// Parse a `REG_SZ` the registry wrote into `text`. `bytes` is the size it
/// reported, which counts the terminating NUL.
#[cfg_attr(not(windows), allow(dead_code))]
fn build_from_reg_sz(text: &[u16], bytes: u32) -> u32 {
    let units = (bytes as usize / 2).saturating_sub(1).min(text.len());
    String::from_utf16_lossy(&text[..units]).trim().parse().unwrap_or(0)
}

/// macOS: setting `NSWindow.sharingType` to `NSWindowSharingNone` keeps the
/// window on the display but hidden from capture APIs, including the
/// `screencapture -v` process the recorder runs.
#[cfg(target_os = "macos")]
fn exclude_from_capture(panel: &Rc<Window>) -> Result<()> {
    use {
        objc2::{msg_send, runtime::AnyObject},
        winit::raw_window_handle::RawWindowHandle,
    };

    /// `NSWindowSharingNone`, an NSUInteger.
    const SHARING_NONE: usize = 0;

    let RawWindowHandle::AppKit(appkit) = native_handle(panel)? else {
        bail!("the recording panel's handle is not an AppKit view");
    };
    // SAFETY: `ns_view` is the live NSView winit created for this window,
    // and `window` is a plain NSView getter.
    let owner: Option<&AnyObject> = unsafe {
        let view: &AnyObject = &*appkit.ns_view.as_ptr().cast::<AnyObject>();
        let ns_window: *mut AnyObject = msg_send![view, window];
        ns_window.as_ref()
    };
    let Some(ns_window) = owner else {
        bail!("the recording panel's view has no NSWindow");
    };
    // SAFETY: `ns_window` is that view's live NSWindow, and
    // `setSharingType:` exists on every macOS release Frame Fang runs on.
    let _: () = unsafe { msg_send![ns_window, setSharingType: SHARING_NONE] };
    Ok(())
}

/// Linux and the BSDs: X11 has no way to hide one window from x11grab, which
/// is why the tray loop decides where (and whether) to show the panel. This
/// keeps `new_at` the same on every platform.
#[cfg(all(not(windows), not(target_os = "macos")))]
fn exclude_from_capture(_: &Rc<Window>) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod checks {
    use super::{
        build_from_reg_sz, clock_text, dot_lit, paint, Canvas, Duration, Rect, BUTTON, BUTTON_EDGE,
        BUTTON_FILL, BUTTON_HOVER, DOT_CENTRE, DOT_DARK, DOT_LIT, PANEL_EDGE, PANEL_FILL, PANEL_H,
        PANEL_W, SIZE,
    };

    fn painted(elapsed: Duration, over_button: bool) -> Vec<u32> {
        let mut px = vec![0u32; (PANEL_W * PANEL_H) as usize];
        let mut canvas = Canvas { px: &mut px, w: PANEL_W, h: PANEL_H };
        paint(&mut canvas, elapsed, over_button);
        px
    }

    fn at(px: &[u32], x: i32, y: i32) -> u32 {
        px[(y * PANEL_W + x) as usize]
    }

    #[test]
    fn button_sits_where_the_layout_puts_it() {
        assert_eq!(BUTTON, Rect { x: 146, y: 16, w: 60, h: 24 });
        assert_eq!(SIZE, (220, 56));
    }

    #[test]
    fn button_hit_test_is_inclusive_left_and_top_only() {
        assert!(BUTTON.contains((146, 16)));
        assert!(BUTTON.contains((205, 39)));
        assert!(!BUTTON.contains((206, 20)));
        assert!(!BUTTON.contains((150, 40)));
        assert!(!BUTTON.contains((145, 20)));
        assert!(!BUTTON.contains((150, 15)));
    }

    #[test]
    fn dot_is_lit_for_the_first_six_tenths_of_each_second() {
        assert!(dot_lit(Duration::ZERO));
        assert!(dot_lit(Duration::from_millis(599)));
        assert!(!dot_lit(Duration::from_millis(600)));
        assert!(!dot_lit(Duration::from_millis(999)));
        assert!(dot_lit(Duration::from_millis(61_250)));
    }

    #[test]
    fn clock_shows_minutes_and_seconds() {
        assert_eq!(clock_text(Duration::ZERO), "00:00");
        assert_eq!(clock_text(Duration::from_millis(12_900)), "00:12");
        assert_eq!(clock_text(Duration::from_secs(61)), "01:01");
        assert_eq!(clock_text(Duration::from_secs(100 * 60 + 5)), "100:05");
    }

    #[test]
    fn panel_has_a_one_pixel_edge_around_its_fill() {
        let px = painted(Duration::from_millis(700), false);
        for (x, y) in [(0, 0), (219, 0), (0, 55), (219, 55), (100, 0), (0, 30)] {
            assert_eq!(at(&px, x, y), PANEL_EDGE, "({x}, {y})");
        }
        assert_eq!(at(&px, 1, 1), PANEL_FILL);
        assert_eq!(at(&px, 120, 50), PANEL_FILL);
    }

    #[test]
    fn button_changes_colour_under_the_pointer() {
        let corner_inside = (BUTTON.x + 2, BUTTON.y + 2);
        let idle = painted(Duration::ZERO, false);
        let hovered = painted(Duration::ZERO, true);
        assert_eq!(at(&idle, corner_inside.0, corner_inside.1), BUTTON_FILL);
        assert_eq!(at(&hovered, corner_inside.0, corner_inside.1), BUTTON_HOVER);
        assert_eq!(at(&idle, BUTTON.x, BUTTON.y), BUTTON_EDGE);
    }

    #[test]
    fn dot_blinks_and_glows_only_while_lit() {
        let (cx, cy) = DOT_CENTRE;
        let lit = painted(Duration::from_millis(100), false);
        let dark = painted(Duration::from_millis(700), false);
        assert_eq!(at(&lit, cx, cy), DOT_LIT);
        assert_eq!(at(&dark, cx, cy), DOT_DARK);
        // Just outside the dot, inside the first glow ring.
        assert_ne!(at(&lit, cx + 8, cy), PANEL_FILL);
        assert_eq!(at(&dark, cx + 8, cy), PANEL_FILL);
    }

    #[test]
    fn glow_mixes_with_rounding() {
        // 70/255 of 0xff3a3a over the panel fill, rounded to nearest, then
        // 30/255 more of the same red on top.
        let (cx, cy) = DOT_CENTRE;
        let px = painted(Duration::ZERO, false);
        let mix = |over: u32, under: u32, a: u32| (over * a + under * (255 - a) + 127) / 255;
        let channel = |shift: u32| {
            let red = (DOT_LIT >> shift) & 0xFF;
            let base = (PANEL_FILL >> shift) & 0xFF;
            mix(red, mix(red, base, 70), 30)
        };
        let want = channel(16) << 16 | channel(8) << 8 | channel(0);
        assert_eq!(at(&px, cx + 9, cy), want);
    }

    #[test]
    fn painting_a_smaller_window_clips_instead_of_panicking() {
        let mut px = vec![0u32; 30 * 10];
        let mut canvas = Canvas { px: &mut px, w: 30, h: 10 };
        paint(&mut canvas, Duration::ZERO, true);
        assert_eq!(px[0], PANEL_EDGE);
    }

    #[test]
    fn registry_build_text_parses_without_its_nul() {
        let text: Vec<u16> = "19045\0".encode_utf16().collect();
        assert_eq!(build_from_reg_sz(&text, 12), 19045);
        let padded: Vec<u16> = " 22631 \0".encode_utf16().collect();
        assert_eq!(build_from_reg_sz(&padded, 16), 22631);
        assert_eq!(build_from_reg_sz(&text, 0), 0);
        let junk: Vec<u16> = "abc\0".encode_utf16().collect();
        assert_eq!(build_from_reg_sz(&junk, 8), 0);
    }
}
