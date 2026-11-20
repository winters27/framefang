//! Annotating a video: the timeline along the bottom of the frame (the
//! clock, the seek track with a tick at each timed annotation, and the chip
//! that says how long new ink stays), the time window each annotation is
//! stamped with, and turning the finished ink into the transparent overlays
//! ffmpeg burns over the clip.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use framefang_core::annotation::Annotation;
use framefang_core::color::Rgba as Color;
use image::{ImageBuffer, Rgba};

use super::geometry::Rect4;
use super::paint::{color, Canvas};
use super::{Overlay, OverlayGroup, VideoCommit, NEON_GREEN};
use crate::painter;

type Image = ImageBuffer<Rgba<u8>, Vec<u8>>;

/// The clip a video session annotates, and where in it the editor is.
pub(super) struct Clip {
    pub path: PathBuf,
    /// Its length from ffmpeg's banner. `None` when that didn't parse: the
    /// timeline then stays hidden and every annotation spans the clip.
    pub duration: Option<f32>,
    /// The playhead, in clip seconds. New ink starts here.
    pub at: f32,
    /// When the frame on screen was taken from, so a seek landing on it
    /// spawns no ffmpeg.
    pub frame_t: f32,
    /// The last frame swap, to space them out while the playhead is dragged.
    pub last_swap: Option<Instant>,
    /// Which of `DURATIONS` the chip shows.
    pub choice: usize,
}

impl Clip {
    pub fn new(path: PathBuf, duration: Option<f32>) -> Self {
        Clip { path, duration, at: 0.0, frame_t: 0.0, last_swap: None, choice: 0 }
    }
}

/// How long new ink stays, as the chip cycles through it. `None` lasts to
/// the clip's end; the pixel font has no infinity sign, so it reads "End".
pub(super) const DURATIONS: [(Option<f32>, &str); 4] = [(None, "End"), (Some(3.0), "3s"), (Some(5.0), "5s"), (Some(10.0), "10s")];

/// Each swap mid-drag is a synchronous ffmpeg run, so they come no closer
/// than this; the press and the release always swap.
const SWAP_SPACING: Duration = Duration::from_millis(200);

const BAR_MAX_W: i32 = 720;
const BAR_H: i32 = 40;
/// Kept clear between the bar and the screen's edges.
const BAR_MARGIN: i32 = 16;
const BAR_PAD: i32 = 10;
const TRACK_H: i32 = 10;
const PLAYHEAD_W: i32 = 8;
const CHIP_W: i32 = 44;
const CHIP_H: i32 = 24;

/// The bar: centered at the foot of `bounds` (one monitor, so it never
/// straddles two), at most 720 px wide, shrinking with a small screen.
pub(super) fn timeline_bar_rect((bx, by, bw, bh): Rect4) -> Rect4 {
    let w = BAR_MAX_W.min(bw - 2 * BAR_MARGIN).max(120);
    (bx + (bw - w) / 2, (by + bh - BAR_MARGIN - BAR_H).max(by), w, BAR_H)
}

/// The duration chip, inside the bar's right end, so the screen's edge
/// can never cut it.
pub(super) fn timeline_chip_rect((bx, by, bw, bh): Rect4) -> Rect4 {
    (bx + bw - BAR_PAD - CHIP_W, by + (bh - CHIP_H) / 2, CHIP_W, CHIP_H)
}

/// The seek track, between the clock and the chip. The clock's room is
/// measured on "total / total", the widest it gets, so the track holds
/// still while the digits change during a scrub.
pub(super) fn timeline_track(bar: Rect4, total: f32) -> Rect4 {
    let widest = format!("{0} / {0}", timecode(total));
    let x = bar.0 + BAR_PAD + crate::bitmap_font::measure(&widest, 2) + 10;
    let w = (timeline_chip_rect(bar).0 - 8 - x).max(40);
    (x, bar.1 + (bar.3 - TRACK_H) / 2, w, TRACK_H)
}

/// "M:SS": minutes unpadded, hours folded into them (recordings are short).
pub(super) fn timecode(secs: f32) -> String {
    let whole = secs.max(0.0).round() as u64;
    format!("{}:{:02}", whole / 60, whole % 60)
}

