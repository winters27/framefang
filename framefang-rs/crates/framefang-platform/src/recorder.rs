//! Screen recording, by driving the system's own tools.
//!
//! No single Rust crate records the screen well on every OS, so Frame Fang
//! runs an outside encoder instead: `ffmpeg` everywhere, or macOS's built-in
//! `screencapture` for silent recordings there. Whatever records, the file
//! that comes out is the same: an MP4 with H.264 video and stereo AAC sound.
//! Everything after recording (conversion, the pin preview, annotating a
//! clip) only has to understand that one format.
//!
//! Where the picture comes from:
//!
//! | Session        | Video                                    | Sound                                  |
//! |----------------|------------------------------------------|----------------------------------------|
//! | Linux, X11     | `ffmpeg -f x11grab`                      | PulseAudio: the mic and the default sink's monitor |
//! | Linux, Wayland | ScreenCast portal, PipeWire frames piped into ffmpeg (`recorder_wayland.rs`) | the same Pulse inputs, served by PipeWire's Pulse layer |
//! | Windows        | `ffmpeg -f gdigrab`                      | WASAPI, sent to ffmpeg over 127.0.0.1 (`recorder_windows_audio.rs`) |
//! | macOS          | `screencapture -v`, or `ffmpeg -f avfoundation` once sound is asked for | avfoundation for the mic, ScreenCaptureKit for the system mix (`recorder_macos_audio.rs`) |
//!
//! Wayland never uses x11grab: XWayland would answer it with a black screen.
//! The session type is decided up front (see `crate::session`) so the two
//! Linux paths never mix. gdigrab is heavy on the CPU and sees some
//! composited surfaces less cleanly than Windows.Graphics.Capture would; it
//! stays because it is proven and mirrors the Linux shape, and moving to
//! Windows.Graphics.Capture with Media Foundation encoding remains possible.
//!
//! # Regions
//!
//! A recording covers the whole desktop unless `start_region` gets a
//! [`CaptureRect`]. Each backend takes it in its own form:
//!
//! * X11: `-video_size WxH` and `-i <display>+X,Y`.
//! * Wayland: a `crop` filter on the shared frames.
//! * Windows: `-offset_x X -offset_y Y -video_size WxH` before `-i desktop`.
//! * macOS: `screencapture -v -R x,y,w,h`, or a `crop` filter on the
//!   avfoundation path.
//!
//! X11, Wayland and gdigrab count from the virtual desktop's corner
//! (`CaptureRect::offset_x`); macOS counts from the main display's corner
//! (`CaptureRect::x`). `framefang_core::region` owns that difference and the
//! even sizes H.264 needs. A region never changes the sound inputs.
//!
//! # Stopping
//!
//! ffmpeg finishes a file cleanly when it reads `q` on stdin, on Windows as
//! on Linux; `screencapture` finishes on SIGINT; the Wayland encoder finishes
//! when its frame pipe closes. Either way the MP4's index gets written. The
//! wait after asking is always bounded (15 s for `stop`, 2 s on drop) and
//! then the process is killed, so a stuck encoder can't hang the UI thread.
//!
//! # Watching
//!
//! Between start and stop, `Recorder::poll_health` notices an encoder that
//! died, and the last lines of its stderr are kept so the user hears the
//! real cause rather than an exit code.

use crate::{Error, Result};
use framefang_core::region::CaptureRect;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

// WASAPI capture for Windows recordings, kept apart because of its COM code.
#[cfg(target_os = "windows")]
#[path = "recorder_windows_audio.rs"]
mod windows_audio;

// ScreenCaptureKit system sound for macOS recordings.
#[cfg(target_os = "macos")]
#[path = "recorder_macos_audio.rs"]
mod macos_audio;

// ScreenCast portal and PipeWire frames for Wayland sessions. X11 sessions
// never touch it.
#[cfg(target_os = "linux")]
#[path = "recorder_wayland.rs"]
pub mod wayland_cast;

/// Which sound sources go into a recording.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RecordingOptions {
    pub mic:          bool,
    pub system_audio: bool,
}

impl RecordingOptions {
    /// Silent.
    pub const NONE: Self = Self { mic: false, system_audio: false };
    pub const MIC_ONLY: Self = Self { mic: true, system_audio: false };
    pub const SYSTEM_ONLY: Self = Self { mic: false, system_audio: true };
    /// Both, mixed into one track.
    pub const MIC_AND_SYS: Self = Self { mic: true, system_audio: true };

    /// Whether any sound source is on.
    pub fn has_audio(self) -> bool {
        self.mic || self.system_audio
    }
}

/// Starts, watches and stops one screen recording at a time.
pub struct Recorder {
    live:   Option<Live>,
    output: Option<PathBuf>,
}

impl Recorder {
    pub fn new() -> Self {
        Recorder { live: None, output: None }
    }

    pub fn is_recording(&self) -> bool {
        self.live.is_some()
    }

    /// The file the running recording is writing.
    pub fn output_path(&self) -> Option<&Path> {
        self.output.as_deref()
    }

    /// Record the whole desktop to `output`; the same as
    /// [`Recorder::start_region`] with no region.
    pub fn start(&mut self, output: PathBuf, options: RecordingOptions)
        -> Result<RecordingOptions>
    {
        self.start_region(output, options, None)
    }

    /// Record to `output`, the whole desktop or just `region`.
    ///
    /// A region is in virtual-desktop pixels and has already been clamped to
    /// the desktop and made even by
    /// `framefang_core::region::record_rect_from_selection`; each backend
    /// turns it into its own form (see the module notes). Sound works the
    /// same either way.
    ///
    /// The answer is what is really being recorded, which can be less than
    /// `options`: sound is best effort, so a machine without a sound server
    /// or a default output still records the picture. The "recording
    /// started" message has to come from this value, or it may promise a
    /// microphone track the file won't have.
    ///
    /// Fails if a recording is already running, if the folder for `output`
    /// can't be made, or if the recording tool can't be started.
    pub fn start_region(&mut self,
                        output:  PathBuf,
                        options: RecordingOptions,
                        region:  Option<CaptureRect>)
        -> Result<RecordingOptions>
    {
        if self.live.is_some() {
            return Err(Error::Recording("a recording is already running".into()));
        }
        if let Some(folder) = output.parent() {
            std::fs::create_dir_all(folder)?;
        }
        let (live, got) = launch(&output, options, region)?;
        self.live = Some(live);
        self.output = Some(output);
        Ok(got)
    }

    /// Check that the recording is still going, cheaply enough for every
    /// event-loop tick.
    ///
    /// An encoder can die in the middle (a full disk, an unplugged device, a
    /// kill for memory), and without this the app would keep showing REC
    /// and `stop` would report success over a cut-off file. When it has died
    /// the recorder is cleaned up and left idle (`is_recording` turns false)
    /// and the reason comes back, with the encoder's last words.
    ///
    /// `Ok` while all is well, and also when nothing is recording.
    pub fn poll_health(&mut self) -> Result<()> {
        let Some(live) = self.live.as_mut() else { return Ok(()) };

        // A screen share the user revoked leaves the encoder running on a
        // frozen picture, so the source is asked separately.
        if let Some(reason) = live.source_failure() {
            if let Some(live) = self.live.take() {
                live.wind_down(DROP_WAIT_TICKS);
            }
            let saved = self.output.take()
                .map(|p| format!(" The recording up to that moment is at {}.", p.display()))
                .unwrap_or_default();
            return Err(Error::Recording(format!("{reason}{saved}")));
        }

        let Some(status) = live.exited() else { return Ok(()) };
        // Read after the clean-up, so lines the reader thread is still
        // catching up on make it into the message.
        let last_words = live.stderr.clone();
        if let Some(live) = self.live.take() {
            live.discard_dead();
        }
        let saved = self.output.take().map(|p| written_so_far(&p)).unwrap_or_default();
        Err(Error::Recording(format!(
            "the recording ended early because the encoder quit by itself ({status}).{saved}{}",
            last_words.as_suffix()
        )))
    }

    /// Stop the recording and hand back the file it made.
    ///
    /// The encoder needs a moment to write its last frames and close the
    /// file; this waits for that (up to 15 seconds), so the file plays as
    /// soon as this returns.
    pub fn stop(&mut self) -> Result<PathBuf> {
        let live = self.live.take()
            .ok_or_else(|| Error::Recording("there is no recording to stop".into()))?;
        let path = self.output.take().unwrap_or_default();
        let finished = live.stop_blocking(&path);

        // The encoder can get through startup and still leave nothing usable
        // behind (it died later, or the muxer failed). Never hand back a path
        // to nothing; and "no file" says more than "had to be killed", so it
        // is checked first.
        let named = !path.as_os_str().is_empty();
        let problem = match named.then(|| std::fs::metadata(&path).map(|meta| meta.len())) {
            Some(Ok(0)) => Some("is empty: the encoder probably failed partway through"),
            Some(Err(_)) => Some("was never written: the encoder probably never got going"),
            _ => None,
        };
        if let Some(problem) = problem {
            return Err(Error::Recording(format!(
                "the recording stopped, but {} {problem}.", path.display())));
        }
        finished.map(|()| path)
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        if let Some(live) = self.live.take() {
            live.wind_down(DROP_WAIT_TICKS);
        }
    }
}

/// " What it wrote so far is at <path>." for messages about a recording that
/// went wrong: a killed or crashed encoder usually still leaves a playable
/// clip, and the user shouldn't have to hunt for it.
fn written_so_far(path: &Path) -> String {
    format!(" What it wrote so far is at {}.", path.display())
}

// ---- The running recording -------------------------------------------------

/// How long one wait step lasts while an encoder finishes.
const TICK: Duration = Duration::from_millis(100);

/// `stop` waits up to 15 s for the file to be finished. A long 4K recording
/// can take seconds to write its index, and killing it early ruins exactly
/// the file the user asked for; but this is the UI thread, so it is bounded.
const STOP_WAIT_TICKS: u32 = 150;

/// On drop, or after a lost screen share, nobody is waiting on the file, so
/// 2 s is enough.
const DROP_WAIT_TICKS: u32 = 20;

