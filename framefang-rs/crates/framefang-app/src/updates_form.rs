//! Themed "Check for updates" dialog — same skin as the Settings + About
//! windows. Shows the installed version, the latest GitHub release tag,
//! the release date, the release notes (markdown body, rendered as
//! word-wrapped plain text in a scrollable pane) and a platform-aware
//! one-click Download button for the matching binary asset. Falls back to
//! "Open releases page" when no asset matches.
//!
//! Network fetch is fire-and-forget on a background thread. While it's in
//! flight the dialog shows "checking…"; on success it shows the parsed
//! release; on failure it shows a polite error and keeps the manual
//! "Open releases page" button working so the user has an out.

use std::num::NonZeroU32;
use std::rc::Rc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use softbuffer::{Context, Surface};
use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::ActiveEventLoop;
use winit::keyboard::{Key, NamedKey};
use winit::window::{CursorIcon, Window, WindowAttributes, WindowId};

use framefang_core::color::Rgba as FramefangRgba;
use framefang_core::install_channel::{InstallChannel, UpdateAction};

use crate::bitmap_font;
use crate::painter;

// Colors: the app's one palette (cyan and magenta on blue-black).
#[allow(unused_imports)]
use crate::palette::*;

const WIN_W: u32 = 560;
const WIN_H: u32 = 460;
const PAD:   i32 = 22;
const BTN_H: i32 = 30;
const HEADER_H: i32 = 84;
const LINE_H:   i32 = bitmap_font::GLYPH_H + 5;
const NOTES_TOP_OFFSET: i32 = 124;
const NOTES_BOTTOM_GAP: i32 = 70;
const SCROLLBAR_W: i32 = 6;

#[derive(Clone, Copy, PartialEq, Eq)]
enum BtnKind { OpenReleases, Download, CopyCommand, Close }

struct Btn {
    kind:  BtnKind,
    label: String,
    rect:  (i32, i32, i32, i32),
}

struct ReleaseInfo {
    tag:        String,
    date:       String,
    has_update: bool,
    notes:      Vec<String>,
    asset_url:  Option<String>,
    /// Hash for `asset_url`'s filename, parsed out of the release's
    /// SHA256SUMS asset. `None` means the release didn't ship one (older
    /// tags) or the file wasn't listed — the in-app installer logs a
    /// warning and proceeds; the MSI's own signature check is the next
    /// line of defense.
    expected_sha256: Option<String>,
    /// The picked asset's size in bytes, for the download's progress bar.
    asset_size: Option<u64>,
}

/// Update now was pressed: the download's progress, then installing, or
/// what went wrong.
struct Updating {
    url: String,
    error: Option<String>,
}

enum FetchState {
    Pending,
    Found(ReleaseInfo),
    Error(String),
}

pub enum UpdatesOutcome {
    Closed,
    OpenReleases,
    /// User clicked the platform-aware Download button (the parallel UI PR
    /// added this) — caller opens the asset URL in the system browser.
    OpenAsset(String),
    /// User clicked "Download & install" — caller (tray_loop) hands the
    /// asset URL + verified hash to `self_updater::download_and_install`
    /// and shows progress while it runs. `expected_sha256 = None` is the
    /// graceful-degradation path for releases that don't yet carry a
    /// SHA256SUMS file (older tags).
    DownloadAndInstall {
        asset_url:       String,
        expected_sha256: Option<String>,
    },
    /// This install is owned by a package manager, so the dialog offered
    /// the exact upgrade command instead of a download. Caller puts it on
    /// the clipboard.
    CopyCommand(String),
}

pub struct UpdatesView {
    window:  Rc<Window>,
    _ctx:    Context<Rc<Window>>,
    surface: Surface<Rc<Window>, Rc<Window>>,
    btns:    Vec<Btn>,
    cursor:  (i32, i32),
    hover:   Option<usize>,
    started: Instant,
    state:   FetchState,
    rx:      Option<mpsc::Receiver<Result<RawRelease, String>>>,
    scroll:  i32,
    notes_rect: (i32, i32, i32, i32),
    /// How this copy of Framefang was installed, and therefore whether the
    /// updater may replace it at all. Resolved once at construction —
    /// nothing it depends on changes while the process runs.
    channel: InstallChannel,
    action:  UpdateAction,
    /// The title strip drawn above the dialog (minimize, close, drag).
    title_bar: crate::chrome::TitleBar,
    /// Set once Update now is pressed. The dialog stays open and shows the
    /// download, then the install; Framefang restarts on its own.
    updating: Option<Updating>,
    pub outcome: Option<UpdatesOutcome>,
}

