//! The Convert image dialog, and the form pieces it shares with Convert
//! video.
//!
//! The user picks a picture, a format to save it as (PNG, JPG, WEBP or
//! BMP), optionally a smaller size and, for the formats that have one, a
//! quality. Convert writes `<name>.framefang.<ext>` beside the original,
//! which is never touched, and says where it went. Everything is done with
//! the `image` crate in-process; nothing has to be installed.
//!
//! The shared half ([`Form`], [`Status`], [`draw_pill`], [`draw_source`],
//! [`output_beside`], [`choose_file`]) is the layout both Convert dialogs
//! use: numbered groups of controls down a column, a file field at the
//! top, and the Convert button with a status line along the bottom.

use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::Result;
use image::imageops::FilterType;
use winit::event::WindowEvent;
use winit::event_loop::ActiveEventLoop;
use winit::keyboard::NamedKey;
use winit::window::WindowId;

use crate::dialog::{fit_middle, inside, Input, Page, Rect, Shell};
use crate::bitmap_font::{measure, GLYPH_H};
use crate::palette::*;

const CARD_W: u32 = 720;
const CARD_H: u32 = 571;
const CARD: (u32, u32) = (CARD_W, CARD_H);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ImgFormat { Png, Jpg, Webp, Bmp }

const FORMATS: [ImgFormat; 4] = [ImgFormat::Png, ImgFormat::Jpg, ImgFormat::Webp, ImgFormat::Bmp];

impl ImgFormat {
    fn ext(self) -> &'static str {
        match self {
            ImgFormat::Png => "png",
            ImgFormat::Jpg => "jpg",
            ImgFormat::Webp => "webp",
            ImgFormat::Bmp => "bmp",
        }
    }

    fn caption(self) -> &'static str {
        match self {
            ImgFormat::Png => "PNG",
            ImgFormat::Jpg => "JPG",
            ImgFormat::Webp => "WEBP",
            ImgFormat::Bmp => "BMP",
        }
    }

    /// Whether the Quality slider means anything for this format, and so
    /// whether it is shown at all.
    fn has_quality(self) -> bool { matches!(self, ImgFormat::Jpg | ImgFormat::Webp) }
}

/// Output size. Shrinking only: scaling a screenshot up adds nothing but
/// blur, so there is no option for it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ImgResize { Source, P50, P25 }

const SIZES: [ImgResize; 3] = [ImgResize::Source, ImgResize::P50, ImgResize::P25];

impl ImgResize {
    /// What both sides are divided by, or `None` to keep the size.
    fn divisor(self) -> Option<u32> {
        match self {
            ImgResize::Source => None,
            ImgResize::P50 => Some(2),
            ImgResize::P25 => Some(4),
        }
    }

