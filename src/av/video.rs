//! Video frame decoding via ffmpeg (no audio), scaled down at decode time.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Once};
use std::time::Duration;

use ffmpeg_next as ff;

static FFMPEG_INIT: Once = Once::new();

pub(super) fn init_ffmpeg() {
    FFMPEG_INIT.call_once(|| {
        let _ = ff::init();
    });
}

pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    pub pts: Duration,
}

/// Decodes video frames (mp4/H.264, webm/VP9 incl. alpha if feasible, gif) without audio,
/// scaled to fit `max_w`×`max_h` (aspect kept, never upscaled).
///
/// VP9 alpha (webm with a separate alpha `BlockAdditional`) is not decoded by libswscale's
/// RGBA conversion path used here; such streams decode opaque (alpha = 255).
pub struct FrameStream {
    path: PathBuf,
    cancel: Option<Arc<AtomicBool>>,
    ictx: ff::format::context::Input,
    decoder: ff::decoder::Video,
    /// Built from the first decoded frame (the stream header can lie) and
    /// rebuilt if a frame changes format or size.
    scaler: Option<(ff::format::Pixel, u32, u32, ff::software::scaling::Context)>,
    stream_index: usize,
    time_base: ff::Rational,
    max_w: u32,
    max_h: u32,
    out_w: u32,
    out_h: u32,
    eof_sent: bool,
}

/// Containers Telegram media comes in. Files are sent by strangers: any
/// other demuxer ffmpeg would probe (images, playlists, exotic formats) is
/// refused, which also keeps its less used code out of reach.
pub(super) const CONTAINERS: &[&str] = &["mov,mp4,m4a,3gp,3g2,mj2", "matroska,webm", "gif"];
/// Codecs of Telegram videos, GIFs, round videos and video stickers.
pub(super) const CODECS: &[ff::codec::Id] = &[
    ff::codec::Id::H264,
    ff::codec::Id::HEVC,
    ff::codec::Id::VP8,
    ff::codec::Id::VP9,
    ff::codec::Id::AV1,
    ff::codec::Id::GIF,
    ff::codec::Id::MPEG4,
];
/// Largest frame side accepted, px.
const MAX_SIDE: u32 = 8192;
/// Timestamps beyond this are treated as broken.
const MAX_PTS: Duration = Duration::from_secs(24 * 3600);

/// Options that make `avformat_open_input`/`avformat_find_stream_info`
/// refuse any demuxer, decoder or protocol outside an explicit allow list,
/// instead of only checking `format().name()`/codec id after the fact.
/// `avformat_find_stream_info` opens decoders and decodes frames for
/// whatever format and codec libavformat's own probing picked (see
/// `libavformat/demux.c: find_probe_decoder`), so a check made only after
/// `input`/`input_from_stream_with_interrupt` return is too late: a
/// stranger's file has already reached that demuxer/decoder. Restricting
/// the protocol to `file` also keeps a crafted container from making the
/// demuxer open another resource (e.g. `concat:`, a referenced sidecar) via
/// a different protocol.
pub(super) fn whitelist_options(
    containers: &[&str],
    codecs: &[ff::codec::Id],
) -> ff::Dictionary<'static> {
    let mut options = ff::Dictionary::new();
    options.set("format_whitelist", &containers.join(","));
    options.set(
        "codec_whitelist",
        &codecs
            .iter()
            .map(|c| c.name())
            .collect::<Vec<_>>()
            .join(","),
    );
    options.set("protocol_whitelist", "file");
    options
}

pub(super) fn valid_frame(format: ff::format::Pixel, width: u32, height: u32) -> bool {
    format != ff::format::Pixel::None
        && (1..=MAX_SIDE).contains(&width)
        && (1..=MAX_SIDE).contains(&height)
}

impl FrameStream {
    pub fn open(path: &Path, max_w: u32, max_h: u32) -> Result<FrameStream, String> {
        Self::open_with_cancel(path, max_w, max_h, None)
    }

    pub(crate) fn open_cancelable(
        path: &Path,
        max_w: u32,
        max_h: u32,
        cancel: Arc<AtomicBool>,
    ) -> Result<FrameStream, String> {
        Self::open_with_cancel(path, max_w, max_h, Some(cancel))
    }