impl UpdatesView {
    pub fn new(loop_target: &ActiveEventLoop) -> Result<Self> {
        let win_h = WIN_H + crate::chrome::TITLE_H as u32;
        let (cx, cy) = centered_origin(loop_target, WIN_W, win_h);
        let attrs = crate::chrome::frameless(WindowAttributes::default())
            .with_title("Framefang — Updates")
            .with_resizable(false)
            .with_inner_size(PhysicalSize::new(WIN_W, win_h))
            .with_position(PhysicalPosition::new(cx, cy))
            .with_window_icon(crate::brand_icon::shared());

        let window = loop_target
            .create_window(attrs)
            .map(Rc::new)
            .map_err(|e| anyhow!("create_window (updates): {e}"))?;
        crate::chrome::finish(&window);

        // Framefang runs as a menu-bar/tray agent, so on macOS there is no Dock
        // icon to click if this opens behind the frontmost app. Ask for focus
        // explicitly; on Windows/Linux it is a harmless raise.
        window.focus_window();
        window.set_cursor(CursorIcon::Default);
        let ctx = Context::new(window.clone())
            .map_err(|e| anyhow!("softbuffer Context::new (updates): {e}"))?;
        let surface = Surface::new(&ctx, window.clone())
            .map_err(|e| anyhow!("softbuffer Surface::new (updates): {e}"))?;

        // Kick off the background fetch immediately so by the time the user
        // looks at the dialog there's usually already a result. `ureq`
        // isn't a workspace dep — we shell out to `curl` instead, which is
        // available on every desktop OS we ship for (curl is preinstalled
        // on macOS 10.15+, Windows 10 build 17063+, every modern Linux).
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let res = fetch_latest_release();
            let _ = tx.send(res);
        });

        let notes_rect = (
            PAD,
            HEADER_H + NOTES_TOP_OFFSET,
            WIN_W as i32 - PAD * 2,
            WIN_H as i32 - HEADER_H - NOTES_TOP_OFFSET - NOTES_BOTTOM_GAP,
        );

        let (channel, action) = crate::install_source::detected().clone();

        let mut me = UpdatesView {
            window, _ctx: ctx, surface,
            btns: Vec::new(),
            cursor: (0, 0),
            hover: None,
            started: Instant::now(),
            state: FetchState::Pending,
            rx: Some(rx),
            scroll: 0,
            notes_rect,
            channel,
            action,
            title_bar: Default::default(),
            updating: None,
            outcome: None,
        };
        me.btns = me.build_btns();
        me.redraw();
        Ok(me)
    }

    pub fn window_id(&self) -> WindowId { self.window.id() }

    /// Called from the tray-loop poll tick so we can advance the
    /// "checking…" animation and pick up the fetch result when it
    /// arrives.
    pub fn tick(&mut self) {
        if let Some(rx) = &self.rx {
            if let Ok(res) = rx.try_recv() {
                self.state = match res {
                    Ok(raw) => {
                        let has_update = !same_version(&raw.tag_name, env!("CARGO_PKG_VERSION"));
                        let date = parse_iso_date(&raw.published_at);
                        let notes = wrap_body(
                            &strip_markdown(&raw.body),
                            self.notes_rect.2 - SCROLLBAR_W - 8,
                        );
                        let asset_url = pick_asset_url(&raw.assets, self.channel);
                        // Resolve SHA-256 for whichever asset we picked
                        // by parsing the release's SHA256SUMS (when
                        // present). Missing → expected_sha256 = None →
                        // the installer logs a warning and proceeds.
                        let expected_sha256 = asset_url.as_deref().and_then(|url| {
                            let fname = url.rsplit('/').next()?.split('?').next()?;
                            crate::self_updater::parse_sha256sums(&raw.sha256sums, fname)
                        });
                        let asset_size = asset_url.as_deref().and_then(|url| {
                            raw.assets.iter().find(|a| a.browser_download_url == url).map(|a| a.size)
                        }).filter(|s| *s > 0);
                        FetchState::Found(ReleaseInfo {
                            tag: raw.tag_name,
                            date,
                            has_update,
                            notes,
                            asset_url,
                            expected_sha256,
                            asset_size,
                        })
                    }
                    Err(e) => FetchState::Error(e),
                };
                self.rx = None;
                self.btns = self.build_btns();
                self.window.request_redraw();
            }
        }
        // Keep the dot-dot-dot animation moving while we're waiting, and the
        // progress bar while an update downloads.
        if matches!(self.state, FetchState::Pending)
            || self.updating.as_ref().is_some_and(|u| u.error.is_none())
        {
            self.window.request_redraw();
        }
    }

    /// The update stopped short (download, checksum or installer): say why
    /// and offer Update now again.
    pub fn update_failed(&mut self, msg: String) {
        if let Some(u) = self.updating.as_mut() { u.error = Some(msg); }
        self.btns = self.build_btns();
        self.window.request_redraw();
    }

    /// Whether Update now installs in place here (rather than opening the
    /// download in the browser): an install we own, and an asset whose
    /// install path is wired up for it.
    fn installs_in_place(&self, url: &str) -> bool {
        matches!(self.action, UpdateAction::SelfInstall) && asset_supports_self_install(url, self.channel)
    }

    pub fn handle_event(&mut self, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => self.outcome = Some(UpdatesOutcome::Closed),
            WindowEvent::KeyboardInput {
                event: winit::event::KeyEvent {
                    logical_key, state: ElementState::Pressed, ..
                }, ..
            } => match logical_key {
                Key::Named(NamedKey::Escape) | Key::Named(NamedKey::Enter) => {
                    self.outcome = Some(UpdatesOutcome::Closed);
                }
                Key::Named(NamedKey::PageDown) => self.scroll_by(LINE_H * 6),
                Key::Named(NamedKey::PageUp)   => self.scroll_by(-LINE_H * 6),
                Key::Named(NamedKey::ArrowDown) => self.scroll_by(LINE_H),
                Key::Named(NamedKey::ArrowUp)   => self.scroll_by(-LINE_H),
                Key::Named(NamedKey::Home) => { self.scroll = 0; self.window.request_redraw(); }
                Key::Named(NamedKey::End)  => { self.scroll = self.max_scroll(); self.window.request_redraw(); }
                _ => {}
            },
            WindowEvent::MouseWheel { delta, .. } => {
                let dy = match delta {
                    MouseScrollDelta::LineDelta(_, y)   => (y * LINE_H as f32 * 3.0) as i32,
                    MouseScrollDelta::PixelDelta(p)     => p.y as i32,
                };
                // Wheel-up scrolls content up (shows later lines), match the rest of the OS.
                self.scroll_by(-dy);
            }
            WindowEvent::CursorLeft { .. } => {
                if self.title_bar.leave() { self.window.request_redraw(); }
            }
            WindowEvent::CursorMoved { position, .. } => {
                let raw = (position.x as i32, position.y as i32);
                if self.title_bar.hover_at(&self.window, raw) { self.window.request_redraw(); }
                // The dialog's own coordinates start below the title strip.
                self.cursor = (raw.0, raw.1 - crate::chrome::TITLE_H);
                let new_hover = self.hit_test(self.cursor.0, self.cursor.1);
                self.window.set_cursor(if new_hover.is_some() { CursorIcon::Pointer } else { CursorIcon::Default });
                if new_hover != self.hover {
                    self.hover = new_hover;
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
                if let Some(i) = self.hit_test(self.cursor.0, self.cursor.1) {
                    let kind = self.btns[i].kind;
                    self.outcome = Some(match kind {
                        BtnKind::OpenReleases => UpdatesOutcome::OpenReleases,
                        BtnKind::Download => {
                            if let FetchState::Found(info) = &self.state {
                                if let Some(url) = &info.asset_url {
                                    // In-app download -> verify -> install ->
                                    // relaunch happens only for a channel we
                                    // own (see framefang_core::install_channel)
                                    // *and* an asset shape whose swap path is
                                    // wired up: the Windows MSI (msiexec
                                    // handoff keeps Add/Remove Programs and
                                    // elevation correct), the Linux tarball
                                    // (straight binary swap), and the
                                    // AppImage (file swap). Everything else
                                    // opens in the browser.
                                    if self.installs_in_place(url) {
                                        // The dialog stays open with the
                                        // download's progress, then the
                                        // install; Framefang restarts itself.
                                        self.updating = Some(Updating { url: url.clone(), error: None });
                                        let _ = std::fs::remove_file(crate::self_updater::temp_path_for_url(url));
                                        self.btns = self.build_btns();
                                        self.window.request_redraw();
                                        UpdatesOutcome::DownloadAndInstall {
                                            asset_url: url.clone(),
                                            expected_sha256: info.expected_sha256.clone(),
                                        }
                                    } else {
                                        UpdatesOutcome::OpenAsset(url.clone())
                                    }
                                } else { UpdatesOutcome::OpenReleases }
                            } else { UpdatesOutcome::OpenReleases }
                        }
                        BtnKind::CopyCommand => {
                            match &self.action {
                                UpdateAction::Managed { command: Some(cmd), .. } => {
                                    UpdatesOutcome::CopyCommand(cmd.clone())
                                }
                                _ => UpdatesOutcome::OpenReleases,
                            }
                        }
                        BtnKind::Close => UpdatesOutcome::Closed,
                    });
                } else {
                    crate::chrome::press_off_controls(&self.window, self.cursor.1, HEADER_H);
                }
            }
            WindowEvent::Resized(_) | WindowEvent::RedrawRequested => self.redraw(),
            _ => {}
        }
    }

    fn scroll_by(&mut self, dy: i32) {
        let max = self.max_scroll();
        self.scroll = (self.scroll + dy).clamp(0, max);
        self.window.request_redraw();
    }

    fn max_scroll(&self) -> i32 {
        if let FetchState::Found(info) = &self.state {
            let total = info.notes.len() as i32 * LINE_H;
            (total - self.notes_rect.3 + 8).max(0)
        } else { 0 }
    }

    fn hit_test(&self, x: i32, y: i32) -> Option<usize> {
        self.btns.iter().position(|b| {
            let (bx, by, bw, bh) = b.rect;
            x >= bx && x < bx + bw && y >= by && y < by + bh
        })
    }

    fn build_btns(&self) -> Vec<Btn> {
        let mut btns = Vec::new();
        let header_btn_y = (HEADER_H - BTN_H) / 2 + 4;
        let close_w = 110;
        let close_x = WIN_W as i32 - PAD - close_w;
        btns.push(Btn { kind: BtnKind::Close, label: "Close".into(), rect: (close_x, header_btn_y, close_w, BTN_H) });

        let bh = 36;
        let by = WIN_H as i32 - PAD - bh;

        // Only offer an install action when there's actually a newer release
        // than what's running. When the user is already on the latest
        // version, drop it entirely — leaving "Open releases page" as the
        // only action — so we never invite a pointless re-download /
        // reinstall of the version they already have.
        let has_update = matches!(&self.state, FetchState::Found(info) if info.has_update);
        let has_asset  = matches!(&self.state, FetchState::Found(info) if info.asset_url.is_some());

        // Updating: nothing to press but Close until it fails (then the
        // same button again, as Try again).
        let failed = match &self.updating {
            Some(u) if u.error.is_none() => return btns,
            Some(_) => true,
            None => false,
        };
        // Update now when it installs in place, plain Download when the
        // browser has to fetch it.
        let in_place = matches!(&self.state, FetchState::Found(ReleaseInfo { asset_url: Some(url), .. })
                                if self.installs_in_place(url));
        let update_label = if failed { "Try again" } else if in_place { "Update now" } else { "Download" };

        // A package-managed install (Snap, Flatpak, Homebrew, deb/rpm/AUR,
        // Scoop) never gets a Download button — replacing those files is
        // either impossible or actively breaks the package. It gets the
        // upgrade command instead, drawn above and copyable from here.
        let primary = match &self.action {
            UpdateAction::Managed { command: Some(_), .. } if has_update => {
                Some(("Copy update command", BtnKind::CopyCommand))
            }
            UpdateAction::Managed { .. } => None,
            _ if has_update && has_asset => {
                Some((update_label, BtnKind::Download))
            }
            _ => None,
        };

        if let Some((label, kind)) = primary {
            let dl_w = 200;
            let rel_w = 180;
            let gap  = 12;
            let total = dl_w + gap + rel_w;
            let dl_x = (WIN_W as i32 - total) / 2;
            let rel_x = dl_x + dl_w + gap;
            btns.push(Btn { kind, label: label.into(), rect: (dl_x, by, dl_w, bh) });
            btns.push(Btn { kind: BtnKind::OpenReleases, label: "Open releases page".into(),      rect: (rel_x, by, rel_w, bh) });
        } else {
            let bw = 220;
            let bx = (WIN_W as i32 - bw) / 2;
            btns.push(Btn { kind: BtnKind::OpenReleases, label: "Open releases page".into(), rect: (bx, by, bw, bh) });
        }
        btns
    }

    fn redraw(&mut self) {
        let phys = self.window.inner_size();
        let (Some(w), Some(h)) = (NonZeroU32::new(phys.width), NonZeroU32::new(phys.height))
            else { return; };
        if let Err(e) = self.surface.resize(w, h) { eprintln!("updates: surface.resize: {e}"); return; }
        let mut frame = match self.surface.buffer_mut() {
            Ok(b) => b,
            Err(e) => { eprintln!("updates: buffer_mut: {e}"); return; }
        };
        let win_w = w.get() as usize;
        // The dialog draws into its own buffer, placed below the title strip.
        let win_h = (h.get() as usize).saturating_sub(crate::chrome::TITLE_H as usize);
        let mut buf = vec![0u32; win_w * win_h];
        crate::hud::backdrop(&mut buf, win_w, win_h, HEADER_H);
        let _ = PANEL_BORDER();

        let mut surf = BufferSurface { buf: &mut buf, w: win_w as i32, h: win_h as i32 };

        // Title strip.
        crate::hud::heading(surf.buf, win_w, win_h, PAD, 22, "FRAMEFANG // UPDATES");
        draw_text(&mut surf, PAD, 50, 1, "Check for new releases on GitHub.",
                  argb_to_framefang(TEXT_MUTED()));

        // Body — current + latest + date.
        let mut y = HEADER_H + 24;
        draw_text(&mut surf, PAD, y, 1, "INSTALLED",  argb_to_framefang(SECTION_TINT()));
        let installed = format!("v{}", env!("CARGO_PKG_VERSION"));
        draw_text(&mut surf, PAD + 120, y, 1, &installed, argb_to_framefang(TEXT_BRIGHT));
        y += 22;
        draw_text(&mut surf, PAD, y, 1, "LATEST",     argb_to_framefang(SECTION_TINT()));

        match &self.state {
            FetchState::Pending => {
                let dots = (self.started.elapsed().as_millis() / 400) % 4;
                let dots_s: String = std::iter::repeat('.').take(dots as usize).collect();
                let s = format!("checking{}", dots_s);
                draw_text(&mut surf, PAD + 120, y, 1, &s, argb_to_framefang(TEXT_MUTED()));
            }
            FetchState::Found(info) => {
                draw_text(&mut surf, PAD + 120, y, 1, &info.tag, argb_to_framefang(TEXT_BRIGHT));
                y += 22;
                if !info.date.is_empty() {
                    draw_text(&mut surf, PAD, y, 1, "RELEASED", argb_to_framefang(SECTION_TINT()));
                    draw_text(&mut surf, PAD + 120, y, 1, &info.date, argb_to_framefang(TEXT_BRIGHT));
                    y += 22;
                }
                if let Some(u) = &self.updating {
                    // Update now: the download's progress, then installing,
                    // or what stopped it.
                    let bar = (PAD, y + 18, WIN_W as i32 - PAD * 2, 8);
                    match &u.error {
                        Some(e) => {
                            draw_text(&mut surf, PAD, y, 1, "The update didn't finish.", argb_to_framefang(DANGER));
                            let msg = truncate_text(e, WIN_W as i32 - PAD * 2);
                            draw_text(&mut surf, PAD, y + 18, 1, &msg, argb_to_framefang(TEXT_MUTED()));
                        }
                        None => {
                            let got = std::fs::metadata(crate::self_updater::temp_path_for_url(&u.url))
                                .map(|m| m.len()).unwrap_or(0);
                            let (line, frac) = update_progress(got, info.asset_size, &info.tag);
                            draw_text(&mut surf, PAD, y, 1, &line, argb_to_framefang(LASER()));
                            fill_rect(&mut surf, bar.0, bar.1, bar.2, bar.3, argb_to_framefang(PANEL_FILL()));
                            stroke_rect_argb(&mut surf, bar.0, bar.1, bar.2, bar.3, argb_to_framefang(PANEL_BORDER()));
                            let fill_w = match frac {
                                Some(f) => ((bar.2 - 2) as f32 * f) as i32,
                                // Size unknown: a block sweeping across.
                                None => (bar.2 - 2) / 4,
                            };
                            let fill_x = match frac {
                                Some(_) => bar.0 + 1,
                                None => {
                                    let t = (self.started.elapsed().as_millis() % 1400) as f32 / 1400.0;
                                    bar.0 + 1 + ((bar.2 - 2 - fill_w) as f32 * t) as i32
                                }
                            };
                            fill_rect(&mut surf, fill_x, bar.1 + 1, fill_w.max(0), bar.3 - 2, argb_to_framefang(LASER()));
                        }
                    }
                } else {
                    let label = if info.has_update {
                        "A newer build is available."
                    } else {
                        "You're on the latest build."
                    };
                    let tint = if info.has_update { LASER() } else { TEXT_MUTED() };
                    draw_text(&mut surf, PAD, y, 1, label, argb_to_framefang(tint));
                }

                // Package-managed install: say who owns it and print the
                // exact upgrade one-liner where the Download button's
                // explanation would otherwise sit.
                if info.has_update {
                    if let UpdateAction::Managed { hint, command } = &self.action {
                        y += 20;
                        draw_text(&mut surf, PAD, y, 1, hint, argb_to_framefang(TEXT_MUTED()));
                        if let Some(cmd) = command {
                            y += 16;
                            draw_text(&mut surf, PAD, y, 1, cmd, argb_to_framefang(SECTION_TINT()));
                        }
                    }
                }

                let total = info.notes.len() as i32 * LINE_H;
                let max_scroll = (total - self.notes_rect.3 + 8).max(0);
                draw_notes(&mut surf, self.notes_rect, self.scroll, max_scroll, &info.notes);
            }
            FetchState::Error(e) => {
                draw_text(&mut surf, PAD + 120, y, 1, "unavailable", argb_to_framefang(DANGER));
                y += 28;
                let msg = format!("Couldn't reach GitHub: {}", e);
                draw_text(&mut surf, PAD, y, 1, &msg, argb_to_framefang(TEXT_MUTED()));
            }
        }

        for (i, b) in self.btns.iter().enumerate() {
            let hovered = self.hover == Some(i);
            render_btn(&mut surf, b, hovered);
        }

        self.title_bar.draw(&mut frame, win_w, &buf, "Updates", BG_TOP());
        if let Err(e) = frame.present() { eprintln!("updates: buf.present: {e}"); }
    }

}

