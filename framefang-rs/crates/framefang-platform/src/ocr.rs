//! Reading the text in a picture, with what the system already has: no
//! model is downloaded and nothing leaves the computer.
//!
//! - Windows: `Windows.Media.Ocr`, in the languages the user has for it.
//! - macOS: Vision's text recognizer.
//! - Linux: the `tesseract` program, when it is installed.
//!
//! Small text reads far better at twice its size (a Windows test read
//! "1.62" for "1.6.2" at 1x and every character right at 2x), so pictures
//! are enlarged first where that stays within the engine's limit. On
//! Windows a small picture is also read as several enlarged masks and the
//! lines the readings agree on are kept (`passes`, `merge_passes`): game
//! text a few pixels tall read one line in two that way (2026-11-19).

use image::RgbaImage;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OcrError {
    /// The system has no language to read with; the text says how to add one.
    NoLanguage(String),
    /// There is no OCR on this system; the text says how to get it.
    Missing(String),
    Failed(String),
}

impl std::fmt::Display for OcrError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OcrError::NoLanguage(s) | OcrError::Missing(s) | OcrError::Failed(s) => f.write_str(s),
        }
    }
}

/// The size a picture is read at: twice its own when that fits within
/// `limit` on both sides and stays under 4 megapixels, else scaled down to
/// fit `limit` (a 1:1 picture that already fits is read as it is).
pub fn read_size(w: u32, h: u32, limit: u32) -> (u32, u32) {
    let (w, h) = (w.max(1), h.max(1));
    if 2 * w.max(h) <= limit && (w as u64) * (h as u64) <= 4_000_000 { return (w * 2, h * 2); }
    if w.max(h) <= limit { return (w, h); }
    let k = limit as f64 / w.max(h) as f64;
    (((w as f64 * k).round() as u32).max(1), ((h as f64 * k).round() as u32).max(1))
}

fn prepared(img: &RgbaImage, limit: u32) -> RgbaImage {
    let (w, h) = read_size(img.width(), img.height(), limit);
    if (w, h) == img.dimensions() { return img.clone(); }
    image::imageops::resize(img, w, h, image::imageops::FilterType::Triangle)
}

/// The text in `img`, one line per line read, or `Ok("")` when there is
/// none. Blocking: call it off the event loop.
pub fn read_text(img: &RgbaImage) -> Result<String, OcrError> {
    imp::read_text(img)
}

#[cfg(not(any(windows, unix)))]
mod imp {
    use super::*;
    pub fn read_text(_: &RgbaImage) -> Result<String, OcrError> { Err(OcrError::Missing("No text reading here.".into())) }
    pub fn words(_: &RgbaImage) -> Result<Vec<WordBox>, OcrError> { Err(OcrError::Missing("No text reading here.".into())) }
}

/// A word read: its text, where it is in the picture's own pixels (x, y,
/// width, height), and its line (lines numbered in reading order).
#[derive(Debug, Clone, PartialEq)]
pub struct WordBox {
    pub text: String,
    pub rect: (f32, f32, f32, f32),
    pub line: usize,
}

/// Strips a tall picture is read in, and how much they overlap: a scrolling
/// capture tens of thousands of pixels tall would otherwise be shrunk to
/// the engine's limit and its text lost.
const STRIP: u32 = 1200;
const STRIP_OVERLAP: u32 = 120;

/// The strips of a picture `h` tall: (top, height, the part of it whose
/// words are kept). Each word is kept by the one strip its middle is in.
pub fn strips(h: u32) -> Vec<(u32, u32, (f32, f32))> {
    if h <= STRIP { return vec![(0, h, (0.0, h as f32))]; }
    let step = STRIP - STRIP_OVERLAP;
    let mut out = Vec::new();
    let mut top = 0;
    loop {
        let height = STRIP.min(h - top);
        let last = top + height >= h;
        let keep_from = if top == 0 { 0.0 } else { (top + STRIP_OVERLAP / 2) as f32 };
        let keep_to = if last { h as f32 } else { (top + step + STRIP_OVERLAP / 2) as f32 };
        out.push((top, height, (keep_from, keep_to)));
        if last { break; }
        top += step;
    }
    out
}

/// Every word in `img` and where it is, in reading order. Blocking: call it
/// off the event loop. A tall picture is read in overlapping strips.
pub fn read_words(img: &RgbaImage) -> Result<Vec<WordBox>, OcrError> {
    // The strips are read side by side, a few at a time (a 4700 px page
    // took 16 s one after another, 2026-11-20).
    let parts = strips(img.height());
    let mut read: Vec<Result<Vec<WordBox>, OcrError>> = Vec::with_capacity(parts.len());
    for batch in parts.chunks(4) {
        let got: Vec<_> = std::thread::scope(|s| {
            let handles: Vec<_> = batch.iter().map(|(top, height, _)| {
                let strip = image::imageops::crop_imm(img, 0, *top, img.width(), *height).to_image();
                s.spawn(move || imp::words(&strip))
            }).collect();
            handles.into_iter()
                .map(|h| h.join().unwrap_or_else(|_| Err(OcrError::Failed("text reading stopped".into()))))
                .collect()
        });
        read.extend(got);
    }
    let mut out = Vec::new();
    let mut lines = 0;
    for ((top, _, (keep_from, keep_to)), words) in parts.into_iter().zip(read) {
        let words = words?;
        let mut last = 0;
        for mut w in words {
            w.rect.1 += top as f32;
            let mid = w.rect.1 + w.rect.3 / 2.0;
            if mid < keep_from || mid >= keep_to || w.text.trim().is_empty() { continue; }
            last = last.max(w.line + 1);
            w.line += lines;
            out.push(w);
        }
        lines += last;
    }
    Ok(out)
}

