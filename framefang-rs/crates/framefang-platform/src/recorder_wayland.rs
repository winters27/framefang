//! Screen recording on Wayland: the ScreenCast portal gives us a PipeWire
//! stream, and we pass its frames to ffmpeg on stdin.
//!
//! ffmpeg cannot read PipeWire itself, and `x11grab` is no use on Wayland
//! (XWayland answers it with black frames, the reason
//! [`crate::session::is_wayland`] exists). So on Wayland this module is the
//! capture device: it asks `xdg-desktop-portal` for a screen cast, reads the
//! PipeWire buffers, and writes raw frames to ffmpeg as `-f rawvideo`.
//!
//! stdin is free for this. Linux audio still comes in through ffmpeg's
//! `-f pulse` inputs (PipeWire's Pulse layer serves those unchanged), and the
//! loopback sockets used for audio on Windows and macOS are not involved.
//!
//! # Threads
//!
//! A [`CastSession`] owns three:
//!
//! * **portal**: performs `CreateSession`, `SelectSources`, `Start` and
//!   `OpenPipeWireRemote`, then keeps the session open until told to stop.
//!   Closing the session removes the PipeWire node, so it has to outlive the
//!   stream.
//! * **pipewire**: runs the PipeWire main loop. Each `process` callback copies
//!   the frame into a one-frame slot, replacing whatever was there. It never
//!   touches the pipe, so a slow encoder cannot hold up the compositor.
//! * **writer**: wakes at a steady rate, takes the newest frame from the slot
//!   and writes it to ffmpeg, writing the previous frame again if nothing new
//!   came in.
//!
//! # Why the writer keeps its own time
//!
//! Raw video has no timestamps; ffmpeg times it by counting frames at the
//! `-framerate` it was given. Compositors only send a frame when the screen
//! changes, so writing frames as they arrive would squeeze half a minute of
//! still screen into a single frame's time. Repeating the last frame at a
//! steady rate keeps the file's length equal to the time that passed.
//!
//! # Failing early
//!
//! [`CastSession::start`] waits until a real frame has been copied out of a
//! real PipeWire buffer. The format given to ffmpeg is therefore measured,
//! and a cast that negotiates but never delivers (unmappable GPU buffers, a
//! stuck compositor) fails with a message before any file exists. The app
//! never reports a recording that is not happening.

use std::{
    io::Write as _,
    os::fd::OwnedFd,
    process::ChildStdin,
    sync::{
        atomic::{AtomicBool, Ordering::Relaxed},
        mpsc::{self, Receiver, Sender},
        Arc, Condvar, Mutex,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use ashpd::desktop::{
    screencast::{CursorMode, Screencast, SourceType},
    PersistMode, Session,
};
use pipewire as pw;
use pw::spa::param::video::VideoFormat;
use pw::spa::utils::{Fraction, Rectangle};

use crate::{Error, Result};

/// The frame rate requested from the compositor and written to ffmpeg. The
/// same 30 used for recording everywhere else, so files match across
/// platforms.
pub const TARGET_FPS: u32 = 30;

/// How long the portal may take to produce a stream: 90 seconds.
///
/// A person is in this wait: most desktops show a screen-sharing prompt where
/// the user picks a monitor and confirms. Long enough not to give up on
/// someone still deciding, finite so a portal that never answers cannot hang
/// the app.
const SHARE_PROMPT_WAIT: Duration = Duration::from_secs(90);

/// How long after the portal hands over a node the first frame may take: 10
/// seconds. Only negotiation and one repaint happen here, so running out
/// means something is broken.
const FIRST_FRAME_WAIT: Duration = Duration::from_secs(10);

/// Every pixel layout we ask for is four bytes per pixel.
const BYTES_PER_PIXEL: usize = 4;

/// The packed 32-bit layouts we accept, in the order they are offered to the
/// compositor (the first is the preferred one), with ffmpeg's `-pix_fmt` name
/// for each.
///
/// PipeWire and ffmpeg both name packed layouts by byte order in memory, so
/// the pairs are direct: `BGRx` and `bgr0` are both blue, green, red, unused.
/// Offering only these keeps the frame copy a plain row copy with no
/// conversion.
const PACKED_FORMATS: [(VideoFormat, &str); 8] = [
    (VideoFormat::BGRx, "bgr0"),
    (VideoFormat::RGBx, "rgb0"),
    (VideoFormat::BGRA, "bgra"),
    (VideoFormat::RGBA, "rgba"),
    (VideoFormat::xRGB, "0rgb"),
    (VideoFormat::xBGR, "0bgr"),
    (VideoFormat::ARGB, "argb"),
    (VideoFormat::ABGR, "abgr"),
];

/// Frame sizes offered: any size from 1x1 to 16384x16384, 1080p preferred.
const SIZE_PREFERRED: Rectangle = Rectangle { width: 1920, height: 1080 };
const SIZE_SMALLEST: Rectangle = Rectangle { width: 1, height: 1 };
const SIZE_LARGEST: Rectangle = Rectangle { width: 16_384, height: 16_384 };

/// Frame rates offered: anything up to 240, our own rate preferred.
const RATE_PREFERRED: Fraction = Fraction { num: TARGET_FPS, denom: 1 };
const RATE_LOWEST: Fraction = Fraction { num: 0, denom: 1 };
const RATE_HIGHEST: Fraction = Fraction { num: 240, denom: 1 };

/// The screen-sharing grant from an earlier recording in this run, so the
/// next recording does not ask again.
///
/// Kept in memory only. [`PersistMode::Application`] limits the grant to this
/// run of the app, which is as much as a screen recorder should hold on to
/// without asking.
static RESTORE_TOKEN: Mutex<Option<String>> = Mutex::new(None);

/// The pixel layout the compositor chose, described the way ffmpeg needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CastFormat {
    pub width: u32,
    pub height: u32,
    /// ffmpeg's `-pix_fmt` name. Always one of the four-byte packed layouts
    /// in `PACKED_FORMATS`.
    pub pix_fmt: &'static str,
}

impl CastFormat {
    /// Stands in until the real format is known; never handed out.
    const UNKNOWN: CastFormat = CastFormat { pix_fmt: "bgr0", width: 0, height: 0 };

    /// Bytes in one row with no padding.
    fn row_len(&self) -> usize {
        self.width as usize * BYTES_PER_PIXEL
    }

    /// Bytes in one frame with no padding.
    fn bytes_per_frame(&self) -> usize {
        self.row_len() * self.height as usize
    }
}

/// ffmpeg's name for a PipeWire video format, if it is one we accept.
fn ffmpeg_pix_fmt(format: VideoFormat) -> Option<&'static str> {
    PACKED_FORMATS
        .iter()
        .find(|(offered, _)| *offered == format)
        .map(|&(_, name)| name)
}