fn draw_notes<S: painter::Surface>(
    surf: &mut S,
    rect: (i32, i32, i32, i32),
    scroll: i32,
    max_scroll: i32,
    lines: &[String],
) {
    let (nx, ny, nw, nh) = rect;
    fill_rect(surf, nx, ny, nw, nh, argb_to_framefang(PANEL_FILL()));
    stroke_rect_argb(surf, nx, ny, nw, nh, argb_to_framefang(PANEL_BORDER()));

    let inner_x = nx + 8;
    let inner_y = ny + 6;
    let inner_h = nh - 12;
    let first_visible = (scroll / LINE_H).max(0);
    let last_visible  = ((scroll + inner_h) / LINE_H + 1).min(lines.len() as i32);
    for i in first_visible..last_visible {
        let line_y = inner_y + (i * LINE_H) - scroll;
        if line_y + bitmap_font::GLYPH_H < inner_y || line_y > inner_y + inner_h { continue; }
        let line = &lines[i as usize];
        if let Some(rest) = line.strip_prefix("# ") {
            draw_text(surf, inner_x, line_y, 1, rest, argb_to_framefang(SECTION_TINT()));
        } else if line.is_empty() {
            continue;
        } else {
            draw_text(surf, inner_x, line_y, 1, line, argb_to_framefang(TEXT_BRIGHT));
        }
    }

    let bar_x = nx + nw - SCROLLBAR_W - 2;
    let bar_y = ny + 4;
    let bar_h = nh - 8;
    fill_rect(surf, bar_x, bar_y, SCROLLBAR_W, bar_h, argb_to_framefang(SCROLLBAR_BG()));
    if max_scroll > 0 {
        let total = (lines.len() as i32 * LINE_H).max(1);
        let thumb_h = ((bar_h as f32) * (inner_h as f32 / total as f32)).max(18.0) as i32;
        let thumb_h = thumb_h.min(bar_h);
        let progress = scroll as f32 / max_scroll as f32;
        let thumb_y = bar_y + ((bar_h - thumb_h) as f32 * progress) as i32;
        fill_rect(surf, bar_x, thumb_y, SCROLLBAR_W, thumb_h, argb_to_framefang(SCROLLBAR_FG()));
    }
}

