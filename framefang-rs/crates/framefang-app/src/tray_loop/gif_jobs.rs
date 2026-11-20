//! Turning a GIF recording into a GIF. The recorder writes an MP4 into a work
//! folder, a worker thread converts it, and the result is named and moved into
//! the recordings folder. Whatever goes wrong, the MP4 is kept.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::Receiver;
use std::sync::Arc;
use std::thread::JoinHandle;

use chrono::Local;
use framefang_core::file_name::{self, NameFacts};

use super::desktop::notify;

/// Ends the name of a file that is still being copied into place. A file
/// carrying it is never a finished clip.
pub(super) const PART_SUFFIX: &str = ".framefang-part";

/// A GIF recording under way: where its MP4 is and how its GIF will be named.
pub(super) struct GifPlan {
    pub(super) work: PathBuf,
    pub(super) dir: PathBuf,
    pub(super) pattern: String,
    pub(super) facts: NameFacts,
}

/// A recording being converted on its own thread.
pub(super) struct GifJob {
    pub(super) rx: Receiver<GifResult>,
    pub(super) cancel: Arc<AtomicBool>,
    pub(super) handle: JoinHandle<()>,
}

pub(super) enum GifResult {
    Done(PathBuf),
    /// The GIF failed or was cancelled. `kept` is where the MP4 went, or
    /// `None` when it stayed in the work folder for the next start's sweep.
    Failed { error: String, kept: Option<PathBuf> },
}

/// Where GIF recordings wait, as MP4, until they are converted.
pub(super) fn gif_work_dir() -> PathBuf {
    directories::ProjectDirs::from("org", "framefang", "Framefang")
        .map(|d| d.cache_dir().join("gif-work"))
        .unwrap_or_else(|| std::env::temp_dir().join("framefang-gif-work"))
}

/// The name a file is copied under until it is whole.
fn part_path(to: &Path) -> PathBuf {
    let mut name = to.as_os_str().to_owned();
    name.push(PART_SUFFIX);
    name.into()
}

/// Move `from` onto `to` (which may already exist, empty, as a claimed name).
/// A rename when possible; across drives, a copy and then a delete.
fn move_file(from: &Path, to: &Path) -> io::Result<()> {
    if std::fs::rename(from, to).is_ok() {
        return Ok(());
    }
    // Copy beside the target under the work name and rename only once it is
    // whole: a quit halfway must never leave a cut-off file under the real
    // name (the start-up sweep deletes work names).
    let staging = part_path(to);
    match std::fs::copy(from, &staging).and_then(|_| std::fs::rename(&staging, to)) {
        Ok(()) => {
            let _ = std::fs::remove_file(from);
            Ok(())
        }
        Err(e) => {
            let _ = std::fs::remove_file(&staging);
            let _ = std::fs::remove_file(to);
            Err(e)
        }
    }
}

/// Keep a GIF recording's MP4 in `dir` under a name of its own. `None` when
/// no name could be claimed or the move failed.
pub(super) fn keep_gif_mp4(work: &Path, dir: &Path, pattern: &str, facts: &NameFacts) -> Option<PathBuf> {
    let _ = std::fs::create_dir_all(dir);
    let (path, claimed) = file_name::claim(dir, pattern, facts, "mp4").ok()?;
    drop(claimed);
    move_file(work, &path).ok()?;
    Some(path)
}

/// Claim the GIF's final name and move the finished file onto it.
fn place_gif(plan: &GifPlan, finished: &Path) -> Result<PathBuf, String> {
    let _ = std::fs::create_dir_all(&plan.dir);
    let (path, claimed) =
        file_name::claim(&plan.dir, &plan.pattern, &plan.facts, "gif").map_err(|e| e.to_string())?;
    drop(claimed);
    move_file(finished, &path).map_err(|e| e.to_string())?;
    Ok(path)
}

/// Convert `plan`'s MP4 into a GIF and name it. Runs on its own thread.
pub(super) fn gif_worker(ffmpeg: PathBuf, plan: GifPlan, cancel: &AtomicBool) -> GifResult {
    let staging = part_path(&plan.work.with_extension("gif"));
    let give_up = |error: String| GifResult::Failed {
        error,
        kept: keep_gif_mp4(&plan.work, &plan.dir, &plan.pattern, &plan.facts),
    };
    if let Err(e) = crate::gif::convert(&ffmpeg, &plan.work, &staging, crate::gif::RECORDING, cancel) {
        return give_up(e);
    }
    match place_gif(&plan, &staging) {
        Ok(path) => {
            let _ = std::fs::remove_file(&plan.work);
            GifResult::Done(path)
        }
        Err(e) => {
            let _ = std::fs::remove_file(&staging);
            give_up(e)
        }
    }
}

/// Files in the work folder left by an earlier run. This run's own files carry
/// its process id in the name and are skipped.
fn leftovers_from_earlier_runs() -> Vec<PathBuf> {
    let mine = format!("-{}.", std::process::id());
    let Ok(listing) = std::fs::read_dir(gif_work_dir()) else { return Vec::new() };
    listing
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| !path.file_name().is_some_and(|n| n.to_string_lossy().contains(&mine)))
        .collect()
}