/// Words of a line whose box is known only as a whole, each given the share
/// of it its characters take (spaces included).
// Only the macOS reader calls it (the tests run it everywhere).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn split_line(text: &str, (x, y, w, h): (f32, f32, f32, f32), line: usize) -> Vec<WordBox> {
    let n = text.chars().count().max(1) as f32;
    let mut out = Vec::new();
    let mut at = 0usize;
    for part in text.split(' ') {
        let len = part.chars().count();
        if !part.trim().is_empty() {
            out.push(WordBox { text: part.to_owned(), rect: (x + w * at as f32 / n, y, w * len as f32 / n, h), line });
        }
        at += len + 1;
    }
    out
}

/// One line read, and where it is (x, y, width, height).
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) struct Line {
    pub text: String,
    pub rect: (f32, f32, f32, f32),
}

/// A picture this side or smaller (in its own pixels) is read several ways.
const MULTI_PASS_SIDE: u32 = 700;

/// The ways to read `img`: each picture with how much it was enlarged and
/// how much margin was added (to map lines back). A big picture is read
/// once, as `prepared` makes it. A small one (UI and game text: a few pixels
/// tall, often light with a dark outline on a busy background, where Windows
/// reads differently every way it is shown; Brandon 2026-11-19) also
/// enlarged up to 4x as black-on-white masks: bright text two ways, dark
/// text, and text brighter than its surroundings, each with a white margin.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn passes(img: &RgbaImage, limit: u32) -> Vec<(RgbaImage, f32, u32)> {
    let (w, h) = (img.width().max(1), img.height().max(1));
    let base = prepared(img, limit);
    let base_scale = base.width() as f32 / w as f32;
    let side = w.max(h);
    let k = ((limit.saturating_sub(80)) / side).min(4);
    if side > MULTI_PASS_SIDE || k < 2 { return vec![(base, base_scale, 0)]; }
    let big = image::imageops::resize(img, w * k, h * k, image::imageops::FilterType::Lanczos3);
    let luma: Vec<u32> = big.pixels()
        .map(|p| (p[0] as u32 * 299 + p[1] as u32 * 587 + p[2] as u32 * 114) / 1000).collect();
    let pad = 10 * k;
    let mask = |ink: &dyn Fn(usize) -> bool| {
        let mut o = RgbaImage::from_pixel(big.width() + 2 * pad, big.height() + 2 * pad, image::Rgba([255, 255, 255, 255]));
        for (i, _) in luma.iter().enumerate() {
            if ink(i) {
                let (x, y) = (i as u32 % big.width(), i as u32 / big.width());
                o.put_pixel(x + pad, y + pad, image::Rgba([0, 0, 0, 255]));
            }
        }
        o
    };
    let local = local_means(&luma, big.width() as usize, big.height() as usize, 5 * k as usize);
    vec![
        (base, base_scale, 0),
        (mask(&|i| luma[i] >= 220), k as f32, pad),
        (mask(&|i| luma[i] >= 150), k as f32, pad),
        (mask(&|i| luma[i] < 110), k as f32, pad),
        (mask(&|i| luma[i] as i64 > local[i] + 30), k as f32, pad),
    ]
}

/// Each pixel's mean brightness over the square `r` around it.
#[cfg_attr(not(windows), allow(dead_code))]
fn local_means(l: &[u32], w: usize, h: usize, r: usize) -> Vec<i64> {
    let mut integ = vec![0i64; (w + 1) * (h + 1)];
    for y in 0..h {
        for x in 0..w {
            let i = (y + 1) * (w + 1) + x + 1;
            integ[i] = l[y * w + x] as i64 + integ[i - 1] + integ[i - w - 1] - integ[i - w - 2];
        }
    }
    let mut out = vec![0i64; w * h];
    for y in 0..h {
        for x in 0..w {
            let (x0, y0, x1, y1) = (x.saturating_sub(r), y.saturating_sub(r), (x + r + 1).min(w), (y + r + 1).min(h));
            let s = integ[y1 * (w + 1) + x1] - integ[y0 * (w + 1) + x1] - integ[y1 * (w + 1) + x0] + integ[y0 * (w + 1) + x0];
            out[y * w + x] = s / ((x1 - x0) * (y1 - y0)) as i64;
        }
    }
    out
}

