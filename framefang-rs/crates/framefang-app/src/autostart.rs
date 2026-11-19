//! "Start with system" in Settings: Frame Fang starts when the user signs in.
//!
//! - **Windows:** a "Frame Fang" value under the user's
//!   `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`.
//! - **macOS:** `~/Library/LaunchAgents/io.github.winters27.framefang.plist`,
//!   run at load.
//! - **Linux:** `~/.config/autostart/framefang.desktop` (the XDG autostart
//!   folder every desktop reads). An AppImage registers the `.AppImage` file,
//!   not the read-only mount it runs from.
//!
//! Per user, no admin rights. The entry names the copy of Frame Fang that is
//! running when the setting is turned on.

use std::path::PathBuf;

/// Turn starting at sign-in on or off. Idempotent: turning on rewrites the
/// entry to name this copy, turning off when there is none is fine.
pub fn set(enabled: bool) -> Result<(), String> {
    let exe = program()?;
    set_os(enabled, &exe)
}

/// The setting is on but no entry exists (the file was edited by hand, or
/// the user removed it from the system's startup list): put it back. An
/// entry that is there is left alone, so a second copy of Frame Fang never
/// takes over another's entry just by running.
pub fn ensure(enabled: bool) -> Result<(), String> {
    if enabled && !exists() { set(true) } else { Ok(()) }
}

/// What to start: the AppImage file on Linux, else this executable.
fn program() -> Result<PathBuf, String> {
    if let Some(p) = crate::install_source::appimage_path() { return Ok(p); }
    std::env::current_exe().map_err(|e| format!("couldn't find Frame Fang's own file: {e}"))
}

// ── Windows ─────────────────────────────────────────────────────────────────

#[cfg(windows)]
const RUN_KEY: windows::core::PCWSTR = windows::core::w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
#[cfg(windows)]
const RUN_NAME: windows::core::PCWSTR = windows::core::w!("Frame Fang");

#[cfg(windows)]
fn set_os(enabled: bool, exe: &std::path::Path) -> Result<(), String> {
    use windows::Win32::Foundation::ERROR_FILE_NOT_FOUND;
    use windows::Win32::System::Registry::{RegDeleteKeyValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ};
    if enabled {
        let wide: Vec<u16> = run_command(exe).encode_utf16().chain(Some(0)).collect();
        let st = unsafe {
            RegSetKeyValueW(HKEY_CURRENT_USER, RUN_KEY, RUN_NAME, REG_SZ.0,
                            Some(wide.as_ptr().cast()), (wide.len() * 2) as u32)
        };
        st.ok().map_err(|e| format!("couldn't add Frame Fang to Windows startup: {e}"))
    } else {
        let st = unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, RUN_KEY, RUN_NAME) };
        if st == ERROR_FILE_NOT_FOUND { return Ok(()); }
        st.ok().map_err(|e| format!("couldn't remove Frame Fang from Windows startup: {e}"))
    }
}

#[cfg(windows)]
fn exists() -> bool {
    use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_SZ};
    let mut size = 0u32;
    unsafe { RegGetValueW(HKEY_CURRENT_USER, RUN_KEY, RUN_NAME, RRF_RT_REG_SZ, None, None, Some(&mut size)) }
        .is_ok()
}

/// The Run value: the path in quotes, since it usually has spaces.
#[cfg_attr(not(windows), allow(dead_code))]
fn run_command(exe: &std::path::Path) -> String {
    format!("\"{}\"", exe.display())
}

// ── macOS and Linux: a file in the user's startup folder ────────────────────

#[cfg(not(windows))]
fn entry_path() -> Result<PathBuf, String> {
    let home = directories::BaseDirs::new().ok_or("no home folder")?;
    Ok(if cfg!(target_os = "macos") {
        home.home_dir().join("Library/LaunchAgents/io.github.winters27.framefang.plist")
    } else {
        home.config_dir().join("autostart/framefang.desktop")
    })
}

#[cfg(not(windows))]
fn set_os(enabled: bool, exe: &std::path::Path) -> Result<(), String> {
    let path = entry_path()?;
    if !enabled {
        return match std::fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(format!("couldn't remove {}: {e}", path.display())),
            _ => Ok(()),
        };
    }
    let text = if cfg!(target_os = "macos") { launch_agent(exe) } else { desktop_entry(exe) };
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("couldn't make {}: {e}", dir.display()))?;
    }
    std::fs::write(&path, text).map_err(|e| format!("couldn't write {}: {e}", path.display()))
}

#[cfg(not(windows))]
fn exists() -> bool {
    entry_path().is_ok_and(|p| p.exists())
}

/// A LaunchAgent that starts `exe` when the user logs in.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn launch_agent(exe: &std::path::Path) -> String {
    let xml = |s: &str| s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n<dict>\n\
         \t<key>Label</key>\n\t<string>io.github.winters27.framefang</string>\n\
         \t<key>ProgramArguments</key>\n\t<array>\n\t\t<string>{}</string>\n\t</array>\n\
         \t<key>RunAtLoad</key>\n\t<true/>\n\
         </dict>\n</plist>\n",
        xml(&exe.to_string_lossy()))
}

/// An XDG autostart entry for `exe`. The path is quoted, with the characters
/// the Desktop Entry spec reserves inside quotes escaped.
#[cfg_attr(any(windows, target_os = "macos"), allow(dead_code))]
fn desktop_entry(exe: &std::path::Path) -> String {
    let mut quoted = String::from("\"");
    for ch in exe.to_string_lossy().chars() {
        if matches!(ch, '"' | '`' | '$' | '\\') { quoted.push('\\'); }
        quoted.push(ch);
    }
    quoted.push('"');
    format!("[Desktop Entry]\nType=Application\nName=Frame Fang\nExec={quoted}\nIcon=framefang\n\
             X-GNOME-Autostart-enabled=true\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn the_windows_command_is_quoted_for_paths_with_spaces() {
        assert_eq!(run_command(Path::new(r"C:\Program Files\Framefang\framefang.exe")),
                   r#""C:\Program Files\Framefang\framefang.exe""#);
    }

    #[test]
    fn the_linux_entry_quotes_and_escapes_the_path() {
        let e = desktop_entry(Path::new("/home/a b/Frame $Fang.AppImage"));
        assert!(e.contains("Exec=\"/home/a b/Frame \\$Fang.AppImage\"\n"), "{e}");
        assert!(e.starts_with("[Desktop Entry]\n"));
    }

    #[test]
    fn the_launch_agent_runs_at_load_and_escapes_xml() {
        let p = launch_agent(Path::new("/Applications/A&B.app/Contents/MacOS/framefang"));
        assert!(p.contains("<string>/Applications/A&amp;B.app/Contents/MacOS/framefang</string>"));
        assert!(p.contains("<key>RunAtLoad</key>\n\t<true/>"));
    }
}
