/*!
Copies an ffmpeg binary next to the built `framefang` executable, so the
Convert video dialog works without the user installing ffmpeg.

Where the ffmpeg comes from, first match wins:

1. `FRAMEFANG_FFMPEG`, an explicit path. Release builds set this to a static
   ffmpeg, because the shipped copy has to run on machines that have no
   libavcodec.
2. `ffmpeg` (`ffmpeg.exe` on Windows) on the build machine's `PATH`, which is
   fine for local development.

The copy goes into `target/<profile>/`, beside the executable, and the app's
ffmpeg lookup in convert_video_form.rs checks there first, so nothing else
needs wiring up.

Finding no ffmpeg is only a warning. At run time the app still uses one on
`PATH`, which most Linux desktops and Homebrew Macs have, and on Windows the
dialog explains what is missing.
*/

use std::{
    env, fs,
    path::{Path, PathBuf},
    time::SystemTime,
};

/// The ffmpeg file name on the machine running the build.
const FFMPEG_NAME: &str = if cfg!(target_os = "windows") { "ffmpeg.exe" } else { "ffmpeg" };

/// What makes cargo run this script again: a changed override (so a CI
/// switch takes effect) or an edit to the script itself.
const RERUN_WHEN: [&str; 2] = ["rerun-if-env-changed=FRAMEFANG_FFMPEG", "rerun-if-changed=build.rs"];

fn main() {
    for directive in RERUN_WHEN {
        println!("cargo:{directive}");
    }
    if let Some(note) = bundle_ffmpeg() {
        println!("cargo:warning={note}");
    }
}

/// Do the copy. Returns the note to show in the build output, or `None` when
/// the bundled copy is already current and there is nothing to say.
fn bundle_ffmpeg() -> Option<String> {
    let Some(source) = ffmpeg_source() else {
        return Some(
            "ffmpeg not found at build time. Convert-video will fall back to runtime PATH lookup. \
             Set FRAMEFANG_FFMPEG=/path/to/ffmpeg (preferably static) to bundle."
                .to_owned(),
        );
    };
    if !source.is_file() {
        return Some(format!("FRAMEFANG_FFMPEG points at a non-file: {}", source.display()));
    }
    let Some(profile_dir) = profile_dir() else {
        return Some(
            "Could not locate cargo target/<profile>/ to bundle ffmpeg into. Skipping bundle.".to_owned(),
        );
    };

    let dest = profile_dir.join(FFMPEG_NAME);
    // Leave an up-to-date copy alone so incremental builds stay quiet.
    if dest.is_file() && !source_is_newer(&source, &dest) {
        return None;
    }
    if let Err(e) = fs::copy(&source, &dest) {
        return Some(format!("Failed to copy ffmpeg {} → {}: {e}", source.display(), dest.display()));
    }
    mark_executable(&dest);
    Some(format!("Bundled ffmpeg: {} -> {}", source.display(), dest.display()))
}

/// Give the copy `rwxr-xr-x` on Unix; a failure here only shows up later as
/// an ffmpeg that will not start, so it is not worth failing the build over.
/// Windows needs nothing.
fn mark_executable(file: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(file, fs::Permissions::from_mode(0o755)).ok();
    }
    #[cfg(not(unix))]
    let _ = file;
}

/// `FRAMEFANG_FFMPEG` if it is set (checked by the caller, since it may name
/// something that is not a file), otherwise the first ffmpeg on `PATH`.
fn ffmpeg_source() -> Option<PathBuf> {
    if let Ok(explicit) = env::var("FRAMEFANG_FFMPEG") {
        return Some(PathBuf::from(explicit));
    }
    let separator = if cfg!(target_os = "windows") { ';' } else { ':' };
    env::var("PATH")
        .ok()?
        .split(separator)
        .map(|dir| Path::new(dir).join(FFMPEG_NAME))
        .find(|candidate| candidate.is_file())
}

/// `target/<profile>/`. Cargo gives build scripts an `OUT_DIR` of
/// `target/<profile>/build/<package>-<hash>/out`, three levels below it.
fn profile_dir() -> Option<PathBuf> {
    let out_dir = PathBuf::from(env::var("OUT_DIR").ok()?);
    let profile = out_dir.ancestors().nth(3)?;
    profile.is_dir().then(|| profile.to_path_buf())
}

/// True unless both files have readable times and `source` is no newer.
fn source_is_newer(source: &Path, dest: &Path) -> bool {
    match (modified(source), modified(dest)) {
        (Some(src), Some(dst)) => src > dst,
        _ => true,
    }
}

fn modified(path: &Path) -> Option<SystemTime> {
    fs::metadata(path).and_then(|meta| meta.modified()).ok()
}
