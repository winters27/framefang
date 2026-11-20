//! A tool's flyout: its settings, slid out beside its button in the tool
//! column. From the top: Step's looks or Highlight's looks when the tool
//! has them, a slider row per setting (a muted label, a track, the value),
//! then the colors: the palette's name between two arrows, its 16 swatches
//! and, under them, the eyedropper over the current color beside a
//! spectrum. One layout feeds both drawing and hit-testing, so they always
//! agree. Also here: the saved style each tool opens with, and the color
//! math the spectrum and the loupe use.

use framefang_core::annotation::{Palettes, Stroke};
use framefang_core::color::Rgba as Color;
use framefang_core::settings::AppSettings;
use framefang_core::step::{StepLook, StepShape};
use framefang_core::tool::Tool;

use super::geometry::Rect4;
use super::panels::ToolSet;

/// The flyout's width.
pub(super) const FLYOUT_W: i32 = 214;
/// Its inner margin.
pub(super) const FLYOUT_PAD: i32 = 8;
/// Between the tool column and the flyout.
pub(super) const FLYOUT_GAP: i32 = 6;
/// How long the slide-out takes.
pub(super) const FLYOUT_OPEN: std::time::Duration = std::time::Duration::from_millis(170);
/// The flyout's type size, in px; the hover label and the loupe use it too.
pub(super) const FLYOUT_TEXT_PX: f32 = 13.0;
/// The little pointer across the gap to the tool's button.
pub(super) const FLYOUT_POINTER: i32 = 5;
/// A slider's grip.
pub(super) const KNOB_W: i32 = 10;

const ROW_H: i32 = 30;
const ROW_GAP: i32 = 4;
/// A slider row: the label, then the track, then the readout.
const LABEL_W: i32 = 48;
const VALUE_W: i32 = 40;
const TRACK_H: i32 = 14;
/// Between the flyout's sections.
const SECTION_GAP: i32 = 10;
const SWATCH: i32 = 21;
const SWATCH_GAP: i32 = 4;
const SWATCH_COLS: i32 = 8;
const PALETTE_ROW_H: i32 = 22;
/// Step's shape tiles, Highlight's look tiles, and the chips under them.
const TILE_GAP: i32 = 4;
const STEP_TILE_H: i32 = 52;
const LOOK_TILE_H: i32 = 44;
const CHIP_H: i32 = 24;
/// Share of the spectrum's width given to the hues; the strip after them
/// runs through the grays.
pub(super) const SPECTRUM_HUES: f32 = 0.88;

/// One setting a tool's flyout offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Slider { Width, Opacity, TextSize, BlockSize, Zoom }

impl Slider {
    /// What `t` lets you set, in the order its flyout lists them.
    pub fn for_tool(t: Tool) -> &'static [Slider] {
        match t {
            Tool::Pen | Tool::Line | Tool::Arrow | Tool::Rectangle | Tool::Ellipse => &[Slider::Width],
            Tool::Marker => &[Slider::Width, Slider::Opacity],
            Tool::Text => &[Slider::TextSize],
            Tool::Pixelate => &[Slider::BlockSize],
            // The lens's ring, and how much it enlarges.
            Tool::Magnify => &[Slider::Width, Slider::Zoom],
            Tool::Step | Tool::Spotlight | Tool::Crop | Tool::Highlight => &[],
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Slider::Width => "Size",
            Slider::Opacity => "Opacity",
            Slider::TextSize => "Text",
            Slider::BlockSize => "Blocks",
            Slider::Zoom => "Zoom",
        }
    }

    /// The whole-number range the slider covers.
    pub fn range(self) -> (f32, f32) {
        match self {
            Slider::Width => (1.0, 24.0),
            // Below about a tenth a marker can't be seen at all.
            Slider::Opacity => (26.0, 255.0),
            Slider::TextSize => (12.0, 96.0),
            Slider::BlockSize => (4.0, 40.0),
            Slider::Zoom => (2.0, 4.0),
        }
    }

    /// What a session opens at when the saved value is missing or broken.
    pub fn fallback(self) -> f32 {
        match self {
            Slider::Width => 4.0,
            Slider::Opacity => 200.0,
            Slider::TextSize => 28.0,
            Slider::BlockSize => 10.0,
            Slider::Zoom => 2.0,
        }
    }

    /// A saved value made usable: rounded when it is in range, else the
    /// fallback (NaN included).
    pub fn usable(self, v: f32) -> f32 {
        let (lo, hi) = self.range();
        if (lo..=hi).contains(&v) { v.round() } else { self.fallback() }
    }

    /// The value as its row shows it.
    pub fn readout(self, v: f32) -> String {
        let whole = v.round() as i32;
        match self {
            Slider::Opacity => format!("{}%", (v * 100.0 / 255.0).round() as i32),
            Slider::Zoom => format!("{whole}x"),
            Slider::Width | Slider::TextSize | Slider::BlockSize => format!("{whole} px"),
        }
    }
}

