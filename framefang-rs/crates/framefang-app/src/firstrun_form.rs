//! The welcome window, shown once on the very first launch.
//!
//! After start-up the app has no window of its own, only a tray icon, and
//! the release builds have no console for a "started" line either. Someone
//! running it for the first time sees a desktop that looks just as it did
//! and can fairly assume nothing happened. This card says otherwise: the
//! app is running, here is the capture key, here is where pictures go, and
//! here is where this OS puts the tray icon.
//!
//! Built on the shared dialog shell (`dialog::Shell`). Closing it never
//! quits the app; the tray loop just drops the view.

use anyhow::Result;
use winit::event::WindowEvent;
use winit::event_loop::ActiveEventLoop;
use winit::keyboard::NamedKey;
use winit::window::WindowId;

use crate::dialog::{fit_middle, inside, Input, Rect, Shell};
use crate::palette::*;

const CARD_W: u32 = 520;
const CARD_H: u32 = 248;
const MARGIN: i32 = 22;
/// The version line, the body's first; the title strip carries the heading.
const VERSION_Y: i32 = 22;
const RUNNING_Y: i32 = VERSION_Y + 26;
/// The HOTKEY line; SAVE TO follows one step lower.
const SETTINGS_Y: i32 = RUNNING_Y + 30;
const SETTINGS_STEP: i32 = 18;
/// Where the values in the HOTKEY and SAVE TO lines start.
const VALUE_X: i32 = MARGIN + 90;
const HINTS_Y: i32 = SETTINGS_Y + SETTINGS_STEP + 32;
const HINT_STEP: i32 = 16;

const OK_W: i32 = 140;
const OK_H: i32 = 34;
/// The one button, in the bottom-right corner.
const OK_BUTTON: Rect = (CARD_W as i32 - MARGIN - OK_W, CARD_H as i32 - MARGIN - OK_H, OK_W, OK_H);

pub enum FirstRunOutcome {
    /// Dismissed by the button, Esc, Enter or a close. The app carries on.
    Closed,
}

pub struct FirstRunView {
    shell: Shell,
    hotkey: String,
    save_dir: String,
    hints: Vec<&'static str>,
    over_button: bool,
    pub outcome: Option<FirstRunOutcome>,
}

impl FirstRunView {
    /// `hotkey` is the capture key as `Hotkey::describe` writes it and
    /// `save_dir` the folder screenshots go to, both ready to show.
    pub fn new(
        loop_target: &ActiveEventLoop,
        hotkey: &str,
        save_dir: &str,
        wayland: bool,
    ) -> Result<Self> {
        let shell = Shell::open(loop_target, "Welcome to Frame Fang", "// WELCOME", "welcome", (CARD_W, CARD_H))?;
        let mut view = FirstRunView {
            shell,
            hotkey: hotkey.into(),
            save_dir: save_dir.into(),
            hints: tray_hints(wayland),
            over_button: false,
            outcome: None,
        };
        view.paint();
        Ok(view)
    }

    pub fn window_id(&self) -> WindowId { self.shell.id() }

    pub fn handle_event(&mut self, event: WindowEvent) {
        match self.shell.input(event) {
            Input::Close | Input::Key(NamedKey::Escape | NamedKey::Enter) => {
                self.outcome = Some(FirstRunOutcome::Closed);
            }
            Input::Moved => {
                let over = inside(OK_BUTTON, self.shell.cursor);
                self.shell.track_hover(&mut self.over_button, over, over);
            }
            Input::Press if inside(OK_BUTTON, self.shell.cursor) => {
                self.outcome = Some(FirstRunOutcome::Closed);
            }
            Input::Paint => self.paint(),
            _ => {}
        }
    }

    fn paint(&mut self) {
        let Some(mut page) = self.shell.page() else { return };

        let version = format!("v{}  //  welcome", env!("CARGO_PKG_VERSION"));
        page.text(MARGIN, VERSION_Y, &version, TEXT_MUTED());
        page.text(MARGIN, RUNNING_Y, "Frame Fang is now running in the tray / menu bar.", TEXT_BRIGHT);

        // The two settings a newcomer most needs, as they are right now, so
        // there's no trip to Settings just to find them.
        page.text(MARGIN, SETTINGS_Y, "HOTKEY", SECTION_TINT());
        crate::keycap::draw_line_on(&mut page, VALUE_X, SETTINGS_Y, &crate::keycap::chord(&self.hotkey), 1, TEXT_BRIGHT);
        let folder_y = SETTINGS_Y + SETTINGS_STEP;
        page.text(MARGIN, folder_y, "SAVE TO", SECTION_TINT());
        let folder = fit_middle(&self.save_dir, CARD_W as i32 - MARGIN - VALUE_X);
        page.text(VALUE_X, folder_y, &folder, TEXT_BRIGHT);

        let mut y = HINTS_Y;
        for hint in &self.hints {
            page.text(MARGIN, y, hint, TEXT_MUTED());
            y += HINT_STEP;
        }

        page.button(OK_BUTTON, "Okay", true, self.over_button);
        self.shell.show(page);
    }
}

/// Where this OS shows the tray icon, a short line each. Chosen at run
/// time, so every branch compiles on every platform.
///
/// Kept concrete on purpose: the usual "it didn't start" report on Windows
/// is an icon tucked away in the hidden-icons overflow.
fn tray_hints(wayland: bool) -> Vec<&'static str> {
    match std::env::consts::OS {
        "windows" => vec![
            "New tray icons start out hidden behind the taskbar's ^ arrow.",
            "Open it and drag the Frame Fang icon onto the taskbar to keep it.",
        ],
        "macos" => vec![
            "The Frame Fang icon sits at the top right, in the menu bar.",
            "There is no Dock icon: Frame Fang lives in the menu bar.",
        ],
        _ => {
            let mut hints = vec![
                "Showing the icon takes a tray host (AppIndicator) on the desktop.",
                "On GNOME, add the AppIndicator extension to see it.",
            ];
            // Wayland lets no app grab a key itself: the capture shortcut is
            // asked for through the desktop's global-shortcuts portal, and
            // the desktop decides what it finally is.
            hints.extend(wayland.then_some("Under Wayland, your desktop picks the capture shortcut."));
            hints
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bitmap_font::measure;

    #[test]
    fn every_line_fits_the_card_in_the_pixel_font() {
        let room = CARD_W as i32 - 2 * MARGIN;
        let lines = tray_hints(true).into_iter().chain(["Frame Fang is now running in the tray / menu bar."]);
        for line in lines {
            assert!(measure(line, 1) <= room, "too wide: {line}");
            assert!(line.chars().all(|c| (' '..='~').contains(&c)), "the font can't draw: {line}");
        }
        assert!(measure("Okay", 1) < OK_W - 16);
    }

    #[test]
    fn the_wayland_line_appears_only_on_wayland_desktops() {
        let (with, without) = (tray_hints(true).len(), tray_hints(false).len());
        if cfg!(any(target_os = "windows", target_os = "macos")) {
            assert_eq!(with, without);
        } else {
            assert_eq!(with, without + 1);
        }
        assert!(without >= 2);
    }

    #[test]
    fn the_hints_end_above_the_button() {
        let lines = tray_hints(true).len() as i32;
        let last_bottom = HINTS_Y + (lines - 1) * HINT_STEP + crate::bitmap_font::GLYPH_H;
        assert!(last_bottom < OK_BUTTON.1 + OK_BUTTON.3, "hints run past the button row");
        assert!(OK_BUTTON.0 + OK_BUTTON.2 == CARD_W as i32 - MARGIN);
    }
}
