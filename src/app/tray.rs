//! Icon in the panel's tray (StatusNotifierItem: KDE, waybar, quickshell
//! panels, GNOME with the AppIndicator extension) with the unread count, as
//! Telegram Desktop shows it: red when there is something that notifies,
//! gray when only muted chats have unread messages. A click brings up the
//! main window. Linux only for now; elsewhere just the window icon.

use iced::advanced::graphics::text::{cosmic_text as ct, font_system};
use tiny_skia::{
    Color, FillRule, LineCap, LinearGradient, Paint, PathBuilder, Pixmap, Point, Rect, SpreadMode,
    Stroke, Transform,
};
#[cfg(target_os = "linux")]
use {
    iced::Task,
    iced::futures::{SinkExt, Stream},
    tokio::sync::mpsc,
};

/// Sizes offered to the panel; it picks the closest one.
const SIZES: [u32; 3] = [22, 32, 64];
fn rgb(r: u8, g: u8, b: u8) -> Color {
    Color::from_rgba8(r, g, b, 255)
}

/// What the badge shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Badge {
    pub(crate) count: i32,
    /// Something unread would notify (red); otherwise only muted chats (gray).
    pub(crate) alert: bool,
}

/// Badge for the unread message counts of the main chat list.
pub(crate) fn badge(unread: i32, unread_unmuted: i32) -> Option<Badge> {
    if unread_unmuted > 0 {
        Some(Badge {
            count: unread_unmuted,
            alert: true,
        })
    } else if unread > 0 {
        Some(Badge {
            count: unread,
            alert: false,
        })
    } else {
        None
    }
}

/// Short form that fits the badge: 7, 42, 999, 1K, 99K+.
fn label(count: i32) -> String {
    if count < 1000 {
        count.to_string()
    } else if count < 100_000 {
        format!("{}K", count / 1000)
    } else {
        "99K+".to_owned()
    }
}

/// The client's icon, `size`×`size`, premultiplied RGBA.
fn draw_icon(size: u32) -> Pixmap {
    let mut pixmap = Pixmap::new(size, size).expect("icon size");
    let s = size as f32;
    let circle = PathBuilder::from_circle(s / 2.0, s / 2.0, s / 2.0).expect("circle");
    let mut paint = Paint {
        anti_alias: true,
        ..Paint::default()
    };
    paint.shader = LinearGradient::new(
        Point::from_xy(0.0, 0.0),
        Point::from_xy(0.0, s),
        vec![
            tiny_skia::GradientStop::new(0.0, rgb(0x3B, 0xB4, 0xEA)),
            tiny_skia::GradientStop::new(1.0, rgb(0x1C, 0x8A, 0xC4)),
        ],
        SpreadMode::Pad,
        Transform::identity(),
    )
    .expect("gradient");
    pixmap.fill_path(
        &circle,
        &paint,
        FillRule::Winding,
        Transform::identity(),
        None,
    );

    // A white "T" with rounded ends.
    let mut t = PathBuilder::new();
    t.move_to(s * 0.30, s * 0.32);
    t.line_to(s * 0.70, s * 0.32);
    t.move_to(s * 0.50, s * 0.32);
    t.line_to(s * 0.50, s * 0.74);
    let t = t.finish().expect("glyph");
    let white = Paint {
        anti_alias: true,
        shader: tiny_skia::Shader::SolidColor(Color::WHITE),
        ..Paint::default()
    };
    let stroke = Stroke {
        width: s * 0.13,
        line_cap: LineCap::Round,
        ..Stroke::default()
    };
    pixmap.stroke_path(&t, &white, &stroke, Transform::identity(), None);
    pixmap
}

