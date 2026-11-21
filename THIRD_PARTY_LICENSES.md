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

The rasterizer itself is the pure-Rust **ab_glyph** crate (`Apache-2.0`, with
**ab_glyph_rasterizer** and **owned_ttf_parser**, both `Apache-2.0`, and
**ttf-parser**, `MIT OR Apache-2.0`), with grapheme segmentation from
**unicode-segmentation** (`MIT OR Apache-2.0`). All are ordinary Cargo
dependencies under permissive licenses, compiled into the binary; none of them
needs a system font stack, so the Text tool behaves the same on all three
platforms.

## rqrr (QR code reader)

QR mode reads QR codes with the pure-Rust **rqrr** crate, compiled into the
binary.

- Project: <https://github.com/WanzenBug/rqrr>
- License: `(MIT OR Apache-2.0) AND ISC`. The ISC part covers the code it is
  based on, **quirc** by Daniel Beer, whose notice follows.

```text
quirc -- QR-code recognition library
Copyright (C) 2010-2012 Daniel Beer <dlbeer@gmail.com>

ISC License
===========

Permission to use, copy, modify, and/or distribute this software for
any purpose with or without fee is hereby granted, provided that the
above copyright notice and this permission notice appear in all
copies.

THE SOFTWARE IS PROVIDED "AS IS" AND THE AUTHOR DISCLAIMS ALL
WARRANTIES WITH REGARD TO THIS SOFTWARE INCLUDING ALL IMPLIED
WARRANTIES OF MERCHANTABILITY AND FITNESS. IN NO EVENT SHALL THE
AUTHOR BE LIABLE FOR ANY SPECIAL, DIRECT, INDIRECT, OR CONSEQUENTIAL
DAMAGES OR ANY DAMAGES WHATSOEVER RESULTING FROM LOSS OF USE, DATA OR
PROFITS, WHETHER IN AN ACTION OF CONTRACT, NEGLIGENCE OR OTHER
TORTIOUS ACTION, ARISING OUT OF OR IN CONNECTION WITH THE USE OR
PERFORMANCE OF THIS SOFTWARE.
```

## Text recognition (TEXT mode)

TEXT mode uses the text recognition the operating system already has. Nothing
is bundled and nothing leaves the computer.

- **Windows:** `Windows.Media.Ocr`, part of Windows.
- **macOS:** the Vision framework, part of macOS.
- **Linux:** **Tesseract** (<https://github.com/tesseract-ocr/tesseract>,
  Apache-2.0), run as a separate process when the user has installed it. It
  is not bundled.

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
