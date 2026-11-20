//! Talking to ffmpeg about recordings: how long a clip is, a single frame out
//! of it, and burning annotation overlays into a new file.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{anyhow, Result};
use image::RgbaImage;

use super::desktop::no_console_window;
use crate::editor::OverlayGroup;

/// Hands out a number per ffmpeg job for temp file names. The process id
/// alone repeats when one session pulls many scrub frames, or when a burn is
/// still encoding on its worker thread while the next session commits.
static JOB_TICKET: AtomicU64 = AtomicU64::new(0);

fn next_ticket() -> u64 {
    JOB_TICKET.fetch_add(1, Ordering::Relaxed)
}

/// The ffmpeg the app found, or the message that says how to get one.
fn ffmpeg_or_hint() -> Result<PathBuf> {
    crate::convert_video_form::locate_ffmpeg()
        .ok_or_else(|| anyhow!(crate::ffmpeg_fetch::missing_message()))
}

/// Run `cmd` to the end with no input, discarding stdout and keeping stderr.
fn run_quiet(mut cmd: Command) -> Result<Output> {
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped());
    no_console_window(&mut cmd);
    cmd.output().map_err(|e| anyhow!("could not start ffmpeg: {e}"))
}

/// The last few non-empty lines of ffmpeg's stderr: enough to say why it
/// stopped without pasting its whole banner into a toast.
fn stderr_tail(stderr: &str) -> String {
    let mut newest: Vec<&str> = stderr
        .lines()
        .rev()
        .filter_map(|line| Some(line.trim()).filter(|t| !t.is_empty()))
        .take(4)
        .collect();
    newest.reverse();
    newest.join(" | ")
}

/// Hours, minutes and seconds out of `HH:MM:SS.cc`, as seconds.
fn clock_seconds(stamp: &str) -> Option<f32> {
    let mut fields = stamp.split(':').map(|field| field.parse::<f32>().ok());
    let hours = fields.next()??;
    let minutes = fields.next()??;
    let seconds = fields.next()??;
    Some(hours * 3600.0 + minutes * 60.0 + seconds)
}

/// The clip length from the input banner ffmpeg prints on stderr:
/// `  Duration: 00:00:12.34, start: 0.000000, bitrate: 1234 kb/s`. Broken files
/// say `N/A`. `None` rather than an error, so a caller can fall back to the
/// editor without a timeline instead of refusing to annotate.
pub(super) fn parse_ffmpeg_duration(stderr: &str) -> Option<f32> {
    let after_label = stderr
        .lines()
        .map(str::trim_start)
        .find_map(|line| line.strip_prefix("Duration:"))?;
    clock_seconds(after_label.split(',').next()?.trim())
}

/// The clip length in seconds. ffprobe is not bundled, but `ffmpeg -i <clip>`
/// prints the same banner before stopping with "at least one output file must
/// be specified"; that exit status is expected, only the banner matters.
pub(super) fn probe_duration(video: &Path) -> Result<f32> {
    let mut cmd = Command::new(ffmpeg_or_hint()?);
    cmd.arg("-i").arg(video);
    let banner = run_quiet(cmd)?;
    parse_ffmpeg_duration(&String::from_utf8_lossy(&banner.stderr))
        .ok_or_else(|| anyhow!("ffmpeg printed no duration for {}", video.display()))
}

/// The frame at `secs` into the clip, at the clip's own resolution. It goes
/// through a PNG in the temp folder and arrives as an RGBA buffer, like any
/// screenshot.
pub(crate) fn extract_frame_at(video: &Path, secs: f32) -> Result<RgbaImage> {
    let ffmpeg = ffmpeg_or_hint()?;
    let png = std::env::temp_dir().join(format!(
        "framefang_annotate_frame_{}_{}.png",
        std::process::id(),
        next_ticket()
    ));
    let mut cmd = Command::new(&ffmpeg);
    cmd.arg("-y");
    // `-ss` ahead of `-i` seeks on the input: jump to a keyframe, then decode
    // up to the exact time. Left off at 0 so the first frame is asked for the
    // way it always was.
    if secs > 0.0 {
        cmd.args(["-ss", &format!("{secs:.3}")]);
    }
    cmd.arg("-i").arg(video).args(["-frames:v", "1", "-update", "1"]).arg(&png);
    let run = run_quiet(cmd)?;
    if !run.status.success() {
        let _ = std::fs::remove_file(&png);
        let why = stderr_tail(&String::from_utf8_lossy(&run.stderr));
        return Err(anyhow!("no frame could be read from {}: {why}", video.display()));
    }
    let decoded = image::open(&png);
    let _ = std::fs::remove_file(&png);
    let frame = decoded.map_err(|e| anyhow!("the extracted frame would not decode: {e}"))?;
    Ok(frame.into_rgba8())
}

