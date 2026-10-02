//! Video player core: demuxing and decoding on a thread of its own, video
//! frames as YUV 4:2:0 planes (the GPU converts them), sound through cpal.
//! The sound is the clock: a frame is shown once the sound has reached it.
//! Without a sound track (or device) a wall clock stands in.
//!
//! The input is any `Read + Seek` source, so a file that is still being
//! downloaded can play: the source blocks until the bytes asked for arrive.

use std::collections::VecDeque;
use std::io::{Read, Seek};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use ffmpeg_next as ff;
use parking_lot::Mutex;

use super::audio::resample;
use super::video::{
    CODECS, CONTAINERS, fit_box, init_ffmpeg, pts_to_duration, valid_frame, whitelist_options,
};

/// Largest decoded picture; the GPU scales it to the widget.
pub const MAX_W: u32 = 1920;
pub const MAX_H: u32 = 1080;
/// Decoded frames kept ahead of the clock (a 1080p frame is ~3 MB).
const AHEAD_FRAMES: usize = 6;
/// Decoded sound kept ahead of the clock.
const AHEAD_AUDIO: Duration = Duration::from_millis(700);
/// Hard cap on buffered sound, independent of how many video frames are
/// queued: a single still frame stretched over a long (or hostile) audio
/// track must not decode the whole file's sound into memory.
const MAX_AHEAD_AUDIO: Duration = Duration::from_secs(3);
/// How long the sound can be starved (device consuming nothing new) before
/// the wall clock takes over the position instead of leaving it frozen —
/// long enough that ordinary decode scheduling jitter never trips it.
const STALL_GRACE: Duration = Duration::from_millis(250);

/// Sound codecs of Telegram videos; anything else plays silent.
const AUDIO_CODECS: &[ff::codec::Id] = &[
    ff::codec::Id::AAC,
    ff::codec::Id::OPUS,
    ff::codec::Id::MP3,
    ff::codec::Id::VORBIS,
];

pub trait Source: Read + Seek + Send + 'static {}
impl<T: Read + Seek + Send + 'static> Source for T {}

/// One picture: Y at full size, U and V at half size (rows packed).
#[derive(Debug)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub y: Vec<u8>,
    pub u: Vec<u8>,
    pub v: Vec<u8>,
    pub pts: Duration,
    /// Unique per decoded frame: tells a new picture from the one shown.
    pub serial: u64,
}

/// Next `Frame::serial`.
pub fn next_serial() -> u64 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

struct Sound {
    /// Interleaved samples at the device's rate and channel count.
    samples: VecDeque<f32>,
    /// Frames (samples per channel) played since `Clock::base`.
    played: u64,
    rate: u32,
    channels: u16,
    /// Set by `fill_sound` the moment the queue runs dry while playing;
    /// cleared once real samples flow again. Lets `position()` keep the
    /// clock moving through a gap or a track that ended before the video
    /// instead of freezing (see `position` and `STALL_GRACE`).
    stalled: Option<Instant>,
}

struct Clock {
    /// Media time at the last seek or pause.
    base: Duration,
    /// Wall clock start while playing without sound.
    since: Option<Instant>,
}

#[derive(Default)]
struct Info {
    duration: Option<Duration>,
    size: Option<(u32, u32)>,
    error: Option<String>,
}

struct Shared {
    frames: Mutex<VecDeque<Arc<Frame>>>,
    sound: Mutex<Option<Sound>>,
    clock: Mutex<Clock>,
    info: Mutex<Info>,
    seek: Mutex<Option<Duration>>,
    paused: AtomicBool,
    /// f32 bits.
    volume: AtomicU32,
    /// All packets read and decoded.
    finished: AtomicBool,
    stop: AtomicBool,
}

