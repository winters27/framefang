//! Downloading Framefang's own FFmpeg, for people who don't have one.
//!
//! Recording and video conversion run FFmpeg. Releases don't bundle it: an
//! FFmpeg the user already has (on `PATH`) always wins, and only when there
//! is none does Framefang offer this download (Windows; elsewhere FFmpeg
//! comes from the system's package manager).
//!
//! What it fetches is the trimmed build `.github/workflows/ffmpeg.yml`
//! publishes to this repo's releases under [`FFMPEG_TAG`]: a zip holding one
//! static `ffmpeg.exe`, checked against the release's `SHA256SUMS` before
//! anything is unpacked, then installed to
//! `framefang_platform::recorder::downloaded_ffmpeg_path()`, which the
//! shared FFmpeg lookup checks last.
//!
//! The download runs on its own thread; any window reads its progress with
//! [`state`].

use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// The release holding Framefang's FFmpeg build.
pub const FFMPEG_TAG: &str = "ffmpeg-7.1.2";
const REPO: &str = "winters27/framefang";
const ASSET: &str = "framefang-ffmpeg-windows-x86_64.zip";

/// Where a download stands.
#[derive(Debug, Clone, PartialEq)]
pub enum Fetch {
    Idle,
    Downloading { got: u64, total: Option<u64> },
    Installing,
    Done,
    Failed(String),
}

static STATE: Mutex<Fetch> = Mutex::new(Fetch::Idle);

fn set(s: Fetch) {
    if let Ok(mut g) = STATE.lock() { *g = s; }
}

/// The current download state.
pub fn state() -> Fetch {
    STATE.lock().map(|g| g.clone()).unwrap_or(Fetch::Idle)
}

/// Whether a download is running.
pub fn busy() -> bool {
    matches!(state(), Fetch::Downloading { .. } | Fetch::Installing)
}

/// Whether Framefang can download FFmpeg on this platform.
pub fn offered() -> bool {
    cfg!(target_os = "windows")
}

/// What to tell someone who has no FFmpeg, per platform.
pub fn missing_message() -> &'static str {
    if cfg!(target_os = "windows") {
        "FFmpeg isn't installed. Download it in Frame Fang's Settings (Captures > FFmpeg)."
    } else if cfg!(target_os = "macos") {
        "FFmpeg isn't installed. Install it with: brew install ffmpeg"
    } else {
        "FFmpeg isn't installed. Install it with your package manager, e.g. sudo apt install ffmpeg"
    }
}

/// Start downloading, unless a download is already running.
pub fn start() {
    if !offered() || busy() { return; }
    set(Fetch::Downloading { got: 0, total: None });
    std::thread::spawn(|| match run() {
        Ok(_) => set(Fetch::Done),
        Err(e) => {
            eprintln!("framefang: ffmpeg download failed: {e}");
            set(Fetch::Failed(e));
        }
    });
}

fn release_url(file: &str) -> String {
    format!("https://github.com/{REPO}/releases/download/{FFMPEG_TAG}/{file}")
}

/// A command that shows no console window (a tray app spawning `curl` or
/// PowerShell otherwise flashes one).
fn quiet(program: &str) -> std::process::Command {
    #[allow(unused_mut)]
    let mut cmd = std::process::Command::new(program);
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    cmd
}

fn run() -> Result<PathBuf, String> {
    let dest = framefang_platform::recorder::downloaded_ffmpeg_path()
        .ok_or("no place to install FFmpeg")?;
    let scratch = std::env::temp_dir().join("framefang-ffmpeg");
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(&scratch).map_err(|e| format!("create {}: {e}", scratch.display()))?;

    // The checksum first: nothing is unpacked unless the zip matches it.
    let sums = quiet("curl").args(["-fsSL", "--retry", "2", &release_url("SHA256SUMS")])
        .output().map_err(|e| format!("curl: {e}"))?;
    if !sums.status.success() {
        return Err("couldn't reach the FFmpeg release (check your connection)".into());
    }
    let sums = String::from_utf8_lossy(&sums.stdout);
    let expected = crate::self_updater::parse_sha256sums(&sums, ASSET)
        .ok_or("the FFmpeg release has no checksum for its download")?;

    let total = content_length(&release_url(ASSET));
    let zip = scratch.join(ASSET);
    let mut child = quiet("curl").args(["-fsSL", "--retry", "2", "-o"]).arg(&zip).arg(release_url(ASSET))
        .spawn().map_err(|e| format!("curl: {e}"))?;
    let status = loop {
        if let Some(status) = child.try_wait().map_err(|e| e.to_string())? { break status; }
        let got = std::fs::metadata(&zip).map(|m| m.len()).unwrap_or(0);
        set(Fetch::Downloading { got, total });
        std::thread::sleep(std::time::Duration::from_millis(150));
    };
    if !status.success() {
        return Err("the download stopped (check your connection)".into());
    }

    set(Fetch::Installing);
    crate::self_updater::verify_sha256(&zip, &expected)?;
    let unpacked = scratch.join("unpacked");
    unzip(&zip, &unpacked)?;
    let exe = crate::self_updater::find_named(&unpacked, "ffmpeg.exe")
        .ok_or("the download has no ffmpeg.exe in it")?;

    let dir = dest.parent().ok_or("bad install path")?;
    std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    // In beside the final name, then over it, so a half-copied file is
    // never what the lookup finds.
    let staged = dir.join("ffmpeg.exe.new");
    std::fs::copy(&exe, &staged).map_err(|e| format!("copy ffmpeg.exe: {e}"))?;
    let _ = std::fs::remove_file(&dest);
    std::fs::rename(&staged, &dest).map_err(|e| format!("install ffmpeg.exe: {e}"))?;
    for extra in ["LICENSE.txt", "BUILD.txt"] {
        if let Some(f) = crate::self_updater::find_named(&unpacked, extra) {
            let _ = std::fs::copy(f, dir.join(extra));
        }
    }

    // Prove it runs before calling it installed.
    let ok = quiet(&dest.to_string_lossy()).arg("-version").output()
        .map(|o| o.status.success()).unwrap_or(false);
    let _ = std::fs::remove_dir_all(&scratch);
    if !ok { return Err("the downloaded FFmpeg didn't start".into()); }
    Ok(dest)
}

