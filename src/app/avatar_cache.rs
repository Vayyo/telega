//! Decoded profile pictures kept on disk, in `paths::avatars()`.
//!
//! Decoding a picture happens once per session today: a restart throws every
//! decoded avatar away and re-decodes the same bytes from disk, and the same
//! picture arriving under two `file_id`s (the same user in several chats) is
//! decoded and kept twice. The cache here is addressed by the content of the
//! picture's file, not by the `file_id`: identical bytes have one entry,
//! whichever peers and sessions ask for them.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use super::avatars::SIDE;

/// Bytes of one decoded picture: the only size an entry may have. Anything
/// else on disk is a torn write, not a picture.
const ENTRY_LEN: usize = (SIDE * SIDE * 4) as usize;

/// Content address of a decoded picture: BLAKE3 of its bytes.
pub(crate) fn hash(bytes: &[u8]) -> [u8; 32] {
    blake3::hash(bytes).into()
}

/// Lowercase hex of all 32 bytes: the whole address is visible in the name,
/// so two different pictures never collide on one file.
fn name(hash: &[u8; 32]) -> String {
    hash.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// The one file holding the picture of `hash`, inside `paths::avatars()`.
fn entry(hash: &[u8; 32]) -> PathBuf {
    crate::paths::avatars().join(format!("{}.rgba", name(hash)))
}

/// Writes a decoded picture under its content address. Storing bytes that
/// are already there rewrites the same file. The directory is created on
/// the first call.
pub(crate) fn store(hash: &[u8; 32], rgba: &[u8]) -> std::io::Result<()> {
    let dir = crate::paths::avatars();
    std::fs::create_dir_all(&dir)?;
    let path = entry(hash);
    // A fresh name per call, then rename: a crash mid-write leaves the
    // entry as it was (or absent), never half a picture, and a name nobody
    // else picks keeps concurrent stores of different pictures apart.
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let tmp = dir.join(format!(
        "{}.{}.{}.tmp",
        name(hash),
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let result = std::fs::write(&tmp, rgba).and_then(|()| std::fs::rename(&tmp, &path));
    if result.is_err() {
        // The temporary file must not outlive the attempt.
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// The decoded picture of `hash`, or `None` if it was never stored (or the
/// entry is unreadable, or is not a whole picture: a broken entry is a miss
/// to be decoded again, not something to paint on screen).
pub(crate) fn load(hash: &[u8; 32]) -> Option<Vec<u8>> {
    let mut file = std::fs::File::open(entry(hash)).ok()?;
    let mut bytes = Vec::with_capacity(ENTRY_LEN);
    file.read_to_end(&mut bytes).ok()?;
    if bytes.len() != ENTRY_LEN {
        return None;
    }
    let _ = file.set_modified(SystemTime::now());
    Some(bytes)
}

pub(crate) fn sweep(limits: crate::settings::CacheLimits) {
    sweep_in(&crate::paths::avatars(), limits);
}

fn sweep_in(dir: &Path, limits: crate::settings::CacheLimits) {
    let now = SystemTime::now();
    let max_age = Duration::from_secs(limits.days.saturating_mul(86_400));
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut survivors = Vec::new();
    let mut bytes = 0u64;
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let Some(stem) = name.strip_suffix(".rgba") else {
            continue;
        };
        if stem.len() != 64 || !stem.bytes().all(|b| b.is_ascii_hexdigit()) {
            continue;
        }
        let Ok(metadata) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if !metadata.file_type().is_file() {
            continue;
        }
        let Ok(modified) = metadata.modified() else {
            continue;
        };
        if now.duration_since(modified).is_ok_and(|age| age > max_age)
            && std::fs::remove_file(&path).is_ok()
        {
            continue;
        }
        bytes = bytes.saturating_add(metadata.len());
        survivors.push((modified, path, metadata.len()));
    }
    if bytes <= limits.bytes {
        return;
    }
    survivors.sort_unstable_by(|a, b| (a.0, &a.1).cmp(&(b.0, &b.1)));
    for (_, path, size) in survivors {
        if bytes <= limits.bytes {
            break;
        }
        if std::fs::remove_file(path).is_ok() {
            bytes = bytes.saturating_sub(size);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Bytes of one decoded 64×64 RGBA picture.
    const RGBA_LEN: usize = 64 * 64 * 4;

    /// A picture with bytes nobody else in the test binary writes into the
    /// shared cache directory.
    fn picture(seed: u8) -> Vec<u8> {
        (0..RGBA_LEN)
            .map(|i| (i as u8).wrapping_mul(37).wrapping_add(seed))
            .collect()
    }

    /// Where the entry of `hash` lives: lowercase hex of the 32 bytes plus
    /// `.rgba`. A leftover of an earlier run in a reused process-id
    /// directory is the same file, since the name comes from the content.
    fn entry(hash: &[u8; 32]) -> std::path::PathBuf {
        let name: String = hash.iter().map(|byte| format!("{byte:02x}")).collect();
        crate::paths::avatars().join(format!("{name}.rgba"))
    }

    /// Files of the cache holding exactly these bytes: the entry itself, and
    /// any leftover a failed store could have left behind.
    fn entries_of(bytes: &[u8]) -> Vec<std::path::PathBuf> {
        let Ok(dir) = std::fs::read_dir(crate::paths::avatars()) else {
            return Vec::new();
        };
        let mut found: Vec<_> = dir
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|path| std::fs::read(path).is_ok_and(|bytes_of| bytes_of == bytes))
            .collect();
        found.sort();
        found
    }

    /// Serialises the tests that touch the shared cache directory, which is
    /// one directory for the whole binary while its tests run in parallel
    /// (see `paths::AVATARS_TEST_LOCK`). A test that panicked while holding
    /// the lock must not fail the others: it serialises, it guards nothing.
    fn shared_cache() -> std::sync::MutexGuard<'static, ()> {
        crate::paths::AVATARS_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Each sweep test owns its directory; never use the shared avatar path.
    fn sweep_dir(label: &str) -> std::path::PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "telega-avatar-sweep-{}-{label}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir(&dir).unwrap();
        dir
    }

    fn sweep_entry(
        dir: &std::path::Path,
        number: u8,
        bytes: usize,
        modified: std::time::SystemTime,
    ) -> std::path::PathBuf {
        let path = dir.join(format!("{number:064x}.rgba"));
        std::fs::write(&path, vec![number; bytes]).unwrap();
        std::fs::File::open(&path)
            .unwrap()
            .set_modified(modified)
            .unwrap();
        path
    }

    #[test]
    fn sweep_discards_expired_entries_but_keeps_fresh_and_future_entries() {
        let dir = sweep_dir("age");
        let now = std::time::SystemTime::now();
        let old = sweep_entry(
            &dir,
            1,
            10,
            now - std::time::Duration::from_secs(10 * 86_400),
        );
        let fresh = sweep_entry(&dir, 2, 10, now - std::time::Duration::from_secs(86_400));
        let future = sweep_entry(&dir, 3, 10, now + std::time::Duration::from_secs(86_400));
        sweep_in(
            &dir,
            crate::settings::CacheLimits {
                bytes: 100,
                days: 5,
            },
        );
        assert!(!old.exists(), "an expired entry is removed");
        assert!(fresh.exists(), "an unexpired entry stays");
        assert!(future.exists(), "a future timestamp is not expired");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn sweep_evicts_oldest_entries_until_remaining_bytes_fit() {
        let dir = sweep_dir("size");
        let now = std::time::SystemTime::now();
        let oldest = sweep_entry(
            &dir,
            4,
            10,
            now - std::time::Duration::from_secs(3 * 86_400),
        );
        let middle = sweep_entry(
            &dir,
            5,
            10,
            now - std::time::Duration::from_secs(2 * 86_400),
        );
        let newest = sweep_entry(&dir, 6, 10, now - std::time::Duration::from_secs(86_400));
        sweep_in(
            &dir,
            crate::settings::CacheLimits {
                bytes: 15,
                days: 30,
            },
        );
        assert!(!oldest.exists(), "the oldest entry leaves first");
        assert!(!middle.exists(), "eviction continues until under budget");
        assert!(newest.exists(), "the newest entry fits and remains");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn sweep_never_touches_files_that_are_not_cache_entries() {
        let dir = sweep_dir("other");
        let notes = dir.join("notes.txt");
        let tmp = dir.join(format!("{:064x}.tmp", 7));
        let bad_hash = dir.join("not-a-hex-name.rgba");
        for file in [&notes, &tmp, &bad_hash] {
            std::fs::write(file, b"other data").unwrap();
        }
        let expired = sweep_entry(
            &dir,
            8,
            10,
            std::time::SystemTime::now() - std::time::Duration::from_secs(10 * 86_400),
        );
        sweep_in(&dir, crate::settings::CacheLimits { bytes: 1, days: 1 });
        assert!(!expired.exists());
        for file in [&notes, &tmp, &bad_hash] {
            assert_eq!(std::fs::read(file).unwrap(), b"other data");
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn store_then_load_returns_the_picture() {
        let _shared = shared_cache();
        let rgba = picture(1);
        let digest = hash(&rgba);
        store(&digest, &rgba).unwrap();
        assert_eq!(
            load(&digest).as_deref(),
            Some(rgba.as_slice()),
            "a stored picture comes back byte for byte"
        );
    }

    #[test]
    fn identical_bytes_hash_alike_and_a_changed_byte_hashes_differently() {
        let _shared = shared_cache();
        assert_eq!(
            hash(&picture(2)),
            hash(&picture(2)),
            "the same bytes always hash the same"
        );
        let mut changed = picture(2);
        changed[1000] ^= 0x01;
        assert_ne!(
            hash(&picture(2)),
            hash(&changed),
            "one changed byte is another picture"
        );
    }

    #[test]
    fn load_of_a_hash_that_was_never_stored_is_none() {
        let _shared = shared_cache();
        // Nothing must answer for a picture of this run, not even an entry
        // of an earlier run that happened to reuse the process-id directory.
        let _ = std::fs::remove_file(entry(&hash(&picture(3))));
        assert_eq!(load(&hash(&picture(3))), None);
    }

    #[test]
    fn store_creates_the_cache_directory() {
        let _shared = shared_cache();
        let rgba = picture(4);
        let digest = hash(&rgba);
        // The only test of the binary that removes the directory: the shared
        // lock keeps the tests of this module from writing into it while it
        // is gone, and it goes away and comes back as few instructions apart
        // as possible for the rest of the binary.
        let dir = crate::paths::avatars();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(!dir.exists(), "no cache directory");
        store(&digest, &rgba).unwrap();
        assert!(dir.is_dir(), "the directory is created on the first store");
        assert_eq!(entries_of(&rgba).len(), 1);
    }

    #[test]
    fn the_same_picture_stored_twice_leaves_one_file() {
        let _shared = shared_cache();
        let rgba = picture(5);
        let digest = hash(&rgba);
        store(&digest, &rgba).unwrap();
        store(&digest, &rgba).unwrap();
        assert_eq!(
            entries_of(&rgba).len(),
            1,
            "storing the same content again rewrites its one entry"
        );
    }

    #[test]
    fn different_pictures_are_different_files() {
        let _shared = shared_cache();
        let first = picture(6);
        let second = picture(7);
        store(&hash(&first), &first).unwrap();
        store(&hash(&second), &second).unwrap();
        assert_ne!(hash(&first), hash(&second));
        assert_ne!(entry(&hash(&first)), entry(&hash(&second)));
        assert_eq!(entries_of(&first).len(), 1);
        assert_eq!(entries_of(&second).len(), 1);
    }

    #[test]
    fn stored_file_is_the_picture_itself_without_a_header() {
        let _shared = shared_cache();
        let rgba = picture(8);
        let digest = hash(&rgba);
        store(&digest, &rgba).unwrap();
        let bytes = std::fs::read(entry(&digest)).unwrap();
        assert_eq!(bytes.len(), RGBA_LEN, "64×64 RGBA on the nose");
        assert_eq!(bytes, rgba, "the decoded bytes, written verbatim");
    }

    #[test]
    fn load_rejects_a_truncated_or_oversized_entry() {
        let _shared = shared_cache();
        let rgba = picture(9);
        let digest = hash(&rgba);
        std::fs::create_dir_all(crate::paths::avatars()).unwrap();
        let mut oversized = rgba.clone();
        oversized.push(0);
        for (what, bytes) in [
            ("empty", &rgba[..0]),
            ("one byte", &rgba[..1]),
            ("a tenth of a picture", &rgba[..RGBA_LEN / 10]),
            ("one byte short", &rgba[..RGBA_LEN - 1]),
            ("one byte too long", oversized.as_slice()),
        ] {
            std::fs::write(entry(&digest), bytes).unwrap();
            assert_eq!(
                load(&digest),
                None,
                "{what} is not a picture: a broken entry must not come back"
            );
        }
    }
}
