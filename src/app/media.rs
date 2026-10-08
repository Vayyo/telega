//! Photos and files: what a message carries, TDLib file state, decoded image
//! cache, sending local files.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use base64::Engine;
use iced::Task;
use iced::widget::image;
use tdlib_rs::enums::{MessageContent, StickerFormat, ThumbnailFormat};
use tdlib_rs::types::{File, Minithumbnail, Photo};

use super::{Msg, WinId};
use crate::td;

/// Largest photo side shown in a bubble, px.
pub(crate) const PHOTO_MAX_W: f32 = 400.0;
pub(crate) const PHOTO_MAX_H: f32 = 480.0;
/// Largest side and decoder allocation accepted for an image file.
const MAX_IMAGE_SIDE: u32 = 4096;
const MAX_IMAGE_ALLOC: u64 = 64 * 1024 * 1024;
/// Decoded images kept in memory at most, bytes (RGBA).
const IMAGE_BUDGET: usize = 64 * 1024 * 1024;
const PHOTO_IDLE: std::time::Duration = std::time::Duration::from_secs(300);

/// Stable identity of one picture widget within one window. A sensor can be
/// removed or change its key without ever publishing `on_hide`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum PhotoOwner {
    Message(i64, i64, PhotoKind),
    Picker(super::picker::Tab, usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum PhotoKind {
    Photo,
    Sticker,
    Thumbnail,
    Link,
}

/// One account/session and one window widget-tree incarnation. Every sensor
/// captures this value so queued callbacks cannot mutate a later incarnation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct PhotoEpoch {
    client: i32,
    generation: u64,
}

/// Read-only snapshot of currently constructed media sources. Shared by
/// immediate show validation and periodic removal reconciliation.
struct PhotoSources<'a> {
    window: WinId,
    pane: &'a super::pane::ChatPane,
    stickers: &'a super::picker::Catalog,
    video: Option<&'a super::video::VideoPlayback>,
    playback: &'a super::playback::Playback,
}

impl PhotoSources<'_> {
    fn contains(&self, owner: PhotoOwner, file_id: i32, range: (usize, usize)) -> bool {
        match owner {
            PhotoOwner::Message(chat_id, message_id, kind) => {
                if self.pane.chat_id != Some(chat_id)
                    || self.pane.forward.is_some()
                    || self.pane.search.as_ref().is_some_and(|search| {
                        search.results.is_some()
                            || search.paging.pending.is_some()
                            || search.paging.error.is_some()
                    })
                {
                    return false;
                }
                self.pane.messages[range.0..range.1].iter().any(|message| {
                    message.id == message_id
                        && match kind {
                            PhotoKind::Photo => matches!(&message.media, Some(Media::Photo { file_id: id, spoiler, .. })
                                if *id == file_id && (!*spoiler || self.pane.revealed.contains(&message.id))),
                            PhotoKind::Sticker => matches!(&message.media, Some(Media::Sticker { file_id: id, kind: Motion::Still, .. }) if *id == file_id),
                            PhotoKind::Thumbnail => match &message.media {
                                Some(Media::Video { file_id: video_id, thumb: Some(id), spoiler, .. }) =>
                                    *id == file_id && (!*spoiler || self.pane.revealed.contains(&message.id))
                                        && !self.video.is_some_and(|video| video.window == self.window
                                            && video.chat_id == chat_id && video.message_id == message_id
                                            && video.file_id == *video_id && !video.expanded),
                                Some(Media::Animation { file_id: animation_id, thumb: Some(id), spoiler, .. }) =>
                                    *id == file_id && (!*spoiler || self.pane.revealed.contains(&message.id))
                                        && self.playback.frame(*animation_id).is_none(),
                                _ => false,
                            },
                            PhotoKind::Link => matches!(message.extra.as_deref(), Some(super::extra::Extra::Link { thumb: Some(id), .. }) if *id == file_id),
                        }
                })
            }
            PhotoOwner::Picker(tab, index) => {
                if self.pane.picker != Some(tab) {
                    return false;
                }
                let stickers = match tab {
                    super::picker::Tab::Emoji => None,
                    super::picker::Tab::Recent => self.stickers.recent.as_deref(),
                    super::picker::Tab::Set(set) => {
                        self.stickers.set_stickers.get(&set).map(Vec::as_slice)
                    }
                };
                stickers
                    .and_then(|list| list.get(index))
                    .and_then(|sticker| sticker.picture.as_ref())
                    .is_some_and(|picture| picture.id == file_id)
            }
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) enum Media {
    Photo {
        /// Tiny inline preview, shown blurred until the photo is loaded.
        mini: Option<image::Handle>,
        file_id: i32,
        width: u32,
        height: u32,
        /// Hidden behind a blur until clicked.
        spoiler: bool,
    },
    Document {
        file_id: i32,
        name: String,
        size: i64,
    },
    Voice {
        file_id: i32,
        duration: i32,
        /// Loudness bars 0..=31.
        waveform: Vec<u8>,
    },
    Sticker {
        file_id: i32,
        kind: Motion,
        width: u32,
        height: u32,
        emoji: String,
    },
    /// Video (including round video notes), played inline with sound.
    Video {
        file_id: i32,
        mini: Option<image::Handle>,
        /// Still preview image file, if TDLib has one.
        thumb: Option<i32>,
        width: u32,
        height: u32,
        duration: i32,
        spoiler: bool,
        round: bool,
    },
    /// GIFs (MP4 in Telegram): looped inline without sound while on screen.
    Animation {
        file_id: i32,
        mini: Option<image::Handle>,
        thumb: Option<i32>,
        width: u32,
        height: u32,
        duration: i32,
        spoiler: bool,
    },
}

/// How a sticker is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Motion {
    Still,
    /// Lottie (.tgs).
    Lottie,
    /// WebM video.
    Video,
}

