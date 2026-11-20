//! Recording system audio and the microphone on Windows, through WASAPI.
//!
//! Windows has no dependable "what the speakers are playing" input for
//! ffmpeg's DirectShow reader: Stereo Mix is off by default or missing, and
//! virtual audio cables need a driver install. WASAPI loopback needs nothing:
//! opening the default playback endpoint for capture yields the exact mix
//! going to the speakers, and the default recording endpoint yields the mic.
//!
//! Getting the samples to ffmpeg: its stdin is already taken (the recorder
//! stops ffmpeg by writing `q` there), so every source gets its own TCP
//! listener on 127.0.0.1. ffmpeg connects to it as an ordinary input
//! (`-f f32le -ar R -ac C -i tcp://127.0.0.1:<port>`) and does the resampling
//! and mixing itself. This side only forwards the device's own PCM and tells
//! ffmpeg what format it is in.
//!
//! One quirk shapes the capture loop: loopback produces no packets at all
//! while nothing is playing. Left alone, the audio track would come out
//! shorter than the video and drift. So whenever the device goes quiet, the
//! loop writes silence matching the wall-clock time that passed.

use std::{
    collections::VecDeque,
    fmt::Display,
    io::{ErrorKind, Write},
    net::{TcpListener, TcpStream},
    sync::{atomic::{AtomicBool, Ordering::Relaxed}, mpsc, Arc},
    time::{Duration, Instant},
};

use wasapi::{Direction, SampleType, ShareMode, WaveFormat};

use super::{AudioPump, WasapiAudioSpec};
use crate::{Error, Result};

/// How long `start_source` waits for the capture thread to open the device.
const STARTUP_WAIT: Duration = Duration::from_secs(5);

/// How long the capture thread waits for ffmpeg to connect.
const CONNECT_WAIT: Duration = Duration::from_secs(10);

/// How often the accept loop checks for ffmpeg's connection.
const ACCEPT_POLL: Duration = Duration::from_millis(10);

/// How long one wait for a device buffer lasts before it counts as quiet.
const BUFFER_WAIT_MS: u32 = 100;

/// Silence is written in blocks of this many frames.
const SILENCE_BLOCK_FRAMES: usize = 1024;

/// A recording error that reads "`what`: `cause`".
fn failed<E: Display>(what: impl Display) -> impl FnOnce(E) -> Error {
    move |cause| Error::Recording(format!("{what}: {cause}"))
}

/// Which device to record.
#[derive(Clone, Copy)]
pub(crate) enum SourceKind {
    /// What the speakers are playing (the default playback device, in
    /// loopback).
    SystemLoopback,
    /// The default recording device.
    Microphone,
}

impl SourceKind {
    /// How the source is named in messages.
    fn display_name(self) -> &'static str {
        match self {
            Self::SystemLoopback => "system audio",
            Self::Microphone => "microphone",
        }
    }

    /// Which default endpoint to open. Loopback opens the playback side.
    fn endpoint(self) -> Direction {
        match self {
            Self::SystemLoopback => Direction::Render,
            Self::Microphone => Direction::Capture,
        }
    }

    fn is_microphone(self) -> bool {
        matches!(self, Self::Microphone)
    }

    fn thread_name(self) -> String {
        format!("framefang-wasapi-{}", self.display_name().replace(' ', "-"))
    }
}

/// A source that is running: the format and port ffmpeg needs for its input,
/// and the pump that owns the capture thread.
pub(crate) struct StartedSource {
    pub spec: WasapiAudioSpec,
    pub pump: AudioPump,
}

/// What the capture thread reports once the device is streaming.
struct StreamFormat {
    rate: u32,
    channels: u16,
    pcm: &'static str,
}

type Report = mpsc::Sender<Result<StreamFormat>>;

