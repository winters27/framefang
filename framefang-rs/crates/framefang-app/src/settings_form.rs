//! Real Framefang — Settings window.
//!
//! Borderless winit + softbuffer + the in-tree bitmap font. Same lifecycle
//! pattern as `PinView` / `Overlay`: TrayApp owns an `Option<SettingsView>`,
//! dispatches `WindowEvent`s by `WindowId`, and polls `outcome` after each
//! event to learn when the user saved or cancelled.
//!
//! Layout — three grouped sections, all keyboard- and mouse-driven:
//!
//!   CAPTURES      Screenshots folder (path display + Browse…)
//!                 Recordings folder  (path display + Browse…)
//!                 Copy screenshots automatically (toggle pill)
//!                   Close the editor after copying (toggle pill, nested)
//!   HOTKEYS       Capture region     (live binding + REBIND button →
//!                                      capture next keypress)
//!                 Capture full screen (same, plus CLEAR — optional)
//!                 Record screen       (same, plus CLEAR — optional)
//!                 The system's own screenshot key (button to its page)
//!   APPEARANCE    Theme   (cycles Light / Dark)
//!                 Start with system (toggle pill)
//!
//! The "Edit as JSON" button in the action bar still opens settings.json in
//! the user's default editor — useful as an escape hatch when the rebind
//! widget doesn't know a particular key (the supported set covers letters,
//! digits, F1–F12, arrows, navigation cluster, PrintScreen, ScrollLock,
//! Pause).

use std::num::NonZeroU32;
use std::path::PathBuf;
use std::rc::Rc;

use anyhow::{anyhow, Result};
use softbuffer::{Context, Surface};
use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::event_loop::ActiveEventLoop;
use winit::keyboard::{Key, KeyCode, ModifiersState, NamedKey, PhysicalKey};
use winit::window::{CursorIcon, Window, WindowAttributes, WindowId};

use framefang_core::{AppSettings, Hotkey, Modifiers as HkMods};
use framefang_core::color::Rgba as FramefangRgba;
use framefang_core::hotkeys::{conflict_message, HotkeyAction};
use framefang_core::settings::{vk_label, SwitchKey};

use crate::bitmap_font;
use crate::painter;

// Colors: the app's one palette (cyan and magenta on blue-black).
#[allow(unused_imports)]
use crate::palette::*;

// ── geometry ────────────────────────────────────────────────────────────────
const WIN_W: u32 = 640;
// Two extra hotkey rows over the single-binding layout this dialog started
// with, each ROW_H plus the 8 px inter-row gap. The bottom margin is a
// padding band rather than the taller footer strip the convert dialogs
// reserve: the only message this dialog shows inline is the hotkey-conflict
// one, and that renders under the HOTKEYS block next to the rows it is
// about. The window doesn't scroll. Its height follows its rows
// (`content_height`); with the Windows Print Screen card and the
// After-a-capture row it needs an 808 px screen once the title bar and a
// 48 px panel are counted (768 until that row). The inter-row gap is 6 px
// rather than the 8 px the other dialogs use.
const BOTTOM_BAND: i32 = 16;
/// Windows' Print Screen guide under HOTKEYS, shown only while the capture
/// key is plain Print Screen (the only key Windows' switch affects): a card a
/// step above the page, holding a status line with the button, the page
/// path, and the switch.
const CARD_H: i32 = 88;
const CARD_PAD: i32 = 12;
const CARD_BTN_H: i32 = 24;
/// A row that refines the one above it is indented by this much and its
/// label is muted, so it reads as part of that setting.
const SUB_INDENT: i32 = 18;
const ROW_GAP: i32 = 6;
const PAD:   i32 = 22;
const ROW_H: i32 = 34;
/// The label column. Wide enough for the longest label plus the indent of a
/// refining row and a clear gap before its control (pinned by a test).
const LABEL_W: i32 = 224;
/// The window's heading, drawn at scale 2 left of the header buttons.
const TITLE: &str = "FRAME FANG // SETTINGS";
// Header buttons, right-anchored: Edit as JSON, Cancel, Save.
const SAVE_W:   i32 = 92;
const CANCEL_W: i32 = 92;
const EDIT_W:   i32 = 112;
const HEADER_BTN_GAP: i32 = 8;
/// Between the Theme row's color chips.
const THEME_CHIP_GAP: i32 = 8;
/// The After-a-capture row: "Open the editor" and "Copy only", then the
/// three switch keys, right-aligned. Chips in a group sit `CHIP_GAP` apart,
/// the two groups at least `GROUP_GAP` apart, so they read as two choices.
const MODE_CHIP_W: (i32, i32) = (110, 76);
const KEY_CHIP_W: i32 = 48;
const CHIP_GAP: i32 = 6;
const GROUP_GAP: i32 = 16;
const BTN_H: i32 = 30;
// Buttons that sit inside a hotkey row, right-anchored: REBIND on the edge,
// CLEAR to its left on the two optional actions.
const REBIND_W: i32 = 100;
const CLEAR_W:  i32 = 76;
// Header band carries the brand strip on top and the action buttons (Save /
// Cancel / Edit-as-JSON) anchored to its right side. Bigger than the body
// padding so the buttons have generous click targets.
const HEADER_H: i32 = 84;
// No footer rule anymore — the action buttons moved into the header.
const FOOTER_H: i32 = 28;

#[derive(Clone, Copy, PartialEq, Eq)]
enum WidgetKind {
    SaveFolder,
    RecordingsFolder,
    /// Where FFmpeg was found, or that it isn't installed (a label and a
    /// read-out; its button is `FfmpegDownload`).
    Ffmpeg,
    /// Download Framefang's FFmpeg. Live only on Windows while none is
    /// found and no download is running.
    FfmpegDownload,
    /// Read-out box for one action's binding.
    HotkeyDisplay(HotkeyAction),
    /// "REBIND" button: arms capture of the next keypress for that action.
    HotkeyRebind(HotkeyAction),
    /// "CLEAR" button, only laid out for the optional actions.
    HotkeyClear(HotkeyAction),
    /// The Theme row's label. The chips beside it are the controls.
    Theme,
    /// One color-scheme chip. Picking it previews at once; Cancel reverts.
    ThemeChoice(crate::palette::Scheme),
    StartWithOs,
    /// The After-a-capture row's label and caption. The chips beside it are
    /// the controls.
    AfterCapture,
    /// Open the editor (`true`) or just copy (`false`) when a drag ends.
    AfterCaptureChoice(bool),
    /// The key that, held, makes one capture do the other thing.
    SwitchKeyChoice(SwitchKey),
    /// Every screenshot goes to the clipboard as it is taken.
    AutoCopy,
    /// The editor's Copy also closes the editor. Shown under AutoCopy.
    CloseAfterCopy,
    /// Screenshots show the mouse pointer. Laid out only where the platform
    /// can draw it (`framefang_platform::cursor::SUPPORTED`).
    CaptureCursor,
    OpenJson,
    SaveBtn,
    CancelBtn,
    /// Opens the system's own screenshot-shortcut page. Laid out only where
    /// `system_shortcuts::page()` has one.
    SystemShortcut,
}

/// The line under HOTKEYS: the system page's button, and whether the system
/// is taking the capture binding right now.
const SYSTEM_BTN_H: i32 = 20;

struct Row {
    kind:  WidgetKind,
    label: &'static str,
    rect:  (i32, i32, i32, i32),
}

pub enum SettingsOutcome {
    /// User clicked [Save]; tray loop applies + persists.
    Saved(AppSettings),
    /// User clicked [Cancel] or hit Esc.
    Cancelled,
    /// User clicked [Edit as JSON]; tray loop should shell-open the config
    /// path. The view stays open so subsequent edits land in the draft when
    /// the user re-opens it.
    OpenJson,
}

pub struct SettingsView {
    window:  Rc<Window>,
    _ctx:    Context<Rc<Window>>,
    surface: Surface<Rc<Window>, Rc<Window>>,
    draft:   AppSettings,
    rows:    Vec<Row>,
    cursor:  (i32, i32),
    hover:   Option<usize>,
    /// The action whose row is in "press a key" capture mode, if any. The
    /// next non-modifier `KeyboardInput::Pressed` event commits its binding;
    /// `Esc` cancels and reverts to `hotkey_before_capture`.
    capturing_hotkey: Option<HotkeyAction>,
    /// Snapshot of the binding the captured action had before the user
    /// pressed REBIND, so Esc can roll back the draft without disk
    /// persistence. `None` when that action was unbound.
    hotkey_before_capture: Option<Hotkey>,
    /// Latest live modifier state, tracked via `WindowEvent::ModifiersChanged`.
    /// The `KeyboardInput` event doesn't carry modifier info on its own.
    mods_state: ModifiersState,
    /// Inline validation message drawn under the HOTKEYS block, in the same
    /// tint the convert dialogs use for failures. Set when Save is refused
    /// because two actions share a chord; cleared by the next rebind or
    /// clear.
    error:   Option<String>,
    /// The system's screenshot-shortcut page, if this platform has one.
    system_page: Option<crate::system_shortcuts::Page>,
    /// The system takes the draft's capture binding (Windows' Print Screen
    /// going to the Snipping Tool). Re-read whenever the window regains
    /// focus, so it clears as soon as the user comes back from the page.
    system_takes_capture: bool,
    /// Shown in the listening hotkey box in place of "[ PRESS A KEY ]" when
    /// the last key pressed can't be used alone. In the box, so nothing
    /// else on the page moves while the rebind is listening.
    capture_hint: Option<String>,
    /// The Windows Print Screen card is laid out (Windows, capture key is
    /// plain Print Screen). `rows` and the window height follow it.
    guide_shown: bool,
    /// The switch's name as this Windows shows it, and the page it is on.
    switch_name: String,
    switch_page: &'static str,
    /// The FFmpeg recording and conversion will use, if any.
    ffmpeg_found: Option<PathBuf>,
    /// Opened because something needed FFmpeg: the row is lit until the
    /// download starts.
    ffmpeg_attention: bool,
    /// The download state last drawn, so `tick` redraws only on change.
    fetch_seen: crate::ffmpeg_fetch::Fetch,
    /// The title strip drawn above the dialog (minimize, close, drag).
    title_bar: crate::chrome::TitleBar,
    pub outcome: Option<SettingsOutcome>,
}

/// The FFmpeg row's button.
const FFMPEG_BTN_W: i32 = 104;