/// Whether `a` shows at clip time `t`: its window is `[start, end)`, no
/// window means the whole clip, and an infinite end means to the clip's end.
pub(super) fn visible_at(a: &Annotation, t: f32) -> bool {
    a.time.map_or(true, |(start, end)| (start..end).contains(&t))
}

/// ffmpeg gives no frame for a seek at or past the clip's end, so frames
/// are taken from just inside it.
pub(super) fn extract_time(t: f32, duration: f32) -> f32 { t.min((duration - 0.05).max(0.0)) }

/// The window new ink gets: from the playhead, for the chip's `preset`.
/// `None` is the whole clip, which is what an untouched session (playhead
/// at 0, "End") produces, so it burns exactly as a clip without a timeline.
///
/// The start is pulled inside the clip like an extraction: the track lets
/// the playhead reach the full duration, and a window starting there would
/// gate the burn on a time no frame reaches (the last frame stamps one
/// frame short of the container's length), dropping ink the preview
/// showed. A preset running past the end is open-ended too, so the burn
/// trusts no parsed duration to the hundredth.
pub(super) fn stamp_window(at: f32, duration: f32, preset: Option<f32>) -> Option<(f32, f32)> {
    let start = extract_time(at, duration);
    let end = preset.map(|d| start + d).filter(|&end| end < duration).unwrap_or(f32::INFINITY);
    (start != 0.0 || end.is_finite()).then_some((start, end))
}

/// Runs of neighbouring annotations sharing one window, in draw order.
/// Only neighbours are merged: with A (whole clip), B (timed), C (whole
/// clip), folding C into A's run would burn B over C while the preview
/// painted C over B. Interleaving costs one overlay per run; a session
/// with no timing at all is still one run.
pub(super) fn group_by_window(ink: &[Annotation]) -> Vec<(Option<(f32, f32)>, Vec<&Annotation>)> {
    let mut runs: Vec<(Option<(f32, f32)>, Vec<&Annotation>)> = Vec::new();
    for a in ink {
        match runs.last_mut() {
            Some((window, run)) if *window == a.time => run.push(a),
            _ => runs.push((a.time, vec![a])),
        }
    }
    runs
}

/// The video version of a finished capture: one transparent overlay the
/// frame's size per run of ink sharing a window, with the window ffmpeg
/// turns it on for.
///
/// Ink can't be painted straight onto transparency: the painter blends
/// against what it reads, and a marker blended over nothing comes out an
/// opaque dark band. So each run is painted onto a live copy of the frame
/// its window starts on, and every pixel touched is copied, opaque, into
/// the overlay (`MirrorSurface`). Blends and pixelation then match the
/// still editor; the only catch is that they carry that frame's pixels.
///
/// Runs on the burn thread: a run starting anywhere but the committed frame
/// needs its own ffmpeg seek, which must not stall the event loop. Seeks
/// are shared between runs starting at the same moment.
pub(crate) fn compose_overlay_groups(commit: &VideoCommit, video: &Path) -> Vec<OverlayGroup> {
    let (w, h) = commit.frame.dimensions();
    let mut runs = group_by_window(&commit.annotations);
    // No ink: one clear whole-clip overlay, so the burn still writes the
    // annotated copy.
    if runs.is_empty() { runs.push((None, Vec::new())); }
    let mut seeks: Vec<(f32, Image)> = Vec::new();
    runs.into_iter().map(|(window, ink)| {
        let start = window.map_or(0.0, |(s, _)| s);
        let pristine = frame_for(commit, video, start, &mut seeks);
        let mut live = pristine.clone();
        let mut overlay = ImageBuffer::from_pixel(w, h, Rgba([0, 0, 0, 0]));
        let mut surface = MirrorSurface { frame: &mut live, out: &mut overlay };
        for a in ink { painter::render_annotation(&mut surface, a, Some(&pristine)); }
        let end = window.map(|(_, e)| e).filter(|e| e.is_finite());
        (overlay, start, end)
    }).collect()
}