/// The first frame of `video`, at the clip's own resolution.
pub(super) fn extract_first_frame(video: &Path) -> Result<RgbaImage> {
    extract_frame_at(video, 0.0)
}

/// `<stem>_annotated.mp4` beside the original clip.
pub(super) fn annotated_output_path(video: &Path) -> PathBuf {
    let stem = video
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "framefang".to_owned());
    let folder = video.parent().map(Path::to_path_buf).unwrap_or_else(std::env::temp_dir);
    folder.join(format!("{stem}_annotated.mp4"))
}

/// The `enable=` option that shows an overlay only while it should be up.
/// A whole-clip overlay needs none, which keeps the common graph the plain
/// `overlay=0:0`. The end is exclusive: ffmpeg's `between()` includes both
/// ends, and a frame landing exactly on the end is the usual case (the 3, 5
/// and 10 second presets sit on the 30 fps grid of our own recordings), so
/// `between()` would burn a frame the preview shows clean.
fn enable_clause(start: f32, end: Option<f32>) -> String {
    match end {
        Some(end) => format!(":enable='gte(t,{start:.3})*lt(t,{end:.3})'"),
        None if start == 0.0 => String::new(),
        // "Until the clip ends" without trusting a parsed duration to the
        // centisecond.
        None => format!(":enable='gte(t,{start:.3})'"),
    }
}

/// The `-filter_complex` graph that chains one `overlay` per group, with the
/// last link labelled `[v]` for `-map`:
/// `[0:v][1:v]overlay=0:0:enable='...'[v0];[v0][2:v]overlay=...[v]`.
///
/// The string is one argument to the child process, so only ffmpeg's own
/// filtergraph parser reads it. In there an unescaped `,` ends a filter's
/// options, so the commas inside `gte(t,..)` and `lt(t,..)` sit in single
/// quotes, which the parser strips before the expression is evaluated.
/// Without them ffmpeg stops at once with `No such filter: '2.000'`.
pub(super) fn overlay_filtergraph(windows: &[(f32, Option<f32>)]) -> String {
    let last = windows.len().saturating_sub(1);
    let mut input = "0:v".to_owned();
    let mut links = Vec::with_capacity(windows.len());
    for (i, &(start, end)) in windows.iter().enumerate() {
        let output = if i == last { "v".to_owned() } else { format!("v{i}") };
        let gate = enable_clause(start, end);
        links.push(format!("[{input}][{}:v]overlay=0:0{gate}[{output}]", i + 1));
        input = output;
    }
    links.join(";")
}

/// Temp files that are removed when this goes out of scope, however the
/// function holding it returns.
struct TempFiles(Vec<PathBuf>);