    fn caption(self) -> &'static str {
        match self {
            ImgResize::Source => "Full size",
            ImgResize::P50 => "50%",
            ImgResize::P25 => "25%",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Control { Convert, Source, Format(ImgFormat), Resize(ImgResize), Quality }

pub enum ConvertImageOutcome {
    Closed,
}

pub struct ConvertImageView {
    shell: Shell,
    form: Form<Control>,
    source: Option<PathBuf>,
    format: ImgFormat,
    /// 0 to 100; used by JPG.
    quality: u8,
    resize: ImgResize,
    hover: Option<Control>,
    /// The Quality knob is held down.
    dragging: bool,
    status: Status,
    pub outcome: Option<ConvertImageOutcome>,
}

impl ConvertImageView {
    pub fn new(loop_target: &ActiveEventLoop) -> Result<Self> {
        let shell = Shell::open(loop_target, "Frame Fang: Convert image", "// CONVERT IMAGE", "convert-image", CARD)?;
        let form = Form::new(CARD_W)
            .field("INPUT", Control::Source)
            .pills("OUTPUT FORMAT", &FORMATS.map(Control::Format))
            .pills("SIZE  (can only shrink)", &SIZES.map(Control::Resize))
            .field("QUALITY", Control::Quality);
        // Starts on PNG at full size; JPG's quality starts at 90.
        let mut view = ConvertImageView {
            shell, form, quality: 90,
            format: ImgFormat::Png, resize: ImgResize::Source,
            source: None, hover: None, dragging: false, status: Status::Idle, outcome: None,
        };
        view.paint();
        Ok(view)
    }

    pub fn window_id(&self) -> WindowId { self.shell.id() }

    pub fn handle_event(&mut self, event: WindowEvent) {
        match self.shell.input(event) {
            Input::Close | Input::Key(NamedKey::Escape) => self.outcome = Some(ConvertImageOutcome::Closed),
            Input::Moved => {
                if self.dragging { self.quality_follows_pointer(); }
                let now = self.control_at(self.shell.cursor);
                self.shell.track_hover(&mut self.hover, now, now.is_some());
            }
            Input::Press => {
                if let Some(control) = self.control_at(self.shell.cursor) {
                    self.press(control);
                    self.shell.repaint();
                }
            }
            Input::Release if self.dragging => {
                self.dragging = false;
                self.shell.repaint();
            }
            Input::Paint => self.paint(),
            _ => {}
        }
    }

    /// Controls hidden for the chosen format neither draw nor take clicks.
    fn shows(&self, control: Control) -> bool {
        control != Control::Quality || self.format.has_quality()
    }

    fn control_at(&self, p: (i32, i32)) -> Option<Control> {
        if inside(convert_button(CARD), p) { return Some(Control::Convert); }
        self.form.hit(p, |c| self.shows(c))
    }

    fn press(&mut self, control: Control) {
        match control {
            Control::Source => {
                let picked = choose_file("Choose an image", self.source.as_deref(), directories::UserDirs::picture_dir,
                    "Images", &["png", "jpg", "jpeg", "webp", "bmp", "gif", "tiff", "tif"]);
                // A new choice clears the last result line.
                if picked.is_some() { (self.source, self.status) = (picked, Status::Idle); }
            }
            Control::Format(f) => (self.format, self.status) = (f, Status::Idle),
            Control::Resize(r) => self.resize = r,
            Control::Quality => {
                self.dragging = true;
                self.quality_follows_pointer();
            }
            Control::Convert => self.convert(),
        }
    }

    fn quality_follows_pointer(&mut self) {
        let Some(row) = self.form.rect_of(Control::Quality) else { return };
        if let Some(q) = quality_at(self.shell.cursor.0, row) {
            self.quality = q;
            self.shell.repaint();
        }
    }

    /// Write the converted copy and leave the result in `status` for the
    /// next paint, good or bad.
    fn convert(&mut self) {
        self.status = match &self.source {
            None => Status::Failed("Choose an image to convert first.".to_owned()),
            Some(src) => {
                let dst = output_beside(src, self.format.ext());
                match write_converted(src, &dst, self.format, self.quality, self.resize) {
                    Ok(()) => Status::Saved(dst),
                    Err(e) => Status::Failed(e),
                }
            }
        };
    }

    fn paint(&mut self) {
        let Some(mut page) = self.shell.page() else { return };
        self.form.paint(&mut page, |c| self.shows(c), |page, control, r| {
            let hovered = self.hover == Some(control);
            match control {
                Control::Source => draw_source(page, r, "Image file", self.source.as_deref(), "(choose an image)", hovered),
                Control::Format(f) => draw_pill(page, r, f.caption(), true, f == self.format, hovered),
                Control::Resize(s) => draw_pill(page, r, s.caption(), false, s == self.resize, hovered),
                Control::Quality => draw_quality(page, r, self.quality),
                Control::Convert => {}
            }
        });
        page.button(convert_button(CARD), "Convert", true, self.hover == Some(Control::Convert));
        self.status.paint(&mut page, CARD);
        self.shell.show(page);
    }
}

const TRACK_H: i32 = 6;
const KNOB: i32 = 14;

/// The Quality row: its name, a track filled up to the value, a knob, and
/// the value as a percentage at the right.
fn draw_quality(page: &mut Page, r: Rect, quality: u8) {
    page.text(r.0 + 6, r.1 + (r.3 - GLYPH_H) / 2, "Quality", TEXT_BRIGHT);
    let (vx, vy, vw, vh) = value_area(r);
    let readout = format!("{quality}%");
    let track = (vx + 4, vy + (vh - TRACK_H) / 2, vw - 8 - (measure(&readout, 1) + 14), TRACK_H);
    page.fill(track, FIELD_BG());
    page.outline(track, FIELD_BORDER());
    let t = f32::from(quality.min(100)) / 100.0;
    let filled = (track.2 as f32 * t).round() as i32;
    if filled > 0 { page.fill((track.0, track.1, filled, TRACK_H), LASER_DIM()); }
    let knob_x = track.0 + ((track.2 - 1) as f32 * t).round() as i32 - KNOB / 2;
    let knob_x = knob_x.clamp(track.0 - 1, track.0 + track.2 - KNOB + 1);
    let knob = (knob_x, track.1 + (TRACK_H - KNOB) / 2, KNOB, KNOB);
    page.fill(knob, LASER());
    page.outline(knob, TEXT_BRIGHT);
    page.text(track.0 + track.2 + 10, vy + (vh - GLYPH_H) / 2, &readout, TEXT_BRIGHT);
}

/// The quality a press or drag at `x` picks on the Quality row `row`: the
/// row's value area, left edge 0 and right edge 100.
fn quality_at(x: i32, row: Rect) -> Option<u8> {
    let (left, _, span, _) = value_area(row);
    if span <= 1 { return None; }
    let t = ((x - left) as f32 / (span - 1) as f32).clamp(0.0, 1.0);
    Some((t * 100.0).round() as u8)
}

/// Read `src`, shrink it if asked, and save it to `dst` as `format`.
fn write_converted(src: &Path, dst: &Path, format: ImgFormat, quality: u8, size: ImgResize) -> Result<(), String> {
    let mut picture = image::open(src).map_err(|e| format!("Couldn't read the image: {e}"))?;
    if let Some(d) = size.divisor() {
        // Lanczos3 is the sharpest downscale the crate has, worth it when a
        // 4K screenshot is being shrunk to drop into a document.
        let (w, h) = ((picture.width() / d).max(1), (picture.height() / d).max(1));
        picture = picture.resize(w, h, FilterType::Lanczos3);
    }
    let saved = match format {
        // Both encoders are always built in; the extension picks which.
        ImgFormat::Png | ImgFormat::Bmp => picture.save(dst).map_err(|e| e.to_string()),
        ImgFormat::Jpg => {
            let rgb = picture.to_rgb8();
            let mut bytes = Vec::new();
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, quality)
                .encode(rgb.as_raw(), rgb.width(), rgb.height(), image::ExtendedColorType::Rgb8)
                .map_err(|e| e.to_string())
                .and_then(|()| std::fs::write(dst, &bytes).map_err(|e| e.to_string()))
        }
        // The crate's WEBP encoder is lossless only, so the slider shows
        // but has no effect here yet.
        ImgFormat::Webp => picture.save_with_format(dst, image::ImageFormat::WebP).map_err(|e| e.to_string()),
    };
    saved.map_err(|e| format!("Couldn't save the image: {e}"))
}

// ── Shared by both Convert dialogs ──────────────────────────────────────────

/// Space between the body's edges and its content.
pub(crate) const EDGE: i32 = 24;
/// The first group's controls; its heading sits `HEADING_LIFT` above.
const FIRST_ROW_Y: i32 = 44;
const HEADING_LIFT: i32 = 22;
/// From the bottom of one group's controls to the top of the next group's.
const GROUP_GAP: i32 = 40;
/// A file field row.
const FIELD_H: i32 = 36;
/// The width of a field row's name, before its value starts.
const LABEL_W: i32 = 150;
const PILL_H: i32 = 46;
const PILL_GAP: i32 = 14;
const BROWSE_W: i32 = 90;
const BTN_H: i32 = 34;
const CONVERT_W: i32 = 140;
/// Behind the chosen pill.
const PICKED_FILL: u32 = 0x000C_2820;

/// A column of numbered groups, each a heading with its controls under it.
pub(crate) struct Form<C> {
    left: i32,
    span: i32,
    next_y: i32,
    groups: Vec<(&'static str, Vec<(C, Rect)>)>,
}

impl<C: Copy + PartialEq> Form<C> {
    /// An empty column filling a card `card_w` wide.
    pub(crate) fn new(card_w: u32) -> Self {
        Form { left: EDGE, span: card_w as i32 - 2 * EDGE, next_y: FIRST_ROW_Y, groups: Vec::new() }
    }

