//! Scrolling capture's engine: you scroll the page, at your own pace and
//! either way, and it stitches what passes through the area (`stitch`).
//!
//! It runs on its own thread, grabbing the area every few milliseconds; a
//! grab that changed is matched to the last one and its new rows added. It
//! scrolls nothing itself (Brandon 2026-11-20: scrolling for you jumped the
//! page and stopped as soon as the mouse moved). A scroll too fast to line
//! up is skipped, and the indicator says to scroll back a little; the next
//! grab that lines up with the last one goes on from there.
//!
//! Enter or a click finishes it, Esc cancels it. They are caught before the
//! page gets them, so Enter can't send a chat message and the click doesn't
//! press anything. The capture key pressed again also finishes it.
//!
//! Windows only for now; elsewhere [`supported`] is false and SCROLL isn't
//! offered.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver};
use std::sync::Arc;
use std::time::{Duration, Instant};

use image::RgbaImage;

use crate::stitch::{Stitcher, Step};

/// A stitched picture stops growing past this many pixels.
pub const CAP_PIXELS: u64 = 32_000_000;

/// How often the area is grabbed.
const EVERY: Duration = Duration::from_millis(15);

/// Changed grabs in a row that don't line up before the indicator says it
/// lost its place (one alone is a hover or a blink).
const LOST_AFTER: u32 = 3;

/// Whether Frame Fang can follow a scrolling page here.
pub fn supported() -> bool { cfg!(windows) }

/// Why a scrolling capture ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum End {
    /// Enter, a click, or the capture key: keep it.
    Done,
    /// Esc: throw it away.
    Cancelled,
    /// The picture reached [`CAP_PIXELS`].
    Cap,
    Failed(String),
}

impl End {
    /// What the notice says, if this end needs one.
    pub fn notice(&self, height: u32) -> Option<String> {
        match self {
            End::Done | End::Cancelled => None,
            End::Cap => Some(format!("Stopped at {height} px, the most one picture holds.")),
            End::Failed(e) => Some(format!("Scrolling capture stopped: {e}")),
        }
    }
}

pub enum Event {
    /// How tall the stitched picture is so far.
    Progress(u32),
    /// The page moved too far to line up (`true`), or lined up again.
    Lost(bool),
    Done { img: RgbaImage, end: End },
}

/// A scrolling capture running on its thread.
pub struct Session {
    rx: Receiver<Event>,
    stop: Arc<AtomicBool>,
    /// The area being followed, in desktop pixels.
    pub area: (i32, i32, i32, i32),
}

impl Session {
    /// Start following the page under `area`.
    pub fn start(area: (i32, i32, i32, i32)) -> Session {
        let (tx, rx) = channel();
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        std::thread::Builder::new().name("scroll".into()).spawn(move || {
            let (img, end) = run(area, &flag, &|ev| { let _ = tx.send(ev); });
            let _ = tx.send(Event::Done { img, end });
        }).ok();
        Session { rx, stop, area }
    }

    /// Finish now, keeping what is stitched.
    pub fn stop(&self) { self.stop.store(true, Ordering::Relaxed); }

    /// What the engine has said since the last call.
    pub fn poll(&self) -> Vec<Event> { self.rx.try_iter().collect() }
}

