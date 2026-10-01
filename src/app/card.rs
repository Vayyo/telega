//! Mini profile of a user, opened by a click on their name or picture:
//! picture, name, status, «Написать» / «Без звука» / «Ещё», bio, username
//! (with a QR code of the link), phone, add to contacts, block.

use iced::widget::{button, column, container, image, mouse_area, row, scrollable, space, text};
use iced::{Element, Fill, Task};

use super::avatars::Peer;
use super::{App, ChatOp, Msg, WinId, rich};
use crate::td;

pub(crate) struct UserCard {
    pub(crate) window: WinId,
    pub(crate) user_id: i64,
    pub(crate) data: Option<Result<td::UserCard, String>>,
    qr: Option<image::Handle>,
    more: bool,
    /// «Заблокировать» asks once more.
    confirm_block: bool,
    /// A request is running (buttons wait).
    busy: bool,
}

#[derive(Debug, Clone)]
pub(crate) enum CardMsg {
    Open(WinId, i64),
    Loaded(i64, Result<td::UserCard, String>),
    Close,
    Message,
    ToggleMute,
    ToggleMore,
    ToggleQr,
    CopyUsername,
    CopyId,
    NewWindow,
    AddContact,
    Block,
    /// Result of adding or (un)blocking: the card reloads.
    Done(Result<(), String>),
}

impl App {
    pub(crate) fn on_card(&mut self, msg: CardMsg) -> Task<Msg> {
        match msg {
            CardMsg::Open(window, user_id) => {
                // The user asked for this person: that is the explicit
                // action which gives a failed picture a second chance.
                self.retry_avatar(Peer::User(user_id));
                self.session.user_card = Some(UserCard {
                    window,
                    user_id,
                    data: None,
                    qr: None,
                    more: false,
                    confirm_block: false,
                    busy: false,
                });
                return self.load_card(user_id);
            }
            CardMsg::Loaded(user_id, result) => {
                if let Some(card) = self
                    .session
                    .user_card
                    .as_mut()
                    .filter(|c| c.user_id == user_id)
                {
                    card.data = Some(result);
                    card.busy = false;
                }
            }
            CardMsg::Close => self.session.user_card = None,
            CardMsg::Message => {
                let Some((window, chat_id)) = self.card_chat() else {
                    return Task::none();
                };
                self.session.user_card = None;
                return self.select_chat(window, chat_id);
            }
            CardMsg::ToggleMute => {
                let Some((_, chat_id)) = self.card_chat() else {
                    return Task::none();
                };
                let muted = self.session.chats.get(&chat_id).is_some_and(|c| c.muted());
                return self.chat_op(chat_id, ChatOp::Mute(!muted));
            }
            CardMsg::ToggleMore => {
                if let Some(card) = &mut self.session.user_card {
                    card.more = !card.more;
                }
            }
            CardMsg::ToggleQr => {
                if let Some(card) = &mut self.session.user_card {
                    card.qr = if card.qr.is_some() {
                        None
                    } else {
                        // The button exists only once `data` is loaded.
                        card.data
                            .as_ref()
                            .and_then(|r| r.as_ref().ok())
                            .and_then(|data| {
                                super::qr::image(&format!("https://t.me/{}", data.username))
                            })
                    };
                }
            }
            CardMsg::CopyUsername => {
                if let Some(Ok(data)) = self
                    .session
                    .user_card
                    .as_ref()
                    .and_then(|c| c.data.as_ref())
                    && !data.username.is_empty()
                {
                    return self.copy_notice(format!("https://t.me/{}", data.username));
                }
            }
            CardMsg::CopyId => {
                if let Some(card) = &self.session.user_card {
                    return self.copy_notice(card.user_id.to_string());
                }
            }
            CardMsg::NewWindow => {
                let Some((_, chat_id)) = self.card_chat() else {
                    return Task::none();
                };
                self.session.user_card = None;
                return self.open_chat_window(Some(chat_id));
            }
            CardMsg::AddContact => {
                let Some(card) = self.session.user_card.as_mut().filter(|c| !c.busy) else {
                    return Task::none();
                };
                let Some(Ok(data)) = &card.data else {
                    return Task::none();
                };
                card.busy = true;
                let (user, name) = (card.user_id, data.name.clone());
                return Task::perform(td::add_contact(self.session.client_id, user, name), |r| {
                    Msg::Card(CardMsg::Done(r))
                });
            }
            CardMsg::Block => {
                let Some(card) = self.session.user_card.as_mut().filter(|c| !c.busy) else {
                    return Task::none();
                };
                let Some(Ok(data)) = &card.data else {
                    return Task::none();
                };
                // Blocking asks for a second click; unblocking does not.
                if !data.blocked && !card.confirm_block {
                    card.confirm_block = true;
                    return Task::none();
                }
                card.confirm_block = false;
                card.busy = true;
                let (user, block) = (card.user_id, !data.blocked);
                return Task::perform(td::set_blocked(self.session.client_id, user, block), |r| {
                    Msg::Card(CardMsg::Done(r))
                });
            }
            CardMsg::Done(result) => {
                let Some(card) = &mut self.session.user_card else {
                    return Task::none();
                };
                if let Err(e) = result {
                    card.busy = false;
                    self.error = Some(e);
                    return Task::none();
                }
                let user = card.user_id;
                return self.load_card(user);
            }
        }
        Task::none()
    }

