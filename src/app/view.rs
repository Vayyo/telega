use iced::ContentFit;
use iced::widget::{
    button, column, container, float, image, mouse_area, pin, responsive, rich_text, row,
    scrollable, sensor, space, span, stack, text, text_editor, text_input, tooltip,
};
use iced::{Element, Fill, Length, Vector};
use std::sync::Arc;
use tdlib_rs::enums::MessageSender;

use super::avatars::Peer;
use super::media::{
    Media, Motion, PHOTO_MAX_H, PHOTO_MAX_W, PhotoKind, PhotoOwner, can_open, human_size,
};
use super::pane::{
    ChatPane, ChatSearch, HISTORY_PADDING, LOADING_ROW_HEIGHT, SPACING, SearchPaging, TYPING_ROOM,
};
use super::rich;
use super::sidebar_view::{DEFAULT_LIST_VIEW, SEARCH_ROW_HEIGHT, search_visible_rows};
use super::tabs::TAB_BAR;
use super::{App, Auth, Link, Menu, Msg, MsgItem, PaneMsg, Receipt, WinId};

impl App {
    /// Color of a person or chat: their name, avatar placeholder, quotes.
    fn sender_color(&self, sender: &MessageSender) -> iced::Color {
        super::look::person_color(match sender {
            MessageSender::User(u) => u.user_id,
            MessageSender::Chat(c) => c.chat_id,
        })
    }

    /// A channel: posts there have one poster (the channel itself), so
    /// unlike a group chat they never get a name or an avatar.
    fn is_channel(&self, chat_id: i64) -> bool {
        self.session.chats.get(&chat_id).is_some_and(
            |c| matches!(&c.kind, Some(tdlib_rs::enums::ChatType::Supergroup(s)) if s.is_channel),
        )
    }

    /// A message gets a name and an avatar next to it: several people can
    /// write to the chat (a group), unlike a private chat or a channel
    /// (whose posts all come from the same, single poster).
    pub(super) fn shows_sender(&self, m: &MsgItem) -> bool {
        let private = self
            .session
            .chats
            .get(&m.chat_id)
            .is_some_and(|c| c.private);
        !m.outgoing && !private && !self.is_channel(m.chat_id)
    }

    fn sender_name(&self, sender: &MessageSender) -> &str {
        self.sender_display(sender)
    }

    pub(crate) fn sender_display(&self, sender: &MessageSender) -> &str {
        match sender {
            MessageSender::User(u) => self
                .session
                .users
                .get(&u.user_id)
                .map_or("…", String::as_str),
            MessageSender::Chat(c) => self
                .session
                .chats
                .get(&c.chat_id)
                .map_or("…", |c| c.title.as_str()),
        }
    }

