//! Client settings, stored as JSON next to the TDLib data. Settings belong to
//! the client, not to an account, so they survive log out.

use std::collections::{BTreeMap, BTreeSet};
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
    /// Per-account downloaded files and separate shared decoded-avatar cache policy.
    pub cache: CacheLimits,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CacheLimits {
    pub bytes: u64,
    pub days: u64,
}

impl Default for CacheLimits {
    fn default() -> Self {
        Self {
            bytes: 10 * 1024 * 1024 * 1024,
            days: 90,
        }
    }
}

impl CacheLimits {
    pub const GIB_MAX: u32 = 100;
    pub const MONTHS_MAX: u32 = 12;

    pub fn from_sliders(gib: f32, months: f32) -> Self {
        let normalized = |value: f32, default: f32, max: u32| {
            if value.is_nan() {
                default as u64
            } else {
                value.round().clamp(1.0, max as f32) as u64
            }
        };
        Self {
            bytes: normalized(gib, 10.0, Self::GIB_MAX) * 1024 * 1024 * 1024,
            days: normalized(months, 3.0, Self::MONTHS_MAX) * 30,
        }
    }

    pub fn gib(self) -> f32 {
        const GIB: u64 = 1024 * 1024 * 1024;
        (self.bytes.saturating_add(GIB / 2) / GIB).clamp(1, Self::GIB_MAX as u64) as f32
    }

