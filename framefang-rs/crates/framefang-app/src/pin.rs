/*!
Pinned images: a captured selection kept floating on screen in its own small
window, frameless and always on top.

Choosing Pin in the editor hands the cropped bitmap here. Dragging with the
left button moves the window; Esc, a right click or a middle click closes it.

The tray app owns the only event loop, so a pin cannot run one of its own.
`TrayApp` keeps a `Vec<PinView>` and routes each `WindowEvent` to the pin
whose `WindowId` it carries, the same way it drives the editor overlay. The
window plumbing (attributes, drawing surface, repaint cycle) is shared with
the recording panel and lives in `recording_indicator`.
*/

use {
    crate::recording_indicator::{drawing_surface, floating_attrs, open_window, repaint, SoftContext, SoftSurface},
    anyhow::Result,
    framefang_core::dpi::sample_index,
    image::{ImageBuffer, Rgba},
    std::rc::Rc,
    winit::{
        event::{ElementState, KeyEvent, MouseButton, WindowEvent},
        event_loop::ActiveEventLoop,
        keyboard::{Key, NamedKey},
        window::{CursorIcon, Window, WindowId},
    },
};

type Bitmap = ImageBuffer<Rgba<u8>, Vec<u8>>;

/// What an empty pin shows: the near-black window background.
const EMPTY_FILL: u32 = 0x0010_1014;

pub struct PinView {
    bitmap: Bitmap,
    window: Rc<Window>,
    // Never read, but the surface is only valid while its context lives.
    _context: SoftContext,
    canvas: SoftSurface,
}

impl PinView {
    /// Open a pin showing `image` with its top-left corner at `screen_pos`,
    /// both in device pixels. The bitmap was cut from a capture taken at
    /// device resolution, so asking for a window whose physical inner size
    /// equals the bitmap's covers exactly the area the user selected, whatever
    /// the display's scale factor: on a 2x panel the bitmap is twice as wide
    /// and the window half as many logical points, which is the same physical
    /// rectangle. If the window manager hands back some other size, the
    /// repaint scales to fit.
    pub fn new(
        loop_target: &ActiveEventLoop, image: Bitmap, screen_pos: (i32, i32),
    ) -> Result<Self> {
        let size = image.dimensions();
        let floating = open_window(loop_target, floating_attrs("Frame Fang: pinned", size, screen_pos), "pin")?;
        // As a tray or menu-bar agent Frame Fang has no Dock icon on macOS,
        // so a pin that opened behind the front app could not be brought
        // forward. Asking for focus fixes that; elsewhere it just raises it.
        floating.focus_window();
        floating.set_cursor(CursorIcon::Move);
        let (context, canvas) = drawing_surface(&floating, "pin")?;

        let mut pin = Self { bitmap: image, window: floating, _context: context, canvas };
        pin.repaint_now();
        Ok(pin)
    }

    pub fn window_id(&self) -> WindowId {
        self.window.id()
    }

    /// Handle one event for this pin. Returns `true` once the pin should
    /// close.
    pub fn handle_event(&mut self, event: WindowEvent) -> bool {
        use WindowEvent as Ev;
        let escape = |key: &KeyEvent| {
            key.state == ElementState::Pressed && key.logical_key == Key::Named(NamedKey::Escape)
        };
        match &event {
            Ev::CloseRequested => return true,
            Ev::KeyboardInput { event: key, .. } if escape(key) => return true,
            Ev::MouseInput { state: ElementState::Pressed, button, .. } => match button {
                MouseButton::Right | MouseButton::Middle => return true,
                // Let the window manager run the move (on X11 this is
                // _NET_WM_MOVERESIZE, which every current WM supports), so it
                // feels like dragging any other window.
                MouseButton::Left => drop(self.window.drag_window()),
                _ => (),
            },
            Ev::Resized(_) | Ev::RedrawRequested => self.repaint_now(),
            _ => (),
        }
        false
    }

