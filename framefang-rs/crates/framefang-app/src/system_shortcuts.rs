//! The operating system's own screenshot shortcut, and the settings page that
//! changes it.
//!
//! Windows 11 hands plain Print Screen to the Snipping Tool unless "Use the
//! Print screen key to open screen capture" is off, and when it is on a
//! Framefang binding on that key never fires. macOS and the Linux desktops have
//! their own screenshot shortcuts too. Settings shows one button that opens
//! the right page for the platform, where there is one.

use framefang_core::Hotkey;

/// Win32 `VK_SNAPSHOT`.
const VK_PRINT_SCREEN: u32 = 0x2C;

/// What the Settings line says and which page its button opens.
#[derive(Clone, Copy)]
pub struct Page {
    /// The quiet line beside the button.
    pub line: &'static str,
    pub button: &'static str,
    target: Target,
}

#[derive(Clone, Copy)]
enum Target {
    #[cfg_attr(not(windows), allow(dead_code))]
    WindowsKeyboard,
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    MacKeyboard,
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    Program(&'static str, &'static [&'static str]),
}

/// The page for this platform, or `None` where there is nothing to open (a
/// Linux desktop without a known shortcuts page).
pub fn page() -> Option<Page> {
    #[cfg(windows)]
    {
        Some(Page {
            line: "Windows' own Print Screen shortcut",
            button: "OPEN WINDOWS SETTINGS",
            target: Target::WindowsKeyboard,
        })
    }
    #[cfg(target_os = "macos")]
    {
        Some(Page {
            line: "macOS screenshot shortcuts",
            button: "OPEN MAC SETTINGS",
            target: Target::MacKeyboard,
        })
    }
    #[cfg(target_os = "linux")]
    {
        linux_page(&std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default())
    }
    #[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
    {
        None
    }
}

/// Which desktop's shortcuts page to open, from `XDG_CURRENT_DESKTOP`
/// (a colon-separated list such as `ubuntu:GNOME`).
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn linux_page(desktop: &str) -> Option<Page> {
    let has = |name: &str| desktop.split(':').any(|d| d.eq_ignore_ascii_case(name));
    let (line, target) = if has("GNOME") || has("Unity") || has("ubuntu") || has("Pantheon") {
        ("GNOME screenshot shortcuts", Target::Program("gnome-control-center", &["keyboard"]))
    } else if has("KDE") {
        ("Plasma screenshot shortcuts", Target::Program("systemsettings", &["kcm_keys"]))
    } else if has("X-Cinnamon") || has("Cinnamon") {
        ("Cinnamon screenshot shortcuts", Target::Program("cinnamon-settings", &["keyboard"]))
    } else if has("XFCE") {
        ("Xfce keyboard shortcuts", Target::Program("xfce4-keyboard-settings", &[]))
    } else {
        return None;
    };
    Some(Page { line, button: "OPEN SYSTEM SETTINGS", target })
}

impl Page {
    /// Windows' page, which Settings explains as a guide card rather than a
    /// one-line link.
    pub fn is_windows(&self) -> bool { matches!(self.target, Target::WindowsKeyboard) }

    pub fn open(&self) -> Result<(), String> {
        match self.target {
            Target::WindowsKeyboard => open_windows_keyboard(),
            Target::MacKeyboard => spawn("open", &["x-apple.systempreferences:com.apple.preference.keyboard?Shortcuts"]),
            Target::Program(cmd, args) => spawn(cmd, args),
        }
    }
}

#[cfg_attr(not(any(target_os = "macos", target_os = "linux")), allow(dead_code))]
fn spawn(cmd: &str, args: &[&str]) -> Result<(), String> {
    std::process::Command::new(cmd)
        .args(args)
        .spawn()
        .map(drop)
        .map_err(|e| format!("Couldn't open {cmd}: {e}"))
}

/// Where the Print Screen switch lives before newer builds moved it.
const PRINT_SCREEN_PAGE_OLD: &str = "ms-settings:easeofaccess-keyboard";
/// Bluetooth & devices > Keyboard, where newer Windows 11 builds put it.
const PRINT_SCREEN_PAGE_NEW: &str = "ms-settings:devices-keyboard";

/// The Settings page one index file (`AllSystemSettings_*.xml`) puts the
/// Print Screen switch on: the first `<PageID>` after the switch's entry.
/// `None` when the file lacks the switch or names its page some other way
/// (newer builds ship an extra index whose PageIDs are GUIDs; reading that
/// one as "the old page" sent the button to Accessibility on build 26300).
fn print_screen_page_in(index_xml: &str) -> Option<&'static str> {
    let at = index_xml.find("UsePrintScreenKeyForSnippingEnabled")?;
    let rest = &index_xml[at..];
    let page = &rest[rest.find("<PageID>")? + "<PageID>".len()..];
    if page.starts_with("SettingsPageDevicesKeyboard") {
        Some(PRINT_SCREEN_PAGE_NEW)
    } else if page.starts_with("SettingsPageEaseOfAccessKeyboard") {
        Some(PRINT_SCREEN_PAGE_OLD)
    } else {
        None
    }
}

/// Across every index file: Bluetooth & devices > Keyboard if any names it
/// (the switch moved there, and no file names it otherwise), else the
/// Accessibility page if one does, else the newer page, which is where
/// current Windows 11 keeps it.
fn print_screen_page_from<'a>(index_files: impl IntoIterator<Item = &'a str>) -> &'static str {
    let found: Vec<&'static str> = index_files.into_iter().filter_map(print_screen_page_in).collect();
    if found.contains(&PRINT_SCREEN_PAGE_NEW) || found.is_empty() {
        PRINT_SCREEN_PAGE_NEW
    } else {
        PRINT_SCREEN_PAGE_OLD
    }
}