/// The download's size from its HTTP headers (after redirects), for the
/// progress bar. `None` when the server doesn't say.
fn content_length(url: &str) -> Option<u64> {
    let out = quiet("curl").args(["-fsSIL", url]).output().ok()?;
    parse_content_length(&String::from_utf8_lossy(&out.stdout))
}

/// The last `Content-Length` in a chain of HTTP header blocks (the final
/// response's, after any redirects).
fn parse_content_length(headers: &str) -> Option<u64> {
    headers.lines().rev().find_map(|l| {
        let (k, v) = l.split_once(':')?;
        k.trim().eq_ignore_ascii_case("content-length").then(|| v.trim().parse().ok()).flatten()
    })
}

fn unzip(zip: &Path, to: &Path) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        crate::self_updater::unzip_windows(zip, to)
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = (zip, to);
        Err("FFmpeg downloads are Windows only".into())
    }
}

/// "7.2 / 14.8 MB", or "7.2 MB" when the size isn't known.
pub fn progress_text(got: u64, total: Option<u64>) -> String {
    let mb = |b: u64| b as f64 / 1_048_576.0;
    match total {
        Some(t) if t > 0 => format!("{:.1} / {:.1} MB", mb(got), mb(t)),
        _ => format!("{:.1} MB", mb(got)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_size_comes_from_the_final_response_after_redirects() {
        let h = "HTTP/2 302\r\ncontent-length: 0\r\nlocation: x\r\n\r\nHTTP/2 200\r\nContent-Length: 15728640\r\n\r\n";
        assert_eq!(parse_content_length(h), Some(15_728_640));
        assert_eq!(parse_content_length("HTTP/2 200\r\n\r\n"), None);
    }

    #[test]
    fn progress_reads_in_megabytes() {
        assert_eq!(progress_text(7_549_747, Some(15_518_924)), "7.2 / 14.8 MB");
        assert_eq!(progress_text(1_048_576, None), "1.0 MB");
    }

    /// The real thing, end to end: fetch the published build, check it,
    /// install it, run it. Needs the network, so only on request:
    /// `cargo test -p framefang-app --bin framefang -- --ignored published_build`
    #[cfg(target_os = "windows")]
    #[test]
    #[ignore = "downloads from GitHub"]
    fn the_published_build_downloads_verifies_and_runs() {
        let path = run().expect("download, verify and install");
        assert_eq!(Some(&path), framefang_platform::recorder::downloaded_ffmpeg_path().as_ref());
        let out = std::process::Command::new(&path).args(["-hide_banner", "-encoders"]).output().unwrap();
        let encoders = String::from_utf8_lossy(&out.stdout);
        for e in ["libx264", "libvpx-vp9", "libopus", "aac", "gif"] {
            assert!(encoders.contains(e), "installed ffmpeg lacks {e}");
        }
    }

    #[test]
    fn the_release_url_names_the_tag_and_asset() {
        assert_eq!(release_url(ASSET),
            "https://github.com/winters27/framefang/releases/download/ffmpeg-7.1.2/framefang-ffmpeg-windows-x86_64.zip");
        // The workflow publishes the same tag for the same version.
        let workflow = include_str!("../../../../.github/workflows/ffmpeg.yml");
        let version = FFMPEG_TAG.trim_start_matches("ffmpeg-");
        assert!(workflow.contains(&format!("FFMPEG_VERSION: '{version}'")), "ffmpeg.yml builds {version}");
        assert!(workflow.contains(ASSET), "ffmpeg.yml publishes {ASSET}");
    }
}
