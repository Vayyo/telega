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

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc};
use std::time::Duration;

use parking_lot::Mutex;

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

/// The TDLib client is unique for this process; the account id also isolates
/// plugin data when a user signs in again with a new client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Origin {
    pub client_id: i32,
    pub account_id: i64,
}

/// Commands from the client to the plugin thread.
#[derive(Debug, Clone)]
pub enum HostCmd {
    Configure(BTreeMap<String, PluginConfig>),
    /// Logged-in account (plugin data is per account); `None` pauses tasks.
    Account(Option<Origin>),
    Event(Origin, Event),
    /// Telegram asked to wait before further requests from this session.
    FloodWait(Origin, u64),
    /// A failed action retains the origin of the request that produced it.
    Requeue(Origin, String, Action),
}

/// Output of the plugin thread.
#[derive(Debug, Clone)]
pub enum HostEvent {
    Ready(HostHandle),
    Plugins(Vec<PluginInfo>),
    Log(String, String),
    Action(Origin, String, Action),
}

/// Configuration and account are latest-value mailboxes, never dropped by
/// message traffic. Disabling a plugin also retains cancellation intent if
/// it is re-enabled before the worker can read either configuration.
#[derive(Default)]
struct Controls {
    account: Option<Option<Origin>>,
    config: Option<BTreeMap<String, PluginConfig>>,
    last_enabled: BTreeSet<String>,
    pending_disabled: BTreeSet<String>,
    disable_all: bool,
    staged_disable: bool,
    flood_wait: Option<(Origin, u64)>,
}

const MAX_PENDING_DISABLED: usize = 256;

struct Transport {
    controls: Arc<Mutex<Controls>>,
    events: mpsc::Receiver<(Origin, Event)>,
    requeues: mpsc::Receiver<(Origin, String, Action)>,
    wake: mpsc::Receiver<()>,
}

impl Transport {
    fn next(&self) -> Option<HostCmd> {
        {
            let mut controls = self.controls.lock();
            if let Some(config) = controls.config.as_ref() {
                // The ordinary latest-value mailbox must not erase a brief
                // off/on switch: the intermediate policy cancels scheduled
                // tasks before the latest enabled policy can run them.
                if controls.disable_all {
                    controls.disable_all = false;
                    controls.pending_disabled.clear();
                    controls.staged_disable = true;
                    return Some(HostCmd::Configure(BTreeMap::new()));
                }
                if controls
                    .pending_disabled
                    .iter()
                    .any(|id| config.get(id).is_some_and(|plugin| plugin.enabled))
                {
                    let mut disabled = (*config).clone();
                    for id in std::mem::take(&mut controls.pending_disabled) {
                        if let Some(plugin) = disabled.get_mut(&id) {
                            plugin.enabled = false;
                        }
                    }
                    controls.staged_disable = true;
                    return Some(HostCmd::Configure(disabled));
                }
                controls.pending_disabled.clear();
            }
            // If an account change was also pending, open its database while
            // the temporary disabled policy is active so its old tasks are
            // purged before the final enabled configuration takes effect.
            if controls.staged_disable {
                controls.staged_disable = false;
                if let Some(account) = controls.account.take() {
                    return Some(HostCmd::Account(account));
                }
            }
            if let Some(config) = controls.config.take() {
                return Some(HostCmd::Configure(config));
            }
            if let Some(account) = controls.account.take() {
                return Some(HostCmd::Account(account));
            }
            if let Some((origin, secs)) = controls.flood_wait.take() {
                return Some(HostCmd::FloodWait(origin, secs));
            }
        }
        if let Ok((origin, id, action)) = self.requeues.try_recv() {
            return Some(HostCmd::Requeue(origin, id, action));
        }
        self.events
            .try_recv()
            .ok()
            .map(|(origin, event)| HostCmd::Event(origin, event))
    }
}

#[derive(Clone)]
pub struct HostHandle {
    controls: Arc<Mutex<Controls>>,
    events: mpsc::SyncSender<(Origin, Event)>,
    requeues: mpsc::SyncSender<(Origin, String, Action)>,
    wake: mpsc::SyncSender<()>,
}

impl HostHandle {
    fn channel() -> (Self, Transport) {
        let controls = Arc::new(Mutex::new(Controls::default()));
        let (events, event_rx) = mpsc::sync_channel(256);
        let (requeues, requeue_rx) = mpsc::sync_channel(256);
        let (wake, wake_rx) = mpsc::sync_channel(1);
        (
            Self {
                controls: controls.clone(),
                events,
                requeues,
                wake,
            },
            Transport {
                controls,
                events: event_rx,
                requeues: requeue_rx,
                wake: wake_rx,
            },
        )
    }