/// Follow the page until it is finished; the picture and why it ended.
fn run(area: (i32, i32, i32, i32), stop: &AtomicBool, say: &dyn Fn(Event)) -> (RgbaImage, End) {
    let (x, y, w, h) = area;
    let grab = || framefang_platform::capture::capture_area(x, y, w as u32, h as u32);
    let keys = input::watch();
    // The capture screen has just closed: let the page repaint.
    std::thread::sleep(Duration::from_millis(150));
    let first = match grab() {
        Ok(f) => f,
        Err(e) => return (RgbaImage::new(1, 1), End::Failed(e.to_string())),
    };
    let mut prev = first.clone();
    let mut s = Stitcher::new(first);
    let mut follow = Follow::default();
    // Once finishing: until when it waits for a last whole picture (a page
    // that never stops moving keeps what it has).
    let mut finishing: Option<Instant> = None;
    let end = loop {
        if let Some(input::Said::Cancel) = keys.said() { break End::Cancelled; }
        let finish = matches!(keys.said(), Some(input::Said::Finish)) || stop.load(Ordering::Relaxed);
        if finish {
            let until = *finishing.get_or_insert_with(|| Instant::now() + Duration::from_millis(500));
            if Instant::now() >= until { break End::Done; }
        }
        std::thread::sleep(EVERY);
        let next = match grab() {
            Ok(n) => n,
            Err(e) => break End::Failed(e.to_string()),
        };
        if next == prev {
            if finish { break End::Done; }
            continue;
        }
        // Only a whole picture: a window can be grabbed halfway through
        // painting what it scrolled into view (Notepad stitched a ghosted
        // line that way, 2026-11-20). Two grabs in a row the same are done.
        match grab() {
            Ok(again) if again == next => {}
            Ok(_) => continue,
            Err(e) => break End::Failed(e.to_string()),
        }
        prev = next.clone();
        let step = s.push(next);
        // Finishing: the page has stopped; this last whole picture is in.
        if finish { break End::Done; }
        if let Some(lost) = follow.after(step) { say(Event::Lost(lost)); }
        if let Step::Moved(_) = step {
            say(Event::Progress(s.height()));
            if s.height() as u64 * w as u64 >= CAP_PIXELS { break End::Cap; }
        }
    };
    // A click that finished it: its release is caught too before the
    // watcher stops.
    keys.let_go();
    (s.finish(), end)
}

/// Whether the stitch has lost its place: [`LOST_AFTER`] changed grabs in a
/// row that didn't line up, until one does.
#[derive(Default)]
struct Follow {
    misses: u32,
    lost: bool,
}

impl Follow {
    /// After a changed grab: `Some` when lost turns on or off.
    fn after(&mut self, step: Step) -> Option<bool> {
        match step {
            Step::Moved(_) | Step::Same => {
                self.misses = 0;
                if self.lost { self.lost = false; return Some(false); }
            }
            Step::NoMatch => {
                self.misses += 1;
                if !self.lost && self.misses >= LOST_AFTER { self.lost = true; return Some(true); }
            }
        }
        None
    }
}

/// Enter, Esc and the left button while a capture runs, caught before any
/// window gets them (low-level hooks on a thread of their own, which pumps
/// messages so the mouse never waits on the grabbing).
#[cfg(windows)]
mod input {
    use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
    use std::time::{Duration, Instant};

    use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
    use windows::Win32::System::Threading::GetCurrentThreadId;
    use windows::Win32::UI::Input::KeyboardAndMouse::{VK_ESCAPE, VK_RETURN};
    use windows::Win32::UI::WindowsAndMessaging::{
        CallNextHookEx, GetMessageW, PostThreadMessageW, SetWindowsHookExW, UnhookWindowsHookEx, KBDLLHOOKSTRUCT,
        MSG, WH_KEYBOARD_LL, WH_MOUSE_LL, WM_KEYDOWN, WM_KEYUP, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_QUIT,
        WM_SYSKEYDOWN, WM_SYSKEYUP,
    };

    pub enum Said { Finish, Cancel }

    const NOTHING: u8 = 0;
    const FINISH: u8 = 1;
    const CANCEL: u8 = 2;
    /// What was pressed (one capture runs at a time).
    static SAID: AtomicU8 = AtomicU8::new(NOTHING);
    /// A press was caught: its release is caught too.
    static BUTTON_HELD: AtomicBool = AtomicBool::new(false);

    pub struct Watch { thread: u32 }

