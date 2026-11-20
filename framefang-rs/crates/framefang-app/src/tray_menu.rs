//! The tray icon's menu on Windows, drawn by the app in the color scheme the
//! user picked, since a native Win32 menu can't be styled. macOS and Linux
//! keep their native menus.
//!
//! A borderless, always-on-top window opened over the taskbar where the icon
//! was clicked, in the dialogs' look (pure black, glowing heading, `01 //`
//! section labels). It closes when it loses focus, on Esc, or when a row is
//! picked; the tray loop then runs the picked action through the same code as
//! the native menu's events. The Sound chips only choose and leave it open.
//!
//! It is kept out of screenshots and recordings, so a capture started from it
//! never shows it.
//!
//! With recent captures kept, a RECENT column on its right shows them as
//! thumbnails, newest first: a click copies one, Shift+click opens it in the
//! editor.

use std::num::NonZeroU32;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use softbuffer::{Context, Surface};
use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::event_loop::ActiveEventLoop;
use winit::keyboard::{Key, NamedKey};
use winit::window::{CursorIcon, Window, WindowAttributes, WindowId, WindowLevel};

use framefang_core::color::Rgba;
use framefang_platform::recorder::RecordingOptions;
use framefang_platform::tray::TrayEvent;

use crate::palette::*;

const W: i32 = 300;
/// The one left and right edge for everything with words: heading, version,
/// section labels, rows, key hints.
const PAD: i32 = TEXT_X;
const HEADER_H: i32 = 50;
const SECTION_H: i32 = 26;
const ROW_H: i32 = 30;
const CHIP_ROW_H: i32 = 30;
const CHIP_H: i32 = 22;
const FOOTER_H: i32 = 40;
/// Rows' hover wash stops this far from the window's sides.
const INSET: i32 = 6;
/// Where a row's words start: past the accent bar and some air.
const TEXT_X: i32 = INSET + 14;
/// A chip row's chips start this far past `TEXT_X`, after the caption.
const CAPTION_W: i32 = 62;
const ROW_PX: f32 = 13.5;
const SMALL_PX: f32 = 12.0;
/// Space between the menu and the taskbar or the screen's edge.
const GAP: i32 = 10;
/// The footer row that ends the app, typed like a shell command in the pixel
/// font (Brandon picked it, 2026-11-19). It names the app, so it can't be read
/// as "close this menu" the way a bare "Exit" was.
const QUIT: &str = "> KILL FRAME_FANG";
/// The block cursor after it while hovered: its width, and how long it stays
/// on and off.
const CURSOR_W: i32 = 5;
/// Space before the cursor: a letter gap and a bit, so it reads as a cursor
/// and not as part of the last letter.
const CURSOR_GAP: i32 = 3;
const BLINK: Duration = Duration::from_millis(500);

fn quit_w() -> i32 {
    crate::bitmap_font::measure(QUIT, 1) + CURSOR_GAP + CURSOR_W
}

/// What the menu shows: the keys as set in Settings and what is going on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuState {
    pub capture_key: Option<String>,
    pub full_key: Option<String>,
    pub record_key: Option<String>,
    pub recording: bool,
    /// Which sound a recording started here takes.
    pub sound: RecordingOptions,
    /// A region has been captured, so "Last region" has one to take.
    pub last_region: bool,
    /// Recordings end as GIF (no sound) instead of MP4.
    pub gif: bool,
    /// The last recording is still becoming a GIF.
    pub converting: bool,
    /// FFmpeg is there (a GIF needs it).
    pub ffmpeg: bool,
}

