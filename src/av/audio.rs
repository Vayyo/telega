//! Audio decoding/encoding (ffmpeg) and playback/recording (cpal).

use parking_lot::Mutex;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Once, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use cpal::Sample;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use ffmpeg_next as ff;

use super::video::whitelist_options;

static FFMPEG_INIT: Once = Once::new();

fn init_ffmpeg() {
    FFMPEG_INIT.call_once(|| {
        let _ = ff::init();
    });
}

/// Interleaved `f32` PCM audio.
pub struct Pcm {
    pub rate: u32,
    pub channels: u16,
    pub samples: Vec<f32>,
}

impl Pcm {
    pub fn duration(&self) -> Duration {
        if self.rate == 0 || self.channels == 0 {
            return Duration::ZERO;
        }
        let frames = self.samples.len() / self.channels as usize;
        Duration::from_secs_f64(frames as f64 / f64::from(self.rate))
    }
}

/// Decode any audio file ffmpeg supports (Telegram voice = OGG/Opus) to f32 interleaved
/// PCM, resampled to `rate`/`channels`.
/// Voice messages come as OGG with Opus (older clients: Vorbis). Other
/// demuxers and decoders ffmpeg would probe on a stranger's file are refused.
const CONTAINERS: &[&str] = &["ogg"];
const CODECS: &[ff::codec::Id] = &[ff::codec::Id::OPUS, ff::codec::Id::VORBIS];
/// Longest audio decoded into memory; the rest of a longer file is cut off
/// (at 48 kHz mono f32 this is ~115 MB).
pub const MAX_DECODE: std::time::Duration = std::time::Duration::from_secs(10 * 60);

pub fn decode_to_pcm(path: &Path, rate: u32, channels: u16) -> Result<Pcm, String> {
    init_ffmpeg();
    let mut ictx = ff::format::input_with_dictionary(path, whitelist_options(CONTAINERS, CODECS))
        .map_err(|e| format!("звук: не удалось открыть файл: {e}"))?;
    if !CONTAINERS.contains(&ictx.format().name()) {
        return Err(format!(
            "звук: неподдерживаемый формат {}",
            ictx.format().name()
        ));
    }
    let stream = ictx
        .streams()
        .best(ff::media::Type::Audio)
        .ok_or_else(|| "звук: в файле нет аудиодорожки".to_string())?;
    let stream_index = stream.index();
    if !CODECS.contains(&stream.parameters().id()) {
        return Err(format!(
            "звук: неподдерживаемый кодек {:?}",
            stream.parameters().id()
        ));
    }
    let ctx = ff::codec::context::Context::from_parameters(stream.parameters())
        .map_err(|e| format!("звук: не удалось создать декодер: {e}"))?;
    let mut decoder = ctx
        .decoder()
        .audio()
        .map_err(|e| format!("звук: не удалось открыть декодер: {e}"))?;
    // libswresample may abort the process on invalid parameters.
    if decoder.format() == ff::format::Sample::None
        || !(1..=384_000).contains(&decoder.rate())
        || !(1..=8).contains(&decoder.channels())
        || rate == 0
        || channels == 0
    {
        return Err("звук: повреждённый файл".to_string());
    }
    // Bounded in samples, not seconds: a 192 kHz or 8-channel output device
    // must not multiply the memory (10 min of 48 kHz mono, ~115 MB).
    let limit = 48_000 * MAX_DECODE.as_secs() as usize;

    let dst_format = ff::format::Sample::F32(ff::format::sample::Type::Packed);
    let dst_layout = ff::ChannelLayout::default(i32::from(channels).max(1));
    let mut resampler = ff::software::resampling::Context::get(
        decoder.format(),
        decoder.channel_layout(),
        decoder.rate(),
        dst_format,
        dst_layout,
        rate,
    )
    .map_err(|e| format!("звук: не удалось создать ресемплер: {e}"))?;

    let mut samples: Vec<f32> = Vec::new();
    let mut decoded = ff::frame::Audio::empty();

    for (stream, packet) in ictx.packets() {
        if samples.len() >= limit {
            break;
        }
        if stream.index() != stream_index {
            continue;
        }
        // A packet libopus/libvorbis rejects must not abort the whole
        // message: the video player skips broken audio packets the same way.
        if decoder.send_packet(&packet).is_err() {
            continue;
        }
        drain_decoded(&mut decoder, &mut resampler, &mut decoded, &mut samples)?;
    }
    decoder
        .send_eof()
        .map_err(|e| format!("звук: ошибка декодирования: {e}"))?;
    drain_decoded(&mut decoder, &mut resampler, &mut decoded, &mut samples)?;

    // Drain whatever remains queued inside the resampler's internal FIFO.
    // With `resample` sizing every call correctly this is only the last
    // few samples of filter delay. Stop the moment a call yields nothing:
    // `swr_get_delay` can keep reporting the same stale nonzero delay
    // forever once the FIFO is actually empty, so looping until it turns
    // `None` (as opposed to until output stops) can hang forever.
    loop {
        let mut tail = ff::frame::Audio::new(dst_format, 8192, dst_layout);
        if resampler.flush(&mut tail).is_err() || tail.samples() == 0 {
            break;
        }
        samples.extend_from_slice(tail.plane::<f32>(0));
    }

    samples.truncate(limit);
    samples.shrink_to_fit();
    Ok(Pcm {
        rate,
        channels,
        samples,
    })
}

