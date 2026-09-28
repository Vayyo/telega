//! Color schemes: which surface has which color. The iced theme carries
//! them (its extended palette is filled in by hand, so the stock widget
//! styles pick them up): chat background, incoming bubbles, own bubbles in
//! the accent, tabs and buttons. The chat list gets its own tone on top.

use iced::Color;
use iced::theme::palette::{Extended, Pair};
use iced::theme::{Mode, Palette};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Scheme {
    /// iced's light or dark, following the system, with the accent.
    System,
    /// Dark blue layers like Telegram Desktop's night theme.
    #[default]
    Telegram,
    /// Neutral dark gray with distinct layers.
    Graphite,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Accent {
    #[default]
    Blue,
    Green,
    Violet,
    Orange,
}

impl Scheme {
    pub const ALL: [Self; 3] = [Self::System, Self::Telegram, Self::Graphite];

    pub fn label(self) -> &'static str {
        match self {
            Self::System => "Как в системе",
            Self::Telegram => "Telegram",
            Self::Graphite => "Графит",
        }
    }
}

impl Accent {
    pub const ALL: [Self; 4] = [Self::Blue, Self::Green, Self::Violet, Self::Orange];

    pub fn color(self) -> Color {
        match self {
            Self::Blue => rgb(0x52, 0x88, 0xC1),
            Self::Green => rgb(0x4F, 0xAE, 0x4E),
            Self::Violet => rgb(0x87, 0x74, 0xE1),
            Self::Orange => rgb(0xE0, 0x8A, 0x3C),
        }
    }
}

/// Look settings, kept in `settings.json`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct LookSettings {
    pub scheme: Scheme,
    pub accent: Accent,
}

const fn rgb(r: u8, g: u8, b: u8) -> Color {
    Color::from_rgb8(r, g, b)
}

fn mix(a: Color, b: Color, t: f32) -> Color {
    Color::from_rgb(
        a.r + (b.r - a.r) * t,
        a.g + (b.g - a.g) * t,
        a.b + (b.b - a.b) * t,
    )
}

/// Everything the views need about colors.
#[derive(Debug, Clone)]
pub(crate) struct Look {
    pub(crate) theme: iced::Theme,
    /// Chat list panel.
    pub(crate) sidebar: Color,
}

impl Look {
    pub(crate) fn new(settings: LookSettings, system: Mode) -> Self {
        let accent = settings.accent.color();
        let text = rgb(0xF5, 0xF5, 0xF5);
        let dark = |name: &'static str, chat, sidebar, incoming, tab, own_mix| {
            let palette = Palette {
                background: chat,
                text,
                primary: accent,
                success: rgb(0x4F, 0xAE, 0x4E),
                warning: rgb(0xE0, 0xA0, 0x3C),
                danger: rgb(0xE5, 0x5B, 0x5B),
            };
            let own = mix(chat, accent, own_mix);
            let theme = iced::Theme::custom_with_fn(name, palette, move |palette| {
                let mut extended = Extended::generate(palette);
                // Incoming bubbles and boxes; own bubbles; tabs and buttons.
                extended.background.weak = Pair::new(incoming, text);
                extended.primary.weak = Pair::new(own, text);
                extended.secondary.base = Pair::new(tab, text);
                extended.secondary.strong = Pair::new(mix(tab, text, 0.12), text);
                extended
            });
            Self { theme, sidebar }
        };
        match settings.scheme {
            Scheme::Telegram => dark(
                "Telegram",
                rgb(0x0E, 0x16, 0x21),
                rgb(0x17, 0x21, 0x2B),
                rgb(0x18, 0x25, 0x33),
                rgb(0x24, 0x2F, 0x3D),
                0.45,
            ),
            Scheme::Graphite => dark(
                "Графит",
                rgb(0x23, 0x24, 0x28),
                rgb(0x1B, 0x1C, 0x1F),
                rgb(0x2E, 0x2F, 0x33),
                rgb(0x3A, 0x3B, 0x40),
                0.75,
            ),
            Scheme::System => {
                let base = if system == Mode::Light {
                    Palette::LIGHT
                } else {
                    Palette::DARK
                };
                let theme = iced::Theme::custom(
                    "Системная",
                    Palette {
                        primary: accent,
                        ..base
                    },
                );
                let sidebar = theme.extended_palette().background.weak.color;
                Self { theme, sidebar }
            }
        }
    }
}

/// Colors of people and chats (avatar placeholders, names, tab dots,
/// quote stripes): the same person always gets the same one.
pub(crate) const PEOPLE: [Color; 7] = [
    rgb(0xE1, 0x72, 0x76),
    rgb(0xF0, 0x9A, 0x47),
    rgb(0x9B, 0x81, 0xE5),
    rgb(0x6D, 0xC1, 0x6C),
    rgb(0x57, 0xA5, 0xD6),
    rgb(0xE0, 0x6F, 0xA8),
    rgb(0x4F, 0xB8, 0xB5),
];

pub(crate) fn person_color(id: i64) -> Color {
    PEOPLE[(id.unsigned_abs() % PEOPLE.len() as u64) as usize]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schemes_keep_the_surfaces_apart() {
        for scheme in [Scheme::Telegram, Scheme::Graphite] {
            for accent in Accent::ALL {
                let look = Look::new(LookSettings { scheme, accent }, Mode::Dark);
                let p = look.theme.extended_palette();
                let surfaces = [
                    p.background.base.color,
                    look.sidebar,
                    p.background.weak.color,
                    p.primary.weak.color,
                    p.secondary.base.color,
                ];
                for (i, a) in surfaces.iter().enumerate() {
                    for b in &surfaces[i + 1..] {
                        assert_ne!(a, b, "{scheme:?}/{accent:?}: two surfaces share a color");
                    }
                }
                assert_eq!(p.primary.base.color, accent.color());
            }
        }
    }

    #[test]
    fn a_person_keeps_one_color() {
        assert_eq!(person_color(7), person_color(7));
        assert_eq!(person_color(-7), person_color(7));
        assert_ne!(person_color(7), person_color(8));
    }
}