    fn group(mut self, title: &'static str, items: Vec<(C, Rect)>, h: i32) -> Self {
        self.groups.push((title, items));
        self.next_y += h + GROUP_GAP;
        self
    }

    /// A group holding one full-width field row.
    pub(crate) fn field(self, title: &'static str, control: C) -> Self {
        let r = (self.left, self.next_y, self.span, FIELD_H);
        self.group(title, vec![(control, r)], FIELD_H)
    }

    /// A group of equal pills side by side across the full width.
    pub(crate) fn pills(self, title: &'static str, controls: &[C]) -> Self {
        let n = controls.len() as i32;
        let w = (self.span - PILL_GAP * (n - 1)) / n;
        let items = controls.iter().zip(0..)
            .map(|(&c, i)| (c, (self.left + i * (w + PILL_GAP), self.next_y, w, PILL_H)))
            .collect();
        self.group(title, items, PILL_H)
    }

    pub(crate) fn rect_of(&self, control: C) -> Option<Rect> {
        self.items().find(|&(c, _)| c == control).map(|(_, r)| r)
    }

    /// The shown control under `p`.
    pub(crate) fn hit(&self, p: (i32, i32), shows: impl Fn(C) -> bool) -> Option<C> {
        self.items().find(|&(c, r)| shows(c) && inside(r, p)).map(|(c, _)| c)
    }

