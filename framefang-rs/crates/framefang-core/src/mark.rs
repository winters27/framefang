//! Highlighting text in a picture (Brandon 2026-11-20): the picture's text
//! is read once, and the Highlight tool snaps to the words it found, so a
//! highlight sits exactly on the words, however the mouse wobbled.
//!
//! Picking works like selecting text in a document: press on a word, drag to
//! another, and every word between them in reading order is taken, line by
//! line. A highlight is one rectangle per line, each as tall as its line.

use serde::{Deserialize, Serialize};

use crate::annotation::{Point2, Rect};
use crate::color::Rgba;

/// How a highlight looks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum MarkStyle {
    /// A real chisel highlighter: slightly uneven edges, slanted ends,
    /// darker where the stroke starts and stops, faint streaks.
    #[default]
    Marker,
    /// An even rounded wash.
    Clean,
    /// A quick swipe across the lower half of the words.
    Lower,
    /// A hand-drawn marker line under the words.
    Underline,
    /// A loose hand-drawn loop around them.
    Circle,
    /// A bright wash with a soft glowing edge.
    Glow,
}

impl MarkStyle {
    /// In the order the tool's panel shows them.
    pub const ALL: [MarkStyle; 6] = [
        MarkStyle::Marker, MarkStyle::Clean, MarkStyle::Lower,
        MarkStyle::Underline, MarkStyle::Circle, MarkStyle::Glow,
    ];

    pub fn label(self) -> &'static str {
        match self {
            MarkStyle::Marker => "Marker",
            MarkStyle::Clean => "Clean",
            MarkStyle::Lower => "Swipe",
            MarkStyle::Underline => "Underline",
            MarkStyle::Circle => "Circle",
            MarkStyle::Glow => "Glow",
        }
    }

    /// Whether it lies over the words (darkening them as ink does) rather
    /// than drawing a line beside them.
    pub fn washes(self) -> bool { !matches!(self, MarkStyle::Underline | MarkStyle::Circle) }

    /// How far it reaches past its lines' rectangles.
    pub fn reach(self) -> f32 {
        match self {
            MarkStyle::Marker | MarkStyle::Clean | MarkStyle::Lower => 4.0,
            MarkStyle::Underline => 7.0,
            MarkStyle::Circle => 12.0,
            MarkStyle::Glow => 9.0,
        }
    }
}

/// A highlight's look: its style and color, and a seed so its hand-made
/// wobble is the same every time it is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct MarkLook {
    #[serde(default)]
    pub style: MarkStyle,
    pub color: Rgba,
    #[serde(default)]
    pub seed: u32,
}

impl Default for MarkLook {
    fn default() -> Self { MarkLook { style: MarkStyle::Marker, color: Rgba::new_opaque(255, 226, 52), seed: 0 } }
}

/// A word read in the picture: where it is, and which line it is on (lines
/// numbered in reading order).
#[derive(Debug, Clone, PartialEq)]
pub struct Word {
    pub rect: Rect,
    pub line: usize,
}

/// Words put in reading order, in rows found from where they are: a word
/// whose middle lies within a row's band joins it; rows run top to bottom,
/// words left to right; each word's `line` becomes its row. The engine's own
/// lines aren't trusted: Windows split each row of a code listing into
/// several and ordered them in columns, so a drag went down the page instead
/// of along it (2026-11-20).
pub fn in_reading_order(mut words: Vec<Word>) -> Vec<Word> {
    words.sort_by(|a, b| (a.rect.y + a.rect.h / 2.0).total_cmp(&(b.rect.y + b.rect.h / 2.0)));
    // Each row: its band, and its words.
    let mut rows: Vec<(f32, f32, Vec<Word>)> = Vec::new();
    for w in words {
        let mid = w.rect.y + w.rect.h / 2.0;
        match rows.last_mut() {
            Some((top, bottom, ws)) if mid >= *top && mid <= *bottom => {
                *top = top.min(w.rect.y);
                *bottom = bottom.max(w.rect.y + w.rect.h);
                ws.push(w);
            }
            _ => rows.push((w.rect.y, w.rect.y + w.rect.h, vec![w])),
        }
    }
    let mut out = Vec::new();
    for (i, (_, _, mut ws)) in rows.into_iter().enumerate() {
        ws.sort_by(|a, b| a.rect.x.total_cmp(&b.rect.x));
        for mut w in ws { w.line = i; out.push(w); }
    }
    out
}

