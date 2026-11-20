//! Real Framefang — Settings window.
//!
//! Borderless winit + softbuffer + the in-tree bitmap font. Same lifecycle
//! pattern as `PinView` / `Overlay`: TrayApp owns an `Option<SettingsView>`,
//! dispatches `WindowEvent`s by `WindowId`, and polls `outcome` after each
//! event to learn when the user saved or cancelled.
//!
//! Layout: four tabs (CAPTURES, FILES, HOTKEYS, GENERAL), each a page of
//! settings with a hairline between them; see `build_rows`.
//!
//! There is no Save button: every change is handed to the tray as it is
//! made (`SettingsOutcome::Changed`), which applies and saves it.
//!
//! GENERAL's last row holds the settings file: Edit as JSON opens
//! settings.json in the user's default editor (a saved edit shows here within
//! a second), Import... loads a copy, Export... saves one where the user
//! picks. What the last of them did shows under the heading.

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
// The window doesn't scroll. Its height follows its tallest page
// (`window_height`) and fits a 1280 x 720 screen with a 48 px panel (a test
// pins it). The bottom margin is a padding band: the hotkey-conflict message
// renders under the HOTKEYS rows it is about.
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
/// The window's heading, drawn in the title bar (`chrome`).
const TITLE: &str = "// SETTINGS";
/// Room under the last hotkey row for the system line or the Windows card.
const SECTION_ABOVE: i32 = 34;
/// Between the Theme row's color chips.
const THEME_CHIP_GAP: i32 = 8;
/// Between a row's choice chips.
const CHIP_GAP: i32 = 6;
// Buttons that sit inside a hotkey row, right-anchored: REBIND on the edge,
// CLEAR to its left on the two optional actions.
const REBIND_W: i32 = 100;
const CLEAR_W:  i32 = 76;
/// Between the settings file's buttons and the line saying what the last of
/// them did.
const STATUS_GAP: i32 = 8;

#[derive(Clone, Copy, PartialEq, Eq)]
enum WidgetKind {
    SaveFolder,
    RecordingsFolder,
    /// Which FFmpeg is used: the one picked, or where one was found, or that
    /// none is installed (a label and a read-out; its buttons follow).
    Ffmpeg,
    /// Download Framefang's FFmpeg. Live only on Windows while none is
    /// found, none is picked and no download is running.
    FfmpegDownload,
    /// Pick an FFmpeg to use (always there, at the row's right end).
    FfmpegBrowse,
    /// Forget the pick and find FFmpeg by itself again. In Download's place,
    /// live only while an FFmpeg is picked.
    FfmpegAuto,
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
    /// Edit, copy and edit, or just copy when a drag ends.
    AfterCaptureChoice(framefang_core::settings::AfterCapture),
    /// The same three for a scrolling capture, set apart (its row's label,
    /// then its chips).
    ScrollAfter,
    ScrollAfterChoice(framefang_core::settings::AfterCapture),
    /// A line under a choice, as a sentence: "Hold [SHIFT] to just copy
    /// instead". Only words; its key is the control.
    HoldLine(Hold),
    /// That line's key: click it, then press any key to bind it.
    HoldKey(Hold),
    /// Keep the last 10 captures for the tray's RECENT list.
    KeepRecent,
    /// The editor's Copy also closes the editor. Shown on After a capture's
    /// sub-strip, under its hold line.
    CloseAfterCopy,
    /// Screenshots show the mouse pointer. Laid out only where the platform
    /// can draw it (`framefang_platform::cursor::SUPPORTED`).
    CaptureCursor,
    /// The Settings file row's label; its three buttons are the controls.
    SettingsFile,
    OpenJson,
    /// Save a copy of the settings to a file the user picks.
    ExportBtn,
    /// Replace the settings with ones from a file the user picks.
    ImportBtn,
    /// Opens the system's own screenshot-shortcut page. Laid out only where
    /// `system_shortcuts::page()` has one.
    SystemShortcut,
    /// One tab in the strip under the header.
    Tab(Tab),
    /// The File names row's label and caption (the next name); its chips are
    /// the controls.
    FileNames,
    /// A file-name preset by index into `file_name::PRESETS`, or `None` for
    /// CUSTOM (a pattern set in settings.json).
    FileNameChoice(Option<usize>),
    /// The Record as row's label; its chips are the controls.
    RecordFormat,
    /// Recordings end as GIF (`true`) or MP4.
    RecordFormatChoice(bool),
    /// The Frame row's label; its chips are the controls.
    Frame,
    FrameChoice(framefang_core::settings::OutputFrame),
    /// The QR links row's label; its chips are the controls.
    QrLinks,
    /// A web link in a QR code opens (`true`) or is copied.
    QrLinksChoice(bool),
    /// A group of settings: a setting and, from the given y down, its
    /// sub-settings. No surface of its own (Brandon 2026-11-20: boxes looked
    /// clunky, picked "A: Lines"): a hairline above it when it isn't the
    /// page's first, and a tree line joining its sub-settings to it.
    Card(Option<i32>),
    /// A hold line's REBIND button, at the line's right end: the same as
    /// clicking its key.
    HoldRebind(Hold),
    /// A hairline between two settings in one card.
    Divider,
}

/// Room above and below a group's rows, above its sub-settings, and between
/// two settings (a hairline runs through the middle of that).
const CARD_IN: i32 = 0;
const SUB_IN: i32 = 0;
const CARD_GAP: i32 = 12;
/// The tree line from a setting down to its sub-settings: its x from the
/// row's left edge, and where each tick ends (the sub-setting's words start
/// a few px after, at `SUB_INDENT`).
const TREE_X: i32 = 10;
const TREE_TICK: i32 = 18;

/// A card being laid out: where it starts, and where its sub-strip does.
struct CardBuild { top: i32, sub: Option<i32> }

impl CardBuild {
    fn open(y: &mut i32) -> CardBuild {
        let c = CardBuild { top: *y, sub: None };
        *y += CARD_IN;
        c
    }

    /// The rows from here down are this card's sub-settings.
    fn sub(&mut self, y: &mut i32) {
        self.sub = Some(*y);
        *y += SUB_IN;
    }

    fn close(self, rows: &mut Vec<Row>, y: &mut i32) {
        *y += if self.sub.is_some() { SUB_IN } else { CARD_IN };
        rows.push(Row { kind: WidgetKind::Card(self.sub), label: "",
                        rect: (PAD, self.top, WIN_W as i32 - 2 * PAD, *y - self.top) });
        *y += CARD_GAP;
    }
}

/// Between two settings in one group: the same gap and hairline as between
/// two groups, so every setting on a page sits the same distance apart.
fn divider(rows: &mut Vec<Row>, y: &mut i32) {
    rows.push(Row { kind: WidgetKind::Divider, label: "",
                    rect: (PAD, *y + CARD_GAP / 2, WIN_W as i32 - 2 * PAD, 1) });
    *y += CARD_GAP;
}

/// Which held key a hold line sets: the capture switch key (editor or just
/// copy), QR's (open a link or copy it), or a scrolling capture's.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Hold { Capture, Qr, Scroll }

impl Hold {
    fn key(self, s: &AppSettings) -> SwitchKey {
        match self {
            Hold::Capture => s.switch_key(),
            Hold::Qr => s.qr_switch_key(),
            Hold::Scroll => s.scroll_switch_key(),
        }
    }

    fn set(self, s: &mut AppSettings, k: SwitchKey) {
        match self {
            Hold::Capture => s.switch_key = k.name(),
            Hold::Qr => s.qr_switch_key = k.name(),
            Hold::Scroll => s.scroll_switch_key = k.name(),
        }
    }

    /// The words after the key: what holding it does, the other of the
    /// two choices above it.
    fn tail(self, s: &AppSettings) -> &'static str {
        match self {
            Hold::Capture if s.open_editor => "to just copy instead",
            Hold::Capture => "to open the editor instead",
            Hold::Qr if s.open_qr_links => "to copy links instead",
            Hold::Qr => "to open links instead",
            Hold::Scroll if s.scroll_after() == framefang_core::settings::AfterCapture::CopyOnly => "to open the editor instead",
            Hold::Scroll => "to just copy instead",
        }
    }
}

/// A hold line: "Hold", the key, then what holding it does. Where its key
/// sits from the line's value column, and the key's name now.
const HOLD_WORD: &str = "Hold";
const HOLD_GAP: i32 = 8;
/// A hold line is shorter than a row: it belongs to the choice above it.
const HOLD_ROW_H: i32 = 28;
/// Its REBIND button: the Hotkeys page's button, a size down to suit the
/// shorter line.
const HOLD_BTN_W: i32 = 72;
const HOLD_BTN_H: i32 = 20;
/// What the key says while it waits for a press.
const HOLD_LISTEN: &str = "PRESS A KEY";

fn hold_key_name(hold: Hold, s: &AppSettings, listening: bool) -> String {
    if listening { HOLD_LISTEN.to_owned() } else { hold.key(s).label().to_uppercase() }
}

/// The key's face top-left and its width (shadow included), for a line
/// whose words have their top at `text_y`.
fn hold_key_rect(line: &Row, name: &str) -> (i32, i32, i32, i32) {
    let x = line.rect.0 + 6 + SUB_INDENT + bitmap_font::measure(HOLD_WORD, 1) + HOLD_GAP;
    let text_y = line.rect.1 + (line.rect.3 - bitmap_font::GLYPH_H) / 2;
    let face_y = text_y - (crate::keycap::face_h(1) - bitmap_font::GLYPH_H) / 2;
    (x, face_y, crate::keycap::key_w(name, 1), crate::keycap::height(1))
}

/// Settings' pages, one tab each, in strip order.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Tab { Captures, Files, Hotkeys, General }

impl Tab {
    const ALL: [Tab; 4] = [Tab::Captures, Tab::Files, Tab::Hotkeys, Tab::General];

    /// The tab's words, numbered like the dialogs' section labels.
    fn label(self) -> &'static str {
        match self {
            Tab::Captures => "01 CAPTURES",
            Tab::Files => "02 FILES",
            Tab::Hotkeys => "03 HOTKEYS",
            Tab::General    => "04 GENERAL",
        }
    }

    fn step(self, by: i32) -> Tab {
        let i = Tab::ALL.iter().position(|t| *t == self).unwrap_or(0) as i32;
        Tab::ALL[(i + by).rem_euclid(Tab::ALL.len() as i32) as usize]
    }
}

/// The tab strip: where it starts under the title bar, how tall a tab is,
/// and the room on either side of a tab's words.
const STRIP_Y: i32 = 16;
const TAB_H: i32 = 28;
const TAB_PAD: i32 = 14;
const TAB_GAP: i32 = 4;
/// A page's first row, under the strip.
const PAGE_TOP: i32 = STRIP_Y + TAB_H + 22;
/// The strip's keyboard hint, at its right end.
const TAB_HINT: &str = "[Ctrl]+[Tab]";

