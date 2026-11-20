/*!
Fills an [`InstallProbe`] from the real machine and remembers the answer.

The decision itself is made in `framefang_core::install_channel`, where it can
be tested without touching the OS. This module only gathers the facts
(environment variables, the executable path, a few files on disk). None of
them change while the app runs, so they are read once and kept, even though
the Updates dialog asks every time it opens.
*/

use {
    framefang_core::install_channel::{detect_action, HostOs, InstallChannel, InstallProbe, UpdateAction},
    std::{
        path::{Path, PathBuf},
        sync::OnceLock,
    },
};

static RESULT: OnceLock<(InstallChannel, UpdateAction)> = OnceLock::new();

/// How this copy was installed, and what the updater may do about a newer
/// release.
pub fn detected() -> &'static (InstallChannel, UpdateAction) {
    RESULT.get_or_init(|| {
        let (channel, action) = detect_action(&gather());
        eprintln!("updates: install channel = {} ({:?})", channel.label(), action);
        (channel, action)
    })
}

/// Just the channel from [`detected`].
pub fn channel() -> InstallChannel {
    let (channel, _) = detected();
    *channel
}

/// `$APPIMAGE`: the `.AppImage` file this process was started from, which is
/// what an AppImage update overwrites. (`current_exe()` points inside the
/// read-only FUSE mount instead.)
pub fn appimage_path() -> Option<PathBuf> {
    env_value("APPIMAGE").map(PathBuf::from)
}

fn gather() -> InstallProbe {
    let os = HostOs::host();
    let probe = InstallProbe {
        os,
        exe_path: std::env::current_exe()
            .map(|exe| exe.to_string_lossy().into_owned())
            .unwrap_or_default(),
        snap_dir: env_value("SNAP"),
        flatpak_id: env_value("FLATPAK_ID"),
        flatpak_info_exists: Path::new("/.flatpak-info").exists(),
        appimage_path: env_value("APPIMAGE"),
        homebrew_cask_marker: os == HostOs::MacOs && caskroom_has_framefang(),
        os_release: if os == HostOs::Linux { os_release_text() } else { None },
    };
    probe
}

/// An environment variable, with empty treated the same as unset.
fn env_value(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

/// Homebrew keeps a cask's records in `<prefix>/Caskroom/framefang` and puts
/// the bundle in `/Applications`, where it looks hand-installed, so that
/// folder is the only way to tell. `$HOMEBREW_PREFIX` goes first when set,
/// then the Apple silicon and Intel defaults.
fn caskroom_has_framefang() -> bool {
    let custom = env_value("HOMEBREW_PREFIX").map(PathBuf::from);
    let defaults = [PathBuf::from("/opt/homebrew"), PathBuf::from("/usr/local")];
    custom
        .into_iter()
        .chain(defaults)
        .any(|prefix| prefix.join("Caskroom").join("framefang").exists())
}

/// The distro's os-release file, so the hint can name apt, dnf, zypper or an
/// AUR helper. `/usr/lib/os-release` is the fallback the spec allows for.
/// Missing or unreadable just means a generic hint.
fn os_release_text() -> Option<String> {
    ["/etc/os-release", "/usr/lib/os-release"]
        .into_iter()
        .find_map(|path| std::fs::read_to_string(path).ok())
}