/// The draggable track of a slider row, between its label and its readout.
pub(super) fn slider_track((x, y, w, h): Rect4) -> Rect4 {
    (x + LABEL_W, y + (h - TRACK_H) / 2, w - LABEL_W - VALUE_W, TRACK_H)
}

/// The color section: the palette row, its swatches, and the any-color
/// row under them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ColorBlock {
    pub prev: Rect4,
    pub next: Rect4,
    /// The palette's name, between the arrows.
    pub name: Rect4,
    pub swatches: [Rect4; 16],
    pub eyedropper: Rect4,
    /// The tool's color as it is, under the eyedropper.
    pub current: Rect4,
    pub spectrum: Rect4,
}

/// The top of Step's flyout: a tile per shape, a chip per part (Body,
/// Ring, Number: the colors below paint the picked one) and the shadow
/// switch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct StepPanel {
    pub tiles: [Rect4; 9],
    pub parts: [Rect4; 3],
    pub shadow: Rect4,
}

/// Where everything in a tool's flyout sits, relative to its top left.
pub(super) struct FlyoutLayout {
    pub w: i32,
    pub h: i32,
    pub sliders: Vec<(Slider, Rect4)>,
    pub colors: Option<ColorBlock>,
    pub step: Option<StepPanel>,
    /// Highlight only: a tile per look.
    pub looks: Option<[Rect4; 6]>,
}

/// Whether a tool draws in a color at all: Pixelate shows the picture
/// itself, Spotlight only dims, and Crop draws nothing.
pub(super) fn has_colors(t: Tool) -> bool { !matches!(t, Tool::Pixelate | Tool::Spotlight | Tool::Crop) }

/// Whether `t` has a flyout: one with nothing to set has none.
pub(super) fn has_flyout(t: Tool) -> bool { has_colors(t) || !Slider::for_tool(t).is_empty() }

/// Three tiles across the flyout's inside, `N / 3` rows of them from `y`
/// down; `y` moves past them.
fn tile_grid<const N: usize>(y: &mut i32, tile_h: i32) -> [Rect4; N] {
    let tile_w = tile_w();
    let top = *y;
    let rows = N as i32 / 3;
    *y += rows * tile_h + (rows - 1) * TILE_GAP;
    std::array::from_fn(|i| {
        let (col, row) = (i as i32 % 3, i as i32 / 3);
        (FLYOUT_PAD + col * (tile_w + TILE_GAP), top + row * (tile_h + TILE_GAP), tile_w, tile_h)
    })
}

fn tile_w() -> i32 { (FLYOUT_W - 2 * FLYOUT_PAD - 2 * TILE_GAP) / 3 }

fn step_panel(y: &mut i32) -> StepPanel {
    let tiles = tile_grid(y, STEP_TILE_H);
    *y += SECTION_GAP;
    let parts = std::array::from_fn(|i| (FLYOUT_PAD + i as i32 * (tile_w() + TILE_GAP), *y, tile_w(), CHIP_H));
    *y += CHIP_H + TILE_GAP;
    let shadow = (FLYOUT_PAD, *y, FLYOUT_W - 2 * FLYOUT_PAD, CHIP_H);
    *y += CHIP_H;
    StepPanel { tiles, parts, shadow }
}

/// The ON / OFF pill at the right of Step's shadow row.
pub(super) fn shadow_switch((x, y, w, h): Rect4) -> Rect4 {
    const PILL_W: i32 = 44;
    (x + w - PILL_W, y + 3, PILL_W, h - 6)
}

