//! Pictures of the chrome to judge by eye, written to `target/` by ignored
//! tests: `cargo test --release -p framefang-app preview -- --ignored`.

use std::time::Duration;

use framefang_core::annotation::{Annotation, Palettes, Point2, Stroke};
use framefang_core::color::Rgba as Color;
use framefang_core::step::{StepLook, StepPart, StepShape};
use framefang_core::tool::Tool;
use image::{ImageBuffer, Rgba};

use super::chrome::{
    draw_action_panel, draw_dimension_chip, draw_flyout, draw_notice_bar, draw_selection_frame, draw_tool_panel,
    draw_tooltip, FlyoutFrame,
};
use super::dock::{draw_dock, draw_reading_frame, DockAnim, ReadLook};
use super::flyout::flyout_layout;
use super::paint::Canvas;
use super::panels::{EDITOR_SLOTS, LOCKED_ACTIONS};
use crate::pick::{self, Look, PickMode};

/// Save a framebuffer as `target/<name>`, `zoom` times bigger.
fn save(px: &[u32], w: usize, h: usize, zoom: u32, name: &str) {
    let img = image::RgbImage::from_fn(w as u32, h as u32, |x, y| {
        let p = px[y as usize * w + x as usize];
        image::Rgb([(p >> 16) as u8, (p >> 8) as u8, p as u8])
    });
    let img = image::imageops::resize(&img, w as u32 * zoom, h as u32 * zoom, image::imageops::FilterType::Nearest);
    img.save(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target").join(name)).unwrap();
}

/// A busy dimmed screen to draw over: blocks, with stripes like text.
fn busy(w: usize, h: usize, blocks: (usize, usize), colors: [u32; 2], stripes: Option<u32>) -> Vec<u32> {
    (0..w * h).map(|i| {
        let (x, y) = (i % w, i / w);
        match stripes {
            Some(ink) if y % 24 < 3 && x % 320 < 250 => ink,
            _ => colors[(x / blocks.0 + y / blocks.1) % 2],
        }
    }).collect()
}

/// The selection on a light page at 2x: mid-drag (frame and size chip, no
/// handles), settled with handles (record region), and a box against the
/// top of the screen whose chip sits inside: `target/selection.png`.
#[test]
#[ignore]
fn preview_selection() {
    let (w, h) = (900usize, 420usize);
    let mut px = busy(w, h, (90, 60), [0x00_EB_EE_F2, 0x00_D2_DC_EB], Some(0x00_28_32_46));
    let mut c = Canvas::new(&mut px, w, h);
    let screen = (0, 0, w as i32, h as i32);
    for (sel, handles) in [((40, 80, 360, 220), false), ((480, 80, 360, 220), true)] {
        draw_selection_frame(&mut c, (sel.0, sel.1, sel.0 + sel.2, sel.1 + sel.3), handles);
        draw_dimension_chip(&mut c, screen, sel, sel.2 as u32, sel.3 as u32);
    }
    let top = (120, 330, 300, 80);
    let low_screen = (0, 330, w as i32, 90);
    draw_selection_frame(&mut c, (top.0, top.1, top.0 + top.2, top.1 + top.3), false);
    draw_dimension_chip(&mut c, low_screen, top, top.2 as u32, top.3 as u32);
    save(&px, w, h, 2, "selection.png");
}

/// The capture dock at moments of its animations (still, mid sweep, mid
/// decode with Tab down, the record version), a tip and a notice, in two
/// color schemes: `target/capture-ui-<scheme>.png`.
#[test]
#[ignore]
fn preview_capture_ui() {
    let (w, h) = (1100usize, 560usize);
    let still = DockAnim { sweep: None, decode: None, pressed: false, cursor_on: true };
    let moments = [
        (false, PickMode::Auto, Some(PickMode::Screen), still),
        (false, PickMode::Auto, None, DockAnim { sweep: Some(0.35), ..still }),
        (false, PickMode::Screen, None, DockAnim { decode: Some(Duration::from_millis(200)), pressed: true, cursor_on: false, ..still }),
        (true, PickMode::Auto, None, still),
    ];
    for scheme in [crate::palette::Scheme::Orange, crate::palette::Scheme::Pink] {
        crate::palette::set(scheme);
        let mut px = busy(w, h, (90, 70), [0x00_2A_30_3A, 0x00_3C_3A_34], None);
        let mut c = Canvas::new(&mut px, w, h);
        for (i, (record, current, over, anim)) in moments.iter().enumerate() {
            let band = (0, 40 + i as i32 * 120, w as i32, 100 + pick::DOCK_MARGIN);
            let dock = pick::dock_layout(band, &PickMode::offered(2, !*record), *record);
            draw_dock(&mut c, &dock, *current, *over, anim);
        }
        draw_tooltip(&mut c, (0, 0, w as i32, h as i32), "Undo [Ctrl]+[Z]", 40, 4);
        draw_notice_bar(&mut c, (560, -30, 540, 200),
                        &["Throw away 2 annotations?", "[Esc] again to discard  //  any other key keeps editing"],
                        0x00_FF_B0_20, 0x00_FF_D8_8A);
        save(&px, w, h, 1, &format!("capture-ui-{}.png", scheme.name().to_lowercase()));
    }
}

/// The editor's tool column beside a picture with a spotlight, a lens and
/// an arrow on it: `target/editor-tools.png`.
#[test]
#[ignore]
fn preview_editor_tools() {
    let (w, h) = (900usize, 640usize);
    let mut pic = ImageBuffer::from_fn(w as u32, h as u32, |x, y| {
        let paper = if (x / 120 + y / 90) % 2 == 0 { [235u8, 238, 242, 255] } else { [210, 220, 235, 255] };
        Rgba(if y % 22 < 3 && x % 300 < 240 { [40, 50, 70, 255] } else { paper })
    });
    let stroke = Stroke { color: Color::new_opaque(255, 60, 90), thickness: 4.0 };
    let drawn = |mut a: Annotation, to: (f32, f32)| { a.extend(Point2::new(to.0, to.1)); a };
    let ink = [
        drawn(Annotation::spotlight(Point2::new(80.0, 120.0)), (420.0, 330.0)),
        drawn(Annotation::magnify(stroke, Point2::new(560.0, 200.0), 2), (640.0, 200.0)),
        drawn(Annotation::arrow(stroke, Point2::new(520.0, 480.0)), (400.0, 330.0)),
    ];
    let pristine = pic.clone();
    crate::painter::render_all(&mut crate::painter::ImageSurface(&mut pic), ink.iter(),
        &crate::painter::Pass { source: Some(&pristine), src_offset: (0, 0), bounds: (0, 0, 760, h as i32) });
    let mut px: Vec<u32> = pic.pixels().map(|p| (u32::from(p[0]) << 16) | (u32::from(p[1]) << 8) | u32::from(p[2])).collect();
    // Arrow fully hovered, Ellipse halfway through easing lit, Copy hovered
    // in the action row under the picture.
    let mut c = Canvas::new(&mut px, w, h);
    let (screen, sel) = ((0, 0, w as i32, h as i32), (20, 20, 760, 560));
    draw_tool_panel(&mut c, &EDITOR_SLOTS, screen, sel, Some(Tool::Magnify), |i| [0.0, 0.0, 1.0, 0.0, 0.5].get(i).copied().unwrap_or(0.0));
    draw_action_panel(&mut c, screen, sel, &LOCKED_ACTIONS, |i| if i == 1 { 1.0 } else { 0.0 });
    save(&px, w, h, 1, "editor-tools.png");
}

/// TEXT and QR's look on a dimmed screen: a highlight with its window's
/// name, a TEXT box being read, a QR box with its reticle, one being read
/// with links flipped, and a box at the screen's top:
/// `target/reading-look.png`.
#[test]
#[ignore]
fn preview_reading_look() {
    let (w, h) = (1100usize, 620usize);
    let mut px = busy(w, h, (140, 100), [0x00_3A_3D_44, 0x00_30_34_3C], Some(0x00_6A_70_80));
    let mut c = Canvas::new(&mut px, w, h);
    let look = |kind, cursor_on, reading: Option<u64>, links| ReadLook { kind, cursor_on, reading: reading.map(Duration::from_millis), links };
    let idle = look(Look::Text, true, None, (false, false));
    draw_reading_frame(&mut c, (40, 70, 300, 180), &idle, Some("Notepad"));
    draw_reading_frame(&mut c, (400, 70, 320, 200), &look(Look::Text, false, Some(560), (false, false)), None);
    draw_reading_frame(&mut c, (780, 70, 220, 220), &look(Look::Qr, true, None, (true, false)), None);
    draw_reading_frame(&mut c, (40, 360, 260, 200), &look(Look::Qr, false, Some(320), (false, true)), None);
    draw_reading_frame(&mut c, (420, 0, 400, 140), &idle, None);
    save(&px, w, h, 1, "reading-look.png");
}

/// Step's flyout (or Highlight's, with `FF_TOOL=Highlight`), twice the
/// size: `target/step-flyout.png`. `FF_STEP` names the shape to show.
#[test]
#[ignore]
fn preview_step_flyout() {
    let tool = if std::env::var("FF_TOOL").as_deref() == Ok("Highlight") { Tool::Highlight } else { Tool::Step };
    let layout = flyout_layout(tool);
    let (w, h) = ((layout.w + 20) as usize, (layout.h + 20) as usize);
    let mut px = vec![0x00_30_30_34u32; w * h];
    let shape = std::env::var("FF_STEP").ok()
        .and_then(|name| StepShape::ALL.into_iter().find(|s| s.label() == name))
        .unwrap_or(StepShape::Disc);
    let look = StepLook::recipe(shape, Palettes::ALL[0].colors[0]);
    let frame = FlyoutFrame {
        rect: (10, 10, layout.w, layout.h), on_right: true, pointer_y: 40, layout, values: vec![],
        color: look.accent(), palette: 0, open: 1.0, picking: false, spectrum_at: None,
        step: (tool == Tool::Step).then_some((look, StepPart::Body)),
        mark: (tool == Tool::Highlight).then_some(framefang_core::mark::MarkLook::default()),
    };
    draw_flyout(&mut Canvas::new(&mut px, w, h), &frame);
    save(&px, w, h, 2, if tool == Tool::Highlight { "highlight-flyout.png" } else { "step-flyout.png" });
}
