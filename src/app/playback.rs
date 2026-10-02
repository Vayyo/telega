//! Sound and motion: voice messages (play, record), looped animations
//! (GIFs, animated stickers) and videos opened externally.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::LazyLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use iced::Task;
use iced::futures::{SinkExt, Stream};
use iced::widget::image;

use super::media::{Media, Motion, STICKER_SIZE};
use super::{App, Msg, WinId};
use crate::av::audio::{Pcm, Player, Recorder};
use crate::av::lottie::Lottie;
use crate::av::video::FrameStream;
use crate::td;

/// Animations played at the same time at most; more on screen stay still.
const MAX_ANIMATIONS: usize = 3;
/// Largest files downloaded automatically when on screen, bytes.
const MAX_AUTO_ANIMATION: i64 = 10 * 1024 * 1024;
const MAX_AUTO_STICKER: i64 = 1024 * 1024;
/// Decoded voice messages kept in memory.
const PCM_CACHE: usize = 1;

/// A recorder handle in a message (it has no `Debug`).
#[derive(Clone)]
pub(crate) struct RecorderHandle(pub(crate) Recorder);

impl std::fmt::Debug for RecorderHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Recorder")
    }
}

/// Decoded voice message in a message.
#[derive(Clone)]
pub(crate) struct PcmHandle(pub(crate) Arc<Pcm>);

impl std::fmt::Debug for PcmHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Pcm({:?})", self.0.duration())
    }
}

pub(crate) struct Recording {
    pub(crate) window: WinId,
    pub(crate) chat_id: i64,
    pub(crate) recorder: Recorder,
    pub(crate) started: Instant,
}

/// Currently played animation.
struct Anim {
    frame: Option<image::Handle>,
    /// The frame before: iced loads a fresh image in the background and
    /// may draw it a frame late, so the previous one is drawn under it and
    /// nothing blinks. (Pre-allocating frames instead leaked: iced keeps
    /// allocations in the cache of a window that may never redraw.)
    prev: Option<image::Handle>,
    stop: iced::task::Handle,
}

#[derive(Default)]
pub(crate) struct Playback {
    player: Option<Player>,
    pcm: Vec<(i32, Arc<Pcm>)>,
    /// Voice message that should start once downloaded / decoded.
    pending_voice: Option<i32>,
    /// (file, position, playing) of the voice message player.
    pub(crate) voice: Option<(i32, Duration, bool)>,
    pub(crate) recording: Option<Recording>,
    anims: HashMap<i32, Anim>,
    /// Animations on screen waiting for their file.
    wanted: HashMap<i32, Motion>,
    /// How many message bubbles currently show each animated file: hiding
    /// one copy must not stop another still on screen (the same sticker
    /// sent twice, a forwarded GIF, or the chat open in two windows).
    shown: HashMap<i32, usize>,
    /// Videos to open in the system player once downloaded.
    open_when_done: Vec<i32>,
}

impl Playback {
    pub(crate) fn frame(&self, file_id: i32) -> Option<&image::Handle> {
        self.anims.get(&file_id).and_then(|a| a.frame.as_ref())
    }

    pub(crate) fn previous_frame(&self, file_id: i32) -> Option<&image::Handle> {
        self.anims.get(&file_id).and_then(|a| a.prev.as_ref())
    }

    pub(crate) fn busy(&self) -> bool {
        self.voice.is_some_and(|v| v.2) || self.recording.is_some()
    }

    #[cfg(test)]
    pub(crate) fn playing(&self, file_id: i32) -> bool {
        self.anims.contains_key(&file_id)
    }

    #[cfg(test)]
    pub(crate) fn waiting(&self, file_id: i32) -> bool {
        self.wanted.contains_key(&file_id)
    }
}