/// The icon with the count in a pill at the bottom right.
fn draw(size: u32, badge: Option<Badge>) -> Pixmap {
    let mut pixmap = draw_icon(size);
    let Some(badge) = badge else {
        return pixmap;
    };
    let s = size as f32;
    let height = (s * 0.56).round();
    let font_size = height * 0.78;

    let mut fonts = font_system().write().expect("font system");
    let fonts = fonts.raw();
    let mut buffer = ct::Buffer::new(fonts, ct::Metrics::new(font_size, height));
    buffer.set_size(fonts, None, None);
    buffer.set_text(
        fonts,
        &label(badge.count),
        &ct::Attrs::new()
            .family(ct::Family::SansSerif)
            .weight(ct::Weight::BOLD),
        ct::Shaping::Advanced,
        None,
    );
    buffer.shape_until_scroll(fonts, false);
    let text_width = buffer
        .layout_runs()
        .map(|run| run.line_w)
        .fold(0.0_f32, f32::max);

    let width = (text_width + height * 0.5).max(height).min(s);
    let (x, y) = (s - width, s - height);
    let pill = PathBuilder::from_rect(Rect::from_xywh(x, y, width, height).expect("badge"));
    let pill = rounded(&pill, height / 2.0).unwrap_or(pill);
    let paint = Paint {
        anti_alias: true,
        shader: tiny_skia::Shader::SolidColor(if badge.alert {
            rgb(0xE5, 0x39, 0x35)
        } else {
            rgb(0x8A, 0x8F, 0x96)
        }),
        ..Paint::default()
    };
    pixmap.fill_path(
        &pill,
        &paint,
        FillRule::Winding,
        Transform::identity(),
        None,
    );

    // Glyph coverage blended in white, centered in the pill.
    let (dx, dy) = (
        (x + (width - text_width) / 2.0).round() as i32,
        y.round() as i32,
    );
    let mut cache = ct::SwashCache::new();
    let pixels = pixmap.pixels_mut();
    buffer.draw(
        fonts,
        &mut cache,
        ct::Color::rgb(255, 255, 255),
        |gx, gy, w, h, color| {
            let alpha = u16::from(color.a());
            if alpha == 0 {
                return;
            }
            for py in gy + dy..gy + dy + h as i32 {
                for px in gx + dx..gx + dx + w as i32 {
                    if px < 0 || py < 0 || px >= size as i32 || py >= size as i32 {
                        continue;
                    }
                    let pixel = &mut pixels[(py as u32 * size + px as u32) as usize];
                    let keep = 255 - alpha;
                    let mix = |d: u8| (alpha * 255 + u16::from(d) * keep).div_ceil(255) as u8;
                    let blended = tiny_skia::PremultipliedColorU8::from_rgba(
                        mix(pixel.red()),
                        mix(pixel.green()),
                        mix(pixel.blue()),
                        mix(pixel.alpha()),
                    );
                    if let Some(p) = blended {
                        *pixel = p;
                    }
                }
            }
        },
    );
    pixmap
}

/// Rectangle path with corners of `radius` (a pill when it is half the height).
fn rounded(rect: &tiny_skia::Path, radius: f32) -> Option<tiny_skia::Path> {
    let b = rect.bounds();
    let (l, t, r, btm) = (b.left(), b.top(), b.right(), b.bottom());
    let k = radius * 0.5523;
    let mut p = PathBuilder::new();
    p.move_to(l + radius, t);
    p.line_to(r - radius, t);
    p.cubic_to(r - radius + k, t, r, t + radius - k, r, t + radius);
    p.line_to(r, btm - radius);
    p.cubic_to(r, btm - radius + k, r - radius + k, btm, r - radius, btm);
    p.line_to(l + radius, btm);
    p.cubic_to(l + radius - k, btm, l, btm - radius + k, l, btm - radius);
    p.line_to(l, t + radius);
    p.cubic_to(l, t + radius - k, l + radius - k, t, l + radius, t);
    p.close();
    p.finish()
}

/// Straight (not premultiplied) RGBA.
fn rgba(pixmap: &Pixmap) -> Vec<u8> {
    pixmap
        .pixels()
        .iter()
        .flat_map(|p| {
            let c = p.demultiply();
            [c.red(), c.green(), c.blue(), c.alpha()]
        })
        .collect()
}

/// Icon for the windows (no badge).
pub(crate) fn window_icon() -> Option<iced::window::Icon> {
    iced::window::icon::from_rgba(rgba(&draw_icon(64)), 64, 64).ok()
}

#[cfg(target_os = "linux")]
/// Icons in the tray's format: ARGB32, network byte order.
fn pixmaps(badge: Option<Badge>) -> Vec<ksni::Icon> {
    SIZES
        .iter()
        .map(|&size| {
            let data = rgba(&draw(size, badge))
                .as_chunks::<4>()
                .0
                .iter()
                .flat_map(|&[r, g, b, a]| [a, r, g, b])
                .collect();
            ksni::Icon {
                width: size as i32,
                height: size as i32,
                data,
            }
        })
        .collect()
}

#[cfg(target_os = "linux")]
fn tool_tip(badge: Option<Badge>) -> String {
    match badge {
        Some(Badge { count, alert: true }) => format!("непрочитанных: {count}"),
        Some(Badge { count, .. }) => format!("непрочитанных в чатах без звука: {count}"),
        None => "нет непрочитанных".to_owned(),
    }
}

