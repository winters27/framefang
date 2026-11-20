/*!
Updating Frame Fang in place: download a release asset, check its SHA-256,
put the new build where the running one is, and restart.

The work runs on its own thread so the event loop keeps going; the Updates
dialog holds the `JoinHandle` and watches it.

How each install is replaced:

- **Linux and macOS.** POSIX allows renaming a new file over an executable
  that is running: the old process keeps its inode until it exits. So the new
  binary is renamed over the old one and started.
- **Windows.** A running `.exe` cannot be deleted or overwritten, but it can
  be renamed. The running `framefang.exe` becomes `framefang.exe.old`, the new
  build takes its name, the new build is started and this process exits. The
  next launch deletes the `.old` file once nothing holds it.
- **MSI installs** are handed to `msiexec`, so Windows Installer replaces the
  files and Add/Remove Programs shows the new version.
- **AppImages** are a single file that is the whole install. The file to
  replace is `$APPIMAGE`, not `current_exe()` (that points into the read-only
  FUSE mount, which goes away with this process).

What the downloaded asset can be:

- `.tar.gz` / `.tgz` (Linux and the macOS update payload): unpacked with the
  system `tar`, and the first file named `framefang` inside is used.
- `.zip` (older Windows releases): unpacked with Windows' own `tar.exe` (or
  `unzip` elsewhere), and `framefang.exe` inside is used.
- `.msi`: run, never unpacked.
- `.AppImage`: used as it is.
- Anything else, such as `Framefang-portable.exe`: the download is the binary.

Package-managed installs (Snap, Flatpak, Homebrew, distro packages, Scoop) are
refused here even though the dialog already hides the button for them, so no
future caller can write into a tree a package manager owns.
`framefang_core::install_channel` decides which installs those are.

The SHA-256 is checked on the downloaded file itself, before anything is
unpacked or installed, because the release's `SHA256SUMS` lists hashes of the
published files. A release with no `SHA256SUMS` can still be installed
without a hash, with a warning in the log; in practice the dialog always finds
one.
*/

use {
    sha2::{Digest as _, Sha256},
    std::{
        fmt::Write as _,
        io::Read as _,
        path::{Path, PathBuf},
        process::Command,
        thread::JoinHandle,
    },
};

/// Set on the process the updater starts, so `main` waits for the
/// single-instance lock to come free instead of treating the outgoing
/// process as a second launch. Nothing else reads it, and it does no harm if
/// a child process inherits it.
pub const RELAUNCH_ENV: &str = "FRAMEFANG_RELAUNCHED_BY_UPDATER";

/// `CREATE_NO_WINDOW`: stops console programs (curl, cmd, tar) started from
/// this windowless tray app from flashing a console.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Start the download, check, install and restart on a worker thread and
/// return its handle straight away; the caller polls `is_finished` and then
/// joins. When everything works the thread never returns, because once the
/// new build has been started this process exits, so an `Ok(())` would only
/// mean the handover happened and an `Err` carries the message to show.
pub fn download_and_install(
    asset_url: String, expected_sha256: Option<String>, on_progress: impl Fn(u64, Option<u64>) + Send + 'static,
) -> JoinHandle<Result<(), String>> {
    std::thread::spawn(move || install(&asset_url, expected_sha256.as_deref(), &on_progress))
}

fn install(url: &str, expected_sha256: Option<&str>, report: &dyn Fn(u64, Option<u64>)) -> Result<(), String> {
    let installed = crate::install_source::channel();
    if installed.is_package_managed() {
        let label = installed.label();
        let why = format!("this Frame Fang was installed as {} {label}; update it with that package manager instead.", article(label));
        return Err(why);
    }

    let asset = temp_path_for_url(url);
    download(url, &asset, report)?;
    if let Some(hash) = expected_sha256 {
        verify_sha256(&asset, hash)?;
    } else {
        eprintln!("self-updater: no SHA-256 was given for {}; installing it unchecked", asset.display());
    }

    match AssetKind::of(url) {
        AssetKind::Msi => launch_msi_and_exit(&asset),
        AssetKind::AppImage => {
            let image = crate::install_source::appimage_path()
                .ok_or("an AppImage update was picked, but this run has no $APPIMAGE (it is not an AppImage)")?;
            allow_execute(&asset)?;
            replace_binary(&asset, &image)?;
            relaunch_and_exit(&image)
        }
        AssetKind::Binary => {
            let new_build = extract_binary(&asset)?;
            allow_execute(&new_build)?;
            let running = std::env::current_exe().map_err(|e| format!("couldn't find the running executable: {e}"))?;
            replace_binary(&new_build, &running)?;
            relaunch_and_exit(&running)
        }
    }
}