/// A recording in progress: the child doing the encoding (`ffmpeg` or
/// `screencapture`) plus whatever feeds it on this OS.
struct Live {
    child:  Child,
    /// The end of the child's stderr, kept up to date by a reader thread.
    stderr: RecentStderr,
    /// Windows: the WASAPI threads sending sound to ffmpeg.
    #[cfg(target_os = "windows")]
    pumps:  Vec<AudioPump>,
    /// macOS: the ScreenCaptureKit session sending system sound to ffmpeg,
    /// when system sound was asked for.
    #[cfg(target_os = "macos")]
    sck:    Option<macos_audio::SckSession>,
    /// Linux: the screen cast writing frames into ffmpeg's stdin, on Wayland
    /// only (x11grab reads the screen itself).
    #[cfg(target_os = "linux")]
    cast:   Option<wayland_cast::CastSession>,
}

impl Live {
    /// A recording with nothing feeding it but the child itself.
    fn bare(child: Child, stderr: RecentStderr) -> Self {
        Live {
            child,
            stderr,
            #[cfg(target_os = "windows")]
            pumps: Vec::new(),
            #[cfg(target_os = "macos")]
            sck: None,
            #[cfg(target_os = "linux")]
            cast: None,
        }
    }

    /// Tell the feeders to stop, before waiting on the encoder. On Wayland
    /// this is what ends the recording: the frame writer owns ffmpeg's stdin,
    /// and its pipe closing is ffmpeg's cue to finish.
    fn halt_feeds(&mut self) {
        #[cfg(target_os = "windows")]
        for pump in &self.pumps {
            pump.request_stop();
        }
        #[cfg(target_os = "linux")]
        if let Some(cast) = self.cast.as_mut() {
            cast.stop();
        }
    }

    /// Release what is left of the feeders once the encoder is gone.
    fn release_feeds(&mut self) {
        #[cfg(target_os = "windows")]
        for pump in &mut self.pumps {
            pump.finish();
        }
        #[cfg(target_os = "macos")]
        if let Some(sck) = self.sck.take() {
            sck.stop();
        }
    }

    /// Ask the encoder to finish, give it `ticks` steps to do so, kill it if
    /// it hasn't, and clean up. True when it finished on its own, meaning the
    /// file is whole.
    fn wind_down(mut self, ticks: u32) -> bool {
        ask_to_finish(&mut self.child);
        self.halt_feeds();
        let finished = exits_within(&mut self.child, ticks);
        if !finished {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
        self.release_feeds();
        crate::child_guard::clear_live();
        finished
    }

    /// The careful stop behind `Recorder::stop`. An error means the encoder
    /// had to be killed, so the file is probably cut short; `output` (empty
    /// if unknown) is named in that message.
    fn stop_blocking(self, output: &Path) -> Result<()> {
        let last_words = self.stderr.clone();
        if self.wind_down(STOP_WAIT_TICKS) {
            return Ok(());
        }
        let saved = if output.as_os_str().is_empty() { String::new() } else { written_so_far(output) };
        Err(Error::Recording(format!(
            "the encoder was still busy {} seconds after being asked to stop, so it was ended by \
             force. The file may be cut short or fail to play.{saved}{}",
            STOP_WAIT_TICKS / 10,
            last_words.as_suffix()
        )))
    }

    /// Clean up after an encoder that has already exited. There is nothing
    /// left to ask nicely.
    fn discard_dead(mut self) {
        let _ = self.child.wait();
        self.halt_feeds();
        self.release_feeds();
        crate::child_guard::clear_live();
    }

    /// The child's exit status if it has quit; `None` while it runs (or if
    /// that can't be told).
    fn exited(&mut self) -> Option<ExitStatus> {
        self.child.try_wait().ok().flatten()
    }

    /// Why the picture stopped coming, when the encoder is fine but its
    /// source isn't.
    ///
    /// Only Wayland can get here: the screen share is a grant the user can
    /// withdraw at any time, and ffmpeg would go on repeating the last frame
    /// without noticing. Elsewhere the encoder reads the screen itself, so a
    /// lost source is a dead child, which `exited` already sees.
    fn source_failure(&self) -> Option<String> {
        #[cfg(target_os = "linux")]
        return self.cast.as_ref().and_then(|cast| cast.failure());
        #[cfg(not(target_os = "linux"))]
        None
    }
}

/// Wait up to `ticks` steps for `child` to exit. A `try_wait` error counts as
/// still running, so the caller kills rather than trusting a finish it can't
/// see.
fn exits_within(child: &mut Child, ticks: u32) -> bool {
    for _ in 0..ticks {
        match child.try_wait() {
            Ok(None) => std::thread::sleep(TICK),
            settled => return settled.is_ok(),
        }
    }
    false // still running
}

/// Ask a recorder to finish its file and exit.
///
/// ffmpeg was started with a stdin pipe and finishes on `q`, on Windows too,
/// where a windowless child can't be sent Ctrl+C. On Wayland the frame writer
/// has already taken that pipe, so nothing is sent here and stopping the
/// writer closes it instead (`Live::halt_feeds`). macOS `screencapture` has
/// no pipe and finishes on SIGINT.
fn ask_to_finish(child: &mut Child) {
    use std::io::Write;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = writeln!(stdin, "q");
        return;
    }
    // libc is a Unix dependency of this crate, but `/bin/kill` ships with
    // every macOS and keeps this a plain process call.
    #[cfg(target_os = "macos")]
    {
        let pid = child.id().to_string();
        let _ = Command::new("/bin/kill").args(["-INT", &pid]).status();
    }
}

// ---- The encoder's last words ------------------------------------------------

/// How many stderr lines are kept. The line that matters ("No space left on
/// device", "Device or resource busy") comes in the last few before the
/// encoder dies.
const KEPT_LINES: usize = 16;

/// The most recent lines of a recorder's stderr.
///
/// The pipe has to be read for the whole recording anyway: once its buffer
/// fills, the encoder blocks mid-write. Keeping the last lines instead of
/// throwing them away is the difference between "recording stopped" and
/// "the disk is full".
#[derive(Clone)]
struct RecentStderr {
    lines: Arc<Mutex<VecDeque<String>>>,
}

impl RecentStderr {
    fn new() -> Self {
        RecentStderr { lines: Arc::new(Mutex::new(VecDeque::with_capacity(KEPT_LINES))) }
    }

    /// Keep one line, forgetting the oldest when full. Blank lines and
    /// ffmpeg's once-a-second progress lines are skipped, since they would
    /// push every real message out of the window.
    fn keep(&self, raw: &[u8]) {
        let printable = printable_ascii(&String::from_utf8_lossy(raw));
        let line = printable.trim();
        if line.is_empty() || line.starts_with("frame=") || line.starts_with("size=") {
            return;
        }
        let Ok(mut lines) = self.lines.lock() else { return };
        if lines.len() == KEPT_LINES {
            lines.pop_front();
        }
        lines.push_back(line.to_owned());
    }

    /// The kept lines, oldest first, one per line.
    fn joined(&self) -> String {
        self.lines.lock()
            .map(|lines| lines.iter().map(String::as_str).collect::<Vec<_>>().join("\n"))
            .unwrap_or_default()
    }

    /// The kept lines, set off by a blank line, ready to end an error
    /// message; empty when the encoder said nothing worth repeating.
    fn as_suffix(&self) -> String {
        match self.joined() {
            t if t.is_empty() => t,
            t => format!("\n\n{t}"),
        }
    }
}

/// Encoder output reduced to printable ASCII. The app's text is drawn with a
/// 5x7 ASCII font, so anything else would show as '?' anyway, and control
/// characters would break the layout; doing it here covers every reader.
fn printable_ascii(s: &str) -> String {
    s.chars().map(|c| if c == ' ' || c.is_ascii_graphic() { c } else { '?' }).collect()
}

/// Keep an eye on a recorder for its first moments.
///
/// ffmpeg and `screencapture` check their inputs as they start, so a missing
/// demuxer, a device that won't open or a bad argument ends the process
/// within a few hundred milliseconds, before any frame is written. If that
/// happens within 400 ms the error carries the end of its stderr. If it
/// doesn't, a thread keeps reading stderr into a [`RecentStderr`] so a later
/// death can still be explained. The child must have been spawned with
/// `stderr(Stdio::piped())`.
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
fn watch_recorder_startup(mut child: Child) -> Result<(Child, RecentStderr)> {
    use std::io::Read;
    let mut stderr = child.stderr.take();

    for _ in 0..8 {
        match child.try_wait() {
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Ok(Some(status)) => {
                // Already gone: forget it as the live recorder now, or the next
                // launch would go looking for its pid.
                crate::child_guard::clear_live();
                let mut said = String::new();
                if let Some(mut pipe) = stderr.take() {
                    let _ = pipe.read_to_string(&mut said);
                }
                return Err(Error::Recording(format!(
                    "the recording couldn't start: the encoder quit straight away ({status}).\n\n{}",
                    startup_diagnosis(&said)
                )));
            }
            Err(_) => break,
        }
    }

    let tail = RecentStderr::new();
    if let Some(pipe) = stderr {
        let keep = tail.clone();
        std::thread::spawn(move || read_stderr_into(pipe, keep));
    }
    Ok((child, tail))
}

/// Read a recorder's stderr until it closes, keeping the last lines in
/// `tail`.
///
/// Lines end at `\n` or `\r`, because ffmpeg redraws its progress line with a
/// carriage return; splitting on newlines alone would make one endless line
/// of a whole recording's progress and miss the error after it. Lines are cut
/// at 512 bytes, which is far past anything useful.
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
fn read_stderr_into(mut pipe: std::process::ChildStderr, tail: RecentStderr) {
    use std::io::Read;
    const LONGEST: usize = 512;
    let mut chunk = [0u8; 8192];
    let mut line = Vec::with_capacity(LONGEST);
    while let Some(n) = pipe.read(&mut chunk).ok().filter(|&n| n > 0) {
        for &byte in &chunk[..n] {
            match byte {
                b'\n' | b'\r' => {
                    tail.keep(&line);
                    line.clear();
                }
                _ if line.len() < LONGEST => line.push(byte),
                _ => {}
            }
        }
    }
    // A last line with no newline is often the reason it died.
    tail.keep(&line);
}

/// The useful end of an encoder's startup stderr: its last six non-blank
/// lines. The message that explains it ("Unknown input format: 'pulse'",
/// "Error opening input files", "Permission denied") comes after the banner
/// and build settings.
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
fn startup_diagnosis(stderr: &str) -> String {
    let lines: Vec<&str> = stderr.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    if lines.is_empty() {
        return "The encoder printed nothing to say why. Check that the capture device exists \
                and that this ffmpeg build supports your sound system."
            .to_string();
    }
    lines[lines.len().saturating_sub(6)..].join("\n")
}

