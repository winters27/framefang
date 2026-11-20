/*!
Working out where this copy of Frame Fang came from, and therefore what the
Updates dialog may do once a newer release shows up.

Frame Fang reaches people through many routes: the release tarballs and
portable exe, the MSI (also used by winget and Chocolatey), the macOS `.dmg`,
an AppImage, Snap, Flatpak, Homebrew, distro packages (deb, rpm, AUR) and
Scoop. Some of those installs belong to us and can be replaced in place. The
rest belong to a package manager: swapping their files would be undone at the
next upgrade, would leave dpkg or rpm checksums lying, or would simply fail (a
Snap lives on a read-only squashfs). For those we tell the user which command
to run instead.

The classification only looks at what an [`InstallProbe`] carries (a few
environment variables, the running executable's path, two marker files and
`/etc/os-release`). Gathering the probe is the caller's job; everything in
here is pure, so every channel on every OS can be tested from any host.
*/

/// The operating systems Frame Fang is built for. It travels inside the probe
/// instead of being read with `cfg!` during detection, so a test on one OS can
/// walk the rules of the others.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum HostOs {
    Windows,
    MacOs,
    #[default]
    Linux,
}

impl HostOs {
    /// The OS this binary targets.
    pub fn host() -> Self {
        match () {
            _ if cfg!(windows) => Self::Windows,
            _ if cfg!(target_os = "macos") => Self::MacOs,
            _ => Self::Linux,
        }
    }
}

/// The observations detection works from. The app fills one from the real
/// machine; tests build them by hand.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct InstallProbe {
    pub os: HostOs,
    /// The running executable (`std::env::current_exe()`, lossy). For an
    /// AppImage this is a path inside the FUSE mount, not the image file.
    pub exe_path: String,
    /// `$SNAP`: root of the snap's read-only mount, such as
    /// `/snap/framefang/42`.
    pub snap_dir: Option<String>,
    /// `$APPIMAGE`: the `.AppImage` file itself, which is what an update
    /// replaces (not `exe_path`).
    pub appimage_path: Option<String>,
    /// The `$FLATPAK_ID` environment variable.
    pub flatpak_id: Option<String>,
    /// Whether `/.flatpak-info` exists; every Flatpak sandbox has one.
    pub flatpak_info_exists: bool,
    /// Whether Homebrew's Caskroom has a framefang entry. The cask copies a
    /// normal `Framefang.app` into `/Applications`, so this directory is the
    /// only sign that brew manages it.
    pub homebrew_cask_marker: bool,
    /// The text of `/etc/os-release`, used only to pick the distro's package
    /// manager command. Without it the hint stays generic.
    pub os_release: Option<String>,
}

/// The route by which this copy was installed, grouped by platform.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InstallChannel {
    /// Installed from `Framefang.msi`, whether run by hand, by winget or by
    /// Chocolatey.
    WindowsMsi,
    /// `Framefang-portable.exe` running from wherever it was saved (releases
    /// up to 1.3.0 shipped it inside `framefang-windows-x86_64.zip`).
    WindowsPortable,
    Scoop,
    /// A `Framefang.app` that no package manager claims, as from the `.dmg`.
    MacAppBundle,
    /// A macOS app bundle installed with `brew install --cask framefang`.
    HomebrewCask,
    /// A Homebrew formula: the binary sits under a `Cellar/` directory.
    HomebrewFormula,
    AppImage,
    Snap,
    Flatpak,
    /// A distro package (deb, rpm, AUR) that owns `/usr/bin/framefang`.
    LinuxSystemPackage,
    /// A loose binary from a release tarball or `install.sh`.
    Portable,
}

impl InstallChannel {
    /// A short name the Updates dialog can show.
    pub fn label(self) -> &'static str {
        match self {
            Self::WindowsMsi => "Windows installer",
            Self::WindowsPortable | Self::Portable => "portable build",
            Self::Scoop => "Scoop",
            Self::MacAppBundle => "app bundle",
            Self::HomebrewCask => "Homebrew cask",
            Self::HomebrewFormula => "Homebrew",
            Self::AppImage => "AppImage",
            Self::Snap => "Snap",
            Self::Flatpak => "Flatpak",
            Self::LinuxSystemPackage => "system package",
        }
    }

    /// True when a package manager owns the installed files, so the in-app
    /// updater must not replace them.
    pub fn is_package_managed(self) -> bool {
        package_manager_steps(self, None).is_some()
    }
}