impl SettingsView {
    /// `ffmpeg_attention`: opened because recording or conversion needed
    /// FFmpeg and found none, so its row starts lit.
    pub fn new(loop_target: &ActiveEventLoop, current: AppSettings, ffmpeg_attention: bool) -> Result<Self> {
        let system_page = crate::system_shortcuts::page();
        let guide_shown = shows_windows_guide(system_page, &current);
        let rows = Self::build_rows(system_page, guide_shown);
        let win_h = content_height(&rows) + crate::chrome::TITLE_H as u32;
        let (cx, cy) = centered_origin(loop_target, WIN_W, win_h);
        let attrs = crate::chrome::frameless(WindowAttributes::default())
            .with_title("Frame Fang — Settings")
            .with_resizable(false)
            .with_inner_size(PhysicalSize::new(WIN_W, win_h))
            .with_position(PhysicalPosition::new(cx, cy))
            .with_window_icon(crate::brand_icon::shared());

        let window = loop_target
            .create_window(attrs)
            .map(Rc::new)
            .map_err(|e| anyhow!("create_window (settings): {e}"))?;
        crate::chrome::finish(&window);

        // Framefang runs as a menu-bar/tray agent, so on macOS there is no Dock
        // icon to click if this opens behind the frontmost app. Ask for focus
        // explicitly; on Windows/Linux it is a harmless raise.
        window.focus_window();
        window.set_cursor(CursorIcon::Default);
        let ctx = Context::new(window.clone())
            .map_err(|e| anyhow!("softbuffer Context::new (settings): {e}"))?;
        let surface = Surface::new(&ctx, window.clone())
            .map_err(|e| anyhow!("softbuffer Surface::new (settings): {e}"))?;

        let mut me = SettingsView {
            window, _ctx: ctx, surface,
            draft: current,
            rows,
            guide_shown,
            cursor: (0, 0),
            hover: None,
            capturing_hotkey: None,
            hotkey_before_capture: None,
            mods_state: ModifiersState::empty(),
            error: None,
            system_page,
            system_takes_capture: false,
            capture_hint: None,
            switch_name: crate::system_shortcuts::switch_name(),
            switch_page: crate::system_shortcuts::windows_page_path(),
            ffmpeg_found: crate::convert_video_form::locate_ffmpeg(),
            ffmpeg_attention,
            fetch_seen: crate::ffmpeg_fetch::state(),
            title_bar: Default::default(),
            outcome: None,
        };
        me.refresh_system_takes();
        me.redraw();
        Ok(me)
    }

    pub fn window_id(&self) -> WindowId { self.window.id() }

    /// Called from the tray loop's poll: redraw when the FFmpeg download
    /// moved on, and look for FFmpeg again once it has finished.
    pub fn tick(&mut self) {
        let now = crate::ffmpeg_fetch::state();
        if now == self.fetch_seen { return; }
        if now == crate::ffmpeg_fetch::Fetch::Done {
            self.ffmpeg_found = crate::convert_video_form::locate_ffmpeg();
        }
        self.fetch_seen = now;
        self.window.request_redraw();
    }

    /// Something needed FFmpeg while Settings was already open: light the row.
    pub fn draw_attention_to_ffmpeg(&mut self) {
        self.ffmpeg_found = crate::convert_video_form::locate_ffmpeg();
        self.ffmpeg_attention = self.ffmpeg_found.is_none();
        self.window.focus_window();
        self.window.request_redraw();
    }

    /// The Download button can be pressed: Windows, no FFmpeg, nothing
    /// downloading.
    fn ffmpeg_actionable(&self) -> bool {
        crate::ffmpeg_fetch::offered() && self.ffmpeg_found.is_none() && !crate::ffmpeg_fetch::busy()
    }

    fn refresh_system_takes(&mut self) {
        let takes = self.draft.hotkey_for(HotkeyAction::Capture)
            .is_some_and(|hk| crate::system_shortcuts::system_takes(&hk));
        if takes != self.system_takes_capture {
            self.system_takes_capture = takes;
            self.window.request_redraw();
        }
    }

    /// The capture key changed: show or hide the Windows card to match, and
    /// fit the window to the rows again.
    fn relayout(&mut self) {
        self.refresh_system_takes();
        let guide = shows_windows_guide(self.system_page, &self.draft);
        if guide == self.guide_shown { return; }
        self.guide_shown = guide;
        self.rows = Self::build_rows(self.system_page, guide);
        self.hover = None;
        let h = content_height(&self.rows) + crate::chrome::TITLE_H as u32;
        let _ = self.window.request_inner_size(PhysicalSize::new(WIN_W, h));
        self.window.request_redraw();
    }

