//! The tray (notification area / menu bar) icon and its menu.
//!
//! `tray-icon` does the per-OS part: `Shell_NotifyIcon` on Windows,
//! StatusNotifierItem over D-Bus on Linux, `NSStatusBar` on macOS. The icon
//! lives on the main thread, and the event loop calls [`Tray::try_recv`] each
//! tick to collect clicks on menu rows.

use std::cell::{Cell, RefCell};

use crate::recorder::RecordingOptions;
use crate::{Error, Result};
use tray_icon::menu::{CheckMenuItem, IsMenuItem, Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem, Submenu};
use tray_icon::{Icon as TrayIconImage, TrayIcon, TrayIconBuilder};

/// The whole state the recording rows are drawn from. Setting it in one
/// piece means one change can't undo another (turning the sound rows back
/// on in the middle of a GIF, for instance).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RecordRows {
    pub recording: bool,
    /// The previous recording is still being turned into a GIF.
    pub converting: bool,
    /// Recordings are saved as GIF, which has no sound.
    pub gif: bool,
}

/// The labels of the menu rows that show a hotkey.
///
/// The app writes the binding into each one (`"Capture (PrintScreen)"`) so
/// the shortcut can be learned from the menu. They come as plain text, not
/// settings, to keep UI wording out of this crate, and
/// [`Tray::set_hotkey_labels`] swaps them after a rebind.
#[derive(Debug, Clone, Default)]
pub struct MenuLabels {
    pub capture:      String,
    pub capture_full: String,
    /// Used for "Record" and, after its verb, for "Stop recording" too: one
    /// hotkey starts and stops a recording, so both rows show it.
    pub record:       String,
}

/// A click on the tray icon itself, in physical screen pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrayClick {
    /// The pointer position.
    pub x: i32,
    pub y: i32,
    /// The icon's rectangle: x, y, width, height.
    pub icon: (i32, i32, u32, u32),
}

/// What the user picked from the tray.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayEvent {
    /// Nothing happened this tick.
    None,
    /// Open the region picker.
    Capture,
    /// Take the whole desktop and save it straight away: no picker, no
    /// editor, one click to a file.
    CaptureFullScreen,
    /// Capture the previous rectangle again.
    CaptureLastRegion,
    /// Open the picker in TEXT mode to copy the words in a region.
    CaptureText,
    /// Open the picker in QR mode to read a code.
    CaptureQr,
    /// Open the picker in SCROLL mode to capture a page that scrolls.
    CaptureScroll,
    /// Copy recent capture `n` (0 is the newest).
    RecentCopy(usize),
    /// Open recent capture `n` in the editor.
    RecentEdit(usize),
    /// Save recordings as GIF (`true`) or MP4.
    SetGif(bool),
    /// Stop turning the last recording into a GIF and keep its MP4.
    CancelGif,
    /// Capture after this many seconds, so menus can be opened and windows
    /// arranged first.
    CaptureDelayed(u32),
    /// Call off a delayed capture that hasn't fired yet. Checked while the
    /// countdown runs.
    CancelPending,
    /// Record the whole desktop with these sound sources.
    StartRecording(RecordingOptions),
    /// Pick a rectangle, then record only that. It carries sound options like
    /// `StartRecording` so both go down one path: a region changes nothing
    /// about sound.
    StartRegionRecording(RecordingOptions),
    StopRecording,
    /// Open the newest recording in the editor, where a drawing made on one
    /// frame is then burned into the whole clip by ffmpeg.
    AnnotateLastRecording,
    /// Show the screenshot folder from Settings in the file manager.
    OpenSaveFolder,
    /// Show the recordings folder (usually `~/Videos`).
    OpenRecordingsFolder,
    Settings,
    About,
    /// Open the releases page to get the newest build.
    CheckForUpdates,
    /// Open the image conversion dialog (PNG, JPG, WEBP, BMP).
    ConvertImage,
    /// Open the video conversion dialog (MP4 to MOV, WEBM, MKV or GIF). It
    /// needs an `ffmpeg` binary, bundled or installed, when it runs.
    ConvertVideo,
    Exit,
}