/// The choice the Updates dialog gets to offer for a newer release.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum UpdateAction {
    /// Fetch the asset, check it, put it in place and restart.
    SelfInstall,
    /// Open the asset URL in the browser and let the user install it.
    BrowserDownload,
    /// A package manager owns this install. Show `hint`, and `command` too
    /// when there is one exact command to suggest.
    Managed {
        hint: &'static str,
        command: Option<String>,
    },
}

/// Decide which channel the probed install belongs to. No I/O happens here.
pub fn detect(probe: &InstallProbe) -> InstallChannel {
    let rules: fn(&InstallProbe) -> InstallChannel = match probe.os {
        HostOs::Windows => windows_channel,
        HostOs::MacOs => macos_channel,
        HostOs::Linux => linux_channel,
    };
    rules(probe)
}

/// What the updater is allowed to do for `channel` on the probed host.
pub fn update_action(channel: InstallChannel, probe: &InstallProbe) -> UpdateAction {
    if let Some((hint, command)) = package_manager_steps(channel, probe.os_release.as_deref()) {
        return UpdateAction::Managed { hint, command: command.map(str::to_owned) };
    }
    // Files we own outright can be replaced in place. The AppImage is one
    // file renamed over the old one; the MSI path hands off to msiexec so
    // Add/Remove Programs stays accurate; the portable exe moves the running
    // copy aside and drops the new one in its place; a Linux tarball install
    // is a plain binary swap. A loose binary anywhere else, like a
    // hand-unpacked macOS app, has a layout and a signing state we would only
    // be guessing at, so the user downloads the new release themselves.
    let in_place = match channel {
        InstallChannel::AppImage | InstallChannel::WindowsMsi | InstallChannel::WindowsPortable => true,
        InstallChannel::Portable => probe.os == HostOs::Linux,
        _ => false,
    };
    if in_place { UpdateAction::SelfInstall } else { UpdateAction::BrowserDownload }
}

/// Shorthand for [`detect`] followed by [`update_action`].
pub fn detect_action(probe: &InstallProbe) -> (InstallChannel, UpdateAction) {
    let found = detect(probe);
    (found, update_action(found, probe))
}

/// The hint and (when we can name it) command for a package-managed
/// channel, or `None` for a channel the updater owns. `os_release` only
/// matters for distro packages.
fn package_manager_steps(
    channel: InstallChannel,
    os_release: Option<&str>,
) -> Option<(&'static str, Option<&'static str>)> {
    const BREW: &str = "Installed with Homebrew - update with:";
    const DISTRO: &str = "Installed from a system package - update with:";

    let steps = match channel {
        InstallChannel::Snap => (
            "Installed from the Snap Store - update with:",
            Some("sudo snap refresh framefang"),
        ),
        InstallChannel::Flatpak => (
            "Installed as a Flatpak - update with:",
            Some("flatpak update org.framefang.Framefang"),
        ),
        InstallChannel::HomebrewCask => (BREW, Some("brew upgrade --cask framefang")),
        InstallChannel::HomebrewFormula => (BREW, Some("brew upgrade framefang")),
        InstallChannel::Scoop => {
            ("Installed with Scoop - update with:", Some("scoop update framefang"))
        }
        InstallChannel::LinuxSystemPackage => match package_tool(os_release) {
            Some(PackageTool::Apt) => {
                (DISTRO, Some("sudo apt update && sudo apt install --only-upgrade framefang"))
            }
            Some(PackageTool::Dnf) => (DISTRO, Some("sudo dnf upgrade framefang")),
            Some(PackageTool::Zypper) => (DISTRO, Some("sudo zypper update framefang")),
            Some(PackageTool::AurHelper) => (
                "Installed from the AUR - update with your AUR helper:",
                Some("yay -Syu framefang-bin"),
            ),
            None => (
                "Installed from a system package - update it with your package manager.",
                None,
            ),
        },
        InstallChannel::AppImage
        | InstallChannel::WindowsMsi
        | InstallChannel::WindowsPortable
        | InstallChannel::MacAppBundle
        | InstallChannel::Portable => return None,
    };
    Some(steps)
}

