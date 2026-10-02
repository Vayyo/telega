use iced::Task;
use tdlib_rs::enums::MessageSender;
use tdlib_rs::types::Message;

use super::{App, Auth, Msg};
use crate::plugins::{self, Action, HostCmd, HostEvent, Origin, Permission};
use crate::settings;
use crate::td;

/// How many journal lines are kept per plugin.
const PLUGIN_LOG_LINES: usize = 50;

impl App {
    pub(super) fn on_plugin(&mut self, event: HostEvent) -> Task<Msg> {
        match event {
            HostEvent::Ready(host) => {
                host.send(HostCmd::Configure(self.settings.plugins.clone()));
                self.plugin_host = Some(host);
                self.announce_account();
            }
            HostEvent::Plugins(infos) => {
                // A plugin file that now asks for more than was granted is
                // turned off until the user enables it again.
                let mut revoked = Vec::new();
                for info in &infos {
                    if let Some(config) = self.settings.plugins.get_mut(&info.id)
                        && config.enabled
                        && info
                            .permissions
                            .iter()
                            .any(|p| !config.granted.iter().any(|g| g == p.name()))
                    {
                        config.enabled = false;
                        revoked.push(info.id.clone());
                    }
                }
                self.plugin_infos = infos;
                if !revoked.is_empty() {
                    for id in revoked {
                        self.plugin_log(
                            id,
                            "плагин запросил новые права и выключен — проверьте и включите снова"
                                .into(),
                        );
                    }
                    self.save_plugin_settings();
                }
            }
            HostEvent::Log(id, line) => self.plugin_log(id, line),
            HostEvent::Action(origin, id, action) => {
                return self.run_plugin_action(origin, id, action);
            }
        }
        Task::none()
    }

    /// Never derive an action's origin from the consumer session: the action
    /// may have spent time in the worker and UI queues before arriving here.
    fn plugin_origin(&self) -> Option<Origin> {
        match (&self.session.auth, self.session.my_id) {
            (Auth::Ready, Some(account_id)) => Some(Origin {
                client_id: self.session.client_id,
                account_id,
            }),
            _ => None,
        }
    }

    fn plugin_allows(&self, origin: Origin, id: &str, action: &Action) -> bool {
        if self.plugin_origin() != Some(origin) {
            return false;
        }
        let permission = match action {
            Action::Delete { .. } => Permission::DeleteOwn,
            Action::Send { .. } => Permission::Send,
        };
        self.plugin_infos
            .iter()
            .find(|info| info.id == id)
            .is_some_and(|info| {
                info.error.is_none()
                    && info.permissions.contains(&permission)
                    && self.settings.plugins.get(id).is_some_and(|config| {
                        config.enabled
                            && !config.dry_run
                            && info
                                .permissions
                                .iter()
                                .all(|p| config.granted.iter().any(|g| g == p.name()))
                    })
            })
    }

    /// Executes a plugin request through the client's own paths.
    fn run_plugin_action(&mut self, origin: Origin, id: String, action: Action) -> Task<Msg> {
        if !self.plugin_allows(origin, &id, &action) {
            return Task::none();
        }
        let original = action.clone();
        match action {
            Action::Delete {
                chat_id,
                ids,
                revoke,
            } => {
                // Only the user's own messages, checked with TDLib first.
                Task::perform(
                    td::own_messages(origin.client_id, chat_id, ids),
                    move |r| match r {
                        Ok(own) => Msg::PluginDeleteOwn(
                            origin,
                            id.clone(),
                            chat_id,
                            own,
                            revoke,
                            original.clone(),
                        ),
                        Err(e) => Msg::PluginDone(origin, id.clone(), original.clone(), Err(e)),
                    },
                )
            }
            Action::Send { chat_id, text } => {
                Task::perform(td::send_plain(origin.client_id, chat_id, text), move |r| {
                    Msg::PluginDone(
                        origin,
                        id,
                        original,
                        r.map(|()| format!("отправлено в чат {chat_id}")),
                    )
                })
            }
        }
    }

