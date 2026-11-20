<!-- markdownlint-disable MD013 MD033 MD041 -->
<!-- Centered HTML header and wide table rows are deliberate on GitHub. -->

<p align="center">
  <img src=".github/assets/social-preview.png" alt="Frame Fang: capture, annotate, paste" width="820">
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

Frame Fang sits quietly in the tray until you press its key. A screenshot
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
Frame Fang updates itself from the tray menu.

| Platform | Get it |
| --- | --- |
| **Windows** | **`Framefang.msi`** installer, or **`Framefang-portable.exe`** to run without installing |
| **macOS** | **`Framefang-macos-arm64.dmg`** (Apple silicon) or **`Framefang-macos-x64.dmg`** (Intel) |
| **Linux** | **`Framefang-linux-x86_64.AppImage`**, or the portable `Framefang-linux-x86_64-portable.tar.gz` / `Framefang-linux-arm64-portable.tar.gz` |

> [!NOTE]
> The macOS and Linux builds are new, so reports are welcome. The Mac app isn't
> signed by Apple yet: the first time, open it from **System Settings > Privacy
> & Security > Open Anyway**.

> [!TIP]
> On Windows 11, Print Screen opens the Snipping Tool by default. Frame Fang's
> Settings shows the exact switch to turn off, with a button that opens its
> page. Or bind any other key you like.

## Features