    pub(crate) fn view(&self, window: WinId) -> Element<'_, Msg> {
        // A window being closed may still be drawn once after its pane is gone.
        let Some(pane) = self.session.panes.get(&window) else {
            return column![].into();
        };
        let main = window == self.main_window;
        let screen: Element<'_, Msg> = if self.session.auth != Auth::Ready {
            if main {
                return self.view_auth();
            }
            container(text("Нет входа в аккаунт")).center(Fill).into()
        } else if main {
            let body = responsive(move |size| {
                let list_width = (size.width - 72.0 - 240.0).clamp(0.0, 300.0);
                let body: Element<'_, Msg> = if self.session.settings_open {
                    row![
                        self.view_chat_list(window, pane, true, list_width),
                        self.view_settings()
                    ]
                    .into()
                } else if let Some(panel) = pane.list.archive_settings.as_ref() {
                    row![
                        self.view_chat_list(window, pane, true, list_width),
                        self.view_archive_settings(window, panel)
                    ]
                    .into()
                } else {
                    row![
                        self.view_chat_list(window, pane, true, list_width),
                        self.view_chat(window, pane, true)
                    ]
                    .into()
                };
                body
            })
            .height(Fill);
            column![self.view_main_header(), body].into()
        } else if let Some(panel) = pane.list.archive_settings.as_ref() {
            row![
                self.view_chat_list(window, pane, false, 220.0),
                self.view_archive_settings(window, panel)
            ]
            .into()
        } else {
            row![
                self.view_chat_list(window, pane, false, 220.0),
                self.view_chat(window, pane, false)
            ]
            .into()
        };
        // An open menu closes on any click that no widget handled. The area is
        // always present: swapping the root widget would reset scroll state.
        let mut area = mouse_area(screen);
        if pane.menu.is_some()
            || pane.text_selection.is_some()
            || (main && (self.session.accounts_open || self.session.confirm_logout))
        {
            let close = if main && (self.session.accounts_open || self.session.confirm_logout) {
                Msg::DismissAccountPopup
            } else {
                Msg::Pane(window, PaneMsg::ClickOutside)
            };
            area = area.on_press(close.clone()).on_right_press(close);
        } else if self.composer_available(window, pane) {
            // A background click returns the keyboard to the chat, not a redraw.
            area = area.on_press(Msg::Pane(window, PaneMsg::ClickOutside));
        }
        // The photo viewer lies over everything; the stack is always there
        // so opening it does not reset the state of the widgets below.
        let overlay = self
            .view_viewer(window)
            .or_else(|| self.view_video_overlay(window))
            .or_else(|| self.view_card(window))
            .unwrap_or_else(|| space().into());
        container(stack![
            area,
            if main {
                self.view_account_popup()
            } else {
                space().into()
            },
            overlay
        ])
        .id(pane.root_id.clone())
        .into()
    }

    /// Controls stay fixed on the left; only the tabs consume the remaining width.
    fn view_main_header(&self) -> Element<'_, Msg> {
        let name = self.account_name(self.session.slot);
        let id = self
            .session
            .my_id
            .or_else(|| {
                self.settings
                    .accounts
                    .iter()
                    .find(|a| a.slot == self.session.slot)
                    .and_then(|a| a.user_id)
            })
            .unwrap_or_default();
        let profile = button(self.avatar(Peer::User(id), name, 36.0))
            .padding([2, 5])
            .style(button::text)
            .on_press(Msg::ToggleAccounts);
        let settings = tooltip(
            button(text("⚙").size(23))
                .padding([2, 7])
                .style(button::text)
                .on_press(Msg::OpenSettings),
            "Настройки",
            tooltip::Position::Bottom,
        );
        let trash = tooltip(
            button(image(super::icons::TRASH.clone()).width(18).height(18))
                .padding([6, 8])
                .style(button::text)
                .on_press(Msg::CloseAllTabs),
            "Закрыть незакреплённые вкладки",
            tooltip::Position::Bottom,
        );
        container(
            row![profile, settings, trash, self.view_tab_bar()]
                .spacing(4)
                .align_y(iced::Center),
        )
        .width(Fill)
        .style(move |_: &iced::Theme| container::background(self.look.sidebar))
        .into()
    }

    /// Account panels are overlays: neither one changes the sidebar's height or scroll identity.
    fn view_account_popup(&self) -> Element<'_, Msg> {
        if !self.session.accounts_open && !self.session.confirm_logout {
            return space().into();
        }
        responsive(|size| {
            let width = (size.width - 12.0).clamp(0.0, 280.0);
            let name = self.account_name(self.session.slot);
            let id = self
                .session
                .my_id
                .or_else(|| {
                    self.settings
                        .accounts
                        .iter()
                        .find(|a| a.slot == self.session.slot)
                        .and_then(|a| a.user_id)
                })
                .unwrap_or_default();
            let panel: Element<'_, Msg> = if self.session.confirm_logout {
                column![
                    text(format!("Выйти из «{name}»?")).size(15),
                    row![
                        button("Да")
                            .style(button::danger)
                            .on_press(Msg::ConfirmLogOut(true)),
                        button("Нет")
                            .style(button::secondary)
                            .on_press(Msg::ConfirmLogOut(false))
                    ]
                    .spacing(8)
                ]
                .spacing(12)
                .into()
            } else {
                column![
                    row![
                        self.avatar(Peer::User(id), name, 42.0),
                        column![
                            text(name).size(16),
                            button("Выйти").style(button::text).on_press(Msg::LogOut)
                        ]
                        .spacing(2)
                    ]
                    .spacing(10)
                    .align_y(iced::Center),
                    self.view_accounts()
                ]
                .spacing(8)
                .into()
            };
            let popup = container(panel)
                .padding(10)
                .width(width)
                .style(container::bordered_box);
            pin(float(
                container(scrollable(popup).height(Length::Shrink))
                    .max_height((size.height - 54.0).max(0.0)),
            )
            .translate(|_, _| Vector::new(6.0, 48.0)))
            .into()
        })
        .into()
    }

    /// Recent chats as browser-like tabs; scrolls sideways when they do not fit.
    fn view_tab_bar(&self) -> Element<'_, Msg> {
        let active = self
            .session
            .panes
            .get(&self.main_window)
            .and_then(|p| p.chat_id);
        let tabs = self.visible_tabs().into_iter().map(|chat_id| {
            let title = self
                .session
                .chats
                .get(&chat_id)
                .map_or_else(|| chat_id.to_string(), |c| c.title.clone());
            let pinned = self.session.tabs.is_pinned(chat_id);
            let limit = if pinned { 10 } else { 22 };
            let (mut label, cut) = take_clusters(&title, limit);
            if cut {
                label.push('…');
            }
            // Reuse the same lazy portrait as the chat list, including initials.
            let mut content = row![
                self.avatar(Peer::Chat(chat_id), &title, 18.0),
                text(label).size(13)
            ]
            .spacing(6)
            .align_y(iced::Center);
            if self.is_bot_chat(chat_id) {
                content = content.push(text("Bot").size(11));
            }
            if let Some(chat) = self.session.chats.get(&chat_id)
                && chat.unread > 0
            {
                content = content.push(text(chat.unread).size(11));
            }
            if !pinned {
                content = content.push(
                    button(text("×").size(12))
                        .padding([0, 4])
                        .style(button::text)
                        .on_press(Msg::CloseTab(chat_id)),
                );
            }
            let style = if active == Some(chat_id) {
                button::primary
            } else {
                button::secondary
            };
            mouse_area(
                button(content)
                    .padding([4, 10])
                    .style(style)
                    .on_press(Msg::SelectTab(chat_id)),
            )
            .on_middle_press(Msg::CloseTab(chat_id))
            .on_right_press(Msg::TogglePin(chat_id))
            .into()
        });
        let bar = scrollable(row(tabs).spacing(4).padding([4, 6]))
            .id(iced::widget::Id::new(TAB_BAR))
            .direction(scrollable::Direction::Horizontal(
                scrollable::Scrollbar::new().width(3).scroller_width(3),
            ))
            .width(Fill);
        // The mouse wheel scrolls the bar sideways.
        mouse_area(bar).on_scroll(Msg::TabsWheel).into()
    }

    fn view_auth(&self) -> Element<'_, Msg> {
        if let Auth::Locked { error, forgot } = &self.session.auth {
            return self.view_lock(error.as_deref(), *forgot);
        }
        if let Auth::Qr { image: code, .. } = &self.session.auth {
            return self.view_qr(code.as_ref());
        }
        let (label, placeholder, secure) = match &self.session.auth {
            Auth::Starting => ("Подключение…", "", false),
            Auth::Phone => ("Номер телефона", "+7…", false),
            Auth::Code => ("Код из Telegram", "12345", false),
            Auth::LoggingOut if self.session.leave.is_some() => {
                ("Переключение аккаунта…", "", false)
            }
            Auth::LoggingOut => ("Выход…", "", false),
            Auth::Password { hint } => (
                if hint.is_empty() {
                    "Пароль 2FA"
                } else {
                    hint.as_str()
                },
                "пароль",
                true,
            ),
            Auth::Unsupported(state) => (state.as_str(), "", false),
            Auth::Ready | Auth::Qr { .. } | Auth::Locked { .. } => unreachable!(),
        };
        let accepts_input = matches!(
            self.session.auth,
            Auth::Phone | Auth::Code | Auth::Password { .. }
        );

        let mut form = column![text(label).size(20)].spacing(12).width(320);
        if accepts_input {
            form = form.push(
                text_input(placeholder, &self.session.input)
                    .secure(secure)
                    .on_input(|value| Msg::Input(crate::lock::SecretString::from(value)))
                    .on_submit(Msg::SubmitAuth),
            );
            let next = button(if self.session.busy {
                "…"
            } else {
                "Далее"
            })
            .on_press_maybe((!self.session.busy).then_some(Msg::SubmitAuth));
            // TDLib accepts a new phone number while waiting for code or password,
            // so going back is purely a UI step.
            let mut buttons = row![next].spacing(8);
            if matches!(self.session.auth, Auth::Code | Auth::Password { .. }) {
                buttons = buttons.push(
                    button("Другой номер")
                        .style(button::secondary)
                        .on_press_maybe((!self.session.busy).then_some(Msg::BackToPhone)),
                );
            }
            if self.session.auth == Auth::Phone {
                buttons = buttons.push(
                    button("Войти по QR-коду")
                        .style(button::secondary)
                        .on_press_maybe((!self.session.busy).then_some(Msg::LoginQr)),
                );
            }
            // Adding an account: the others are one click away.
            if self.other_account().is_some() {
                buttons = buttons.push(
                    button("Отмена")
                        .style(button::secondary)
                        .on_press(Msg::CancelAddAccount),
                );
            }
            form = form.push(buttons);
        }
        if let Some(e) = &self.error {
            form = form.push(text(e).style(text::danger));
        }
        container(form).center(Fill).into()
    }

    /// QR code to scan with a phone where Telegram is logged in.
    fn view_qr(&self, code: Option<&image::Handle>) -> Element<'_, Msg> {
        let picture: Element<'_, Msg> = match code {
            Some(code) => image(code.clone())
                .width(260)
                .height(260)
                .filter_method(image::FilterMethod::Nearest)
                .into(),
            None => text("не удалось построить QR-код")
                .style(text::danger)
                .into(),
        };
        let mut buttons = row![
            button("Войти по номеру")
                .style(button::secondary)
                .on_press(Msg::BackToPhone)
        ]
        .spacing(8);
        if self.other_account().is_some() {
            buttons = buttons.push(
                button("Отмена")
                    .style(button::secondary)
                    .on_press(Msg::CancelAddAccount),
            );
        }
        let mut form = column![
            text("Вход по QR-коду").size(20),
            // White frame: the code stays readable on a dark theme.
            container(picture)
                .padding(8)
                .style(|_| container::background(iced::Color::WHITE)),
            text(
                "Откройте Telegram на телефоне → Настройки → Устройства → \
                 Подключить устройство и наведите камеру на код."
            )
            .size(13),
            buttons,
        ]
        .spacing(12)
        .width(320)
        .align_x(iced::Center);
        if let Some(e) = &self.error {
            form = form.push(text(e).style(text::danger));
        }
        container(form).center(Fill).into()
    }

    fn view_chat<'a>(&'a self, window: WinId, pane: &'a ChatPane, main: bool) -> Element<'a, Msg> {
        let Some(chat_id) = pane.chat_id else {
            let mut content = column![text("Выберите чат")]
                .spacing(8)
                .align_x(iced::Center);
            if let Some(error) = &self.error {
                content = content.push(text(error).style(text::danger));
            }
            return container(content).center(Fill).into();
        };
        let title = self
            .session
            .chats
            .get(&chat_id)
            .map_or("", |c| c.title.as_str());
        let on_pane = move |msg| Msg::Pane(window, msg);
        let mut chat_name = row![text(title).size(18)].spacing(4).align_y(iced::Center);
        if self.is_bot_chat(chat_id) {
            chat_name = chat_name.push(text("Bot").size(11).style(text::primary));
        }

        // Avatar and title open the profile panel.
        let mut header = row![
            button(
                row![self.avatar(Peer::Chat(chat_id), title, 32.0), chat_name]
                    .spacing(8)
                    .align_y(iced::Center),
            )
            .padding(0)
            .style(button::text)
            .on_press(Msg::ToggleProfile(window)),
            space().width(Fill),
        ]
        .spacing(8)
        .align_y(iced::Center);
        header = header.push(
            button(if pane.search.is_some() {
                "Закрыть поиск"
            } else {
                "Поиск"
            })
            .style(button::secondary)
            .on_press(on_pane(PaneMsg::SearchToggle)),
        );
        if main {
            header = header.push(
                button("В новом окне")
                    .style(button::secondary)
                    .on_press(Msg::OpenChatWindow(Some(chat_id))),
            );
        }
        // Selection mode replaces the header with actions on the selection.
        let header: Element<'_, Msg> = match &pane.selected {
            Some(selected) => self.view_selection_bar(window, pane, selected.len()),
            None => header.into(),
        };
        let mut top = column![header].spacing(6);
        let pins = pane.pins();
        if let Some((pinned, own)) = pins.get(pane.pinned_shown) {
            let count = pins.len();
            let kind = if *own {
                "Закреплено для себя"
            } else {
                "Закреплённое"
            };
            let title = if count > 1 {
                format!("{kind} {}/{count}", count - pane.pinned_shown)
            } else {
                kind.to_owned()
            };
            top = top.push(
                button(
                    column![
                        text(title).size(12).style(text::primary),
                        text(one_line(&pinned.preview(), 90)).size(13),
                    ]
                    .spacing(1),
                )
                .width(Fill)
                .padding([4, 10])
                .style(super::card::surface_button(4.0))
                .on_press(on_pane(PaneMsg::PinnedClicked)),
            );
        }
        if let Some(search) = &pane.search {
            top = top.push(
                text_input("Найти в чате… (Enter)", &search.query)
                    .on_input(move |q| on_pane(PaneMsg::SearchQuery(q)))
                    .on_submit(on_pane(PaneMsg::SearchSubmit)),
            );
        }

        // Search results and the forward picker replace the history.
        let body: Element<'_, Msg> = if let Some((ids, query)) = &pane.forward {
            self.view_forward(window, ids.len(), query)
        } else if let Some(search) = pane.search.as_ref().filter(|s| {
            s.results.is_some() || s.paging.pending.is_some() || s.paging.error.is_some()
        }) {
            self.view_found(
                search,
                move |m| on_pane(PaneMsg::JumpTo(m.id)),
                on_pane(PaneMsg::SearchMore),
                window,
            )
        } else {
            self.view_history(window, pane)
        };
        // "печатает…" floats over the bottom of the history, so the messages
        // do not move when it appears; it catches no clicks or scrolling. The
        // stack is always there: a changing widget tree would reset the
        // history's scroll state.
        let overlay: Element<'_, Msg> = match self.typing_label(chat_id) {
            Some(typing) => container(text(typing).size(13).style(text::primary))
                .padding([2, 10])
                .style(container::rounded_box)
                .into(),
            None => space().into(),
        };
        let body: Element<'_, Msg> = iced::widget::stack![
            body,
            container(overlay)
                .padding([2, 8])
                .align_bottom(Fill)
                .align_left(Fill),
        ]
        .into();

        let mut bottom = column![].spacing(4);
        if !pane.newest_loaded {
            bottom = bottom.push(
                button(text("↓ К последним сообщениям").size(13))
                    .style(button::secondary)
                    .on_press(on_pane(PaneMsg::ToLatest)),
            );
        }
        if let Some((link, title, members)) = &pane.confirm_join {
            bottom = bottom.push(
                container(
                    row![
                        text(format!("Вступить в «{title}»? Участников: {members}"))
                            .size(13)
                            .width(Fill),
                        button(text("Вступить").size(12))
                            .on_press(on_pane(PaneMsg::JoinChat(link.clone()))),
                        button(text("Отмена").size(12))
                            .style(button::secondary)
                            .on_press(on_pane(PaneMsg::CancelJoin)),
                    ]
                    .spacing(6)
                    .align_y(iced::Center),
                )
                .padding(6)
                .style(container::bordered_box),
            );
        }
        if let Some(url) = &pane.confirm_link {
            // A link whose text shows something else: the real address first.
            bottom = bottom.push(
                container(
                    row![
                        text(format!("Ссылка ведёт на: {url}")).size(12).width(Fill),
                        button(text("Открыть").size(12))
                            .on_press(on_pane(PaneMsg::OpenLink(url.clone()))),
                        button(text("Отмена").size(12))
                            .style(button::secondary)
                            .on_press(on_pane(PaneMsg::CancelLink)),
                    ]
                    .spacing(6)
                    .align_y(iced::Center),
                )
                .padding(6)
                .style(container::bordered_box),
            );
        }
        if let Some(reply) = pane.reply_to {
            let preview = self
                .reply_lookup(chat_id, reply)
                .map_or_else(|| "сообщение".to_owned(), |m| one_line(&m.preview(), 60));
            bottom = bottom.push(
                row![
                    text(format!("Ответ: {preview}")).size(12).width(Fill),
                    button(text("×").size(12))
                        .style(button::text)
                        .on_press(on_pane(PaneMsg::CancelReply)),
                ]
                .align_y(iced::Center),
            );
        }
        if let Some((id, _)) = pane.editing {
            let preview = self
                .reply_lookup(chat_id, id)
                .map_or_else(|| "сообщение".to_owned(), |m| one_line(&m.preview(), 60));
            bottom = bottom.push(
                row![
                    text(format!("Редактирование: {preview}"))
                        .size(12)
                        .style(text::primary)
                        .width(Fill),
                    button(text("×").size(12))
                        .style(button::text)
                        .on_press(on_pane(PaneMsg::CancelEdit)),
                ]
                .align_y(iced::Center),
            );
        }
        if let Some(tab) = pane.picker {
            bottom = bottom.push(self.view_picker(window, tab));
        }
        let recording = self
            .session
            .playback
            .recording
            .as_ref()
            .filter(|r| r.window == window);
        let can_compose = self.composer_available(window, pane) && recording.is_none();
        let mut editor = text_editor(&pane.compose)
            .id(pane.compose_id.clone())
            .placeholder("Сообщение…")
            // The draft stays legible while menus and modal surfaces own keys.
            .style(|theme, status| {
                let mut style = text_editor::default(theme, status);
                if status == text_editor::Status::Disabled {
                    style.value = theme.extended_palette().background.base.text;
                    style.placeholder = theme.extended_palette().secondary.base.text;
                }
                style
            })
            // Enter sends, Shift+Enter starts a new line.
            .key_binding(move |key| {
                use iced::keyboard::{Key, key::Named};
                use iced::widget::text_editor::{Binding, Status};
                if key.key == Key::Named(Named::Enter)
                    && !key.modifiers.shift()
                    && matches!(key.status, Status::Focused { .. })
                {
                    return Some(Binding::Custom(on_pane(PaneMsg::Send)));
                }
                Binding::from_key_press(key)
            })
            .max_height(160);
        if can_compose {
            editor = editor.on_action(move |a| on_pane(PaneMsg::Compose(a)));
        }
        let theme = &self.look.theme;
        bottom = bottom.push(match recording {
            Some(rec) => recording_controls(theme, rec.started.elapsed().as_secs()),
            None => row![
                editor,
                composer_control(
                    theme,
                    super::icons::ComposerIcon::Smile,
                    "Эмодзи",
                    false,
                    can_compose.then(|| on_pane(PaneMsg::TogglePicker)),
                ),
                composer_control(
                    theme,
                    if pane.editing.is_some() {
                        super::icons::ComposerIcon::Check
                    } else {
                        super::icons::ComposerIcon::Plane
                    },
                    if pane.editing.is_some() {
                        "Сохранить"
                    } else {
                        "Отправить"
                    },
                    true,
                    can_compose.then(|| on_pane(PaneMsg::Send)),
                ),
                composer_control(
                    theme,
                    super::icons::ComposerIcon::Clip,
                    "Прикрепить файл",
                    false,
                    can_compose.then(|| on_pane(PaneMsg::Attach)),
                ),
                composer_control(
                    theme,
                    super::icons::ComposerIcon::Microphone,
                    "Записать голосовое сообщение",
                    false,
                    can_compose.then_some(Msg::RecordStart(window)),
                ),
            ]
            .spacing(6)
            .align_y(iced::Center)
            .into(),
        });

        let mut view = column![top, body, bottom].spacing(8).padding(10);
        if let Some(e) = &self.error {
            view = view.push(text(e).style(text::danger));
        }
        if let Some(notice) = &self.session.notice {
            view = view.push(text(notice).size(13).style(text::success));
        }
        match &pane.profile {
            Some((id, data)) => row![
                view.width(Fill),
                self.view_profile(window, *id, data.as_ref())
            ]
            .into(),
            _ => view.into(),
        }
    }

    fn view_history<'a>(&'a self, window: WinId, pane: &'a ChatPane) -> Element<'a, Msg> {
        let on_pane = move |msg| Msg::Pane(window, msg);
        // Virtualization: only messages near the viewport are built.
        let visible = pane.visible();
        let bubbles = (visible.start..visible.end).map(|i| {
            let prev = i.checked_sub(1).map(|p| &pane.messages[p]);
            self.view_bubble(window, pane, &pane.messages[i], prev)
        });

        // A plain column: its diff checks widget types. iced's keyed column
        // hands child state to a different widget type when several rows
        // change at once (panicked with "Downcast on stateless state").
        let mut history = column![].spacing(SPACING).padding(HISTORY_PADDING);
        if pane.loading_older && visible.start == 0 {
            history = history.push(
                container(text("Загрузка…").size(12))
                    .center_x(Fill)
                    .height(LOADING_ROW_HEIGHT),
            );
        }
        let mut history = history
            .push(space().height(visible.above))
            .extend(bubbles)
            .push(space().height(visible.below))
            // Room for the "печатает…" line, so it never covers the last message.
            .push(space().height(TYPING_ROOM));
        if pane.loading_newer {
            history = history.push(
                container(text("Загрузка…").size(12))
                    .center_x(Fill)
                    .height(LOADING_ROW_HEIGHT),
            );
        }
        let history = scrollable(super::selectable::Gesture::history(
            history,
            window,
            pane.rectangle.as_ref().map(|r| (r.anchor, r.focus)),
        ))
        .id(pane.scroll_id.clone())
        .anchor_bottom()
        .on_scroll(move |v| on_pane(PaneMsg::Scrolled(v)))
        .height(Fill);

        // The popup is a sibling of the scrollable, not part of its measured
        // rows. Both children remain in place on toggle, preserving scroll
        // state; responsive gives the real viewport even before on_scroll.
        let popup = responsive(move |size| {
            let Some(menu) = pane.menu.as_ref() else {
                return space().into();
            };
            let Some(index) = (visible.start..visible.end)
                .find(|&index| pane.messages[index].id == menu.message_id)
            else {
                return space().into();
            };
            let m = &pane.messages[index];
            let rows = &pane.messages[visible.start..visible.end];
            let after: f32 = pane.messages[index + 1..visible.end]
                .iter()
                .map(|item| pane.height_estimate(item))
                .sum::<f32>()
                + visible.below;
            let bubble_height = pane.height_estimate(m) - SPACING;
            let newer_height = if pane.loading_newer {
                LOADING_ROW_HEIGHT + SPACING
            } else {
                0.0
            };
            let older_height = if pane.loading_older && visible.start == 0 {
                LOADING_ROW_HEIGHT + SPACING
            } else {
                0.0
            };
            // Measured rows include day/unread separators. Empty off-screen
            // spacers and the two gaps around them are real column children.
            let content_height = visible.above
                + visible.below
                + rows
                    .iter()
                    .map(|item| pane.height_estimate(item))
                    .sum::<f32>()
                + TYPING_ROOM
                + 2.0 * (HISTORY_PADDING + SPACING)
                + older_height
                + newer_height;
            let offset = pane
                .scroll
                .map_or(0.0, |offset| offset.y)
                .clamp(0.0, (content_height - size.height).max(0.0));
            let bubble_bottom = size.height.min(content_height) + offset
                - TYPING_ROOM
                - HISTORY_PADDING
                - 2.0 * SPACING
                - newer_height
                - after;
            let bubble_top = bubble_bottom - bubble_height;
            if bubble_bottom <= 0.0 || bubble_top >= size.height {
                return space().into();
            }
            let selected_menu = pane
                .selected
                .as_ref()
                .is_some_and(|ids| ids.contains(&m.id));
            let x = if m.outgoing {
                (size.width - 260.0).max(0.0)
            } else if self.shows_sender(m) {
                42.0
            } else {
                10.0
            };
            let actions = if selected_menu {
                view_selected_menu(window, pane)
            } else {
                view_menu(window, m, menu, pane.pinned_locally(m.id))
            };
            let menu = container(scrollable(actions).height(Length::Shrink).width(250))
                .max_height(size.height);
            pin(float(menu).translate(move |bounds, _viewport| {
                let h = bounds.height;
                let y = if bubble_bottom + h <= size.height {
                    bubble_bottom
                } else if bubble_top >= h {
                    bubble_top - h
                } else {
                    bubble_bottom.min(size.height - h)
                };
                Vector::new(
                    x.min((size.width - bounds.width).max(0.0)) + 0.01,
                    y.max(0.0) + 0.01,
                )
            }))
            .into()
        });
        stack![history, popup].into()
    }

    fn view_bubble<'a>(
        &'a self,
        window: WinId,
        pane: &'a ChatPane,
        m: &'a MsgItem,
        prev: Option<&MsgItem>,
    ) -> Element<'a, Msg> {
        let on_pane = move |msg| Msg::Pane(window, msg);
        // Own messages: on the right in the accent color, without a name.
        let mut body = column![].spacing(4);
        // Names and pictures only where several people write, once per run
        // of messages from the same sender on the same day.
        let group_incoming = self.shows_sender(m);
        let run_start = prev.is_none_or(|p| {
            p.sender != m.sender || p.outgoing || p.date <= 0 || day(p.date) != day(m.date)
        });
        if group_incoming && run_start {
            let label: Element<'a, Msg> = match &m.sender {
                MessageSender::User(u) => mouse_area(
                    text(self.sender_name(&m.sender))
                        .size(12)
                        .color(self.sender_color(&m.sender)),
                )
                .interaction(iced::mouse::Interaction::Pointer)
                .on_press(Msg::Card(super::card::CardMsg::Open(window, u.user_id)))
                .into(),
                MessageSender::Chat(_) => text(self.sender_name(&m.sender))
                    .size(12)
                    .color(self.sender_color(&m.sender))
                    .into(),
            };
            let mut name = row![label].spacing(6).align_y(iced::Center);
            if let MessageSender::User(u) = &m.sender
                && self.is_bot_user(u.user_id)
            {
                name = name.push(text("Bot").size(11).style(text::primary));
            }
            // Roles in the accent, plain member tags faded, like Telegram.
            if let Some((tag, role)) = self.sender_badge(m) {
                name = name.push(text(tag).size(11).style(move |theme: &iced::Theme| {
                    let palette = theme.extended_palette();
                    text::Style {
                        color: Some(if role {
                            palette.primary.base.color
                        } else {
                            iced::Color {
                                a: 0.55,
                                ..palette.background.weak.text
                            }
                        }),
                    }
                }));
            }
            body = body.push(name);
        }
        if let Some(origin) = &m.forwarded {
            let from = match origin {
                super::Origin::User(id) => self.session.users.get(id).map_or("…", String::as_str),
                super::Origin::Name(name) => name.as_str(),
                super::Origin::Chat(id) => {
                    self.session.chats.get(id).map_or("…", |c| c.title.as_str())
                }
            };
            let own = m.outgoing;
            // A forwarded channel or group: its name opens its profile.
            let origin_chat = match origin {
                super::Origin::Chat(id) if self.session.chats.contains_key(id) => Some(*id),
                _ => None,
            };
            let forwarded =
                text(format!("Переслано от {from}"))
                    .size(12)
                    .style(move |theme: &iced::Theme| {
                        // Accent on the accent-colored own bubble would vanish.
                        if own {
                            text::Style {
                                color: Some(theme.extended_palette().primary.weak.text),
                            }
                        } else {
                            text::primary(theme)
                        }
                    });
            body = body.push(match origin_chat {
                Some(id) => Element::from(
                    mouse_area(forwarded)
                        .interaction(iced::mouse::Interaction::Pointer)
                        .on_press(Msg::OpenProfile(window, id)),
                ),
                None => forwarded.into(),
            });
        }
        if let Some(reply) = m.reply_to {
            body = body.push(self.view_quote(window, m.chat_id, reply));
        }
        if let Some(media) = &m.media {
            body = body.push(self.view_media(window, pane, m, media));
        }
        let has_text = m.rich.iter().any(|p| !p.text.is_empty());
        if has_text {
            body = body.push(view_rich(
                window,
                m,
                pane.revealed.contains(&m.id),
                pane.text_selection.as_ref(),
            ));
        }
        if let Some(extra) = &m.extra {
            body = body.push(self.view_extra(window, m, extra));
        }
        if m.deleted {
            body = body.push(text("удалено").size(11).style(text::danger));
        }
        if !m.reactions.is_empty() {
            let chips = m.reactions.iter().map(|r| {
                let chosen = r.chosen;
                button(
                    row![
                        text(r.key.label()).size(13).font(rich::EMOJI_FONT),
                        text(r.count).size(13)
                    ]
                    .spacing(4),
                )
                .padding([1, 8])
                .style(move |theme: &iced::Theme, status| reaction_chip(theme, status, chosen))
                .on_press(on_pane(PaneMsg::React(m.id, r.key.clone())))
                .into()
            });
            body = body.push(row(chips).spacing(4).wrap());
        }
        // Time and, for own messages, status in the bottom right corner;
        // the content above stays left-aligned.
        let receipt = if m.failed {
            Some(Receipt::Failed)
        } else {
            self.receipt(m.chat_id, m.id, m.outgoing, m.pending)
        };
        let mut footer = row![].spacing(4).align_y(iced::Center);
        if pane.pinned_locally(m.id) {
            footer = footer.push(text("📌").size(10).font(rich::EMOJI_FONT));
        }
        if m.edited {
            footer = footer.push(footnote("изменено".to_owned(), m.outgoing));
        }
        if m.date > 0 {
            footer = footer.push(footnote(stamp(m.date), m.outgoing));
        }
        if let Some(receipt) = receipt {
            let label = match receipt {
                Receipt::Sending => "отправляется",
                Receipt::Sent => "отправлено",
                Receipt::Read => "прочитано",
                Receipt::Failed => "не отправлено",
            };
            footer = footer.push(footnote(label.to_owned(), true));
            footer = footer.push(receipt_dot(receipt));
        }
        let body = column![body, footer]
            .spacing(2)
            .align_x(iced::Alignment::End);
        let highlighted = pane.highlight == Some(m.id);
        let selecting = pane.selected.is_some();
        let selected = pane.selected.as_ref().is_some_and(|s| s.contains(&m.id))
            || pane
                .rectangle
                .as_ref()
                .is_some_and(|r| r.hits.contains(&m.id));
        let own = m.outgoing;
        let bubble = mouse_area(container(body).padding(8).max_width(520).style(
            move |theme: &iced::Theme| {
                let base = if highlighted {
                    container::bordered_box(theme)
                } else if own {
                    own_box(theme)
                } else {
                    container::rounded_box(theme)
                };
                if selected {
                    // On the accent-colored own bubbles an accent frame
                    // would vanish; they get a light one.
                    let accent = if own {
                        theme.palette().text
                    } else {
                        theme.extended_palette().primary.strong.color
                    };
                    container::Style {
                        border: iced::Border {
                            color: accent,
                            width: 2.0,
                            radius: 4.0.into(),
                        },
                        ..base
                    }
                } else {
                    base
                }
            },
        ))
        .on_right_press(on_pane(PaneMsg::OpenMenu(m.id)));
        // In selection mode a click picks the message.
        let bubble = if selecting && !m.pending {
            bubble.on_press(on_pane(PaneMsg::Select(m.id)))
        } else {
            bubble
        };
        let bubble = super::selectable::Gesture::bubble(
            bubble,
            window,
            m.id,
            !m.pending && !m.deleted,
            pane.rectangle.as_ref().map(|r| (r.anchor, r.focus)),
        );
        let stack: Element<'a, Msg> = if group_incoming {
            let side: Element<'a, Msg> = if run_start {
                let peer = match &m.sender {
                    MessageSender::User(u) => Peer::User(u.user_id),
                    MessageSender::Chat(c) => Peer::Chat(c.chat_id),
                };
                let picture = self.avatar(peer, self.sender_name(&m.sender), 30.0);
                match &m.sender {
                    MessageSender::User(u) => mouse_area(picture)
                        .interaction(iced::mouse::Interaction::Pointer)
                        .on_press(Msg::Card(super::card::CardMsg::Open(window, u.user_id)))
                        .into(),
                    MessageSender::Chat(_) => picture,
                }
            } else {
                space().width(30).into()
            };
            row![side, bubble].spacing(6).into()
        } else {
            bubble.into()
        };
        let aligned = container(stack).width(Fill);
        let aligned = if m.outgoing {
            aligned.align_right(Fill)
        } else {
            aligned
        };
        // Day and unread separators belong to the message under them, so
        // they are measured with it (virtualization works per message).
        let mut block = column![].spacing(SPACING);
        if m.date > 0 && prev.is_none_or(|p| p.date <= 0 || day(p.date) != day(m.date)) {
            block = block.push(
                container(
                    container(text(day_label(m.date)).size(12))
                        .padding([2, 10])
                        .style(container::rounded_box),
                )
                .center_x(Fill),
            );
        }
        if let Some(read) = pane.unread_after
            && m.id > read
            && !m.outgoing
            && prev.is_none_or(|p| p.id <= read)
        {
            block = block.push(
                container(
                    text("Непрочитанные сообщения")
                        .size(12)
                        .style(text::primary),
                )
                .padding(4)
                .center_x(Fill)
                .style(container::bordered_box),
            );
        }
        let aligned = block.push(aligned);
        // Real heights replace the estimates used for off-screen space.
        let id = m.id;
        sensor(aligned)
            .on_resize(move |size| on_pane(PaneMsg::Measured(id, size.height)))
            .into()
    }

    /// Side panel about the chat: picture, name, username, bio or
    /// description, members.
    fn view_profile<'a>(
        &'a self,
        window: WinId,
        chat_id: i64,
        data: Option<&'a Result<crate::td::Profile, String>>,
    ) -> Element<'a, Msg> {
        use super::ProfileAction as A;
        let pane = &self.session.panes[&window];
        let act = move |a: A| Msg::Profile(window, a);
        let title = self
            .session
            .chats
            .get(&chat_id)
            .map_or("", |c| c.title.as_str());
        let mut profile_name = row![text(title).size(18)].spacing(4).align_y(iced::Center);
        if self.is_bot_chat(chat_id) {
            profile_name = profile_name.push(text("Bot").size(11).style(text::primary));
        }
        let mut head = column![
            row![
                space().width(Fill),
                button(text("×").size(16))
                    .style(button::text)
                    .on_press(Msg::ToggleProfile(window)),
            ],
            container(self.avatar(Peer::Chat(chat_id), title, 96.0)).center_x(Fill),
            container(profile_name).center_x(Fill),
        ]
        .spacing(6);
        let mut body = column![].spacing(10);
        match data {
            None => head = head.push(container(text("…")).center_x(Fill)),
            Some(Err(e)) => body = body.push(text(e).style(text::danger)),
            Some(Ok(profile)) => {
                if !profile.subtitle.is_empty() {
                    head = head.push(
                        container(super::card::faded(profile.subtitle.clone(), 13)).center_x(Fill),
                    );
                }
                // Tiles: sound, discussion, more.
                let muted = self.session.chats.get(&chat_id).is_some_and(|c| c.muted());
                let tile = |icon: &'static str, label: &'static str, msg: Option<Msg>| {
                    button(
                        column![
                            text(icon).size(20).font(rich::EMOJI_FONT),
                            text(label).size(12)
                        ]
                        .spacing(2)
                        .width(Fill)
                        .align_x(iced::Center),
                    )
                    .width(Fill)
                    .padding([8, 2])
                    .style(super::card::tile_style)
                    .on_press_maybe(msg)
                };
                let mut tiles = row![if muted {
                    tile("🔕", "Со звуком", Some(act(A::ToggleMute)))
                } else {
                    tile("🔔", "Без звука", Some(act(A::ToggleMute)))
                }]
                .spacing(6);
                if profile.linked_chat_id != 0 {
                    let label = if profile.is_channel {
                        "Обсуждение"
                    } else {
                        "Канал"
                    };
                    tiles = tiles.push(tile("💬", label, Some(act(A::Discuss))));
                }
                tiles = tiles.push(tile("⋯", "Ещё", Some(act(A::ToggleMore))));
                head = head.push(tiles);
                if pane.profile_more {
                    let item = |label: &'static str, a: A| {
                        button(text(label).size(14))
                            .width(Fill)
                            .style(button::text)
                            .on_press(act(a))
                    };
                    let mut menu = column![item("Открыть в новом окне", A::NewWindow)];
                    if !profile.usernames.is_empty() {
                        menu = menu.push(item("Скопировать ссылку", A::CopyLink));
                    }
                    head = head.push(container(menu).padding(4).style(container::bordered_box));
                }

                // Link, other names, description, ID.
                let mut info = column![].spacing(10);
                if let Some((main, others)) = profile.usernames.split_first() {
                    let mut names = column![
                        button(text(format!("t.me/{main}")).size(14).style(text::primary))
                            .padding(0)
                            .style(button::text)
                            .on_press(act(A::CopyLink))
                    ];
                    if !others.is_empty() {
                        let also: Vec<String> = others.iter().map(|n| format!("@{n}")).collect();
                        names = names
                            .push(super::card::faded(format!("также {}", also.join(", ")), 12));
                    }
                    info = info.push(
                        row![
                            names.width(Fill),
                            button(text("QR").size(12).style(text::primary))
                                .padding([2, 6])
                                .style(button::text)
                                .on_press(act(A::ToggleQr)),
                        ]
                        .align_y(iced::Center),
                    );
                    if let Some(code) = &pane.profile_qr {
                        info = info.push(
                            container(
                                container(image(code.clone()).width(170).height(170))
                                    .padding(6)
                                    .style(|_| container::background(iced::Color::WHITE)),
                            )
                            .center_x(Fill),
                        );
                    }
                }
                if !profile.about.is_empty() {
                    let label =
                        if profile.is_channel || profile.linked_chat_id != 0 || profile.is_member {
                            "Описание"
                        } else {
                            "О себе"
                        };
                    info = info.push(column![
                        linked_text(&self.look.theme, window, &profile.about),
                        super::card::faded(label.into(), 12)
                    ]);
                }
                info = info.push(
                    button(super::card::faded(format!("ID: {chat_id}"), 12))
                        .padding(0)
                        .style(button::text)
                        .on_press(Msg::Pane(
                            window,
                            PaneMsg::Link(Link::Copy(chat_id.to_string().into())),
                        )),
                );
                // Opened from elsewhere (a forwarded channel): a way into it.
                if !pane.shows(chat_id) {
                    let label = if profile.is_channel {
                        "ПЕРЕЙТИ В КАНАЛ"
                    } else {
                        "ОТКРЫТЬ ЧАТ"
                    };
                    info = info.push(
                        button(text(label).size(13).style(text::primary))
                            .padding(0)
                            .style(button::text)
                            .on_press(act(A::OpenChat)),
                    );
                }
                body = body.push(super::card::section(info));

                if !profile.members.is_empty() {
                    let members = profile.members.iter().map(|member| {
                        let name: Element<'_, Msg> = if member.is_bot {
                            row![
                                container(
                                    text(&member.name).size(13).wrapping(text::Wrapping::None)
                                )
                                .max_width(150),
                                text("Bot").size(11).style(text::primary),
                            ]
                            .spacing(6)
                            .align_y(iced::Center)
                            .into()
                        } else {
                            text(&member.name).size(13).into()
                        };
                        button(
                            row![self.avatar(Peer::User(member.id), &member.name, 28.0), name]
                                .spacing(8)
                                .align_y(iced::Center),
                        )
                        .padding([2, 4])
                        .width(Fill)
                        .style(button::text)
                        .on_press(Msg::Card(super::card::CardMsg::Open(window, member.id)))
                        .into()
                    });
                    body = body.push(super::card::section(
                        column![text("Участники").size(13).style(text::primary)]
                            .push(column(members).spacing(4))
                            .spacing(6),
                    ));
                }
                if profile.is_member {
                    let label = match (pane.confirm_leave, profile.is_channel) {
                        (true, _) => "Точно покинуть? Нажмите ещё раз",
                        (false, true) => "Покинуть канал",
                        (false, false) => "Покинуть группу",
                    };
                    body = body.push(super::card::section(column![
                        button(
                            row![
                                text("🚪").size(16).font(rich::EMOJI_FONT),
                                text(label).size(14)
                            ]
                            .spacing(10)
                            .align_y(iced::Center),
                        )
                        .width(Fill)
                        .style(super::card::danger_text_button)
                        .on_press(act(A::Leave))
                    ]));
                }
            }
        }
        let panel = scrollable(column![head, body].spacing(12));
        container(panel)
            .padding(10)
            .width(280)
            .height(Fill)
            .style(container::bordered_box)
            .into()
    }

    /// Chat picker for forwarding, filtered by title.
    fn view_forward<'a>(&'a self, window: WinId, count: usize, query: &'a str) -> Element<'a, Msg> {
        let on_pane = move |msg| Msg::Pane(window, msg);
        let needle = query.to_lowercase();
        let chats = self
            .session
            .order
            .iter()
            .map(|&(_, id)| id)
            .filter_map(|id| self.session.chats.get(&id).map(|c| (id, c)))
            .filter(|(_, c)| needle.is_empty() || c.title.to_lowercase().contains(&needle))
            .take(100)
            .map(|(id, c)| {
                button(
                    row![
                        self.avatar(Peer::Chat(id), &c.title, 32.0),
                        text(&c.title).size(15)
                    ]
                    .spacing(8)
                    .align_y(iced::Center),
                )
                .width(Fill)
                .style(button::text)
                .on_press(on_pane(PaneMsg::ForwardTo(id)))
                .into()
            });
        let title = if count == 1 {
            "Переслать сообщение в…".to_owned()
        } else {
            format!("Переслать {count} сообщ. в…")
        };
        column![
            row![
                text(title).size(16).width(Fill),
                button("Отмена")
                    .style(button::secondary)
                    .on_press(on_pane(PaneMsg::CancelForward)),
            ]
            .align_y(iced::Center),
            text_input("Поиск чата", query)
                .on_input(move |q| on_pane(PaneMsg::ForwardQuery(q)))
                .size(14),
            scrollable(column(chats).spacing(2)).height(Fill),
        ]
        .spacing(8)
        .into()
    }

    fn view_selection_bar(
        &self,
        window: WinId,
        _pane: &ChatPane,
        count: usize,
    ) -> Element<'_, Msg> {
        row![
            text(format!("Выбрано: {count}")).size(16).width(Fill),
            button("Отмена")
                .style(button::secondary)
                .on_press(Msg::Pane(window, PaneMsg::ClearSelection)),
        ]
        .spacing(6)
        .align_y(iced::Center)
        .into()
    }

    /// Quoted replied message; a click scrolls to it.
    fn view_quote(&self, window: WinId, chat_id: i64, reply: i64) -> Element<'_, Msg> {
        let (who, what, color) = match self.reply_lookup(chat_id, reply) {
            Some(r) => (
                self.sender_name(&r.sender).to_owned(),
                one_line(&r.preview(), 70),
                self.sender_color(&r.sender),
            ),
            None => (
                "…".to_owned(),
                "сообщение".to_owned(),
                iced::Color::from_rgb8(0x80, 0x80, 0x80),
            ),
        };
        // A stripe and the name in the quoted author's color. The quote
        // shrinks to its text: a full-width one stretched the bubble (own
        // replies left the right edge).
        let stripe = container(space().width(3).height(30)).style(move |_| container::Style {
            background: Some(color.into()),
            border: iced::border::rounded(2),
            ..container::Style::default()
        });
        mouse_area(
            container(
                row![
                    stripe,
                    column![text(who).size(11).color(color), text(what).size(12)].spacing(1)
                ]
                .spacing(6)
                .align_y(iced::Center),
            )
            .padding([4, 8])
            .style(|theme: &iced::Theme| container::Style {
                background: Some(
                    iced::Color {
                        a: 0.25,
                        ..theme.extended_palette().background.base.color
                    }
                    .into(),
                ),
                border: iced::border::rounded(4),
                ..container::Style::default()
            }),
        )
        .interaction(iced::mouse::Interaction::Pointer)
        .on_press(Msg::Pane(window, PaneMsg::JumpTo(reply)))
        .into()
    }

    /// A visible footer for initial loading, later pages, failures and end.
    pub(super) fn search_status<'a, C>(
        paging: &'a SearchPaging<C>,
        started: bool,
        more: Msg,
    ) -> Option<Element<'a, Msg>> {
        if paging.pending.is_some() {
            Some(text("Загрузка результатов…").size(12).into())
        } else if let Some(error) = &paging.error {
            Some(
                row![
                    text(error).size(12).style(text::danger),
                    button("Повторить").on_press(more)
                ]
                .spacing(4)
                .into(),
            )
        } else if paging.next.is_some() {
            Some(button("Загрузить ещё").on_press(more).into())
        } else if started {
            Some(text("Все результаты показаны").size(12).into())
        } else {
            None
        }
    }

    /// Search results: sender, chat, text; click opens a message.
    fn view_found<'a>(
        &'a self,
        search: &'a ChatSearch,
        open: impl Fn(&MsgItem) -> Msg + 'a,
        more: Msg,
        window: WinId,
    ) -> Element<'a, Msg> {
        let results = search.results.as_deref();
        let mut list = column![].spacing(2).padding(4);
        if let Some(results) = results {
            if results.is_empty() {
                list = list.push(text("Ничего не найдено").size(13));
            }
            let visible = search_visible_rows(
                search.scroll,
                search.view_height.unwrap_or(DEFAULT_LIST_VIEW),
                6.0,
                results.len(),
            );
            list = list.push(space().height(visible.above));
            for m in &results[visible.start..visible.end] {
                let chat = self
                    .session
                    .chats
                    .get(&m.chat_id)
                    .map_or("", |c| c.title.as_str());
                list = list.push(
                    button(
                        column![
                            text(format!("{} · {}", chat, self.sender_name(&m.sender))).size(11),
                            text(one_line(&m.preview(), 90)).size(13),
                        ]
                        .spacing(2),
                    )
                    .width(Fill)
                    .height(SEARCH_ROW_HEIGHT - 2.0)
                    .style(button::text)
                    .on_press(open(m)),
                );
            }
            list = list.push(space().height(visible.below));
        }
        if let Some(status) = Self::search_status(&search.paging, results.is_some(), more) {
            list = list.push(status);
        }
        let id = search.scroll_id.clone();
        scrollable(list)
            .id(id.clone())
            .height(Fill)
            .on_scroll(move |v| Msg::Pane(window, PaneMsg::SearchScrolled(id.clone(), v)))
            .into()
    }
}

