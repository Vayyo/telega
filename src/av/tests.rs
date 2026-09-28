//! Hostile input: files from other people reach these decoders without a
//! click. Every one must come back as an error, never abort the process.

use ffmpeg_next as ff;
use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;

struct Temp(PathBuf);

impl Temp {
    fn new(name: &str, bytes: &[u8]) -> Self {
        let temp = Self::path(name);
        std::fs::write(&temp.0, bytes).unwrap();
        temp
    }

    /// Reserves a unique path without writing to it, for callers that
    /// encode into it themselves (still cleaned up on drop).
    fn path(name: &str) -> Self {
        Self(std::env::temp_dir().join(format!("telega-av-{}-{name}", std::process::id())))
    }
}

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn noise(seed: &mut u64, n: usize) -> Vec<u8> {
    (0..n)
        .map(|_| {
            *seed ^= *seed << 13;
            *seed ^= *seed >> 7;
            *seed ^= *seed << 17;
            *seed as u8
        })
        .collect()
}

#[test]
fn garbage_behind_real_signatures_is_an_error() {
    let mut seed = 7;
    let cases: [(&str, &[u8], usize); 9] = [
        // Once aborted the process inside libswscale (image2 demuxer).
        ("jpg", &[0xFF, 0xD8, 0xFF, 0xE0], 5000),
        ("png", b"\x89PNG\r\n\x1a\n", 5000),
        ("webp", b"RIFF\x00\x10\x00\x00WEBPVP8 ", 5000),
        ("gif", b"GIF89a", 5000),
        ("mp4", b"\x00\x00\x00\x18ftypmp42", 20000),
        ("webm", b"\x1a\x45\xdf\xa3", 20000),
        ("ogg", b"OggS", 20000),
        ("tgs", &[0x1f, 0x8b, 0x08, 0x00], 5000),
        ("empty", b"", 0),
    ];
    for (name, magic, len) in cases {
        let file = Temp::new(name, &[magic, &noise(&mut seed, len)[..]].concat());
        let path = &file.0;
        assert!(
            crate::app::media::decode(&path.to_string_lossy()).is_err(),
            "{name} image"
        );
        assert!(
            super::video::FrameStream::open(path, 64, 64).is_err(),
            "{name} video"
        );
        assert!(
            super::audio::decode_to_pcm(path, 48_000, 2).is_err(),
            "{name} audio"
        );
        assert!(super::lottie::Lottie::open(path).is_err(), "{name} lottie");
    }
}

#[test]
fn compressed_sticker_bomb_is_refused_early() {
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
    let spaces = vec![b' '; 1 << 20];
    for _ in 0..64 {
        gz.write_all(&spaces).unwrap();
    }
    let file = Temp::new("bomb.tgs", &gz.finish().unwrap());
    let err = super::lottie::Lottie::open(&file.0)
        .err()
        .expect("64 MB of JSON accepted");
    assert!(err.contains("слишком большая"), "{err}");
}

#[test]
fn image_claiming_huge_size_is_refused() {
    // Must match MAX_IMAGE_SIDE in app/media.rs (private to that module): a
    // decoder that enforces the limit must reject one pixel over it and
    // accept exactly the limit, without relying on a corrupt file to fail
    // for an unrelated reason (e.g. a bad CRC) before the check even runs.
    const MAX_IMAGE_SIDE: u32 = 4096;
    let encode = |width: u32, height: u32| {
        let mut png = Vec::new();
        image::DynamicImage::new_luma8(width, height)
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        png
    };

    let huge = Temp::new("huge.png", &encode(MAX_IMAGE_SIDE + 1, 1));
    assert!(crate::app::media::decode(&huge.0.to_string_lossy()).is_err());

    let at_limit = Temp::new("at-limit.png", &encode(MAX_IMAGE_SIDE, 1));
    assert!(crate::app::media::decode(&at_limit.0.to_string_lossy()).is_ok());
}