/// A permit is acquired before the UI reports playback and stays in the OS
/// thread even if the stream is hidden while native decoding is in progress.
static ANIMATION_WORKERS: LazyLock<Arc<tokio::sync::Semaphore>> =
    LazyLock::new(|| Arc::new(tokio::sync::Semaphore::new(MAX_ANIMATIONS)));

/// The same admission path is used by animation decoding and controlled
/// blocking worker tests. A refused (or unspawnable) worker never starts.
pub(crate) fn spawn_animation_worker(
    workers: &Arc<tokio::sync::Semaphore>,
    work: impl FnOnce(Arc<AtomicBool>) + Send + 'static,
) -> Option<Arc<AtomicBool>> {
    let permit = Arc::clone(workers).try_acquire_owned().ok()?;
    let cancel = Arc::new(AtomicBool::new(false));
    let worker_cancel = Arc::clone(&cancel);
    std::thread::Builder::new()
        .name("animation-decoder".into())
        .spawn(move || {
            let _permit = permit;
            work(worker_cancel);
        })
        .ok()?;
    Some(cancel)
}

struct CancelOnDrop(Arc<AtomicBool>);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

/// Dropping the stream marks native decode canceled and closes the bounded
/// frame receiver, waking a worker waiting to send.
fn frames(
    mut rx: tokio::sync::mpsc::Receiver<(u32, u32, Vec<u8>)>,
    cancel: Arc<AtomicBool>,
) -> impl Stream<Item = (u32, u32, Vec<u8>)> {
    let guard = CancelOnDrop(cancel);
    iced::stream::channel(2, async move |mut out| {
        let _cancel = guard;
        while let Some(frame) = rx.recv().await {
            if out.send(frame).await.is_err() {
                return;
            }
        }
    })
}

type FrameTx = tokio::sync::mpsc::Sender<(u32, u32, Vec<u8>)>;

fn canceled(cancel: &AtomicBool, tx: &FrameTx) -> bool {
    cancel.load(Ordering::Relaxed) || tx.is_closed()
}

fn play_video(
    path: &str,
    w: u32,
    h: u32,
    tx: &FrameTx,
    cancel: Arc<AtomicBool>,
) -> Result<(), String> {
    if canceled(&cancel, tx) {
        return Ok(());
    }
    let mut stream =
        FrameStream::open_cancelable(std::path::Path::new(path), w, h, cancel.clone())?;
    if canceled(&cancel, tx) {
        return Ok(());
    }
    loop {
        if canceled(&cancel, tx) {
            return Ok(());
        }
        let start = Instant::now();
        let mut any = false;
        loop {
            if canceled(&cancel, tx) {
                return Ok(());
            }
            let Some(frame) = stream.next_frame()? else {
                break;
            };
            if canceled(&cancel, tx) {
                return Ok(());
            }
            any = true;
            // A broken timestamp must not freeze the animation for long.
            if let Some(wait) = frame.pts.checked_sub(start.elapsed()) {
                std::thread::sleep(wait.min(Duration::from_secs(1)));
            }
            if canceled(&cancel, tx)
                || tx
                    .blocking_send((frame.width, frame.height, frame.rgba))
                    .is_err()
            {
                return Ok(());
            }
        }
        if !any || canceled(&cancel, tx) {
            return Ok(());
        }
        stream.rewind()?;
        if canceled(&cancel, tx) {
            return Ok(());
        }
    }
}

fn play_lottie(
    path: &str,
    w: u32,
    h: u32,
    tx: &FrameTx,
    cancel: &AtomicBool,
) -> Result<(), String> {
    if canceled(cancel, tx) {
        return Ok(());
    }
    let mut lottie = Lottie::open(std::path::Path::new(path))?;
    if canceled(cancel, tx) {
        return Ok(());
    }
    let count = lottie.frame_count().max(1);
    let step = Duration::from_secs_f64(1.0 / lottie.frame_rate().clamp(1.0, 60.0));
    loop {
        for i in 0..count {
            if canceled(cancel, tx) {
                return Ok(());
            }
            let started = Instant::now();
            // Native rendering cannot be preempted; its permit remains held.
            let rgba = lottie.render(i, w, h);
            if canceled(cancel, tx) || tx.blocking_send((w, h, rgba)).is_err() {
                return Ok(());
            }
            if let Some(rest) = step.checked_sub(started.elapsed()) {
                std::thread::sleep(rest);
            }
        }
    }
}

