//! What Frame Fang tells the user when a finished capture has nowhere to go.
//!
//! By the time any of these happen the overlay is gone, so a message on
//! stderr would never be seen and a failed save would pass for a good one.
//! The tray shows each of these as a toast instead. Keeping the text in one
//! place keeps the tone even across them and lets the tests below check it
//! without a desktop.
//!
//! Each message follows the same shape:
//!   1. say what went wrong, naming the folder or file when there is one,
//!   2. pass the operating system's reason through untouched, since that is
//!      usually the part that tells the user what to fix,
//!   3. finish with the single thing the user can do next.

/// Text for when the save folder can't be created or opened.
pub fn save_folder_failure(dir: &str, reason: &str) -> String {
    format!(
        "The save folder {dir} can't be used ({reason}), so this screenshot \
         wasn't saved. Choose another folder in Settings > FILES > \
         Screenshots folder and take it again."
    )
}

/// Text for when the folder is usable but writing the image file into it
/// failed.
pub fn save_write_failure(path: &str, reason: &str) -> String {
    format!(
        "Writing {path} failed ({reason}), so this screenshot wasn't saved. \
         Make sure the disk has room and the folder accepts new files, then \
         take it again."
    )
}

/// Text for when the clipboard rejects the image. Copying never writes a
/// file, so the user has to hear that the shot is not on disk either.
pub fn clipboard_failure(reason: &str) -> String {
    format!(
        "The clipboard wouldn't take the screenshot ({reason}). It wasn't \
         saved to disk either. Take it again and press Enter to put it in \
         your save folder."
    )
}

/// Text for when the window that pins a shot on screen can't be opened.
pub fn pin_failure(reason: &str) -> String {
    format!(
        "The screenshot couldn't be pinned on screen ({reason}). It wasn't \
         saved to disk either. Take it again and press Enter to put it in \
         your save folder."
    )
}

/// Text for when changed settings can't be written out. They stay in effect
/// until the app quits, and the user should know they won't come back.
pub fn settings_write_failure(reason: &str) -> String {
    format!(
        "Your settings couldn't be saved ({reason}). They work until Frame \
         Fang restarts, then they're gone. Check that the config folder \
         exists and can be written to."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One sample of every message, built with recognisable inputs.
    fn samples() -> Vec<(String, Vec<&'static str>)> {
        vec![
            (save_folder_failure("/srv/caps", "Access is denied"),
             vec!["/srv/caps", "Access is denied"]),
            (save_write_failure("/srv/caps/framefang_7.png", "Disk full"),
             vec!["/srv/caps/framefang_7.png", "Disk full"]),
            (clipboard_failure("clipboard is locked"), vec!["clipboard is locked"]),
            (pin_failure("window class missing"), vec!["window class missing"]),
            (settings_write_failure("config is read only"), vec!["config is read only"]),
        ]
    }

    /// The user can only act on a message that says which file or folder
    /// was involved and what the OS said about it.
    #[test]
    fn each_message_carries_its_path_and_os_reason() {
        for (text, needles) in samples() {
            for needle in needles {
                assert!(text.contains(needle), "{needle:?} missing from {text}");
            }
        }
    }

    /// Notification popups cut long bodies short, so the advice at the end
    /// has to survive inside a toast.
    #[test]
    fn each_message_fits_in_a_toast() {
        for (text, _) in samples() {
            assert!(text.len() < 240, "{} chars is too long for a toast: {text}", text.len());
        }
    }

    /// No path keeps the shot after one of these failures, so every message
    /// must say it was not saved rather than leave the user hunting for it.
    #[test]
    fn each_message_says_the_shot_was_not_kept() {
        for text in [
            save_folder_failure("/d", "r"),
            save_write_failure("/d/f.png", "r"),
            clipboard_failure("r"),
            pin_failure("r"),
        ] {
            assert!(text.contains("wasn't saved"), "{text}");
        }
        assert!(clipboard_failure("r").contains("saved to disk either"));
        assert!(pin_failure("r").contains("saved to disk either"));
    }
}
