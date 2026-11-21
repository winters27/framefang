/*!
Who Frame Fang's Windows toasts come from.

Windows files a toast under the identity (AppUserModelID) it is sent with.
It shows that identity's name and icon when it can tie the identity to an app,
which for an app without an MSIX package means a Start menu shortcut carrying
the ID (StreamNook's toasts show their name and logo that way). So:

- the installer's Start menu shortcut carries `APP_ID` (`main.wxs`, 1.7.1 on);
- the name and logo are also registered under
  `HKCU\Software\Classes\AppUserModelId\<ID>`, the logo written next to the
  settings, since Windows only reads icons that are files on disk.

Not settled (2026-11-20): on Brandon's machine, toasts in the newer generic
layout never displayed for this identity, so toasts use the plain two-line
template, and it is not yet confirmed that the installer's shortcut brings the
name and logo in.
*/

#![cfg(target_os = "windows")]

use std::path::{Path, PathBuf};

/// The identity every toast is sent under.
pub const APP_ID: &str = "winters27.FrameFang";

/// The logo for toasts, a file next to the settings.
fn logo_path() -> Option<PathBuf> {
    let settings = framefang_core::settings::AppSettings::settings_path()?;
    Some(settings.parent()?.join("toast-icon.png"))
}

/// Save the logo and register the name and icon. Cheap and idempotent, so
/// it runs at every start (which also follows a moved settings folder). Best
/// effort: a failure only costs toasts their name or logo, and is logged.
pub fn register() {
    match save_logo() {
        Ok(Some(logo)) => register_name(&logo),
        Ok(None) => {}
        Err(e) => eprintln!("toasts: couldn't save their logo ({e})"),
    }
}

fn save_logo() -> std::io::Result<Option<PathBuf>> {
    let Some(logo) = logo_path() else { return Ok(None) };
    if !std::fs::read(&logo).is_ok_and(|on_disk| on_disk == crate::brand_icon::BRAND_PNG) {
        if let Some(dir) = logo.parent() { std::fs::create_dir_all(dir)?; }
        std::fs::write(&logo, crate::brand_icon::BRAND_PNG)?;
    }
    Ok(Some(logo))
}

/// `DisplayName` and `IconUri` under the ID's key, as the Windows App SDK
/// registers an unpackaged app.
fn register_name(logo: &Path) {
    use windows::core::HSTRING;
    use windows::Win32::System::Registry::{RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ};
    let key = HSTRING::from(format!("Software\\Classes\\AppUserModelId\\{APP_ID}"));
    for (name, value) in [("DisplayName", "Frame Fang".to_owned()), ("IconUri", logo.display().to_string())] {
        let wide: Vec<u16> = value.encode_utf16().chain([0]).collect();
        let status = unsafe {
            RegSetKeyValueW(HKEY_CURRENT_USER, &key, &HSTRING::from(name), REG_SZ.0,
                            Some(wide.as_ptr().cast()), (wide.len() * 2) as u32)
        };
        if status.is_err() { eprintln!("toasts: couldn't register their {name} ({status:?})"); }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_installer_shortcut_carries_the_same_id() {
        let wxs = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../wix/main.wxs"));
        let property = format!(r#"<ShortcutProperty Key="System.AppUserModel.ID" Value="{}" />"#, super::APP_ID);
        assert!(wxs.contains(&property), "main.wxs must give its shortcut {}", super::APP_ID);
    }
}
