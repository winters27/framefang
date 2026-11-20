//! What the tray loop asks of the desktop itself: toasts, closing the shell's
//! own popups, opening links, and reading whether a key is held.

use std::process::{Command, Stdio};

/// `CREATE_NO_WINDOW`: keeps Windows from flashing a console for every child
/// process this app starts.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Stop `cmd` from opening a console window. A no-op away from Windows.
pub(super) fn no_console_window(cmd: &mut Command) -> &mut Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// A command whose output is thrown away, for fire-and-forget helpers.
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
fn silent(program: &str) -> Command {
    let mut cmd = Command::new(program);
    cmd.stdout(Stdio::null()).stderr(Stdio::null());
    cmd
}

/// Show a desktop toast through whatever the OS offers (`notify-send`,
/// `osascript`, a PowerShell toast). Best effort: a missing helper means no
/// toast and no error. `urgent` keeps it up for 5 s instead of 3 s where the
/// OS lets us choose.
pub(super) fn notify(title: &str, body: &str, urgent: bool) {
    #[cfg(target_os = "linux")]
    {
        let ms = if urgent { "5000" } else { "3000" };
        let _ = silent("notify-send")
            .args(["-a", "Frame Fang", "-t", ms, title, body])
            .spawn();
    }
    #[cfg(target_os = "macos")]
    {
        let quote = |text: &str| text.replace('"', "\\\"");
        let script = format!(
            "display notification \"{}\" with title \"{}\"",
            quote(body),
            quote(title),
        );
        let _ = urgent;
        let _ = silent("osascript").args(["-e", &script]).spawn();
    }
    #[cfg(target_os = "windows")]
    {
        let _ = urgent;
        let quote = |text: &str| text.replace('\'', "''");
        let set_title = format!("$template.GetElementsByTagName('text').Item(0).InnerText = '{}';", quote(title));
        let set_body = format!("$template.GetElementsByTagName('text').Item(1).InnerText = '{}';", quote(body));
        let steps = [
            "[Windows.UI.Notifications.ToastNotificationManager,Windows.UI.Notifications,ContentType=WindowsRuntime] | Out-Null;",
            "[Windows.Data.Xml.Dom.XmlDocument,Windows.Data.Xml.Dom.XmlDocument,ContentType=WindowsRuntime] | Out-Null;",
            "$template = [Windows.UI.Notifications.ToastNotificationManager]::GetTemplateContent([Windows.UI.Notifications.ToastTemplateType]::ToastText02);",
            set_title.as_str(),
            set_body.as_str(),
            "[Windows.UI.Notifications.ToastNotificationManager]::CreateToastNotifier('Framefang').Show($template);",
        ];
        let script = steps.join(" ");
        let mut cmd = silent("powershell");
        cmd.args(["-NoProfile", "-Command", &script]);
        let _ = no_console_window(&mut cmd).spawn();
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        let _ = (title, body, urgent);
    }
}

/// Windows: close the tray's open popup menu and the hidden-icons flyout right
/// now, so the screen we are about to freeze does not have them in it. The
/// caller then waits a moment for the menu's fade to finish painting.
#[cfg(target_os = "windows")]
pub(super) fn dismiss_tray_flyouts() {
    use windows::core::w;
    use windows::Win32::UI::WindowsAndMessaging::{EndMenu, FindWindowW, ShowWindow, SW_HIDE};
    unsafe {
        // The tray menu is a popup menu of this thread once its click arrives.
        let _ = EndMenu();
        // Someone who reached our icon through the chevron has the overflow
        // flyout open; it stays until it is told to go. Absent is fine.
        let flyout = FindWindowW(w!("NotifyIconOverflowWindow"), None)
            .ok()
            .filter(|hwnd| !hwnd.is_invalid());
        if let Some(hwnd) = flyout {
            let _ = ShowWindow(hwnd, SW_HIDE);
        }
    }
}

/// The programs behind Windows' shell popups: Start, search, the notification
/// center and quick settings. Lowercase file names.
#[cfg(target_os = "windows")]
const SHELL_POPUP_HOSTS: [&str; 5] = [
    "startmenuexperiencehost.exe", "searchhost.exe", "searchapp.exe",
    "shellexperiencehost.exe", "shellhost.exe",
];

/// Whether a (lowercase) executable file name is one of the shell's popup hosts.
#[cfg(target_os = "windows")]
fn is_shell_popup_host(file: &str) -> bool {
    SHELL_POPUP_HOSTS.contains(&file)
}

