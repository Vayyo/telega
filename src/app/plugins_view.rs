//! Plugin section of the settings page and the plugin API reference.

use iced::widget::{
    button, column, container, pick_list, row, scrollable, text, text_input, toggler,
};
use iced::{Element, Fill};

use super::{App, Msg};
use crate::plugins::{self, PluginInfo, SettingKind, SettingSpec};
use crate::settings::PluginConfig;

/// How many journal lines a plugin card shows.
const CARD_LOG_LINES: usize = 5;

#[derive(Debug, Clone, PartialEq)]
struct ChatChoice {
    id: i64,
    title: String,
}

impl std::fmt::Display for ChatChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.title)
    }
}

impl App {
    pub(super) fn view_plugins(&self) -> Element<'_, Msg> {
        let header = row![
            text("Плагины").size(16).width(Fill),
            button("Справка по API")
                .style(button::secondary)
                .on_press(Msg::OpenPluginHelp(true)),
            button("Остановить все")
                .style(button::danger)
                .on_press(Msg::StopAllPlugins),
        ]
        .spacing(6)
        .align_y(iced::Center);

        let mut section = column![
            header,
            text(format!(
                "Свои плагины: файлы .lua в папке {} (подхватываются при запуске клиента). \
                 Там же лежит telega.d.lua для автодополнения в редакторе.",
                crate::paths::plugins().display()
            ))
            .size(12),
        ]
        .spacing(10);
        for info in &self.plugin_infos {
            section = section.push(self.view_plugin_card(info));
        }
        if let Some(log) = self.plugin_logs.get("") {
            for line in log.iter().rev().take(CARD_LOG_LINES).rev() {
                section = section.push(text(line).size(12).style(text::danger));
            }
        }
        section.into()
    }

    fn view_plugin_card<'a>(&'a self, info: &'a PluginInfo) -> Element<'a, Msg> {
        let title = if info.name.is_empty() {
            &info.id
        } else {
            &info.name
        };
        let origin = if info.builtin {
            "встроенный"
        } else {
            "свой"
        };
        let mut card = column![
            row![
                text(title).size(15).width(Fill),
                text(format!("{origin} · {}", info.id)).size(11)
            ]
            .align_y(iced::Center)
        ]
        .spacing(6);

        if let Some(error) = &info.error {
            card = card.push(text(format!("не загружен: {error}")).style(text::danger));
            return container(card)
                .padding(10)
                .width(Fill)
                .style(container::bordered_box)
                .into();
        }
        if !info.description.is_empty() {
            card = card.push(text(&info.description).size(13));
        }
        for permission in &info.permissions {
            let line = text(format!("• {}", permission.label())).size(12);
            card = card.push(if permission.destructive() {
                line.style(text::danger)
            } else {
                line
            });
        }

        let config = self
            .settings
            .plugins
            .get(&info.id)
            .cloned()
            .unwrap_or_default();
        let id = info.id.clone();
        card = card.push(
            toggler(config.enabled)
                .label("Включён")
                .on_toggle(move |on| Msg::PluginToggle(id.clone(), on)),
        );
        if info.destructive() {
            let id = info.id.clone();
            card = card.push(
                toggler(config.dry_run)
                    .label("Пробный прогон: только записывать в журнал, ничего не делать")
                    .on_toggle(move |on| Msg::PluginDryRun(id.clone(), on)),
            );
        }
        for spec in &info.settings {
            card = card.push(self.view_plugin_setting(info, &config, spec));
        }
        if let Some(log) = self.plugin_logs.get(&info.id) {
            let mut journal = column![text("Журнал").size(12)].spacing(2);
            for line in log.iter().rev().take(CARD_LOG_LINES).rev() {
                journal = journal.push(text(line).size(11));
            }
            card = card.push(journal);
        }
        container(card)
            .padding(10)
            .width(Fill)
            .style(container::bordered_box)
            .into()
    }

    fn view_plugin_setting<'a>(
        &'a self,
        info: &'a PluginInfo,
        config: &PluginConfig,
        spec: &'a SettingSpec,
    ) -> Element<'a, Msg> {
        let value = info.value(config, &spec.key).clone();
        let (id, key) = (info.id.clone(), spec.key.clone());
        let set = move |v: serde_json::Value| Msg::PluginSetting(id.clone(), key.clone(), v);
        match spec.kind {
            SettingKind::Bool => toggler(value.as_bool().unwrap_or(false))
                .label(spec.label.as_str())
                .on_toggle(move |b| set(serde_json::json!(b)))
                .into(),
            SettingKind::Number => {
                let draft_key = (info.id.clone(), spec.key.clone());
                // While the field is being edited, show exactly what was
                // typed (so "5." is not immediately snapped back to "5");
                // only committed values ever leave this local draft.
                let shown = self
                    .session
                    .plugin_number_drafts
                    .get(&draft_key)
                    .cloned()
                    .unwrap_or_else(|| value.as_f64().map(|n| n.to_string()).unwrap_or_default());
                let (id, key) = (info.id.clone(), spec.key.clone());
                row![
                    text(&spec.label).size(13).width(Fill),
                    text_input("0", &shown).width(120).on_input(move |s| {
                        // Only digits and a single decimal point are kept as
                        // a draft; anything else is not a number in progress.
                        let plausible = s.chars().all(|c| c.is_ascii_digit() || c == '.')
                            && s.matches('.').count() <= 1;
                        if plausible {
                            Msg::PluginNumberSetting(id.clone(), key.clone(), s)
                        } else {
                            Msg::Ignore
                        }
                    }),
                ]
                .spacing(8)
                .align_y(iced::Center)
                .into()
            }
            SettingKind::Text => row![
                text(&spec.label).size(13).width(Fill),
                text_input("", value.as_str().unwrap_or(""))
                    .width(240)
                    .on_input(move |s| set(serde_json::json!(s))),
            ]
            .spacing(8)
            .align_y(iced::Center)
            .into(),
            SettingKind::Chats => {
                let selected: Vec<i64> = value
                    .as_array()
                    .map(|a| a.iter().filter_map(|v| v.as_i64()).collect())
                    .unwrap_or_default();
                let mut list = column![text(&spec.label).size(13)].spacing(4);
                for &chat_id in &selected {
                    let rest: Vec<i64> =
                        selected.iter().copied().filter(|&c| c != chat_id).collect();
                    let set = set.clone();
                    let title = self
                        .session
                        .chats
                        .get(&chat_id)
                        .map_or_else(|| chat_id.to_string(), |c| c.title.clone());
                    list = list.push(
                        row![
                            text(title).size(13).width(Fill),
                            button(text("×").size(13))
                                .style(button::text)
                                .on_press(set(serde_json::json!(rest))),
                        ]
                        .align_y(iced::Center),
                    );
                }
                let options: Vec<ChatChoice> = self
                    .session
                    .order
                    .iter()
                    .filter(|(_, id)| !selected.contains(id))
                    .filter_map(|&(_, id)| {
                        self.session.chats.get(&id).map(|c| ChatChoice {
                            id,
                            title: c.title.clone(),
                        })
                    })
                    .collect();
                list.push(
                    pick_list(options, None::<ChatChoice>, move |choice| {
                        let mut chats = selected.clone();
                        chats.push(choice.id);
                        set(serde_json::json!(chats))
                    })
                    .placeholder("Добавить чат…"),
                )
                .into()
            }
        }
    }

    pub(super) fn view_plugin_help(&self) -> Element<'_, Msg> {
        let mut list = column![
            row![
                text("API плагинов").size(18).width(Fill),
                button("Назад")
                    .style(button::secondary)
                    .on_press(Msg::OpenPluginHelp(false)),
            ]
            .align_y(iced::Center),
            text(
                "Плагин — файл .lua (язык Luau) в папке плагинов. Он описывает себя через \
                 telega.plugin, подписывается на события и просит клиент выполнить действия. \
                 Доступа к сети, файлам и программам у плагина нет."
            )
            .size(13),
        ]
        .spacing(12)
        .padding(10);
        for api in plugins::api::API {
            list = list.push(
                column![
                    text(api.signature).size(14).font(iced::Font::MONOSPACE),
                    text(api.doc).size(13),
                ]
                .spacing(4),
            );
        }
        list = list.push(text("Пример — встроенный плагин автоудаления:").size(13));
        list = list.push(
            container(
                text(plugins::BUILTIN[0].1)
                    .size(12)
                    .font(iced::Font::MONOSPACE),
            )
            .padding(8)
            .width(Fill)
            .style(container::bordered_box),
        );
        scrollable(list.max_width(760)).height(Fill).into()
    }
}