/// Runs `resampler` on `input`, sizing the output to fit everything
/// libswresample can produce right now instead of leaving the surplus
/// queued in its internal FIFO.
///
/// `ff::software::resampling::Context::run` allocates its own output frame
/// from `input.samples()` when given an empty frame (ffmpeg-next
/// `software/resampling/context.rs`), not from the destination rate.
/// Upsampling (a 44.1 kHz track resampled for a 48 kHz device, a 16 kHz
/// microphone resampled to 48 kHz for Opus, ...) then always needs more
/// room than the input has, and the shortfall stays queued inside swr
/// instead of coming out — every later frame is short the same fraction,
/// and a long enough input never fully drains through a flush loop with a
/// fixed iteration cap.
///
/// Matches libswresample's own sizing for an unallocated output frame
/// (`libswresample/swresample_frame.c`): `delay(out_rate) + 3 +
/// in_samples * out_rate / in_rate`.
pub(super) fn resample(
    resampler: &mut ff::software::resampling::Context,
    input: &ff::frame::Audio,
) -> Result<ff::frame::Audio, ff::Error> {
    let delay = resampler.delay().map_or(0i64, |d| d.output);
    let in_rate = i64::from(resampler.input().rate.max(1));
    let out_rate = i64::from(resampler.output().rate);
    let samples = delay + 3 + (input.samples() as i64 * out_rate) / in_rate;
    let mut output = ff::frame::Audio::new(
        resampler.output().format,
        samples.max(0) as usize,
        resampler.output().channel_layout,
    );
    resampler.run(input, &mut output)?;
    Ok(output)
}

fn drain_decoded(
    decoder: &mut ff::decoder::Audio,
    resampler: &mut ff::software::resampling::Context,
    decoded: &mut ff::frame::Audio,
    samples: &mut Vec<f32>,
) -> Result<(), String> {
    while decoder.receive_frame(decoded).is_ok() {
        let resampled = resample(resampler, decoded)
            .map_err(|e| format!("звук: ошибка ресемплирования: {e}"))?;
        if resampled.samples() > 0 {
            samples.extend_from_slice(resampled.plane::<f32>(0));
        }
    }
    Ok(())
}

/// Unpack Telegram voice-note waveform (5-bit packed values, already base64-decoded bytes)
/// into values 0..=31.
pub fn unpack_waveform(data: &[u8]) -> Vec<u8> {
    let value_count = data.len() * 8 / 5;
    let mut out = Vec::with_capacity(value_count);
    for i in 0..value_count {
        let bit_offset = i * 5;
        let byte_offset = bit_offset / 8;
        let bit_shift = bit_offset % 8;
        let mut value = u32::from(data[byte_offset]) >> bit_shift;
        if bit_shift > 3
            && let Some(&next) = data.get(byte_offset + 1)
        {
            value |= u32::from(next) << (8 - bit_shift);
        }
        out.push((value & 0x1f) as u8);
    }
    out
}

/// Pack values 0..=31 into Telegram's 5-bit format.
pub fn pack_waveform(values: &[u8]) -> Vec<u8> {
    let byte_count = (values.len() * 5).div_ceil(8);
    let mut out = vec![0u8; byte_count];
    for (i, &v) in values.iter().enumerate() {
        let value = u32::from(v & 0x1f);
        let bit_offset = i * 5;
        let byte_offset = bit_offset / 8;
        let bit_shift = bit_offset % 8;
        out[byte_offset] |= (value << bit_shift) as u8;
        if bit_shift > 3
            && let Some(slot) = out.get_mut(byte_offset + 1)
        {
            *slot |= (value >> (8 - bit_shift)) as u8;
        }
    }
    out
}