/// "a" or "an" for a channel label, so the refusal reads naturally.
fn article(label: &str) -> &'static str {
    match label.chars().next() {
        Some(first) if "aeiouAEIOU".contains(first) => "an",
        _ => "a",
    }
}

/// How an asset gets installed, judged from the file name in its URL.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AssetKind {
    /// Run through msiexec.
    Msi,
    /// Renamed over `$APPIMAGE`.
    AppImage,
    /// An archive to unpack, or the binary itself.
    Binary,
}

impl AssetKind {
    fn of(url: &str) -> Self {
        let name = asset_file_name(url).to_ascii_lowercase();
        if name.ends_with(".msi") {
            AssetKind::Msi
        } else if name.ends_with(".appimage") {
            AssetKind::AppImage
        } else {
            AssetKind::Binary
        }
    }
}

/// The last path segment of `url`, without any query string.
fn asset_file_name(url: &str) -> &str {
    let segment = url.rsplit_once('/').map_or(url, |(_, last)| last);
    segment.split_once('?').map_or(segment, |(name, _)| name)
}

// Downloading

/// Where `url` is downloaded to. The name depends only on the URL, so a
/// retry overwrites a half-finished earlier attempt instead of piling up
/// files, and the Updates dialog can watch the file grow to show progress.
pub(crate) fn temp_path_for_url(url: &str) -> PathBuf {
    let digest = to_hex(&Sha256::digest(url.as_bytes()));
    // 16 hex digits (64 bits) is far more than enough to keep URLs apart.
    let tag = &digest[..16];
    std::env::temp_dir().join(format!("framefang-update-{tag}{}", download_suffix(url)))
}

/// The extension to give the downloaded file. It decides how
/// `extract_binary` treats the file later, so it must stay in step with that.
fn download_suffix(url: &str) -> &'static str {
    let name = asset_file_name(url);
    // Archive extensions are matched as published (lowercase).
    if let Some(archive) = ARCHIVE_SUFFIXES.into_iter().find(|ext| name.ends_with(ext)) {
        return archive;
    }
    let lower = name.to_ascii_lowercase();
    if lower.ends_with(".msi") {
        ".msi"
    } else if lower.ends_with(".exe") {
        // The portable Windows build is published as the exe itself.
        ".exe"
    } else if lower.ends_with(".appimage") {
        // The file becomes the installed AppImage, and some desktop
        // integrations go by the extension.
        ".AppImage"
    } else {
        ".bin"
    }
}

const TARBALL_SUFFIXES: [&str; 2] = [".tar.gz", ".tgz"];
const ARCHIVE_SUFFIXES: [&str; 3] = [TARBALL_SUFFIXES[0], TARBALL_SUFFIXES[1], ".zip"];

/// Fetch `url` into `dest` with `curl`, like the rest of the app does, rather
/// than carrying an HTTP client. `-f` turns HTTP errors into a failure, `-L`
/// follows GitHub's redirect to its CDN, `-sS` keeps curl quiet except for
/// errors. There is no streaming progress from curl here: the dialog shows a
/// busy state (it can also watch the file size) and gets the final byte count
/// once curl is done.
fn download(url: &str, dest: &Path, report: &dyn Fn(u64, Option<u64>)) -> Result<(), String> {
    report(0, None);
    // A partial file from an earlier failed try must not survive. It may not
    // exist, which is fine.
    std::fs::remove_file(dest).ok();

    let mut curl = Command::new("curl");
    curl.args(["-fsSL", "-A", "framefang-self-updater", "--retry", "2", "--retry-delay", "1", "-o"])
        .arg(dest)
        .arg(url);
    run_tool(&mut curl, &format!("downloading {url}"))?;

    let size = std::fs::metadata(dest)
        .map(|meta| meta.len())
        .map_err(|e| format!("the download finished but {} can't be read: {e}", dest.display()))?;
    report(size, Some(size));
    Ok(())
}