/// Windows' settings index files (`AllSystemSettings_*.xml`), as text.
#[cfg(windows)]
fn index_files() -> Vec<String> {
    let root = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
    let dir = std::path::Path::new(&root).join("ImmersiveControlPanel").join("Settings");
    std::fs::read_dir(dir).into_iter().flatten().flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("AllSystemSettings_"))
        .filter_map(|e| std::fs::read_to_string(e.path()).ok())
        .collect()
}

#[cfg(windows)]
fn print_screen_page() -> &'static str {
    let files = index_files();
    print_screen_page_from(files.iter().map(String::as_str))
}

/// What Windows calls the Print Screen switch, in English. The real wording
/// is read from Windows itself (`switch_name`), in the user's language.
const SWITCH_NAME_EN: &str = "Use the Print screen key to open screen capture";

/// The resource reference Windows' index gives as the switch's name, e.g.
/// `@{windows?ms-resource://.../UsePrintScreenKeyForSnippingEnabled3/Description}`.
fn switch_name_resource(index_xml: &str) -> Option<&str> {
    let at = index_xml.find("UsePrintScreenKeyForSnippingEnabled")?;
    let rest = &index_xml[at..];
    let start = rest.find("<Description>")? + "<Description>".len();
    let end = start + rest[start..].find("</Description>")?;
    let value = rest[start..end].trim();
    (value.starts_with("@{") && value.contains("UsePrintScreenKeyForSnippingEnabled")).then_some(value)
}

/// The switch's name exactly as this Windows shows it (its language, its
/// build's wording), so the guide in Settings matches what the user sees.
pub fn switch_name() -> String {
    #[cfg(windows)]
    {
        use windows::core::HSTRING;
        use windows::Win32::UI::Shell::SHLoadIndirectString;
        for xml in index_files() {
            let Some(res) = switch_name_resource(&xml) else { continue };
            let mut buf = [0u16; 512];
            if unsafe { SHLoadIndirectString(&HSTRING::from(res), &mut buf, None) }.is_ok() {
                let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
                let name = String::from_utf16_lossy(&buf[..len]);
                if !name.trim().is_empty() { return name; }
            }
        }
    }
    SWITCH_NAME_EN.to_owned()
}

/// Where the switch lives, as a path through Windows Settings.
pub fn windows_page_path() -> &'static str {
    #[cfg(windows)]
    {
        if print_screen_page() == PRINT_SCREEN_PAGE_OLD { return "Accessibility > Keyboard"; }
    }
    "Bluetooth & devices > Keyboard"
}

#[cfg(windows)]
fn open_windows_keyboard() -> Result<(), String> {
    use windows::core::{w, HSTRING, PCWSTR};
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    let page = HSTRING::from(print_screen_page());
    // ShellExecute, not `cmd /c start`: a tray app spawning cmd flashes a
    // console window.
    let h = unsafe {
        ShellExecuteW(
            None,
            w!("open"),
            PCWSTR(page.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    // Values above 32 mean success.
    if h.0 as isize > 32 { Ok(()) } else { Err("Couldn't open Windows Settings".into()) }
}

#[cfg(not(windows))]
fn open_windows_keyboard() -> Result<(), String> {
    Err("not Windows".into())
}

/// True when the system itself takes `hk`, so a Framefang binding on it never
/// fires. Only Windows' Print Screen is detected; elsewhere this is `false`.
pub fn system_takes(hk: &Hotkey) -> bool {
    hk.virtual_key == VK_PRINT_SCREEN && hk.modifiers.is_empty() && windows_print_screen_opens_snipping()
}

/// `HKCU\Control Panel\Keyboard\PrintScreenKeyForSnippingEnabled`. When the
/// value is missing, Windows 11 22H2 and later treat it as on (the default
/// changed in 2023); earlier builds as off.
#[cfg(windows)]
fn windows_print_screen_opens_snipping() -> bool {
    use windows::core::w;
    use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};
    let mut value: u32 = 0;
    let mut size = std::mem::size_of::<u32>() as u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Control Panel\\Keyboard"),
            w!("PrintScreenKeyForSnippingEnabled"),
            RRF_RT_REG_DWORD,
            None,
            Some((&mut value as *mut u32).cast()),
            Some(&mut size),
        )
    };
    if status.is_ok() {
        value != 0
    } else {
        crate::recording_indicator::windows_build_number() >= 22621
    }
}

