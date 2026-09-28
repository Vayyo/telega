//! QR code of the login link for "scan with the phone" sign-in.

use iced::widget::image::Handle;

/// White margin around the code, in modules (the standard asks for 4).
const QUIET: usize = 4;
/// Pixels per module; the view scales with nearest-neighbor filtering.
const SCALE: usize = 6;

/// Dark-on-white RGBA image of `link`, as scanners expect regardless of the
/// app theme.
pub(crate) fn rgba(link: &str) -> Option<(u32, Vec<u8>)> {
    let code = qrcode::QrCode::new(link.as_bytes()).ok()?;
    let modules = code.width();
    let colors = code.to_colors();
    let side = (modules + 2 * QUIET) * SCALE;
    let mut pixels = vec![255u8; side * side * 4];
    for (i, color) in colors.iter().enumerate() {
        if *color != qrcode::Color::Dark {
            continue;
        }
        let (mx, my) = (i % modules + QUIET, i / modules + QUIET);
        for y in my * SCALE..(my + 1) * SCALE {
            for x in mx * SCALE..(mx + 1) * SCALE {
                let at = (y * side + x) * 4;
                pixels[at..at + 3].fill(0);
            }
        }
    }
    Some((side as u32, pixels))
}

pub(crate) fn image(link: &str) -> Option<Handle> {
    let (side, pixels) = rgba(link)?;
    Some(Handle::from_rgba(side, side, pixels))
}

#[cfg(test)]
mod tests {
    #[test]
    fn code_has_a_white_margin_and_dark_finder_corner() {
        let (side, px) =
            super::rgba("tg://login?token=AQIDBAUGBwgJCgsMDQ4PEBESExQVFhcYGRobHB0eHyA").unwrap();
        let at = |x: usize, y: usize| px[(y * side as usize + x) * 4];
        let margin = super::QUIET * super::SCALE;
        assert_eq!(at(0, 0), 255);
        assert_eq!(at(margin - 1, margin - 1), 255, "quiet zone is white");
        assert_eq!(at(margin, margin), 0, "finder pattern starts dark");
        assert_eq!(side as usize % super::SCALE, 0);
    }
}