#[cfg(target_os = "linux")]
#[derive(Debug, Clone)]
pub(crate) enum TrayEvent {
    Ready(TrayHandle),
    /// Click on the icon or "Открыть".
    Activate,
    Quit,
}

#[cfg(target_os = "linux")]
struct Tray {
    icons: Vec<ksni::Icon>,
    tip: String,
    events: mpsc::UnboundedSender<TrayEvent>,
}

#[cfg(target_os = "linux")]
impl ksni::Tray for Tray {
    fn id(&self) -> String {
        "telega".into()
    }

    fn title(&self) -> String {
        "Telega".into()
    }

    fn category(&self) -> ksni::Category {
        ksni::Category::Communications
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        self.icons.clone()
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        ksni::ToolTip {
            title: "Telega".into(),
            description: self.tip.clone(),
            ..Default::default()
        }
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        let _ = self.events.send(TrayEvent::Activate);
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::StandardItem;
        vec![
            StandardItem {
                label: "Открыть Telega".into(),
                activate: Box::new(|t: &mut Self| {
                    let _ = t.events.send(TrayEvent::Activate);
                }),
                ..Default::default()
            }
            .into(),
            ksni::MenuItem::Separator,
            StandardItem {
                label: "Выход".into(),
                activate: Box::new(|t: &mut Self| {
                    let _ = t.events.send(TrayEvent::Quit);
                }),
                ..Default::default()
            }
            .into(),
        ]
    }
}

#[cfg(target_os = "linux")]
/// Remembers the last badge drawn, so a repeat with the same count and
/// alert state can skip the redraw.
#[derive(Default)]
struct BadgeCache(parking_lot::Mutex<Option<Badge>>);

#[cfg(target_os = "linux")]
impl BadgeCache {
    /// Records `badge` as the one now shown; `true` if it differs from
    /// what was recorded before, meaning a redraw is actually needed.
    fn update(&self, badge: Option<Badge>) -> bool {
        let mut last = self.0.lock();
        let changed = *last != badge;
        *last = badge;
        changed
    }
}

#[cfg(target_os = "linux")]
/// Handle to change the icon from the UI; remembers the last badge it
/// drew, so a repeat with the same count and alert state is a no-op.
#[derive(Clone)]
pub(crate) struct TrayHandle {
    handle: ksni::Handle<Tray>,
    drawn: std::sync::Arc<BadgeCache>,
}

#[cfg(target_os = "linux")]
impl std::fmt::Debug for TrayHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("TrayHandle")
    }
}

#[cfg(target_os = "linux")]
impl TrayHandle {
    fn new(handle: ksni::Handle<Tray>) -> Self {
        Self {
            handle,
            drawn: std::sync::Arc::new(BadgeCache::default()),
        }
    }

    /// Redraws the icon for `badge`, unless it already shows it. `pixmaps`
    /// shapes text for three sizes under `font_system`'s write lock, so a
    /// repeat with the same count and alert on every
    /// `UpdateUnreadMessageCount` would waste it; the draw that does run
    /// happens with the D-Bus update, off the caller's thread.
    pub(crate) fn show<M: Send + 'static>(&self, badge: Option<Badge>) -> Task<M> {
        if !self.drawn.update(badge) {
            return Task::none();
        }
        let handle = self.handle.clone();
        Task::future(async move {
            let (icons, tip) = (pixmaps(badge), tool_tip(badge));
            handle
                .update(|t| {
                    t.icons = icons;
                    t.tip = tip;
                })
                .await
        })
        .discard()
    }
}

#[cfg(target_os = "linux")]
/// Registers the tray icon and streams its clicks. Without a tray host in
/// the panel the stream just ends: the client works as before.
/// `assume_sni_available` keeps the service running and retries
/// registration if the panel's `StatusNotifierWatcher` starts after us
/// (e.g. waybar not up yet at login), instead of giving up for the
/// session.
pub(crate) fn run() -> impl Stream<Item = TrayEvent> {
    iced::stream::channel(8, async |mut out| {
        use ksni::TrayMethods;
        let (tx, mut rx) = mpsc::unbounded_channel();
        let tray = Tray {
            icons: pixmaps(None),
            tip: tool_tip(None),
            events: tx,
        };
        let handle = match tray.assume_sni_available(true).spawn().await {
            Ok(handle) => handle,
            Err(e) => {
                eprintln!("значок в трее недоступен: {e}");
                return;
            }
        };
        if out
            .send(TrayEvent::Ready(TrayHandle::new(handle)))
            .await
            .is_err()
        {
            return;
        }
        while let Some(event) = rx.recv().await {
            if out.send(event).await.is_err() {
                return;
            }
        }
    })
}

