//! The Convert video dialog: re-encode a clip with FFmpeg.
//!
//! The user picks a video, a container (MP4, MOV, WEBM, MKV or GIF), and
//! for the ones that re-encode a quality preset and a size cap; GIF adds a
//! frame rate. Convert runs FFmpeg on a worker thread and writes
//! `<name>.framefang.<ext>` beside the original. The status line counts
//! the seconds while it runs and says where the result went, or why there
//! is none.
//!
//! MKV is a plain container swap (streams copied as they are), so quality
//! and size are hidden for it. GIF goes through `crate::gif`, which makes
//! a palette first and then the GIF, in two FFmpeg runs.
//!
//! The layout pieces are shared with Convert image (`convert_image_form`).

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc;
use std::time::Instant;

use anyhow::Result;
use winit::event::WindowEvent;
use winit::event_loop::ActiveEventLoop;
use winit::keyboard::NamedKey;
use winit::window::WindowId;

use crate::dialog::{inside, Input, Shell};
use crate::convert_image_form::{choose_file, convert_button, draw_pill, draw_source, output_beside, Form, Status};
use crate::gif::GifOpts;

const CARD_W: u32 = 760;
const CARD_H: u32 = 671;
const CARD: (u32, u32) = (CARD_W, CARD_H);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum VidFormat { Mp4, Mov, Webm, Mkv, Gif }

const FORMATS: [VidFormat; 5] = [VidFormat::Mp4, VidFormat::Mov, VidFormat::Webm, VidFormat::Mkv, VidFormat::Gif];

impl VidFormat {
    fn ext(self) -> &'static str {
        match self {
            VidFormat::Mp4 => "mp4",
            VidFormat::Mov => "mov",
            VidFormat::Webm => "webm",
            VidFormat::Mkv => "mkv",
            VidFormat::Gif => "gif",
        }
    }

    fn caption(self) -> &'static str {
        match self {
            VidFormat::Mp4 => "MP4",
            VidFormat::Mov => "MOV",
            VidFormat::Webm => "WEBM",
            VidFormat::Mkv => "MKV",
            VidFormat::Gif => "GIF",
        }
    }
}

/// Size against looks, in three steps.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum VidQuality { Low, Med, High }

const QUALITIES: [VidQuality; 3] = [VidQuality::Low, VidQuality::Med, VidQuality::High];

/// What each quality step sets.
struct Knobs {
    /// libx264's speed against compression.
    x264_preset: &'static str,
    /// libx264's constant rate factor; lower looks better. 28, 23 and 20
    /// are roughly FFmpeg's "a bit lossy", "the default" and "very close
    /// to the original".
    x264_crf: &'static str,
    /// libvpx-vp9's (0 to 63), chosen to match the x264 steps.
    vp9_crf: &'static str,
}

impl VidQuality {
    fn knobs(self) -> Knobs {
        let (x264_preset, x264_crf, vp9_crf) = match self {
            VidQuality::Low => ("fast", "28", "38"),
            VidQuality::Med => ("medium", "23", "32"),
            VidQuality::High => ("slow", "20", "28"),
        };
        Knobs { x264_preset, x264_crf, vp9_crf }
    }

    fn caption(self) -> &'static str {
        match self {
            VidQuality::Low => "Low",
            VidQuality::Med => "Medium",
            VidQuality::High => "High",
        }
    }
}

/// A cap on the width. It only ever shrinks: `min(cap, iw)` leaves a
/// smaller clip at its own size instead of blowing it up.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum VidResize { Source, P1080, P720, P480 }

const SIZES: [VidResize; 4] = [VidResize::Source, VidResize::P1080, VidResize::P720, VidResize::P480];

impl VidResize {
    /// The `scale` filter for the cap. Width is the edge capped, desktop
    /// captures being mostly landscape; `-2` keeps the aspect ratio and an
    /// even height, which several encoders insist on.
    fn scale_expr(self) -> Option<&'static str> {
        match self {
            VidResize::Source => None,
            VidResize::P1080 => Some("scale='min(1920,iw)':'-2'"),
            VidResize::P720 => Some("scale='min(1280,iw)':'-2'"),
            VidResize::P480 => Some("scale='min(854,iw)':'-2'"),
        }
    }

    fn caption(self) -> &'static str {
        match self {
            VidResize::Source => "Full size",
            VidResize::P1080 => "1080p",
            VidResize::P720 => "720p",
            VidResize::P480 => "480p",
        }
    }
}