/// The clip's frame at `start`, for a run's blends to read: the committed
/// frame when it is that one, a seek already made, or a new seek. A failed
/// seek falls back to the committed frame; one run blending against the
/// wrong frame beats losing the whole burn.
fn frame_for(commit: &VideoCommit, video: &Path, start: f32, seeks: &mut Vec<(f32, Image)>) -> Image {
    if start == commit.frame_t { return commit.frame.clone(); }
    if let Some((_, frame)) = seeks.iter().find(|(t, _)| *t == start) { return frame.clone(); }
    let t = commit.duration.map_or(start, |d| extract_time(start, d));
    match crate::tray_loop::extract_frame_at(video, t) {
        Ok(frame) => {
            seeks.push((start, frame.clone()));
            frame
        }
        Err(e) => {
            eprintln!("framefang: no frame at {t:.2}s for a timed overlay, using the committed one: {e}");
            commit.frame.clone()
        }
    }
}

/// A paint surface that reads and writes a live copy of the frame, so the
/// painter blends and samples exactly as on screen, and copies each pixel
/// it writes, opaque, into `out`. Pixels nothing touches stay clear.
pub(super) struct MirrorSurface<'a> {
    pub frame: &'a mut Image,
    pub out: &'a mut Image,
}

impl painter::Surface for MirrorSurface<'_> {
    fn width(&self) -> i32 { self.frame.width() as i32 }
    fn height(&self) -> i32 { self.frame.height() as i32 }
    fn read(&self, x: i32, y: i32) -> [u8; 4] { self.frame.get_pixel(x as u32, y as u32).0 }
    fn write(&mut self, x: i32, y: i32, rgba: [u8; 4]) {
        let (x, y) = (x as u32, y as u32);
        *self.frame.get_pixel_mut(x, y) = Rgba(rgba);
        *self.out.get_pixel_mut(x, y) = Rgba([rgba[0], rgba[1], rgba[2], 0xFF]);
    }
}

/// The timeline: the clock on the left, the seek track with its played part
/// in the selection's blue, a green tick where each timed annotation starts
/// and the playhead, then the duration chip. Framed in the editor's green.
pub(super) fn draw_timeline_bar(c: &mut Canvas, bounds: Rect4, at: f32, total: f32, chip: &str, starts: &[f32]) {
    const SURFACE: u32 = 0x00_22_22_24;
    const CHIP_FILL: u32 = 0x00_2E_2E_32;
    const GROOVE: u32 = 0x00_0A_0A_0E;
    const PLAYED: u32 = 0x00_64_95_ED;
    const CLOCK: u32 = 0x00_E8_E8_EC;
    let bar = timeline_bar_rect(bounds);
    let (bx, by, bw, bh) = bar;
    c.fill_rect(bar, SURFACE);
    c.outline(bx, by, bx + bw, by + bh, NEON_GREEN);

    let clock = format!("{} / {}", timecode(at), timecode(total));
    let clock_y = by + (bh - 2 * crate::bitmap_font::GLYPH_H) / 2;
    painter::draw_text(&mut c.surface(), bx + BAR_PAD, clock_y, 2, &clock, color(CLOCK));

    let (tx, ty, tw, th) = timeline_track(bar, total);
    let x_at = |t: f32| {
        let share = if total > 0.0 { (t / total).clamp(0.0, 1.0) } else { 0.0 };
        tx + ((tw - 1) as f32 * share).round() as i32
    };
    let head = x_at(at);
    c.fill(tx, ty, tx + tw, ty + th, GROOVE);
    c.fill(tx, ty, head, ty + th, PLAYED);
    c.outline(tx, ty, tx + tw, ty + th, NEON_GREEN);
    // Ticks through the groove, so timed ink is found without scrubbing.
    for x in starts.iter().map(|&t| x_at(t)) {
        for y in ty + 1..ty + th - 1 { c.set(x, y, NEON_GREEN); }
    }
    // The playhead stands taller than the groove.
    let kx = (head - PLAYHEAD_W / 2).clamp(tx - 1, tx + tw - PLAYHEAD_W + 1);
    let (kh, ky) = (th + 6, ty - 3);
    c.fill(kx, ky, kx + PLAYHEAD_W, ky + kh, 0x00_FF_FF_FF);
    c.outline(kx, ky, kx + PLAYHEAD_W, ky + kh, NEON_GREEN);

    let (cx, cy, cw, ch) = timeline_chip_rect(bar);
    c.fill(cx, cy, cx + cw, cy + ch, CHIP_FILL);
    c.outline(cx, cy, cx + cw, cy + ch, NEON_GREEN);
    let word_x = cx + (cw - crate::bitmap_font::measure(chip, 2)) / 2;
    let word_y = cy + (ch - 2 * crate::bitmap_font::GLYPH_H) / 2;
    painter::draw_text(&mut c.surface(), word_x, word_y, 2, chip, Color::WHITE);
}