// ── network + parsing ───────────────────────────────────────────────────────

struct RawAsset {
    name: String,
    browser_download_url: String,
    // Kept for a future "Download (12.3 MB)" hover affordance.
    #[allow(dead_code)]
    size: u64,
}

struct RawRelease {
    tag_name:     String,
    body:         String,
    published_at: String,
    // Kept for a future "View on GitHub" link in the dialog footer.
    #[allow(dead_code)]
    html_url:     String,
    assets:       Vec<RawAsset>,
    /// Body of the release's `SHA256SUMS` asset (the standard
    /// `sha256sum -b` output the release-builder CI generates). Empty
    /// when the release didn't ship one — older tags pre-`feat/self-
    /// updater` won't have it.
    sha256sums:   String,
}

/// Shell out to `curl` (always-present on Linux / macOS / Windows 10+).
/// Parses the full release JSON via `serde_json` so we can pull the tag,
/// body, date, and asset list without re-doing string scanning per field.
fn fetch_latest_release() -> Result<RawRelease, String> {
    let url = "https://api.github.com/repos/winters27/framefang/releases/latest";
    let out = crate::self_updater::hide_console(std::process::Command::new("curl")
        .args([
            "-sS", "-A", "framefang-updater",
            "--max-time", "8",
            "-H", "Accept: application/vnd.github+json",
            url,
        ]))
        .output()
        .map_err(|e| format!("curl: {e}"))?;
    if !out.status.success() {
        return Err(format!("curl exit {}", out.status));
    }
    let body = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value = serde_json::from_str(&body)
        .map_err(|e| format!("parse JSON: {e}"))?;

    let tag_name = v.get("tag_name").and_then(|x| x.as_str())
        .ok_or_else(|| "tag_name missing".to_owned())?.to_owned();
    let body_md = v.get("body").and_then(|x| x.as_str()).unwrap_or("").to_owned();
    // GitHub returns `published_at` even for drafts so we use it as the canonical date.
    let published_at = v.get("published_at").and_then(|x| x.as_str()).unwrap_or("").to_owned();
    let html_url = v.get("html_url").and_then(|x| x.as_str()).unwrap_or("").to_owned();

    let mut assets = Vec::new();
    let mut sha256sums_url: Option<String> = None;
    if let Some(arr) = v.get("assets").and_then(|x| x.as_array()) {
        for a in arr {
            let name = a.get("name").and_then(|x| x.as_str()).unwrap_or("").to_owned();
            let bdu  = a.get("browser_download_url").and_then(|x| x.as_str()).unwrap_or("").to_owned();
            let size = a.get("size").and_then(|x| x.as_u64()).unwrap_or(0);
            if name.eq_ignore_ascii_case("SHA256SUMS") && !bdu.is_empty() {
                sha256sums_url = Some(bdu.clone());
            }
            if !name.is_empty() && !bdu.is_empty() {
                assets.push(RawAsset { name, browser_download_url: bdu, size });
            }
        }
    }

    // Best-effort SHA256SUMS fetch; failures here are non-fatal so the
    // dialog still shows release notes even when the hash file is absent.
    let sha256sums = sha256sums_url
        .and_then(|url| {
            crate::self_updater::hide_console(std::process::Command::new("curl")
                .args(["-fsSL", "-A", "framefang-updater", "--max-time", "8"])
                .arg(&url))
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        })
        .unwrap_or_default();

    Ok(RawRelease { tag_name, body: body_md, published_at, html_url, assets, sha256sums })
}