/// Largest side of a sticker in a bubble, px.
pub(crate) const STICKER_SIZE: f32 = 180.0;
/// Diameter of a round video message, px.
pub(crate) const ROUND_SIZE: f32 = 220.0;

impl Media {
    pub(crate) fn file_id(&self) -> i32 {
        match self {
            Self::Photo { file_id, .. }
            | Self::Document { file_id, .. }
            | Self::Voice { file_id, .. }
            | Self::Sticker { file_id, .. }
            | Self::Video { file_id, .. }
            | Self::Animation { file_id, .. } => *file_id,
        }
    }

    /// Size a photo takes in the bubble, keeping its aspect ratio.
    pub(crate) fn photo_box(width: u32, height: u32) -> (f32, f32) {
        let (w, h) = (width.max(1) as f32, height.max(1) as f32);
        let scale = (PHOTO_MAX_W / w).min(PHOTO_MAX_H / h).min(1.0);
        ((w * scale).max(60.0), (h * scale).max(40.0))
    }

    pub(crate) fn sticker_box(width: u32, height: u32) -> (f32, f32) {
        let (w, h) = (width.max(1) as f32, height.max(1) as f32);
        let scale = STICKER_SIZE / w.max(h);
        (w * scale, h * scale)
    }

    /// Size of the inline picture of visual media.
    pub(crate) fn display_box(&self) -> Option<(f32, f32)> {
        match self {
            Self::Video { round: true, .. } => Some((ROUND_SIZE, ROUND_SIZE)),
            Self::Photo { width, height, .. }
            | Self::Video { width, height, .. }
            | Self::Animation { width, height, .. } => Some(Self::photo_box(*width, *height)),
            Self::Sticker { width, height, .. } => Some(Self::sticker_box(*width, *height)),
            Self::Document { .. } | Self::Voice { .. } => None,
        }
    }

    /// Height the media adds to a bubble, for virtualization estimates.
    pub(crate) fn height(&self) -> f32 {
        match self.display_box() {
            Some((_, h)) => h + 6.0,
            None => 44.0,
        }
    }
}

/// Media of a message plus the TDLib files it references.
pub(crate) fn media_of(content: &MessageContent) -> Option<(Media, Vec<File>)> {
    match content {
        MessageContent::MessagePhoto(m) => photo_media(&m.photo, m.has_spoiler),
        MessageContent::MessageDocument(m) => {
            let d = &m.document;
            let file = &d.document;
            Some((
                Media::Document {
                    file_id: file.id,
                    name: d.file_name.clone(),
                    size: file.size.max(file.expected_size),
                },
                vec![file.clone()],
            ))
        }
        MessageContent::MessageAudio(m) => {
            let a = &m.audio;
            let file = &a.audio;
            let name = if a.file_name.is_empty() {
                format!("{} — {}", a.performer, a.title)
            } else {
                a.file_name.clone()
            };
            Some((
                Media::Document {
                    file_id: file.id,
                    name,
                    size: file.size.max(file.expected_size),
                },
                vec![file.clone()],
            ))
        }
        MessageContent::MessageVoiceNote(m) => {
            let v = &m.voice_note;
            let packed = base64::engine::general_purpose::STANDARD
                .decode(&v.waveform)
                .unwrap_or_default();
            Some((
                Media::Voice {
                    file_id: v.voice.id,
                    duration: v.duration,
                    waveform: crate::av::audio::unpack_waveform(&packed),
                },
                vec![v.voice.clone()],
            ))
        }
        MessageContent::MessageSticker(m) => {
            let s = &m.sticker;
            let kind = match s.format {
                StickerFormat::Webp => Motion::Still,
                StickerFormat::Tgs => Motion::Lottie,
                StickerFormat::Webm => Motion::Video,
            };
            Some((
                Media::Sticker {
                    file_id: s.sticker.id,
                    kind,
                    width: s.width.max(1) as u32,
                    height: s.height.max(1) as u32,
                    emoji: s.emoji.clone(),
                },
                vec![s.sticker.clone()],
            ))
        }
        MessageContent::MessageVideo(m) => {
            let v = &m.video;
            let thumb = still_thumbnail(v.thumbnail.as_ref());
            let mut files = vec![v.video.clone()];
            files.extend(v.thumbnail.as_ref().map(|t| t.file.clone()));
            Some((
                Media::Video {
                    file_id: v.video.id,
                    mini: v.minithumbnail.as_ref().and_then(mini_handle),
                    thumb,
                    width: v.width.max(1) as u32,
                    height: v.height.max(1) as u32,
                    duration: v.duration,
                    spoiler: m.has_spoiler,
                    round: false,
                },
                files,
            ))
        }
        MessageContent::MessageAnimation(m) => {
            let a = &m.animation;
            let mut files = vec![a.animation.clone()];
            files.extend(a.thumbnail.as_ref().map(|t| t.file.clone()));
            Some((
                Media::Animation {
                    file_id: a.animation.id,
                    mini: a.minithumbnail.as_ref().and_then(mini_handle),
                    thumb: still_thumbnail(a.thumbnail.as_ref()),
                    width: a.width.max(1) as u32,
                    height: a.height.max(1) as u32,
                    duration: a.duration,
                    spoiler: m.has_spoiler,
                },
                files,
            ))
        }
        MessageContent::MessageVideoNote(m) => {
            let v = &m.video_note;
            let mut files = vec![v.video.clone()];
            files.extend(v.thumbnail.as_ref().map(|t| t.file.clone()));
            Some((
                Media::Video {
                    file_id: v.video.id,
                    mini: v.minithumbnail.as_ref().and_then(mini_handle),
                    thumb: still_thumbnail(v.thumbnail.as_ref()),
                    width: v.length.max(1) as u32,
                    height: v.length.max(1) as u32,
                    duration: v.duration,
                    round: true,
                    spoiler: false,
                },
                files,
            ))
        }
        _ => None,
    }
}