/// Remove half-copied files a quit left in the recordings folder.
fn clear_part_files(recordings: &Path) {
    let Ok(listing) = std::fs::read_dir(recordings) else { return };
    for entry in listing.flatten() {
        if entry.file_name().to_string_lossy().ends_with(PART_SUFFIX) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// At start-up: GIF recordings that a crash or a quit left unconverted move to
/// the recordings folder as MP4, half-made files are removed, and one toast
/// says so.
pub(super) fn sweep_gif_work(recordings: PathBuf, pattern: String) {
    // Listed now, before anything can start recording, so that only files from
    // earlier runs are swept and never one this run is writing.
    let found = leftovers_from_earlier_runs();
    clear_part_files(&recordings);
    if found.is_empty() {
        return;
    }
    std::thread::spawn(move || {
        let mut rescued = 0;
        for path in found {
            let is_mp4 = path.extension().is_some_and(|x| x.eq_ignore_ascii_case("mp4"));
            let has_content = std::fs::metadata(&path).is_ok_and(|m| m.len() > 0);
            if !(is_mp4 && has_content) {
                let _ = std::fs::remove_file(&path);
                continue;
            }
            let facts = NameFacts::at(Local::now(), 0, 0);
            let _ = std::fs::create_dir_all(&recordings);
            if keep_gif_mp4(&path, &recordings, &pattern, &facts).is_some() {
                rescued += 1;
            }
        }
        if rescued > 0 {
            let (noun, verb) = if rescued == 1 { ("recording", "was") } else { ("recordings", "were") };
            notify(
                "Frame Fang: unfinished GIF recordings",
                &format!(
                    "{rescued} {noun} that never became a GIF {verb} saved as MP4 in {}.",
                    recordings.display()
                ),
                false,
            );
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ff-gif-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_failed_conversion_keeps_the_recording_as_mp4() {
        let dir = scratch_dir("fail");
        let work = dir.join("123-4.mp4");
        std::fs::write(&work, b"video").unwrap();
        let recordings = dir.join("rec");
        let plan = GifPlan {
            work: work.clone(),
            dir: recordings.clone(),
            pattern: "clip".into(),
            facts: NameFacts::at(Local::now(), 0, 0),
        };
        // An ffmpeg that is not there fails the same way one that exits 1 does.
        let result = gif_worker(dir.join("no-ffmpeg.exe"), plan, &AtomicBool::new(false));
        match result {
            GifResult::Failed { kept: Some(path), .. } => {
                assert_eq!(path, recordings.join("clip.mp4"));
                assert_eq!(std::fs::read(&path).unwrap(), b"video");
                assert!(!work.exists(), "moved, not copied");
            }
            _ => panic!("expected the MP4 to be kept"),
        }
        assert!(!recordings.join("clip.gif").exists(), "no empty GIF under the name");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_work_name_is_the_target_plus_the_part_suffix() {
        assert_eq!(part_path(Path::new("/r/clip.gif")), PathBuf::from("/r/clip.gif.framefang-part"));
        assert_eq!(
            part_path(&Path::new("/w/1-2.mp4").with_extension("gif")),
            PathBuf::from("/w/1-2.gif.framefang-part")
        );
    }

    #[test]
    fn moving_a_file_replaces_the_claimed_empty_name() {
        let dir = scratch_dir("move");
        let from = dir.join("a.mp4");
        let to = dir.join("b.mp4");
        std::fs::write(&from, b"data").unwrap();
        std::fs::write(&to, b"").unwrap();
        move_file(&from, &to).unwrap();
        assert_eq!(std::fs::read(&to).unwrap(), b"data");
        assert!(!from.exists());
        assert!(move_file(&dir.join("missing.mp4"), &dir.join("c.mp4")).is_err());
        assert!(!dir.join("c.mp4").exists(), "no stray file after a failed move");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn keeping_a_recording_numbers_around_a_taken_name() {
        let dir = scratch_dir("keep");
        let facts = NameFacts::at(Local::now(), 0, 0);
        let rec = dir.join("rec");
        for round in 0..2 {
            let work = dir.join(format!("w{round}.mp4"));
            std::fs::write(&work, b"v").unwrap();
            let kept = keep_gif_mp4(&work, &rec, "clip", &facts).expect("kept");
            assert!(kept.exists());
        }
        assert_eq!(std::fs::read_dir(&rec).unwrap().count(), 2, "the second did not overwrite the first");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn part_files_in_the_recordings_folder_are_cleared() {
        let dir = scratch_dir("parts");
        std::fs::write(dir.join("keep.mp4"), b"x").unwrap();
        std::fs::write(dir.join("half.mp4.framefang-part"), b"x").unwrap();
        clear_part_files(&dir);
        assert!(dir.join("keep.mp4").exists());
        assert!(!dir.join("half.mp4.framefang-part").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
