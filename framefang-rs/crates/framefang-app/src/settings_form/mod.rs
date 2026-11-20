//! The Settings window.
//!
//! A frameless winit window drawn with softbuffer and the in-tree bitmap
//! font. The tray owns an `Option<SettingsView>`, hands it each `WindowEvent`
//! for its `WindowId`, and drains `take_outcomes` afterwards to learn what the
//! person did here.
//!
//! Four tabs (CAPTURES, FILES, HOTKEYS, GENERAL), each a page of settings
//! with a hairline between them; `build_rows` lays them out.
//!
//! There is no Save button: each change goes to the tray as it happens
//! (`SettingsOutcome::Patch`), which applies and saves it.
//!
//! GENERAL ends with the settings file: Edit as JSON opens settings.json in
//! the default editor (a saved edit shows here within a second), Import...
//! loads a copy, Export... saves one wherever the person picks. What the last
//! of them did shows under the buttons.
//!
//! Submodules: `draw` paints rows and holds the drawing kit, `keys` turns
//! winit keys and modifiers into the numbers `settings.json` stores.

mod draw;
mod keys;

use std::num::NonZeroU32;
use std::path::PathBuf;
use std::rc::Rc;

use anyhow::{anyhow, Result};
use softbuffer::{Context, Surface};
use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::event_loop::ActiveEventLoop;
use winit::keyboard::{Key, ModifiersState, NamedKey, PhysicalKey};
use winit::window::{CursorIcon, Window, WindowAttributes, WindowId};

use framefang_core::{AppSettings, Hotkey};
use framefang_core::hotkeys::{conflict_message, HotkeyAction};
use framefang_core::settings::SwitchKey;

use crate::bitmap_font;
use crate::painter;

use self::draw::*;
use self::keys::*;
pub(crate) use self::keys::vk_of;

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
/// Space between rows that sit in a stack of their own.
const ROW_GAP: i32 = 6;
/// Side margin of every page.
const PAD: i32 = 22;
/// A setting row's height.
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
/// Widths of the two buttons at a hotkey row's right end: REBIND on the
/// edge, and CLEAR just left of it (optional actions only).
const REBIND_W: i32 = 100;
const CLEAR_W: i32 = 76;
/// Between the settings file's buttons and the line saying what the last of
/// them did.
const STATUS_GAP: i32 = 8;

/// What a laid-out row is, and so what clicking it does.
#[derive(Clone, Copy, PartialEq, Eq)]
enum WidgetKind {
    /// The screenshots folder: a path field and Browse....
    SaveFolder,
    /// The recordings folder, the same way.
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
    /// The box showing one action's binding.
    HotkeyDisplay(HotkeyAction),
    /// REBIND: the next key pressed becomes that action's binding.
    HotkeyRebind(HotkeyAction),
    /// CLEAR: unbinds the action. Laid out only for the optional ones.
    HotkeyClear(HotkeyAction),
    /// The Theme row's label. The chips beside it are the controls.
    Theme,
    /// One color-scheme chip. Picking it previews at once; Cancel reverts.
    ThemeChoice(crate::palette::Scheme),
    /// Run Frame Fang when the system starts.
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
    /// Open settings.json in the default editor.
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

/// One laid-out control or label: what it is, its words, and where it sits
/// in the page's own coordinates (x, y, width, height).
struct Row {
    kind: WidgetKind,
    label: &'static str,
    rect: Rect,
}

/// What the window tells the tray, collected by `take_outcomes`.
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
    /// Edit as JSON was clicked: the tray opens settings.json with the
    /// system's handler. The window stays open, and an edit saved there shows
    /// in it.
    OpenJson,
}