// Detection rules, one function per OS. Each list runs from the most
// specific sign to the least, and the first match wins.

fn linux_channel(p: &InstallProbe) -> InstallChannel {
    if runs_from_snap(p) {
        InstallChannel::Snap
    } else if p.flatpak_info_exists || filled(&p.flatpak_id) {
        InstallChannel::Flatpak
    } else if filled(&p.appimage_path) {
        InstallChannel::AppImage
    } else if in_brew_cellar(&p.exe_path) {
        // Linuxbrew uses the same <prefix>/Cellar/<formula>/<version> layout.
        InstallChannel::HomebrewFormula
    } else if under_distro_tree(&p.exe_path) {
        InstallChannel::LinuxSystemPackage
    } else {
        InstallChannel::Portable
    }
}

fn macos_channel(p: &InstallProbe) -> InstallChannel {
    if in_brew_cellar(&p.exe_path) {
        InstallChannel::HomebrewFormula
    } else if !p.exe_path.contains(".app/Contents/MacOS/") {
        InstallChannel::Portable
    } else if p.homebrew_cask_marker {
        // A cask-installed bundle is byte for byte a hand-installed one; the
        // Caskroom entry is the only difference.
        InstallChannel::HomebrewCask
    } else {
        InstallChannel::MacAppBundle
    }
}

fn windows_channel(p: &InstallProbe) -> InstallChannel {
    // Compare in one spelling: lowercase with backslashes.
    let exe = p.exe_path.to_ascii_lowercase().replace('/', "\\");
    let under = |tree: &str| exe.contains(tree);
    // Scoop unpacks into <scoop>\apps\framefang\<version>\. That is never
    // under Program Files, but testing it first keeps the order readable.
    // The MSI is per-machine, so it always lands in one of the Program Files
    // trees and nothing else of ours does; winget and Chocolatey run that
    // same MSI.
    if under("\\scoop\\apps\\") {
        InstallChannel::Scoop
    } else if under("\\program files\\") || under("\\program files (x86)\\") {
        InstallChannel::WindowsMsi
    } else {
        InstallChannel::WindowsPortable
    }
}

/// `Some` and non-empty. An empty environment variable counts as unset.
fn filled(value: &Option<String>) -> bool {
    value.as_deref().is_some_and(|s| !s.is_empty())
}

/// A snap's own binary is always under the /snap mount. `$SNAP` on its own
/// proves nothing, because every child of a snapped program inherits it (a
/// snapped terminal starting a tarball Frame Fang, say), so it only counts
/// when the executable sits inside that snap's root.
fn runs_from_snap(p: &InstallProbe) -> bool {
    let inside_snap_root = p
        .snap_dir
        .as_deref()
        .is_some_and(|root| !root.is_empty() && p.exe_path.starts_with(root));
    p.exe_path.starts_with("/snap/") || inside_snap_root
}

fn in_brew_cellar(exe: &str) -> bool {
    exe.contains("/Cellar/")
}

/// Trees a distro package owns. `/usr/local` is left out on purpose: it is
/// for software installed by hand, which is what `install.sh` produces when
/// run as root, and that binary is still ours to update.
fn under_distro_tree(exe: &str) -> bool {
    ["/usr/bin/", "/usr/sbin/", "/usr/lib/", "/usr/libexec/"]
        .iter()
        .any(|tree| exe.starts_with(tree))
}

// Reading /etc/os-release.

/// Which package manager a distro family upgrades with.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PackageTool {
    Apt,
    Dnf,
    AurHelper,
    Zypper,
}

