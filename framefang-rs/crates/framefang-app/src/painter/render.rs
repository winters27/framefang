//! From annotations to pixels: which drawing routine each kind uses, and
//! the order a whole list is drawn in.

use framefang_core::annotation::{Annotation, AnnotationKind, Point2};
use image::{ImageBuffer, Rgba};

use super::{
    arrow, dim_outside_spots, draw_unicode_text, line, magnify, pixelate_rect, polyline, step_marker,
    stroke_ellipse, stroke_rect, text_mark, Surface,
};

/// Where a pass of annotations draws: the picture they sit on, and what
/// their effects read.
pub struct Pass<'a> {
    /// The untouched capture Pixelate averages, and where the surface's
    /// origin sits in it.
    pub source: Option<&'a ImageBuffer<Rgba<u8>, Vec<u8>>>,
    pub src_offset: (i32, i32),
    /// The picture's rectangle on the surface: spotlights dim inside it,
    /// lenses read only from it.
    pub bounds: (i32, i32, i32, i32),
}

/// Draw `anns` in order. Spotlights first, as one dim under all the ink;
/// a Pixelate block landing outside them is dimmed the same, so it never
/// shows bright in the dark; a lens magnifies what is drawn before it.
pub fn render_all<'b, S: Surface>(s: &mut S, anns: impl Iterator<Item = &'b Annotation> + Clone, pass: &Pass) {
    let spots: Vec<(Point2, Point2)> = anns.clone().filter_map(|a| match a.kind {
        AnnotationKind::Spotlight { start, end } => Some((start, end)),
        _ => None,
    }).collect();
    dim_outside_spots(s, &spots, pass.bounds);
    for a in anns {
        match &a.kind {
            AnnotationKind::Spotlight { .. } => {}
            AnnotationKind::Pixelate { start, end, .. } => {
                render_annotation_offset(s, a, pass.source, pass.src_offset);
                let (x0, y0) = (start.x.min(end.x) as i32, start.y.min(end.y) as i32);
                let (x1, y1) = (start.x.max(end.x) as i32, start.y.max(end.y) as i32);
                if let Some(r) = intersect((x0, y0, x1 - x0, y1 - y0), pass.bounds) {
                    dim_outside_spots(s, &spots, r);
                }
            }
            AnnotationKind::Magnify { stroke, center, edge, zoom } =>
                magnify(s, *stroke, *center, *edge, *zoom, pass.bounds),
            _ => render_annotation_offset(s, a, pass.source, pass.src_offset),
        }
    }
}

fn intersect(a: (i32, i32, i32, i32), b: (i32, i32, i32, i32)) -> Option<(i32, i32, i32, i32)> {
    let (x0, y0) = (a.0.max(b.0), a.1.max(b.1));
    let (x1, y1) = ((a.0 + a.2).min(b.0 + b.2), (a.1 + a.3).min(b.1 + b.3));
    (x1 > x0 && y1 > y0).then_some((x0, y0, x1 - x0, y1 - y0))
}

/// Draws one annotation, with `source` (for Pixelate) in the surface's own
/// coordinates.
pub fn render_annotation<S: Surface>(s: &mut S, a: &Annotation, source: Option<&ImageBuffer<Rgba<u8>, Vec<u8>>>) {
    render_annotation_offset(s, a, source, (0, 0));
}

