//! Keeping Frame Fang to one running copy, and letting a second launch pass
//! a request to the first.
//!
//! Frame Fang lives in the tray. Without a guard, launching it again (the
//! desktop icon, a duplicate autostart entry, typing `framefang` in a shell)
//! gives a second tray icon, a hotkey the OS refuses to register twice, and a
//! second recorder writing to the same file names.
//!
//! The guard is a lock on a file in the config directory, held by an open
//! handle for the life of the process. The kernel owns it, so it goes away
//! when the process does, even after a crash or a kill, and there is no stale
//! PID to second-guess:
//!
//! * Unix: `flock(LOCK_EX | LOCK_NB)` on the open file.
//! * Windows: the file is opened with a share mode of 0, so any other open of
//!   it fails with a sharing violation while we hold it.
//!
//! A launch that finds the lock taken is not an error. It drops a small
//! request file next to the lock and exits, and the running copy notices the
//! file on its next tick and opens the capture overlay (or Settings for
//! `--settings`). Launching again therefore does something useful.

use std::{
    fs::{self, File, OpenOptions},
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};

/// The lock file's name in the config directory. This and the request file
/// names below are shared by every build of Frame Fang (an update relaunch
/// has an old and a new build meeting through them), so they never change.
const LOCK_NAME: &str = "instance.lock";

/// How often `acquire_waiting` tries again while the lock is taken.
const RETRY_EVERY: Duration = Duration::from_millis(100);

fn config_dir() -> Option<PathBuf> {
    framefang_core::AppSettings::config_dir()
}

/// Proof that this process is the running Frame Fang. The lock is released
/// when this is dropped, so `main` keeps it alive until exit.
pub struct InstanceLock {
    /// The handle the lock lives on. Its contents are never touched; holding
    /// it open is all it is for.
    _handle: File,
}

/// What happened when we tried to become the running Frame Fang.
pub enum Instance {
    /// We have the lock. Hold on to the guard for the whole run.
    Primary(InstanceLock),
    /// Another Frame Fang has it. The caller should pass on its request and
    /// exit.
    AlreadyRunning,
    /// The lock could not be tried at all (no config directory, a read-only
    /// one, a filesystem without locks). Starting anyway is the lesser evil:
    /// a duplicate tray icon beats an app that will not open.
    Unsupported,
}

/// Become the running instance if nobody else is, without waiting.
///
/// Winning also deletes any request files left from an earlier run. They
/// were meant for a process that is gone, and acting on them now would open
/// a window nobody asked for.
pub fn acquire() -> Instance {
    acquire_waiting(Duration::ZERO)
}

/// Like [`acquire`], but keep trying for up to `grace` while another instance
/// holds the lock.
///
/// The only expected overlap is the self-updater: it starts the newly
/// installed binary and only then exits itself. A new process that gave up
/// at once would take its own predecessor for a duplicate and quit, leaving
/// no Frame Fang running after the update.
pub fn acquire_waiting(grace: Duration) -> Instance {
    let give_up_at = Instant::now() + grace;
    let attempt = || config_dir().map_or(Instance::Unsupported, |dir| acquire_in(&dir));
    let mut outcome = attempt();
    while matches!(outcome, Instance::AlreadyRunning) && Instant::now() < give_up_at {
        thread::sleep(RETRY_EVERY);
        outcome = attempt();
    }
    outcome
}

/// One attempt at the lock in `dir`.
fn acquire_in(dir: &Path) -> Instance {
    let Ok(()) = fs::create_dir_all(dir) else { return Instance::Unsupported };
    match claim_lock(&dir.join(LOCK_NAME)) {
        Claim::Held(handle) => {
            for stale in Request::ALL {
                let _ = fs::remove_file(stale.path_in(dir));
            }
            Instance::Primary(InstanceLock { _handle: handle })
        }
        Claim::Taken => Instance::AlreadyRunning,
        Claim::Unknown => Instance::Unsupported,
    }
}

// ---- requests from a second launch ----