/// Find the package manager from the `ID` and `ID_LIKE` keys of
/// `/etc/os-release`. Derivatives (Mint, Pop!_OS, Rocky, EndeavourOS and so
/// on) have their own `ID` and name their parent in `ID_LIKE`, so every word
/// of both keys is tried, in file order, until one is recognised.
fn package_tool(os_release: Option<&str>) -> Option<PackageTool> {
    os_release?
        .lines()
        .filter_map(|line| line.trim().split_once('='))
        .filter(|(key, _)| matches!(*key, "ID" | "ID_LIKE"))
        .flat_map(|(_, value)| value.trim().trim_matches('"').trim_matches('\'').split_whitespace())
        .find_map(|word| tool_for(&word.to_ascii_lowercase()))
}

fn tool_for(distro_id: &str) -> Option<PackageTool> {
    Some(match distro_id {
        "debian" | "ubuntu" | "linuxmint" | "pop" | "raspbian" => PackageTool::Apt,
        "fedora" | "rhel" | "centos" | "rocky" | "almalinux" => PackageTool::Dnf,
        "arch" | "archlinux" | "manjaro" | "endeavouros" => PackageTool::AurHelper,
        "opensuse" | "opensuse-tumbleweed" | "opensuse-leap" | "suse" | "sles" => PackageTool::Zypper,
        _ => return None,
    })
}

#[cfg(test)]
mod checks {
    use super::{
        detect, detect_action, package_tool, update_action, HostOs, InstallChannel as Ch,
        InstallProbe, PackageTool, UpdateAction as Act,
    };

    const EVERY_CHANNEL: [Ch; 11] = [
        Ch::WindowsMsi,
        Ch::WindowsPortable,
        Ch::Scoop,
        Ch::MacAppBundle,
        Ch::HomebrewCask,
        Ch::HomebrewFormula,
        Ch::AppImage,
        Ch::Snap,
        Ch::Flatpak,
        Ch::LinuxSystemPackage,
        Ch::Portable,
    ];

    fn probe(os: HostOs, exe: &str) -> InstallProbe {
        InstallProbe { os, exe_path: exe.into(), ..InstallProbe::default() }
    }

    fn on_linux(exe: &str) -> InstallProbe {
        probe(HostOs::Linux, exe)
    }

    fn on_windows(exe: &str) -> InstallProbe {
        probe(HostOs::Windows, exe)
    }

    fn on_mac(exe: &str) -> InstallProbe {
        probe(HostOs::MacOs, exe)
    }

    /// Run detection on a probe after letting `tweak` adjust it.
    fn channel_with(mut base: InstallProbe, tweak: impl FnOnce(&mut InstallProbe)) -> Ch {
        tweak(&mut base);
        detect(&base)
    }

    fn managed_command(p: &InstallProbe) -> Option<String> {
        match detect_action(p).1 {
            Act::Managed { command, .. } => command,
            other => panic!("expected a managed action, got {other:?}"),
        }
    }