pub enum MenuOutcome {
    Closed,
    Run(TrayEvent),
    /// Recent capture `index` (newest first): copy it, or edit it.
    Recent { index: usize, edit: bool },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Act {
    Run(TrayEvent),
    Sound(RecordingOptions),
    /// Record as GIF (`true`) or MP4.
    Format(bool),
    /// Recent capture `n`.
    Recent(usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Row,
    Chip,
    Footer,
    /// A recent capture's thumbnail in the RECENT column.
    Recent,
}

/// The RECENT column: its width, its thumbnails' cells (2 across, 5 down),
/// the room between them, and the line under them.
const COL_W: i32 = PAD * 2 + 2 * CELL.0 + CELL_GAP;
const CELL: (i32, i32) = (crate::recent::THUMB.0 as i32 + 4, crate::recent::THUMB.1 as i32 + 4);
const CELL_GAP: i32 = 8;
const RECENT_COLS: usize = 2;
const RECENT_HINT: &str = "Click copies  [Shift]+click edits";

/// The menu's layout with the RECENT column's cells, one per kept capture
/// (`recent`: how many, or `None` without the column).
fn full_layout(s: &MenuState, recent: Option<usize>) -> Layout {
    let mut l = layout(s);
    if let Some(n) = recent {
        let (x0, y0) = (W + PAD, HEADER_H + 4 + SECTION_H);
        for i in 0..n.min(crate::recent::KEEP) {
            let (c, r) = ((i % RECENT_COLS) as i32, (i / RECENT_COLS) as i32);
            l.items.push(Item { kind: Kind::Recent,
                                rect: (x0 + c * (CELL.0 + CELL_GAP), y0 + r * (CELL.1 + CELL_GAP), CELL.0, CELL.1),
                                label: "", hint: None, act: Act::Recent(i), enabled: true });
        }
    }
    l
}

/// Where the line under the RECENT grid goes: its top.
fn recent_caption_y() -> i32 {
    let rows = crate::recent::KEEP.div_ceil(RECENT_COLS) as i32;
    HEADER_H + 4 + SECTION_H + rows * (CELL.1 + CELL_GAP) + 2
}

#[derive(Debug, Clone)]
struct Item {
    kind: Kind,
    /// x, y, width, height in the window.
    rect: (i32, i32, i32, i32),
    label: &'static str,
    /// The key that does the same, quoted at the row's right.
    hint: Option<String>,
    act: Act,
    enabled: bool,
}

#[derive(Debug, Clone, Default)]
struct Layout {
    items: Vec<Item>,
    /// Section labels: top of the text, number, name.
    sections: Vec<(i32, usize, &'static str)>,
    /// Chip rows' captions: row top, text.
    captions: Vec<(i32, &'static str)>,
    /// A line of quiet text in a chip row's place: row top, text.
    notes: Vec<(i32, &'static str)>,
    /// Hairlines between groups.
    rules: Vec<i32>,
    footer_y: i32,
    h: i32,
}

fn chip_w(label: &str) -> i32 {
    (framefang_core::text::line_width(label, SMALL_PX).ceil() as i32 + 20).max(36)
}

fn layout(s: &MenuState) -> Layout {
    fn section(l: &mut Layout, y: &mut i32, name: &'static str) {
        let n = l.sections.len() + 1;
        // The label sits low in its band, nearer the rows it names.
        l.sections.push((*y + 12, n, name));
        *y += SECTION_H;
    }
    fn row(l: &mut Layout, y: &mut i32, label: &'static str, hint: Option<String>, act: Act, enabled: bool) {
        l.items.push(Item { kind: Kind::Row, rect: (INSET, *y, W - 2 * INSET, ROW_H), label, hint, act, enabled });
        *y += ROW_H;
    }
    fn chips(l: &mut Layout, y: &mut i32, caption: &'static str, list: &[(&'static str, Act, bool)]) {
        l.captions.push((*y, caption));
        let mut x = TEXT_X + CAPTION_W;
        let cy = *y + (CHIP_ROW_H - CHIP_H) / 2;
        for &(label, act, enabled) in list {
            let cw = chip_w(label);
            l.items.push(Item { kind: Kind::Chip, rect: (x, cy, cw, CHIP_H), label, hint: None, act, enabled });
            x += cw + 6;
        }
        *y += CHIP_ROW_H;
    }
    fn note(l: &mut Layout, y: &mut i32, caption: &'static str, text: &'static str) {
        l.captions.push((*y, caption));
        l.notes.push((*y, text));
        *y += CHIP_ROW_H;
    }
    // An unbound key says so, so the row still shows where keys go.
    let key = |k: &Option<String>| Some(k.clone().unwrap_or_else(|| "not set".to_owned()));

    let mut l = Layout::default();
    let mut y = HEADER_H + 4;

    section(&mut l, &mut y, "CAPTURE");
    row(&mut l, &mut y, "Capture", key(&s.capture_key), Act::Run(TrayEvent::Capture), true);
    row(&mut l, &mut y, "Capture full screen", key(&s.full_key), Act::Run(TrayEvent::CaptureFullScreen), true);
    chips(&mut l, &mut y, "Delay", &[
        ("3s", Act::Run(TrayEvent::CaptureDelayed(3)), true),
        ("5s", Act::Run(TrayEvent::CaptureDelayed(5)), true),
        ("10s", Act::Run(TrayEvent::CaptureDelayed(10)), true),
    ]);
    // The other ways to capture, one chip each: keys for them are set in
    // Settings.
    chips(&mut l, &mut y, "Also", &[
        ("Last region", Act::Run(TrayEvent::CaptureLastRegion), s.last_region),
        ("Scroll", Act::Run(TrayEvent::CaptureScroll), crate::scroll::supported()),
    ]);
    // Captures that read what is on screen instead of keeping a picture.
    chips(&mut l, &mut y, "Read", &[
        ("Text", Act::Run(TrayEvent::CaptureText), true),
        ("QR", Act::Run(TrayEvent::CaptureQr), true),
    ]);

    section(&mut l, &mut y, "RECORD");
    let idle = !s.recording && !s.converting;
    if s.recording {
        row(&mut l, &mut y, "Stop recording", key(&s.record_key), Act::Run(TrayEvent::StopRecording), true);
    } else if s.converting {
        row(&mut l, &mut y, "Cancel GIF", None, Act::Run(TrayEvent::CancelGif), true);
    } else {
        row(&mut l, &mut y, "Record screen", key(&s.record_key), Act::Run(TrayEvent::StartRecording(s.sound)), true);
    }
    row(&mut l, &mut y, "Record region", None, Act::Run(TrayEvent::StartRegionRecording(s.sound)), idle);
    chips(&mut l, &mut y, "Format", &[
        ("MP4", Act::Format(false), idle),
        // Without FFmpeg it stays clickable: it says how to get FFmpeg.
        ("GIF", if s.ffmpeg { Act::Format(true) } else { Act::Run(TrayEvent::SetGif(true)) }, idle),
    ]);
    if s.gif {
        note(&mut l, &mut y, "Sound", "GIF has no sound");
    } else {
        chips(&mut l, &mut y, "Sound", &[
            ("None", Act::Sound(RecordingOptions::NONE), idle),
            ("Mic", Act::Sound(RecordingOptions::MIC_ONLY), idle),
            ("PC", Act::Sound(RecordingOptions::SYSTEM_ONLY), idle),
            ("Both", Act::Sound(RecordingOptions::MIC_AND_SYS), idle),
        ]);
    }
    row(&mut l, &mut y, "Annotate last recording", None, Act::Run(TrayEvent::AnnotateLastRecording), true);

    section(&mut l, &mut y, "FILES");
    row(&mut l, &mut y, "Open screenshots", None, Act::Run(TrayEvent::OpenSaveFolder), true);
    row(&mut l, &mut y, "Open recordings", None, Act::Run(TrayEvent::OpenRecordingsFolder), true);
    chips(&mut l, &mut y, "Convert", &[
        ("Image", Act::Run(TrayEvent::ConvertImage), true),
        ("Video", Act::Run(TrayEvent::ConvertVideo), true),
    ]);

    y += 7;
    l.rules.push(y);
    y += 6;
    row(&mut l, &mut y, "Settings", None, Act::Run(TrayEvent::Settings), true);
    row(&mut l, &mut y, "Check for updates", None, Act::Run(TrayEvent::CheckForUpdates), true);
    y += 8;

    l.footer_y = y;
    let fh = 26;
    let fy = y + (FOOTER_H - fh) / 2;
    let text_w = |t: &str| framefang_core::text::line_width(t, SMALL_PX).ceil() as i32;
    let about_w = text_w("About") + 2 * (TEXT_X - INSET);
    l.items.push(Item { kind: Kind::Footer, rect: (INSET, fy, about_w, fh), label: "About", hint: None,
                        act: Act::Run(TrayEvent::About), enabled: true });
    // Named in full: a bare "Exit" at the foot of a popup reads as "close
    // this menu", and Brandon quit the app that way (2026-11-19).
    let exit_w = quit_w() + 2 * (TEXT_X - INSET);
    l.items.push(Item { kind: Kind::Footer, rect: (W - INSET - exit_w, fy, exit_w, fh), label: QUIT, hint: None,
                        act: Act::Run(TrayEvent::Exit), enabled: true });
    l.h = y + FOOTER_H;
    l
}

/// Where the menu's top left goes for a click at `click`, given its `size`
/// and the work area (`left, top, right, bottom`) of that screen: above the
/// pointer when there is room (a taskbar at the bottom), otherwise below it,
/// starting at the pointer and flipped to end there near the right edge, and
/// always on screen.
fn place(click: (i32, i32), size: (i32, i32), work: (i32, i32, i32, i32)) -> (i32, i32) {
    let ((cx, cy), (w, h), (l, t, r, b)) = (click, size, work);
    let above = cy.min(b) - GAP - h;
    let y = if above >= t + GAP { above } else { cy.max(t) + GAP };
    let x = if cx + w <= r - GAP { cx } else { cx - w };
    (x.clamp(l + GAP, (r - GAP - w).max(l + GAP)), y.clamp(t + GAP, (b - GAP - h).max(t + GAP)))
}

pub struct TrayMenu {
    window: Rc<Window>,
    _ctx: Context<Rc<Window>>,
    surface: Surface<Rc<Window>, Rc<Window>>,
    state: MenuState,
    layout: Layout,
    hover: Option<usize>,
    /// When `hover` last changed: the quit row's cursor blinks from then.
    hover_since: Instant,
    /// Whether the last frame drew that cursor, so `tick` redraws only
    /// when it flips.
    drawn_cursor: bool,
    /// Focus came at least once, so losing it means the user went elsewhere.
    had_focus: bool,
    /// The foreground window when the menu opened (the taskbar), for the
    /// case where Windows never hands the menu focus: a change means the user
    /// went elsewhere.
    #[cfg(windows)]
    fg_at_open: isize,
    opened: Instant,
    /// The recent captures, newest first; `None` while keeping them is off.
    recent: Option<Arc<Vec<crate::recent::Entry>>>,
    /// Shift is down: a recent capture opens in the editor.
    shift: bool,
    pub outcome: Option<MenuOutcome>,
}

impl TrayMenu {
    /// Open the menu for a tray click at `click` (physical screen pixels),
    /// with the RECENT column when `recent` is there.
    pub fn new(loop_target: &ActiveEventLoop, click: (i32, i32), state: MenuState,
               recent: Option<Arc<Vec<crate::recent::Entry>>>) -> Result<Self> {
        let lay = full_layout(&state, recent.as_ref().map(|r| r.len()));
        let width = W + if recent.is_some() { COL_W } else { 0 };
        let work = work_area(loop_target, click);
        let (x, y) = place(click, (width, lay.h), work);
        let attrs = crate::chrome::frameless(WindowAttributes::default())
            .with_title("Frame Fang")
            .with_resizable(false)
            .with_visible(false)
            .with_inner_size(PhysicalSize::new(width as u32, lay.h as u32))
            .with_position(PhysicalPosition::new(x, y))
            .with_window_level(WindowLevel::AlwaysOnTop);
        #[cfg(windows)]
        let attrs = {
            use winit::platform::windows::WindowAttributesExtWindows;
            attrs.with_skip_taskbar(true)
        };
        let window = loop_target.create_window(attrs).map(Rc::new)
            .map_err(|e| anyhow!("create_window (tray menu): {e}"))?;
        crate::chrome::finish(&window);
        #[cfg(windows)]
        let _ = crate::recording_indicator::exclude_from_capture(&window);
        window.set_cursor(CursorIcon::Default);
        crate::reveal::cloak(&window);
        window.set_visible(true);
        let ctx = Context::new(window.clone()).map_err(|e| anyhow!("softbuffer Context::new (tray menu): {e}"))?;
        let surface = Surface::new(&ctx, window.clone()).map_err(|e| anyhow!("softbuffer Surface::new (tray menu): {e}"))?;
        let mut me = TrayMenu {
            window, _ctx: ctx, surface, state, layout: lay,
            hover: None, hover_since: Instant::now(), drawn_cursor: false, had_focus: false,
            #[cfg(windows)]
            fg_at_open: foreground(),
            opened: Instant::now(),
            recent, shift: false,
            outcome: None,
        };
        me.redraw();
        crate::reveal::uncloak(&me.window);
        me.window.focus_window();
        Ok(me)
    }

    pub fn window_id(&self) -> WindowId { self.window.id() }

    /// The sound chosen here, kept by the tray for the next time it opens.
    pub fn sound(&self) -> RecordingOptions { self.state.sound }

    /// Whether GIF is chosen here.
    pub fn gif(&self) -> bool { self.state.gif }

    /// Close when the user went to another window although the menu never
    /// had focus (Windows can refuse it to a background app).
    pub fn tick(&mut self) {
        if self.cursor_on() != self.drawn_cursor { self.window.request_redraw(); }
        #[cfg(windows)]
        if !self.had_focus && self.outcome.is_none() && self.opened.elapsed() > Duration::from_millis(150) {
            let fg = foreground();
            if fg != self.fg_at_open && fg != self.hwnd() {
                self.outcome = Some(MenuOutcome::Closed);
            }
        }
        #[cfg(not(windows))]
        let _ = (self.opened, Duration::ZERO);
    }

    #[cfg(windows)]
    fn hwnd(&self) -> isize {
        use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
        match self.window.window_handle().map(|h| h.as_raw()) {
            Ok(RawWindowHandle::Win32(h)) => h.hwnd.get(),
            _ => 0,
        }
    }

    pub fn handle_event(&mut self, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => self.close(),
            WindowEvent::Focused(true) => self.had_focus = true,
            WindowEvent::Focused(false) => if self.had_focus { self.close() },
            WindowEvent::ModifiersChanged(m) => self.shift = m.state().shift_key(),
            WindowEvent::KeyboardInput { event: winit::event::KeyEvent { logical_key, state: ElementState::Pressed, .. }, .. } => {
                match logical_key {
                    Key::Named(NamedKey::Escape) => self.close(),
                    Key::Named(NamedKey::ArrowDown) | Key::Named(NamedKey::Tab) => self.step(1),
                    Key::Named(NamedKey::ArrowUp) => self.step(-1),
                    Key::Named(NamedKey::ArrowRight) => self.step(1),
                    Key::Named(NamedKey::ArrowLeft) => self.step(-1),
                    Key::Named(NamedKey::Enter) | Key::Named(NamedKey::Space) => {
                        if let Some(i) = self.hover { self.activate(i); }
                    }
                    _ => {}
                }
            }
            WindowEvent::CursorLeft { .. } => self.set_hover(None),
            WindowEvent::CursorMoved { position, .. } => {
                let hit = self.hit(position.x as i32, position.y as i32);
                self.window.set_cursor(if hit.is_some() { CursorIcon::Pointer } else { CursorIcon::Default });
                self.set_hover(hit);
            }
            // Picked as the button comes up, like a native menu.
            WindowEvent::MouseInput { state: ElementState::Released, button: MouseButton::Left, .. } => {
                if let Some(i) = self.hover { self.activate(i); }
            }
            WindowEvent::Resized(_) | WindowEvent::RedrawRequested => self.redraw(),
            _ => {}
        }
    }

    fn close(&mut self) {
        if self.outcome.is_none() { self.outcome = Some(MenuOutcome::Closed); }
    }

    fn hit(&self, x: i32, y: i32) -> Option<usize> {
        self.layout.items.iter().position(|it| {
            let (ix, iy, iw, ih) = it.rect;
            it.enabled && x >= ix && x < ix + iw && y >= iy && y < iy + ih
        })
    }

    /// The quit row is hovered and its cursor is in an "on" half-second.
    fn cursor_on(&self) -> bool {
        let quit = self.hover.and_then(|i| self.layout.items.get(i)).is_some_and(|it| it.label == QUIT);
        quit && (self.hover_since.elapsed().as_millis() / BLINK.as_millis()) % 2 == 0
    }

    fn set_hover(&mut self, h: Option<usize>) {
        if h != self.hover {
            self.hover = h;
            self.hover_since = Instant::now();
            self.window.request_redraw();
        }
    }

    /// Move the keyboard highlight `dir` places through the enabled items.
    fn step(&mut self, dir: i32) {
        let n = self.layout.items.len() as i32;
        if n == 0 { return; }
        let mut i = match self.hover { Some(i) => i as i32, None => if dir > 0 { -1 } else { n } };
        for _ in 0..n {
            i = (i + dir).rem_euclid(n);
            if self.layout.items[i as usize].enabled {
                self.set_hover(Some(i as usize));
                return;
            }
        }
    }

    fn activate(&mut self, i: usize) {
        let Some(it) = self.layout.items.get(i) else { return };
        if !it.enabled || self.outcome.is_some() { return; }
        let recent_n = self.recent.as_ref().map(|r| r.len());
        match it.act {
            Act::Run(ev) => self.outcome = Some(MenuOutcome::Run(ev)),
            Act::Recent(index) => self.outcome = Some(MenuOutcome::Recent { index, edit: self.shift }),
            Act::Sound(o) => {
                self.state.sound = o;
                // The record rows carry the sound, so lay out again.
                self.layout = full_layout(&self.state, recent_n);
                self.window.request_redraw();
            }
            Act::Format(gif) => {
                self.state.gif = gif;
                // GIF swaps the Sound chips for a note; the height is the same.
                self.layout = full_layout(&self.state, recent_n);
                self.window.request_redraw();
            }
        }
    }

    fn redraw(&mut self) {
        let phys = self.window.inner_size();
        let (Some(w), Some(h)) = (NonZeroU32::new(phys.width), NonZeroU32::new(phys.height)) else { return; };
        if self.surface.resize(w, h).is_err() { return; }
        self.drawn_cursor = self.cursor_on();
        let Ok(mut buf) = self.surface.buffer_mut() else { return; };
        draw(&mut buf, w.get() as usize, h.get() as usize, &self.layout, self.hover, &self.state,
             self.drawn_cursor);
        if let Some(recent) = &self.recent {
            draw_recent(&mut buf, w.get() as usize, h.get() as usize, &self.layout, self.hover, recent);
        }
        let _ = buf.present();
    }
}

fn rgba(c: u32) -> Rgba { Rgba::new_opaque((c >> 16) as u8, (c >> 8) as u8, c as u8) }

fn fill(buf: &mut [u32], w: usize, h: usize, (x, y, rw, rh): (i32, i32, i32, i32), c: u32) {
    for yy in y.max(0)..(y + rh).min(h as i32) {
        for xx in x.max(0)..(x + rw).min(w as i32) {
            buf[yy as usize * w + xx as usize] = c;
        }
    }
}

fn stroke(buf: &mut [u32], w: usize, h: usize, (x, y, rw, rh): (i32, i32, i32, i32), c: u32) {
    fill(buf, w, h, (x, y, rw, 1), c);
    fill(buf, w, h, (x, y + rh - 1, rw, 1), c);
    fill(buf, w, h, (x, y, 1, rh), c);
    fill(buf, w, h, (x + rw - 1, y, 1, rh), c);
}

/// Smooth text with its line box centered in a band `y..y + band_h`.
fn text(buf: &mut [u32], w: usize, h: usize, x: f32, y: i32, band_h: i32, px: f32, t: &str, c: u32) {
    let ty = y as f32 + (band_h as f32 - framefang_core::text::line_height(px)) / 2.0;
    let mut s = crate::painter::U32Surface { buf, stride: w as i32, height: h as i32 };
    crate::painter::draw_unicode_text(&mut s, x, ty, px, t, rgba(c));
}

fn draw(buf: &mut [u32], w: usize, h: usize, l: &Layout, hover: Option<usize>, s: &MenuState,
        cursor_on: bool) {
    crate::hud::backdrop(buf, w, h, HEADER_H);
    crate::hud::heading(buf, w, h, PAD, 18, "FRAME FANG");
    {
        // The version, quiet, on the heading's middle, at the window's right.
        let v = format!("v{}", env!("CARGO_PKG_VERSION"));
        let vx = w as i32 - PAD - crate::bitmap_font::measure(&v, 1);
        let vy = 18 + (crate::bitmap_font::GLYPH_H * 2 - crate::bitmap_font::GLYPH_H) / 2;
        let mut s = crate::painter::U32Surface { buf, stride: w as i32, height: h as i32 };
        crate::painter::draw_text(&mut s, vx, vy, 1, &v, rgba(TEXT_DIM()));
    }
    for &(y, n, name) in &l.sections {
        crate::hud::section(buf, w, h, PAD, W - PAD, y, n, name);
    }
    for &(y, caption) in &l.captions {
        text(buf, w, h, TEXT_X as f32, y, CHIP_ROW_H, SMALL_PX, caption, TEXT_MUTED());
    }
    for &(y, note) in &l.notes {
        text(buf, w, h, (TEXT_X + CAPTION_W) as f32, y, CHIP_ROW_H, SMALL_PX, note, TEXT_DIM());
    }
    for &y in &l.rules {
        fill(buf, w, h, (PAD, y, W - 2 * PAD, 1), HEADER_RULE());
    }
    // The footer: a band in the header's shade under a hairline, the
    // window's whole width.
    fill(buf, w, h, (0, l.footer_y, w as i32, h as i32 - l.footer_y), BG_TOP());
    fill(buf, w, h, (0, l.footer_y, w as i32, 1), HEADER_RULE());

    for (i, it) in l.items.iter().enumerate() {
        let hot = hover == Some(i) && it.enabled;
        let (x, y, iw, ih) = it.rect;
        match it.kind {
            Kind::Row => {
                if hot {
                    fill(buf, w, h, it.rect, HOVER_FILL());
                    fill(buf, w, h, (x, y + 5, 2, ih - 10), LASER());
                }
                let c = if !it.enabled { TEXT_DIM() } else { TEXT_BRIGHT };
                text(buf, w, h, TEXT_X as f32, y, ih, ROW_PX, it.label, c);
                if let Some(hint) = &it.hint {
                    if hint == "not set" {
                        let tw = framefang_core::text::line_width(hint, SMALL_PX);
                        text(buf, w, h, (W - PAD) as f32 - tw, y, ih, SMALL_PX, hint, TEXT_DIM());
                    } else {
                        // The keys themselves, drawn as keys, right-aligned.
                        let keys = crate::keycap::chord(hint);
                        let kw = crate::keycap::line_width(&keys, 1);
                        let text_y = y + (ih - crate::bitmap_font::GLYPH_H) / 2;
                        crate::keycap::draw_line(buf, w, h, W - PAD - kw, text_y, &keys, 1, TEXT_MUTED());
                    }
                }
            }
            Kind::Chip => {
                let chosen = it.act == Act::Sound(s.sound) || it.act == Act::Format(s.gif);
                // GIF without FFmpeg: dim, but it still answers (with how to
                // get FFmpeg).
                let unavailable = it.act == Act::Run(TrayEvent::SetGif(true));
                let (bg, border, fg) = if !it.enabled {
                    (0, FIELD_BORDER(), TEXT_DIM())
                } else if chosen {
                    (LASER(), LASER(), BG_BODY())
                } else if hot {
                    (HOVER_FILL(), LASER_DIM(), if unavailable { TEXT_MUTED() } else { TEXT_BRIGHT })
                } else {
                    (0, FIELD_BORDER(), if unavailable { TEXT_DIM() } else { TEXT_MUTED() })
                };
                if bg != 0 { fill(buf, w, h, it.rect, bg); }
                stroke(buf, w, h, it.rect, border);
                let tw = framefang_core::text::line_width(it.label, SMALL_PX);
                text(buf, w, h, x as f32 + (iw as f32 - tw) / 2.0, y, ih, SMALL_PX, it.label, fg);
            }
            Kind::Footer if it.label == QUIT => {
                if hot { fill(buf, w, h, it.rect, HOVER_FILL()); }
                let (tx, ty) = (x + (iw - quit_w()) / 2, y + (ih - crate::bitmap_font::GLYPH_H) / 2);
                if hot {
                    // Lit like a heading, with the cursor blinking after it.
                    crate::hud::glow_text(buf, w, h, tx, ty, 1, 3, QUIT);
                    if cursor_on {
                        let cx = tx + crate::bitmap_font::measure(QUIT, 1) + CURSOR_GAP;
                        fill(buf, w, h, (cx, ty, CURSOR_W, crate::bitmap_font::GLYPH_H), LASER());
                    }
                } else {
                    let mut s = crate::painter::U32Surface { buf: &mut *buf, stride: w as i32, height: h as i32 };
                    crate::painter::draw_text(&mut s, tx, ty, 1, QUIT, rgba(TEXT_MUTED()));
                }
            }
            Kind::Footer => {
                if hot { fill(buf, w, h, it.rect, HOVER_FILL()); }
                let c = if hot { TEXT_BRIGHT } else { TEXT_MUTED() };
                let tw = framefang_core::text::line_width(it.label, SMALL_PX);
                text(buf, w, h, x as f32 + (iw as f32 - tw) / 2.0, y, ih, SMALL_PX, it.label, c);
            }
            // Drawn by `draw_recent`.
            Kind::Recent => {}
        }
    }
}

/// The RECENT column right of the menu: a hairline apart, its `04 //`
/// label, the thumbnails (the pointed-at one ringed in the accent), and
/// under them the pointed-at capture's time and size, or what a click does.
/// With nothing kept yet, it says so.
fn draw_recent(buf: &mut [u32], w: usize, h: usize, l: &Layout, hover: Option<usize>,
               recent: &[crate::recent::Entry]) {
    let x0 = W;
    fill(buf, w, h, (x0, HEADER_H + 8, 1, l.footer_y - HEADER_H - 16), HEADER_RULE());
    crate::hud::section(buf, w, h, x0 + PAD, x0 + COL_W - PAD, HEADER_H + 4 + 12, 4, "RECENT");
    let mut hovered: Option<&crate::recent::Entry> = None;
    for (i, it) in l.items.iter().enumerate() {
        let Act::Recent(n) = it.act else { continue };
        let Some(e) = recent.get(n) else { continue };
        let hot = hover == Some(i);
        if hot { hovered = Some(e); }
        let (x, y, cw, ch) = it.rect;
        fill(buf, w, h, it.rect, FIELD_BG());
        // The thumbnail, centered in its cell.
        let (tw, th) = (e.thumb.width() as i32, e.thumb.height() as i32);
        let (tx, ty) = (x + (cw - tw) / 2, y + (ch - th) / 2);
        for (px, py, p) in e.thumb.enumerate_pixels() {
            let (bx, by) = (tx + px as i32, ty + py as i32);
            if bx >= 0 && by >= 0 && (bx as usize) < w && (by as usize) < h {
                buf[by as usize * w + bx as usize] = ((p[0] as u32) << 16) | ((p[1] as u32) << 8) | p[2] as u32;
            }
        }
        stroke(buf, w, h, it.rect, if hot { LASER() } else { FIELD_BORDER() });
        if hot { stroke(buf, w, h, (x + 1, y + 1, cw - 2, ch - 2), LASER()); }
    }
    let cy = recent_caption_y();
    let band = 22;
    match hovered {
        Some(e) => text(buf, w, h, (x0 + PAD) as f32, cy, band, SMALL_PX, &e.caption(), TEXT_BRIGHT),
        None if recent.is_empty() => {
            let gy = HEADER_H + 4 + SECTION_H;
            text(buf, w, h, (x0 + PAD) as f32, gy + 6, band, SMALL_PX, "Nothing yet", TEXT_MUTED());
            text(buf, w, h, (x0 + PAD) as f32, gy + 6 + band, band, SMALL_PX,
                 "Captures you copy or save show here.", TEXT_DIM());
        }
        None => {
            let text_y = cy + (band - crate::bitmap_font::GLYPH_H) / 2;
            crate::keycap::draw_line(buf, w, h, x0 + PAD, text_y, RECENT_HINT, 1, TEXT_DIM());
        }
    }
}

/// The work area (screen minus taskbar) of the screen holding `p`, as
/// `left, top, right, bottom`.
#[cfg(windows)]
pub(crate) fn work_area(_loop_target: &ActiveEventLoop, p: (i32, i32)) -> (i32, i32, i32, i32) {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTONEAREST};
    unsafe {
        let mon = MonitorFromPoint(POINT { x: p.0, y: p.1 }, MONITOR_DEFAULTTONEAREST);
        let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        if GetMonitorInfoW(mon, &mut mi).as_bool() {
            let r = mi.rcWork;
            return (r.left, r.top, r.right, r.bottom);
        }
    }
    (0, 0, 1920, 1080)
}

#[cfg(not(windows))]
pub(crate) fn work_area(loop_target: &ActiveEventLoop, p: (i32, i32)) -> (i32, i32, i32, i32) {
    let rect = |m: &winit::monitor::MonitorHandle| {
        let (pos, size) = (m.position(), m.size());
        (pos.x, pos.y, pos.x + size.width as i32, pos.y + size.height as i32)
    };
    loop_target.available_monitors().map(|m| rect(&m))
        .find(|&(l, t, r, b)| p.0 >= l && p.0 < r && p.1 >= t && p.1 < b)
        .or_else(|| loop_target.primary_monitor().map(|m| rect(&m)))
        .unwrap_or((0, 0, 1920, 1080))
}

#[cfg(windows)]
fn foreground() -> isize {
    unsafe { windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow().0 as isize }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> MenuState {
        MenuState {
            capture_key: Some("Print Screen".into()),
            full_key: None,
            record_key: Some("Ctrl + Shift + R".into()),
            recording: false,
            sound: RecordingOptions::NONE,
            last_region: true,
            gif: false,
            converting: false,
            ffmpeg: true,
        }
    }

    /// The tallest the menu gets (every key bound, a recording running) fits
    /// a 1280 x 720 screen with a 48 px taskbar, gaps included.
    #[test]
    fn the_tallest_menu_fits_a_small_screen() {
        let worst = MenuState {
            capture_key: Some("Ctrl + Shift + Alt + Print Screen".into()),
            full_key: Some("Ctrl + Shift + F".into()),
            record_key: Some("Ctrl + Shift + R".into()),
            recording: true,
            ..state()
        };
        for s in [worst.clone(), MenuState { recording: false, converting: true, ..worst.clone() },
                  MenuState { gif: true, ..worst }] {
            let l = layout(&s);
            assert!(l.h + 2 * GAP <= 720 - 48, "{} px tall", l.h);
        }
    }

    #[test]
    fn gif_swaps_the_sound_chips_for_a_note_and_converting_offers_cancel() {
        let gif = layout(&MenuState { gif: true, ..state() });
        assert!(gif.items.iter().all(|i| !matches!(i.act, Act::Sound(_))), "no sound chips for a GIF");
        assert_eq!(gif.notes.len(), 1);
        assert_eq!(gif.h, layout(&state()).h, "the menu keeps its height");
        let busy = layout(&MenuState { converting: true, ..state() });
        assert_eq!(find(&busy, "Cancel GIF").act, Act::Run(TrayEvent::CancelGif));
        assert!(!find(&busy, "Record region").enabled);
        assert!(busy.items.iter().filter(|i| i.kind == Kind::Chip && matches!(i.act, Act::Format(_)))
            .all(|i| !i.enabled), "the format can't change while a GIF is made");
        let none = layout(&MenuState { ffmpeg: false, ..state() });
        assert_eq!(find(&none, "GIF").act, Act::Run(TrayEvent::SetGif(true)), "GIF without FFmpeg says how to get it");
        let first = layout(&MenuState { last_region: false, ..state() });
        assert!(!find(&first, "Last region").enabled, "nothing to take again yet");
    }

    /// Writes target/tray-menu-recent.png: the menu with a full RECENT
    /// column (the third thumbnail pointed at) and target/tray-menu-empty.png
    /// with nothing kept yet. Thumbnails from the kept captures in
    /// `FF_RECENT_DIR` when set, else made up.
    #[test]
    #[ignore]
    fn render_tray_menu_with_recent_to_png() {
        let mut thumbs: Vec<image::RgbaImage> = std::env::var("FF_RECENT_DIR").ok()
            .and_then(|d| std::fs::read_dir(d).ok())
            .map(|rd| rd.flatten().filter(|f| f.file_name().to_string_lossy().ends_with(".thumb.png"))
                .filter_map(|f| image::open(f.path()).ok().map(|i| i.to_rgba8())).collect())
            .unwrap_or_default();
        let shades: [(u8, u8, u8); 5] = [(40, 90, 160), (200, 200, 205), (30, 30, 34), (150, 60, 90), (240, 240, 240)];
        let mut k = 0;
        while thumbs.len() < crate::recent::KEEP {
            let (r, g, b) = shades[k % shades.len()];
            let (w, h) = if k % 3 == 0 { (96, 54) } else if k % 3 == 1 { (60, 60) } else { (96, 40) };
            thumbs.push(image::RgbaImage::from_fn(w, h, |x, y| {
                let stripe = if y % 9 < 2 && x > 8 && x < w - 8 { 40 } else { 0 };
                image::Rgba([r.saturating_sub(stripe), g.saturating_sub(stripe), b.saturating_sub(stripe), 255])
            }));
            k += 1;
        }
        let entries: Vec<crate::recent::Entry> = thumbs.into_iter().enumerate().map(|(i, thumb)| crate::recent::Entry {
            id: format!("{i}"), ms: 1_795_140_000_000 + i as u64 * 60_000, size: (1280 + i as u32 * 10, 720), thumb,
        }).collect();
        let s = state();
        for (name, list, hover_cell) in [("recent", entries.clone(), Some(2)), ("empty", Vec::new(), None)] {
            let l = full_layout(&s, Some(list.len()));
            let (w, h) = ((W + COL_W) as usize, l.h as usize);
            let mut buf = vec![0u32; w * h];
            let hover = hover_cell.and_then(|c| l.items.iter().position(|i| i.act == Act::Recent(c)));
            draw(&mut buf, w, h, &l, hover, &s, false);
            draw_recent(&mut buf, w, h, &l, hover, &list);
            let img = image::RgbImage::from_fn(w as u32, h as u32, |x, y| {
                let p = buf[y as usize * w + x as usize];
                image::Rgb([(p >> 16) as u8, (p >> 8) as u8, p as u8])
            });
            img.save(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("../../target/tray-menu-{name}.png"))).unwrap();
        }
    }