    pub fn handle_event(&mut self, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => {
                self.outcome = Some(SettingsOutcome::Cancelled);
            }
            // Back from the system page: the setting may have changed.
            WindowEvent::Focused(true) => {
                self.refresh_system_takes();
                // FFmpeg may have been installed meanwhile.
                let found = crate::convert_video_form::locate_ffmpeg();
                if found != self.ffmpeg_found {
                    self.ffmpeg_found = found;
                    self.window.request_redraw();
                }
            }
            WindowEvent::ModifiersChanged(mods) => {
                // Track the live modifier mask so the rebind capture step can
                // pair `Ctrl+Shift+P` together — `KeyboardInput` on its own
                // doesn't carry modifier info in winit 0.30.
                self.mods_state = mods.state();
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
                let want_cursor = if new_hover.is_some() { CursorIcon::Pointer } else { CursorIcon::Default };
                self.window.set_cursor(want_cursor);
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
                    self.activate(i);
                } else {
                    crate::chrome::press_off_controls(&self.window, self.cursor.1, HEADER_H);
                }
            }
            WindowEvent::KeyboardInput { event: key_ev, .. } => {
                if self.capturing_hotkey.is_some() {
                    self.on_capture_key(key_ev);
                } else {
                    self.on_key(key_ev);
                }
            }
            WindowEvent::Resized(_) => {
                // The card came or went: on Linux/X11 the rounded cut has to
                // follow the new size (elsewhere this is a no-op re-apply).
                crate::chrome::finish(&self.window);
                self.redraw();
            }
            WindowEvent::RedrawRequested => self.redraw(),
            _ => {}
        }
    }

    fn on_key(&mut self, ev: winit::event::KeyEvent) {
        if ev.state != ElementState::Pressed { return; }
        // Esc cancels, Enter saves.
        match ev.logical_key {
            Key::Named(NamedKey::Escape) => self.outcome = Some(SettingsOutcome::Cancelled),
            Key::Named(NamedKey::Enter)  => self.try_save(),
            _ => {}
        }
    }

    /// Hotkey-capture key handler. Active only while `capturing_hotkey` is
    /// set. Eats every key event so they don't fall through into the regular
    /// `on_key` text-input path.
    ///
    /// Behavior:
    /// - `Esc` cancels capture and rolls back to `hotkey_before_capture`.
    /// - A bare modifier press (Ctrl / Shift / Alt / Super) is ignored —
    ///   we want a real key alongside the modifiers.
    /// - Any supported `KeyCode` commits the binding using the live
    ///   `mods_state` mask. The draft is mutated; persistence waits for the
    ///   Save button (or rolls back on Cancel).
    /// - Unsupported keys ignore the press; the user can hit `Esc` and use
    ///   the "Edit as JSON" escape hatch for exotic keys.
    fn on_capture_key(&mut self, ev: winit::event::KeyEvent) {
        if ev.state != ElementState::Pressed { return; }
        let Some(action) = self.capturing_hotkey else { return; };
        // Esc always aborts capture and reverts the draft.
        if matches!(ev.logical_key, Key::Named(NamedKey::Escape)) {
            self.draft.set_hotkey_for(action, self.hotkey_before_capture);
            self.capturing_hotkey = None;
            self.capture_hint = None;
            self.relayout();
            self.window.request_redraw();
            return;
        }
        let vk = match ev.physical_key {
            PhysicalKey::Code(code) => {
                // Bare modifier press → keep waiting for a non-modifier.
                if is_modifier_code(code) { return; }
                match keycode_to_vk(code) {
                    Some(vk) => vk,
                    // Named but unmapped: try what Windows calls it.
                    None => match native_vk(ev.physical_key) { Some(vk) => vk, None => return },
                }
            }
            // A key winit can't name (some media and OEM keys): Windows still can.
            other => match native_vk(other) { Some(vk) => vk, None => return },
        };
        let modifiers = mods_state_to_hk(self.mods_state);
        let hk = Hotkey { modifiers, virtual_key: vk };
        // A typing key (alone, or with only Shift) would stop that key typing
        // in every app; keep listening and say what to hold with it.
        if framefang_core::settings::blocks_typing(&hk) {
            self.capture_hint = Some(typing_key_hint(vk));
            self.window.request_redraw();
            return;
        }
        self.capture_hint = None;
        self.draft.set_hotkey_for(action, Some(hk));
        self.capturing_hotkey = None;
        // The binding just changed, so whatever the last Save complained
        // about may well be resolved — let the next Save re-check instead of
        // leaving a stale message on screen.
        self.error = None;
        // Print Screen in or out: the Windows card comes or goes with it.
        self.relayout();
        self.window.request_redraw();
    }

    /// Commit the draft, unless two actions are bound to the same chord.
    ///
    /// A duplicate is not something the OS can arbitrate for us: on Windows
    /// the second `RegisterHotKey` just fails, and under X11 both grabs land
    /// on one key so whichever binding we happen to match first silently
    /// wins. Refusing the save and naming both rows is the only outcome the
    /// user can act on, so the dialog stays open with the message in the
    /// footer.
    fn try_save(&mut self) {
        if let Some((a, b)) = self.draft.hotkey_conflict() {
            let chord = self.draft.hotkey_for(a).unwrap_or_default();
            self.error = Some(conflict_message(a, b, chord));
            self.window.request_redraw();
            return;
        }
        self.error = None;
        self.outcome = Some(SettingsOutcome::Saved(self.draft.clone()));
    }

    fn hit_test(&self, x: i32, y: i32) -> Option<usize> {
        // Reverse so action-bar buttons on top of any overlapping layout win.
        self.rows.iter().enumerate().rev().find_map(|(i, r)| {
            // A hotkey error takes the system line's place, button included.
            if r.kind == WidgetKind::SystemShortcut && self.error.is_some() { return None; }
            // Only a label; its chips are the controls.
            if matches!(r.kind, WidgetKind::Theme | WidgetKind::AfterCapture) { return None; }
            // A read-out, and a button that is live only when there is
            // something to download.
            if r.kind == WidgetKind::Ffmpeg { return None; }
            if r.kind == WidgetKind::FfmpegDownload && !self.ffmpeg_actionable() { return None; }
            let (rx, ry, rw, rh) = r.rect;
            if x >= rx && x < rx + rw && y >= ry && y < ry + rh { Some(i) } else { None }
        })
    }

    fn activate(&mut self, idx: usize) {
        let kind = self.rows[idx].kind;
        match kind {
            WidgetKind::Ffmpeg => {}
            WidgetKind::FfmpegDownload => {
                crate::ffmpeg_fetch::start();
                self.ffmpeg_attention = false;
            }
            WidgetKind::SaveFolder => {
                let starting = if self.draft.save_directory.is_empty() {
                    directories::UserDirs::new()
                        .and_then(|u| u.picture_dir().map(|p| p.to_path_buf()))
                        .unwrap_or_else(std::env::temp_dir)
                } else {
                    PathBuf::from(&self.draft.save_directory)
                };
                if let Some(p) = rfd::FileDialog::new()
                    .set_title("Frame Fang — Screenshots folder")
                    .set_directory(&starting)
                    .pick_folder()
                {
                    self.draft.save_directory = p.to_string_lossy().to_string();
                }
            }
            WidgetKind::RecordingsFolder => {
                let starting = if self.draft.recordings_directory.is_empty() {
                    directories::UserDirs::new()
                        .and_then(|u| u.video_dir().map(|p| p.to_path_buf()))
                        .unwrap_or_else(std::env::temp_dir)
                } else {
                    PathBuf::from(&self.draft.recordings_directory)
                };
                if let Some(p) = rfd::FileDialog::new()
                    .set_title("Frame Fang — Recordings folder")
                    .set_directory(&starting)
                    .pick_folder()
                {
                    self.draft.recordings_directory = p.to_string_lossy().to_string();
                }
            }
            WidgetKind::Theme => {}
            WidgetKind::ThemeChoice(scheme) => {
                self.draft.theme = scheme.name().to_owned();
                // The whole window redraws in it now, so the choice is seen
                // before it is saved.
                crate::palette::set(scheme);
            }
            WidgetKind::AfterCapture => {}
            WidgetKind::AfterCaptureChoice(editor) => {
                self.draft.open_editor = editor;
            }
            WidgetKind::SwitchKeyChoice(key) => {
                self.draft.switch_key = key.name().to_owned();
            }
            WidgetKind::AutoCopy => {
                self.draft.auto_copy = !self.draft.auto_copy;
            }
            WidgetKind::CloseAfterCopy => {
                self.draft.close_after_copy = !self.draft.close_after_copy;
            }
            WidgetKind::CaptureCursor => {
                self.draft.capture_cursor = !self.draft.capture_cursor;
            }
            WidgetKind::StartWithOs => {
                self.draft.start_with_windows = !self.draft.start_with_windows;
            }
            WidgetKind::HotkeyDisplay(_) => {
                // Clicking the display box itself is a no-op — the user has
                // to hit REBIND. Avoids accidental re-capture from a stray
                // click on the green border.
            }
            WidgetKind::HotkeyRebind(action) => {
                // Snapshot the current binding so Esc during capture can
                // roll back. Tray-loop has already unregistered every global
                // hotkey while Settings is open, so the user can press the
                // current PrintScreen freely without it firing.
                self.hotkey_before_capture = self.draft.hotkey_for(action);
                self.capturing_hotkey = Some(action);
                self.capture_hint = None;
            }
            WidgetKind::HotkeyClear(action) => {
                // Only laid out for the optional actions, so this can never
                // strip the mandatory capture binding.
                self.draft.set_hotkey_for(action, None);
                self.capturing_hotkey = None;
                self.error = None;
            }
            WidgetKind::OpenJson => {
                self.outcome = Some(SettingsOutcome::OpenJson);
                return;
            }
            WidgetKind::SaveBtn => {
                self.try_save();
                return;
            }
            WidgetKind::CancelBtn => {
                self.outcome = Some(SettingsOutcome::Cancelled);
                return;
            }
            WidgetKind::SystemShortcut => {
                if let Some(page) = self.system_page {
                    if let Err(e) = page.open() {
                        self.error = Some(e);
                    }
                }
            }
        }
        self.window.request_redraw();
    }

    /// Lay out section headers + every row. Sections are spaced apart with
    /// a single section-rule line between them. Row rect spans the full
    /// content width; the value half is computed inside `render_row`.
    ///
    /// Action buttons (Save / Cancel / Edit as JSON) are pinned to the
    /// right side of the header band at the very top — clicking them is
    /// the user's commit/abort action so they want to be reachable
    /// without scrolling, like a real app's toolbar.
    fn build_rows(system_page: Option<crate::system_shortcuts::Page>, windows_guide: bool) -> Vec<Row> {
        let mut rows = Vec::new();
        let x = PAD;
        let row_w = WIN_W as i32 - PAD * 2;

        // Header action bar — Edit-as-JSON sits leftmost on the right
        // side, then Cancel, then the primary Save button. Sized to their
        // words rather than generously, so the heading on the left has room.
        let header_btn_y = (HEADER_H - BTN_H) / 2 + 4;
        let save_x   = WIN_W as i32 - PAD - SAVE_W;
        let cancel_x = save_x   - HEADER_BTN_GAP - CANCEL_W;
        let edit_x   = cancel_x - HEADER_BTN_GAP - EDIT_W;
        rows.push(Row { kind: WidgetKind::OpenJson,  label: "Edit as JSON", rect: (edit_x,   header_btn_y, EDIT_W,   BTN_H) });
        rows.push(Row { kind: WidgetKind::CancelBtn, label: "Cancel",       rect: (cancel_x, header_btn_y, CANCEL_W, BTN_H) });
        rows.push(Row { kind: WidgetKind::SaveBtn,   label: "Save",         rect: (save_x,   header_btn_y, SAVE_W,   BTN_H) });

        // Content rows start below the header band + first section title.
        let mut y = HEADER_H + 14 + 18;

        // CAPTURES: where they go, and whether they also go to the clipboard.
        rows.push(Row { kind: WidgetKind::SaveFolder,       label: "Screenshots folder", rect: (x, y, row_w, ROW_H) }); y += ROW_H + ROW_GAP;
        rows.push(Row { kind: WidgetKind::RecordingsFolder, label: "Recordings folder",  rect: (x, y, row_w, ROW_H) }); y += ROW_H + ROW_GAP;
        // FFmpeg, which recording and video conversion run: where it is, or
        // a Download button (`render_ffmpeg_row`).
        rows.push(Row { kind: WidgetKind::Ffmpeg, label: "FFmpeg", rect: (x, y, row_w, ROW_H) });
        rows.push(Row { kind: WidgetKind::FfmpegDownload, label: "DOWNLOAD",
                        rect: (x + row_w - 4 - FFMPEG_BTN_W, y + 4, FFMPEG_BTN_W, ROW_H - 8) });
        y += ROW_H + ROW_GAP;
        // After a capture: two choices, then the key that flips one capture
        // to the other, each a group of chips across the value column.
        rows.push(Row { kind: WidgetKind::AfterCapture, label: "After a capture", rect: (x, y, row_w, ROW_H) });
        let (chip_y, chip_h) = (y + 4, ROW_H - 8);
        let mut cx = x + LABEL_W;
        for (editor, label, w) in [(true, "Open the editor", MODE_CHIP_W.0), (false, "Copy only", MODE_CHIP_W.1)] {
            rows.push(Row { kind: WidgetKind::AfterCaptureChoice(editor), label, rect: (cx, chip_y, w, chip_h) });
            cx += w + CHIP_GAP;
        }
        // The keys end where the fields and buttons above and below end.
        let keys_w = KEY_CHIP_W * SwitchKey::ALL.len() as i32 + CHIP_GAP * (SwitchKey::ALL.len() as i32 - 1);
        cx = x + row_w - 4 - keys_w;
        for key in SwitchKey::ALL {
            rows.push(Row { kind: WidgetKind::SwitchKeyChoice(key), label: key.label(), rect: (cx, chip_y, KEY_CHIP_W, chip_h) });
            cx += KEY_CHIP_W + CHIP_GAP;
        }
        y += ROW_H + ROW_GAP;
        // What goes into the picture, before where it goes.
        if framefang_platform::cursor::SUPPORTED {
            rows.push(Row { kind: WidgetKind::CaptureCursor, label: "Include the mouse pointer", rect: (x, y, row_w, ROW_H) }); y += ROW_H + ROW_GAP;
        }
        rows.push(Row { kind: WidgetKind::AutoCopy,        label: "Copy screenshots automatically", rect: (x, y, row_w, ROW_H) }); y += ROW_H + ROW_GAP;
        rows.push(Row { kind: WidgetKind::CloseAfterCopy,   label: "Close the editor after copying", rect: (x, y, row_w, ROW_H) }); y += ROW_H + 22;

        // HOTKEYS — one row per action: a display box filling the left part
        // of the value column, REBIND anchored to the right edge, and CLEAR
        // just inside it on the two optional actions. The buttons are their
        // own rows laid out over the display row's rect; `hit_test` scans in
        // reverse, so they win the click.
        y += 18;
        for action in HotkeyAction::ALL {
            rows.push(Row { kind:  WidgetKind::HotkeyDisplay(action),
                            label: action.label(),
                            rect:  (x, y, row_w, ROW_H) });
            let rebind_x = x + row_w - REBIND_W;
            rows.push(Row { kind:  WidgetKind::HotkeyRebind(action),
                            label: "REBIND",
                            rect:  (rebind_x, y, REBIND_W, ROW_H) });
            if action.is_optional() {
                rows.push(Row { kind:  WidgetKind::HotkeyClear(action),
                                label: "CLEAR",
                                rect:  (rebind_x - 8 - CLEAR_W, y, CLEAR_W, ROW_H) });
            }
            y += ROW_H + ROW_GAP;
        }
        // Taller than the usual inter-section gap: the system-shortcut line
        // (or, in its place, the hotkey-conflict message) renders in here,
        // between the last binding row and the APPEARANCE header.
        match system_page {
            // Windows, capture key isn't Print Screen: Windows' switch
            // doesn't touch it, so no card; the next heading sits as far
            // below the last row as the other headings do.
            Some(page) if page.is_windows() && !windows_guide => y += 16,
            Some(page) if page.is_windows() => {
                // The guide card; its button sits in the card's top-right.
                let w = bitmap_font::measure(page.button, 1) + 24;
                // Right under the last hotkey row, a row gap away, so the
                // dialog still fits a 768 px screen with the FFmpeg row.
                let top = y;
                rows.push(Row { kind:  WidgetKind::SystemShortcut,
                                label: page.button,
                                rect:  (x + row_w - CARD_PAD - w, top + CARD_PAD, w, CARD_BTN_H) });
                // The APPEARANCE heading then sits as far below the card as
                // the other headings sit below their rows.
                y = top + CARD_H + 22;
            }
            Some(page) => {
                let w = bitmap_font::measure(page.button, 1) + 24;
                rows.push(Row { kind:  WidgetKind::SystemShortcut,
                                label: page.button,
                                rect:  (x + row_w - w, y, w, SYSTEM_BTN_H) });
                // Enough that the APPEARANCE heading clears the system line's
                // button by about what the other sections get.
                y += 44;
            }
            None => y += 44,
        }

        // APPEARANCE
        y += 18;
        rows.push(Row { kind: WidgetKind::Theme, label: "Theme", rect: (x, y, row_w, ROW_H) });
        // One chip per scheme across the value column, a swatch and a word.
        let chips = crate::palette::Scheme::ALL;
        let chip_w = (row_w - LABEL_W - 4 - THEME_CHIP_GAP * (chips.len() as i32 - 1)) / chips.len() as i32;
        for (i, scheme) in chips.into_iter().enumerate() {
            let cx = x + LABEL_W + i as i32 * (chip_w + THEME_CHIP_GAP);
            rows.push(Row { kind: WidgetKind::ThemeChoice(scheme), label: scheme.name(),
                            rect: (cx, y + 4, chip_w, ROW_H - 8) });
        }
        y += ROW_H + ROW_GAP;
        rows.push(Row { kind: WidgetKind::StartWithOs, label: "Start with system", rect: (x, y, row_w, ROW_H) }); y += ROW_H;

        let _ = y;
        rows
    }

    fn redraw(&mut self) {
        // Read before the surface is borrowed below.
        let ffmpeg_actionable = self.ffmpeg_actionable();
        let phys = self.window.inner_size();
        let (Some(w), Some(h)) = (NonZeroU32::new(phys.width), NonZeroU32::new(phys.height))
            else { return; };
        if let Err(e) = self.surface.resize(w, h) {
            eprintln!("settings: surface.resize: {e}"); return;
        }
        let mut frame = match self.surface.buffer_mut() {
            Ok(b) => b,
            Err(e) => { eprintln!("settings: buffer_mut: {e}"); return; }
        };
        let win_w = w.get() as usize;
        // The dialog draws into its own buffer, placed below the title strip.
        let win_h = (h.get() as usize).saturating_sub(crate::chrome::TITLE_H as usize);
        let mut buf = vec![0u32; win_w * win_h];

        // Header band and body with faint scanlines, the glowing rule
        // under the header (`hud`).
        crate::hud::backdrop(&mut buf, win_w, win_h, HEADER_H);
        let _ = FOOTER_H;

        let mut surf = BufferSurface { buf: &mut buf, w: win_w as i32, h: win_h as i32 };

        // The heading, glowing.
        crate::hud::heading(surf.buf, win_w, win_h, PAD, 18, TITLE);
        // Heading and subtitle must both end before the right-anchored
        // action bar; `the_heading_and_labels_clear_their_controls` checks.
        draw_text(&mut surf, PAD, 44, 1, "Captures, hotkeys, appearance.",
                  argb_to_framefang(TEXT_MUTED()));

        // Section labels, numbered like a terminal's, anchored to their
        // first row.
        let right = WIN_W as i32 - PAD;
        let firsts = [
            self.rows.iter().find(|r| r.kind == WidgetKind::SaveFolder).map(|r| (r.rect.1, "CAPTURES")),
            self.rows.iter().find(|r| matches!(r.kind, WidgetKind::HotkeyDisplay(_))).map(|r| (r.rect.1, "HOTKEYS")),
            self.rows.iter().find(|r| r.kind == WidgetKind::Theme).map(|r| (r.rect.1, "APPEARANCE")),
        ];
        for (i, (y, name)) in firsts.into_iter().flatten().enumerate() {
            crate::hud::section(surf.buf, win_w, win_h, PAD, right, y - 22, i + 1, name);
        }

        let capturing = self.capturing_hotkey;
        for (i, row) in self.rows.iter().enumerate() {
            let hovered = self.hover == Some(i);
            if row.kind == WidgetKind::SystemShortcut {
                if self.error.is_none() {
                    if self.guide_shown {
                        let guide = WindowsGuide {
                            takes:  self.system_takes_capture,
                            switch: &self.switch_name,
                            page:   self.switch_page,
                        };
                        render_windows_card(&mut surf, row, hovered, &guide);
                    } else {
                        render_system_line(&mut surf, row, hovered, self.system_page, self.system_takes_capture);
                    }
                }
                continue;
            }
            if let WidgetKind::ThemeChoice(scheme) = row.kind {
                render_theme_chip(&mut surf, row, scheme, hovered, &self.draft);
                continue;
            }
            if matches!(row.kind, WidgetKind::AfterCaptureChoice(_) | WidgetKind::SwitchKeyChoice(_)) {
                render_choice_chip(&mut surf, row, hovered, &self.draft);
                continue;
            }
            if row.kind == WidgetKind::Ffmpeg {
                let btn = self.rows.iter().position(|r| r.kind == WidgetKind::FfmpegDownload);
                let ff = FfmpegRow {
                    found: self.ffmpeg_found.as_deref(),
                    fetch: &self.fetch_seen,
                    button: btn.filter(|_| ffmpeg_actionable).map(|b| (&self.rows[b], self.hover == Some(b))),
                    attention: self.ffmpeg_attention && self.ffmpeg_found.is_none(),
                };
                render_ffmpeg_row(&mut surf, row, &ff);
                continue;
            }
            if row.kind == WidgetKind::FfmpegDownload { continue; } // drawn with its row
            render_row(&mut surf, row, hovered, capturing, self.capture_hint.as_deref(), &self.draft);
        }

        // Inline validation, in the same danger tint the convert dialogs
        // use — but parked directly under the HOTKEYS block instead of at
        // the window's bottom edge, because that's the section it is about
        // and the gap before the APPEARANCE header is already reserved.
        if let Some(msg) = &self.error {
            if let Some(r) = self.rows.iter().rev()
                .find(|r| matches!(r.kind, WidgetKind::HotkeyDisplay(_)))
            {
                draw_text(&mut surf, PAD, r.rect.1 + ROW_H + 6, 1, msg,
                          argb_to_framefang(DANGER));
            }
        }

        self.title_bar.draw(&mut frame, win_w, &buf, "Settings", BG_TOP());
        if let Err(e) = frame.present() {
            eprintln!("settings: buf.present: {e}");
        }
    }

}

