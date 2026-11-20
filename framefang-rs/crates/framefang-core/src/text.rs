//! Laying out and rasterizing what the user types into a Text annotation.
//!
//! The app's own labels (buttons, the size chip, step numbers) are drawn by
//! `bitmap_font.rs` in `framefang-app`, a crisp 5x7 grid that only needs
//! ASCII. User text needs far more: accented letters, Greek, Cyrillic,
//! currency signs, arrows. This module handles that.
//!
//! It depends on nothing installed on the machine. The typeface
//! (`assets/fonts/NotoSans-Regular.ttf`, OFL-1.1) is built into the binary and
//! `fontdue` rasterizes it, so a Text annotation looks identical on Windows,
//! Linux and macOS.
//!
//! Coordinates are relative to the top-left of the text block, with y
//! growing downward like the painter's surfaces. To draw a [`TextBlock`],
//! add the annotation's position to each glyph's `x` and `y`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use fontdue::{Font, FontSettings, Metrics};
use unicode_segmentation::UnicodeSegmentation;

/// The built-in typeface. Provenance is in `assets/fonts/README.md`, the
/// license in `assets/fonts/OFL.txt`.
pub const FONT_DATA: &[u8] = include_bytes!("../assets/fonts/NotoSans-Regular.ttf");

/// Smallest em-size, in pixels, we draw at. Below this glyphs are noise.
pub const MIN_PX: f32 = 6.0;
/// Largest em-size we draw at, so a silly `font_size` in a hand-edited
/// session file cannot ask for a glyph bitmap hundreds of megabytes big.
pub const MAX_PX: f32 = 400.0;

/// The em-size used when nothing else decides. It is the size the middle
/// step of the editor's thickness cycle produces.
pub const DEFAULT_PX: f32 = 28.0;

/// The parsed built-in font, loaded on first use and kept for the life of
/// the process.
pub fn font() -> &'static Font {
    static PARSED: OnceLock<Font> = OnceLock::new();
    PARSED.get_or_init(|| {
        let settings = FontSettings::default();
        match Font::from_bytes(FONT_DATA, settings) {
            Ok(parsed) => parsed,
            Err(why) => panic!("could not read the built-in Noto Sans font ({why}); assets/fonts is damaged"),
        }
    })
}

/// Bring any requested size into the range we draw at. A size that is not
/// a number at all becomes [`DEFAULT_PX`].
pub fn clamp_px(px: f32) -> f32 {
    match px.is_finite() {
        true => px.clamp(MIN_PX, MAX_PX),
        false => DEFAULT_PX,
    }
}

/// Text size at zero stroke thickness.
const BASE_TEXT_PX: f32 = 14.0;
/// Extra text size per pixel of stroke thickness.
const TEXT_PX_PER_STROKE_PX: f32 = 3.5;

/// The text size that goes with a stroke thickness. The editor's thickness
/// cycle of 2, 4 and 8 px gives 21, 28 and 42 px text: three sizes that are
/// easy to tell apart and all readable at 100 % zoom.
pub fn font_size_for_thickness(thickness: f32) -> f32 {
    // Reject NaN and infinities here: `f32::max` below would quietly turn a
    // NaN into 0 and hide the bad input instead of falling back.
    if !thickness.is_finite() {
        return DEFAULT_PX;
    }
    let stroke = thickness.max(0.0);
    clamp_px(BASE_TEXT_PX + stroke * TEXT_PX_PER_STROKE_PX)
}

/// Distance from one baseline to the next at `px`.
pub fn line_height(px: f32) -> f32 {
    let size = clamp_px(px);
    font().horizontal_line_metrics(size).map_or(size * 1.35, |lm| lm.new_line_size)
}

/// Distance from the top of the block down to the first baseline at `px`.
pub fn ascent(px: f32) -> f32 {
    let size = clamp_px(px);
    font().horizontal_line_metrics(size).map_or(size, |lm| lm.ascent)
}

// Measuring

