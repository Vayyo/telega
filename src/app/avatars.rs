//! Profile photos of chats and users: small 64 px images for lists and
//! lazily decoded 256 px images for profile cards. Without a photo, initials.

use std::collections::{HashMap, HashSet};

use iced::widget::{container, image, sensor, text};
use iced::{Element, Task};
use tdlib_rs::types::File;

use super::{App, Msg};

/// Side of the decoded picture: sharp up to 32 px at 2× scale. The disk
/// cache sizes its entries by it (`avatar_cache::ENTRY_LEN`).
pub(crate) const SIDE: u32 = 64;
/// Decoded pictures kept (16 KB each).
const MAX_CACHED: usize = 600;
/// Round profile pictures stay sharp at 96 px on 2× displays.
const PROFILE_SIDE: u32 = 256;
/// At most 8 MiB of decoded profile pictures.
const MAX_PROFILE_CACHED: usize = 32;

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
    /// Number of peers referring to a small file, for O(1) lookup when a
    /// download finishes without losing shared files on photo removal.
    owners: HashMap<i32, usize>,
    /// Decoded picture, last-use clock tick, and content digest for an own
    /// photo decoded before the client's account identity is known.
    /// A chat pinned at the top must not lose its picture just because
    /// hundreds of strangers' avatars were decoded since.
    decoded: HashMap<i32, (image::Handle, u64, [u8; 32])>,
    clock: u64,
    pub(crate) decoding: HashSet<i32>,
    /// Big photo files and their live reference counts (multiple peers may
    /// share a TDLib file id).
    profile_files: HashMap<Peer, i32>,
    profile_owners: HashMap<i32, usize>,
    profile_decoded: HashMap<i32, (image::Handle, u64)>,
    profile_decoding: HashSet<i32>,
    /// A profile only downloads after its large avatar has actually appeared.
    profile_wanted: HashSet<i32>,
    profile_failed: HashSet<i32>,
}

impl Avatars {
    pub(crate) fn handle(&self, peer: Peer) -> Option<&image::Handle> {
        let file_id = self.files.get(&peer)?;
        self.decoded.get(file_id).map(|(h, _, _)| h)
    }

    pub(crate) fn portrait(&self, peer: Peer) -> Option<(&image::Handle, [u8; 32])> {
        let file_id = self.files.get(&peer)?;
        self.decoded.get(file_id).map(|(h, _, digest)| (h, *digest))
    }

    fn store(&mut self, file_id: i32, handle: image::Handle, digest: [u8; 32]) {
        self.decoding.remove(&file_id);
        self.clock += 1;
        self.decoded.insert(file_id, (handle, self.clock, digest));
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

    fn profile_handle(&self, peer: Peer) -> Option<&image::Handle> {
        let file_id = self.profile_files.get(&peer)?;
        self.profile_decoded.get(file_id).map(|(h, _)| h)
    }

    fn touch_profile(&mut self, file_id: i32) -> bool {
        let Some(entry) = self.profile_decoded.get_mut(&file_id) else {
            return false;
        };
        self.clock += 1;
        entry.1 = self.clock;
        true
    }

    fn store_profile(&mut self, file_id: i32, handle: image::Handle) {
        self.profile_decoding.remove(&file_id);
        self.profile_failed.remove(&file_id);
        self.clock += 1;
        self.profile_decoded.insert(file_id, (handle, self.clock));
        while self.profile_decoded.len() > MAX_PROFILE_CACHED {
            let Some((&oldest, _)) = self.profile_decoded.iter().min_by_key(|(_, e)| e.1) else {
                break;
            };
            self.profile_decoded.remove(&oldest);
        }
    }
}

impl App {
    /// A peer's photo changed (or became known). The picture itself is
    /// fetched lazily, only once actually shown (`avatar_shown`, driven by
    /// the sensor in `avatar`): a contact list may know thousands of users
    /// whose picture is never displayed.
    pub(crate) fn set_avatar(&mut self, peer: Peer, small: Option<&File>) -> Task<Msg> {
        let old = self.session.avatars.files.get(&peer).copied();
        let same = small.is_some_and(|file| old == Some(file.id));
        if matches!(peer, Peer::User(id) if self.session.my_id.is_none_or(|my_id| my_id == id)
            && self.settings.accounts.iter().any(|a| a.slot == self.session.slot && a.user_id == Some(id)))
            && !same
            && (old.is_some() || small.is_none())
        {
            self.account_portraits.remove(&self.session.slot);
            if let Some(account) = self
                .settings
                .accounts
                .iter_mut()
                .find(|a| a.slot == self.session.slot)
                && account.avatar_digest.take().is_some()
            {
                self.save_settings();
            }
        }
        if old != small.map(|file| file.id) {
            if let Some(old) = self.session.avatars.files.remove(&peer)
                && let Some(count) = self.session.avatars.owners.get_mut(&old)
            {
                *count -= 1;
                if *count == 0 {
                    self.session.avatars.owners.remove(&old);
                }
            }
            if let Some(file) = small {
                self.session.avatars.files.insert(peer, file.id);
                *self.session.avatars.owners.entry(file.id).or_default() += 1;
            }
        }
        if let Some(file) = small {
            self.remember_file(file);
        }
        Task::none()
    }

