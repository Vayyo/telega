use iced::Task;
use tdlib_rs::enums::MessageSender;
use tdlib_rs::types::Message;

use super::{App, Auth, Msg};
use crate::plugins::{self, Action, HostCmd, HostEvent};
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
            HostEvent::Action(id, action) => return self.run_plugin_action(id, action),
        }
        Task::none()
    }

    /// Executes a plugin request through the client's own paths.
    fn run_plugin_action(&mut self, id: String, action: Action) -> Task<Msg> {
        // Defense in depth: the host already gates this on `enabled` and
        // granted permissions, but a `Configure` round trip in flight must
        // not let a stale action through here either.
        let allowed = self
            .plugin_infos
            .iter()
            .find(|i| i.id == id)
            .is_some_and(|info| {
                self.settings.plugins.get(&id).is_some_and(|c| {
                    c.enabled
                        && info
                            .permissions
                            .iter()
                            .all(|p| c.granted.iter().any(|g| g == p.name()))
                })
            });
        if !allowed {
            return Task::none();
        }
        let client_id = self.session.client_id;
        let original = action.clone();
        match action {
            Action::Delete {
                chat_id,
                ids,
                revoke,
            } => {
                // Only the user's own messages, checked with TDLib first.
                Task::perform(
                    td::own_messages(client_id, chat_id, ids),
                    move |r| match r {
                        Ok(own) => {
                            Msg::PluginDeleteOwn(id.clone(), chat_id, own, revoke, original.clone())
                        }
                        Err(e) => Msg::PluginDone(id.clone(), original.clone(), Err(e)),
                    },
                )
            }
            Action::Send { chat_id, text } => {
                Task::perform(td::send_plain(client_id, chat_id, text), move |r| {
                    Msg::PluginDone(
                        id,
                        original,
                        r.map(|()| format!("отправлено в чат {chat_id}")),
                    )
                })
            }
        }
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
        if let (Some(host), Some(id), Auth::Ready) =
            (&self.plugin_host, self.session.my_id, &self.session.auth)
        {
            host.send(HostCmd::Account(Some(id)));
        }
    }

    pub(super) fn plugin_event(&self, event: plugins::Event) {
        if let Some(host) = &self.plugin_host {
            host.send(HostCmd::Event(event));
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