/// Thumbnail file usable as a still image (not an animated one).
fn still_thumbnail(thumb: Option<&tdlib_rs::types::Thumbnail>) -> Option<i32> {
    thumb
        .filter(|t| {
            matches!(
                t.format,
                ThumbnailFormat::Jpeg | ThumbnailFormat::Png | ThumbnailFormat::Webp
            )
        })
        .map(|t| t.file.id)
}

fn photo_media(photo: &Photo, spoiler: bool) -> Option<(Media, Vec<File>)> {
    let size = pick_size(photo)?;
    Some((
        Media::Photo {
            mini: photo.minithumbnail.as_ref().and_then(mini_handle),
            file_id: size.photo.id,
            width: size.width.max(0) as u32,
            height: size.height.max(0) as u32,
            spoiler,
        },
        vec![size.photo.clone()],
    ))
}

/// The largest size up to 1280 px (Telegram's "y"), else the smallest one:
/// sharp enough for the bubble and for opening, without 2560 px downloads.
fn pick_size(photo: &Photo) -> Option<&tdlib_rs::types::PhotoSize> {
    let area = |s: &&tdlib_rs::types::PhotoSize| i64::from(s.width) * i64::from(s.height);
    photo
        .sizes
        .iter()
        .filter(|s| s.width.max(s.height) <= 1280)
        .max_by_key(area)
        .or_else(|| photo.sizes.iter().min_by_key(area))
}

fn mini_handle(mini: &Minithumbnail) -> Option<image::Handle> {
    base64::engine::general_purpose::STANDARD
        .decode(&mini.data)
        .ok()
        .map(image::Handle::from_bytes)
}

/// What the client knows about a TDLib file.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct FileState {
    pub(crate) path: String,
    pub(crate) size: i64,
    pub(crate) downloaded: i64,
    pub(crate) uploaded: i64,
    pub(crate) downloading: bool,
    pub(crate) uploading: bool,
    pub(crate) done: bool,
    /// The client asked for the file and TDLib did not finish it (stopped,
    /// or refused the request outright): TDLib will not resume it on its
    /// own, so only an explicit action asks again in this session.
    pub(crate) failed: bool,
}

impl From<&File> for FileState {
    fn from(f: &File) -> Self {
        Self {
            path: f.local.path.clone(),
            size: f.size.max(f.expected_size),
            downloaded: f.local.downloaded_size,
            uploaded: f.remote.uploaded_size,
            downloading: f.local.is_downloading_active,
            uploading: f.remote.is_uploading_active,
            done: f.local.is_downloading_completed,
            failed: false,
        }
    }
}

impl FileState {
    /// Download or upload progress in percent.
    pub(crate) fn percent(&self) -> u32 {
        let part = if self.uploading {
            self.uploaded
        } else {
            self.downloaded
        };
        if self.size <= 0 {
            return 0;
        }
        ((part as f64 / self.size as f64) * 100.0).clamp(0.0, 100.0) as u32
    }

    /// Whether what the client already knows about the file — a download in
    /// flight, finished, or failed — must outlive this snapshot, which
    /// confirms none of it. TDLib repeats stale file objects (the same
    /// photo in a user update, a message reparsed), and those must not
    /// erase a request the client already made.
    pub(crate) fn survives(&self, snapshot: &File) -> bool {
        (self.done || self.downloading || self.failed)
            && !(snapshot.local.is_downloading_completed || snapshot.local.is_downloading_active)
    }
}

/// Decoded photos; offscreen entries are LRU-evicted under pressure.
#[derive(Default)]
pub(crate) struct ImageCache {
    entries: HashMap<i32, (image::Handle, usize, u64, Option<std::time::Instant>)>,
    total: usize,
    clock: u64,
    /// Files being decoded right now.
    pub(crate) decoding: HashSet<i32>,
}

impl ImageCache {
    pub(crate) fn get(&mut self, file_id: i32) -> Option<image::Handle> {
        self.clock += 1;
        let clock = self.clock;
        self.entries.get_mut(&file_id).map(|e| {
            e.2 = clock;
            e.0.clone()
        })
    }

    pub(crate) fn peek(&self, file_id: i32) -> Option<&image::Handle> {
        self.entries.get(&file_id).map(|e| &e.0)
    }

    pub(crate) fn has_entries(&self) -> bool {
        !self.entries.is_empty()
    }

    /// None while a photo is visible; first time it became invisible otherwise.
    #[cfg(test)]
    pub(crate) fn inactive_since(&self, file_id: i32) -> Option<std::time::Instant> {
        self.entries.get(&file_id).and_then(|entry| entry.3)
    }

    pub(crate) fn insert(
        &mut self,
        file_id: i32,
        width: u32,
        height: u32,
        rgba: Vec<u8>,
        visible: &HashSet<i32>,
    ) {
        self.decoding.remove(&file_id);
        let bytes = rgba.len();
        self.clock += 1;
        let now = std::time::Instant::now();
        let handle = image::Handle::from_rgba(width, height, rgba);
        let since = (!visible.contains(&file_id)).then_some(now);
        if let Some((_, old, _, old_since)) = self
            .entries
            .insert(file_id, (handle, bytes, self.clock, since))
        {
            self.total -= old;
            if since.is_some() {
                self.entries.get_mut(&file_id).unwrap().3 = old_since.or(since);
            }
        }
        self.total += bytes;
        self.trim(visible, now);
    }