/// The line under HOTKEYS: what the button is for on the left, the button on
/// the right. When the system is taking the capture binding the line says so
/// in the warning tint and the button lights up as the thing to press.
fn render_system_line<S: painter::Surface>(
    surf: &mut S, row: &Row, hovered: bool,
    page: Option<crate::system_shortcuts::Page>, takes: bool,
) {
    let Some(page) = page else { return };
    let (rx, ry, rw, rh) = row.rect;
    let text_y = ry + (rh - bitmap_font::GLYPH_H) / 2;
    let (line, line_color) = if takes {
        ("Print Screen opens the Snipping Tool, not Frame Fang", WARN)
    } else {
        (page.line, TEXT_MUTED())
    };
    draw_text(surf, PAD + 6, text_y, 1, line, argb_to_framefang(line_color));

    let border = if takes { WARN } else if hovered { LASER_DIM() } else { FIELD_BORDER() };
    if hovered {
        fill_rect(surf, rx, ry, rw, rh, argb_to_framefang(HOVER_FILL()));
    }
    stroke_rect_argb(surf, rx, ry, rw, rh, argb_to_framefang(border));
    let tw = bitmap_font::measure(row.label, 1);
    let color = if takes { WARN } else { TEXT_BRIGHT };
    draw_text(surf, rx + (rw - tw) / 2, text_y, 1, row.label, argb_to_framefang(color));
}

/// What the FFmpeg row shows.
struct FfmpegRow<'a> {
    found: Option<&'a std::path::Path>,
    fetch: &'a crate::ffmpeg_fetch::Fetch,
    /// The Download button and whether it is hovered, when it can be pressed.
    button: Option<(&'a Row, bool)>,
    /// Lit because recording or conversion just needed FFmpeg.
    attention: bool,
}

/// The FFmpeg row: its label, a read-out field, and the Download button
/// when there is nothing to use. While downloading, the field fills from
/// the left like a progress bar.
fn render_ffmpeg_row<S: painter::Surface>(surf: &mut S, row: &Row, ff: &FfmpegRow) {
    use crate::ffmpeg_fetch::Fetch;
    let (rx, ry, rw, rh) = row.rect;
    let label_y = ry + (rh - bitmap_font::GLYPH_H) / 2;
    let label_color = if ff.attention { LASER() } else { TEXT_BRIGHT };
    draw_text(surf, rx + 6, label_y, 1, row.label, argb_to_framefang(label_color));

    let val_x = rx + LABEL_W;
    let (val_y, val_h) = (ry + 4, rh - 8);
    let right = match ff.button { Some((b, _)) => b.rect.0 - 8, None => rx + rw - 4 };
    let val_w = right - val_x;

    let border = if ff.attention { LASER() } else { FIELD_BORDER() };
    fill_rect(surf, val_x, val_y, val_w, val_h, argb_to_framefang(FIELD_BG()));
    let (text, color) = match (ff.found, ff.fetch) {
        (_, Fetch::Downloading { got, total }) => {
            // The field fills as the download comes in.
            if let Some(t) = total.filter(|t| *t > 0) {
                let filled = ((val_w as f64) * (*got as f64 / t as f64)).clamp(0.0, val_w as f64) as i32;
                fill_rect(surf, val_x, val_y, filled, val_h, argb_to_framefang(PRIMARY_FILL()));
            }
            (format!("Downloading FFmpeg  {}", crate::ffmpeg_fetch::progress_text(*got, *total)), TEXT_BRIGHT)
        }
        (_, Fetch::Installing) => ("Checking and installing...".to_owned(), TEXT_BRIGHT),
        (Some(path), _) => (path.display().to_string(), TEXT_MUTED()),
        (None, Fetch::Failed(e)) => (format!("Download failed: {e}"), DANGER),
        (None, _) if crate::ffmpeg_fetch::offered() =>
            ("Not installed. Recording and video conversion need it.".to_owned(), TEXT_MUTED()),
        (None, _) => (crate::ffmpeg_fetch::missing_message().to_owned(), TEXT_MUTED()),
    };
    stroke_rect_argb(surf, val_x, val_y, val_w, val_h, argb_to_framefang(border));
    let shown = truncate_for(&text, val_w - 16);
    draw_text(surf, val_x + 8, val_y + (val_h - bitmap_font::GLYPH_H) / 2, 1, &shown, argb_to_framefang(color));

    if let Some((b, hovered)) = ff.button {
        let (bx, by, bw, bh) = b.rect;
        if hovered || ff.attention { fill_rect(surf, bx, by, bw, bh, argb_to_framefang(PRIMARY_FILL())); }
        stroke_rect_argb(surf, bx, by, bw, bh, argb_to_framefang(LASER()));
        let label = if matches!(ff.fetch, Fetch::Failed(_)) { "RETRY" } else { b.label };
        let tw = bitmap_font::measure(label, 1);
        draw_text(surf, bx + (bw - tw) / 2, by + (bh - bitmap_font::GLYPH_H) / 2, 1, label, argb_to_framefang(LASER()));
    }
}

