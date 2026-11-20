//! Keeping the recorder process from outliving Frame Fang.
//!
//! A recording is a long-running child (`ffmpeg`, or `screencapture` on
//! macOS). If Frame Fang crashed, was killed, or was taken down at logout,
//! nothing stopped that child: it kept the screen grab or camera open and kept
//! writing a file that no window could stop any more. So there are two
//! defences.
//!
//! **While we run, the child is tied to us.** Each OS gets the strongest tie
//! it offers:
//!
//! * Linux: `PR_SET_PDEATHSIG` set to SIGTERM just before exec, so the kernel
//!   signals the child the moment we die. ffmpeg finalizes the file on
//!   SIGTERM, so the clip usually still plays.
//! * Windows: the child joins a Job Object marked kill-on-close. Only we hold
//!   a handle to the job, the kernel closes it when we exit for any reason,
//!   and that kills everything inside.
//! * macOS: neither exists, so a small detached shell loop watches both pids
//!   and interrupts the child once we are gone.
//!
//! **On the next launch, leftovers are stopped.** The running recorder's pid
//! and program name are written to a file in the config directory and removed
//! when the recording ends normally. A file still present at startup means the
//! last run never cleaned up (the watchdog was killed too, the power went).
//! The pid is stopped only if it is still running a recorder program we know,
//! since the OS may have handed that number to something else since.

use std::{path::{Path, PathBuf}, process::{Child, Command}};
#[cfg(any(windows, target_os = "macos", test))]
use std::process::Stdio;

/// File in the config directory that describes the live recorder. Older and
/// newer builds must agree on it, since a record can outlive an update.
const RECORD_FILE: &str = "recorder.pid";

/// Program names a leftover record may point at. Anything else under that pid
/// is an unrelated process that inherited the number, and leaving an encoder
/// running is far better than killing it.
const REAPABLE_PROGRAMS: &[&str] = &["ffmpeg", "ffmpeg.exe", "screencapture"];

fn record_path() -> Option<PathBuf> {
    framefang_core::AppSettings::config_dir().map(|dir| dir.join(RECORD_FILE))
}

fn is_reapable(image: &str) -> bool {
    REAPABLE_PROGRAMS.iter().any(|known| *known == image)
}

/// Spawn `cmd` (already set up by the recorder with its arguments and stdio)
/// so that it cannot outlive this process, and note it as the live recorder.
///
/// This only adds lifetime plumbing; every choice about what runs stays with
/// the caller.
pub(crate) fn spawn_guarded(cmd: &mut Command) -> std::io::Result<Child> {
    let image = image_name(Path::new(cmd.get_program()));
    tie_before_exec(cmd);
    cmd.spawn().map(|recorder| {
        tie_after_spawn(&recorder);
        write_live_record(&LiveRecord { pid: recorder.id(), image });
        recorder
    })
}

/// Remove the live-recorder record. Every recorder shutdown path calls this,
/// so a record that survives means the process died without shutting down.
pub(crate) fn clear_live() {
    let _ = record_path().map(std::fs::remove_file);
}

/// Stop a recorder that a previous run of Frame Fang left behind, returning
/// its pid if one was stopped.
///
/// Cheap to call on every startup: normally the file is not there. Only the
/// instance that won the single-instance lock calls it, so it never sees a
/// recording that is still legitimately running.
pub fn reap_orphaned_recorder() -> Option<u32> {
    let record_file = record_path()?;
    let contents = std::fs::read_to_string(&record_file).ok();
    // The record is deleted whatever happens next. It describes a run that is
    // over, and one we cannot act on should not be retried on every launch.
    let _ = std::fs::remove_file(&record_file);

    let LiveRecord { pid, image } = LiveRecord::decode(&contents?)?;
    (is_reapable(&image) && stop_if_still_running(pid, &image)).then_some(pid)
}

fn write_live_record(record: &LiveRecord) {
    let Some(target) = record_path() else { return };
    if let Some(folder) = target.parent() {
        let _ = std::fs::create_dir_all(folder);
    }
    let _ = framefang_core::atomic_file::write_atomic(&target, record.encode().as_bytes());
}