    pub fn send(&self, cmd: HostCmd) {
        match cmd {
            HostCmd::Configure(config) => {
                let mut controls = self.controls.lock();
                let enabled: BTreeSet<String> = config
                    .iter()
                    .filter(|(_, plugin)| plugin.enabled)
                    .map(|(id, _)| id.clone())
                    .collect();
                let previous = std::mem::take(&mut controls.last_enabled);
                if !controls.disable_all {
                    for id in previous.difference(&enabled) {
                        if controls.pending_disabled.len() >= MAX_PENDING_DISABLED {
                            // A pathological stream of unique plugin ids
                            // must not grow the control mailbox without bound.
                            controls.pending_disabled.clear();
                            controls.disable_all = true;
                            break;
                        }
                        controls.pending_disabled.insert(id.clone());
                    }
                }
                controls.last_enabled = enabled;
                controls.config = Some(config);
            }
            HostCmd::Account(account) => self.controls.lock().account = Some(account),
            HostCmd::FloodWait(origin, secs) => {
                let mut controls = self.controls.lock();
                let wait = &mut controls.flood_wait;
                *wait = Some(match *wait {
                    Some((previous, old)) if previous == origin => (origin, old.max(secs)),
                    _ => (origin, secs),
                });
            }
            HostCmd::Requeue(origin, id, action) => {
                // Drop newest on overflow: old requests cannot consume more
                // than 256 slots while the worker is busy.
                let _ = self.requeues.try_send((origin, id, action));
            }
            HostCmd::Event(origin, event) => {
                // Remote events never block iced or delay a control change.
                let _ = self.events.try_send((origin, event));
            }
        }
        let _ = self.wake.try_send(());
    }

    #[cfg(test)]
    pub fn for_test() -> (Self, TestReceiver) {
        let (handle, transport) = Self::channel();
        (handle, TestReceiver(transport))
    }
}

/// Inspect the real bounded/coalesced transport without starting Lua.
#[cfg(test)]
pub struct TestReceiver(Transport);

#[cfg(test)]
impl TestReceiver {
    pub fn try_iter(&self) -> impl Iterator<Item = HostCmd> {
        std::iter::from_fn(|| self.0.next())
    }

    /// Exercise the same single worker step used after a wakeup, without
    /// starting a thread or relying on wall-clock scheduling.
    pub fn drive_host(&self, host: &mut Host, at: f64) -> bool {
        drive_host(host, &self.0, at)
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

fn pending_control(transport: &Transport) -> bool {
    let controls = transport.controls.lock();
    controls.config.is_some() || controls.account.is_some() || controls.flood_wait.is_some()
}

/// Handle one ready command before running due tasks/actions. The worker and
/// deterministic transport tests use the exact same dispatch boundary.
fn drive_host(host: &mut Host, transport: &Transport, at: f64) -> bool {
    let Some(cmd) = transport.next() else {
        return false;
    };
    host.handle(cmd, at);
    if !pending_control(transport) {
        host.tick(at);
    }
    true
}

/// Stream for an iced subscription: starts the plugin thread and yields its
/// events, the first one being the handle to send commands.
pub fn run() -> impl Stream<Item = HostEvent> {
    iced::stream::channel(128, async |mut out| {
        let (handle, transport) = HostHandle::channel();
        let (ev_tx, mut ev_rx) = tokio::sync::mpsc::channel(256);
        if out.send(HostEvent::Ready(handle)).await.is_err() {
            return;
        }
        std::thread::Builder::new()
            .name("plugins".into())
            .spawn(move || {
                let mut host = Host::new(sources(&crate::paths::plugins()), now());
                loop {
                    for event in host.drain() {
                        // Drop newest output on overflow; logs (even a burst
                        // of them) cannot stall control processing.
                        if matches!(
                            ev_tx.try_send(event),
                            Err(tokio::sync::mpsc::error::TrySendError::Closed(_))
                        ) {
                            return;
                        }
                    }
                    if drive_host(&mut host, &transport, now()) {
                        continue;
                    }
                    let t = now();
                    let wait = host.next_wakeup(t).map_or(Duration::from_secs(3600), |at| {
                        Duration::from_secs_f64((at - t).clamp(0.0, 3600.0))
                    });
                    match transport.wake.recv_timeout(wait) {
                        Ok(()) => continue,
                        Err(mpsc::RecvTimeoutError::Timeout) => {
                            if pending_control(&transport) {
                                continue;
                            }
                            host.tick(now());
                        }
                        Err(mpsc::RecvTimeoutError::Disconnected) => return,
                    }
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