/// The text from several readings of one picture: lines are matched across
/// readings by where they are; each needs at least two readings to have
/// seen it (a stray mark one way of looking reads as junk), and of a line's
/// readings the one most like the others wins, a cleaner one breaking ties.
/// Top to bottom, then left to right.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn merge_passes(passes: &[Vec<Line>]) -> Vec<String> {
    struct Group { rect: (f32, f32, f32, f32), seen: Vec<(usize, Line)> }
    let overlaps = |a: (f32, f32, f32, f32), b: (f32, f32, f32, f32)| {
        let v = (a.1 + a.3).min(b.1 + b.3) - a.1.max(b.1);
        let hz = (a.0 + a.2).min(b.0 + b.2) - a.0.max(b.0);
        v >= 0.5 * a.3.min(b.3) && hz > 0.0
    };
    let union = |a: (f32, f32, f32, f32), b: (f32, f32, f32, f32)| {
        let (x0, y0) = (a.0.min(b.0), a.1.min(b.1));
        (x0, y0, (a.0 + a.2).max(b.0 + b.2) - x0, (a.1 + a.3).max(b.1 + b.3) - y0)
    };
    let mut groups: Vec<Group> = Vec::new();
    for (p, lines) in passes.iter().enumerate() {
        for l in lines {
            if l.text.trim().is_empty() { continue; }
            match groups.iter_mut().find(|g| overlaps(g.rect, l.rect)) {
                Some(g) => {
                    g.rect = union(g.rect, l.rect);
                    // One reading split the line in two: join its pieces.
                    match g.seen.iter_mut().find(|(q, _)| *q == p) {
                        Some((_, mine)) => {
                            let (a, b) = if l.rect.0 < mine.rect.0 { (l.text.clone(), mine.text.clone()) } else { (mine.text.clone(), l.text.clone()) };
                            mine.text = format!("{} {}", a.trim(), b.trim());
                            mine.rect = union(mine.rect, l.rect);
                        }
                        None => g.seen.push((p, l.clone())),
                    }
                }
                None => groups.push(Group { rect: l.rect, seen: vec![(p, l.clone())] }),
            }
        }
    }
    let need = if passes.len() > 1 { 2 } else { 1 };
    groups.retain(|g| g.seen.len() >= need);
    groups.sort_by(|a, b| (a.rect.1 + a.rect.3 / 2.0).total_cmp(&(b.rect.1 + b.rect.3 / 2.0))
        .then(a.rect.0.total_cmp(&b.rect.0)));
    groups.iter().map(|g| {
        let texts: Vec<String> = g.seen.iter().map(|(_, l)| l.text.trim().to_owned()).collect();
        let score = |i: usize| {
            let agree: f32 = texts.iter().enumerate().filter(|(j, _)| *j != i).map(|(_, o)| likeness(&texts[i], o)).sum();
            // A tie goes to the reading with more letters and digits: a
            // dropped letter is the usual miss.
            let alnum = texts[i].chars().filter(|c| c.is_alphanumeric()).count() as f32;
            agree + 0.5 * cleanness(&texts[i]) + 0.01 * alnum
        };
        let best = (0..texts.len()).max_by(|a, b| score(*a).total_cmp(&score(*b))).unwrap_or(0);
        texts[best].clone()
    }).collect()
}

/// How alike two readings are, 0 to 1, case aside.
#[cfg_attr(not(windows), allow(dead_code))]
fn likeness(a: &str, b: &str) -> f32 {
    let (a, b): (Vec<char>, Vec<char>) = (a.to_uppercase().chars().collect(), b.to_uppercase().chars().collect());
    let n = a.len().max(b.len());
    if n == 0 { return 1.0; }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut cur = vec![i + 1; b.len() + 1];
        for (j, cb) in b.iter().enumerate() {
            cur[j + 1] = (prev[j] + (ca != cb) as usize).min(prev[j + 1] + 1).min(cur[j] + 1);
        }
        prev = cur;
    }
    1.0 - prev[b.len()] as f32 / n as f32
}

/// The share of a reading that looks like ordinary text: letters, digits,
/// spaces and everyday punctuation.
#[cfg_attr(not(windows), allow(dead_code))]
fn cleanness(t: &str) -> f32 {
    let n = t.chars().count();
    if n == 0 { return 0.0; }
    let ok = t.chars().filter(|c| c.is_alphanumeric() || " .,:;-'\"!?()/&+%#@".contains(*c)).count();
    let odd = t.chars().filter(|c| matches!(c, '\'' | '!' | ';' | '?') ).count();
    (ok as f32 - odd as f32 * 0.5) / n as f32
}

#[cfg(windows)]
mod imp {
    use super::*;
    use windows::Graphics::Imaging::{BitmapPixelFormat, SoftwareBitmap};
    use windows::Media::Ocr::OcrEngine;
    use windows::Storage::Streams::DataWriter;

    fn engine() -> Result<OcrEngine, OcrError> {
        if let Ok(e) = OcrEngine::TryCreateFromUserProfileLanguages() { return Ok(e); }
        // None of the user's languages has OCR: any language that does.
        if let Ok(langs) = OcrEngine::AvailableRecognizerLanguages() {
            for lang in langs {
                if let Ok(e) = OcrEngine::TryCreateFromLanguage(&lang) { return Ok(e); }
            }
        }
        Err(OcrError::NoLanguage(
            "Windows has no text-recognition language installed. Add one in Settings > Time & language > \
             Language & region (open a language's options and add Optical character recognition), or run \
             Add-WindowsCapability -Online -Name \"Language.OCR~~~en-US~0.0.1.0\" in an admin PowerShell.".into()))
    }

