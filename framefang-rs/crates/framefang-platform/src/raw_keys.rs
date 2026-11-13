//! A second way to hear the capture keys on Windows: raw keyboard input.
//!
//! `RegisterHotKey` is checked late in Windows' keyboard path. A program that
//! holds a low-level keyboard hook (remote desktop and VM windows, some games
//! and their anti-cheat, other screenshot and macro tools) can swallow a key
//! before it gets there, and a game that registers raw input with
//! `RIDEV_NOHOTKEYS` turns every hotkey off while it is focused. Either way the
//! press never reaches us and Print Screen "does nothing in that app".
//!
//! Raw input with `RIDEV_INPUTSINK` is delivered to every listener, focused or
//! not, by a path those programs cannot suppress (it is how voice chat and
//! streaming apps hear their push-to-talk keys). This module listens there on
//! its own thread, matches the same bindings `RegisterHotKey` holds, and hands
//! each match to the hotkey manager, which keeps one press to one capture
//! whichever path reports it first.
//!
//! What it cannot hear: keys typed into an elevated (administrator) window,
//! which Windows withholds from non-elevated listeners. `RegisterHotKey` still
//! fires there, so the two together cover both.

use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};

use framefang_core::hotkeys::HotkeyAction;
use framefang_core::settings::{Hotkey, Modifiers};
use windows::core::w;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS,
    KEYEVENTF_KEYUP, VIRTUAL_KEY, VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT,
};
use windows::Win32::UI::Input::{
    GetRawInputData, RegisterRawInputDevices, HRAWINPUT, RAWINPUT, RAWINPUTDEVICE, RAWINPUTHEADER,
    RIDEV_INPUTSINK, RID_INPUT, RIM_TYPEKEYBOARD,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, RegisterClassW, HWND_MESSAGE,
    MSG, WINDOW_EX_STYLE, WINDOW_STYLE, WM_INPUT, WNDCLASSW,
};

/// The bindings to listen for. Only ones `RegisterHotKey` accepted are put
/// here, so a key the system owns stays the system's.
pub type Bindings = Arc<Mutex<Vec<(HotkeyAction, Hotkey)>>>;

/// `RAWKEYBOARD.Flags`: the key went up.
const RI_KEY_BREAK: u16 = 1;
/// A virtual key no keyboard sends. Raw input reports it for the fake shift
/// around some extended keys, and we inject it ourselves (see `take_focus`).
const VK_NONE: u16 = 0xFF;
/// Marks our own injected key so nothing mistakes it for typing.
const OURS: usize = 0x4652_4647; // "FRFG"

/// Start the listener. Returns the bindings to fill and the channel matches
/// arrive on, or `None` when raw input could not be set up (the hotkeys still
/// work through `RegisterHotKey`; this is a second path, not the only one).
pub fn start() -> Option<(Bindings, Receiver<HotkeyAction>)> {
    let bindings: Bindings = Arc::new(Mutex::new(Vec::new()));
    let (tx, rx) = mpsc::channel();
    let (ready_tx, ready_rx) = mpsc::channel();
    let shared = bindings.clone();
    std::thread::Builder::new()
        .name("framefang-raw-keys".into())
        .spawn(move || run(shared, tx, ready_tx))
        .ok()?;
    match ready_rx.recv() {
        Ok(true) => Some((bindings, rx)),
        _ => None,
    }
}

struct State {
    bindings: Bindings,
    tx: Sender<HotkeyAction>,
    /// Keys currently down and when each was last reported, so auto-repeat
    /// is one press. A key whose release we never saw (let go while an
    /// elevated window had the keyboard) would otherwise read as held for
    /// good; repeats arrive every few dozen ms after at most a second, so a
    /// "repeat" older than `STALE` is a new press.
    held: Vec<(u16, std::time::Instant)>,
}

/// Longer than Windows' longest key-repeat delay (one second).
const STALE: std::time::Duration = std::time::Duration::from_millis(1100);

thread_local! {
    static STATE: std::cell::RefCell<Option<State>> = const { std::cell::RefCell::new(None) };
}

fn run(bindings: Bindings, tx: Sender<HotkeyAction>, ready: Sender<bool>) {
    let hwnd = match unsafe { message_window() } {
        Ok(h) => h,
        Err(e) => {
            eprintln!("raw keys: no listener window ({e}); hotkeys use RegisterHotKey only");
            let _ = ready.send(false);
            return;
        }
    };
    let device = RAWINPUTDEVICE {
        usUsagePage: 0x01, // generic desktop
        usUsage: 0x06,     // keyboard
        dwFlags: RIDEV_INPUTSINK,
        hwndTarget: hwnd,
    };
    if let Err(e) = unsafe { RegisterRawInputDevices(&[device], std::mem::size_of::<RAWINPUTDEVICE>() as u32) } {
        eprintln!("raw keys: RegisterRawInputDevices failed ({e}); hotkeys use RegisterHotKey only");
        let _ = ready.send(false);
        return;
    }
    STATE.with(|s| *s.borrow_mut() = Some(State { bindings, tx, held: Vec::new() }));
    let _ = ready.send(true);

    let mut msg = MSG::default();
    while unsafe { GetMessageW(&mut msg, None, 0, 0) }.0 > 0 {
        unsafe { DispatchMessageW(&msg) };
    }
}