/// Encode mono f32 PCM at `rate` into an OGG/Opus file (libopus via ffmpeg, 48 kHz;
/// resamples automatically since Opus only supports a fixed set of sample rates).
pub fn encode_opus_ogg(samples: &[f32], rate: u32, path: &Path) -> Result<(), String> {
    init_ffmpeg();
    const TARGET_RATE: u32 = 48_000;

    let codec = ff::encoder::find_by_name("libopus")
        .or_else(|| ff::encoder::find(ff::codec::Id::OPUS))
        .ok_or_else(|| "звук: кодировщик Opus недоступен".to_string())?;

    let mut octx =
        ff::format::output(path).map_err(|e| format!("звук: не удалось создать файл: {e}"))?;
    let global_header = octx
        .format()
        .flags()
        .contains(ff::format::Flags::GLOBAL_HEADER);

    let mut ost = octx
        .add_stream(codec)
        .map_err(|e| format!("звук: не удалось добавить дорожку: {e}"))?;
    let ost_index = ost.index();

    let sample_fmt = codec
        .audio()
        .ok()
        .and_then(|a| a.formats())
        .and_then(|mut it| it.find(|f| matches!(f, ff::format::Sample::F32(_))))
        .unwrap_or(ff::format::Sample::F32(ff::format::sample::Type::Packed));
    let layout = ff::ChannelLayout::MONO;

    let encoder_ctx = ff::codec::context::Context::new_with_codec(codec);
    let mut encoder = encoder_ctx
        .encoder()
        .audio()
        .map_err(|e| format!("звук: не удалось создать кодировщик: {e}"))?;
    encoder.set_rate(TARGET_RATE as i32);
    encoder.set_channel_layout(layout);
    encoder.set_format(sample_fmt);
    if global_header {
        encoder.set_flags(ff::codec::Flags::GLOBAL_HEADER);
    }
    encoder.set_time_base((1, TARGET_RATE as i32));

    let mut encoder = encoder
        .open_as(codec)
        .map_err(|e| format!("звук: не удалось открыть кодировщик: {e}"))?;
    ost.set_time_base((1, TARGET_RATE as i32));
    ost.set_parameters(&encoder);

    octx.write_header()
        .map_err(|e| format!("звук: не удалось записать заголовок: {e}"))?;
    let out_time_base = octx
        .stream(ost_index)
        .ok_or_else(|| "звук: не удалось получить дорожку".to_string())?
        .time_base();

    let mut resampler = ff::software::resampling::Context::get(
        ff::format::Sample::F32(ff::format::sample::Type::Packed),
        ff::ChannelLayout::MONO,
        rate,
        sample_fmt,
        layout,
        TARGET_RATE,
    )
    .map_err(|e| format!("звук: не удалось создать ресемплер: {e}"))?;

    let frame_size = encoder.frame_size().max(1) as usize;
    // Resampled samples not yet a full Opus frame; never holds more than
    // one frame's worth between chunks.
    let mut pending: Vec<f32> = Vec::with_capacity(frame_size);
    let mut pts: i64 = 0;

    // Feeds the recording to the resampler, and from there to the
    // encoder, in chunks sized off the encoder's own frame size instead
    // of resampling and converting it whole: at the allowed 10-minute,
    // 48 kHz maximum that used to peak at about 460 MB (`source` +
    // `resampled` + `converted`, each the size of the whole recording,
    // plus a `Vec` per Opus frame) — now it is a handful of frames'
    // worth regardless of how long the recording is.
    let input_chunk =
        ((frame_size as u64 * u64::from(rate)).div_ceil(u64::from(TARGET_RATE)) as usize).max(1);
    let mut offset = 0usize;
    loop {
        let end = (offset + input_chunk).min(samples.len());
        let block = &samples[offset..end];
        let mut source = ff::frame::Audio::new(
            ff::format::Sample::F32(ff::format::sample::Type::Packed),
            block.len(),
            ff::ChannelLayout::MONO,
        );
        source.set_rate(rate);
        if !block.is_empty() {
            source.plane_mut::<f32>(0).copy_from_slice(block);
        }
        let resampled = resample(&mut resampler, &source)
            .map_err(|e| format!("звук: ошибка ресемплирования: {e}"))?;
        if resampled.samples() > 0 {
            feed_encoder(
                &mut pending,
                resampled.plane::<f32>(0),
                frame_size,
                sample_fmt,
                layout,
                TARGET_RATE,
                &mut pts,
                &mut encoder,
                &mut octx,
                ost_index,
                out_time_base,
            )?;
        }
        if end == samples.len() {
            break;
        }
        offset = end;
    }

    // Drain whatever remains queued inside the resampler's internal FIFO.
    // With `resample` sizing every call correctly this is only the last
    // few samples of filter delay. Stop the moment a call yields nothing:
    // `swr_get_delay` can keep reporting the same stale nonzero delay
    // forever once the FIFO is actually empty, so looping until it turns
    // `None` can hang forever instead of just truncating the tail.
    loop {
        let mut tail = ff::frame::Audio::new(sample_fmt, 8192, layout);
        if resampler.flush(&mut tail).is_err() || tail.samples() == 0 {
            break;
        }
        feed_encoder(
            &mut pending,
            tail.plane::<f32>(0),
            frame_size,
            sample_fmt,
            layout,
            TARGET_RATE,
            &mut pts,
            &mut encoder,
            &mut octx,
            ost_index,
            out_time_base,
        )?;
    }

    // Whatever is left is shorter than one Opus frame: zero-pad it so the
    // tail of the recording is not silently dropped.
    if !pending.is_empty() {
        pending.resize(frame_size, 0.0);
        let mut in_frame = ff::frame::Audio::new(sample_fmt, frame_size, layout);
        in_frame.set_rate(TARGET_RATE);
        in_frame.plane_mut::<f32>(0).copy_from_slice(&pending);
        in_frame.set_pts(Some(pts));
        encoder
            .send_frame(&in_frame)
            .map_err(|e| format!("звук: ошибка кодирования: {e}"))?;
        write_encoded(&mut encoder, &mut octx, ost_index, out_time_base)?;
    }

    encoder
        .send_eof()
        .map_err(|e| format!("звук: ошибка кодирования: {e}"))?;
    write_encoded(&mut encoder, &mut octx, ost_index, out_time_base)?;

    octx.write_trailer()
        .map_err(|e| format!("звук: не удалось завершить запись: {e}"))?;
    Ok(())
}

