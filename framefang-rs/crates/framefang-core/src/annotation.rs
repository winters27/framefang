//! The marks a user draws on a capture, as plain data.
//!
//! Nothing here draws anything: the app's painter turns these into pixels,
//! and [`crate::edit`] does the click and move geometry. A new kind of mark
//! means a new [`AnnotationKind`] variant, a new [`crate::tool::Tool`] (and
//! its place in `Tool::ALL`), and a case in the app's painter.
//!
//! The serde forms of these types (field names, variant names, the shape of
//! each variant) are a data format, so they stay as they are.

use serde::{Deserialize, Serialize};

use crate::color::Rgba;

/// A point on the canvas, in canvas pixels.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Point2 {
    pub x: f32,
    pub y: f32,
}

impl Point2 {
    pub const fn new(x: f32, y: f32) -> Self { Point2 { x, y } }
}

/// An upright rectangle: its top left corner and its size.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    /// The rectangle with `a` and `b` as opposite corners, whichever way
    /// round they were given.
    pub fn from_corners(a: Point2, b: Point2) -> Self {
        Rect {
            x: a.x.min(b.x),
            y: a.y.min(b.y),
            w: (b.x - a.x).abs(),
            h: (b.y - a.y).abs(),
        }
    }

    /// Whether `p` is inside, edges included.
    pub fn contains(self, p: Point2) -> bool {
        let inside_x = p.x >= self.x && p.x <= self.x + self.w;
        let inside_y = p.y >= self.y && p.y <= self.y + self.h;
        inside_x && inside_y
    }

    /// Whether it has no area: a side of zero or less.
    pub fn is_empty(self) -> bool {
        self.w.min(self.h) <= 0.0
    }
}

/// The colour and width of a drawn line, shared by most kinds of mark.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Stroke {
    pub color: Rgba,
    pub thickness: f32,
}

impl Default for Stroke {
    /// Red, 3 px wide.
    fn default() -> Self {
        Self { thickness: 3.0, color: Rgba::RED }
    }
}

/// What a mark is, with the points and style it needs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum AnnotationKind {
    /// A freehand line through every point the mouse passed.
    Pen { stroke: Stroke, points: Vec<Point2> },
    /// A straight line.
    Line { stroke: Stroke, start: Point2, end: Point2 },
    /// A straight line with a head at `end`.
    Arrow { stroke: Stroke, start: Point2, end: Point2 },
    /// A hollow box with `start` and `end` as corners.
    Rectangle { stroke: Stroke, start: Point2, end: Point2 },
    /// A hollow ellipse filling the box from `start` to `end`.
    Ellipse { stroke: Stroke, start: Point2, end: Point2 },
    /// A freehand highlighter stroke, wide and see-through.
    Marker { stroke: Stroke, points: Vec<Point2> },
    /// Words typed onto the picture, `font_size` being a pixel em-size.
    Text { color: Rgba, position: Point2, text: String, font_size: f32 },
    /// Highlighted words: one box per line of text (see [`crate::mark`]).
    TextMark { rects: Vec<Rect>, look: crate::mark::MarkLook },
    /// A numbered step, placed by its anchor (see [`crate::step`]).
    Step { center: Point2, number: u32, look: crate::step::StepLook },
    /// A box whose picture is blurred into blocks of `block_size` pixels.
    Pixelate { start: Point2, end: Point2, block_size: u32 },
    /// Dims the picture outside this box (and outside any other spotlight);
    /// other marks stay at full brightness.
    Spotlight { start: Point2, end: Point2 },
    /// A round lens centred on `center` and reaching to `edge`, showing what
    /// lies under it `zoom` times larger inside a ring drawn with `stroke`.
    Magnify { stroke: Stroke, center: Point2, edge: Point2, zoom: u32 },
}

/// The biggest radius a Magnify lens gets. A larger lens costs time on
/// every frame and shows hardly anything more.
pub const MAGNIFY_MAX_RADIUS: f32 = 400.0;