impl App {
    /// ▶ on a voice message: download, decode, play; ⏸ pauses.
    /// A video started (or resumed): a playing voice message stops
    /// talking over it, and starting or resuming the voice message pauses
    /// the video the same way (`playback_pause_video`, video.rs).
    pub(super) fn playback_pause_voice(&mut self) {
        if let (Some(player), Some((id, position, true))) =
            (&self.session.playback.player, self.session.playback.voice)
        {
            player.pause();
            self.session.playback.voice = Some((id, position, false));
        }
    }

    pub(super) fn voice_toggle(&mut self, file_id: i32) -> Task<Msg> {
        let player = match &self.session.playback.player {
            Some(p) => p.clone(),
            None => match Player::new() {
                Ok(p) => {
                    self.session.playback.player = Some(p.clone());
                    p
                }
                Err(e) => {
                    self.error = Some(e);
                    return Task::none();
                }
            },
        };
        if let Some((id, position, playing)) = self.session.playback.voice
            && id == file_id
        {
            if playing {
                player.pause();
                self.session.playback.voice = Some((id, position, false));
                return Task::none();
            }
            if let Some(pcm) = self.cached_pcm(file_id) {
                player.play(i64::from(file_id), pcm, position);
                self.session.playback.voice = Some((id, position, true));
                self.playback_pause_video();
                return Task::none();
            }
        }
        self.session.playback.pending_voice = Some(file_id);
        if let Some(pcm) = self.cached_pcm(file_id) {
            return self.voice_start(file_id, pcm);
        }
        match self.session.files.get(&file_id) {
            Some(f) if f.done => {
                let path = PathBuf::from(&f.path);
                // Mono is enough for voice (the player duplicates channels)
                // and halves the memory of the decoded message.
                let (rate, _) = player.output_format();
                let channels = 1;
                Task::perform(
                    async move {
                        tokio::task::spawn_blocking(move || {
                            crate::av::audio::decode_to_pcm(&path, rate, channels)
                        })
                        .await
                        .map_err(|e| e.to_string())
                        .and_then(|r| r)
                    },
                    move |r| Msg::VoiceDecoded(file_id, r.map(|p| PcmHandle(Arc::new(p)))),
                )
            }
            Some(f) if f.downloading => Task::none(),
            _ => self.request_download(file_id, 30),
        }
    }

    fn cached_pcm(&self, file_id: i32) -> Option<Arc<Pcm>> {
        self.session
            .playback
            .pcm
            .iter()
            .find(|(id, _)| *id == file_id)
            .map(|(_, p)| p.clone())
    }

    pub(super) fn voice_decoded(&mut self, file_id: i32, pcm: Arc<Pcm>) -> Task<Msg> {
        self.session.playback.pcm.retain(|(id, _)| *id != file_id);
        self.session.playback.pcm.push((file_id, pcm.clone()));
        if self.session.playback.pcm.len() > PCM_CACHE {
            self.session.playback.pcm.remove(0);
        }
        self.voice_start(file_id, pcm)
    }

    fn voice_start(&mut self, file_id: i32, pcm: Arc<Pcm>) -> Task<Msg> {
        if self.session.playback.pending_voice != Some(file_id) {
            return Task::none();
        }
        self.session.playback.pending_voice = None;
        if let Some(player) = &self.session.playback.player {
            player.play(i64::from(file_id), pcm, Duration::ZERO);
            self.session.playback.voice = Some((file_id, Duration::ZERO, true));
            self.playback_pause_video();
        }
        Task::none()
    }

