//! The mouse pointer in a screenshot, when Settings asks for it.
//!
//! A screen capture never contains the pointer: the system draws it over
//! the screen rather than into it. So it is painted in afterwards, from the
//! pointer's own image, at the spot it was in when the screen was read.
//! Windows only for now; elsewhere the setting is not offered.

use crate::capture::Captured;

/// Whether this platform can draw the pointer into a capture.
pub const SUPPORTED: bool = cfg!(windows);

/// Paint the pointer into `shot` where it is now. `false` when it is hidden
/// or could not be read; the capture is then left as it was.
#[cfg(windows)]
pub fn draw_into(shot: &mut Captured) -> bool {
    let Some(cursor) = imp::read() else { return false };
    let (bx, by) = shot.map.logical_to_physical((cursor.pos.0 as f32, cursor.pos.1 as f32));
    let at = (bx.round() as i32 - cursor.hotspot.0, by.round() as i32 - cursor.hotspot.1);
    composite(&mut shot.bitmap, at, &cursor.sprite);
    true
}

#[cfg(not(windows))]
pub fn draw_into(_shot: &mut Captured) -> bool { false }

/// The pointer's picture, straight (not premultiplied) RGBA.
#[cfg_attr(not(windows), allow(dead_code))]
struct Sprite {
    w: usize,
    h: usize,
    rgba: Vec<u8>,
}

/// Blend `sprite` over `dst` with its top left at `at`, clipped to `dst`.
#[cfg_attr(not(windows), allow(dead_code))]
fn composite(dst: &mut image::ImageBuffer<image::Rgba<u8>, Vec<u8>>, at: (i32, i32), sprite: &Sprite) {
    let (dw, dh) = (dst.width() as i32, dst.height() as i32);
    for sy in 0..sprite.h as i32 {
        for sx in 0..sprite.w as i32 {
            let (x, y) = (at.0 + sx, at.1 + sy);
            if x < 0 || y < 0 || x >= dw || y >= dh { continue; }
            let i = (sy as usize * sprite.w + sx as usize) * 4;
            let a = sprite.rgba[i + 3] as u32;
            if a == 0 { continue; }
            let p = dst.get_pixel_mut(x as u32, y as u32);
            for c in 0..3 {
                p.0[c] = ((sprite.rgba[i + c] as u32 * a + p.0[c] as u32 * (255 - a) + 127) / 255) as u8;
            }
        }
    }
}

/// Recover a picture's alpha from two renders of it, over black and over
/// white: where it is opaque both agree, where it is clear they differ by
/// the full 255. Both renders are BGRA. A pixel the pointer inverts (the
/// classic I-beam) comes out darker over white than over black; it is drawn
/// solid black, which reads on the light screens such pointers sit over.
#[cfg_attr(not(windows), allow(dead_code))]
fn unblend(black: &[u8], white: &[u8]) -> Vec<u8> {
    let mut out = vec![0u8; black.len()];
    for (i, (b, w)) in black.chunks_exact(4).zip(white.chunks_exact(4)).enumerate() {
        let o = &mut out[i * 4..i * 4 + 4];
        let diff = (0..3).map(|c| w[c] as i32 - b[c] as i32).max().unwrap_or(0);
        if diff < 0 {
            o.copy_from_slice(&[0, 0, 0, 255]);
            continue;
        }
        let a = 255 - diff.min(255);
        if a == 0 { continue; }
        let un = |c: u8| ((c as i32 * 255 + a / 2) / a).min(255) as u8;
        o.copy_from_slice(&[un(b[2]), un(b[1]), un(b[0]), a as u8]);
    }
    out
}

#[cfg(windows)]
mod imp {
    use super::{unblend, Sprite};
    use windows::Win32::Graphics::Gdi::{
        CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GdiFlush, GetObjectW, SelectObject,
        BITMAP, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HBITMAP,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        DrawIconEx, GetCursorInfo, GetIconInfo, CURSORINFO, CURSOR_SHOWING, DI_NORMAL, HICON, ICONINFO,
    };

    pub struct Pointer {
        /// Screen position of the pointer's hotspot, in device pixels.
        pub pos: (i32, i32),
        pub hotspot: (i32, i32),
        pub sprite: Sprite,
    }

