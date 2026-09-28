//! Profile photos of chats and users: small (TDLib's 160 px `small` file),
//! decoded once into a round 64 px picture and kept in a bounded cache.
//! Without a photo, a colored circle with initials.

use std::collections::{HashMap, HashSet};

use iced::widget::{container, image, sensor, text};
use iced::{Element, Task};
use tdlib_rs::types::File;

use super::media::FileState;
use super::{App, Msg};
use crate::td;

/// Side of the decoded picture: sharp up to 32 px at 2× scale.
const SIDE: u32 = 64;
/// Decoded pictures kept (16 KB each).
const MAX_CACHED: usize = 600;

/// Whose picture.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Peer {
    Chat(i64),
    User(i64),
}

#[derive(Default)]
pub(crate) struct Avatars {
    /// Photo file of each peer that has one.
    files: HashMap<Peer, i32>,
    /// The peer owning each file, for O(1) lookup when a download finishes
    /// (instead of scanning every peer known to the client).
    owners: HashMap<i32, Peer>,
    /// Decoded picture and the clock tick of its last use, for eviction of
    /// the least recently *used* picture: a chat pinned at the top of the
    /// list must not lose its photo just because hundreds of strangers'
    /// avatars were decoded since.
    decoded: HashMap<i32, (image::Handle, u64)>,
    clock: u64,
    pub(crate) decoding: HashSet<i32>,
}

impl Avatars {
    pub(crate) fn handle(&self, peer: Peer) -> Option<&image::Handle> {
        let file_id = self.files.get(&peer)?;
        self.decoded.get(file_id).map(|(h, _)| h)
    }

    fn store(&mut self, file_id: i32, handle: image::Handle) {
        self.decoding.remove(&file_id);
        self.clock += 1;
        self.decoded.insert(file_id, (handle, self.clock));
        while self.decoded.len() > MAX_CACHED {
            let Some((&oldest, _)) = self.decoded.iter().min_by_key(|(_, e)| e.1) else {
                break;
            };
            self.decoded.remove(&oldest);
        }
    }

    /// Marks `file_id` as just used, protecting it from eviction a while
    /// longer. Returns whether it was actually cached: a miss means the
    /// picture was evicted (or never decoded) and must be fetched again.
    fn touch(&mut self, file_id: i32) -> bool {
        self.clock += 1;
        let clock = self.clock;
        match self.decoded.get_mut(&file_id) {
            Some(entry) => {
                entry.1 = clock;
                true
            }
            None => false,
        }
    }
}

impl App {
    /// A peer's photo changed (or became known). The picture itself is
    /// fetched lazily, only once actually shown (`avatar_shown`, driven by
    /// the sensor in `avatar`): a contact list may know thousands of users
    /// whose picture is never displayed.
    pub(crate) fn set_avatar(&mut self, peer: Peer, small: Option<&File>) -> Task<Msg> {
        if let Some(old) = self.session.avatars.files.remove(&peer) {
            self.session.avatars.owners.remove(&old);
        }
        let Some(file) = small else {
            return Task::none();
        };
        self.session.avatars.files.insert(peer, file.id);
        self.session.avatars.owners.insert(file.id, peer);
        if !self.session.files.get(&file.id).is_some_and(|f| f.done) {
            self.session.files.insert(file.id, FileState::from(file));
        }
        Task::none()
    }

    /// A picture's sensor reported it on screen: fetch it if it was never
    /// downloaded, or fetch it again if it was since evicted from the
    /// decoded-picture cache.
    pub(crate) fn avatar_shown(&mut self, peer: Peer) -> Task<Msg> {
        let Some(&file_id) = self.session.avatars.files.get(&peer) else {
            return Task::none();
        };
        if self.session.avatars.touch(file_id) {
            return Task::none();
        }
        self.fetch_avatar(file_id)
    }

    fn fetch_avatar(&mut self, file_id: i32) -> Task<Msg> {
        match self.session.files.get(&file_id) {
            Some(f) if f.done => self.decode_avatar(file_id),
            Some(f) if f.downloading => Task::none(),
            // Low priority: pictures wait behind what the user opened.
            _ => Task::perform(
                td::download_file(self.session.client_id, file_id, 1),
                Msg::Done,
            ),
        }
    }

    /// Called for every finished download.
    pub(crate) fn avatar_file_done(&mut self, file_id: i32) -> Task<Msg> {
        if self.session.avatars.owners.contains_key(&file_id) {
            self.decode_avatar(file_id)
        } else {
            Task::none()
        }
    }

