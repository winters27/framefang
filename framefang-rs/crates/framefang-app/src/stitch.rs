//! Scrolling capture's stitcher: frames of one area taken between scrolls,
//! joined into one tall picture.
//!
//! Each new frame is matched to the one before by its rows: a row is a
//! fingerprint of its pixels, the right edge left out (a scrollbar's thumb
//! moves as the page does). A band at the top or bottom that stays the same
//! in both frames (a sticky header, a footer, a toolbar) is not content: the
//! scroll is found in the rows between, as the offset where the most rows of
//! the new frame line up with rows of the last, and only the rows that are
//! new are added. The header is kept once, at the top, and the footer once,
//! at the bottom.
//!
//! The page can be scrolled either way. The stitcher keeps where each frame
//! sits on the page, so a frame that goes back over rows it already has adds
//! nothing, and one that goes past the top adds its new rows above.

use std::collections::VecDeque;

use image::RgbaImage;

/// The right edge left out of matching: a scrollbar is narrower.
pub const IGNORE_RIGHT: u32 = 24;

/// What one new frame added.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// The view moved by this many rows; that many new rows were added.
    Moved(u32),
    /// The same picture as before: the end of the page, or it didn't scroll.
    Same,
    /// No offset lines the frames up: not a plain scroll (a page that
    /// redraws or animates). Nothing was added.
    NoMatch,
}

/// A stitch in progress.
///
/// Rows are placed on the page by where the first frame sat: its row 0 is
/// page row 0, and rows above it are negative.
pub struct Stitcher {
    width: u32,
    height: u32,
    /// The content known so far, top to bottom, without the header and
    /// footer bands. It starts as the whole first frame; the bands come off
    /// it as they are found.
    content: VecDeque<u8>,
    /// Each content row's fingerprint, to find a frame anywhere in it.
    hashes: VecDeque<u64>,
    /// The last frame matched, and the page row of its row 0.
    last: RgbaImage,
    pos: i64,
    /// The frames that gave the content's first and last rows, with their
    /// page rows. The header comes from the top one, the footer from the
    /// bottom one, and the content always starts at the top one's band and
    /// ends at the bottom one's.
    top_frame: (RgbaImage, i64),
    bottom_frame: (RgbaImage, i64),
    /// The sticky bands found so far (rows), grown as frames show them.
    top: u32,
    bottom: u32,
    /// Which of the right edge's columns changed between matched rows: a
    /// scrollbar's thumb, cut off the finished picture.
    right_moves: [bool; IGNORE_RIGHT as usize],
}

impl Stitcher {
    pub fn new(first: RgbaImage) -> Stitcher {
        let (w, h) = first.dimensions();
        Stitcher { content: first.as_raw().iter().copied().collect(), hashes: rows(&first).into(),
                   width: w, height: h, last: first.clone(),
                   pos: 0, top_frame: (first.clone(), 0), bottom_frame: (first, 0), top: 0, bottom: 0,
                   right_moves: [false; IGNORE_RIGHT as usize] }
    }

    fn row_bytes(&self) -> usize { 4 * self.width as usize }

    /// How tall the stitched picture is now, header and footer included.
    pub fn height(&self) -> u32 { self.top + (self.content.len() / self.row_bytes()) as u32 + self.bottom }