/// The cast format for a negotiated layout and size, or `None` when the
/// layout is not one we offered or the size is empty.
fn cast_format(format: VideoFormat, width: u32, height: u32) -> Option<CastFormat> {
    let pix_fmt = ffmpeg_pix_fmt(format)?;
    (width > 0 && height > 0).then_some(CastFormat { width, height, pix_fmt })
}

/// Copy `rows` rows of `row_len` bytes into `out`, the first starting at
/// `offset` in `src` and each next one `stride` bytes on.
///
/// Returns false if `src` ends early. A partial frame would put a torn
/// picture in the file, so the caller drops it and waits for the next one.
fn copy_rows(
    src: &[u8],
    offset: usize,
    stride: usize,
    row_len: usize,
    rows: usize,
    out: &mut Vec<u8>,
) -> bool {
    out.clear();
    out.reserve(row_len * rows);
    for row in 0..rows {
        let start = offset + row * stride;
        let Some(line) = src.get(start..start + row_len) else { return false };
        out.extend_from_slice(line);
    }
    true
}

/// A recording error that reads "`what`: `cause`".
fn failed<E: std::fmt::Display>(what: &'static str) -> impl FnOnce(E) -> Error {
    move |cause| Error::Recording(format!("{what}: {cause}"))
}

/// Start a named helper thread; `what` names it in the error if it cannot.
fn spawn_worker(name: &str, what: &'static str, body: impl FnOnce() + Send + 'static)
    -> Result<JoinHandle<()>>
{
    thread::Builder::new().name(name.to_owned()).spawn(body).map_err(failed(what))
}

// ---- handing frames from PipeWire to the writer ----

/// A single frame passed from the PipeWire thread to the writer.
///
/// Only one is ever held: if the writer falls behind, the newest frame is the
/// one worth sending, and a queue would only grow. The counter lets the
/// writer tell a new frame from no news without comparing pixels.
struct LatestFrame {
    latest: Mutex<Frame>,
    arrived: Condvar,
}

struct Frame {
    pixels: Vec<u8>,
    /// How many frames have been published; 0 means none yet.
    count: u64,
}

impl LatestFrame {
    fn empty() -> Self {
        LatestFrame {
            latest: Mutex::new(Frame { pixels: Vec::new(), count: 0 }),
            arrived: Condvar::new(),
        }
    }

    /// Publish the frame in `pixels`. The buffers are swapped rather than
    /// copied, so the caller gets an old allocation back to reuse.
    fn publish(&self, pixels: &mut Vec<u8>) {
        let Ok(mut latest) = self.latest.lock() else { return };
        std::mem::swap(&mut latest.pixels, pixels);
        latest.count = latest.count.saturating_add(1);
        self.arrived.notify_all();
    }

    /// Wait up to `wait` for the first frame ever. True if one arrived.
    fn first_arrives_within(&self, wait: Duration) -> bool {
        let Ok(latest) = self.latest.lock() else { return false };
        self.arrived
            .wait_timeout_while(latest, wait, |f| f.count == 0)
            .is_ok_and(|(latest, _)| latest.count != 0)
    }

    /// If a frame newer than `seen` is waiting, swap it into `pixels`, move
    /// `seen` up to it and return true. Otherwise leave both alone and return
    /// false, and the caller sends what it already has.
    fn take_newer(&self, pixels: &mut Vec<u8>, seen: &mut u64) -> bool {
        let Ok(mut latest) = self.latest.lock() else { return false };
        if latest.count == *seen {
            return false;
        }
        std::mem::swap(&mut latest.pixels, pixels);
        *seen = latest.count;
        true
    }
}