/// The file name of `program`, lowercased so that Windows image names compare
/// the way Windows itself compares them.
fn image_name(program: &Path) -> String {
    match program.file_name() {
        Some(name) => name.to_string_lossy().to_ascii_lowercase(),
        None => String::new(),
    }
}

/// What the pidfile holds: which process, and which program it was running.
#[derive(Debug, PartialEq, Eq)]
struct LiveRecord {
    pid: u32,
    image: String,
}

impl LiveRecord {
    /// Two lines, pid then program name. Plain text keeps this crate free of
    /// a serde dependency.
    fn encode(&self) -> String {
        format!("{}\n{}\n", self.pid, self.image)
    }

    /// Read back what [`encode`](Self::encode) wrote. Anything incomplete or
    /// odd (a half-written file, a hand edit, pid 0) yields `None`, because a
    /// guess here would end with us signalling the wrong process.
    fn decode(text: &str) -> Option<Self> {
        let mut lines = text.lines().map(str::trim);
        let pid = lines.next()?.parse::<u32>().ok().filter(|&p| p != 0)?;
        let image = lines.next()?.to_ascii_lowercase();
        (!image.is_empty()).then_some(LiveRecord { pid, image })
    }
}

// ---- stopping a leftover recorder ----

/// Unix: interrupt the process if it still runs `image`, give it a second to
/// finish its file, then force it.
#[cfg(unix)]
fn stop_if_still_running(pid: u32, image: &str) -> bool {
    use std::time::Duration;

    if process_image(pid).as_deref() != Some(image) {
        return false;
    }
    let target = pid as libc::pid_t;
    let is_alive = || unsafe { libc::kill(target, 0) } == 0;

    // ffmpeg and screencapture both treat SIGINT as "stop and write the file
    // out properly", so a reaped recording is usually still playable. It gets
    // ten checks, 100 ms apart, to exit.
    unsafe { libc::kill(target, libc::SIGINT) };
    let exited = (0..10).any(|_| {
        std::thread::sleep(Duration::from_millis(100));
        !is_alive()
    });
    if !exited {
        // A second was its chance to go quietly. Nothing is meant to
        // survive this.
        unsafe { libc::kill(target, libc::SIGKILL) };
    }
    true
}

/// Linux: the kernel keeps the program name in `/proc/<pid>/comm`, no
/// subprocess needed. It is cut at 15 characters, which every name in
/// [`REAPABLE_PROGRAMS`] fits inside.
#[cfg(target_os = "linux")]
fn process_image(pid: u32) -> Option<String> {
    let comm_file = format!("/proc/{pid}/comm");
    let name = std::fs::read_to_string(comm_file).ok()?;
    Some(name.trim().to_ascii_lowercase())
}