/// Choose the cached monochrome raster by WCAG contrast against the actual
/// button fill. Hover moves that fill farther from the selected icon color.
fn composer_control(
    theme: &iced::Theme,
    icon: super::icons::ComposerIcon,
    label: &'static str,
    primary: bool,
    action: Option<Msg>,
) -> Element<'static, Msg> {
    let palette = theme.extended_palette();
    let background = if primary {
        palette.primary.base.color
    } else {
        palette.secondary.base.color
    };
    // The two contrast ratios meet at sqrt(1.05 × 0.05) - 0.05.
    let light = background.relative_luminance() < 0.179_128_78;
    let opacity: f32 = if action.is_some() { 1.0 } else { 0.5 };
    let control = button(
        image(icon.handle(light))
            .width(22)
            .height(22)
            .opacity(opacity),
    )
    .width(40)
    .height(40)
    .padding(0)
    .style(move |theme, status| {
        let mut style = if primary {
            button::primary(theme, status)
        } else {
            button::secondary(theme, status)
        };
        if status == button::Status::Hovered {
            // The stock "strong" shade may cross the light/dark icon
            // boundary. Move the base surface *away* from the icon instead.
            let palette = theme.extended_palette();
            let base = if primary {
                palette.primary.base.color
            } else {
                palette.secondary.base.color
            };
            let t = 0.08;
            let toward = if light { 0.0 } else { 1.0 };
            style.background = Some(iced::Background::Color(iced::Color::from_rgb(
                base.r + (toward - base.r) * t,
                base.g + (toward - base.g) * t,
                base.b + (toward - base.b) * t,
            )));
        }
        style.border = iced::border::rounded(12);
        style
    })
    .on_press_maybe(action);
    tooltip(control, label, tooltip::Position::Top).into()
}

