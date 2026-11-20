//! Recent captures: the last [`KEEP`] pictures that left Frame Fang (copied,
//! saved or pinned, copy-only included), unframed, as PNGs with small
//! thumbnails in the app's cache folder, for the tray's RECENT list.
//!
//! One worker thread owns the folder and does every write, clear and load in
//! the order asked, so a Clear can never be undone by a write still queued
//! before it, and nothing writes from two threads at once. A second output
//! of one capture session (copied, then saved) replaces that session's item
//! instead of adding another.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use image::RgbaImage;

/// How many captures are kept.
pub const KEEP: usize = 10;
/// A thumbnail's box: the picture fits inside it, its shape kept.
pub const THUMB: (u32, u32) = (96, 60);

/// One kept capture.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    /// `<unix ms>-<pid>-<n>`: names its files and orders the list.
    pub id: String,
    /// When it was taken, in Unix milliseconds.
    pub ms: u64,
    /// The full picture's size.
    pub size: (u32, u32),
    pub thumb: RgbaImage,
}

impl Entry {
    /// "11:42  1280 x 720": when (local time) and how big.
    pub fn caption(&self) -> String {
        let when = chrono::DateTime::from_timestamp_millis(self.ms as i64)
            .map(|t| t.with_timezone(&chrono::Local).format("%H:%M").to_string())
            .unwrap_or_default();
        format!("{when}  {} x {}", self.size.0, self.size.1)
    }
}

enum Job {
    Write { session: u64, img: RgbaImage },
    Clear,
    Load { id: String, edit: bool },
    Enable(bool),
    Shutdown,
}

/// What the worker says back, picked up by [`Recent::poll`].
pub enum Reply {
    /// The list now, newest first.
    Changed(Vec<Entry>),
    /// A kept picture, read for copying (`edit: false`) or editing.
    Loaded { img: RgbaImage, edit: bool },
    /// The asked-for item is not kept any more.
    Gone,
}

/// The tray's handle on the worker.
pub struct Recent {
    tx: Sender<Job>,
    rx: Receiver<Reply>,
    worker: Option<std::thread::JoinHandle<()>>,
}

/// Where recent captures are kept.
pub fn dir() -> PathBuf {
    directories::ProjectDirs::from("org", "framefang", "Framefang")
        .map(|d| d.cache_dir().join("recent"))
        .unwrap_or_else(|| std::env::temp_dir().join("framefang-recent"))
}

impl Recent {
    /// Start the worker on `dir`. Kept captures load at once (a
    /// [`Reply::Changed`] follows); with keeping off, any left over are
    /// deleted.
    pub fn start(dir: PathBuf, enabled: bool) -> Recent {
        let (tx, jobs) = channel();
        let (out, rx) = channel();
        let worker = std::thread::Builder::new().name("recent".into())
            .spawn(move || Worker::new(dir, enabled, out).run(jobs)).ok();
        Recent { tx, rx, worker }
    }

    /// Keep `img`, the picture of capture `session` (replacing what that
    /// session kept before).
    pub fn record(&self, session: u64, img: RgbaImage) { let _ = self.tx.send(Job::Write { session, img }); }

    /// Read a kept picture back, to copy it or to edit it.
    pub fn load(&self, id: &str, edit: bool) { let _ = self.tx.send(Job::Load { id: id.to_owned(), edit }); }

    /// Turn keeping on or off; off deletes what is kept.
    pub fn set_enabled(&self, on: bool) { let _ = self.tx.send(Job::Enable(on)); }

    /// Delete everything kept.
    #[allow(dead_code)]
    pub fn clear(&self) { let _ = self.tx.send(Job::Clear); }

    /// What the worker has said since the last call.
    pub fn poll(&self) -> Vec<Reply> { self.rx.try_iter().collect() }

    /// Let the worker finish what is queued, waiting up to a second.
    pub fn shutdown(&mut self) {
        let _ = self.tx.send(Job::Shutdown);
        if let Some(h) = self.worker.take() {
            let until = std::time::Instant::now() + Duration::from_secs(1);
            while !h.is_finished() && std::time::Instant::now() < until {
                std::thread::sleep(Duration::from_millis(10));
            }
            if h.is_finished() { let _ = h.join(); }
        }
    }
}

struct Worker {
    dir: PathBuf,
    enabled: bool,
    out: Sender<Reply>,
    /// Newest first.
    entries: Vec<Entry>,
    /// Which item each capture session kept.
    sessions: HashMap<u64, String>,
    last_ms: u64,
    n: u64,
}

impl Worker {
    fn new(dir: PathBuf, enabled: bool, out: Sender<Reply>) -> Worker {
        let mut w = Worker { dir, enabled, out, entries: Vec::new(), sessions: HashMap::new(), last_ms: 0, n: 0 };
        if enabled { w.entries = load_entries(&w.dir); w.prune(); } else { clear_dir(&w.dir); }
        w.last_ms = w.entries.first().map_or(0, |e| e.ms);
        w.changed();
        w
    }