    /// Periodic refresh of the voice player and recording timer. A
    /// recording stops and is sent at the length the decoder can play
    /// back, unless its window no longer shows its chat (closing the
    /// window or switching the chat already cancels it, but this covers
    /// any other path that changes what a window shows).
    pub(super) fn playback_tick(&mut self) -> Task<Msg> {
        if let Some(rec) = self.session.playback.recording.as_ref()
            && rec.started.elapsed() >= crate::av::audio::MAX_DECODE
        {
            let send = self
                .session
                .panes
                .get(&rec.window)
                .is_some_and(|p| p.shows(rec.chat_id));
            return self.record_finish(send);
        }
        if let Some(player) = &self.session.playback.player {
            self.session.playback.voice = player
                .state()
                .map(|(id, position, playing)| (id as i32, position, playing));
        }
        Task::none()
    }

    /// A finished download may be waited for by playback.
    pub(super) fn playback_file_done(&mut self, file_id: i32) -> Task<Msg> {
        let mut tasks = Vec::new();
        if self.session.playback.pending_voice == Some(file_id) {
            self.session.playback.pending_voice = None;
            self.session.playback.voice = None;
            tasks.push(self.voice_toggle(file_id));
        }
        if let Some(motion) = self.session.playback.wanted.remove(&file_id) {
            tasks.push(self.start_animation(file_id, motion));
        }
        if let Some(i) = self
            .session
            .playback
            .open_when_done
            .iter()
            .position(|&f| f == file_id)
        {
            self.session.playback.open_when_done.remove(i);
            self.open_file(file_id, false);
        }
        Task::batch(tasks)
    }

    /// Video: open in the system player (with sound), downloading first.
    pub(super) fn play_video(&mut self, file_id: i32) -> Task<Msg> {
        match self.session.files.get(&file_id) {
            Some(f) if f.done => {
                self.open_file(file_id, false);
                Task::none()
            }
            Some(f) if f.downloading => {
                self.session.playback.open_when_done.push(file_id);
                Task::none()
            }
            _ => {
                self.session.playback.open_when_done.push(file_id);
                self.request_download(file_id, 28)
            }
        }
    }

    /// An animation came on screen: play it (downloading first). The same
    /// file is often on screen more than once (a sticker sent twice, a
    /// forwarded GIF, or the same chat open in two windows); ref-counted so
    /// hiding one copy does not stop the others.
    pub(super) fn animation_shown(&mut self, file_id: i32, motion: Motion) -> Task<Msg> {
        let refs = self.session.playback.shown.entry(file_id).or_insert(0);
        *refs += 1;
        if *refs > 1 {
            return Task::none();
        }
        // Large files are not fetched just because they scrolled by: those
        // play on click (system player) instead.
        let limit = match motion {
            Motion::Video if self.is_sticker(file_id) => MAX_AUTO_STICKER,
            Motion::Lottie | Motion::Still => MAX_AUTO_STICKER,
            Motion::Video => MAX_AUTO_ANIMATION,
        };
        if self
            .session
            .files
            .get(&file_id)
            .is_some_and(|f| f.size > limit)
        {
            return Task::none();
        }
        match self.session.files.get(&file_id) {
            Some(f) if f.done => self.start_animation(file_id, motion),
            Some(f) if f.downloading => {
                self.session.playback.wanted.insert(file_id, motion);
                Task::none()
            }
            _ => {
                self.session.playback.wanted.insert(file_id, motion);
                self.request_download(file_id, 12)
            }
        }
    }

    pub(super) fn animation_hidden(&mut self, file_id: i32) {
        if let Some(refs) = self.session.playback.shown.get_mut(&file_id) {
            *refs = refs.saturating_sub(1);
            if *refs > 0 {
                return;
            }
            self.session.playback.shown.remove(&file_id);
        }
        self.session.playback.wanted.remove(&file_id);
        if let Some(anim) = self.session.playback.anims.remove(&file_id) {
            anim.stop.abort();
        }
    }