/// Whether the cast is still delivering, as seen by the PipeWire thread.
///
/// A recording can lose its picture without ffmpeg noticing: the user presses
/// "Stop sharing" in the desktop's indicator, or the compositor ends the
/// session. Frames just stop, and because the writer repeats the last one the
/// file fills with a frozen image. A lack of frames is not the sign to watch
/// (a still screen sends none, quite properly); the stream leaving the
/// streaming state is.
#[derive(Default)]
struct Liveness {
    /// The latest reason the cast failed, or `None` while it is fine.
    lost: Mutex<Option<String>>,
}

impl Liveness {
    fn mark_lost(&self, reason: String) {
        *self.lost.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(reason);
    }

    fn reason(&self) -> Option<String> {
        self.lost.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clone()
    }
}

// ---- the session ----

/// The one message the PipeWire thread accepts: quit the main loop.
struct Quit;

/// What the PipeWire thread sends once: the format with the first frame, or
/// why it could not get one.
type FirstFrame = std::result::Result<CastFormat, String>;

/// What the portal thread sends once: the node and remote to read from, or
/// why the cast could not be set up.
type PortalReady = std::result::Result<(u32, OwnedFd), String>;

/// A running screen cast: the portal session, the PipeWire stream and, once
/// [`attach`](CastSession::attach) has been called, the thread writing to
/// ffmpeg.
///
/// [`stop`](CastSession::stop), or dropping the session, shuts things down in
/// pipeline order: the writer first, so ffmpeg sees its stdin close and
/// finishes the file, then PipeWire, then the portal session.
pub struct CastSession {
    layout: CastFormat,
    newest: Arc<LatestFrame>,
    liveness: Arc<Liveness>,
    /// Tells the writer to finish, and tells [`failure`](Self::failure) that
    /// what follows is our own shutdown.
    finishing: Arc<AtomicBool>,
    pipewire_quit: Option<pw::channel::Sender<Quit>>,
    /// Dropping this lets the portal thread close the portal session.
    portal_release: Option<Sender<()>>,
    pipewire: Option<JoinHandle<()>>,
    writer: Option<JoinHandle<()>>,
    portal: Option<JoinHandle<()>>,
}

impl CastSession {
    /// Set up a screen cast and start receiving frames.
    ///
    /// Returns once a frame has been copied out of a real buffer, so
    /// [`format`](Self::format) describes pixels that were actually seen.
    /// Frames that come in before [`attach`](Self::attach) just replace one
    /// another; only the newest matters.
    pub fn start() -> Result<Self> {
        let (node, remote, portal_release, portal) = start_portal()?;

        let newest = Arc::new(LatestFrame::empty());
        let liveness = Arc::new(Liveness::default());
        let (first_tx, first_rx) = mpsc::channel::<FirstFrame>();
        let (pipewire_quit, quit_rx) = pw::channel::channel::<Quit>();

        let pipewire = {
            let (newest, liveness) = (Arc::clone(&newest), Arc::clone(&liveness));
            let body = move || pipewire_main(remote, node, newest, liveness, first_tx, quit_rx);
            spawn_worker("framefang-pipewire", "the screen capture thread did not start", body)?
        };

        // From here the session exists, so any early return goes through
        // `Drop` and shuts down the threads already running.
        let mut cast = CastSession {
            layout: CastFormat::UNKNOWN,
            newest,
            liveness,
            finishing: Arc::default(),
            pipewire_quit: Some(pipewire_quit),
            portal_release: Some(portal_release),
            pipewire: Some(pipewire),
            writer: None,
            portal: Some(portal),
        };

        cast.layout = match first_rx.recv_timeout(FIRST_FRAME_WAIT) {
            Ok(first) => first.map_err(Error::Recording)?,
            Err(_) => return Err(no_usable_frame()),
        };
        // The format is only sent along with a published frame, so this
        // holds already. It is checked anyway because every buffer from here
        // on is sized from that format.
        match cast.newest.first_arrives_within(FIRST_FRAME_WAIT) {
            true => Ok(cast),
            false => Err(Error::Recording("screen sharing started but sent no picture".into())),
        }
    }

    /// `Some(reason)` once the cast has ended by itself: sharing was revoked
    /// or the compositor closed the session. Cheap enough to check on every
    /// event-loop pass, which `Recorder::poll_health` does.
    pub fn failure(&self) -> Option<String> {
        // Our own shutdown disconnects the stream, which looks just like the
        // other side ending it. After we have asked to stop, nothing the
        // stream reports is news.
        match self.finishing.load(Relaxed) {
            true => None,
            false => self.liveness.reason(),
        }
    }

    /// The negotiated layout, for ffmpeg's `-f rawvideo`, `-pix_fmt` and
    /// `-video_size` arguments.
    pub fn format(&self) -> CastFormat {
        self.layout
    }

    /// Start writing frames to `stdin` at [`TARGET_FPS`].
    ///
    /// The writer keeps ffmpeg's stdin until the recording ends; dropping it
    /// then is what tells ffmpeg to finish the file.
    pub fn attach(&mut self, stdin: ChildStdin) -> Result<()> {
        if self.writer.is_some() {
            return Err(Error::Recording("this screen recording already has a writer".into()));
        }
        let (newest, finishing, layout) = (Arc::clone(&self.newest), Arc::clone(&self.finishing), self.layout);
        let body = move || writer_thread(stdin, &newest, &finishing, layout);
        self.writer = Some(spawn_worker("framefang-cast-writer", "the frame writer did not start", body)?);
        Ok(())
    }