    /// A completed request may belong to a replaced TDLib client. Such
    /// completions must not log, pause or requeue anything in the new session.
    pub(super) fn plugin_done(
        &mut self,
        origin: Origin,
        id: String,
        action: Action,
        result: Result<String, String>,
    ) -> Task<Msg> {
        if !self.plugin_allows(origin, &id, &action) {
            return Task::none();
        }
        let line = match result {
            Ok(line) => line,
            Err(error) => {
                if let (Some(secs), Some(host)) = (td::flood_wait(&error), &self.plugin_host) {
                    host.send(HostCmd::FloodWait(origin, secs));
                    host.send(HostCmd::Requeue(origin, id.clone(), action));
                }
                format!("ошибка: {error}")
            }
        };
        self.plugin_log(id, line);
        Task::none()
    }

    /// Recheck policy after the asynchronous own-message lookup, before
    /// mutating self_deleted or issuing a TDLib deletion.
    pub(super) fn plugin_delete_own(
        &mut self,
        origin: Origin,
        plugin: String,
        chat_id: i64,
        own: Vec<i64>,
        revoke: bool,
        action: Action,
    ) -> Task<Msg> {
        if !self.plugin_allows(origin, &plugin, &action) {
            return Task::none();
        }
        if own.is_empty() {
            self.plugin_log(plugin, "нечего удалять: сообщения не ваши".into());
            return Task::none();
        }
        self.session
            .self_deleted
            .extend(own.iter().map(|&message| (chat_id, message)));
        let ids = own.clone();
        Task::perform(
            td::delete_messages(origin.client_id, chat_id, own, revoke),
            move |result| {
                Msg::PluginDeleted(
                    origin,
                    plugin.clone(),
                    chat_id,
                    ids.clone(),
                    action.clone(),
                    result,
                )
            },
        )
    }

    pub(super) fn plugin_deleted(
        &mut self,
        origin: Origin,
        plugin: String,
        chat_id: i64,
        ids: Vec<i64>,
        action: Action,
        result: Result<(), String>,
    ) -> Task<Msg> {
        if self.plugin_origin() != Some(origin) {
            return Task::none();
        }
        let line = match result {
            Ok(()) => Ok(format!("удалено {} сообщ. в чате {chat_id}", ids.len())),
            Err(error) => {
                for id in &ids {
                    self.session.self_deleted.remove(&(chat_id, *id));
                }
                Err(error)
            }
        };
        self.plugin_done(origin, plugin, action, line)
    }

    pub(super) fn plugin_log(&mut self, id: String, line: String) {
        let log = self.plugin_logs.entry(id).or_default();
        if log.len() == PLUGIN_LOG_LINES {
            log.pop_front();
        }
        log.push_back(line);
    }

    pub(super) fn plugin_config(
        &mut self,
        id: &str,
        change: impl FnOnce(&mut settings::PluginConfig),
    ) {
        change(self.settings.plugins.entry(id.to_owned()).or_default());
        self.save_plugin_settings();
    }

    pub(super) fn save_plugin_settings(&mut self) {
        self.save_settings();
        if let Some(host) = &self.plugin_host {
            host.send(HostCmd::Configure(self.settings.plugins.clone()));
        }
    }

    /// Plugins act only for a logged-in account.
    pub(super) fn announce_account(&self) {
        if let Some(host) = &self.plugin_host {
            host.send(HostCmd::Account(self.plugin_origin()));
        }
    }

    pub(super) fn plugin_event(&self, event: plugins::Event) {
        if let (Some(host), Some(origin)) = (&self.plugin_host, self.plugin_origin()) {
            host.send(HostCmd::Event(origin, event));
        }
    }
}

pub(super) fn message_event(m: &Message) -> plugins::Event {
    plugins::Event::New {
        chat_id: m.chat_id,
        id: m.id,
        text: td::message_text(&m.content),
        outgoing: m.is_outgoing,
        sender_id: match &m.sender_id {
            MessageSender::User(u) => u.user_id,
            MessageSender::Chat(c) => c.chat_id,
        },
    }
}
