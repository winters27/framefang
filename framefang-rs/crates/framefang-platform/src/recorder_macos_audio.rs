//! Recording the system audio mix on macOS, through ScreenCaptureKit.
//!
//! ffmpeg's avfoundation input can add a microphone to a recording but has
//! no way to hear what the Mac is playing without a virtual audio driver such
//! as BlackHole. ScreenCaptureKit (macOS 13 and later) can, under the Screen
//! Recording permission Frame Fang already needs, so system audio comes from
//! here while video and the mic stay on avfoundation.
//!
//! An `SCStream` with audio turned on hands Float32 sample buffers to our
//! output object on a background queue. It writes the PCM to a socket on
//! 127.0.0.1 that ffmpeg reads as one more raw input, the same arrangement as
//! the Windows WASAPI sources, and ffmpeg mixes everything.
//!
//! The samples arrive planar (one buffer per channel) and are interleaved into
//! plain `f32le` on the way out. Unlike WASAPI loopback, ScreenCaptureKit keeps
//! sending buffers (silent ones included) when nothing is playing, so there is
//! no gap to fill.

use std::{
    fmt::Display,
    io::{ErrorKind, Write as _},
    mem::size_of,
    net::{TcpListener, TcpStream},
    ptr::{self, NonNull},
    sync::{atomic::{AtomicBool, Ordering::Relaxed}, mpsc, Arc, Mutex},
    thread::{self, JoinHandle},
    time::Duration,
};

use block2::RcBlock;
use objc2::{
    define_class, msg_send,
    rc::Retained,
    runtime::{NSObject, NSObjectProtocol, ProtocolObject},
    AllocAnyThread, DefinedClass,
};
use objc2_core_audio_types::{AudioBuffer as Plane, AudioBufferList as PlaneList};
use objc2_core_foundation::CFRetained;
use objc2_core_media::{CMBlockBuffer, CMSampleBuffer};
use objc2_foundation::{NSArray, NSError};
use objc2_screen_capture_kit::{
    SCContentFilter, SCDisplay, SCShareableContent, SCStream, SCStreamConfiguration,
    SCStreamOutput, SCStreamOutputType, SCWindow,
};

use crate::{Error, Result};

/// The stream is configured for exactly this format, so ffmpeg's input can
/// be described up front with no handshake: `build_macos_ffmpeg_args` in
/// recorder.rs reads it as `f32le` at these numbers.
const RATE_HZ: u32 = 48_000;
const STEREO: u16 = 2;

/// How long to wait for the display list.
const DISPLAY_WAIT: Duration = Duration::from_secs(5);
/// How long to wait for capture to start.
const START_WAIT: Duration = Duration::from_secs(10);
/// How long `stop` waits for capture to confirm it has stopped.
const STOP_WAIT: Duration = Duration::from_secs(5);
/// ffmpeg gets this many polls, `ACCEPT_POLL` apart, to connect.
const ACCEPT_TRIES: u32 = 1000;
const ACCEPT_POLL: Duration = Duration::from_millis(10);

/// Room for a buffer list of up to this many channel planes.
const MAX_PLANES: usize = 8;
/// `kCMSampleBufferFlag_AudioBufferList_Assure16ByteAlignment`.
const ASSURE_16_BYTE_ALIGNMENT: u32 = 1;

/// The connection to ffmpeg, once it has connected. The output object writes
/// to it from the capture queue; `stop` closes it.
type SharedSocket = Arc<Mutex<Option<TcpStream>>>;

/// A recording error that reads "`what`: `cause`".
fn failed<E: Display>(what: &'static str) -> impl FnOnce(E) -> Error {
    move |cause| Error::Recording(format!("{what}: {cause}"))
}

/// A running system-audio capture. The recorder keeps it beside the ffmpeg
/// child and calls [`stop`](SckSession::stop) when the recording ends.
pub(crate) struct SckSession {
    stream: Retained<SCStream>,
    _output: Retained<AudioOutput>,
    halt: Arc<AtomicBool>,
    socket: SharedSocket,
    accept_thread: Option<JoinHandle<()>>,
    pub port: u16,
}