    /// Add the next frame (same size as the first), scrolled either way.
    pub fn push(&mut self, next: RgbaImage) -> Step {
        if next.dimensions() != (self.width, self.height) { return Step::NoMatch; }
        let a = rows(&self.last);
        let b = rows(&next);
        if a == b { return Step::Same; }
        let h = self.height as usize;
        // Bands that stay put: rows equal at the same place in both frames.
        // A band once found stays one (a hovered button in a header doesn't
        // turn it into content).
        let top = a.iter().zip(&b).take_while(|(x, y)| x == y).count().min(h / 3).max(self.top as usize);
        let bottom = a.iter().rev().zip(b.iter().rev()).take_while(|(x, y)| x == y).count()
            .min(h / 3).max(self.bottom as usize);
        let band = top..h - bottom;
        // How far the new frame sits below the last (above it is negative):
        // whichever way lines up better, and the smaller move if both do.
        let down = best_offset(&a[band.clone()], &b[band.clone()]).map(|(d, s)| (d as i64, s));
        let up = best_offset(&b[band.clone()], &a[band.clone()]).map(|(d, s)| (-(d as i64), s));
        let off = match (down, up) {
            (Some(d), Some(u)) => {
                let better = if (d.1 - u.1).abs() > 1e-6 { d.1 > u.1 } else { d.0.abs() <= u.0.abs() };
                Some(if better { d.0 } else { u.0 })
            }
            (d, u) => d.or(u).map(|(o, _)| o),
        };
        // Too far from the last frame (a scroll too fast to follow, then
        // back): it may still be somewhere in what is known. The bands stay
        // as they were. If not, the last frame stays the one to match: its
        // place is known.
        let (top, bottom, off) = match off {
            Some(off) => (top, bottom, off),
            None => match self.locate(&b) {
                Some(off) => (self.top as usize, self.bottom as usize, off),
                None => return Step::NoMatch,
            },
        };
        let band = top..h - bottom;
        // Rows newly found to be a band come off the content's ends: they
        // are the top frame's first rows and the bottom frame's last, and
        // that frame's band now gives them once.
        let row_bytes = self.row_bytes();
        let (newly_header, newly_footer) = (top - self.top as usize, bottom - self.bottom as usize);
        self.content.drain(..(newly_header * row_bytes).min(self.content.len()));
        self.content.truncate(self.content.len().saturating_sub(newly_footer * row_bytes));
        self.hashes.drain(..newly_header.min(self.hashes.len()));
        self.hashes.truncate(self.hashes.len().saturating_sub(newly_footer));
        self.top = top as u32;
        self.bottom = bottom as u32;
        // Rows that line up but differ at the right edge: a scrollbar there.
        if self.width > IGNORE_RIGHT {
            let x0 = self.width - IGNORE_RIGHT;
            for y in band.clone().step_by(3) {
                let in_last = y as i64 + off;
                if in_last < band.start as i64 || in_last >= band.end as i64 { continue; }
                for c in 0..IGNORE_RIGHT {
                    if next.get_pixel(x0 + c, y as u32) != self.last.get_pixel(x0 + c, in_last as u32) {
                        self.right_moves[c as usize] = true;
                    }
                }
            }
        }
        // The new frame's content band on the page, and what is known.
        let pos = self.pos + off;
        let (band_lo, band_hi) = (pos + top as i64, pos + band.end as i64);
        let known_lo = self.top_frame.1 + top as i64;
        let known_hi = self.bottom_frame.1 + band.end as i64;
        let row = |page: i64| (page - pos) as usize;
        let raw = next.as_raw();
        if band_hi > known_hi {
            let new = row(known_hi.max(band_lo))..row(band_hi);
            self.content.extend(&raw[new.start * row_bytes..new.end * row_bytes]);
            self.hashes.extend(&b[new]);
            self.bottom_frame = (next.clone(), pos);
        }
        if band_lo < known_lo {
            let new = row(band_lo)..row(known_lo.min(band_hi));
            for &byte in raw[new.start * row_bytes..new.end * row_bytes].iter().rev() { self.content.push_front(byte); }
            for &fp in b[new].iter().rev() { self.hashes.push_front(fp); }
            self.top_frame = (next.clone(), pos);
        }
        self.pos = pos;
        self.last = next;
        Step::Moved(off.unsigned_abs() as u32)
    }