/// The tray icon, its menu, and the rows whose text or state changes.
pub struct Tray {
    icon: TrayIcon,
    /// The event each fixed row sends. The GIF row's value is filled in from
    /// its check mark when clicked.
    routes: Vec<(MenuId, TrayEvent)>,
    capture: MenuItem,
    capture_full: MenuItem,
    last_region: MenuItem,
    cancel_pending: MenuItem,
    record: MenuItem,
    /// "Record + mic", "Record + audio", "Record + mic+audio".
    record_with_sound: [MenuItem; 3],
    record_region: MenuItem,
    stop: MenuItem,
    gif: CheckMenuItem,
    cancel_gif: MenuItem,
    rows: Cell<RecordRows>,
    /// One row per recent capture under each submenu, newest first; see
    /// `set_recent`.
    copy_recent: Submenu,
    edit_recent: Submenu,
    recent: RefCell<Vec<(MenuItem, MenuItem)>>,
}

fn tray_error(e: impl std::fmt::Display) -> Error {
    Error::Tray(e.to_string())
}

impl Tray {
    /// Put the icon in the tray with Frame Fang's menu. `tooltip` names the
    /// capture hotkey (for example "Frame Fang: press PrintScreen to
    /// capture") and `labels` gives the rows that show a binding; both can be
    /// changed later with `set_tooltip` and `set_hotkey_labels`.
    ///
    /// Call it only once the OS event loop is running: macOS drops an
    /// `NSStatusItem` made before `NSApplication` has finished launching, so
    /// the app builds the tray on its first loop tick, not in `main`.
    ///
    /// On Linux GTK is set up first, because the tray backend
    /// (libayatana-appindicator) needs GTK running on the main thread before
    /// any menu item exists. Setting it up twice is harmless; it fails only
    /// when no display can be reached, and that becomes a tray error.
    pub fn new(tooltip: impl Into<String>, labels: &MenuLabels) -> Result<Self> {
        #[cfg(target_os = "linux")]
        gtk::init().map_err(|e| Error::Tray(format!("GTK wouldn't start: {e}")))?;

        let row = |text: &str, enabled: bool| MenuItem::new(text, enabled, None);

        // One flat menu. Cinnamon, KDE and some GNOME extensions draw
        // StatusNotifierItem submenus badly (rows cut off or on top of each
        // other), so every choice is a top-level row: a taller menu that
        // draws right everywhere we've tried. Labels stay short because
        // Cinnamon's renderer and some KDE plasmoids cut long ones with "...".
        let capture = row(&labels.capture, true);
        let capture_full = row(&labels.capture_full, true);
        let last_region = row("Capture last region", false); // on once there is one
        let text = row("Copy screen text", true);
        let qr = row("Read QR code", true);
        // Scrolling other apps' windows only works on Windows so far.
        let scroll = row("Scrolling capture", cfg!(windows));
        let in_3 = row("Capture in 3s", true);
        let in_5 = row("Capture in 5s", true);
        let in_10 = row("Capture in 10s", true);
        let cancel_pending = row("Cancel pending", false); // on during a countdown

        // Plain-text labels: emoji render differently on every backend.
        let record = row(&labels.record, true);
        let record_with_sound = [
            row("Record + mic", true),
            row("Record + audio", true),
            row("Record + mic+audio", true),
        ];
        // Region recording gets one row, not its own four sound variants:
        // that would double the tallest block of a menu that can't use
        // submenus. Its event carries sound options anyway, so the rows can
        // be added later without touching anything else.
        let record_region = row("Record region", true);
        let stop = row(&stop_label(&labels.record), false);
        let gif = CheckMenuItem::new("Record as GIF", true, false, None);
        let cancel_gif = row("Cancel GIF", false);
        let annotate = row("Annotate last recording", true);

        let copy_recent = Submenu::new("Copy recent", false);
        let edit_recent = Submenu::new("Edit recent", false);
        let shots = row("Open shots", true);
        let recordings = row("Open recordings", true);
        let convert_image = row("Convert image", true);
        let convert_video = row("Convert video", true);
        let settings = row("Settings", true);
        let about = row("About", true);
        let updates = row("Check for updates", true);
        // Spelled out so it can't be taken for "close this menu".
        let quit = row("Quit Frame Fang", true);

        let routes = vec![
            (capture.id().clone(), TrayEvent::Capture),
            (capture_full.id().clone(), TrayEvent::CaptureFullScreen),
            (last_region.id().clone(), TrayEvent::CaptureLastRegion),
            (text.id().clone(), TrayEvent::CaptureText),
            (qr.id().clone(), TrayEvent::CaptureQr),
            (scroll.id().clone(), TrayEvent::CaptureScroll),
            (in_3.id().clone(), TrayEvent::CaptureDelayed(3)),
            (in_5.id().clone(), TrayEvent::CaptureDelayed(5)),
            (in_10.id().clone(), TrayEvent::CaptureDelayed(10)),
            (cancel_pending.id().clone(), TrayEvent::CancelPending),
            (record.id().clone(), TrayEvent::StartRecording(RecordingOptions::NONE)),
            (record_with_sound[0].id().clone(), TrayEvent::StartRecording(RecordingOptions::MIC_ONLY)),
            (record_with_sound[1].id().clone(), TrayEvent::StartRecording(RecordingOptions::SYSTEM_ONLY)),
            (record_with_sound[2].id().clone(), TrayEvent::StartRecording(RecordingOptions::MIC_AND_SYS)),
            (record_region.id().clone(), TrayEvent::StartRegionRecording(RecordingOptions::NONE)),
            (stop.id().clone(), TrayEvent::StopRecording),
            (gif.id().clone(), TrayEvent::SetGif(false)),
            (cancel_gif.id().clone(), TrayEvent::CancelGif),
            (annotate.id().clone(), TrayEvent::AnnotateLastRecording),
            (shots.id().clone(), TrayEvent::OpenSaveFolder),
            (recordings.id().clone(), TrayEvent::OpenRecordingsFolder),
            (convert_image.id().clone(), TrayEvent::ConvertImage),
            (convert_video.id().clone(), TrayEvent::ConvertVideo),
            (settings.id().clone(), TrayEvent::Settings),
            (about.id().clone(), TrayEvent::About),
            (updates.id().clone(), TrayEvent::CheckForUpdates),
            (quit.id().clone(), TrayEvent::Exit),
        ];

        let gaps: Vec<PredefinedMenuItem> = (0..5).map(|_| PredefinedMenuItem::separator()).collect();
        let mut order: Vec<&dyn IsMenuItem> = vec![&capture, &capture_full, &last_region, &text, &qr];
        #[cfg(windows)]
        order.push(&scroll);
        order.extend([&in_3 as &dyn IsMenuItem, &in_5, &in_10]);
        // Linux is the only place this row can ever be used: the countdown
        // keeps GTK running there, while on Windows and macOS it blocks the
        // thread the menu needs, so the menu can't open during it.
        #[cfg(target_os = "linux")]
        order.push(&cancel_pending);
        order.extend([
            &gaps[0] as &dyn IsMenuItem,
            &record, &record_with_sound[0], &record_with_sound[1], &record_with_sound[2],
            &record_region, &stop, &gif, &cancel_gif, &annotate,
            &gaps[1],
            &copy_recent, &edit_recent, &shots, &recordings,
            &gaps[2],
            &convert_image, &convert_video,
            &gaps[3],
            &settings, &about, &updates,
            &gaps[4],
            &quit,
        ]);
        let menu = Menu::new();
        menu.append_items(&order).map_err(tray_error)?;

        let builder = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_icon(brand_icon())
            .with_tooltip(tooltip.into());
        // On Windows the app opens its own menu, drawn in the app's colours
        // (see `take_click`). The native menu stays attached but closed, for
        // `show_native_menu` when the app's menu window can't be created.
        #[cfg(windows)]
        let builder = builder.with_menu_on_left_click(false).with_menu_on_right_click(false);
        let icon = builder.build().map_err(tray_error)?;

        Ok(Tray {
            icon,
            routes,
            capture,
            capture_full,
            last_region,
            cancel_pending,
            record,
            record_with_sound,
            record_region,
            stop,
            gif,
            cancel_gif,
            rows: Cell::new(RecordRows::default()),
            copy_recent,
            edit_recent,
            recent: RefCell::new(Vec::new()),
        })
    }