    /// Maintains ownership timestamps and evicts oldest unowned photos until
    /// under budget; visible photos alone may exceed the 64 MiB limit.
    pub(crate) fn trim(&mut self, visible: &HashSet<i32>, now: std::time::Instant) {
        for (id, entry) in &mut self.entries {
            if visible.contains(id) {
                entry.3 = None;
            } else if entry.3.is_none() {
                entry.3 = Some(now);
            }
        }
        while self.total > IMAGE_BUDGET {
            let Some((&oldest, _)) = self
                .entries
                .iter()
                .filter(|(id, _)| !visible.contains(id))
                .min_by_key(|(_, e)| e.2)
            else {
                break;
            };
            if let Some((_, b, _, _)) = self.entries.remove(&oldest) {
                self.total -= b;
            }
        }
    }

    /// Unlike pressure trimming, time-based reclamation includes small images.
    /// An out-of-order sweep cannot expire a future inactivity timestamp.
    pub(crate) fn expire_inactive(&mut self, visible: &HashSet<i32>, now: std::time::Instant) {
        self.entries.retain(|id, (_, bytes, _, since)| {
            let expired = !visible.contains(id)
                && since
                    .as_ref()
                    .and_then(|at| now.checked_duration_since(*at))
                    .is_some_and(|age| age >= PHOTO_IDLE);
            if expired {
                self.total -= *bytes;
            }
            !expired
        });
    }

    #[cfg(test)]
    pub(crate) fn total(&self) -> usize {
        self.total
    }
}

/// Bounds accepted from a stranger's file, whatever its header claims.
fn limits() -> ::image::Limits {
    let mut limits = ::image::Limits::default();
    limits.max_image_width = Some(MAX_IMAGE_SIDE);
    limits.max_image_height = Some(MAX_IMAGE_SIDE);
    limits.max_alloc = Some(MAX_IMAGE_ALLOC);
    limits
}

/// Opens an image from a stranger: bounded dimensions and allocation
/// instead of whatever a crafted header claims.
pub(crate) fn open_image(path: &str) -> Result<::image::DynamicImage, String> {
    let mut reader = ::image::ImageReader::open(path)
        .and_then(|r| r.with_guessed_format())
        .map_err(|e| format!("фото: {e}"))?;
    reader.limits(limits());
    reader.decode().map_err(|e| format!("фото: {e}"))
}

/// The same, for bytes already in hand (a caller that hashes them anyway,
/// like `avatars::decode_cached`, reads the file once).
pub(crate) fn open_image_bytes(bytes: &[u8]) -> Result<::image::DynamicImage, String> {
    let mut reader = ::image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| format!("фото: {e}"))?;
    reader.limits(limits());
    reader.decode().map_err(|e| format!("фото: {e}"))
}

/// Decodes a photo file and scales it down to at most twice the bubble size
/// (sharp on HiDPI). Runs on a blocking thread.
pub(crate) fn decode(path: &str) -> Result<(u32, u32, Vec<u8>), String> {
    let img = open_image(path)?;
    let (max_w, max_h) = ((PHOTO_MAX_W * 2.0) as u32, (PHOTO_MAX_H * 2.0) as u32);
    let img = if img.width() > max_w || img.height() > max_h {
        img.resize(max_w, max_h, ::image::imageops::FilterType::Triangle)
    } else {
        img
    };
    let rgba = img.into_rgba8();
    Ok((rgba.width(), rgba.height(), rgba.into_raw()))
}

/// Files sent as photos (compressed by Telegram); everything else is sent
/// as a document, untouched.
pub(crate) fn is_photo_file(path: &Path) -> bool {
    matches!(
        extension(path).as_deref(),
        Some("jpg" | "jpeg" | "png" | "webp")
    )
}

/// TDLib's own limits for `inputMessagePhoto`: bigger than 10 MB, or an
/// oddly shaped image, and the compressed-photo pipeline rejects the file
/// outright instead of sending it (it must go as a document then).
const PHOTO_MAX_BYTES: u64 = 10 * 1024 * 1024;
const PHOTO_MAX_WH_SUM: u32 = 10000;
const PHOTO_MAX_ASPECT: u32 = 20;

/// Whether `path` fits those limits. Reads only the image header
/// (`image::image_dimensions`), not the whole file.
fn fits_photo_limits(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if meta.len() > PHOTO_MAX_BYTES {
        return false;
    }
    let Ok((w, h)) = ::image::image_dimensions(path) else {
        return false;
    };
    if w == 0 || h == 0 || w.saturating_add(h) > PHOTO_MAX_WH_SUM {
        return false;
    }
    w.max(h) <= w.min(h).saturating_mul(PHOTO_MAX_ASPECT)
}

/// Whether `path` should be sent as `inputMessagePhoto`: the right
/// extension and within TDLib's size, dimension and aspect-ratio limits.
pub(crate) fn send_as_photo(path: &Path) -> bool {
    is_photo_file(path) && fits_photo_limits(path)
}

/// Files the client opens with one click in the system: media, PDF and
/// plain text. Anything else (executables, scripts, shortcuts, help files,
/// installers, office documents with macros, and whatever a system may
/// "run" on open) is only shown in its folder. An allow-list, because a
/// deny-list of dangerous types is never complete across systems.
pub(crate) fn can_open(name: &str) -> bool {
    matches!(
        extension(Path::new(name)).as_deref(),
        Some(
            "jpg"
                | "jpeg"
                | "png"
                | "webp"
                | "gif"
                | "bmp"
                | "heic"
                | "avif"
                | "pdf"
                | "txt"
                | "md"
                | "csv"
                | "log"
                | "mp3"
                | "ogg"
                | "oga"
                | "opus"
                | "m4a"
                | "flac"
                | "wav"
                | "aac"
                | "mp4"
                | "m4v"
                | "mov"
                | "mkv"
                | "webm"
                | "avi"
        )
    )
}