/// Appends newly resampled samples to `pending` and encodes every full
/// `frame_size` chunk it now holds, leaving fewer than `frame_size`
/// queued. Keeps `encode_opus_ogg`'s peak memory at a handful of Opus
/// frames instead of the whole recording.
#[allow(clippy::too_many_arguments)]
fn feed_encoder(
    pending: &mut Vec<f32>,
    new_samples: &[f32],
    frame_size: usize,
    sample_fmt: ff::format::Sample,
    layout: ff::ChannelLayout,
    rate: u32,
    pts: &mut i64,
    encoder: &mut ff::encoder::Audio,
    octx: &mut ff::format::context::Output,
    stream_index: usize,
    out_time_base: ff::Rational,
) -> Result<(), String> {
    pending.extend_from_slice(new_samples);
    let mut start = 0;
    while pending.len() - start >= frame_size {
        let mut in_frame = ff::frame::Audio::new(sample_fmt, frame_size, layout);
        in_frame.set_rate(rate);
        in_frame
            .plane_mut::<f32>(0)
            .copy_from_slice(&pending[start..start + frame_size]);
        in_frame.set_pts(Some(*pts));
        *pts += frame_size as i64;
        encoder
            .send_frame(&in_frame)
            .map_err(|e| format!("звук: ошибка кодирования: {e}"))?;
        write_encoded(encoder, octx, stream_index, out_time_base)?;
        start += frame_size;
    }
    pending.drain(..start);
    Ok(())
}

fn write_encoded(
    encoder: &mut ff::encoder::Audio,
    octx: &mut ff::format::context::Output,
    stream_index: usize,
    out_time_base: ff::Rational,
) -> Result<(), String> {
    let in_time_base = encoder.time_base();
    let mut packet = ff::Packet::empty();
    while encoder.receive_packet(&mut packet).is_ok() {
        packet.set_stream(stream_index);
        packet.rescale_ts(in_time_base, out_time_base);
        packet
            .write_interleaved(octx)
            .map_err(|e| format!("звук: ошибка записи: {e}"))?;
    }
    Ok(())
}

struct PlaybackState {
    id: i64,
    pcm: Arc<Pcm>,
    cursor: usize,
    playing: bool,
}

/// Clone+Send handle to a playback thread owning the cpal output stream.
#[derive(Clone)]
pub struct Player {
    state: Arc<Mutex<Option<PlaybackState>>>,
    rate: u32,
    channels: u16,
    _keepalive: mpsc::Sender<()>,
}

impl Player {
    /// Opens the default output device eagerly on a dedicated thread.
    pub fn new() -> Result<Player, String> {
        let state: Arc<Mutex<Option<PlaybackState>>> = Arc::new(Mutex::new(None));
        let (done_tx, done_rx) = mpsc::channel::<()>();
        let (ready_tx, ready_rx) = mpsc::sync_channel::<Result<(u32, u16), String>>(0);
        let thread_state = Arc::clone(&state);
        thread::Builder::new()
            .name("av-player".into())
            .spawn(move || run_player_thread(thread_state, done_rx, ready_tx))
            .map_err(|e| format!("звук: не удалось создать поток воспроизведения: {e}"))?;
        let setup = ready_rx
            .recv()
            .map_err(|_| "звук: поток воспроизведения не запустился".to_string())?;
        let (rate, channels) = setup?;
        Ok(Player {
            state,
            rate,
            channels,
            _keepalive: done_tx,
        })
    }