/// A Magnify lens's radius: the distance from `center` to `edge`, held to
/// [`MAGNIFY_MAX_RADIUS`].
pub fn magnify_radius(center: Point2, edge: Point2) -> f32 {
    let (dx, dy) = (edge.x - center.x, edge.y - center.y);
    (dx.powi(2) + dy.powi(2)).sqrt().min(MAGNIFY_MAX_RADIUS)
}

/// One mark on the canvas.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Annotation {
    pub kind: AnnotationKind,
    /// When a mark shows in a video, in seconds of the clip: from the first
    /// number (included) up to the second (not included), which may be
    /// `f32::INFINITY` for "to the end". `None` means the whole clip, which
    /// is what screenshots and untouched video marks have, so they export
    /// exactly as they would without a timeline. The serde default lets
    /// data saved without this field load (no saved form of a mark exists
    /// today, but one may).
    #[serde(default)]
    pub time: Option<(f32, f32)>,
}

impl AnnotationKind {
    /// This kind as a mark shown for the whole clip.
    fn into_mark(self) -> Annotation {
        Annotation { kind: self, time: None }
    }
}

impl Annotation {
    /// A freehand pen line, starting as the single point `p`.
    pub fn pen(stroke: Stroke, p: Point2) -> Annotation {
        AnnotationKind::Pen { stroke, points: vec![p] }.into_mark()
    }

    /// A line that begins and ends at `a` until it is dragged.
    pub fn line(stroke: Stroke, a: Point2) -> Annotation {
        AnnotationKind::Line { stroke, start: a, end: a }.into_mark()
    }

    /// An arrow that begins and ends at `a` until it is dragged.
    pub fn arrow(stroke: Stroke, a: Point2) -> Annotation {
        AnnotationKind::Arrow { stroke, start: a, end: a }.into_mark()
    }

    /// A box with both corners at `a` until it is dragged.
    pub fn rectangle(stroke: Stroke, a: Point2) -> Annotation {
        AnnotationKind::Rectangle { stroke, start: a, end: a }.into_mark()
    }

    /// An ellipse with both box corners at `a` until it is dragged.
    pub fn ellipse(stroke: Stroke, a: Point2) -> Annotation {
        AnnotationKind::Ellipse { stroke, start: a, end: a }.into_mark()
    }

    /// A highlighter stroke starting at `p`. It is drawn six times as wide
    /// as `stroke` asks, and its opacity is `alpha` whatever the colour's
    /// own: the editor passes the Marker opacity setting
    /// (`AppSettings::marker_opacity`) as it stands, so the slider's preview
    /// and the finished stroke always agree.
    pub fn marker(stroke: Stroke, p: Point2, alpha: u8) -> Annotation {
        let wide = Stroke { color: stroke.color.with_alpha(alpha), thickness: 6.0 * stroke.thickness };
        AnnotationKind::Marker { stroke: wide, points: vec![p] }.into_mark()
    }

    /// Text at the standard size. Sizes here are pixel em-sizes passed
    /// straight to `crate::text`, not points.
    pub fn text(color: Rgba, p: Point2, text: impl Into<String>) -> Annotation {
        Self::text_sized(color, p, text, crate::text::DEFAULT_PX)
    }

    /// Text at `font_size` pixels, brought into the range the text layout
    /// accepts (the editor picks the size from the stroke width with
    /// `crate::text::font_size_for_thickness`).
    pub fn text_sized(color: Rgba, p: Point2, text: impl Into<String>, font_size: f32) -> Annotation {
        AnnotationKind::Text {
            color,
            position: p,
            text: text.into(),
            font_size: crate::text::clamp_px(font_size),
        }
        .into_mark()
    }

    /// A step drawn as a plain disc of `color`.
    pub fn step(color: Rgba, center: Point2, number: u32) -> Annotation {
        let disc = crate::step::StepLook::recipe(crate::step::StepShape::Disc, color);
        Self::step_styled(center, number, disc)
    }

    /// A step with a chosen look.
    pub fn step_styled(center: Point2, number: u32, look: crate::step::StepLook) -> Annotation {
        AnnotationKind::Step { center, number, look }.into_mark()
    }

    /// A blur box starting at `a`, with 10 px blocks.
    pub fn pixelate(a: Point2) -> Annotation {
        AnnotationKind::Pixelate { start: a, end: a, block_size: 10 }.into_mark()
    }