fn extension(path: &Path) -> Option<String> {
    path.extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
}

pub(crate) fn human_size(bytes: i64) -> String {
    let b = bytes.max(0) as f64;
    match b {
        b if b >= 1024.0 * 1024.0 * 1024.0 => format!("{:.1} ГБ", b / 1024.0 / 1024.0 / 1024.0),
        b if b >= 1024.0 * 1024.0 => format!("{:.1} МБ", b / 1024.0 / 1024.0),
        b if b >= 1024.0 => format!("{:.0} КБ", b / 1024.0),
        b => format!("{b:.0} Б"),
    }
}

/// Reads an image from the system clipboard and stores it as PNG for
/// upload. `Ok(None)` when the clipboard holds no image. Blocking.
pub(crate) fn paste_image() -> Result<Option<PathBuf>, String> {
    let mut clipboard = arboard::Clipboard::new().map_err(|e| format!("буфер обмена: {e}"))?;
    let Ok(img) = clipboard.get_image() else {
        return Ok(None);
    };
    let buffer =
        ::image::RgbaImage::from_raw(img.width as u32, img.height as u32, img.bytes.into_owned())
            .ok_or("буфер обмена: повреждённая картинка")?;
    std::fs::create_dir_all(crate::paths::tmp()).map_err(|e| e.to_string())?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let path = crate::paths::tmp().join(format!("paste-{stamp}.png"));
    buffer
        .save(&path)
        .map_err(|e| format!("буфер обмена: {e}"))?;
    Ok(Some(path))
}