impl Overlay {
    /// The clip's length when this is a video session that has one: what
    /// the timeline needs to show at all.
    pub(super) fn timeline_len(&self) -> Option<f32> { self.clip.as_ref()?.duration }

    /// Whether `a` is on screen now: everything outside a video session,
    /// only what the playhead is inside of in one.
    pub(super) fn shows(&self, a: &Annotation) -> bool {
        self.clip.as_ref().map_or(true, |clip| visible_at(a, clip.at))
    }

    /// The window new ink gets at the playhead with the chip's preset.
    pub(super) fn ink_window(&self) -> Option<(f32, f32)> {
        let clip = self.clip.as_ref()?;
        stamp_window(clip.at, clip.duration?, DURATIONS[clip.choice % DURATIONS.len()].0)
    }

    /// Move the playhead to the pointer's place along the track. `exact`
    /// (the press and the release) always swaps the frame; a drag swaps at
    /// most every `SWAP_SPACING`.
    pub(super) fn scrub_to_pointer(&mut self, exact: bool) {
        let Some(total) = self.timeline_len() else { return };
        let (tx, _, tw, _) = timeline_track(timeline_bar_rect(self.chrome_bounds(self.pointer)), total);
        if tw < 2 { return; }
        let share = (self.pointer.0 - tx) as f32 / (tw - 1) as f32;
        let share = if share.is_finite() { share.clamp(0.0, 1.0) } else { 0.0 };
        let Some(clip) = self.clip.as_mut() else { return };
        clip.at = share * total;
        let due = clip.last_swap.map_or(true, |t| t.elapsed() >= SWAP_SPACING);
        if exact || due { self.show_playhead_frame(); }
        self.repaint();
    }

