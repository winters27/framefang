//! The About window.
//!
//! A small fixed card: the version, a byline whose name links to the
//! author's page, three label and value facts, and two buttons along the
//! bottom. The tray loop keeps it in an `Option<AboutView>`, hands it the
//! window's events, and reads `outcome` afterwards to learn what the user
//! asked for. The window itself is a [`crate::dialog::Shell`].

use anyhow::Result;
use winit::event::WindowEvent;
use winit::event_loop::ActiveEventLoop;
use winit::keyboard::NamedKey;
use winit::window::WindowId;

use crate::bitmap_font::{self, GLYPH_H};
use crate::dialog::{inside, Input, Rect, Shell};
use crate::palette::*;

const CARD_W: u32 = 480;
const CARD_H: u32 = 280;
const MARGIN: i32 = 22;
/// The version line: the body's first line, straight under the title strip
/// (the strip carries the heading).
const VERSION_Y: i32 = 22;
/// The byline, one line gap lower.
const BYLINE_Y: i32 = VERSION_Y + 26;
const BYLINE_LEAD: &str = "Made by ";
/// Drawn as a link to the author's page.
const AUTHOR: &str = "winters27.";

/// Label and value lines under the byline, one every `FACT_STEP` pixels.
const FACTS: [(&str, &str); 3] = [
    ("SOURCE", "github.com/winters27/framefang"),
    ("LICENSE", "Apache-2.0"),
    // FFmpeg is shipped beside the app and run as a separate program for
    // recording and conversion, so it keeps its own licence.
    ("CREDITS", "Uses FFmpeg (ffmpeg.org)"),
];
const FACTS_Y: i32 = BYLINE_Y + 30;
const FACT_STEP: i32 = 18;
const FACT_VALUE_X: i32 = MARGIN + 60;

const LINK_W: i32 = 200;
const LINK_H: i32 = 36;
const LINK_GAP: i32 = 12;

/// The things on the card that react to the pointer.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Spot { Author, Project, Updates }

/// Every clickable spot and where it is. The author spot covers only the
/// underlined name (not the lead-in), with a few pixels above and below so
/// it is easy to hit; the two buttons share the bottom edge, centered.
fn spots() -> [(Spot, Rect); 3] {
    let name_x = MARGIN + bitmap_font::measure(BYLINE_LEAD, 1);
    let name_w = bitmap_font::measure(AUTHOR, 1);
    let row_y = CARD_H as i32 - MARGIN - LINK_H;
    let first_x = (CARD_W as i32 - (LINK_W * 2 + LINK_GAP)) / 2;
    [
        (Spot::Author, (name_x, BYLINE_Y - 3, name_w, GLYPH_H + 8)),
        (Spot::Project, (first_x, row_y, LINK_W, LINK_H)),
        (Spot::Updates, (first_x + LINK_W + LINK_GAP, row_y, LINK_W, LINK_H)),
    ]
}

impl Spot {
    /// The button caption, or `None` for the byline link (drawn as text).
    fn caption(self) -> Option<&'static str> {
        match self {
            Spot::Author => None,
            Spot::Project => Some("Source on GitHub"),
            Spot::Updates => Some("Look for updates"),
        }
    }

    fn outcome(self) -> AboutOutcome {
        match self {
            Spot::Author => AboutOutcome::OpenAuthor,
            Spot::Project => AboutOutcome::OpenProject,
            Spot::Updates => AboutOutcome::OpenUpdates,
        }
    }
}

pub enum AboutOutcome {
    /// Dismissed: the strip's close button, Esc, Enter or the OS close.
    Closed,
    /// Open the project's source page. The card stays up.
    OpenProject,
    /// Show the Updates dialog in place of this one.
    OpenUpdates,
    /// Open the author's page. The card stays up.
    OpenAuthor,
}

pub struct AboutView {
    shell: Shell,
    /// Index into [`spots`] under the pointer.
    hover: Option<usize>,
    pub outcome: Option<AboutOutcome>,
}