fn color_block(y: &mut i32) -> ColorBlock {
    let inner = FLYOUT_W - 2 * FLYOUT_PAD;
    let arrow = PALETTE_ROW_H;
    let prev = (FLYOUT_PAD, *y, arrow, PALETTE_ROW_H);
    let next = (FLYOUT_PAD + inner - arrow, *y, arrow, PALETTE_ROW_H);
    let name = (FLYOUT_PAD + arrow, *y, inner - 2 * arrow, PALETTE_ROW_H);
    *y += PALETTE_ROW_H + 6;
    // Two rows of eight swatches, centered.
    let pitch = SWATCH + SWATCH_GAP;
    let grid_w = SWATCH_COLS * pitch - SWATCH_GAP;
    let left = FLYOUT_PAD + (inner - grid_w) / 2;
    let top = *y;
    let swatches = std::array::from_fn(|i| {
        let (col, row) = (i as i32 % SWATCH_COLS, i as i32 / SWATCH_COLS);
        (left + col * pitch, top + row * pitch, SWATCH, SWATCH)
    });
    let two_rows = 2 * SWATCH + SWATCH_GAP;
    *y += two_rows + SECTION_GAP;
    // Any other color, on the same grid: the first column holds the
    // eyedropper over the current color, the spectrum fills the rest.
    let eyedropper = (left, *y, SWATCH, SWATCH);
    let current = (left, *y + pitch, SWATCH, SWATCH);
    let spectrum = (left + pitch, *y, grid_w - pitch, two_rows);
    *y += two_rows;
    ColorBlock { prev, next, name, swatches, eyedropper, current, spectrum }
}

pub(super) fn flyout_layout(t: Tool) -> FlyoutLayout {
    let mut y = FLYOUT_PAD;
    let step = (t == Tool::Step).then(|| step_panel(&mut y));
    let looks = (t == Tool::Highlight).then(|| tile_grid(&mut y, LOOK_TILE_H));
    let mut sliders = Vec::new();
    for (i, s) in Slider::for_tool(t).iter().enumerate() {
        if i > 0 { y += ROW_GAP; }
        sliders.push((*s, (FLYOUT_PAD, y, FLYOUT_W - 2 * FLYOUT_PAD, ROW_H)));
        y += ROW_H;
    }
    let colors = has_colors(t).then(|| {
        if !sliders.is_empty() || step.is_some() || looks.is_some() { y += SECTION_GAP; }
        color_block(&mut y)
    });
    FlyoutLayout { w: FLYOUT_W, h: y + FLYOUT_PAD, sliders, colors, step, looks }
}

/// `r` moved by `by`.
pub(super) fn shifted((x, y, w, h): Rect4, by: (i32, i32)) -> Rect4 { (x + by.0, y + by.1, w, h) }

/// Ease-out cubic: quick out of the column, settling into place.
pub(super) fn ease_out(t: f32) -> f32 {
    let rest = 1.0 - t.clamp(0.0, 1.0);
    1.0 - rest.powi(3)
}

/// Step shape tile `i`'s number: its place, but the pill shows 12, since
/// with one digit it is a plain round.
pub(super) fn step_tile_number(i: usize) -> u32 {
    if StepShape::ALL[i] == StepShape::Pill { 12 } else { i as u32 + 1 }
}

/// The tool a session opens on: the one the last session ended on, when
/// this session has it and it may open a session (Crop never does), else
/// the Pen.
pub(super) fn opening_tool(settings: &AppSettings, set: ToolSet) -> Tool {
    Tool::ALL.into_iter()
        .find(|&t| t.can_open_with() && set.has(t) && t.name() == settings.last_tool)
        .unwrap_or(Tool::Pen)
}

/// `t`'s saved color and width as the stroke it draws with. The color may
/// be off every palette (the spectrum and the eyedropper pick anything);
/// the flyout then rings the current-color chip instead of a swatch.
pub(super) fn tool_stroke(settings: &AppSettings, t: Tool) -> Stroke {
    // Step paints one part of its marker at a time and opens on the part
    // that wears its main color.
    let color = if t == Tool::Step { step_look(settings).accent() } else { saved_color(settings, t) };
    Stroke { color, thickness: Slider::Width.usable(settings.tool_style(t).thickness) }
}

/// `t`'s saved color, opaque. Pure red was every tool's default before
/// there were palettes; saved that way, it opens as the first palette's
/// red, as it always has.
pub(super) fn saved_color(settings: &AppSettings, t: Tool) -> Color {
    let saved = Color::from_argb(settings.tool_style(t).color_argb).with_alpha(0xFF);
    if saved == Color::new_opaque(255, 0, 0) { Palettes::ALL[0].colors[0] } else { saved }
}