impl SckSession {
    pub fn stop(mut self) {
        self.halt.store(true, Relaxed);

        // Wait for capture to confirm it stopped, so no audio callback can
        // run after the socket below is gone.
        let (done, stopped) = mpsc::channel::<()>();
        let on_stopped = RcBlock::new(move |_error: *mut NSError| {
            let _ = done.send(());
        });
        unsafe { self.stream.stopCaptureWithCompletionHandler(Some(&on_stopped)); }
        let _ = stopped.recv_timeout(STOP_WAIT);

        // Closing the socket ends this input for ffmpeg.
        if let Ok(mut connection) = self.socket.lock() {
            *connection = None;
        }
        if let Some(waiter) = self.accept_thread.take() {
            let _ = waiter.join();
        }
    }
}

/// Start capturing the system audio mix.
///
/// The session carries the port ffmpeg should read from; the format is the
/// fixed `f32le`, 48 kHz stereo set on the stream.
pub(crate) fn start_system_audio() -> Result<SckSession> {
    // Bound first, so the port exists before ffmpeg is started.
    let server = TcpListener::bind(("127.0.0.1", 0))
        .map_err(failed("no local socket could be opened for the system audio"))?;
    let port = server.local_addr().map_err(failed("the local audio socket has no port"))?.port();

    // A content filter needs a display even when only audio is wanted.
    let screen = first_display()?;
    let settings = audio_only_config();
    let no_windows: Retained<NSArray<SCWindow>> = NSArray::new();
    let content = unsafe {
        SCContentFilter::initWithDisplay_excludingWindows(SCContentFilter::alloc(), &screen, &no_windows)
    };

    let halt = Arc::<AtomicBool>::default();
    let socket = SharedSocket::default();
    let output = AudioOutput::new(socket.clone(), halt.clone());

    let blank = SCStream::alloc();
    let capture = unsafe { SCStream::initWithFilter_configuration_delegate(blank, &content, &settings, None) };
    let as_output: &ProtocolObject<dyn SCStreamOutput> = ProtocolObject::from_ref(&*output);
    // No queue given: ScreenCaptureKit calls us on a serial queue of its own.
    let attached = unsafe {
        capture.addStreamOutput_type_sampleHandlerQueue_error(as_output, SCStreamOutputType::Audio, None)
    };
    attached.map_err(failed("the system audio could not be connected to the capture"))?;

    start_capture(&capture)?;
    let accept_thread = spawn_acceptor(server, socket.clone(), halt.clone())?;

    Ok(SckSession {
        stream: capture,
        _output: output,
        halt,
        socket,
        accept_thread: Some(accept_thread),
        port,
    })
}

/// A configuration that captures audio in our fixed format. The video side
/// is shrunk to 2x2 because its frames are never read, and Frame Fang's own
/// sounds are left out of the mix.
fn audio_only_config() -> Retained<SCStreamConfiguration> {
    unsafe {
        let config = SCStreamConfiguration::new();
        config.setCapturesAudio(true);
        config.setSampleRate(RATE_HZ as isize);
        config.setChannelCount(STEREO as isize);
        config.setExcludesCurrentProcessAudio(true);
        config.setWidth(2);
        config.setHeight(2);
        config
    }
}

/// Start the stream and block until its completion handler reports.
fn start_capture(stream: &SCStream) -> Result<()> {
    let (report, outcome) = mpsc::channel::<Option<String>>();
    let on_started = RcBlock::new(move |error: *mut NSError| {
        let failure = unsafe { error.as_ref() }.map(|e| e.localizedDescription().to_string());
        let _ = report.send(failure);
    });
    unsafe { stream.startCaptureWithCompletionHandler(Some(&on_started)); }

    match outcome.recv_timeout(START_WAIT) {
        Ok(None) => Ok(()),
        Ok(Some(why)) => Err(Error::Recording(format!(
            "macOS would not start recording the system audio: {why}. Allow Framefang \
             under System Settings, Privacy & Security, Screen Recording, and then \
             try again."
        ))),
        Err(_) => Err(Error::Recording(
            "macOS did not start recording the system audio within 10 seconds.".into())),
    }
}

