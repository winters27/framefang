Frame Fang icons

A white fang-shaped F with a soft glow, in a hot pink capture frame, on a
clear background (since 2026-11-19; before that, white fangs on a black
rounded tile).

The logo is vector. Everything else here is rendered from it.

- framefang.svg: the logo, glow and all. Used above 96 px.
- framefang-medium.svg: 33 to 96 px. The same glow plus a light dark edge
  round the F, which the glow alone is too faint to give on a light
  background.
- framefang-small.svg: 16 to 32 px (taskbar, tray, title bar). No blur: a
  thin dark edge keeps the white F visible on light taskbars, the F is a
  little larger and the brackets thicker.
- framefang-icon-master.png: 1024x1024 render of framefang.svg.
- Framefang.ico: Windows ICO with 16, 20, 24, 32, 40, 48, 64, 96, 128, 256 px.
  framefang-rs/wix/Framefang.ico is a copy for the MSI.
- windows_png/: the same sizes as separate PNGs.
- linux_hicolor/: freedesktop hicolor sizes. The app embeds three of them:
  256x256 for its windows, 64x64 for the tray icon, 16x16 for the title bar.
- Framefang.icns: the finished macOS app icon (Retina sizes 16@2x to
  512@2x), written by Pillow, so no Mac is needed.
- macos_iconset/Framefang.iconset/: the same as loose PNGs, for
  iconutil -c icns Framefang.iconset if a Mac build prefers it.
- macos_menubar_template/: black alpha templates for the macOS menu bar
  (the frame and the F, without the glow).

The SVGs come from .local/icon-tools/vectorize_v2.py, traced from Brandon's
mockup, and the rest from .local/icon-tools/make_icons.py.