    /// Where a frame (`b`, its rows' fingerprints) sits in the known
    /// content, as its offset from the last frame: its band must overlap
    /// the content by [`min_overlap`] and nine in ten of those rows agree.
    /// Among places that agree as well, the one nearest the last frame wins.
    fn locate(&self, b: &[u64]) -> Option<i64> {
        let band = self.top as usize..self.height as usize - self.bottom as usize;
        let (n, m) = (band.len() as i64, self.hashes.len() as i64);
        if n < 8 || m < 8 { return None; }
        let need = min_overlap(n as usize) as i64;
        let known_lo = self.top_frame.1 + self.top as i64;
        // Where the last frame's band starts in the content.
        let here = self.pos + self.top as i64 - known_lo;
        let mut best: Option<(i64, f32)> = None;
        for q in need - n..=m - need {
            let (mut agree, mut telling) = (0usize, 0usize);
            for i in (-q).max(0)..n.min(m - q) {
                let y = band.start + i as usize;
                if y > band.start && b[y] == b[y - 1] { continue; }
                telling += 1;
                if self.hashes[(q + i) as usize] == b[y] { agree += 1; }
            }
            if telling < 3 { continue; }
            let score = agree as f32 / telling as f32;
            if score < 0.9 { continue; }
            let better = match best {
                None => true,
                Some((bq, bs)) => score > bs + 1e-6 || ((score - bs).abs() <= 1e-6 && (q - here).abs() < (bq - here).abs()),
            };
            if better { best = Some((q, score)); }
        }
        best.map(|(q, _)| q - here)
    }

    /// The stitched picture: the header from the top frame, the content,
    /// then the footer from the bottom frame.
    pub fn finish(self) -> RgbaImage {
        let row_bytes = self.row_bytes();
        let h = self.height as usize;
        let (head, foot) = (self.top as usize, self.bottom as usize);
        let mut out = Vec::with_capacity((head + foot) * row_bytes + self.content.len());
        out.extend_from_slice(&self.top_frame.0.as_raw()[..head * row_bytes]);
        let (front, back) = self.content.as_slices();
        out.extend_from_slice(front);
        out.extend_from_slice(back);
        out.extend_from_slice(&self.bottom_frame.0.as_raw()[(h - foot) * row_bytes..h * row_bytes]);
        let rows = (out.len() / row_bytes) as u32;
        let img = RgbaImage::from_raw(self.width, rows, out).expect("whole rows");
        // A scrollbar's thumb stitched down the edge is no part of the page.
        match self.right_moves.iter().position(|m| *m) {
            Some(c) => image::imageops::crop_imm(&img, 0, 0, self.width - IGNORE_RIGHT + c as u32, rows).to_image(),
            None => img,
        }
    }
}

/// Each row's fingerprint, the right edge left out.
fn rows(img: &RgbaImage) -> Vec<u64> {
    let (w, h) = img.dimensions();
    let keep = w.saturating_sub(IGNORE_RIGHT).max(1) as usize;
    let raw = img.as_raw();
    (0..h as usize).map(|y| {
        let row = &raw[y * w as usize * 4..(y * w as usize + keep) * 4];
        // FNV-1a over the row's bytes.
        row.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, &b| (h ^ b as u64).wrapping_mul(0x100_0000_01b3))
    }).collect()
}

/// How many rows of a band of `n` two frames must share to be lined up: a
/// fifth, so a quick scroll (Notepad jumps lines at a time, with no smooth
/// scrolling) still overlaps.
fn min_overlap(n: usize) -> usize { (n / 5).max(4) }