#[cfg(not(windows))]
fn windows_print_screen_opens_snipping() -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linux_desktops_map_to_their_shortcuts_page() {
        let line = |d: &str| linux_page(d).map(|p| p.line);
        assert_eq!(line("ubuntu:GNOME"), Some("GNOME screenshot shortcuts"));
        assert_eq!(line("KDE"), Some("Plasma screenshot shortcuts"));
        assert_eq!(line("X-Cinnamon"), Some("Cinnamon screenshot shortcuts"));
        assert_eq!(line("XFCE"), Some("Xfce keyboard shortcuts"));
        assert_eq!(line("sway"), None);
        assert_eq!(line(""), None);
    }

    #[test]
    fn the_button_opens_the_page_this_windows_keeps_the_switch_on() {
        // Shaped like the real index: the entry before belongs to another
        // page, the switch's own PageID comes after its name.
        let moved = "<PageID>SettingsPageEaseOfAccessAudio</PageID>\
            <Filename>AAA_SystemSettings_Accessibility_UsePrintScreenKeyForSnippingEnabled3</Filename>\
            <SettingIdentity><PageID>SettingsPageDevicesKeyboard</PageID></SettingIdentity>";
        let old = "<Filename>AAA_SystemSettings_Accessibility_UsePrintScreenKeyForSnippingEnabled3</Filename>\
            <SettingIdentity><PageID>SettingsPageEaseOfAccessKeyboard</PageID></SettingIdentity>";
        assert_eq!(print_screen_page_in(moved), Some(PRINT_SCREEN_PAGE_NEW));
        assert_eq!(print_screen_page_in(old), Some(PRINT_SCREEN_PAGE_OLD));
        assert_eq!(print_screen_page_in("<PageID>SettingsPageDevicesKeyboard</PageID>"), None);
    }

    #[test]
    fn the_switch_name_comes_from_its_own_index_entry() {
        let xml = "<Filename>AAA_SystemSettings_Accessibility_SoundSentry</Filename>\
            <Description>@{windows?ms-resource://X/SearchResources/SystemSettings_Accessibility_SoundSentry/Description}</Description>\
            <Filename>AAA_SystemSettings_Accessibility_UsePrintScreenKeyForSnippingEnabled3</Filename>\
            <PageID>SettingsPageDevicesKeyboard</PageID>\
            <Description>@{windows?ms-resource://X/SearchResources/SystemSettings_Accessibility_UsePrintScreenKeyForSnippingEnabled3/Description}</Description>";
        assert_eq!(switch_name_resource(xml),
            Some("@{windows?ms-resource://X/SearchResources/SystemSettings_Accessibility_UsePrintScreenKeyForSnippingEnabled3/Description}"));
        // Never another setting's name.
        assert_eq!(switch_name_resource("<Filename>UsePrintScreenKeyForSnippingEnabled</Filename><Description>plain</Description>"), None);
        assert!(!switch_name().trim().is_empty());
    }

    #[test]
    fn an_index_naming_pages_by_guid_does_not_decide_the_page() {
        // Build 26300 ships three index files; the first one read names the
        // switch's page by GUID. That must not win over the two that say
        // Bluetooth & devices > Keyboard.
        let guid = "<Filename>AAA_SystemSettings_Accessibility_UsePrintScreenKeyForSnippingEnabled3</Filename>\
            <SettingIdentity><PageID>{6E6DDBCB-9C89-434B-A994-D5F22239523B}</PageID></SettingIdentity>";
        let named = "<Filename>AAA_SystemSettings_Accessibility_UsePrintScreenKeyForSnippingEnabled3</Filename>\
            <SettingIdentity><PageID>SettingsPageDevicesKeyboard</PageID></SettingIdentity>";
        let old = "<Filename>UsePrintScreenKeyForSnippingEnabled</Filename>\
            <SettingIdentity><PageID>SettingsPageEaseOfAccessKeyboard</PageID></SettingIdentity>";
        assert_eq!(print_screen_page_in(guid), None);
        assert_eq!(print_screen_page_from([guid, named, named]), PRINT_SCREEN_PAGE_NEW);
        assert_eq!(print_screen_page_from([old]), PRINT_SCREEN_PAGE_OLD);
        assert_eq!(print_screen_page_from([guid]), PRINT_SCREEN_PAGE_NEW, "nothing named: today's page");
    }

    #[test]
    fn only_plain_print_screen_can_be_taken() {
        use framefang_core::Modifiers;
        let with = |m: Modifiers, vk: u32| Hotkey { modifiers: m, virtual_key: vk };
        // A modified chord or another key is never the system's.
        assert!(!system_takes(&with(Modifiers::CONTROL, VK_PRINT_SCREEN)));
        assert!(!system_takes(&with(Modifiers::empty(), 0x41)));
    }
}