unsafe fn message_window() -> windows::core::Result<HWND> {
    let instance = unsafe { GetModuleHandleW(None) }?;
    let class = WNDCLASSW {
        lpfnWndProc: Some(wndproc),
        hInstance: instance.into(),
        lpszClassName: w!("FramefangRawKeys"),
        ..Default::default()
    };
    unsafe { RegisterClassW(&class) };
    unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0), w!("FramefangRawKeys"), w!(""), WINDOW_STYLE(0),
            0, 0, 0, 0, Some(HWND_MESSAGE), None, Some(instance.into()), None,
        )
    }
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == WM_INPUT {
        if let Some((vk, down)) = unsafe { read_key(lparam) } {
            on_key(vk, down);
        }
    }
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

/// The virtual key and direction of one raw keyboard event.
unsafe fn read_key(lparam: LPARAM) -> Option<(u16, bool)> {
    let mut raw = RAWINPUT::default();
    let mut size = std::mem::size_of::<RAWINPUT>() as u32;
    let got = unsafe {
        GetRawInputData(
            HRAWINPUT(lparam.0 as *mut core::ffi::c_void),
            RID_INPUT,
            Some(std::ptr::from_mut(&mut raw).cast()),
            &mut size,
            std::mem::size_of::<RAWINPUTHEADER>() as u32,
        )
    };
    if got == u32::MAX || got == 0 || raw.header.dwType != RIM_TYPEKEYBOARD.0 {
        return None;
    }
    let kb = unsafe { raw.data.keyboard };
    if kb.VKey == VK_NONE || kb.ExtraInformation as usize == OURS { return None; }
    Some((kb.VKey, kb.Flags & RI_KEY_BREAK == 0))
}

fn on_key(vk: u16, down: bool) {
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        let Some(state) = s.as_mut() else { return };
        let now = std::time::Instant::now();
        if !down {
            state.held.retain(|&(k, _)| k != vk);
            return;
        }
        if let Some(entry) = state.held.iter_mut().find(|(k, _)| *k == vk) {
            let repeat = now.duration_since(entry.1) < STALE;
            entry.1 = now;
            if repeat { return; }
        } else {
            state.held.push((vk, now));
        }
        let mods = held_modifiers();
        let hit = state.bindings.lock().ok().and_then(|b| {
            b.iter().find(|(_, hk)| matches(hk, vk, mods)).map(|(a, _)| *a)
        });
        if let Some(action) = hit {
            take_focus();
            let _ = state.tx.send(action);
        }
    });
}

/// Whether `vk` with `mods` held is exactly the binding `hk`.
pub fn matches(hk: &Hotkey, vk: u16, mods: Modifiers) -> bool {
    hk.virtual_key == vk as u32 && hk.modifiers == mods
}

fn held_modifiers() -> Modifiers {
    let down = |vk: VIRTUAL_KEY| unsafe { GetAsyncKeyState(vk.0 as i32) } as u16 & 0x8000 != 0;
    let mut m = Modifiers::empty();
    if down(VK_CONTROL) { m |= Modifiers::CONTROL; }
    if down(VK_MENU) { m |= Modifiers::ALT; }
    if down(VK_SHIFT) { m |= Modifiers::SHIFT; }
    if down(VK_LWIN) || down(VK_RWIN) { m |= Modifiers::SUPER; }
    m
}

/// Let the capture window come to the front. Windows only hands focus to the
/// process that received the last input, and a raw-input listener does not
/// count, so without this the overlay could open behind the focused app's
/// keyboard. A key no keyboard has, down and up, makes us that process and
/// means nothing to any program.
fn take_focus() {
    let key = |flags: KEYBD_EVENT_FLAGS| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT { wVk: VIRTUAL_KEY(VK_NONE), wScan: 0, dwFlags: flags, time: 0, dwExtraInfo: OURS },
        },
    };
    let inputs = [key(KEYBD_EVENT_FLAGS(0)), key(KEYEVENTF_KEYUP)];
    unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_binding_matches_only_its_exact_chord() {
        let prt = Hotkey { modifiers: Modifiers::empty(), virtual_key: 0x2C };
        assert!(matches(&prt, 0x2C, Modifiers::empty()));
        assert!(!matches(&prt, 0x2C, Modifiers::CONTROL), "Ctrl+PrtSc is a different chord");
        assert!(!matches(&prt, 0x2D, Modifiers::empty()));
        let chord = Hotkey { modifiers: Modifiers::CONTROL | Modifiers::SHIFT, virtual_key: 0x53 };
        assert!(matches(&chord, 0x53, Modifiers::CONTROL | Modifiers::SHIFT));
        assert!(!matches(&chord, 0x53, Modifiers::CONTROL));
    }
}