/// macOS: no `/proc`, so ask `ps` for the executable path (no header line).
/// A pid that has exited prints nothing and fails.
#[cfg(target_os = "macos")]
fn process_image(pid: u32) -> Option<String> {
    let output = Command::new("/bin/ps")
        .arg("-p")
        .arg(pid.to_string())
        .args(["-o", "comm="])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()
        .filter(|finished| finished.status.success())?;
    let printed = String::from_utf8(output.stdout).ok()?;
    let exe = printed.trim();
    if exe.is_empty() {
        return None;
    }
    Some(image_name(Path::new(exe))).filter(|name| !name.is_empty())
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
fn process_image(_pid: u32) -> Option<String> {
    None
}

/// Windows: `taskkill` filtered on both the pid and the image name, so it
/// only acts if that pid still runs that program. Windows offers no gentle
/// stop for a child without a console, so the file is left unfinished; the
/// job object normally means there is no orphan to reap.
#[cfg(windows)]
fn stop_if_still_running(pid: u32, image: &str) -> bool {
    use std::os::windows::process::CommandExt as _;

    /// Keeps the console program from flashing a window at startup.
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    let pid_filter = format!("PID eq {pid}");
    let image_filter = format!("IMAGENAME eq {image}");
    Command::new("taskkill")
        .args(["/F", "/FI", &pid_filter, "/FI", &image_filter])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .status()
        .is_ok_and(|status| status.success())
}

#[cfg(not(any(unix, windows)))]
fn stop_if_still_running(_pid: u32, _image: &str) -> bool {
    false
}

// ---- tying the child to our lifetime ----

/// Linux: have the kernel send SIGTERM to the child when we die.
#[cfg(target_os = "linux")]
fn tie_before_exec(cmd: &mut Command) {
    use std::os::unix::process::CommandExt as _;

    let our_pid = std::process::id() as libc::pid_t;
    // prctl reads the signal as an unsigned long through varargs, so it is
    // widened here rather than passed as a c_int with an undefined upper half.
    let death_signal = libc::SIGTERM as libc::c_ulong;
    let in_child = move || -> std::io::Result<()> {
        unsafe { libc::prctl(libc::PR_SET_PDEATHSIG, death_signal) };
        // If we died between the fork and the prctl, the death signal has
        // already been missed. A changed parent pid gives that away.
        if unsafe { libc::getppid() } != our_pid {
            unsafe { libc::_exit(0) };
        }
        Ok(())
    };
    // SAFETY: this runs in the child between fork and exec, where only
    // async-signal-safe functions may be called; prctl, getppid and _exit are.
    unsafe { cmd.pre_exec(in_child) };
}

#[cfg(not(target_os = "linux"))]
fn tie_before_exec(_cmd: &mut Command) {}

/// Windows: place the child in our kill-on-close job.
#[cfg(windows)]
fn tie_after_spawn(child: &Child) {
    use std::os::windows::io::AsRawHandle as _;
    use windows::Win32::Foundation::HANDLE as Handle;

    let as_handle = |raw: *mut core::ffi::c_void| Handle(raw);
    let Some(job_address) = process_job() else { return };
    let child_handle = as_handle(child.as_raw_handle() as *mut _);
    // SAFETY: the job handle is never closed, and `child` owns its process
    // handle for at least as long as this call.
    let _ = unsafe {
        windows::Win32::System::JobObjects::AssignProcessToJobObject(
            as_handle(job_address as *mut _),
            child_handle,
        )
    };
}

/// The one job for this process, made on first use and never closed: the
/// open handle is the whole mechanism. When the process ends, however it
/// ends, the kernel closes the handle and kills every process in the job.
/// Stored as an address because a raw handle is not `Sync`.
#[cfg(windows)]
fn process_job() -> Option<usize> {
    use windows::Win32::System::JobObjects as jobs;

    type Limits = jobs::JOBOBJECT_EXTENDED_LIMIT_INFORMATION;
    static JOB_ADDRESS: std::sync::OnceLock<Option<usize>> = std::sync::OnceLock::new();

    let create = || unsafe {
        // No name, so no other process can open the same job by guessing it.
        let handle = jobs::CreateJobObjectW(None, windows::core::PCWSTR::null()).ok()?;
        let mut limits = Limits::default();
        limits.BasicLimitInformation.LimitFlags = jobs::JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let limits_size = std::mem::size_of::<Limits>() as u32;
        let limits_ptr: *const core::ffi::c_void = (&limits as *const Limits).cast();
        let class = jobs::JobObjectExtendedLimitInformation;
        jobs::SetInformationJobObject(handle, class, limits_ptr, limits_size).ok()?;
        Some(handle.0 as usize)
    };
    *JOB_ADDRESS.get_or_init(create)
}

/// macOS: start a detached watchdog shell for this child.
#[cfg(target_os = "macos")]
fn tie_after_spawn(child: &Child) {
    // The shell we start puts the loop in the background and exits straight
    // away, so waiting on it is instant and leaves no zombie, while the loop
    // itself is adopted by launchd and keeps watching.
    let watchdog = watchdog_command(std::process::id(), child.id());
    let launched = Command::new("/bin/sh")
        .args(["-c", watchdog.as_str()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    if let Ok(mut shell) = launched {
        let _ = shell.wait();
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
fn tie_after_spawn(_child: &Child) {}

/// The macOS watchdog, as one shell command line.
///
/// Once a second it checks that both processes are alive. When one of them
/// is gone it looks at why: if the recorder ended first (the normal case) it
/// simply stops; if we are the one that vanished, it interrupts the recorder
/// so the file gets finalized, and kills it outright five seconds later if it
/// is still there. Built as a plain string so tests can inspect it anywhere.
#[cfg(any(target_os = "macos", test))]
fn watchdog_command(parent_pid: u32, recorder_pid: u32) -> String {
    let alive = |pid: u32| format!("/bin/kill -0 {pid} 2>/dev/null");
    let watch = format!("while {} && {}; do sleep 1; done", alive(parent_pid), alive(recorder_pid));
    let stop = format!(
        "{{ /bin/kill -INT {recorder_pid} 2>/dev/null; sleep 5; /bin/kill -9 {recorder_pid} 2>/dev/null; }}"
    );
    format!("( {watch}; {} || {stop} ) >/dev/null 2>&1 &", alive(parent_pid))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(pid: u32, image: &str) -> LiveRecord {
        LiveRecord { pid, image: image.to_owned() }
    }

    #[test]
    fn a_record_reads_back_as_written() {
        let written = record(4321, "ffmpeg").encode();
        assert_eq!(written, "4321\nffmpeg\n");
        assert_eq!(LiveRecord::decode(&written), Some(record(4321, "ffmpeg")));
    }

    #[test]
    fn the_image_is_read_back_lowercase() {
        let written = record(7, "FFmpeg.EXE").encode();
        assert_eq!(LiveRecord::decode(&written), Some(record(7, "ffmpeg.exe")));
    }

    #[test]
    fn whitespace_and_extra_lines_are_tolerated() {
        assert_eq!(LiveRecord::decode(" 99 \r\n ffmpeg \r\nleftover\n"), Some(record(99, "ffmpeg")));
    }

    /// Anything incomplete must decode to nothing, never to a pid we would
    /// then go and signal.
    #[test]
    fn broken_records_decode_to_nothing() {
        let broken = ["", "\n", "notapid\nffmpeg\n", "1234", "1234\n", "1234\n\n", "0\nffmpeg\n", "-5\nffmpeg\n"];
        for text in broken {
            assert_eq!(LiveRecord::decode(text), None, "{text:?} should not decode");
        }
    }

    #[test]
    fn only_recorder_programs_may_be_stopped() {
        for name in ["ffmpeg", "ffmpeg.exe", "screencapture"] {
            assert!(is_reapable(name), "{name} should be reapable");
        }
        for name in ["bash", "explorer.exe", "framefang", "FFMPEG", ""] {
            assert!(!is_reapable(name), "{name:?} must never be reapable");
        }
    }

    /// The name recorded at spawn time must be one the reaper accepts, or a
    /// leftover would be recorded and then refused. Paths use the host's own
    /// separators, since `Path::file_name` only splits on `\` on Windows.
    #[test]
    fn recorder_programs_record_reapable_names() {
        #[cfg(windows)]
        let programs = ["ffmpeg.exe", "FFMPEG.EXE", r"C:\Program Files\Framefang\ffmpeg.exe"];
        #[cfg(not(windows))]
        let programs = [
            "ffmpeg",
            "/usr/bin/ffmpeg",
            "screencapture",
            "/Applications/Frame Fang.app/Contents/Resources/ffmpeg",
        ];
        for program in programs {
            let name = image_name(Command::new(program).get_program().as_ref());
            assert!(is_reapable(&name), "{program} gave {name:?}");
        }
    }

    #[test]
    fn a_path_with_no_file_name_gives_an_empty_image() {
        assert_eq!(image_name(Path::new("")), "");
        assert!(!is_reapable(&image_name(Path::new(".."))));
    }

    #[test]
    fn the_watchdog_watches_both_and_detaches() {
        let script = watchdog_command(111, 222);
        assert!(script.contains("-0 111") && script.contains("-0 222"), "{script}");
        assert!(script.trim_end().ends_with('&'), "the watchdog must run detached: {script}");
        let interrupt = script.find("-INT 222").expect(&script);
        let kill = script.find("-9 222").expect(&script);
        assert!(interrupt < kill, "interrupt first, force only after: {script}");
    }

    /// `sh -n` parses the script without running it.
    #[cfg(unix)]
    #[test]
    fn the_watchdog_is_valid_shell() {
        use std::io::Write as _;

        let mut sh = Command::new("/bin/sh")
            .arg("-n")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("/bin/sh should start");
        let script = watchdog_command(1, 2);
        sh.stdin.take().unwrap().write_all(script.as_bytes()).unwrap();
        assert!(sh.wait().unwrap().success(), "not valid shell: {script}");
    }
}