    /// Small pictures (game and app UI text: a few pixels tall, outlined,
    /// on busy backgrounds) are read several ways and the lines most passes
    /// agree on are kept (`super::merge_passes`); a big picture once.
    pub fn read_text(img: &RgbaImage) -> Result<String, OcrError> {
        let limit = OcrEngine::MaxImageDimension().unwrap_or(2600);
        let engine = engine()?;
        let passes = super::passes(img, limit);
        if passes.len() == 1 {
            let (p, _, _) = &passes[0];
            return Ok(lines(&engine, p)?.into_iter().map(|l| l.text).collect::<Vec<_>>().join("\n"));
        }
        let mut read = Vec::new();
        for (p, scale, pad) in &passes {
            // Back to the picture's own pixels, so every pass lines up.
            let ls = lines(&engine, p)?.into_iter().map(|mut l| {
                l.rect = ((l.rect.0 - *pad as f32) / scale, (l.rect.1 - *pad as f32) / scale,
                          l.rect.2 / scale, l.rect.3 / scale);
                l
            }).collect();
            read.push(ls);
        }
        Ok(super::merge_passes(&read).join("\n"))
    }

    /// What Windows reads in `img` exactly as given, one line per line.
    #[cfg(test)]
    pub fn recognize(img: &RgbaImage) -> Result<String, OcrError> {
        Ok(lines(&engine()?, img)?.into_iter().map(|l| l.text).collect::<Vec<_>>().join("\n"))
    }

    /// Every word Windows reads in `img`, in its own pixels, read once at
    /// the size `prepared` gives (twice its own when it fits).
    pub fn words(img: &RgbaImage) -> Result<Vec<WordBox>, OcrError> {
        let fail = |e: windows::core::Error| OcrError::Failed(format!("Windows OCR: {e}"));
        let limit = OcrEngine::MaxImageDimension().unwrap_or(2600);
        let engine = engine()?;
        let p = super::prepared(img, limit);
        let k = p.width() as f32 / img.width().max(1) as f32;
        let result = engine.RecognizeAsync(&bitmap(&p)?).map_err(fail)?.join().map_err(fail)?;
        let mut out = Vec::new();
        for (line, l) in result.Lines().map_err(fail)?.into_iter().enumerate() {
            for word in l.Words().map_err(fail)? {
                let r = word.BoundingRect().map_err(fail)?;
                let text = word.Text().map_err(fail)?.to_string_lossy();
                out.push(WordBox { text, rect: (r.X / k, r.Y / k, r.Width / k, r.Height / k), line });
            }
        }
        Ok(out)
    }

    fn bitmap(img: &RgbaImage) -> Result<SoftwareBitmap, OcrError> {
        let fail = |e: windows::core::Error| OcrError::Failed(format!("Windows OCR: {e}"));
        let mut bgra = img.as_raw().clone();
        for px in bgra.chunks_exact_mut(4) { px.swap(0, 2); }
        let w = DataWriter::new().map_err(fail)?;
        w.WriteBytes(&bgra).map_err(fail)?;
        let buf = w.DetachBuffer().map_err(fail)?;
        SoftwareBitmap::CreateCopyFromBuffer(&buf, BitmapPixelFormat::Bgra8, img.width() as i32, img.height() as i32)
            .map_err(fail)
    }