    /// TDLib's full-sized peer photo is separate from the 64 px portrait
    /// cache: it never changes account portrait digests or disk entries.
    pub(crate) fn set_profile_avatar(&mut self, peer: Peer, big: Option<&File>) {
        let previous = self.session.avatars.profile_files.get(&peer).copied();
        let current = big.map(|file| file.id);
        if previous != current {
            if let Some(old) = self.session.avatars.profile_files.remove(&peer) {
                let avatars = &mut self.session.avatars;
                if let Some(count) = avatars.profile_owners.get_mut(&old) {
                    *count -= 1;
                    if *count == 0 {
                        avatars.profile_owners.remove(&old);
                        avatars.profile_decoded.remove(&old);
                        avatars.profile_wanted.remove(&old);
                        avatars.profile_failed.remove(&old);
                    }
                }
            }
            if let Some(file) = big {
                self.session.avatars.profile_files.insert(peer, file.id);
                *self
                    .session
                    .avatars
                    .profile_owners
                    .entry(file.id)
                    .or_default() += 1;
            }
        }
        if let Some(file) = big {
            self.remember_file(file);
        }
    }

    /// Only large avatars opt into downloading/decoding the big TDLib file.
    /// Also fetch the small file for an immediate fallback if big fails.
    pub(crate) fn profile_avatar_shown(&mut self, peer: Peer) -> Task<Msg> {
        let small = self.avatar_shown(peer);
        let Some(&file_id) = self.session.avatars.profile_files.get(&peer) else {
            return small;
        };
        self.session.avatars.profile_wanted.insert(file_id);
        let big = if self.session.avatars.touch_profile(file_id)
            || self.session.avatars.profile_failed.contains(&file_id)
        {
            Task::none()
        } else {
            self.fetch_profile_avatar(file_id)
        };
        Task::batch([small, big])
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

    /// An explicit action — a person's card (`CardMsg::Open`), a chat's
    /// profile (`open_profile`) — is the user asking for this peer, so a
    /// picture whose download gave up is asked for again. The sensor
    /// (`avatar_shown`) never does this: it runs on every display, and a
    /// picture that fails each time would otherwise be requested from
    /// TDLib over and over without the user ever asking for it.
    pub(super) fn retry_avatar(&mut self, peer: Peer) {
        for file_id in [
            self.session.avatars.files.get(&peer).copied(),
            self.session.avatars.profile_files.get(&peer).copied(),
        ]
        .into_iter()
        .flatten()
        {
            if let Some(state) = self.session.files.get_mut(&file_id) {
                state.failed = false;
            }
            self.session.avatars.profile_failed.remove(&file_id);
        }
    }

    fn fetch_avatar(&mut self, file_id: i32) -> Task<Msg> {
        match self.session.files.get(&file_id) {
            Some(f) if f.done => self.decode_avatar(file_id),
            // In flight, or failed before: the sensor never asks again.
            Some(f) if f.downloading || f.failed => Task::none(),
            // Low priority: pictures wait behind what the user opened.
            _ => self.request_download(file_id, 1),
        }
    }

    /// Called for every finished download; big photos are decoded only
    /// after a profile sensor has requested them.
    pub(crate) fn avatar_file_done(&mut self, file_id: i32) -> Task<Msg> {
        let small = if self.session.avatars.owners.contains_key(&file_id) {
            self.decode_avatar(file_id)
        } else {
            Task::none()
        };
        let big = if self.session.avatars.profile_wanted.contains(&file_id)
            && self.session.avatars.profile_owners.contains_key(&file_id)
        {
            self.decode_profile_avatar(file_id)
        } else {
            Task::none()
        };
        Task::batch([small, big])
    }

    fn fetch_profile_avatar(&mut self, file_id: i32) -> Task<Msg> {
        match self.session.files.get(&file_id) {
            Some(f) if f.done => self.decode_profile_avatar(file_id),
            Some(f) if f.downloading || f.failed => Task::none(),
            _ => self.request_download(file_id, 1),
        }
    }

    fn decode_profile_avatar(&mut self, file_id: i32) -> Task<Msg> {
        let Some(path) = self.session.files.get(&file_id).map(|f| f.path.clone()) else {
            return Task::none();
        };
        if self.session.avatars.profile_failed.contains(&file_id)
            || self.session.avatars.profile_decoded.contains_key(&file_id)
            || !self.session.avatars.profile_decoding.insert(file_id)
        {
            return Task::none();
        }
        let client_id = self.session.client_id;
        Task::perform(
            async move {
                super::viewer::decode_with_viewer_slot(&super::media::DECODES, move || {
                    decode_profile(&path)
                })
                .await
                .and_then(|r| r)
            },
            move |r| Msg::ProfileAvatarDecoded(client_id, file_id, r),
        )
    }

    pub(crate) fn profile_avatar_decoded(&mut self, file_id: i32, result: Result<Vec<u8>, String>) {
        self.session.avatars.profile_decoding.remove(&file_id);
        if !self.session.avatars.profile_owners.contains_key(&file_id) {
            return;
        }
        match result {
            Ok(rgba) => self.session.avatars.store_profile(
                file_id,
                image::Handle::from_rgba(PROFILE_SIDE, PROFILE_SIDE, rgba),
            ),
            Err(_) => {
                self.session.avatars.profile_failed.insert(file_id);
            }
        }
    }

    fn decode_avatar(&mut self, file_id: i32) -> Task<Msg> {
        let Some(path) = self.session.files.get(&file_id).map(|f| f.path.clone()) else {
            return Task::none();
        };
        if !self.session.avatars.decoding.insert(file_id) {
            return Task::none();
        }
        let client_id = self.session.client_id;
        Task::perform(
            async move {
                super::viewer::decode_with_viewer_slot(&super::media::DECODES, move || {
                    decode_cached(&path)
                })
                .await
                .and_then(|r| r)
            },
            move |r| Msg::AvatarDecoded(client_id, file_id, r),
        )
    }

    pub(crate) fn avatar_decoded(
        &mut self,
        file_id: i32,
        result: Result<(Vec<u8>, [u8; 32]), String>,
    ) {
        match result {
            Ok((rgba, digest)) => {
                let handle = image::Handle::from_rgba(SIDE, SIDE, rgba);
                if self.session.avatars.owners.contains_key(&file_id)
                    && let Some(account) = self.settings.accounts.iter_mut().find(|a| {
                        a.slot == self.session.slot
                            && a.user_id.is_some_and(|id| {
                                self.session.my_id.is_none_or(|my_id| my_id == id)
                                    && self.session.avatars.files.get(&Peer::User(id))
                                        == Some(&file_id)
                            })
                    })
                {
                    self.account_portraits
                        .insert(self.session.slot, handle.clone());
                    if account.avatar_digest != Some(digest) {
                        account.avatar_digest = Some(digest);
                        self.save_settings();
                    }
                }
                self.session.avatars.store(file_id, handle, digest);
            }
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
        let profile = size > SIDE as f32;
        let picture: Element<'a, Msg> = if let Some(handle) = (profile
            .then(|| self.session.avatars.profile_handle(peer))
            .flatten())
        .or_else(|| self.session.avatars.handle(peer))
        .or_else(|| {
            let Peer::User(id) = peer else {
                return None;
            };
            self.account_portraits.get(&self.session.slot).filter(|_| {
                self.session.my_id.is_none_or(|my_id| my_id == id)
                    && self
                        .settings
                        .accounts
                        .iter()
                        .any(|a| a.slot == self.session.slot && a.user_id == Some(id))
            })
        }) {
            image(handle.clone()).width(size).height(size).into()
        } else {
            let id = match peer {
                Peer::Chat(id) | Peer::User(id) => id,
            };
            placeholder(id, name, size)
        };
        if profile {
            sensor(picture)
                .key((
                    peer,
                    self.session.avatars.files.get(&peer).copied(),
                    self.session.avatars.profile_files.get(&peer).copied(),
                ))
                .on_show(move |_| Msg::ProfileAvatarShown(peer))
                .into()
        } else {
            sensor(picture)
                .key((peer, self.session.avatars.files.get(&peer).copied()))
                .on_show(move |_| Msg::AvatarShown(peer))
                .into()
        }
    }

    /// An inactive account has no live TDLib peer or avatar sensor.
    pub(crate) fn account_avatar<'a>(
        &self,
        slot: u32,
        user_id: i64,
        name: &str,
        size: f32,
    ) -> Element<'a, Msg> {
        match self.account_portraits.get(&slot) {
            Some(handle) => image(handle.clone()).width(size).height(size).into(),
            None => placeholder(user_id, name, size),
        }
    }
}