    /// What `decode_to_pcm` should target so playback needs no further resampling.
    pub fn output_format(&self) -> (u32, u16) {
        (self.rate, self.channels)
    }

    pub fn play(&self, id: i64, pcm: Arc<Pcm>, from: Duration) {
        let rate = pcm.rate.max(1);
        let cursor = (from.as_secs_f64() * f64::from(rate)).round().max(0.0) as usize;
        let mut guard = self.state.lock();
        *guard = Some(PlaybackState {
            id,
            pcm,
            cursor,
            playing: true,
        });
    }

    pub fn pause(&self) {
        let mut guard = self.state.lock();
        if let Some(st) = guard.as_mut() {
            st.playing = false;
        }
    }

    pub fn stop(&self) {
        let mut guard = self.state.lock();
        *guard = None;
    }

    /// (id of current track, position, playing?); `None` when nothing loaded.
    pub fn state(&self) -> Option<(i64, Duration, bool)> {
        let guard = self.state.lock();
        guard.as_ref().map(|st| {
            let rate = st.pcm.rate.max(1);
            (
                st.id,
                Duration::from_secs_f64(st.cursor as f64 / f64::from(rate)),
                st.playing,
            )
        })
    }
}

fn run_player_thread(
    state: Arc<Mutex<Option<PlaybackState>>>,
    done_rx: mpsc::Receiver<()>,
    ready_tx: mpsc::SyncSender<Result<(u32, u16), String>>,
) {
    let host = cpal::default_host();
    let device = match host.default_output_device() {
        Some(d) => d,
        None => {
            let _ = ready_tx.send(Err("звук: нет устройства воспроизведения".to_string()));
            return;
        }
    };
    let config = match device.default_output_config() {
        Ok(c) => c,
        Err(e) => {
            let _ = ready_tx.send(Err(format!("звук: {e}")));
            return;
        }
    };
    let stream_config: cpal::StreamConfig = config.config();
    let rate = stream_config.sample_rate;
    let channels = stream_config.channels;
    let mut sent_ready = false;
    // A stream that errors out (device unplugged, format renegotiation,
    // ...) otherwise leaves `playing` stuck true with a frozen position
    // forever (the data callback simply stops running): rebuild it on the
    // same device instead of giving up after the first failure.
    loop {
        let failed = Arc::new(AtomicBool::new(false));
        let stream = match build_output_stream(
            &device,
            &stream_config,
            config.sample_format(),
            Arc::clone(&state),
            Arc::clone(&failed),
        ) {
            Ok(s) => s,
            Err(e) => {
                if !sent_ready {
                    let _ = ready_tx.send(Err(e));
                }
                return;
            }
        };
        if let Err(e) = stream.play() {
            if !sent_ready {
                let _ = ready_tx.send(Err(format!(
                    "звук: не удалось запустить воспроизведение: {e}"
                )));
            }
            return;
        }
        if !sent_ready {
            if ready_tx.send(Ok((rate, channels))).is_err() {
                return;
            }
            sent_ready = true;
        }
        loop {
            match done_rx.recv_timeout(Duration::from_millis(200)) {
                Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => return,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if failed.load(Ordering::Relaxed) {
                        break;
                    }
                }
            }
        }
        drop(stream);
    }
}

fn build_output_stream(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    format: cpal::SampleFormat,
    state: Arc<Mutex<Option<PlaybackState>>>,
    failed: Arc<AtomicBool>,
) -> Result<cpal::Stream, String> {
    let channels = config.channels as usize;
    let err_fn = move |_: cpal::Error| failed.store(true, Ordering::Relaxed);
    macro_rules! build {
        ($t:ty) => {
            device.build_output_stream(
                *config,
                move |data: &mut [$t], _: &cpal::OutputCallbackInfo| {
                    fill_output::<$t>(data, channels, &state);
                },
                err_fn,
                None,
            )
        };
    }
    let stream = match format {
        cpal::SampleFormat::F32 => build!(f32),
        cpal::SampleFormat::I8 => build!(i8),
        cpal::SampleFormat::I16 => build!(i16),
        cpal::SampleFormat::I32 => build!(i32),
        cpal::SampleFormat::I64 => build!(i64),
        cpal::SampleFormat::U8 => build!(u8),
        cpal::SampleFormat::U16 => build!(u16),
        cpal::SampleFormat::U32 => build!(u32),
        cpal::SampleFormat::U64 => build!(u64),
        cpal::SampleFormat::F64 => build!(f64),
        other => {
            return Err(format!(
                "звук: неподдерживаемый формат устройства: {other:?}"
            ));
        }
    };
    stream.map_err(|e| format!("звук: не удалось создать поток вывода: {e}"))
}