/// Stop `cmd` from opening a console window on Windows (a no-op elsewhere).
/// Every helper program the updater starts goes through this.
pub(crate) fn hide_console(cmd: &mut Command) -> &mut Command {
    #[cfg(windows)]
    std::os::windows::process::CommandExt::creation_flags(cmd, CREATE_NO_WINDOW);
    cmd
}

/// Run a helper program (curl, tar, unzip) to the end without a console
/// window. `doing` says what it was for, for the error message.
fn run_tool(cmd: &mut Command, doing: &str) -> Result<(), String> {
    let tool = Path::new(cmd.get_program()).display().to_string();
    match hide_console(cmd).status() {
        Ok(exit) if exit.success() => Ok(()),
        Ok(exit) => Err(format!("{tool} failed ({exit}) while {doing}")),
        Err(e) => Err(format!("couldn't start {tool} for {doing}: {e}")),
    }
}

// Unpacking

/// The new Frame Fang binary inside `download`. Archives are unpacked into a
/// folder beside the download and searched; any other file is taken to be the
/// binary itself (the portable exe, or an asset shape we do not know yet).
fn extract_binary(download: &Path) -> Result<PathBuf, String> {
    let name = download.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let wanted = if TARBALL_SUFFIXES.iter().any(|ext| name.ends_with(ext)) {
        let folder = fresh_unpack_folder(download)?;
        let mut tar = Command::new("tar");
        tar.arg("-xzf").arg(download).arg("-C").arg(&folder);
        run_tool(&mut tar, &format!("unpacking {}", download.display()))?;
        (folder, "framefang")
    } else if name.ends_with(".zip") {
        let folder = fresh_unpack_folder(download)?;
        unzip(download, &folder)?;
        (folder, if cfg!(windows) { "framefang.exe" } else { "framefang" })
    } else {
        return Ok(download.to_path_buf());
    };
    let (folder, binary) = wanted;
    find_named(&folder, binary).ok_or_else(|| format!("{} has no `{binary}` in it", download.display()))
}

/// `<download stem>.extracted` next to the download, emptied first so the
/// search afterwards cannot find a binary left by an earlier attempt.
fn fresh_unpack_folder(download: &Path) -> Result<PathBuf, String> {
    let folder = unpack_folder_for(download);
    std::fs::remove_dir_all(&folder).ok();
    std::fs::create_dir_all(&folder)
        .map_err(|e| format!("couldn't create the folder {} to unpack into: {e}", folder.display()))?;
    Ok(folder)
}

fn unpack_folder_for(download: &Path) -> PathBuf {
    let stem = download.file_stem().and_then(|s| s.to_str()).unwrap_or("framefang-update");
    download.with_file_name(format!("{stem}.extracted"))
}

#[cfg(windows)]
fn unzip(archive: &Path, into: &Path) -> Result<(), String> {
    unzip_windows(archive, into)
}

#[cfg(not(windows))]
fn unzip(archive: &Path, into: &Path) -> Result<(), String> {
    let mut unzip = Command::new("unzip");
    unzip.arg("-o").arg(archive).arg("-d").arg(into);
    run_tool(&mut unzip, &format!("unpacking {}", archive.display()))
}

/// Unpack a zip on Windows with the `tar.exe` in System32 (there since
/// Windows 10 1803, and it reads zips), without a console window.
///
/// PowerShell's Expand-Archive is not used because it can fail silently:
/// started from a PowerShell 7 terminal, Windows PowerShell 5.1 inherits 7's
/// module path, loads 7's Expand-Archive, fails inside it and still exits 0,
/// so the download looked empty ("no ffmpeg.exe in it"). Found 2026-11-13.
#[cfg(windows)]
pub(crate) fn unzip_windows(zip: &Path, to: &Path) -> Result<(), String> {
    std::fs::create_dir_all(to).map_err(|e| format!("couldn't create {}: {e}", to.display()))?;
    // By full path, so some other tar earlier on PATH is never picked up.
    let windows_dir = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
    let mut tar = Command::new(Path::new(&windows_dir).join("System32").join("tar.exe"));
    tar.arg("-xf").arg(zip).arg("-C").arg(to);
    run_tool(&mut tar, &format!("unpacking {}", zip.display()))
}