// ---- Finding ffmpeg --------------------------------------------------------

/// Where Frame Fang keeps the FFmpeg it downloads for people without one,
/// `%LOCALAPPDATA%\Framefang\ffmpeg\ffmpeg.exe`. Windows only: other systems
/// get FFmpeg from their package manager.
pub fn downloaded_ffmpeg_path() -> Option<PathBuf> {
    if cfg!(target_os = "windows") {
        let local = std::env::var_os("LOCALAPPDATA")?;
        Some(PathBuf::from(local).join("Framefang").join("ffmpeg").join("ffmpeg.exe"))
    } else {
        None
    }
}

/// The FFmpeg chosen in Settings (`FfmpegPath`), or `None` to search. The
/// tray sets it whenever the settings load or change.
static CHOSEN_FFMPEG: Mutex<Option<PathBuf>> = Mutex::new(None);

pub fn set_chosen_ffmpeg(path: Option<PathBuf>) {
    *CHOSEN_FFMPEG.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = path;
}

/// The `ffmpeg` that recording, conversion and Settings all use: the one
/// chosen in Settings while that file exists, otherwise [`auto_ffmpeg`].
///
/// `None` when there is none anywhere; callers then run plain `"ffmpeg"` so
/// the system's own "not found" error still comes through.
pub fn locate_ffmpeg() -> Option<PathBuf> {
    let chosen = CHOSEN_FFMPEG.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clone();
    locate_ffmpeg_with(chosen.as_deref())
}

/// [`locate_ffmpeg`] with `chosen` standing in for the Settings choice, so
/// Settings can ask about its unsaved draft.
pub fn locate_ffmpeg_with(chosen: Option<&Path>) -> Option<PathBuf> {
    match chosen {
        Some(file) if file.is_file() => Some(file.to_path_buf()),
        _ => auto_ffmpeg(),
    }
}

/// Look for FFmpeg without the Settings choice. The first file found wins:
///
/// 1. beside our own executable (a local build's copy);
/// 2. `Contents/Resources/ffmpeg` in a macOS app bundle;
/// 3. each `PATH` folder in order, so an FFmpeg the user installed comes
///    before ours;
/// 4. the copy Frame Fang downloaded ([`downloaded_ffmpeg_path`]).
pub fn auto_ffmpeg() -> Option<PathBuf> {
    let file_name = if cfg!(windows) { "ffmpeg.exe" } else { "ffmpeg" };
    let exe = std::env::current_exe().ok()?;
    let home = exe.parent()?;

    let mut candidates = vec![home.join(file_name)];
    if cfg!(target_os = "macos") {
        if let Some(contents) = home.parent() {
            candidates.push(contents.join("Resources").join("ffmpeg"));
        }
    }
    if let Ok(search) = std::env::var("PATH") {
        let separator = if cfg!(windows) { ';' } else { ':' };
        candidates.extend(search.split(separator).map(|folder| Path::new(folder).join(file_name)));
    }
    candidates.extend(downloaded_ffmpeg_path());
    candidates.into_iter().find(|candidate| candidate.is_file())
}

// ---- Building ffmpeg command lines ---------------------------------------------
//
// Pure functions, so every argument list can be checked in tests on any
// machine. Every ffmpeg line has the same shape: `-y` (overwrite), the video
// input, any sound inputs, H.264 encoding with an even-size filter, the
// sound output, and the file.

/// An ffmpeg argument list being put together.
struct Argv(Vec<String>);

impl Argv {
    fn new() -> Self {
        Argv(vec!["-y".to_string()])
    }

    fn opt(&mut self, flag: &str, value: impl Into<String>) -> &mut Self {
        self.0.push(flag.to_string());
        self.0.push(value.into());
        self
    }

    fn finish(mut self, output_path: &str) -> Vec<String> {
        self.0.push(output_path.to_string());
        self.0
    }
}

/// Rounds the picture up to even sides, which yuv420p H.264 requires. A
/// `CaptureRect` is already even, so after a crop it does nothing; it is
/// there for a full screen of odd size.
const EVEN_PAD: &str = "pad=ceil(iw/2)*2:ceil(ih/2)*2";

/// The `-vf` chain: an optional `crop=w:h:x:y`, then [`EVEN_PAD`].
fn video_filter(crop: Option<(u32, u32, i32, i32)>) -> String {
    match crop {
        Some((w, h, x, y)) => format!("crop={w}:{h}:{x}:{y},{EVEN_PAD}"),
        None => EVEN_PAD.to_string(),
    }
}

/// Video encoding shared by every backend: fast H.264 in yuv420p, the pixel
/// layout every common player accepts.
fn h264(argv: &mut Argv, filter: String) {
    argv.opt("-c:v", "libx264").opt("-preset", "ultrafast").opt("-pix_fmt", "yuv420p").opt("-vf", filter);
}

/// The stream labels of sound inputs numbered from 1 (video being input 0).
#[cfg(any(target_os = "linux", target_os = "windows", test))]
fn numbered_sound(count: usize) -> Vec<String> {
    (1..=count).map(|i| format!("{i}:a")).collect()
}

/// Put the sound `inputs` (stream labels such as `"1:a"`) into one AAC
/// track: as it is when there is one, through `amix` when there are more.
///
/// `mapped` is how macOS and Windows do it: every stream mapped by hand and
/// the track made two-channel. Linux leaves a lone input to ffmpeg's default
/// stream choice, with the channel count it came with.
fn sound_out(argv: &mut Argv, inputs: &[String], mapped: bool) {
    match inputs {
        [] => return,
        [only] => {
            if mapped {
                argv.opt("-map", "0:v").opt("-map", only.as_str());
            }
        }
        several => {
            let labels: String = several.iter().map(|l| format!("[{l}]")).collect();
            let graph = format!(
                "{labels}amix=inputs={}:duration=longest:dropout_transition=0[aout]",
                several.len()
            );
            argv.opt("-filter_complex", graph).opt("-map", "0:v").opt("-map", "[aout]");
        }
    }
    argv.opt("-c:a", "aac").opt("-b:a", "160k");
    if mapped {
        argv.opt("-ac", "2");
    }
}

/// One raw PCM sound input that ffmpeg reads from a socket we listen on. The
/// queue size stops the demuxer dropping packets while the encoder is busy.
#[cfg(any(target_os = "macos", target_os = "windows", test))]
fn pcm_input(argv: &mut Argv, format: &str, rate: u32, channels: u16, port: u16) {
    argv.opt("-thread_queue_size", "1024")
        .opt("-f", format)
        .opt("-ar", rate.to_string())
        .opt("-ac", channels.to_string())
        .opt("-i", format!("tcp://127.0.0.1:{port}"));
}

/// The Pulse sound inputs both Linux paths use: the default source as the
/// mic, and the named monitor for what is playing.
#[cfg(any(target_os = "linux", test))]
fn pulse_inputs(argv: &mut Argv, mic: bool, monitor: Option<&str>) {
    if mic {
        argv.opt("-f", "pulse").opt("-i", "default");
    }
    if let Some(source) = monitor {
        argv.opt("-f", "pulse").opt("-i", source);
    }
}

/// Everything after the inputs on Linux, the same for X11 and Wayland:
/// video is input 0 on both, then the mic, then the monitor.
#[cfg(any(target_os = "linux", test))]
fn linux_encode(argv: &mut Argv, mic: bool, monitor: bool, crop: Option<(u32, u32, i32, i32)>) {
    h264(argv, video_filter(crop));
    sound_out(argv, &numbered_sound(usize::from(mic) + usize::from(monitor)), false);
}

/// ffmpeg's arguments for an X11 recording.
///
/// x11grab takes a region's size as `-video_size` ahead of the input and its
/// corner as `+X,Y` after the display name (`:0.0+120,80`), counted from the
/// root window's corner, which is the virtual desktop's corner; hence the
/// offsets, not the absolute position.
#[cfg(any(target_os = "linux", test))]
fn build_linux_ffmpeg_args(display: &str, output_path: &str, sound: RecordingOptions,
                           monitor: Option<&str>, region: Option<CaptureRect>) -> Vec<String> {
    let mut argv = Argv::new();
    argv.opt("-f", "x11grab").opt("-framerate", "30");
    let screen = match region {
        Some(r) => {
            argv.opt("-video_size", r.video_size());
            format!("{display}+{},{}", r.offset_x(), r.offset_y())
        }
        None => display.to_string(),
    };
    argv.opt("-i", screen);
    pulse_inputs(&mut argv, sound.mic, monitor);
    linux_encode(&mut argv, sound.mic, monitor.is_some(), None);
    argv.finish(output_path)
}

/// ffmpeg's arguments for a Wayland recording: raw frames on stdin as input
/// 0, then the same Pulse inputs as X11.
///
/// There are two `-pix_fmt`s on purpose. The one before `-i` describes the
/// bytes we write; the one in the encoding asks for yuv420p in the file.
/// Raw video carries no timestamps, so `-framerate` before `-i` is its only
/// clock, and the frame writer keeps exactly that pace. A pipe has no
/// offset option, so a region becomes a `crop`, counted from the desktop's
/// corner, which is where the shared frame starts when the whole desktop is
/// shared. A crop outside the shared picture makes ffmpeg fail as it
/// starts, which the startup watch reports.
#[cfg(any(target_os = "linux", test))]
#[allow(clippy::too_many_arguments)]
fn build_wayland_ffmpeg_args(pix_fmt: &str, width: u32, height: u32, fps: u32, output_path: &str,
                             sound: RecordingOptions, monitor: Option<&str>, region: Option<CaptureRect>)
    -> Vec<String>
{
    let mut argv = Argv::new();
    argv.opt("-f", "rawvideo")
        .opt("-pix_fmt", pix_fmt)
        .opt("-video_size", format!("{width}x{height}"))
        .opt("-framerate", fps.to_string())
        // Same reason as the socket inputs: don't drop frames while busy.
        .opt("-thread_queue_size", "1024")
        .opt("-i", "pipe:0");
    pulse_inputs(&mut argv, sound.mic, monitor);
    let crop = region.map(|r| (r.width, r.height, r.offset_x(), r.offset_y()));
    linux_encode(&mut argv, sound.mic, monitor.is_some(), crop);
    argv.finish(output_path)
}