/// Removes pasted images old enough that their upload is surely done.
/// Skips voice recordings: TDLib may still need that local file to retry a
/// send queued while the client was offline across a restart.
pub(crate) fn clean_paste_dir() {
    let Ok(entries) = std::fs::read_dir(crate::paths::tmp()) else {
        return;
    };
    // A week, not a day: long enough to outlast an offline stretch with a
    // message still waiting to be sent.
    let max_age = std::time::Duration::from_secs(7 * 24 * 3600);
    for entry in entries.flatten() {
        if entry.file_name().to_string_lossy().starts_with("voice-") {
            continue;
        }
        let old = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age > max_age);
        if old {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

impl super::App {
    pub(crate) fn photo_epoch(&self, window: WinId) -> PhotoEpoch {
        PhotoEpoch {
            client: self.session.client_id,
            generation: self
                .session
                .photo_generations
                .get(&window)
                .copied()
                .unwrap_or(0),
        }
    }

    /// Advance one window's widget generation without dropping unrelated
    /// pictures that remain displayed there.
    fn advance_photo_epoch(&mut self, window: WinId) {
        let generation = self.session.photo_generations.entry(window).or_default();
        *generation = generation
            .checked_add(1)
            .expect("photo generation exhausted");
        let epoch = PhotoEpoch {
            client: self.session.client_id,
            generation: *generation,
        };
        for ((owner_window, _), (_, shown_epoch)) in &mut self.session.photo_owners {
            if *owner_window == window {
                *shown_epoch = epoch;
            }
        }
    }

    /// A widget show is idempotent for its identity and cannot resurrect a
    /// prior widget generation or a media source removed by a server update.
    pub(super) fn photo_shown(
        &mut self,
        window: WinId,
        owner: PhotoOwner,
        file_id: i32,
        epoch: PhotoEpoch,
    ) -> Task<Msg> {
        if epoch != self.photo_epoch(window) {
            return Task::none();
        }
        let Some(pane) = self.session.panes.get(&window) else {
            return Task::none();
        };
        if (window == self.main_window
            && (self.main_window_state != super::MainWindowState::Open
                || self.session.settings_open))
            || pane.list.archive_settings.is_some()
        {
            return Task::none();
        }
        let sources = PhotoSources {
            window,
            pane,
            stickers: &self.session.stickers,
            video: self.session.video.as_ref(),
            playback: &self.session.playback,
        };
        let bounds = if matches!(owner, PhotoOwner::Message(..)) {
            let visible = pane.visible();
            (visible.start, visible.end)
        } else {
            (0, pane.messages.len())
        };
        if !sources.contains(owner, file_id, bounds) {
            return Task::none();
        }
        if self
            .session
            .photo_owners
            .insert((window, owner), (file_id, epoch))
            != Some((file_id, epoch))
        {
            self.refresh_photo_owners(std::time::Instant::now());
        }
        self.show_photo(file_id)
    }

    pub(super) fn photo_hidden(
        &mut self,
        window: WinId,
        owner: PhotoOwner,
        file_id: i32,
        epoch: PhotoEpoch,
    ) {
        let key = (window, owner);
        if epoch == self.photo_epoch(window)
            && self.session.photo_owners.get(&key) == Some(&(file_id, epoch))
        {
            self.session.photo_owners.remove(&key);
            self.refresh_photo_owners(std::time::Instant::now());
        }
    }

    pub(super) fn clear_photo_owners(&mut self, window: WinId) {
        let before = self.session.photo_owners.len();
        self.session.photo_owners.retain(|(w, _), _| *w != window);
        self.advance_photo_epoch(window);
        if self.session.photo_owners.len() != before {
            self.refresh_photo_owners(std::time::Instant::now());
        }
    }

    pub(super) fn clear_picker_photos(&mut self, window: WinId) {
        let before = self.session.photo_owners.len();
        self.session
            .photo_owners
            .retain(|(w, owner), _| *w != window || !matches!(owner, PhotoOwner::Picker(..)));
        self.advance_photo_epoch(window);
        if self.session.photo_owners.len() != before {
            self.refresh_photo_owners(std::time::Instant::now());
        }
    }

    pub(super) fn clear_history_photos(&mut self, window: WinId) {
        let before = self.session.photo_owners.len();
        self.session
            .photo_owners
            .retain(|(w, owner), _| *w != window || !matches!(owner, PhotoOwner::Message(..)));
        self.advance_photo_epoch(window);
        if self.session.photo_owners.len() != before {
            self.refresh_photo_owners(std::time::Instant::now());
        }
    }

    /// A server replacement invalidates every displayed copy of this one
    /// message, without touching other messages using the same file.
    pub(super) fn clear_message_photos(&mut self, chat_id: i64, message_id: i64) {
        let before = self.session.photo_owners.len();
        self.session.photo_owners.retain(|(_, owner), _| {
            !matches!(owner, PhotoOwner::Message(chat, message, _) if *chat == chat_id && *message == message_id)
        });
        for (&window, pane) in &self.session.panes {
            if pane.chat_id != Some(chat_id) {
                continue;
            }
            let generation = self.session.photo_generations.entry(window).or_default();
            *generation = generation
                .checked_add(1)
                .expect("photo generation exhausted");
        }
        for ((window, _), (_, shown_epoch)) in &mut self.session.photo_owners {
            if self
                .session
                .panes
                .get(window)
                .is_some_and(|pane| pane.chat_id == Some(chat_id))
            {
                *shown_epoch = PhotoEpoch {
                    client: self.session.client_id,
                    generation: self
                        .session
                        .photo_generations
                        .get(window)
                        .copied()
                        .unwrap_or(0),
                };
            }
        }
        if self.session.photo_owners.len() != before {
            self.refresh_photo_owners(std::time::Instant::now());
        }
    }

    /// The inline player replaces this particular video's still preview.
    pub(super) fn clear_video_thumbnail(&mut self, window: WinId, chat_id: i64, message_id: i64) {
        if self
            .session
            .photo_owners
            .remove(&(
                window,
                PhotoOwner::Message(chat_id, message_id, PhotoKind::Thumbnail),
            ))
            .is_some()
        {
            self.refresh_photo_owners(std::time::Instant::now());
        }
    }

    /// The first decoded animation frame replaces its still thumbnail in
    /// every window displaying the animation.
    pub(super) fn clear_animation_thumbnails(&mut self, animation_id: i32) {
        let before = self.session.photo_owners.len();
        self.session.photo_owners.retain(|(window, owner), _| {
            let PhotoOwner::Message(chat_id, message_id, PhotoKind::Thumbnail) = *owner else {
                return true;
            };
            !self.session.panes.get(window).is_some_and(|pane| {
                pane.chat_id == Some(chat_id) && pane.messages.iter().any(|message| {
                    message.id == message_id
                        && matches!(&message.media, Some(Media::Animation { file_id, .. }) if *file_id == animation_id)
                })
            })
        });
        if self.session.photo_owners.len() != before {
            self.refresh_photo_owners(std::time::Instant::now());
        }
    }

    fn refresh_photo_owners(&mut self, now: std::time::Instant) {
        self.session.wanted_photos.clear();
        self.session.wanted_photos.extend(
            self.session
                .photo_owners
                .values()
                .map(|(file_id, _)| *file_id),
        );
        self.session.images.trim(&self.session.wanted_photos, now);
    }

    /// Reconcile against constructed widget sources: iced never publishes a
    /// hide when a sensor is removed or changes its key.
    fn reconcile_photo_owners(&mut self, now: std::time::Instant, only_window: Option<WinId>) {
        // A scroll event visits one pane and does not allocate. The periodic
        // sweep caches one visible range per pane rather than walking its
        // entire history once for every displayed image.
        let scrolled_range = only_window
            .and_then(|window| self.session.panes.get(&window).map(|pane| pane.visible()));
        let mut ranges = HashMap::new();
        if only_window.is_none() {
            for &(window, owner) in self.session.photo_owners.keys() {
                if matches!(owner, PhotoOwner::Message(..))
                    && let Some(pane) = self.session.panes.get(&window)
                {
                    ranges.entry(window).or_insert_with(|| pane.visible());
                }
            }
        }
        let before = self.session.photo_owners.len();
        self.session
            .photo_owners
            .retain(|(window, owner), (file_id, epoch)| {
                if only_window.is_some_and(|target| target != *window) {
                    return true;
                }
                let Some(pane) = self.session.panes.get(window) else {
                    return false;
                };
                if epoch.client != self.session.client_id
                    || epoch.generation
                        != self
                            .session
                            .photo_generations
                            .get(window)
                            .copied()
                            .unwrap_or(0)
                    || (*window == self.main_window
                        && (self.main_window_state != super::MainWindowState::Open
                            || self.session.settings_open))
                    || pane.list.archive_settings.is_some()
                {
                    return false;
                }
                let range = scrolled_range.as_ref().or_else(|| ranges.get(window));
                let bounds = range.map_or((0, pane.messages.len()), |visible| {
                    (visible.start, visible.end)
                });
                PhotoSources {
                    window: *window,
                    pane,
                    stickers: &self.session.stickers,
                    video: self.session.video.as_ref(),
                    playback: &self.session.playback,
                }
                .contains(*owner, *file_id, bounds)
            });
        if before != self.session.photo_owners.len() {
            self.refresh_photo_owners(now);
        } else if only_window.is_none() {
            self.session.images.trim(&self.session.wanted_photos, now);
        }
    }

    pub(super) fn photo_scrolled(&mut self, window: WinId, now: std::time::Instant) {
        if self.session.photo_owners.keys().any(|(w, _)| *w == window) {
            self.reconcile_photo_owners(now, Some(window));
        }
    }

    pub(super) fn memory_sweep(&mut self, now: std::time::Instant) {
        self.reconcile_photo_owners(now, None);
        self.session
            .images
            .expire_inactive(&self.session.wanted_photos, now);
        self.reclaim_inactive_history(now);
    }

    /// Remembers a file from a snapshot (a message parsed, a peer's photo).
    /// A stale snapshot repeats a file object that confirms no download at
    /// all, so what the client already knows — a download in flight, done,
    /// or failed — outlives it (`FileState::survives`); otherwise the
    /// snapshot is the fresher truth.
    pub(super) fn remember_file(&mut self, file: &File) {
        let survives = self
            .session
            .files
            .get(&file.id)
            .is_some_and(|state| state.survives(file));
        if !survives {
            self.session.files.insert(file.id, FileState::from(file));
        }
    }

    /// Remembers the files a message refers to.
    pub(super) fn note_files(&mut self, content: &MessageContent) {
        if let Some((_, files)) = media_of(content) {
            for file in files {
                self.remember_file(&file);
            }
        }
        if let Some((_, Some(file))) = super::extra::extra_of(content) {
            self.remember_file(&file);
        }
    }

    /// Like `note_files`, but for files already collected elsewhere
    /// (`MsgItem::parse`) instead of re-parsing the message content for them.
    pub(super) fn note_files_of(&mut self, files: &[File]) {
        for file in files {
            self.remember_file(file);
        }
    }

    pub(super) fn on_file(&mut self, file: &File) -> Task<Msg> {
        let mut state = FileState::from(file);
        let previous = self.session.files.get(&file.id);
        let finished = state.done && !previous.is_some_and(|s| s.done);
        // A download that was active and stopped without completing (no
        // space, network error, ...): the viewer must not be left showing
        // "Загрузка…" forever.
        let stopped = !state.done && !state.downloading && previous.is_some_and(|s| s.downloading);
        // The failure outlives the updates that follow it (TDLib repeating
        // the file as it was before the request, for instance): only
        // progress or completion clear it, so that an automatic path cannot
        // restart by itself what the user never asked for again.
        state.failed = if state.done || state.downloading {
            false
        } else {
            stopped || previous.is_some_and(|s| s.failed)
        };
        self.session.files.insert(file.id, state);
        if stopped {
            return self.viewer_file_failed(file.id);
        }
        if !finished {
            return Task::none();
        }
        let photo = if self.session.wanted_photos.contains(&file.id) {
            self.decode_photo(file.id)
        } else {
            Task::none()
        };
        Task::batch([
            photo,
            self.playback_file_done(file.id),
            self.avatar_file_done(file.id),
            self.viewer_file_done(file.id),
        ])
    }

    /// Asks TDLib for the whole file, once, and remembers the request on
    /// the file's state: a sensor reporting the same picture again then
    /// finds it "downloading" instead of asking a second time. A request
    /// TDLib refuses is remembered as a failure rather than leaving the
    /// file in flight forever. Explicit actions (the viewer, "Скачать",
    /// playing) go through here too: they are what retries a failed file.
    pub(super) fn request_download(&mut self, file_id: i32, priority: i32) -> Task<Msg> {
        if let Some(state) = self.session.files.get_mut(&file_id)
            && !state.done
        {
            state.downloading = true;
            state.failed = false;
        }
        let client_id = self.session.client_id;
        Task::perform(
            td::download_file(client_id, file_id, priority),
            move |result| {
                Msg::ForClient(client_id, Box::new(Msg::FileRequestDone(file_id, result)))
            },
        )
    }

    /// A photo came into view: decode it if downloaded, else download it.
    pub(super) fn show_photo(&mut self, file_id: i32) -> Task<Msg> {
        if self.session.images.get(file_id).is_some() {
            return Task::none();
        }
        match self.session.files.get(&file_id) {
            Some(f) if f.done => self.decode_photo(file_id),
            // In flight, or failed before: an automatic path never asks
            // again (that is an explicit action, through `request_download`).
            Some(f) if f.downloading || f.failed => Task::none(),
            _ => self.request_download(file_id, 16),
        }
    }

    fn decode_photo(&mut self, file_id: i32) -> Task<Msg> {
        let Some(path) = self.session.files.get(&file_id).map(|f| f.path.clone()) else {
            return Task::none();
        };
        if !self.session.images.decoding.insert(file_id) {
            return Task::none();
        }
        Task::perform(
            async move {
                let permit = DECODES.acquire().await.map_err(|e| e.to_string())?;
                tokio::task::spawn_blocking(move || {
                    let _permit = permit;
                    decode(&path)
                })
                .await
                .map_err(|e| e.to_string())
                .and_then(|r| r)
            },
            move |r| Msg::ImageDecoded(file_id, r),
        )
    }

    /// Opens a downloaded file with the system, or its folder. Only types
    /// from `can_open` are opened directly; either way the file itself (not
    /// the folder) is tagged as downloaded from the internet first, so a
    /// user who opens an executable from its folder still gets SmartScreen.
    pub(super) fn open_file(&mut self, file_id: i32, folder: bool) {
        let Some(path) = self
            .session
            .files
            .get(&file_id)
            .filter(|f| f.done)
            .map(|f| PathBuf::from(&f.path))
        else {
            return;
        };
        mark_from_internet(&path);
        let target = if folder {
            path.parent().map(Path::to_path_buf).unwrap_or(path)
        } else if !can_open(&path.to_string_lossy()) {
            self.error = Some("такие файлы открываются только через папку".into());
            return;
        } else {
            path
        };
        if let Err(e) = open::that_detached(&target) {
            self.error = Some(format!("не удалось открыть: {e}"));
        }
    }

    /// Sends local files to the chat of a window.
    pub(super) fn send_files(&mut self, window: WinId, paths: Vec<PathBuf>) -> Task<Msg> {
        let Some(chat_id) = self.session.panes.get(&window).and_then(|p| p.chat_id) else {
            return Task::none();
        };
        let client_id = self.session.client_id;
        Task::batch(paths.into_iter().filter(|p| p.is_file()).map(|path| {
            let as_photo = send_as_photo(&path);
            Task::perform(
                td::send_file(
                    client_id,
                    chat_id,
                    path.to_string_lossy().into_owned(),
                    as_photo,
                ),
                move |r| Msg::ForClient(client_id, Box::new(Msg::Done(r))),
            )
        }))
    }
}

/// Windows: tags a file as downloaded from the internet (Mark of the Web),
/// so SmartScreen and Office Protected View treat it as untrusted.
fn mark_from_internet(path: &Path) {
    #[cfg(windows)]
    {
        let mut stream = path.as_os_str().to_owned();
        stream.push(":Zone.Identifier");
        let _ = std::fs::write(stream, "[ZoneTransfer]\r\nZoneId=3\r\n");
    }
    #[cfg(not(windows))]
    let _ = path;
}

/// At most this many images are decoded at once (each may take tens of MB
/// while decoding).
pub(crate) static DECODES: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(2);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_evicts_least_recently_used_beyond_budget() {
        let mut cache = ImageCache::default();
        let none = HashSet::new();
        // 3 images of 30 MB against a 64 MB budget.
        let big = || vec![0u8; 30 * 1024 * 1024];
        cache.insert(1, 1, 1, big(), &none);
        cache.insert(2, 1, 1, big(), &none);
        assert!(cache.get(1).is_some(), "touch 1");
        cache.insert(3, 1, 1, big(), &none);
        assert!(cache.peek(2).is_none(), "2 was least recently used");
        assert!(cache.peek(1).is_some() && cache.peek(3).is_some());
        assert!(cache.total() <= IMAGE_BUDGET);
    }

    #[test]
    fn cache_does_not_evict_a_visible_photo() {
        let mut cache = ImageCache::default();
        let visible: HashSet<i32> = [1].into_iter().collect();
        let big = || vec![0u8; 30 * 1024 * 1024];
        cache.insert(1, 1, 1, big(), &visible);
        cache.insert(2, 1, 1, big(), &visible);
        cache.insert(3, 1, 1, big(), &visible);
        assert!(cache.peek(1).is_some(), "1 is on screen, must survive");
    }

    #[test]
    fn photo_box_keeps_aspect_and_limits() {
        assert_eq!(Media::photo_box(1280, 960), (400.0, 300.0));
        assert_eq!(Media::photo_box(600, 1200), (240.0, 480.0));
        assert_eq!(
            Media::photo_box(200, 100),
            (200.0, 100.0),
            "small photos are not enlarged"
        );
    }

    #[test]
    fn only_known_safe_types_open_with_one_click() {
        assert!(can_open("report.PDF"));
        assert!(can_open("photo.jpg"));
        assert!(can_open("clip.mp4"));
        for risky in [
            "setup.exe",
            "invoice.pif",
            "help.chm",
            "panel.cpl",
            "link.url",
            "run.sh",
            "Telega.AppImage",
            "macro.docm",
            "noext",
            "archive.zip",
            "photo.jpg.exe",
        ] {
            assert!(!can_open(risky), "{risky}");
        }
    }

    #[test]
    fn photos_are_sent_as_photos_other_files_as_documents() {
        assert!(is_photo_file(Path::new("/tmp/a.JPG")));
        assert!(is_photo_file(Path::new("b.webp")));
        assert!(!is_photo_file(Path::new("c.gif")), "GIFs go as files");
        assert!(!is_photo_file(Path::new("d.pdf")));
    }

    #[test]
    fn oversized_or_extreme_aspect_photos_go_as_documents() {
        let dir = std::env::temp_dir().join(format!("telega-media-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        // Bigger than TDLib's 10 MB limit: the extension alone says photo.
        let big = dir.join("big.png");
        std::fs::write(&big, vec![0u8; 11 * 1024 * 1024]).unwrap();
        assert!(is_photo_file(&big), "extension still says photo");
        assert!(!send_as_photo(&big), "over 10 MB must go as a document");

        // A real, tiny image, but far past the 20:1 aspect ratio TDLib allows.
        let tall = dir.join("tall.png");
        ::image::RgbaImage::new(10, 300).save(&tall).unwrap();
        assert!(!send_as_photo(&tall), "30:1 aspect must go as a document");

        // An ordinary small photo still goes as a photo.
        let ok = dir.join("ok.png");
        ::image::RgbaImage::new(100, 100).save(&ok).unwrap();
        assert!(send_as_photo(&ok));

        let _ = std::fs::remove_dir_all(&dir);
    }

    // `mark_from_internet` only writes on Windows (NTFS alternate data
    // streams); on other systems it is a no-op and there is nothing on
    // disk to assert on, so this regression test only runs there. It
    // guards `open_file` calling it unconditionally (including the
    // "open the containing folder" branch, where the target passed to
    // `open::that_detached` is the parent directory, not the file).
    #[cfg(windows)]
    #[test]
    fn mark_from_internet_tags_the_file_with_zone_identifier() {
        let dir = std::env::temp_dir().join(format!("telega-motw-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.txt");
        std::fs::write(&file, b"hi").unwrap();

        mark_from_internet(&file);

        let mut stream = file.as_os_str().to_owned();
        stream.push(":Zone.Identifier");
        let tag = std::fs::read_to_string(&stream).unwrap();
        assert!(tag.contains("ZoneId=3"));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