/// The first file called `want` under `root`, searching depth first in
/// directory order. Release archives hold a handful of files, so a plain
/// recursive walk is plenty.
pub(crate) fn find_named(root: &Path, want: &str) -> Option<PathBuf> {
    std::fs::read_dir(root).ok()?.flatten().find_map(|entry| {
        let candidate = entry.path();
        if candidate.is_dir() {
            return find_named(&candidate, want);
        }
        let matches = candidate.is_file() && candidate.file_name().is_some_and(|name| name == want);
        matches.then_some(candidate)
    })
}

// Checking the download

/// Hash `path` with SHA-256 and compare it with `expected_hex`, ignoring
/// case and surrounding whitespace in the expected value.
pub(crate) fn verify_sha256(path: &Path, expected_hex: &str) -> Result<(), String> {
    let shown = path.display();
    let mut file = std::fs::File::open(path).map_err(|e| format!("couldn't open {shown} to check its hash: {e}"))?;
    let mut digest = Sha256::default();
    let mut chunk = vec![0u8; 64 * 1024];
    while let filled @ 1.. = file.read(&mut chunk).map_err(|e| format!("couldn't read {shown} while hashing it: {e}"))? {
        digest.update(&chunk[..filled]);
    }

    let actual = to_hex(&digest.finalize());
    let expected = expected_hex.trim().to_ascii_lowercase();
    if actual == expected {
        Ok(())
    } else {
        Err(format!("{shown} is damaged or not the published file: its SHA-256 is {actual}, but the release lists {expected}"))
    }
}

/// Lowercase hex, two digits per byte.
fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::with_capacity(bytes.len() * 2), |mut hex, byte| {
        let _ = write!(hex, "{byte:02x}");
        hex
    })
}

/// The hash for `want_filename` in a `SHA256SUMS` file, in lowercase. Reads
/// the GNU `sha256sum` layout (`<hash>  <name>`, or `<hash> *<name>` in binary
/// mode) and also the BSD `shasum --tag` layout (`SHA256 (<name>) = <hash>`)
/// in case the release job ever switches. Blank lines, `#` comments, CRLF
/// endings and extra spaces are all fine. A line only counts if its hash is
/// 64 hex digits.
pub fn parse_sha256sums(text: &str, want_filename: &str) -> Option<String> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .find_map(|line| {
            gnu_sums_entry(line)
                .into_iter()
                .chain(bsd_sums_entry(line))
                .find(|&(name, hash)| name == want_filename && is_sha256_hex(hash))
                .map(|(_, hash)| hash.to_ascii_lowercase())
        })
}

/// `(name, hash)` from a GNU line: the hash, whitespace, then the name with
/// an optional `*` in front.
fn gnu_sums_entry(line: &str) -> Option<(&str, &str)> {
    let (hash, rest) = line.split_once(char::is_whitespace)?;
    Some((rest.trim_start().trim_start_matches('*').trim(), hash))
}

/// `(name, hash)` from a BSD line, `SHA256 (<name>) = <hash>`.
fn bsd_sums_entry(line: &str) -> Option<(&str, &str)> {
    let (name, after) = line.strip_prefix("SHA256 (")?.split_once(')')?;
    let (_, hash) = after.split_once('=')?;
    Some((name, hash.trim()))
}

fn is_sha256_hex(candidate: &str) -> bool {
    candidate.len() == 64 && candidate.chars().all(|c| c.is_ascii_hexdigit())
}

// Putting the new build in place

/// Make `file` executable (`rwxr-xr-x`) on Unix. Windows decides what can run
/// from the extension and the PE header, not a permission bit, so there it
/// does nothing.
fn allow_execute(file: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mut mode = std::fs::metadata(file)
            .map(|meta| meta.permissions())
            .map_err(|e| format!("couldn't read the permissions of {}: {e}", file.display()))?;
        mode.set_mode(0o755);
        std::fs::set_permissions(file, mode).map_err(|e| format!("couldn't mark {} executable: {e}", file.display()))?;
    }
    #[cfg(not(unix))]
    let _ = file;
    Ok(())
}