/// Wait on a small thread for ffmpeg to connect, then hand the connection to
/// the output object. Gives up quietly on stop, on a socket error, or after
/// `ACCEPT_TRIES` polls.
fn spawn_acceptor(server: TcpListener, socket: SharedSocket, halt: Arc<AtomicBool>)
    -> Result<JoinHandle<()>>
{
    let wait_for_ffmpeg = move || {
        let _ = server.set_nonblocking(true);
        for _ in 0..ACCEPT_TRIES {
            if halt.load(Relaxed) {
                break;
            }
            match server.accept() {
                Ok((connection, _)) => {
                    let _ = connection.set_nodelay(true);
                    if let Ok(mut slot) = socket.lock() {
                        *slot = Some(connection);
                    }
                    break;
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock => thread::sleep(ACCEPT_POLL),
                Err(_) => break,
            }
        }
    };
    thread::Builder::new()
        .name(String::from("framefang-sck-accept"))
        .spawn(wait_for_ffmpeg)
        .map_err(failed("the system audio connection thread did not start"))
}

/// The first display ScreenCaptureKit offers, waiting on its asynchronous
/// lookup.
///
/// The display crosses from the completion handler's thread as a raw pointer
/// held in a `usize`. Its retain count is atomic, so this is sound, and it
/// avoids asserting `Send` on a `Retained`.
fn first_display() -> Result<Retained<SCDisplay>> {
    let (report, found) = mpsc::channel::<std::result::Result<usize, String>>();
    let on_content = RcBlock::new(move |content: *mut SCShareableContent, error: *mut NSError| {
        let answer = if let Some(e) = unsafe { error.as_ref() } {
            Err(e.localizedDescription().to_string())
        } else if let Some(content) = unsafe { content.as_ref() } {
            match unsafe { content.displays() }.firstObject() {
                Some(display) => Ok(Retained::into_raw(display) as usize),
                None => Err("there is no display to capture".to_string()),
            }
        } else {
            Err("macOS returned nothing to capture".to_string())
        };
        let _ = report.send(answer);
    });
    unsafe { SCShareableContent::getShareableContentWithCompletionHandler(&on_content); }

    match found.recv_timeout(DISPLAY_WAIT) {
        Ok(Ok(address)) => unsafe { Retained::from_raw(address as *mut SCDisplay) }
            .ok_or_else(|| Error::Recording("macOS handed back an empty display".into())),
        Ok(Err(why)) => Err(Error::Recording(format!(
            "macOS could not list the displays needed for system audio: {why}"))),
        Err(_) => Err(Error::Recording("macOS took too long to list the displays.".into())),
    }
}

/// What the output object shares with the session.
struct OutputState {
    socket: SharedSocket,
    halt: Arc<AtomicBool>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "FrameFangSystemAudioOutput"]
    #[ivars = OutputState]
    struct AudioOutput;

    unsafe impl NSObjectProtocol for AudioOutput {}

    unsafe impl SCStreamOutput for AudioOutput {
        #[unsafe(method(stream:didOutputSampleBuffer:ofType:))]
        fn on_sample_buffer(&self, _from: &SCStream, buffer: &CMSampleBuffer, kind: SCStreamOutputType) {
            let wanted = kind == SCStreamOutputType::Audio && !self.ivars().halt.load(Relaxed);
            if wanted {
                self.send_audio(buffer);
            }
        }
    }
);

impl AudioOutput {
    fn new(socket: SharedSocket, halt: Arc<AtomicBool>) -> Retained<Self> {
        let allocated = Self::alloc().set_ivars(OutputState { socket, halt });
        unsafe { msg_send![super(allocated), init] }
    }

    /// Take the PCM out of one sample buffer, interleave it, and write it to
    /// ffmpeg. Before ffmpeg connects the buffer is simply dropped, which
    /// loses at most a few milliseconds at the start.
    fn send_audio(&self, sample_buffer: &CMSampleBuffer) {
        // The list type declares one plane; room for the rest follows it.
        let list_bytes = size_of::<PlaneList>() + (MAX_PLANES - 1) * size_of::<Plane>();
        let mut list_storage = vec![0u8; list_bytes];
        let list = list_storage.as_mut_ptr().cast::<PlaneList>();
        let mut needed = 0usize;
        let mut block: *mut CMBlockBuffer = ptr::null_mut();

        let result = unsafe {
            sample_buffer.audio_buffer_list_with_retained_block_buffer(
                &mut needed,
                list,
                list_bytes,
                None, None,
                ASSURE_16_BYTE_ALIGNMENT,
                &mut block,
            )
        };
        if result != 0 {
            return;
        }
        // We are handed the block buffer already retained; wrapping it means
        // it is released when this function returns.
        let _block = NonNull::new(block).map(|b| unsafe { CFRetained::from_raw(b) });

        let pcm = unsafe { interleave_pcm(list) };
        if pcm.is_empty() {
            return;
        }
        let Ok(mut slot) = self.ivars().socket.lock() else { return };
        let ffmpeg_gone = slot.as_mut().is_some_and(|connection| connection.write_all(&pcm).is_err());
        if ffmpeg_gone {
            // ffmpeg closed its end, so the recording is stopping.
            *slot = None;
        }
    }
}