/// The progress line while Update now runs, and how full the bar is (`None`
/// when the size is unknown): downloading with megabytes so far, then, once
/// all of it is in, installing.
fn update_progress(got: u64, total: Option<u64>, tag: &str) -> (String, Option<f32>) {
    let mb = |b: u64| b as f64 / (1024.0 * 1024.0);
    match total {
        Some(t) if got >= t => (format!("Installing {tag}... Framefang restarts by itself."), Some(1.0)),
        Some(t) => (format!("Downloading {tag}  {:.1} / {:.1} MB", mb(got), mb(t)), Some(got as f32 / t as f32)),
        None => (format!("Downloading {tag}..."), None),
    }
}

/// `s` cut with `...` to fit `max_w` pixels of the 1x bitmap font.
fn truncate_text(s: &str, max_w: i32) -> String {
    if bitmap_font::measure(s, 1) <= max_w { return s.to_owned(); }
    let mut out: String = s.chars().collect();
    while !out.is_empty() && bitmap_font::measure(&format!("{out}..."), 1) > max_w { out.pop(); }
    format!("{out}...")
}

/// First 10 chars of an ISO-8601 timestamp, e.g. "2026-05-17T18:22:11Z" -> "2026-05-17".
fn parse_iso_date(iso: &str) -> String {
    if iso.len() >= 10 { iso[..10].to_owned() } else { String::new() }
}

/// `tag_name` from GitHub may be "v0.1" or "0.1" or "v0.1.0"; the embedded
/// CARGO_PKG_VERSION is always plain "0.1.0". Strip "v" prefixes and trailing
/// ".0" tails before comparing so the obvious shapes match.
fn same_version(tag: &str, pkg: &str) -> bool {
    fn norm(s: &str) -> String {
        let s = s.trim().trim_start_matches('v').trim_start_matches('V');
        let mut parts: Vec<&str> = s.split('.').collect();
        while parts.last().map(|p| *p == "0").unwrap_or(false) && parts.len() > 1 {
            parts.pop();
        }
        parts.join(".")
    }
    norm(tag) == norm(pkg)
}