/// `screencapture`'s arguments for a silent macOS recording.
///
/// `-R x,y,w,h` is in macOS's global display space, which starts at the main
/// display's corner (the same origin as `CaptureRect`'s absolute fields) but
/// counts points, while the rectangle arrives in device pixels. So the
/// fields are divided by the capture's scale; otherwise a Retina recording
/// would be twice as big and twice as far from the corner.
#[cfg(any(target_os = "macos", test))]
fn build_macos_screencapture_args(output_path: &str, region: Option<CaptureRect>) -> Vec<String> {
    // getopt parsing, so the rectangle can be its own argument after `-R`.
    let rectangle = region.map(|area| {
        let (left, top, wide, high) = area.points();
        ["-R".to_string(), format!("{left},{top},{wide},{high}")]
    });
    std::iter::once("-v".to_string())
        .chain(rectangle.into_iter().flatten())
        .chain(std::iter::once(output_path.to_string()))
        .collect()
}

/// ffmpeg's arguments for a macOS recording with sound: avfoundation video
/// (with the mic folded into the same input) as input 0, and the
/// ScreenCaptureKit system sound as a socket input 1 when there is one.
///
/// avfoundation has no capture rectangle, so a region is a `crop`. Its
/// frame is the main display at device resolution, the same origin and unit
/// the rectangle's absolute fields use, so they go in as they are. A region
/// wholly on another display falls outside that frame; the silent
/// `screencapture` path has no such limit.
#[cfg(any(target_os = "macos", test))]
fn build_macos_ffmpeg_args(screen: usize, mic: Option<usize>, system_port: Option<u16>,
                           output_path: &str, region: Option<CaptureRect>) -> Vec<String> {
    let mut argv = Argv::new();
    // "<video>:<audio>", with the audio half empty when there's no mic.
    let devices = format!("{screen}:{}", mic.map(|i| i.to_string()).unwrap_or_default());
    argv.opt("-f", "avfoundation").opt("-framerate", "30").opt("-i", devices);
    // ScreenCaptureKit delivers 48 kHz stereo float; see recorder_macos_audio.rs.
    if let Some(socket) = system_port {
        pcm_input(&mut argv, "f32le", 48000, 2, socket);
    }
    h264(&mut argv, video_filter(region.map(|r| (r.width, r.height, r.x, r.y))));
    // The mic is the avfoundation input's own sound; system sound is input 1.
    let sound: Vec<String> = [(mic.is_some(), "0:a"), (system_port.is_some(), "1:a")]
        .into_iter()
        .filter_map(|(present, label)| present.then(|| label.to_string()))
        .collect();
    sound_out(&mut argv, &sound, true);
    argv.finish(output_path)
}

/// What ffmpeg needs to read one WASAPI source: the socket port and the PCM
/// layout the device delivers. Plain data, so the Windows argument list can
/// be tested on any machine.
#[cfg(any(target_os = "windows", test))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WasapiAudioSpec {
    pub ffmpeg_fmt:  &'static str,
    pub sample_rate: u32,
    pub channels:    u16,
    pub port:        u16,
}

/// ffmpeg's arguments for a Windows recording: gdigrab video, then one
/// socket input per WASAPI source, in the format the device gave, mixed to
/// one stereo track.
///
/// gdigrab reads a region's `-offset_x`, `-offset_y` and `-video_size` before
/// `-i desktop`, and adds the offsets to `SM_XVIRTUALSCREEN` and
/// `SM_YVIRTUALSCREEN`: it counts from the virtual desktop's corner. With a
/// monitor left of the primary one that is 1920 away from the absolute
/// position, hence the offsets.
#[cfg(any(target_os = "windows", test))]
fn build_windows_ffmpeg_args(output_path: &str, sources: &[WasapiAudioSpec], region: Option<CaptureRect>)
    -> Vec<String>
{
    let mut argv = Argv::new();
    // `desktop` is the whole virtual screen; a region narrows it.
    argv.opt("-f", "gdigrab").opt("-framerate", "30");
    if let Some(area) = region {
        argv.opt("-offset_x", area.offset_x().to_string())
            .opt("-offset_y", area.offset_y().to_string())
            .opt("-video_size", area.video_size());
    }
    argv.opt("-i", "desktop");
    // We already listen on each port; ffmpeg connects to it.
    for source in sources {
        pcm_input(&mut argv, source.ffmpeg_fmt, source.sample_rate, source.channels, source.port);
    }
    h264(&mut argv, video_filter(None));
    sound_out(&mut argv, &numbered_sound(sources.len()), true);
    argv.finish(output_path)
}

// ---- Starting a recorder ---------------------------------------------------

/// An ffmpeg process with stdin piped (for `q`), stdout discarded and stderr
/// piped (for the startup watch). On Windows it gets no console window,
/// which would otherwise sit on screen for the whole recording.
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
fn ffmpeg_command(program: &Path, args: &[String]) -> Command {
    let mut cmd = Command::new(program);
    cmd.args(args).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::piped());
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// The output path as text, which is how ffmpeg gets it.
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
fn path_text(output: &Path) -> Result<&str> {
    output.to_str().ok_or_else(|| Error::Recording("the recording's file path isn't valid UTF-8".into()))
}

/// The error for an ffmpeg that wouldn't start, with install advice when it
/// simply isn't there.
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
fn ffmpeg_start_failure(e: std::io::Error, not_found: &str) -> Error {
    if e.kind() == std::io::ErrorKind::NotFound {
        Error::Recording(not_found.into())
    } else {
        Error::Recording(format!("ffmpeg couldn't be started: {e}"))
    }
}

#[cfg(target_os = "linux")]
const LINUX_NO_FFMPEG: &str =
    "ffmpeg wasn't found on PATH. Install it (for example: sudo apt install ffmpeg) and try again.";

/// Start Linux's ffmpeg with `args`, through the recorder guard.
#[cfg(target_os = "linux")]
fn start_linux_ffmpeg(args: &[String]) -> Result<Child> {
    let program = locate_ffmpeg().unwrap_or_else(|| "ffmpeg".into());
    crate::child_guard::spawn_guarded(&mut ffmpeg_command(&program, args))
        .map_err(|e| ffmpeg_start_failure(e, LINUX_NO_FFMPEG))
}

#[cfg(target_os = "linux")]
fn launch(file: &Path, wanted: RecordingOptions, area: Option<CaptureRect>) -> Result<(Live, RecordingOptions)> {
    // x11grab under XWayland records only XWayland windows (usually a black
    // frame), and a pure Wayland session may have no DISPLAY at all. The two
    // stacks share their sound setup and nothing else.
    if crate::session::is_wayland() {
        return launch_wayland(file, wanted, area);
    }
    let display = std::env::var("DISPLAY").unwrap_or_else(|_| String::from(":0"));
    let path = path_text(file)?;
    let (got, monitor) = usable_pulse_sources(wanted);
    let encoder = start_linux_ffmpeg(&build_linux_ffmpeg_args(&display, path, got, monitor.as_deref(), area))?;
    watch_recorder_startup(encoder).map(|(child, stderr)| (Live::bare(child, stderr), got))
}

/// `pactl` with its input and error output discarded.
#[cfg(target_os = "linux")]
fn pactl(request: &str) -> Command {
    let mut cmd = Command::new("pactl");
    cmd.arg(request).stdin(Stdio::null()).stderr(Stdio::null());
    cmd
}

/// Which of the requested sound sources Pulse can really give, and the
/// monitor source to name for system sound. Both Linux stacks use this,
/// since PipeWire's Pulse layer serves the same inputs on Wayland.
///
/// Sound is best effort: with no sound server, or no default output to
/// monitor, the picture is still recorded. The caller reports the returned
/// options, not the requested ones.
#[cfg(target_os = "linux")]
fn usable_pulse_sources(wanted: RecordingOptions) -> (RecordingOptions, Option<String>) {
    // `pactl info` succeeds only with a server to talk to. Without one, both
    // sources are dropped and the recording goes ahead silently.
    let server_up = pactl("info").stdout(Stdio::null()).status().is_ok_and(|done| done.success());
    let mut got = if server_up { wanted } else { RecordingOptions::NONE };

    // System sound is the default sink's monitor. Without one to name, only
    // system sound goes; the picture and the mic don't need it.
    let monitor = if got.system_audio { default_sink_monitor() } else { None };
    got.system_audio = monitor.is_some();
    (got, monitor)
}

/// Wayland: the ScreenCast portal, then PipeWire, then raw frames into
/// ffmpeg's stdin.
///
/// The cast comes first because only it knows the frame size and layout,
/// and ffmpeg needs both on its command line before reading raw video. So
/// whatever goes wrong before ffmpeg starts (no portal, the user declining
/// the share, buffers we can't read) is an error here, with no file made.
#[cfg(target_os = "linux")]
fn launch_wayland(file: &Path, wanted: RecordingOptions, area: Option<CaptureRect>)
    -> Result<(Live, RecordingOptions)>
{
    let path = path_text(file)?;
    let (got, monitor) = usable_pulse_sources(wanted);

    // This waits while the desktop asks the user what to share.
    let mut share = wayland_cast::CastSession::start()?;
    let layout = share.format();
    let mut encoder = start_linux_ffmpeg(&build_wayland_ffmpeg_args(
        layout.pix_fmt, layout.width, layout.height, wayland_cast::TARGET_FPS,
        path, got, monitor.as_deref(), area))?;

    // The frame writer takes ffmpeg's stdin. With `encoder.stdin` now empty,
    // `ask_to_finish` sends no `q` here: stopping the writer closes the pipe.
    let hooked_up = encoder.stdin.take()
        .ok_or_else(|| Error::Recording("ffmpeg started, but without a stdin pipe to send it frames".into()))
        .and_then(|pipe| share.attach(pipe));
    if let Err(why) = hooked_up {
        encoder.kill().ok();
        encoder.wait().ok();
        return Err(why);
    }

    let (child, stderr) = watch_recorder_startup(encoder).inspect_err(|_| share.stop())?;
    let mut live = Live::bare(child, stderr);
    live.cast = Some(share);
    Ok((live, got))
}

/// The Pulse monitor source of the default sink: the input that carries what
/// the speakers play.
///
/// `pactl get-default-sink` can fail, or succeed and print nothing, when
/// there is no server or no default sink. Treating either as a sink named ""
/// would hand ffmpeg ".monitor", which it can't open, and a bad input takes
/// the whole recording down. `None` means record without system sound.
#[cfg(target_os = "linux")]
fn default_sink_monitor() -> Option<String> {
    let answer = pactl("get-default-sink").output().ok().filter(|out| out.status.success())?;
    let printed = String::from_utf8(answer.stdout).ok()?;
    match printed.trim() {
        "" => None,
        sink => Some(format!("{sink}.monitor")),
    }
}