/// The audio in `list` as interleaved little-endian `f32` bytes.
///
/// One buffer is already interleaved (or mono) and is copied unchanged. More
/// than one means one Float32 plane per channel.
///
/// # Safety
///
/// `list` must point at a filled-in buffer list whose planes, up to
/// `mNumberBuffers`, are all inside the allocation and point at live data.
unsafe fn interleave_pcm(list: *mut PlaneList) -> Vec<u8> {
    let count = unsafe { (*list).mNumberBuffers } as usize;
    // Taken through a raw pointer so the planes past the first, which live
    // beyond the declared one-element array, stay in bounds of `list`.
    let first = unsafe { ptr::addr_of!((*list).mBuffers) }.cast::<Plane>();
    let planes = unsafe { std::slice::from_raw_parts(first, count) };

    match planes {
        [] => Vec::new(),
        [only] => unsafe { plane_bytes(only) }.to_vec(),
        several => {
            let channels: Vec<&[f32]> = several.iter().map(|p| unsafe { plane_samples(p) }).collect();
            interleave_planes(&channels)
        }
    }
}

/// The bytes of one plane, or nothing if it has no data.
unsafe fn plane_bytes(plane: &Plane) -> &[u8] {
    match plane.mData.is_null() {
        true => &[],
        false => unsafe { std::slice::from_raw_parts(plane.mData.cast::<u8>(), plane.mDataByteSize as usize) },
    }
}

/// The Float32 samples of one plane, or nothing if it has no data.
unsafe fn plane_samples(plane: &Plane) -> &[f32] {
    let samples = plane.mDataByteSize as usize / size_of::<f32>();
    match plane.mData.is_null() {
        true => &[],
        false => unsafe { std::slice::from_raw_parts(plane.mData.cast::<f32>(), samples) },
    }
}

/// Weave per-channel planes into one frame-ordered byte stream. If the planes
/// differ in length, only the frames every plane has are kept.
fn interleave_planes(channels: &[&[f32]]) -> Vec<u8> {
    let shared_frames = channels.iter().map(|samples| samples.len()).min().unwrap_or(0);
    let mut woven = Vec::with_capacity(shared_frames * channels.len() * size_of::<f32>());
    for frame in 0..shared_frames {
        for samples in channels {
            woven.extend_from_slice(&samples[frame].to_le_bytes());
        }
    }
    woven
}

#[cfg(test)]
mod tests {
    use super::*;

    fn floats(bytes: &[u8]) -> Vec<f32> {
        bytes.chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect()
    }

    #[test]
    fn stereo_planes_alternate_left_and_right() {
        let left = [1.0f32, 2.0, 3.0];
        let right = [-1.0f32, -2.0, -3.0];
        let out = interleave_planes(&[&left[..], &right[..]]);
        assert_eq!(floats(&out), [1.0, -1.0, 2.0, -2.0, 3.0, -3.0]);
    }

    #[test]
    fn uneven_planes_keep_only_shared_frames() {
        let a = [0.5f32, 0.25, 0.125];
        let b = [9.0f32];
        assert_eq!(floats(&interleave_planes(&[&a[..], &b[..]])), [0.5, 9.0]);
    }

    #[test]
    fn an_empty_plane_means_no_output() {
        let a = [1.0f32, 2.0];
        assert!(interleave_planes(&[&a[..], &[]]).is_empty());
        assert!(interleave_planes(&[]).is_empty());
    }

    #[test]
    fn the_fixed_format_matches_what_ffmpeg_is_told() {
        // build_macos_ffmpeg_args hard-codes these for the socket input.
        assert_eq!((RATE_HZ, STEREO), (48_000, 2));
    }
}