/// Walk the pen along one line, adding kerning between each pair of
/// characters. `advance` is called once per character with the pen position
/// where that character starts and returns how far it moves the pen. The
/// result is the line's width (never negative).
///
/// Measuring and layout both go through here, so they add up the same
/// numbers in the same order and always agree.
fn walk_line(line: &str, px: f32, mut advance: impl FnMut(char, f32) -> f32) -> f32 {
    let face = font();
    let mut pen = 0.0f32;
    let mut before: Option<char> = None;
    line.chars().for_each(|letter| {
        if let Some(left) = before {
            pen += face.horizontal_kern(left, letter, px).unwrap_or(0.0);
        }
        pen += advance(letter, pen);
        before = Some(letter);
    });
    pen.max(0.0)
}

/// How far one line of text (no newlines) advances the pen, kerning
/// included.
pub fn line_width(line: &str, px: f32) -> f32 {
    let size = clamp_px(px);
    walk_line(line, size, |ch, _| font().metrics(ch, size).advance_width)
}

/// The advance width of every line. Text ending in a newline has an empty
/// last line, which is where the caret goes after Shift+Enter.
pub fn line_widths(text: &str, px: f32) -> Vec<f32> {
    split_lines(text).into_iter().map(|line| line_width(line, px)).collect()
}

/// Size of the whole block: the widest line by (number of lines times line
/// height). Uses metrics only, nothing is rasterized, so the editor can call
/// it every frame to size the typing frame.
pub fn measure(text: &str, px: f32) -> (f32, f32) {
    let size = clamp_px(px);
    let per_line = line_widths(text, size);
    (widest(&per_line), per_line.len() as f32 * line_height(size))
}

/// The largest of `widths`, or 0 for none.
fn widest(widths: &[f32]) -> f32 {
    widths.iter().copied().fold(0.0f32, f32::max)
}

/// The lines of `text`, split at `\n`, with the one `\r` of a `\r\n` pair
/// removed. There is always at least one line, so even an empty string is
/// one line tall.
pub fn split_lines(text: &str) -> Vec<&str> {
    text.split('\n').map(|line| line.strip_suffix('\r').unwrap_or(line)).collect()
}

// Rectangles

/// An axis-aligned box in block coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TextRect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl TextRect {
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        TextRect { x, y, w, h }
    }

    pub fn right(self) -> f32 {
        self.x + self.w
    }

    pub fn bottom(self) -> f32 {
        self.y + self.h
    }

    /// True when the box has no area.
    pub fn is_empty(self) -> bool {
        self.w <= 0.0 || self.h <= 0.0
    }

    /// True when both boxes have area and some of it is shared. Boxes that
    /// only touch along an edge do not overlap.
    pub fn overlaps(self, other: TextRect) -> bool {
        if self.is_empty() || other.is_empty() {
            return false;
        }
        let across = self.x < other.right() && other.x < self.right();
        let down = self.y < other.bottom() && other.y < self.bottom();
        across && down
    }

    /// The shared part of the two boxes, or `None` when there is none.
    pub fn intersect(self, other: TextRect) -> Option<TextRect> {
        if !self.overlaps(other) {
            return None;
        }
        let (left, top) = (self.x.max(other.x), self.y.max(other.y));
        let (right, bottom) = (self.right().min(other.right()), self.bottom().min(other.bottom()));
        Some(TextRect::new(left, top, right - left, bottom - top))
    }
}

// Layout

/// One glyph with ink, rasterized and positioned in block coordinates.
#[derive(Debug, Clone)]
pub struct PlacedGlyph {
    pub ch: char,
    /// Which line (from 0) the glyph is on.
    pub line: usize,
    /// Top-left corner of the coverage bitmap.
    pub x: i32,
    pub y: i32,
    pub w: usize,
    pub h: usize,
    /// `w * h` coverage values, row by row: 0 is no ink, 255 is full ink.
    /// The same buffer lives in the glyph cache, so treat it as read-only.
    pub coverage: Arc<Vec<u8>>,
}

impl PlacedGlyph {
    /// The area the coverage bitmap covers.
    pub fn bounds(&self) -> TextRect {
        let (w, h) = (self.w as f32, self.h as f32);
        TextRect { x: self.x as f32, y: self.y as f32, w, h }
    }