impl Drop for TempFiles {
    fn drop(&mut self) {
        for path in &self.0 {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// Burn the overlays into a copy of `video` written to `out`: one transparent
/// PNG per (start, end) group, each laid over the clip by its own `overlay`
/// filter, the audio copied through untouched. The encoder settings follow the
/// recorder (libx264 ultrafast, yuv420p so every player takes it). Runs on a
/// worker thread.
pub(super) fn run_annotation_burn(
    ffmpeg: &Path,
    video: &Path,
    overlays: &[OverlayGroup],
    out: &Path,
) -> Result<()> {
    if overlays.is_empty() {
        return Err(anyhow!("there are no annotation overlays to burn in"));
    }
    // Burns run on detached threads, so a second session can commit while an
    // earlier clip is still encoding. Process id and group index would then
    // repeat; the ticket keeps the names apart.
    let ticket = next_ticket();
    let mut pngs = TempFiles(Vec::with_capacity(overlays.len()));
    for (i, (picture, _, _)) in overlays.iter().enumerate() {
        let png = std::env::temp_dir().join(format!(
            "framefang_annotate_overlay_{}_{ticket}_{i}.png",
            std::process::id()
        ));
        pngs.0.push(png.clone());
        picture.save(&png).map_err(|e| anyhow!("could not write an overlay image: {e}"))?;
    }
    let windows: Vec<(f32, Option<f32>)> = overlays.iter().map(|&(_, s, e)| (s, e)).collect();

    let mut cmd = Command::new(ffmpeg);
    cmd.args(["-y", "-i"]).arg(video);
    for png in &pngs.0 {
        cmd.arg("-i").arg(png);
    }
    cmd.arg("-filter_complex").arg(overlay_filtergraph(&windows));
    // `0:a?` maps the audio only when the clip has some.
    cmd.args(["-map", "[v]", "-map", "0:a?"])
        .args(["-c:v", "libx264", "-preset", "ultrafast", "-pix_fmt", "yuv420p"])
        .args(["-c:a", "copy", "-movflags", "+faststart"])
        .arg(out);
    let run = run_quiet(cmd)?;
    if !run.status.success() {
        let why = stderr_tail(&String::from_utf8_lossy(&run.stderr));
        return Err(anyhow!("ffmpeg ended with {}: {why}", run.status));
    }
    let written = std::fs::metadata(out).map(|m| m.len()).unwrap_or(0);
    if written == 0 {
        return Err(anyhow!("ffmpeg finished without error but {} is missing or empty", out.display()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_last_four_lines_of_stderr_survive() {
        let text = "banner\n\nline1\nline2\nline3\nline4\nline5\n";
        assert_eq!(stderr_tail(text), "line2 | line3 | line4 | line5");
        assert_eq!(stderr_tail("  one  \n\n  two\n"), "one | two", "trimmed, blanks skipped");
        assert_eq!(stderr_tail(""), "");
    }

    #[test]
    fn the_annotated_copy_sits_beside_the_original() {
        let out = annotated_output_path(Path::new("/tmp/vids/framefang_20260606_101010.mp4"));
        assert_eq!(out, PathBuf::from("/tmp/vids/framefang_20260606_101010_annotated.mp4"));
    }

    #[test]
    fn the_duration_is_read_from_the_input_banner() {
        // The real shape: two-space indent, field ended by a comma.
        let banner = "ffmpeg version 6.1.1 Copyright (c) 2000-2023 the FFmpeg developers\n\
            Input #0, mov,mp4,m4a,3gp,3g2,mj2, from 'clip.mp4':\n\
            \x20 Metadata:\n\
            \x20   encoder         : Lavf60.3.100\n\
            \x20 Duration: 00:00:12.34, start: 0.000000, bitrate: 2048 kb/s\n\
            \x20 Stream #0:0[0x1](und): Video: h264 (High), yuv420p, 1280x720\n";
        let secs = parse_ffmpeg_duration(banner).unwrap();
        assert!((secs - 12.34).abs() < 0.01, "got {secs}");
        let long = parse_ffmpeg_duration("  Duration: 01:02:03.45, start: 0.000000, bitrate: 5 kb/s\n").unwrap();
        assert!((long - 3723.45).abs() < 0.01, "got {long}");
    }

    #[test]
    fn no_duration_comes_back_for_na_or_a_missing_line() {
        let broken = "  Duration: N/A, start: 0.000000, bitrate: N/A\n";
        assert!(parse_ffmpeg_duration(broken).is_none());
        assert!(parse_ffmpeg_duration("nothing useful\n").is_none());
        assert!(parse_ffmpeg_duration("").is_none());
        assert_eq!(parse_ffmpeg_duration("  Duration: 00:12, start: 0\n"), None, "needs three fields");
    }

    #[test]
    fn a_whole_clip_overlay_is_the_plain_graph() {
        assert_eq!(overlay_filtergraph(&[(0.0, None)]), "[0:v][1:v]overlay=0:0[v]");
        assert_eq!(overlay_filtergraph(&[]), "");
    }

    #[test]
    fn timed_overlays_are_gated_and_chained() {
        let graph = overlay_filtergraph(&[(0.0, None), (1.5, Some(4.5)), (2.0, None)]);
        assert_eq!(
            graph,
            "[0:v][1:v]overlay=0:0[v0];\
             [v0][2:v]overlay=0:0:enable='gte(t,1.500)*lt(t,4.500)'[v1];\
             [v1][3:v]overlay=0:0:enable='gte(t,2.000)'[v]"
        );
    }

    #[test]
    fn an_overlay_ends_before_the_frame_on_its_end_time() {
        assert_eq!(enable_clause(0.0, Some(3.0)), ":enable='gte(t,0.000)*lt(t,3.000)'");
        assert_eq!(enable_clause(0.0, None), "");
    }

    #[test]
    fn temp_files_are_removed_when_the_guard_drops() {
        let path = std::env::temp_dir().join(format!("ff-media-guard-{}.png", std::process::id()));
        std::fs::write(&path, b"x").unwrap();
        drop(TempFiles(vec![path.clone(), path.with_extension("missing")]));
        assert!(!path.exists());
    }

    #[test]
    fn nothing_is_burned_without_overlays() {
        let err = run_annotation_burn(Path::new("ffmpeg"), Path::new("a.mp4"), &[], Path::new("b.mp4"));
        assert!(err.is_err());
    }
}