/// Pick the right release asset for the current OS+arch. Substring-matches
/// so suffix tweaks (e.g. `-v0.3.0`) don't break the lookup.
///
/// On Windows we resolve in two passes: if this framefang.exe was installed
/// from `Framefang.msi` we prefer the `.msi` asset (which `self_updater`
/// hands off to msiexec so MajorUpgrade replaces in place + bumps the
/// ARP entry); if no MSI is in the release, or we're running portable,
/// we fall back to the per-arch zip.
fn pick_asset_url(assets: &[RawAsset], channel: InstallChannel) -> Option<String> {
    // An AppImage run must be handed another AppImage: its `current_exe()`
    // lives in the read-only FUSE mount, so the tarball payload has nowhere
    // to go. If the release ships no AppImage for this architecture we offer
    // no download at all rather than the wrong artifact.
    if channel == InstallChannel::AppImage {
        return assets
            .iter()
            .find(|a| {
                let name = a.name.to_ascii_lowercase();
                name.ends_with(".appimage") && arch_tokens().iter().any(|t| name.contains(t))
            })
            .map(|a| a.browser_download_url.clone());
    }

    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    {
        // "Installed by the MSI" is the install-channel question, answered
        // once in framefang_core::install_channel rather than re-sniffed here.
        if channel == InstallChannel::WindowsMsi {
            if let Some(a) = assets.iter().find(|a| a.name.eq_ignore_ascii_case("Framefang.msi")) {
                return Some(a.browser_download_url.clone());
            }
            // MSI-installed user, release shipped no MSI: fall through to
            // the zip — the swap path still works as a fallback, just
            // leaves ARP showing the old version until next manual MSI run.
        }
    }

    // Release asset names (`.github/workflows/release.yml`), matched without
    // regard to case.
    let needle: &str = if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        "framefang-linux-x86_64-portable.tar.gz"
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        "framefang-linux-arm64-portable.tar.gz"
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        // The .dmg: a Mac app bundle updates through the browser, so the
        // user gets the same installer as a first download.
        "framefang-macos-arm64.dmg"
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        "framefang-macos-x64.dmg"
    } else if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        // The portable exe, published loose since 1.3.1; releases up to
        // 1.3.0 zipped it.
        if let Some(a) = assets.iter().find(|a| a.name.eq_ignore_ascii_case(PORTABLE_EXE)) {
            return Some(a.browser_download_url.clone());
        }
        "framefang-windows-x86_64.zip"
    } else {
        return None;
    };
    assets.iter()
        .find(|a| a.name.to_ascii_lowercase().contains(needle))
        .map(|a| a.browser_download_url.clone())
}

/// Architecture spellings that appear in release-asset names, most specific
/// first. Used to keep an arm64 host from grabbing an x86_64 AppImage.
fn arch_tokens() -> &'static [&'static str] {
    if cfg!(target_arch = "x86_64") {
        &["x86_64", "amd64", "x64"]
    } else if cfg!(target_arch = "aarch64") {
        &["aarch64", "arm64"]
    } else {
        &[]
    }
}

/// The portable Windows build's release asset: the app itself, unzipped.
const PORTABLE_EXE: &str = "Framefang-portable.exe";

/// Does the picked asset have a shape whose in-place install path is wired
/// up for `channel`? The channel decides whether we may replace this install
/// at all; this decides whether we know *how* to. A mismatch (e.g. an
/// MSI-installed Framefang on a release that only shipped the portable zip)
/// falls back to a browser download.
fn asset_supports_self_install(url: &str, channel: InstallChannel) -> bool {
    let name = url.rsplit('/').next().unwrap_or("");
    let name = name.split('?').next().unwrap_or(name).to_ascii_lowercase();
    match channel {
        InstallChannel::WindowsMsi => name.ends_with(".msi"),
        InstallChannel::AppImage => name.ends_with(".appimage"),
        // The plain tarball layout (install.sh, or an unpacked release
        // tarball) is the only portable install whose swap path is proven.
        InstallChannel::Portable => {
            cfg!(target_os = "linux") && (name.ends_with(".tar.gz") || name.ends_with(".tgz"))
        }
        // The Windows portable exe (or the zip older releases wrapped it
        // in): the swap renames the running .exe aside.
        InstallChannel::WindowsPortable => name.ends_with(".exe") || name.ends_with(".zip"),
        _ => false,
    }
}

/// Strip the bare-minimum markdown markers we expect from GitHub release
/// bodies: `**bold**`, `*em*`, `_em_`, `` `code` ``, leading `#` headings,
/// leading `- ` / `* ` bullets, and `[text](url)` links. Keeps line breaks.
fn strip_markdown(md: &str) -> String {
    let mut out = String::with_capacity(md.len());
    for raw_line in md.lines() {
        let mut line = raw_line.trim_end_matches('\r').to_owned();
        // Heading.
        if let Some(rest) = line.strip_prefix("### ") { line = format!("# {rest}"); }
        else if let Some(rest) = line.strip_prefix("## ") { line = format!("# {rest}"); }
        // "# " stays as-is so renderer can tint it.
        // Bullet — the bitmap font (bitmap_font.rs) only ships glyphs for
        // ASCII 0x20..=0x7E, so we stick to `>` here. A literal `•` would
        // fall back to `?` in the renderer.
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("- ").or_else(|| trimmed.strip_prefix("* ")) {
            let indent = line.len() - trimmed.len();
            line = format!("{}> {}", " ".repeat(indent), rest);
        }
        // Inline replacements.
        line = strip_inline(&line);
        out.push_str(&line);
        out.push('\n');
    }
    out
}

fn strip_inline(s: &str) -> String {
    // Order matters: bold (**…**) before em (*…*).
    let s = replace_pair(s, "**", "");
    let s = replace_pair(&s, "__", "");
    let s = replace_pair(&s, "*",  "");
    let s = replace_pair(&s, "_",  "");
    let s = replace_pair(&s, "`",  "");
    // [text](url) -> "text (url)". Walk by byte indices on the &str (not over
    // raw bytes) so multi-byte UTF-8 characters survive — the previous
    // `bytes[i] as char` cast turned every UTF-8 continuation byte into its
    // own char, and the bitmap font rendered each as `?`.
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        let rest = &s[i..];
        if rest.starts_with('[') {
            if let Some(end_txt) = rest[1..].find(']') {
                let txt = &rest[1..1 + end_txt];
                let after = &rest[1 + end_txt + 1..];
                if let Some(after_paren) = after.strip_prefix('(') {
                    if let Some(end_url) = after_paren.find(')') {
                        let url = &after_paren[..end_url];
                        out.push_str(txt);
                        out.push_str(" (");
                        out.push_str(url);
                        out.push(')');
                        // advance past `[txt](url)` — sizes are byte counts.
                        i += 1 + end_txt + 1 + 1 + end_url + 1;
                        continue;
                    }
                }
            }
        }
        // Step one full UTF-8 char.
        let ch = rest.chars().next().expect("non-empty rest");
        out.push(ch);
        i += ch.len_utf8();
    }
    // Final pass: any remaining non-ASCII (e.g. em-dashes, smart quotes from
    // GitHub release bodies) gets replaced with ASCII fallbacks so the
    // bitmap font (0x20..=0x7E only) doesn't render `?`.
    out
        .replace('—', "-")
        .replace('–', "-")
        .replace('•', ">")
        .replace('“', "\"")
        .replace('”', "\"")
        .replace('‘', "'")
        .replace('’', "'")
        .replace('…', "...")
}