    /// Tell the writer to finish, without waiting for it.
    ///
    /// The recorder signals every part first, then waits for ffmpeg, then
    /// joins, the same as the Windows audio pumps. The writer drops stdin as
    /// it exits, which is what starts ffmpeg finishing the file.
    pub fn signal_stop(&self) {
        self.finishing.store(true, Relaxed);
    }

    /// Stop everything and wait for it. Calling it again does nothing more.
    pub fn stop(&mut self) {
        self.signal_stop();
        join(self.writer.take());
        if let Some(quit) = self.pipewire_quit.take() {
            let _ = quit.send(Quit);
        }
        join(self.pipewire.take());
        // Releasing the portal session comes last: closing it removes the
        // PipeWire node, which must not happen under a stream still shutting
        // down.
        drop(self.portal_release.take());
        join(self.portal.take());
    }
}

/// The error for a cast that negotiated but never delivered a readable frame.
fn no_usable_frame() -> Error {
    let seconds = FIRST_FRAME_WAIT.as_secs();
    Error::Recording(format!(
        "screen sharing started, but no picture Framefang can use arrived within \
         {seconds} seconds. Most often the desktop is sending GPU-only buffers that \
         cannot be read yet. Recording in an X11 (Xorg) session works now; please \
         report which desktop and version you use so this can be fixed."
    ))
}

/// Wait for a helper thread, if there is one.
fn join(worker: Option<JoinHandle<()>>) {
    if let Some(worker) = worker {
        let _ = worker.join();
    }
}

impl Drop for CastSession {
    fn drop(&mut self) {
        self.stop();
    }
}

// ---- the portal ----

/// Start the portal thread and wait for it to produce a stream.
///
/// Gives back the PipeWire node id, the PipeWire remote, a sender whose drop
/// lets the portal session close, and the thread.
fn start_portal() -> Result<(u32, OwnedFd, Sender<()>, JoinHandle<()>)> {
    let (ready_tx, ready_rx) = mpsc::channel::<PortalReady>();
    let (release_tx, release_rx) = mpsc::channel::<()>();

    let body = move || pollster::block_on(run_portal(ready_tx, release_rx));
    let portal = spawn_worker("framefang-screencast", "the screen sharing thread did not start", body)?;

    let seconds = SHARE_PROMPT_WAIT.as_secs();
    let ready = ready_rx.recv_timeout(SHARE_PROMPT_WAIT).unwrap_or_else(|_| Err(format!(
        "screen sharing was not set up within {seconds} seconds, so nothing is being \
         recorded. The desktop's sharing prompt may still be open, or the portal \
         service may not be running."
    )));
    let (node, remote) = ready.map_err(Error::Recording)?;
    Ok((node, remote, release_tx, portal))
}

/// Reach the ScreenCast portal and open a session on it.
async fn connect_portal() -> std::result::Result<(Screencast<'static>, Session<'static, Screencast<'static>>), String> {
    let proxy = Screencast::new().await.map_err(|e| portal_unavailable(&e))?;
    let session = proxy.create_session().await.map_err(|e| portal_unavailable(&e))?;
    Ok((proxy, session))
}

/// The portal thread: open a session, get a stream, report it, then hold the
/// session open until released. The session is closed here and nowhere else,
/// and on a failure it is closed before the failure is reported.
async fn run_portal(ready: Sender<PortalReady>, release: Receiver<()>) {
    let (proxy, session) = match connect_portal().await {
        Ok(pair) => pair,
        Err(why) => {
            let _ = ready.send(Err(why));
            return;
        }
    };

    let problem = match open_stream(&proxy, &session).await {
        // Wait for the release (or for the session's owner to vanish, which
        // drops the sender) before closing.
        Ok(stream) => {
            if ready.send(Ok(stream)).is_ok() {
                let _ = release.recv();
            }
            None
        }
        Err(why) => Some(why),
    };
    let _closed = session.close().await;
    if let Some(why) = problem {
        let _ = ready.send(Err(why));
    }
}

/// Ask for a monitor, start the cast, and open its PipeWire remote.
async fn open_stream(
    proxy: &Screencast<'_>,
    session: &Session<'_, Screencast<'_>>,
) -> std::result::Result<(u32, OwnedFd), String> {
    let remembered = RESTORE_TOKEN.lock().ok().and_then(|t| t.clone());
    proxy
        .select_sources(
            session,
            // The pointer belongs in a screen recording, and embedding it is
            // the one cursor mode every portal backend supports.
            CursorMode::Embedded,
            SourceType::Monitor.into(),
            false,
            remembered.as_deref(),
            PersistMode::Application,
        )
        .await
        .map_err(|e| format!("the desktop would not set up screen sharing ({e})."))?;

    let started = proxy.start(session, None).await.map_err(|e| sharing_declined(&e))?;
    let streams = started.response().map_err(|e| sharing_declined(&e))?;

    if let Some(granted) = streams.restore_token() {
        if let Ok(mut token) = RESTORE_TOKEN.lock() {
            *token = Some(granted.to_string());
        }
    }

    let Some(node) = streams.streams().first().map(|s| s.pipe_wire_node_id()) else {
        return Err("screen sharing started without a screen. Choose a monitor when \
                    the sharing prompt appears."
            .to_string());
    };
    let remote = proxy
        .open_pipe_wire_remote(session)
        .await
        .map_err(|e| format!("the desktop would not open the shared screen's stream ({e})."))?;
    Ok((node, remote))
}