fn tab_w(t: Tab) -> i32 { bitmap_font::measure(t.label(), 1) + 2 * TAB_PAD }

/// The line under HOTKEYS: the system page's button, and whether the system
/// is taking the capture binding right now.
const SYSTEM_BTN_H: i32 = 20;

struct Row {
    kind:  WidgetKind,
    label: &'static str,
    rect:  (i32, i32, i32, i32),
}

pub enum SettingsOutcome {
    /// Settings changed here: only what changed, which the tray applies to
    /// `settings.json` as it is on disk and saves at once. There is no Save
    /// button; the window stays open.
    Patch(framefang_core::settings_store::Patch),
    /// Import: these settings, all of them, replace the file.
    Replace(AppSettings),
    /// The window was closed (Esc or its close button). Everything is
    /// already saved; the tray turns the shortcuts back on.
    Closed,
    /// User clicked [Edit as JSON]; tray loop should shell-open the config
    /// path. The window stays open, and an edit saved there shows in it.
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
    /// A hold line's key waiting for a press to bind, and why the last press
    /// was refused, if it was.
    listening_hold: Option<Hold>,
    hold_refused: Option<String>,
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
    /// What the last Export or Import did, shown in the subtitle's place;
    /// `true` for a failure.
    status: Option<(String, bool)>,
    /// The settings the tray last gave this window. A change is sent as the
    /// difference from these, so it carries nothing this window didn't
    /// change.
    baseline: AppSettings,
    /// The draft came from Import, so the next report replaces the file.
    imported: bool,
    /// The page shown.
    tab: Tab,
    /// Answers for the tray, in order (one click can give two: a change,
    /// then opening settings.json).
    outcome: std::collections::VecDeque<SettingsOutcome>,
}

/// The FFmpeg row's buttons: Download or Auto (one slot), then Browse...
/// at the right end, as wide as the folder rows' Browse... above it.
const FFMPEG_BTN_W: i32 = 104;
const BROWSE_W: i32 = 80;
/// Auto is a short word: narrower than Download, so a picked path has room.
const AUTO_W: i32 = 64;

