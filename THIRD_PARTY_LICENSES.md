# Third-party components

Framefang itself is licensed under **Apache-2.0** (see `LICENSE`). It bundles or
invokes the following third-party software.

## Noto Sans (bundled font)

Framefang compiles **Noto Sans** (Regular, static hinted TTF) into the binary and
rasterizes it at runtime for the annotation **Text** tool, so typed text renders
identically on Windows, Linux and macOS without depending on any system font.

- File: `framefang-rs/crates/framefang-core/assets/fonts/NotoSans-Regular.ttf`
- Project: <https://github.com/notofonts/latin-greek-cyrillic>
- License: **SIL Open Font License, Version 1.1 (OFL-1.1)** — full text ships
  next to the font at `framefang-rs/crates/framefang-core/assets/fonts/OFL.txt`
- Copyright 2022 The Noto Project Authors
- The font is used unmodified and is not sold separately, as OFL-1.1 requires.

The rasterizer itself is the pure-Rust **fontdue** crate (`MIT OR Apache-2.0 OR
Zlib`, pulling in **ttf-parser**, `MIT OR Apache-2.0`), with grapheme
segmentation from **unicode-segmentation** (`MIT OR Apache-2.0`). All three are
ordinary Cargo dependencies under permissive licenses, compiled into the
binary; none of them needs a system font stack, so the Text tool behaves the
same on all three platforms.

## FFmpeg

Framefang calls **FFmpeg** as a **separate process** (subprocess) for:

- screen recording (encoding captured video/audio to MP4 / H.264 / AAC), and
- the in-app video / image format conversion dialogs.

FFmpeg is **not linked** into the Framefang executable — it is a standalone
binary launched via the operating system. Under the GPL this is "mere
aggregation": Framefang remains licensed under Apache-2.0.

- Project: <https://ffmpeg.org>
- **Official releases do not bundle FFmpeg.** The installer and the portable
  zip contain only Framefang; recording and conversion use the FFmpeg the user
  installed (the one next to the app, else the one on `PATH`).
- **Local builds:** `framefang-app/build.rs` copies the FFmpeg named by the
  `FRAMEFANG_FFMPEG` environment variable, or else the one on `PATH`, next to
  the built executable, for convenience.
- Typical FFmpeg builds include GPL-licensed components (notably **libx264**
  for H.264 video encoding) and are distributed under the **GNU General Public
  License, version 2 or later (GPL-2.0-or-later)**; license text:
  <https://www.gnu.org/licenses/gpl-2.0.html>.
- **Corresponding source** (GPLv2 §3): anyone who redistributes a Framefang
  build *with* FFmpeg next to it must also offer that FFmpeg's corresponding
  source (for example the matching `ffmpeg-<version>.tar.xz` from
  <https://ffmpeg.org/releases/>).

We gratefully credit the FFmpeg project and its contributors.