/// GIF frame rate. A GIF's size grows in step with it, so it is worth a
/// choice there; the other formats keep the clip's own rate.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum GifFps { Eight, Twelve, TwentyFour }

const FRAME_RATES: [GifFps; 3] = [GifFps::Eight, GifFps::Twelve, GifFps::TwentyFour];

impl GifFps {
    fn per_second(self) -> u32 {
        match self {
            GifFps::Eight => 8,
            GifFps::Twelve => 12,
            GifFps::TwentyFour => 24,
        }
    }

    fn caption(self) -> &'static str {
        match self {
            GifFps::Eight => "8 fps",
            GifFps::Twelve => "12 fps",
            GifFps::TwentyFour => "24 fps",
        }
    }
}

/// How a conversion runs.
enum Job {
    /// One FFmpeg run with these arguments.
    Single(Vec<String>),
    /// Two runs through `crate::gif`.
    Gif(GifOpts),
}

/// The work for turning `src` into `dst` as `format`. MKV copies the
/// streams, so it ignores quality and size.
fn plan(format: VidFormat, src: &str, dst: &str, quality: VidQuality, size: VidResize, fps: GifFps) -> Job {
    let knobs = quality.knobs();
    let encode = match format {
        VidFormat::Gif => return Job::Gif(GifOpts { fps: fps.per_second(), scale: size.scale_expr() }),
        VidFormat::Mkv => vec!["-c", "copy"],
        VidFormat::Webm => vec![
            "-c:v", "libvpx-vp9", "-crf", knobs.vp9_crf, "-b:v", "0",
            "-c:a", "libopus", "-b:a", "128k",
        ],
        VidFormat::Mp4 | VidFormat::Mov => vec![
            "-c:v", "libx264", "-preset", knobs.x264_preset, "-crf", knobs.x264_crf,
            "-c:a", "aac", "-b:a", "160k",
            "-movflags", "+faststart",
        ],
    };
    let scale = match format {
        VidFormat::Mkv => None,
        _ => size.scale_expr(),
    };
    let mut args = vec!["-y", "-i", src];
    if let Some(filter) = scale {
        args.extend(["-vf", filter]);
    }
    args.extend(encode);
    args.push(dst);
    Job::Single(args.into_iter().map(str::to_owned).collect())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Control {
    Convert,
    Source,
    Format(VidFormat),
    Quality(VidQuality),
    Resize(VidResize),
    Fps(GifFps),
}

/// Whether `control` means anything for `format`: no quality or size for a
/// stream copy, and a frame rate only for GIF.
fn shown_for(format: VidFormat, control: Control) -> bool {
    match control {
        Control::Quality(_) | Control::Resize(_) => format != VidFormat::Mkv,
        Control::Fps(_) => format == VidFormat::Gif,
        _ => true,
    }
}

pub enum ConvertVideoOutcome {
    Closed,
}

pub struct ConvertVideoView {
    shell: Shell,
    form: Form<Control>,
    source: Option<PathBuf>,
    format: VidFormat,
    quality: VidQuality, resize: VidResize, fps: GifFps,
    hover: Option<Control>,
    status: Status,
    /// The worker's answer, while a conversion runs.
    result: Option<mpsc::Receiver<Result<PathBuf, String>>>,
    pub outcome: Option<ConvertVideoOutcome>,
}

impl ConvertVideoView {
    pub fn new(loop_target: &ActiveEventLoop) -> Result<Self> {
        let shell = Shell::open(loop_target, "Frame Fang: Convert video", "// CONVERT VIDEO", "convert-video", CARD)?;
        // Every group keeps its place when it is hidden, so changing the
        // format never moves the rest of the form.
        let form = Form::new(CARD_W)
            .field("INPUT", Control::Source)
            .pills("OUTPUT FORMAT", &FORMATS.map(Control::Format))
            .pills("QUALITY", &QUALITIES.map(Control::Quality))
            .pills("SIZE  (can only shrink)", &SIZES.map(Control::Resize))
            .pills("GIF FRAMES PER SECOND", &FRAME_RATES.map(Control::Fps));
        // Starts on an MP4 at medium quality and full size (12 fps for GIF).
        let mut view = ConvertVideoView {
            shell, form,
            format: VidFormat::Mp4, quality: VidQuality::Med, resize: VidResize::Source, fps: GifFps::Twelve,
            source: None, hover: None, status: Status::Idle, result: None, outcome: None,
        };
        view.paint();
        Ok(view)
    }

    pub fn window_id(&self) -> WindowId { self.shell.id() }

    /// Called on every poll tick of the tray loop: picks up the worker's
    /// result when it lands, and keeps the running line's clock moving.
    pub fn tick(&mut self) {
        if let Some(Ok(done)) = self.result.as_ref().map(mpsc::Receiver::try_recv) {
            self.status = match done {
                Ok(path) => Status::Saved(path),
                Err(why) => Status::Failed(why),
            };
            self.result = None;
            self.shell.repaint();
        }
        if matches!(self.status, Status::Working(_)) {
            self.shell.repaint();
        }
    }

    pub fn handle_event(&mut self, event: WindowEvent) {
        match self.shell.input(event) {
            Input::Close | Input::Key(NamedKey::Escape) => self.outcome = Some(ConvertVideoOutcome::Closed),
            Input::Moved => {
                let now = self.control_at(self.shell.cursor);
                self.shell.track_hover(&mut self.hover, now, now.is_some());
            }
            Input::Press => {
                if let Some(control) = self.control_at(self.shell.cursor) {
                    self.press(control);
                    self.shell.repaint();
                }
            }
            Input::Paint => self.paint(),
            _ => {}
        }
    }

    fn control_at(&self, p: (i32, i32)) -> Option<Control> {
        if inside(convert_button(CARD), p) { return Some(Control::Convert); }
        self.form.hit(p, |c| shown_for(self.format, c))
    }

    fn press(&mut self, control: Control) {
        match control {
            Control::Source => {
                let picked = choose_file("Choose a video", self.source.as_deref(), directories::UserDirs::video_dir,
                    "Videos", &["mp4", "mov", "webm", "mkv", "avi", "m4v", "gif"]);
                // A new choice clears the last result line.
                if picked.is_some() { (self.source, self.status) = (picked, Status::Idle); }
            }
            Control::Format(f) => (self.format, self.status) = (f, Status::Idle),
            Control::Quality(q) => self.quality = q,
            Control::Resize(r) => self.resize = r,
            Control::Fps(f) => self.fps = f,
            Control::Convert => self.start(),
        }
    }

    /// Start FFmpeg on a worker thread, unless one is already running.
    fn start(&mut self) {
        if matches!(self.status, Status::Working(_)) { return; }
        let ready = match &self.source {
            None => Err("Choose a video to convert first."),
            Some(src) => locate_ffmpeg().map(|ffmpeg| (src.clone(), ffmpeg)).ok_or(crate::ffmpeg_fetch::missing_message()),
        };
        let (src, ffmpeg) = match ready {
            Ok(both) => both,
            Err(why) => {
                self.status = Status::Failed(why.to_owned());
                return;
            }
        };
        let dst = output_beside(&src, self.format.ext());
        let job = plan(self.format, &src.to_string_lossy(), &dst.to_string_lossy(), self.quality, self.resize, self.fps);

        let (send, receive) = mpsc::channel();
        (self.result, self.status) = (Some(receive), Status::Working(Instant::now()));
        std::thread::spawn(move || { let _ = send.send(run_job(&ffmpeg, &src, dst, job)); });
    }

    fn paint(&mut self) {
        let Some(mut page) = self.shell.page() else { return };
        let format = self.format;
        self.form.paint(&mut page, |c| shown_for(format, c), |page, control, r| {
            let hovered = self.hover == Some(control);
            match control {
                Control::Source => draw_source(page, r, "Video file", self.source.as_deref(), "(choose a video)", hovered),
                Control::Format(f) => draw_pill(page, r, f.caption(), true, f == self.format, hovered),
                Control::Quality(q) => draw_pill(page, r, q.caption(), false, q == self.quality, hovered),
                Control::Resize(s) => draw_pill(page, r, s.caption(), false, s == self.resize, hovered),
                Control::Fps(f) => draw_pill(page, r, f.caption(), false, f == self.fps, hovered),
                Control::Convert => {}
            }
        });
        page.button(convert_button(CARD), "Convert", true, self.hover == Some(Control::Convert));
        self.status.paint(&mut page, CARD);
        self.shell.show(page);
    }
}

/// Carry out `job` (on the worker thread), answering with where the result
/// went. Nothing cancels a conversion: closing the dialog lets it finish.
fn run_job(ffmpeg: &std::path::Path, src: &std::path::Path, dst: PathBuf, job: Job) -> Result<PathBuf, String> {
    let never = AtomicBool::new(false);
    let ran = match job {
        Job::Gif(opts) => crate::gif::convert(ffmpeg, src, &dst, opts, &never),
        Job::Single(args) => crate::gif::run(ffmpeg, &args, &never),
    };
    ran.map(|()| dst)
}

/// A usable FFmpeg, found the way the recorder finds it (beside the app,
/// in a Mac bundle, on `PATH`, then the copy Frame Fang downloaded). The
/// tray loop uses it as well, for recordings and the annotate burn-in.
pub(crate) fn locate_ffmpeg() -> Option<PathBuf> {
    framefang_platform::recorder::locate_ffmpeg()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn args(format: VidFormat, quality: VidQuality, size: VidResize) -> Vec<String> {
        match plan(format, "in.mov", "out.mp4", quality, size, GifFps::Twelve) {
            Job::Single(a) => a,
            Job::Gif(_) => panic!("{format:?} should be a single run"),
        }
    }

    #[test]
    fn mp4_and_mov_encode_with_x264_and_aac() {
        let want = [
            "-y", "-i", "in.mov",
            "-c:v", "libx264", "-preset", "medium", "-crf", "23",
            "-c:a", "aac", "-b:a", "160k", "-movflags", "+faststart",
            "out.mp4",
        ];
        assert_eq!(args(VidFormat::Mp4, VidQuality::Med, VidResize::Source), want);
        assert_eq!(args(VidFormat::Mov, VidQuality::Med, VidResize::Source), want);
        let small = [
            "-y", "-i", "in.mov", "-vf", "scale='min(1280,iw)':'-2'",
            "-c:v", "libx264", "-preset", "fast", "-crf", "28",
            "-c:a", "aac", "-b:a", "160k", "-movflags", "+faststart",
            "out.mp4",
        ];
        assert_eq!(args(VidFormat::Mp4, VidQuality::Low, VidResize::P720), small);
        let sharp = args(VidFormat::Mov, VidQuality::High, VidResize::P1080);
        assert_eq!(&sharp[3..11], ["-vf", "scale='min(1920,iw)':'-2'", "-c:v", "libx264", "-preset", "slow", "-crf", "20"]);
    }

    #[test]
    fn webm_encodes_with_vp9_and_opus() {
        assert_eq!(args(VidFormat::Webm, VidQuality::Med, VidResize::Source), [
            "-y", "-i", "in.mov",
            "-c:v", "libvpx-vp9", "-crf", "32", "-b:v", "0",
            "-c:a", "libopus", "-b:a", "128k",
            "out.mp4",
        ]);
        assert_eq!(args(VidFormat::Webm, VidQuality::Low, VidResize::P480), [
            "-y", "-i", "in.mov", "-vf", "scale='min(854,iw)':'-2'",
            "-c:v", "libvpx-vp9", "-crf", "38", "-b:v", "0",
            "-c:a", "libopus", "-b:a", "128k",
            "out.mp4",
        ]);
        assert_eq!(args(VidFormat::Webm, VidQuality::High, VidResize::Source)[6], "28");
    }

    #[test]
    fn mkv_copies_the_streams_whatever_the_knobs_say() {
        let copy = ["-y", "-i", "in.mov", "-c", "copy", "out.mp4"];
        for q in QUALITIES {
            for s in SIZES {
                assert_eq!(args(VidFormat::Mkv, q, s), copy);
            }
        }
    }

    #[test]
    fn gif_takes_the_two_run_path_with_the_chosen_rate_and_cap() {
        let Job::Gif(opts) = plan(VidFormat::Gif, "in.mp4", "out.gif", VidQuality::Med, VidResize::P720, GifFps::Twelve)
        else { panic!("GIF must go through crate::gif") };
        assert_eq!(opts.fps, 12);
        assert_eq!(opts.scale, Some("scale='min(1280,iw)':'-2'"));
        let Job::Gif(plain) = plan(VidFormat::Gif, "a", "b", VidQuality::Low, VidResize::Source, GifFps::TwentyFour)
        else { unreachable!() };
        assert_eq!((plain.fps, plain.scale), (24, None));

        // The two command lines `gif::convert` runs for the first one.
        let (src, out, palette) = (Path::new("in.mp4"), Path::new("out.gif"), Path::new("out.gif.palette.png"));
        assert_eq!(crate::gif::palette_args(src, palette, opts), [
            "-y", "-i", "in.mp4",
            "-vf", "fps=12,scale='min(1280,iw)':'-2',palettegen=stats_mode=diff",
            "out.gif.palette.png",
        ]);
        assert_eq!(crate::gif::gif_args(src, palette, out, opts), [
            "-y", "-i", "in.mp4", "-i", "out.gif.palette.png",
            "-lavfi", "fps=12,scale='min(1280,iw)':'-2'[x];[x][1:v]paletteuse=dither=sierra2_4a",
            "-f", "gif", "out.gif",
        ]);
    }

    #[test]
    fn controls_hide_when_the_format_ignores_them() {
        let q = Control::Quality(VidQuality::Low);
        let s = Control::Resize(VidResize::P720);
        let f = Control::Fps(GifFps::Eight);
        assert!(shown_for(VidFormat::Mp4, q) && shown_for(VidFormat::Mp4, s) && !shown_for(VidFormat::Mp4, f));
        assert!(!shown_for(VidFormat::Mkv, q) && !shown_for(VidFormat::Mkv, s) && !shown_for(VidFormat::Mkv, f));
        assert!(shown_for(VidFormat::Gif, q) && shown_for(VidFormat::Gif, s) && shown_for(VidFormat::Gif, f));
        for format in FORMATS {
            assert!(shown_for(format, Control::Source) && shown_for(format, Control::Format(VidFormat::Gif)));
        }
    }

    #[test]
    fn the_form_keeps_every_group_in_place() {
        let form = Form::new(CARD_W)
            .field("INPUT", Control::Source)
            .pills("OUTPUT FORMAT", &FORMATS.map(Control::Format))
            .pills("QUALITY", &QUALITIES.map(Control::Quality))
            .pills("SIZE", &SIZES.map(Control::Resize))
            .pills("FPS", &FRAME_RATES.map(Control::Fps));
        assert_eq!(form.rect_of(Control::Source), Some((24, 44, 712, 36)));
        assert_eq!(form.rect_of(Control::Format(VidFormat::Gif)), Some((24 + 4 * 145, 120, 131, 46)));
        assert_eq!(form.rect_of(Control::Quality(VidQuality::High)), Some((24 + 2 * 242, 206, 228, 46)));
        assert_eq!(form.rect_of(Control::Resize(VidResize::P480)), Some((24 + 3 * 181, 292, 167, 46)));
        assert_eq!(form.rect_of(Control::Fps(GifFps::Eight)), Some((24, 378, 228, 46)));
        // The hidden frame-rate pills take no clicks outside GIF.
        assert!(form.hit((30, 390), |c| shown_for(VidFormat::Mp4, c)).is_none());
        assert!(form.hit((30, 390), |c| shown_for(VidFormat::Gif, c)) == Some(Control::Fps(GifFps::Eight)));
        assert_eq!(convert_button(CARD), (596, 613, 140, 34));
    }
}