fn portal_unavailable(cause: &ashpd::Error) -> String {
    format!(
        "on Wayland, screen recording has to go through xdg-desktop-portal, which \
         did not respond ({cause}). Install and start it together with your \
         desktop's portal backend (such as xdg-desktop-portal-gnome, -kde or -wlr) \
         and try again."
    )
}

fn sharing_declined(cause: &ashpd::Error) -> String {
    format!("screen sharing was not allowed, so recording did not start ({cause}).")
}

// ---- PipeWire ----

/// State shared by the stream callbacks.
struct Callbacks {
    /// The layout the compositor settled on, once it has.
    agreed: Option<CastFormat>,
    /// Where the first frame's format is reported. Taken after that, so it is
    /// reported exactly once.
    first_frame: Option<Sender<FirstFrame>>,
    /// Reused for every frame so copying does not allocate a full frame
    /// thirty times a second.
    copy_buffer: Vec<u8>,
    /// Buffers skipped because their memory was not readable from the CPU.
    skipped_unmappable: u64,
}

/// The PipeWire thread. Anything that goes wrong before streaming starts is
/// sent to `first_frame` for `CastSession::start` to report.
fn pipewire_main(
    remote: OwnedFd,
    node: u32,
    newest: Arc<LatestFrame>,
    liveness: Arc<Liveness>,
    first_frame: Sender<FirstFrame>,
    quit: pw::channel::Receiver<Quit>,
) {
    if let Err(why) = stream_frames(remote, node, &newest, &liveness, &first_frame, quit) {
        let _ = first_frame.send(Err(why));
    }
}

fn stream_frames(
    remote: OwnedFd,
    node: u32,
    newest: &Arc<LatestFrame>,
    liveness: &Arc<Liveness>,
    first_frame: &Sender<FirstFrame>,
    quit: pw::channel::Receiver<Quit>,
) -> std::result::Result<(), String> {
    use pw::keys;

    // `pw_init` is global to the process and must only run once; xcap calls
    // it as well.
    static PIPEWIRE_INIT: std::sync::Once = std::sync::Once::new();
    PIPEWIRE_INIT.call_once(pw::init);

    let main_loop = pw::main_loop::MainLoopRc::new(None)
        .map_err(|e| format!("PipeWire could not be started: {e}"))?;
    let context = pw::context::ContextRc::new(&main_loop, None)
        .map_err(|e| format!("a PipeWire context could not be created: {e}"))?;
    let core = context
        .connect_fd_rc(remote, None)
        .map_err(|e| format!("the screen stream the desktop opened could not be reached: {e}"))?;

    let stream_props = pw::properties::properties! {
        *keys::MEDIA_ROLE => "Screen",
        *keys::MEDIA_TYPE => "Video",
        *keys::MEDIA_CATEGORY => "Capture",
    };
    let stream = pw::stream::StreamRc::new(core, "framefang-screen", stream_props)
        .map_err(|e| format!("the capture stream could not be created: {e}"))?;

    let callbacks = Callbacks {
        agreed: None,
        first_frame: Some(first_frame.clone()),
        copy_buffer: Vec::new(),
        skipped_unmappable: 0,
    };
    let liveness = Arc::clone(liveness);
    let newest = Arc::clone(newest);
    let registration = stream
        .add_local_listener_with_user_data(callbacks)
        .state_changed(move |_, _, _, now| match now {
            pw::stream::StreamState::Error(ref why) => {
                liveness.mark_lost(format!("screen sharing failed: {why}"));
            }
            // The other side ended the session: "Stop sharing" was pressed,
            // or the compositor closed it. Our own stop never gets here,
            // because the loop quits and this listener is dropped first.
            pw::stream::StreamState::Unconnected => liveness.mark_lost(
                "screen sharing was ended from the desktop, so the recording has no \
                 picture any more.".to_string()),
            _ => {}
        })
        .param_changed(|_, shared, param_id, pod| {
            if param_id != pw::spa::param::ParamType::Format.as_raw() {
                return;
            }
            let Some(info) = pod.and_then(raw_video_info) else { return };
            let extent = info.size();
            // Only offered layouts can come back, so `None` here means the
            // compositor answered outside the offer. With no format the
            // `process` callback copies nothing, and the first-frame timeout
            // reports the problem.
            shared.agreed = cast_format(info.format(), extent.width, extent.height);
        })
        .process(move |stream, shared| copy_next_frame(stream, shared, &newest))
        .register()
        .map_err(|e| format!("the capture stream's events could not be watched: {e}"))?;

    let offer = format_offer();
    let pod = pw::spa::pod::Pod::from_bytes(&offer)
        .ok_or_else(|| "the video formats could not be described to PipeWire".to_string())?;
    let mut offers = [pod];
    stream
        .connect(
            pw::spa::utils::Direction::Input,
            Some(node),
            // MAP_BUFFERS is what makes the buffer data readable from here
            // at all. RT_PROCESS is left off on purpose: the callback copies
            // a frame under a mutex, which has no place on a real-time thread.
            pw::stream::StreamFlags::AUTOCONNECT | pw::stream::StreamFlags::MAP_BUFFERS,
            &mut offers,
        )
        .map_err(|e| format!("the capture stream could not be started: {e}"))?;

    let looper = main_loop.clone();
    let _quit_watch = quit.attach(main_loop.loop_(), move |Quit| looper.quit());
    main_loop.run();

    // Stop the flow of buffers before dropping the callbacks that hold the
    // frame slot.
    let _disconnected = stream.disconnect();
    drop(registration);
    Ok(())
}

