//! Client password: a key derived from it encrypts the TDLib database (via
//! TDLib) and the texts in the archive of deleted messages. Without the
//! password neither opens. Downloaded files are not encrypted.

use base64::Engine;
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};

pub type Key = [u8; 32];

/// Random salt for a new password, hex.
pub fn new_salt() -> String {
    let mut salt = [0u8; 16];
    getrandom::fill(&mut salt).expect("system random source");
    salt.iter().map(|b| format!("{b:02x}")).collect()
}

/// Key of a password (Argon2id; takes a fraction of a second on purpose,
/// so run it off the UI thread).
pub fn derive(password: &str, salt: &str) -> Result<Key, String> {
    let mut key = [0u8; 32];
    argon2::Argon2::default()
        .hash_password_into(password.as_bytes(), salt.as_bytes(), &mut key)
        .map_err(|e| format!("пароль: {e}"))?;
    Ok(key)
}

/// The key as TDLib takes it (bytes, base64 in JSON).
pub fn tdlib_key(key: Option<&Key>) -> String {
    key.map(|k| base64::engine::general_purpose::STANDARD.encode(k))
        .unwrap_or_default()
}

/// A short fingerprint of the key, to check a password without keeping it.
pub fn check_value(key: &Key) -> String {
    // Deterministic: the first bytes of the encryption of zeros under a zero
    // nonce identify the key without revealing it.
    let cipher = XChaCha20Poly1305::new(key.into());
    let sealed = cipher
        .encrypt(XNonce::from_slice(&[0u8; 24]), [0u8; 16].as_slice())
        .expect("encrypting 16 bytes");
    sealed[..8].iter().map(|b| format!("{b:02x}")).collect()
}

const PREFIX: &str = "enc:";
/// Marks text stored without a password: without this, a plain message that
/// happens to start with `enc:` would be mistaken for ciphertext by `open`,
/// which then refuses it (or, worse, `rekey` fails and the client is stuck
/// re-keying TDLib without ever enabling the archive password).
const PLAIN_PREFIX: &str = "txt:";

/// Encrypts a text for storage: "enc:" + base64(nonce ‖ ciphertext).
pub fn seal(key: &Key, plain: &[u8]) -> Result<String, String> {
    let mut nonce = [0u8; 24];
    getrandom::fill(&mut nonce).map_err(|e| e.to_string())?;
    let sealed = XChaCha20Poly1305::new(key.into())
        .encrypt(XNonce::from_slice(&nonce), plain)
        .map_err(|e| e.to_string())?;
    let mut bytes = nonce.to_vec();
    bytes.extend(sealed);
    Ok(format!(
        "{PREFIX}{}",
        base64::engine::general_purpose::STANDARD.encode(bytes)
    ))
}

/// Wraps plain text for storage so it can never collide with `PREFIX`, no
/// matter what the text itself starts with.
pub fn mark_plain(text: &str) -> String {
    format!("{PLAIN_PREFIX}{text}")
}

/// Stores `plain` under `key` if there is one, or marks it as plain text.
/// The single place that decides how a text is written, so callers never
/// write raw, marker-less text that `open` could misread.
pub fn store(key: Option<&Key>, plain: &str) -> Result<String, String> {
    match key {
        Some(key) => seal(key, plain.as_bytes()),
        None => Ok(mark_plain(plain)),
    }
}

/// Reverses `seal` or `mark_plain`; a row written before either marker
/// existed comes back as is (old plain text, never `PREFIX`-clashing since
/// it predates the bug this marker fixes).
pub fn open(key: Option<&Key>, stored: &str) -> Result<String, String> {
    if let Some(data) = stored.strip_prefix(PREFIX) {
        let key = key.ok_or("архив зашифрован, нужен пароль")?;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(data)
            .map_err(|e| e.to_string())?;
        if bytes.len() < 24 {
            return Err("повреждённая запись архива".into());
        }
        let (nonce, sealed) = bytes.split_at(24);
        let plain = XChaCha20Poly1305::new(key.into())
            .decrypt(XNonce::from_slice(nonce), sealed)
            .map_err(|_| "неверный ключ архива".to_owned())?;
        String::from_utf8(plain).map_err(|e| e.to_string())
    } else if let Some(text) = stored.strip_prefix(PLAIN_PREFIX) {
        Ok(text.to_owned())
    } else {
        Ok(stored.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sealed_text_opens_with_its_key_only() {
        let salt = new_salt();
        let key = derive("correct horse", &salt).unwrap();
        let other = derive("battery staple", &salt).unwrap();
        let sealed = seal(&key, "секрет".as_bytes()).unwrap();
        assert!(!sealed.contains("секрет"));
        assert_eq!(open(Some(&key), &sealed).unwrap(), "секрет");
        assert!(open(Some(&other), &sealed).is_err());
        assert!(open(None, &sealed).is_err());
        assert_eq!(
            open(None, "plain").unwrap(),
            "plain",
            "old unencrypted rows"
        );
        assert_ne!(
            seal(&key, b"x").unwrap(),
            seal(&key, b"x").unwrap(),
            "fresh nonce each time"
        );
    }

    #[test]
    fn plain_text_starting_with_the_cipher_marker_is_unambiguous() {
        // A message that happens to start with "enc:" must not be mistaken
        // for ciphertext once it is written with `mark_plain`/`store`.
        let marked = mark_plain("enc:это не шифротекст");
        assert_eq!(store(None, "enc:это не шифротекст").unwrap(), marked);
        assert_eq!(open(None, &marked).unwrap(), "enc:это не шифротекст");
        let key = derive("пароль", &new_salt()).unwrap();
        // Even with a key at hand, a plain-marked row is never decrypted.
        assert_eq!(open(Some(&key), &marked).unwrap(), "enc:это не шифротекст");
    }

    #[test]
    fn same_password_same_key_and_check() {
        let salt = new_salt();
        let a = derive("пароль", &salt).unwrap();
        assert_eq!(a, derive("пароль", &salt).unwrap());
        assert_ne!(a, derive("пароль", &new_salt()).unwrap(), "salt matters");
        assert_eq!(
            check_value(&a),
            check_value(&derive("пароль", &salt).unwrap())
        );
        assert_ne!(
            check_value(&a),
            check_value(&derive("Пароль", &salt).unwrap())
        );
        assert_eq!(tdlib_key(None), "");
        assert_eq!(tdlib_key(Some(&[0; 32])).len(), 44);
    }
}