/// What the Windows guide card shows. It exists only while the capture key
/// is plain Print Screen, so it is always about that key.
struct WindowsGuide<'a> {
    /// Windows' switch is on, so it is taking Print Screen.
    takes: bool,
    /// The switch's name as this Windows shows it, and its page.
    switch: &'a str,
    page: &'static str,
}

fn is_plain_print_screen(hk: &Hotkey) -> bool {
    hk.virtual_key == 0x2C && hk.modifiers.is_empty()
}

/// Whether Settings shows the Windows Print Screen card: only on Windows,
/// and only while the capture key is plain Print Screen, the one key the
/// switch affects. macOS and Linux have no such switch.
fn shows_windows_guide(page: Option<crate::system_shortcuts::Page>, draft: &AppSettings) -> bool {
    cfg!(windows)
        && page.is_some_and(|p| p.is_windows())
        && draft.hotkey_for(HotkeyAction::Capture).is_some_and(|hk| is_plain_print_screen(&hk))
}

/// The window's content height for `rows`: down to the lowest row, plus the
/// bottom band.
fn content_height(rows: &[Row]) -> u32 {
    let bottom = rows.iter().map(|r| r.rect.1 + r.rect.3).max().unwrap_or(0);
    (bottom + BOTTOM_BAND).max(0) as u32
}

/// The status line, the page line and the color of the card's edge for `g`.
/// Both say plainly that the switch is about using Print Screen here.
fn guide_text(g: &WindowsGuide) -> (String, String, u32) {
    if g.takes {
        (
            "Windows is using Print Screen for its own capture".to_owned(),
            format!("To use Print Screen here, turn this off in {}:", g.page),
            WARN,
        )
    } else {
        (
            "Print Screen goes straight to Frame Fang".to_owned(),
            format!("Keep this off in {} for Print Screen to work:", g.page),
            LASER(),
        )
    }
}

/// Windows' Print Screen guide: a card a step above the page. Its edge and
/// dot say the state at a glance; the first line says it in words beside
/// the button; the second names the Settings page; the third shows the
/// switch itself (a small on/off drawn like Windows' own, then its exact
/// name in the smooth font, since those are Windows' words, not ours).
fn render_windows_card<S: painter::Surface>(surf: &mut S, btn: &Row, hovered: bool, g: &WindowsGuide) {
    let (bx, by, bw, bh) = btn.rect;
    let (cx, cy) = (PAD, by - CARD_PAD);
    let cw = WIN_W as i32 - 2 * PAD;
    let (headline, page_line, status) = guide_text(g);

    // Surface, hairline, and a status edge down the left.
    fill_rect(surf, cx, cy, cw, CARD_H, argb_to_framefang(BG_TOP()));
    stroke_rect_argb(surf, cx, cy, cw, CARD_H, argb_to_framefang(FIELD_BORDER()));
    fill_rect(surf, cx, cy, 3, CARD_H, argb_to_framefang(status));

    // Line 1: a status dot and the state, level with the button.
    let tx = cx + 16;
    let line1 = by + (bh - bitmap_font::GLYPH_H) / 2;
    fill_rect(surf, tx, line1 + 1, 5, 5, argb_to_framefang(status));
    draw_text(surf, tx + 12, line1, 1, &headline, argb_to_framefang(TEXT_BRIGHT));

    // The button: the thing to press while Windows has the key.
    let primary = g.takes;
    let border = if primary { LASER() } else if hovered { LASER_DIM() } else { FIELD_BORDER() };
    if hovered { fill_rect(surf, bx, by, bw, bh, argb_to_framefang(HOVER_FILL())); }
    stroke_rect_argb(surf, bx, by, bw, bh, argb_to_framefang(border));
    let lw = bitmap_font::measure(btn.label, 1);
    draw_text(surf, bx + (bw - lw) / 2, line1, 1, btn.label,
              argb_to_framefang(if primary { LASER() } else { TEXT_BRIGHT }));

    // Line 2: where the switch is.
    let line2 = by + bh + 10;
    draw_text(surf, tx + 12, line2, 1, &page_line, argb_to_framefang(TEXT_MUTED()));

    // Line 3: the switch, drawn, then its name.
    let row3 = line2 + bitmap_font::GLYPH_H + 8;
    let mut x = tx + 12;
    if g.takes {
        // On now, off is the goal.
        mini_switch(surf, x, row3 + 1, true, status);
        x += MINI_SWITCH_W + 6;
        draw_text(surf, x, row3 + 4, 1, "->", argb_to_framefang(TEXT_MUTED()));
        x += bitmap_font::measure("->", 1) + 6;
        mini_switch(surf, x, row3 + 1, false, TEXT_BRIGHT);
    } else {
        // Off, as it should stay.
        mini_switch(surf, x, row3 + 1, false, status);
    }
    x += MINI_SWITCH_W + 10;
    let name = format!("\u{201C}{}\u{201D}", g.switch);
    // Windows' wording in another language can run long: shrink to fit.
    let room = (cx + cw - CARD_PAD - x) as f32;
    let mut px = 13.0_f32;
    let w = framefang_core::text::layout(&name, px).width();
    if w > room { px = (px * room / w).max(9.0); }
    let ty = row3 as f32 + (16.0 - framefang_core::text::line_height(px)) / 2.0;
    painter::draw_unicode_text(surf, x as f32, ty, px, &name, argb_to_framefang(TEXT_BRIGHT));
}

const MINI_SWITCH_W: i32 = 26;

/// A small on/off switch in the app's square style: filled with the knob at
/// the right when on, an outline with the knob at the left when off.
fn mini_switch<S: painter::Surface>(surf: &mut S, x: i32, y: i32, on: bool, color: u32) {
    let h = 14;
    let knob = h - 6;
    if on {
        fill_rect(surf, x, y, MINI_SWITCH_W, h, argb_to_framefang(color));
        fill_rect(surf, x + MINI_SWITCH_W - 3 - knob, y + 3, knob, knob, argb_to_framefang(BG_BODY()));
    } else {
        stroke_rect_argb(surf, x, y, MINI_SWITCH_W, h, argb_to_framefang(color));
        fill_rect(surf, x + 3, y + 3, knob, knob, argb_to_framefang(color));
    }
}