fn fill_output<T>(data: &mut [T], channels: usize, state: &Arc<Mutex<Option<PlaybackState>>>)
where
    T: cpal::Sample + cpal::FromSample<f32>,
{
    let mut guard = state.lock();
    let active = guard.as_mut().filter(|st| st.playing);
    let Some(st) = active else {
        data.fill(T::EQUILIBRIUM);
        return;
    };
    let src_channels = usize::from(st.pcm.channels).max(1);
    let total_frames = st.pcm.samples.len() / src_channels;
    let out_frames = data.len().checked_div(channels).unwrap_or(0);
    for frame in 0..out_frames {
        if st.cursor >= total_frames {
            for c in 0..channels {
                data[frame * channels + c] = T::EQUILIBRIUM;
            }
            continue;
        }
        for c in 0..channels {
            let src_c = c.min(src_channels - 1);
            let sample = st.pcm.samples[st.cursor * src_channels + src_c];
            data[frame * channels + c] = T::from_sample(sample);
        }
        st.cursor += 1;
    }
    if st.cursor >= total_frames {
        st.playing = false;
    }
}

/// Result of a finished recording.
pub struct Recording {
    pub path: PathBuf,
    pub duration: Duration,
    pub waveform: Vec<u8>,
}

/// Clone+Send handle; records from default input device on its own thread.
#[derive(Clone)]
pub struct Recorder {
    samples: Arc<Mutex<Vec<f32>>>,
    recording: Arc<AtomicBool>,
    rate: u32,
    start: Instant,
    _keepalive: mpsc::Sender<()>,
}

impl Recorder {
    pub fn start() -> Result<Recorder, String> {
        let samples: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::new()));
        let recording = Arc::new(AtomicBool::new(true));
        let (done_tx, done_rx) = mpsc::channel::<()>();
        let (ready_tx, ready_rx) = mpsc::sync_channel::<Result<u32, String>>(0);
        let thread_samples = Arc::clone(&samples);
        let thread_recording = Arc::clone(&recording);
        thread::Builder::new()
            .name("av-recorder".into())
            .spawn(move || run_recorder_thread(thread_samples, thread_recording, done_rx, ready_tx))
            .map_err(|e| format!("звук: не удалось создать поток записи: {e}"))?;
        let setup = ready_rx
            .recv()
            .map_err(|_| "звук: поток записи не запустился".to_string())?;
        let rate = setup?;
        Ok(Recorder {
            samples,
            recording,
            rate,
            start: Instant::now(),
            _keepalive: done_tx,
        })
    }

    pub fn elapsed(&self) -> Duration {
        Instant::now().saturating_duration_since(self.start)
    }

    /// Stop and encode to `path` (OGG/Opus), returning the waveform (100 values,
    /// peak-normalised) packed to Telegram's 5-bit format.
    pub fn finish(self, path: &Path) -> Result<Recording, String> {
        self.recording.store(false, Ordering::SeqCst);
        let samples = std::mem::take(&mut *self.samples.lock());
        let duration = Duration::from_secs_f64(samples.len() as f64 / f64::from(self.rate.max(1)));
        encode_opus_ogg(&samples, self.rate, path)?;
        let waveform = pack_waveform(&compute_waveform(&samples));
        Ok(Recording {
            path: path.to_path_buf(),
            duration,
            waveform,
        })
    }

    pub fn cancel(self) {
        self.recording.store(false, Ordering::SeqCst);
    }

    /// A recorder that touches no real audio device, for tests of the
    /// recording lifecycle (window closing, chat switching) that must not
    /// depend on a capture device being present.
    #[cfg(test)]
    pub(crate) fn stub() -> Recorder {
        let (keepalive, _rx) = mpsc::channel::<()>();
        Recorder {
            samples: Arc::new(Mutex::new(Vec::new())),
            recording: Arc::new(AtomicBool::new(true)),
            rate: 48_000,
            start: Instant::now(),
            _keepalive: keepalive,
        }
    }
}

fn run_recorder_thread(
    samples: Arc<Mutex<Vec<f32>>>,
    recording: Arc<AtomicBool>,
    done_rx: mpsc::Receiver<()>,
    ready_tx: mpsc::SyncSender<Result<u32, String>>,
) {
    let host = cpal::default_host();
    let device = match host.default_input_device() {
        Some(d) => d,
        None => {
            let _ = ready_tx.send(Err("звук: нет устройства записи".to_string()));
            return;
        }
    };
    let config = match device.default_input_config() {
        Ok(c) => c,
        Err(e) => {
            let _ = ready_tx.send(Err(format!("звук: {e}")));
            return;
        }
    };
    let stream_config: cpal::StreamConfig = config.config();
    let rate = stream_config.sample_rate;
    let channels = usize::from(stream_config.channels);
    let stream = match build_input_stream(
        &device,
        &stream_config,
        config.sample_format(),
        channels,
        Arc::clone(&samples),
        Arc::clone(&recording),
    ) {
        Ok(s) => s,
        Err(e) => {
            let _ = ready_tx.send(Err(e));
            return;
        }
    };
    if let Err(e) = stream.play() {
        let _ = ready_tx.send(Err(format!("звук: не удалось запустить запись: {e}")));
        return;
    }
    if ready_tx.send(Ok(rate)).is_err() {
        return;
    }
    let _ = done_rx.recv();
}

