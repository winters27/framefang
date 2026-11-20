//! Which part of a window scrolls, for SCROLL mode's highlight: point at a
//! browser and the page lights up, not the tabs and address bar around it.
//!
//! Windows asks UI Automation, walking down from the window under the
//! pointer toward the pointer and keeping the innermost element that
//! scrolls up and down and is big enough to be a page. The capture screen
//! covers everything, so the window is asked by its handle, never by point.
//! Slow apps can take a while to answer, so callers run this off the UI
//! thread. Elsewhere there is no answer and the whole window lights up.

/// A screen rectangle, `(x, y, w, h)`, in virtual-screen device pixels.
pub type Rect = (i32, i32, i32, i32);

/// Smaller than this is a list box or a code block, not the page.
pub const MIN_PART: (i32, i32) = (160, 120);

/// The part of window `hwnd` under virtual-screen point `p` that scrolls,
/// or `None` when nothing there does (or the window doesn't say).
#[cfg(windows)]
pub fn part_at(hwnd: isize, p: (i32, i32)) -> Option<Rect> {
    imp::part_at(hwnd, p)
}

#[cfg(not(windows))]
pub fn part_at(_hwnd: isize, _p: (i32, i32)) -> Option<Rect> {
    None
}

/// From the path walked down toward the pointer (outermost first): each
/// element's rectangle and whether it scrolls up and down. The innermost
/// one that scrolls, holds the point and is big enough to be a page.
pub fn innermost_part(path: &[(Rect, bool)], p: (i32, i32)) -> Option<Rect> {
    path.iter().rev()
        .find(|(r, scrolls)| *scrolls && contains(*r, p) && r.2 >= MIN_PART.0 && r.3 >= MIN_PART.1)
        .map(|(r, _)| *r)
}

fn contains(r: Rect, p: (i32, i32)) -> bool {
    p.0 >= r.0 && p.0 < r.0 + r.2 && p.1 >= r.1 && p.1 < r.1 + r.3
}

