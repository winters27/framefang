//! One-time welcome window, shown on the very first launch.
//!
//! Framefang has no main window: after startup the only evidence it is alive is
//! a tray icon, and the "Framefang is running" line it prints goes to a console
//! that neither the Windows nor the macOS build has. A first-time user is
//! therefore left staring at a desktop that looks unchanged. This window is
//! that missing confirmation — it names the app, the hotkey and the save
//! folder, and says where on *this* OS the tray icon actually appears.
//!
//! Structure mirrors `about_form.rs` (same palette, same `BufferSurface`
//! painter, same `Option<View>` + `outcome` lifecycle in the tray app), with
//! one button instead of four. Unlike the tray-failure About fallback,
//! dismissing this window never exits the app.

use std::num::NonZeroU32;
use std::rc::Rc;

use anyhow::{anyhow, Result};
use softbuffer::{Context, Surface};
use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::event_loop::ActiveEventLoop;
use winit::keyboard::{Key, NamedKey};
use winit::window::{CursorIcon, Window, WindowAttributes, WindowId};

use framefang_core::color::Rgba as FramefangRgba;

use crate::bitmap_font;
use crate::painter;

// Colors: the app's one palette (cyan and magenta on blue-black).
#[allow(unused_imports)]
use crate::palette::*;

const WIN_W: u32 = 520;
const WIN_H: u32 = 310;
const PAD:   i32 = 22;
const HEADER_H: i32 = 84;
const BTN_W: i32 = 140;
const BTN_H: i32 = 34;
/// Left edge of the value column in the HOTKEY / SAVE TO rows.
const VALUE_X: i32 = PAD + 90;

pub enum FirstRunOutcome {
    /// User clicked "Got it" / hit Esc or Enter / closed the window. The tray
    /// app just drops the view — the app keeps running.
    Closed,
}

pub struct FirstRunView {
    window:  Rc<Window>,
    _ctx:    Context<Rc<Window>>,
    surface: Surface<Rc<Window>, Rc<Window>>,
    hotkey:   String,
    save_dir: String,
    /// Where to look for the tray icon on this OS, one short line each.
    notes:    Vec<String>,
    btn:      (i32, i32, i32, i32),
    cursor:   (i32, i32),
    hover:    bool,
    /// The title strip drawn above the dialog (minimize, close, drag).
    title_bar: crate::chrome::TitleBar,
    pub outcome: Option<FirstRunOutcome>,
}

impl FirstRunView {
    /// `hotkey` is `settings.hotkey().describe()` and `save_dir` the resolved
    /// screenshot folder — both are passed in already rendered so this view
    /// never has to know how either is derived.
    pub fn new(
        loop_target: &ActiveEventLoop,
        hotkey:      &str,
        save_dir:    &str,
        wayland:     bool,
    ) -> Result<Self> {
        let win_h = WIN_H + crate::chrome::TITLE_H as u32;
        let (cx, cy) = centered_origin(loop_target, WIN_W, win_h);
        let attrs = crate::chrome::frameless(WindowAttributes::default())
            .with_title("Welcome to Framefang")
            .with_resizable(false)
            .with_inner_size(PhysicalSize::new(WIN_W, win_h))
            .with_position(PhysicalPosition::new(cx, cy))
            .with_window_icon(crate::brand_icon::shared());

        let window = loop_target
            .create_window(attrs)
            .map(Rc::new)
            .map_err(|e| anyhow!("create_window (firstrun): {e}"))?;
        crate::chrome::finish(&window);

        // Same reason as the About window: Framefang is a tray/menu-bar agent
        // with no Dock icon, so a window that opens behind the frontmost app
        // is a window the user never finds.
        window.focus_window();
        window.set_cursor(CursorIcon::Default);
        let ctx = Context::new(window.clone())
            .map_err(|e| anyhow!("softbuffer Context::new (firstrun): {e}"))?;
        let surface = Surface::new(&ctx, window.clone())
            .map_err(|e| anyhow!("softbuffer Surface::new (firstrun): {e}"))?;

        let mut me = FirstRunView {
            window, _ctx: ctx, surface,
            hotkey:   hotkey.to_owned(),
            save_dir: save_dir.to_owned(),
            notes:    locator_notes(wayland),
            btn:      (WIN_W as i32 - PAD - BTN_W, WIN_H as i32 - PAD - BTN_H, BTN_W, BTN_H),
            cursor:   (0, 0),
            hover:    false,
            title_bar: Default::default(),
            outcome:  None,
        };
        me.redraw();
        Ok(me)
    }

    pub fn window_id(&self) -> WindowId { self.window.id() }