fn build_input_stream(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    format: cpal::SampleFormat,
    channels: usize,
    samples: Arc<Mutex<Vec<f32>>>,
    recording: Arc<AtomicBool>,
) -> Result<cpal::Stream, String> {
    let err_fn = |_: cpal::Error| {};
    macro_rules! build {
        ($t:ty) => {
            device.build_input_stream(
                *config,
                move |data: &[$t], _: &cpal::InputCallbackInfo| {
                    capture_input::<$t>(data, channels, &samples, &recording);
                },
                err_fn,
                None,
            )
        };
    }
    let stream = match format {
        cpal::SampleFormat::F32 => build!(f32),
        cpal::SampleFormat::I8 => build!(i8),
        cpal::SampleFormat::I16 => build!(i16),
        cpal::SampleFormat::I32 => build!(i32),
        cpal::SampleFormat::I64 => build!(i64),
        cpal::SampleFormat::U8 => build!(u8),
        cpal::SampleFormat::U16 => build!(u16),
        cpal::SampleFormat::U32 => build!(u32),
        cpal::SampleFormat::U64 => build!(u64),
        cpal::SampleFormat::F64 => build!(f64),
        other => {
            return Err(format!(
                "звук: неподдерживаемый формат микрофона: {other:?}"
            ));
        }
    };
    stream.map_err(|e| format!("звук: не удалось открыть микрофон: {e}"))
}

fn capture_input<T>(
    data: &[T],
    channels: usize,
    samples: &Arc<Mutex<Vec<f32>>>,
    recording: &Arc<AtomicBool>,
) where
    T: cpal::Sample,
    f32: cpal::FromSample<T>,
{
    if !recording.load(Ordering::Relaxed) {
        return;
    }
    let mut guard = samples.lock();
    if channels <= 1 {
        guard.extend(data.iter().map(|&s| f32::from_sample(s)));
    } else {
        guard.extend(data.chunks(channels).map(|frame| {
            let sum: f32 = frame.iter().map(|&s| f32::from_sample(s)).sum();
            sum / channels as f32
        }));
    }
}

fn compute_waveform(samples: &[f32]) -> Vec<u8> {
    const N: usize = 100;
    if samples.is_empty() {
        return vec![0; N];
    }
    let chunk = samples.len().div_ceil(N).max(1);
    let mut peaks = [0f32; N];
    for (i, peak) in peaks.iter_mut().enumerate() {
        let start = i * chunk;
        if start >= samples.len() {
            break;
        }
        let end = (start + chunk).min(samples.len());
        *peak = samples[start..end].iter().fold(0f32, |m, s| m.max(s.abs()));
    }
    let max_peak = peaks.iter().copied().fold(0f32, f32::max).max(1e-6);
    peaks
        .iter()
        .map(|&p| ((p / max_peak) * 31.0).round().clamp(0.0, 31.0) as u8)
        .collect()
}

