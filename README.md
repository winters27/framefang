<!-- markdownlint-disable MD013 MD033 MD041 -->
<!-- Centered HTML header and wide table rows are deliberate on GitHub. -->

<p align="center">
  <img src=".github/assets/social-preview.png" alt="Framefang: capture, annotate, paste" width="820">
</p>

<p align="center">
  <a href="https://github.com/winters27/framefang/releases/latest"><img src="https://img.shields.io/github/v/release/winters27/framefang?color=FC0C7A&label=release" alt="Latest release"></a>
  <a href="https://github.com/winters27/framefang/releases"><img src="https://img.shields.io/github/downloads/winters27/framefang/total?color=FC0C7A&label=downloads" alt="Downloads"></a>
  <img src="https://img.shields.io/badge/built%20with-Rust-0B090C?logo=rust" alt="Built with Rust">
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-Apache%202.0-0B090C" alt="Apache 2.0 license"></a>
</p>

<h3 align="center">A snappy screenshot and annotation tool, built in pure Rust.</h3>

<p align="center">
  Press a key, drag over what you want, paste it anywhere.
</p>

<p align="center">
  <a href="#download">Download</a> &nbsp;·&nbsp;
  <a href="#features">Features</a> &nbsp;·&nbsp;
  <a href="#keyboard-shortcuts">Shortcuts</a> &nbsp;·&nbsp;
  <a href="#faq">FAQ</a> &nbsp;·&nbsp;
  <a href="#building-from-source">Build</a>
</p>

## Highlights

Framefang sits quietly in the tray until you press its key. A screenshot
takes about as long as dragging a box.

- **Instant freeze.** The screen freezes the moment you press the key, so
  open menus and tooltips stay in the shot.
- **Copy only, or edit.** Send the region straight to the clipboard, or open
  the editor. Hold Shift for the other.
- **Edit in place.** The editor opens right where the picture was, with the
  rest of your desktop live around it.
- **Private.** Nothing is uploaded. It only goes online for updates and FFmpeg.

## Download

Grab it from the [latest release](https://github.com/winters27/framefang/releases/latest).
Framefang updates itself from the tray menu.

| Platform | Get it |
| --- | --- |
| **Windows** | **`Framefang.msi`** installer, or the portable `framefang-windows-x86_64.zip` |
| **macOS, Linux** | [Build from source](#building-from-source) for now |

> [!TIP]
> On Windows 11, Print Screen opens the Snipping Tool by default. Framefang's
> Settings shows the exact switch to turn off, with a button that opens its
> page. Or bind any other key you like.

## Features

| Feature | What it does |
| --- | --- |
| **Region capture** | Drag over any part of any monitor, across mixed scaling and multiple screens. |
| **Full screen** | Bind a key and it saves the whole desktop to your screenshots folder. |
| **Annotate** | Pen, arrows, shapes, highlighter, text, numbered steps and pixelate. |
| **Copy, save, pin** | Copy to the clipboard, save as PNG, or pin the picture on top of other windows. |
| **Copy only** | Captures go straight to the clipboard, ready to paste into chat, email or docs. |
| **Screen recorder** | Record the screen or a region to MP4, with system audio and microphone. |
| **Converters** | Images (PNG, JPG, WEBP, BMP) and videos (MP4, MOV, WEBM, MKV, GIF). |
| **Any hotkey** | Any key or chord, including Print Screen, F13 to F24, Pause and media keys. |
| **Color schemes** | Pink, Green, Cyan or Yellow, with a retro terminal look. |

> [!NOTE]
> Recording and video conversion need FFmpeg. Settings can download
> Framefang's own trimmed build, checked against its published checksum, or
> you can install it yourself:
>
> ```powershell
> winget install Gyan.FFmpeg
> ```

## Keyboard shortcuts

### Capturing

| Key | Does |
| --- | --- |
| Capture key | Capture a region (set it in Settings) |
| Hold `Shift` | Flip between editor and copy only, once |
| `Esc` | Cancel |

### In the editor

| Key | Does |
| --- | --- |
| `P` `L` `A` `R` `E` | Pen, line, arrow, box, circle |
| `M` `T` `N` `B` | Highlighter, text, numbered step, pixelate |
| `S` | Select and move what you drew |
| `Ctrl+Z` / `Ctrl+Y` | Undo / redo |
| `Ctrl+C` | Copy to the clipboard |
| `Ctrl+S` or `Enter` | Save |
| `Ctrl+P` | Pin on top of other windows |
| `Esc` | Put the tool down, then close |

## Coming from another tool?

Framefang covers the everyday screenshot loop of the **Windows Snipping
Tool**, **Snip & Sketch**, **ShareX**, **Greenshot**, **Lightshot** and
**Flameshot**: capture a region, mark it up, copy or save it. It keeps to that
loop and does it quickly, with no uploaders, accounts or workflows to set up.

## FAQ

<details>
<summary><b>On Windows 11, how do I make Print Screen open Framefang?</b></summary>

Open Framefang's Settings. While Print Screen is your capture key, it shows
the system switch to turn off (Bluetooth & devices > Keyboard) and a button
that opens that page.

</details>

<details>
<summary><b>Can I take a screenshot without the editor opening?</b></summary>

Yes. In Settings, set **After a capture** to **Copy only**. The region goes to
your clipboard as soon as you let go. Hold your switch key (Shift by default)
when you want the editor for that one capture.

</details>

<details>
<summary><b>Where are screenshots saved?</b></summary>

In your Pictures folder unless you pick another one in Settings. Copy-only
captures go to the clipboard and are not saved to disk.

</details>

<details>
<summary><b>Does it work with multiple monitors and high-DPI screens?</b></summary>

Yes. The capture spans every monitor and maps your selection back to real
pixels on scaled displays.

</details>

## Building from source

Needs Rust 1.80 or later.

```bash
cd framefang-rs
cargo test --release --workspace
cargo build --release -p framefang-app
```

The app lands in `framefang-rs/target/release/`. A local build copies the
`ffmpeg` on `PATH` (or the one `FRAMEFANG_FFMPEG` names) next to it.
Releases are built by GitHub Actions when a `v*` tag is pushed.

The macOS and Linux builds share the same code but are not tested yet, so
reports are welcome.

## License

[Apache License 2.0](LICENSE). Framefang began as a fork of
[Kashot](https://github.com/singhpratech/kashot) by Prateek Singh. See
[NOTICE](NOTICE) for what changed and
[THIRD_PARTY_LICENSES.md](THIRD_PARTY_LICENSES.md) for bundled components.