fn render_row<S: painter::Surface>(
    surf: &mut S, row: &Row,
    hovered: bool,
    capturing_hotkey: Option<HotkeyAction>,
    // What the listening box says instead of "[ PRESS A KEY ]", if anything.
    capture_hint: Option<&str>,
    draft: &AppSettings,
) {
    let (rx, ry, rw, rh) = row.rect;

    // ── Action-bar buttons ─────────────────────────────────────────────────
    if matches!(row.kind, WidgetKind::SaveBtn | WidgetKind::CancelBtn | WidgetKind::OpenJson) {
        let is_primary = row.kind == WidgetKind::SaveBtn;
        let border = if is_primary { LASER() } else if hovered { LASER_DIM() } else { PANEL_BORDER() };
        let fill   = if is_primary && hovered { PRIMARY_FILL() } else if hovered { HOVER_FILL() } else { 0x0000_0000 };
        if fill != 0 {
            fill_rect(surf, rx, ry, rw, rh, argb_to_framefang(fill));
        }
        stroke_rect_argb(surf, rx, ry, rw, rh, argb_to_framefang(border));
        let tw = bitmap_font::measure(row.label, 1);
        let tx = rx + (rw - tw) / 2;
        let ty = ry + (rh - bitmap_font::GLYPH_H) / 2;
        let color = if is_primary { LASER() } else { TEXT_BRIGHT };
        draw_text(surf, tx, ty, 1, row.label, argb_to_framefang(color));
        return;
    }

    // ── CLEAR button (unbinds an optional action) ──────────────────────────
    if let WidgetKind::HotkeyClear(action) = row.kind {
        // Dimmed when there is nothing to clear, so the row reads as
        // "already unbound" rather than as a button that did nothing.
        let bound  = draft.hotkey_for(action).is_some();
        let border = if hovered && bound { LASER_DIM() } else { FIELD_BORDER() };
        if hovered && bound {
            fill_rect(surf, rx, ry, rw, rh, argb_to_framefang(HOVER_FILL()));
        }
        stroke_rect_argb(surf, rx, ry, rw, rh, argb_to_framefang(border));
        let tw = bitmap_font::measure(row.label, 1);
        let tx = rx + (rw - tw) / 2;
        let ty = ry + (rh - bitmap_font::GLYPH_H) / 2;
        let color = if bound { TEXT_BRIGHT } else { TEXT_DIM() };
        draw_text(surf, tx, ty, 1, row.label, argb_to_framefang(color));
        return;
    }

    // ── REBIND button (sits to the right of the HotkeyDisplay box) ─────────
    if let WidgetKind::HotkeyRebind(action) = row.kind {
        let active = capturing_hotkey == Some(action);
        let border = if active { LASER() } else if hovered { LASER_DIM() } else { FIELD_BORDER() };
        let fill   = if active { PRIMARY_FILL() } else if hovered { HOVER_FILL() } else { 0x0000_0000 };
        if fill != 0 {
            fill_rect(surf, rx, ry, rw, rh, argb_to_framefang(fill));
        }
        stroke_rect_argb(surf, rx, ry, rw, rh, argb_to_framefang(border));
        // Label flips while we're listening for a key so the user knows the
        // button "armed" the capture state, even though the actual readout
        // lives in the display box to the left.
        let label = if active { "LISTENING" } else { row.label };
        let tw = bitmap_font::measure(label, 1);
        let tx = rx + (rw - tw) / 2;
        let ty = ry + (rh - bitmap_font::GLYPH_H) / 2;
        let color = if active { LASER() } else { TEXT_BRIGHT };
        draw_text(surf, tx, ty, 1, label, argb_to_framefang(color));
        return;
    }

    // ── After a capture: the label, and under it what the held key does ────
    if row.kind == WidgetKind::AfterCapture {
        let gap = rh - 2 * bitmap_font::GLYPH_H;
        let top = ry + gap / 3;
        draw_text(surf, rx + 6, top, 1, row.label, argb_to_framefang(TEXT_BRIGHT));
        draw_text(surf, rx + 6, top + bitmap_font::GLYPH_H + gap / 3, 1, &switch_caption(draft),
                  argb_to_framefang(TEXT_MUTED()));
        return;
    }

    // ── Setting rows ───────────────────────────────────────────────────────
    if hovered { fill_rect(surf, rx, ry, rw, rh, argb_to_framefang(HOVER_FILL())); }

    let label_y = ry + (rh - bitmap_font::GLYPH_H) / 2;
    // A refining row sits indented under its parent, label a step quieter.
    let (indent, label_color) = if row.kind == WidgetKind::CloseAfterCopy {
        (SUB_INDENT, TEXT_MUTED())
    } else {
        (0, TEXT_BRIGHT)
    };
    draw_text(surf, rx + 6 + indent, label_y, 1, row.label, argb_to_framefang(label_color));

    // Geometry of the value half: starts right of the label column, ends at
    // the row edge. Folder rows reserve a "Browse…" button on the right.
    let val_x = rx + LABEL_W;
    let val_w = rw - LABEL_W - 4;
    let val_y = ry + 4;
    let val_h = rh - 8;

    match row.kind {
        WidgetKind::SaveFolder | WidgetKind::RecordingsFolder => {
            let browse_w = 80;
            let path_w   = val_w - browse_w - 8;
            // path field
            fill_rect(surf, val_x, val_y, path_w, val_h, argb_to_framefang(FIELD_BG()));
            stroke_rect_argb(surf, val_x, val_y, path_w, val_h, argb_to_framefang(FIELD_BORDER()));
            let val = match row.kind {
                // Pre-populate both folder fields with the actual resolved
                // path when no explicit value is set, so users see exactly
                // where output is going (matches what `save_directory` /
                // `recordings_directory_for` in tray_loop.rs pick at
                // runtime).
                WidgetKind::SaveFolder => folder_or_default_picture(&draft.save_directory),
                _                      => folder_or_default_video(&draft.recordings_directory),
            };
            let truncated = truncate_for(&val, path_w - 16);
            let ty = val_y + (val_h - bitmap_font::GLYPH_H) / 2;
            // Dim the path when it's the resolved fallback (no explicit value
            // in settings) so the user can tell at a glance "this hasn't been
            // customized yet".
            let user_set = match row.kind {
                WidgetKind::SaveFolder       => !draft.save_directory.is_empty(),
                WidgetKind::RecordingsFolder => !draft.recordings_directory.is_empty(),
                _                            => true,
            };
            let color = if user_set { TEXT_BRIGHT } else { TEXT_MUTED() };
            draw_text(surf, val_x + 8, ty, 1, &truncated, argb_to_framefang(color));
            // Browse… button
            let bx = val_x + path_w + 8;
            let by = val_y;
            let bw = browse_w;
            let bh = val_h;
            let b_border = if hovered { LASER_DIM() } else { FIELD_BORDER() };
            stroke_rect_argb(surf, bx, by, bw, bh, argb_to_framefang(b_border));
            let label = "Browse...";
            let tw = bitmap_font::measure(label, 1);
            let tx = bx + (bw - tw) / 2;
            let ty = by + (bh - bitmap_font::GLYPH_H) / 2;
            draw_text(surf, tx, ty, 1, label, argb_to_framefang(TEXT_BRIGHT));
        }
        WidgetKind::HotkeyDisplay(action) => {
            // The display box sits in the value column but stops short of
            // the REBIND button (which is its own row laid out on top of
            // this rect, anchored right), and of CLEAR where that exists.
            // Reserve their footprints plus an 8 px gutter each so the
            // laser-green border doesn't run under either button.
            let reserved = REBIND_W + 8
                + if action.is_optional() { CLEAR_W + 8 } else { 0 };
            let box_w = val_w - reserved;
            let listening = capturing_hotkey == Some(action);
            // While in capture mode the border lights up bright laser-green
            // so the user can see the field is now listening. Otherwise we
            // use the standard field border with a brighter hover state.
            let border = if listening {
                FIELD_FOCUS()
            } else if hovered {
                LASER_DIM()
            } else {
                FIELD_BORDER()
            };
            fill_rect(surf, val_x, val_y, box_w, val_h, argb_to_framefang(FIELD_BG()));
            stroke_rect_argb(surf, val_x, val_y, box_w, val_h, argb_to_framefang(border));
            let (text, color) = if listening {
                match capture_hint {
                    Some(hint) => (hint.to_owned(), WARN),
                    None => ("[ PRESS A KEY ]".to_owned(), LASER()),
                }
            } else {
                match draft.hotkey_for(action) {
                    Some(hk) => (hk.describe(), TEXT_BRIGHT),
                    // Optional actions start out unbound; say so instead of
                    // showing an empty box that reads as a render bug.
                    None     => ("(not set)".to_owned(), TEXT_DIM()),
                }
            };
            let truncated = truncate_for(&text, box_w - 16);
            let ty = val_y + (val_h - bitmap_font::GLYPH_H) / 2;
            draw_text(surf, val_x + 8, ty, 1, &truncated, argb_to_framefang(color));
        }
        WidgetKind::StartWithOs | WidgetKind::AutoCopy | WidgetKind::CloseAfterCopy | WidgetKind::CaptureCursor => {
            let on = match row.kind {
                WidgetKind::AutoCopy         => draft.auto_copy,
                WidgetKind::CloseAfterCopy   => draft.close_after_copy,
                WidgetKind::CaptureCursor    => draft.capture_cursor,
                _                            => draft.start_with_windows,
            };
            draw_toggle(surf, val_x, val_y, val_h, on);
        }
        _ => {}
    }
}

/// What holding the switch key does, under the After-a-capture label.
fn switch_caption(draft: &AppSettings) -> String {
    let key = draft.switch_key().label();
    if draft.open_editor {
        format!("Hold {key} to copy only")
    } else {
        format!("Hold {key} to open the editor")
    }
}

/// An After-a-capture chip. The picked one is filled and framed; the picked
/// switch key is framed a step quieter than the picked mode, since the mode
/// is the main choice and the key only refines it.
fn render_choice_chip<S: painter::Surface>(surf: &mut S, row: &Row, hovered: bool, draft: &AppSettings) {
    let (rx, ry, rw, rh) = row.rect;
    let (chosen, accent) = match row.kind {
        WidgetKind::AfterCaptureChoice(editor) => (draft.open_editor == editor, LASER()),
        WidgetKind::SwitchKeyChoice(key) => (draft.switch_key() == key, LASER_DIM()),
        _ => return,
    };
    if chosen || hovered {
        fill_rect(surf, rx, ry, rw, rh, argb_to_framefang(HOVER_FILL()));
    }
    let border = if chosen { accent } else if hovered { LASER_DIM() } else { FIELD_BORDER() };
    stroke_rect_argb(surf, rx, ry, rw, rh, argb_to_framefang(border));
    let tw = bitmap_font::measure(row.label, 1);
    let color = if chosen { TEXT_BRIGHT } else { TEXT_MUTED() };
    draw_text(surf, rx + (rw - tw) / 2, ry + (rh - bitmap_font::GLYPH_H) / 2, 1,
              row.label, argb_to_framefang(color));
}

/// A color-scheme chip: the scheme's own accent as a swatch, then its name.
/// The chosen one is framed in its accent; the rest stay quiet until hovered.
fn render_theme_chip<S: painter::Surface>(
    surf: &mut S, row: &Row, scheme: crate::palette::Scheme, hovered: bool, draft: &AppSettings,
) {
    let (rx, ry, rw, rh) = row.rect;
    let chosen = crate::palette::Scheme::parse(&draft.theme) == scheme;
    if chosen || hovered {
        fill_rect(surf, rx, ry, rw, rh, argb_to_framefang(HOVER_FILL()));
    }
    let border = if chosen { scheme.accent() } else if hovered { LASER_DIM() } else { FIELD_BORDER() };
    stroke_rect_argb(surf, rx, ry, rw, rh, argb_to_framefang(border));

    const SWATCH: i32 = 8;
    const SWATCH_GAP: i32 = 6;
    let tw = bitmap_font::measure(row.label, 1);
    let gx = rx + (rw - (SWATCH + SWATCH_GAP + tw)) / 2;
    fill_rect(surf, gx, ry + (rh - SWATCH) / 2, SWATCH, SWATCH, argb_to_framefang(scheme.accent()));
    let color = if chosen { TEXT_BRIGHT } else { TEXT_MUTED() };
    draw_text(surf, gx + SWATCH + SWATCH_GAP, ry + (rh - bitmap_font::GLYPH_H) / 2, 1,
              row.label, argb_to_framefang(color));
}

/// Draw an ON/OFF toggle pill anchored at (x, y) with the given height; width
/// is fixed at ~64 px. Knob slides to the right when on.
fn draw_toggle<S: painter::Surface>(s: &mut S, x: i32, y: i32, h: i32, on: bool) {
    let w = 64;
    let bg = if on { TOGGLE_ON() } else { TOGGLE_OFF() };
    fill_rect(s, x, y, w, h, argb_to_framefang(bg));
    let border = if on { LASER_DIM() } else { FIELD_BORDER() };
    stroke_rect_argb(s, x, y, w, h, argb_to_framefang(border));
    // Knob — small filled square that sits on left when off, right when on.
    let knob_w = h - 8;
    let knob_x = if on { x + w - knob_w - 4 } else { x + 4 };
    let knob_y = y + 4;
    let knob_color = if on { TOGGLE_KNOB() } else { TOGGLE_OFF_K() };
    fill_rect(s, knob_x, knob_y, knob_w, knob_w, argb_to_framefang(knob_color));
    // Label inside, opposite side of the knob.
    let label = if on { "ON" } else { "OFF" };
    let tw = bitmap_font::measure(label, 1);
    let ty = y + (h - bitmap_font::GLYPH_H) / 2;
    let tx = if on { x + 8 } else { x + w - tw - 8 };
    let color = if on { TEXT_BRIGHT } else { TEXT_MUTED() };
    draw_text(s, tx, ty, 1, label, argb_to_framefang(color));
}