/// Shared by the live recording row and its offline no-hardware smoke.
pub(super) fn recording_controls(theme: &iced::Theme, seconds: u64) -> Element<'static, Msg> {
    row![
        text(format!("● Запись {}", mmss(seconds as i32)))
            .style(text::danger)
            .width(Fill),
        composer_control(
            theme,
            super::icons::ComposerIcon::Plane,
            "Отправить запись",
            true,
            Some(Msg::RecordStop(true)),
        ),
        composer_control(
            theme,
            super::icons::ComposerIcon::Cross,
            "Отменить запись",
            false,
            Some(Msg::RecordStop(false)),
        ),
    ]
    .spacing(6)
    .align_y(iced::Center)
    .into()
}

/// A grapheme-ish cluster: one base character plus everything that glues to
/// it visually (zero-width-joiner chains, variation selectors, skin-tone
/// modifiers, the keycap combiner, and the second half of a regional-
/// indicator/flag pair). Cutting a string at a cluster boundary, unlike at
/// a `char` boundary, never breaks a flag or a ZWJ emoji into a stray glyph.
fn clusters(text: &str) -> Vec<&str> {
    fn is_regional_indicator(c: char) -> bool {
        matches!(c, '\u{1F1E6}'..='\u{1F1FF}')
    }
    fn glues_to_previous(c: char) -> bool {
        matches!(
            c,
            '\u{200D}' | '\u{FE0E}' | '\u{FE0F}' | '\u{1F3FB}'..='\u{1F3FF}' | '\u{20E3}'
        )
    }
    let mut out = Vec::new();
    let mut chars = text.char_indices().peekable();
    while let Some((start, c)) = chars.next() {
        let mut end = start + c.len_utf8();
        // A flag: two regional-indicator symbols form one cluster.
        if is_regional_indicator(c)
            && let Some(&(_, next)) = chars.peek()
            && is_regional_indicator(next)
        {
            chars.next();
            end += next.len_utf8();
        }
        loop {
            match chars.peek() {
                // A ZWJ glues the following character into this cluster too
                // (families, professions: person-ZWJ-person-ZWJ-person…).
                Some(&(_, '\u{200D}')) => {
                    chars.next();
                    end += '\u{200D}'.len_utf8();
                    let Some(&(_, glued)) = chars.peek() else {
                        break;
                    };
                    chars.next();
                    end += glued.len_utf8();
                }
                Some(&(_, next)) if glues_to_previous(next) => {
                    chars.next();
                    end += next.len_utf8();
                }
                _ => break,
            }
        }
        out.push(&text[start..end]);
    }
    out
}