fn placeholder<'a>(id: i64, name: &str, size: f32) -> Element<'a, Msg> {
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
}

/// "Игорь Петров" → "ИП", "Rust чат" → "RЧ", "Маша" → "М".
pub(crate) fn initials(name: &str) -> String {
    name.split_whitespace()
        .filter_map(|w| w.chars().find(|c| c.is_alphanumeric()))
        .take(2)
        .flat_map(char::to_uppercase)
        .collect()
}

/// Square-cropped, scaled and cut into a circle with a soft edge. Only
/// tests call it: the client decodes through `decode_cached`, which decides
/// by the file's bytes whether there is anything to decode at all.
#[cfg(test)]
pub(crate) fn decode(path: &str) -> Result<Vec<u8>, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("фото: {e}"))?;
    decode_bytes(&bytes)
}

/// Decodes the bytes of a picture file into the round `SIDE`×`SIDE` RGBA
/// picture the cache stores.
fn decode_bytes(bytes: &[u8]) -> Result<Vec<u8>, String> {
    let img = super::media::open_image_bytes(bytes)?;
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

/// HD pictures stay in memory only; the legacy 64 px content-addressed
/// on-disk entries must remain byte-for-byte compatible with older sessions.
fn decode_profile(path: &str) -> Result<Vec<u8>, String> {
    let img = super::media::open_image(path)?;
    let side = img.width().min(img.height());
    let img = img
        .crop_imm(
            (img.width() - side) / 2,
            (img.height() - side) / 2,
            side,
            side,
        )
        .resize_exact(
            PROFILE_SIDE,
            PROFILE_SIDE,
            ::image::imageops::FilterType::Triangle,
        );
    let mut rgba = img.into_rgba8();
    let center = PROFILE_SIDE as f32 / 2.0;
    for (x, y, pixel) in rgba.enumerate_pixels_mut() {
        let d = ((x as f32 + 0.5 - center).powi(2) + (y as f32 + 0.5 - center).powi(2)).sqrt();
        pixel.0[3] = (f32::from(pixel.0[3]) * (center - d + 0.5).clamp(0.0, 1.0)) as u8;
    }
    Ok(rgba.into_raw())
}

/// The picture of `path`, decoded in this session. The file is read once:
/// its bytes address the entry in the on-disk cache, so a picture decoded
/// by an earlier session, or decoded for another `file_id` carrying the
/// same bytes, comes back without touching the decoder.
pub(crate) fn decode_cached(path: &str) -> Result<(Vec<u8>, [u8; 32]), String> {
    let bytes = std::fs::read(path).map_err(|e| format!("фото: {e}"))?;
    let digest = super::avatar_cache::hash(&bytes);
    if let Some(rgba) = super::avatar_cache::load(&digest) {
        return Ok((rgba, digest));
    }
    let rgba = decode_bytes(&bytes)?;
    // Not being able to cache must not stop the picture from being shown:
    // the next session decodes the file again, as it always did.
    let _ = super::avatar_cache::store(&digest, &rgba);
    Ok((rgba, digest))
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
            avatars.store(id, image::Handle::from_rgba(1, 1, vec![0; 4]), [0; 32]);
        }
        assert_eq!(avatars.decoded.len(), MAX_CACHED);
        assert!(!avatars.decoded.contains_key(&0), "oldest went first");
    }

    /// A 96 px profile image must use the big local photo rather than
    /// enlarging the 64 px thumbnail: narrow alternating bands disappear
    /// when the thumbnail is downsampled and then enlarged.
    #[tokio::test]
    async fn requested_changes_profile_avatar_at_96_preserves_fine_photo_detail() {
        use iced::advanced::renderer::Headless;
        use iced::futures::StreamExt;
        use iced::{Size, Theme, mouse};
        use iced_runtime::core::renderer::Style;
        use iced_runtime::user_interface::{Cache, UserInterface};
        use iced_runtime::{Action, task::into_stream};
        use serde_json::json;

        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        struct Photo(std::path::PathBuf);
        impl Drop for Photo {
            fn drop(&mut self) {
                let _ = std::fs::remove_file(&self.0);
            }
        }
        let photo = Photo(std::env::temp_dir().join(format!(
            "telega-profile-detail-{}-{}.png",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        )));
        // At 96 px each band is a pixel wide, but the 64 px decoder cannot
        // resolve its two-pixel period. The central scan avoids the round edge.
        let source =
            ::image::DynamicImage::ImageRgb8(::image::RgbImage::from_fn(192, 192, |x, _| {
                let value = if (x / 2) % 2 == 0 { 0 } else { 255 };
                ::image::Rgb([value, value, value])
            }));
        source.save(&photo.0).unwrap();
        let contrast = |rgba: &[u8], side: usize| -> f32 {
            let y = side / 2;
            let start = side / 4;
            let end = side * 3 / 4;
            let total: u32 = (start..end - 1)
                .map(|x| {
                    let a = rgba[(y * side + x) * 4];
                    let b = rgba[(y * side + x + 1) * 4];
                    a.abs_diff(b) as u32
                })
                .sum();
            total as f32 / (end - start - 1) as f32
        };
        let ideal = source
            .resize_exact(96, 96, ::image::imageops::FilterType::Triangle)
            .into_rgba8();
        let thumbnail = source
            .resize_exact(64, 64, ::image::imageops::FilterType::Triangle)
            .resize_exact(96, 96, ::image::imageops::FilterType::Triangle)
            .into_rgba8();
        let ideal_detail = contrast(ideal.as_raw(), 96);
        let thumbnail_detail = contrast(thumbnail.as_raw(), 96);
        assert!(
            ideal_detail > thumbnail_detail + 35.0,
            "synthetic photo must distinguish HD detail from a 64 px upscale"
        );

        let mut app = super::super::tests::app();
        let peer = Peer::Chat(829);
        let small = super::super::sandbox::local_file(28_291, &photo.0);
        let big = super::super::sandbox::local_file(28_292, &photo.0);
        super::super::tests::td(
            &mut app,
            json!({"@type": "updateChatPhoto", "chat_id": 829, "photo": {
                "@type": "chatPhotoInfo", "small": small, "big": big,
                "minithumbnail": null, "has_animation": false, "is_personal": false
            }}),
        );

        let mut renderer = super::super::sandbox::renderer();
        let mut messages = Vec::new();
        {
            let mut ui = UserInterface::build(
                app.avatar(peer, "Photo", 96.0),
                Size::new(96.0, 96.0),
                Cache::default(),
                &mut renderer,
            );
            let _ = ui.update(
                &[iced::Event::Window(iced::window::Event::RedrawRequested(
                    std::time::Instant::now(),
                ))],
                mouse::Cursor::Unavailable,
                &mut renderer,
                &mut iced_runtime::core::clipboard::Null,
                &mut messages,
            );
        }
        assert!(
            !messages.is_empty(),
            "the visible avatar requests its photo"
        );
        for message in messages {
            if let Some(stream) = into_stream(app.update(message)) {
                for action in stream.collect::<Vec<_>>().await {
                    if let Action::Output(decoded) = action {
                        let _ = app.update(decoded);
                    }
                }
            }
        }
        let mut pixels = Vec::new();
        for _ in 0..4 {
            let mut ui = UserInterface::build(
                app.avatar(peer, "Photo", 96.0),
                Size::new(96.0, 96.0),
                Cache::default(),
                &mut renderer,
            );
            ui.draw(
                &mut renderer,
                &Theme::Dark,
                &Style::default(),
                mouse::Cursor::Unavailable,
            );
            pixels = renderer.screenshot(Size::new(96, 96), 1.0, Theme::Dark.palette().background);
        }
        let observed = contrast(&pixels, 96);
        assert!(
            observed > (ideal_detail + thumbnail_detail) / 2.0,
            "96 px avatar should retain narrow bands: observed {observed:.1}, \
             HD reference {ideal_detail:.1}, 64 px upscale {thumbnail_detail:.1}"
        );
    }
}
