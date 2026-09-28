//! Lua (Luau) plugins. Every plugin runs in its own sandboxed VM on a
//! dedicated thread; it reacts to message events and asks the client to
//! perform actions, which the client executes through its normal paths.
//!
//! Safety rules enforced here, not by plugin authors:
//! - capabilities are declared in the manifest and checked on every call;
//! - no network, file or process access (Luau sandbox, no `io`);
//! - every callback has a time budget and every VM a memory limit;
//! - actions are paced globally and pause on Telegram flood errors;
//! - "delete" only ever removes the user's own messages (checked by the client);
//! - a freshly enabled plugin starts in dry run mode.

pub mod api;
mod host;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use iced::futures::{SinkExt, Stream};
use serde::Serialize;

use crate::settings::PluginConfig;
pub use host::Host;

/// Plugins shipped with the client: (id, source).
pub const BUILTIN: &[(&str, &str)] = &[("autodelete", include_str!("builtin/autodelete.lua"))];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Permission {
    Read,
    DeleteOwn,
    Send,
}

impl Permission {
    fn parse(s: &str) -> Option<Self> {
        match s {
            "read" => Some(Self::Read),
            "delete_own" => Some(Self::DeleteOwn),
            "send" => Some(Self::Send),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::DeleteOwn => "delete_own",
            Self::Send => "send",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Read => "читает ваши сообщения",
            Self::DeleteOwn => "может удалять ваши сообщения",
            Self::Send => "может отправлять сообщения от вашего имени",
        }
    }

    /// Irreversible actions: the plugin card warns and offers dry run.
    pub fn destructive(self) -> bool {
        matches!(self, Self::DeleteOwn | Self::Send)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingKind {
    Number,
    Text,
    Bool,
    /// List of chat ids.
    Chats,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SettingSpec {
    pub key: String,
    pub label: String,
    pub kind: SettingKind,
    pub default: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PluginInfo {
    pub id: String,
    pub name: String,
    pub description: String,
    pub permissions: Vec<Permission>,
    pub settings: Vec<SettingSpec>,
    pub builtin: bool,
    /// Plugin failed to load; it is shown but cannot be enabled.
    pub error: Option<String>,
}

impl PluginInfo {
    pub fn destructive(&self) -> bool {
        self.permissions.iter().any(|p| p.destructive())
    }

    /// Value of a setting from `config`, falling back to the plugin default.
    pub fn value<'a>(&'a self, config: &'a PluginConfig, key: &str) -> &'a serde_json::Value {
        config
            .values
            .get(key)
            .or_else(|| {
                self.settings
                    .iter()
                    .find(|s| s.key == key)
                    .map(|s| &s.default)
            })
            .unwrap_or(&serde_json::Value::Null)
    }
}

/// Message events delivered to plugins with the "read" permission.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub enum Event {
    New {
        chat_id: i64,
        id: i64,
        text: String,
        outgoing: bool,
        /// User or chat id of the sender.
        sender_id: i64,
    },
    Edited {
        chat_id: i64,
        id: i64,
        text: String,
    },
    Deleted {
        chat_id: i64,
        ids: Vec<i64>,
    },
}

impl Event {
    fn name(&self) -> &'static str {
        match self {
            Self::New { .. } => "message_new",
            Self::Edited { .. } => "message_edited",
            Self::Deleted { .. } => "message_deleted",
        }
    }
}

/// Requests from plugins, executed by the client.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    Delete {
        chat_id: i64,
        ids: Vec<i64>,
        revoke: bool,
    },
    Send {
        chat_id: i64,
        text: String,
    },
}

/// Commands from the client to the plugin thread.
#[derive(Debug, Clone)]
pub enum HostCmd {
    Configure(BTreeMap<String, PluginConfig>),
    /// Logged-in account (plugin data is per account); `None` pauses tasks.
    Account(Option<i64>),
    Event(Event),
    /// Telegram asked to wait before further requests.
    FloodWait(u64),
    /// An action failed (e.g. Telegram's flood wait); put it back at the
    /// front of the queue instead of losing it.
    Requeue(String, Action),
}

/// Output of the plugin thread.
#[derive(Debug, Clone)]
pub enum HostEvent {
    Ready(HostHandle),
    Plugins(Vec<PluginInfo>),
    Log(String, String),
    Action(String, Action),
}

#[derive(Clone)]
pub struct HostHandle(mpsc::Sender<HostCmd>);

impl HostHandle {
    pub fn send(&self, cmd: HostCmd) {
        // The thread only stops with the program.
        let _ = self.0.send(cmd);
    }

    #[cfg(test)]
    pub fn for_test() -> (Self, mpsc::Receiver<HostCmd>) {
        let (tx, rx) = mpsc::channel();
        (Self(tx), rx)
    }
}

impl std::fmt::Debug for HostHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("HostHandle")
    }
}

/// Built-in plugins plus `*.lua` files from the user directory. Also writes
/// the `telega.d.lua` stubs there for editors.
fn sources(dir: &Path) -> Vec<(String, String, bool)> {
    let mut out: Vec<(String, String, bool)> = BUILTIN
        .iter()
        .map(|(id, src)| ((*id).to_owned(), (*src).to_owned(), true))
        .collect();
    let _ = std::fs::create_dir_all(dir);
    let _ = std::fs::write(dir.join("telega.d.lua"), api::stubs());
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    let mut files: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "lua"))
        .filter(|p| p.file_name().is_some_and(|n| n != "telega.d.lua"))
        .collect();
    files.sort();
    for path in files {
        let (Some(id), Ok(src)) = (
            path.file_stem().and_then(|s| s.to_str()),
            std::fs::read_to_string(&path),
        ) else {
            continue;
        };
        out.push((id.to_owned(), src, false));
    }
    out
}

fn now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64())
}

/// Stream for an iced subscription: starts the plugin thread and yields its
/// events, the first one being the handle to send commands.
pub fn run() -> impl Stream<Item = HostEvent> {
    iced::stream::channel(128, async |mut out| {
        let (cmd_tx, cmd_rx) = mpsc::channel();
        let (ev_tx, mut ev_rx) = tokio::sync::mpsc::unbounded_channel();
        if out
            .send(HostEvent::Ready(HostHandle(cmd_tx)))
            .await
            .is_err()
        {
            return;
        }
        std::thread::Builder::new()
            .name("plugins".into())
            .spawn(move || {
                let mut host = Host::new(sources(&crate::paths::plugins()), now());
                loop {
                    for event in host.drain() {
                        if ev_tx.send(event).is_err() {
                            return;
                        }
                    }
                    let t = now();
                    let wait = host.next_wakeup(t).map_or(Duration::from_secs(3600), |at| {
                        Duration::from_secs_f64((at - t).clamp(0.0, 3600.0))
                    });
                    match cmd_rx.recv_timeout(wait) {
                        Ok(cmd) => host.handle(cmd, now()),
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                        Err(mpsc::RecvTimeoutError::Disconnected) => return,
                    }
                    host.tick(now());
                }
            })
            .expect("spawn plugin thread");
        while let Some(event) = ev_rx.recv().await {
            if out.send(event).await.is_err() {
                return;
            }
        }
    })
}