/// Resolve the screenshots-folder display value: explicit `save_directory`
/// when set, otherwise the actual XDG Pictures dir on disk (with a generic
/// `~/Pictures` fallback if no UserDirs are available).
fn folder_or_default_picture(v: &str) -> String {
    if !v.is_empty() { return v.to_owned(); }
    directories::UserDirs::new()
        .and_then(|u| u.picture_dir().map(|p| p.to_string_lossy().to_string()))
        .unwrap_or_else(|| "~/Pictures".to_owned())
}

/// Resolve the recordings-folder display value: explicit
/// `recordings_directory` when set, otherwise the actual XDG Videos dir.
/// Mirrors `recordings_directory_for` in tray_loop.rs so what the user sees
/// here is what the recorder will actually use.
fn folder_or_default_video(v: &str) -> String {
    if !v.is_empty() { return v.to_owned(); }
    directories::UserDirs::new()
        .and_then(|u| u.video_dir().map(|p| p.to_string_lossy().to_string()))
        .unwrap_or_else(|| "~/Videos".to_owned())
}

// ── tiny rendering helpers, scoped to this file ─────────────────────────────

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

fn fill_rect<S: painter::Surface>(s: &mut S, x: i32, y: i32, w: i32, h: i32, color: FramefangRgba) {
    let rgba = [color.r, color.g, color.b, color.a];
    for yy in y..y + h {
        for xx in x..x + w {
            s.write(xx, yy, rgba);
        }
    }
}

fn stroke_rect_argb<S: painter::Surface>(s: &mut S, x: i32, y: i32, w: i32, h: i32, color: FramefangRgba) {
    let rgba = [color.r, color.g, color.b, color.a];
    for xx in x..x + w { s.write(xx, y, rgba); s.write(xx, y + h - 1, rgba); }
    for yy in y..y + h { s.write(x, yy, rgba); s.write(x + w - 1, yy, rgba); }
}