| Feature | What it does |
| --- | --- |
| **Region capture** | Drag over any part of any monitor, across mixed scaling and multiple screens. |
| **Window and screen capture** | Point at a window and it lights up; click to take just that window. The bar at the top switches to a whole screen or every screen. |
| **Full screen** | Bind a key and it saves the whole desktop to your screenshots folder. |
| **Annotate** | Pen, arrows, shapes, highlighter, text, numbered steps, pixelate, spotlight and a magnifier lens. |
| **Highlight text** | The Highlight tool reads the words in the picture (offline, like TEXT mode) and snaps to them: press on a word and drag to another, and every word between lights up, line by line, as when selecting text. No wobble, however the mouse moves. Six looks (a real chisel marker, a clean wash, a quick swipe under the words, an underline, a hand-drawn circle and a glow), in any color; on dark pages it lights the page instead of darkening it, so the words stay readable. |
| **Numbered steps** | Nine looks to pick from, previewed in the Step tool's panel: disc, ring, outline, square, halo, pin, hollow, pill and a terminal tag. Color the body, ring and digits separately, and turn the shadow on or off. Your choice is remembered. |
| **Crop** | Trim the picture in the editor; undo brings it back. |
| **Any color** | Palettes, a spectrum for any other color, and an eyedropper that takes one from the picture. |
| **Copy, save, pin** | Copy to the clipboard, save as PNG (or Save as PNG or JPEG), or pin the picture on top of other windows. |
| **Copy only** | Captures go straight to the clipboard, ready to paste into chat, email or docs. Or pick Copy + edit to have it copied and open the editor too. |
| **Capture last region** | Take the same rectangle again, from the tray or its own hotkey. |
| **Recent captures** | Your last 10 captures, copy-only ones too, wait in the tray menu. Click one to copy it again, Shift+click to edit it. Turn it off in Settings and they're deleted. |
| **Copy text from the screen** | TEXT mode reads the words in a region and copies them (offline, with the system's own text recognition). Works on pictures, videos and anything you can't select. |
| **Read QR codes** | QR mode reads a QR code on screen, light or dark, and copies what it holds: a link, a Wi-Fi password, an email address or plain text. Web links can open in your browser instead (Settings), and holding Ctrl as you let go does the other one (pick any key for this in Settings). |
| **Scrolling capture** | SCROLL mode follows you as you scroll a page, a document or a chat, and stitches it into one tall picture. Point at a window and the part that scrolls lights up; click it (or drag over any area), then scroll it yourself, down or up, at your own pace. A counter shows how tall it is; scroll too fast and it asks you to scroll back a little. Press Enter or click to finish, or Esc to cancel. Sticky headers show once and the scrollbar is left out. A page taller than the screen opens in a scrolling editor: the wheel scrolls it, a page map beside it shows the whole page with your drawings (click it to jump), and drawing near the top or bottom edge scrolls the page along. What happens after is its own choice in Settings (**After a scrolling capture**): Copy + edit unless you change it, so the editor opens even when quick captures only copy. Hold its own key (Shift unless you rebind it) as you click the part to do the other thing for that one capture. Windows only for now. |
| **File names** | Pick how saves are named (date, app, a counter), or write your own pattern in settings.json. Two captures in one second never overwrite each other. |
| **Border or shadow** | Every saved or copied screenshot can get a thin border or a soft drop shadow. |
| **Screen recorder** | Record the screen or a region to MP4, with system audio and microphone, or straight to GIF. |
| **Converters** | Images (PNG, JPG, WEBP, BMP) and videos (MP4, MOV, WEBM, MKV, GIF). |
| **Any hotkey** | Any key or chord, including Print Screen, F13 to F24, Pause and media keys. |
| **Color schemes** | Pink, Green, Cyan, Yellow or Orange, with a retro terminal look. |

> [!NOTE]
> Recording and video conversion need FFmpeg. Settings can download
> Frame Fang's own trimmed build, checked against its published checksum, or
> you can install it yourself:
>
> ```powershell
> winget install Gyan.FFmpeg
> ```
>
> Frame Fang finds it on its own (next to the app, then on `PATH`, then its
> downloaded copy). To use a particular one, pick it with Browse... on the
> FFmpeg row in Settings > Files; Auto goes back to finding it.

## Keyboard shortcuts

### Capturing

| Key | Does |
| --- | --- |
| Capture key | Capture a region (set it in Settings) |
| Click | Take the lit window (or screen) |
| `Tab` | Next mode: region or window, screen, all screens, TEXT, QR, SCROLL |
| `Ctrl+A` | The whole screen you're on (again for every screen) |
| Hold `Shift` | Flip between editor and copy only, once (the key can be changed in Settings) |
| Hold `Ctrl` in QR mode | Open a link instead of copying it, or the other way round (the key can be changed in Settings) |
| `Esc` | Cancel |

### In the editor

| Key | Does |
| --- | --- |
| `P` `L` `A` `R` `E` | Pen, line, arrow, box, circle |
| `M` `H` `T` `N` `B` | Highlighter, highlight text, text, numbered step, pixelate |
| `F` `Z` | Spotlight (dim everything else), magnify (a 2x to 4x lens) |
| `C` | Crop (in the editor window): drag a new rectangle or the crop's edges |
| `S` | Select and move what you drew |
| `Ctrl+Z` / `Ctrl+Y` | Undo / redo |
| `Ctrl+C` | Copy to the clipboard |
| `Ctrl+Shift+C` | Copy the picture's text (or Shift with the Copy button) |
| `Ctrl+S` or `Enter` | Save |
| `Ctrl+Shift+S` | Save as, with a name and PNG or JPEG |
| `Ctrl+P` | Pin on top of other windows |
| `Esc` | Put the tool down, then close |

## Coming from another tool?

Frame Fang covers the everyday screenshot loop of the **Windows Snipping
Tool**, **Snip & Sketch**, **ShareX**, **Greenshot**, **Lightshot** and
**Flameshot**: capture a region, mark it up, copy or save it. It keeps to that
loop and does it quickly, with no uploaders, accounts or workflows to set up.

## FAQ

<details>
<summary><b>On Windows 11, how do I make Print Screen open Frame Fang?</b></summary>

Open Frame Fang's Settings, tab **03 HOTKEYS**. While Print Screen is your
capture key, it shows the system switch to turn off (Bluetooth & devices >
Keyboard) and a button that opens that page.

</details>

<details>
<summary><b>Can I take a screenshot without the editor opening?</b></summary>

Yes. In Settings, tab **01 CAPTURES**, set **After a capture** to **Copy
only**. The region goes to
your clipboard as soon as you let go. Hold your switch key (Shift by default)
when you want the editor for that one capture.

</details>

<details>
<summary><b>Where are screenshots saved?</b></summary>

In your Pictures folder unless you pick another one in Settings, tab
**02 FILES**, where you also choose how they are named. Copy-only captures go
to the clipboard and are not saved to disk. `Ctrl+Shift+S` (or Shift with the
Save button) lets you pick the name, folder and format.

</details>

<details>
<summary><b>Can I write my own file-name pattern?</b></summary>

Yes. Pick **CUSTOM** under **File names**: settings.json opens with
`FileNamePattern`, which takes `{yyyy}` `{MM}` `{dd}` `{HH}` `{mm}` `{ss}`,
`{date}`, `{time}`, `{app}`, `{title}`, `{w}`, `{h}` and `{n}` (the first free
number). For example `{app} {date} {n}` saves `Chrome 2026-11-19 1.png`. A
change saved in the file applies within a second.

</details>

<details>
<summary><b>How do I copy text out of a picture or video?</b></summary>

Capture as usual, press `Tab` until the bar says **TEXT**, and drag over the
words. They are on your clipboard a moment later. Bind **Copy screen text** in
Settings, tab **03 HOTKEYS**, to open straight in TEXT mode. The reading
happens on your computer and nothing is uploaded. Windows reads in the
languages installed for your account. macOS 13 or later picks the language
itself. On Linux it needs `tesseract` (for example
`sudo apt install tesseract-ocr`).

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

Every release is built for Windows, macOS and Linux from this same code. The
macOS and Linux builds pass their tests in CI but are new on real machines, so
reports are welcome.

## License

[Apache License 2.0](LICENSE). Frame Fang began as a fork of
[Kashot](https://github.com/singhpratech/kashot) by Prateek Singh. See
[NOTICE](NOTICE) for what changed and
[THIRD_PARTY_LICENSES.md](THIRD_PARTY_LICENSES.md) for bundled components.