/// The lowercase file name of the program that owns the foreground window.
#[cfg(target_os = "windows")]
fn foreground_program() -> Option<String> {
    use windows::core::PWSTR;
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};
    unsafe {
        let window = GetForegroundWindow();
        if window.is_invalid() {
            return None;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(window, Some(&mut pid));
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 260];
        let mut len = buf.len() as u32;
        let asked = QueryFullProcessImageNameW(process, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len);
        let _ = CloseHandle(process);
        asked.ok()?;
        let full = String::from_utf16_lossy(&buf[..len as usize]);
        Some(full.rsplit('\\').next().unwrap_or("").to_ascii_lowercase())
    }
}

/// Windows: if the Start menu (or search, the notification center, quick
/// settings) has the keyboard, press Esc to close it. These draw in a layer
/// above every always-on-top window, so the capture screen would open
/// underneath them. Called once the screen is frozen, so they are already in
/// the picture. Nothing is ever sent to any other window.
#[cfg(target_os = "windows")]
pub(super) fn dismiss_shell_popups() {
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, VK_ESCAPE,
    };
    let Some(program) = foreground_program() else { return };
    if !is_shell_popup_host(&program) {
        return;
    }
    let press = |flags| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: VK_ESCAPE, wScan: 0, dwFlags: flags, time: 0, dwExtraInfo: 0 } },
    };
    let esc_down_up = [press(Default::default()), press(KEYEVENTF_KEYUP)];
    unsafe {
        SendInput(&esc_down_up, std::mem::size_of::<INPUT>() as i32);
    }
}

/// Open a web link the way a double-click would. `false` when the system
/// could not be asked; the caller has already shown the address as text, so
/// the user can still copy it.
///
/// Windows goes through ShellExecute, never `cmd /c start`: starting cmd from
/// a tray app flashes a console window on every click.
pub(super) fn open_url(url: &str) -> bool {
    #[cfg(target_os = "windows")]
    {
        use windows::core::{w, HSTRING, PCWSTR};
        use windows::Win32::UI::Shell::ShellExecuteW;
        use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
        let target = HSTRING::from(url);
        let result = unsafe {
            ShellExecuteW(None, w!("open"), PCWSTR(target.as_ptr()), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL)
        };
        // Anything above 32 means ShellExecute launched it.
        let code = result.0 as isize;
        if code <= 32 {
            eprintln!("Couldn't open {url}: ShellExecute returned {code}");
        }
        code > 32
    }
    #[cfg(not(target_os = "windows"))]
    {
        let opener = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
        let started = Command::new(opener).arg(url).spawn();
        if let Err(e) = &started {
            eprintln!("Couldn't open {url}: {e}");
        }
        started.is_ok()
    }
}

/// Whether `key` is held right now, anywhere on the desktop. Only Windows can
/// be asked; elsewhere the capture overlay reads it as the drag is let go.
pub(super) fn switch_key_down(key: framefang_core::settings::SwitchKey) -> bool {
    #[cfg(windows)]
    {
        use windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
        // The top bit says the key is down at this instant.
        let state = unsafe { GetAsyncKeyState(key.vk() as i32) } as u16;
        state & 0x8000 != 0
    }
    #[cfg(not(windows))]
    {
        let _ = key;
        false
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_copied_pill_sits_under_the_picture_and_on_its_screen() {
        let screen = framefang_core::virtual_desktop::MonitorRect { x: 0, y: 0, width: 1920, height: 1080 };
        let (x, y) = crate::editor::toast_spot((100, 100, 400, 300), screen, (180, 32));
        assert_eq!((x, y), (100 + (400 - 180) / 2, 100 + 300 + 12));
        // With no room underneath it rides up over the picture, still on screen.
        let (_, y) = crate::editor::toast_spot((100, 900, 400, 170), screen, (180, 32));
        assert!(y + 32 <= 1080 - 12 && y < 900 + 170);
    }

    #[cfg(windows)]
    #[test]
    fn only_the_shell_popup_hosts_are_closed_with_esc() {
        assert!(super::is_shell_popup_host("startmenuexperiencehost.exe"));
        assert!(super::is_shell_popup_host("shellhost.exe"));
        assert!(!super::is_shell_popup_host("notepad.exe"));
        assert!(!super::is_shell_popup_host(""));
    }

    #[cfg(not(windows))]
    #[test]
    fn the_switch_key_is_never_reported_held_off_windows() {
        assert!(!super::switch_key_down(framefang_core::settings::SwitchKey::Shift));
    }
}