/// The raw-video description in a `Format` param, or `None` if it is not
/// raw video or does not parse.
fn raw_video_info(param: &pw::spa::pod::Pod) -> Option<pw::spa::param::video::VideoInfoRaw> {
    use pw::spa::param::format::{MediaSubtype, MediaType};

    let (media_type, media_subtype) = pw::spa::param::format_utils::parse_format(param).ok()?;
    if media_type != MediaType::Video || media_subtype != MediaSubtype::Raw {
        return None;
    }
    let mut info = pw::spa::param::video::VideoInfoRaw::default();
    info.parse(param).ok()?;
    Some(info)
}

/// The `process` callback: copy the buffer's pixels into the frame slot.
fn copy_next_frame(stream: &pw::stream::Stream, shared: &mut Callbacks, newest: &LatestFrame) {
    let Some(format) = shared.agreed else { return };
    let Some(mut buffer) = stream.dequeue_buffer() else { return };
    let Some(data) = buffer.datas_mut().first_mut() else { return };

    let chunk = data.chunk();
    let (size, offset, stride) = (chunk.size() as usize, chunk.offset() as usize, chunk.stride());
    // An empty chunk means nothing changed; the writer repeats the last
    // frame by itself.
    if size == 0 {
        return;
    }
    // Zero or negative means rows are packed with no padding.
    let stride = if stride > 0 { stride as usize } else { format.row_len() };

    let Some(pixels) = data.data() else {
        // A GPU buffer (DMA-BUF) or other memory we cannot map: there are no
        // CPU-visible pixels to copy.
        shared.skipped_unmappable = shared.skipped_unmappable.saturating_add(1);
        return;
    };
    let rows = format.height as usize;
    if !copy_rows(pixels, offset, stride, format.row_len(), rows, &mut shared.copy_buffer) {
        return;
    }

    newest.publish(&mut shared.copy_buffer);
    // Reported once, and only after a frame that really copied.
    if let Some(first_frame) = shared.first_frame.take() {
        let _ = first_frame.send(Ok(format));
    }
}

/// The `EnumFormat` param offered to the compositor, serialized: the layouts
/// in `PACKED_FORMATS`, any size, and a frame rate up to a sensible ceiling.
///
/// No DMA-BUF modifier is offered. Without one the compositor falls back to
/// ordinary shared memory, which is what keeps the frame copy a memcpy rather
/// than a GPU import.
fn format_offer() -> Vec<u8> {
    use pw::spa::pod::{serialize::PodSerializer, Value};

    let (bytes, _) = PodSerializer::serialize(std::io::Cursor::new(Vec::new()), &Value::Object(format_offer_object()))
        .expect("a fixed, well-formed pod always serializes");
    bytes.into_inner()
}

fn format_offer_object() -> pw::spa::pod::Object {
    use pw::spa::param::format::{FormatProperties as Key, MediaSubtype, MediaType};
    use pw::spa::pod::{ChoiceValue, Object, Property, PropertyFlags, Value};
    use pw::spa::utils::{Choice, ChoiceEnum, ChoiceFlags, Id, SpaTypes};

    let property = |key: Key, value: Value| Property { key: key.as_raw(), flags: PropertyFlags::empty(), value };
    // The preferred layout is the default, and the alternatives list every
    // accepted layout, the default included.
    let layouts = ChoiceEnum::Enum {
        default: Id(PACKED_FORMATS[0].0.as_raw()),
        alternatives: PACKED_FORMATS.iter().map(|(f, _)| Id(f.as_raw())).collect(),
    };
    let sizes = ChoiceEnum::Range { default: SIZE_PREFERRED, min: SIZE_SMALLEST, max: SIZE_LARGEST };
    let rates = ChoiceEnum::Range { default: RATE_PREFERRED, min: RATE_LOWEST, max: RATE_HIGHEST };

    Object {
        type_: SpaTypes::ObjectParamFormat.as_raw(),
        id: pw::spa::param::ParamType::EnumFormat.as_raw(),
        properties: vec![
            property(Key::MediaType, Value::Id(Id(MediaType::Video.as_raw()))),
            property(Key::MediaSubtype, Value::Id(Id(MediaSubtype::Raw.as_raw()))),
            property(Key::VideoFormat, Value::Choice(ChoiceValue::Id(Choice(ChoiceFlags::empty(), layouts)))),
            property(Key::VideoSize, Value::Choice(ChoiceValue::Rectangle(Choice(ChoiceFlags::empty(), sizes)))),
            property(Key::VideoFramerate, Value::Choice(ChoiceValue::Fraction(Choice(ChoiceFlags::empty(), rates)))),
        ],
    }
}

