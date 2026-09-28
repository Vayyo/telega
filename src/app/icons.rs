//! Small icons drawn once at start (tiny-skia), for buttons that need a
//! picture rather than a glyph.

use std::sync::LazyLock;

use iced::widget::image;
use tiny_skia::{
    Color, FillRule, LineCap, LineJoin, Paint, PathBuilder, Pixmap, Stroke, Transform,
};

/// Side of the drawn icons: sharp at 16–20 px on 2× screens.
const SIDE: u32 = 40;

/// Red trash can: "close all tabs".
pub(crate) static TRASH: LazyLock<image::Handle> = LazyLock::new(|| {
    let s = SIDE as f32;
    let mut pixmap = Pixmap::new(SIDE, SIDE).expect("icon size");
    let red = Paint {
        anti_alias: true,
        shader: tiny_skia::Shader::SolidColor(Color::from_rgba8(0xE5, 0x39, 0x35, 255)),
        ..Paint::default()
    };
    let stroke = |width: f32| Stroke {
        width,
        line_cap: LineCap::Round,
        line_join: LineJoin::Round,
        ..Stroke::default()
    };
    let draw = |pixmap: &mut Pixmap, build: &dyn Fn(&mut PathBuilder), width: f32| {
        let mut path = PathBuilder::new();
        build(&mut path);
        if let Some(path) = path.finish() {
            pixmap.stroke_path(&path, &red, &stroke(width), Transform::identity(), None);
        }
    };
    // Lid and handle.
    draw(
        &mut pixmap,
        &|p| {
            p.move_to(s * 0.18, s * 0.26);
            p.line_to(s * 0.82, s * 0.26);
        },
        s * 0.08,
    );
    draw(
        &mut pixmap,
        &|p| {
            p.move_to(s * 0.40, s * 0.24);
            p.line_to(s * 0.40, s * 0.15);
            p.line_to(s * 0.60, s * 0.15);
            p.line_to(s * 0.60, s * 0.24);
        },
        s * 0.06,
    );
    // Body: a filled can, slightly narrower at the bottom.
    let mut body = PathBuilder::new();
    body.move_to(s * 0.25, s * 0.33);
    body.line_to(s * 0.75, s * 0.33);
    body.line_to(s * 0.70, s * 0.88);
    body.line_to(s * 0.30, s * 0.88);
    body.close();
    if let Some(body) = body.finish() {
        pixmap.fill_path(&body, &red, FillRule::Winding, Transform::identity(), None);
    }
    // Ribs, cut out of the body.
    let cut = Paint {
        anti_alias: true,
        blend_mode: tiny_skia::BlendMode::Clear,
        ..Paint::default()
    };
    for x in [0.41, 0.59] {
        let mut rib = PathBuilder::new();
        rib.move_to(s * x, s * 0.43);
        rib.line_to(s * x, s * 0.78);
        if let Some(rib) = rib.finish() {
            pixmap.stroke_path(&rib, &cut, &stroke(s * 0.06), Transform::identity(), None);
        }
    }
    let rgba: Vec<u8> = pixmap
        .pixels()
        .iter()
        .flat_map(|p| {
            let c = p.demultiply();
            [c.red(), c.green(), c.blue(), c.alpha()]
        })
        .collect();
    image::Handle::from_rgba(SIDE, SIDE, rgba)
});