    fn managed_hint(p: &InstallProbe) -> &'static str {
        match detect_action(p).1 {
            Act::Managed { hint, .. } => hint,
            other => panic!("expected a managed action, got {other:?}"),
        }
    }

    // Linux

    #[test]
    fn snap_is_found_by_its_mount_with_or_without_the_env_var() {
        let with_env = channel_with(on_linux("/snap/framefang/88/framefang"), |p| {
            p.snap_dir = Some("/snap/framefang/88".into());
        });
        assert_eq!(with_env, Ch::Snap);
        // Some wrappers drop $SNAP, but the binary is still on the snap mount.
        assert_eq!(detect(&on_linux("/snap/framefang/x1/framefang")), Ch::Snap);
    }

    #[test]
    fn snap_env_var_matches_an_exe_inside_that_snap_root() {
        // A snap mounted somewhere other than /snap still counts when the
        // binary lives under $SNAP.
        let found = channel_with(on_linux("/var/lib/snapd/snap/framefang/9/framefang"), |p| {
            p.snap_dir = Some("/var/lib/snapd/snap/framefang/9".into());
        });
        assert_eq!(found, Ch::Snap);
    }

    #[test]
    fn inherited_snap_env_var_does_not_make_a_tarball_a_snap() {
        let found = channel_with(on_linux("/home/ana/bin/framefang"), |p| {
            p.snap_dir = Some("/snap/alacritty/31".into());
        });
        assert_eq!(found, Ch::Portable);
    }

    #[test]
    fn empty_snap_env_var_is_ignored() {
        // An empty prefix would match every path, so it must count as unset.
        let found = channel_with(on_linux("/home/ana/bin/framefang"), |p| p.snap_dir = Some(String::new()));
        assert_eq!(found, Ch::Portable);
    }

    #[test]
    fn flatpak_info_file_means_flatpak() {
        let found = channel_with(on_linux("/app/bin/framefang"), |p| p.flatpak_info_exists = true);
        assert_eq!(found, Ch::Flatpak);
    }

    #[test]
    fn flatpak_id_means_flatpak_unless_empty() {
        let with_id = |id: &str| {
            let id = id.to_owned();
            channel_with(on_linux("/app/bin/framefang"), move |p| p.flatpak_id = Some(id))
        };
        assert_eq!(with_id("org.framefang.Framefang"), Ch::Flatpak);
        assert_eq!(with_id(""), Ch::Portable);
    }

    #[test]
    fn appimage_env_var_wins_over_the_usr_bin_inside_its_mount() {
        // The mounted image has usr/bin in its path, but not at the start, so
        // the distro-package rule must not claim it.
        let mounted = "/tmp/.mount_Frame9QxZ/usr/bin/framefang";
        let with_image = |image: &str| {
            let image = image.to_owned();
            channel_with(on_linux(mounted), move |p| p.appimage_path = Some(image))
        };
        assert_eq!(with_image("/home/ana/Applications/Framefang.AppImage"), Ch::AppImage);
        assert_eq!(with_image(""), Ch::Portable);
    }

    #[test]
    fn snap_outranks_flatpak_and_appimage_signs() {
        let found = channel_with(on_linux("/snap/framefang/7/framefang"), |p| {
            p.flatpak_info_exists = true;
            p.appimage_path = Some("/x.AppImage".into());
        });
        assert_eq!(found, Ch::Snap);
    }

    #[test]
    fn distro_owned_trees_mean_a_system_package() {
        for exe in [
            "/usr/bin/framefang",
            "/usr/sbin/framefang",
            "/usr/lib/framefang/framefang",
            "/usr/libexec/framefang/framefang",
        ] {
            assert_eq!(detect(&on_linux(exe)), Ch::LinuxSystemPackage, "{exe}");
        }
    }

    #[test]
    fn usr_local_stays_ours() {
        // Where install.sh puts the binary when run as root.
        assert_eq!(detect(&on_linux("/usr/local/bin/framefang")), Ch::Portable);
    }

    #[test]
    fn linuxbrew_cellar_is_a_formula() {
        let exe = "/home/linuxbrew/.linuxbrew/Cellar/framefang/1.6.2/bin/framefang";
        assert_eq!(detect(&on_linux(exe)), Ch::HomebrewFormula);
    }

    #[test]
    fn home_directory_binary_is_portable() {
        assert_eq!(detect(&on_linux("/home/ana/.local/bin/framefang")), Ch::Portable);
    }

    // macOS

    #[test]
    fn caskroom_marker_decides_cask_versus_plain_bundle() {
        let bundle = "/Applications/Frame Fang.app/Contents/MacOS/framefang";
        assert_eq!(detect(&on_mac(bundle)), Ch::MacAppBundle);
        assert_eq!(channel_with(on_mac(bundle), |p| p.homebrew_cask_marker = true), Ch::HomebrewCask);
    }

    #[test]
    fn mac_binary_outside_a_bundle_is_portable() {
        let loose = "/Users/ana/tools/framefang";
        assert_eq!(detect(&on_mac(loose)), Ch::Portable);
        // The cask marker alone does not turn a loose binary into a cask.
        assert_eq!(channel_with(on_mac(loose), |p| p.homebrew_cask_marker = true), Ch::Portable);
    }

    #[test]
    fn mac_cellar_is_a_formula() {
        let exe = "/usr/local/Cellar/framefang/1.5.0/bin/framefang";
        assert_eq!(detect(&on_mac(exe)), Ch::HomebrewFormula);
    }

    // Windows

    #[test]
    fn either_program_files_tree_means_the_msi() {
        for exe in [
            r"D:\Program Files\Frame Fang\framefang.exe",
            r"C:\Program Files (x86)\Frame Fang\framefang.exe",
        ] {
            assert_eq!(detect(&on_windows(exe)), Ch::WindowsMsi, "{exe}");
        }
    }

    #[test]
    fn windows_paths_are_compared_ignoring_case_and_slash_style() {
        assert_eq!(detect(&on_windows("c:/PROGRAM FILES/Framefang/framefang.exe")), Ch::WindowsMsi);
        assert_eq!(detect(&on_windows("C:/Users/ana/Scoop/Apps/framefang/1.0/framefang.exe")), Ch::Scoop);
    }

    #[test]
    fn windows_exe_anywhere_else_is_portable() {
        assert_eq!(detect(&on_windows(r"E:\portable\Framefang-portable.exe")), Ch::WindowsPortable);
    }

    #[test]
    fn scoop_apps_folder_means_scoop() {
        let exe = r"C:\Users\ana\scoop\apps\framefang\current\framefang.exe";
        assert_eq!(detect(&on_windows(exe)), Ch::Scoop);
    }

    // What the updater may do

    #[test]
    fn package_managed_channels_get_a_managed_action() {
        for channel in EVERY_CHANNEL {
            let action = update_action(channel, &on_linux("/usr/bin/framefang"));
            let is_managed = matches!(&action, Act::Managed { .. });
            assert_eq!(channel.is_package_managed(), is_managed, "{channel:?} gave {action:?}");
        }
        let managed = EVERY_CHANNEL.iter().filter(|c| c.is_package_managed()).count();
        assert_eq!(managed, 6);
    }

    #[test]
    fn snap_and_flatpak_suggest_their_own_commands() {
        let snap = InstallProbe { snap_dir: Some("/snap/framefang/5".into()), ..on_linux("/snap/framefang/5/framefang") };
        let want = Act::Managed {
            hint: "Installed from the Snap Store - update with:",
            command: Some("sudo snap refresh framefang".into()),
        };
        assert_eq!(detect_action(&snap), (Ch::Snap, want));

        let flatpak = InstallProbe { flatpak_info_exists: true, ..on_linux("/app/bin/framefang") };
        assert_eq!(managed_command(&flatpak).as_deref(), Some("flatpak update org.framefang.Framefang"));
    }

    #[test]
    fn brew_and_scoop_suggest_their_own_commands() {
        let cases = [
            (Ch::HomebrewCask, "Installed with Homebrew - update with:", "brew upgrade --cask framefang"),
            (Ch::HomebrewFormula, "Installed with Homebrew - update with:", "brew upgrade framefang"),
            (Ch::Scoop, "Installed with Scoop - update with:", "scoop update framefang"),
        ];
        for (channel, hint, command) in cases {
            let action = update_action(channel, &InstallProbe::default());
            assert_eq!(action, Act::Managed { hint, command: Some(command.into()) }, "{channel:?}");
        }
    }

    #[test]
    fn appimage_and_msi_update_themselves() {
        let image = InstallProbe {
            appimage_path: Some("/opt/Framefang.AppImage".into()),
            ..on_linux("/tmp/.mount_FrameQ2/usr/bin/framefang")
        };
        assert_eq!(detect_action(&image).1, Act::SelfInstall);
        let msi = on_windows(r"C:\Program Files\Framefang\framefang.exe");
        assert_eq!(detect_action(&msi).1, Act::SelfInstall);
    }

    #[test]
    fn portable_builds_update_themselves_except_on_macos() {
        let outcomes = [
            (on_linux("/home/ana/.local/bin/framefang"), Act::SelfInstall),
            // The portable exe has updated in place since 2026-11-19.
            (on_windows(r"E:\portable\Framefang-portable.exe"), Act::SelfInstall),
            (on_mac("/Applications/Frame Fang.app/Contents/MacOS/framefang"), Act::BrowserDownload),
            (on_mac("/Users/ana/tools/framefang"), Act::BrowserDownload),
        ];
        for (p, want) in outcomes {
            assert_eq!(detect_action(&p).1, want, "{}", p.exe_path);
        }
    }

    #[test]
    fn tarball_channel_on_windows_falls_back_to_the_browser() {
        // Windows detection never yields Portable, but if it is ever asked
        // for, only Linux may swap the binary itself.
        let action = update_action(Ch::Portable, &on_windows(r"C:\x\framefang.exe"));
        assert_eq!(action, Act::BrowserDownload);
    }

    #[test]
    fn distro_package_command_follows_os_release() {
        let command_for = |os_release: Option<&str>| {
            let p = InstallProbe { os_release: os_release.map(Into::into), ..on_linux("/usr/bin/framefang") };
            managed_command(&p)
        };

        let cases = [
            ("ID=pop\nID_LIKE=\"ubuntu debian\"\n", "sudo apt update && sudo apt install --only-upgrade framefang"),
            ("ID=\"almalinux\"\nID_LIKE=\"rhel centos fedora\"\n", "sudo dnf upgrade framefang"),
            ("ID=manjaro\nID_LIKE=arch\n", "yay -Syu framefang-bin"),
            ("ID=opensuse-leap\nID_LIKE=\"suse opensuse\"\n", "sudo zypper update framefang"),
            ("ID='Debian'\n", "sudo apt update && sudo apt install --only-upgrade framefang"),
        ];
        for (os_release, expected) in cases {
            assert_eq!(command_for(Some(os_release)).as_deref(), Some(expected), "{os_release:?}");
        }

        // An unrecognised or missing os-release still refuses to self-install,
        // it just cannot name the command.
        assert_eq!(command_for(Some("ID=gentoo\n")), None);
        assert_eq!(command_for(None), None);
    }

    #[test]
    fn unknown_distro_gets_the_generic_hint() {
        let p = on_linux("/usr/lib/framefang/framefang");
        assert_eq!(managed_hint(&p), "Installed from a system package - update it with your package manager.");
        assert_eq!(managed_command(&p), None);
    }

    #[test]
    fn aur_installs_get_the_aur_hint() {
        let p = InstallProbe { os_release: Some("ID=endeavouros\n".into()), ..on_linux("/usr/bin/framefang") };
        assert_eq!(managed_hint(&p), "Installed from the AUR - update with your AUR helper:");
    }

    #[test]
    fn os_release_reads_quoted_multi_word_id_like() {
        let text = "NAME=\"Pop!_OS\"\nID=pop\nID_LIKE=\"ubuntu debian\"\nVERSION_ID=\"22.04\"\n";
        assert_eq!(package_tool(Some(text)), Some(PackageTool::Apt));
    }

    #[test]
    fn os_release_skips_unknown_ids_and_other_keys() {
        // NAME and VERSION_ID are not consulted; the unknown ID falls through
        // to the parent named in ID_LIKE.
        let text = "NAME=fedora\nVERSION_ID=arch\nID=zorin\nID_LIKE=\"Ubuntu\"\n";
        assert_eq!(package_tool(Some(text)), Some(PackageTool::Apt));
        assert_eq!(package_tool(Some("PRETTY_NAME=ubuntu\n")), None);
    }

    #[test]
    fn labels_are_stable() {
        let labels: Vec<_> = EVERY_CHANNEL.iter().map(|c| c.label()).collect();
        assert_eq!(
            labels,
            [
                "Windows installer",
                "portable build",
                "Scoop",
                "app bundle",
                "Homebrew cask",
                "Homebrew",
                "AppImage",
                "Snap",
                "Flatpak",
                "system package",
                "portable build",
            ]
        );
    }

    #[test]
    fn host_os_follows_the_compile_target() {
        // Checked against the standard library's own idea of the target.
        let built_for = match std::env::consts::OS {
            "windows" => HostOs::Windows,
            "macos" => HostOs::MacOs,
            _ => HostOs::Linux,
        };
        assert_eq!([HostOs::host(), HostOs::default()], [built_for, HostOs::Linux]);
    }
}
