# How Frame Fang is built

Frame Fang lives in the system tray. A hotkey or a tray click opens a capture
picker over every screen; the picked region opens in an annotation editor,
and from there it is copied, saved, pinned to the screen or recorded as
video. One Rust code base produces the Windows, macOS and Linux apps.

## The workspace

`framefang-rs/` is a Cargo workspace of three crates, each depending only on
the ones above it in this table:

| Crate | What it holds |
| --- | --- |
| `framefang-core` | Logic with no operating-system code: tools and annotations, hit-testing and the undo log, settings and their file, the shortcut model, screen geometry (DPI, virtual desktop, recording regions), text layout, install-channel detection. Tested on every OS. |
| `framefang-platform` | One wrapper per OS service: screen capture, global hotkeys (including the Wayland portal), the tray icon, the clipboard, recording, single-instance locking, child-process cleanup. |
| `framefang-app` | The `framefang` binary: the winit event loop, the capture editor, every window and dialog, the self-updater. |

```sh
cd framefang-rs
cargo test -p framefang-core          # the pure logic, no system packages needed
cargo test --workspace                # everything
cargo build --release --bin framefang
```

On Linux the build needs the development packages the release workflow
installs (`.github/workflows/release.yml`, the "Install system libraries"
step): Wayland, xkbcommon and the XCB libraries, GTK 3, D-Bus, Ayatana
AppIndicator, PipeWire and SPA (with clang for their bindings), GBM, udev,
xdo, OpenSSL and pkg-config. Windows and macOS need nothing beyond Rust.

## Releases

Pushing a `v*` tag runs `.github/workflows/release.yml`. It checks the tag
against the workspace version, builds on all three systems and attaches
each file to the GitHub release:

| File | For |
| --- | --- |
| `Framefang.msi` | Windows installer (WiX, `framefang-rs/wix/`) |
| `Framefang-portable.exe` | Windows without installing |
| `Framefang-macos-arm64.dmg`, `Framefang-macos-x64.dmg` | macOS (`.app` built by `dist/macos/make-app.sh`); not signed or notarized yet |
| `Framefang-linux-x86_64.AppImage` | Linux, one file (`dist/appimage/AppRun`) |
| `Framefang-linux-<x86_64/arm64>-portable.tar.gz` | Linux, the bare binary (a tarball keeps the executable bit) |
| `SHA256SUMS` | The checksums the in-app updater verifies |

A release needs the Windows job; a failed macOS or Linux job only leaves its
files out. Started by hand from the Actions tab, the workflow builds the same
files as artifacts and publishes nothing. The updater (`updates_form.rs`,
`self_updater.rs`) finds its download by these exact names, so renaming one
is an updater change too.

macOS gets no bare binary on purpose: a browser-downloaded Mach-O file is
quarantined and won't open from Finder, so the `.dmg` is the only download.

FFmpeg is never bundled. Recording and video conversion use the FFmpeg on
`PATH`; on Windows, when there is none, the app offers its own trimmed static
build (published by `.github/workflows/ffmpeg.yml`, fetched and checked by
`ffmpeg_fetch.rs`). A developer build can still put a local FFmpeg next to
the binary: `build.rs` copies the one named by `FRAMEFANG_FFMPEG`, or the one
on `PATH`.

## The app at run time

`main.rs` takes the single-instance lock, then hands control to `TrayLoop`
(`tray_loop.rs`), which owns the tray menu, the hotkeys and the life of every
window. Each window paints itself on the CPU: tiny-skia draws into a pixel
buffer, softbuffer puts it on screen, so nothing depends on a GPU driver.

| File (`framefang-app/src/`) | Job |
| --- | --- |
| `tray_loop.rs`, `tray_menu.rs` | Routing: hotkeys, tray menu, which window is open, what happens to a finished capture |
| `editor.rs` | The capture picker and annotation editor (selection, tools, flyouts, undo, Esc handling) |
| `painter.rs`, `palette.rs`, `chrome.rs`, `icons.rs` | Shared drawing: annotation rendering, the color schemes, window chrome, the tool glyphs |
| `settings_form.rs`, `about_form.rs`, `updates_form.rs`, `firstrun_form.rs` | The dialogs |
| `convert_image_form.rs`, `convert_video_form.rs` | Picture and video conversion (video through FFmpeg) |
| `recording_indicator.rs` | The floating REC window with its timer and Stop button |
| `pin.rs` | A capture pinned above other windows |
| `window_shape.rs`, `reveal.rs`, `backdrop.rs` | Windows cut to the shape of their panels, shown without the grow-from-a-corner flash |
| `self_updater.rs`, `install_source.rs`, `ffmpeg_fetch.rs` | Updating in place, knowing how this copy was installed, fetching FFmpeg |

### Things that cut across files

- **Hotkeys.** Region capture is bound by default; full-screen capture and
  record start/stop are optional. `framefang-core::hotkeys` names the actions,
  maps them to settings and finds conflicts; `framefang-platform::hotkey`
  registers them. On Windows a second, raw-input path (`raw_keys.rs`) catches
  presses that other programs' keyboard hooks would swallow, and presses seen
  by both paths count once. A binding is saved as a Win32 modifier mask plus
  virtual-key code on every OS, with key `0` meaning unbound.
