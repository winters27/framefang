//! What kind of desktop session Frame Fang is running in.
//!
//! The only question so far is "Wayland or not?", but several places ask it:
//! the recorder must not use `x11grab` under Wayland, the hotkey manager picks
//! the portal backend there, and the editor skips X11-only window tricks.
//! Guessing wrong fails quietly (XWayland gives `x11grab` black frames rather
//! than an error), so the answer lives in one place where it can be tested.

/// True when the desktop session is Wayland.
///
/// Either of two signals is enough:
///
/// * `XDG_SESSION_TYPE` says `wayland` (any case). The login manager sets this
///   and it is the authoritative answer when present.
/// * `WAYLAND_DISPLAY` names a compositor socket. This covers sessions where
///   the type is missing or wrong, such as nested compositors, `sudo` shells
///   and login managers that never set it.
///
/// On Windows and macOS this is always false (the environment is not even
/// read), so callers can test it without their own `cfg`.
pub fn is_wayland() -> bool {
    let var = |name: &str| std::env::var(name).ok();
    cfg!(target_os = "linux")
        && wayland_signals(var("XDG_SESSION_TYPE").as_deref(), var("WAYLAND_DISPLAY").as_deref())
}

/// The decision itself, separated from the environment so it can be tested
/// without touching process-wide variables. Surrounding whitespace is ignored
/// in both values, and a blank socket name does not count.
fn wayland_signals(session_type: Option<&str>, display: Option<&str>) -> bool {
    let says_wayland = session_type
        .is_some_and(|t| t.trim().eq_ignore_ascii_case("wayland"));
    let has_socket = display.is_some_and(|d| !d.trim().is_empty());
    says_wayland || has_socket
}

#[cfg(test)]
mod tests {
    use super::wayland_signals;

    #[test]
    fn nothing_set_is_not_wayland() {
        assert!(!wayland_signals(None, None));
    }

    #[test]
    fn session_type_is_matched_without_case() {
        assert!(wayland_signals(Some("wayland"), None));
        assert!(wayland_signals(Some("Wayland"), None));
        assert!(wayland_signals(Some(" WAYLAND\n"), None));
        assert!(!wayland_signals(Some("x11"), None));
        assert!(!wayland_signals(Some("tty"), None));
    }

    #[test]
    fn a_socket_alone_is_enough() {
        assert!(wayland_signals(Some("x11"), Some("wayland-0")));
        assert!(wayland_signals(None, Some("wayland-1")));
    }

    #[test]
    fn a_blank_socket_does_not_count() {
        // Some shells export the variable with nothing in it.
        assert!(!wayland_signals(Some("x11"), Some("")));
        assert!(!wayland_signals(None, Some("   ")));
    }

    /// One pass over the real environment, so the public function is known
    /// to read the variables the pure helper is tested on. The variables are
    /// shared by the whole process, which is why every case lives in this one
    /// test and the old values are put back at the end.
    #[cfg(target_os = "linux")]
    #[test]
    fn is_wayland_reads_the_environment() {
        use super::is_wayland;
        use std::env;

        let saved: Vec<(&str, Option<String>)> = ["XDG_SESSION_TYPE", "WAYLAND_DISPLAY"]
            .into_iter()
            .map(|name| (name, env::var(name).ok()))
            .collect();

        env::remove_var("XDG_SESSION_TYPE");
        env::remove_var("WAYLAND_DISPLAY");
        assert!(!is_wayland());

        env::set_var("XDG_SESSION_TYPE", "x11");
        assert!(!is_wayland());
        env::set_var("XDG_SESSION_TYPE", "Wayland");
        assert!(is_wayland());

        env::set_var("XDG_SESSION_TYPE", "x11");
        env::set_var("WAYLAND_DISPLAY", "");
        assert!(!is_wayland());
        env::set_var("WAYLAND_DISPLAY", "wayland-0");
        assert!(is_wayland());

        for (name, value) in saved {
            match value {
                Some(v) => env::set_var(name, v),
                None => env::remove_var(name),
            }
        }
    }
}