/// POSIX: rename the new binary over `target`.
///
/// `rename` only works within one filesystem, and the download sits in the
/// temp folder (often a tmpfs) while the install is usually under `$HOME`, so
/// a direct rename would fail with EXDEV. The new build is first copied to
/// `<target>.new` beside the target, which is the same filesystem, and that
/// copy is renamed over the target in one step. The `.new` file is removed if
/// anything fails.
#[cfg(not(windows))]
fn replace_binary(new_build: &Path, target: &Path) -> Result<(), String> {
    let beside = target.with_extension("new");
    std::fs::copy(new_build, &beside).map_err(|e| {
        format!("couldn't copy {} next to {} as {}: {e}", new_build.display(), target.display(), beside.display())
    })?;
    let placed = allow_execute(&beside).and_then(|()| {
        std::fs::rename(&beside, target)
            .map_err(|e| format!("couldn't move {} into place as {}: {e}", beside.display(), target.display()))
    });
    if placed.is_err() {
        std::fs::remove_file(&beside).ok();
    }
    placed
}

/// Windows: move the running exe aside to `<exe>.old` (allowed even while it
/// runs), then move the new build into its place. If the second move fails
/// the first is undone, as far as possible, so the user is not left without a
/// Frame Fang. The `.old` file is deleted on the next launch by
/// [`cleanup_stale_old_binary`].
#[cfg(windows)]
fn replace_binary(new_build: &Path, target: &Path) -> Result<(), String> {
    let aside = aside_path(target);
    move_over(target, &aside)?;
    move_over(new_build, target).inspect_err(|_| {
        let _ = move_over(&aside, target);
    })
}

/// `<exe>.old`, beside the exe so the move stays on one volume (where
/// `MoveFileExW` is atomic).
#[cfg(windows)]
fn aside_path(exe: &Path) -> PathBuf {
    let mut name = exe.as_os_str().to_owned();
    name.push(".old");
    name.into()
}

#[cfg(windows)]
fn move_over(from: &Path, to: &Path) -> Result<(), String> {
    use {
        std::os::windows::ffi::OsStrExt as _,
        windows::{core::PCWSTR, Win32::Storage::FileSystem::{MoveFileExW, MOVEFILE_REPLACE_EXISTING}},
    };

    let wide = |p: &Path| -> Vec<u16> { p.as_os_str().encode_wide().chain([0]).collect() };
    let (source, dest) = (wide(from), wide(to));
    // SAFETY: both buffers are NUL-terminated UTF-16 and outlive the call.
    let moved = unsafe { MoveFileExW(PCWSTR(source.as_ptr()), PCWSTR(dest.as_ptr()), MOVEFILE_REPLACE_EXISTING) };
    moved.map_err(|e| format!("couldn't move {} to {}: {e}", from.display(), to.display()))
}

/// At startup: after a Windows update the previous build was left as
/// `<exe>.old`, and now that its process has exited it can be deleted. If it
/// is somehow still locked it stays for the next launch to try again. Linux
/// and macOS leave nothing behind.
pub fn cleanup_stale_old_binary() {
    #[cfg(windows)]
    if let Ok(exe) = std::env::current_exe() {
        std::fs::remove_file(aside_path(&exe)).ok();
    }
}

// Restarting

/// Start the freshly installed build and exit, so it can take over the tray.
/// On Unix a spawned child is already independent enough, and Rust does not
/// wait for it on drop; on Windows the tray app has no console for the child
/// to inherit. The child starts while this process still holds the
/// single-instance lock, so [`RELAUNCH_ENV`] tells it to wait for the
/// handover rather than quit as a duplicate.
fn relaunch_and_exit(binary: &Path) -> ! {
    if let Err(e) = Command::new(binary).env(RELAUNCH_ENV, "1").spawn() {
        // The new build is already in place, so the next manual start gets
        // it; all that is lost is the automatic restart.
        eprintln!("self-updater: the update is installed but restarting failed ({e}); open Frame Fang again");
    }
    std::process::exit(0)
}

/// Run the downloaded MSI and exit.
///
/// It runs `/passive`: no wizard and no "ready to install?" question, since
/// the user already chose to update, just Windows Installer's progress bar
/// (and one elevation prompt, as the MSI installs to Program Files). The
/// MSI's MajorUpgrade replaces the old version and keeps Add/Remove Programs
/// correct, which swapping the exe by hand would not. A hidden cmd waits for
/// the installer and then starts Frame Fang again whether the install went
/// ahead or was declined. This process exits almost at once so the installer
/// can replace its exe.
#[cfg(windows)]
fn launch_msi_and_exit(msi: &Path) -> Result<(), String> {
    use std::os::windows::process::CommandExt as _;

    let exe = std::env::current_exe().map_err(|e| format!("couldn't find the running executable: {e}"))?;
    let mut runner = Command::new("cmd");
    runner.raw_arg(msi_update_command(msi, &exe)).env(RELAUNCH_ENV, "1").creation_flags(CREATE_NO_WINDOW);
    if let Err(e) = runner.spawn() {
        return Err(format!("couldn't start the Windows installer: {e}"));
    }
    std::thread::sleep(std::time::Duration::from_millis(300));
    std::process::exit(0)
}

