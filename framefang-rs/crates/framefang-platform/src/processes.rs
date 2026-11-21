//! The programs running right now, by file name.
//!
//! Only used to notice another screenshot tool that wants Print Screen
//! (`framefang_core::rivals`), which is a Windows story: elsewhere the
//! desktop owns the screenshot key, so the list is empty there.

/// The file names (like `Lightshot.exe`) of the running programs. Empty if
/// the list can't be read, or off Windows.
pub fn running_programs() -> Vec<String> {
    imp::running_programs()
}

#[cfg(target_os = "windows")]
mod imp {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
    };

    /// A snapshot of the process list: needs no rights over the processes,
    /// unlike opening each one to ask for its path.
    pub fn running_programs() -> Vec<String> {
        let Ok(snapshot) = (unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }) else { return Vec::new() };
        let mut names = Vec::new();
        let mut entry = PROCESSENTRY32W { dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        let mut more = unsafe { Process32FirstW(snapshot, &mut entry) }.is_ok();
        while more {
            let len = entry.szExeFile.iter().position(|&c| c == 0).unwrap_or(entry.szExeFile.len());
            names.push(String::from_utf16_lossy(&entry.szExeFile[..len]));
            more = unsafe { Process32NextW(snapshot, &mut entry) }.is_ok();
        }
        unsafe { let _ = CloseHandle(snapshot); }
        names
    }
}

#[cfg(not(target_os = "windows"))]
mod imp {
    pub fn running_programs() -> Vec<String> { Vec::new() }
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_list_has_this_test_in_it_on_windows() {
        let running = super::running_programs();
        if cfg!(target_os = "windows") {
            // This test binary is itself a running program.
            let me = std::env::current_exe().unwrap();
            let me = me.file_name().unwrap().to_string_lossy().to_string();
            assert!(running.iter().any(|n| n.eq_ignore_ascii_case(&me)), "{me} not in {} programs", running.len());
        } else {
            assert!(running.is_empty());
        }
    }
}