    /// The frame stream ended on its own (a broken or empty file: `frames`
    /// swallows decode errors, so this is the only sign of it) rather than
    /// through `animation_hidden`'s explicit abort: frees the slot it held
    /// in `MAX_ANIMATIONS` so another animation can take it. A no-op if the
    /// entry is already gone (e.g. `animation_hidden` ran first).
    pub(super) fn animation_ended(&mut self, file_id: i32) {
        self.session.playback.anims.remove(&file_id);
    }

    fn start_animation(&mut self, file_id: i32, motion: Motion) -> Task<Msg> {
        self.start_animation_with_workers(file_id, motion, &ANIMATION_WORKERS)
    }

    /// Admission path shared by the UI and controlled local-pool tests.
    pub(crate) fn start_animation_with_workers(
        &mut self,
        file_id: i32,
        motion: Motion,
        workers: &Arc<tokio::sync::Semaphore>,
    ) -> Task<Msg> {
        if self.session.playback.anims.len() >= MAX_ANIMATIONS {
            return Task::none();
        }
        let Some(path) = self.session.files.get(&file_id).map(|f| f.path.clone()) else {
            return Task::none();
        };
        let (w, h) = match motion {
            Motion::Lottie => (STICKER_SIZE as u32 * 2, STICKER_SIZE as u32 * 2),
            _ => (
                (super::media::PHOTO_MAX_W * 2.0) as u32,
                (super::media::PHOTO_MAX_H * 2.0) as u32,
            ),
        };
        let (tx, rx) = tokio::sync::mpsc::channel(2);
        let Some(cancel) = spawn_animation_worker(workers, move |cancel| {
            let _ = match motion {
                Motion::Lottie => play_lottie(&path, w, h, &tx, &cancel),
                _ => play_video(&path, w, h, &tx, cancel),
            };
        }) else {
            return Task::none();
        };
        let (task, stop) = Task::run(frames(rx, cancel), move |(fw, fh, rgba)| {
            Msg::AnimFrame(file_id, fw, fh, rgba)
        })
        // Runs only if the stream above ends by itself (broken/empty
        // file): an explicit hide aborts this whole chain instead, so it
        // does not also fire here on top of `animation_hidden`'s own
        // cleanup.
        .chain(Task::done(Msg::AnimEnded(file_id)))
        .abortable();
        self.session.playback.anims.insert(
            file_id,
            Anim {
                frame: None,
                prev: None,
                stop,
            },
        );
        task
    }

    fn is_sticker(&self, file_id: i32) -> bool {
        self.session
            .panes
            .values()
            .flat_map(|p| p.messages.iter())
            .any(|m| matches!(&m.media, Some(Media::Sticker { file_id: f, .. }) if *f == file_id))
    }

    pub(super) fn animation_frame(&mut self, file_id: i32, w: u32, h: u32, rgba: Vec<u8>) {
        if let Some(anim) = self.session.playback.anims.get_mut(&file_id) {
            anim.prev = anim.frame.replace(image::Handle::from_rgba(w, h, rgba));
        }
    }

    /// 🎤 in a chat: start recording a voice message.
    pub(super) fn record_start(&mut self, window: WinId) -> Task<Msg> {
        if self.session.playback.recording.is_some() {
            return Task::none();
        }
        let Some(chat_id) = self.session.panes.get(&window).and_then(|p| p.chat_id) else {
            return Task::none();
        };
        Task::perform(
            async {
                tokio::task::spawn_blocking(Recorder::start)
                    .await
                    .map_err(|e| e.to_string())
                    .and_then(|r| r)
            },
            move |r| Msg::RecordStarted(window, chat_id, r.map(RecorderHandle)),
        )
    }