/// Open `kind` and begin serving its PCM on a new local TCP port.
///
/// Returns once the capture thread has the device streaming, so the format
/// is known when ffmpeg's arguments are built. If the device cannot be opened
/// the error says what to do about it (most often: Windows privacy settings
/// are blocking the microphone).
pub(crate) fn start_source(kind: SourceKind) -> Result<StartedSource> {
    let name = kind.display_name();
    // The listener is bound here, before ffmpeg starts, so the port is known
    // for its arguments and the connection queues up even if ffmpeg is quick.
    let server = TcpListener::bind(("127.0.0.1", 0))
        .map_err(failed(format_args!("no local socket could be opened for the {name}")))?;
    let port = server.local_addr().map_err(failed("the local audio socket has no port"))?.port();
    server.set_nonblocking(true).map_err(failed("the local audio socket could not be set up"))?;

    let cancel = Arc::<AtomicBool>::default();
    let (report, started) = mpsc::channel();
    let worker = {
        let cancel = Arc::clone(&cancel);
        std::thread::Builder::new()
            .name(kind.thread_name())
            .spawn(move || capture_thread(kind, server, &cancel, &report))
            .map_err(failed(format_args!("the {name} capture thread did not start")))?
    };

    let failure = match started.recv_timeout(STARTUP_WAIT) {
        Ok(Ok(StreamFormat { rate, channels, pcm })) => {
            let spec = WasapiAudioSpec { port, sample_rate: rate, channels, ffmpeg_fmt: pcm };
            return Ok(StartedSource { spec, pump: AudioPump { stop: cancel, handle: Some(worker) } });
        }
        Ok(Err(e)) => e,
        Err(_) => Error::Recording(format!(
            "the {name} did not begin recording within 5 seconds. Another app may be \
             using the audio device in exclusive mode."
        )),
    };
    // Whatever went wrong, the thread is told to stop and joined before the
    // error goes back to the caller.
    cancel.store(true, Relaxed);
    let _ = worker.join();
    Err(failure)
}

/// Body of the capture thread. Every WASAPI and COM object is created and
/// dropped on this thread, since COM objects belong to the thread that made
/// them.
///
/// A failure before the format is reported reaches `start_source` through
/// `report`. A failure later only ends this source: its socket closes and
/// ffmpeg sees the input end. The report is then sent to a receiver that has
/// already gone, which is harmless.
fn capture_thread(kind: SourceKind, server: TcpListener, cancel: &AtomicBool, report: &Report) {
    if let Err(e) = capture(kind, server, cancel, report) {
        let _ = report.send(Err(e));
    }
}

/// An open, running WASAPI stream and the pieces of it the loop needs.
///
/// Fields drop in order, so the event handle goes first and the device last,
/// the reverse of the order they were opened in.
struct LiveStream {
    buffer_ready: wasapi::Handle,
    capture_client: wasapi::AudioCaptureClient,
    audio_client: wasapi::AudioClient,
    _device: wasapi::Device,
    rate: u32,
    /// Bytes per frame (all channels of one sample), never zero.
    frame_bytes: usize,
}

fn capture(kind: SourceKind, server: TcpListener, cancel: &AtomicBool, report: &Report) -> Result<()> {
    let (stream, format) = open_stream(kind)?;

    // Streaming now: ffmpeg can be started with this format.
    let _ = report.send(Ok(format));

    let mut ffmpeg = accept_with_timeout(&server, cancel, CONNECT_WAIT)?;
    forward(&stream, &mut ffmpeg, cancel);

    let stopped = stream.audio_client.stop_stream();
    stopped.map_err(device_error(kind, "stopping the capture"))
}

