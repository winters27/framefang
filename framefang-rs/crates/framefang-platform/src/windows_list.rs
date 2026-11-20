//! The windows on screen when a capture froze it, for the capture screen's
//! hover highlight: point at a window and it lights up, click and it is the
//! picture.
//!
//! Read in the same breath as the screen itself, so the frames match what
//! the frozen picture shows. Windows only for now; elsewhere the list is
//! empty and the hover falls back to the screen under the cursor.

/// One window, as it was on screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OnScreenWindow {
    /// Its visible frame in virtual-screen device pixels, `(x, y, w, h)`:
    /// what the eye sees as the window, without the invisible resize
    /// border Windows 10 and 11 put around it.
    pub rect: (i32, i32, i32, i32),
    /// Its title, empty when it has none.
    pub title: String,
    /// The process it belongs to (0 where unknown), for its app's name.
    pub pid: u32,
    /// Its handle (0 where there is none), to ask which part of it scrolls
    /// (`scroll_parts`).
    pub hwnd: isize,
}

/// The app a process runs, as a file name would want it: the program's
/// name with a capital first letter ("Chrome", "Code"), or empty where it
/// can't be read. A store app's frame host names no app, so it is empty and
/// the window's title says what it is.
#[cfg(windows)]
pub fn app_name(pid: u32) -> String {
    imp::app_name(pid)
}

#[cfg(not(windows))]
pub fn app_name(_pid: u32) -> String {
    String::new()
}

/// Every window a click could mean, frontmost first. Hidden, minimized and
/// cloaked windows (other virtual desktops, suspended store apps), windows
/// that let clicks through (overlays), the desktop itself and Framefang's
/// own windows are left out.
#[cfg(windows)]
pub fn on_screen_windows() -> Vec<OnScreenWindow> {
    imp::list()
}

#[cfg(not(windows))]
pub fn on_screen_windows() -> Vec<OnScreenWindow> {
    Vec::new()
}

#[cfg(windows)]
mod imp {
    use super::OnScreenWindow;
    use windows::core::BOOL;
    use windows::Win32::Foundation::{HWND, LPARAM, RECT};
    use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS};
    use windows::Win32::System::Threading::GetCurrentProcessId;
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetClassNameW, GetWindowLongW, GetWindowRect, GetWindowTextW, GetWindowThreadProcessId,
        IsIconic, IsWindowVisible, GWL_EXSTYLE, WS_EX_TRANSPARENT,
    };

    struct Walk {
        own_pid: u32,
        found: Vec<OnScreenWindow>,
    }

    pub fn list() -> Vec<OnScreenWindow> {
        let mut walk = Walk { own_pid: unsafe { GetCurrentProcessId() }, found: Vec::new() };
        // EnumWindows visits top-level windows in z-order, frontmost first.
        unsafe {
            let _ = EnumWindows(Some(visit), LPARAM(std::ptr::from_mut(&mut walk) as isize));
        }
        walk.found
    }

    unsafe extern "system" fn visit(hwnd: HWND, lp: LPARAM) -> BOOL {
        let walk = &mut *(lp.0 as *mut Walk);
        if let Some(w) = describe(hwnd, walk.own_pid) {
            walk.found.push(w);
        }
        BOOL::from(true)
    }

    unsafe fn describe(hwnd: HWND, own_pid: u32) -> Option<OnScreenWindow> {
        if !IsWindowVisible(hwnd).as_bool() || IsIconic(hwnd).as_bool() { return None; }
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == own_pid { return None; }
        if GetWindowLongW(hwnd, GWL_EXSTYLE) as u32 & WS_EX_TRANSPARENT.0 != 0 { return None; }
        let mut cloaked = 0u32;
        let got = DwmGetWindowAttribute(hwnd, DWMWA_CLOAKED, std::ptr::from_mut(&mut cloaked).cast(),
                                        std::mem::size_of::<u32>() as u32);
        if got.is_ok() && cloaked != 0 { return None; }
        let mut class = [0u16; 64];
        let n = GetClassNameW(hwnd, &mut class).max(0) as usize;
        let class = String::from_utf16_lossy(&class[..n]);
        // The desktop's own windows: the screen highlight stands for those.
        if class == "Progman" || class == "WorkerW" { return None; }
        let rect = frame_bounds(hwnd)?;
        let (w, h) = (rect.right - rect.left, rect.bottom - rect.top);
        if w < 8 || h < 8 { return None; }
        let mut title = [0u16; 256];
        let n = GetWindowTextW(hwnd, &mut title).max(0) as usize;
        Some(OnScreenWindow {
            rect: (rect.left, rect.top, w, h),
            title: String::from_utf16_lossy(&title[..n]),
            pid,
            hwnd: hwnd.0 as isize,
        })
    }

    pub fn app_name(pid: u32) -> String {
        use windows::core::PWSTR;
        use windows::Win32::Foundation::CloseHandle;
        use windows::Win32::System::Threading::{
            OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
        };
        if pid == 0 { return String::new(); }
        let path = unsafe {
            let Ok(h) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else { return String::new() };
            let mut buf = [0u16; 512];
            let mut len = buf.len() as u32;
            let ok = QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len).is_ok();
            let _ = CloseHandle(h);
            if !ok { return String::new(); }
            String::from_utf16_lossy(&buf[..len as usize])
        };
        let stem = std::path::Path::new(&path).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        if stem.eq_ignore_ascii_case("ApplicationFrameHost") { return String::new(); }
        let mut c = stem.chars();
        match c.next() {
            Some(f) => f.to_uppercase().chain(c).collect(),
            None => String::new(),
        }
    }

    /// The frame DWM draws, falling back to the window rect when DWM has
    /// none (it then includes the invisible resize border).
    unsafe fn frame_bounds(hwnd: HWND) -> Option<RECT> {
        let mut r = RECT::default();
        let dwm = DwmGetWindowAttribute(hwnd, DWMWA_EXTENDED_FRAME_BOUNDS, std::ptr::from_mut(&mut r).cast(),
                                        std::mem::size_of::<RECT>() as u32);
        if dwm.is_ok() && r.right > r.left && r.bottom > r.top { return Some(r); }
        GetWindowRect(hwnd, &mut r).ok()?;
        Some(r)
    }
}