/// The next step marker's look: the saved one, or a plain disc in the Step
/// tool's color until a look is picked.
pub(super) fn step_look(settings: &AppSettings) -> StepLook {
    settings.step_look.unwrap_or_else(|| StepLook::recipe(StepShape::Disc, saved_color(settings, Tool::Step)))
}

/// The spectrum's color at `(u, v)`, each 0 to 1 and clamped: the hues run
/// across, light at the top through full color to dark at the foot; the
/// last strip goes from white down to black.
pub(super) fn spectrum_color(u: f32, v: f32) -> Color {
    let (u, v) = (u.clamp(0.0, 1.0), v.clamp(0.0, 1.0));
    if u < SPECTRUM_HUES {
        return hsl(u / SPECTRUM_HUES * 360.0, 1.0, 0.92 - v * 0.80);
    }
    let gray = ((1.0 - v) * 255.0).round() as u8;
    Color::new_opaque(gray, gray, gray)
}

/// Hue in degrees, saturation and lightness 0 to 1, as an opaque color.
fn hsl(hue: f32, sat: f32, light: f32) -> Color {
    let chroma = (1.0 - (2.0 * light - 1.0).abs()) * sat;
    let sector = hue.rem_euclid(360.0) / 60.0;
    let second = chroma * (1.0 - (sector % 2.0 - 1.0).abs());
    let order = [
        (chroma, second, 0.0), (second, chroma, 0.0), (0.0, chroma, second),
        (0.0, second, chroma), (second, 0.0, chroma), (chroma, 0.0, second),
    ];
    let (r, g, b) = order[(sector as usize).min(5)];
    let lift = light - chroma / 2.0;
    let byte = |c: f32| ((c + lift).clamp(0.0, 1.0) * 255.0).round() as u8;
    Color::new_opaque(byte(r), byte(g), byte(b))
}

/// `c` as hue in whole degrees, saturation and lightness in whole percent.
pub(super) fn to_hsl(c: Color) -> (u32, u32, u32) {
    let [r, g, b] = [c.r, c.g, c.b].map(|v| f32::from(v) / 255.0);
    let (hi, lo) = (r.max(g).max(b), r.min(g).min(b));
    let light = (hi + lo) / 2.0;
    let spread = hi - lo;
    let percent = |v: f32| (v * 100.0).round() as u32;
    if spread < 1e-6 { return (0, 0, percent(light)); }
    let sat = spread / (1.0 - (2.0 * light - 1.0).abs());
    let sector = if hi == r {
        ((g - b) / spread).rem_euclid(6.0)
    } else if hi == g {
        (b - r) / spread + 2.0
    } else {
        (r - g) / spread + 4.0
    };
    ((sector * 60.0).round() as u32 % 360, percent(sat).min(100), percent(light))
}

#[cfg(test)]
mod tests {
    use super::*;
    use framefang_core::settings::ToolStyle;

    fn inside(r: Rect4, l: &FlyoutLayout) -> bool { r.0 >= 0 && r.1 >= 0 && r.0 + r.2 <= l.w && r.1 + r.3 <= l.h }

    #[test]
    fn saved_values_are_rounded_or_replaced() {
        let w = Slider::Width;
        assert_eq!(w.usable(9.0), 9.0);
        assert_eq!(w.usable(6.4), 6.0);
        assert_eq!(w.usable(24.0), 24.0);
        for broken in [0.0, 99.0, f32::NAN, f32::INFINITY] { assert_eq!(w.usable(broken), 4.0, "{broken}"); }
        assert_eq!(Slider::TextSize.usable(500.0), 28.0);
        assert_eq!(Slider::BlockSize.usable(0.0), 10.0);
        assert_eq!(Slider::Zoom.usable(9.0), 2.0);
        for s in [Slider::Width, Slider::Opacity, Slider::TextSize, Slider::BlockSize, Slider::Zoom] {
            let (lo, hi) = s.range();
            assert!((lo..=hi).contains(&s.fallback()), "{s:?} falls back inside its own range");
        }
    }

    #[test]
    fn readouts_say_what_the_value_means() {
        assert_eq!(Slider::Zoom.readout(3.0), "3x");
        assert_eq!(Slider::Width.readout(6.4), "6 px");
        assert_eq!(Slider::Opacity.readout(255.0), "100%");
        assert_eq!(Slider::Opacity.readout(128.0), "50%");
    }