/// Like [`render_annotation`], for a surface whose (0, 0) sits at
/// `src_offset` in `source`. The editor needs that when its window is not
/// lined up exactly with the capture it shows.
pub fn render_annotation_offset<S: Surface>(s: &mut S, a: &Annotation, source: Option<&ImageBuffer<Rgba<u8>, Vec<u8>>>,
                                            src_offset: (i32, i32)) {
    use AnnotationKind as K;
    match &a.kind {
        // Pen and Marker differ only in the color and width they are given.
        K::Pen { stroke, points } | K::Marker { stroke, points } =>
            polyline(s, points, stroke.thickness, stroke.color),
        // A Line goes through whole pixels: its ends are truncated first.
        K::Line { stroke, start, end } => {
            let (from, to) = ((start.x as i32, start.y as i32), (end.x as i32, end.y as i32));
            line(s, from.0, from.1, to.0, to.1, stroke.thickness, stroke.color);
        }
        K::Arrow { stroke, start, end } => arrow(s, *start, *end, stroke.thickness, stroke.color),
        K::Rectangle { stroke, start, end } => stroke_rect(s, *start, *end, stroke.thickness, stroke.color),
        K::Ellipse { stroke, start, end } => stroke_ellipse(s, *start, *end, stroke.thickness, stroke.color),
        K::Step { center, number, look } => step_marker(s, *center, *number, look),
        K::TextMark { rects, look } => text_mark(s, rects, look),
        // Without the clean capture there is nothing to average, so the
        // area is left as it is.
        K::Pixelate { start, end, block_size } => {
            if let Some(src) = source {
                pixelate_rect(s, src, *start, *end, *block_size, src_offset);
            }
        }
        // `position` is the top left of the text block and `font_size` the
        // em size in pixels.
        K::Text { color, position, text, font_size } =>
            draw_unicode_text(s, position.x, position.y, *font_size, text, *color),
        // Drawn by `render_all`, which knows the picture's bounds: a lone
        // spotlight dims the whole surface it is given, a lone lens reads it.
        K::Spotlight { start, end } => {
            let all = (0, 0, s.width(), s.height());
            dim_outside_spots(s, &[(*start, *end)], all);
        }
        K::Magnify { stroke, center, edge, zoom } => {
            let all = (0, 0, s.width(), s.height());
            magnify(s, *stroke, *center, *edge, *zoom, all);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::painter::{ImageSurface, U32Surface};
    use framefang_core::annotation::Stroke;
    use framefang_core::color::Rgba as K;

    #[test]
    fn a_two_line_text_annotation_inks_rows_a_line_apart() {
        let (w, h) = (300usize, 200usize);
        let mut px = vec![0u32; w * h];
        let note = Annotation::text_sized(K::WHITE, Point2::new(10.0, 10.0), "one\ntwo", 28.0);
        render_annotation(&mut U32Surface { buf: &mut px, stride: w as i32, height: h as i32 }, &note, None);
        let inked: Vec<usize> = (0..h).filter(|&y| px[y * w..(y + 1) * w].iter().any(|&p| p != 0)).collect();
        let (first, last) = (inked.first().copied().expect("drawn"), inked.last().copied().expect("drawn"));
        assert!((last - first) as f32 > framefang_core::text::line_height(28.0),
                "the second line is not a full line below the first");
    }

    #[test]
    fn a_pixelate_without_its_source_leaves_the_picture_alone() {
        let mut img = ImageBuffer::from_fn(20, 20, |x, y| Rgba([x as u8 * 10, y as u8 * 10, 7, 255]));
        let before = img.clone();
        let mut blur = Annotation::pixelate(Point2::new(2.0, 2.0));
        blur.extend(Point2::new(18.0, 18.0));
        render_annotation(&mut ImageSurface(&mut img), &blur, None);
        assert_eq!(img, before);
        // With the source it does change.
        render_annotation(&mut ImageSurface(&mut img), &blur, Some(&before));
        assert_ne!(img, before);
    }

    #[test]
    fn pen_and_marker_strokes_draw_the_same_path() {
        let stroke = Stroke { color: K::WHITE, thickness: 3.0 };
        let points = vec![Point2::new(4.0, 4.0), Point2::new(26.0, 20.0)];
        let pen = Annotation { kind: AnnotationKind::Pen { stroke, points: points.clone() }, time: None };
        let marker = Annotation { kind: AnnotationKind::Marker { stroke, points }, time: None };
        let draw = |a: &Annotation| {
            let mut px = vec![0u32; 30 * 30];
            render_annotation(&mut U32Surface { buf: &mut px, stride: 30, height: 30 }, a, None);
            px
        };
        assert_eq!(draw(&pen), draw(&marker));
    }
}