/// Only Windows has MSI installs; reaching this elsewhere means the asset
/// picker chose the wrong file.
#[cfg(not(windows))]
fn launch_msi_and_exit(_msi: &Path) -> Result<(), String> {
    Err("an MSI update was picked, but MSI installers only run on Windows".to_owned())
}

/// The arguments for the hidden cmd: install `msi` with only a progress bar,
/// wait for it, then start `exe`. With `/s`, cmd strips only the outermost
/// pair of quotes after `/c`, so the quoted paths inside survive.
#[cfg_attr(not(windows), allow(dead_code))]
fn msi_update_command(msi: &Path, exe: &Path) -> String {
    format!(
        r#"/s /c "start "" /wait msiexec /i "{}" /passive /norestart & start "" "{}"""#,
        msi.display(),
        exe.display()
    )
}

#[cfg(test)]
mod checks {
    use super::{
        article, asset_file_name, download_suffix, extract_binary, find_named, msi_update_command,
        parse_sha256sums, temp_path_for_url, to_hex, unpack_folder_for, verify_sha256, AssetKind, Path, PathBuf,
        Sha256,
    };
    use sha2::Digest as _;

    const LINUX_HASH: &str = "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08";
    const WINDOWS_HASH: &str = "7777777777777777777777777777777777777777777777777777777777777777";

