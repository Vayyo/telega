//! Where the client keeps its data: the per-user data directory
//! (`~/.local/share/telega` on Linux, `%APPDATA%\telega` on Windows), or
//! `TELEGA_DATA_DIR` if set.

use std::path::{Path, PathBuf};
use std::sync::LazyLock;

/// Tests never touch the user's data: they get a directory of their own.
#[cfg(test)]
static BASE: LazyLock<PathBuf> =
    LazyLock::new(|| std::env::temp_dir().join(format!("telega-test-data-{}", std::process::id())));

#[cfg(not(test))]
static BASE: LazyLock<PathBuf> = LazyLock::new(|| {
    data_dir_from_env(std::env::var_os("TELEGA_DATA_DIR"))
        .or_else(|| dirs::data_dir().map(|d| d.join("telega")))
        .unwrap_or_else(|| PathBuf::from("telega-data"))
});

/// Serialises the tests that use `avatars()`: one of them removes the
/// directory to prove `store` creates it, and the binary's tests run in
/// parallel against the same data directory (see `paths::BASE`).
#[cfg(test)]
pub static AVATARS_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// An empty `TELEGA_DATA_DIR` means "not set". Taken literally it is a path
/// of zero length, so `db`, `files` and `plugins` land next to the working
/// directory of whoever started the client instead of in the data
/// directory: a launcher that exports the variable without a value would
/// split the client's data in two.
fn data_dir_from_env(value: Option<std::ffi::OsString>) -> Option<PathBuf> {
    value.filter(|v| !v.is_empty()).map(PathBuf::from)
}

/// Creates the data directory, readable only by the user: it holds the
/// session, the message archive and downloaded files. Call once at start.
pub fn init() -> Result<(), String> {
    let error = |e: std::io::Error| format!("папка данных {}: {e}", base().display());
    std::fs::create_dir_all(base()).map_err(error)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(base(), std::fs::Permissions::from_mode(0o700)).map_err(error)?;
    }
    Ok(())
}

pub fn base() -> &'static Path {
    &BASE
}

/// Data of one account slot. The first account lives in the data directory
/// itself, where it always was; further ones in `accounts/<slot>`.
pub fn account_dir(slot: u32) -> PathBuf {
    if slot == 0 {
        base().to_owned()
    } else {
        base().join("accounts").join(slot.to_string())
    }
}

pub fn db_dir(slot: u32) -> PathBuf {
    account_dir(slot).join("db")
}

pub fn files_dir(slot: u32) -> PathBuf {
    account_dir(slot).join("files")
}

/// Removes the TDLib data of a slot (session, cache, downloads). Only for a
/// closed client.
pub fn remove_account(slot: u32) -> std::io::Result<()> {
    let remove = |dir: PathBuf| match std::fs::remove_dir_all(dir) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    };
    if slot == 0 {
        remove(db_dir(0))?;
        remove(files_dir(0))
    } else {
        remove(account_dir(slot))
    }
}

pub fn settings() -> PathBuf {
    base().join("settings.json")
}

pub fn archive(account_id: i64) -> PathBuf {
    base().join(format!("archive-{account_id}.sqlite"))
}

/// Per-account plugin database (see `plugins::host::Host`), distinct from
/// `plugins()` which holds the plugin scripts shared by every account.
pub fn plugins_db(account_id: i64) -> PathBuf {
    base().join(format!("plugins-{account_id}.sqlite"))
}

pub fn plugins() -> PathBuf {
    base().join("plugins")
}

pub fn tmp() -> PathBuf {
    base().join("tmp")
}

/// Content-addressed cache of decoded profile pictures (see
/// `app::avatar_cache`): at the top of the data directory, shared by every
/// account, so a forgotten account keeps nothing of it and `remove_account`
/// does not touch it. Kept in the open, like the files TDLib downloads.
pub fn avatars() -> PathBuf {
    base().join("avatars")
}

/// Removes an account's archive of deleted messages and its plugin
/// database, including WAL/SHM side files. Not covered by `remove_account`,
/// which only clears the TDLib slot: both live at the top of the data
/// directory, keyed by TDLib user id rather than by slot, so a forgotten
/// account must drop them explicitly, or a later login with the same user
/// id reopens the previous account's leftovers (plaintext or, with the
/// wrong key, an unreadable archive).
pub fn remove_account_data(user_id: i64) -> std::io::Result<()> {
    let remove_with_side_files = |path: PathBuf| -> std::io::Result<()> {
        for suffix in ["", "-wal", "-shm"] {
            let mut target = path.clone().into_os_string();
            target.push(suffix);
            match std::fs::remove_file(&target) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
                _ => {}
            }
        }
        Ok(())
    };
    remove_with_side_files(archive(user_id))?;
    remove_with_side_files(plugins_db(user_id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_data_dir_means_not_set() {
        assert_eq!(data_dir_from_env(None), None);
        assert_eq!(data_dir_from_env(Some("".into())), None);
        assert_eq!(
            data_dir_from_env(Some("/tmp/telega-data".into())),
            Some(PathBuf::from("/tmp/telega-data"))
        );
    }
}