    fn repaint_now(&mut self) {
        let bitmap = &self.bitmap;
        repaint(&self.window, &mut self.canvas, "pin", |frame, width, height| {
            paint(bitmap, frame, width, height);
        });
    }
}

/// Fill `frame` (`width` by `height`, softbuffer's 0RGB pixels) with
/// `bitmap` scaled to fit. Normally the window is exactly the bitmap's size
/// and this is a straight copy, but a window manager that clamps the window,
/// or a move to a display with another scale factor, would otherwise crop the
/// picture rather than show all of it. An empty bitmap paints the background.
fn paint(bitmap: &Bitmap, frame: &mut [u32], width: usize, height: usize) {
    let (src_w, src_h) = bitmap.dimensions();
    if src_w == 0 || src_h == 0 {
        frame.fill(EMPTY_FILL);
        return;
    }
    let pixels = bitmap.as_raw();
    let src_stride = src_w as usize * 4;
    // Every row samples the same columns, so work them out once.
    let columns: Vec<usize> = (0..width)
        .map(|x| sample_index(x as u32, width as u32, src_w) as usize * 4)
        .collect();
    for (y, out_row) in frame.chunks_exact_mut(width).take(height).enumerate() {
        let row_start = sample_index(y as u32, height as u32, src_h) as usize * src_stride;
        let src_row = &pixels[row_start..row_start + src_stride];
        for (out, &at) in out_row.iter_mut().zip(&columns) {
            let [r, g, b] = [src_row[at], src_row[at + 1], src_row[at + 2]].map(u32::from);
            *out = r << 16 | g << 8 | b;
        }
    }
}

#[cfg(test)]
mod checks {
    use super::{paint, sample_index, Bitmap, ImageBuffer, Rgba, EMPTY_FILL};

    fn bitmap(w: u32, h: u32, colour: impl Fn(u32, u32) -> [u8; 4]) -> Bitmap {
        ImageBuffer::from_fn(w, h, |x, y| Rgba(colour(x, y)))
    }

    #[test]
    fn same_size_is_a_straight_copy_without_alpha() {
        let src = bitmap(3, 2, |x, y| [x as u8 * 10, y as u8 * 20, 7, 128]);
        let mut frame = vec![0u32; 6];
        paint(&src, &mut frame, 3, 2);
        assert_eq!(
            frame,
            [0x00_00_07, 0x0A_00_07, 0x14_00_07, 0x00_14_07, 0x0A_14_07, 0x14_14_07]
        );
    }

    #[test]
    fn a_larger_window_repeats_source_pixels() {
        let src = bitmap(2, 1, |x, _| if x == 0 { [255, 0, 0, 255] } else { [0, 0, 255, 255] });
        let mut frame = vec![0u32; 8];
        paint(&src, &mut frame, 4, 2);
        let red = 0xFF_00_00;
        let blue = 0x00_00_FF;
        assert_eq!(frame, [red, red, blue, blue, red, red, blue, blue]);
    }

    #[test]
    fn every_pixel_matches_sample_index() {
        let src = bitmap(5, 3, |x, y| [x as u8, y as u8, (x * y) as u8, 255]);
        let (w, h) = (7usize, 4usize);
        let mut frame = vec![0u32; w * h];
        paint(&src, &mut frame, w, h);
        for y in 0..h {
            for x in 0..w {
                let sx = sample_index(x as u32, w as u32, 5);
                let sy = sample_index(y as u32, h as u32, 3);
                let p = src.get_pixel(sx, sy).0;
                let want = u32::from(p[0]) << 16 | u32::from(p[1]) << 8 | u32::from(p[2]);
                assert_eq!(frame[y * w + x], want, "({x}, {y})");
            }
        }
    }

    #[test]
    fn an_empty_bitmap_paints_the_background() {
        let src = Bitmap::new(0, 0);
        let mut frame = vec![1u32; 4];
        paint(&src, &mut frame, 2, 2);
        assert!(frame.iter().all(|&p| p == EMPTY_FILL));
    }
}