    #[test]
    fn the_recent_column_sits_right_of_the_menu_above_the_footer() {
        let worst = MenuState { recording: true, full_key: Some("Ctrl + Shift + F".into()), ..state() };
        let l = full_layout(&worst, Some(crate::recent::KEEP));
        let cells: Vec<&Item> = l.items.iter().filter(|i| i.kind == Kind::Recent).collect();
        assert_eq!(cells.len(), crate::recent::KEEP);
        for c in &cells {
            let (x, y, w, h) = c.rect;
            assert!(x >= W + PAD && x + w <= W + COL_W - PAD + 1, "{:?} in the column", c.rect);
            assert!(y >= HEADER_H && y + h <= recent_caption_y(), "{:?} above its caption", c.rect);
        }
        assert!(recent_caption_y() + 22 <= l.footer_y, "the caption clears the footer");
        assert!(W + COL_W <= 1024 - 2 * GAP, "fits a small screen");
        // The menu's own items don't move.
        assert_eq!(l.items.iter().filter(|i| i.kind != Kind::Recent).count(), layout(&worst).items.len());
        assert!(crate::keycap::line_width(RECENT_HINT, 1) <= COL_W - 2 * PAD, "the hint fits");
        // Fewer kept: that many cells; keeping off: none.
        assert_eq!(full_layout(&state(), Some(3)).items.iter().filter(|i| i.kind == Kind::Recent).count(), 3);
        assert_eq!(full_layout(&state(), None).items.len(), layout(&state()).items.len());
    }