    fn run(mut self, jobs: Receiver<Job>) {
        for job in jobs {
            match job {
                Job::Write { session, img } => if self.enabled { self.write(session, &img); },
                Job::Clear => self.clear(),
                Job::Load { id, edit } => {
                    let img = self.entries.iter().any(|e| e.id == id)
                        .then(|| image::open(self.dir.join(format!("{id}.png"))).ok().map(|i| i.to_rgba8()))
                        .flatten();
                    let _ = self.out.send(match img { Some(img) => Reply::Loaded { img, edit }, None => Reply::Gone });
                }
                Job::Enable(on) => {
                    if self.enabled && !on { self.clear(); }
                    self.enabled = on;
                }
                Job::Shutdown => break,
            }
        }
    }

    fn write(&mut self, session: u64, img: &RgbaImage) {
        if img.width() == 0 || img.height() == 0 { return; }
        // The same session's earlier picture gives way to this one.
        if let Some(old) = self.sessions.remove(&session) { self.remove(&old); }
        let now = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64);
        let ms = now.max(self.last_ms + 1);
        self.last_ms = ms;
        self.n += 1;
        let id = format!("{ms:013}-{}-{}", std::process::id(), self.n);
        let _ = std::fs::create_dir_all(&self.dir);
        let thumb = thumbnail(img);
        let ok = write_atomic(&self.dir, &format!("{id}.png"), img)
            && write_atomic(&self.dir, &format!("{id}.thumb.png"), &thumb);
        if !ok { self.remove_files(&id); return; }
        self.sessions.insert(session, id.clone());
        self.entries.insert(0, Entry { id, ms, size: img.dimensions(), thumb });
        self.prune();
        self.changed();
    }

    fn remove(&mut self, id: &str) {
        self.entries.retain(|e| e.id != id);
        self.remove_files(id);
    }

    fn remove_files(&self, id: &str) {
        let _ = std::fs::remove_file(self.dir.join(format!("{id}.png")));
        let _ = std::fs::remove_file(self.dir.join(format!("{id}.thumb.png")));
    }

    fn prune(&mut self) {
        while self.entries.len() > KEEP {
            if let Some(e) = self.entries.pop() { self.remove_files(&e.id); }
        }
        let kept: Vec<String> = self.entries.iter().map(|e| e.id.clone()).collect();
        self.sessions.retain(|_, id| kept.contains(id));
    }

    fn clear(&mut self) {
        self.entries.clear();
        self.sessions.clear();
        clear_dir(&self.dir);
        self.changed();
    }

    fn changed(&self) { let _ = self.out.send(Reply::Changed(self.entries.clone())); }
}

/// The picture shrunk to fit [`THUMB`], its shape kept.
pub fn thumbnail(img: &RgbaImage) -> RgbaImage {
    let (w, h) = img.dimensions();
    let k = (THUMB.0 as f32 / w as f32).min(THUMB.1 as f32 / h as f32).min(1.0);
    let (tw, th) = (((w as f32 * k).round() as u32).max(1), ((h as f32 * k).round() as u32).max(1));
    image::imageops::thumbnail(img, tw, th)
}

/// Write `img` as `name` in `dir` under a work name first, so a quit
/// mid-write never leaves half a file under the real name.
fn write_atomic(dir: &Path, name: &str, img: &RgbaImage) -> bool {
    let tmp = dir.join(format!(".tmp-{name}"));
    let ok = img.save_with_format(&tmp, image::ImageFormat::Png).is_ok()
        && std::fs::rename(&tmp, dir.join(name)).is_ok();
    if !ok { let _ = std::fs::remove_file(&tmp); }
    ok
}

/// The kept captures in `dir`, newest first: every `<id>.png` with its
/// thumbnail, the thumbnail made again if it is missing. Work files a quit
/// left behind are deleted.
fn load_entries(dir: &Path) -> Vec<Entry> {
    let Ok(read) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut out = Vec::new();
    for f in read.flatten() {
        let name = f.file_name().to_string_lossy().into_owned();
        if name.starts_with(".tmp-") { let _ = std::fs::remove_file(f.path()); continue; }
        let Some(id) = name.strip_suffix(".png").filter(|s| !s.ends_with(".thumb")) else { continue };
        let Some(ms) = id.split('-').next().and_then(|m| m.parse::<u64>().ok()) else { continue };
        let Ok(size) = image::image_dimensions(f.path()) else { continue };
        let thumb = image::open(dir.join(format!("{id}.thumb.png"))).map(|i| i.to_rgba8())
            .or_else(|_| image::open(f.path()).map(|i| thumbnail(&i.to_rgba8())));
        let Ok(thumb) = thumb else { continue };
        out.push(Entry { id: id.to_owned(), ms, size, thumb });
    }
    out.sort_by(|a, b| b.id.cmp(&a.id));
    out
}