impl SettingsView {
    /// `ffmpeg_attention`: opened because recording or conversion needed
    /// FFmpeg and found none, so its row starts lit.
    pub fn new(loop_target: &ActiveEventLoop, current: AppSettings, ffmpeg_attention: bool) -> Result<Self> {
        let system_page = crate::system_shortcuts::page();
        let guide_shown = shows_windows_guide(system_page, &current);
        // Something needed FFmpeg: open where it is set up.
        let tab = if ffmpeg_attention { Tab::Files } else { Tab::Captures };
        let rows = Self::build_rows(system_page, guide_shown, tab);
        let win_h = window_height(system_page);
        let (cx, cy) = centered_origin(loop_target, WIN_W, win_h);
        let attrs = crate::chrome::frameless(WindowAttributes::default())
            .with_title("Frame Fang: Settings")
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
            baseline: current.clone(),
            imported: false,
            tab,
            draft: current,
            rows,
            guide_shown,
            cursor: (0, 0),
            hover: None,
            capturing_hotkey: None,
            listening_hold: None,
            hold_refused: None,
            hotkey_before_capture: None,
            mods_state: ModifiersState::empty(),
            error: None,
            system_page,
            system_takes_capture: false,
            capture_hint: None,
            switch_name: crate::system_shortcuts::switch_name(),
            switch_page: crate::system_shortcuts::windows_page_path(),
            ffmpeg_found: None,
            ffmpeg_attention,
            fetch_seen: crate::ffmpeg_fetch::state(),
            title_bar: Default::default(),
            status: None,
            outcome: Default::default(),
        };
        me.refresh_system_takes();
        me.ffmpeg_found = me.find_ffmpeg();
        me.redraw();
        Ok(me)
    }

    /// The FFmpeg recording and conversion would use with the draft: its
    /// pick while that file exists, else the one found by itself.
    fn find_ffmpeg(&self) -> Option<PathBuf> {
        framefang_platform::recorder::locate_ffmpeg_with(self.draft.chosen_ffmpeg().as_deref())
    }

    pub fn window_id(&self) -> WindowId { self.window.id() }

    /// The settings as the tray now has them (after any save, a hand edit of
    /// `settings.json`, or a refused change): show them, and send later
    /// changes as differences from them. A rebind in progress is left
    /// listening, and Esc there now goes back to this binding.
    pub fn replace_draft(&mut self, s: AppSettings) {
        self.baseline = s.clone();
        self.imported = false;
        if let Some(a) = self.capturing_hotkey { self.hotkey_before_capture = s.hotkey_for(a); }
        if s == self.draft { return; }
        self.draft = s;
        crate::palette::set(crate::palette::Scheme::parse(&self.draft.theme));
        self.ffmpeg_found = self.find_ffmpeg();
        self.relayout();
        self.window.request_redraw();
    }

    /// Answers for the tray since the last call, oldest first.
    pub fn take_outcomes(&mut self) -> Vec<SettingsOutcome> {
        self.outcome.drain(..).collect()
    }

    /// Hand the tray what changed, if `before` differs from the draft.
    fn report_change(&mut self, before: &AppSettings) {
        if self.draft == *before { self.imported = false; return; }
        if std::mem::take(&mut self.imported) {
            self.outcome.push_back(SettingsOutcome::Replace(self.draft.clone()));
        } else {
            let patch = framefang_core::settings_store::diff(&self.baseline, &self.draft);
            if !patch.is_empty() { self.outcome.push_back(SettingsOutcome::Patch(patch)); }
        }
        self.baseline = self.draft.clone();
    }

    /// Called from the tray loop's poll: redraw when the FFmpeg download
    /// moved on, and look for FFmpeg again once it has finished.
    pub fn tick(&mut self) {
        let now = crate::ffmpeg_fetch::state();
        if now == self.fetch_seen { return; }
        if now == crate::ffmpeg_fetch::Fetch::Done {
            self.ffmpeg_found = self.find_ffmpeg();
        }
        self.fetch_seen = now;
        self.window.request_redraw();
    }

    /// Something needed FFmpeg while Settings was already open: light the row.
    pub fn draw_attention_to_ffmpeg(&mut self) {
        self.ffmpeg_found = self.find_ffmpeg();
        self.ffmpeg_attention = self.ffmpeg_found.is_none();
        self.switch_tab(Tab::Files);
        self.window.focus_window();
        self.window.request_redraw();
    }

    /// The Download button can be pressed: Windows, no FFmpeg, nothing
    /// downloading.
    fn ffmpeg_actionable(&self) -> bool {
        crate::ffmpeg_fetch::offered() && self.ffmpeg_found.is_none() && !crate::ffmpeg_fetch::busy()
            && self.draft.chosen_ffmpeg().is_none()
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
        // The window keeps its size: it was made for the tallest page.
        self.rows = Self::build_rows(self.system_page, guide, self.tab);
        self.hover = None;
        self.window.request_redraw();
    }

    /// Show page `t`. A rebind still listening on the page left behind is
    /// cancelled first, as Esc would, so no key binds a row out of sight.
    fn switch_tab(&mut self, t: Tab) {
        self.stop_hold_listen();
        if let Some(action) = self.capturing_hotkey.take() {
            self.draft.set_hotkey_for(action, self.hotkey_before_capture);
            self.capture_hint = None;
            self.error = None;
            self.relayout();
        }
        if t == self.tab { return; }
        self.tab = t;
        self.rows = Self::build_rows(self.system_page, self.guide_shown, t);
        // What is under the pointer now.
        self.hover = self.hit_test(self.cursor.0, self.cursor.1);
        self.window.request_redraw();
    }

    pub fn handle_event(&mut self, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => {
                self.outcome.push_back(SettingsOutcome::Closed);
            }
            // Back from the system page: the setting may have changed.
            WindowEvent::Focused(true) => {
                self.refresh_system_takes();
                // FFmpeg may have been installed meanwhile.
                let found = self.find_ffmpeg();
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
                let hit = self.hit_test(self.cursor.0, self.cursor.1);
                // A click anywhere but the waiting key stops it waiting.
                if self.listening_hold.is_some()
                    && !hit.is_some_and(|i| matches!(self.rows[i].kind, WidgetKind::HoldKey(_))) {
                    self.stop_hold_listen();
                }
                if let Some(i) = hit {
                    self.activate(i);
                } else {
                    crate::chrome::press_off_controls(&self.window, self.cursor.1, 0);
                }
            }
            WindowEvent::KeyboardInput { event: key_ev, .. } => {
                if self.listening_hold.is_some() {
                    self.on_hold_key(key_ev);
                } else if self.capturing_hotkey.is_some() {
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
        // Esc doesn't close the window (Brandon 2026-11-19): it closes
        // from its title bar like any other window.
        // Ctrl+Tab and Ctrl+Shift+Tab go through the pages (a listening
        // rebind takes every key before this is reached).
        if ev.logical_key == Key::Named(NamedKey::Tab) && self.mods_state.control_key() {
            let by = if self.mods_state.shift_key() { -1 } else { 1 };
            self.switch_tab(self.tab.step(by));
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
    /// A hold line's key is waiting: the next press binds it. Esc gives up;
    /// a key the capture screen or the system already uses is refused, and
    /// the line says so while it keeps waiting.
    fn on_hold_key(&mut self, ev: winit::event::KeyEvent) {
        if ev.state != ElementState::Pressed || ev.repeat { return; }
        let Some(hold) = self.listening_hold else { return };
        if ev.logical_key == Key::Named(NamedKey::Escape) {
            self.stop_hold_listen();
            return;
        }
        let Some(vk) = vk_of(ev.physical_key) else { return };
        match SwitchKey::from_vk(vk) {
            Some(key) => {
                let before = self.draft.clone();
                hold.set(&mut self.draft, key);
                self.stop_hold_listen();
                self.report_change(&before);
            }
            None => {
                let name = framefang_core::settings::vk_label(vk).unwrap_or("That key");
                self.hold_refused = Some(format!("{name} is in use"));
                self.window.request_redraw();
            }
        }
    }

    fn stop_hold_listen(&mut self) {
        if self.listening_hold.take().is_some() || self.hold_refused.take().is_some() {
            self.window.request_redraw();
        }
    }

    fn on_capture_key(&mut self, ev: winit::event::KeyEvent) {
        if ev.state != ElementState::Pressed { return; }
        let Some(action) = self.capturing_hotkey else { return; };
        // Esc always aborts capture and reverts the draft.
        if matches!(ev.logical_key, Key::Named(NamedKey::Escape)) {
            self.draft.set_hotkey_for(action, self.hotkey_before_capture);
            self.capturing_hotkey = None;
            self.capture_hint = None;
            self.error = None;
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
        let before = self.draft.clone();
        self.draft.set_hotkey_for(action, Some(hk));
        // A chord another action already has is refused as it is pressed,
        // and the box keeps listening for another key. The OS can't share
        // one: on Windows the second `RegisterHotKey` just fails, and under
        // X11 whichever grab matches first silently wins.
        if let Some((a, b)) = self.draft.hotkey_conflict() {
            let chord = self.draft.hotkey_for(a).unwrap_or_default();
            self.error = Some(conflict_message(a, b, chord));
            self.draft = before;
            self.window.request_redraw();
            return;
        }
        self.capturing_hotkey = None;
        self.error = None;
        // Print Screen in or out: the Windows card comes or goes with it.
        self.relayout();
        self.report_change(&before);
        self.window.request_redraw();
    }

    fn hit_test(&self, x: i32, y: i32) -> Option<usize> {
        // Reverse so action-bar buttons on top of any overlapping layout win.
        self.rows.iter().enumerate().rev().find_map(|(i, r)| {
            // A hotkey error takes the system line's place, button included.
            if r.kind == WidgetKind::SystemShortcut && self.error.is_some() { return None; }
            // Only a label; its chips are the controls.
            if matches!(r.kind, WidgetKind::Theme | WidgetKind::SettingsFile | WidgetKind::AfterCapture
                | WidgetKind::ScrollAfter | WidgetKind::FileNames
                | WidgetKind::RecordFormat | WidgetKind::Frame | WidgetKind::QrLinks
                | WidgetKind::HoldLine(_) | WidgetKind::Card(_) | WidgetKind::Divider) { return None; }
            // A hold line's key: the key as drawn, a little room around it.
            if let WidgetKind::HoldKey(hold) = r.kind {
                let name = hold_key_name(hold, &self.draft, self.listening_hold == Some(hold));
                let (kx, ky, kw, kh) = hold_key_rect(r, &name);
                let hit = x >= kx - 4 && x < kx + kw + 4 && y >= ky - 4 && y < ky + kh + 4;
                return hit.then_some(i);
            }
            // A listening row's CLEAR is hidden, and so can't be pressed.
            if let WidgetKind::HotkeyClear(a) = r.kind { if self.capturing_hotkey == Some(a) { return None; } }
            // A read-out, and a button that is live only when there is
            // something to download.
            if r.kind == WidgetKind::Ffmpeg { return None; }
            if r.kind == WidgetKind::FfmpegDownload && !self.ffmpeg_actionable() { return None; }
            if r.kind == WidgetKind::FfmpegAuto && self.draft.chosen_ffmpeg().is_none() { return None; }
            let (rx, ry, rw, rh) = r.rect;
            if x >= rx && x < rx + rw && y >= ry && y < ry + rh { Some(i) } else { None }
        })
    }

    fn activate(&mut self, idx: usize) {
        let kind = self.rows[idx].kind;
        // Every control that changes a setting reports it as it happens.
        let before = self.draft.clone();
        match kind {
            WidgetKind::ExportBtn => self.export(),
            WidgetKind::ImportBtn => self.import(),
            WidgetKind::Ffmpeg => {}
            WidgetKind::FfmpegDownload => {
                crate::ffmpeg_fetch::start();
                self.ffmpeg_attention = false;
            }
            WidgetKind::FfmpegBrowse => {
                let start = self.ffmpeg_found.as_ref().and_then(|p| p.parent().map(|d| d.to_path_buf()))
                    .unwrap_or_else(std::env::temp_dir);
                let mut dialog = rfd::FileDialog::new()
                    .set_title("Frame Fang: FFmpeg to use")
                    .set_directory(&start);
                if cfg!(windows) { dialog = dialog.add_filter("FFmpeg", &["exe"]); }
                if let Some(p) = dialog.pick_file() {
                    self.draft.ffmpeg_path = p.to_string_lossy().to_string();
                    self.ffmpeg_found = self.find_ffmpeg();
                    self.ffmpeg_attention = false;
                }
            }
            WidgetKind::FfmpegAuto => {
                self.draft.ffmpeg_path.clear();
                self.ffmpeg_found = self.find_ffmpeg();
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
                    .set_title("Frame Fang: Screenshots folder")
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
                    .set_title("Frame Fang: Recordings folder")
                    .set_directory(&starting)
                    .pick_folder()
                {
                    self.draft.recordings_directory = p.to_string_lossy().to_string();
                }
            }
            WidgetKind::Theme | WidgetKind::SettingsFile => {}
            WidgetKind::ThemeChoice(scheme) => {
                self.draft.theme = scheme.name().to_owned();
                // The whole window redraws in it now, so the choice is seen
                // before it is saved.
                crate::palette::set(scheme);
            }
            WidgetKind::AfterCapture | WidgetKind::ScrollAfter => {}
            WidgetKind::AfterCaptureChoice(after) => {
                self.draft.set_after_capture(after);
            }
            WidgetKind::ScrollAfterChoice(after) => {
                self.draft.set_scroll_after(after);
            }
            WidgetKind::HoldKey(hold) | WidgetKind::HoldRebind(hold) => {
                // Click again to give up; otherwise wait for a key (a
                // hotkey rebind that was waiting gives way).
                if self.listening_hold == Some(hold) {
                    self.stop_hold_listen();
                } else {
                    if let Some(action) = self.capturing_hotkey.take() {
                        self.draft.set_hotkey_for(action, self.hotkey_before_capture);
                        self.capture_hint = None;
                    }
                    self.listening_hold = Some(hold);
                    self.hold_refused = None;
                }
            }
            WidgetKind::KeepRecent => {
                self.draft.keep_recent = !self.draft.keep_recent;
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
                self.error = None;
            }
            WidgetKind::HotkeyClear(action) => {
                // Only laid out for the optional actions, so this can never
                // strip the mandatory capture binding.
                self.draft.set_hotkey_for(action, None);
                self.capturing_hotkey = None;
                self.error = None;
            }
            WidgetKind::OpenJson => {
                self.outcome.push_back(SettingsOutcome::OpenJson);
                return;
            }
            WidgetKind::Tab(t) => {
                self.switch_tab(t);
                return;
            }
            WidgetKind::FileNames | WidgetKind::RecordFormat | WidgetKind::Frame | WidgetKind::QrLinks
            | WidgetKind::HoldLine(_) | WidgetKind::Card(_) | WidgetKind::Divider => {}
            WidgetKind::FileNameChoice(choice) => {
                use framefang_core::file_name::{preset_index, PRESETS};
                let custom_now = preset_index(&self.draft.file_name_pattern).is_none();
                match choice {
                    Some(i) => {
                        // Leaving a hand-made pattern keeps it for CUSTOM.
                        if custom_now { self.draft.file_name_custom = self.draft.file_name_pattern.clone(); }
                        self.draft.file_name_pattern = PRESETS[i].1.to_owned();
                    }
                    None => {
                        if !custom_now {
                            // Back to the kept pattern, or a copy of this
                            // preset to start from, so the file has one to edit.
                            let kept = self.draft.file_name_custom.clone();
                            self.draft.file_name_pattern =
                                if kept.trim().is_empty() { self.draft.file_name_pattern.clone() } else { kept };
                            self.draft.file_name_custom = self.draft.file_name_pattern.clone();
                        }
                        // The pattern is edited in settings.json; saved first.
                        self.report_change(&before);
                        self.outcome.push_back(SettingsOutcome::OpenJson);
                        self.window.request_redraw();
                        return;
                    }
                }
            }
            WidgetKind::RecordFormatChoice(gif) => {
                if gif && self.ffmpeg_found.is_none() {
                    // A GIF needs FFmpeg: point at where it is set up.
                    self.ffmpeg_attention = true;
                } else {
                    self.draft.record_format =
                        if gif { "GIF" } else { "MP4" }.to_owned();
                }
            }
            WidgetKind::FrameChoice(f) => {
                self.draft.output_frame = f.name().to_owned();
            }
            WidgetKind::QrLinksChoice(open) => {
                self.draft.open_qr_links = open;
            }
            WidgetKind::SystemShortcut => {
                if let Some(page) = self.system_page {
                    if let Err(e) = page.open() {
                        self.error = Some(e);
                    }
                }
            }
        }
        self.report_change(&before);
        self.window.request_redraw();
    }

    /// Save a copy of the settings where the user picks, as JSON.
    fn export(&mut self) {
        let dir = directories::UserDirs::new()
            .and_then(|u| u.document_dir().map(|p| p.to_path_buf()))
            .unwrap_or_else(std::env::temp_dir);
        let Some(path) = rfd::FileDialog::new()
            .set_title("Frame Fang: Export settings")
            .set_directory(&dir)
            .set_file_name("Frame Fang settings.json")
            .add_filter("Settings (JSON)", &["json"])
            .save_file()
        else { return };
        self.status = Some(match self.draft.write_to(&path) {
            Ok(()) => (format!("Exported to {}", path.display()), false),
            Err(e) => (format!("Couldn't export: {e}"), true),
        });
    }

    /// Replace every setting with the ones in a file the user picks. A file
    /// that isn't Frame Fang settings changes nothing and says why.
    fn import(&mut self) {
        let dir = directories::UserDirs::new()
            .and_then(|u| u.document_dir().map(|p| p.to_path_buf()))
            .unwrap_or_else(std::env::temp_dir);
        let Some(path) = rfd::FileDialog::new()
            .set_title("Frame Fang: Import settings")
            .set_directory(&dir)
            .add_filter("Settings (JSON)", &["json"])
            .pick_file()
        else { return };
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        match AppSettings::read_from(&path) {
            Ok(s) => {
                self.draft = s;
                self.imported = true;
                crate::palette::set(crate::palette::Scheme::parse(&self.draft.theme));
                self.ffmpeg_found = self.find_ffmpeg();
                self.capturing_hotkey = None;
                self.error = None;
                self.relayout();
                self.status = Some((format!("Imported {name}"), false));
            }
            Err(e) => self.status = Some((format!("Couldn't import {name}: {e}"), true)),
        }
    }

    /// Lay out section headers + every row. Sections are spaced apart with
    /// a single section-rule line between them. Row rect spans the full
    /// content width; the value half is computed inside `render_row`.
    ///
    /// Action buttons (Save / Cancel / Edit as JSON) are pinned to the
    /// right side of the header band at the very top — clicking them is
    /// the user's commit/abort action so they want to be reachable
    /// without scrolling, like a real app's toolbar.
    fn build_rows(system_page: Option<crate::system_shortcuts::Page>, windows_guide: bool, tab: Tab) -> Vec<Row> {
        let mut rows = Vec::new();
        // Setting rows span the page, the tab strip's width.
        let x = PAD;
        let row_w = WIN_W as i32 - 2 * PAD;
        let mut tx = PAD;
        for t in Tab::ALL {
            rows.push(Row { kind: WidgetKind::Tab(t), label: t.label(), rect: (tx, STRIP_Y, tab_w(t), TAB_H) });
            tx += tab_w(t) + TAB_GAP;
        }

        // A row of chips on a labelled row, starting at the value column.
        // Its chips share the value column evenly, out to the right edge the
        // folder fields and REBIND buttons end at (Brandon 2026-11-19: small
        // chips left every page empty on its right).
        let chip_row = |rows: &mut Vec<Row>, y: i32, list: &[(WidgetKind, &'static str)]| {
            let (left, right) = (x + LABEL_W, x + row_w - 4);
            let n = list.len() as i32;
            let w = (right - left - CHIP_GAP * (n - 1)) / n;
            for (i, &(kind, label)) in list.iter().enumerate() {
                let cx = left + i as i32 * (w + CHIP_GAP);
                let cw = if i as i32 == n - 1 { right - cx } else { w };
                rows.push(Row { kind, label, rect: (cx, y + 4, cw, ROW_H - 8) });
            }
        };
        // A hold line, right under the choice it flips: the sentence, and its
        // key (laid over it; `hit_test` finds the key as drawn).
        let hold_line = |rows: &mut Vec<Row>, hold: Hold, y: &mut i32| {
            rows.push(Row { kind: WidgetKind::HoldLine(hold), label: "", rect: (x, *y, row_w, HOLD_ROW_H) });
            rows.push(Row { kind: WidgetKind::HoldKey(hold), label: "", rect: (x, *y, row_w, HOLD_ROW_H) });
            rows.push(Row { kind: WidgetKind::HoldRebind(hold), label: "REBIND",
                            rect: (x + row_w - 4 - HOLD_BTN_W, *y + (HOLD_ROW_H - HOLD_BTN_H) / 2, HOLD_BTN_W, HOLD_BTN_H) });
            *y += HOLD_ROW_H;
        };

        // A page's rows start under the tab strip.
        let mut y = PAGE_TOP;

        if tab == Tab::Files {
        // FILES: where captures and recordings go, then what they are
        // called and made as.
        let c = CardBuild::open(&mut y);
        rows.push(Row { kind: WidgetKind::SaveFolder,       label: "Screenshots folder", rect: (x, y, row_w, ROW_H) }); y += ROW_H;
        divider(&mut rows, &mut y);
        rows.push(Row { kind: WidgetKind::RecordingsFolder, label: "Recordings folder",  rect: (x, y, row_w, ROW_H) }); y += ROW_H;
        divider(&mut rows, &mut y);
        // FFmpeg, which recording and video conversion run: which one, then
        // Download or Auto in one slot, then Browse... (`render_ffmpeg_row`).
        rows.push(Row { kind: WidgetKind::Ffmpeg, label: "FFmpeg", rect: (x, y, row_w, ROW_H) });
        let browse_x = x + row_w - 4 - BROWSE_W;
        let slot_x = browse_x - 8 - FFMPEG_BTN_W;
        rows.push(Row { kind: WidgetKind::FfmpegDownload, label: "DOWNLOAD",
                        rect: (slot_x, y + 4, FFMPEG_BTN_W, ROW_H - 8) });
        rows.push(Row { kind: WidgetKind::FfmpegAuto, label: "Auto",
                        rect: (browse_x - 8 - AUTO_W, y + 4, AUTO_W, ROW_H - 8) });
        rows.push(Row { kind: WidgetKind::FfmpegBrowse, label: "Browse...",
                        rect: (browse_x, y + 4, BROWSE_W, ROW_H - 8) });
        y += ROW_H;
        c.close(&mut rows, &mut y);
        let c = CardBuild::open(&mut y);
        // File names: three presets and CUSTOM, with the next name under
        // the label.
        rows.push(Row { kind: WidgetKind::FileNames, label: "File names", rect: (x, y, row_w, ROW_H) });
        let presets = framefang_core::file_name::PRESETS;
        let mut list: Vec<(WidgetKind, &'static str)> = presets.iter().enumerate()
            .map(|(i, (word, _))| (WidgetKind::FileNameChoice(Some(i)), *word)).collect();
        list.push((WidgetKind::FileNameChoice(None), "CUSTOM"));
        chip_row(&mut rows, y, &list);
        y += ROW_H;
        divider(&mut rows, &mut y);
        rows.push(Row { kind: WidgetKind::RecordFormat, label: "Record as", rect: (x, y, row_w, ROW_H) });
        chip_row(&mut rows, y, &[(WidgetKind::RecordFormatChoice(false), "MP4"),
                                 (WidgetKind::RecordFormatChoice(true), "GIF")]);
        y += ROW_H;
        divider(&mut rows, &mut y);
        rows.push(Row { kind: WidgetKind::Frame, label: "Frame", rect: (x, y, row_w, ROW_H) });
        {
            use framefang_core::settings::OutputFrame;
            chip_row(&mut rows, y, &[(WidgetKind::FrameChoice(OutputFrame::None), "NONE"),
                                     (WidgetKind::FrameChoice(OutputFrame::Border), "BORDER"),
                                     (WidgetKind::FrameChoice(OutputFrame::Shadow), "SHADOW")]);
        }
        y += ROW_H;
        c.close(&mut rows, &mut y);
        // What Frame Fang keeps by itself: the tray's RECENT list.
        let c = CardBuild::open(&mut y);
        rows.push(Row { kind: WidgetKind::KeepRecent, label: "Keep the last 10 captures", rect: (x, y, row_w, ROW_H) });
        y += ROW_H;
        c.close(&mut rows, &mut y);
        }

        if tab == Tab::Captures {
        // After a capture: edit, copy and edit, or just copy (auto-copy
        // folded in, Brandon 2026-11-20). On the card's sub-strip, the key
        // that flips one capture between the editor and copying (one quiet
        // sentence, the key a keycap to click and rebind), then what the
        // editor's Copy does.
        let mut c = CardBuild::open(&mut y);
        rows.push(Row { kind: WidgetKind::AfterCapture, label: "After a capture", rect: (x, y, row_w, ROW_H) });
        {
            use framefang_core::settings::AfterCapture;
            let list: Vec<(WidgetKind, &'static str)> = AfterCapture::ALL.into_iter()
                .map(|a| (WidgetKind::AfterCaptureChoice(a), a.label())).collect();
            chip_row(&mut rows, y, &list);
        }
        y += ROW_H;
        c.sub(&mut y);
        hold_line(&mut rows, Hold::Capture, &mut y);
        rows.push(Row { kind: WidgetKind::CloseAfterCopy, label: "Close the editor after copying", rect: (x, y, row_w, ROW_H) }); y += ROW_H;
        c.close(&mut rows, &mut y);
        // SCROLL mode, right under it: a long page is usually worth a look,
        // so it has its own choice (Copy + edit unless changed). The switch
        // key flips it as it does a region.
        if crate::scroll::supported() {
            let mut c = CardBuild::open(&mut y);
            rows.push(Row { kind: WidgetKind::ScrollAfter, label: "After a scrolling capture", rect: (x, y, row_w, ROW_H) });
            {
                use framefang_core::settings::AfterCapture;
                let list: Vec<(WidgetKind, &'static str)> = AfterCapture::ALL.into_iter()
                    .map(|a| (WidgetKind::ScrollAfterChoice(a), a.label())).collect();
                chip_row(&mut rows, y, &list);
            }
            y += ROW_H;
            c.sub(&mut y);
            hold_line(&mut rows, Hold::Scroll, &mut y);
            c.close(&mut rows, &mut y);
        }
        // What goes into the picture, before where it goes.
        if framefang_platform::cursor::SUPPORTED {
            let c = CardBuild::open(&mut y);
            rows.push(Row { kind: WidgetKind::CaptureCursor, label: "Include the mouse pointer", rect: (x, y, row_w, ROW_H) }); y += ROW_H;
            c.close(&mut rows, &mut y);
        }
        // QR mode: what a web link in a code does (other codes are copied),
        // and its own key on the sub-strip.
        let mut c = CardBuild::open(&mut y);
        rows.push(Row { kind: WidgetKind::QrLinks, label: "Links in QR codes", rect: (x, y, row_w, ROW_H) });
        chip_row(&mut rows, y, &[(WidgetKind::QrLinksChoice(false), "Copy"),
                                 (WidgetKind::QrLinksChoice(true), "Open in browser")]);
        y += ROW_H;
        c.sub(&mut y);
        hold_line(&mut rows, Hold::Qr, &mut y);
        c.close(&mut rows, &mut y);
        }

        if tab == Tab::Hotkeys {
        // HOTKEYS — one row per action: a display box filling the left part
        // of the value column, REBIND anchored to the right edge, and CLEAR
        // just inside it on the optional actions. The buttons are their
        // own rows laid out over the display row's rect; `hit_test` scans in
        // reverse, so they win the click.
        let c = CardBuild::open(&mut y);
        for (i, action) in HotkeyAction::ALL.into_iter().enumerate() {
            if i > 0 { divider(&mut rows, &mut y); }
            rows.push(Row { kind:  WidgetKind::HotkeyDisplay(action),
                            label: action.label(),
                            rect:  (x, y, row_w, ROW_H) });
            // As tall as the key box and the chips elsewhere, ending where
            // every other row's controls end.
            let rebind_x = x + row_w - 4 - REBIND_W;
            rows.push(Row { kind:  WidgetKind::HotkeyRebind(action),
                            label: "REBIND",
                            rect:  (rebind_x, y + 4, REBIND_W, ROW_H - 8) });
            if action.is_optional() {
                rows.push(Row { kind:  WidgetKind::HotkeyClear(action),
                                label: "CLEAR",
                                rect:  (rebind_x - 8 - CLEAR_W, y + 4, CLEAR_W, ROW_H - 8) });
            }
            y += ROW_H;
        }
        c.close(&mut rows, &mut y);
        // `close` left a card's gap; the code below adds its own.
        y -= CARD_GAP - ROW_GAP;
        // Taller than the usual inter-section gap: the system-shortcut line
        // (or, in its place, the hotkey-conflict message) renders in here,
        // under the last binding row.
        match system_page {
            // Windows, capture key isn't Print Screen: Windows' switch
            // doesn't touch it, so no card; the next heading sits as far
            // below the last row as the other headings do.
            Some(page) if page.is_windows() && !windows_guide => y += SECTION_ABOVE - ROW_GAP,
            Some(page) if page.is_windows() => {
                // The guide card; its button sits in the card's top-right.
                let w = bitmap_font::measure(page.button, 1) + 24;
                // Right under the last hotkey row, a row gap away, so the
                // dialog still fits a 768 px screen with the FFmpeg row.
                let top = y;
                rows.push(Row { kind:  WidgetKind::SystemShortcut,
                                label: page.button,
                                rect:  (WIN_W as i32 - PAD - CARD_PAD - w, top + CARD_PAD, w, CARD_BTN_H) });
                // The page then ends as far below the card as
                // the other headings sit below their rows.
                y = top + CARD_H + SECTION_ABOVE;
            }
            Some(page) => {
                let w = bitmap_font::measure(page.button, 1) + 24;
                rows.push(Row { kind:  WidgetKind::SystemShortcut,
                                label: page.button,
                                rect:  (x + row_w - w, y, w, SYSTEM_BTN_H) });
                y += SYSTEM_BTN_H + SECTION_ABOVE;
            }
            None => y += SECTION_ABOVE - ROW_GAP,
        }
        }

        if tab == Tab::General {
        // How the app behaves, how it looks, then its settings file
        // (Brandon 2026-11-20: the file's buttons left the header, and Start
        // with system left a page named for looks).
        let c = CardBuild::open(&mut y);
        rows.push(Row { kind: WidgetKind::StartWithOs, label: "Start with system", rect: (x, y, row_w, ROW_H) }); y += ROW_H;
        c.close(&mut rows, &mut y);
        let c = CardBuild::open(&mut y);
        rows.push(Row { kind: WidgetKind::Theme, label: "Theme", rect: (x, y, row_w, ROW_H) });
        // One chip per scheme across the value column, a swatch and a word.
        let chips = crate::palette::Scheme::ALL;
        let chip_w = (row_w - LABEL_W - 4 - THEME_CHIP_GAP * (chips.len() as i32 - 1)) / chips.len() as i32;
        for (i, scheme) in chips.into_iter().enumerate() {
            let cx = x + LABEL_W + i as i32 * (chip_w + THEME_CHIP_GAP);
            rows.push(Row { kind: WidgetKind::ThemeChoice(scheme), label: scheme.name(),
                            rect: (cx, y + 4, chip_w, ROW_H - 8) });
        }
        y += ROW_H;
        c.close(&mut rows, &mut y);
        let c = CardBuild::open(&mut y);
        rows.push(Row { kind: WidgetKind::SettingsFile, label: "Settings file", rect: (x, y, row_w, ROW_H) });
        chip_row(&mut rows, y, &[(WidgetKind::OpenJson, "Edit as JSON"),
                                 (WidgetKind::ImportBtn, "Import..."),
                                 (WidgetKind::ExportBtn, "Export...")]);
        y += ROW_H;
        c.close(&mut rows, &mut y);
        }

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

        // Pure black body under the title bar (which holds the heading),
        // the glowing rule along its top (`hud`).
        crate::hud::backdrop(&mut buf, win_w, win_h, 0);

        let mut surf = BufferSurface { buf: &mut buf, w: win_w as i32, h: win_h as i32 };

        // What the settings file's last Export or Import did, under its
        // buttons on GENERAL.
        if let (Some((msg, failed)), Some(r)) = (&self.status, self.rows.iter().find(|r| r.kind == WidgetKind::SettingsFile)) {
            let color = if *failed { DANGER() } else { OK_TINT };
            let x = r.rect.0 + LABEL_W;
            let line_w = r.rect.0 + r.rect.2 - 4 - x;
            draw_text(&mut surf, x, r.rect.1 + r.rect.3 + STATUS_GAP, 1,
                      &truncate_for(msg, line_w), argb_to_framefang(color));
        }

        // The tab strip: the page shown is a raised tab joined to the page,
        // the others sit back on the strip's line.
        let right = WIN_W as i32 - PAD;
        let strip_base = STRIP_Y + TAB_H;
        fill_rect(&mut surf, PAD, strip_base - 1, right - PAD, 1, argb_to_framefang(HEADER_RULE()));
        {
            let hint_w = crate::keycap::line_width(TAB_HINT, 1);
            let hint_y = STRIP_Y + (TAB_H - bitmap_font::GLYPH_H) / 2;
            crate::keycap::draw_line_on(&mut surf, right - hint_w, hint_y, TAB_HINT, 1, TEXT_DIM());
        }
        let capturing = self.capturing_hotkey;
        // The lines first, under every row: a hairline between settings, and
        // each group's tree line to its sub-settings.
        let mut first_group = true;
        for row in &self.rows {
            match row.kind {
                WidgetKind::Card(sub) => {
                    let (rx, ry, rw, _) = row.rect;
                    if !first_group {
                        fill_rect(&mut surf, rx, ry - CARD_GAP / 2, rw, 1, argb_to_framefang(PANEL_BORDER()));
                    }
                    first_group = false;
                    if let Some(sy) = sub { render_tree(&mut surf, row, sy, &self.rows); }
                }
                WidgetKind::Divider => {
                    let (rx, ry, rw, rh) = row.rect;
                    fill_rect(&mut surf, rx, ry, rw, rh, argb_to_framefang(PANEL_BORDER()));
                }
                _ => {}
            }
        }
        for (i, row) in self.rows.iter().enumerate() {
            if matches!(row.kind, WidgetKind::Card(_) | WidgetKind::Divider) { continue; }
            if let WidgetKind::HoldRebind(hold) = row.kind {
                render_rebind_button(&mut surf, row, self.listening_hold == Some(hold), self.hover == Some(i));
                continue;
            }
            let hovered = self.hover == Some(i);
            if let WidgetKind::Tab(t) = row.kind {
                render_tab(&mut surf, row, t == self.tab, hovered);
                continue;
            }
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
            if let WidgetKind::HoldLine(hold) = row.kind {
                let key_hovered = self.hover.is_some_and(|h| self.rows[h].kind == WidgetKind::HoldKey(hold));
                render_hold_line(&mut surf, row, hold, &self.draft, self.listening_hold == Some(hold),
                                 key_hovered, self.hold_refused.as_deref());
                continue;
            }
            if matches!(row.kind, WidgetKind::HoldKey(_)) { continue; }
            if matches!(row.kind, WidgetKind::AfterCaptureChoice(_) | WidgetKind::ScrollAfterChoice(_) | WidgetKind::FileNameChoice(_) | WidgetKind::RecordFormatChoice(_) | WidgetKind::FrameChoice(_)
                | WidgetKind::QrLinksChoice(_)) {
                let unavailable = row.kind == WidgetKind::RecordFormatChoice(true) && self.ffmpeg_found.is_none();
                render_choice_chip(&mut surf, row, hovered, &self.draft, unavailable);
                continue;
            }
            if row.kind == WidgetKind::RecordFormat && self.ffmpeg_found.is_none() {
                render_label_with_caption(&mut surf, row, "GIF needs FFmpeg (above)");
                continue;
            }
            if row.kind == WidgetKind::FileNames {
                let next = framefang_core::file_name::render(&self.draft.file_name_pattern,
                    &framefang_core::file_name::NameFacts {
                        now: chrono::Local::now(), app: "Chrome".into(), title: "Notes".into(), w: 1280, h: 720,
                    });
                let caption = truncate_for(&format!("Next: {next}.png"), LABEL_W - 12);
                render_label_with_caption(&mut surf, row, &caption);
                continue;
            }
            if row.kind == WidgetKind::Ffmpeg {
                let find = |k: WidgetKind| self.rows.iter().position(|r| r.kind == k).map(|b| (&self.rows[b], self.hover == Some(b)));
                let chosen = self.draft.chosen_ffmpeg();
                let ff = FfmpegRow {
                    found: self.ffmpeg_found.as_deref(),
                    chosen: chosen.as_deref(),
                    fetch: &self.fetch_seen,
                    button: find(WidgetKind::FfmpegDownload).filter(|_| ffmpeg_actionable),
                    auto: find(WidgetKind::FfmpegAuto).filter(|_| chosen.is_some()),
                    browse: find(WidgetKind::FfmpegBrowse),
                    attention: self.ffmpeg_attention && self.ffmpeg_found.is_none(),
                };
                render_ffmpeg_row(&mut surf, row, &ff);
                continue;
            }
            // Drawn with their row.
            if matches!(row.kind, WidgetKind::FfmpegDownload | WidgetKind::FfmpegAuto | WidgetKind::FfmpegBrowse) { continue; }
            render_row(&mut surf, row, hovered, capturing, self.capture_hint.as_deref(), &self.draft);
        }

        // Inline validation, in the same danger tint the convert dialogs
        // use — but parked directly under the HOTKEYS block instead of at
        // the window's bottom edge, because that's the section it is about
        // and the gap under it is already reserved.
        if let Some(msg) = &self.error {
            if let Some(r) = self.rows.iter().rev()
                .find(|r| matches!(r.kind, WidgetKind::HotkeyDisplay(_)))
            {
                draw_text(&mut surf, PAD, r.rect.1 + ROW_H + CARD_IN + 8, 1, msg,
                          argb_to_framefang(DANGER()));
            }
        }

        self.title_bar.draw(&mut frame, win_w, &buf, TITLE, BG_TOP());
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
        ("Print Screen opens the Snipping Tool, not Frame Fang", WARN())
    } else {
        (page.line, TEXT_MUTED())
    };
    draw_text(surf, PAD + 6, text_y, 1, line, argb_to_framefang(line_color));

    let border = if takes { WARN() } else if hovered { LASER_DIM() } else { FIELD_BORDER() };
    if hovered {
        fill_rect(surf, rx, ry, rw, rh, argb_to_framefang(HOVER_FILL()));
    }
    stroke_rect_argb(surf, rx, ry, rw, rh, argb_to_framefang(border));
    let tw = bitmap_font::measure(row.label, 1);
    let color = if takes { WARN() } else { TEXT_BRIGHT };
    draw_text(surf, rx + (rw - tw) / 2, text_y, 1, row.label, argb_to_framefang(color));
}

/// What the FFmpeg row shows.
struct FfmpegRow<'a> {
    /// The FFmpeg that will be used.
    found: Option<&'a std::path::Path>,
    /// The one picked in Settings, whether or not it still exists.
    chosen: Option<&'a std::path::Path>,
    fetch: &'a crate::ffmpeg_fetch::Fetch,
    /// The Download button and whether it is hovered, when it can be pressed.
    button: Option<(&'a Row, bool)>,
    /// The Auto button, while an FFmpeg is picked.
    auto: Option<(&'a Row, bool)>,
    browse: Option<(&'a Row, bool)>,
    /// Lit because recording or conversion just needed FFmpeg.
    attention: bool,
}

/// What the FFmpeg field says, and in which color, outside a download: the
/// pick in bright text, one found by itself muted (like the folders'
/// defaults), and a pick that is gone in the warning color with what is
/// used instead.
fn ffmpeg_field_text(found: Option<&std::path::Path>, chosen: Option<&std::path::Path>) -> Option<(String, u32)> {
    match (chosen, found) {
        (Some(c), Some(f)) if c == f => Some((f.display().to_string(), TEXT_BRIGHT)),
        (Some(_), Some(f)) => Some((format!("Picked FFmpeg is gone, using {}", f.display()), WARN())),
        (Some(c), None) => Some((format!("Not found: {}", c.display()), WARN())),
        (None, Some(f)) => Some((f.display().to_string(), TEXT_MUTED())),
        (None, None) => None,
    }
}

/// The FFmpeg row: its label, a read-out field, then Download (when there
/// is nothing to use) or Auto (while one is picked), and Browse... While
/// downloading, the field fills from the left like a progress bar.
fn render_ffmpeg_row<S: painter::Surface>(surf: &mut S, row: &Row, ff: &FfmpegRow) {
    use crate::ffmpeg_fetch::Fetch;
    let (rx, ry, rw, rh) = row.rect;
    let label_y = ry + (rh - bitmap_font::GLYPH_H) / 2;
    let label_color = if ff.attention { LASER() } else { TEXT_BRIGHT };
    draw_text(surf, rx + 6, label_y, 1, row.label, argb_to_framefang(label_color));

    let val_x = rx + LABEL_W;
    let (val_y, val_h) = (ry + 4, rh - 8);
    let right = ff.button.or(ff.auto).or(ff.browse).map_or(rx + rw - 4, |(b, _)| b.rect.0 - 8);
    let val_w = right - val_x;

    let border = if ff.attention { LASER() } else { FIELD_BORDER() };
    fill_rect(surf, val_x, val_y, val_w, val_h, argb_to_framefang(FIELD_BG()));
    let ff_text = ffmpeg_field_text(ff.found, ff.chosen);
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
        _ if ff_text.is_some() => ff_text.clone().unwrap_or_default(),
        // Covered by `ff_text` above; kept so the match is complete.
        (Some(path), _) => (path.display().to_string(), TEXT_MUTED()),
        (None, Fetch::Failed(e)) => (format!("Download failed: {e}"), DANGER()),
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
    // Auto and Browse...: the folder rows' quiet outlined buttons.
    for (b, hovered) in ff.auto.into_iter().chain(ff.browse) {
        let (bx, by, bw, bh) = b.rect;
        if hovered { fill_rect(surf, bx, by, bw, bh, argb_to_framefang(HOVER_FILL())); }
        stroke_rect_argb(surf, bx, by, bw, bh, argb_to_framefang(if hovered { LASER_DIM() } else { FIELD_BORDER() }));
        let tw = bitmap_font::measure(b.label, 1);
        draw_text(surf, bx + (bw - tw) / 2, by + (bh - bitmap_font::GLYPH_H) / 2, 1, b.label, argb_to_framefang(TEXT_BRIGHT));
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

/// A page's height with the Windows card drawn whole: the card is taller
/// than its button, the only row it has.
fn page_height(rows: &[Row], windows_guide: bool) -> u32 {
    let card = rows.iter().find(|r| windows_guide && r.kind == WidgetKind::SystemShortcut)
        .map(|r| (r.rect.1 - CARD_PAD + CARD_H + BOTTOM_BAND) as u32).unwrap_or(0);
    content_height(rows).max(card)
}

/// The window's height: made for its tallest page (the Windows card
/// counted), so switching tabs never resizes it.
fn window_height(system_page: Option<crate::system_shortcuts::Page>) -> u32 {
    let guide = system_page.is_some_and(|p| p.is_windows());
    let pages = Tab::ALL.iter().map(|t| page_height(&SettingsView::build_rows(system_page, guide, *t), guide));
    pages.max().unwrap_or(0) + crate::chrome::TITLE_H as u32
}

/// The status line, the page line and the color of the card's edge for `g`.
/// Both say plainly that the switch is about using Print Screen here.
fn guide_text(g: &WindowsGuide) -> (String, String, u32) {
    if g.takes {
        (
            "Windows is using Print Screen for its own capture".to_owned(),
            format!("To use Print Screen here, turn this off in {}:", g.page),
            WARN(),
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
    if matches!(row.kind, WidgetKind::ExportBtn | WidgetKind::ImportBtn | WidgetKind::OpenJson) {
        // No primary button: nothing here commits anything.
        let is_primary = false;
        let border = if hovered { LASER_DIM() } else { PANEL_BORDER() };
        let fill   = if hovered { HOVER_FILL() } else { 0x0000_0000 };
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
        render_rebind_button(surf, row, capturing_hotkey == Some(action), hovered);
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
                    Some(hint) => (hint.to_owned(), WARN()),
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
            let ty = val_y + (val_h - bitmap_font::GLYPH_H) / 2;
            let bound = !listening && draft.hotkey_for(action).is_some();
            let keys = crate::keycap::chord(&text);
            if bound && crate::keycap::line_width(&keys, 1) <= box_w - 16 {
                // The binding as the keys themselves (`keycap`).
                crate::keycap::draw_line_on(surf, val_x + 8, ty, &keys, 1, color);
            } else {
                let truncated = truncate_for(&text, box_w - 16);
                draw_text(surf, val_x + 8, ty, 1, &truncated, argb_to_framefang(color));
            }
        }
        WidgetKind::StartWithOs | WidgetKind::CloseAfterCopy | WidgetKind::CaptureCursor
        | WidgetKind::KeepRecent => {
            let on = match row.kind {
                WidgetKind::KeepRecent      => draft.keep_recent,
                WidgetKind::CloseAfterCopy   => draft.close_after_copy,
                WidgetKind::CaptureCursor    => draft.capture_cursor,
                _                            => draft.start_with_windows,
            };
            // At the row's right edge, where the other controls end.
            draw_toggle(surf, rx + rw - 4 - TOGGLE_W, val_y, val_h, on);
        }
        _ => {}
    }
}

/// A group's tree line: down from its setting's row to the middle of its
/// last sub-setting, with a tick out to each sub-setting's words, so they
/// read as hanging off the setting above.
fn render_tree<S: painter::Surface>(surf: &mut S, group: &Row, sub: i32, rows: &[Row]) {
    let (gx, gy, _, gh) = group.rect;
    let mids: Vec<i32> = rows.iter()
        .filter(|r| matches!(r.kind, WidgetKind::HoldLine(_) | WidgetKind::CloseAfterCopy)
            && r.rect.1 >= sub && r.rect.1 < gy + gh)
        .map(|r| r.rect.1 + r.rect.3 / 2).collect();
    let Some(&last) = mids.last() else { return };
    let color = argb_to_framefang(FIELD_BORDER());
    let x = gx + TREE_X;
    fill_rect(surf, x, sub - 6, 1, last - sub + 7, color);
    for m in mids { fill_rect(surf, x, m, TREE_TICK - TREE_X, 1, color); }
}

/// A REBIND button: an outlined button that fills while it waits for a
/// key, when it reads LISTENING (Hotkeys page and hold lines alike).
fn render_rebind_button<S: painter::Surface>(surf: &mut S, row: &Row, active: bool, hovered: bool) {
    let (rx, ry, rw, rh) = row.rect;
    let border = if active { LASER() } else if hovered { LASER_DIM() } else { FIELD_BORDER() };
    let fill   = if active { PRIMARY_FILL() } else if hovered { HOVER_FILL() } else { 0x0000_0000 };
    if fill != 0 {
        fill_rect(surf, rx, ry, rw, rh, argb_to_framefang(fill));
    }
    stroke_rect_argb(surf, rx, ry, rw, rh, argb_to_framefang(border));
    let label = if active { "LISTENING" } else { row.label };
    let tw = bitmap_font::measure(label, 1);
    let tx = rx + (rw - tw) / 2;
    let ty = ry + (rh - bitmap_font::GLYPH_H) / 2;
    let color = if active { LASER() } else { TEXT_BRIGHT };
    draw_text(surf, tx, ty, 1, label, argb_to_framefang(color));
}

/// A hold line: "Hold", the key as a keycap, then what holding it does,
/// hanging off the choice above like any sub-setting, with REBIND at its
/// right end. The key sinks under the pointer, like a press; waiting for a
/// key it says so, and the words after it say how to give up (or why a key
/// was refused).
fn render_hold_line<S: painter::Surface>(surf: &mut S, line: &Row, hold: Hold, draft: &AppSettings,
                                         listening: bool, key_hovered: bool, refused: Option<&str>) {
    let name = hold_key_name(hold, draft, listening);
    let (kx, ky, kw, _) = hold_key_rect(line, &name);
    let text_y = line.rect.1 + (line.rect.3 - bitmap_font::GLYPH_H) / 2;
    draw_text(surf, line.rect.0 + 6 + SUB_INDENT, text_y, 1, HOLD_WORD, argb_to_framefang(TEXT_MUTED()));
    crate::keycap::draw_key_on(surf, kx, ky, &name, 1, key_hovered && !listening);
    let (tail, color) = match (listening, refused) {
        (true, Some(why)) => (why, WARN()),
        (true, None) => ("Esc cancels", LASER()),
        _ => (hold.tail(draft), TEXT_MUTED()),
    };
    draw_text(surf, kx + kw + HOLD_GAP, text_y, 1, tail, argb_to_framefang(color));
}

/// One tab in the strip. The one shown is a raised tab on the page's
/// surface with the accent along its top; the rest sit back.
fn render_tab<S: painter::Surface>(surf: &mut S, row: &Row, shown: bool, hovered: bool) {
    let (rx, ry, rw, rh) = row.rect;
    if shown {
        fill_rect(surf, rx, ry, rw, rh, argb_to_framefang(BG_TOP()));
        fill_rect(surf, rx, ry, rw, 2, argb_to_framefang(LASER()));
        fill_rect(surf, rx, ry, 1, rh, argb_to_framefang(HEADER_RULE()));
        fill_rect(surf, rx + rw - 1, ry, 1, rh, argb_to_framefang(HEADER_RULE()));
    } else if hovered {
        fill_rect(surf, rx, ry + 2, rw, rh - 3, argb_to_framefang(HOVER_FILL()));
    }
    let ty = ry + (rh - bitmap_font::GLYPH_H) / 2 + 1;
    let color = if shown { TEXT_BRIGHT } else if hovered { TEXT_BRIGHT } else { TEXT_MUTED() };
    draw_text(surf, rx + TAB_PAD, ty, 1, row.label, argb_to_framefang(color));
}

/// A row's label with one quiet line under it (the next file name, or why a
/// choice is greyed).
fn render_label_with_caption<S: painter::Surface>(surf: &mut S, row: &Row, caption: &str) {
    let (rx, ry, _, rh) = row.rect;
    let gap = rh - 2 * bitmap_font::GLYPH_H;
    let top = ry + gap / 3;
    draw_text(surf, rx + 6, top, 1, row.label, argb_to_framefang(TEXT_BRIGHT));
    draw_text(surf, rx + 6, top + bitmap_font::GLYPH_H + gap / 3, 1, caption, argb_to_framefang(TEXT_MUTED()));
}

/// A chip in a row of choices. The picked one is filled and framed.
/// `unavailable`: dim, though it still answers (GIF without
/// FFmpeg points at where FFmpeg is set up).
fn render_choice_chip<S: painter::Surface>(surf: &mut S, row: &Row, hovered: bool, draft: &AppSettings,
                                           unavailable: bool) {
    let (rx, ry, rw, rh) = row.rect;
    let (chosen, accent) = match row.kind {
        WidgetKind::AfterCaptureChoice(after) => (draft.after_capture() == after, LASER()),
        WidgetKind::ScrollAfterChoice(after) => (draft.scroll_after() == after, LASER()),
        WidgetKind::FileNameChoice(choice) =>
            (framefang_core::file_name::preset_index(&draft.file_name_pattern) == choice, LASER()),
        WidgetKind::RecordFormatChoice(gif) =>
            ((draft.record_format() == framefang_core::settings::RecordFormat::Gif) == gif, LASER()),
        WidgetKind::FrameChoice(f) => (draft.output_frame() == f, LASER()),
        WidgetKind::QrLinksChoice(open) => (draft.open_qr_links == open, LASER()),
        _ => return,
    };
    if chosen || hovered {
        fill_rect(surf, rx, ry, rw, rh, argb_to_framefang(HOVER_FILL()));
    }
    let border = if chosen { accent } else if hovered { LASER_DIM() } else { FIELD_BORDER() };
    stroke_rect_argb(surf, rx, ry, rw, rh, argb_to_framefang(border));
    let tw = bitmap_font::measure(row.label, 1);
    let color = if unavailable { TEXT_DIM() } else if chosen { TEXT_BRIGHT } else { TEXT_MUTED() };
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
const TOGGLE_W: i32 = 64;

fn draw_toggle<S: painter::Surface>(s: &mut S, x: i32, y: i32, h: i32, on: bool) {
    let w = TOGGLE_W;
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

/// The Win32 virtual-key code of any key, modifiers included (by side): for
/// binding a hold key, and for the capture window's held keys off Windows.
pub(crate) fn vk_of(key: PhysicalKey) -> Option<u32> {
    if let PhysicalKey::Code(code) = key {
        let side = match code {
            KeyCode::ShiftLeft => Some(0xA0), KeyCode::ShiftRight => Some(0xA1),
            KeyCode::ControlLeft => Some(0xA2), KeyCode::ControlRight => Some(0xA3),
            KeyCode::AltLeft => Some(0xA4), KeyCode::AltRight => Some(0xA5),
            KeyCode::SuperLeft => Some(0x5B), KeyCode::SuperRight => Some(0x5C),
            KeyCode::Escape => Some(0x1B),
            _ => None,
        };
        if let Some(vk) = side.or_else(|| keycode_to_vk(code)) { return Some(vk); }
    }
    native_vk(key)
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

    /// Every page's rows, one after another.
    fn all_rows(page: Option<crate::system_shortcuts::Page>, guide: bool) -> Vec<Row> {
        Tab::ALL.into_iter().flat_map(|t| SettingsView::build_rows(page, guide, t)).collect()
    }

    #[test]
    fn every_page_fits_a_small_screen_and_its_rows_stay_in_the_window() {
        let page = crate::system_shortcuts::page();
        let win_h = window_height(page) as i32;
        // 1280 x 720 with a 48 px taskbar.
        assert!(win_h <= 720 - 48, "{win_h} px tall");
        for t in Tab::ALL {
            for guide in [true, false] {
                for r in SettingsView::build_rows(page, guide, t) {
                    let (x, y, w, h) = r.rect;
                    assert!(x >= 0 && x + w <= WIN_W as i32, "{} leaves the window", r.label);
                    assert!(y >= 0 && y + h + crate::chrome::TITLE_H as i32 <= win_h, "{} is cut off on {t:?}", r.label);
                    if !matches!(r.kind, WidgetKind::Tab(_) | WidgetKind::OpenJson | WidgetKind::ImportBtn | WidgetKind::ExportBtn) {
                        assert!(y >= PAGE_TOP, "{} sits over the tab strip", r.label);
                    }
                }
            }
        }
    }

    #[test]
    fn the_tabs_and_their_hint_fit_the_strip_and_each_setting_is_on_one_page() {
        let rows = SettingsView::build_rows(None, false, Tab::Captures);
        let last = rows.iter().filter(|r| matches!(r.kind, WidgetKind::Tab(_))).last().unwrap();
        let hint_x = WIN_W as i32 - PAD - crate::keycap::line_width(TAB_HINT, 1);
        assert!(last.rect.0 + last.rect.2 + MIN_GAP <= hint_x, "the tabs run into the hint");
        // A tab's words sit in the middle of it.
        assert_eq!(last.rect.2, bitmap_font::measure(last.label, 1) + 2 * TAB_PAD);
        let mut seen = std::collections::HashSet::new();
        for t in Tab::ALL {
            for r in SettingsView::build_rows(None, false, t) {
                let setting = matches!(r.kind, WidgetKind::SaveFolder | WidgetKind::RecordingsFolder
                    | WidgetKind::AfterCapture | WidgetKind::ScrollAfter | WidgetKind::CaptureCursor
                    | WidgetKind::CloseAfterCopy | WidgetKind::HotkeyDisplay(_) | WidgetKind::Theme
                    | WidgetKind::StartWithOs | WidgetKind::FileNames | WidgetKind::RecordFormat
                    | WidgetKind::Frame | WidgetKind::QrLinks | WidgetKind::KeepRecent);
                if setting { assert!(seen.insert(r.label), "{} on two pages", r.label); }
            }
        }
        assert_eq!(Tab::Captures.step(-1), Tab::General);
        assert_eq!(Tab::General.step(1), Tab::Captures);
    }

    #[test]
    fn the_new_rows_keep_their_chips_in_the_value_column() {
        let rows = SettingsView::build_rows(None, false, Tab::Files);
        for (label, kind) in [("File names", WidgetKind::FileNames), ("Record as", WidgetKind::RecordFormat),
                              ("Frame", WidgetKind::Frame)] {
            let row = rows.iter().find(|r| r.kind == kind).unwrap_or_else(|| panic!("no {label} row"));
            let right = row.rect.0 + row.rect.2 - 4;
            let chips = rows.iter().filter(|r| r.rect.1 == row.rect.1 + 4 && matches!(r.kind,
                WidgetKind::FileNameChoice(_) | WidgetKind::RecordFormatChoice(_) | WidgetKind::FrameChoice(_)));
            for c in chips {
                assert!(c.rect.0 >= row.rect.0 + LABEL_W && c.rect.0 + c.rect.2 <= right, "{} leaves {label}", c.label);
                assert!(bitmap_font::measure(c.label, 1) + 16 <= c.rect.2, "{} is cramped", c.label);
            }
        }
    }

    #[cfg(windows)]
    #[test]
    fn scrolling_captures_have_their_own_after_choice_on_captures() {
        use framefang_core::settings::AfterCapture;
        let rows = SettingsView::build_rows(None, false, Tab::Captures);
        let row = rows.iter().find(|r| r.kind == WidgetKind::ScrollAfter).expect("an After a scrolling capture row");
        let chips: Vec<_> = rows.iter().filter(|r| matches!(r.kind, WidgetKind::ScrollAfterChoice(_))).collect();
        assert_eq!(chips.len(), 3);
        for c in &chips {
            assert_eq!(c.rect.1, row.rect.1 + 4, "on its row");
            assert!(c.rect.0 >= row.rect.0 + LABEL_W && c.rect.0 + c.rect.2 <= row.rect.0 + row.rect.2 - 4);
        }
        // It sits below the region's choice, and the page still fits.
        let after = rows.iter().find(|r| r.kind == WidgetKind::AfterCapture).unwrap();
        assert!(row.rect.1 > after.rect.1);
        assert!((row.rect.1 + row.rect.3) as u32 + crate::chrome::TITLE_H as u32 <= window_height(None), "fits the window");
        // A click on a chip sets only the scrolling choice.
        let mut s = AppSettings::default();
        s.set_scroll_after(AfterCapture::CopyOnly);
        assert_eq!((s.scroll_after(), s.after_capture()), (AfterCapture::CopyOnly, AfterCapture::Edit));
    }

    #[test]
    fn qr_links_sit_on_captures_and_say_what_the_switch_key_does() {
        let rows = SettingsView::build_rows(None, false, Tab::Captures);
        let row = rows.iter().find(|r| r.kind == WidgetKind::QrLinks).expect("a QR links row");
        let chips: Vec<_> = rows.iter().filter(|r| matches!(r.kind, WidgetKind::QrLinksChoice(_))).collect();
        assert_eq!(chips.len(), 2);
        for c in &chips {
            assert_eq!(c.rect.1, row.rect.1 + 4, "on its row");
            assert!(c.rect.0 >= row.rect.0 + LABEL_W && c.rect.0 + c.rect.2 <= row.rect.0 + row.rect.2 - 4);
        }
        // Its own key, Ctrl unless set.
        let mut s = AppSettings::default();
        assert_eq!(s.qr_switch_key(), SwitchKey::Ctrl, "not Shift: that is the capture switch key");
        s.open_qr_links = true;
        assert_eq!(Hold::Qr.tail(&s), "to copy links instead");
    }

    #[test]
    fn the_heading_and_labels_clear_their_controls() {
        let rows = all_rows(crate::system_shortcuts::page(), true);

        // No header band: the title bar is the heading, and the tabs sit
        // right under it. The settings file's buttons are GENERAL's last
        // row, with room under them for what the last one did.
        assert_eq!(TITLE, "// SETTINGS");
        assert!(STRIP_Y <= 20, "the tabs start right under the title bar");
        let general_rows = SettingsView::build_rows(None, false, Tab::General);
        let file_row = general_rows.iter().find(|r| r.kind == WidgetKind::SettingsFile).unwrap();
        let status_bottom = file_row.rect.1 + file_row.rect.3 + STATUS_GAP + bitmap_font::GLYPH_H;
        assert!(status_bottom as u32 + crate::chrome::TITLE_H as u32 <= window_height(None), "the status line fits");
        for r in &rows {
            if matches!(r.kind, WidgetKind::OpenJson | WidgetKind::ImportBtn | WidgetKind::ExportBtn) {
                assert!(r.rect.1 >= PAGE_TOP, "{} is still in the header", r.label);
            }
        }
        let general = SettingsView::build_rows(None, false, Tab::General);
        let order: Vec<&str> = general.iter()
            .filter(|r| matches!(r.kind, WidgetKind::StartWithOs | WidgetKind::Theme | WidgetKind::SettingsFile))
            .map(|r| r.label).collect();
        assert_eq!(order, ["Start with system", "Theme", "Settings file"]);
        assert_eq!(general.iter().filter(|r| matches!(r.kind,
            WidgetKind::OpenJson | WidgetKind::ImportBtn | WidgetKind::ExportBtn)).count(), 3);

        // Every row label, indent included, ends before its control.
        for r in &rows {
            let indent = match r.kind {
                WidgetKind::CloseAfterCopy => SUB_INDENT,
                WidgetKind::SaveFolder | WidgetKind::RecordingsFolder | WidgetKind::Ffmpeg
                | WidgetKind::CaptureCursor | WidgetKind::AfterCapture | WidgetKind::ScrollAfter
                | WidgetKind::FileNames | WidgetKind::RecordFormat | WidgetKind::Frame | WidgetKind::QrLinks
                | WidgetKind::KeepRecent
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
        let rows = SettingsView::build_rows(Some(page), true, Tab::Hotkeys);
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
        // The card sits under the last hotkey row, whole inside the window
        // with the bottom band under it (it was cut off, 2026-11-19).
        let last_hotkey = rows.iter().filter(|r| matches!(r.kind, WidgetKind::HotkeyDisplay(_))).last().unwrap();
        let card_top = btn.rect.1 - CARD_PAD;
        assert!(card_top >= last_hotkey.rect.1 + ROW_H);
        assert!(card_top + CARD_H + BOTTOM_BAND + crate::chrome::TITLE_H as i32 <= window_height(Some(page)) as i32);

        // Another capture key: no card.
        let without = SettingsView::build_rows(Some(page), false, Tab::Hotkeys);
        assert!(!without.iter().any(|r| r.kind == WidgetKind::SystemShortcut));
        assert!(content_height(&without) < content_height(&rows));
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
        for r in all_rows(None, false) {
            if matches!(r.kind, WidgetKind::OpenJson | WidgetKind::ImportBtn | WidgetKind::ExportBtn) {
                assert!(bitmap_font::measure(r.label, 1) + 2 * MIN_GAP <= r.rect.2, "{}", r.label);
            }
            // A theme chip holds an 8 px swatch, a 6 px gap and the name.
            if matches!(r.kind, WidgetKind::ThemeChoice(_)) {
                assert!(8 + 6 + bitmap_font::measure(r.label, 1) + MIN_GAP <= r.rect.2, "{}", r.label);
            }
            // An After-a-capture chip keeps 8 px either side of its word.
            if matches!(r.kind, WidgetKind::AfterCaptureChoice(_)) {
                assert!(bitmap_font::measure(r.label, 1) + 16 <= r.rect.2, "{}", r.label);
            }
        }
    }

    #[test]
    fn each_hold_line_sits_under_the_choice_it_flips_and_fits_any_key() {
        let rows = SettingsView::build_rows(None, false, Tab::Captures);
        let mut holds = vec![(WidgetKind::AfterCapture, Hold::Capture), (WidgetKind::QrLinks, Hold::Qr)];
        if crate::scroll::supported() { holds.push((WidgetKind::ScrollAfter, Hold::Scroll)); }
        for (choice, hold) in holds {
            let above = rows.iter().find(|r| r.kind == choice).unwrap();
            let line = rows.iter().find(|r| r.kind == WidgetKind::HoldLine(hold)).unwrap();
            assert_eq!(line.rect.1, above.rect.1 + ROW_H + SUB_IN, "{hold:?}: right under its choice, close");
            // On its card's sub-strip, with its choice on the card above it.
            let card = rows.iter().find(|r| matches!(r.kind, WidgetKind::Card(_))
                && r.rect.1 <= above.rect.1 && line.rect.1 + line.rect.3 <= r.rect.1 + r.rect.3).expect("a card");
            let WidgetKind::Card(Some(sub)) = card.kind else { panic!("{hold:?}: no sub-strip") };
            assert!(above.rect.1 + above.rect.3 <= sub && sub <= line.rect.1);
            assert!(rows.iter().any(|r| r.kind == WidgetKind::HoldKey(hold)));
            // Its REBIND button sits inside the line at its right end, and
            // reads LISTENING without spilling while it waits.
            let btn = rows.iter().find(|r| r.kind == WidgetKind::HoldRebind(hold)).expect("a REBIND button");
            assert_eq!(btn.rect.0 + btn.rect.2, line.rect.0 + line.rect.2 - 4, "{hold:?}: at the right end");
            assert!(btn.rect.1 >= line.rect.1 && btn.rect.1 + btn.rect.3 <= line.rect.1 + line.rect.3);
            assert!(bitmap_font::measure("LISTENING", 1) + 12 <= btn.rect.2);
            // The longest it gets, waiting or refused or with a long key
            // and the longer words, ends before the button.
            let mut s = AppSettings::default();
            for open in [true, false] {
                s.open_editor = open;
                s.open_qr_links = open;
                s.set_scroll_after(if open { framefang_core::settings::AfterCapture::CopyAndEdit }
                                   else { framefang_core::settings::AfterCapture::CopyOnly });
                for key in [SwitchKey::Key(0x2C), SwitchKey::Ctrl] {
                    hold.set(&mut s, key);
                    for (listening, tail) in [(false, hold.tail(&s).to_owned()), (true, "Print Screen is in use".to_owned())] {
                        let name = hold_key_name(hold, &s, listening);
                        let (kx, _, kw, _) = hold_key_rect(line, &name);
                        let end = kx + kw + HOLD_GAP + bitmap_font::measure(&tail, 1);
                        assert!(end + MIN_GAP <= btn.rect.0, "{hold:?} {name} {tail}: ends at {end}");
                    }
                }
            }
        }
        // It says what holding the key does, which flips with the choice.
        let s = AppSettings::default();
        assert_eq!(Hold::Capture.tail(&s), "to just copy instead");
        assert_eq!(Hold::Qr.tail(&s), "to open links instead");
        assert_eq!(Hold::Scroll.tail(&s), "to just copy instead", "a scrolling capture opens the editor by default");
    }

    #[test]
    fn the_ffmpeg_row_can_pick_one_and_says_which_is_used() {
        let rows = SettingsView::build_rows(None, false, Tab::Files);
        let get = |k: WidgetKind| rows.iter().find(|r| r.kind == k).unwrap().rect;
        let (browse, auto, download) = (get(WidgetKind::FfmpegBrowse), get(WidgetKind::FfmpegAuto), get(WidgetKind::FfmpegDownload));
        let row = get(WidgetKind::Ffmpeg);
        // Browse... ends where the folder rows' Browse... does; Auto or
        // Download sits left of it, a gap apart.
        assert_eq!(browse.0 + browse.2, row.0 + row.2 - 4);
        for slot in [auto, download] { assert_eq!(slot.0 + slot.2 + 8, browse.0); }
        assert!(auto.0 - 8 - (row.0 + LABEL_W) >= 160, "a picked path has room");
        for (label, w) in [("Browse...", BROWSE_W), ("Auto", AUTO_W), ("DOWNLOAD", FFMPEG_BTN_W), ("RETRY", FFMPEG_BTN_W)] {
            assert!(bitmap_font::measure(label, 1) + 16 <= w, "{label}");
        }
        // The field: the pick bright, a found one muted, a missing pick
        // warned about with what is used instead.
        let (p, q) = (std::path::Path::new("D:/tools/ffmpeg.exe"), std::path::Path::new("C:/ff/ffmpeg.exe"));
        assert_eq!(ffmpeg_field_text(Some(p), Some(p)).unwrap().1, TEXT_BRIGHT);
        assert_eq!(ffmpeg_field_text(Some(q), None).unwrap().1, TEXT_MUTED());
        let (gone, color) = ffmpeg_field_text(Some(q), Some(p)).unwrap();
        assert!(gone.contains("gone") && gone.contains("C:/ff") && color == WARN());
        assert!(ffmpeg_field_text(None, Some(p)).unwrap().0.contains("Not found"));
        assert!(ffmpeg_field_text(None, None).is_none());
        // Blank means automatic.
        let mut s = AppSettings::default();
        assert_eq!(s.chosen_ffmpeg(), None);
        s.ffmpeg_path = "  ".into();
        assert_eq!(s.chosen_ffmpeg(), None);
    }

    #[test]
    fn after_a_capture_offers_three_choices_and_holds_the_editor_copy_setting() {
        use framefang_core::settings::AfterCapture;
        let rows = SettingsView::build_rows(None, false, Tab::Captures);
        let chips: Vec<_> = rows.iter().filter_map(|r| match r.kind {
            WidgetKind::AfterCaptureChoice(a) => Some(a), _ => None }).collect();
        assert_eq!(chips, AfterCapture::ALL.to_vec());
        // No separate auto-copy switch: its card is gone.
        assert!(!rows.iter().any(|r| r.label == "Copy screenshots automatically"));
        // Close-after-copy sits under the hold line, on the same card's
        // sub-strip as After a capture.
        let line = rows.iter().find(|r| r.kind == WidgetKind::HoldLine(Hold::Capture)).unwrap();
        let close = rows.iter().find(|r| r.kind == WidgetKind::CloseAfterCopy).unwrap();
        assert_eq!(close.rect.1, line.rect.1 + line.rect.3, "right under the hold line");
        let card = rows.iter().find(|r| matches!(r.kind, WidgetKind::Card(Some(sub)) if sub <= line.rect.1)
            && r.rect.1 <= line.rect.1 && close.rect.1 + close.rect.3 <= r.rect.1 + r.rect.3).expect("one card");
        assert!(card.rect.1 < line.rect.1);
        // Picking a chip sets both stored fields.
        let mut s = AppSettings::default();
        s.set_after_capture(AfterCapture::CopyAndEdit);
        assert!(s.open_editor && s.auto_copy);
        assert_eq!(Hold::Capture.tail(&s), "to just copy instead");
        s.set_after_capture(AfterCapture::CopyOnly);
        assert_eq!(Hold::Capture.tail(&s), "to open the editor instead");
    }
}