/// Something a second launch can ask the running instance to do. Each is a
/// file in the config directory: writing it asks, deleting it answers.
#[derive(Clone, Copy)]
enum Request {
    Capture,
    Settings,
}

impl Request {
    const ALL: [Request; 2] = [Request::Capture, Request::Settings];

    /// Written by a second launch to ask for the capture overlay, or by
    /// `framefang --settings` to ask for the Settings window.
    fn file_name(self) -> &'static str {
        match self {
            Request::Capture => "capture.request",
            Request::Settings => "settings.request",
        }
    }

    /// What goes in the file. Nothing reads it; it only makes a stray file
    /// self-explanatory.
    fn contents(self) -> &'static [u8] {
        match self {
            Request::Capture => b"capture\n",
            Request::Settings => b"settings\n",
        }
    }

    fn path_in(self, dir: &Path) -> PathBuf {
        dir.join(self.file_name())
    }

    /// Leave the request in `dir`. Failures are ignored: the worst case is a
    /// second launch that does nothing, which is what it did before the guard.
    fn raise_in(self, dir: &Path) {
        let _ = fs::create_dir_all(dir);
        let _ = fs::write(self.path_in(dir), self.contents());
    }

    /// Claim a waiting request in `dir`. Deleting the file is the claim, so
    /// only one caller can ever see a given request as true, and when nothing
    /// is waiting it costs one failed unlink.
    fn take_in(self, dir: &Path) -> bool {
        fs::remove_file(self.path_in(dir)).is_ok()
    }

    fn raise(self) {
        if let Some(dir) = config_dir() {
            self.raise_in(&dir);
        }
    }

    fn take(self) -> bool {
        config_dir().is_some_and(|dir| self.take_in(&dir))
    }
}

/// Ask the running instance to open the capture overlay. Called by a launch
/// that lost the lock, just before it exits.
pub fn request_capture() {
    Request::Capture.raise();
}

/// Claim a pending capture request. Cheap enough for every event-loop tick.
pub fn take_capture_request() -> bool {
    Request::Capture.take()
}

/// Ask for the Settings window (`framefang --settings`). The running
/// instance, or this one once its loop starts, picks it up.
pub fn request_settings() {
    Request::Settings.raise();
}

/// Claim a pending Settings request, the same way as captures.
pub fn take_settings_request() -> bool {
    Request::Settings.take()
}

// ---- the lock itself, per platform ----

/// The result of trying to take the lock file.
enum Claim {
    /// We hold it, on this handle.
    Held(File),
    /// Someone else holds it.
    Taken,
    /// Locking is not possible here, so we cannot tell.
    Unknown,
}

/// Unix: open (never truncating) and then `flock` without blocking.
#[cfg(unix)]
fn claim_lock(path: &Path) -> Claim {
    use std::os::unix::io::AsRawFd as _;

    // Any failure to open is about the environment, not another instance:
    // on Unix contention only shows up at the flock.
    let Ok(handle) = open_token(path) else { return Claim::Unknown };
    // Exclusive, and non-blocking so a held lock answers at once.
    let mode = libc::LOCK_EX | libc::LOCK_NB;
    if unsafe { libc::flock(handle.as_raw_fd(), mode) } == 0 {
        return Claim::Held(handle);
    }
    match std::io::Error::last_os_error().raw_os_error() {
        // EWOULDBLOCK (the same number as EAGAIN everywhere we ship) is the
        // only answer that means another holder.
        Some(code) if code == libc::EWOULDBLOCK => Claim::Taken,
        // Some network filesystems do not implement flock at all.
        _ => Claim::Unknown,
    }
}

/// Windows: the exclusive open is the whole lock.
#[cfg(windows)]
fn claim_lock(path: &Path) -> Claim {
    /// Another handle's share mode shuts us out.
    const ERROR_SHARING_VIOLATION: i32 = 32;
    /// Some filesystems report the same situation as access denied.
    const ERROR_ACCESS_DENIED: i32 = 5;

    match open_token(path) {
        Ok(file) => Claim::Held(file),
        Err(e) => match e.raw_os_error() {
            Some(ERROR_SHARING_VIOLATION | ERROR_ACCESS_DENIED) => Claim::Taken,
            _ => Claim::Unknown,
        },
    }
}