impl Shared {
    fn position(&self) -> Duration {
        let clock = self.clock.lock();
        if let Some(sound) = &*self.sound.lock() {
            let played = clock.base
                + Duration::from_secs_f64(sound.played as f64 / f64::from(sound.rate.max(1)));
            return match sound.stalled {
                // The sound track ended before the video, hit a gap the
                // decoder cannot cross on its own (see `full`), or the
                // device fell behind: run the wall clock from the moment
                // it stalled instead of leaving playback stuck forever.
                Some(since)
                    if self.finished.load(Ordering::Relaxed) || since.elapsed() > STALL_GRACE =>
                {
                    played + since.elapsed()
                }
                _ => played,
            };
        }
        match clock.since {
            Some(since) => clock.base + since.elapsed(),
            None => clock.base,
        }
    }
}

pub struct Player {
    shared: Arc<Shared>,
    /// Keeps the sound thread (and its cpal stream) alive.
    _sound: Option<std::sync::mpsc::Sender<()>>,
}

impl Player {
    /// Starts playing `source` paused at the start. `sound` = use the
    /// default output device (tests pass `false`). Opening happens on the
    /// player thread: a streaming source may block there; problems show up
    /// in `error()`.
    pub fn open(source: impl Source, name: Option<String>, sound: bool) -> Player {
        let shared = Arc::new(Shared {
            frames: Mutex::new(VecDeque::new()),
            sound: Mutex::new(None),
            clock: Mutex::new(Clock {
                base: Duration::ZERO,
                since: None,
            }),
            info: Mutex::new(Info::default()),
            seek: Mutex::new(None),
            paused: AtomicBool::new(true),
            volume: AtomicU32::new(1.0f32.to_bits()),
            finished: AtomicBool::new(false),
            stop: AtomicBool::new(false),
        });
        let keepalive = if sound {
            open_sound(Arc::clone(&shared))
        } else {
            None
        };
        let thread_shared = Arc::clone(&shared);
        let spawned = thread::Builder::new()
            .name("video-player".into())
            .spawn(move || {
                if let Err(e) = decode(source, name, &thread_shared)
                    && !thread_shared.stop.load(Ordering::Relaxed)
                {
                    thread_shared.info.lock().error = Some(e);
                }
                thread_shared.finished.store(true, Ordering::Relaxed);
            });
        if let Err(e) = spawned {
            shared.info.lock().error = Some(format!("видео: поток не запустился: {e}"));
        }
        Player {
            shared,
            _sound: keepalive,
        }
    }

    /// The frame to show now; older frames are dropped.
    pub fn frame(&self) -> Option<Arc<Frame>> {
        let position = self.shared.position();
        let mut frames = self.shared.frames.lock();
        while frames.len() > 1 && frames[1].pts <= position {
            frames.pop_front();
        }
        frames.front().cloned()
    }

    pub fn position(&self) -> Duration {
        let position = self.shared.position();
        match self.duration() {
            Some(d) => position.min(d),
            None => position,
        }
    }

    pub fn duration(&self) -> Option<Duration> {
        self.shared.info.lock().duration
    }

    /// Size of the decoded picture, once known.
    pub fn size(&self) -> Option<(u32, u32)> {
        self.shared.info.lock().size
    }

    pub fn error(&self) -> Option<String> {
        self.shared.info.lock().error.clone()
    }

    pub fn paused(&self) -> bool {
        self.shared.paused.load(Ordering::Relaxed)
    }

    pub fn set_paused(&self, paused: bool) {
        let position = self.shared.position();
        let mut clock = self.shared.clock.lock();
        let mut sound = self.shared.sound.lock();
        match sound.as_mut() {
            Some(sound) if paused => {
                // The wall clock may have been standing in for a stalled or
                // finished sound track (see `Shared::position`): fold what
                // it already added into `base` and clear the marker so
                // pausing freezes the position instead of the wall clock
                // continuing to run underneath it while paused.
                // `fill_sound` marks a fresh stall if still starved once
                // resumed.
                if let Some(since) = sound.stalled.take()
                    && (self.shared.finished.load(Ordering::Relaxed)
                        || since.elapsed() > STALL_GRACE)
                {
                    clock.base += since.elapsed();
                }
            }
            Some(_) => {}
            None => {
                clock.base = position;
                clock.since = (!paused).then(Instant::now);
            }
        }
        self.shared.paused.store(paused, Ordering::Relaxed);
    }