    #[test]
    fn the_other_captures_have_chips_that_open_their_pickers() {
        let l = layout(&state());
        let (last, scroll) = (find(&l, "Last region"), find(&l, "Scroll"));
        let (text, qr) = (find(&l, "Text"), find(&l, "QR"));
        assert_eq!(text.act, Act::Run(TrayEvent::CaptureText));
        assert_eq!(qr.act, Act::Run(TrayEvent::CaptureQr));
        assert_eq!(scroll.act, Act::Run(TrayEvent::CaptureScroll));
        assert!(text.enabled && qr.enabled);
        assert_eq!(scroll.enabled, crate::scroll::supported());
        assert_eq!(last.rect.1, scroll.rect.1, "Last region and Scroll share a row");
        assert_eq!(text.rect.1, qr.rect.1, "Text and QR share the Read row");
        assert!(text.rect.1 > last.rect.1, "Read comes after Also");
        for it in [scroll, qr] {
            assert!(it.rect.0 + it.rect.2 <= W - PAD, "{} inside the words' right edge", it.label);
        }
    }

    fn find<'a>(l: &'a Layout, label: &str) -> &'a Item {
        l.items.iter().find(|i| i.label == label).unwrap_or_else(|| panic!("no {label}"))
    }

    #[test]
    fn every_item_is_inside_the_window_and_none_overlap() {
        let l = layout(&state());
        for it in &l.items {
            let (x, y, w, h) = it.rect;
            assert!(x >= 0 && y >= HEADER_H && x + w <= W && y + h <= l.h, "{} at {:?}", it.label, it.rect);
        }
        for (i, a) in l.items.iter().enumerate() {
            for b in &l.items[i + 1..] {
                let (ax, ay, aw, ah) = a.rect;
                let (bx, by, bw, bh) = b.rect;
                let apart = ax + aw <= bx || bx + bw <= ax || ay + ah <= by || by + bh <= ay;
                assert!(apart, "{} overlaps {}", a.label, b.label);
            }
        }
    }

    #[test]
    fn rows_quote_their_keys_and_say_when_one_is_not_set() {
        let l = layout(&state());
        assert_eq!(find(&l, "Capture").hint.as_deref(), Some("Print Screen"));
        assert_eq!(find(&l, "Capture full screen").hint.as_deref(), Some("not set"));
        assert_eq!(find(&l, "Record screen").hint.as_deref(), Some("Ctrl + Shift + R"));
    }

    #[test]
    fn while_recording_the_record_row_stops_and_region_is_off() {
        let l = layout(&MenuState { recording: true, ..state() });
        assert_eq!(find(&l, "Stop recording").act, Act::Run(TrayEvent::StopRecording));
        assert!(!find(&l, "Record region").enabled);
        assert!(l.items.iter().all(|i| i.label != "Record screen"));
    }

    #[test]
    fn the_chosen_sound_rides_on_both_record_rows() {
        let l = layout(&MenuState { sound: RecordingOptions::MIC_AND_SYS, ..state() });
        assert_eq!(find(&l, "Record screen").act, Act::Run(TrayEvent::StartRecording(RecordingOptions::MIC_AND_SYS)));
        assert_eq!(find(&l, "Record region").act,
                   Act::Run(TrayEvent::StartRegionRecording(RecordingOptions::MIC_AND_SYS)));
    }

    /// Writes the menu in every scheme, plain and hovered, to
    /// `target/tray-menu/*.png` for a look by eye.
    /// `cargo test --release -p framefang-app tray_menu -- --ignored`
    #[test]
    #[ignore]
    fn render_the_menu_to_png() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/tray-menu");
        std::fs::create_dir_all(&dir).unwrap();
        let st = MenuState { sound: RecordingOptions::MIC_ONLY, ..state() };
        let l = layout(&st);
        let hovered = l.items.iter().position(|i| i.label == "Capture full screen");
        let chip = l.items.iter().position(|i| i.label == "5s");
        let quit = l.items.iter().position(|i| i.label == QUIT);
        for scheme in crate::palette::Scheme::ALL {
            crate::palette::set(scheme);
            for (tag, hover) in [("plain", None), ("row", hovered), ("chip", chip), ("quit", quit)] {
                let (w, h) = (W as usize, l.h as usize);
                let mut buf = vec![0u32; w * h];
                draw(&mut buf, w, h, &l, hover, &st, true);
                let img = image::RgbImage::from_fn(w as u32, h as u32, |x, y| {
                    let p = buf[y as usize * w + x as usize];
                    image::Rgb([(p >> 16) as u8, (p >> 8) as u8, p as u8])
                });
                img.save(dir.join(format!("{}-{tag}.png", scheme.name().to_lowercase()))).unwrap();
            }
        }
    }

    #[test]
    fn the_menu_opens_above_a_bottom_taskbar_and_stays_on_screen() {
        // 1920x1080 with a 48 px taskbar; the icon near the right end.
        let work = (0, 0, 1920, 1032);
        let (x, y) = place((1800, 1056), (W, 500), work);
        assert_eq!(y + 500, 1032 - GAP, "bottom just above the taskbar");
        assert!(x + W <= 1920 - GAP, "flipped left of the pointer");
        // A taskbar at the top: the menu drops below it.
        let (_, y) = place((1800, 20), (W, 500), (0, 48, 1920, 1080));
        assert_eq!(y, 48 + GAP);
        // A taskbar at the left: the menu starts right of it.
        let (x, _) = place((20, 900), (W, 500), (60, 0, 1920, 1080));
        assert_eq!(x, 60 + GAP);
    }
}
