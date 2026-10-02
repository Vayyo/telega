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

/// Monochrome composer artwork; the light/dark variants are cached once and
/// selected to contrast with the current button surface, never rasterized in view.
#[derive(Clone, Copy)]
pub(crate) enum ComposerIcon {
    Smile,
    Plane,
    Clip,
    Microphone,
    Check,
    Cross,
}

impl ComposerIcon {
    pub(crate) fn handle(self, light: bool) -> image::Handle {
        let icons = if light {
            &*COMPOSER_LIGHT
        } else {
            &*COMPOSER_DARK
        };
        icons[self as usize].clone()
    }
}

static COMPOSER_LIGHT: LazyLock<[image::Handle; 6]> = LazyLock::new(|| {
    std::array::from_fn(|i| composer_icon(i, Color::from_rgba8(255, 255, 255, 255)))
});
static COMPOSER_DARK: LazyLock<[image::Handle; 6]> =
    LazyLock::new(|| std::array::from_fn(|i| composer_icon(i, Color::from_rgba8(0, 0, 0, 255))));

fn composer_icon(icon: usize, color: Color) -> image::Handle {
    let mut pixmap = Pixmap::new(SIDE, SIDE).expect("icon size");
    let paint = Paint {
        anti_alias: true,
        shader: tiny_skia::Shader::SolidColor(color),
        ..Paint::default()
    };
    let stroke = Stroke {
        width: 3.6,
        line_cap: LineCap::Round,
        line_join: LineJoin::Round,
        ..Stroke::default()
    };
    let mut path = PathBuilder::new();
    match icon {
        0 => {
            path.push_circle(20.0, 20.0, 13.0);
            path.move_to(13.0, 23.0);
            path.cubic_to(16.0, 29.0, 24.0, 29.0, 27.0, 23.0);
            pixmap.stroke_path(
                &path.finish().unwrap(),
                &paint,
                &stroke,
                Transform::identity(),
                None,
            );
            let mut eyes = PathBuilder::new();
            eyes.push_circle(15.0, 17.0, 1.8);
            eyes.push_circle(25.0, 17.0, 1.8);
            pixmap.fill_path(
                &eyes.finish().unwrap(),
                &paint,
                FillRule::Winding,
                Transform::identity(),
                None,
            );
        }
        1 => {
            // A swept Telegram-style plane with a folded wing and tail.
            path.move_to(5.0, 18.0);
            path.line_to(34.0, 6.0);
            path.line_to(27.0, 34.0);
            path.line_to(19.0, 24.0);
            path.line_to(5.0, 18.0);
            path.close();
            pixmap.stroke_path(
                &path.finish().unwrap(),
                &paint,
                &stroke,
                Transform::identity(),
                None,
            );
            let mut fold = PathBuilder::new();
            fold.move_to(19.0, 24.0);
            fold.line_to(34.0, 6.0);
            fold.move_to(19.0, 24.0);
            fold.line_to(16.0, 32.0);
            fold.line_to(23.0, 29.0);
            pixmap.stroke_path(
                &fold.finish().unwrap(),
                &paint,
                &stroke,
                Transform::identity(),
                None,
            );
        }
        2 => {
            path.move_to(16.0, 27.0);
            path.line_to(27.0, 16.0);
            path.cubic_to(31.0, 12.0, 25.0, 6.0, 21.0, 10.0);
            path.line_to(9.0, 22.0);
            path.cubic_to(2.0, 29.0, 12.0, 39.0, 19.0, 32.0);
            path.line_to(31.0, 20.0);
            pixmap.stroke_path(
                &path.finish().unwrap(),
                &paint,
                &stroke,
                Transform::identity(),
                None,
            );
        }
        3 => {
            path.move_to(15.0, 12.0);
            path.cubic_to(15.0, 5.0, 25.0, 5.0, 25.0, 12.0);
            path.line_to(25.0, 21.0);
            path.cubic_to(25.0, 28.0, 15.0, 28.0, 15.0, 21.0);
            path.close();
            path.move_to(10.0, 20.0);
            path.cubic_to(10.0, 34.0, 30.0, 34.0, 30.0, 20.0);
            path.move_to(20.0, 31.0);
            path.line_to(20.0, 36.0);
            path.move_to(15.0, 36.0);
            path.line_to(25.0, 36.0);
            pixmap.stroke_path(
                &path.finish().unwrap(),
                &paint,
                &stroke,
                Transform::identity(),
                None,
            );
        }
        4 => {
            path.move_to(8.0, 21.0);
            path.line_to(17.0, 29.0);
            path.line_to(33.0, 11.0);
            pixmap.stroke_path(
                &path.finish().unwrap(),
                &paint,
                &stroke,
                Transform::identity(),
                None,
            );
        }
        _ => {
            path.move_to(11.0, 11.0);
            path.line_to(29.0, 29.0);
            path.move_to(29.0, 11.0);
            path.line_to(11.0, 29.0);
            pixmap.stroke_path(
                &path.finish().unwrap(),
                &paint,
                &stroke,
                Transform::identity(),
                None,
            );
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
}