    /// Jumps to `to` (the nearest earlier key frame is decoded from, the
    /// frames before `to` are skipped).
    pub fn seek(&self, to: Duration) {
        let to = match self.duration() {
            Some(d) => to.min(d),
            None => to,
        };
        *self.shared.seek.lock() = Some(to);
        self.shared.finished.store(false, Ordering::Relaxed);
    }

    /// 0.0..=1.0.
    pub fn set_volume(&self, volume: f32) {
        self.shared
            .volume
            .store(volume.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
    }

    pub fn volume(&self) -> f32 {
        f32::from_bits(self.shared.volume.load(Ordering::Relaxed))
    }

    /// Nothing left to show: decoded to the end and the clock is past it.
    pub fn ended(&self) -> bool {
        if !self.shared.finished.load(Ordering::Relaxed) || self.shared.seek.lock().is_some() {
            return false;
        }
        let position = self.shared.position();
        let frames_done = self
            .shared
            .frames
            .lock()
            .back()
            .is_none_or(|last| last.pts <= position);
        let sound_done = self
            .shared
            .sound
            .lock()
            .as_ref()
            .is_none_or(|s| s.samples.is_empty());
        frames_done && sound_done
    }

    /// Playing, but nothing to show yet (the file is still coming).
    pub fn buffering(&self) -> bool {
        !self.shared.finished.load(Ordering::Relaxed) && self.shared.frames.lock().is_empty()
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        // The decode thread sees it at its next step or read and exits.
        self.shared.stop.store(true, Ordering::Relaxed);
    }
}

/// Opens the default output device on a thread of its own (cpal streams
/// may not move between threads); the device's format goes into `shared`.
fn open_sound(shared: Arc<Shared>) -> Option<std::sync::mpsc::Sender<()>> {
    let (keep_tx, keep_rx) = std::sync::mpsc::channel::<()>();
    let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel::<bool>(0);
    thread::Builder::new()
        .name("video-sound".into())
        .spawn(move || {
            let callback_shared = Arc::clone(&shared);
            let fill = move |out: &mut [f32]| fill_sound(&callback_shared, out);
            match super::audio::open_output(fill) {
                Ok((stream, rate, channels)) => {
                    *shared.sound.lock() = Some(Sound {
                        samples: VecDeque::new(),
                        played: 0,
                        rate,
                        channels,
                        stalled: None,
                    });
                    let _ = ready_tx.send(true);
                    let _ = keep_rx.recv();
                    drop(stream);
                }
                Err(_) => {
                    let _ = ready_tx.send(false);
                }
            }
        })
        .ok()?;
    ready_rx.recv().ok().filter(|ok| *ok).map(|_| keep_tx)
}

fn fill_sound(shared: &Shared, out: &mut [f32]) {
    let playing = !shared.paused.load(Ordering::Relaxed);
    let volume = f32::from_bits(shared.volume.load(Ordering::Relaxed));
    let mut resumed_after = None;
    {
        let mut guard = shared.sound.lock();
        let Some(sound) = guard.as_mut() else {
            out.fill(0.0);
            return;
        };
        let channels = usize::from(sound.channels).max(1);
        let mut filled = 0usize;
        for sample in out.iter_mut() {
            *sample = if playing {
                match sound.samples.pop_front() {
                    Some(s) => {
                        filled += 1;
                        s * volume
                    }
                    // Starved: silence, and the clock does not move.
                    None => 0.0,
                }
            } else {
                // Paused: output silence without consuming buffered samples.
                0.0
            };
        }
        sound.played += (filled / channels) as u64;
        if playing && filled == 0 && !out.is_empty() {
            sound.stalled.get_or_insert_with(Instant::now);
        } else if filled > 0 {
            resumed_after = sound.stalled.take();
        }
    }
    if let Some(since) = resumed_after {
        // The gap is over: fold the wall-clock time it lasted into `base`
        // so the position does not jump backward now that real samples are
        // flowing again. Locks `clock` only after `sound` is released,
        // matching `Shared::position()`'s clock-then-sound order.
        shared.clock.lock().base += since.elapsed();
    }
}

struct Streams {
    video: ff::decoder::Video,
    video_index: usize,
    video_tb: ff::Rational,
    scaler: Option<(ff::format::Pixel, u32, u32, ff::software::scaling::Context)>,
    audio: Option<(ff::decoder::Audio, usize, ff::Rational)>,
    resampler: Option<ff::software::resampling::Context>,
    /// Set on open and after every seek; cleared by `drain_audio` once it
    /// anchors `clock.base` on the first accepted audio frame's pts (see
    /// there) instead of leaving the position based on assumed zero/seek-
    /// target offsets that a nonzero stream `start_time` or a seek landing
    /// slightly ahead of its target would throw out of sync.
    audio_base_pending: bool,
}

/// Falls back to the wall clock: clears `sound` and re-bases `clock` on the
/// current position so playback does not freeze waiting for samples that
/// will never come (no usable audio stream, or its resampler could not be
/// (re)built — see `decode` and `drain_audio`).
fn use_wall_clock(shared: &Shared) {
    let position = shared.position();
    *shared.sound.lock() = None;
    let mut clock = shared.clock.lock();
    clock.base = position;
    clock.since = (!shared.paused.load(Ordering::Relaxed)).then(Instant::now);
}

fn decode(source: impl Source, name: Option<String>, shared: &Arc<Shared>) -> Result<(), String> {
    init_ffmpeg();
    let io =
        ff::format::context::StreamIo::from_read_seek(source).map_err(|e| format!("видео: {e}"))?;
    let stop = Arc::clone(shared);
    // Files come from strangers: only the containers and codecs Telegram
    // uses reach ffmpeg's demuxer/decoders — passed at open time so
    // `avformat_find_stream_info` (which decodes frames to probe streams)
    // is bound by the same allow list, not just the check below (see
    // `video::whitelist_options`).
    let codecs: Vec<ff::codec::Id> = CODECS.iter().chain(AUDIO_CODECS).copied().collect();
    let options = whitelist_options(CONTAINERS, &codecs);
    let mut input = ff::format::input_from_stream_with_interrupt(
        io,
        name.as_deref(),
        Some(options),
        move || stop.stop.load(Ordering::Relaxed),
    )
    .map_err(|e| format!("видео: не удалось открыть: {e}"))?;
    if !CONTAINERS.contains(&input.format().name()) {
        return Err(format!(
            "видео: неподдерживаемый формат {}",
            input.format().name()
        ));
    }
    let mut streams = open_streams(&input)?;
    {
        let mut info = shared.info.lock();
        if input.duration() > 0 {
            info.duration = Some(Duration::from_micros(input.duration() as u64));
        }
        let (w, h) = fit_even(streams.video.width(), streams.video.height());
        info.size = Some((w, h));
    }
    if streams.audio.is_none() {
        // No sound track: the wall clock runs the show.
        use_wall_clock(shared);
    }
    let mut skip_until = Duration::ZERO;
    let mut eof = false;
    let mut packet = ff::Packet::empty();
    loop {
        if shared.stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        // Released before the seek I/O below: matroska/webm and fragmented
        // MP4 can read from the (possibly still-downloading) source there,
        // and the UI thread calls `ended()` on every `view()`, which also
        // locks `seek` — holding it through the read would freeze the UI.
        let pending = shared.seek.lock().take();
        if let Some(to) = pending {
            let ts = to.as_micros() as i64;
            input
                .seek(ts, ..ts + 1)
                .map_err(|e| format!("видео: перемотка: {e}"))?;
            streams.video.flush();
            if let Some((audio, _, _)) = &mut streams.audio {
                audio.flush();
            }
            shared.frames.lock().clear();
            {
                let mut clock = shared.clock.lock();
                clock.base = to;
                clock.since = (!shared.paused.load(Ordering::Relaxed)).then(Instant::now);
            }
            if let Some(sound) = &mut *shared.sound.lock() {
                sound.samples.clear();
                sound.played = 0;
                sound.stalled = None;
            }
            skip_until = to;
            eof = false;
            shared.finished.store(false, Ordering::Relaxed);
            // A seek lands on the nearest key frame, not exactly `to`; the
            // first accepted post-seek audio frame's own pts (see
            // `drain_audio`) replaces this provisional base once known, so
            // the audio does not end up leading the picture by up to one
            // frame.
            streams.audio_base_pending = true;
        }
        if eof || full(shared) {
            thread::sleep(Duration::from_millis(5));
            continue;
        }
        match packet.read(&mut input) {
            Ok(()) => {
                if packet.stream() == streams.video_index {
                    streams
                        .video
                        .send_packet(&packet)
                        .map_err(|e| format!("видео: {e}"))?;
                    drain_video(&mut streams, shared, skip_until)?;
                } else if let Some((audio, index, _)) = &mut streams.audio
                    && packet.stream() == *index
                {
                    // A broken sound packet is skipped, the picture goes on.
                    if audio.send_packet(&packet).is_ok() {
                        drain_audio(&mut streams, shared, skip_until);
                    }
                }
            }
            Err(ff::Error::Eof) => {
                let _ = streams.video.send_eof();
                drain_video(&mut streams, shared, skip_until)?;
                if let Some((audio, _, _)) = &mut streams.audio {
                    let _ = audio.send_eof();
                }
                drain_audio(&mut streams, shared, skip_until);
                eof = true;
                shared.finished.store(true, Ordering::Relaxed);
            }
            Err(ff::Error::Exit) => return Ok(()),
            Err(e) => return Err(format!("видео: ошибка чтения: {e}")),
        }
    }
}

/// Enough is decoded ahead: wait for the clock.
fn full(shared: &Shared) -> bool {
    // Frames the clock has passed make room, whether or not anyone drew them.
    let position = shared.position();
    let frames = {
        let mut frames = shared.frames.lock();
        while frames.len() > 1 && frames[1].pts <= position {
            frames.pop_front();
        }
        frames.len()
    };
    if frames >= AHEAD_FRAMES * 3 {
        return true;
    }
    let sound = shared.sound.lock();
    let Some(sound) = sound.as_ref() else {
        return frames >= AHEAD_FRAMES;
    };
    let per_second = sound.rate as usize * usize::from(sound.channels);
    let buffered = sound.samples.len();
    // However few video frames there are, buffered sound must not grow
    // without bound: a single still frame stretched over a long (or
    // hostile) audio track would otherwise decode the whole file's sound
    // into memory before ever hitting the frame-count cap above.
    if buffered >= per_second * MAX_AHEAD_AUDIO.as_secs() as usize {
        return true;
    }
    frames >= AHEAD_FRAMES && buffered >= per_second * AHEAD_AUDIO.as_millis() as usize / 1000
}

/// Even sides fitting the decode limit (4:2:0 planes halve them).
fn fit_even(w: u32, h: u32) -> (u32, u32) {
    let (w, h) = fit_box(w, h, MAX_W, MAX_H);
    ((w & !1).max(2), (h & !1).max(2))
}

fn open_streams(input: &ff::format::context::Input) -> Result<Streams, String> {
    let stream = input
        .streams()
        .best(ff::media::Type::Video)
        .ok_or("видео: в файле нет видеодорожки")?;
    if !CODECS.contains(&stream.parameters().id()) {
        return Err(format!(
            "видео: неподдерживаемый кодек {:?}",
            stream.parameters().id()
        ));
    }
    let mut ctx = ff::codec::context::Context::from_parameters(stream.parameters())
        .map_err(|e| format!("видео: {e}"))?;
    ctx.set_threading(ff::threading::Config {
        kind: ff::threading::Type::Frame,
        count: 0,
    });
    let video = ctx.decoder().video().map_err(|e| format!("видео: {e}"))?;
    if !valid_frame(video.format(), video.width(), video.height()) {
        return Err("видео: повреждённый файл".into());
    }
    let audio = input
        .streams()
        .best(ff::media::Type::Audio)
        .filter(|s| AUDIO_CODECS.contains(&s.parameters().id()))
        .and_then(|s| {
            let decoder = ff::codec::context::Context::from_parameters(s.parameters())
                .ok()?
                .decoder()
                .audio()
                .ok()?;
            let valid = (1..=384_000).contains(&decoder.rate())
                && (1..=8).contains(&decoder.channels())
                && decoder.format() != ff::format::Sample::None;
            valid.then_some((decoder, s.index(), s.time_base()))
        });
    Ok(Streams {
        video_index: stream.index(),
        video_tb: stream.time_base(),
        video,
        scaler: None,
        audio,
        resampler: None,
        audio_base_pending: true,
    })
}

fn drain_video(streams: &mut Streams, shared: &Shared, skip_until: Duration) -> Result<(), String> {
    let mut decoded = ff::frame::Video::empty();
    while streams.video.receive_frame(&mut decoded).is_ok() {
        let pts = decoded
            .pts()
            .map(|p| pts_to_duration(p, streams.video_tb))
            .unwrap_or_default();
        if pts + Duration::from_millis(40) < skip_until {
            continue;
        }
        let frame = to_yuv(streams, &decoded, pts)?;
        shared.info.lock().size = Some((frame.width, frame.height));
        shared.frames.lock().push_back(Arc::new(frame));
    }
    Ok(())
}

fn to_yuv(
    streams: &mut Streams,
    decoded: &ff::frame::Video,
    pts: Duration,
) -> Result<Frame, String> {
    let (format, w, h) = (decoded.format(), decoded.width(), decoded.height());
    // libswscale aborts on invalid input: checked before it sees a frame.
    if !valid_frame(format, w, h) {
        return Err("видео: повреждённый кадр".into());
    }
    if !matches!(&streams.scaler, Some((f, sw, sh, _)) if (*f, *sw, *sh) == (format, w, h)) {
        let (out_w, out_h) = fit_even(w, h);
        let scaler = ff::software::scaling::Context::get(
            format,
            w,
            h,
            ff::format::Pixel::YUV420P,
            out_w,
            out_h,
            ff::software::scaling::Flags::BILINEAR,
        )
        .map_err(|e| format!("видео: масштабирование: {e}"))?;
        streams.scaler = Some((format, w, h, scaler));
    }
    let Some((_, _, _, scaler)) = &mut streams.scaler else {
        return Err("видео: нет масштабировщика".into());
    };
    let mut out = ff::frame::Video::empty();
    scaler
        .run(decoded, &mut out)
        .map_err(|e| format!("видео: {e}"))?;
    let plane = |i: usize| -> Vec<u8> {
        let (pw, ph) = (out.plane_width(i) as usize, out.plane_height(i) as usize);
        let stride = out.stride(i);
        let data = out.data(i);
        let mut packed = Vec::with_capacity(pw * ph);
        for row in 0..ph {
            packed.extend_from_slice(&data[row * stride..row * stride + pw]);
        }
        packed
    };
    Ok(Frame {
        width: out.width(),
        height: out.height(),
        y: plane(0),
        u: plane(1),
        v: plane(2),
        pts,
        serial: next_serial(),
    })
}

fn drain_audio(streams: &mut Streams, shared: &Shared, skip_until: Duration) {
    let Some((decoder, _, tb)) = &mut streams.audio else {
        return;
    };
    let Some((rate, channels)) = shared.sound.lock().as_ref().map(|s| (s.rate, s.channels)) else {
        return;
    };
    let mut decoded = ff::frame::Audio::empty();
    while decoder.receive_frame(&mut decoded).is_ok() {
        let pts = decoded
            .pts()
            .map(|p| pts_to_duration(p, *tb))
            .unwrap_or_default();
        if pts < skip_until {
            continue;
        }
        if streams.audio_base_pending {
            // The position is `clock.base + played/rate` (see
            // `Shared::position`): anchoring it here instead of leaving it
            // at zero/the seek target keeps audio in sync with the
            // picture for a stream whose audio starts with an offset or
            // has a nonzero `start_time`, and removes the up-to-one-frame
            // drift a seek would otherwise leave (see the seek handling
            // in `decode`).
            streams.audio_base_pending = false;
            shared.clock.lock().base = pts;
        }
        let layout = if decoded.channel_layout().is_empty() {
            ff::ChannelLayout::default(i32::from(decoded.channels()))
        } else {
            decoded.channel_layout()
        };
        // Rebuild the resampler whenever the stream's actual format, rate
        // or channel layout no longer match what it was created for (some
        // files change these mid-stream): reusing a stale one just makes
        // every later frame fail to convert and get skipped forever below.
        let stale = streams.resampler.as_ref().is_some_and(|r| {
            let input = r.input();
            input.format != decoded.format()
                || input.channel_layout != layout
                || input.rate != decoded.rate()
        });
        if streams.resampler.is_none() || stale {
            streams.resampler = ff::software::resampling::Context::get(
                decoded.format(),
                layout,
                decoded.rate(),
                ff::format::Sample::F32(ff::format::sample::Type::Packed),
                ff::ChannelLayout::default(i32::from(channels)),
                rate,
            )
            .ok();
        }
        let Some(resampler) = &mut streams.resampler else {
            // Can't resample this stream's audio: fall back to the wall
            // clock instead of leaving `Sound` stuck with an empty queue
            // that never advances the position.
            use_wall_clock(shared);
            return;
        };
        let Ok(out) = resample(resampler, &decoded) else {
            continue;
        };
        let count = out.samples() * usize::from(channels);
        let bytes = &out.data(0)[..count * 4];
        let mut sound = shared.sound.lock();
        if let Some(sound) = sound.as_mut() {
            sound.samples.extend(
                bytes
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|b| f32::from_ne_bytes(*b)),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clip(name: &str, frames: usize) -> std::path::PathBuf {
        let path =
            std::env::temp_dir().join(format!("telega-player-{}-{name}.mkv", std::process::id()));
        super::super::video::tests::encode_test_clip(&path, 64, 48, frames);
        path
    }

    fn wait(what: impl Fn() -> bool) {
        let start = Instant::now();
        while !what() {
            assert!(start.elapsed() < Duration::from_secs(10), "timed out");
            thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn plays_to_the_end_in_yuv_on_the_clock() {
        let path = clip("play", 25);
        let player = Player::open(std::fs::File::open(&path).unwrap(), None, false);
        wait(|| player.frame().is_some());
        let first = player.frame().unwrap();
        assert_eq!((first.width, first.height), (64, 48));
        assert_eq!(first.y.len(), 64 * 48);
        assert_eq!(first.u.len(), 32 * 24);
        let duration = player.duration().unwrap();
        assert!((Duration::from_millis(900)..=Duration::from_secs(1)).contains(&duration));
        // Paused: the clock stands, the first frame stays.
        thread::sleep(Duration::from_millis(100));
        assert_eq!(player.position(), Duration::ZERO);
        assert!(Arc::ptr_eq(&player.frame().unwrap(), &first));

        player.set_paused(false);
        thread::sleep(Duration::from_millis(300));
        let shown = player.frame().unwrap();
        assert!(shown.pts >= Duration::from_millis(200), "{:?}", shown.pts);
        assert!(shown.pts <= player.position());
        wait(|| player.ended());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn seeking_shows_the_frame_at_the_new_time() {
        let path = clip("seek", 50);
        let player = Player::open(std::fs::File::open(&path).unwrap(), None, false);
        wait(|| player.frame().is_some());
        player.seek(Duration::from_millis(1400));
        wait(|| {
            player
                .frame()
                .is_some_and(|f| f.pts >= Duration::from_millis(1300))
        });
        assert_eq!(player.position(), Duration::from_millis(1400));
        // Back to the start works too.
        player.seek(Duration::ZERO);
        wait(|| {
            player
                .frame()
                .is_some_and(|f| f.pts < Duration::from_millis(100))
        });
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn decoding_stays_a_few_frames_ahead() {
        let path = clip("ahead", 100);
        let player = Player::open(std::fs::File::open(&path).unwrap(), None, false);
        wait(|| player.frame().is_some());
        thread::sleep(Duration::from_millis(200));
        let queued = player.shared.frames.lock().len();
        assert!(queued <= AHEAD_FRAMES * 3, "{queued} frames decoded ahead");
        drop(player);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn garbage_is_an_error_not_a_crash() {
        let player = Player::open(
            std::io::Cursor::new(vec![0x1a, 0x45, 0xdf, 0xa3, 1, 2, 3]),
            None,
            false,
        );
        wait(|| player.error().is_some());
    }
}

#[cfg(test)]
mod security_diagnostics {
    use super::*;

    #[test]
    fn security_fix_pausing_preserves_audio_samples_and_resumes_in_order_at_the_selected_volume() {
        let source = [0.125, 0.25, 0.375, 0.5, 0.625, 0.75];
        let shared = Shared {
            frames: Mutex::new(VecDeque::new()),
            sound: Mutex::new(Some(Sound {
                samples: source.into_iter().collect(),
                played: 0,
                rate: 48_000,
                channels: 2,
                stalled: None,
            })),
            clock: Mutex::new(Clock {
                base: Duration::ZERO,
                since: None,
            }),
            info: Mutex::new(Info::default()),
            seek: Mutex::new(None),
            paused: AtomicBool::new(true),
            volume: AtomicU32::new(1.0_f32.to_bits()),
            finished: AtomicBool::new(false),
            stop: AtomicBool::new(false),
        };

        for _ in 0..2 {
            let mut paused_output = [f32::NAN; 2];
            fill_sound(&shared, &mut paused_output);
            assert_eq!(paused_output, [0.0; 2]);
            let guard = shared.sound.lock();
            let sound = guard.as_ref().unwrap();
            assert_eq!(sound.samples.iter().copied().collect::<Vec<_>>(), source);
            assert_eq!(sound.played, 0);
            assert!(sound.stalled.is_none());
        }
        assert_eq!(shared.position(), Duration::ZERO);

        shared.paused.store(false, Ordering::Relaxed);
        let mut resumed_output = [f32::NAN; 4];
        fill_sound(&shared, &mut resumed_output);
        assert_eq!(resumed_output, source[..4]);
        {
            let guard = shared.sound.lock();
            let sound = guard.as_ref().unwrap();
            assert_eq!(
                sound.samples.iter().copied().collect::<Vec<_>>(),
                source[4..]
            );
            assert_eq!(sound.played, 2);
        }

        shared.volume.store(0.5_f32.to_bits(), Ordering::Relaxed);
        let mut quieter_output = [f32::NAN; 2];
        fill_sound(&shared, &mut quieter_output);
        assert_eq!(quieter_output, [source[4] * 0.5, source[5] * 0.5]);
        let guard = shared.sound.lock();
        let sound = guard.as_ref().unwrap();
        assert!(sound.samples.is_empty());
        assert_eq!(sound.played, 3);
    }
}