#[cfg(windows)]
mod imp {
    use super::{contains, innermost_part, Rect};
    use std::time::{Duration, Instant};
    use windows::Win32::Foundation::{HWND, POINT, RECT};
    use windows::Win32::Graphics::Gdi::ScreenToClient;
    use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED};
    use windows::Win32::UI::Accessibility::{
        CUIAutomation, IUIAutomation, IUIAutomationElement, IUIAutomationScrollPattern, UIA_DocumentControlTypeId,
        UIA_ScrollPatternId,
    };
    use windows::Win32::UI::WindowsAndMessaging::{ChildWindowFromPointEx, CWP_SKIPINVISIBLE, CWP_SKIPTRANSPARENT};

    /// How long one answer may take, and how far down and across to look.
    const BUDGET: Duration = Duration::from_millis(1500);
    const MAX_DEPTH: usize = 40;
    const MAX_SIBLINGS: usize = 400;

    pub fn part_at(hwnd: isize, p: (i32, i32)) -> Option<Rect> {
        unsafe {
            // Safe to call again on a thread that already joined.
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            let uia: IUIAutomation = CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER).ok()?;
            let top = HWND(hwnd as *mut _);
            let deadline = Instant::now() + BUDGET;
            // The child window under the pointer has the smaller tree; but
            // Chromium apps lay a see-through layer window over their page
            // that knows nothing, so the window itself is asked next.
            let host = deepest_child(top, p);
            let found = walk(&uia, host, p, deadline);
            if found.is_some() || host == top { return found; }
            walk(&uia, top, p, deadline)
        }
    }

    unsafe fn walk(uia: &IUIAutomation, from: HWND, p: (i32, i32), deadline: Instant) -> Option<Rect> {
        let walker = uia.ControlViewWalker().ok()?;
        let el: IUIAutomationElement = uia.ElementFromHandle(from).ok()?;
        let mut found = Found::default();
        let mut path = Vec::new();
        down(&walker, &el, p, deadline, &mut path, &mut found);
        found.part.or(found.document).map(|(_, r)| r)
    }

    /// The deepest part that says it scrolls, and the outermost page
    /// document (Firefox-family browsers say nothing scrolls; their page is
    /// still a document the size of what shows), each with its depth.
    #[derive(Default)]
    struct Found { part: Option<(usize, Rect)>, document: Option<(usize, Rect)> }

    /// Depth first through every child that holds the point: an element
    /// that knows nothing (a see-through layer) can share the spot with the
    /// one that is the page. Keeps the deepest part found on any branch.
    /// (Not pruned by "off screen": Firefox calls its showing page that.)
    unsafe fn down(walker: &windows::Win32::UI::Accessibility::IUIAutomationTreeWalker, el: &IUIAutomationElement,
                   p: (i32, i32), deadline: Instant, path: &mut Vec<(Rect, bool)>, found: &mut Found) {
        if path.len() >= MAX_DEPTH || Instant::now() > deadline { return; }
        let Some(r) = rect_of(el) else { return };
        path.push((r, scrolls(el)));
        let depth = path.len();
        // The outermost document: every tab's page document has the page
        // area's rectangle (Firefox calls all of them off screen, the one
        // showing too), while one nested deeper is a frame inside a page,
        // possibly a hidden tab's.
        if el.CurrentControlType().is_ok_and(|t| t == UIA_DocumentControlTypeId)
            && innermost_part(&[(r, true)], p).is_some()
            && found.document.is_none_or(|(d, _)| depth < d) {
            found.document = Some((depth, r));
        }
        #[cfg(test)]
        if std::env::var_os("FF_PART_DEBUG").is_some() {
            let name = el.CurrentClassName().map(|s| s.to_string()).unwrap_or_default();
            let kind = el.CurrentControlType().map(|t| t.0).unwrap_or(0);
            let off = el.CurrentIsOffscreen().is_ok_and(|b| b.as_bool());
            let doc_name = el.CurrentName().map(|s| s.to_string()).unwrap_or_default();
            if kind == UIA_DocumentControlTypeId.0 || std::env::var_os("FF_PART_ALL").is_some() {
                println!("{}{r:?} scrolls={} off={off} class={name} type={kind} name={doc_name}", "  ".repeat(path.len()), path.last().is_some_and(|p| p.1));
            }
        }
        if let Some(part) = innermost_part(path, p) {
            let depth = path.iter().rposition(|(r, _)| *r == part).unwrap_or(0);
            if found.part.is_none_or(|(d, _)| depth > d) { found.part = Some((depth, part)); }
        }
        let mut child = walker.GetFirstChildElement(el).ok();
        let mut seen = 0;
        while let Some(c) = child {
            if Instant::now() > deadline || seen >= MAX_SIBLINGS { break; }
            if rect_of(&c).is_some_and(|r| contains(r, p)) { down(walker, &c, p, deadline, path, found); }
            child = walker.GetNextSiblingElement(&c).ok();
            seen += 1;
        }
        path.pop();
    }

    /// The innermost child window under `p`: a browser's page has a window
    /// of its own, and asking it gives a much smaller tree to walk.
    unsafe fn deepest_child(top: HWND, p: (i32, i32)) -> HWND {
        let mut h = top;
        for _ in 0..16 {
            let mut pt = POINT { x: p.0, y: p.1 };
            if !ScreenToClient(h, &mut pt).as_bool() { break; }
            let c = ChildWindowFromPointEx(h, pt, CWP_SKIPINVISIBLE | CWP_SKIPTRANSPARENT);
            if c.is_invalid() || c == h { break; }
            h = c;
        }
        h
    }

    unsafe fn rect_of(el: &IUIAutomationElement) -> Option<Rect> {
        let r: RECT = el.CurrentBoundingRectangle().ok()?;
        let (w, h) = (r.right - r.left, r.bottom - r.top);
        (w > 0 && h > 0).then_some((r.left, r.top, w, h))
    }

    unsafe fn scrolls(el: &IUIAutomationElement) -> bool {
        el.GetCurrentPatternAs::<IUIAutomationScrollPattern>(UIA_ScrollPatternId)
            .and_then(|s| s.CurrentVerticallyScrollable())
            .is_ok_and(|b| b.as_bool())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_innermost_big_scrolling_part_under_the_point_wins() {
        let window = ((0, 0, 1200, 900), false);
        let page = ((0, 100, 1200, 800), true);
        let article = ((200, 150, 800, 700), true);
        let code_box = ((300, 400, 400, 80), true);
        let p = (350, 420);
        assert_eq!(innermost_part(&[window, page, article, code_box], p), Some(article.0),
                   "a short code box is not the page");
        assert_eq!(innermost_part(&[window, page], p), Some(page.0));
        assert_eq!(innermost_part(&[window], p), None, "nothing scrolls");
        // A scrolling part that doesn't hold the point is not it.
        assert_eq!(innermost_part(&[window, ((900, 0, 300, 900), true)], p), None);
    }

    /// Asks a real window: FF_PART="hwnd,x,y" (decimal). Prints the part
    /// and how long the answer took.
    #[test]
    #[ignore]
    fn part_of_a_real_window() {
        let v: Vec<i64> = std::env::var("FF_PART").unwrap().split(',').map(|s| s.trim().parse().unwrap()).collect();
        let t = std::time::Instant::now();
        let part = part_at(v[0] as isize, (v[1] as i32, v[2] as i32));
        println!("PART {part:?} in {:?}", t.elapsed());
    }
}
