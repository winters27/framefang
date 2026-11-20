//! Video to GIF with FFmpeg, in two runs: the first finds the best 256
//! colors for the clip (`palettegen`), the second encodes with them
//! (`paletteuse`).
//!
//! One run with `split ... palettegen ... paletteuse` looks simpler, but the
//! palette only exists once the whole clip has been read, so FFmpeg holds
//! every decoded frame in memory until then: about 3.6 MB per 720p frame,
//! measured at 1.2 GB for 300 frames (2026-11-19). Two runs stay under
//! 100 MB for any length.
//!
//! Used by GIF recordings and by Convert video.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// Frame rate and size cap for a GIF.
#[derive(Debug, Clone, Copy)]
pub struct GifOpts {
    pub fps: u32,
    /// An ffmpeg `scale=` expression, or `None` for the source size.
    pub scale: Option<&'static str>,
}

/// What GIF recordings use: 15 fps, at most 1280 wide.
pub const RECORDING: GifOpts = GifOpts { fps: 15, scale: Some("scale='min(1280,iw)':-2:flags=lanczos") };

fn filters(o: GifOpts) -> String {
    match o.scale {
        Some(s) => format!("fps={},{s}", o.fps),
        None => format!("fps={}", o.fps),
    }
}

/// Run one: the palette, into `palette` (a PNG).
pub fn palette_args(src: &Path, palette: &Path, o: GifOpts) -> Vec<String> {
    vec![
        "-y".into(), "-i".into(), src.to_string_lossy().into_owned(),
        "-vf".into(), format!("{},palettegen=stats_mode=diff", filters(o)),
        palette.to_string_lossy().into_owned(),
    ]
}

/// Run two: the GIF, into `out`. `-f gif` names the format, so `out` may
/// end in anything (a `.part` work name).
pub fn gif_args(src: &Path, palette: &Path, out: &Path, o: GifOpts) -> Vec<String> {
    vec![
        "-y".into(), "-i".into(), src.to_string_lossy().into_owned(),
        "-i".into(), palette.to_string_lossy().into_owned(),
        "-lavfi".into(), format!("{}[x];[x][1:v]paletteuse=dither=sierra2_4a", filters(o)),
        "-f".into(), "gif".into(),
        out.to_string_lossy().into_owned(),
    ]
}

/// Run `ffmpeg args`, killing it when `cancel` turns on. The error is
/// FFmpeg's last line, or "cancelled".
pub fn run(ffmpeg: &Path, args: &[String], cancel: &AtomicBool) -> Result<(), String> {
    let mut cmd = Command::new(ffmpeg);
    cmd.args(args).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped());
    // CREATE_NO_WINDOW: no console flashes up while it works.
    #[cfg(target_os = "windows")]
    { use std::os::windows::process::CommandExt; cmd.creation_flags(0x0800_0000); }
    let mut child = cmd.spawn().map_err(|e| format!("ffmpeg couldn't start: {e}"))?;
    // Read stderr as it comes, or a chatty FFmpeg blocks on a full pipe.
    let err = child.stderr.take().map(|mut s| std::thread::spawn(move || {
        let mut text = String::new();
        let _ = std::io::Read::read_to_string(&mut s, &mut text);
        text
    }));
    let status = loop {
        if cancel.load(Ordering::Relaxed) {
            let _ = child.kill();
            let _ = child.wait();
            return Err("cancelled".to_owned());
        }
        match child.try_wait() {
            Ok(Some(st)) => break st,
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("ffmpeg: {e}"));
            }
        }
    };
    let text = err.and_then(|h| h.join().ok()).unwrap_or_default();
    if status.success() { Ok(()) } else {
        Err(format!("ffmpeg: {}", text.lines().last().unwrap_or("failed")))
    }
}

/// `src` to a GIF at `out`, in two runs. The palette lives beside `out`
/// and is removed afterwards; on failure `out` is removed too.
pub fn convert(ffmpeg: &Path, src: &Path, out: &Path, o: GifOpts, cancel: &AtomicBool) -> Result<(), String> {
    let palette: PathBuf = {
        let mut p = out.as_os_str().to_owned();
        p.push(".palette.png");
        p.into()
    };
    let result = run(ffmpeg, &palette_args(src, &palette, o), cancel)
        .and_then(|_| run(ffmpeg, &gif_args(src, &palette, out, o), cancel));
    let _ = std::fs::remove_file(&palette);
    if result.is_err() { let _ = std::fs::remove_file(out); }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_runs_and_the_second_reads_the_palette() {
        let (src, pal, out) = (Path::new("in.mp4"), Path::new("p.png"), Path::new("o.gif.part"));
        let a = palette_args(src, pal, RECORDING);
        assert!(a.iter().any(|s| s.contains("palettegen") && !s.contains("split")), "{a:?}");
        assert_eq!(a.last().unwrap(), "p.png");
        let b = gif_args(src, pal, out, RECORDING);
        let lavfi = &b[b.iter().position(|s| s == "-lavfi").unwrap() + 1];
        assert!(lavfi.contains("[1:v]paletteuse") && lavfi.starts_with("fps=15,"), "{lavfi}");
        assert_eq!(b.iter().filter(|s| *s == "-i").count(), 2, "the palette is the second input");
        let f = b.iter().position(|s| s == "-f").unwrap();
        assert_eq!(b[f + 1], "gif", "a work name with any ending still makes a GIF");
        assert_eq!(b.last().unwrap(), "o.gif.part");
    }

    /// Needs FFmpeg on PATH: `cargo test -- --ignored gif::`.
    #[test]
    #[ignore]
    fn encodes_a_two_second_clip() {
        let ffmpeg = crate::convert_video_form::locate_ffmpeg().expect("ffmpeg");
        let dir = std::env::temp_dir().join(format!("ff-gif-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("clip.mp4");
        let made = Command::new(&ffmpeg)
            .args(["-y", "-f", "lavfi", "-i", "testsrc=size=320x240:rate=15", "-t", "2", "-pix_fmt", "yuv420p"])
            .arg(&src).output().unwrap();
        assert!(made.status.success());
        let out = dir.join("clip.gif.part");
        convert(&ffmpeg, &src, &out, RECORDING, &AtomicBool::new(false)).unwrap();
        let bytes = std::fs::read(&out).unwrap();
        assert_eq!(&bytes[..6], b"GIF89a");
        // More than one frame: a graphic control extension per frame.
        assert!(bytes.windows(2).filter(|w| w == &[0x21, 0xF9]).count() > 1);
        let fail = convert(&ffmpeg, &dir.join("missing.mp4"), &dir.join("x.gif"), RECORDING, &AtomicBool::new(false));
        assert!(fail.is_err() && !dir.join("x.gif").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