    #[test]
    fn every_flyout_holds_what_its_tool_uses_and_all_of_it_fits() {
        for t in Tool::ALL.into_iter().filter(|t| has_flyout(*t)) {
            let l = flyout_layout(t);
            assert!(!l.sliders.is_empty() || l.colors.is_some(), "{t:?} has an empty flyout");
            assert_eq!(l.colors.is_some(), has_colors(t), "{t:?}");
            for (_, r) in &l.sliders { assert!(inside(*r, &l), "{t:?} {r:?}"); }
            let Some(c) = l.colors else { continue };
            for r in [c.prev, c.next, c.name, c.eyedropper, c.current, c.spectrum].iter().chain(&c.swatches) {
                assert!(inside(*r, &l), "{t:?} {r:?}");
            }
            let lowest_swatch = c.swatches.iter().map(|r| r.1 + r.3).max().unwrap();
            assert!(c.eyedropper.1 > lowest_swatch, "{t:?}: the any-color row is under the swatches");
            assert!(c.current.1 >= c.eyedropper.1 + c.eyedropper.3, "{t:?}: current under the eyedropper");
            assert!(c.spectrum.0 >= c.eyedropper.0 + c.eyedropper.2, "{t:?}: spectrum beside them");
            assert_eq!(c.spectrum.0 + c.spectrum.2, c.swatches[7].0 + c.swatches[7].2, "{t:?}: ends with the grid");
        }
        assert!(flyout_layout(Tool::Step).sliders.is_empty() && flyout_layout(Tool::Step).colors.is_some());
        assert!(flyout_layout(Tool::Pixelate).colors.is_none());
        assert_eq!(Slider::for_tool(Tool::Marker), &[Slider::Width, Slider::Opacity]);
    }

    #[test]
    fn only_tools_with_something_to_set_have_a_flyout() {
        assert!(!has_flyout(Tool::Spotlight) && !has_flyout(Tool::Crop));
        assert!(has_flyout(Tool::Magnify) && has_flyout(Tool::Pen) && has_flyout(Tool::Pixelate));
    }

    #[test]
    fn step_shows_its_looks_parts_and_shadow_before_its_colors() {
        let l = flyout_layout(Tool::Step);
        let p = l.step.expect("Step has its own panel");
        let within = |(x, y, w, h): Rect4| x >= FLYOUT_PAD && x + w <= l.w - FLYOUT_PAD && y >= FLYOUT_PAD && y + h <= l.h - FLYOUT_PAD;
        for r in p.tiles.iter().chain(&p.parts).chain([&p.shadow]) { assert!(within(*r), "{r:?}"); }
        for (i, a) in p.tiles.iter().enumerate() {
            for b in &p.tiles[i + 1..] {
                assert!(a.0 + a.2 <= b.0 || b.0 + b.2 <= a.0 || a.1 + a.3 <= b.1 || b.1 + b.3 <= a.1, "tiles touch");
            }
            // The biggest marker fits its tile.
            let look = StepLook::recipe(StepShape::ALL[i], Color::RED);
            let (x0, y0, x1, y1) = framefang_core::step::extent(step_tile_number(i), &look);
            assert!((x1 - x0) as i32 + 4 <= a.2 && (y1 - y0) as i32 + 4 <= a.3, "{:?} fits", look.shape);
        }
        let colors = l.colors.expect("and its colors");
        assert!(p.shadow.1 + p.shadow.3 < colors.prev.1, "the colors come after");
        assert!(l.h <= 520, "short enough for a small screen: {}", l.h);
        // Each part chip holds its dot and its word.
        for (i, r) in p.parts.iter().enumerate() {
            let label = framefang_core::step::StepPart::ALL[i].label();
            let words = framefang_core::text::layout(label, FLYOUT_TEXT_PX).width();
            assert!(10 + 6 + words.round() as i32 + 8 <= r.2, "{label} spills");
        }
        assert!(flyout_layout(Tool::Pen).step.is_none());
        let highlight = flyout_layout(Tool::Highlight);
        assert!(highlight.looks.is_some_and(|t| t.iter().all(|r| inside(*r, &highlight))));
    }