fn draw_text<S: painter::Surface>(s: &mut S, x: i32, y: i32, scale: i32, text: &str, color: FramefangRgba) {
    painter::draw_text(s, x, y, scale, text, color);
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

/// True for `KeyCode`s that represent a bare modifier — the rebind widget
/// rejects these because "Ctrl alone" is rarely a meaningful global hotkey
/// and would fire on every modifier press.
fn is_modifier_code(c: KeyCode) -> bool {
    matches!(
        c,
        KeyCode::ControlLeft | KeyCode::ControlRight
            | KeyCode::ShiftLeft | KeyCode::ShiftRight
            | KeyCode::AltLeft   | KeyCode::AltRight
            | KeyCode::SuperLeft | KeyCode::SuperRight
            | KeyCode::Meta      | KeyCode::Hyper
            | KeyCode::Fn        | KeyCode::FnLock
            | KeyCode::CapsLock  | KeyCode::NumLock
    )
}

/// Translate winit's `ModifiersState` mask into the framefang-core `Modifiers`
/// flag set whose bit layout matches Win32 `MOD_*` (the JSON wire format).
fn mods_state_to_hk(s: ModifiersState) -> HkMods {
    let mut out = HkMods::empty();
    if s.control_key() { out |= HkMods::CONTROL; }
    if s.shift_key()   { out |= HkMods::SHIFT; }
    if s.alt_key()     { out |= HkMods::ALT; }
    if s.super_key()   { out |= HkMods::SUPER; }
    out
}

/// Map a winit `KeyCode` to the Win32 virtual-key code we serialize in
/// settings.json (canonical wire format shared with the C# build).
///
/// Covers every key in `framefang_core::settings::KEYS`: letters, digits,
/// punctuation, F1-F24, the numpad, the navigation cluster, Print Screen,
/// Pause, Scroll Lock, Menu, and the media, volume, browser and launch keys.
/// Esc is not here: it cancels the rebind. A key winit can't name is read
/// from its scancode instead (`native_vk`).
fn keycode_to_vk(c: KeyCode) -> Option<u32> {
    Some(match c {
        // Punctuation (US layout names; the VK is the key's position).
        KeyCode::Semicolon => 0xBA, KeyCode::Equal => 0xBB, KeyCode::Comma => 0xBC,
        KeyCode::Minus => 0xBD, KeyCode::Period => 0xBE, KeyCode::Slash => 0xBF,
        KeyCode::Backquote => 0xC0, KeyCode::BracketLeft => 0xDB, KeyCode::Backslash => 0xDC,
        KeyCode::BracketRight => 0xDD, KeyCode::Quote => 0xDE, KeyCode::IntlBackslash => 0xE2,
        // Typing and whitespace keys (allowed only with a modifier).
        KeyCode::Space => 0x20, KeyCode::Tab => 0x09, KeyCode::Enter => 0x0D,
        KeyCode::Backspace => 0x08,
        // F13-F24
        KeyCode::F13 => 0x7C, KeyCode::F14 => 0x7D, KeyCode::F15 => 0x7E, KeyCode::F16 => 0x7F,
        KeyCode::F17 => 0x80, KeyCode::F18 => 0x81, KeyCode::F19 => 0x82, KeyCode::F20 => 0x83,
        KeyCode::F21 => 0x84, KeyCode::F22 => 0x85, KeyCode::F23 => 0x86, KeyCode::F24 => 0x87,
        // Numpad
        KeyCode::Numpad0 => 0x60, KeyCode::Numpad1 => 0x61, KeyCode::Numpad2 => 0x62,
        KeyCode::Numpad3 => 0x63, KeyCode::Numpad4 => 0x64, KeyCode::Numpad5 => 0x65,
        KeyCode::Numpad6 => 0x66, KeyCode::Numpad7 => 0x67, KeyCode::Numpad8 => 0x68,
        KeyCode::Numpad9 => 0x69, KeyCode::NumpadMultiply => 0x6A, KeyCode::NumpadAdd => 0x6B,
        KeyCode::NumpadSubtract => 0x6D, KeyCode::NumpadDecimal => 0x6E,
        KeyCode::NumpadDivide => 0x6F, KeyCode::NumpadEnter => 0x0D,
        // Menu, sleep, media, volume, browser and launch keys.
        KeyCode::ContextMenu => 0x5D, KeyCode::Sleep => 0x5F,
        KeyCode::BrowserBack => 0xA6, KeyCode::BrowserForward => 0xA7,
        KeyCode::BrowserRefresh => 0xA8, KeyCode::BrowserStop => 0xA9,
        KeyCode::BrowserSearch => 0xAA, KeyCode::BrowserFavorites => 0xAB,
        KeyCode::BrowserHome => 0xAC,
        KeyCode::AudioVolumeMute => 0xAD, KeyCode::AudioVolumeDown => 0xAE,
        KeyCode::AudioVolumeUp => 0xAF,
        KeyCode::MediaTrackNext => 0xB0, KeyCode::MediaTrackPrevious => 0xB1,
        KeyCode::MediaStop => 0xB2, KeyCode::MediaPlayPause => 0xB3,
        KeyCode::LaunchMail => 0xB4, KeyCode::MediaSelect => 0xB5,
        KeyCode::LaunchApp1 => 0xB6, KeyCode::LaunchApp2 => 0xB7,
        // Letters
        KeyCode::KeyA => 0x41, KeyCode::KeyB => 0x42, KeyCode::KeyC => 0x43,
        KeyCode::KeyD => 0x44, KeyCode::KeyE => 0x45, KeyCode::KeyF => 0x46,
        KeyCode::KeyG => 0x47, KeyCode::KeyH => 0x48, KeyCode::KeyI => 0x49,
        KeyCode::KeyJ => 0x4A, KeyCode::KeyK => 0x4B, KeyCode::KeyL => 0x4C,
        KeyCode::KeyM => 0x4D, KeyCode::KeyN => 0x4E, KeyCode::KeyO => 0x4F,
        KeyCode::KeyP => 0x50, KeyCode::KeyQ => 0x51, KeyCode::KeyR => 0x52,
        KeyCode::KeyS => 0x53, KeyCode::KeyT => 0x54, KeyCode::KeyU => 0x55,
        KeyCode::KeyV => 0x56, KeyCode::KeyW => 0x57, KeyCode::KeyX => 0x58,
        KeyCode::KeyY => 0x59, KeyCode::KeyZ => 0x5A,
        // Digits (top row only — numpad omitted intentionally because Win32
        // splits those into VK_NUMPAD0–9 and the rebind widget doesn't yet
        // expose a distinction).
        KeyCode::Digit0 => 0x30, KeyCode::Digit1 => 0x31, KeyCode::Digit2 => 0x32,
        KeyCode::Digit3 => 0x33, KeyCode::Digit4 => 0x34, KeyCode::Digit5 => 0x35,
        KeyCode::Digit6 => 0x36, KeyCode::Digit7 => 0x37, KeyCode::Digit8 => 0x38,
        KeyCode::Digit9 => 0x39,
        // F-keys
        KeyCode::F1 => 0x70, KeyCode::F2 => 0x71, KeyCode::F3 => 0x72,
        KeyCode::F4 => 0x73, KeyCode::F5 => 0x74, KeyCode::F6 => 0x75,
        KeyCode::F7 => 0x76, KeyCode::F8 => 0x77, KeyCode::F9 => 0x78,
        KeyCode::F10 => 0x79, KeyCode::F11 => 0x7A, KeyCode::F12 => 0x7B,
        // Navigation / editing cluster
        KeyCode::PrintScreen => 0x2C,
        KeyCode::ScrollLock  => 0x91,
        KeyCode::Pause       => 0x13,
        KeyCode::Insert      => 0x2D,
        KeyCode::Delete      => 0x2E,
        KeyCode::Home        => 0x24,
        KeyCode::End         => 0x23,
        KeyCode::PageUp      => 0x21,
        KeyCode::PageDown    => 0x22,
        KeyCode::ArrowLeft   => 0x25,
        KeyCode::ArrowUp     => 0x26,
        KeyCode::ArrowRight  => 0x27,
        KeyCode::ArrowDown   => 0x28,
        _ => return None,
    })
}

/// What the listening box says when a typing key was pressed without a
/// modifier that stops it typing: what to hold, in one instruction.
fn typing_key_hint(vk: u32) -> String {
    let key = vk_label(vk).map(str::to_owned).unwrap_or_else(|| format!("(0x{vk:02X})"));
    let hold = if cfg!(target_os = "macos") { "Cmd or Ctrl" } else { "Ctrl, Alt or Win" };
    format!("Hold {hold} with {key}")
}

/// The Win32 virtual key of a key winit couldn't name, from the scancode
/// Windows reported for it. `None` elsewhere, or when Windows has no VK for
/// that scancode either.
fn native_vk(key: PhysicalKey) -> Option<u32> {
    #[cfg(windows)]
    {
        use winit::keyboard::NativeKeyCode;
        use windows::Win32::UI::Input::KeyboardAndMouse::{MapVirtualKeyW, MAPVK_VSC_TO_VK_EX};
        if let PhysicalKey::Unidentified(NativeKeyCode::Windows(scancode)) = key {
            let vk = unsafe { MapVirtualKeyW(scancode as u32, MAPVK_VSC_TO_VK_EX) };
            // Esc stays the way out of the rebind.
            return (vk != 0 && vk != 0x1B).then_some(vk);
        }
        None
    }
    #[cfg(not(windows))]
    {
        let _ = key;
        None
    }
}

/// Surface the `vk_label` map for any caller outside this module that needs
/// the same label table. Currently unused at module scope but kept here so
/// future code (e.g. tray tooltip) doesn't reach into framefang-core directly.
#[allow(dead_code)]
fn vk_label_or_hex(vk: u32) -> String {
    vk_label(vk).map(str::to_owned).unwrap_or_else(|| format!("(0x{:02X})", vk))
}

/// Shrink `s` until it fits in `max_px` at scale 1, eliding the middle instead
/// of the tail: these fields mostly hold filesystem paths, and the leaf name at
/// the end is the part worth reading.
fn truncate_for(s: &str, max_px: i32) -> String {
    if bitmap_font::measure(s, 1) <= max_px { return s.to_owned(); }
    let ellipsis = "..";
    // A glyph advances GLYPH_W plus the one-pixel gap that follows it; `measure`
    // drops that gap only after the very last glyph, so a run of `n` chars is
    // `n * advance - 1` wide and `n` chars fit while `n * advance - 1 <= max_px`.
    let advance = bitmap_font::GLYPH_W + 1;
    let fits = (max_px + 1) / advance;
    let ell_n = ellipsis.chars().count() as i32;
    if fits <= ell_n {
        return ellipsis.chars().take(fits.max(0) as usize).collect();
    }
    // Split the remaining budget two-thirds in favour of the tail.
    let keep = fits - ell_n;
    let tail = (keep * 2 / 3).max(1) as usize;
    let head = keep as usize - tail;
    // `fits < s.chars().count()` here (the early return covers the rest), so the
    // head and tail slices can never overlap.
    let chars: Vec<char> = s.chars().collect();
    let mut out: String = chars[..head].iter().collect();
    out.push_str(ellipsis);
    out.extend(&chars[chars.len() - tail..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Space every label keeps before the control beside it.
    const MIN_GAP: i32 = 12;

    #[test]
    fn the_heading_and_labels_clear_their_controls() {
        let rows = SettingsView::build_rows(crate::system_shortcuts::page(), true);

        // The heading ends before the first header button.
        let first_btn = rows.iter()
            .filter(|r| matches!(r.kind, WidgetKind::OpenJson | WidgetKind::CancelBtn | WidgetKind::SaveBtn))
            .map(|r| r.rect.0)
            .min()
            .unwrap();
        let heading_end = PAD + bitmap_font::measure(TITLE, 2);
        assert!(heading_end + MIN_GAP <= first_btn,
            "heading ends at {heading_end}, the buttons start at {first_btn}");

        // Every row label, indent included, ends before its control.
        for r in &rows {
            let indent = match r.kind {
                WidgetKind::CloseAfterCopy => SUB_INDENT,
                WidgetKind::SaveFolder | WidgetKind::RecordingsFolder | WidgetKind::Ffmpeg | WidgetKind::AutoCopy
                | WidgetKind::CaptureCursor | WidgetKind::AfterCapture
                | WidgetKind::HotkeyDisplay(_) | WidgetKind::Theme | WidgetKind::StartWithOs => 0,
                _ => continue,
            };
            let end = 6 + indent + bitmap_font::measure(r.label, 1);
            assert!(end + MIN_GAP <= LABEL_W, "\"{}\" ends at {end}, its control at {LABEL_W}", r.label);
        }
    }

    #[cfg(windows)]
    #[test]
    fn the_windows_guide_reads_in_every_state_without_touching_its_button() {
        let page = crate::system_shortcuts::page().expect("Windows has a page");
        let rows = SettingsView::build_rows(Some(page), true);
        let btn = rows.iter().find(|r| r.kind == WidgetKind::SystemShortcut).unwrap();
        let text_x = PAD + 16 + 12;
        let card_right = WIN_W as i32 - PAD - CARD_PAD;
        // The listening box fits the longest "needs a modifier" hint.
        let hint_room = WIN_W as i32 - 2 * PAD - LABEL_W - 4 - (REBIND_W + 8) - 16;
        let longest_typing = framefang_core::settings::KEYS.iter()
            .filter(|k| !k.alone).max_by_key(|k| k.label.len()).unwrap().vk;
        let hint = typing_key_hint(longest_typing);
        assert!(bitmap_font::measure(&hint, 1) <= hint_room, "\"{hint}\" is cut off in the box");
        for takes in [true, false] {
            let g = WindowsGuide { takes, switch: "x", page: "Bluetooth & devices > Keyboard" };
            let (headline, page_line, _) = guide_text(&g);
            let end = text_x + bitmap_font::measure(&headline, 1);
            assert!(end + MIN_GAP <= btn.rect.0, "\"{headline}\" runs into the button");
            assert!(text_x + bitmap_font::measure(&page_line, 1) <= card_right, "\"{page_line}\" leaves the card");
        }
        // The card sits between the last hotkey row and the APPEARANCE heading.
        let last_hotkey = rows.iter().filter(|r| matches!(r.kind, WidgetKind::HotkeyDisplay(_))).last().unwrap();
        let theme = rows.iter().find(|r| r.kind == WidgetKind::Theme).unwrap();
        let card_top = btn.rect.1 - CARD_PAD;
        assert!(card_top >= last_hotkey.rect.1 + ROW_H);
        assert!(card_top + CARD_H + 12 <= theme.rect.1 - 22, "room for the APPEARANCE heading");

        // Another capture key: no card, and the window is shorter by it.
        let without = SettingsView::build_rows(Some(page), false);
        assert!(!without.iter().any(|r| r.kind == WidgetKind::SystemShortcut));
        let theme2 = without.iter().find(|r| r.kind == WidgetKind::Theme).unwrap();
        assert_eq!(theme2.rect.1 - (last_hotkey.rect.1 + ROW_H), 40, "same spacing as the other sections");
        assert!(content_height(&without) < content_height(&rows));
        // The After-a-capture row (2026-11-17) took the last 40 px a 768 px
        // screen had: with the card it needed about 808 (title bar and a
        // 48 px taskbar included), so 1366x768 cuts off the bottom row. The
        // mouse-pointer row (2026-11-19, Brandon asked for it) adds 40 more.
        // Pinned so the next row is a deliberate choice too.
        assert!(content_height(&rows) + crate::chrome::TITLE_H as u32 + 48 <= 848, "fits an 848 px screen");
    }

    #[test]
    fn the_windows_card_shows_only_for_plain_print_screen_on_windows() {
        let mut s = AppSettings::default();
        let page = crate::system_shortcuts::page();
        s.set_hotkey_for(HotkeyAction::Capture, Some(Hotkey { modifiers: HkMods::empty(), virtual_key: 0x2C }));
        assert_eq!(shows_windows_guide(page, &s), cfg!(windows), "Windows only");
        s.set_hotkey_for(HotkeyAction::Capture, Some(Hotkey { modifiers: HkMods::CONTROL, virtual_key: 0x2C }));
        assert!(!shows_windows_guide(page, &s), "Ctrl+PrtSc is not what the switch takes");
        s.set_hotkey_for(HotkeyAction::Capture, Some(Hotkey { modifiers: HkMods::empty(), virtual_key: 0x7C }));
        assert!(!shows_windows_guide(page, &s), "F13 has nothing to do with the switch");
    }

    #[test]
    fn header_button_words_fit_their_buttons() {
        for r in SettingsView::build_rows(None, false) {
            if matches!(r.kind, WidgetKind::OpenJson | WidgetKind::CancelBtn | WidgetKind::SaveBtn) {
                assert!(bitmap_font::measure(r.label, 1) + 2 * MIN_GAP <= r.rect.2, "{}", r.label);
            }
            // A theme chip holds an 8 px swatch, a 6 px gap and the name.
            if matches!(r.kind, WidgetKind::ThemeChoice(_)) {
                assert!(8 + 6 + bitmap_font::measure(r.label, 1) + MIN_GAP <= r.rect.2, "{}", r.label);
            }
            // An After-a-capture chip keeps 8 px either side of its word.
            if matches!(r.kind, WidgetKind::AfterCaptureChoice(_) | WidgetKind::SwitchKeyChoice(_)) {
                assert!(bitmap_font::measure(r.label, 1) + 16 <= r.rect.2, "{}", r.label);
            }
        }
    }

    #[test]
    fn the_after_capture_chips_stay_in_the_value_column_with_their_caption_clear() {
        let rows = SettingsView::build_rows(None, false);
        let row = rows.iter().find(|r| r.kind == WidgetKind::AfterCapture).unwrap();
        let chips: Vec<&Row> = rows.iter()
            .filter(|r| matches!(r.kind, WidgetKind::AfterCaptureChoice(_) | WidgetKind::SwitchKeyChoice(_)))
            .collect();
        assert_eq!(chips.len(), 5);
        let right = row.rect.0 + row.rect.2 - 4;
        // The keys end flush with the row's other controls, a clear gap
        // after the two modes.
        assert_eq!(chips[4].rect.0 + chips[4].rect.2, right);
        assert!(chips[2].rect.0 - (chips[1].rect.0 + chips[1].rect.2) >= GROUP_GAP);
        for c in &chips {
            assert!(c.rect.0 >= row.rect.0 + LABEL_W && c.rect.0 + c.rect.2 <= right, "{} leaves the row", c.label);
            assert!(c.rect.1 >= row.rect.1 && c.rect.1 + c.rect.3 <= row.rect.1 + ROW_H);
        }
        // The caption under the label, at its longest, ends before the chips.
        let mut s = AppSettings::default();
        for open_editor in [true, false] {
            for key in SwitchKey::ALL {
                s.open_editor = open_editor;
                s.switch_key = key.name().into();
                let caption = switch_caption(&s);
                assert!(6 + bitmap_font::measure(&caption, 1) + MIN_GAP <= LABEL_W, "\"{caption}\"");
            }
        }
    }
}