    /// A spotlight box starting at `a`.
    pub fn spotlight(a: Point2) -> Annotation {
        AnnotationKind::Spotlight { start: a, end: a }.into_mark()
    }

    /// A lens at `center` with no size yet, enlarging by `zoom` held to 2
    /// through 4.
    pub fn magnify(stroke: Stroke, center: Point2, zoom: u32) -> Annotation {
        AnnotationKind::Magnify { stroke, center, edge: center, zoom: zoom.clamp(2, 4) }.into_mark()
    }

    /// Whether the mark is too small to be on purpose: a click, rather than
    /// a drag, with Spotlight or Magnify. Such a mark is thrown away when
    /// the mouse is let go and never reaches the undo history.
    pub fn is_degenerate(&self) -> bool {
        match &self.kind {
            AnnotationKind::Spotlight { start, end } => {
                let r = Rect::from_corners(*start, *end);
                r.w < 4.0 || r.h < 4.0
            }
            AnnotationKind::Magnify { center, edge, .. } => magnify_radius(*center, *edge) < 8.0,
            _ => false,
        }
    }

    /// Follow the mouse to `p` while the mark is being drawn: freehand marks
    /// gain a point, dragged shapes move their far corner, and a lens grows
    /// to reach it. Text and steps are placed by a click and ignore this,
    /// as does a highlight, whose words the editor fills in itself.
    pub fn extend(&mut self, p: Point2) {
        use AnnotationKind as K;
        match &mut self.kind {
            K::Pen { points, .. } | K::Marker { points, .. } => points.push(p),
            K::Line { end, .. }
            | K::Arrow { end, .. }
            | K::Rectangle { end, .. }
            | K::Ellipse { end, .. }
            | K::Pixelate { end, .. }
            | K::Spotlight { end, .. }
            | K::Magnify { edge: end, .. } => *end = p,
            K::Text { .. } | K::Step { .. } | K::TextMark { .. } => {}
        }
    }
}

/// A named set of 16 swatches for the toolbar.
#[derive(Debug, Clone)]
pub struct ColorPalette {
    pub name: &'static str,
    pub colors: [Rgba; 16],
}