/// Encodes `samples` (mono, at `rate` Hz) as an OGG/Vorbis file. Unlike
/// Opus, libvorbis accepts any sample rate natively (no internal
/// resampling to a fixed rate), so this is how the tests below get a file
/// whose audio genuinely decodes at e.g. 44.1 kHz.
fn encode_vorbis_ogg(samples: &[f32], rate: u32, path: &std::path::Path) {
    let _ = ff::init();
    let codec = ff::encoder::find_by_name("libvorbis")
        .or_else(|| ff::encoder::find(ff::codec::Id::VORBIS))
        .expect("vorbis encoder available");
    let mut octx = ff::format::output(path).expect("create output");
    let global_header = octx
        .format()
        .flags()
        .contains(ff::format::Flags::GLOBAL_HEADER);
    let mut ost = octx.add_stream(codec).expect("add stream");
    let ost_index = ost.index();
    let sample_fmt = codec
        .audio()
        .ok()
        .and_then(|a| a.formats())
        .and_then(|mut it| it.find(|f| matches!(f, ff::format::Sample::F32(_))))
        .expect("float sample format available");
    let layout = ff::ChannelLayout::MONO;

    let mut encoder_ctx = ff::codec::context::Context::new_with_codec(codec);
    // Only takes effect for FFmpeg's own experimental `vorbis` encoder, in
    // case a build without `libvorbis` is ever used to run the tests.
    encoder_ctx.compliance(ff::codec::Compliance::Experimental);
    let mut encoder = encoder_ctx.encoder().audio().expect("audio encoder");
    encoder.set_rate(rate as i32);
    encoder.set_channel_layout(layout);
    encoder.set_format(sample_fmt);
    if global_header {
        encoder.set_flags(ff::codec::Flags::GLOBAL_HEADER);
    }
    encoder.set_time_base((1, rate as i32));
    let mut encoder = encoder.open_as(codec).expect("open encoder");
    ost.set_time_base((1, rate as i32));
    ost.set_parameters(&encoder);

    octx.write_header().expect("write header");
    let out_tb = octx.stream(ost_index).unwrap().time_base();

    // Vorbis accepts frames of any size (`frame_size() == 0`); chunk large
    // inputs anyway so one call doesn't have to hold the whole recording.
    let frame_size = match encoder.frame_size() {
        0 => 4096,
        n => n as usize,
    };
    let mut pts: i64 = 0;
    let mut i = 0;
    while i < samples.len() {
        let end = (i + frame_size).min(samples.len());
        let chunk = &samples[i..end];
        let mut frame = ff::frame::Audio::new(sample_fmt, chunk.len(), layout);
        frame.set_rate(rate);
        frame.plane_mut::<f32>(0)[..chunk.len()].copy_from_slice(chunk);
        frame.set_pts(Some(pts));
        pts += chunk.len() as i64;
        encoder.send_frame(&frame).expect("send frame");
        let mut packet = ff::Packet::empty();
        while encoder.receive_packet(&mut packet).is_ok() {
            packet.set_stream(ost_index);
            packet.rescale_ts((1, rate as i32), out_tb);
            packet.write_interleaved(&mut octx).expect("write packet");
        }
        i = end;
    }
    encoder.send_eof().expect("eof");
    let mut packet = ff::Packet::empty();
    while encoder.receive_packet(&mut packet).is_ok() {
        packet.set_stream(ost_index);
        packet.rescale_ts((1, rate as i32), out_tb);
        packet.write_interleaved(&mut octx).expect("write packet");
    }
    octx.write_trailer().expect("trailer");
}

fn sine(rate: u32, seconds: f64) -> Vec<f32> {
    let n = (f64::from(rate) * seconds) as usize;
    (0..n)
        .map(|i| {
            (2.0 * std::f64::consts::PI * 440.0 * i as f64 / f64::from(rate)).sin() as f32 * 0.5
        })
        .collect()
}

#[test]
fn upsampling_44100_to_48000_keeps_the_sample_count() {
    // A 44.1 kHz track resampled to a 48 kHz device used to leave the ~8.8%
    // surplus queued inside libswresample forever instead of producing it:
    // one second in must come out as ~48000 samples, not ~44100.
    let rate = 44_100u32;
    let samples = sine(rate, 1.0);
    let file = Temp::path("upsample.ogg");
    encode_vorbis_ogg(&samples, rate, &file.0);
    let pcm = super::audio::decode_to_pcm(&file.0, 48_000, 1).expect("decode");
    assert!(
        (47_000..=49_000).contains(&pcm.samples.len()),
        "expected ~48000 samples resampled from 44.1 kHz, got {}",
        pcm.samples.len()
    );
}

#[test]
fn a_long_low_rate_recording_encodes_to_its_full_length() {
    // A 16 kHz recording resampled to 48 kHz for Opus used to have its
    // internal FIFO drained by a flush loop capped at 65536 samples: past
    // ~2 s the tail was silently cut (kept about a third for much longer
    // recordings). Three seconds must round-trip to ~three seconds.
    let rate = 16_000u32;
    let seconds = 3.0;
    let samples = sine(rate, seconds);
    let file = Temp::path("longrec.ogg");
    super::audio::encode_opus_ogg(&samples, rate, &file.0).expect("encode");
    let pcm = super::audio::decode_to_pcm(&file.0, 48_000, 1).expect("decode");
    let target = Duration::from_secs_f64(seconds);
    let diff = pcm.duration().abs_diff(target);
    assert!(
        diff <= Duration::from_millis(150),
        "a {seconds}s recording at {rate} Hz lost its tail: decoded to {:?} instead of ~{target:?}",
        pcm.duration()
    );
}
