# Framefang

A pure Rust screenshot tool: fast, tray-resident, for Windows, with Linux and
macOS builds from the same code.

Press Print Screen, drag over what you want, and the picture opens in a small
editor right where it was on your screen, with the rest of the desktop live
again around it.

## Install

Windows: download `Framefang.msi` from the
[latest release](https://github.com/winters27/framefang/releases/latest) and
run it. Framefang starts in the tray and checks for updates from its About
and Updates windows.

Screen recording and video conversion use FFmpeg, which is not bundled. If
you want them, install it once, for example:

```powershell
winget install Gyan.FFmpeg
```

Windows 11 sends Print Screen to the Snipping Tool by default. Framefang's
Settings has a button that opens the Windows page where you hand the key
over.

## What it does

- **Freeze and pick:** the screen freezes the moment you press the key, so
  menus and tooltips you were hovering stay in the shot.
- **Edit in place:** the picture floats where you took it, with its tools
  beside it and nothing covering the rest of your screen. With no tool
  picked, drag the picture to move it.
- **Tools that remember:** pen, line, arrow, box, circle, highlighter, text,
  numbered steps and pixelate. Click a tool and its size and colors slide out
  beside it; each tool keeps its own.
- **Copy, save or pin:** copy to the clipboard (optionally automatically on
  every capture), save to a folder, or pin the picture on top of other
  windows.
- **Recording:** record the screen or a region to MP4 through FFmpeg.
- **Color schemes:** Pink, Green, Cyan or Yellow for the app's windows.

## Building

Rust 1.80 or later.

```bash
cd framefang-rs
cargo test --release --workspace
cargo build --release -p framefang-app
```

The app is `framefang-rs/target/release/framefang.exe` (`framefang` on Linux
and macOS). A local build copies the `ffmpeg` on `PATH` (or the one
`FRAMEFANG_FFMPEG` names) next to the app.

Releases are built by GitHub Actions (`.github/workflows/release.yml`) when a
`v*` tag is pushed: the MSI, a portable zip and `SHA256SUMS`.