    /// Coverage at column `col`, row `row` of the bitmap; 0 outside it.
    pub fn coverage_at(&self, col: usize, row: usize) -> u8 {
        if col < self.w && row < self.h {
            self.coverage[row * self.w + col]
        } else {
            0
        }
    }
}

/// Text that has been laid out and rasterized.
#[derive(Debug, Clone)]
pub struct TextBlock {
    pub px: f32,
    pub line_height: f32,
    pub ascent: f32,
    /// How far each line advances the pen. Empty lines are included.
    pub line_widths: Vec<f32>,
    /// One entry per glyph with ink. Spaces move the pen but get no entry.
    pub glyphs: Vec<PlacedGlyph>,
}

impl TextBlock {
    pub fn line_count(&self) -> usize {
        self.line_widths.len()
    }

    /// Width of the widest line.
    pub fn width(&self) -> f32 {
        widest(&self.line_widths)
    }

    pub fn height(&self) -> f32 {
        self.line_count() as f32 * self.line_height
    }

    /// The block's box by advance widths, which the editor draws the typing
    /// frame around. This is the metric box, not the ink: a glyph that
    /// overhangs (an italic-like tail, say) can stick out a pixel or two.
    pub fn bounds(&self) -> TextRect {
        TextRect { x: 0.0, y: 0.0, w: self.width(), h: self.height() }
    }

    /// Where the typing caret goes, as `(x, top)`: the end of the last line.
    /// The caret is `line_height` tall.
    pub fn caret(&self) -> (f32, f32) {
        let last_line = self.line_count().saturating_sub(1);
        let x = self.line_widths.last().copied().unwrap_or(0.0);
        (x, last_line as f32 * self.line_height)
    }

    /// The glyphs whose bitmaps reach into `clip`. This only saves work for
    /// text that is partly off screen; the painter still checks every pixel,
    /// so correctness does not depend on it.
    pub fn visible_glyphs(&self, clip: TextRect) -> impl Iterator<Item = &PlacedGlyph> {
        self.glyphs.iter().filter(move |glyph| glyph.bounds().overlaps(clip))
    }
}

/// Lay out `text` at em-size `px` (clamped) and rasterize every glyph.
pub fn layout(text: &str, px: f32) -> TextBlock {
    let size = clamp_px(px);
    let line_height = line_height(size);
    let ascent = ascent(size);
    let rows = split_lines(text);

    let mut line_widths = Vec::with_capacity(rows.len());
    let mut inked: Vec<PlacedGlyph> = Vec::new();
    for (line, content) in rows.into_iter().enumerate() {
        let baseline = ascent + line as f32 * line_height;
        let width = walk_line(content, size, |letter, pen| {
            let shape = rasterized(letter, size);
            let m = shape.metrics;
            if m.width > 0 && m.height > 0 {
                // fontdue's `ymin` is how far the bitmap's bottom edge sits
                // above the baseline (y up). Our y points down, so the top
                // edge is the baseline minus the bitmap's full rise.
                let rise = (m.ymin + m.height as i32) as f32;
                inked.push(PlacedGlyph {
                    ch: letter,
                    line,
                    x: (pen + m.xmin as f32).round() as i32,
                    y: (baseline - rise).round() as i32,
                    w: m.width,
                    h: m.height,
                    coverage: shape.coverage,
                });
            }
            m.advance_width
        });
        line_widths.push(width);
    }

    TextBlock { px: size, line_height, ascent, line_widths, glyphs: inked }
}

// Glyph cache

/// A rasterized glyph as fontdue returned it.
#[derive(Clone)]
struct Rasterized {
    metrics: Metrics,
    coverage: Arc<Vec<u8>>,
}

/// Cache key: the character and the exact bits of the em-size.
type GlyphKey = (char, u32);

/// Entries kept before the cache is emptied and starts over. Annotation
/// text is short and comes in a handful of sizes, so real use never gets
/// close; the limit only stops unbounded growth.
const CACHE_LIMIT: usize = 4096;