    /// A folder of our own under the temp dir, removed when dropped.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir()
                .join(format!("framefang-self-updater-test-{tag}-{}", std::process::id()));
            std::fs::remove_dir_all(&dir).ok();
            std::fs::create_dir_all(&dir).unwrap();
            Scratch(dir)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).ok();
        }
    }

    #[test]
    fn msi_command_line_is_exact() {
        let line = msi_update_command(Path::new(r"D:\tmp\next.msi"), Path::new(r"D:\apps\framefang.exe"));
        assert_eq!(
            line,
            r#"/s /c "start "" /wait msiexec /i "D:\tmp\next.msi" /passive /norestart & start "" "D:\apps\framefang.exe"""#
        );
    }

    #[test]
    fn msi_update_installs_quietly_then_reopens_frame_fang() {
        let msi = r"C:\Users\Jo Doe\AppData\Local\Temp\framefang-update-ab12.msi";
        let exe = r"C:\Program Files\Frame Fang\framefang.exe";
        let line = msi_update_command(Path::new(msi), Path::new(exe));
        assert!(line.starts_with("/s /c \"") && line.ends_with("\"\""), "{line}");
        assert!(line.contains(&format!(r#"start "" /wait msiexec /i "{msi}""#)), "{line}");
        assert!(line.contains("/passive /norestart"), "{line}");
        // Reopens even when the user declined the elevation prompt.
        assert!(line.contains(&format!(r#"& start "" "{exe}""#)), "{line}");
    }

    #[test]
    fn sums_gnu_layout_finds_each_file() {
        let mac_hash = "ab".repeat(32);
        let sums = format!(
            "{LINUX_HASH}  Framefang-linux-x86_64-portable.tar.gz\n\
             {WINDOWS_HASH}  Framefang-portable.exe\n\
             {mac_hash}  Framefang-macos-arm64.dmg\n"
        );
        assert_eq!(parse_sha256sums(&sums, "Framefang-linux-x86_64-portable.tar.gz").as_deref(), Some(LINUX_HASH));
        assert_eq!(parse_sha256sums(&sums, "Framefang-portable.exe").as_deref(), Some(WINDOWS_HASH));
        assert_eq!(parse_sha256sums(&sums, "Framefang.msi"), None);
    }

    #[test]
    fn sums_accept_crlf_and_the_binary_mode_star() {
        let hash = "0f1e2d3c".repeat(8);
        let sums = format!("{hash} *Framefang.msi\r\n");
        assert_eq!(parse_sha256sums(&sums, "Framefang.msi"), Some(hash));
    }

    #[test]
    fn sums_bsd_layout_is_lowercased() {
        let sums = format!("SHA256 (Framefang-portable.exe) = {}\n", "BEEF".repeat(16));
        assert_eq!(parse_sha256sums(&sums, "Framefang-portable.exe"), Some("beef".repeat(16)));
        assert_eq!(parse_sha256sums(&sums, "Framefang.msi"), None);
    }

    #[test]
    fn sums_skip_comments_and_blank_lines() {
        let hash = "fedcba98".repeat(8);
        let sums = format!("# release v1.6.2\n\n \t \n{hash}  Framefang.msi\n");
        assert_eq!(parse_sha256sums(&sums, "Framefang.msi"), Some(hash));
    }

    #[test]
    fn sums_ignore_hashes_of_the_wrong_length_or_alphabet() {
        assert_eq!(parse_sha256sums("cafe  Framefang.msi\n", "Framefang.msi"), None);
        let not_hex = "z".repeat(64);
        assert_eq!(parse_sha256sums(&format!("{not_hex}  Framefang.msi\n"), "Framefang.msi"), None);
        // A bad line for the same name does not hide a good one later.
        let sums = format!("cafe  Framefang.msi\n{LINUX_HASH}  Framefang.msi\n");
        assert_eq!(parse_sha256sums(&sums, "Framefang.msi").as_deref(), Some(LINUX_HASH));
    }

    #[test]
    fn sums_need_an_exact_name_match() {
        let sums = format!("{LINUX_HASH}  Framefang.msi\n");
        assert_eq!(parse_sha256sums(&sums, "Framefang"), None);
        assert_eq!(parse_sha256sums(&sums, "Framefang.MSI"), None);
    }

    #[test]
    fn hex_is_lowercase_two_digits_per_byte() {
        assert_eq!(to_hex(&[0x01, 0xAB, 0xFF]), "01abff");
        assert_eq!(to_hex(&[0x7F]), "7f");
        assert!(to_hex(&[]).is_empty());
    }

    #[test]
    fn download_suffix_follows_the_asset_name() {
        let cases = [
            ("https://dl.example/Framefang-linux-x86_64-portable.tar.gz", ".tar.gz"),
            ("https://dl.example/framefang-windows-x86_64.zip?sig=9", ".zip"),
            // The macOS update payload is a tarball too, so it gets unpacked.
            ("https://dl.example/framefang-macos-x86_64-update.tar.gz", ".tar.gz"),
            ("https://dl.example/framefang-arm64.tgz", ".tgz"),
            ("https://dl.example/Framefang.msi", ".msi"),
            ("https://dl.example/FRAMEFANG.Msi", ".msi"),
            ("https://dl.example/Framefang-portable.exe", ".exe"),
            ("https://dl.example/Framefang-portable.Exe?t=2", ".exe"),
            // Anything unknown is an opaque blob.
            ("https://dl.example/framefang-macos-x86_64", ".bin"),
            // Archive extensions are case-sensitive, unlike the others.
            ("https://dl.example/framefang.TGZ", ".bin"),
        ];
        for (url, suffix) in cases {
            assert_eq!(download_suffix(url), suffix, "{url}");
        }
    }

    #[test]
    fn appimage_assets_keep_their_extension_and_are_recognised() {
        assert_eq!(download_suffix("https://dl.example/Framefang-linux-x86_64.AppImage"), ".AppImage");
        let kinds = [
            ("https://github.com/o/r/releases/download/v1.6.2/Framefang-linux-x86_64.AppImage", AssetKind::AppImage),
            ("https://dl.example/framefang.APPIMAGE?sig=1", AssetKind::AppImage),
            ("https://dl.example/Framefang-linux-x86_64-portable.tar.gz", AssetKind::Binary),
            ("https://dl.example/Framefang.msi", AssetKind::Msi),
        ];
        for (url, kind) in kinds {
            assert_eq!(AssetKind::of(url), kind, "{url}");
        }
    }

    #[test]
    fn msi_assets_are_recognised_by_name() {
        for url in [
            "https://github.com/o/r/releases/download/v1.6.2/Framefang.msi",
            "https://dl.example/Framefang.Msi",
            "https://dl.example/framefang.msi?sig=7",
        ] {
            assert_eq!(AssetKind::of(url), AssetKind::Msi, "{url}");
        }
        for url in [
            "https://dl.example/framefang-windows-x86_64.zip",
            "https://dl.example/Framefang-linux-x86_64-portable.tar.gz",
            "https://dl.example/framefang-macos-x86_64-update.tar.gz",
            "https://dl.example/Framefang-portable.exe",
        ] {
            assert_eq!(AssetKind::of(url), AssetKind::Binary, "{url}");
        }
    }

    #[test]
    fn asset_file_name_drops_path_and_query() {
        assert_eq!(asset_file_name("https://h/p/q/Framefang.msi?a=1&b=2"), "Framefang.msi");
        assert_eq!(asset_file_name("Framefang.msi"), "Framefang.msi");
        assert_eq!(asset_file_name("https://h/p/"), "");
    }

    #[test]
    fn refusal_uses_the_right_article() {
        assert_eq!(article("AppImage"), "an");
        assert_eq!(article("Homebrew cask"), "a");
        assert_eq!(article("system package"), "a");
        assert_eq!(article(""), "a");
    }

    #[test]
    fn temp_path_is_stable_per_url_and_keeps_the_suffix() {
        let url = "https://dl.example/v1.6.2/Framefang-portable.exe";
        let path = temp_path_for_url(url);
        assert_eq!(path, temp_path_for_url(url));
        assert_ne!(path, temp_path_for_url("https://dl.example/v1.6.3/Framefang-portable.exe"));
        assert_eq!(path.parent(), Some(std::env::temp_dir().as_path()));

        let name = path.file_name().unwrap().to_str().unwrap();
        let tag = name.strip_prefix("framefang-update-").unwrap().strip_suffix(".exe").unwrap();
        assert_eq!(tag.len(), 16);
        assert_eq!(tag, &to_hex(&Sha256::digest(url.as_bytes()))[..16]);
    }

    #[test]
    fn unpack_folder_sits_beside_the_download() {
        let download = Path::new("/var/tmp/framefang-update-ffee.tgz");
        assert_eq!(unpack_folder_for(download), Path::new("/var/tmp/framefang-update-ffee.extracted"));
    }

    #[test]
    fn unknown_shapes_are_the_binary_itself() {
        for blob in ["/var/tmp/framefang-update-ffee.exe", "/var/tmp/framefang-update-ffee.bin"] {
            assert_eq!(extract_binary(Path::new(blob)), Ok(PathBuf::from(blob)));
        }
    }

    #[test]
    fn find_named_searches_subfolders_for_a_file() {
        let scratch = Scratch::new("find");
        let deep = scratch.0.join("release").join("usr").join("bin");
        std::fs::create_dir_all(&deep).unwrap();
        // A folder with the wanted name is not a match.
        std::fs::create_dir_all(scratch.0.join("share").join("framefang")).unwrap();
        std::fs::write(deep.join("framefang"), b"elf").unwrap();
        std::fs::write(scratch.0.join("LICENSE"), b"text").unwrap();

        assert_eq!(find_named(&scratch.0, "framefang"), Some(deep.join("framefang")));
        assert_eq!(find_named(&scratch.0, "framefang.exe"), None);
        assert_eq!(find_named(&scratch.0.join("absent"), "framefang"), None);
    }

    #[test]
    fn verify_sha256_accepts_the_right_hash_in_any_case() {
        let scratch = Scratch::new("verify");
        let file = scratch.0.join("download.bin");
        std::fs::write(&file, b"test").unwrap();
        // SHA-256("test"), a well-known value.
        assert_eq!(verify_sha256(&file, LINUX_HASH), Ok(()));
        assert_eq!(verify_sha256(&file, &format!("\t{}  ", LINUX_HASH.to_uppercase())), Ok(()));

        let wrong = verify_sha256(&file, WINDOWS_HASH).unwrap_err();
        assert!(wrong.contains(&format!("its SHA-256 is {LINUX_HASH}")), "{wrong}");
        assert!(wrong.ends_with(&format!("the release lists {WINDOWS_HASH}")), "{wrong}");

        let missing = verify_sha256(&scratch.0.join("gone.bin"), LINUX_HASH).unwrap_err();
        assert!(missing.starts_with("couldn't open ") && missing.contains("to check its hash"), "{missing}");
    }
}