    pub fn handle_event(&mut self, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => self.outcome = Some(FirstRunOutcome::Closed),
            WindowEvent::KeyboardInput {
                event: winit::event::KeyEvent {
                    logical_key: Key::Named(NamedKey::Escape) | Key::Named(NamedKey::Enter),
                    state: ElementState::Pressed, ..
                }, ..
            } => self.outcome = Some(FirstRunOutcome::Closed),
            WindowEvent::CursorLeft { .. } => {
                if self.title_bar.leave() { self.window.request_redraw(); }
            }
            WindowEvent::CursorMoved { position, .. } => {
                let raw = (position.x as i32, position.y as i32);
                if self.title_bar.hover_at(&self.window, raw) { self.window.request_redraw(); }
                // The dialog's own coordinates start below the title strip.
                self.cursor = (raw.0, raw.1 - crate::chrome::TITLE_H);
                let hovered = self.hit_test(self.cursor.0, self.cursor.1);
                self.window.set_cursor(if hovered { CursorIcon::Pointer } else { CursorIcon::Default });
                if hovered != self.hover {
                    self.hover = hovered;
                    self.window.request_redraw();
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left, ..
            } => {
                match self.title_bar.press(&self.window, self.cursor) {
                    Some(crate::chrome::TitlePart::Close) => return self.handle_event(WindowEvent::CloseRequested),
                    Some(_) => return,
                    None => {}
                }
                if self.hit_test(self.cursor.0, self.cursor.1) {
                    self.outcome = Some(FirstRunOutcome::Closed);
                } else {
                    crate::chrome::press_off_controls(&self.window, self.cursor.1, HEADER_H);
                }
            }
            WindowEvent::Resized(_) | WindowEvent::RedrawRequested => self.redraw(),
            _ => {}
        }
    }

    fn hit_test(&self, x: i32, y: i32) -> bool {
        let (bx, by, bw, bh) = self.btn;
        x >= bx && x < bx + bw && y >= by && y < by + bh
    }

    fn redraw(&mut self) {
        let phys = self.window.inner_size();
        let (Some(w), Some(h)) = (NonZeroU32::new(phys.width), NonZeroU32::new(phys.height))
            else { return; };
        if let Err(e) = self.surface.resize(w, h) { eprintln!("firstrun: surface.resize: {e}"); return; }
        let mut frame = match self.surface.buffer_mut() {
            Ok(b) => b,
            Err(e) => { eprintln!("firstrun: buffer_mut: {e}"); return; }
        };
        let win_w = w.get() as usize;
        // The dialog draws into its own buffer, placed below the title strip.
        let win_h = (h.get() as usize).saturating_sub(crate::chrome::TITLE_H as usize);
        let mut buf = vec![0u32; win_w * win_h];
        for y in 0..win_h {
            let band = if (y as i32) < HEADER_H { BG_TOP() } else { BG_BODY() };
            for x in 0..win_w { buf[y * win_w + x] = band; }
        }
        crate::palette::header_rule(&mut buf, win_w, win_h, HEADER_H);

        let mut surf = BufferSurface { buf: &mut buf, w: win_w as i32, h: win_h as i32 };

        // Title strip.
        draw_text(&mut surf, PAD, 22, 2, "FRAMEFANG // WELCOME", argb_to_framefang(LASER()));
        let version = format!("v{}  //  first run", env!("CARGO_PKG_VERSION"));
        draw_text(&mut surf, PAD, 50, 1, &version, argb_to_framefang(TEXT_MUTED()));

        // The one thing a first-time user needs to know.
        let mut y = HEADER_H + 26;
        draw_text(&mut surf, PAD, y, 1,
            "Framefang is running in your tray / menu bar.", argb_to_framefang(TEXT_BRIGHT));

        // Resolved settings, so the user can confirm both without opening the
        // Settings dialog.
        y += 30;
        draw_text(&mut surf, PAD, y, 1, "HOTKEY", argb_to_framefang(SECTION_TINT()));
        draw_text(&mut surf, VALUE_X, y, 1, &self.hotkey, argb_to_framefang(TEXT_BRIGHT));
        y += 18;
        let dir_w = WIN_W as i32 - PAD - VALUE_X;
        draw_text(&mut surf, PAD, y, 1, "SAVE TO", argb_to_framefang(SECTION_TINT()));
        draw_text(&mut surf, VALUE_X, y, 1, &truncate_for(&self.save_dir, dir_w),
            argb_to_framefang(TEXT_BRIGHT));

        // Where the icon actually is on this desktop.
        y += 32;
        for note in &self.notes {
            draw_text(&mut surf, PAD, y, 1, note, argb_to_framefang(TEXT_MUTED()));
            y += 16;
        }

        let (bx, by, bw, bh) = self.btn;
        if self.hover { fill_rect(&mut surf, bx, by, bw, bh, argb_to_framefang(0x0000_2818)); }
        stroke_rect_argb(&mut surf, bx, by, bw, bh, argb_to_framefang(LASER()));
        let label = "Got it";
        let tw = bitmap_font::measure(label, 1);
        draw_text(&mut surf, bx + (bw - tw) / 2, by + (bh - bitmap_font::GLYPH_H) / 2, 1,
            label, argb_to_framefang(LASER()));

        self.title_bar.draw(&mut frame, win_w, &buf, "Welcome", BG_TOP());
        if let Err(e) = frame.present() { eprintln!("firstrun: buf.present: {e}"); }
    }
}