/// Every annotation is redrawn on every frame (up to 30 a second while the
/// tool panel animates), so without this cache a screenshot with a lot of
/// text would rasterize every glyph every frame.
fn glyph_cache() -> &'static Mutex<HashMap<GlyphKey, Rasterized>> {
    static CACHE: OnceLock<Mutex<HashMap<GlyphKey, Rasterized>>> = OnceLock::new();
    CACHE.get_or_init(Default::default)
}

/// `ch` at `px`, from the cache when possible.
fn rasterized(ch: char, px: f32) -> Rasterized {
    let render = || {
        let (metrics, alpha) = font().rasterize(ch, px);
        Rasterized { metrics, coverage: alpha.into() }
    };
    // If a panic elsewhere poisoned the lock, skip the cache rather than
    // take the editor down: rasterizing again is always possible.
    let Ok(mut cache) = glyph_cache().lock() else {
        return render();
    };
    let key: GlyphKey = (ch, px.to_bits());
    if let Some(hit) = cache.get(&key) {
        return hit.clone();
    }
    if cache.len() >= CACHE_LIMIT {
        cache.clear();
    }
    let fresh = render();
    cache.insert(key, fresh.clone());
    fresh
}

// Editing

/// Delete the last user-visible character of `s` and return it.
///
/// "Character" means grapheme cluster, not `char`. `String::pop` would turn
/// a decomposed "é" (`e` plus U+0301) into a bare `e`, and take a flag or a
/// ZWJ emoji apart one piece at a time. One Backspace should remove one
/// thing the user can see.
pub fn pop_grapheme(s: &mut String) -> Option<String> {
    let (start, _) = s.grapheme_indices(true).next_back()?;
    Some(s.split_off(start))
}

/// How many user-visible characters `s` holds, i.e. how many caret steps.
pub fn grapheme_count(s: &str) -> usize {
    UnicodeSegmentation::graphemes(s, true).count()
}

