//! Panel next to the input field: emoji to insert, recent stickers and the
//! installed sticker sets to send. Loaded when first opened.

use std::collections::HashMap;

use iced::widget::{button, column, container, image, row, scrollable, sensor, text, text_editor};
use iced::{Element, Fill, Task};

use super::media::{FileState, PhotoOwner};
use super::{App, Msg, PaneMsg, WinId, rich};
use crate::td::{self, StickerRef};

/// Emoji offered in the panel, by group.
pub(crate) const EMOJI: [(&str, &str); 6] = [
    (
        "Смайлы",
        "😀 😃 😄 😁 😆 😅 😂 🤣 🙂 🙃 😉 😊 😇 🥰 😍 🤩 😘 😗 😚 😋 😛 😜 🤪 😝 🤑 🤗 🤭 🤫 🤔 🤐 🤨 😐 😑 😶 😏 😒 🙄 😬 😌 😔 😪 🤤 😴 😷 🤒 🤕 🤢 🤮 🥵 🥶 🥴 😵 🤯 🤠 🥳 😎 🤓 🧐 😕 😟 🙁 😮 😯 😲 😳 🥺 😦 😧 😨 😰 😥 😢 😭 😱 😖 😣 😞 😓 😩 😫 🥱 😤 😡 😠 🤬 😈 👿 💀 💩 🤡 👻 👽 🤖",
    ),
    (
        "Жесты",
        "👍 👎 👌 🤌 ✌️ 🤞 🤟 🤘 🤙 👈 👉 👆 👇 ☝️ ✋ 🤚 🖐️ 🖖 👋 👏 🙌 👐 🤲 🤝 🙏 ✍️ 💪 🦾 🫶 👀 🧠",
    ),
    (
        "Сердца",
        "❤️ 🧡 💛 💚 💙 💜 🖤 🤍 🤎 💔 ❣️ 💕 💞 💓 💗 💖 💘 💝 💯 💢 💥 💫 💦 💨 🔥 ✨ ⭐ 🌟",
    ),
    (
        "Природа",
        "🐶 🐱 🐭 🐹 🐰 🦊 🐻 🐼 🐨 🐯 🦁 🐮 🐷 🐸 🐵 🐔 🐧 🐦 🦆 🦉 🐺 🐴 🦄 🐝 🦋 🐌 🐞 🐢 🐍 🐙 🐬 🐳 🌸 🌹 🌻 🌲 🌵 🍀 🍁 🌈 ☀️ 🌙 ⛄ 🌊",
    ),
    (
        "Еда и дела",
        "🍎 🍊 🍋 🍌 🍉 🍇 🍓 🍒 🍑 🥑 🍕 🍔 🍟 🌭 🌮 🍣 🍜 🍩 🍪 🎂 🍫 🍿 ☕ 🍵 🍺 🍷 🥂 ⚽ 🏀 🎮 🎲 🎸 🎧 🎬 🎉 🎁 🏆 🚗 ✈️ 🚀 🏠 💻 📱 📷 💡 📚 ✏️ 📌 🔒 🔑 💰 ⏰",
    ),
    (
        "Символы",
        "✅ ❌ ❓ ❗ ⚠️ 🚫 ♻️ ➕ ➖ ➗ ✖️ 💲 🔴 🟢 🔵 ⚫ ⚪ 🟡 🆗 🆕 🆒 🔝 ▶️ ⏸️ ⏹️ 🔁 🔊 🔇 🔔 🔕",
    ),
];

/// Which part of the panel is shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Tab {
    Emoji,
    Recent,
    Set(i64),
}

/// Stickers loaded so far (for the whole app: same account, same sets).
#[derive(Default)]
pub(crate) struct Catalog {
    pub(crate) recent: Option<Vec<StickerRef>>,
    pub(crate) sets: Option<Vec<(i64, String)>>,
    pub(crate) set_stickers: HashMap<i64, Vec<StickerRef>>,
}

impl App {
    /// Opens the panel on a tab, loading what it needs.
    pub(crate) fn picker_tab(&mut self, window: WinId, tab: Tab) -> Task<Msg> {
        if self
            .session
            .panes
            .get(&window)
            .is_some_and(|p| p.picker.is_some_and(|old| old != tab))
        {
            self.clear_picker_photos(window);
        }
        if let Some(pane) = self.session.panes.get_mut(&window) {
            pane.picker = Some(tab);
        }
        let client = self.session.client_id;
        let mut tasks = Vec::new();
        if self.session.stickers.sets.is_none() {
            tasks.push(Task::perform(td::sticker_sets(client), move |r| {
                Msg::ForClient(client, Box::new(Msg::StickerSets(r)))
            }));
        }
        match tab {
            Tab::Recent if self.session.stickers.recent.is_none() => {
                tasks.push(Task::perform(td::recent_stickers(client), move |r| {
                    Msg::ForClient(client, Box::new(Msg::RecentStickers(r)))
                }));
            }
            Tab::Set(id) if !self.session.stickers.set_stickers.contains_key(&id) => {
                tasks.push(Task::perform(td::sticker_set(client, id), move |r| {
                    Msg::ForClient(client, Box::new(Msg::StickerSet(id, r)))
                }));
            }
            _ => {}
        }
        Task::batch(tasks)
    }

    /// A network hiccup while the tab was already open: without a retry it
    /// would stay on "…" until the user switches tabs and back.
    pub(crate) fn retry_recent_stickers(&self) -> Task<Msg> {
        let client = self.session.client_id;
        Task::perform(
            async move {
                tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                td::recent_stickers(client).await
            },
            move |r| Msg::ForClient(client, Box::new(Msg::RecentStickers(r))),
        )
    }