    fn open_with_cancel(
        path: &Path,
        max_w: u32,
        max_h: u32,
        cancel: Option<Arc<AtomicBool>>,
    ) -> Result<FrameStream, String> {
        Self::check_cancel_flag(cancel.as_deref())?;
        init_ffmpeg();
        let options = whitelist_options(CONTAINERS, CODECS);
        let ictx = if let Some(flag) = cancel.as_ref() {
            let flag = Arc::clone(flag);
            ff::format::input_with_interrupt_and_dictionary(
                path,
                move || flag.load(Ordering::Relaxed),
                options,
            )
        } else {
            ff::format::input_with_dictionary(path, options)
        }
        .map_err(|e| format!("видео: не удалось открыть файл: {e}"))?;
        Self::check_cancel_flag(cancel.as_deref())?;
        if !CONTAINERS.contains(&ictx.format().name()) {
            return Err(format!(
                "видео: неподдерживаемый формат {}",
                ictx.format().name()
            ));
        }
        let stream = ictx
            .streams()
            .best(ff::media::Type::Video)
            .ok_or_else(|| "видео: в файле нет видеодорожки".to_string())?;
        let stream_index = stream.index();
        let time_base = stream.time_base();
        if !CODECS.contains(&stream.parameters().id()) {
            return Err(format!(
                "видео: неподдерживаемый кодек {:?}",
                stream.parameters().id()
            ));
        }
        Self::check_cancel_flag(cancel.as_deref())?;
        let ctx = ff::codec::context::Context::from_parameters(stream.parameters())
            .map_err(|e| format!("видео: не удалось создать декодер: {e}"))?;
        Self::check_cancel_flag(cancel.as_deref())?;
        let decoder = ctx
            .decoder()
            .video()
            .map_err(|e| format!("видео: не удалось открыть декодер: {e}"))?;
        Self::check_cancel_flag(cancel.as_deref())?;
        // libswscale aborts the whole process on an invalid pixel format, so
        // nothing reaches it without these checks.
        if !valid_frame(decoder.format(), decoder.width(), decoder.height()) {
            return Err("видео: повреждённый файл".to_string());
        }
        let (out_w, out_h) = fit_box(decoder.width(), decoder.height(), max_w, max_h);
        let scaler = None;

        Ok(FrameStream {
            path: path.to_path_buf(),
            cancel,
            ictx,
            decoder,
            scaler,
            stream_index,
            time_base,
            max_w,
            max_h,
            out_w,
            out_h,
            eof_sent: false,
        })
    }