/// COM setup, device, format and stream start, in the order WASAPI needs.
fn open_stream(kind: SourceKind) -> Result<(LiveStream, StreamFormat)> {
    // Multithreaded apartment: this thread has no windows or message loop.
    let com = wasapi::initialize_mta();
    com.ok().map_err(failed("Windows audio (COM) could not be initialized"))?;

    let device = wasapi::get_default_device(&kind.endpoint())
        .map_err(device_error(kind, "finding the default device"))?;
    let mut audio_client = device
        .get_iaudioclient()
        .map_err(device_error(kind, "opening the audio client"))?;

    let mix = audio_client
        .get_mixformat()
        .map_err(device_error(kind, "reading the device's audio format"))?;
    let format = StreamFormat {
        rate: mix.get_samplespersec(),
        channels: mix.get_nchannels(),
        pcm: ffmpeg_format_of(&mix)?,
    };
    let frame_bytes = (mix.get_blockalign() as usize).max(1);

    let (_default_period, min_period) = audio_client
        .get_periods()
        .map_err(device_error(kind, "reading the device's timing"))?;

    // The stream is always set up for capture. For system audio, opening the
    // playback device above is what makes it loopback. No automatic format
    // conversion: ffmpeg gets the device's own mix format.
    audio_client
        .initialize_client(&mix, min_period, &Direction::Capture, &ShareMode::Shared, false)
        .map_err(device_error(kind, "setting up the audio stream"))?;

    let capture_client = audio_client
        .get_audiocaptureclient()
        .map_err(device_error(kind, "opening the capture client"))?;
    // The client is always set up for event callbacks, so an event handle has
    // to exist before the stream starts or starting fails. It also lets the
    // loop sleep until a buffer is ready instead of polling.
    let buffer_ready = audio_client
        .set_get_eventhandle()
        .map_err(device_error(kind, "creating the audio event"))?;
    audio_client
        .start_stream()
        .map_err(device_error(kind, "starting the capture"))?;

    let stream = LiveStream {
        buffer_ready,
        capture_client,
        audio_client,
        _device: device,
        rate: format.rate,
        frame_bytes,
    };
    Ok((stream, format))
}

/// Copy audio from the device to ffmpeg until told to stop or until ffmpeg
/// closes the connection (which means the recording is ending).
fn forward(stream: &LiveStream, ffmpeg: &mut TcpStream, cancel: &AtomicBool) {
    let silence = vec![0u8; stream.frame_bytes * SILENCE_BLOCK_FRAMES];
    let mut pending: VecDeque<u8> = VecDeque::new();
    // The point in time the audio sent so far reaches. Real packets move it
    // to now; silence moves it on by exactly the silence written.
    let mut sent_until = Instant::now();
    let rate = f64::from(stream.rate);

    while !cancel.load(Relaxed) {
        if stream.buffer_ready.wait_for_event(BUFFER_WAIT_MS).is_ok() {
            // Take every packet that is ready; each read fetches one.
            pending.clear();
            loop {
                let before = pending.len();
                let read = stream.capture_client.read_from_device_to_deque(&mut pending);
                if read.is_err() || pending.len() == before {
                    break;
                }
            }
            if pending.is_empty() {
                continue;
            }
            if ffmpeg.write_all(pending.make_contiguous()).is_err() {
                return;
            }
            pending.clear();
            sent_until = Instant::now();
        } else {
            // No buffer in time: the device is quiet. Fill the gap.
            let frames = (sent_until.elapsed().as_secs_f64() * rate) as usize;
            if frames == 0 {
                continue;
            }
            if write_silence(ffmpeg, &silence, frames * stream.frame_bytes).is_err() {
                return;
            }
            sent_until += Duration::from_secs_f64(frames as f64 / rate);
        }
    }
}

/// Write `len` zero bytes, a block at a time.
fn write_silence(out: &mut impl Write, block: &[u8], len: usize) -> std::io::Result<()> {
    let mut left = len;
    while left > 0 {
        let chunk = left.min(block.len());
        out.write_all(&block[..chunk])?;
        left -= chunk;
    }
    Ok(())
}

/// Wait for ffmpeg to connect, giving up if recording is cancelled or the
/// timeout passes.
fn accept_with_timeout(server: &TcpListener, cancel: &AtomicBool, timeout: Duration)
    -> Result<TcpStream>
{
    let give_up_at = Instant::now() + timeout;
    while !cancel.load(Relaxed) {
        match server.accept() {
            Ok((connection, _)) => {
                let _ = connection.set_nodelay(true);
                return Ok(connection);
            }
            Err(e) if e.kind() != ErrorKind::WouldBlock => {
                return Err(failed("the audio socket stopped accepting connections")(e));
            }
            Err(_) if Instant::now() >= give_up_at => {
                return Err(Error::Recording("ffmpeg did not connect to the audio socket in time".into()));
            }
            Err(_) => std::thread::sleep(ACCEPT_POLL),
        }
    }
    Err(Error::Recording("audio capture was cancelled before ffmpeg connected".into()))
}