/// Strip every occurrence of `marker` in `s`, replacing with `replacement`.
fn replace_pair(s: &str, marker: &str, replacement: &str) -> String {
    s.replace(marker, replacement)
}

/// Word-wrap `text` to `max_px` using the bitmap-font's 5×7-per-glyph
/// width. Each `\n` forces a hard break and is preserved as an empty
/// entry so paragraphs stay separated in the rendered output.
fn wrap_body(text: &str, max_px: i32) -> Vec<String> {
    let mut out = Vec::new();
    for raw_line in text.lines() {
        if raw_line.trim().is_empty() {
            out.push(String::new());
            continue;
        }
        // Preserve the heading marker so the renderer can re-detect it.
        let (prefix, rest) = if let Some(r) = raw_line.strip_prefix("# ") {
            ("# ", r)
        } else { ("", raw_line) };
        let words: Vec<&str> = rest.split(' ').collect();
        let mut cur = String::new();
        for w in words {
            if w.is_empty() {
                if !cur.is_empty() { cur.push(' '); }
                continue;
            }
            let probe = if cur.is_empty() {
                w.to_owned()
            } else {
                format!("{} {}", cur, w)
            };
            let measured = bitmap_font::measure(&format!("{prefix}{probe}"), 1);
            if measured <= max_px || cur.is_empty() {
                cur = probe;
            } else {
                out.push(format!("{prefix}{cur}"));
                cur = w.to_owned();
            }
        }
        if !cur.is_empty() { out.push(format!("{prefix}{cur}")); }
    }
    out
}

// ── button rendering ────────────────────────────────────────────────────────

fn render_btn<S: painter::Surface>(surf: &mut S, b: &Btn, hovered: bool) {
    let (x, y, w, h) = b.rect;
    let is_primary = matches!(b.kind, BtnKind::Close | BtnKind::Download | BtnKind::CopyCommand);
    let border = if is_primary { LASER() } else if hovered { LASER_DIM() } else { PANEL_BORDER() };
    let fill   = if is_primary && hovered { PRIMARY_FILL() } else if hovered { HOVER_FILL() } else { 0x0000_0000 };
    if fill != 0 { fill_rect(surf, x, y, w, h, argb_to_framefang(fill)); }
    stroke_rect_argb(surf, x, y, w, h, argb_to_framefang(border));
    let tw = bitmap_font::measure(&b.label, 1);
    let tx = x + (w - tw) / 2;
    let ty = y + (h - bitmap_font::GLYPH_H) / 2;
    let color = if is_primary { LASER() } else { TEXT_BRIGHT };
    draw_text(surf, tx, ty, 1, &b.label, argb_to_framefang(color));
}

// ── tiny rendering helpers (same shape as settings_form) ────────────────────

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