/// Cluster-safe replacement for `text.chars().take(max).collect()`: cuts at
/// cluster boundaries, so a flag or a ZWJ emoji sequence is either kept
/// whole or dropped whole. Returns the cut text and whether it was shorter.
pub(super) fn take_clusters(text: &str, max: usize) -> (String, bool) {
    let clusters = clusters(text);
    if clusters.len() <= max {
        (text.to_owned(), false)
    } else {
        (clusters[..max].concat(), true)
    }
}

/// First line of a chat's last message, cut for the chat list. Called once
/// when the message arrives (`app.rs`), not on every `view`.
pub(super) fn preview_text(last: &str) -> String {
    take_clusters(last.lines().next().unwrap_or(""), 48).0
}

/// First line of a text, shortened.
pub(super) fn one_line(text: &str, max: usize) -> String {
    let line = text.lines().next().unwrap_or("");
    let (mut out, cut) = take_clusters(line, max);
    if cut {
        out.push('…');
    }
    out
}

/// Formatted text with links; hidden spoilers are blocks that open on click.
/// Message text: styled, with links and spoilers, and selectable with the
/// mouse (one widget per line; the selection is painted as a highlight).
fn view_rich<'a>(
    window: WinId,
    m: &'a MsgItem,
    revealed: bool,
    selection: Option<&super::pane::TextSelection>,
) -> Element<'a, Msg> {
    let text: String = m.rich.iter().map(|p| p.text.as_str()).collect();
    let line_ranges = super::selectable::lines(&text);
    let mut lines: Vec<Vec<text::Span<'a, Link>>> = vec![Vec::new(); line_ranges.len()];
    let selected = selection
        .filter(|s| s.message == m.id && s.anchor != s.focus)
        .map(|s| (s.anchor.min(s.focus), s.anchor.max(s.focus)));
    // On the accent-colored own bubbles an accent highlight would vanish.
    let highlight = if m.outgoing {
        iced::Color::from_rgba8(0xFF, 0xFF, 0xFF, 0.35)
    } else {
        iced::Color::from_rgba8(0x52, 0x88, 0xC1, 0.55)
    };
    let mut offset = 0;
    let mut line_index = 0;
    for piece in &m.rich {
        let style = &piece.style;
        let code_copy =
            (style.code && style.link.is_none()).then(|| Arc::<str>::from(piece.text.as_str()));
        for (run, emoji) in rich::emoji_runs(piece.text.as_str()) {
            let run_start = offset + (run.as_ptr() as usize - piece.text.as_ptr() as usize);
            // Cut at line breaks (any Bidi_Class=B separator, matching
            // `selectable::lines()` so a span never straddles two of the
            // per-line `Selectable` widgets) and at the ends of the
            // selection.
            let mut cuts = vec![0, run.len()];
            for (i, c) in run.char_indices() {
                if super::selectable::is_line_separator(c) {
                    cuts.extend([i, i + c.len_utf8()]);
                }
            }
            if let Some((a, b)) = selected {
                for x in [a, b] {
                    if x > run_start
                        && x < run_start + run.len()
                        && run.is_char_boundary(x - run_start)
                    {
                        cuts.push(x - run_start);
                    }
                }
            }
            cuts.sort_unstable();
            cuts.dedup();
            for pair in cuts.windows(2) {
                let part = &run[pair[0]..pair[1]];
                if part.is_empty()
                    || part.chars().next().is_some_and(|c| {
                        part.len() == c.len_utf8() && super::selectable::is_line_separator(c)
                    })
                {
                    continue;
                }
                let at = run_start + pair[0];
                // Runs and cuts follow the message's byte order; advance past
                // empty lines and separators without rescanning earlier lines.
                while line_index < line_ranges.len() && at >= line_ranges[line_index].end {
                    line_index += 1;
                }
                if line_index == line_ranges.len() || at < line_ranges[line_index].start {
                    continue;
                }
                let line = line_index;
                let mut font = iced::Font::DEFAULT;
                if style.bold {
                    font.weight = iced::font::Weight::Bold;
                }
                if style.italic {
                    font.style = iced::font::Style::Italic;
                }
                if style.code {
                    font = iced::Font::MONOSPACE;
                }
                if emoji {
                    font = rich::EMOJI_FONT;
                }
                let mut s = span(part)
                    .font(font)
                    .underline(style.underline || style.link.is_some())
                    .strikethrough(style.strike);
                if let Some(url) = &style.link {
                    s = s
                        .link(Link::Url {
                            url: url.clone(),
                            hidden: style.link_hidden,
                        })
                        .color(iced::Color::from_rgb8(0x6a, 0xb7, 0xff));
                }
                // Monospace text copies its entire piece, shared by every span.
                if let Some(code) = &code_copy {
                    s = s.link(Link::Copy(Arc::clone(code)));
                }
                if style.spoiler && !revealed {
                    // Covered: text and background of one colour, like a blur.
                    let cover = iced::Color::from_rgb8(0x5a, 0x5f, 0x6b);
                    s = s
                        .color(cover)
                        .background(cover)
                        .underline(false)
                        .link(Link::Spoiler(m.id));
                } else if selected.is_some_and(|(a, b)| at >= a && at < b) {
                    s = s.background(highlight);
                }
                lines[line].push(s);
            }
        }
        offset += piece.text.len();
    }
    let dragging = selection.is_some_and(|s| s.message == m.id && s.dragging);
    let id = m.id;
    let rows = lines
        .into_iter()
        .zip(line_ranges)
        .map(|(mut spans, range)| {
            if spans.is_empty() {
                // An empty line keeps its height.
                spans.push(span(" "));
            }
            super::selectable::Selectable::new(
                spans,
                range.start,
                range.end,
                dragging,
                super::selectable::Handlers {
                    on_press: Box::new(move |at| Msg::Pane(window, PaneMsg::TextPress(id, at))),
                    on_drag: Box::new(move |at| Msg::Pane(window, PaneMsg::TextDrag(id, at))),
                    on_link: Box::new(move |link| Msg::Pane(window, PaneMsg::Link(link))),
                },
            )
            .into()
        });
    column(rows).into()
}

