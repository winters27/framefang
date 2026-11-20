//! Where captures and recordings go on disk, and how they get there.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{anyhow, Result};
use chrono::Local;
use framefang_core::failure::{save_folder_failure, save_write_failure};
use framefang_core::file_name::{self, NameFacts};
use framefang_core::AppSettings;
use framefang_platform::windows_list::{self, OnScreenWindow};
use image::RgbaImage;

use super::gif_jobs::PART_SUFFIX;

/// The folder named in a setting, if it is set and exists.
fn existing_folder(setting: &str) -> Option<PathBuf> {
    if setting.is_empty() {
        return None;
    }
    Some(PathBuf::from(setting)).filter(|p| p.is_dir())
}

/// Where screenshots are saved: the folder from Settings when it exists, else
/// the Pictures folder, else the temp folder.
pub(super) fn save_directory(s: &AppSettings) -> PathBuf {
    existing_folder(&s.save_directory)
        .or_else(|| directories::UserDirs::new().and_then(|u| u.picture_dir().map(Path::to_path_buf)))
        .unwrap_or_else(std::env::temp_dir)
}

/// Where recordings land: the folder from Settings when it exists, else the
/// Videos folder, then the home folder, then the temp folder. Kept apart from
/// `save_directory` because Pictures is the wrong home for video clips on
/// every desktop.
pub(super) fn recordings_directory_for(s: &AppSettings) -> PathBuf {
    let user = directories::UserDirs::new();
    existing_folder(&s.recordings_directory)
        .or_else(|| user.as_ref().and_then(|u| u.video_dir().map(Path::to_path_buf)))
        .or_else(|| user.as_ref().map(|u| u.home_dir().to_path_buf()))
        .unwrap_or_else(std::env::temp_dir)
}

/// Whether a file in the recordings folder is a finished clip: an MP4 or a
/// GIF, and not a half-copied work file.
fn is_finished_clip(path: &Path) -> bool {
    let is_video = path
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("mp4") || ext.eq_ignore_ascii_case("gif"));
    let is_work_file = path
        .file_name()
        .is_some_and(|name| name.to_string_lossy().contains(PART_SUFFIX));
    is_video && !is_work_file
}

/// The most recently modified clip in the recordings folder. It backs
/// "Annotate last recording" after a restart, when the app no longer
/// remembers which clip that was. On equal times the first one listed wins.
pub(super) fn newest_recording(dir: &Path) -> Option<PathBuf> {
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if !is_finished_clip(&path) {
                return None;
            }
            let modified: SystemTime = entry.metadata().and_then(|m| m.modified()).ok()?;
            Some((modified, path))
        })
        .reduce(|best, next| if next.0 > best.0 { next } else { best })
        .map(|(_, path)| path)
}

/// Encode `img` as PNG into an already-claimed file. The writer is flushed by
/// hand because a dropped `BufWriter` swallows the last write's error, and a
/// full disk must not read as saved.
fn write_png(img: &RgbaImage, file: File) -> std::result::Result<(), String> {
    let mut out = BufWriter::new(file);
    img.write_with_encoder(image::codecs::png::PngEncoder::new(&mut out))
        .map_err(|e| e.to_string())?;
    out.flush().map_err(|e| e.to_string())
}

/// Save `bmp`, framed as Settings says, into the screenshots folder under a
/// fresh name from the file-name pattern (never over an existing file).
/// Returns the path, plus the folder when Settings had none yet so the caller
/// can remember it.
pub(super) fn save_capture(
    settings: &AppSettings,
    bmp: &RgbaImage,
    facts: &NameFacts,
) -> Result<(PathBuf, Option<PathBuf>)> {
    let folder = save_directory(settings);
    let folder_text = folder.display().to_string();
    // The text of these errors goes to the user in a toast, so each one names
    // the path and the OS's reason: "save failed" alone gives nothing to act on.
    std::fs::create_dir_all(&folder)
        .map_err(|e| anyhow!(save_folder_failure(&folder_text, &e.to_string())))?;
    let framed = crate::frame::apply(bmp, settings.output_frame());
    let (path, file) = file_name::claim(&folder, &settings.file_name_pattern, facts, "png")
        .map_err(|e| anyhow!(save_write_failure(&folder_text, &e.to_string())))?;
    if let Err(reason) = write_png(&framed, file) {
        // A claimed name holding half a picture is worse than no file.
        let _ = std::fs::remove_file(&path);
        return Err(anyhow!(save_write_failure(&path.display().to_string(), &reason)));
    }
    Ok((path, settings.save_directory.is_empty().then_some(folder)))
}