/// How far from `r` the point `p` is (0 inside).
fn distance(r: &Rect, p: Point2) -> f32 {
    let dx = (r.x - p.x).max(p.x - (r.x + r.w)).max(0.0);
    let dy = (r.y - p.y).max(p.y - (r.y + r.h)).max(0.0);
    (dx * dx + dy * dy).sqrt()
}

/// The word under `p`, or the nearest one within `reach` (a press in the
/// gap between words still takes one). Distances across lines count double,
/// so a point between two lines takes a word on its own line.
pub fn word_at(words: &[Word], p: Point2, reach: f32) -> Option<usize> {
    let score = |w: &Word| {
        let r = &w.rect;
        let dx = (r.x - p.x).max(p.x - (r.x + r.w)).max(0.0);
        let dy = (r.y - p.y).max(p.y - (r.y + r.h)).max(0.0);
        dx + 2.0 * dy
    };
    words.iter().enumerate()
        .filter(|(_, w)| distance(&w.rect, p) <= reach)
        .min_by(|a, b| score(a.1).total_cmp(&score(b.1)))
        .map(|(i, _)| i)
}

/// The highlight for the words from `a` to `b` (either way round, both
/// taken) of `words` in reading order: one rectangle per line, from its
/// first word's left to its last word's right, as tall as the whole line.
pub fn span(words: &[Word], a: usize, b: usize) -> Vec<Rect> {
    if words.is_empty() { return Vec::new(); }
    let (a, b) = (a.min(b), a.max(b).min(words.len() - 1));
    let mut out: Vec<(usize, Rect)> = Vec::new();
    for w in &words[a..=b] {
        match out.last_mut() {
            Some((line, r)) if *line == w.line => {
                let x1 = (r.x + r.w).max(w.rect.x + w.rect.w);
                r.x = r.x.min(w.rect.x);
                r.w = x1 - r.x;
            }
            _ => out.push((w.line, Rect { x: w.rect.x, y: 0.0, w: w.rect.w, h: 0.0 })),
        }
    }
    // Each line's full height, over all its words (taken or not), so every
    // highlight on a line is the same height.
    for (line, r) in &mut out {
        let (mut y0, mut y1) = (f32::MAX, f32::MIN);
        for w in words.iter().filter(|w| w.line == *line) {
            y0 = y0.min(w.rect.y);
            y1 = y1.max(w.rect.y + w.rect.h);
        }
        r.y = y0;
        r.h = y1 - y0;
    }
    out.into_iter().map(|(_, r)| r).collect()
}