    /// The next menu click waiting in the queue, or `TrayEvent::None` when
    /// there is none this tick.
    pub fn try_recv(&self) -> TrayEvent {
        let Ok(clicked) = MenuEvent::receiver().try_recv() else { return TrayEvent::None };
        let id = &clicked.id;

        let recent = self.recent.borrow();
        if let Some(n) = recent.iter().position(|(copy, _)| copy.id() == id) {
            return TrayEvent::RecentCopy(n);
        }
        if let Some(n) = recent.iter().position(|(_, edit)| edit.id() == id) {
            return TrayEvent::RecentEdit(n);
        }

        match self.routes.iter().find(|(row, _)| row == id).map(|(_, event)| *event) {
            // The check box has already toggled itself, so its new state is
            // the choice. `set_record_rows` undoes it if the app says no.
            Some(TrayEvent::SetGif(_)) => TrayEvent::SetGif(self.gif.is_checked()),
            Some(event) => event,
            None => TrayEvent::None,
        }
    }

    /// The last left or right click on the icon since the previous call,
    /// taken when the button came up.
    ///
    /// Only Windows answers with a click, because only there does the app
    /// open its own menu; elsewhere the native menu opens by itself and this
    /// is `None`. The icon's event queue is emptied on every OS so hover
    /// events don't build up.
    pub fn take_click(&self) -> Option<TrayClick> {
        use tray_icon::{MouseButton, MouseButtonState, TrayIconEvent};
        let mut last = None;
        while let Ok(event) = TrayIconEvent::receiver().try_recv() {
            let TrayIconEvent::Click {
                position, rect,
                button: MouseButton::Left | MouseButton::Right,
                button_state: MouseButtonState::Up,
                ..
            } = event else { continue };
            last = Some(TrayClick {
                x: position.x.round() as i32,
                y: position.y.round() as i32,
                icon: (rect.position.x.round() as i32, rect.position.y.round() as i32,
                       rect.size.width, rect.size.height),
            });
        }
        last.filter(|_| cfg!(windows))
    }

