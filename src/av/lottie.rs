//! Telegram animated sticker (.tgs = gzip-compressed Lottie JSON) rendering via rlottie.

use std::io::Read;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use flate2::read::GzDecoder;

static NEXT_CACHE_KEY: AtomicU64 = AtomicU64::new(0);

/// Telegram animated sticker (.tgs = gzip-compressed Lottie JSON).
/// Largest .tgs file and inflated Lottie JSON accepted.
const MAX_FILE: usize = 1024 * 1024;
const MAX_JSON: usize = 8 * 1024 * 1024;

pub struct Lottie {
    animation: rlottie::Animation,
    surface: Option<rlottie::Surface>,
}

impl Lottie {
    /// Opens a `.tgs` (gzip) or plain Lottie JSON file.
    ///
    /// Telegram limits .tgs to 64 KB; generous bounds still stop bombs.
    pub fn open(path: &Path) -> Result<Lottie, String> {
        let bytes =
            std::fs::read(path).map_err(|e| format!("стикер: не удалось прочитать файл: {e}"))?;
        if bytes.len() > MAX_FILE {
            return Err("стикер: слишком большой файл".to_string());
        }
        let json = if is_gzip(&bytes) {
            // A gzip bomb must not inflate into gigabytes: stop one byte
            // past the limit and refuse.
            let mut out = String::new();
            GzDecoder::new(&bytes[..])
                .take(MAX_JSON as u64 + 1)
                .read_to_string(&mut out)
                .map_err(|e| format!("стикер: не удалось распаковать: {e}"))?;
            if out.len() > MAX_JSON {
                return Err("стикер: слишком большая анимация".to_string());
            }
            out
        } else {
            String::from_utf8(bytes)
                .map_err(|e| format!("стикер: файл не в кодировке UTF-8: {e}"))?
        };
        Self::from_json(&json)
    }

    pub fn from_json(json: &str) -> Result<Lottie, String> {
        // rlottie caches parsed animations by this key; keep every instance unique so
        // callers never see a stale animation from an unrelated cache hit.
        let key = NEXT_CACHE_KEY.fetch_add(1, Ordering::Relaxed);
        let cache_key = format!("telega-lottie-{key}");
        let animation = rlottie::Animation::from_data(json.to_owned(), cache_key, "")
            .ok_or_else(|| "стикер: не удалось разобрать Lottie-анимацию".to_string())?;
        Ok(Lottie {
            animation,
            surface: None,
        })
    }

    pub fn frame_count(&self) -> usize {
        self.animation.totalframe()
    }

    pub fn frame_rate(&self) -> f64 {
        self.animation.framerate()
    }

    /// Render frame to straight (non-premultiplied) RGBA of size `w`×`h`.
    pub fn render(&mut self, frame: usize, w: u32, h: u32) -> Vec<u8> {
        let size = rlottie::Size::new(w as usize, h as usize);
        let needs_new = !matches!(&self.surface, Some(s) if s.size() == size);
        if needs_new {
            self.surface = Some(rlottie::Surface::new(size));
        }
        let surface = self.surface.as_mut().expect("surface just ensured");
        self.animation.render(frame, surface);
        bgra_premultiplied_to_rgba(surface.data_as_bytes())
    }
}

fn is_gzip(bytes: &[u8]) -> bool {
    bytes.len() >= 2 && bytes[0] == 0x1f && bytes[1] == 0x8b
}

fn bgra_premultiplied_to_rgba(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    for px in data.as_chunks::<4>().0 {
        let (b, g, r, a) = (px[0], px[1], px[2], px[3]);
        let unmul = |c: u8| -> u8 {
            if a == 0 {
                0
            } else {
                ((u32::from(c) * 255) / u32::from(a)).min(255) as u8
            }
        };
        out.push(unmul(r));
        out.push(unmul(g));
        out.push(unmul(b));
        out.push(a);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const TINY_ANIMATION: &str = r#"{
        "v": "5.5.2",
        "fr": 2,
        "ip": 0,
        "op": 2,
        "w": 64,
        "h": 64,
        "nm": "test",
        "ddd": 0,
        "assets": [],
        "layers": [
            {
                "ddd": 0,
                "ind": 1,
                "ty": 4,
                "nm": "rect",
                "sr": 1,
                "ks": {
                    "o": {"a": 0, "k": 100},
                    "r": {"a": 0, "k": 0},
                    "p": {"a": 0, "k": [32, 32, 0]},
                    "a": {"a": 0, "k": [0, 0, 0]},
                    "s": {"a": 0, "k": [100, 100, 100]}
                },
                "ao": 0,
                "shapes": [
                    {
                        "ty": "rc",
                        "d": 1,
                        "s": {"a": 0, "k": [40, 40]},
                        "p": {"a": 0, "k": [0, 0]},
                        "r": {"a": 0, "k": 0},
                        "nm": "rect"
                    },
                    {
                        "ty": "fl",
                        "c": {"a": 0, "k": [1, 0, 0, 1]},
                        "o": {"a": 0, "k": 100},
                        "nm": "fill"
                    }
                ],
                "ip": 0,
                "op": 2,
                "st": 0,
                "bm": 0
            }
        ]
    }"#;

    #[test]
    fn renders_tiny_inline_animation() {
        let mut lottie = Lottie::from_json(TINY_ANIMATION).expect("parse Lottie JSON");
        assert_eq!(lottie.frame_count(), 2);
        assert!((lottie.frame_rate() - 2.0).abs() < 0.001);

        let rgba = lottie.render(0, 32, 32);
        assert_eq!(rgba.len(), 32 * 32 * 4);
        // The rectangle covers the frame center, so some pixels must be non-transparent
        // and colored (not just black-with-zero-alpha).
        assert!(
            rgba.as_chunks::<4>()
                .0
                .iter()
                .any(|px| px[3] != 0 && (px[0] != 0 || px[1] != 0 || px[2] != 0))
        );
    }
}