/// macOS. A silent recording uses the built-in `screencapture -v`, which
/// needs no ffmpeg. With sound, ffmpeg records the picture and the mic
/// through avfoundation, and system sound comes from ScreenCaptureKit over a
/// socket (no BlackHole or Aggregate device), mixed by ffmpeg like on Linux.
#[cfg(target_os = "macos")]
fn launch(file: &Path, wanted: RecordingOptions, area: Option<CaptureRect>) -> Result<(Live, RecordingOptions)> {
    let path = path_text(file)?;

    if wanted == RecordingOptions::NONE {
        // stdin stays closed: `ask_to_finish` tells the two tools apart by it.
        let mut silent = Command::new("screencapture");
        silent.args(build_macos_screencapture_args(path, area))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        let recorder = crate::child_guard::spawn_guarded(&mut silent)
            .map_err(|e| Error::Recording(format!("screencapture couldn't be started: {e}")))?;
        return watch_recorder_startup(recorder)
            .map(|(child, stderr)| (Live::bare(child, stderr), RecordingOptions::NONE));
    }

    let Some(ffmpeg) = locate_ffmpeg() else {
        return Err(Error::Recording(
            "recording with sound on macOS needs ffmpeg, and there is none next to Frame Fang or \
             on your PATH. Install it (brew install ffmpeg) and try again; recording without \
             sound works without it.".into()));
    };
    let (cameras, sound_devices) = parse_avfoundation_devices(&list_avfoundation_devices(&ffmpeg));
    let screen = pick_macos_screen_index(&cameras)?;
    let mic = wanted.mic.then(|| pick_macos_mic_device(&sound_devices)).flatten();

    // Started before the arguments are built, because they need its port.
    let system = if wanted.system_audio { Some(macos_audio::start_system_audio()?) } else { None };
    // A mic avfoundation didn't list won't be in the file, whatever was asked.
    let got = RecordingOptions { mic: mic.is_some(), system_audio: system.is_some() };

    let args = build_macos_ffmpeg_args(screen, mic, system.as_ref().map(|s| s.port), path, area);
    let started = crate::child_guard::spawn_guarded(&mut ffmpeg_command(&ffmpeg, &args))
        .map_err(|e| ffmpeg_start_failure(e, "ffmpeg wasn't found. Install it with brew install ffmpeg and try again."))
        .and_then(watch_recorder_startup);
    match started {
        Ok((child, stderr)) => {
            let mut live = Live::bare(child, stderr);
            live.sck = system;
            Ok((live, got))
        }
        Err(why) => {
            // Nothing will read the system sound now.
            if let Some(session) = system {
                session.stop();
            }
            Err(why)
        }
    }
}

/// avfoundation's device table, from
/// `ffmpeg -f avfoundation -list_devices true -i ""`. It prints the table to
/// stderr and then exits with an error, having opened nothing; only the text
/// matters.
#[cfg(target_os = "macos")]
fn list_avfoundation_devices(ffmpeg: &Path) -> String {
    Command::new(ffmpeg)
        .args(["-hide_banner", "-f", "avfoundation", "-list_devices", "true", "-i", ""])
        .stdin(Stdio::null())
        .output()
        .map(|out| String::from_utf8_lossy(&out.stderr).into_owned())
        .unwrap_or_default()
}

/// Split avfoundation's device table into `(video, audio)` lists of
/// `(index, name)`. The table reads:
///
/// ```text
/// [AVFoundation indev @ 0x..] AVFoundation video devices:
/// [AVFoundation indev @ 0x..] [0] FaceTime HD Camera
/// [AVFoundation indev @ 0x..] [1] Capture screen 0
/// [AVFoundation indev @ 0x..] AVFoundation audio devices:
/// [AVFoundation indev @ 0x..] [0] MacBook Pro Microphone
/// ```
#[cfg(any(target_os = "macos", test))]
fn parse_avfoundation_devices(table: &str) -> (Vec<(usize, String)>, Vec<(usize, String)>) {
    let mut lists: [Vec<(usize, String)>; 2] = [Vec::new(), Vec::new()];
    let mut section = None;
    for line in table.lines() {
        let lower = line.to_ascii_lowercase();
        if lower.contains("video devices:") {
            section = Some(0);
        } else if lower.contains("audio devices:") {
            section = Some(1);
        } else if let (Some(list), Some(device)) = (section, avfoundation_entry(line)) {
            lists[list].push(device);
        }
    }
    let [cameras, sound] = lists;
    (cameras, sound)
}

/// One `"[N] Name"` table row. The index is the line's last bracket, after
/// the log prefix's own; a row without a number or a name isn't a device.
#[cfg(any(target_os = "macos", test))]
fn avfoundation_entry(line: &str) -> Option<(usize, String)> {
    let open = line.rfind('[')?;
    let close = open + line[open..].find(']')?;
    let index = line[open + 1..close].trim().parse().ok()?;
    let name = line[close + 1..].trim();
    (!name.is_empty()).then(|| (index, name.to_string()))
}

/// The avfoundation index of the screen, which it lists as a camera called
/// "Capture screen N". None listed usually means the Screen Recording
/// permission is missing, which hides the screens.
#[cfg(any(target_os = "macos", test))]
fn pick_macos_screen_index(cameras: &[(usize, String)]) -> Result<usize> {
    let is_screen = |name: &str| name.to_ascii_lowercase().contains("capture screen");
    match cameras.iter().find(|(_, name)| is_screen(name)) {
        Some(&(index, _)) => Ok(index),
        None => Err(Error::Recording(
            "ffmpeg lists no screen to capture, which usually means Frame Fang lacks Screen \
             Recording permission. Turn it on in System Settings > Privacy & Security > Screen \
             Recording, then reopen Frame Fang and try again.".into())),
    }
}

/// The avfoundation microphone to use: one whose name sounds like a mic,
/// else the first sound device, else none. System sound doesn't come from
/// this list (ScreenCaptureKit gives it), so loopback devices aren't wanted.
#[cfg(any(target_os = "macos", test))]
fn pick_macos_mic_device(devices: &[(usize, String)]) -> Option<usize> {
    const MIC_WORDS: [&str; 5] = ["microphone", "mic", "built-in", "macbook", "headset"];
    let sounds_like_mic = |name: &str| {
        let lower = name.to_ascii_lowercase();
        MIC_WORDS.iter().any(|word| lower.contains(word))
    };
    let (index, _) = devices.iter().find(|(_, name)| sounds_like_mic(name)).or(devices.first())?;
    Some(*index)
}

/// Windows: gdigrab for the picture and WASAPI for sound, each source sent
/// to ffmpeg over its own 127.0.0.1 socket. Playback in loopback mode gives
/// the system sound and the default capture device the mic, with no Stereo
/// Mix or virtual cable; ffmpeg resamples and mixes them.
#[cfg(target_os = "windows")]
fn launch(file: &Path, wanted: RecordingOptions, area: Option<CaptureRect>) -> Result<(Live, RecordingOptions)> {
    use windows_audio::{SourceKind, StartedSource};

    /// Stop the sources already running, so no capture thread is left.
    fn stop_all(sources: Vec<StartedSource>) {
        for mut source in sources {
            source.pump.request_stop();
            source.pump.finish();
        }
    }

    let path = path_text(file)?;
    let ffmpeg = locate_ffmpeg().unwrap_or_else(|| "ffmpeg.exe".into());

    // Mic first, then system sound; the order only sets input numbers, and
    // amix doesn't care. One that won't start (the mic privacy switch is
    // the usual reason) stops the others and its error is returned.
    let kinds = [(wanted.mic, SourceKind::Microphone), (wanted.system_audio, SourceKind::SystemLoopback)];
    let mut sources: Vec<StartedSource> = Vec::new();
    for (_, kind) in kinds.into_iter().filter(|(on, _)| *on) {
        match windows_audio::start_source(kind) {
            Ok(source) => sources.push(source),
            Err(why) => {
                stop_all(sources);
                return Err(why);
            }
        }
    }

    let specs: Vec<WasapiAudioSpec> = sources.iter().map(|s| s.spec.clone()).collect();
    let command_line = build_windows_ffmpeg_args(path, &specs, area);
    let started = crate::child_guard::spawn_guarded(&mut ffmpeg_command(&ffmpeg, &command_line))
        .map_err(|e| ffmpeg_start_failure(e,
            "ffmpeg.exe wasn't found. The installer normally puts it beside framefang.exe; \
             reinstall, or copy ffmpeg.exe into the folder framefang.exe is in, and try again."))
        .and_then(watch_recorder_startup);
    match started {
        Ok((child, stderr)) => {
            let mut live = Live::bare(child, stderr);
            live.pumps = sources.into_iter().map(|s| s.pump).collect();
            // Any source that failed ended the start above, so the recording
            // has everything that was asked for.
            Ok((live, wanted))
        }
        Err(why) => {
            stop_all(sources);
            Err(why)
        }
    }
}

/// Builds on other systems, where there is no recorder yet.
#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn launch(_: &Path, _: RecordingOptions, _: Option<CaptureRect>) -> Result<(Live, RecordingOptions)> {
    Err(Error::Recording("Frame Fang can't record the screen on this operating system yet".into()))
}

/// A thread sending WASAPI sound to ffmpeg's socket, and the flag that ends
/// it. Made by `windows_audio`.
#[cfg(target_os = "windows")]
struct AudioPump {
    stop:   Arc<std::sync::atomic::AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

#[cfg(target_os = "windows")]
impl AudioPump {
    fn request_stop(&self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
    }

    /// Wait for the thread to end. Safe to call twice.
    fn finish(&mut self) {
        if let Some(thread) = self.handle.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use framefang_core::region::{record_rect_from_selection, DesktopBounds};

    /// One 1920x1080 monitor at the origin.
    fn one_screen() -> DesktopBounds {
        DesktopBounds::new(0, 0, 1920, 1080)
    }

    /// A 640x480 area at (320, 180), as the overlay would hand it over.
    fn window_area() -> CaptureRect {
        record_rect_from_selection((320, 180, 640, 480), one_screen()).unwrap()
    }

    fn words(list: &[&str]) -> Vec<String> {
        list.iter().map(|w| w.to_string()).collect()
    }

    /// Whether `flag` is directly followed by `value`.
    fn has(args: &[String], flag: &str, value: &str) -> bool {
        args.windows(2).any(|pair| pair[0] == flag && pair[1] == value)
    }

    fn value_of<'a>(args: &'a [String], flag: &str) -> &'a str {
        let at = args.iter().position(|a| a == flag).unwrap_or_else(|| panic!("no {flag} in {args:?}"));
        &args[at + 1]
    }