/// The ffmpeg raw-PCM format name (`-f <name>`) for the device's mix format.
/// Shared mode is 32-bit float in practice; 16 and 32-bit integer are
/// accepted just in case.
fn ffmpeg_format_of(mix: &WaveFormat) -> Result<&'static str> {
    let bits = mix.get_bitspersample();
    let sample_type = mix.get_subformat().map_err(failed("the device's sample type is unreadable"))?;
    ffmpeg_pcm_name(&sample_type, bits).ok_or_else(|| Error::Recording(format!(
        "the audio device delivers samples Framefang cannot pass on yet \
         ({sample_type:?}, {bits}-bit). Please report this along with your \
         audio hardware so support can be added."
    )))
}

fn ffmpeg_pcm_name(sample_type: &SampleType, bits: u16) -> Option<&'static str> {
    match (bits, sample_type) {
        (32, SampleType::Float) => Some("f32le"),
        (16, SampleType::Int) => Some("s16le"),
        (32, SampleType::Int) => Some("s32le"),
        _ => None,
    }
}

/// Build the `map_err` adapter for one WASAPI step: `doing` says what was
/// being attempted, for the fallback message.
fn device_error(kind: SourceKind, doing: &'static str)
    -> impl FnOnce(Box<dyn std::error::Error>) -> Error
{
    move |e| classify_wasapi_error(kind, doing, &e.to_string())
}

/// Turn a WASAPI error into a message the user can act on.
///
/// Far and away the most common failure is Windows privacy settings blocking
/// the microphone (E_ACCESSDENIED, 0x80070005), so that gets its own
/// instructions, as does a missing microphone.
fn classify_wasapi_error(kind: SourceKind, doing: &str, raw: &str) -> Error {
    let text = raw.to_lowercase();
    let mentions = |needles: &[&str]| needles.iter().any(|n| text.contains(n));
    let name = kind.display_name();

    let advice = if kind.is_microphone() && mentions(&["denied", "0x80070005"]) {
        MIC_BLOCKED.to_owned()
    // ELEMENT_NOT_FOUND (0x80070490): there is no default recording device.
    // Usually no mic is connected or chosen as default, though the privacy
    // setting can also hide it from desktop apps.
    } else if kind.is_microphone() && mentions(&["element not found", "0x80070490", "not found"]) {
        MIC_MISSING.to_owned()
    // AUDCLNT_E_UNSUPPORTED_FORMAT.
    } else if mentions(&["0x88890008", "unsupported"]) {
        format!("recording {name} failed because the device's format is not supported ({raw}).")
    } else {
        format!("recording {name} failed while {doing} ({raw}).")
    };
    Error::Recording(advice)
}

const MIC_BLOCKED: &str = "Windows is not letting Framefang use the microphone.\n\n\
    In Settings, go to Privacy & Security, then Microphone, and switch on both \
    \"Microphone access\" and \"Let desktop apps access your microphone\". \
    Then try again.";

const MIC_MISSING: &str = "No microphone was found.\n\n\
    Connect a microphone and make it the default recording device in the \
    Windows Sound settings, then try again. If one is already connected, \
    check that \"Microphone access\" and \"Let desktop apps access your \
    microphone\" are both on, under Settings, Privacy & Security, Microphone.\n\n\
    To record without a microphone, choose system audio only.";

#[cfg(test)]
mod tests {
    use super::*;

    fn message(e: Error) -> String {
        match e {
            Error::Recording(m) => m,
            other => panic!("expected a recording error, got {other:?}"),
        }
    }

    #[test]
    fn a_blocked_microphone_points_at_privacy_settings() {
        for raw in ["Access is denied.", "HRESULT 0x80070005"] {
            let m = message(classify_wasapi_error(SourceKind::Microphone, "x", raw));
            assert_eq!(m, MIC_BLOCKED, "{raw}");
            assert!(m.contains("Privacy & Security"), "{m}");
        }
    }

    #[test]
    fn a_missing_microphone_says_so() {
        for raw in ["Element not found.", "0x80070490", "device not found"] {
            let m = message(classify_wasapi_error(SourceKind::Microphone, "x", raw));
            assert_eq!(m, MIC_MISSING, "{raw}");
            assert!(m.ends_with("choose system audio only."), "{m}");
        }
    }

    #[test]
    fn microphone_advice_is_only_for_the_microphone() {
        let m = message(classify_wasapi_error(
            SourceKind::SystemLoopback, "finding the default device", "Access is denied."));
        assert_eq!(m, "recording system audio failed while finding the default device (Access is denied.).");
    }

    #[test]
    fn an_unsupported_format_is_named() {
        for kind in [SourceKind::SystemLoopback, SourceKind::Microphone] {
            let m = message(classify_wasapi_error(kind, "x", "AUDCLNT 0x88890008"));
            assert_eq!(m, format!(
                "recording {} failed because the device's format is not supported (AUDCLNT 0x88890008).",
                kind.display_name()));
        }
    }

    #[test]
    fn other_errors_say_what_was_being_done() {
        let m = message(classify_wasapi_error(
            SourceKind::Microphone, "setting up the audio stream", "E_FAIL"));
        assert_eq!(m, "recording microphone failed while setting up the audio stream (E_FAIL).");
    }

    #[test]
    fn the_failed_helper_joins_context_and_cause() {
        let e = failed::<&str>("the thing broke")("because");
        assert_eq!(message(e), "the thing broke: because");
    }

    #[test]
    fn pcm_names_match_ffmpeg() {
        assert_eq!(ffmpeg_pcm_name(&SampleType::Float, 32), Some("f32le"));
        assert_eq!(ffmpeg_pcm_name(&SampleType::Int, 16), Some("s16le"));
        assert_eq!(ffmpeg_pcm_name(&SampleType::Int, 32), Some("s32le"));
        assert_eq!(ffmpeg_pcm_name(&SampleType::Float, 64), None);
        assert_eq!(ffmpeg_pcm_name(&SampleType::Int, 24), None);
    }

    #[test]
    fn sources_open_the_right_endpoint_and_name_their_threads() {
        assert!(matches!(SourceKind::SystemLoopback.endpoint(), Direction::Render));
        assert!(matches!(SourceKind::Microphone.endpoint(), Direction::Capture));
        assert_eq!(SourceKind::SystemLoopback.thread_name(), "framefang-wasapi-system-audio");
        assert_eq!(SourceKind::Microphone.thread_name(), "framefang-wasapi-microphone");
    }

    #[test]
    fn silence_is_written_in_blocks_to_the_exact_length() {
        let block = [0u8; 8];
        let mut out = Vec::new();
        write_silence(&mut out, &block, 21).unwrap();
        assert_eq!(out.len(), 21);
        assert!(out.iter().all(|&b| b == 0));

        let mut none = Vec::new();
        write_silence(&mut none, &block, 0).unwrap();
        assert!(none.is_empty());
    }

    fn quiet_listener() -> TcpListener {
        let server = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        server.set_nonblocking(true).unwrap();
        server
    }

    #[test]
    fn accept_gives_up_when_stopped() {
        let err = accept_with_timeout(&quiet_listener(), &AtomicBool::new(true), Duration::from_secs(10));
        assert_eq!(message(err.err().unwrap()), "audio capture was cancelled before ffmpeg connected");
    }

    #[test]
    fn accept_gives_up_at_the_deadline() {
        let err = accept_with_timeout(&quiet_listener(), &AtomicBool::new(false), Duration::ZERO);
        assert_eq!(message(err.err().unwrap()), "ffmpeg did not connect to the audio socket in time");
    }

    #[test]
    fn accept_returns_the_connection() {
        let server = quiet_listener();
        let port = server.local_addr().unwrap().port();
        let _client = TcpStream::connect(("127.0.0.1", port)).unwrap();
        assert!(accept_with_timeout(&server, &AtomicBool::new(false), Duration::from_secs(5)).is_ok());
    }
}