/// An opaque colour from a `0xRRGGBB` number.
const fn rgb(hex: u32) -> Rgba {
    Rgba::new_opaque((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

/// A highlighter swatch from a `0xRRGGBB` number: two thirds opaque.
const fn tint(hex: u32) -> Rgba {
    Rgba::new((hex >> 16) as u8, (hex >> 8) as u8, hex as u8, 170)
}

/// The palettes the toolbar offers.
pub struct Palettes;

impl Palettes {
    pub const ALL: [ColorPalette; 4] = [
        ColorPalette {
            name: "Vivid",
            colors: [
                rgb(0xDC2626), rgb(0xFF6400), rgb(0xFFB400), rgb(0xFFE600),
                rgb(0x82DC00), rgb(0x00B450), rgb(0x00C8C8), rgb(0x00B4F0),
                rgb(0x0064FF), rgb(0x5050DC), rgb(0xA03CF0), rgb(0xF03CDC),
                rgb(0xFF64C8), rgb(0xFF5050), rgb(0xFFFFFF), rgb(0x000000),
            ],
        },
        ColorPalette {
            name: "Highlighter",
            colors: [
                tint(0xFFEB00), tint(0x64FF64), tint(0xFF64C8), tint(0x64E6FF),
                tint(0xFF9600), tint(0xC864FF), tint(0x32DC32), tint(0x6496FF),
                tint(0xFF5050), tint(0xF03CDC), tint(0x00C8C8), tint(0xFFB464),
                tint(0xB4FF64), tint(0x6464FF), tint(0xE6E6E6), tint(0x505050),
            ],
        },
        ColorPalette {
            name: "Pastel",
            colors: [
                rgb(0xFFC8C8), rgb(0xFFDCB4), rgb(0xFFEBB4), rgb(0xFFF5C8),
                rgb(0xE6FFC8), rgb(0xC8FFDC), rgb(0xC8F5F5), rgb(0xC8E6FF),
                rgb(0xC8D7FF), rgb(0xDCC8FF), rgb(0xF0C8FF), rgb(0xFFC8EB),
                rgb(0xFFD7D7), rgb(0xF0F0F0), rgb(0xC8C8D2), rgb(0x64646E),
            ],
        },
        ColorPalette {
            name: "Pro",
            colors: [
                rgb(0xDC2626), rgb(0x1E64DC), rgb(0x1EA03C), rgb(0xF07800),
                rgb(0x8A2BE2), rgb(0xDCB400), rgb(0x00A0C8), rgb(0xC83C82),
                rgb(0x641E1E), rgb(0x1E1E64), rgb(0x1E501E), rgb(0x643C00),
                rgb(0x000000), rgb(0x505050), rgb(0xC8C8C8), rgb(0xFFFFFF),
            ],
        },
    ];

    /// The palette at `index`, or the last one when `index` is past the end.
    pub fn get(index: usize) -> &'static ColorPalette {
        let last = Self::ALL.len() - 1;
        &Self::ALL[index.min(last)]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ORIGIN: Point2 = Point2::new(0.0, 0.0);

    #[test]
    fn corners_given_either_way_round_make_the_same_rect() {
        let (a, b) = (Point2::new(50.0, 80.0), Point2::new(10.0, 20.0));
        let expected = Rect { x: 10.0, y: 20.0, w: 40.0, h: 60.0 };
        assert_eq!(Rect::from_corners(a, b), expected);
        assert_eq!(Rect::from_corners(b, a), expected);
        assert!(expected.contains(Point2::new(50.0, 80.0)), "edges count as inside");
        assert!(!expected.contains(Point2::new(50.1, 80.0)));
        assert!(Rect { x: 0.0, y: 0.0, w: 5.0, h: 0.0 }.is_empty());
    }

    #[test]
    fn a_pen_line_gains_a_point_per_mouse_move() {
        let mut pen = Annotation::pen(Stroke::default(), Point2::new(1.0, 2.0));
        pen.extend(Point2::new(3.0, 4.0));
        pen.extend(Point2::new(5.0, 6.0));
        let AnnotationKind::Pen { points, .. } = &pen.kind else { panic!("not a pen line") };
        assert_eq!(points, &[Point2::new(1.0, 2.0), Point2::new(3.0, 4.0), Point2::new(5.0, 6.0)]);
    }

    #[test]
    fn dragged_shapes_move_only_their_far_end() {
        let mut arrow = Annotation::arrow(Stroke::default(), ORIGIN);
        arrow.extend(Point2::new(9.0, 9.0));
        arrow.extend(Point2::new(20.0, 5.0));
        let AnnotationKind::Arrow { start, end, .. } = arrow.kind else { panic!("not an arrow") };
        assert_eq!((start, end), (ORIGIN, Point2::new(20.0, 5.0)));
        let mut step = Annotation::step(Rgba::RED, ORIGIN, 1);
        let before = step.clone();
        step.extend(Point2::new(50.0, 50.0));
        assert_eq!(step, before, "a step stays where it was clicked");
    }

    fn font_size_of(text: Annotation) -> f32 {
        match text.kind {
            AnnotationKind::Text { font_size, .. } => font_size,
            other => panic!("not text: {other:?}"),
        }
    }

    #[test]
    fn text_sizes_are_kept_in_the_range_the_layout_accepts() {
        // The size is a pixel em-size and always one the layout can draw.
        assert_eq!(font_size_of(Annotation::text(Rgba::RED, ORIGIN, "hi")), crate::text::DEFAULT_PX);
        assert_eq!(font_size_of(Annotation::text_sized(Rgba::RED, ORIGIN, "hi", 1.0e9)), crate::text::MAX_PX);
        assert_eq!(font_size_of(Annotation::text_sized(Rgba::RED, ORIGIN, "hi", f32::NAN)), crate::text::DEFAULT_PX);
    }

    /// Order-sensitive fingerprint of a palette: changes if any swatch, or
    /// the order of the swatches, changes.
    fn fingerprint(palette: &ColorPalette) -> (u64, u64) {
        palette.colors.iter().fold((0, 0), |(sum, hash), c| {
            let (r, g, b, a) = (c.r as u64, c.g as u64, c.b as u64, c.a as u64);
            (sum + r + g + b + a, (hash * 31 + r * 65536 + g * 256 + b + a * 7) % 1_000_000_007)
        })
    }

    #[test]
    fn the_palettes_hold_the_same_sixteen_swatches_as_ever() {
        let names: Vec<_> = Palettes::ALL.iter().map(|p| p.name).collect();
        assert_eq!(names, ["Vivid", "Highlighter", "Pastel", "Pro"]);
        let expected = [(10531, 274714184), (10380, 662982351), (14550, 920443298), (9118, 356128239)];
        for (palette, want) in Palettes::ALL.iter().zip(expected) {
            assert_eq!(palette.colors.len(), 16);
            assert_eq!(fingerprint(palette), want, "{} changed", palette.name);
        }
        let first = Palettes::ALL[0].colors[0];
        assert_eq!((first.r, first.g, first.b, first.a), (220, 38, 38, 255));
        let glow = Palettes::ALL[1].colors[0];
        assert_eq!((glow.r, glow.g, glow.b, glow.a), (255, 235, 0, 170));
        assert_eq!(Palettes::get(99).name, "Pro", "past the end gives the last");
    }

    #[test]
    fn a_marker_takes_its_opacity_from_the_setting_not_the_swatch() {
        // Whatever alpha the swatch had is replaced; the colour itself stays,
        // so the slider preview matches what gets painted.
        let solid_red = Stroke { color: Rgba::new(255, 0, 0, 0xFF), thickness: 4.0 };
        let mark = Annotation::marker(solid_red, Point2::new(10.0, 20.0), 0x80);
        let AnnotationKind::Marker { stroke, points } = mark.kind else { panic!("not a marker") };
        assert_eq!((stroke.color.r, stroke.color.g, stroke.color.b, stroke.color.a), (255, 0, 0, 0x80));
        assert_eq!(stroke.thickness, 24.0, "six times the 4 px asked for");
        assert_eq!(points, vec![Point2::new(10.0, 20.0)]);
    }

    #[test]
    fn every_marker_opacity_from_clear_to_solid_is_kept() {
        for alpha in [0u8, 1, 0x40, 0x7F, 0xC8, 0xFE, 0xFF] {
            let AnnotationKind::Marker { stroke, .. } = Annotation::marker(Stroke::default(), ORIGIN, alpha).kind
            else { panic!("not a marker") };
            assert_eq!(stroke.color.a, alpha);
        }
    }

    #[test]
    fn new_marks_show_for_the_whole_clip() {
        // Only the editor's video mode gives a mark a time window.
        let at = Point2::new(1.0, 2.0);
        let fresh = [
            Annotation::pen(Stroke::default(), at),
            Annotation::marker(Stroke::default(), at, 0x80),
            Annotation::text(Rgba::RED, at, "hi"),
            Annotation::step(Rgba::RED, at, 1),
            Annotation::pixelate(at),
            Annotation::spotlight(at),
            Annotation::magnify(Stroke::default(), at, 3),
        ];
        assert!(fresh.iter().all(|a| a.time.is_none()));
    }

    #[test]
    fn drawing_further_keeps_the_time_window() {
        let mut pen = Annotation::pen(Stroke::default(), ORIGIN);
        pen.time = Some((1.5, 4.5));
        pen.extend(Point2::new(3.0, 3.0));
        assert_eq!(pen.time, Some((1.5, 4.5)));
    }

    #[test]
    fn a_mark_without_a_time_field_loads_as_whole_clip() {
        let pen = Annotation::pen(Stroke::default(), Point2::new(1.0, 2.0));
        let mut json = serde_json::to_value(&pen).unwrap();
        json.as_object_mut().unwrap().remove("time");
        let back: Annotation = serde_json::from_value(json).unwrap();
        assert_eq!(back, pen);
        let text = serde_json::to_string(&pen.kind).unwrap();
        assert!(text.starts_with("{\"Pen\":{\"stroke\":"), "{text}");
    }
}