/// What a highlight covers, its reach included.
pub fn bounds(rects: &[Rect], style: MarkStyle) -> Rect {
    let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for r in rects {
        x0 = x0.min(r.x); y0 = y0.min(r.y);
        x1 = x1.max(r.x + r.w); y1 = y1.max(r.y + r.h);
    }
    if rects.is_empty() { return Rect { x: 0.0, y: 0.0, w: 0.0, h: 0.0 }; }
    let m = style.reach();
    Rect { x: x0 - m, y: y0 - m, w: x1 - x0 + 2.0 * m, h: y1 - y0 + 2.0 * m }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two lines of three words each, then a line of one.
    fn page() -> Vec<Word> {
        let w = |x: f32, y: f32, ww: f32, line: usize| Word { rect: Rect { x, y, w: ww, h: 14.0 }, line };
        in_reading_order(vec![
            w(70.0, 10.0, 40.0, 0), w(10.0, 10.0, 50.0, 0), w(120.0, 11.0, 30.0, 0),
            w(10.0, 40.0, 30.0, 1), w(50.0, 40.0, 60.0, 1), w(120.0, 39.0, 45.0, 1),
            w(10.0, 70.0, 80.0, 2),
        ])
    }

    #[test]
    fn words_are_read_by_line_then_left_to_right() {
        let p = page();
        assert_eq!(p[0].rect.x, 10.0);
        assert_eq!(p[2].rect.x, 120.0);
        assert_eq!(p[3].line, 1);
    }

    #[test]
    fn rows_come_from_where_words_are_not_the_engines_lines() {
        // A code listing as Windows read it: every word its own "line",
        // numbered down the page column by column.
        let w = |x: f32, y: f32, line: usize| Word { rect: Rect { x, y, w: 30.0, h: 12.0 }, line };
        let mut words = Vec::new();
        for col in 0..3 {
            for row in 0..4 {
                words.push(w(10.0 + col as f32 * 40.0, 10.0 + row as f32 * 18.0 + (col as f32 * 0.7), col * 4 + row));
            }
        }
        let p = in_reading_order(words);
        // Row by row, left to right.
        for (i, word) in p.iter().enumerate() {
            assert_eq!(word.line, i / 3, "word {i}");
            assert_eq!(word.rect.x, 10.0 + (i % 3) as f32 * 40.0);
        }
        // So a drag from the first word to the fifth takes row 0 whole and
        // the start of row 1: one rectangle a row.
        let r = span(&p, 0, 4);
        assert_eq!(r.len(), 2);
        assert_eq!((r[0].x, r[0].x + r[0].w), (10.0, 120.0));
        assert_eq!((r[1].x, r[1].x + r[1].w), (10.0, 80.0));
    }

    #[test]
    fn a_press_takes_the_word_under_it_or_the_nearest_on_its_line() {
        let p = page();
        assert_eq!(word_at(&p, Point2::new(80.0, 15.0), 12.0), Some(1), "on it");
        assert!(matches!(word_at(&p, Point2::new(64.0, 17.0), 12.0), Some(0) | Some(1)), "in the gap: a neighbour");
        assert_eq!(word_at(&p, Point2::new(400.0, 15.0), 12.0), None, "nothing near");
        // Between lines but nearer the first: a word on the first line.
        assert_eq!(p[word_at(&p, Point2::new(30.0, 27.0), 12.0).unwrap()].line, 0);
    }

    #[test]
    fn a_drag_takes_every_word_between_line_by_line_whichever_way() {
        let p = page();
        // From the second word of line 0 to the second word of line 1.
        let r = span(&p, 1, 4);
        assert_eq!(r.len(), 2);
        assert_eq!((r[0].x, r[0].x + r[0].w), (70.0, 150.0), "line 0 from its second word to its end");
        assert_eq!((r[1].x, r[1].x + r[1].w), (10.0, 110.0), "line 1 from its start to its second word");
        // Each as tall as its line, all words counted.
        assert_eq!((r[0].y, r[0].h), (10.0, 15.0));
        assert_eq!((r[1].y, r[1].h), (39.0, 15.0));
        assert_eq!(span(&p, 4, 1), r, "dragging back up takes the same words");
        assert_eq!(span(&p, 6, 6).len(), 1, "a click: one word");
        assert!(span(&[], 0, 3).is_empty());
    }

    #[test]
    fn its_bounds_reach_as_far_as_its_look_does() {
        let r = span(&page(), 0, 2);
        let tight = bounds(&r, MarkStyle::Clean);
        let loose = bounds(&r, MarkStyle::Circle);
        assert!(loose.w > tight.w && loose.h > tight.h);
        assert!(MarkStyle::Marker.washes() && !MarkStyle::Underline.washes());
    }
}