impl AboutView {
    pub fn new(loop_target: &ActiveEventLoop) -> Result<Self> {
        let shell = Shell::open(loop_target, "About Frame Fang", "// ABOUT", "about", (CARD_W, CARD_H))?;
        let mut view = AboutView { shell, hover: None, outcome: None };
        view.paint();
        Ok(view)
    }

    pub fn window_id(&self) -> WindowId { self.shell.id() }

    pub fn handle_event(&mut self, event: WindowEvent) {
        match self.shell.input(event) {
            Input::Close | Input::Key(NamedKey::Escape | NamedKey::Enter) => {
                self.outcome = Some(AboutOutcome::Closed);
            }
            Input::Moved => {
                let now = spot_at(self.shell.cursor);
                self.shell.track_hover(&mut self.hover, now, now.is_some());
            }
            Input::Press => {
                if let Some(i) = spot_at(self.shell.cursor) {
                    self.outcome = Some(spots()[i].0.outcome());
                }
            }
            Input::Paint => self.paint(),
            _ => {}
        }
    }

    fn paint(&mut self) {
        let Some(mut page) = self.shell.page() else { return };

        // The pixel font only has printable ASCII, so the separator is
        // slashes rather than a middle dot.
        let version = format!("v{}  //  a small, fast screenshot tool", env!("CARGO_PKG_VERSION"));
        page.text(MARGIN, VERSION_Y, &version, TEXT_MUTED());

        page.text(MARGIN, BYLINE_Y, BYLINE_LEAD, TEXT_BRIGHT);
        let (_, name) = spots()[0];
        let link_ink = if self.hover == Some(0) { LASER() } else { SECTION_TINT() };
        page.text(name.0, BYLINE_Y, AUTHOR, link_ink);
        // Underlined so it reads as a link.
        page.fill((name.0, BYLINE_Y + GLYPH_H, name.2, 1), link_ink);

        for (row, (label, value)) in FACTS.iter().enumerate() {
            let y = FACTS_Y + row as i32 * FACT_STEP;
            page.text(MARGIN, y, label, SECTION_TINT());
            page.text(FACT_VALUE_X, y, value, TEXT_BRIGHT);
        }

        // No Close button of its own: the title strip has one.
        for (i, (spot, rect)) in spots().into_iter().enumerate() {
            if let Some(caption) = spot.caption() {
                page.button(rect, caption, false, self.hover == Some(i));
            }
        }

        self.shell.show(page);
    }
}

fn spot_at(p: (i32, i32)) -> Option<usize> {
    spots().iter().position(|&(_, r)| inside(r, p))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_author_link_covers_just_the_name() {
        let (spot, r) = spots()[0];
        assert!(spot == Spot::Author);
        let lead_end = MARGIN + bitmap_font::measure(BYLINE_LEAD, 1);
        assert!(!inside(r, (lead_end - 1, BYLINE_Y)), "the lead-in isn't part of the link");
        assert!(inside(r, (lead_end, BYLINE_Y)));
        assert!(inside(r, (lead_end + bitmap_font::measure(AUTHOR, 1) - 1, BYLINE_Y + GLYPH_H)));
        assert!(spot_at((lead_end + 1, BYLINE_Y)) == Some(0));
    }

    #[test]
    fn about_buttons_sit_on_the_card_side_by_side_and_their_captions_fit() {
        let [_, (_, a), (_, b)] = spots();
        assert_eq!(a.1, b.1);
        assert_eq!(b.0 - (a.0 + a.2), LINK_GAP);
        assert_eq!(a.0, CARD_W as i32 - (b.0 + b.2), "centered");
        assert!(b.1 + b.3 <= CARD_H as i32 - MARGIN);
        for (spot, r) in spots() {
            if let Some(caption) = spot.caption() {
                assert!(bitmap_font::measure(caption, 1) < r.2 - 16, "{caption}");
            }
        }
        for (label, value) in FACTS {
            assert!(FACT_VALUE_X - MARGIN > bitmap_font::measure(label, 1), "{label}");
            assert!(FACT_VALUE_X + bitmap_font::measure(value, 1) <= CARD_W as i32 - MARGIN, "{value}");
        }
    }
}