/// Whether the point lies inside the `(x, y, w, h)` rectangle.
fn rect_holds(rect: (i32, i32, i32, i32), x: i32, y: i32) -> bool {
    let (left, top, w, h) = rect;
    x >= left && x < left + w && y >= top && y < top + h
}

/// The app and window title for a region of the screen (virtual-screen
/// pixels): the frontmost window under the region's middle, or two empty
/// strings over the bare desktop. A region more than half again the window's
/// area spans more than that one app, so it names none.
pub(super) fn facts_at(windows: &[OnScreenWindow], region: (i32, i32, i32, i32)) -> (String, String) {
    let (w, h) = (region.2 as i64, region.3 as i64);
    let middle = (region.0 + region.2 / 2, region.1 + region.3 / 2);
    let under = windows.iter().find(|win| rect_holds(win.rect, middle.0, middle.1));
    match under {
        Some(win) if w * h * 2 <= win.rect.2 as i64 * win.rect.3 as i64 * 3 => {
            (windows_list::app_name(win.pid), win.title.clone())
        }
        _ => (String::new(), String::new()),
    }
}

/// What the name of a screenshot is built from, as of now.
pub(super) fn name_facts(bmp: &RgbaImage) -> NameFacts {
    NameFacts::at(Local::now(), bmp.width(), bmp.height())
}

/// The error for a picture that could not be written to `path`.
fn write_failure(path: &Path, reason: &dyn std::fmt::Display) -> anyhow::Error {
    anyhow!(save_write_failure(&path.display().to_string(), &reason.to_string()))
}

/// Write a JPEG at quality 92. JPEG has no transparency, so the picture is
/// laid on white first (a shadow's clear margin would turn black otherwise).
fn write_jpeg(path: &Path, bmp: &RgbaImage) -> Result<()> {
    let file = File::create(path).map_err(|e| write_failure(path, &e))?;
    let rgb = image::DynamicImage::ImageRgba8(crate::frame::on_white(bmp)).to_rgb8();
    image::codecs::jpeg::JpegEncoder::new_with_quality(BufWriter::new(file), 92)
        .encode_image(&rgb)
        .map_err(|e| write_failure(path, &e))
}