    /// Open the native menu at the pointer, for when the app's own menu
    /// window can't be created. Windows and macOS only.
    pub fn show_native_menu(&self) {
        self.icon.show_menu();
    }

    /// Change the hover text, e.g. after the capture hotkey is rebound so the
    /// old key isn't advertised. Some Linux panels have no tooltips and say
    /// so; that is only cosmetic, so the error is handed back for the caller
    /// to log rather than hidden here.
    pub fn set_tooltip(&self, tooltip: &str) -> Result<()> {
        self.icon.set_tooltip(Some(tooltip)).map_err(tray_error)
    }

    /// Give every row that shows a hotkey its new text, e.g. a new
    /// `"Capture (PrintScreen)"` suffix after a rebind. This can't fail, so a
    /// rebind shows up without a restart.
    pub fn set_hotkey_labels(&self, labels: &MenuLabels) {
        self.capture.set_text(&labels.capture);
        self.capture_full.set_text(&labels.capture_full);
        self.record.set_text(&labels.record);
        self.stop.set_text(stop_label(&labels.record));
    }

    /// Turn "Cancel pending" on while a delayed capture counts down, and off
    /// again after, the way "Stop recording" follows the recording.
    pub fn set_pending(&self, pending: bool) {
        self.cancel_pending.set_enabled(pending);
    }

    /// A recording started or ended: the Record rows and "Stop recording"
    /// swap which of them can be clicked.
    pub fn set_recording(&self, recording: bool) {
        self.set_record_rows(RecordRows { recording, ..self.rows.get() });
    }

    /// The last recording started or finished becoming a GIF.
    pub fn set_converting(&self, converting: bool) {
        self.set_record_rows(RecordRows { converting, ..self.rows.get() });
    }

    /// Recordings are now saved as GIF (`true`) or MP4.
    pub fn set_gif(&self, gif: bool) {
        self.set_record_rows(RecordRows { gif, ..self.rows.get() });
    }