    /// Draw every shown group: its heading, numbered among the shown ones,
    /// then each control through `draw`. A group is shown when its first
    /// control is.
    pub(crate) fn paint(&self, page: &mut Page, shows: impl Fn(C) -> bool, mut draw: impl FnMut(&mut Page, C, Rect)) {
        let mut number = 0;
        for (title, items) in &self.groups {
            let Some(&(first, r)) = items.first() else { continue };
            if !shows(first) { continue; }
            number += 1;
            page.section(self.left, self.left + self.span, r.1 - HEADING_LIFT, number, title);
            for &(c, r) in items.iter().filter(|(c, _)| shows(*c)) {
                draw(page, c, r);
            }
        }
    }

    fn items(&self) -> impl Iterator<Item = (C, Rect)> + '_ {
        self.groups.iter().flat_map(|(_, items)| items.iter().copied())
    }
}

/// Where a Convert dialog's main button goes: the bottom-right corner.
pub(crate) fn convert_button(card: (u32, u32)) -> Rect {
    (card.0 as i32 - EDGE - CONVERT_W, card.1 as i32 - EDGE - BTN_H, CONVERT_W, BTN_H)
}

/// A choice pill: filled and lettered in the accent when chosen. `big`
/// letters are for the main choice (the format), so the other rows read
/// as secondary.
pub(crate) fn draw_pill(page: &mut Page, r: Rect, caption: &str, big: bool, chosen: bool, hovered: bool) {
    let (fill, edge, ink) = if chosen {
        (PICKED_FILL, LASER(), LASER())
    } else if hovered {
        (HOVER_FILL(), LASER_DIM(), TEXT_BRIGHT)
    } else {
        (FIELD_BG(), FIELD_BORDER(), TEXT_BRIGHT)
    };
    page.fill(r, fill);
    page.outline(r, edge);
    page.centered(r, caption, if big { 2 } else { 1 }, ink);
}

/// The value part of a field row: right of its name, inset a little.
fn value_area(r: Rect) -> Rect { (r.0 + LABEL_W, r.1 + 4, r.2 - LABEL_W - 4, r.3 - 8) }

/// The file row: its name, the chosen path (or `placeholder`) in a field,
/// and a Browse button. The whole row is one click target.
pub(crate) fn draw_source(page: &mut Page, r: Rect, caption: &str, chosen: Option<&Path>, placeholder: &str, hovered: bool) {
    if hovered { page.fill(r, HOVER_FILL()); }
    page.text(r.0 + 6, r.1 + (r.3 - GLYPH_H) / 2, caption, TEXT_BRIGHT);
    let (x, y, w, h) = value_area(r);
    let field = (x, y, w - BROWSE_W - 8, h);
    page.fill(field, FIELD_BG());
    page.outline(field, FIELD_BORDER());
    let (shown, ink) = match chosen {
        Some(p) => (p.to_string_lossy().into_owned(), TEXT_BRIGHT),
        None => (placeholder.to_owned(), TEXT_DIM()),
    };
    page.text(x + 7, y + (h - GLYPH_H) / 2, &fit_middle(&shown, field.2 - 14), ink);
    let browse = (x + field.2 + 8, y, BROWSE_W, h);
    page.outline(browse, if hovered { LASER_DIM() } else { FIELD_BORDER() });
    page.centered(browse, "Browse...", 1, TEXT_BRIGHT);
}

/// What the last Convert press came to.
pub(crate) enum Status {
    Idle,
    /// Running in the background since then.
    Working(Instant),
    Saved(PathBuf),
    Failed(String),
}

impl Status {
    /// The status line, left of the Convert button and level with it.
    pub(crate) fn paint(&self, page: &mut Page, card: (u32, u32)) {
        let (line, ink) = match self {
            Status::Idle => return,
            Status::Working(since) => {
                let ms = since.elapsed().as_millis();
                let dots = ".".repeat((ms / 400 % 4) as usize);
                (format!("working{dots}  {}s  (keep using the app meanwhile)", ms / 1000), TEXT_MUTED())
            }
            Status::Saved(path) => (format!("Saved to {}", path.display()), OK_TINT),
            Status::Failed(why) => (why.clone(), DANGER()),
        };
        let y = card.1 as i32 - EDGE - (BTN_H + GLYPH_H) / 2;
        let room = card.0 as i32 - EDGE - CONVERT_W - 16 - EDGE;
        page.text(EDGE, y, &fit_middle(&line, room), ink);
    }
}

/// Where a converted copy of `src` goes: beside it, as
/// `<stem>.framefang.<ext>`, so the original stays as it was.
pub(crate) fn output_beside(src: &Path, ext: &str) -> PathBuf {
    let stem = src.file_stem().map_or_else(|| "framefang".into(), |s| s.to_string_lossy());
    let folder = src.parent().map_or_else(std::env::temp_dir, Path::to_path_buf);
    folder.join(format!("{stem}.framefang.{ext}"))
}

/// Ask for one file with the system's picker, opening in the current
/// file's folder, else the user's `usual` folder, else the temp folder.
pub(crate) fn choose_file(
    title: &str,
    current: Option<&Path>,
    usual: fn(&directories::UserDirs) -> Option<&Path>,
    kind: &str,
    extensions: &[&str],
) -> Option<PathBuf> {
    let start = current
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .or_else(|| directories::UserDirs::new().and_then(|dirs| usual(&dirs).map(Path::to_path_buf)))
        .unwrap_or_else(std::env::temp_dir);
    rfd::FileDialog::new()
        .set_title(title)
        .set_directory(&start)
        .add_filter(kind, extensions)
        .pick_file()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout() -> Form<Control> {
        Form::new(CARD_W)
            .field("INPUT", Control::Source)
            .pills("OUTPUT FORMAT", &FORMATS.map(Control::Format))
            .pills("SIZE", &SIZES.map(Control::Resize))
            .field("QUALITY", Control::Quality)
    }

    #[test]
    fn the_form_stacks_its_groups_forty_pixels_apart() {
        let form = layout();
        assert_eq!(form.rect_of(Control::Source), Some((24, 44, 672, 36)));
        assert_eq!(form.rect_of(Control::Format(ImgFormat::Png)), Some((24, 120, 157, 46)));
        assert_eq!(form.rect_of(Control::Format(ImgFormat::Bmp)), Some((24 + 3 * 171, 120, 157, 46)));
        assert_eq!(form.rect_of(Control::Resize(ImgResize::P25)), Some((24 + 2 * 228, 206, 214, 46)));
        assert_eq!(form.rect_of(Control::Quality), Some((24, 292, 672, 36)));
        assert_eq!(convert_button(CARD), (556, 513, 140, 34));
    }

    #[test]
    fn hidden_controls_take_no_clicks() {
        let form = layout();
        let quality = form.rect_of(Control::Quality).unwrap();
        let p = (quality.0 + 200, quality.1 + 5);
        assert!(form.hit(p, |_| true) == Some(Control::Quality));
        assert!(form.hit(p, |c| c != Control::Quality).is_none());
        // Between two pills is nothing.
        assert!(form.hit((24 + 157 + 5, 130), |_| true).is_none());
    }

    #[test]
    fn quality_runs_from_the_left_of_the_value_area_to_its_right() {
        let row = (24, 292, 672, 36);
        let (left, _, span, _) = value_area(row);
        assert_eq!(quality_at(left, row), Some(0));
        assert_eq!(quality_at(left - 50, row), Some(0));
        assert_eq!(quality_at(left + span - 1, row), Some(100));
        assert_eq!(quality_at(left + span + 50, row), Some(100));
        assert_eq!(quality_at(left + (span - 1) / 2, row), Some(50));
        assert_eq!(quality_at(0, (0, 0, LABEL_W + 5, 36)), None, "no room to drag in");
        assert!(ImgFormat::Jpg.has_quality() && ImgFormat::Webp.has_quality());
        assert!(!ImgFormat::Png.has_quality() && !ImgFormat::Bmp.has_quality());
    }

    #[test]
    fn the_copy_goes_beside_the_original_with_a_framefang_tag() {
        let src = Path::new("shots").join("cat.final.png");
        assert_eq!(output_beside(&src, "webp"), Path::new("shots").join("cat.final.framefang.webp"));
        assert_eq!(output_beside(Path::new(""), "jpg"), std::env::temp_dir().join("framefang.framefang.jpg"));
    }

    #[test]
    fn every_format_round_trips_and_shrinks_on_request() {
        let dir = std::env::temp_dir().join(format!("ff-convert-image-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("in.png");
        image::RgbaImage::from_fn(16, 12, |x, y| image::Rgba([x as u8 * 15, y as u8 * 20, 90, 255]))
            .save(&src).unwrap();

        for (format, size, want, kind) in [
            (ImgFormat::Png, ImgResize::Source, (16, 12), image::ImageFormat::Png),
            (ImgFormat::Jpg, ImgResize::P50, (8, 6), image::ImageFormat::Jpeg),
            (ImgFormat::Webp, ImgResize::P50, (8, 6), image::ImageFormat::WebP),
            (ImgFormat::Bmp, ImgResize::P25, (4, 3), image::ImageFormat::Bmp),
        ] {
            let dst = output_beside(&src, format.ext());
            write_converted(&src, &dst, format, 90, size).unwrap();
            let bytes = std::fs::read(&dst).unwrap();
            assert_eq!(image::guess_format(&bytes).unwrap(), kind, "{format:?} wrote the wrong kind of file");
            let back = image::load_from_memory(&bytes).unwrap();
            assert_eq!((back.width(), back.height()), want, "{format:?}");
        }

        let missing = write_converted(&dir.join("gone.png"), &dir.join("x.png"), ImgFormat::Png, 90, ImgResize::Source);
        assert!(missing.unwrap_err().starts_with("Couldn't read the image"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn captions_fit_their_pills() {
        let form = layout();
        for (c, r) in form.items() {
            let (caption, scale) = match c {
                Control::Format(f) => (f.caption(), 2),
                Control::Resize(s) => (s.caption(), 1),
                _ => continue,
            };
            assert!(measure(caption, scale) < r.2 - 8, "{caption}");
        }
    }
}
