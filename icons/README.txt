Framefang icons

White fangs in a hot pink capture frame, on a black rounded tile.

The logo is vector. Everything else here is rendered from it.

- framefang.svg: the logo. Used for every size above 32 px.
- framefang-small.svg: the same logo tuned for 16 to 32 px (taskbar, tray,
  title bar): thicker brackets, a slightly larger fang without its tiny
  inner tooth, and a brighter rim so the tile edge shows on dark taskbars.
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
  (the frame and fang only, not the tile).

