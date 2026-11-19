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

use std::num::NonZeroU32;
use std::rc::Rc;
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
const CHIP_ROW_H: i32 = 34;
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
}

pub enum MenuOutcome {
    Closed,
    Run(TrayEvent),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Act {
    Run(TrayEvent),
    Sound(RecordingOptions),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Row,
    Chip,
    Footer,
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
    fn chips(l: &mut Layout, y: &mut i32, caption: &'static str, list: &[(&'static str, Act)]) {
        l.captions.push((*y, caption));
        let mut x = TEXT_X + CAPTION_W;
        let cy = *y + (CHIP_ROW_H - CHIP_H) / 2;
        for &(label, act) in list {
            let cw = chip_w(label);
            l.items.push(Item { kind: Kind::Chip, rect: (x, cy, cw, CHIP_H), label, hint: None, act, enabled: true });
            x += cw + 6;
        }
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
        ("3s", Act::Run(TrayEvent::CaptureDelayed(3))),
        ("5s", Act::Run(TrayEvent::CaptureDelayed(5))),
        ("10s", Act::Run(TrayEvent::CaptureDelayed(10))),
    ]);

    section(&mut l, &mut y, "RECORD");
    if s.recording {
        row(&mut l, &mut y, "Stop recording", key(&s.record_key), Act::Run(TrayEvent::StopRecording), true);
    } else {
        row(&mut l, &mut y, "Record screen", key(&s.record_key), Act::Run(TrayEvent::StartRecording(s.sound)), true);
    }
    row(&mut l, &mut y, "Record region", None, Act::Run(TrayEvent::StartRegionRecording(s.sound)), !s.recording);
    chips(&mut l, &mut y, "Sound", &[
        ("None", Act::Sound(RecordingOptions::NONE)),
        ("Mic", Act::Sound(RecordingOptions::MIC_ONLY)),
        ("PC", Act::Sound(RecordingOptions::SYSTEM_ONLY)),
        ("Both", Act::Sound(RecordingOptions::MIC_AND_SYS)),
    ]);
    row(&mut l, &mut y, "Annotate last recording", None, Act::Run(TrayEvent::AnnotateLastRecording), true);

    section(&mut l, &mut y, "FILES");
    row(&mut l, &mut y, "Open screenshots", None, Act::Run(TrayEvent::OpenSaveFolder), true);
    row(&mut l, &mut y, "Open recordings", None, Act::Run(TrayEvent::OpenRecordingsFolder), true);
    chips(&mut l, &mut y, "Convert", &[
        ("Image", Act::Run(TrayEvent::ConvertImage)),
        ("Video", Act::Run(TrayEvent::ConvertVideo)),
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
    pub outcome: Option<MenuOutcome>,
}

impl TrayMenu {
    /// Open the menu for a tray click at `click` (physical screen pixels).
    pub fn new(loop_target: &ActiveEventLoop, click: (i32, i32), state: MenuState) -> Result<Self> {
        let lay = layout(&state);
        let work = work_area(loop_target, click);
        let (x, y) = place(click, (W, lay.h), work);
        let attrs = crate::chrome::frameless(WindowAttributes::default())
            .with_title("Frame Fang")
            .with_resizable(false)
            .with_visible(false)
            .with_inner_size(PhysicalSize::new(W as u32, lay.h as u32))
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
        match it.act {
            Act::Run(ev) => self.outcome = Some(MenuOutcome::Run(ev)),
            Act::Sound(o) => {
                self.state.sound = o;
                // The record rows carry the sound, so lay out again.
                self.layout = layout(&self.state);
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
        draw(&mut buf, w.get() as usize, h.get() as usize, &self.layout, self.hover, self.state.sound,
             self.drawn_cursor);
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

fn draw(buf: &mut [u32], w: usize, h: usize, l: &Layout, hover: Option<usize>, sound: RecordingOptions,
        cursor_on: bool) {
    crate::hud::backdrop(buf, w, h, HEADER_H);
    crate::hud::heading(buf, w, h, PAD, 18, "FRAME FANG");
    {
        // The version, quiet, on the heading's middle.
        let v = format!("v{}", env!("CARGO_PKG_VERSION"));
        let vx = W - PAD - crate::bitmap_font::measure(&v, 1);
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
    for &y in &l.rules {
        fill(buf, w, h, (PAD, y, W - 2 * PAD, 1), HEADER_RULE());
    }
    // The footer: a band in the header's shade under a hairline.
    fill(buf, w, h, (0, l.footer_y, W, h as i32 - l.footer_y), BG_TOP());
    fill(buf, w, h, (0, l.footer_y, W, 1), HEADER_RULE());

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
                let chosen = it.act == Act::Sound(sound);
                let (bg, border, fg) = if chosen {
                    (LASER(), LASER(), BG_BODY())
                } else if hot {
                    (HOVER_FILL(), LASER_DIM(), TEXT_BRIGHT)
                } else {
                    (0, FIELD_BORDER(), TEXT_MUTED())
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
        }
    }
}

/// The work area (screen minus taskbar) of the screen holding `p`, as
/// `left, top, right, bottom`.
#[cfg(windows)]
fn work_area(_loop_target: &ActiveEventLoop, p: (i32, i32)) -> (i32, i32, i32, i32) {
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
fn work_area(loop_target: &ActiveEventLoop, p: (i32, i32)) -> (i32, i32, i32, i32) {
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
                draw(&mut buf, w, h, &l, hover, st.sound, true);
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