    fn load_card(&self, user_id: i64) -> Task<Msg> {
        Task::perform(td::user_card(self.session.client_id, user_id), move |r| {
            Msg::Card(CardMsg::Loaded(user_id, r))
        })
    }

    fn card_chat(&self) -> Option<(WinId, i64)> {
        let card = self.session.user_card.as_ref()?;
        match &card.data {
            Some(Ok(data)) => Some((card.window, data.chat_id)),
            _ => None,
        }
    }

    /// The card over the window it was opened in.
    pub(crate) fn view_card(&self, window: WinId) -> Option<Element<'_, Msg>> {
        let card = self
            .session
            .user_card
            .as_ref()
            .filter(|c| c.window == window)?;
        let name = match &card.data {
            Some(Ok(d)) => d.name.as_str(),
            _ => self
                .session
                .users
                .get(&card.user_id)
                .map_or("…", String::as_str),
        };
        let mut card_name = row![text(name).size(18)].spacing(4).align_y(iced::Center);
        if self.is_bot_user(card.user_id) {
            card_name = card_name.push(text("Bot").size(11).style(text::primary));
        }
        let close = button(text("✕").size(16))
            .style(button::text)
            .on_press(Msg::Card(CardMsg::Close));
        let mut head = column![
            row![space().width(Fill), close],
            container(self.avatar(Peer::User(card.user_id), name, 96.0)).center_x(Fill),
            container(card_name).center_x(Fill),
        ]
        .spacing(6);

        let mut body = column![].spacing(10);
        match &card.data {
            None => head = head.push(container(text("…").size(13)).center_x(Fill)),
            Some(Err(e)) => body = body.push(text(e.clone()).size(13).style(text::danger)),
            Some(Ok(data)) => {
                if !data.status.is_empty() {
                    head = head.push(container(faded(data.status.clone(), 13)).center_x(Fill));
                }
                if !data.is_self {
                    head = head.push(self.card_actions(card, data));
                }
                body = self.card_details(card, data);
            }
        }
        // Shown here, not in the chat behind the card: the panel's dim
        // background would otherwise hide them entirely.
        if let Some(e) = &self.error {
            body = body.push(text(e.clone()).size(13).style(text::danger));
        }
        if let Some(notice) = &self.session.notice {
            body = body.push(text(notice.clone()).size(13).style(text::success));
        }