    pub fn months(self) -> f32 {
        (self.days.saturating_add(15) / 30).clamp(1, Self::MONTHS_MAX as u64) as f32
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Account {
    pub slot: u32,
    /// Known once logged in; `None` while the login is not finished.
    pub user_id: Option<i64>,
    pub name: String,
    /// Content digest of the cached, decoded own profile photo.
    pub avatar_digest: Option<[u8; 32]>,
    /// Client password: salt of the key and a check value of it (the
    /// password itself is never stored).
    pub lock_salt: Option<String>,
    pub lock_check: Option<String>,
    /// Forward-only key transition; both candidate credentials survive a crash.
    pub pending_key_change: Option<PendingKeyChange>,
    /// Per chat and message, independent of the TDLib file identifier.
    pub video_volumes: BTreeMap<i64, BTreeMap<i64, VideoVolume>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingKeyChange {
    pub old_salt: Option<String>,
    pub old_check: Option<String>,
    pub new_salt: Option<String>,
    pub new_check: Option<String>,
    /// Only password-to-password changes need cross-sealed keys. Never plaintext.
    pub new_sealed_by_old: Option<String>,
    pub old_sealed_by_new: Option<String>,
    /// Permanent server deletions received while archive rekey owns the handle.
    /// Only ids are persisted; no message text is written to settings.
    #[serde(default)]
    pub deleted_ids: BTreeMap<i64, BTreeSet<i64>>,
    #[serde(default)]
    pub purged_ids: BTreeMap<i64, BTreeSet<i64>>,
}

impl PendingKeyChange {
    pub fn new(
        old_salt: Option<String>,
        old_check: Option<String>,
        new_salt: Option<String>,
        new_check: Option<String>,
        old_key: Option<&crate::lock::Key>,
        new_key: Option<&crate::lock::Key>,
    ) -> Result<Self, String> {
        if old_salt.is_some() != old_check.is_some()
            || new_salt.is_some() != new_check.is_some()
            || old_salt.is_some() != old_key.is_some()
            || new_salt.is_some() != new_key.is_some()
            || old_key.is_some_and(|k| Some(crate::lock::check_value(k)) != old_check)
            || new_key.is_some_and(|k| Some(crate::lock::check_value(k)) != new_check)
        {
            return Err("несогласованные ключи перехода".into());
        }
        let (new_sealed_by_old, old_sealed_by_new) = match (old_key, new_key) {
            (Some(old), Some(new)) => (
                Some(crate::lock::seal(old, &new[..])?),
                Some(crate::lock::seal(new, &old[..])?),
            ),
            _ => (None, None),
        };
        Ok(Self {
            old_salt,
            old_check,
            new_salt,
            new_check,
            new_sealed_by_old,
            old_sealed_by_new,
            deleted_ids: BTreeMap::new(),
            purged_ids: BTreeMap::new(),
        })
    }

    /// The unlock password may be either side of the transition. Returns both
    /// candidates without ever writing either plaintext key to settings.
    pub fn keys(
        &self,
        password: &str,
    ) -> Result<(Option<crate::lock::Key>, Option<crate::lock::Key>), String> {
        let derive = |salt: &Option<String>, check: &Option<String>| -> Option<crate::lock::Key> {
            let key = crate::lock::derive(password, salt.as_deref()?).ok()?;
            (Some(crate::lock::check_value(&key)) == *check).then_some(key)
        };
        if let Some(old) = derive(&self.old_salt, &self.old_check) {
            let new = match &self.new_sealed_by_old {
                Some(sealed) => Some(crate::lock::open_key(&old, sealed)?),
                None => None,
            };
            if new.as_ref().map(crate::lock::check_value) != self.new_check {
                return Err("повреждён журнал перехода".into());
            }
            return Ok((Some(old), new));
        }
        if let Some(new) = derive(&self.new_salt, &self.new_check) {
            let old = match &self.old_sealed_by_new {
                Some(sealed) => Some(crate::lock::open_key(&new, sealed)?),
                None => None,
            };
            if old.as_ref().map(crate::lock::check_value) != self.old_check {
                return Err("повреждён журнал перехода".into());
            }
            return Ok((old, Some(new)));
        }
        Err("неверный пароль".into())
    }
}

/// Playback level and the last audible level (for unmuting after a restart).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct VideoVolume {
    pub volume: f32,
    pub unmuted: f32,
}

impl Default for VideoVolume {
    fn default() -> Self {
        Self {
            volume: 1.0,
            unmuted: 1.0,
        }
    }
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
            cache: CacheLimits::default(),
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

    /// Writes and syncs a temporary file, renames, then syncs its parent.
    /// A failure after rename is ambiguous: callers retain pending recovery.
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
        std::fs::rename(tmp, dest)?;
        std::fs::File::open(dest.parent().unwrap_or_else(|| Path::new(".")))?.sync_all()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialized_default_settings_include_cache_policy() {
        assert_eq!(
            serde_json::to_value(Settings::default()).unwrap()["cache"],
            serde_json::json!({"bytes": 10_737_418_240_u64, "days": 90}),
        );
    }

    #[test]
    fn cache_defaults_to_ten_gib_and_ninety_days() {
        assert_eq!(
            Settings::default().cache,
            CacheLimits {
                bytes: 10 * 1024 * 1024 * 1024,
                days: 90,
            }
        );
    }

    #[test]
    fn nondefault_cache_limits_survive_settings_save_and_load() {
        let dir = std::env::temp_dir().join(format!(
            "telega-settings-cache-roundtrip-{}",
            std::process::id()
        ));
        let path = dir.join("settings.json");
        let settings = Settings {
            cache: CacheLimits {
                bytes: 27 * 1024 * 1024 * 1024,
                days: 150,
            },
            ..Settings::default()
        };
        settings.save(&path).unwrap();
        assert_eq!(Settings::load(&path), Ok(settings));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn older_settings_without_cache_and_empty_settings_use_cache_defaults() {
        let dir = std::env::temp_dir().join(format!(
            "telega-settings-cache-legacy-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");
        std::fs::write(&path, r#"{"notifications":false,"keep_deleted":false}"#).unwrap();
        let loaded = Settings::load(&path).unwrap();
        assert_eq!(loaded.cache, CacheLimits::default());
        assert!(!loaded.notifications);
        assert!(!loaded.keep_deleted);
        std::fs::write(&path, "{}").unwrap();
        assert_eq!(Settings::load(&path).unwrap().cache, CacheLimits::default());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn cache_sliders_convert_months_and_gib_and_clamp_both_ends() {
        let gib: u64 = 1024 * 1024 * 1024;
        assert_eq!(
            CacheLimits::from_sliders(10.0, 3.0),
            CacheLimits {
                bytes: 10 * gib,
                days: 90,
            }
        );
        assert_eq!(
            CacheLimits::from_sliders(0.0, 0.0),
            CacheLimits {
                bytes: gib,
                days: 30,
            }
        );
        assert_eq!(
            CacheLimits::from_sliders(500.0, 500.0),
            CacheLimits {
                bytes: 100 * gib,
                days: 360,
            }
        );
        let selected = CacheLimits::from_sliders(27.0, 5.0);
        assert_eq!((selected.gib(), selected.months()), (27.0, 5.0));
        assert_eq!(
            CacheLimits::from_sliders(selected.gib(), selected.months()),
            selected
        );
    }

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
            cache: CacheLimits::default(),
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