/// The open Settings window and everything it is showing or waiting on.
pub struct SettingsView {
    window: Rc<Window>,
    _ctx: Context<Rc<Window>>,
    surface: Surface<Rc<Window>, Rc<Window>>,
    /// The settings as shown, changed by each click before it is reported.
    draft: AppSettings,
    rows: Vec<Row>,
    /// The pointer in page coordinates (below the title bar), and the row
    /// under it.
    cursor: (i32, i32),
    hover: Option<usize>,
    /// The hotkey row waiting for a key to bind, if any. The next key that
    /// isn't a bare modifier becomes its binding, and Esc puts back
    /// `rebind_backup`.
    rebinding: Option<HotkeyAction>,
    /// A hold line's key waiting for a press to bind, and why the last press
    /// was refused, if it was.
    listening_hold: Option<Hold>,
    hold_refused: Option<String>,
    /// What that action was bound to when REBIND was pressed, so Esc can
    /// put it back (`None` if it was unbound).
    rebind_backup: Option<Hotkey>,
    /// The modifiers held right now. winit reports them in their own event,
    /// not with each key press.
    held_mods: ModifiersState,
    /// A message under the HOTKEYS rows, in the danger tint: a chord another
    /// action already has, or a system page that wouldn't open. The next
    /// rebind or clear removes it.
    notice: Option<String>,
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
        // Something needed FFmpeg: open on the page where it is set up.
        let tab = if ffmpeg_attention { Tab::Files } else { Tab::Captures };
        let rows = Self::build_rows(system_page, guide_shown, tab);
        let height = window_height(system_page);
        let (left, top) = centred_origin(
            loop_target.primary_monitor().or_else(|| loop_target.available_monitors().next())
                .map(|m| (m.position().x, m.position().y, m.size().width as i32, m.size().height as i32)),
            WIN_W,
            height,
        );
        let attrs = crate::chrome::frameless(WindowAttributes::default())
            .with_title("Frame Fang: Settings")
            .with_resizable(false)
            .with_inner_size(PhysicalSize::new(WIN_W, height))
            .with_position(PhysicalPosition::new(left, top))
            .with_window_icon(crate::brand_icon::shared());

        let window = Rc::new(
            loop_target.create_window(attrs).map_err(|e| anyhow!("settings window: {e}"))?,
        );
        crate::chrome::finish(&window);

        // The app lives in the tray (or the macOS menu bar), so with no Dock
        // icon to click this window could open out of sight behind the one in
        // front. Ask for focus outright; where that isn't needed it is just a
        // raise.
        window.focus_window();
        window.set_cursor(CursorIcon::Default);
        let ctx = Context::new(window.clone()).map_err(|e| anyhow!("settings graphics context: {e}"))?;
        let surface = Surface::new(&ctx, window.clone()).map_err(|e| anyhow!("settings drawing surface: {e}"))?;