impl App {
    fn view_media<'a>(
        &'a self,
        window: WinId,
        pane: &'a ChatPane,
        m: &'a MsgItem,
        media: &'a Media,
    ) -> Element<'a, Msg> {
        let file_id = media.file_id();
        let file = self.session.files.get(&file_id);
        let epoch = self.photo_epoch(window);
        match media {
            Media::Photo {
                mini,
                width,
                height,
                spoiler,
                ..
            } => {
                let (w, h) = Media::photo_box(*width, *height);
                let hidden = *spoiler && !pane.revealed.contains(&m.id);
                if hidden {
                    // Spoiler: only the tiny blurred preview until clicked;
                    // the photo itself is not even downloaded before that.
                    return spoiler_cover(window, m.id, mini.as_ref(), w, h);
                }
                let picture: Element<'_, Msg> =
                    match self.session.images.peek(file_id).or(mini.as_ref()) {
                        Some(handle) => image(handle.clone())
                            .width(w)
                            .height(h)
                            .content_fit(ContentFit::Cover)
                            .into(),
                        None => container(text("Фото").size(12))
                            .width(w)
                            .height(h)
                            .center(Fill)
                            .style(container::bordered_box)
                            .into(),
                    };
                let mut layers = stack![picture];
                if let Some(f) = file.filter(|f| f.uploading || (f.downloading && !f.done)) {
                    let label = if f.uploading {
                        "Отправка"
                    } else {
                        "Загрузка"
                    };
                    layers = layers.push(
                        container(text(format!("{label} {}%", f.percent())).size(12))
                            .center(Fill)
                            .width(w)
                            .height(h),
                    );
                }
                // Key changes and tree removal do not trigger iced's on_hide;
                // the owner is also reconciled against the rendered range.
                let owner = PhotoOwner::Message(m.chat_id, m.id, PhotoKind::Photo);
                sensor(
                    mouse_area(layers)
                        .interaction(iced::mouse::Interaction::Pointer)
                        .on_press(Msg::ViewPhoto(window, file_id)),
                )
                .key((m.chat_id, m.id, file_id, epoch))
                .anticipate(400)
                .on_show(move |_| Msg::Pane(window, PaneMsg::MediaShown(owner, file_id, epoch)))
                .on_hide(Msg::Pane(window, PaneMsg::MediaGone(owner, file_id, epoch)))
                .into()
            }
            Media::Document { name, size, .. } => {
                let status = match file {
                    Some(f) if m.pending || f.uploading => format!("отправка {}%", f.percent()),
                    Some(f) if f.downloading && !f.done => {
                        format!("{} из {}", human_size(f.downloaded), human_size(*size))
                    }
                    _ => human_size(*size),
                };
                let mut actions = row![].spacing(4);
                match file {
                    Some(f) if f.done => {
                        if can_open(name) {
                            actions = actions.push(
                                button(text("Открыть").size(12))
                                    .style(button::secondary)
                                    .on_press(Msg::FileOpen(file_id)),
                            );
                        }
                        actions = actions.push(
                            button(text("В папке").size(12))
                                .style(button::secondary)
                                .on_press(Msg::FileShowFolder(file_id)),
                        );
                    }
                    Some(f) if f.downloading => {
                        actions = actions.push(
                            button(text("Отмена").size(12))
                                .style(button::secondary)
                                .on_press(Msg::FileCancel(file_id)),
                        );
                    }
                    _ if !m.pending => {
                        actions = actions.push(
                            button(text("Скачать").size(12))
                                .style(button::secondary)
                                .on_press(Msg::FileDownload(file_id)),
                        );
                    }
                    _ => {}
                }
                let kind = std::path::Path::new(name)
                    .extension()
                    .and_then(|e| e.to_str())
                    .map_or_else(|| "ФАЙЛ".to_owned(), str::to_uppercase);
                row![
                    container(text(kind).size(11))
                        .padding(6)
                        .center_y(32)
                        .style(container::bordered_box),
                    column![text(name.as_str()).size(14), text(status).size(11)]
                        .spacing(2)
                        .width(Fill),
                    actions,
                ]
                .spacing(8)
                .align_y(iced::Center)
                .into()
            }
            Media::Voice {
                duration, waveform, ..
            } => self.view_voice(file_id, *duration, waveform),
            Media::Sticker {
                kind,
                width,
                height,
                emoji,
                ..
            } => {
                let (w, h) = Media::sticker_box(*width, *height);
                let still = match kind {
                    Motion::Still => self.session.images.peek(file_id).cloned(),
                    _ => self.session.playback.frame(file_id).cloned(),
                };
                let picture: Element<'_, Msg> = match still {
                    Some(handle) => image(handle).width(w).height(h).into(),
                    None => container(text(emoji.as_str()).size(48))
                        .width(w)
                        .height(h)
                        .center(Fill)
                        .into(),
                };
                let sensor = sensor(picture).anticipate(200);
                match kind {
                    Motion::Still => {
                        let owner = PhotoOwner::Message(m.chat_id, m.id, PhotoKind::Sticker);
                        sensor
                            .key((m.chat_id, m.id, file_id, epoch))
                            .on_show(move |_| {
                                Msg::Pane(window, PaneMsg::MediaShown(owner, file_id, epoch))
                            })
                            .on_hide(Msg::Pane(window, PaneMsg::MediaGone(owner, file_id, epoch)))
                            .into()
                    }
                    motion => {
                        let motion = *motion;
                        sensor
                            .key((m.chat_id, m.id, file_id))
                            .on_show(move |_| Msg::AnimShow(file_id, motion))
                            .on_hide(Msg::AnimHide(file_id))
                            .into()
                    }
                }
            }
            Media::Video {
                mini,
                thumb,
                duration,
                spoiler,
                ..
            }
            | Media::Animation {
                mini,
                thumb,
                spoiler,
                duration,
                ..
            } => {
                let (w, h) = media.display_box().unwrap_or((PHOTO_MAX_W, PHOTO_MAX_H));
                let animated = matches!(media, Media::Animation { .. });
                let round = matches!(media, Media::Video { round: true, .. });
                if *spoiler && !pane.revealed.contains(&m.id) {
                    return spoiler_cover(window, m.id, mini.as_ref(), w, h);
                }
                let animation_frame = animated
                    .then(|| self.session.playback.frame(file_id))
                    .flatten();
                let frame = animation_frame
                    .or_else(|| thumb.and_then(|t| self.session.images.peek(t)))
                    .or(mini.as_ref());
                // Animations: the previous frame under the current one, so a
                // frame iced has not loaded yet shows the last one, not a gap.
                let under = animated
                    .then(|| self.session.playback.previous_frame(file_id))
                    .flatten();
                let layer = |handle: &image::Handle| {
                    image(handle.clone())
                        .width(w)
                        .height(h)
                        .content_fit(ContentFit::Cover)
                        .border_radius(if round { w / 2.0 } else { 0.0 })
                };
                let picture: Element<'_, Msg> = match frame {
                    Some(handle) => match under {
                        Some(prev) => stack![layer(prev), layer(handle)].into(),
                        None => layer(handle).into(),
                    },
                    None => container(space())
                        .width(w)
                        .height(h)
                        .style(move |theme| {
                            let mut style = container::dark(theme);
                            if round {
                                style.border.radius = (w / 2.0).into();
                            }
                            style
                        })
                        .into(),
                };
                let picture: Element<'_, Msg> = if animated && animation_frame.is_none() {
                    if let Some(t) = *thumb {
                        let owner = PhotoOwner::Message(m.chat_id, m.id, PhotoKind::Thumbnail);
                        sensor(picture)
                            .key((m.chat_id, m.id, t, epoch))
                            .anticipate(200)
                            .on_show(move |_| {
                                Msg::Pane(window, PaneMsg::MediaShown(owner, t, epoch))
                            })
                            .on_hide(Msg::Pane(window, PaneMsg::MediaGone(owner, t, epoch)))
                            .into()
                    } else {
                        picture
                    }
                } else {
                    picture
                };
                let mut label = if animated {
                    String::new()
                } else {
                    format!("▶ {}", mmss(*duration))
                };
                if let Some(f) = file.filter(|f| f.downloading && !f.done && !animated) {
                    label = format!("Загрузка {}%", f.percent());
                }
                let mut layers = stack![picture];
                if !label.is_empty() {
                    layers = layers.push(
                        container(
                            container(text(label).size(13))
                                .padding([4, 8])
                                .style(container::dark),
                        )
                        .center(Fill)
                        .width(w)
                        .height(h),
                    );
                }
                // A video playing here is drawn by the player (unless it
                // is expanded over the window).
                if !animated
                    && let Some(video) = self.video_of(window, m.chat_id, m.id, file_id, round)
                    && !video.expanded
                {
                    return self.view_video(video, w, h);
                }
                let thumb = *thumb;
                // Videos play here; a GIF click starts its inline animation.
                let play = if animated {
                    Msg::AnimPlay(file_id)
                } else {
                    Msg::Video(super::video::VideoMsg::Play(
                        window, m.chat_id, m.id, file_id, round,
                    ))
                };
                let clickable = mouse_area(layers)
                    .interaction(iced::mouse::Interaction::Pointer)
                    .on_press(play);
                let sensor = sensor(clickable).anticipate(200);
                if animated {
                    sensor
                        .key((m.chat_id, m.id, file_id, thumb))
                        .on_show(move |_| Msg::AnimShow(file_id, Motion::Video))
                        .on_hide(Msg::AnimHide(file_id))
                        .into()
                } else {
                    let owner = PhotoOwner::Message(m.chat_id, m.id, PhotoKind::Thumbnail);
                    sensor
                        .key((m.chat_id, m.id, file_id, thumb, epoch))
                        .on_show(move |_| match thumb {
                            Some(t) => Msg::Pane(window, PaneMsg::MediaShown(owner, t, epoch)),
                            None => Msg::Ignore,
                        })
                        .on_hide(match thumb {
                            Some(t) => Msg::Pane(window, PaneMsg::MediaGone(owner, t, epoch)),
                            None => Msg::Ignore,
                        })
                        .into()
                }
            }
        }
    }

    /// ▶/⏸, loudness bars (played part highlighted) and time.
    fn view_voice<'a>(
        &'a self,
        file_id: i32,
        duration: i32,
        waveform: &'a [u8],
    ) -> Element<'a, Msg> {
        let state = self.session.playback.voice.filter(|v| v.0 == file_id);
        let playing = state.is_some_and(|s| s.2);
        let position = state.map_or(0.0, |s| s.1.as_secs_f32());
        let progress = if duration > 0 {
            (position / duration as f32).clamp(0.0, 1.0)
        } else {
            0.0
        };
        const BARS: usize = 40;
        let bars = (0..BARS).map(|i| {
            let value = if waveform.is_empty() {
                8
            } else {
                waveform[i * waveform.len() / BARS]
            };
            let played = (i as f32 + 0.5) / BARS as f32 <= progress;
            container(space())
                .width(3)
                .height(4.0 + f32::from(value))
                .style(if played {
                    container::primary
                } else {
                    container::secondary
                })
                .into()
        });
        let time = if state.is_some() {
            format!("{} / {}", mmss(position as i32), mmss(duration))
        } else {
            mmss(duration)
        };
        let loading = !playing
            && self
                .session
                .files
                .get(&file_id)
                .is_some_and(|f| f.downloading && !f.done);
        row![
            button(text(if playing { "⏸" } else { "▶" }).size(16))
                .style(button::secondary)
                .on_press(Msg::VoiceToggle(file_id)),
            row(bars).spacing(2).align_y(iced::Center),
            text(if loading {
                "загрузка…".to_owned()
            } else {
                time
            })
            .size(12),
        ]
        .spacing(8)
        .align_y(iced::Center)
        .into()
    }
}

/// Blurred preview with a hint; a click reveals the media.
fn spoiler_cover<'a>(
    window: WinId,
    id: i64,
    mini: Option<&image::Handle>,
    w: f32,
    h: f32,
) -> Element<'a, Msg> {
    let cover: Element<'_, Msg> = match mini {
        Some(handle) => image(handle.clone())
            .width(w)
            .height(h)
            .content_fit(ContentFit::Cover)
            .into(),
        None => container(space()).width(w).height(h).into(),
    };
    mouse_area(stack![
        cover,
        container(text("Спойлер — нажмите, чтобы показать").size(12))
            .center(Fill)
            .width(w)
            .height(h)
            .style(container::dark),
    ])
    .interaction(iced::mouse::Interaction::Pointer)
    .on_press(Msg::Pane(window, PaneMsg::Reveal(id)))
    .into()
}

fn mmss(secs: i32) -> String {
    let secs = secs.max(0);
    format!("{}:{:02}", secs / 60, secs % 60)
}

/// Actions for a selection are shown only on a selected bubble's right press.
fn view_selected_menu(window: WinId, pane: &ChatPane) -> Element<'_, Msg> {
    let action = |label, message| {
        button(text(label).size(14))
            .width(Fill)
            .style(menu_button)
            .on_press(Msg::Pane(window, message))
    };
    let mut items = column![];
    match pane.delete_selection {
        None => {
            let ids: Vec<i64> = pane
                .selected
                .as_ref()
                .map_or_else(Vec::new, |selected| selected.iter().copied().collect());
            items = items
                .push(action("Копировать", PaneMsg::CopySelection))
                .push(action("Переслать", PaneMsg::Forward(ids)))
                .push(action("Удалить", PaneMsg::DeleteSelection));
        }
        Some(None) => items = items.push(text("…").size(14)),
        Some(Some(rights)) => {
            if !rights.delete_for_self && !rights.delete_for_all {
                items = items.push(text("эти сообщения удалить нельзя").size(13));
            }
            if rights.delete_for_self {
                items = items.push(action(
                    "Удалить у меня",
                    PaneMsg::DeleteSelected { revoke: false },
                ));
            }
            if rights.delete_for_all {
                items = items.push(action(
                    "Удалить у всех",
                    PaneMsg::DeleteSelected { revoke: true },
                ));
            }
        }
    }
    items = items.push(action("Отмена", PaneMsg::CloseMenu));
    container(items.spacing(2))
        .padding(4)
        .width(250)
        .style(|theme: &iced::Theme| {
            let palette = theme.extended_palette();
            let mut surface = palette.background.strong.color;
            surface.a = 0.88;
            container::Style {
                background: Some(surface.into()),
                text_color: Some(palette.background.base.text),
                border: iced::border::rounded(7)
                    .width(1)
                    .color(palette.background.strongest.color),
                ..container::Style::default()
            }
        })
        .into()
}