    /// Put the frame under the playhead on screen. A failed seek keeps the
    /// old frame: a stale picture beats ending the session mid-drag.
    fn show_playhead_frame(&mut self) {
        let Some(clip) = self.clip.as_mut() else { return };
        let Some(total) = clip.duration else { return };
        clip.last_swap = Some(Instant::now());
        let t = extract_time(clip.at, total);
        if t == clip.frame_t { return; }
        match crate::tray_loop::extract_frame_at(&clip.path, t) {
            Ok(frame) => {
                clip.frame_t = t;
                self.shot = frame;
                // The dimmed picture was made from the old frame.
                self.backdrop.invalidate();
            }
            Err(e) => eprintln!("framefang: the playhead's frame could not be read, keeping the last one: {e}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use framefang_core::annotation::{Point2, Stroke};

    fn frame(w: u32, h: u32, px: [u8; 4]) -> Image { ImageBuffer::from_pixel(w, h, Rgba(px)) }

    fn line(stroke: Stroke, from: (f32, f32), to: (f32, f32), marker: Option<u8>) -> Annotation {
        let start = Point2::new(from.0, from.1);
        let mut a = match marker {
            Some(alpha) => Annotation::marker(stroke, start, alpha),
            None => Annotation::pen(stroke, start),
        };
        a.extend(Point2::new(to.0, to.1));
        a
    }

    #[test]
    fn untouched_overlay_pixels_stay_clear() {
        let (mut live, mut out) = (frame(32, 32, [50, 60, 70, 255]), frame(32, 32, [0; 4]));
        let red = Stroke { color: Color::new_opaque(255, 0, 0), thickness: 1.0 };
        let pen = line(red, (4.0, 4.0), (10.0, 4.0), None);
        painter::render_annotation(&mut MirrorSurface { frame: &mut live, out: &mut out }, &pen, None);
        assert_eq!(out.get_pixel(4, 4).0[3], 255, "inked pixels are opaque");
        // Past the round, anti-aliased start the pixel is pure stroke.
        assert_eq!(out.get_pixel(7, 4).0[0], 255);
        assert_eq!(out.get_pixel(20, 20).0, [0; 4], "the clip shows through the rest");
    }

    #[test]
    fn a_marker_blends_with_the_frame_not_with_transparency() {
        let (mut live, mut out) = (frame(32, 32, [200, 200, 200, 255]), frame(32, 32, [0; 4]));
        let yellow = Stroke { color: Color::new_opaque(255, 255, 0), thickness: 4.0 };
        let marker = line(yellow, (8.0, 8.0), (16.0, 8.0), Some(128));
        painter::render_annotation(&mut MirrorSurface { frame: &mut live, out: &mut out }, &marker, None);
        let p = out.get_pixel(10, 8).0;
        assert_eq!(p[3], 255);
        // Half yellow over light gray is about (227, 227, 100); blended over
        // nothing it would be a dark (127, 127, 0).
        assert!(p[0] > 180 && p[1] > 180, "{p:?}");
        assert!(p[2] < 160, "{p:?}");
    }

    #[test]
    fn a_window_includes_its_start_and_not_its_end() {
        let mut a = Annotation::pen(Stroke::default(), Point2::new(0.0, 0.0));
        assert!(visible_at(&a, 0.0) && visible_at(&a, 99.0), "no window: the whole clip");
        a.time = Some((1.0, 4.0));
        assert!(!visible_at(&a, 0.999));
        assert!(visible_at(&a, 1.0) && visible_at(&a, 3.999));
        assert!(!visible_at(&a, 4.0));
        a.time = Some((2.0, f32::INFINITY));
        assert!(visible_at(&a, 9999.0), "to the clip's end");
    }

    #[test]
    fn only_neighbours_with_one_window_share_an_overlay() {
        let at = Point2::new(0.0, 0.0);
        let whole = || Annotation::pen(Stroke::default(), at);
        let mut timed = whole();
        timed.time = Some((1.0, 4.0));
        let ink = [whole(), whole(), timed, whole()];
        let runs = group_by_window(&ink);
        let shape: Vec<_> = runs.iter().map(|(w, run)| (*w, run.len())).collect();
        // The last whole-clip stroke is a run of its own; folded into the
        // first, the timed one would burn over it.
        assert_eq!(shape, [(None, 2), (Some((1.0, 4.0)), 1), (None, 1)]);
    }

    #[test]
    fn ink_stamped_at_the_clips_end_stays_inside_it() {
        // The playhead dragged fully right sits on the duration itself.
        assert_eq!(stamp_window(10.0, 10.0, None), Some((10.0 - 0.05, f32::INFINITY)));
        assert_eq!(stamp_window(10.0, 10.0, Some(3.0)), Some((10.0 - 0.05, f32::INFINITY)), "a preset past the end is open");
    }

    #[test]
    fn an_untouched_session_stamps_no_window() {
        assert_eq!(stamp_window(0.0, 10.0, None), None);
        assert_eq!(stamp_window(0.0, 10.0, Some(3.0)), Some((0.0, 3.0)));
        assert_eq!(stamp_window(2.0, 10.0, Some(3.0)), Some((2.0, 5.0)));
        assert_eq!(stamp_window(2.0, 10.0, None), Some((2.0, f32::INFINITY)));
    }

    #[test]
    fn the_clock_reads_minutes_and_seconds() {
        assert_eq!(timecode(0.0), "0:00");
        assert_eq!(timecode(12.34), "0:12");
        assert_eq!(timecode(75.0), "1:15");
        assert_eq!(timecode(3601.0), "60:01");
        assert_eq!(timecode(-3.0), "0:00");
    }

    #[test]
    fn the_track_runs_between_the_clock_and_the_chip() {
        for screen in [(0, 0, 1920, 1080), (0, 0, 300, 200), (-1920, 0, 1920, 1080)] {
            let bar = timeline_bar_rect(screen);
            let chip = timeline_chip_rect(bar);
            let track = timeline_track(bar, 125.0);
            assert!(chip.0 >= bar.0 && chip.0 + chip.2 <= bar.0 + bar.2, "{screen:?}");
            assert!(track.0 > bar.0 + BAR_PAD && track.2 >= 40, "{screen:?}");
            if bar.2 >= 400 { assert!(track.0 + track.2 < chip.0, "{screen:?}"); }
        }
        assert_eq!(timeline_bar_rect((0, 0, 1920, 1080)).2, BAR_MAX_W);
        assert_eq!(DURATIONS[0], (None, "End"));
    }
}