/// Brings our window forward on Hyprland, off the UI thread: up to two
/// blocking socket round-trips (500 ms timeout each) would otherwise stall
/// `update`. A tray click or a notification click carries no activation
/// token, and Hyprland (without `misc:focus_on_activate`) ignores a focus
/// request without one; its own socket does it. Other compositors: nothing,
/// the regular request stands.
#[cfg(target_os = "linux")]
pub(crate) fn raise_on_hyprland<M: Send + 'static>() -> Task<M> {
    Task::future(async {
        let _ = tokio::task::spawn_blocking(raise_on_hyprland_now).await;
    })
    .discard()
}

/// The blocking half of `raise_on_hyprland`, run on a blocking-pool thread.
#[cfg(target_os = "linux")]
fn raise_on_hyprland_now() {
    use std::io::{Read, Write};
    let (Some(runtime), Some(instance)) = (
        std::env::var_os("XDG_RUNTIME_DIR"),
        std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE"),
    ) else {
        return;
    };
    let socket = std::path::Path::new(&runtime)
        .join("hypr")
        .join(instance)
        .join(".socket.sock");
    let pid = std::process::id();
    let ask = |command: String| -> bool {
        let Ok(mut stream) = std::os::unix::net::UnixStream::connect(&socket) else {
            return false;
        };
        let _ = stream.set_read_timeout(Some(std::time::Duration::from_millis(500)));
        let mut reply = String::new();
        stream.write_all(command.as_bytes()).is_ok()
            && stream.read_to_string(&mut reply).is_ok()
            && reply.trim() == "ok"
    };
    // Lua dispatchers (Hyprland 0.56+), then the older syntax.
    if !ask(format!(
        "dispatch hl.dsp.focus({{ window = \"pid:{pid}\" }})"
    )) {
        ask(format!("dispatch focuswindow pid:{pid}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "linux")]
    #[test]
    fn a_repeated_badge_is_not_redrawn() {
        let cache = BadgeCache::default();
        let alert = Some(Badge {
            count: 3,
            alert: true,
        });
        assert!(cache.update(alert), "the first draw is always needed");
        assert!(!cache.update(alert), "same count and alert: no redraw");
        let muted = Some(Badge {
            count: 3,
            alert: false,
        });
        assert!(cache.update(muted), "a different alert state redraws");
        assert!(cache.update(None), "clearing the badge redraws");
    }

    #[test]
    fn badge_is_red_for_notifying_chats_and_gray_for_muted_only() {
        assert_eq!(badge(0, 0), None);
        assert_eq!(
            badge(12, 0),
            Some(Badge {
                count: 12,
                alert: false
            })
        );
        // Muted messages are not added to the red count.
        assert_eq!(
            badge(12, 3),
            Some(Badge {
                count: 3,
                alert: true
            })
        );
    }

    #[test]
    fn long_counts_are_shortened() {
        assert_eq!(label(999), "999");
        assert_eq!(label(1500), "1K");
        assert_eq!(label(250_000), "99K+");
    }

    #[test]
    fn badge_is_drawn_in_its_color_with_white_digits() {
        let size = 64;
        let px = |p: &Pixmap, x: u32, y: u32| p.pixel(x, y).unwrap().demultiply();
        let plain = draw(size, None);
        let red = draw(
            size,
            Some(Badge {
                count: 7,
                alert: true,
            }),
        );
        let gray = draw(
            size,
            Some(Badge {
                count: 7,
                alert: false,
            }),
        );
        // Pill edge (bottom right, clear of the digit) takes the badge color.
        let (r, g) = (px(&red, 60, 50), px(&gray, 60, 50));
        assert!(r.red() > 200 && r.green() < 90, "{r:?}");
        assert!(
            g.red() == g.green() || g.red().abs_diff(g.green()) < 10,
            "{g:?}"
        );
        assert_ne!(px(&plain, 60, 50), r);
        // Some white digit pixels inside the pill.
        let white = (36..64)
            .flat_map(|y| (40..64).map(move |x| (x, y)))
            .filter(|&(x, y)| {
                let c = px(&red, x, y);
                c.red() > 240 && c.green() > 240 && c.blue() > 240
            })
            .count();
        assert!(white > 20, "{white} white pixels");
    }
}