    /// Everything from the video codec on.
    fn encoding(args: &[String]) -> Vec<String> {
        let at = args.iter().position(|a| a == "-c:v").expect("no encoding section");
        args[at..].to_vec()
    }

    fn wasapi(port: u16, rate: u32, channels: u16, fmt: &'static str) -> WasapiAudioSpec {
        WasapiAudioSpec { port, sample_rate: rate, channels, ffmpeg_fmt: fmt }
    }

    const MIX_TWO: &str = "[1:a][2:a]amix=inputs=2:duration=longest:dropout_transition=0[aout]";

    // ---- ffmpeg lookup

    #[test]
    fn chosen_ffmpeg_is_used_only_while_it_is_a_file() {
        let folder = std::env::temp_dir().join(format!("framefang-chosen-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        let chosen = folder.join(if cfg!(windows) { "ffmpeg.exe" } else { "ffmpeg" });
        std::fs::write(&chosen, b"not really ffmpeg").unwrap();

        assert_eq!(locate_ffmpeg_with(Some(&chosen)), Some(chosen.clone()));
        assert_eq!(locate_ffmpeg_with(None), auto_ffmpeg());
        std::fs::remove_dir_all(&folder).unwrap();
        assert_eq!(locate_ffmpeg_with(Some(&chosen)), auto_ffmpeg(), "gone, so search instead");
        assert_eq!(locate_ffmpeg_with(Some(&std::env::temp_dir())), auto_ffmpeg(), "a folder doesn't count");
    }

    // ---- full argument lists, one per branch

    #[test]
    fn x11_lists_are_exact() {
        let tail = |sound: &[&str]| {
            let mut t = words(&["-c:v", "libx264", "-preset", "ultrafast", "-pix_fmt", "yuv420p", "-vf", EVEN_PAD]);
            t.extend(words(sound));
            t.push("/o.mp4".into());
            t
        };
        let head = |inputs: &[&str]| {
            let mut h = words(&["-y", "-f", "x11grab", "-framerate", "30"]);
            h.extend(words(inputs));
            h
        };
        let aac = ["-c:a", "aac", "-b:a", "160k"];

        let mut want = head(&["-i", ":0"]);
        want.extend(tail(&[]));
        assert_eq!(build_linux_ffmpeg_args(":0", "/o.mp4", RecordingOptions::NONE, None, None), want);

        let mut want = head(&["-i", ":0", "-f", "pulse", "-i", "default"]);
        want.extend(tail(&aac));
        assert_eq!(build_linux_ffmpeg_args(":0", "/o.mp4", RecordingOptions::MIC_ONLY, None, None), want);

        let mut want = head(&["-i", ":0", "-f", "pulse", "-i", "s.monitor"]);
        want.extend(tail(&aac));
        assert_eq!(build_linux_ffmpeg_args(":0", "/o.mp4", RecordingOptions::SYSTEM_ONLY, Some("s.monitor"), None),
                   want);

        let mut want = head(&["-video_size", "640x480", "-i", ":0+320,180",
                              "-f", "pulse", "-i", "default", "-f", "pulse", "-i", "s.monitor"]);
        want.extend(tail(&["-filter_complex", MIX_TWO, "-map", "0:v", "-map", "[aout]",
                           "-c:a", "aac", "-b:a", "160k"]));
        assert_eq!(build_linux_ffmpeg_args(":0", "/o.mp4", RecordingOptions::MIC_AND_SYS, Some("s.monitor"),
                                           Some(window_area())), want);
    }

    #[test]
    fn wayland_lists_are_exact() {
        let mut silent = words(&["-y", "-f", "rawvideo", "-pix_fmt", "bgr0", "-video_size", "1920x1080",
                                 "-framerate", "30", "-thread_queue_size", "1024", "-i", "pipe:0"]);
        let mut loud = silent.clone();
        silent.extend(words(&["-c:v", "libx264", "-preset", "ultrafast", "-pix_fmt", "yuv420p",
                              "-vf", EVEN_PAD, "/o.mp4"]));
        assert_eq!(build_wayland_ffmpeg_args("bgr0", 1920, 1080, 30, "/o.mp4", RecordingOptions::NONE, None, None),
                   silent);

        loud.extend(words(&["-f", "pulse", "-i", "default", "-f", "pulse", "-i", "s.monitor",
                            "-c:v", "libx264", "-preset", "ultrafast", "-pix_fmt", "yuv420p",
                            "-vf", "crop=640:480:320:180,pad=ceil(iw/2)*2:ceil(ih/2)*2",
                            "-filter_complex", MIX_TWO, "-map", "0:v", "-map", "[aout]",
                            "-c:a", "aac", "-b:a", "160k", "/o.mp4"]));
        assert_eq!(build_wayland_ffmpeg_args("bgr0", 1920, 1080, 30, "/o.mp4", RecordingOptions::MIC_AND_SYS,
                                             Some("s.monitor"), Some(window_area())), loud);
    }

    #[test]
    fn macos_ffmpeg_lists_are_exact() {
        let input = |devices: &str| words(&["-y", "-f", "avfoundation", "-framerate", "30", "-i", devices]);
        let socket = |port: &str| words(&["-thread_queue_size", "1024", "-f", "f32le", "-ar", "48000",
                                          "-ac", "2", "-i", port]);
        let encode = |vf: &str| words(&["-c:v", "libx264", "-preset", "ultrafast", "-pix_fmt", "yuv420p", "-vf", vf]);
        let stereo_aac = words(&["-c:a", "aac", "-b:a", "160k", "-ac", "2", "/o.mp4"]);

        let mut want = input("2:");
        want.extend(encode(EVEN_PAD));
        want.push("/o.mp4".into());
        assert_eq!(build_macos_ffmpeg_args(2, None, None, "/o.mp4", None), want);

        let mut want = input("1:0");
        want.extend(encode(EVEN_PAD));
        want.extend(words(&["-map", "0:v", "-map", "0:a"]));
        want.extend(stereo_aac.clone());
        assert_eq!(build_macos_ffmpeg_args(1, Some(0), None, "/o.mp4", None), want);

        let mut want = input("2:");
        want.extend(socket("tcp://127.0.0.1:50321"));
        want.extend(encode(EVEN_PAD));
        want.extend(words(&["-map", "0:v", "-map", "1:a"]));
        want.extend(stereo_aac.clone());
        assert_eq!(build_macos_ffmpeg_args(2, None, Some(50321), "/o.mp4", None), want);

        let mut want = input("1:0");
        want.extend(socket("tcp://127.0.0.1:40044"));
        want.extend(encode("crop=640:480:320:180,pad=ceil(iw/2)*2:ceil(ih/2)*2"));
        want.extend(words(&["-filter_complex",
                            "[0:a][1:a]amix=inputs=2:duration=longest:dropout_transition=0[aout]",
                            "-map", "0:v", "-map", "[aout]"]));
        want.extend(stereo_aac);
        assert_eq!(build_macos_ffmpeg_args(1, Some(0), Some(40044), "/o.mp4", Some(window_area())), want);
    }

    #[test]
    fn windows_lists_are_exact() {
        let encode = words(&["-c:v", "libx264", "-preset", "ultrafast", "-pix_fmt", "yuv420p", "-vf", EVEN_PAD]);
        let stereo_aac = words(&["-c:a", "aac", "-b:a", "160k", "-ac", "2", "C:/o.mp4"]);

        let mut want = words(&["-y", "-f", "gdigrab", "-framerate", "30", "-i", "desktop"]);
        want.extend(encode.clone());
        want.push("C:/o.mp4".into());
        assert_eq!(build_windows_ffmpeg_args("C:/o.mp4", &[], None), want);

        let mut want = words(&["-y", "-f", "gdigrab", "-framerate", "30",
                               "-offset_x", "320", "-offset_y", "180", "-video_size", "640x480",
                               "-i", "desktop",
                               "-thread_queue_size", "1024", "-f", "f32le", "-ar", "48000", "-ac", "2",
                               "-i", "tcp://127.0.0.1:54123"]);
        want.extend(encode.clone());
        want.extend(words(&["-map", "0:v", "-map", "1:a"]));
        want.extend(stereo_aac.clone());
        assert_eq!(build_windows_ffmpeg_args("C:/o.mp4", &[wasapi(54123, 48000, 2, "f32le")], Some(window_area())),
                   want);

        let mut want = words(&["-y", "-f", "gdigrab", "-framerate", "30", "-i", "desktop",
                               "-thread_queue_size", "1024", "-f", "f32le", "-ar", "48000", "-ac", "2",
                               "-i", "tcp://127.0.0.1:40001",
                               "-thread_queue_size", "1024", "-f", "s16le", "-ar", "44100", "-ac", "1",
                               "-i", "tcp://127.0.0.1:40002"]);
        want.extend(encode);
        want.extend(words(&["-filter_complex", MIX_TWO, "-map", "0:v", "-map", "[aout]"]));
        want.extend(stereo_aac);
        assert_eq!(build_windows_ffmpeg_args("C:/o.mp4",
            &[wasapi(40001, 48000, 2, "f32le"), wasapi(40002, 44100, 1, "s16le")], None), want);
    }

    #[test]
    fn three_windows_sources_are_mixed_together() {
        let sources = [wasapi(1, 48000, 2, "f32le"), wasapi(2, 48000, 2, "f32le"), wasapi(3, 48000, 2, "f32le")];
        let args = build_windows_ffmpeg_args("C:/o.mp4", &sources, None);
        assert_eq!(value_of(&args, "-filter_complex"),
                   "[1:a][2:a][3:a]amix=inputs=3:duration=longest:dropout_transition=0[aout]");
    }

    // ---- X11

    #[test]
    fn x11_without_sound_has_no_pulse_inputs_or_audio_codec() {
        let args = build_linux_ffmpeg_args(":0", "/tmp/out.mp4", RecordingOptions::NONE, None, None);
        assert!(has(&args, "-f", "x11grab"));
        assert!(has(&args, "-c:v", "libx264"));
        assert!(!args.iter().any(|a| a == "pulse" || a == "-c:a"), "{args:?}");
        assert_eq!(args.last().unwrap(), "/tmp/out.mp4");
    }

    #[test]
    fn x11_mic_is_the_default_pulse_source() {
        let args = build_linux_ffmpeg_args(":0", "/tmp/out.mp4", RecordingOptions::MIC_ONLY, None, None);
        assert!(has(&args, "-f", "pulse"));
        assert!(has(&args, "-i", "default"));
        assert!(has(&args, "-c:a", "aac"));
    }

    #[test]
    fn x11_mic_and_monitor_are_mixed() {
        let args = build_linux_ffmpeg_args(":0", "/tmp/out.mp4", RecordingOptions::MIC_AND_SYS,
                                           Some("alsa_output.0.monitor"), None);
        assert!(value_of(&args, "-filter_complex").contains("amix=inputs=2"));
        assert!(has(&args, "-i", "alsa_output.0.monitor"));
    }

    #[test]
    fn x11_region_declares_its_size_before_the_offset_display() {
        let args = build_linux_ffmpeg_args(":0", "/tmp/out.mp4", RecordingOptions::NONE, None, Some(window_area()));
        assert!(has(&args, "-video_size", "640x480"));
        assert!(has(&args, "-i", ":0+320,180"));
        let size = args.iter().position(|a| a == "-video_size").unwrap();
        let input = args.iter().position(|a| a == "-i").unwrap();
        assert!(size < input, "x11grab only reads -video_size ahead of its -i: {args:?}");
    }

    #[test]
    fn x11_full_screen_has_a_bare_display() {
        let args = build_linux_ffmpeg_args(":0", "/tmp/out.mp4", RecordingOptions::NONE, None, None);
        assert!(!args.iter().any(|a| a == "-video_size"));
        assert!(has(&args, "-i", ":0"));
    }

    #[test]
    fn x11_region_leaves_the_sound_alone() {
        let monitor = Some("alsa_output.0.monitor");
        let whole = build_linux_ffmpeg_args(":0", "/o.mp4", RecordingOptions::MIC_AND_SYS, monitor, None);
        let part = build_linux_ffmpeg_args(":0", "/o.mp4", RecordingOptions::MIC_AND_SYS, monitor, Some(window_area()));
        assert_eq!(encoding(&whole), encoding(&part));
        assert!(has(&part, "-i", "alsa_output.0.monitor"));
    }

    #[test]
    fn x11_region_counts_from_the_desktop_corner() {
        // The root window begins at the desktop's corner, here 1920 left of
        // the primary monitor.
        let desktop = DesktopBounds::new(-1920, 0, 3840, 1080);
        let area = record_rect_from_selection((-1600, 200, 800, 600), desktop).unwrap();
        let args = build_linux_ffmpeg_args(":0.0", "/tmp/out.mp4", RecordingOptions::NONE, None, Some(area));
        assert!(has(&args, "-i", ":0.0+320,200"), "{args:?}");
    }

    #[test]
    fn losing_system_sound_keeps_the_mic() {
        // What the Linux path does when no default sink can be named.
        let mut got = RecordingOptions::MIC_AND_SYS;
        got.system_audio = false;
        assert_eq!(got, RecordingOptions::MIC_ONLY);
        let args = build_linux_ffmpeg_args(":0", "/tmp/out.mp4", got, None, None);
        assert!(has(&args, "-i", "default"));
        assert!(!args.iter().any(|a| a.ends_with(".monitor") || a == "-filter_complex"), "{args:?}");
    }

    // ---- Wayland

    #[test]
    fn wayland_video_comes_from_stdin_with_its_size_and_rate() {
        let args = build_wayland_ffmpeg_args("bgr0", 2560, 1440, 30, "/tmp/out.mp4", RecordingOptions::NONE, None, None);
        assert!(has(&args, "-f", "rawvideo"));
        assert!(has(&args, "-i", "pipe:0"));
        assert!(has(&args, "-video_size", "2560x1440"));
        assert!(has(&args, "-framerate", "30"));
        assert!(!args.iter().any(|a| a == "x11grab"), "{args:?}");
        assert_eq!(args.last().unwrap(), "/tmp/out.mp4");
    }

    #[test]
    fn wayland_input_and_output_pixel_formats_are_separate() {
        let args = build_wayland_ffmpeg_args("bgra", 800, 600, 30, "/tmp/out.mp4", RecordingOptions::NONE, None, None);
        let input = args.iter().position(|a| a == "-i").unwrap();
        let first = args.iter().position(|a| a == "-pix_fmt").unwrap();
        let last = args.iter().rposition(|a| a == "-pix_fmt").unwrap();
        assert!(first < input && args[first + 1] == "bgra", "{args:?}");
        assert!(last > input && args[last + 1] == "yuv420p", "{args:?}");
    }

    #[test]
    fn wayland_sound_is_wired_like_x11() {
        let args = build_wayland_ffmpeg_args("bgr0", 1920, 1080, 30, "/tmp/out.mp4", RecordingOptions::MIC_AND_SYS,
                                             Some("alsa_output.0.monitor"), None);
        assert!(has(&args, "-i", "default"));
        assert!(has(&args, "-i", "alsa_output.0.monitor"));
        assert!(value_of(&args, "-filter_complex").starts_with("[1:a][2:a]amix=inputs=2"));
        assert!(has(&args, "-map", "0:v") && has(&args, "-map", "[aout]"));
    }

    #[test]
    fn wayland_and_x11_encode_the_same_way() {
        for options in [RecordingOptions::NONE, RecordingOptions::MIC_ONLY,
                        RecordingOptions::SYSTEM_ONLY, RecordingOptions::MIC_AND_SYS] {
            let monitor = options.system_audio.then_some("sink.monitor");
            let x11 = build_linux_ffmpeg_args(":0", "/tmp/out.mp4", options, monitor, None);
            let wayland = build_wayland_ffmpeg_args("bgr0", 1920, 1080, 30, "/tmp/out.mp4", options, monitor, None);
            assert_eq!(encoding(&x11), encoding(&wayland), "{options:?}");
        }
    }

    #[test]
    fn wayland_region_crops_from_the_desktop_corner() {
        let desktop = DesktopBounds::new(-1920, 0, 3840, 1080);
        let area = record_rect_from_selection((-1600, 200, 800, 600), desktop).unwrap();
        let args = build_wayland_ffmpeg_args("bgr0", 3840, 1080, 30, "/o.mp4", RecordingOptions::NONE, None, Some(area));
        assert_eq!(value_of(&args, "-vf"), "crop=800:600:320:200,pad=ceil(iw/2)*2:ceil(ih/2)*2");
    }

    // ---- Windows

    #[test]
    fn windows_silent_recording_opens_no_socket() {
        let args = build_windows_ffmpeg_args("C:/tmp/out.mp4", &[], None);
        assert!(has(&args, "-f", "gdigrab") && has(&args, "-i", "desktop"));
        assert!(has(&args, "-c:v", "libx264"));
        assert!(!args.iter().any(|a| a.starts_with("tcp://") || a == "-c:a"), "{args:?}");
        assert_eq!(args.last().unwrap(), "C:/tmp/out.mp4");
    }

    #[test]
    fn windows_one_source_is_input_one_in_the_device_format() {
        let args = build_windows_ffmpeg_args("C:/tmp/out.mp4", &[wasapi(54123, 48000, 2, "f32le")], None);
        for (flag, value) in [("-f", "f32le"), ("-ar", "48000"), ("-ac", "2"), ("-i", "tcp://127.0.0.1:54123"),
                              ("-map", "0:v"), ("-map", "1:a"), ("-c:a", "aac")] {
            assert!(has(&args, flag, value), "{flag} {value} missing: {args:?}");
        }
    }

    #[test]
    fn windows_two_sources_are_mixed_and_keep_their_formats() {
        let args = build_windows_ffmpeg_args("C:/tmp/out.mp4",
            &[wasapi(40001, 48000, 2, "f32le"), wasapi(40002, 44100, 1, "s16le")], None);
        assert!(value_of(&args, "-filter_complex").starts_with("[1:a][2:a]amix=inputs=2"));
        assert!(has(&args, "-map", "[aout]"));
        assert!(has(&args, "-i", "tcp://127.0.0.1:40001") && has(&args, "-i", "tcp://127.0.0.1:40002"));
        assert!(has(&args, "-f", "s16le"));
    }

    #[test]
    fn windows_sound_inputs_get_a_deep_queue() {
        let args = build_windows_ffmpeg_args("C:/tmp/out.mp4", &[wasapi(50000, 48000, 2, "f32le")], None);
        assert!(has(&args, "-thread_queue_size", "1024"));
    }

    #[test]
    fn windows_region_offsets_come_before_the_input() {
        let args = build_windows_ffmpeg_args("C:/tmp/out.mp4", &[], Some(window_area()));
        assert!(has(&args, "-offset_x", "320") && has(&args, "-offset_y", "180"));
        assert!(has(&args, "-video_size", "640x480") && has(&args, "-i", "desktop"));
        let offset = args.iter().position(|a| a == "-offset_x").unwrap();
        let input = args.iter().position(|a| a == "-i").unwrap();
        assert!(offset < input, "{args:?}");
    }

    #[test]
    fn windows_region_counts_from_the_virtual_screen_corner() {
        // SM_XVIRTUALSCREEN is negative with a monitor left of the primary.
        let desktop = DesktopBounds::new(-1920, -120, 3840, 1200);
        let area = record_rect_from_selection((-1000, 0, 500, 400), desktop).unwrap();
        let args = build_windows_ffmpeg_args("C:/tmp/out.mp4", &[], Some(area));
        assert!(has(&args, "-offset_x", "920") && has(&args, "-offset_y", "120"), "{args:?}");
    }

    #[test]
    fn windows_region_leaves_the_sound_alone() {
        let sources = [wasapi(40001, 48000, 2, "f32le"), wasapi(40002, 44100, 1, "s16le")];
        let whole = build_windows_ffmpeg_args("C:/o.mp4", &sources, None);
        let part = build_windows_ffmpeg_args("C:/o.mp4", &sources, Some(window_area()));
        assert_eq!(encoding(&whole), encoding(&part));
        assert!(has(&part, "-i", "tcp://127.0.0.1:40001") && has(&part, "-i", "tcp://127.0.0.1:40002"));
    }

    #[test]
    fn windows_full_screen_has_no_offsets() {
        let args = build_windows_ffmpeg_args("C:/tmp/out.mp4", &[], None);
        assert!(!args.iter().any(|a| a == "-offset_x" || a == "-video_size"), "{args:?}");
    }

    // ---- macOS

    #[test]
    fn macos_mic_rides_in_the_avfoundation_input() {
        let args = build_macos_ffmpeg_args(1, Some(0), None, "/tmp/out.mp4", None);
        assert!(has(&args, "-f", "avfoundation") && has(&args, "-i", "1:0"));
        assert!(!args.iter().any(|a| a.starts_with("tcp://")));
        assert!(has(&args, "-map", "0:a") && has(&args, "-c:a", "aac") && has(&args, "-ac", "2"));
        assert_eq!(args.last().unwrap(), "/tmp/out.mp4");
    }

    #[test]
    fn macos_system_sound_is_a_socket_input() {
        let args = build_macos_ffmpeg_args(2, None, Some(50321), "/tmp/out.mp4", None);
        assert!(has(&args, "-i", "2:") && has(&args, "-i", "tcp://127.0.0.1:50321"));
        assert!(has(&args, "-f", "f32le") && has(&args, "-map", "1:a") && has(&args, "-c:a", "aac"));
    }

    #[test]
    fn macos_mic_and_system_sound_are_mixed() {
        let args = build_macos_ffmpeg_args(1, Some(0), Some(40044), "/tmp/out.mp4", None);
        assert!(value_of(&args, "-filter_complex").starts_with("[0:a][1:a]amix=inputs=2"));
        assert!(has(&args, "-map", "[aout]") && has(&args, "-i", "tcp://127.0.0.1:40044"));
    }

    #[test]
    fn macos_without_sound_has_no_audio_codec() {
        let args = build_macos_ffmpeg_args(2, None, None, "/tmp/out.mp4", None);
        assert!(has(&args, "-i", "2:"));
        assert!(!args.iter().any(|a| a == "-c:a"), "{args:?}");
    }

    #[test]
    fn macos_region_is_a_crop_ahead_of_the_pad() {
        let args = build_macos_ffmpeg_args(1, Some(0), None, "/tmp/out.mp4", Some(window_area()));
        assert_eq!(value_of(&args, "-vf"), "crop=640:480:320:180,pad=ceil(iw/2)*2:ceil(ih/2)*2");
        assert!(has(&args, "-map", "0:a"));
    }

    #[test]
    fn macos_full_screen_only_pads() {
        let args = build_macos_ffmpeg_args(1, None, None, "/tmp/out.mp4", None);
        assert_eq!(value_of(&args, "-vf"), EVEN_PAD);
    }

    #[test]
    fn screencapture_gets_the_absolute_rectangle() {
        // It counts from the main display's corner, like the absolute fields.
        assert_eq!(build_macos_screencapture_args("/tmp/out.mp4", Some(window_area())),
                   words(&["-v", "-R", "320,180,640,480", "/tmp/out.mp4"]));
    }

    #[test]
    fn screencapture_rectangle_is_in_points_on_retina() {
        let retina = DesktopBounds::new(0, 0, 2880, 1800).with_scale(2.0);
        let area = record_rect_from_selection((200, 100, 800, 600), retina).unwrap();
        assert_eq!(build_macos_screencapture_args("/tmp/out.mp4", Some(area)),
                   words(&["-v", "-R", "100,50,400,300", "/tmp/out.mp4"]));
        // avfoundation's frame is in device pixels, so its crop is not halved.
        let args = build_macos_ffmpeg_args(1, None, None, "/tmp/out.mp4", Some(area));
        assert!(value_of(&args, "-vf").starts_with("crop=800:600:200:100,"));
    }

    #[test]
    fn screencapture_full_screen_is_just_the_file() {
        assert_eq!(build_macos_screencapture_args("/tmp/out.mp4", None), words(&["-v", "/tmp/out.mp4"]));
    }

    #[test]
    fn avfoundation_table_is_split_into_video_and_audio() {
        let table = "\
[AVFoundation indev @ 0x600002d1c000] AVFoundation video devices:
[AVFoundation indev @ 0x600002d1c000] [0] Studio Display Camera
[AVFoundation indev @ 0x600002d1c000] [1] Capture screen 0
[AVFoundation indev @ 0x600002d1c000] [2] Capture screen 1
[AVFoundation indev @ 0x600002d1c000] AVFoundation audio devices:
[AVFoundation indev @ 0x600002d1c000] [0] Studio Display Microphone
[AVFoundation indev @ 0x600002d1c000] [1] Loopback Audio
";
        let (video, audio) = parse_avfoundation_devices(table);
        let named = |list: &[(usize, &str)]| list.iter().map(|&(i, n)| (i, n.to_string())).collect::<Vec<_>>();
        assert_eq!(video, named(&[(0, "Studio Display Camera"), (1, "Capture screen 0"), (2, "Capture screen 1")]));
        assert_eq!(audio, named(&[(0, "Studio Display Microphone"), (1, "Loopback Audio")]));
    }

    #[test]
    fn an_empty_avfoundation_table_has_no_devices() {
        let (video, audio) = parse_avfoundation_devices("");
        assert!(video.is_empty() && audio.is_empty());
    }

    #[test]
    fn avfoundation_rows_outside_a_section_or_without_a_number_are_skipped() {
        let table = "\
[AVFoundation indev @ 0x1] [0] Before any heading
[AVFoundation indev @ 0x1] AVFoundation audio devices:
[AVFoundation indev @ 0x1] [x] Not a number
[AVFoundation indev @ 0x1] [2]
[AVFoundation indev @ 0x1] [3] Real Mic
";
        let (video, audio) = parse_avfoundation_devices(table);
        assert!(video.is_empty());
        assert_eq!(audio, vec![(3, "Real Mic".to_string())]);
    }

    #[test]
    fn the_screen_device_is_found_by_name() {
        let cameras = [(0, "USB Webcam".to_string()), (4, "Capture Screen 0".to_string()),
                       (5, "Capture screen 1".to_string())];
        assert_eq!(pick_macos_screen_index(&cameras).ok(), Some(4), "the first screen, any case");
    }

    #[test]
    fn no_screen_device_points_at_the_permission() {
        let Err(Error::Recording(message)) = pick_macos_screen_index(&[(0, "FaceTime HD Camera".to_string())])
        else { panic!("expected a recording error") };
        assert!(message.to_lowercase().contains("screen recording"), "{message}");
    }

    #[test]
    fn a_mic_sounding_device_beats_the_first_one() {
        let devices = [(0, "Aggregate Device".to_string()), (1, "Loopback Audio".to_string()),
                       (2, "USB Headset".to_string())];
        assert_eq!(pick_macos_mic_device(&devices), Some(2));
    }

    #[test]
    fn with_no_mic_sounding_device_the_first_is_used() {
        let devices = [(3, "Line In".to_string()), (7, "Aux".to_string())];
        assert_eq!(pick_macos_mic_device(&devices), Some(3));
    }

    #[test]
    fn no_sound_devices_means_no_mic() {
        assert!(pick_macos_mic_device(&[]).is_none());
    }

    // ---- every backend

    #[test]
    fn an_odd_selection_reaches_every_backend_even() {
        let area = record_rect_from_selection((11, 23, 401, 303), one_screen()).unwrap();
        assert_eq!(area.video_size(), "400x302");
        assert!(has(&build_linux_ffmpeg_args(":0", "/o.mp4", RecordingOptions::NONE, None, Some(area)),
                    "-video_size", "400x302"));
        assert!(has(&build_windows_ffmpeg_args("C:/o.mp4", &[], Some(area)), "-video_size", "400x302"));
        assert!(build_macos_screencapture_args("/o.mp4", Some(area)).iter().any(|a| a == "11,23,400,302"));
        let mac = build_macos_ffmpeg_args(1, None, None, "/o.mp4", Some(area));
        assert!(value_of(&mac, "-vf").starts_with("crop=400:302:"));
    }

    #[test]
    fn has_audio_is_true_for_any_source() {
        assert!(!RecordingOptions::NONE.has_audio());
        for on in [RecordingOptions::MIC_ONLY, RecordingOptions::SYSTEM_ONLY, RecordingOptions::MIC_AND_SYS] {
            assert!(on.has_audio(), "{on:?}");
        }
    }

    // ---- stderr

    #[test]
    fn the_tail_keeps_only_the_newest_lines() {
        let tail = RecentStderr::new();
        for n in 0..KEPT_LINES + 20 {
            tail.keep(format!("line {n}").as_bytes());
        }
        let text = tail.joined();
        assert_eq!(KEPT_LINES, text.lines().count());
        assert!(text.ends_with(&format!("line {}", KEPT_LINES + 19)), "{text}");
        assert!(!text.lines().any(|l| l == "line 0"), "{text}");
    }

    #[test]
    fn progress_and_blank_lines_are_not_kept() {
        let tail = RecentStderr::new();
        tail.keep(b"No space left on device");
        for _ in 0..KEPT_LINES * 2 {
            tail.keep(b"frame= 1234 fps= 30 q=28.0 size=   65536kB");
            tail.keep(b"size=   65536kB time=00:00:10.00");
            tail.keep(b"   ");
        }
        assert_eq!(tail.joined(), "No space left on device");
    }

    #[test]
    fn kept_lines_are_printable_ascii() {
        let tail = RecentStderr::new();
        tail.keep("caf\u{e9}.mp4: \u{7} Invalid\targument".as_bytes());
        assert_eq!(tail.joined(), "caf?.mp4: ? Invalid?argument");
    }

    #[test]
    fn the_error_suffix_is_empty_until_something_is_said() {
        let tail = RecentStderr::new();
        assert_eq!(tail.as_suffix(), "");
        tail.keep(b"Device or resource busy");
        assert_eq!(tail.as_suffix(), "\n\nDevice or resource busy");
    }

    #[test]
    fn startup_diagnosis_keeps_the_last_six_lines() {
        let said = (1..=10).map(|n| format!("  line {n}  ")).collect::<Vec<_>>().join("\n\n");
        assert_eq!(startup_diagnosis(&said), "line 5\nline 6\nline 7\nline 8\nline 9\nline 10");
    }

    #[test]
    fn startup_diagnosis_gives_advice_when_nothing_was_said() {
        let advice = startup_diagnosis(" \n \n");
        assert!(advice.contains("capture device") && advice.is_ascii(), "{advice}");
    }
}