    fn decode_avatar(&mut self, file_id: i32) -> Task<Msg> {
        let Some(path) = self.session.files.get(&file_id).map(|f| f.path.clone()) else {
            return Task::none();
        };
        if !self.session.avatars.decoding.insert(file_id) {
            return Task::none();
        }
        Task::perform(
            async move {
                let _permit = super::media::DECODES
                    .acquire()
                    .await
                    .map_err(|e| e.to_string())?;
                tokio::task::spawn_blocking(move || decode(&path))
                    .await
                    .map_err(|e| e.to_string())
                    .and_then(|r| r)
            },
            move |r| Msg::AvatarDecoded(file_id, r),
        )
    }

    pub(crate) fn avatar_decoded(&mut self, file_id: i32, result: Result<Vec<u8>, String>) {
        match result {
            Ok(rgba) => self
                .session
                .avatars
                .store(file_id, image::Handle::from_rgba(SIDE, SIDE, rgba)),
            // A broken picture falls back to initials.
            Err(_) => {
                self.session.avatars.decoding.remove(&file_id);
            }
        }
    }

    /// Round picture of `size` px, or initials on a color of the peer. A
    /// sensor drives lazy loading: fetched the first time it comes on
    /// screen, and re-fetched if it was since evicted from the cache.
    pub(crate) fn avatar<'a>(&self, peer: Peer, name: &str, size: f32) -> Element<'a, Msg> {
        let picture: Element<'a, Msg> = if let Some(handle) = self.session.avatars.handle(peer) {
            image(handle.clone()).width(size).height(size).into()
        } else {
            let id = match peer {
                Peer::Chat(id) | Peer::User(id) => id,
            };
            let color = super::look::person_color(id);
            container(
                text(initials(name))
                    .size(size * 0.4)
                    .color(iced::Color::WHITE),
            )
            .center_x(size)
            .center_y(size)
            .style(move |_| container::Style {
                background: Some(color.into()),
                border: iced::border::rounded(size / 2.0),
                ..container::Style::default()
            })
            .into()
        };
        sensor(picture)
            .on_show(move |_| Msg::AvatarShown(peer))
            .into()
    }
}

/// "Игорь Петров" → "ИП", "Rust чат" → "RЧ", "Маша" → "М".
pub(crate) fn initials(name: &str) -> String {
    name.split_whitespace()
        .filter_map(|w| w.chars().find(|c| c.is_alphanumeric()))
        .take(2)
        .flat_map(char::to_uppercase)
        .collect()
}

/// Square-cropped, scaled and cut into a circle with a soft edge.
pub(crate) fn decode(path: &str) -> Result<Vec<u8>, String> {
    let img = super::media::open_image(path)?;
    let side = img.width().min(img.height());
    let img = img
        .crop_imm(
            (img.width() - side) / 2,
            (img.height() - side) / 2,
            side,
            side,
        )
        .resize_exact(SIDE, SIDE, ::image::imageops::FilterType::Triangle);
    let mut rgba = img.into_rgba8();
    let center = SIDE as f32 / 2.0;
    for (x, y, pixel) in rgba.enumerate_pixels_mut() {
        let d = ((x as f32 + 0.5 - center).powi(2) + (y as f32 + 0.5 - center).powi(2)).sqrt();
        let coverage = (center - d + 0.5).clamp(0.0, 1.0);
        pixel.0[3] = (f32::from(pixel.0[3]) * coverage) as u8;
    }
    Ok(rgba.into_raw())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initials_take_first_letters_of_two_words() {
        assert_eq!(initials("Игорь Петров"), "ИП");
        assert_eq!(initials("маша"), "М");
        assert_eq!(initials("🎰 Нет времени"), "НВ");
        assert_eq!(initials(""), "");
    }

    #[test]
    fn decoded_avatar_is_round() {
        let path = std::env::temp_dir().join(format!("telega-avatar-{}.png", std::process::id()));
        ::image::RgbaImage::from_pixel(200, 120, ::image::Rgba([10, 200, 30, 255]))
            .save(&path)
            .unwrap();
        let rgba = decode(&path.to_string_lossy()).unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!(rgba.len(), (SIDE * SIDE * 4) as usize);
        let alpha = |x: u32, y: u32| rgba[((y * SIDE + x) * 4 + 3) as usize];
        assert_eq!(alpha(0, 0), 0, "corner is transparent");
        assert_eq!(alpha(SIDE / 2, SIDE / 2), 255, "center is opaque");
    }

    #[test]
    fn cache_is_bounded() {
        let mut avatars = Avatars::default();
        for id in 0..(MAX_CACHED as i32 + 50) {
            avatars.store(id, image::Handle::from_rgba(1, 1, vec![0; 4]));
        }
        assert_eq!(avatars.decoded.len(), MAX_CACHED);
        assert!(!avatars.decoded.contains_key(&0), "oldest went first");
    }
}