/// Where to look for the tray icon, per OS. `cfg!` rather than `#[cfg]` so
/// every branch is type-checked on every platform.
///
/// Wording is deliberately concrete: the single most common "Framefang didn't
/// start" report is a Windows icon sitting in the hidden-icons overflow.
fn locator_notes(wayland: bool) -> Vec<String> {
    let mut notes: Vec<String> = Vec::new();
    if cfg!(target_os = "windows") {
        notes.push("Windows hides new tray icons behind the taskbar chevron.".to_owned());
        notes.push("Click it, then drag the Framefang icon onto the bar to pin it.".to_owned());
    } else if cfg!(target_os = "macos") {
        notes.push("Look for the Framefang icon at the top right of the menu bar.".to_owned());
        notes.push("Framefang runs as a menu-bar agent - it has no Dock icon.".to_owned());
    } else {
        notes.push("The icon needs an AppIndicator / tray host on your desktop.".to_owned());
        notes.push("GNOME needs the AppIndicator extension to show it.".to_owned());
        if wayland {
            // Wayland has no key grab: the shortcut is requested from the
            // desktop's global-shortcuts portal, and the desktop has the final
            // say on what it ends up being.
            notes.push("On Wayland your desktop assigns the capture shortcut.".to_owned());
        }
    }
    notes
}

// ── tiny rendering helpers (same shape as about_form) ───────────────────────

struct BufferSurface<'a> {
    buf: &'a mut [u32],
    w:   i32,
    h:   i32,
}

impl painter::Surface for BufferSurface<'_> {
    fn width(&self)  -> i32 { self.w }
    fn height(&self) -> i32 { self.h }
    fn read(&self, x: i32, y: i32) -> [u8; 4] {
        if x < 0 || y < 0 || x >= self.w || y >= self.h { return [0, 0, 0, 0xFF]; }
        let p = self.buf[(y as usize) * (self.w as usize) + (x as usize)];
        [((p >> 16) & 0xFF) as u8, ((p >> 8) & 0xFF) as u8, (p & 0xFF) as u8, 0xFF]
    }
    fn write(&mut self, x: i32, y: i32, rgba: [u8; 4]) {
        if x < 0 || y < 0 || x >= self.w || y >= self.h { return; }
        let dst = (y as usize) * (self.w as usize) + (x as usize);
        self.buf[dst] = ((rgba[0] as u32) << 16) | ((rgba[1] as u32) << 8) | rgba[2] as u32;
    }
}

fn argb_to_framefang(argb: u32) -> FramefangRgba {
    FramefangRgba {
        r: ((argb >> 16) & 0xFF) as u8,
        g: ((argb >>  8) & 0xFF) as u8,
        b: ( argb        & 0xFF) as u8,
        a: 255,
    }
}

fn centered_origin(loop_target: &ActiveEventLoop, w: u32, h: u32) -> (i32, i32) {
    let primary = loop_target.primary_monitor()
        .or_else(|| loop_target.available_monitors().next());
    let (mon_x, mon_y, mon_w, mon_h) = match primary {
        Some(m) => {
            let pos  = m.position();
            let size = m.size();
            (pos.x as i32, pos.y as i32, size.width as i32, size.height as i32)
        }
        None => (0, 0, 1920, 1080),
    };
    let x = mon_x + (mon_w - w as i32) / 2;
    let y = mon_y + (mon_h - h as i32) / 2;
    (x.max(mon_x), y.max(mon_y))
}


fn fill_rect<S: painter::Surface>(s: &mut S, x: i32, y: i32, w: i32, h: i32, color: FramefangRgba) {
    let rgba = [color.r, color.g, color.b, color.a];
    for yy in y..y + h { for xx in x..x + w { s.write(xx, yy, rgba); } }
}

fn stroke_rect_argb<S: painter::Surface>(s: &mut S, x: i32, y: i32, w: i32, h: i32, color: FramefangRgba) {
    let rgba = [color.r, color.g, color.b, color.a];
    for xx in x..x + w { s.write(xx, y, rgba); s.write(xx, y + h - 1, rgba); }
    for yy in y..y + h { s.write(x, yy, rgba); s.write(x + w - 1, yy, rgba); }
}

fn draw_text<S: painter::Surface>(s: &mut S, x: i32, y: i32, scale: i32, text: &str, color: FramefangRgba) {
    painter::draw_text(s, x, y, scale, text, color);
}

/// Shrink `s` until it fits in `max_px` at scale 1, eliding the middle: this
/// only ever renders a filesystem path, whose leaf is the part worth reading.
fn truncate_for(s: &str, max_px: i32) -> String {
    if bitmap_font::measure(s, 1) <= max_px { return s.to_owned(); }
    let ellipsis = "..";
    let advance = bitmap_font::GLYPH_W + 1;
    let fits = (max_px + 1) / advance;
    let ell_n = ellipsis.chars().count() as i32;
    if fits <= ell_n {
        return ellipsis.chars().take(fits.max(0) as usize).collect();
    }
    let keep = fits - ell_n;
    let tail = (keep * 2 / 3).max(1) as usize;
    let head = keep as usize - tail;
    let chars: Vec<char> = s.chars().collect();
    let mut out: String = chars[..head].iter().collect();
    out.push_str(ellipsis);
    out.extend(&chars[chars.len() - tail..]);
    out
}