    pub(super) fn record_finish(&mut self, send: bool) -> Task<Msg> {
        let Some(rec) = self.session.playback.recording.take() else {
            return Task::none();
        };
        if !send {
            rec.recorder.cancel();
            return Task::none();
        }
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let path = crate::paths::tmp().join(format!("voice-{stamp}.ogg"));
        let (chat_id, client_id) = (rec.chat_id, self.session.client_id);
        let recorder = rec.recorder;
        Task::perform(
            async move {
                let _ = std::fs::create_dir_all(crate::paths::tmp());
                let recording = tokio::task::spawn_blocking(move || recorder.finish(&path))
                    .await
                    .map_err(|e| e.to_string())
                    .and_then(|r| r)?;
                let sent = td::send_voice(
                    client_id,
                    chat_id,
                    recording.path.to_string_lossy().into_owned(),
                    recording.duration.as_secs() as i32,
                    &recording.waveform,
                )
                .await;
                // TDLib uploads after `sendMessage` returns, so a sent
                // recording stays until the daily tmp cleanup; a failed one
                // is not needed.
                if sent.is_err() {
                    let _ = std::fs::remove_file(&recording.path);
                }
                sent
            },
            Msg::Done,
        )
    }

    /// `RecordStarted` may arrive after its window closed, after its chat
    /// changed, or while another recording is already running (a double
    /// click on 🎤 before the first `RecordStarted` came back): the new
    /// recorder is not needed and is cancelled instead of silently
    /// replacing the real one or recording into the void.
    pub(super) fn record_started(&mut self, window: WinId, chat_id: i64, recorder: Recorder) {
        let live = self
            .session
            .panes
            .get(&window)
            .is_some_and(|p| p.shows(chat_id));
        if !live || self.session.playback.recording.is_some() {
            recorder.cancel();
            return;
        }
        self.session.playback.recording = Some(Recording {
            window,
            chat_id,
            recorder,
            started: Instant::now(),
        });
    }

    /// The recording in `window`, if any, does not survive the window
    /// closing: otherwise the microphone keeps recording invisibly and
    /// `playback_tick` sends it after `MAX_DECODE` to a chat nobody is
    /// looking at any more.
    pub(super) fn cancel_recording_in(&mut self, window: WinId) -> Task<Msg> {
        if self
            .session
            .playback
            .recording
            .as_ref()
            .is_some_and(|r| r.window == window)
        {
            self.record_finish(false)
        } else {
            Task::none()
        }
    }

    /// Same, but only if `window` is actually about to show a different
    /// chat than the recording's: reselecting the chat that is already
    /// open must not interrupt it.
    pub(super) fn cancel_recording_leaving(&mut self, window: WinId, chat_id: i64) -> Task<Msg> {
        if self.session.panes.get(&window).and_then(|p| p.chat_id) == Some(chat_id) {
            return Task::none();
        }
        self.cancel_recording_in(window)
    }
}

#[cfg(test)]
mod frame_tests {
    use super::*;

    #[test]
    fn the_previous_frame_stays_under_the_new_one() {
        let mut app = crate::app::tests::app();
        let (_, stop) = Task::<Msg>::none().abortable();
        app.session.playback.anims.insert(
            7,
            Anim {
                frame: None,
                prev: None,
                stop,
            },
        );
        app.animation_frame(7, 1, 1, vec![1; 4]);
        let first = app.session.playback.frame(7).cloned();
        assert!(app.session.playback.previous_frame(7).is_none());
        app.animation_frame(7, 1, 1, vec![2; 4]);
        assert_eq!(app.session.playback.previous_frame(7), first.as_ref());
        assert_ne!(app.session.playback.frame(7), first.as_ref());
    }