    fn check_cancel_flag(cancel: Option<&AtomicBool>) -> Result<(), String> {
        if cancel.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
            Err("видео: воспроизведение отменено".to_string())
        } else {
            Ok(())
        }
    }

    fn check_cancel(&self) -> Result<(), String> {
        Self::check_cancel_flag(self.cancel.as_deref())
    }

    /// Output frame size (fit inside the requested box).
    pub fn size(&self) -> (u32, u32) {
        (self.out_w, self.out_h)
    }

    pub fn next_frame(&mut self) -> Result<Option<Frame>, String> {
        self.check_cancel()?;
        let mut decoded = ff::frame::Video::empty();
        loop {
            self.check_cancel()?;
            if self.decoder.receive_frame(&mut decoded).is_ok() {
                self.check_cancel()?;
                let frame = self.convert(&decoded)?;
                self.check_cancel()?;
                return Ok(Some(frame));
            }
            if self.eof_sent {
                return Ok(None);
            }
            loop {
                self.check_cancel()?;
                let mut packet = ff::Packet::empty();
                let read = packet.read(&mut self.ictx);
                self.check_cancel()?;
                match read {
                    Ok(()) => {
                        if packet.stream() == self.stream_index {
                            self.decoder
                                .send_packet(&packet)
                                .map_err(|e| format!("видео: ошибка декодирования: {e}"))?;
                            self.check_cancel()?;
                            break;
                        }
                    }
                    Err(ff::Error::Eof) => {
                        self.decoder
                            .send_eof()
                            .map_err(|e| format!("видео: ошибка декодирования: {e}"))?;
                        self.check_cancel()?;
                        self.eof_sent = true;
                        break;
                    }
                    Err(e) => return Err(format!("видео: ошибка чтения: {e}")),
                }
            }
        }
    }

    /// Rewinds to the start of the stream, for looping GIF-like animations.
    pub fn rewind(&mut self) -> Result<(), String> {
        self.check_cancel()?;
        self.decoder.flush();
        self.check_cancel()?;
        match self.ictx.seek(0, ..) {
            Ok(()) => {
                self.check_cancel()?;
                self.eof_sent = false;
                Ok(())
            }
            Err(_) => {
                self.check_cancel()?;
                *self = FrameStream::open_with_cancel(
                    &self.path,
                    self.max_w,
                    self.max_h,
                    self.cancel.clone(),
                )?;
                self.check_cancel()
            }
        }
    }

    fn convert(&mut self, decoded: &ff::frame::Video) -> Result<Frame, String> {
        let (format, w, h) = (decoded.format(), decoded.width(), decoded.height());
        if !valid_frame(format, w, h) {
            return Err("видео: повреждённый кадр".to_string());
        }
        if !matches!(&self.scaler, Some((f, sw, sh, _)) if (*f, *sw, *sh) == (format, w, h)) {
            let (out_w, out_h) = fit_box(w, h, self.max_w, self.max_h);
            let scaler = ff::software::scaling::Context::get(
                format,
                w,
                h,
                ff::format::Pixel::RGBA,
                out_w,
                out_h,
                ff::software::scaling::Flags::BILINEAR,
            )
            .map_err(|e| format!("видео: не удалось создать масштабировщик: {e}"))?;
            self.out_w = out_w;
            self.out_h = out_h;
            self.scaler = Some((format, w, h, scaler));
        }
        let Some((_, _, _, scaler)) = &mut self.scaler else {
            return Err("видео: нет масштабировщика".to_string());
        };
        let mut scaled = ff::frame::Video::empty();
        scaler
            .run(decoded, &mut scaled)
            .map_err(|e| format!("видео: ошибка масштабирования: {e}"))?;

        let width = scaled.width();
        let height = scaled.height();
        let stride = scaled.stride(0);
        let data = scaled.data(0);
        let row_bytes = width as usize * 4;
        let mut rgba = Vec::with_capacity(row_bytes * height as usize);
        for row in 0..height as usize {
            let start = row * stride;
            rgba.extend_from_slice(&data[start..start + row_bytes]);
        }

        let pts = decoded
            .pts()
            .map(|p| pts_to_duration(p, self.time_base))
            .unwrap_or_default();

        Ok(Frame {
            width,
            height,
            rgba,
            pts,
        })
    }
}

pub(super) fn pts_to_duration(pts: i64, tb: ff::Rational) -> Duration {
    if pts <= 0 || tb.denominator() == 0 {
        return Duration::ZERO;
    }
    let secs = pts as f64 * f64::from(tb.numerator()) / f64::from(tb.denominator());
    // Crafted timestamps must neither panic nor stall playback for ages.
    Duration::try_from_secs_f64(secs.max(0.0))
        .unwrap_or(MAX_PTS)
        .min(MAX_PTS)
}

/// Scales `(w, h)` down to fit inside `(max_w, max_h)`, keeping aspect ratio, never upscaling.
pub(super) fn fit_box(w: u32, h: u32, max_w: u32, max_h: u32) -> (u32, u32) {
    let max_w = max_w.max(1);
    let max_h = max_h.max(1);
    if w == 0 || h == 0 {
        return (max_w, max_h);
    }
    if w <= max_w && h <= max_h {
        return (w, h);
    }
    let scale = (f64::from(max_w) / f64::from(w)).min(f64::from(max_h) / f64::from(h));
    let out_w = ((f64::from(w) * scale).round() as u32).max(1);
    let out_h = ((f64::from(h) * scale).round() as u32).max(1);
    (out_w, out_h)
}