    pub fn watch() -> Watch {
        SAID.store(NOTHING, Ordering::Relaxed);
        BUTTON_HELD.store(false, Ordering::Relaxed);
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::Builder::new().name("scroll keys".into()).spawn(move || unsafe {
            let mouse = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_hook), None, 0);
            let keys = SetWindowsHookExW(WH_KEYBOARD_LL, Some(key_hook), None, 0);
            let _ = tx.send(GetCurrentThreadId());
            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).0 > 0 {}
            if let Ok(h) = mouse { let _ = UnhookWindowsHookEx(h); }
            if let Ok(h) = keys { let _ = UnhookWindowsHookEx(h); }
        }).ok();
        Watch { thread: rx.recv_timeout(Duration::from_secs(2)).unwrap_or(0) }
    }

    impl Watch {
        pub fn said(&self) -> Option<Said> {
            match SAID.load(Ordering::Relaxed) {
                FINISH => Some(Said::Finish),
                CANCEL => Some(Said::Cancel),
                _ => None,
            }
        }

        /// Wait (briefly) for a caught press to be released.
        pub fn let_go(&self) {
            let until = Instant::now() + Duration::from_millis(600);
            while BUTTON_HELD.load(Ordering::Relaxed) && Instant::now() < until {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }

    impl Drop for Watch {
        fn drop(&mut self) {
            if self.thread != 0 { unsafe { let _ = PostThreadMessageW(self.thread, WM_QUIT, WPARAM(0), LPARAM(0)); } }
        }
    }

    unsafe extern "system" fn mouse_hook(code: i32, w: WPARAM, l: LPARAM) -> LRESULT {
        if code >= 0 {
            match w.0 as u32 {
                WM_LBUTTONDOWN => {
                    let _ = SAID.compare_exchange(NOTHING, FINISH, Ordering::Relaxed, Ordering::Relaxed);
                    BUTTON_HELD.store(true, Ordering::Relaxed);
                    return LRESULT(1);
                }
                WM_LBUTTONUP if BUTTON_HELD.swap(false, Ordering::Relaxed) => return LRESULT(1),
                _ => {}
            }
        }
        unsafe { CallNextHookEx(None, code, w, l) }
    }

    unsafe extern "system" fn key_hook(code: i32, w: WPARAM, l: LPARAM) -> LRESULT {
        if code >= 0 {
            let vk = unsafe { (*(l.0 as *const KBDLLHOOKSTRUCT)).vkCode };
            let down = matches!(w.0 as u32, WM_KEYDOWN | WM_SYSKEYDOWN);
            let up = matches!(w.0 as u32, WM_KEYUP | WM_SYSKEYUP);
            let said = if vk == VK_RETURN.0 as u32 { FINISH } else if vk == VK_ESCAPE.0 as u32 { CANCEL } else { NOTHING };
            if said != NOTHING && (down || up) {
                if down { let _ = SAID.compare_exchange(NOTHING, said, Ordering::Relaxed, Ordering::Relaxed); }
                return LRESULT(1);
            }
        }
        unsafe { CallNextHookEx(None, code, w, l) }
    }
}

#[cfg(not(windows))]
mod input {
    #[allow(dead_code)]
    pub enum Said { Finish, Cancel }
    pub struct Watch;
    pub fn watch() -> Watch { Watch }
    impl Watch {
        pub fn said(&self) -> Option<Said> { None }
        pub fn let_go(&self) {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_says_it_lost_its_place_only_after_a_few_misses_and_found_it_again() {
        let mut f = Follow::default();
        assert_eq!(f.after(Step::NoMatch), None, "one miss is a hover or a blink");
        assert_eq!(f.after(Step::Moved(40)), None);
        for _ in 1..LOST_AFTER { assert_eq!(f.after(Step::NoMatch), None); }
        assert_eq!(f.after(Step::NoMatch), Some(true), "lost");
        assert_eq!(f.after(Step::NoMatch), None, "said once");
        assert_eq!(f.after(Step::Moved(30)), Some(false), "lined up again");
    }

    #[test]
    fn ends_that_need_a_notice_say_why() {
        assert_eq!(End::Done.notice(900), None);
        assert_eq!(End::Cancelled.notice(900), None);
        assert!(End::Cap.notice(31_000).unwrap().contains("31000 px"));
        assert!(End::Failed("x".into()).notice(0).unwrap().contains("x"));
    }
}
