//! Client settings, stored as JSON next to the TDLib data. Settings belong to
//! the client, not to an account, so they survive log out.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Archive seen messages and keep showing them after deletion. When off,
    /// nothing new is archived and already archived deleted messages are
    /// hidden (not erased) until the setting is turned back on.
    pub keep_deleted: bool,
    /// Per plugin id. A plugin without an entry is off.
    pub plugins: BTreeMap<String, PluginConfig>,
    /// Open tabs of the main window, per account id.
    pub tabs: BTreeMap<String, TabsState>,
    /// Account IDs whose archive entry lives in the account menu, not the main list.
    pub collapsed_archives: BTreeMap<i64, bool>,
    /// Desktop notifications about new messages.
    pub notifications: bool,
    /// Accounts added to the client, each in its own data slot.
    pub accounts: Vec<Account>,
    /// Slot the client opens.
    pub active_account: u32,
    /// Color scheme and accent.
    pub look: crate::app::look::LookSettings,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Account {
    pub slot: u32,
    /// Known once logged in; `None` while the login is not finished.
    pub user_id: Option<i64>,
    pub name: String,
    /// Client password: salt of the key and a check value of it (the
    /// password itself is never stored).
    pub lock_salt: Option<String>,
    pub lock_check: Option<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            keep_deleted: true,
            plugins: BTreeMap::new(),
            tabs: BTreeMap::new(),
            collapsed_archives: BTreeMap::new(),
            notifications: true,
            accounts: Vec::new(),
            active_account: 0,
            look: Default::default(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TabsState {
    /// Tab order, pinned tabs first.
    pub chats: Vec<i64>,
    pub pinned: Vec<i64>,
    pub active: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PluginConfig {
    pub enabled: bool,
    /// Destructive actions are only logged, not performed. On by default so a
    /// freshly enabled plugin shows what it would do first.
    pub dry_run: bool,
    /// Values of the plugin's own settings; missing ones use its defaults.
    pub values: BTreeMap<String, serde_json::Value>,
    /// Permissions the user agreed to when enabling it. A plugin file that
    /// later asks for more is turned off until enabled again.
    pub granted: Vec<String>,
}

impl Default for PluginConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            dry_run: true,
            values: BTreeMap::new(),
            granted: Vec::new(),
        }
    }
}

impl Settings {
    /// Missing file means defaults; a broken file is reported, not overwritten
    /// silently.
    pub fn load(path: &Path) -> Result<Self, String> {
        match std::fs::read_to_string(path) {
            Ok(json) => serde_json::from_str(&json).map_err(|e| format!("настройки: {e}")),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(format!("настройки: {e}")),
        }
    }

    /// Writes via a temporary file, `fsync`s it, then renames it over
    /// `path`: a crash or a full disk leaves the previous file intact
    /// instead of a truncated or half-written one. `lock_salt`/`lock_check`
    /// live only here, so a corrupt file would lock the account out for
    /// good.
    pub fn save(&self, path: &Path) -> Result<(), String> {
        let dir = path.parent().unwrap_or_else(|| Path::new("."));
        std::fs::create_dir_all(dir).map_err(|e| format!("настройки: {e}"))?;
        let json = serde_json::to_string_pretty(self).expect("settings serialize");
        let mut tmp_name = path.file_name().unwrap_or_default().to_owned();
        tmp_name.push(".tmp");
        let tmp: PathBuf = dir.join(tmp_name);
        Self::write_then_rename(&tmp, path, json.as_bytes()).map_err(|e| format!("настройки: {e}"))
    }

    fn write_then_rename(tmp: &Path, dest: &Path, data: &[u8]) -> std::io::Result<()> {
        let mut file = {
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                std::fs::OpenOptions::new()
                    .write(true)
                    .create(true)
                    .truncate(true)
                    .mode(0o600)
                    .open(tmp)?
            }
            #[cfg(not(unix))]
            {
                std::fs::File::create(tmp)?
            }
        };
        file.write_all(data)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(tmp, dest)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saved_settings_load_back_and_missing_file_gives_defaults() {
        let dir = std::env::temp_dir().join(format!("telega-settings-{}", std::process::id()));
        let path = dir.join("settings.json");
        assert_eq!(Settings::load(&path), Ok(Settings::default()));

        let off = Settings {
            keep_deleted: false,
            plugins: [(
                "autodelete".to_owned(),
                PluginConfig {
                    enabled: true,
                    dry_run: false,
                    values: [("minutes".to_owned(), serde_json::json!(10))].into(),
                    granted: vec!["read".into()],
                },
            )]
            .into(),
            tabs: [(
                "42".to_owned(),
                TabsState {
                    chats: vec![1, 2],
                    pinned: vec![1],
                    active: Some(2),
                },
            )]
            .into(),
            collapsed_archives: [(42, true)].into(),
            notifications: false,
            accounts: vec![Account {
                slot: 1,
                user_id: Some(42),
                name: "Alice".into(),
                ..Default::default()
            }],
            active_account: 1,
            look: crate::app::look::LookSettings {
                scheme: crate::app::look::Scheme::Graphite,
                accent: crate::app::look::Accent::Violet,
            },
        };
        off.save(&path).unwrap();
        assert_eq!(Settings::load(&path), Ok(off.clone()));
        // A pre-placement settings file remains readable with the visible default.
        let mut legacy = serde_json::to_value(&off).unwrap();
        legacy.as_object_mut().unwrap().remove("collapsed_archives");
        std::fs::write(&path, serde_json::to_vec(&legacy).unwrap()).unwrap();
        let mut expected = off.clone();
        expected.collapsed_archives.clear();
        assert_eq!(Settings::load(&path), Ok(expected));

        // Fields added in later versions fall back to defaults.
        std::fs::write(&path, "{}").unwrap();
        assert_eq!(Settings::load(&path), Ok(Settings::default()));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    #[cfg(unix)]
    fn atomic_save_leaves_the_old_file_untouched_on_failure() {
        use std::os::unix::fs::PermissionsExt;

        let dir =
            std::env::temp_dir().join(format!("telega-settings-atomic-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");
        let good = Settings {
            notifications: false,
            ..Settings::default()
        };
        good.save(&path).unwrap();
        let before = std::fs::read_to_string(&path).unwrap();

        // No write permission on the directory: the temporary file cannot be
        // created, so `save` must fail before ever touching `path`.
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o500)).unwrap();
        let other = Settings {
            notifications: true,
            ..Settings::default()
        };
        let result = other.save(&path);
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();

        assert!(result.is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