fn view_menu<'a>(
    window: WinId,
    m: &'a MsgItem,
    menu: &'a Menu,
    pinned_locally: bool,
) -> Element<'a, Msg> {
    let item = |label: &'a str, msg: PaneMsg| {
        button(text(label).size(14))
            .width(Fill)
            .style(menu_button)
            .on_press(Msg::Pane(window, msg))
    };
    let mut items = column![];
    let select_item = (!m.pending).then(|| item("Выбрать", PaneMsg::Select(m.id)));
    if !menu.reactions.is_empty() && !m.deleted && !m.pending {
        let quick = menu.reactions.iter().map(|emoji| {
            button(text(emoji.as_str()).size(18).font(rich::EMOJI_FONT))
                .padding([2, 4])
                .style(menu_button)
                .on_press(Msg::Pane(
                    window,
                    PaneMsg::React(m.id, crate::td::ReactionKey::Emoji(emoji.clone())),
                ))
                .into()
        });
        items = items.push(row(quick).spacing(2).wrap());
    }
    items = items.push(item("Копировать", PaneMsg::CopyText(m.id)));
    if !m.deleted && !m.pending {
        items = items.push(item("Ответить", PaneMsg::Reply(m.id)));
    }
    match menu.rights {
        None => items = items.push(text("…").size(14)),
        Some(_) if m.deleted => {
            items = items.push(item("Убрать из архива", PaneMsg::Forget(m.id)));
        }
        Some(rights) => {
            if rights.edit && m.outgoing && m.media.is_none() && !m.pending {
                items = items.push(item("Редактировать", PaneMsg::StartEdit(m.id)));
            }
            if rights.delete_for_self {
                items = items.push(item(
                    "Удалить у меня",
                    PaneMsg::Delete {
                        id: m.id,
                        revoke: false,
                    },
                ));
            }
            if rights.delete_for_all {
                items = items.push(
                    item(
                        "Удалить у всех",
                        PaneMsg::Delete {
                            id: m.id,
                            revoke: true,
                        },
                    )
                    .style(|theme, status| {
                        let mut style = menu_button(theme, status);
                        let danger = theme.extended_palette().danger.base;
                        style.text_color = danger.color;
                        if matches!(status, button::Status::Hovered | button::Status::Pressed) {
                            style.background =
                                Some(theme.extended_palette().danger.weak.color.into());
                            style.text_color = theme.extended_palette().danger.weak.text;
                        }
                        style
                    }),
                );
            }
        }
    }
    if !m.deleted && !m.pending {
        items = items.push(item("Переслать", PaneMsg::Forward(vec![m.id])));
    }
    // Works in any chat or channel: the pin is kept on this computer only.
    if !m.pending {
        items = items.push(if pinned_locally {
            item("Открепить (для себя)", PaneMsg::PinLocal(m.id, false))
        } else {
            item("Закрепить для себя", PaneMsg::PinLocal(m.id, true))
        });
    }
    if let Some(select) = select_item {
        items = items.push(select);
    }
    items = items.push(item("Отмена", PaneMsg::CloseMenu));
    container(items.spacing(2))
        .padding(4)
        .width(250)
        .style(|theme: &iced::Theme| {
            let palette = theme.extended_palette();
            let mut surface = palette.background.strong.color;
            surface.a = 0.88;
            container::Style {
                background: Some(surface.into()),
                text_color: Some(palette.background.base.text),
                border: iced::border::rounded(7)
                    .width(1)
                    .color(palette.background.strongest.color),
                ..container::Style::default()
            }
        })
        .into()
}

fn menu_button(theme: &iced::Theme, status: button::Status) -> button::Style {
    let palette = theme.extended_palette();
    let pair = match status {
        button::Status::Pressed => palette.primary.strong,
        button::Status::Hovered => palette.background.strongest,
        _ => palette.background.weak,
    };
    button::Style {
        background: Some(pair.color.into()),
        text_color: if matches!(status, button::Status::Disabled) {
            palette.background.base.text
        } else {
            pair.text
        },
        border: iced::border::rounded(4)
            .width(1)
            .color(palette.background.strongest.color),
        ..button::Style::default()
    }
}

/// Bubble of the user's own message.
fn own_box(theme: &iced::Theme) -> container::Style {
    let pair = theme.extended_palette().primary.weak;
    container::Style {
        background: Some(pair.color.into()),
        text_color: Some(pair.text),
        ..container::rounded_box(theme)
    }
}

/// Reaction under a message; the user's own stands out.
fn reaction_chip(theme: &iced::Theme, status: button::Status, chosen: bool) -> button::Style {
    let palette = theme.extended_palette();
    let pair = if chosen {
        palette.primary.base
    } else if matches!(status, button::Status::Hovered | button::Status::Pressed) {
        palette.background.strongest
    } else {
        palette.background.strong
    };
    button::Style {
        background: Some(pair.color.into()),
        text_color: pair.text,
        border: iced::border::rounded(10),
        ..button::Style::default()
    }
}

/// Dot for an own message: gray when sent, light blue when read, faint
/// while still sending; a red "!" when the server rejected the send.
pub(super) fn receipt_dot<'a>(receipt: Receipt) -> Element<'a, Msg> {
    let (glyph, color) = match receipt {
        Receipt::Sending => ("●", iced::Color::from_rgba8(0x9A, 0xA0, 0xA6, 0.45)),
        Receipt::Sent => ("●", iced::Color::from_rgb8(0x9A, 0xA0, 0xA6)),
        Receipt::Read => ("●", iced::Color::from_rgb8(0x7F, 0xD1, 0xFF)),
        Receipt::Failed => ("!", iced::Color::from_rgb8(0xE5, 0x5B, 0x5B)),
    };
    text(glyph).size(10).color(color).into()
}

/// Small faded text in the bubble's text color.
fn footnote<'a>(label: String, own: bool) -> Element<'a, Msg> {
    text(label)
        .size(11)
        .style(move |theme: &iced::Theme| {
            let palette = theme.extended_palette();
            let pair = if own {
                palette.primary.weak
            } else {
                palette.background.weak
            };
            text::Style {
                color: Some(iced::Color {
                    a: 0.7,
                    ..pair.text
                }),
            }
        })
        .into()
}

/// Local calendar day of a unix time.
fn day(date: i32) -> Option<chrono::NaiveDate> {
    use chrono::TimeZone;
    chrono::Local
        .timestamp_opt(i64::from(date), 0)
        .single()
        .map(|d| d.date_naive())
}

/// «Сегодня», «Вчера», «24 сентября», «24 сентября 2024».
fn day_label(date: i32) -> String {
    let today = chrono::Local::now().date_naive();
    day(date).map_or_else(String::new, |d| day_label_at(d, today))
}

fn day_label_at(d: chrono::NaiveDate, today: chrono::NaiveDate) -> String {
    use chrono::Datelike;
    const MONTHS: [&str; 12] = [
        "января",
        "февраля",
        "марта",
        "апреля",
        "мая",
        "июня",
        "июля",
        "августа",
        "сентября",
        "октября",
        "ноября",
        "декабря",
    ];
    if d == today {
        return "Сегодня".into();
    }
    if today.pred_opt() == Some(d) {
        return "Вчера".into();
    }
    let month = MONTHS[d.month0() as usize];
    if d.year() == today.year() {
        format!("{} {month}", d.day())
    } else {
        format!("{} {month} {}", d.day(), d.year())
    }
}

/// Send time, local "14:05"; the day is on the separator above.
fn stamp(date: i32) -> String {
    use chrono::TimeZone;
    chrono::Local
        .timestamp_opt(i64::from(date), 0)
        .single()
        .map_or_else(String::new, |d| d.format("%H:%M").to_string())
}

/// Description text with its links and @mentions clickable.
fn linked_text<'a>(
    theme: &iced::Theme,
    window: WinId,
    pieces: &'a [rich::Piece],
) -> Element<'a, Msg> {
    // The accent, deviated for contrast against the background: plain
    // `primary` is the own-bubble tint and would not stand out here.
    let link_color = theme.extended_palette().primary.strong.color;
    let spans: Vec<text::Span<'a, Link>> = pieces
        .iter()
        .map(|piece| {
            let mut s = span(piece.text.as_str());
            if let Some(url) = &piece.style.link {
                s = s
                    .link(Link::Url {
                        url: url.clone(),
                        hidden: piece.style.link_hidden,
                    })
                    .color(link_color);
            }
            s
        })
        .collect();
    rich_text(spans)
        .size(14)
        .on_link_click(move |link| Msg::Pane(window, PaneMsg::Link(link)))
        .into()
}

#[cfg(test)]
mod stamp_tests {
    use chrono::NaiveDate;

    #[test]
    fn day_separators_read_naturally() {
        let d = |y, m, day| NaiveDate::from_ymd_opt(y, m, day).unwrap();
        let today = d(2026, 9, 25);
        assert_eq!(super::day_label_at(d(2026, 9, 25), today), "Сегодня");
        assert_eq!(super::day_label_at(d(2026, 9, 24), today), "Вчера");
        assert_eq!(super::day_label_at(d(2026, 3, 1), today), "1 марта");
        assert_eq!(
            super::day_label_at(d(2025, 12, 31), today),
            "31 декабря 2025"
        );
    }
}

#[cfg(test)]
mod multiline_security_tests {
    use super::*;
    use iced::Point;
    use iced_runtime::user_interface::{Cache, UserInterface};

    #[test]
    fn security_fix_multiline_code_click_copies_whole_piece_and_selection_keeps_newline() {
        let content = "код 😀\nещё код";
        let item = MsgItem {
            chat_id: 1,
            id: 10,
            sender: MessageSender::User(tdlib_rs::types::MessageSenderUser { user_id: 7 }),
            text: content.into(),
            outgoing: false,
            deleted: false,
            media: None,
            pending: false,
            failed: false,
            date: 0,
            edited: false,
            reactions: Vec::new(),
            forwarded: None,
            extra: None,
            sender_tag: String::new(),
            rich: vec![rich::Piece {
                text: content.into(),
                style: rich::Style {
                    code: true,
                    ..Default::default()
                },
            }],
            reply_to: None,
        };
        let window = WinId::unique();
        let mut renderer = super::super::sandbox::renderer();
        let screen = iced::Size::new(400.0, 90.0);
        let dispatch = |at: Point, event: iced::Event, selection, renderer: &mut iced::Renderer| {
            let mut output = Vec::new();
            let mut ui = UserInterface::build(
                view_rich(window, &item, false, selection),
                screen,
                Cache::default(),
                renderer,
            );
            let _ = ui.update(
                &[event],
                iced::mouse::Cursor::Available(at),
                renderer,
                &mut iced::advanced::clipboard::Null,
                &mut output,
            );
            output
        };
        let press =
            iced::Event::Mouse(iced::mouse::Event::ButtonPressed(iced::mouse::Button::Left));
        let first = dispatch(Point::new(0.0, 8.0), press.clone(), None, &mut renderer);
        let anchor = first
            .iter()
            .find_map(|event| match event {
                Msg::Pane(_, PaneMsg::TextPress(10, at)) => Some(*at),
                _ => None,
            })
            .expect("first code line receives selection press");
        assert_eq!(anchor, 0);

        let second_line = content.find('\n').unwrap() + 1;
        let y = (16..70)
            .step_by(2)
            .find(|&y| {
                dispatch(
                    Point::new(1.0, y as f32),
                    press.clone(),
                    None,
                    &mut renderer,
                )
                .iter()
                .any(|event| matches!(event, Msg::Pane(_, PaneMsg::TextPress(10, at)) if *at == second_line))
            })
            .expect("the second rendered line begins after the newline");
        let point = Point::new(8.0, y as f32);
        let mut output = Vec::new();
        let mut ui = UserInterface::build(
            view_rich(window, &item, false, None),
            screen,
            Cache::default(),
            &mut renderer,
        );
        for event in [
            iced::mouse::Event::CursorMoved { position: point },
            iced::mouse::Event::ButtonPressed(iced::mouse::Button::Left),
            iced::mouse::Event::ButtonReleased(iced::mouse::Button::Left),
        ] {
            let _ = ui.update(
                &[iced::Event::Mouse(event)],
                iced::mouse::Cursor::Available(point),
                &mut renderer,
                &mut iced::advanced::clipboard::Null,
                &mut output,
            );
        }
        assert!(
            output.iter().any(|event| matches!(
                event,
                Msg::Pane(_, PaneMsg::Link(Link::Copy(copied))) if copied.as_ref() == content
            )),
            "clicking only the second line must copy the whole code piece"
        );

        let dragging = super::super::pane::TextSelection {
            message: item.id,
            anchor,
            focus: anchor,
            dragging: true,
        };
        let dragged = dispatch(
            Point::new(350.0, y as f32),
            iced::Event::Mouse(iced::mouse::Event::CursorMoved {
                position: Point::new(350.0, y as f32),
            }),
            Some(&dragging),
            &mut renderer,
        );
        let focus = dragged
            .iter()
            .find_map(|event| match event {
                Msg::Pane(_, PaneMsg::TextDrag(10, at)) => Some(*at),
                _ => None,
            })
            .expect("dragging into the second line reports its byte offset");
        assert_eq!(focus, content.len());
        assert_eq!(
            super::super::selectable::selected(content, anchor, focus),
            content
        );
    }
}

#[cfg(test)]
mod requested_changes_gesture_tests {
    use super::*;
    use iced::{Event, Point, Size, mouse};
    use iced_runtime::user_interface::{Cache, UserInterface};
    use std::collections::{BTreeMap, BTreeSet};

    const SIZE: Size = Size::new(1000.0, 700.0);

    fn conversation() -> App {
        let mut app = super::super::tests::app();
        let window = app.main_window;
        app.session
            .chats
            .insert(1, super::super::tests::chat_item("Private chat", true));
        app.session.panes.get_mut(&window).unwrap().switch_to(1);
        let history = [
            (10, "first message with enough text to reach the pointer"),
            (11, "second message with enough text to reach the pointer"),
            (12, "third message with enough text to reach the pointer"),
        ]
        .into_iter()
        .map(|(id, text)| {
            serde_json::from_value(super::super::tests::message(1, id, text)).unwrap()
        })
        .collect();
        let _ = app.update(Msg::Pane(window, PaneMsg::HistoryLoaded(1, Ok(history))));
        app
    }

    struct Pointer {
        renderer: iced::Renderer,
        cache: Cache,
    }

    impl Pointer {
        fn new() -> Self {
            Self {
                renderer: super::super::sandbox::renderer(),
                cache: Cache::default(),
            }
        }