        let mut view = SettingsView {
            window,
            _ctx: ctx,
            surface,
            baseline: current.clone(),
            imported: false,
            tab,
            draft: current,
            rows,
            guide_shown,
            cursor: (0, 0),
            hover: None,
            rebinding: None,
            listening_hold: None,
            hold_refused: None,
            rebind_backup: None,
            held_mods: ModifiersState::empty(),
            notice: None,
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
        view.refresh_system_takes();
        view.ffmpeg_found = view.find_ffmpeg();
        view.redraw();
        Ok(view)
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
        if let Some(a) = self.rebinding { self.rebind_backup = s.hotkey_for(a); }
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
        if let Some(action) = self.rebinding.take() {
            self.draft.set_hotkey_for(action, self.rebind_backup);
            self.capture_hint = None;
            self.notice = None;
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
            // winit sends the held modifiers on their own, apart from the
            // key presses; keep them to pair with a rebound key.
            WindowEvent::ModifiersChanged(mods) => self.held_mods = mods.state(),
            WindowEvent::CursorLeft { .. } => {
                if self.title_bar.leave() { self.window.request_redraw(); }
            }
            WindowEvent::CursorMoved { position, .. } => self.on_pointer_moved((position.x as i32, position.y as i32)),
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. } => {
                self.on_left_press();
            }
            WindowEvent::KeyboardInput { event: key_ev, .. } => {
                // A waiting hold key gets the press first, then a waiting
                // hotkey box; otherwise it is an ordinary key.
                if self.listening_hold.is_some() {
                    self.on_hold_key(key_ev);
                } else if self.rebinding.is_some() {
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

    /// The pointer moved to `raw` (window coordinates): title bar hover, the
    /// row under it, and the pointer's shape.
    fn on_pointer_moved(&mut self, raw: (i32, i32)) {
        if self.title_bar.hover_at(&self.window, raw) { self.window.request_redraw(); }
        // Page coordinates start below the title strip.
        self.cursor = (raw.0, raw.1 - crate::chrome::TITLE_H);
        let under = self.hit_test(self.cursor.0, self.cursor.1);
        self.window.set_cursor(if under.is_some() { CursorIcon::Pointer } else { CursorIcon::Default });
        if under != self.hover {
            self.hover = under;
            self.window.request_redraw();
        }
    }

    /// A left press: the title bar's buttons first, then the row under the
    /// pointer, or the window's drag area if there is none.
    fn on_left_press(&mut self) {
        match self.title_bar.press(&self.window, self.cursor) {
            Some(crate::chrome::TitlePart::Close) => {
                self.handle_event(WindowEvent::CloseRequested);
                return;
            }
            Some(_) => return,
            None => {}
        }
        let hit = self.hit_test(self.cursor.0, self.cursor.1);
        // A click anywhere but the waiting key stops it waiting.
        let on_waiting_key = hit.is_some_and(|i| matches!(self.rows[i].kind, WidgetKind::HoldKey(_)));
        if self.listening_hold.is_some() && !on_waiting_key {
            self.stop_hold_listen();
        }
        match hit {
            Some(i) => self.activate(i),
            None => crate::chrome::press_off_controls(&self.window, self.cursor.1, 0),
        }
    }

    /// A key with nothing waiting for it. Esc doesn't close the window (the
    /// title bar does, like any window's); Ctrl+Tab and Ctrl+Shift+Tab step
    /// through the pages. A waiting rebind takes every key before this.
    fn on_key(&mut self, ev: winit::event::KeyEvent) {
        if ev.state != ElementState::Pressed { return; }
        if ev.logical_key == Key::Named(NamedKey::Tab) && self.held_mods.control_key() {
            let by = if self.held_mods.shift_key() { -1 } else { 1 };
            self.switch_tab(self.tab.step(by));
        }
    }

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
        let Some(key) = SwitchKey::from_vk(vk) else {
            let name = framefang_core::settings::vk_label(vk).unwrap_or("That key");
            self.hold_refused = Some(format!("{name} is in use"));
            self.window.request_redraw();
            return;
        };
        let before = self.draft.clone();
        hold.set(&mut self.draft, key);
        self.stop_hold_listen();
        self.report_change(&before);
    }

    /// Give up waiting on a hold key, and forget why a key was refused.
    fn stop_hold_listen(&mut self) {
        let was_listening = self.listening_hold.take().is_some();
        let was_refusing = self.hold_refused.take().is_some();
        if was_listening || was_refusing { self.window.request_redraw(); }
    }

    /// The key that ends the rebind of a hotkey box, as far as winit names it:
    /// its Win32 number, or `None` for a bare modifier or a key with no number.
    fn rebind_vk(key: PhysicalKey) -> Option<u32> {
        match key {
            PhysicalKey::Code(code) if is_modifier_code(code) => None,
            // Named but unmapped: ask Windows what it calls the key.
            PhysicalKey::Code(code) => keycode_to_vk(code).or_else(|| native_vk(key)),
            // A key winit can't name (some media and OEM keys): Windows can.
            other => native_vk(other),
        }
    }

    /// The key handler while a hotkey box waits (`rebinding` is set).
    /// It takes every key, so none reaches `on_key`.
    ///
    /// - Esc cancels and puts back `rebind_backup`.
    /// - A bare modifier is ignored: the box wants a real key to go with it.
    /// - A typing key that would stop typing is refused with a hint, and the
    ///   box keeps listening.
    /// - A chord another action has is refused too, with its message.
    /// - Anything else becomes the binding, with the modifiers held now, and
    ///   is reported at once.
    /// - A key with no Win32 number is ignored; Edit as JSON is the way to
    ///   bind an exotic one.
    fn on_capture_key(&mut self, ev: winit::event::KeyEvent) {
        if ev.state != ElementState::Pressed { return; }
        let Some(action) = self.rebinding else { return; };
        if matches!(ev.logical_key, Key::Named(NamedKey::Escape)) {
            self.draft.set_hotkey_for(action, self.rebind_backup);
            self.rebinding = None;
            self.capture_hint = None;
            self.notice = None;
            self.relayout();
            self.window.request_redraw();
            return;
        }
        let Some(vk) = Self::rebind_vk(ev.physical_key) else { return };
        let hk = Hotkey { modifiers: mods_state_to_hk(self.held_mods), virtual_key: vk };
        // Alone (or with only Shift) a typing key would stop typing that
        // character in every app: keep listening and say what to hold.
        if framefang_core::settings::blocks_typing(&hk) {
            self.capture_hint = Some(typing_key_hint(vk));
            self.window.request_redraw();
            return;
        }
        self.capture_hint = None;
        let before = self.draft.clone();
        self.draft.set_hotkey_for(action, Some(hk));
        // The system can't give one chord to two actions (on Windows the
        // second `RegisterHotKey` fails; under X11 the first grab to match
        // wins quietly), so a repeat is refused as it is pressed and the box
        // keeps listening.
        if let Some((a, b)) = self.draft.hotkey_conflict() {
            let chord = self.draft.hotkey_for(a).unwrap_or_default();
            self.notice = Some(conflict_message(a, b, chord));
            self.draft = before;
            self.window.request_redraw();
            return;
        }
        self.rebinding = None;
        self.notice = None;
        // Print Screen in or out: the Windows card comes or goes with it.
        self.relayout();
        self.report_change(&before);
        self.window.request_redraw();
    }

    /// The row under (x, y), if any; later rows win where they overlap, which
    /// is how the REBIND and CLEAR buttons beat the box they sit on.
    fn hit_test(&self, x: i32, y: i32) -> Option<usize> {
        self.rows.iter().enumerate().rev().find_map(|(i, r)| {
            let live = match r.kind {
                // A hotkey error takes the system line's place, button too.
                WidgetKind::SystemShortcut => self.notice.is_none(),
                // Words only (their chips or buttons are the controls), and
                // the lines and surfaces behind rows.
                WidgetKind::Theme | WidgetKind::SettingsFile | WidgetKind::AfterCapture
                | WidgetKind::ScrollAfter | WidgetKind::FileNames | WidgetKind::RecordFormat
                | WidgetKind::Frame | WidgetKind::QrLinks | WidgetKind::HoldLine(_)
                | WidgetKind::Card(_) | WidgetKind::Divider => false,
                // The FFmpeg read-out can't be pressed; its Download is live
                // only when there is something to download, and Auto only
                // while a pick exists.
                WidgetKind::Ffmpeg => false,
                WidgetKind::FfmpegDownload => self.ffmpeg_actionable(),
                WidgetKind::FfmpegAuto => self.draft.chosen_ffmpeg().is_some(),
                // A listening row hides its CLEAR, so it can't be pressed.
                WidgetKind::HotkeyClear(a) => self.rebinding != Some(a),
                // A hold key is where it is drawn, with a little room around.
                WidgetKind::HoldKey(hold) => {
                    let name = hold_key_name(hold, &self.draft, self.listening_hold == Some(hold));
                    let (kx, ky, kw, kh) = hold_key_rect(r, &name);
                    return rect_has((kx - 4, ky - 4, kw + 8, kh + 8), x, y).then_some(i);
                }
                _ => true,
            };
            (live && rect_has(r.rect, x, y)).then_some(i)
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
                let start = folder_dialog_start(&self.draft.save_directory, directories::UserDirs::picture_dir);
                let picked = rfd::FileDialog::new()
                    .set_title("Frame Fang: Screenshots folder")
                    .set_directory(&start)
                    .pick_folder();
                if let Some(dir) = picked {
                    self.draft.save_directory = dir.to_string_lossy().into_owned();
                }
            }
            WidgetKind::RecordingsFolder => {
                let start = folder_dialog_start(&self.draft.recordings_directory, directories::UserDirs::video_dir);
                let picked = rfd::FileDialog::new()
                    .set_title("Frame Fang: Recordings folder")
                    .set_directory(&start)
                    .pick_folder();
                if let Some(dir) = picked {
                    self.draft.recordings_directory = dir.to_string_lossy().into_owned();
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
                    if let Some(action) = self.rebinding.take() {
                        self.draft.set_hotkey_for(action, self.rebind_backup);
                        self.capture_hint = None;
                    }
                    self.listening_hold = Some(hold);
                    self.hold_refused = None;
                }
            }
            WidgetKind::KeepRecent => self.draft.keep_recent ^= true,
            WidgetKind::CloseAfterCopy => self.draft.close_after_copy ^= true,
            WidgetKind::CaptureCursor => self.draft.capture_cursor ^= true,
            WidgetKind::StartWithOs => self.draft.start_with_windows ^= true,
            // Only REBIND starts a capture; a click on the box itself does
            // nothing, so a stray click can't throw a binding away.
            WidgetKind::HotkeyDisplay(_) => {}
            WidgetKind::HotkeyRebind(action) => {
                // Remember the binding for Esc. The tray has already let go
                // of every global shortcut while this window is open, so
                // pressing the current one here doesn't fire it.
                self.rebind_backup = self.draft.hotkey_for(action);
                self.rebinding = Some(action);
                self.capture_hint = None;
                self.notice = None;
            }
            WidgetKind::HotkeyClear(action) => {
                // Laid out for optional actions only, so the capture binding
                // can't be cleared from here.
                self.draft.set_hotkey_for(action, None);
                self.rebinding = None;
                self.notice = None;
            }
            // Neither changes a setting, so nothing to report or redraw here.
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
                        self.notice = Some(e);
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
                self.rebinding = None;
                self.notice = None;
                self.relayout();
                self.status = Some((format!("Imported {name}"), false));
            }
            Err(e) => self.status = Some((format!("Couldn't import {name}: {e}"), true)),
        }
    }

    /// The tab strip's tabs, then page `tab`'s rows top to bottom. A
    /// setting's row spans the page; the value half of it is worked out in
    /// `draw::render_row`. `system_page` and `windows_guide` decide what sits
    /// under the hotkeys.
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
        // HOTKEYS: a row per action. The display box takes the value column
        // up to the buttons; REBIND ends at the right edge with CLEAR just
        // left of it on the optional actions. Each button is a row of its
        // own laid over the display row, and `hit_test` looks from the last
        // row back, so the button gets the click.
        let c = CardBuild::open(&mut y);
        for (i, action) in HotkeyAction::ALL.into_iter().enumerate() {
            if i > 0 { divider(&mut rows, &mut y); }
            rows.push(Row { kind: WidgetKind::HotkeyDisplay(action), label: action.label(),
                            rect: (x, y, row_w, ROW_H) });
            // As tall as the key box and the chips elsewhere, ending where
            // every other row's controls end.
            let rebind_x = x + row_w - 4 - REBIND_W;
            rows.push(Row { kind: WidgetKind::HotkeyRebind(action), label: "REBIND",
                            rect: (rebind_x, y + 4, REBIND_W, ROW_H - 8) });
            if action.is_optional() {
                rows.push(Row { kind: WidgetKind::HotkeyClear(action), label: "CLEAR",
                                rect: (rebind_x - 8 - CLEAR_W, y + 4, CLEAR_W, ROW_H - 8) });
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
        rows.push(Row { kind: WidgetKind::StartWithOs, label: "Start with system",
                        rect: (x, y, row_w, ROW_H) });
        y += ROW_H;
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

        // The last page's final advance of `y` has no row after it to use it.
        let _ = y;
        rows
    }

    /// Paint the whole window: the page's body into a buffer of its own
    /// (placed under the title strip), then the title bar over the top.
    fn redraw(&mut self) {
        // Read before the surface is borrowed below.
        let ffmpeg_actionable = self.ffmpeg_actionable();
        let phys = self.window.inner_size();
        let (Some(w), Some(h)) = (NonZeroU32::new(phys.width), NonZeroU32::new(phys.height))
            else { return; };
        if let Err(e) = self.surface.resize(w, h) {
            eprintln!("settings: could not resize the surface: {e}");
            return;
        }
        let mut frame = match self.surface.buffer_mut() {
            Ok(b) => b,
            Err(e) => {
                eprintln!("settings: could not get a frame buffer: {e}");
                return;
            }
        };
        let win_w = w.get() as usize;
        let body_h = (h.get() as usize).saturating_sub(crate::chrome::TITLE_H as usize);
        let mut buf = vec![0u32; win_w * body_h];

        // Black body under the title bar (which holds the heading), with the
        // glowing rule along its top (`hud`).
        crate::hud::backdrop(&mut buf, win_w, body_h, 0);

        let mut surf = PixelBuf { px: &mut buf, wide: win_w as i32, tall: body_h as i32 };

        // What the settings file's last Export or Import did, under its
        // buttons on GENERAL.
        let file_row = self.rows.iter().find(|r| r.kind == WidgetKind::SettingsFile);
        if let (Some((msg, failed)), Some(r)) = (&self.status, file_row) {
            let color = if *failed { DANGER() } else { OK_TINT };
            let x = r.rect.0 + LABEL_W;
            let room = r.rect.0 + r.rect.2 - 4 - x;
            put_text(&mut surf, x, r.rect.1 + r.rect.3 + STATUS_GAP, &truncate_for(msg, room), color);
        }

        // The tab strip: the page shown is a raised tab joined to the page,
        // the others sit back on the strip's line.
        let right = WIN_W as i32 - PAD;
        fill_box(&mut surf, PAD, STRIP_Y + TAB_H - 1, right - PAD, 1, HEADER_RULE());
        let hint_x = right - crate::keycap::line_width(TAB_HINT, 1);
        let hint_y = STRIP_Y + (TAB_H - bitmap_font::GLYPH_H) / 2;
        crate::keycap::draw_line_on(&mut surf, hint_x, hint_y, TAB_HINT, 1, TEXT_DIM());
        let capturing = self.rebinding;
        // The lines go down first, under every row: a hairline above each
        // group but the first, between settings, and each group's tree line
        // to its sub-settings.
        let mut groups_seen = 0;
        for row in &self.rows {
            let (rx, ry, rw, rh) = row.rect;
            match row.kind {
                WidgetKind::Card(sub) => {
                    if groups_seen > 0 {
                        fill_box(&mut surf, rx, ry - CARD_GAP / 2, rw, 1, PANEL_BORDER());
                    }
                    groups_seen += 1;
                    if let Some(sy) = sub { render_tree(&mut surf, row, sy, &self.rows); }
                }
                WidgetKind::Divider => fill_box(&mut surf, rx, ry, rw, rh, PANEL_BORDER()),
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
                if self.notice.is_none() {
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

        // The error (a refused chord, a system page that wouldn't open) goes
        // in the gap left under the last hotkey row, since that is the block
        // it is about.
        let last_hotkey = self.rows.iter().rev().find(|r| matches!(r.kind, WidgetKind::HotkeyDisplay(_)));
        if let (Some(msg), Some(r)) = (&self.notice, last_hotkey) {
            put_text(&mut surf, PAD, r.rect.1 + ROW_H + CARD_IN + 8, msg, DANGER());
        }

        self.title_bar.draw(&mut frame, win_w, &buf, TITLE, BG_TOP());
        if let Err(e) = frame.present() {
            eprintln!("settings: could not present the frame: {e}");
        }
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

/// Top left for a `w` x `h` window centred on `monitor` (x, y, width,
/// height), kept on that monitor; with no monitor, a 1920 x 1080 screen at
/// the origin.
fn centred_origin(monitor: Option<Rect>, w: u32, h: u32) -> (i32, i32) {
    let (mx, my, mw, mh) = monitor.unwrap_or((0, 0, 1920, 1080));
    (mx + ((mw - w as i32) / 2).max(0), my + ((mh - h as i32) / 2).max(0))
}

/// Where a folder dialog opens: at the folder already chosen, or else the
/// system's folder for that kind of file (`system_dir`), or the temp folder.
fn folder_dialog_start(
    chosen: &str,
    system_dir: fn(&directories::UserDirs) -> Option<&std::path::Path>,
) -> PathBuf {
    if !chosen.is_empty() { return PathBuf::from(chosen); }
    directories::UserDirs::new()
        .and_then(|u| system_dir(&u).map(std::path::Path::to_path_buf))
        .unwrap_or_else(std::env::temp_dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use framefang_core::Modifiers as HkMods;

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

    #[test]
    fn the_window_centres_on_its_monitor_and_never_starts_off_it() {
        assert_eq!(centred_origin(Some((0, 0, 1920, 1080)), 640, 500), (640, 290));
        // A second monitor to the left, offset in both directions.
        assert_eq!(centred_origin(Some((-1280, 100, 1280, 720)), 640, 500), (-960, 210));
        // Bigger than the screen: pinned to its top left, not off it.
        assert_eq!(centred_origin(Some((50, 60, 500, 400)), 640, 500), (50, 60));
        assert_eq!(centred_origin(None, 640, 500), (640, 290));
    }

    #[test]
    fn a_folder_dialog_opens_on_the_pick_or_a_system_folder() {
        let picked = folder_dialog_start("D:/Shots", directories::UserDirs::picture_dir);
        assert_eq!(picked, PathBuf::from("D:/Shots"));
        assert!(!folder_dialog_start("", directories::UserDirs::video_dir).as_os_str().is_empty());
    }

    #[test]
    fn a_hold_key_is_found_where_it_is_drawn_with_room_around_it() {
        let rows = SettingsView::build_rows(None, false, Tab::Captures);
        let line = rows.iter().find(|r| r.kind == WidgetKind::HoldLine(Hold::Capture)).unwrap();
        let name = hold_key_name(Hold::Capture, &AppSettings::default(), false);
        let (kx, ky, kw, kh) = hold_key_rect(line, &name);
        let inside = (kx + kw / 2, ky + kh / 2);
        assert!(rect_has((kx - 4, ky - 4, kw + 8, kh + 8), inside.0, inside.1));
        assert!(!rect_has((kx - 4, ky - 4, kw + 8, kh + 8), kx - 5, ky));
        // While waiting it shows a longer word, so the key grows.
        let waiting = hold_key_name(Hold::Capture, &AppSettings::default(), true);
        assert_eq!(waiting, HOLD_LISTEN);
        assert!(hold_key_rect(line, &waiting).2 > kw);
    }
}