// ---- the writer ----

/// Keeps the writer on a steady beat.
struct Pacer {
    interval: Duration,
    next: Instant,
}

impl Pacer {
    fn new(fps: u32, start: Instant) -> Self {
        Pacer { interval: Duration::from_nanos(1_000_000_000 / fps as u64), next: start }
    }

    /// Called after each frame is written, at time `now`. Returns how long to
    /// sleep before the next one, if at all.
    ///
    /// When more than a whole frame behind (an encoder that stalled, a laptop
    /// that slept), the beat restarts from now rather than rushing out the
    /// missed frames, which would play back sped up.
    fn after_frame(&mut self, now: Instant) -> Option<Duration> {
        self.next += self.interval;
        if now < self.next {
            Some(self.next - now)
        } else {
            if now - self.next > self.interval {
                self.next = now;
            }
            None
        }
    }
}

/// Write frames to ffmpeg at [`TARGET_FPS`] until told to stop.
///
/// Returning drops ffmpeg's stdin, which gives it its end of input. A failed
/// write also ends the loop: ffmpeg is gone, and the recorder's own health
/// check tells the user.
fn writer_thread(mut to_ffmpeg: ChildStdin, newest: &LatestFrame, finishing: &AtomicBool, layout: CastFormat) {
    let frame_len = layout.bytes_per_frame();
    // Swapped with the frame slot, so taking a frame moves a pointer instead
    // of copying a whole frame.
    let mut incoming: Vec<u8> = Vec::with_capacity(frame_len);
    // The frame being sent, kept apart from `incoming` so that a frame of the
    // wrong size (only a resolution change mid-recording makes one) is
    // skipped rather than blanking the picture. ffmpeg was told one size; a
    // frame of another size would shear everything after it.
    let mut current: Vec<u8> = Vec::with_capacity(frame_len);
    let mut seen = 0u64;
    let mut pacer = Pacer::new(TARGET_FPS, Instant::now());

    loop {
        if finishing.load(Relaxed) {
            break;
        }
        if newest.take_newer(&mut incoming, &mut seen) && incoming.len() == frame_len {
            std::mem::swap(&mut current, &mut incoming);
        }
        // Until the first frame there is nothing to send, and ffmpeg is
        // content to wait on a quiet pipe.
        if current.len() == frame_len && to_ffmpeg.write_all(&current).is_err() {
            return;
        }
        if let Some(pause) = pacer.after_frame(Instant::now()) {
            thread::sleep(pause);
        }
    }
    let _flushed = to_ffmpeg.flush();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_offered_layout_has_an_ffmpeg_name() {
        let expected = [
            (VideoFormat::BGRx, "bgr0"), (VideoFormat::RGBx, "rgb0"),
            (VideoFormat::BGRA, "bgra"), (VideoFormat::RGBA, "rgba"),
            (VideoFormat::xRGB, "0rgb"), (VideoFormat::xBGR, "0bgr"),
            (VideoFormat::ARGB, "argb"), (VideoFormat::ABGR, "abgr"),
        ];
        for (format, name) in expected {
            assert_eq!(ffmpeg_pix_fmt(format), Some(name));
        }
        for other in [VideoFormat::I420, VideoFormat::RGB, VideoFormat::NV12, VideoFormat::Unknown] {
            assert_eq!(ffmpeg_pix_fmt(other), None);
        }
    }

    #[test]
    fn a_cast_format_needs_a_known_layout_and_a_real_size() {
        assert_eq!(cast_format(VideoFormat::BGRx, 1920, 1080),
            Some(CastFormat { width: 1920, height: 1080, pix_fmt: "bgr0" }));
        assert_eq!(cast_format(VideoFormat::BGRx, 0, 1080), None);
        assert_eq!(cast_format(VideoFormat::BGRx, 1920, 0), None);
        assert_eq!(cast_format(VideoFormat::I420, 1920, 1080), None);
    }

    #[test]
    fn frame_sizes_are_four_bytes_a_pixel() {
        let format = CastFormat { width: 3, height: 2, pix_fmt: "rgba" };
        assert_eq!(format.row_len(), 12);
        assert_eq!(format.bytes_per_frame(), 24);
    }

    #[test]
    fn rows_are_copied_without_their_padding() {
        // Two rows of 4 bytes, each padded to a stride of 6, after a 2-byte
        // offset.
        let src = [9, 9, 1, 2, 3, 4, 0, 0, 5, 6, 7, 8, 0, 0];
        let mut out = vec![42];
        assert!(copy_rows(&src, 2, 6, 4, 2, &mut out));
        assert_eq!(out, [1, 2, 3, 4, 5, 6, 7, 8]);
    }

    #[test]
    fn a_short_buffer_is_not_a_frame() {
        let src = [1, 2, 3, 4, 5, 6, 7];
        let mut out = Vec::new();
        assert!(!copy_rows(&src, 0, 4, 4, 2, &mut out));
    }

    #[test]
    fn the_slot_hands_over_only_newer_frames() {
        let slot = LatestFrame::empty();
        let mut taken = Vec::new();
        let mut seen = 0;
        assert!(!slot.take_newer(&mut taken, &mut seen), "nothing published yet");

        let mut frame = vec![1, 2, 3];
        slot.publish(&mut frame);
        assert!(slot.take_newer(&mut taken, &mut seen));
        assert_eq!((taken.as_slice(), seen), (&[1, 2, 3][..], 1));
        assert!(!slot.take_newer(&mut taken, &mut seen), "no news since");

        let mut first = vec![4];
        let mut second = vec![5];
        slot.publish(&mut first);
        slot.publish(&mut second);
        assert!(slot.take_newer(&mut taken, &mut seen));
        assert_eq!((taken.as_slice(), seen), (&[5][..], 3), "only the newest frame is kept");
    }

    #[test]
    fn waiting_for_the_first_frame() {
        let slot = Arc::new(LatestFrame::empty());
        assert!(!slot.first_arrives_within(Duration::from_millis(10)));

        let publisher = {
            let slot = Arc::clone(&slot);
            thread::spawn(move || {
                thread::sleep(Duration::from_millis(20));
                slot.publish(&mut vec![0; 4]);
            })
        };
        assert!(slot.first_arrives_within(Duration::from_secs(5)));
        publisher.join().unwrap();
        assert!(slot.first_arrives_within(Duration::ZERO), "an earlier frame still counts");
    }

    #[test]
    fn liveness_reports_the_latest_loss() {
        let liveness = Liveness::default();
        assert_eq!(liveness.reason(), None);
        liveness.mark_lost("first".into());
        liveness.mark_lost("second".into());
        assert_eq!(liveness.reason().as_deref(), Some("second"));
    }

    #[test]
    fn the_failed_helper_joins_context_and_cause() {
        let Error::Recording(text) = failed::<&str>("it broke")("this way") else { panic!() };
        assert_eq!(text, "it broke: this way");
    }

    #[test]
    fn the_pacer_sleeps_until_the_next_beat() {
        let start = Instant::now();
        let mut pacer = Pacer::new(TARGET_FPS, start);
        let beat = pacer.interval;
        assert_eq!(beat, Duration::from_nanos(33_333_333));

        assert_eq!(pacer.after_frame(start + Duration::from_millis(5)), Some(beat - Duration::from_millis(5)));
        // Slightly late: no sleep, and the beat is kept.
        assert_eq!(pacer.after_frame(start + beat * 2 + Duration::from_millis(1)), None);
        assert_eq!(pacer.next, start + beat * 2);
    }

    #[test]
    fn the_pacer_restarts_after_falling_far_behind() {
        let start = Instant::now();
        let mut pacer = Pacer::new(TARGET_FPS, start);
        let late = start + Duration::from_secs(2);
        assert_eq!(pacer.after_frame(late), None);
        assert_eq!(pacer.next, late, "missed beats are dropped, not caught up");
        assert_eq!(pacer.after_frame(late), Some(pacer.interval));
    }

    #[test]
    fn the_format_offer_lists_every_layout_preferred_first() {
        use pw::spa::param::format::FormatProperties;
        use pw::spa::pod::{ChoiceValue, Value};
        use pw::spa::utils::{Choice, ChoiceEnum, Id};

        let object = format_offer_object();
        let layouts = object.properties.iter()
            .find(|p| p.key == FormatProperties::VideoFormat.as_raw())
            .expect("the offer names its layouts");
        let Value::Choice(ChoiceValue::Id(Choice(_, ChoiceEnum::Enum { default, alternatives }))) = &layouts.value
            else { panic!("layouts should be an enum choice: {:?}", layouts.value) };
        assert_eq!(*default, Id(VideoFormat::BGRx.as_raw()));
        let offered: Vec<Id> = PACKED_FORMATS.iter().map(|(f, _)| Id(f.as_raw())).collect();
        assert_eq!(alternatives, &offered);
        assert_eq!(object.properties.len(), 5);
    }

    #[test]
    fn the_format_offer_ranges_size_and_rate() {
        use pw::spa::param::format::FormatProperties;
        use pw::spa::pod::{ChoiceValue, Value};
        use pw::spa::utils::{Choice, ChoiceEnum};

        let object = format_offer_object();
        let value_of = |key: FormatProperties| {
            &object.properties.iter().find(|p| p.key == key.as_raw()).expect("property present").value
        };
        let Value::Choice(ChoiceValue::Rectangle(Choice(_, ChoiceEnum::Range { default, min, max }))) =
            value_of(FormatProperties::VideoSize) else { panic!("size should be a range") };
        assert_eq!((default.width, default.height), (1920, 1080));
        assert_eq!((min.width, min.height, max.width, max.height), (1, 1, 16384, 16384));

        let Value::Choice(ChoiceValue::Fraction(Choice(_, ChoiceEnum::Range { default, min, max }))) =
            value_of(FormatProperties::VideoFramerate) else { panic!("rate should be a range") };
        assert_eq!((default.num, default.denom), (TARGET_FPS, 1));
        assert_eq!((min.num, min.denom, max.num, max.denom), (0, 1, 240, 1));
    }

    #[test]
    fn the_format_offer_serializes_to_a_pod() {
        let bytes = format_offer();
        assert!(pw::spa::pod::Pod::from_bytes(&bytes).is_some());
    }
}