        let panel = container(scrollable(column![head, body].spacing(14)).height(Fill))
            .padding(14)
            .width(360)
            .height(Fill)
            .style(|theme: &iced::Theme| container::Style {
                background: Some(theme.extended_palette().background.base.color.into()),
                border: iced::border::rounded(10),
                ..container::Style::default()
            });
        // Clicks on the panel stay in it; around it they close the card.
        let panel = mouse_area(panel).on_press(Msg::Ignore);
        Some(
            mouse_area(
                container(panel)
                    .center(Fill)
                    .style(|_| container::background(iced::Color::from_rgba8(0, 0, 0, 0.55))),
            )
            .on_press(Msg::Card(CardMsg::Close))
            .on_scroll(|_| Msg::Ignore)
            .into(),
        )
    }

    /// «Написать», «Без звука», «Ещё» tiles, and the «Ещё» menu.
    fn card_actions<'a>(&self, card: &'a UserCard, data: &'a td::UserCard) -> Element<'a, Msg> {
        let muted = self
            .session
            .chats
            .get(&data.chat_id)
            .is_some_and(|c| c.muted());
        let tile = |icon: &'static str, label: &'static str, msg: CardMsg| {
            button(
                column![
                    text(icon).size(20).font(rich::EMOJI_FONT),
                    text(label).size(12),
                ]
                .spacing(2)
                .width(Fill)
                .align_x(iced::Center),
            )
            .width(Fill)
            .padding([8, 4])
            .style(tile_style)
            .on_press(Msg::Card(msg))
        };
        let mut actions = column![
            row![
                tile("💬", "Написать", CardMsg::Message),
                if muted {
                    tile("🔕", "Со звуком", CardMsg::ToggleMute)
                } else {
                    tile("🔔", "Без звука", CardMsg::ToggleMute)
                },
                tile("⋯", "Ещё", CardMsg::ToggleMore),
            ]
            .spacing(8),
        ]
        .spacing(4);
        if card.more {
            let item = |label: &'static str, msg: CardMsg| {
                button(text(label).size(14))
                    .width(Fill)
                    .style(button::text)
                    .on_press(Msg::Card(msg))
            };
            let mut menu = column![item("Открыть в новом окне", CardMsg::NewWindow)];
            if !data.username.is_empty() {
                menu = menu.push(item("Скопировать ссылку", CardMsg::CopyUsername));
            }
            actions = actions.push(container(menu).padding(4).style(container::bordered_box));
        }
        actions.into()
    }

    /// Bio, username with QR, phone, contacts and blocking.
    fn card_details<'a>(
        &self,
        card: &'a UserCard,
        data: &'a td::UserCard,
    ) -> iced::widget::Column<'a, Msg> {
        let mut info = column![].spacing(10);
        if !data.bio.is_empty() {
            info = info.push(column![
                text(data.bio.clone()).size(14),
                faded("О себе".into(), 12)
            ]);
        }
        if !data.username.is_empty() {
            let name = button(
                text(format!("@{}", data.username))
                    .size(14)
                    .style(text::primary),
            )
            .padding(0)
            .style(button::text)
            .on_press(Msg::Card(CardMsg::CopyUsername));
            let qr = button(text("QR").size(12).style(text::primary))
                .padding([2, 6])
                .style(button::text)
                .on_press(Msg::Card(CardMsg::ToggleQr));
            info = info.push(
                row![
                    column![
                        name,
                        faded("Имя пользователя · клик копирует ссылку".into(), 12)
                    ]
                    .width(Fill),
                    qr
                ]
                .align_y(iced::Center),
            );
            if let Some(code) = &card.qr {
                info = info.push(
                    container(
                        container(image(code.clone()).width(180).height(180))
                            .padding(6)
                            .style(|_| container::background(iced::Color::WHITE)),
                    )
                    .center_x(Fill),
                );
            }
        }
        info = info.push(
            button(column![
                text(card.user_id.to_string()).size(14),
                faded("ID · клик копирует".into(), 12)
            ])
            .padding(0)
            .style(button::text)
            .on_press(Msg::Card(CardMsg::CopyId)),
        );
        if !data.phone.is_empty() {
            info = info.push(column![
                text(data.phone.clone()).size(14),
                faded("Телефон".into(), 12)
            ]);
        }
        if !data.is_contact && !data.is_self {
            info = info.push(
                button(text("ДОБАВИТЬ В КОНТАКТЫ").size(13).style(text::primary))
                    .padding(0)
                    .style(button::text)
                    .on_press_maybe((!card.busy).then_some(Msg::Card(CardMsg::AddContact))),
            );
        }
        let mut sections = column![section(info)].spacing(8);
        if !data.is_self {
            let label = match (data.blocked, card.confirm_block) {
                (true, _) => "Разблокировать",
                (false, false) => "Заблокировать",
                (false, true) => "Точно заблокировать? Нажмите ещё раз",
            };
            let block = button(
                row![
                    text("✋").size(16).font(rich::EMOJI_FONT),
                    text(label).size(14)
                ]
                .spacing(10)
                .align_y(iced::Center),
            )
            .width(Fill)
            .style(danger_text_button)
            .on_press_maybe((!card.busy).then_some(Msg::Card(CardMsg::Block)));
            sections = sections.push(section(column![block]));
        }
        sections
    }
}

pub(crate) fn faded<'a>(label: String, size: u32) -> Element<'a, Msg> {
    text(label)
        .size(size)
        .style(|theme: &iced::Theme| text::Style {
            color: Some(iced::Color {
                a: 0.55,
                ..theme.palette().text
            }),
        })
        .into()
}

pub(crate) fn section<'a>(content: iced::widget::Column<'a, Msg>) -> Element<'a, Msg> {
    container(content)
        .padding(10)
        .width(Fill)
        .style(|theme: &iced::Theme| container::Style {
            background: Some(theme.extended_palette().background.weak.color.into()),
            border: iced::border::rounded(8),
            ..container::Style::default()
        })
        .into()
}

/// A flat button in the background colors, lighter on hover; used for
/// grids of tiles (`radius` 8) and bars like the pinned-message strip
/// (`radius` 4).
pub(crate) fn surface_button(
    radius: f32,
) -> impl Fn(&iced::Theme, button::Status) -> button::Style {
    move |theme, status| {
        let palette = theme.extended_palette();
        let pair = match status {
            button::Status::Hovered | button::Status::Pressed => palette.background.strong,
            _ => palette.background.weak,
        };
        button::Style {
            background: Some(pair.color.into()),
            text_color: pair.text,
            border: iced::border::rounded(radius),
            ..button::Style::default()
        }
    }
}

pub(crate) fn tile_style(theme: &iced::Theme, status: button::Status) -> button::Style {
    surface_button(8.0)(theme, status)
}

/// A text button styled as a destructive action: «Заблокировать»,
/// «Покинуть».
pub(crate) fn danger_text_button(theme: &iced::Theme, status: button::Status) -> button::Style {
    let mut style = button::text(theme, status);
    style.text_color = theme.palette().danger;
    style
}