    /// Each line Windows reads in `img` as given, with where it is.
    fn lines(engine: &OcrEngine, img: &RgbaImage) -> Result<Vec<Line>, OcrError> {
        let fail = |e: windows::core::Error| OcrError::Failed(format!("Windows OCR: {e}"));
        let result = engine.RecognizeAsync(&bitmap(img)?).map_err(fail)?.join().map_err(fail)?;
        let mut out = Vec::new();
        for line in result.Lines().map_err(fail)? {
            let text = line.Text().map_err(fail)?.to_string_lossy();
            let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
            for word in line.Words().map_err(fail)? {
                let r = word.BoundingRect().map_err(fail)?;
                x0 = x0.min(r.X); y0 = y0.min(r.Y);
                x1 = x1.max(r.X + r.Width); y1 = y1.max(r.Y + r.Height);
            }
            if x1 < x0 { x0 = 0.0; y0 = 0.0; x1 = 0.0; y1 = 0.0; }
            out.push(Line { text, rect: (x0, y0, x1 - x0, y1 - y0) });
        }
        Ok(out)
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use super::*;

    pub fn read_text(img: &RgbaImage) -> Result<String, OcrError> {
        objc2::rc::autoreleasepool(|_| Ok(read(img)?.into_iter().map(|(t, _)| t).collect::<Vec<_>>().join("\n")))
    }

    /// Every word, each line's box shared out among its words (Vision
    /// gives boxes per line here), lines top to bottom.
    pub fn words(img: &RgbaImage) -> Result<Vec<WordBox>, OcrError> {
        let (w, h) = (img.width() as f32, img.height() as f32);
        let mut lines: Vec<(String, (f32, f32, f32, f32))> = objc2::rc::autoreleasepool(|_| read(img))?
            .into_iter()
            .map(|(t, (x, y, bw, bh))| (t, (x * w, (1.0 - y - bh) * h, bw * w, bh * h)))
            .collect();
        lines.sort_by(|a, b| (a.1 .1).total_cmp(&b.1 .1).then((a.1 .0).total_cmp(&b.1 .0)));
        Ok(lines.iter().enumerate().flat_map(|(i, (t, r))| super::split_line(t, *r, i)).collect())
    }

    /// Each line Vision reads, with its box: normalized, origin lower left.
    fn read(img: &RgbaImage) -> Result<Vec<(String, (f32, f32, f32, f32))>, OcrError> {
        use objc2::AnyThread;
        use objc2_core_foundation::CFData;
        use objc2_core_graphics::{
            CGBitmapInfo, CGColorRenderingIntent, CGColorSpace, CGDataProvider, CGImage, CGImageAlphaInfo,
        };
        use objc2_foundation::{NSArray, NSDictionary};
        use objc2_vision::{VNImageRequestHandler, VNRecognizeTextRequest, VNRequest, VNRequestTextRecognitionLevel};

        let failed = |what: &str| OcrError::Failed(format!("macOS text recognition: {what}"));
        let img = prepared(img, 10_000);
        let (w, h) = (img.width() as usize, img.height() as usize);
        let data = CFData::from_bytes(img.as_raw());
        let provider = CGDataProvider::with_cf_data(Some(&data)).ok_or_else(|| failed("no image data"))?;
        let space = CGColorSpace::new_device_rgb().ok_or_else(|| failed("no color space"))?;
        let info = CGBitmapInfo(CGImageAlphaInfo::Last.0);
        let image = unsafe {
            CGImage::new(w, h, 8, 32, 4 * w, Some(&space), info, Some(&provider), std::ptr::null(), false,
                         CGColorRenderingIntent::RenderingIntentDefault)
        }.ok_or_else(|| failed("no image"))?;
        let request = VNRecognizeTextRequest::new();
        request.setRecognitionLevel(VNRequestTextRecognitionLevel::Accurate);
        request.setUsesLanguageCorrection(true);
        // Any language the text is in, where the system can tell (13+);
        // older systems read with Vision's defaults.
        if objc2::available!(macos = 13.0) {
            request.setAutomaticallyDetectsLanguage(true);
        }
        let handler = unsafe {
            VNImageRequestHandler::initWithCGImage_options(VNImageRequestHandler::alloc(), &image, &NSDictionary::new())
        };
        let req: &VNRequest = &request;
        handler.performRequests_error(&NSArray::from_slice(&[req]))
            .map_err(|e| failed(&e.to_string()))?;
        let mut out = Vec::new();
        if let Some(results) = request.results() {
            for obs in results.iter() {
                if let Some(t) = obs.topCandidates(1).firstObject() {
                    let b = unsafe { obs.boundingBox() };
                    out.push((t.string().to_string(),
                              (b.origin.x as f32, b.origin.y as f32, b.size.width as f32, b.size.height as f32)));
                }
            }
        }
        Ok(out)
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
mod imp {
    use super::*;
    use std::io::Write;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    const MISSING: &str = "Reading text needs Tesseract. Install it with your package manager \
                           (for example: sudo apt install tesseract-ocr), then try again.";

    pub fn read_text(img: &RgbaImage) -> Result<String, OcrError> {
        let (text, _) = tesseract(img, &[])?;
        Ok(text.lines().map(str::trim_end).filter(|l| !l.is_empty()).collect::<Vec<_>>().join("\n"))
    }

    /// Every word, from Tesseract's table of them (`tsv`): a row per word
    /// (level 5) with its box, its line known by block, paragraph and line.
    pub fn words(img: &RgbaImage) -> Result<Vec<WordBox>, OcrError> {
        let (tsv, k) = tesseract(img, &["tsv"])?;
        Ok(super::words_from_tsv(&tsv, k))
    }

    /// What Tesseract prints for `img` (read enlarged as `prepared` makes
    /// it), and how much it was enlarged. `extra` picks an output format.
    fn tesseract(img: &RgbaImage, extra: &[&str]) -> Result<(String, f32), OcrError> {
        let langs = Command::new("tesseract").arg("--list-langs").output()
            .map_err(|_| OcrError::Missing(MISSING.into()))?;
        let have = String::from_utf8_lossy(&langs.stdout).into_owned();
        let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        let locale = env("LC_ALL").or_else(|| env("LC_MESSAGES")).or_else(|| env("LANG")).unwrap_or_default();
        let Some(l) = tesseract_langs(&locale, &have) else {
            return Err(OcrError::NoLanguage(
                "Tesseract has no language data. Install one (for example: sudo apt install tesseract-ocr-eng).".into()));
        };
        let big = prepared(img, 10_000);
        let k = big.width() as f32 / img.width().max(1) as f32;
        let mut png = Vec::new();
        big.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .map_err(|e| OcrError::Failed(e.to_string()))?;
        let mut child = Command::new("tesseract")
            .args(["stdin", "stdout", "-l", &l, "--dpi", "300"])
            .args(extra)
            .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null())
            .spawn().map_err(|_| OcrError::Missing(MISSING.into()))?;
        // Fed and read on threads, so neither pipe can fill and stall it.
        let mut stdin = child.stdin.take();
        std::thread::spawn(move || { if let Some(s) = stdin.as_mut() { let _ = s.write_all(&png); } });
        let mut stdout = child.stdout.take();
        let reader = std::thread::spawn(move || {
            let mut text = String::new();
            if let Some(s) = stdout.as_mut() { let _ = std::io::Read::read_to_string(s, &mut text); }
            text
        });
        let until = Instant::now() + Duration::from_secs(20);
        loop {
            match child.try_wait() {
                Ok(Some(st)) if st.success() => break,
                Ok(Some(_)) => return Err(OcrError::Failed("Tesseract couldn't read the picture.".into())),
                Ok(None) if Instant::now() < until => std::thread::sleep(Duration::from_millis(30)),
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(OcrError::Failed("Tesseract took too long.".into()));
                }
            }
        }
        Ok((reader.join().unwrap_or_default(), k))
    }
}

/// Words from Tesseract's `tsv` output, read at `k` times the picture's
/// size: rows of level 5 (a word), their box scaled back, lines numbered in
/// the order Tesseract gives them.
#[cfg_attr(any(windows, target_os = "macos"), allow(dead_code))]
pub(crate) fn words_from_tsv(tsv: &str, k: f32) -> Vec<WordBox> {
    let k = if k > 0.0 { k } else { 1.0 };
    let mut out = Vec::new();
    let mut lines: Vec<(u32, u32, u32, u32)> = Vec::new();
    for row in tsv.lines().skip(1) {
        let c: Vec<&str> = row.split('\t').collect();
        if c.len() < 12 || c[0] != "5" { continue; }
        let n = |i: usize| c[i].trim().parse::<f32>().unwrap_or(0.0);
        let text = c[11].trim();
        if text.is_empty() { continue; }
        let key = (n(1) as u32, n(2) as u32, n(3) as u32, n(4) as u32);
        let line = match lines.iter().position(|l| *l == key) {
            Some(i) => i,
            None => { lines.push(key); lines.len() - 1 }
        };
        out.push(WordBox { text: text.to_owned(), rect: (n(6) / k, n(7) / k, n(8) / k, n(9) / k), line });
    }
    out
}

/// Tesseract's `-l` for `locale` (`de_DE.UTF-8`), from the languages
/// installed (`tesseract --list-langs` output): the locale's language and
/// English, those of them that are there, or `None` when neither is.
pub fn tesseract_langs(locale: &str, installed: &str) -> Option<String> {
    let have: Vec<&str> = installed.lines().map(str::trim).filter(|l| !l.is_empty() && !l.contains(' ')).collect();
    let tag = locale.split(['.', '@']).next().unwrap_or("");
    let lang = tag.split('_').next().unwrap_or("");
    let mapped = match (lang, tag) {
        ("zh", t) if t.contains("TW") || t.contains("HK") => "chi_tra",
        ("zh", _) => "chi_sim",
        ("de", _) => "deu", ("fr", _) => "fra", ("es", _) => "spa", ("it", _) => "ita",
        ("pt", _) => "por", ("nl", _) => "nld", ("ru", _) => "rus", ("ja", _) => "jpn",
        ("ko", _) => "kor", ("pl", _) => "pol", ("sv", _) => "swe", ("tr", _) => "tur",
        ("uk", _) => "ukr", ("cs", _) => "ces", ("da", _) => "dan", ("fi", _) => "fin",
        ("nb" | "no" | "nn", _) => "nor", ("el", _) => "ell", ("he", _) => "heb",
        ("ar", _) => "ara", ("hi", _) => "hin", ("hu", _) => "hun", ("ro", _) => "ron",
        _ => "",
    };
    let picked: Vec<&str> = [mapped, "eng"].into_iter().filter(|l| !l.is_empty() && have.contains(l)).collect();
    (!picked.is_empty()).then(|| picked.join("+"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Experiment: what Windows reads in `FF_OCR_IMG` prepared different
    /// ways. Prints each; asserts nothing.
    #[cfg(windows)]
    #[test]
    #[ignore]
    fn ocr_variants_on_a_file() {
        use image::imageops::FilterType;
        let path = std::env::var("FF_OCR_IMG").expect("FF_OCR_IMG");
        let img = image::open(&path).unwrap().to_rgba8();
        let (w, h) = img.dimensions();
        let up = |k: u32, f: FilterType| image::imageops::resize(&img, w * k, h * k, f);
        let luma = |p: &image::Rgba<u8>| (p[0] as u32 * 299 + p[1] as u32 * 587 + p[2] as u32 * 114) / 1000;
        // Bright text only: light pixels black on white.
        let bright = |src: &RgbaImage, t: u32| {
            let mut o = src.clone();
            for p in o.pixels_mut() { let v = if luma(p) >= t { 0 } else { 255 }; *p = image::Rgba([v, v, v, 255]); }
            o
        };
        let gray_inv = |src: &RgbaImage| {
            let mut o = src.clone();
            for p in o.pixels_mut() { let v = 255 - luma(p) as u8; *p = image::Rgba([v, v, v, 255]); }
            o
        };
        let pad = |src: &RgbaImage, m: u32, bg: u8| {
            let mut o = RgbaImage::from_pixel(src.width() + 2 * m, src.height() + 2 * m, image::Rgba([bg, bg, bg, 255]));
            image::imageops::overlay(&mut o, src, m as i64, m as i64);
            o
        };
        let x4 = up(4, FilterType::Lanczos3);
        // Local contrast: a pixel brighter than its neighbourhood by `k`.
        let local = |src: &RgbaImage, r: i64, k: i64| {
            let (w, h) = (src.width() as i64, src.height() as i64);
            let l: Vec<i64> = src.pixels().map(|p| luma(p) as i64).collect();
            let mut integ = vec![0i64; ((w + 1) * (h + 1)) as usize];
            for y in 0..h { for x in 0..w {
                let i = ((y + 1) * (w + 1) + x + 1) as usize;
                integ[i] = l[(y * w + x) as usize] + integ[i - 1] + integ[i - (w + 1) as usize] - integ[i - (w + 2) as usize];
            } }
            let mut o = src.clone();
            for y in 0..h { for x in 0..w {
                let (x0, y0, x1, y1) = ((x - r).max(0), (y - r).max(0), (x + r + 1).min(w), (y + r + 1).min(h));
                let s = integ[(y1 * (w + 1) + x1) as usize] - integ[(y0 * (w + 1) + x1) as usize]
                    - integ[(y1 * (w + 1) + x0) as usize] + integ[(y0 * (w + 1) + x0) as usize];
                let mean = s / ((x1 - x0) * (y1 - y0));
                let v = if l[(y * w + x) as usize] > mean + k { 0 } else { 255 };
                o.put_pixel(x as u32, y as u32, image::Rgba([v, v, v, 255]));
            } }
            o
        };
        let variants: Vec<(String, RgbaImage)> = vec![
            ("4x bright>150 padded".into(), pad(&bright(&x4, 150), 40, 255)),
            ("4x bright>185 padded".into(), pad(&bright(&x4, 185), 40, 255)),
            ("4x local r20 k30 padded".into(), pad(&local(&x4, 20, 30), 40, 255)),
            ("4x local r30 k20 padded".into(), pad(&local(&x4, 30, 20), 40, 255)),
            ("3x bright>200 padded".into(), pad(&bright(&up(3, FilterType::Lanczos3), 200), 30, 255)),
            ("as is".into(), img.clone()),
            ("2x triangle (now)".into(), up(2, FilterType::Triangle)),
            ("3x lanczos".into(), up(3, FilterType::Lanczos3)),
            ("4x lanczos".into(), x4.clone()),
            ("4x gray inverted".into(), gray_inv(&x4)),
            ("4x bright>170".into(), bright(&x4, 170)),
            ("4x bright>200".into(), bright(&x4, 200)),
            ("4x bright>200 padded".into(), pad(&bright(&x4, 200), 40, 255)),
            ("4x bright>220 padded".into(), pad(&bright(&x4, 220), 40, 255)),
            ("6x bright>200 padded".into(), pad(&bright(&up(6, FilterType::Lanczos3), 200), 60, 255)),
        ];
        for (name, v) in variants {
            let _ = v.save(format!("{path}.{}.png", name.replace(['>', ' ', '(', ')'], "_")));
            println!("--- {name} ({}x{})\n{}", v.width(), v.height(), imp::recognize(&v).unwrap_or_else(|e| e.to_string()));
        }
    }

    /// Experiment: the real pipeline on `FF_OCR_IMG`: each pass, then what
    /// TEXT mode would copy.
    #[cfg(windows)]
    #[test]
    #[ignore]
    fn ocr_pipeline_on_a_file() {
        let path = std::env::var("FF_OCR_IMG").expect("FF_OCR_IMG");
        let img = image::open(&path).unwrap().to_rgba8();
        for (i, (p, k, pad)) in passes(&img, 2600).iter().enumerate() {
            println!("--- pass {i} ({}x{}, x{k}, pad {pad})\n{}", p.width(), p.height(),
                     imp::recognize(p).unwrap_or_else(|e| e.to_string()));
        }
        println!("=== copied:\n{}", read_text(&img).unwrap_or_else(|e| e.to_string()));
    }

    #[test]
    fn a_tall_picture_is_read_in_overlapping_strips_that_keep_each_word_once() {
        assert_eq!(strips(900), vec![(0, 900, (0.0, 900.0))], "short: whole");
        let s = strips(4714);
        assert!(s.len() >= 4);
        // Strips overlap, and the kept parts tile the picture exactly.
        let mut at = 0.0;
        for (i, (top, h, (from, to))) in s.iter().enumerate() {
            assert!(*h <= STRIP);
            assert_eq!(*from, at, "strip {i} keeps on from where the last stopped");
            assert!(*from >= *top as f32 && *to <= (*top + *h) as f32);
            if i > 0 { assert!(*top < s[i - 1].0 + s[i - 1].1, "overlaps the one before"); }
            at = *to;
        }
        assert_eq!(at, 4714.0);
    }

    #[test]
    fn tesseract_words_come_with_their_boxes_and_lines() {
        let tsv = "level\tpage_num\tblock_num\tpar_num\tline_num\tword_num\tleft\ttop\twidth\theight\tconf\ttext\n\
                   1\t1\t0\t0\t0\t0\t0\t0\t800\t600\t-1\t\n\
                   5\t1\t1\t1\t1\t1\t20\t40\t100\t30\t96\tHello\n\
                   5\t1\t1\t1\t1\t2\t140\t40\t120\t30\t95\tworld\n\
                   5\t1\t1\t1\t2\t1\t20\t90\t80\t30\t91\tagain\n\
                   5\t1\t1\t1\t2\t2\t110\t90\t10\t30\t20\t \n";
        let w = words_from_tsv(tsv, 2.0);
        assert_eq!(w.len(), 3, "the blank one is no word");
        assert_eq!(w[0].rect, (10.0, 20.0, 50.0, 15.0), "back to the picture's own pixels");
        assert_eq!((w[0].line, w[1].line, w[2].line), (0, 0, 1));
        assert_eq!(w[2].text, "again");
    }

    #[test]
    fn a_line_known_only_as_a_whole_is_shared_out_by_characters() {
        let w = split_line("ab cdef", (0.0, 0.0, 70.0, 10.0), 3);
        assert_eq!(w.len(), 2);
        assert_eq!(w[0].rect, (0.0, 0.0, 20.0, 10.0));
        assert_eq!(w[1].rect, (30.0, 0.0, 40.0, 10.0));
        assert!(w.iter().all(|x| x.line == 3));
    }

    /// Experiment: the words Windows finds in `FF_OCR_IMG`, with boxes.
    #[cfg(windows)]
    #[test]
    #[ignore]
    fn words_on_a_file() {
        let path = std::env::var("FF_OCR_IMG").expect("FF_OCR_IMG");
        let img = image::open(&path).unwrap().to_rgba8();
        let t = std::time::Instant::now();
        let words = read_words(&img).unwrap();
        println!("{} words in {:?}", words.len(), t.elapsed());
        for w in words.iter().take(40) { println!("{:>3} {:?} {}", w.line, w.rect, w.text); }
    }

    fn line(text: &str, y: f32) -> Line { Line { text: text.into(), rect: (10.0, y, 100.0, 10.0) } }

    #[test]
    fn the_reading_most_passes_agree_on_wins_and_a_lone_stray_is_dropped() {
        // The game menu from Brandon's screenshot, as the passes read it.
        let passes = vec![
            vec![line("RECENTER", 0.0), line("sÉ'T DESTINATION'O-", 60.0)],
            vec![line("RECENTER", 0.0), line("2D.VIEW", 15.0), line("SET DESTINATION O", 60.0)],
            vec![line("RECENT R", 1.0), line("2D IEW", 16.0)],
            vec![],
            vec![line("IRECENTERLV", 0.5), line("01", 90.0)],
        ];
        assert_eq!(merge_passes(&passes), ["RECENTER", "2D.VIEW", "SET DESTINATION O"],
                   "\"01\" was seen once, so it isn't text");
        // One pass: as it read.
        assert_eq!(merge_passes(&[vec![line("only", 0.0)]]), ["only"]);
        // A pass that split a line in two is joined left to right.
        let split = vec![
            vec![Line { text: "World".into(), rect: (60.0, 0.0, 40.0, 10.0) }, Line { text: "Hello".into(), rect: (0.0, 0.0, 50.0, 10.0) }],
            vec![Line { text: "Hello World".into(), rect: (0.0, 0.0, 100.0, 10.0) }],
        ];
        assert_eq!(merge_passes(&split), ["Hello World"]);
    }

    #[test]
    fn small_pictures_get_extra_passes_and_big_ones_one() {
        let small = RgbaImage::from_pixel(120, 120, image::Rgba([30, 30, 30, 255]));
        let p = passes(&small, 2600);
        assert_eq!(p.len(), 5);
        assert_eq!((p[1].0.width(), p[1].1, p[1].2), (120 * 4 + 80, 4.0, 40), "4x with a margin");
        let big = RgbaImage::from_pixel(1200, 300, image::Rgba([30, 30, 30, 255]));
        assert_eq!(passes(&big, 2600).len(), 1);
        assert_eq!(likeness("RECENTER", "recenter"), 1.0);
        assert!(cleanness("SET DESTINATION") > cleanness("sÉ'T DESTINATION'O-"));
    }

    #[test]
    fn small_pictures_are_read_twice_as_large_and_big_ones_fit_the_limit() {
        assert_eq!(read_size(520, 120, 10_000), (1040, 240));
        assert_eq!(read_size(2560, 1440, 10_000), (5120, 2880), "a whole screen still doubles");
        assert_eq!(read_size(2600, 1600, 10_000), (2600, 1600), "over 4 MP: as it is");
        assert_eq!(read_size(1200, 20_000, 10_000), (600, 10_000), "scaled down to the limit");
        assert_eq!(read_size(3000, 400, 2600), (2600, 347));
        assert_eq!(read_size(0, 0, 10_000), (2, 2), "never empty");
    }

    #[test]
    fn tesseract_reads_the_locales_language_and_english_when_installed() {
        let list = "List of available languages in \"/usr/share/tesseract-ocr/5/tessdata/\" (3):\ndeu\neng\nosd\n";
        assert_eq!(tesseract_langs("de_DE.UTF-8", list).as_deref(), Some("deu+eng"));
        assert_eq!(tesseract_langs("fr_FR.UTF-8", list).as_deref(), Some("eng"), "no French data: English");
        assert_eq!(tesseract_langs("", list).as_deref(), Some("eng"));
        assert_eq!(tesseract_langs("zh_TW.UTF-8", "chi_tra\n").as_deref(), Some("chi_tra"));
        assert_eq!(tesseract_langs("en_US.UTF-8", "osd\n"), None, "no language data at all");
    }
}