/// First frame as RGBA (thumbnail for videos without a preview).
pub fn first_frame(path: &Path, max_w: u32, max_h: u32) -> Result<Frame, String> {
    let mut stream = FrameStream::open(path, max_w, max_h)?;
    stream
        .next_frame()?
        .ok_or_else(|| "видео: не удалось получить кадр".to_string())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn encode_test_clip(path: &Path, width: u32, height: u32, frames: usize) {
        let _ = ff::init();
        let codec = ff::encoder::find(ff::codec::Id::MPEG4).expect("mpeg4 encoder available");
        let mut octx = ff::format::output(path).expect("create output");
        let global_header = octx
            .format()
            .flags()
            .contains(ff::format::Flags::GLOBAL_HEADER);
        let mut ost = octx.add_stream(codec).expect("add stream");
        let ost_index = ost.index();

        let ctx = ff::codec::context::Context::new_with_codec(codec);
        let mut encoder = ctx.encoder().video().expect("video encoder");
        encoder.set_width(width);
        encoder.set_height(height);
        encoder.set_format(ff::format::Pixel::YUV420P);
        encoder.set_time_base((1, 25));
        if global_header {
            encoder.set_flags(ff::codec::Flags::GLOBAL_HEADER);
        }
        let mut encoder = encoder.open_as(codec).expect("open encoder");
        ost.set_time_base((1, 25));
        ost.set_parameters(&encoder);

        octx.write_header().expect("write header");
        let out_tb = octx.stream(ost_index).unwrap().time_base();

        for i in 0..frames {
            let mut frame = ff::frame::Video::new(ff::format::Pixel::YUV420P, width, height);
            let y_val = (i * 20 % 256) as u8;
            {
                let stride = frame.stride(0);
                let h = frame.plane_height(0) as usize;
                let w = frame.plane_width(0) as usize;
                let data = frame.data_mut(0);
                for row in 0..h {
                    data[row * stride..row * stride + w].fill(y_val);
                }
            }
            for plane in [1usize, 2] {
                let stride = frame.stride(plane);
                let h = frame.plane_height(plane) as usize;
                let w = frame.plane_width(plane) as usize;
                let data = frame.data_mut(plane);
                for row in 0..h {
                    data[row * stride..row * stride + w].fill(128u8);
                }
            }
            frame.set_pts(Some(i as i64));
            encoder.send_frame(&frame).expect("send frame");
            let mut packet = ff::Packet::empty();
            while encoder.receive_packet(&mut packet).is_ok() {
                packet.set_stream(ost_index);
                packet.rescale_ts((1, 25), out_tb);
                packet.write_interleaved(&mut octx).expect("write packet");
            }
        }
        encoder.send_eof().expect("eof");
        let mut packet = ff::Packet::empty();
        while encoder.receive_packet(&mut packet).is_ok() {
            packet.set_stream(ost_index);
            packet.rescale_ts((1, 25), out_tb);
            packet.write_interleaved(&mut octx).expect("write packet");
        }
        octx.write_trailer().expect("trailer");
    }

    #[test]
    fn decodes_scaled_frames_and_rewinds() {
        let path =
            std::env::temp_dir().join(format!("telega-av-video-test-{}.mkv", std::process::id()));
        encode_test_clip(&path, 64, 48, 10);

        let mut stream = FrameStream::open(&path, 32, 32).expect("open");
        assert_eq!(stream.size(), (32, 24));

        let mut count = 0;
        while let Some(frame) = stream.next_frame().expect("next_frame") {
            assert_eq!(frame.width, 32);
            assert_eq!(frame.height, 24);
            assert_eq!(frame.rgba.len(), 32 * 24 * 4);
            count += 1;
        }
        assert_eq!(count, 10);

        stream.rewind().expect("rewind");
        let first = stream
            .next_frame()
            .expect("next_frame")
            .expect("frame after rewind");
        assert_eq!((first.width, first.height), (32, 24));

        let thumb = first_frame(&path, 32, 32).expect("first_frame");
        assert_eq!((thumb.width, thumb.height), (32, 24));

        let _ = std::fs::remove_file(&path);
    }
}