/// How far `b` scrolled past `a` (rows of the same band): the offset where
/// the most of `b`'s rows equal `a`'s rows that far below. Needs
/// [`min_overlap`] and nine in ten overlapping rows to agree; among offsets
/// that agree as well, the smallest wins (a page of repeating rows moved the
/// least it could have). The offset and how well it agrees.
fn best_offset(a: &[u64], b: &[u64]) -> Option<(usize, f32)> {
    let n = a.len();
    if n < 8 { return None; }
    let mut best: Option<(usize, f32)> = None;
    for dy in 1..=n - min_overlap(n) {
        let overlap = n - dy;
        // Uniform rows (blank space) say nothing about the offset.
        let mut agree = 0usize;
        let mut telling = 0usize;
        for y in 0..overlap {
            let distinct = y == 0 || b[y] != b[y - 1];
            if !distinct { continue; }
            telling += 1;
            if b[y] == a[y + dy] { agree += 1; }
        }
        if telling < 3 { continue; }
        let score = agree as f32 / telling as f32;
        if score >= 0.9 && best.is_none_or(|(_, s)| score > s + 1e-6) {
            best = Some((dy, score));
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tall "page": rows of varied stripes, so every row is distinct (no
    /// repeat a long scroll could line up with).
    fn page(w: u32, h: u32) -> RgbaImage {
        RgbaImage::from_fn(w, h, |x, y| {
            let r = y.wrapping_mul(0x9E37_79B9).rotate_left(13);
            let v = (r.wrapping_add((x / 7).wrapping_mul(0x045D_9F3B)) >> 24) as u8;
            image::Rgba([v, v.wrapping_mul(3), 255 - v, 255])
        })
    }

    /// The view of `page` scrolled to `top`, `h` tall, with a fixed header
    /// and footer of `band` rows, and a scrollbar column that changes.
    fn view(page: &RgbaImage, top: u32, h: u32, band: u32) -> RgbaImage {
        let w = page.width();
        RgbaImage::from_fn(w, h, |x, y| {
            if x >= w - 10 { return image::Rgba([top as u8, 0, 0, 255]); }
            if y < band { return image::Rgba([20, 20, 20, 255]); }
            if y >= h - band { return image::Rgba([60, 60, 60, 255]); }
            *page.get_pixel(x, top + y - band)
        })
    }

    #[test]
    fn a_scrolled_page_stitches_back_into_the_page_with_one_header_and_footer() {
        let p = page(120, 600);
        let (h, band) = (200, 20);
        let mut s = Stitcher::new(view(&p, 0, h, band));
        let content = h - 2 * band;
        let mut top = 0;
        // Each step at most two thirds of the view (the engine scrolls less
        // than that), the last a short one at the end of the page.
        for step in [70, 70, 100, 90, 100, 100] {
            let next = (top + step).min(p.height() - content);
            let moved = next - top;
            top = next;
            assert_eq!(s.push(view(&p, top, h, band)), Step::Moved(moved), "at {top}");
        }
        assert_eq!(s.push(view(&p, top, h, band)), Step::Same, "the end");
        let out = s.finish();
        assert_eq!(out.height(), band + (top + content) + band, "header, all the content seen, footer");
        assert_eq!(out.width(), p.width() - 10, "the moving scrollbar column is cut off");
        // The content matches the page, row for row (the scrollbar aside).
        for y in 0..top + content {
            for x in [0, 50, 100] {
                assert_eq!(out.get_pixel(x, band + y), p.get_pixel(x, y), "row {y}");
            }
        }
        assert_eq!(out.get_pixel(5, 0)[0], 20, "header on top");
        assert_eq!(out.get_pixel(5, out.height() - 1)[0], 60, "footer at the bottom");
    }

    /// Spike: scroll the window under `FF_SCROLL_RECT` ("x,y,w,h", desktop
    /// pixels) one wheel notch at a time, stitching what shows, and save the
    /// result to `FF_SCROLL_OUT`. Prints each step.
    #[cfg(windows)]
    #[test]
    #[ignore]
    fn spike_scroll_a_real_window() {
        use windows::Win32::UI::Input::KeyboardAndMouse::{SendInput, INPUT, INPUT_0, INPUT_MOUSE, MOUSEINPUT, MOUSEEVENTF_WHEEL};
        use windows::Win32::UI::WindowsAndMessaging::SetCursorPos;
        let r: Vec<i32> = std::env::var("FF_SCROLL_RECT").unwrap().split(',').map(|v| v.trim().parse().unwrap()).collect();
        let out = std::env::var("FF_SCROLL_OUT").unwrap();
        let grab = || framefang_platform::capture::capture_area(r[0], r[1], r[2] as u32, r[3] as u32).unwrap();
        // After a scroll, the picture once it has stopped moving (smooth
        // scrolling animates): two grabs in a row the same, or 600 ms.
        let settled = |prev: &RgbaImage| {
            let until = std::time::Instant::now() + std::time::Duration::from_millis(600);
            let mut last = grab();
            let mut moved = last != *prev;
            loop {
                std::thread::sleep(std::time::Duration::from_millis(35));
                let now = grab();
                if now == last && (moved || std::time::Instant::now() >= until) { return now; }
                if now != *prev { moved = true; }
                last = now;
                if std::time::Instant::now() >= until { return last; }
            }
        };
        let wheel = |delta: i32| unsafe {
            let input = INPUT { r#type: INPUT_MOUSE, Anonymous: INPUT_0 { mi: MOUSEINPUT {
                dx: 0, dy: 0, mouseData: delta as u32, dwFlags: MOUSEEVENTF_WHEEL, time: 0, dwExtraInfo: 0 } } };
            SendInput(&[input], std::mem::size_of::<INPUT>() as i32);
        };
        unsafe { let _ = SetCursorPos(r[0] + r[2] - 40, r[1] + r[3] / 2); }
        std::thread::sleep(std::time::Duration::from_millis(300));
        let t0 = std::time::Instant::now();
        let mut prev = grab();
        let mut s = Stitcher::new(prev.clone());
        let (mut same, mut notches, mut per_notch) = (0, 1, 0u32);
        for i in 0..80 {
            // One event per notch: some windows ignore a bigger delta.
            for _ in 0..notches { wheel(-120); }
            let next = settled(&prev);
            let step = s.push(next.clone());
            prev = next;
            println!("step {i}: {notches} notch(es) {step:?} (height {})", s.height());
            if step == Step::Same {
                let shot = framefang_platform::capture::capture_all_screens().unwrap();
                let (ox, oy) = shot.geometry().origin();
                let full = crate::editor::crop(&shot.bitmap, (r[0] - ox, r[1] - oy, r[2], r[3]));
                println!("  area grab == whole-screen grab: {}, == previous: {}", full == prev, full == grab());
            }
            match step {
                Step::Same => { same += 1; if same >= 2 { break; } }
                Step::Moved(d) => {
                    same = 0;
                    // Learn one notch's size, then move about half a view a step.
                    if per_notch == 0 { per_notch = (d / notches as u32).max(1); }
                    if std::env::var("FF_SCROLL_FIXED").is_err() {
                        notches = ((r[3] as u32 / 2) / per_notch).max(1) as i32;
                    }
                }
                Step::NoMatch => { same = 0; notches = 1; }
            }
        }
        let img = s.finish();
        println!("done in {:.1}s: {}x{}", t0.elapsed().as_secs_f32(), img.width(), img.height());
        img.save(&out).unwrap();
    }

    #[test]
    fn a_page_that_redraws_is_not_stitched() {
        let mut s = Stitcher::new(page(100, 120));
        let other = RgbaImage::from_fn(100, 120, |x, y| image::Rgba([(x * y % 256) as u8, 9, 9, 255]));
        assert_eq!(s.push(other), Step::NoMatch);
        assert_eq!(s.finish().height(), 120, "nothing added");
    }

    /// The content rows of a stitched picture match the page from `from`.
    fn assert_is_page(out: &RgbaImage, p: &RgbaImage, band: u32, from: u32, rows: u32) {
        assert_eq!(out.height(), band + rows + band, "header, {rows} rows of content, footer");
        for y in 0..rows {
            for x in [0, 50, 100] {
                assert_eq!(out.get_pixel(x, band + y), p.get_pixel(x, from + y), "row {y}");
            }
        }
        assert_eq!(out.get_pixel(5, 0)[0], 20, "header on top");
        assert_eq!(out.get_pixel(5, out.height() - 1)[0], 60, "footer at the bottom");
    }

    #[test]
    fn a_page_started_in_the_middle_stitches_down_then_back_up_past_where_it_began() {
        let p = page(120, 900);
        let (h, band) = (200, 20);
        let content = h - 2 * band;
        let end = p.height() - content;
        let mut top = 400;
        let mut s = Stitcher::new(view(&p, top, h, band));
        // Down to the end.
        while top < end {
            let next = (top + 60).min(end);
            assert_eq!(s.push(view(&p, next, h, band)), Step::Moved(next - top));
            top = next;
        }
        let tall = s.height();
        // Back up over what it has (nothing added), then on to the top.
        while top > 0 {
            let next = top.saturating_sub(70);
            assert_eq!(s.push(view(&p, next, h, band)), Step::Moved(top - next), "up to {next}");
            if next >= 400 { assert_eq!(s.height(), tall, "rows it already had add nothing"); }
            top = next;
        }
        assert_eq!(s.push(view(&p, 0, h, band)), Step::Same, "the top");
        let out = s.finish();
        assert_is_page(&out, &p, band, 0, p.height());
        assert_eq!(out.width(), p.width() - 10, "the moving scrollbar column is cut off");
    }

    #[test]
    fn a_flick_too_far_is_skipped_and_scrolling_back_goes_on_from_where_it_was() {
        let p = page(120, 900);
        let (h, band) = (200, 20);
        let mut s = Stitcher::new(view(&p, 0, h, band));
        assert_eq!(s.push(view(&p, 80, h, band)), Step::Moved(80));
        // Flicked past: nothing of it overlaps the last frame.
        assert_eq!(s.push(view(&p, 500, h, band)), Step::NoMatch);
        // Scrolled back near where it was, it goes on from there.
        assert_eq!(s.push(view(&p, 140, h, band)), Step::Moved(60));
        assert_is_page(&s.finish(), &p, band, 0, 140 + h - 2 * band);
    }

    #[test]
    fn after_a_flick_scrolling_back_anywhere_it_has_picks_up_again() {
        let p = page(120, 900);
        let (h, band) = (200, 20);
        let mut s = Stitcher::new(view(&p, 0, h, band));
        assert_eq!(s.push(view(&p, 80, h, band)), Step::Moved(80));
        assert_eq!(s.push(view(&p, 160, h, band)), Step::Moved(80));
        assert_eq!(s.push(view(&p, 600, h, band)), Step::NoMatch, "flicked past");
        // Back near the top: it no longer overlaps the last frame enough,
        // but it is in what was stitched.
        assert_eq!(s.push(view(&p, 20, h, band)), Step::Moved(140));
        let tall = s.height();
        assert_eq!(s.push(view(&p, 100, h, band)), Step::Moved(80));
        assert_eq!(s.height(), tall, "rows it had add nothing");
        assert_eq!(s.push(view(&p, 210, h, band)), Step::Moved(110));
        assert_is_page(&s.finish(), &p, band, 0, 210 + h - 2 * band);
    }

    #[test]
    fn a_page_started_at_its_end_stitches_upward() {
        // A chat: it opens at its last message.
        let p = page(120, 700);
        let (h, band) = (200, 20);
        let content = h - 2 * band;
        let mut top = p.height() - content;
        let mut s = Stitcher::new(view(&p, top, h, band));
        assert_eq!(s.push(view(&p, top, h, band)), Step::Same, "nothing below");
        while top > 100 {
            let next = top - 90;
            assert_eq!(s.push(view(&p, next, h, band)), Step::Moved(90));
            top = next;
        }
        // Stopped partway (Esc): from there to the end.
        assert_is_page(&s.finish(), &p, band, top, p.height() - top);
    }
}