    /// Show the recent captures, newest first, each titled by its label
    /// (such as "11:42  1280 x 720"), once under "Copy recent" and once under
    /// "Edit recent". Both submenus are off while the list is empty.
    pub fn set_recent(&self, labels: &[String]) {
        let mut recent = self.recent.borrow_mut();
        for (copy, edit) in recent.drain(..) {
            let _ = self.copy_recent.remove(&copy);
            let _ = self.edit_recent.remove(&edit);
        }
        for label in labels {
            let pair = (MenuItem::new(label, true, None), MenuItem::new(label, true, None));
            let _ = self.copy_recent.append(&pair.0);
            let _ = self.edit_recent.append(&pair.1);
            recent.push(pair);
        }
        let any = !labels.is_empty();
        self.copy_recent.set_enabled(any);
        self.edit_recent.set_enabled(any);
    }

    /// Whether "Capture last region" has a rectangle to reuse.
    pub fn set_last_region(&self, available: bool) {
        self.last_region.set_enabled(available);
    }

    /// Draw every recording row from one state. Starting a recording needs
    /// the recorder free (not recording, not converting), and the sound
    /// variants also need MP4. Stop needs a recording, Cancel GIF a
    /// conversion. The GIF box can only change while free, and always shows
    /// the setting.
    pub fn set_record_rows(&self, state: RecordRows) {
        self.rows.set(state);
        let free = !(state.recording || state.converting);
        self.record.set_enabled(free);
        self.record_region.set_enabled(free);
        for with_sound in &self.record_with_sound {
            with_sound.set_enabled(free && !state.gif);
        }
        self.stop.set_enabled(state.recording);
        self.gif.set_enabled(free);
        self.gif.set_checked(state.gif);
        self.cancel_gif.set_enabled(state.converting);
    }

    /// Run the native event sources the tray depends on that winit doesn't
    /// drive. On Linux that is GTK's main loop, which is what turns a menu
    /// click into an event for `try_recv`. Windows and macOS need nothing:
    /// winit's loop already serves their backends.
    pub fn pump_events(&self) {
        #[cfg(target_os = "linux")]
        while gtk::events_pending() {
            gtk::main_iteration_do(false);
        }
    }
}

/// The "Stop recording" label, ending in the same hotkey as the Record row.
///
/// One hotkey starts and stops a recording, so the stop row shows it too.
/// Only the trailing `" (chord)"` is taken over; the words change, or the
/// menu would say "Record" twice.
fn stop_label(record_label: &str) -> String {
    let chord = record_label.find(" (").map_or("", |at| &record_label[at..]);
    format!("Stop recording{chord}")
}

/// The tray icon, from the same 32 px PNG the Linux launcher installs
/// (`icons/linux_hicolor/...`), built into the binary so the tray, the
/// launcher and the master artwork can never disagree.
///
/// It is the small-icon variant (no glow, a dark outline round the F,
/// heavier brackets). Trays draw at 16 to 32 px, where the shrunken 64 px
/// icon turned to mush. `tray-icon` resamples the RGBA for each platform.
fn brand_icon() -> TrayIconImage {
    const PNG: &[u8] = include_bytes!("../../../../icons/linux_hicolor/32x32/apps/framefang.png");
    let rgba = image::load_from_memory(PNG)
        .expect("the tray PNG built into the binary decodes")
        .into_rgba8();
    let (w, h) = rgba.dimensions();
    TrayIconImage::from_rgba(rgba.into_raw(), w, h).expect("decoded RGBA makes a tray icon")
}

#[cfg(test)]
mod tests {
    use super::stop_label;

    #[test]
    fn stop_label_carries_the_record_hotkey() {
        assert_eq!(stop_label("Record (Ctrl+Shift+R)"), "Stop recording (Ctrl+Shift+R)");
        assert_eq!(stop_label("Record screen (F9)"), "Stop recording (F9)");
    }

    #[test]
    fn stop_label_without_a_hotkey_is_plain() {
        assert_eq!(stop_label("Record"), "Stop recording");
        assert_eq!(stop_label(""), "Stop recording");
    }
}
