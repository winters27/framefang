//! Replace a file so that nobody, including the next launch after a crash,
//! ever sees it half written.
//!
//! `fs::write` empties the file first and then fills it, so dying partway
//! (a crash, a power cut, the OOM killer) leaves a truncated file. For
//! `settings.json` that means `AppSettings::load` fails to parse, quietly
//! starts from defaults, and the user's save folder, hotkey, tools and theme
//! are lost.
//!
//! So the new bytes go to a sibling temp file first, are flushed to disk, and
//! only then is the temp renamed over the real name. Readers see either the
//! complete old file or the complete new one. Three details matter:
//!
//! * The temp sits in the destination's own directory. A rename is only
//!   atomic inside one filesystem, and the system temp directory is often on
//!   another.
//! * The data is fsynced before the rename. Otherwise the filesystem may
//!   commit the rename first, and a crash in between leaves an empty file
//!   under the real name.
//! * `fs::rename` replaces an existing file atomically everywhere we ship:
//!   `rename(2)` on Unix, and `MoveFileExW` with `MOVEFILE_REPLACE_EXISTING`
//!   in Rust's Windows standard library.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::Duration;

/// How many times to try the final rename. On Windows an antivirus scanner
/// or the search indexer often opens a file the moment it appears, and the
/// rename fails with a sharing violation until it lets go a few milliseconds
/// later. On Unix the first attempt always settles it.
const RENAME_TRIES: u32 = 4;

/// Pause between rename attempts.
const RENAME_BACKOFF: Duration = Duration::from_millis(40);

/// Per-process counter that makes every temp name distinct. The process id
/// in the name separates writers in different processes.
static TEMP_SERIAL: AtomicU64 = AtomicU64::new(0);

/// Where the bytes for `dest` are staged before the rename.
///
/// Same directory as `dest`, hidden on Unix by a leading dot, and unique per
/// call: `.{name}.tmp-{pid}-{serial}`.
fn staging_path(dest: &Path) -> PathBuf {
    let name = match dest.file_name() {
        Some(n) => n.to_string_lossy().into_owned(),
        None => String::from("file"),
    };
    let serial = TEMP_SERIAL.fetch_add(1, Ordering::Relaxed);
    let staged = format!(".{name}.tmp-{}-{serial}", std::process::id());
    match dest.parent() {
        Some(dir) => dir.join(staged),
        None => Path::new(".").join(staged),
    }
}