- **Settings** are one `settings.json` in the config folder from
  `ProjectDirs::from("org", "framefang", "Framefang")`. Writes go through
  `framefang-core::atomic_file` (temp file, flush, rename), so a crash leaves
  the old file whole rather than a truncated one.
- **Screens and DPI.** `framefang-core::dpi` (`DisplayMap`) is the one place
  that converts between the units a monitor is reported in and the pixels its
  capture comes back in. They differ by OS (macOS reports points, X11 divides
  by `Xft.dpi / 96`, Windows reports device pixels), so the scale is measured
  per monitor as captured size over reported size instead of trusting the OS
  factor. `framefang-core::virtual_desktop` maps between the editor window,
  the stitched bitmap and screen coordinates; the editor is one window over
  the union of all monitors so a drag can cross from one screen to the next.
- **Text.** User text is laid out by `framefang-core::text` with the bundled
  Noto Sans (`framefang-core/assets/fonts/`, OFL) through ab_glyph, so it is the
  same on every OS; glyphs the font lacks draw as a visible box. The small
  fixed labels in the app's own chrome use the 5x7 pixel font in
  `bitmap_font.rs`.
- **Recording.** `framefang-platform::recorder` drives FFmpeg: `gdigrab` plus
  DirectShow microphone on Windows, with system audio captured by WASAPI
  loopback and sent to FFmpeg over a local TCP socket; `x11grab` with Pulse
  inputs on X11; `screencapture` or AVFoundation on macOS, with system audio
  from ScreenCaptureKit over the same TCP route. A picked region is clamped to
  the desktop and rounded to even sizes for H.264 by `framefang-core::region`.
  `Recorder::start` returns the options that actually took effect, and
  messages are built from that, not from what was asked for.
- **No orphaned FFmpeg.** `framefang-platform::child_guard` ties the encoder to
  the app (a kill-on-close Job Object on Windows, `PR_SET_PDEATHSIG` on Linux, a
  small watchdog on macOS) and records its pid; the next start only kills a
  leftover pid after checking it still runs a recorder.
- **One instance.** `framefang-platform::instance` holds a lock file in the
  config folder. A second launch leaves a `capture.request` file and exits;
  the running copy picks it up and opens the picker. The updater's relaunch is
  the one allowed overlap.
- **Failures people see** (a save, copy, pin or settings write that fails)
  become tray notifications whose wording, built in `framefang-core::failure`,
  names the path and the OS reason.

### Linux: X11 and Wayland

A Wayland session withholds what X11 hands out freely: a global key grab, a
readable screen and a screen another process may record. Frame Fang checks
the session at run time (`framefang_platform::session::is_wayland()`: the
`XDG_SESSION_TYPE`, or a non-empty `WAYLAND_DISPLAY`), so one binary serves
both.

| | X11 | Wayland |
| --- | --- | --- |
| Global hotkey | `global-hotkey` (key grab) | GlobalShortcuts portal (`hotkey_portal.rs`) |
| Screenshot | `xcap` | `xcap`, falling back to the Screenshot portal (`capture_portal.rs`) |
| Recording video | FFmpeg `x11grab` | ScreenCast portal, PipeWire frames written to FFmpeg's stdin (`recorder_wayland.rs`) |
| Recording audio | Pulse inputs | The same Pulse inputs, served by PipeWire |

- The X11 key grab is never a Wayland fallback: XWayland accepts the grab and
  the key then never fires, which is worse than an error. Without the portal,
  hotkey setup fails with a message the tray shows once.
- On Wayland the desktop owns the key. Frame Fang asks for one; the desktop
  decides and lists it in its own keyboard settings.
- On Wayland the frames use FFmpeg's stdin, so stopping means closing that
  pipe instead of sending `q`. The writer sends a steady 30 frames a second,
  repeating the last frame when nothing changed, because raw video carries no
  timestamps; and a cast only counts as started once a real frame has
  arrived, so a broken session fails before any file exists.

## Editor keys

With a region picked, single letters pick tools: P pen, L line, A arrow,
R rectangle, E ellipse, M marker, T text, N numbered step, B pixelate. S turns
select mode on and off: clicks then pick the topmost annotation under the
pointer to drag or delete, and any tool key leaves it. Rectangles and
ellipses are picked by their outline only, so a click inside a large frame
reaches what is under it.

| Keys | Action |
| --- | --- |
| Ctrl+A | Pick the whole screen under the pointer |
| Enter, Ctrl+S | Save |
| Ctrl+Shift+S | Save under a name and format you choose |
| Ctrl+C | Copy |
| Ctrl+P | Pin to the screen |
| Ctrl+Z / Ctrl+Y or Ctrl+Shift+Z | Undo / redo (adds, moves, deletes, clears) |
| Delete, Backspace | In select mode, remove the picked annotation |
| Shift+Enter | New line in a text annotation (Enter finishes it) |
| Esc | Back out one step at a time: the stroke being drawn, the select-mode highlight, select mode, the tool in hand, then the region; when that would throw away ink it asks first |