    /// A stream that ends by itself (a broken or empty file: `frames`
    /// swallows the decode error) must free its `MAX_ANIMATIONS` slot via
    /// `AnimEnded`, exactly as an explicit `AnimHide` would, even though it
    /// never goes through `animation_hidden`.
    #[test]
    fn a_stream_that_ends_on_its_own_frees_its_slot() {
        let mut app = crate::app::tests::app();
        for file_id in 100..100 + MAX_ANIMATIONS as i32 {
            let (_, stop) = Task::<Msg>::none().abortable();
            app.session.playback.anims.insert(
                file_id,
                Anim {
                    frame: None,
                    prev: None,
                    stop,
                },
            );
        }
        assert_eq!(app.session.playback.anims.len(), MAX_ANIMATIONS);

        let _ = app.update(Msg::AnimEnded(100));
        assert!(
            !app.session.playback.playing(100),
            "the broken stream's slot must be freed"
        );
        assert_eq!(app.session.playback.anims.len(), MAX_ANIMATIONS - 1);

        // Already gone (e.g. `animation_hidden` ran first): a no-op, not a
        // panic or an extra removal.
        app.animation_ended(100);
        assert_eq!(app.session.playback.anims.len(), MAX_ANIMATIONS - 1);
    }

    #[test]
    fn security_fix_hidden_animation_workers_hold_all_slots_until_native_decode_exits() {
        use std::sync::mpsc;

        let workers = Arc::new(tokio::sync::Semaphore::new(MAX_ANIMATIONS));

        let (started_tx, started_rx) = mpsc::channel();
        let mut releases = Vec::new();
        let mut cancellations = Vec::new();
        for id in 0..MAX_ANIMATIONS {
            let (release_tx, release_rx) = mpsc::channel::<()>();
            let started_tx = started_tx.clone();
            let cancel = spawn_animation_worker(&workers, move |_| {
                started_tx.send(id).unwrap();
                // Simulates a native decode that cannot notice cancellation
                // until it returns; the slot must remain occupied meanwhile.
                let _ = release_rx.recv();
            })
            .expect("a free decoder slot admits an animation");
            releases.push(release_tx);
            cancellations.push(cancel);
        }
        for _ in 0..MAX_ANIMATIONS {
            started_rx
                .recv_timeout(Duration::from_secs(5))
                .expect("admitted native worker starts");
        }

        for cancel in &cancellations {
            cancel.store(true, Ordering::Relaxed);
        }
        assert_eq!(workers.available_permits(), 0);
        assert!(
            spawn_animation_worker(&workers, |_| panic!("refused worker must not run")).is_none(),
            "canceling a UI stream cannot admit a fourth native worker"
        );

        let mut app = crate::app::tests::app();
        app.session.files.insert(
            44,
            super::super::media::FileState {
                path: "not-opened-when-admission-is-refused".into(),
                done: true,
                ..Default::default()
            },
        );
        let _ = app.start_animation_with_workers(44, Motion::Video, &workers);
        assert!(
            !app.session.playback.playing(44),
            "refused animation must never be reported as playing"
        );

        releases.remove(0).send(()).unwrap();
        let start = Instant::now();
        while workers.available_permits() == 0 {
            assert!(
                start.elapsed() < Duration::from_secs(5),
                "finished native worker did not release its slot"
            );
            std::thread::yield_now();
        }
        let (replacement_tx, replacement_rx) = mpsc::channel();
        let replacement = spawn_animation_worker(&workers, move |_| {
            replacement_tx.send(()).unwrap();
        });
        assert!(
            replacement.is_some(),
            "a completed native worker frees admission"
        );
        replacement_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("replacement worker actually starts");
        // Dropping the remaining gates releases their workers even if a
        // subsequent assertion fails; no worker leaks into another test.
        drop(releases);
        let start = Instant::now();
        while workers.available_permits() != MAX_ANIMATIONS {
            assert!(
                start.elapsed() < Duration::from_secs(5),
                "native worker did not relinquish its slot"
            );
            std::thread::yield_now();
        }
    }
}