/// Create `path` (it must not exist yet), write `bytes` into it and flush
/// them to the disk. The handle is closed on return, which Windows needs
/// before it will rename the file.
fn write_and_flush(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file: File = OpenOptions::new().write(true).create_new(true).open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

/// Rename `from` over `to`, retrying a few times for transient Windows
/// sharing violations. Returns the error from the final attempt.
fn rename_with_retries(from: &Path, to: &Path) -> io::Result<()> {
    let mut attempt = 1;
    loop {
        match fs::rename(from, to) {
            Ok(()) => return Ok(()),
            Err(e) if attempt >= RENAME_TRIES => return Err(e),
            Err(_) => {
                attempt += 1;
                thread::sleep(RENAME_BACKOFF);
            }
        }
    }
}

/// Atomically replace the contents of `dest` with `bytes`.
///
/// The parent directory has to exist already. Whether it should be created
/// is the caller's decision, not this function's.
///
/// If anything fails, the staged temp file is deleted and `dest` keeps
/// whatever it held before, so a failed save never costs the last good copy.
pub fn write_atomic(dest: &Path, bytes: &[u8]) -> io::Result<()> {
    let staged = staging_path(dest);
    let result = write_and_flush(&staged, bytes).and_then(|()| rename_with_retries(&staged, dest));
    match result {
        Ok(()) => {
            flush_directory_entry(dest);
            Ok(())
        }
        Err(e) => {
            let _ = fs::remove_file(&staged);
            Err(e)
        }
    }
}

/// Make the rename itself durable by syncing the directory that holds it.
///
/// Best effort only: readers already see the rename atomically, and this
/// just narrows the window where a power cut right afterwards could lose it.
/// Windows has nothing equivalent to sync (and `MoveFileExW` already orders
/// the metadata), so there it does nothing.
#[cfg(unix)]
fn flush_directory_entry(dest: &Path) {
    let Some(dir) = dest.parent() else { return };
    if let Ok(handle) = File::open(dir) {
        let _ = handle.sync_all();
    }
}

#[cfg(not(unix))]
fn flush_directory_entry(_dest: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh empty directory under the OS temp dir, removed on drop.
    /// framefang-core keeps its dependency list short, so no `tempfile`.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(label: &str) -> Scratch {
            let serial = TEMP_SERIAL.fetch_add(1, Ordering::Relaxed);
            let dir = std::env::temp_dir()
                .join(format!("framefang-atomic-{}-{label}-{serial}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Scratch(dir)
        }

        fn settings(&self) -> PathBuf {
            self.0.join("settings.json")
        }

        /// Names in the directory other than `settings.json`: anything here
        /// is a temp file that should have been cleaned up.
        fn leftovers(&self) -> Vec<String> {
            let mut names: Vec<String> = fs::read_dir(&self.0)
                .unwrap()
                .flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| n != "settings.json")
                .collect();
            names.sort();
            names
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn read(path: &Path) -> String {
        fs::read_to_string(path).unwrap()
    }

    #[test]
    fn creates_a_file_that_did_not_exist() {
        let s = Scratch::new("create");
        write_atomic(&s.settings(), b"{\"a\":1}").unwrap();
        assert_eq!(read(&s.settings()), "{\"a\":1}");
    }

    #[test]
    fn a_shorter_write_leaves_no_tail_of_the_old_one() {
        let s = Scratch::new("shorter");
        write_atomic(&s.settings(), b"a long first version of the file").unwrap();
        write_atomic(&s.settings(), b"v2").unwrap();
        assert_eq!(read(&s.settings()), "v2");
    }

    #[test]
    fn repeated_saves_clean_up_their_temp_files() {
        let s = Scratch::new("tidy");
        for round in 0..5 {
            write_atomic(&s.settings(), round.to_string().as_bytes()).unwrap();
        }
        assert_eq!(read(&s.settings()), "4");
        assert!(s.leftovers().is_empty(), "stray temps: {:?}", s.leftovers());
    }

    #[test]
    fn staging_happens_beside_the_destination() {
        // Same directory means same filesystem, which the atomic rename needs.
        let dest = Path::new("some").join("dir").join("settings.json");
        let staged = staging_path(&dest);
        assert_eq!(staged.parent(), dest.parent());
        assert_ne!(staged.file_name(), dest.file_name());
    }

    #[test]
    fn staged_names_are_hidden_and_never_repeat() {
        let target = Path::new("/not/a/real/dir/config.json");
        let first = staging_path(target);
        let second = staging_path(target);
        assert_ne!(first, second);
        let name = first.file_name().unwrap().to_string_lossy().into_owned();
        let prefix = format!(".config.json.tmp-{}-", std::process::id());
        assert!(name.starts_with(&prefix), "{name}");
    }

    #[test]
    fn a_missing_directory_is_reported_not_created() {
        let s = Scratch::new("nodir");
        let orphan = s.0.join("absent").join("settings.json");
        assert!(write_atomic(&orphan, b"lost").is_err());
        assert!(!orphan.try_exists().unwrap(), "nothing may appear at the destination");
        assert!(!s.0.join("absent").is_dir(), "the folder must not be created");
    }

    #[test]
    fn a_failed_save_keeps_the_last_good_copy() {
        let s = Scratch::new("keep");
        write_atomic(&s.settings(), b"good").unwrap();
        let unreachable = s.0.join("missing").join("settings.json");
        assert!(write_atomic(&unreachable, b"bad").is_err());
        assert_eq!(read(&s.settings()), "good");
        assert!(s.leftovers().is_empty());
    }

    #[test]
    fn a_rename_that_keeps_failing_cleans_up_after_itself() {
        // A directory squatting on the destination name makes every rename
        // attempt fail on all platforms, which exercises the retry path.
        let s = Scratch::new("squat");
        let dest = s.0.join("taken");
        fs::create_dir(&dest).unwrap();
        assert!(write_atomic(&dest, b"payload").is_err());
        assert!(dest.is_dir(), "the existing directory must be left alone");
        let others: Vec<String> = s.leftovers().into_iter().filter(|n| n != "taken").collect();
        assert!(others.is_empty(), "stray temps: {others:?}");
    }

    #[test]
    fn empty_content_is_a_valid_save() {
        let s = Scratch::new("empty");
        write_atomic(&s.settings(), b"something").unwrap();
        write_atomic(&s.settings(), b"").unwrap();
        assert_eq!(read(&s.settings()), "");
    }

    #[test]
    fn parallel_writers_never_produce_a_mixed_file() {
        // The last rename wins, and the result is one whole payload.
        let s = Scratch::new("race");
        let dest = s.settings();
        let payloads: Vec<String> = ["x", "y", "z"].iter().map(|c| c.repeat(256)).collect();

        thread::scope(|scope| {
            for body in &payloads {
                let dest = &dest;
                scope.spawn(move || {
                    for _round in 0..12 {
                        write_atomic(dest, body.as_bytes()).unwrap();
                    }
                });
            }
        });

        let landed = read(&dest);
        assert!(payloads.contains(&landed), "torn write of {} bytes", landed.len());
        assert!(s.leftovers().is_empty());
    }
}