/// Delete everything Frame Fang keeps in `dir` (and only that).
fn clear_dir(dir: &Path) {
    let Ok(read) = std::fs::read_dir(dir) else { return };
    for f in read.flatten() {
        let name = f.file_name().to_string_lossy().into_owned();
        if name.ends_with(".png") { let _ = std::fs::remove_file(f.path()); }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ff-recent-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    fn pic(w: u32, h: u32, v: u8) -> RgbaImage { RgbaImage::from_pixel(w, h, image::Rgba([v, v, v, 255])) }

    /// Wait (up to 5 s) for the list the worker reports to be `done`, and
    /// return it as it last was.
    fn wait_for(r: &Recent, done: impl Fn(&[Entry]) -> bool) -> Vec<Entry> {
        let until = std::time::Instant::now() + Duration::from_secs(5);
        let mut last = Vec::new();
        while std::time::Instant::now() < until {
            for reply in r.poll() { if let Reply::Changed(l) = reply { last = l; } }
            if done(&last) { return last; }
            std::thread::sleep(Duration::from_millis(10));
        }
        last
    }

    fn settle(r: &Recent, n: usize) -> Vec<Entry> { wait_for(r, |l| l.len() == n) }

    #[test]
    fn keeps_the_newest_ten_and_one_item_per_session() {
        let d = temp_dir("keep");
        let mut r = Recent::start(d.clone(), true);
        for i in 0..12 { r.record(i, pic(40 + i as u32, 30, i as u8)); }
        // Every write done: the last one is newest.
        let list = wait_for(&r, |l| l.first().is_some_and(|e| e.size == (51, 30)));
        assert_eq!(list.len(), KEEP);
        assert_eq!(list[0].size, (51, 30), "newest first");
        assert_eq!(list[KEEP - 1].size, (42, 30), "the two oldest went");
        // Copied, then saved: one item, the later picture.
        r.record(11, pic(200, 100, 9));
        let list = wait_for(&r, |l| l.first().is_some_and(|e| e.size == (200, 100)));
        assert_eq!(list[0].size, (200, 100));
        assert_eq!(list.iter().filter(|e| e.size == (51, 30)).count(), 0, "replaced, not added");
        let files = std::fs::read_dir(&d).unwrap().count();
        assert_eq!(files, 2 * KEEP, "a picture and a thumbnail each, nothing else");
        // Read back, after a restart too.
        r.shutdown();
        let r = Recent::start(d.clone(), true);
        let again = settle(&r, KEEP);
        assert_eq!(again.iter().map(|e| e.id.clone()).collect::<Vec<_>>(),
                   list.iter().map(|e| e.id.clone()).collect::<Vec<_>>());
        assert!(again[0].thumb.width() <= THUMB.0 && again[0].thumb.height() <= THUMB.1);
        r.load(&again[0].id, true);
        r.load("nope", false);
        let until = std::time::Instant::now() + Duration::from_secs(5);
        let (mut loaded, mut gone) = (None, false);
        while std::time::Instant::now() < until && (loaded.is_none() || !gone) {
            for reply in r.poll() {
                match reply {
                    Reply::Loaded { img, edit } => loaded = Some((img.dimensions(), edit)),
                    Reply::Gone => gone = true,
                    Reply::Changed(_) => {}
                }
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(loaded, Some(((200, 100), true)));
        assert!(gone);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn turning_it_off_clears_and_stops_keeping() {
        let d = temp_dir("off");
        let mut r = Recent::start(d.clone(), true);
        r.record(1, pic(10, 10, 1));
        r.record(2, pic(10, 10, 2));
        settle(&r, 2);
        r.set_enabled(false);
        r.record(3, pic(10, 10, 3));
        assert!(settle(&r, 0).is_empty());
        r.shutdown();
        assert_eq!(std::fs::read_dir(&d).map(|i| i.count()).unwrap_or(0), 0, "nothing left on disk");
        // Off at start: left-over pictures are deleted.
        std::fs::create_dir_all(&d).unwrap();
        pic(5, 5, 1).save(d.join("0000000000001-1-1.png")).unwrap();
        let mut r = Recent::start(d.clone(), false);
        settle(&r, 0);
        r.shutdown();
        assert_eq!(std::fs::read_dir(&d).unwrap().count(), 0);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_thumbnail_fits_its_box_and_keeps_its_shape() {
        assert_eq!(thumbnail(&pic(1920, 1080, 0)).dimensions(), (96, 54));
        assert_eq!(thumbnail(&pic(300, 900, 0)).dimensions(), (20, 60));
        assert_eq!(thumbnail(&pic(40, 20, 0)).dimensions(), (40, 20), "never enlarged");
    }
}