    pub fn read() -> Option<Pointer> {
        unsafe {
            let mut ci = CURSORINFO { cbSize: std::mem::size_of::<CURSORINFO>() as u32, ..Default::default() };
            GetCursorInfo(&mut ci).ok()?;
            if ci.flags.0 & CURSOR_SHOWING.0 == 0 || ci.hCursor.is_invalid() { return None; }
            let icon = HICON(ci.hCursor.0);
            let mut ii = ICONINFO::default();
            GetIconInfo(icon, &mut ii).ok()?;
            // A color pointer has its own bitmap; a monochrome one stacks
            // its AND and XOR masks in one bitmap twice its height.
            let size = bitmap_size(ii.hbmColor).or_else(|| bitmap_size(ii.hbmMask).map(|(w, h)| (w, h / 2)));
            for b in [ii.hbmColor, ii.hbmMask] {
                if !b.is_invalid() { let _ = DeleteObject(b.into()); }
            }
            let (w, h) = size.filter(|&(w, h)| w > 0 && h > 0 && w <= 256 && h <= 256)?;
            let black = render(icon, w, h, 0x00)?;
            let white = render(icon, w, h, 0xFF)?;
            Some(Pointer {
                pos: (ci.ptScreenPos.x, ci.ptScreenPos.y),
                hotspot: (ii.xHotspot as i32, ii.yHotspot as i32),
                sprite: Sprite { w, h, rgba: unblend(&black, &white) },
            })
        }
    }

    unsafe fn bitmap_size(bmp: HBITMAP) -> Option<(usize, usize)> {
        if bmp.is_invalid() { return None; }
        let mut bm = BITMAP::default();
        let got = GetObjectW(bmp.into(), std::mem::size_of::<BITMAP>() as i32,
                             Some(std::ptr::from_mut(&mut bm).cast()));
        (got > 0).then(|| (bm.bmWidth.max(0) as usize, bm.bmHeight.unsigned_abs() as usize))
    }

    /// The pointer drawn over a `w` x `h` field of `bg`, as BGRA.
    unsafe fn render(icon: HICON, w: usize, h: usize, bg: u8) -> Option<Vec<u8>> {
        let dc = CreateCompatibleDC(None);
        if dc.is_invalid() { return None; }
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w as i32,
                biHeight: -(h as i32), // top-down rows
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
        let out = match CreateDIBSection(Some(dc), &bmi, DIB_RGB_COLORS, &mut bits, None, 0) {
            Ok(bmp) if !bits.is_null() => {
                let old = SelectObject(dc, bmp.into());
                std::ptr::write_bytes(bits.cast::<u8>(), bg, w * h * 4);
                let drawn = DrawIconEx(dc, 0, 0, icon, w as i32, h as i32, 0, None, DI_NORMAL).is_ok();
                let _ = GdiFlush();
                let pixels = drawn.then(|| std::slice::from_raw_parts(bits.cast::<u8>(), w * h * 4).to_vec());
                SelectObject(dc, old);
                let _ = DeleteObject(bmp.into());
                pixels
            }
            _ => None,
        };
        let _ = DeleteDC(dc);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opaque_pixels_keep_their_color_and_clear_ones_vanish() {
        // BGRA: an opaque red pixel, then a clear one.
        let black = [0, 0, 255, 0, 0, 0, 0, 0];
        let white = [0, 0, 255, 0, 255, 255, 255, 0];
        let out = unblend(&black, &white);
        assert_eq!(&out[0..4], &[255, 0, 0, 255]);
        assert_eq!(out[7], 0);
    }

    #[test]
    fn a_half_clear_pixel_comes_back_at_its_own_color() {
        // 50 % white over black is 128; over white it is 255.
        let out = unblend(&[128, 128, 128, 0], &[255, 255, 255, 0]);
        assert!((out[3] as i32 - 128).abs() <= 1);
        assert!(out[0] >= 250, "the color is white, not grey");
    }

    #[test]
    fn an_inverting_pixel_is_drawn_black() {
        assert_eq!(unblend(&[255, 255, 255, 0], &[0, 0, 0, 0]), vec![0, 0, 0, 255]);
    }

    #[test]
    fn the_sprite_blends_in_and_clips_at_the_edges() {
        let mut dst = image::ImageBuffer::from_pixel(4, 4, image::Rgba([0, 0, 0, 255]));
        let sprite = Sprite { w: 2, h: 2, rgba: vec![255; 16] };
        composite(&mut dst, (3, 3), &sprite);
        assert_eq!(dst.get_pixel(3, 3).0, [255, 255, 255, 255]);
        assert_eq!(dst.get_pixel(2, 2).0, [0, 0, 0, 255]);
    }
}