/// Opens the default output device for a caller that fills f32 samples
/// (interleaved, the device's rate and channels), whatever format the
/// device takes. Returns the running stream and (rate, channels).
pub(crate) fn open_output<F>(mut fill: F) -> Result<(cpal::Stream, u32, u16), String>
where
    F: FnMut(&mut [f32]) + Send + 'static,
{
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    let device = cpal::default_host()
        .default_output_device()
        .ok_or("звук: нет устройства воспроизведения")?;
    let config = device
        .default_output_config()
        .map_err(|e| format!("звук: {e}"))?;
    let stream_config: cpal::StreamConfig = config.config();
    let (rate, channels) = (stream_config.sample_rate, stream_config.channels);
    let mut scratch: Vec<f32> = Vec::new();
    macro_rules! build {
        ($t:ty) => {
            device.build_output_stream(
                stream_config,
                move |data: &mut [$t], _: &cpal::OutputCallbackInfo| {
                    scratch.resize(data.len(), 0.0);
                    fill(&mut scratch);
                    for (out, s) in data.iter_mut().zip(&scratch) {
                        *out = <$t as cpal::FromSample<f32>>::from_sample_(*s);
                    }
                },
                |_: cpal::Error| {},
                None,
            )
        };
    }
    // Same format list as `build_output_stream` (voice-message playback)
    // and `build_input_stream` (recording): an output device usable for
    // one must be usable for the other, or a device that plays voice
    // messages fine could still fail to open here for video/GIF sound
    // just because its native format is a less common integer width.
    let stream = match config.sample_format() {
        cpal::SampleFormat::F32 => build!(f32),
        cpal::SampleFormat::I8 => build!(i8),
        cpal::SampleFormat::I16 => build!(i16),
        cpal::SampleFormat::I32 => build!(i32),
        cpal::SampleFormat::I64 => build!(i64),
        cpal::SampleFormat::U8 => build!(u8),
        cpal::SampleFormat::U16 => build!(u16),
        cpal::SampleFormat::U32 => build!(u32),
        cpal::SampleFormat::U64 => build!(u64),
        cpal::SampleFormat::F64 => build!(f64),
        other => {
            return Err(format!(
                "звук: неподдерживаемый формат устройства: {other:?}"
            ));
        }
    }
    .map_err(|e| format!("звук: {e}"))?;
    stream.play().map_err(|e| format!("звук: {e}"))?;
    Ok((stream, rate, channels))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn waveform_roundtrip() {
        let values: Vec<u8> = (0..100).map(|i| (i % 32) as u8).collect();
        let packed = pack_waveform(&values);
        let unpacked = unpack_waveform(&packed);
        assert_eq!(&unpacked[..100], &values[..]);
    }

    #[test]
    fn waveform_max_value_roundtrip() {
        let values = vec![31u8; 100];
        let packed = pack_waveform(&values);
        let unpacked = unpack_waveform(&packed);
        assert_eq!(&unpacked[..100], &values[..]);
    }

    #[test]
    fn opus_roundtrip_preserves_duration_and_signal() {
        let rate = 16_000u32;
        let seconds = 1.0f64;
        let n = (f64::from(rate) * seconds) as usize;
        let samples: Vec<f32> = (0..n)
            .map(|i| {
                (2.0 * std::f64::consts::PI * 440.0 * i as f64 / f64::from(rate)).sin() as f32 * 0.5
            })
            .collect();

        let path =
            std::env::temp_dir().join(format!("telega-av-audio-test-{}.ogg", std::process::id()));
        encode_opus_ogg(&samples, rate, &path).expect("encode");

        let pcm = decode_to_pcm(&path, 48_000, 2).expect("decode");
        let _ = std::fs::remove_file(&path);

        let duration = pcm.duration();
        let target = Duration::from_secs_f64(seconds);
        let diff = duration.abs_diff(target);
        assert!(
            diff <= Duration::from_millis(60),
            "duration off by {diff:?}"
        );

        let rms =
            (pcm.samples.iter().map(|&s| s * s).sum::<f32>() / pcm.samples.len() as f32).sqrt();
        assert!(rms > 0.01, "rms too low: {rms}");
    }

    #[test]
    fn opus_roundtrip_streams_a_long_recording() {
        // 30 s at 16 kHz (upsampled to the encoder's fixed 48 kHz — the
        // worst case for `resample`'s sizing, see its doc comment) is
        // long enough that copying it whole (as `encode_opus_ogg` used
        // to) would show up as a multi-hundred-MB spike; chunking must
        // not lose or misplace any of it.
        let rate = 16_000u32;
        let seconds = 30.0f64;
        let n = (f64::from(rate) * seconds) as usize;
        let samples: Vec<f32> = (0..n)
            .map(|i| {
                (2.0 * std::f64::consts::PI * 440.0 * i as f64 / f64::from(rate)).sin() as f32 * 0.5
            })
            .collect();

        let path = std::env::temp_dir().join(format!(
            "telega-av-audio-test-long-{}.ogg",
            std::process::id()
        ));
        encode_opus_ogg(&samples, rate, &path).expect("encode");

        let pcm = decode_to_pcm(&path, 48_000, 1).expect("decode");
        let _ = std::fs::remove_file(&path);

        let duration = pcm.duration();
        let target = Duration::from_secs_f64(seconds);
        let diff = duration.abs_diff(target);
        assert!(
            diff <= Duration::from_millis(100),
            "duration off by {diff:?}"
        );

        // The tail is not silently dropped (the concrete failure mode a
        // whole-buffer resample sized off the input length has for
        // upsampling): the signal is still present near the end.
        let tail = &pcm.samples[pcm.samples.len() - 4_000..];
        let rms = (tail.iter().map(|&s| s * s).sum::<f32>() / tail.len() as f32).sqrt();
        assert!(rms > 0.01, "tail rms too low: {rms}");
    }
}