    pub(crate) fn retry_sticker_sets(&self) -> Task<Msg> {
        let client = self.session.client_id;
        Task::perform(
            async move {
                tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                td::sticker_sets(client).await
            },
            move |r| Msg::ForClient(client, Box::new(Msg::StickerSets(r))),
        )
    }

    pub(crate) fn retry_sticker_set(&self, id: i64) -> Task<Msg> {
        let client = self.session.client_id;
        Task::perform(
            async move {
                tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                td::sticker_set(client, id).await
            },
            move |r| Msg::ForClient(client, Box::new(Msg::StickerSet(id, r))),
        )
    }

    /// Registers sticker files so their pictures can be downloaded.
    pub(crate) fn note_stickers(&mut self, stickers: &[StickerRef]) {
        for s in stickers {
            for file in std::iter::once(&s.file).chain(&s.picture) {
                self.session
                    .files
                    .entry(file.id)
                    .or_insert_with(|| FileState::from(file));
            }
        }
    }

    pub(crate) fn view_picker<'a>(&'a self, window: WinId, tab: Tab) -> Element<'a, Msg> {
        let on_pane = move |msg| Msg::Pane(window, msg);
        let tab_button = |label: String, t: Tab| {
            button(text(label).size(13))
                .padding([3, 8])
                .style(if tab == t {
                    button::primary
                } else {
                    button::text
                })
                .on_press(on_pane(PaneMsg::PickerTab(t)))
                .into()
        };
        let mut tabs: Vec<Element<'a, Msg>> = vec![
            tab_button("Эмодзи".into(), Tab::Emoji),
            tab_button("Недавние".into(), Tab::Recent),
        ];
        for (id, title) in self.session.stickers.sets.iter().flatten() {
            tabs.push(tab_button(title.clone(), Tab::Set(*id)));
        }
        let tabs = scrollable(row(tabs).spacing(2)).direction(scrollable::Direction::Horizontal(
            scrollable::Scrollbar::new().width(2).scroller_width(2),
        ));

        let content: Element<'a, Msg> = match tab {
            Tab::Emoji => {
                let mut groups = column![].spacing(6);
                for (title, list) in EMOJI {
                    let cells = list.split_whitespace().map(|e| {
                        button(text(e).size(22).font(rich::EMOJI_FONT))
                            .padding(2)
                            .style(button::text)
                            .on_press(on_pane(PaneMsg::InsertEmoji(e.to_owned())))
                            .into()
                    });
                    groups = groups
                        .push(text(title).size(12).style(text::primary))
                        .push(row(cells).wrap());
                }
                groups.into()
            }
            Tab::Recent => self.sticker_grid(
                window,
                tab,
                self.session.stickers.recent.as_deref(),
                "Недавних стикеров нет",
            ),
            Tab::Set(id) => self.sticker_grid(
                window,
                tab,
                self.session
                    .stickers
                    .set_stickers
                    .get(&id)
                    .map(Vec::as_slice),
                "Пустой набор",
            ),
        };
        container(
            column![
                row![
                    container(tabs).width(Fill),
                    button(text("×").size(14))
                        .style(button::text)
                        .on_press(on_pane(PaneMsg::ClosePicker)),
                ]
                .align_y(iced::Center),
                scrollable(content).height(220),
            ]
            .spacing(6),
        )
        .padding(6)
        .style(container::bordered_box)
        .into()
    }

    fn sticker_grid<'a>(
        &'a self,
        window: WinId,
        tab: Tab,
        stickers: Option<&'a [StickerRef]>,
        empty: &'static str,
    ) -> Element<'a, Msg> {
        let on_pane = move |msg| Msg::Pane(window, msg);
        let Some(stickers) = stickers else {
            return text("…").into();
        };
        if stickers.is_empty() {
            return text(empty).size(13).into();
        }
        let epoch = self.photo_epoch(window);
        let cells = stickers.iter().enumerate().map(|(index, s)| {
            let picture: Element<'a, Msg> = match s
                .picture
                .as_ref()
                .and_then(|p| self.session.images.peek(p.id))
            {
                Some(handle) => image(handle.clone()).width(64).height(64).into(),
                None => container(text(&s.emoji).size(28).font(rich::EMOJI_FONT))
                    .center_x(64)
                    .center_y(64)
                    .into(),
            };
            let picture: Element<'a, Msg> = match &s.picture {
                // Pictures load when they come into view.
                Some(p) => {
                    let id = p.id;
                    let owner = PhotoOwner::Picker(tab, index);
                    sensor(picture)
                        .key((tab, index, id, epoch))
                        .on_show(move |_| on_pane(PaneMsg::MediaShown(owner, id, epoch)))
                        .on_hide(on_pane(PaneMsg::MediaGone(owner, id, epoch)))
                        .into()
                }
                None => picture,
            };
            button(picture)
                .padding(2)
                .style(button::text)
                .on_press(on_pane(PaneMsg::SendSticker(Box::new(s.clone()))))
                .into()
        });
        row(cells).wrap().into()
    }
}

/// Inserts `emoji` at the cursor of the input field.
pub(crate) fn insert(content: &mut text_editor::Content, emoji: &str) {
    content.perform(text_editor::Action::Edit(text_editor::Edit::Paste(
        std::sync::Arc::new(emoji.to_owned()),
    )));
}

#[cfg(test)]
mod tests {
    #[test]
    fn every_emoji_is_drawn_with_the_emoji_font() {
        for (_, list) in super::EMOJI {
            for e in list.split_whitespace() {
                let runs = super::rich::emoji_runs(e);
                assert_eq!(runs, [(e, true)], "{e} would fall back to a plain font");
            }
        }
    }
}