    #[test]
    fn a_tool_opens_in_its_own_color_and_width() {
        let mut s = AppSettings::default();
        let blue = Color::new_opaque(0, 100, 255);
        s.set_tool_style(Tool::Arrow, ToolStyle { color_argb: blue.to_argb(), thickness: 7.0 });
        let arrow = tool_stroke(&s, Tool::Arrow);
        assert_eq!((arrow.color, arrow.thickness), (blue, 7.0));
        assert_eq!(tool_stroke(&s, Tool::Marker).thickness, 16.0, "the marker keeps its own default");
        // Pure red from before the palettes opens as the first palette's red.
        let red = Color::new_opaque(255, 0, 0);
        s.set_tool_style(Tool::Pen, ToolStyle { color_argb: red.to_argb(), thickness: 3.0 });
        assert_eq!(tool_stroke(&s, Tool::Pen).color, Palettes::ALL[0].colors[0]);
        // Any other color is kept, off every palette or not.
        let teal = Color::new_opaque(18, 140, 126);
        s.set_tool_style(Tool::Pen, ToolStyle { color_argb: teal.to_argb(), thickness: 3.0 });
        assert_eq!(tool_stroke(&s, Tool::Pen).color, teal);
    }

    #[test]
    fn a_session_never_opens_on_a_tool_it_does_not_show() {
        let mut s = AppSettings::default();
        s.last_tool = "Marker".into();
        assert_eq!(opening_tool(&s, ToolSet::Editor), Tool::Marker);
        s.last_tool = "Nonsense".into();
        assert_eq!(opening_tool(&s, ToolSet::Editor), Tool::Pen);
        s.last_tool = "Magnify".into();
        assert_eq!(opening_tool(&s, ToolSet::Video), Tool::Pen, "a video frame has no lens");
        assert_eq!(opening_tool(&s, ToolSet::Editor), Tool::Magnify);
        s.last_tool = "Crop".into();
        assert_eq!(opening_tool(&s, ToolSet::Editor), Tool::Pen, "Crop never opens a session");
    }

    #[test]
    fn the_spectrum_runs_through_the_hues_and_ends_in_grays() {
        let mid = (0.92 - 0.5) / 0.80;
        assert_eq!(spectrum_color(0.0, mid), Color::new_opaque(255, 0, 0), "full red halfway down the left");
        let green = spectrum_color(SPECTRUM_HUES / 3.0, mid);
        assert!(green.g > 250 && green.r < 5 && green.b < 5, "{green:?}");
        let blue = spectrum_color(SPECTRUM_HUES * 2.0 / 3.0, mid);
        assert!(blue.b > 250 && blue.r < 5 && blue.g < 5, "{blue:?}");
        let (top, foot) = (spectrum_color(0.0, 0.0), spectrum_color(0.0, 1.0));
        assert!(top.g > 200 && foot.r < 70, "{top:?} {foot:?}");
        assert_eq!(spectrum_color(1.0, 0.0), Color::new_opaque(255, 255, 255));
        assert_eq!(spectrum_color(1.0, 1.0), Color::new_opaque(0, 0, 0));
        assert_eq!(spectrum_color(-1.0, 5.0), spectrum_color(0.0, 1.0), "clamped, never wrapped");
    }

    #[test]
    fn colors_read_out_in_hsl() {
        assert_eq!(to_hsl(Color::new_opaque(255, 0, 0)), (0, 100, 50));
        assert_eq!(to_hsl(Color::new_opaque(0, 255, 0)), (120, 100, 50));
        assert_eq!(to_hsl(Color::new_opaque(0, 0, 255)), (240, 100, 50));
        assert_eq!(to_hsl(Color::new_opaque(128, 128, 128)), (0, 0, 50));
        assert_eq!(to_hsl(Color::new_opaque(255, 255, 255)), (0, 0, 100));
        let (h, s, l) = to_hsl(Color::new_opaque(0x3A, 0x7B, 0xD5));
        assert!((214..=215).contains(&h) && (64..=66).contains(&s) && (52..=54).contains(&l), "{h} {s} {l}");
    }

    #[test]
    fn the_slide_out_finishes_and_starts_quickly() {
        assert_eq!(ease_out(0.0), 0.0);
        assert_eq!(ease_out(1.0), 1.0);
        assert_eq!(ease_out(2.0), 1.0);
        assert!(ease_out(0.25) > 0.5, "most of the way out in the first quarter");
    }
}