/// Clean text from a key press or an IME commit before it goes into an
/// annotation. Control characters are dropped, except line breaks: `\n`,
/// `\r` and `\r\n` all become one `\n`, so Shift+Enter starts a new line.
pub fn sanitize_input(raw: &str) -> String {
    let mut clean = String::with_capacity(raw.len());
    let mut after_cr = false;
    for typed in raw.chars() {
        // The `\n` of a `\r\n` pair was already written when the `\r` came.
        let second_half_of_crlf = after_cr && typed == '\n';
        after_cr = typed == '\r';
        let keep = match typed {
            _ if second_half_of_crlf => None,
            '\r' | '\n' => Some('\n'),
            other if other.is_control() => None,
            other => Some(other),
        };
        clean.extend(keep);
    }
    clean
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 0.01
    }

    #[test]
    fn the_built_in_font_loads() {
        // Fails only if the checked-in TTF is damaged.
        let metrics = font().horizontal_line_metrics(30.0);
        assert!(metrics.is_some_and(|lm| lm.ascent > 0.0));
        assert!(ascent(30.0) > 0.0 && ascent(30.0) < line_height(30.0));
    }

    #[test]
    fn wider_text_and_bigger_sizes_measure_wider() {
        let at = |s: &str, px: f32| line_width(s, px);
        assert!(at("Frame", 20.0) > at("Fram", 20.0));
        assert!(at("Frame", 40.0) > at("Frame", 20.0));
        assert_eq!(at("", 50.0), 0.0);
    }

    #[test]
    fn non_ascii_scripts_have_width_and_ink() {
        // Text like this used to come out as '?' boxes.
        let samples = ["naïve", "Straße", "Здравствуй", "Αθήνα", "¼ £ ¶ §"];
        for sample in samples {
            assert!(line_width(sample, 22.0) > 0.0, "{sample} has no width");
            assert!(!layout(sample, 22.0).glyphs.is_empty(), "{sample} has no glyphs");
        }
    }

    #[test]
    fn missing_characters_still_draw_a_box() {
        // Noto Sans has no CJK. Such characters must still take space and
        // draw the font's fallback box; dropping them would make typed text
        // vanish.
        let boxed = layout("\u{4e2d}", 30.0);
        assert_eq!((boxed.glyphs.len(), boxed.line_count()), (1, 1));
        assert!(boxed.width() > 0.0);
        assert!(boxed.glyphs[0].coverage.iter().any(|&ink| ink > 0), "the fallback box has no ink");
    }

    #[test]
    fn lines_split_on_newlines_with_or_without_cr() {
        assert_eq!(split_lines(""), vec![""]);
        assert_eq!(split_lines("a\nb"), vec!["a", "b"]);
        assert_eq!(split_lines("a\r\nb"), vec!["a", "b"]);
        // Trailing newline: an empty last line for the caret.
        assert_eq!(split_lines("a\n"), vec!["a", ""]);
        assert_eq!(split_lines("\n\n"), vec!["", "", ""]);
        // Only the CR directly before the LF goes; a lone CR stays text.
        assert_eq!(split_lines("a\r\r\nb\rc"), vec!["a\r", "b\rc"]);
    }

    #[test]
    fn measure_takes_the_widest_line_and_every_line_height() {
        let size = 26.0;
        let (wide, tall) = measure("tiny\na noticeably longer row", size);
        assert!(close(wide, line_width("a noticeably longer row", size)));
        assert!(close(tall, 2.0 * line_height(size)));

        // Empty text is still one line tall, so the typing frame has a
        // height as soon as the caret appears.
        let (empty_w, empty_h) = measure("", size);
        assert_eq!(empty_w, 0.0);
        assert!(close(empty_h, line_height(size)));
    }

    #[test]
    fn measure_and_layout_agree() {
        let size = 30.0;
        let sample = "Ärger\nслово\nthree";
        let (wide, tall) = measure(sample, size);
        let laid = layout(sample, size);
        assert!(close(laid.width(), wide) && close(laid.height(), tall));
        assert_eq!(laid.line_count(), 3);
        assert_eq!(laid.line_widths, line_widths(sample, size));
    }

    #[test]
    fn sizes_are_clamped_the_same_everywhere() {
        // A size below the minimum lays out exactly like the minimum.
        let tiny = layout("Wq", 1.0);
        assert_eq!(tiny.px, MIN_PX);
        assert_eq!(tiny.line_widths, layout("Wq", MIN_PX).line_widths);
        assert_eq!(line_height(1.0), line_height(MIN_PX));
        assert_eq!(measure("Wq", f32::NAN), measure("Wq", DEFAULT_PX));
    }

    #[test]
    fn each_line_sits_one_line_height_lower() {
        let stacked = layout("B\nB", 34.0);
        let on = |line: usize| -> Vec<&PlacedGlyph> { stacked.glyphs.iter().filter(|g| g.line == line).collect() };
        let (top, under) = (on(0), on(1));
        assert_eq!((top.len(), under.len()), (1, 1));
        // Same glyph, same column, one line height further down.
        assert_eq!(top[0].x, under[0].x);
        let dy = under[0].y - top[0].y;
        let lh = stacked.line_height;
        assert!((dy as f32 - lh).abs() <= 1.0, "lines {dy} apart, expected about {lh}");
    }

    #[test]
    fn capitals_hang_from_the_ascent() {
        // A capital's top is a little below the block's top and its bottom
        // is on the first baseline (within rounding).
        let cap_block = layout("H", 40.0);
        let cap = &cap_block.glyphs[0];
        assert!(cap.y >= 0, "cap top {} above the block", cap.y);
        assert!(((cap.y + cap.h as i32) as f32 - cap_block.ascent).abs() <= 2.0);
    }

    #[test]
    fn spaces_move_the_pen_without_ink() {
        let spaced = layout("x y", 26.0);
        assert!(spaced.glyphs.iter().all(|g| g.ch != ' '), "a space was rasterized");
        assert!(spaced.line_widths[0] > line_width("xy", 26.0));
        assert!(layout("   ", 26.0).glyphs.is_empty());
    }

    #[test]
    fn the_caret_follows_the_end_of_the_last_line() {
        let size = 26.0;
        let two_lines = layout("hi\nwider row", size);
        let (x, y) = two_lines.caret();
        assert!(close(x, line_width("wider row", size)));
        assert!(close(y, two_lines.line_height));

        // After a trailing newline it drops to the start of a new line.
        let (x, y) = layout("hi\n", size).caret();
        assert_eq!(x, 0.0);
        assert!(close(y, line_height(size)));

        // A block built by hand with no lines puts it at the origin.
        let bare = TextBlock { px: size, line_height: 30.0, ascent: 20.0, line_widths: vec![], glyphs: vec![] };
        assert_eq!(bare.caret(), (0.0, 0.0));
        assert!(bare.bounds().is_empty() && bare.bounds().right() == 0.0);
    }

    #[test]
    fn rectangles_overlap_only_with_shared_area() {
        let first = TextRect::new(2.0, 2.0, 10.0, 10.0);
        let second = TextRect::new(7.0, 7.0, 10.0, 10.0);
        assert_eq!(first.intersect(second), Some(TextRect::new(7.0, 7.0, 5.0, 5.0)));
        assert_eq!(second.intersect(first), first.intersect(second));
        assert_eq!((first.right(), first.bottom()), (12.0, 12.0));
        // Sharing an edge is not overlapping.
        assert!(!first.overlaps(TextRect::new(12.0, 2.0, 5.0, 5.0)));
        assert_eq!(first.intersect(TextRect::new(60.0, 60.0, 1.0, 1.0)), None);
        // A box with no area overlaps nothing, even inside another.
        assert!(!first.overlaps(TextRect::new(3.0, 3.0, 0.0, 5.0)));
        assert!(TextRect::new(0.0, 0.0, 3.0, -1.0).is_empty());
    }

    #[test]
    fn visible_glyphs_keeps_only_what_reaches_the_clip() {
        let block = layout("MMMMMMMMMM", 32.0);
        let total = block.glyphs.len();
        assert_eq!(total, 10);

        // A clip a little larger than the block keeps everything.
        let b = block.bounds();
        let roomy = TextRect::new(b.x - 8.0, b.y - 8.0, b.w + 16.0, b.h + 16.0);
        assert_eq!(block.visible_glyphs(roomy).count(), total);

        // The left quarter keeps some, not all.
        let left = TextRect::new(0.0, 0.0, block.width() / 4.0, block.height());
        let kept = block.visible_glyphs(left).count();
        assert!(kept > 0 && kept < total, "kept {kept} of {total}");

        // Off to the right keeps none.
        let beyond = TextRect::new(block.width() + 100.0, 0.0, 50.0, block.height());
        assert_eq!(block.visible_glyphs(beyond).count(), 0);
    }

    #[test]
    fn glyph_edges_are_smooth_and_reads_are_bounded() {
        let round = layout("O", 52.0);
        let o = &round.glyphs[0];
        assert_eq!(o.w * o.h, o.coverage.len());
        let soft = o.coverage.iter().filter(|&&c| c > 0 && c < 255).count();
        assert!(soft > 0, "a 52 px 'O' should be anti-aliased");
        // Inside reads match the buffer; outside reads are empty.
        assert_eq!(o.coverage_at(2, 3), o.coverage[3 * o.w + 2]);
        assert_eq!((o.coverage_at(o.w, 0), o.coverage_at(0, o.h)), (0, 0));
        let b = o.bounds();
        assert_eq!((b.x, b.y, b.w, b.h), (o.x as f32, o.y as f32, o.w as f32, o.h as f32));
    }

    #[test]
    fn thickness_steps_map_to_growing_text_sizes() {
        let sizes: Vec<f32> = [2.0, 4.0, 8.0].into_iter().map(font_size_for_thickness).collect();
        assert_eq!(sizes, vec![21.0, 28.0, 42.0]);
        assert_eq!(sizes[1], DEFAULT_PX, "the middle step is the default size");
        // Bad input stays inside the drawable range.
        for junk in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            assert_eq!(font_size_for_thickness(junk), DEFAULT_PX, "{junk}");
        }
        assert_eq!(font_size_for_thickness(-10.0), 14.0, "negative thickness counts as zero");
        assert_eq!(font_size_for_thickness(1.0e9), MAX_PX);
    }

    #[test]
    fn clamp_px_handles_junk() {
        assert_eq!(clamp_px(f32::NAN), DEFAULT_PX);
        assert_eq!(clamp_px(f32::INFINITY), DEFAULT_PX);
        assert_eq!(clamp_px(0.0), MIN_PX);
        assert_eq!(clamp_px(-5.0), MIN_PX);
        assert_eq!(clamp_px(1.0e6), MAX_PX);
        assert_eq!(clamp_px(33.5), 33.5);
    }

    /// Backspace once on `text`: what was removed, and what is left.
    fn backspace(text: &str) -> (Option<String>, String) {
        let mut buffer = text.to_owned();
        let removed = pop_grapheme(&mut buffer);
        (removed, buffer)
    }

    #[test]
    fn backspace_removes_one_visible_character() {
        // (typed, removed, left behind, visible characters before)
        let cases = [
            // Precomposed é.
            ("café", "é", "caf", 4),
            // Decomposed: the e and its combining accent leave together.
            ("cafe\u{0301}", "e\u{0301}", "caf", 4),
            // CR LF is one character.
            ("a\r\n", "\r\n", "a", 2),
        ];
        for (typed, gone, left, count) in cases {
            assert_eq!(grapheme_count(typed), count, "{typed:?}");
            assert_eq!(backspace(typed), (Some(gone.to_owned()), left.to_owned()), "{typed:?}");
        }
        // Nothing to remove is not an error.
        assert_eq!(backspace(""), (None, String::new()));
    }

    #[test]
    fn backspace_takes_whole_flags_and_emoji_sequences() {
        // Two regional indicators make one flag.
        let flag = "\u{1F1EE}\u{1F1F3}";
        assert_eq!(grapheme_count(&format!("hi{flag}")), 3);
        assert_eq!(backspace(&format!("hi{flag}")), (Some(flag.to_owned()), "hi".to_owned()));

        // A ZWJ sequence (woman, joiner, laptop) is one character on screen.
        let coder = "\u{1F469}\u{200D}\u{1F4BB}";
        assert_eq!(backspace(&format!("x{coder}")).1, "x");
    }

    #[test]
    fn input_keeps_text_and_line_breaks_only() {
        assert_eq!(sanitize_input("é"), "é");
        assert_eq!(sanitize_input("añ日"), "añ日");
        assert_eq!(sanitize_input("\u{8}"), "", "backspace");
        assert_eq!(sanitize_input("\u{1b}"), "", "escape");
        assert_eq!(sanitize_input("\t"), "", "tab");
        assert_eq!(sanitize_input(" "), " ");
        assert_eq!(sanitize_input("a\r\nb"), "a\nb");
        assert_eq!(sanitize_input("a\rb"), "a\nb");
        assert_eq!(sanitize_input("a\nb"), "a\nb");
        // Each CR is a break of its own unless an LF follows it directly.
        assert_eq!(sanitize_input("a\r\r\nb"), "a\n\nb");
        assert_eq!(sanitize_input("a\n\rb"), "a\n\nb");
        assert_eq!(sanitize_input("\r\n\r\n"), "\n\n");
        assert_eq!(sanitize_input("a\r\u{7}\nb"), "a\n\nb", "a dropped control still separates CR from LF");
    }

    #[test]
    fn cached_glyphs_match_fresh_ones() {
        // The second layout comes from the cache and must be identical.
        let first = layout("Caché ✓", 26.0);
        let second = layout("Caché ✓", 26.0);
        assert_eq!(first.glyphs.len(), second.glyphs.len());
        for (a, b) in first.glyphs.iter().zip(&second.glyphs) {
            assert_eq!((a.ch, a.line, a.x, a.y, a.w, a.h), (b.ch, b.line, b.x, b.y, b.w, b.h));
            assert_eq!(a.coverage, b.coverage);
        }
    }
}