fn centered_origin(loop_target: &winit::event_loop::ActiveEventLoop, w: u32, h: u32) -> (i32, i32) {
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

// Quiet unused-imports warnings for items kept around for parity with the
// other dialog modules.
fn _quiet() { let _ = Duration::from_secs(0); }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_progress_line_counts_megabytes_then_says_installing() {
        let mb = 1024 * 1024;
        let (line, f) = update_progress(mb, Some(4 * mb), "v1.3.1");
        assert_eq!(line, "Downloading v1.3.1  1.0 / 4.0 MB");
        assert_eq!(f, Some(0.25));
        let (line, f) = update_progress(4 * mb, Some(4 * mb), "v1.3.1");
        assert!(line.starts_with("Installing v1.3.1"), "{line}");
        assert_eq!(f, Some(1.0));
        assert_eq!(update_progress(10, None, "v1.3.1").1, None);
        // Every line is plain ASCII for the bitmap font.
        assert!(line.is_ascii());
    }

    #[test]
    fn each_platform_finds_its_own_file_in_a_full_release() {
        let assets: Vec<RawAsset> = [
            "Framefang.msi", "Framefang-portable.exe",
            "Framefang-macos-arm64.dmg", "Framefang-macos-x64.dmg",
            "Framefang-linux-x86_64.AppImage",
            "Framefang-linux-x86_64-portable.tar.gz", "Framefang-linux-arm64-portable.tar.gz",
            "SHA256SUMS",
        ].iter().map(|n| RawAsset { name: (*n).into(), browser_download_url: format!("https://x/{n}"), size: 1 }).collect();
        let pick = |c| pick_asset_url(&assets, c).map(|u| u.rsplit('/').next().unwrap().to_owned());
        if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
            assert_eq!(pick(InstallChannel::WindowsMsi).as_deref(), Some("Framefang.msi"));
            assert_eq!(pick(InstallChannel::WindowsPortable).as_deref(), Some("Framefang-portable.exe"));
        }
        if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
            assert_eq!(pick(InstallChannel::AppImage).as_deref(), Some("Framefang-linux-x86_64.AppImage"));
            assert_eq!(pick(InstallChannel::Portable).as_deref(), Some("Framefang-linux-x86_64-portable.tar.gz"));
        }
        if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
            assert_eq!(pick(InstallChannel::MacAppBundle).as_deref(), Some("Framefang-macos-arm64.dmg"));
        }
    }

    #[test]
    fn the_portable_windows_zip_updates_in_place() {
        let zip = "https://x/framefang-windows-x86_64.zip";
        assert!(asset_supports_self_install(zip, InstallChannel::WindowsPortable));
        assert!(asset_supports_self_install("https://x/Framefang-portable.exe", InstallChannel::WindowsPortable));
        assert!(asset_supports_self_install("https://x/Framefang.msi", InstallChannel::WindowsMsi));
        assert!(!asset_supports_self_install(zip, InstallChannel::WindowsMsi));
        assert!(!asset_supports_self_install(zip, InstallChannel::MacAppBundle));
    }

    /// The bitmap font (bitmap_font.rs) only has glyphs for ASCII
    /// 0x20..=0x7E; everything else renders as `?`. After strip_markdown
    /// the rendered text must therefore be pure printable ASCII, or the
    /// `???`-per-bullet bug (and friends) come back.
    fn assert_renderable_ascii(s: &str) {
        for ch in s.chars() {
            assert!(
                ch == '\n' || (' '..='~').contains(&ch),
                "non-renderable char {:?} (U+{:04X}) survived strip_markdown — \
                 the bitmap font would draw it as '?'",
                ch, ch as u32
            );
        }
    }

    #[test]
    fn github_release_body_renders_as_ascii() {
        // Verbatim "What's Changed" block GitHub auto-generates, which is
        // exactly what tripped the `???` bug: `* ` bullets plus an
        // em-dash and a couple of links.
        let body = "## What's Changed\n\
            * fix(docs): drop stale icon.svg by @winters27 in https://github.com/winters27/framefang/pull/37\n\
            * fix(windows): dismiss tray menu — overflow flyout before capture by @winters27 in https://github.com/winters27/framefang/pull/39\n\
            \n\
            **Full Changelog**: https://github.com/winters27/framefang/compare/v0.3.6...v0.3.7";
        let out = strip_markdown(body);
        assert_renderable_ascii(&out);
        // The literal U+2022 bullet must never appear — we use ASCII '>'.
        assert!(!out.contains('\u{2022}'), "U+2022 bullet leaked into output");
        assert!(out.contains("> fix(docs)"), "bullet not rewritten to '>': {out:?}");
        // The em-dash must have been downgraded to ASCII '-'.
        assert!(!out.contains('\u{2014}'), "em-dash leaked into output");
    }

    #[test]
    fn strip_inline_keeps_multibyte_text_intact() {
        // A line with a non-ASCII char that is NOT in our fallback table
        // still must not be split into per-byte garbage — strip_inline
        // walks chars, so the single char survives as a single char
        // (which the renderer then maps to one '?', not three).
        let out = strip_inline("café [docs](https://x.y)");
        assert_eq!(out, "café docs (https://x.y)");
    }

    /// The window is a fixed 560px framebuffer with no wrapping in the info
    /// block, so every hint / command we might print there has to fit on one
    /// line — and the copy button's label inside its 200px box.
    #[test]
    fn managed_hints_and_commands_fit_the_dialog() {
        use framefang_core::install_channel::{update_action, HostOs, InstallProbe};

        let max_line = WIN_W as i32 - PAD * 2;
        let max_label = 200 - 16;
        assert!(bitmap_font::measure("Copy update command", 1) <= max_label);

        let os_releases = [
            None,
            Some("ID=ubuntu\nID_LIKE=debian\n"),
            Some("ID=fedora\n"),
            Some("ID=arch\n"),
            Some("ID=opensuse-leap\nID_LIKE=suse\n"),
            Some("ID=voidlinux\n"),
        ];
        let channels = [
            (InstallChannel::Snap, HostOs::Linux),
            (InstallChannel::Flatpak, HostOs::Linux),
            (InstallChannel::LinuxSystemPackage, HostOs::Linux),
            (InstallChannel::HomebrewCask, HostOs::MacOs),
            (InstallChannel::HomebrewFormula, HostOs::MacOs),
            (InstallChannel::Scoop, HostOs::Windows),
        ];

        for (channel, os) in channels {
            for os_release in os_releases {
                let probe = InstallProbe {
                    os,
                    os_release: os_release.map(str::to_owned),
                    ..Default::default()
                };
                let UpdateAction::Managed { hint, command } = update_action(channel, &probe) else {
                    panic!("{channel:?} must be package-managed");
                };
                assert!(
                    bitmap_font::measure(hint, 1) <= max_line,
                    "hint too wide for the dialog ({channel:?}): {hint:?}"
                );
                if let Some(cmd) = &command {
                    assert!(
                        bitmap_font::measure(cmd, 1) <= max_line,
                        "command too wide for the dialog ({channel:?}): {cmd:?}"
                    );
                    // The bitmap font only has glyphs for printable ASCII.
                    assert!(
                        cmd.chars().all(|c| (' '..='~').contains(&c)),
                        "command has unrenderable characters: {cmd:?}"
                    );
                }
            }
        }
    }

    fn asset(name: &str) -> RawAsset {
        RawAsset {
            name: name.to_owned(),
            browser_download_url: format!("https://example.com/dl/{name}"),
            size: 0,
        }
    }

    #[test]
    fn an_appimage_install_is_only_ever_offered_an_appimage() {
        let assets = vec![
            asset("framefang-linux-x86_64.tar.gz"),
            asset("framefang-x86_64.AppImage"),
            asset("SHA256SUMS"),
        ];
        let picked = pick_asset_url(&assets, InstallChannel::AppImage);
        if cfg!(target_arch = "x86_64") {
            assert_eq!(
                picked.as_deref(),
                Some("https://example.com/dl/framefang-x86_64.AppImage")
            );
        } else {
            // No AppImage for this architecture in the release: offer
            // nothing rather than a tarball we can't install.
            assert_eq!(picked, None);
        }
    }

    #[test]
    fn an_appimage_install_gets_nothing_when_the_release_has_no_appimage() {
        let assets = vec![asset("framefang-linux-x86_64.tar.gz"), asset("Framefang.msi")];
        assert_eq!(pick_asset_url(&assets, InstallChannel::AppImage), None);
    }

    #[test]
    fn self_install_is_gated_on_the_asset_shape() {
        assert!(asset_supports_self_install(
            "https://example.com/Framefang.msi",
            InstallChannel::WindowsMsi
        ));
        // MSI-installed but the release only shipped the portable zip:
        // fall back to a browser download rather than hand-swapping the
        // .exe out from under the installer's bookkeeping.
        assert!(!asset_supports_self_install(
            "https://example.com/framefang-windows-x86_64.zip",
            InstallChannel::WindowsMsi
        ));
        assert!(asset_supports_self_install(
            "https://example.com/framefang-x86_64.AppImage?t=1",
            InstallChannel::AppImage
        ));
        assert!(!asset_supports_self_install(
            "https://example.com/framefang-linux-x86_64.tar.gz",
            InstallChannel::AppImage
        ));
        assert_eq!(
            asset_supports_self_install(
                "https://example.com/framefang-linux-x86_64.tar.gz",
                InstallChannel::Portable
            ),
            cfg!(target_os = "linux")
        );
        // Package-managed channels never self-install, whatever the asset.
        for channel in [
            InstallChannel::Snap,
            InstallChannel::Flatpak,
            InstallChannel::HomebrewCask,
            InstallChannel::LinuxSystemPackage,
            InstallChannel::Scoop,
        ] {
            assert!(!asset_supports_self_install(
                "https://example.com/framefang-linux-x86_64.tar.gz",
                channel
            ));
        }
    }

    #[test]
    fn strip_inline_rewrites_markdown_link() {
        let out = strip_inline("see [the release](https://github.com/a/b/releases) now");
        assert_eq!(out, "see the release (https://github.com/a/b/releases) now");
    }
}