/// Write `bmp` where Save As pointed, in the format its extension names: JPEG
/// for `.jpg` and `.jpeg`, PNG for `.png`, and PNG with `.png` appended for a
/// name with no image extension. Returns the path actually written.
pub(super) fn write_image(path: PathBuf, bmp: &RgbaImage) -> Result<PathBuf> {
    let ext = path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase);
    let target = match ext.as_deref() {
        Some("jpg" | "jpeg") => {
            write_jpeg(&path, bmp)?;
            return Ok(path);
        }
        Some("png") => path,
        _ => {
            let mut named = path.into_os_string();
            named.push(".png");
            PathBuf::from(named)
        }
    };
    bmp.save(&target).map_err(|e| write_failure(&target, &e))?;
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ff-files-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn window(rect: (i32, i32, i32, i32), title: &str) -> OnScreenWindow {
        OnScreenWindow { rect, title: title.into(), pid: 0, hwnd: 0 }
    }

    #[test]
    fn a_region_is_named_for_the_window_under_it_but_a_screen_is_not() {
        let windows = vec![window((100, 100, 800, 600), "Notes"), window((0, 0, 1920, 1080), "Back")];
        assert_eq!(facts_at(&windows, (200, 200, 300, 200)).1, "Notes", "frontmost window under the middle");
        assert_eq!(facts_at(&windows, (1500, 900, 100, 100)).1, "Back");
        assert_eq!(facts_at(&windows, (-500, -500, 50, 50)), (String::new(), String::new()), "over nothing");
        // Centred in "Back" but half again its size: no one window's.
        assert_eq!(facts_at(&windows, (0, 0, 3600, 1000)).1, "", "much bigger than the window");
        assert_eq!(facts_at(&windows, (0, 0, 1900, 1000)).1, "Back");
    }

    #[test]
    fn a_rectangle_holds_its_top_left_but_not_its_far_edge() {
        assert!(rect_holds((10, 10, 5, 5), 10, 10));
        assert!(rect_holds((10, 10, 5, 5), 14, 14));
        assert!(!rect_holds((10, 10, 5, 5), 15, 10));
        assert!(!rect_holds((10, 10, 5, 5), 10, 15));
        assert!(!rect_holds((10, 10, 5, 5), 9, 12));
    }

    #[test]
    fn jpeg_lays_transparency_on_white() {
        let dir = scratch_dir("jpeg");
        let clear = image::ImageBuffer::from_pixel(8, 8, image::Rgba([0u8, 0, 0, 0]));
        let path = write_image(dir.join("x.jpg"), &clear).unwrap();
        let back = image::open(&path).unwrap().to_rgb8();
        assert!(back.get_pixel(4, 4)[0] > 240, "white, not black: {:?}", back.get_pixel(4, 4));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_name_without_an_image_extension_gets_png() {
        let dir = scratch_dir("noext");
        let pic = image::ImageBuffer::from_pixel(4, 4, image::Rgba([10u8, 20, 30, 255]));
        let written = write_image(dir.join("shot"), &pic).unwrap();
        assert_eq!(written, dir.join("shot.png"));
        assert!(image::open(&written).is_ok());
        let upper = write_image(dir.join("TWO.PNG"), &pic).unwrap();
        assert_eq!(upper, dir.join("TWO.PNG"), "a known extension is kept as typed");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_newest_clip_wins_and_work_files_and_pictures_do_not() {
        let dir = scratch_dir("newest");
        let first = dir.join("a.mp4");
        let second = dir.join("b.mp4");
        std::fs::write(&first, b"1").unwrap();
        std::fs::write(dir.join("c.png"), b"1").unwrap();
        // A strictly later modification time for the second clip.
        std::thread::sleep(std::time::Duration::from_millis(30));
        std::fs::write(&second, b"2").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(30));
        std::fs::write(dir.join("d.mp4.framefang-part"), b"3").unwrap();
        assert_eq!(newest_recording(&dir), Some(second));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_empty_or_missing_folder_has_no_newest_clip() {
        let dir = scratch_dir("empty");
        assert_eq!(newest_recording(&dir), None);
        assert_eq!(newest_recording(&dir.join("not-here")), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_gif_counts_as_a_clip() {
        assert!(is_finished_clip(Path::new("clip.GIF")));
        assert!(is_finished_clip(Path::new("clip.mp4")));
        assert!(!is_finished_clip(Path::new("clip.png")));
        assert!(!is_finished_clip(Path::new("clip.gif.framefang-part")));
    }

    #[test]
    fn a_setting_only_counts_when_it_names_a_real_folder() {
        let dir = scratch_dir("setting");
        assert_eq!(existing_folder(""), None);
        assert_eq!(existing_folder(&dir.join("nope").to_string_lossy()), None);
        assert_eq!(existing_folder(&dir.to_string_lossy()), Some(dir.clone()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_saved_capture_takes_a_new_name_and_reports_a_new_folder() {
        let dir = scratch_dir("save");
        let mut settings = AppSettings::default();
        settings.save_directory = dir.to_string_lossy().into_owned();
        settings.file_name_pattern = "shot".into();
        let pic = image::ImageBuffer::from_pixel(6, 6, image::Rgba([1u8, 2, 3, 255]));
        let (path, new_folder) = save_capture(&settings, &pic, &name_facts(&pic)).unwrap();
        assert_eq!(path, dir.join("shot.png"));
        assert!(new_folder.is_none(), "the folder was already in Settings");
        assert!(image::open(&path).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