        fn events(&mut self, app: &App, at: Point, events: &[mouse::Event]) -> Vec<Msg> {
            let mut output = Vec::new();
            let mut ui = UserInterface::build(
                app.view(app.main_window),
                SIZE,
                std::mem::take(&mut self.cache),
                &mut self.renderer,
            );
            for event in events {
                let _ = ui.update(
                    &[Event::Mouse(*event)],
                    mouse::Cursor::Available(at),
                    &mut self.renderer,
                    &mut iced::advanced::clipboard::Null,
                    &mut output,
                );
            }
            self.cache = ui.into_cache();
            output
        }

        fn send(&mut self, app: &mut App, at: Point, event: mouse::Event) {
            for message in self.events(app, at, std::slice::from_ref(&event)) {
                let _ = app.update(message);
            }
            if matches!(event, mouse::Event::ButtonReleased(mouse::Button::Left)) {
                // `event::listen_with` delivers this even when a widget captured release.
                let _ = app.update(Msg::MouseReleased);
            }
        }

        fn click(&mut self, app: &mut App, at: Point) {
            self.send(app, at, mouse::Event::ButtonPressed(mouse::Button::Left));
            self.send(app, at, mouse::Event::ButtonReleased(mouse::Button::Left));
        }
    }

    // Probe laid-out bubbles with real right presses, without dispatching the
    // probe events to App (and therefore without opening a menu).
    fn bubbles(app: &App, pointer: &mut Pointer) -> BTreeMap<i64, (f32, f32)> {
        bubbles_at(app, pointer, 422.0)
    }

    fn bubbles_at(app: &App, pointer: &mut Pointer, x: f32) -> BTreeMap<i64, (f32, f32)> {
        let mut hits = BTreeMap::new();
        for y in (100..610).step_by(2) {
            let at = Point::new(x, y as f32);
            for message in pointer.events(
                app,
                at,
                &[mouse::Event::ButtonPressed(mouse::Button::Right)],
            ) {
                if let Msg::Pane(_, PaneMsg::OpenMenu(id)) = message {
                    let range = hits.entry(id).or_insert((y as f32, y as f32));
                    range.1 = y as f32;
                }
            }
        }
        for id in [10, 11, 12] {
            assert!(hits.contains_key(&id), "message {id} must be visible");
        }
        hits
    }

    #[test]
    fn requested_changes_two_left_clicks_reply_to_the_message_but_one_does_not() {
        let mut app = conversation();
        let mut pointer = Pointer::new();
        let hit = bubbles(&app, &mut pointer);
        let (top, bottom) = hit[&11];
        let at = Point::new(422.0, (top + bottom) / 2.0);
        let window = app.main_window;

        pointer.click(&mut app, at);
        assert_eq!(app.session.panes[&window].reply_to, None);
        pointer.click(&mut app, at);
        let pane = &app.session.panes[&window];
        assert_eq!(
            pane.reply_to,
            Some(11),
            "the composer must answer the clicked message"
        );
        assert!(
            pane.menu.is_none(),
            "a double left click does not open actions"
        );
    }

    #[test]
    fn requested_changes_background_rectangle_selects_only_intersected_bubbles_without_menu() {
        let mut app = conversation();
        let mut pointer = Pointer::new();
        let hit = bubbles(&app, &mut pointer);
        let window = app.main_window;
        let before = app.session.panes[&window].visible();
        let start = Point::new(900.0, hit[&10].0 + 2.0);
        let end = Point::new(422.0, hit[&11].1 - 2.0);
        assert!(
            end.y < hit[&12].0,
            "third bubble must stay outside the rectangle"
        );

        pointer.send(
            &mut app,
            start,
            mouse::Event::ButtonPressed(mouse::Button::Left),
        );
        pointer.send(&mut app, end, mouse::Event::CursorMoved { position: end });
        pointer.send(
            &mut app,
            end,
            mouse::Event::ButtonReleased(mouse::Button::Left),
        );
        let pane = &app.session.panes[&window];
        assert_eq!(pane.selected, Some(BTreeSet::from([10, 11])));
        assert!(
            pane.menu.is_none(),
            "release highlights; it does not open actions"
        );
        let after = pane.visible();
        assert_eq!((after.start, after.end), (before.start, before.end));
        assert_eq!((after.above, after.below), (before.above, before.below));

        pointer.click(&mut app, Point::new(900.0, 500.0));
        assert!(
            app.session.panes[&window].selected.is_none(),
            "a left click on empty history must clear the message selection"
        );
    }

    #[test]
    fn requested_changes_rectangle_retracted_to_its_anchor_selects_nothing() {
        let mut app = conversation();
        let mut pointer = Pointer::new();
        let hit = bubbles(&app, &mut pointer);
        let window = app.main_window;
        let start = Point::new(900.0, hit[&10].0 + 2.0);
        let across = Point::new(422.0, hit[&11].1 - 2.0);
        let back = Point::new(start.x - 1.0, start.y + 1.0);

        pointer.send(
            &mut app,
            start,
            mouse::Event::ButtonPressed(mouse::Button::Left),
        );
        pointer.send(
            &mut app,
            across,
            mouse::Event::CursorMoved { position: across },
        );
        pointer.send(&mut app, back, mouse::Event::CursorMoved { position: back });
        pointer.send(
            &mut app,
            back,
            mouse::Event::ButtonReleased(mouse::Button::Left),
        );
        let pane = &app.session.panes[&window];
        assert_eq!(
            pane.selected, None,
            "moving the rectangle back inside the click threshold must not commit stale hits"
        );
        assert!(pane.menu.is_none());
    }

    #[test]
    fn requested_changes_right_click_on_selected_message_forwards_the_whole_selection() {
        let mut app = conversation();
        let mut pointer = Pointer::new();
        let hit = bubbles(&app, &mut pointer);
        let window = app.main_window;
        // Selection is the precondition of this test; the rectangle gesture
        // is exercised separately, so a menu failure stays independent of it.
        for id in [10, 11] {
            let _ = app.update(Msg::Pane(window, PaneMsg::Select(id)));
        }
        assert_eq!(
            app.session.panes[&window].selected,
            Some(BTreeSet::from([10, 11]))
        );
        assert!(app.session.panes[&window].menu.is_none());

        let (top, bottom) = hit[&11];
        pointer.send(
            &mut app,
            Point::new(422.0, (top + bottom) / 2.0),
            mouse::Event::ButtonPressed(mouse::Button::Right),
        );
        assert!(
            app.session.panes[&window].menu.is_some(),
            "only right click opens actions for the selection"
        );

        // The floating actions live outside the scrollable (ADR 004). Find
        // its actual button by pointer events, not a fixed menu row offset.
        let action = (100..610).step_by(3).find_map(|y| {
            pointer
                .events(
                    &app,
                    Point::new(422.0, y as f32),
                    &[
                        mouse::Event::ButtonPressed(mouse::Button::Left),
                        mouse::Event::ButtonReleased(mouse::Button::Left),
                    ],
                )
                .into_iter()
                .find(|event| matches!(event, Msg::Pane(_, PaneMsg::Forward(_))))
        });
        let action = action.expect("the floating menu exposes a clickable forward action");
        let _ = app.update(action);
        assert_eq!(
            app.session.panes[&window]
                .forward
                .as_ref()
                .map(|(ids, _)| ids.as_slice()),
            Some([10, 11].as_slice()),
            "forwarding from the selected bubble's menu must address both messages"
        );
    }

    #[test]
    fn requested_changes_second_press_can_turn_into_text_drag_without_replying() {
        let mut app = conversation();
        let mut pointer = Pointer::new();
        let hit = bubbles(&app, &mut pointer);
        let window = app.main_window;
        let (top, bottom) = hit[&11];
        let y = (top + bottom) / 2.0;
        let from = Point::new(394.0, y);
        let to = Point::new(572.0, y);

        pointer.click(&mut app, from);
        assert_eq!(app.session.panes[&window].reply_to, None);
        pointer.send(
            &mut app,
            from,
            mouse::Event::ButtonPressed(mouse::Button::Left),
        );
        assert_eq!(
            app.session.panes[&window].reply_to, None,
            "the second press is not a completed double click"
        );
        pointer.send(&mut app, to, mouse::Event::CursorMoved { position: to });
        pointer.send(
            &mut app,
            to,
            mouse::Event::ButtonReleased(mouse::Button::Left),
        );
        let pane = &app.session.panes[&window];
        assert_eq!(
            pane.reply_to, None,
            "dragging after the second press cancels the double-click reply"
        );
        assert!(
            pane.selected_text().is_some(),
            "the drag still selects text"
        );
        assert!(
            pane.selected.is_none(),
            "a text drag does not select bubbles"
        );
    }

    #[test]
    fn requested_changes_dragging_message_text_remains_text_selection() {
        let mut app = conversation();
        let mut pointer = Pointer::new();
        let hit = bubbles(&app, &mut pointer);
        let window = app.main_window;
        let (top, bottom) = hit[&11];
        let y = (top + bottom) / 2.0;
        let from = Point::new(394.0, y);
        let to = Point::new(572.0, y);

        pointer.send(
            &mut app,
            from,
            mouse::Event::ButtonPressed(mouse::Button::Left),
        );
        assert_eq!(
            app.session.panes[&window]
                .text_selection
                .as_ref()
                .map(|s| s.message),
            Some(11),
            "text press starts text selection, not rectangle selection"
        );
        pointer.send(&mut app, to, mouse::Event::CursorMoved { position: to });
        pointer.send(
            &mut app,
            to,
            mouse::Event::ButtonReleased(mouse::Button::Left),
        );
        let pane = &app.session.panes[&window];
        assert!(
            pane.selected_text().is_some(),
            "dragging across the text selects actual characters"
        );
        assert!(
            pane.selected.is_none(),
            "text drag must not select message bubbles"
        );
    }
    /// Optional visual proof using the real group fixture and the same iced
    /// pointer driver as the behavior tests. Explicit run only: writes PNGs.
    #[test]
    #[ignore = "writes target/sandbox gesture screenshots; run explicitly"]
    fn sandbox_requested_changes_group_gestures() {
        let make_group = || {
            let mut app = super::super::sandbox::world();
            super::super::sandbox::open_with(
                &mut app,
                1,
                vec![
                    super::super::sandbox::msg(
                        1,
                        10,
                        7,
                        0,
                        "First group message, available for rectangle selection",
                    ),
                    super::super::sandbox::msg(
                        1,
                        11,
                        8,
                        0,
                        "Second group message, available for rectangle selection",
                    ),
                    super::super::sandbox::msg(
                        1,
                        12,
                        9,
                        0,
                        "Third group message remains outside the rectangle",
                    ),
                ],
            );
            app
        };
        let snapshot = |name: &str, app: &mut App| {
            let window = app.main_window;
            let pixels = super::super::sandbox::run_frames(
                app,
                window,
                SIZE,
                4,
                &mut super::super::sandbox::renderer(),
            );
            let path = super::super::sandbox::save(name, SIZE, pixels);
            println!("{}", path.display());
        };
        let mut app = make_group();
        let mut pointer = Pointer::new();
        let hits = bubbles_at(&app, &mut pointer, 492.0);
        let start = Point::new(900.0, hits[&10].0 + 2.0);
        let across = Point::new(492.0, hits[&11].1 - 2.0);
        pointer.send(
            &mut app,
            start,
            mouse::Event::ButtonPressed(mouse::Button::Left),
        );
        pointer.send(
            &mut app,
            across,
            mouse::Event::CursorMoved { position: across },
        );
        assert!(app.session.panes[&app.main_window].rectangle.is_some());
        snapshot("gesture-group-rectangle-drag", &mut app);

        pointer.send(
            &mut app,
            across,
            mouse::Event::ButtonReleased(mouse::Button::Left),
        );
        assert_eq!(
            app.session.panes[&app.main_window].selected,
            Some(BTreeSet::from([10, 11]))
        );
        let hits = bubbles_at(&app, &mut pointer, 492.0);
        let (top, bottom) = hits[&11];
        pointer.send(
            &mut app,
            Point::new(492.0, (top + bottom) / 2.0),
            mouse::Event::ButtonPressed(mouse::Button::Right),
        );
        assert!(app.session.panes[&app.main_window].menu.is_some());
        snapshot("gesture-group-selected-menu", &mut app);
        pointer.click(&mut app, Point::new(900.0, 500.0));
        assert!(app.session.panes[&app.main_window].selected.is_none());
        assert!(app.session.panes[&app.main_window].menu.is_none());
        snapshot("gesture-group-background-deselected", &mut app);

        let mut app = make_group();
        let mut pointer = Pointer::new();
        let hits = bubbles_at(&app, &mut pointer, 492.0);
        let (top, bottom) = hits[&11];
        let at = Point::new(492.0, (top + bottom) / 2.0);
        pointer.click(&mut app, at);
        pointer.click(&mut app, at);
        assert_eq!(app.session.panes[&app.main_window].reply_to, Some(11));
        snapshot("gesture-group-double-reply", &mut app);
    }
}