/// Anywhere else there is no lock to take, but that must not stop the app.
#[cfg(not(any(unix, windows)))]
fn claim_lock(path: &Path) -> Claim {
    let _ = open_token(path);
    Claim::Unknown
}

/// Open (creating if needed) the lock file without truncating it: it is a
/// token another process may have open, not data to rewrite.
fn open_token(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        // Share mode 0: no other open of this file succeeds until our handle
        // closes, which the kernel guarantees even if we crash.
        options.share_mode(0);
    }
    options.open(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh, empty directory for one test, removed again when the test
    /// ends (passing or not). The name carries the process id and the test's
    /// tag, so parallel tests and parallel runs never share one.
    struct TestDir(PathBuf);

    impl TestDir {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir()
                .join(format!("framefang-instance-test-{}-{tag}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).expect("temp dir should be writable");
            TestDir(dir)
        }

        fn lock_path(&self) -> PathBuf {
            self.0.join(LOCK_NAME)
        }
    }

    impl std::ops::Deref for TestDir {
        type Target = Path;
        fn deref(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn the_lock_is_exclusive_until_released() {
        let dir = TestDir::new("exclusive");

        let Claim::Held(holder) = claim_lock(&dir.lock_path()) else {
            panic!("the first claim should win")
        };
        assert!(matches!(claim_lock(&dir.lock_path()), Claim::Taken),
            "a second claim must see the lock as taken");

        drop(holder);
        assert!(matches!(claim_lock(&dir.lock_path()), Claim::Held(_)),
            "dropping the holder must free the lock");
    }

    #[test]
    fn a_second_acquire_sees_the_first_running() {
        let dir = TestDir::new("acquire");
        let running = acquire_in(&dir);
        assert!(matches!(running, Instance::Primary(_)));
        assert!(matches!(acquire_in(&dir), Instance::AlreadyRunning));
        drop(running);
        assert!(matches!(acquire_in(&dir), Instance::Primary(_)));
    }

    #[test]
    fn winning_the_lock_clears_stale_requests() {
        let dir = TestDir::new("stale");
        Request::Capture.raise_in(&dir);
        Request::Settings.raise_in(&dir);

        let won = acquire_in(&dir);
        assert!(matches!(won, Instance::Primary(_)));
        assert!(!Request::Capture.take_in(&dir));
        assert!(!Request::Settings.take_in(&dir));
    }

    #[test]
    fn opening_the_lock_file_keeps_its_contents() {
        let dir = TestDir::new("keep");
        fs::write(dir.lock_path(), b"held by someone").unwrap();
        drop(open_token(&dir.lock_path()).unwrap());
        assert_eq!(fs::read(dir.lock_path()).unwrap(), b"held by someone");
    }

    #[test]
    fn each_request_is_taken_once() {
        let dir = TestDir::new("once");
        assert!(!Request::Capture.take_in(&dir), "nothing waiting yet");

        Request::Capture.raise_in(&dir);
        assert!(Request::Capture.take_in(&dir), "the first take wins");
        assert!(!Request::Capture.take_in(&dir), "a second take finds nothing");
    }

    #[test]
    fn capture_and_settings_requests_are_independent() {
        let dir = TestDir::new("independent");
        Request::Settings.raise_in(&dir);
        assert!(!Request::Capture.take_in(&dir));
        assert!(Request::Settings.take_in(&dir));
    }

    /// Taking a request deletes a file, so no request may share a name with
    /// the lock the running instance is holding.
    #[test]
    fn request_files_never_name_the_lock() {
        for request in Request::ALL {
            assert_ne!(request.file_name(), LOCK_NAME);
        }
        assert_ne!(Request::Capture.file_name(), Request::Settings.file_name());
    }
}
