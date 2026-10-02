//! Chat list, folder tabs, account switcher, and settings views.

use iced::widget::{
    button, column, container, mouse_area, rich_text, row, scrollable, slider, space, span, text,
    text_input, toggler, tooltip,
};
use iced::{Element, Fill};

use super::avatars::Peer;
use super::pane::{ChatPane, Visible};
use super::rich;
use super::session::{FolderReorderState, SettingsSection};
use super::view::{one_line, receipt_dot, take_clusters};
use super::{App, Msg, PaneMsg, WinId};

/// Fixed row heights include the two-pixel column gap; spacers replace rows
/// without changing the scroll extent, even after thousands of pages.
const LIST_ROW_FULL: f32 = 52.0;
const LIST_ROW_COMPACT: f32 = 40.0;
const LIST_HEADER_HEIGHT: f32 = 38.0;
const LIST_MENU_HEIGHT: f32 = 186.0;
const LIST_FOLDER_MENU_HEIGHT: f32 = 222.0;
const LIST_PICKER_HEIGHT: f32 = 326.0;
const LIST_NEW_FOLDER_HEIGHT: f32 = 230.0;
const SEARCH_HEADING_HEIGHT: f32 = 20.0;
pub(super) const SEARCH_ROW_HEIGHT: f32 = 48.0;

/// Assumed chat list viewport height before it has reported its size.
pub(super) const DEFAULT_LIST_VIEW: f32 = 700.0;

/// Translate a viewport within a scrollable that has preceding content into
/// a result slice; callers construct widgets only for this slice.
pub(super) fn search_visible_rows(offset: f32, height: f32, prefix: f32, count: usize) -> Visible {
    if offset + height + height.max(400.0) < prefix {
        return Visible {
            start: 0,
            end: 0,
            above: 0.0,
            below: count as f32 * SEARCH_ROW_HEIGHT,
        };
    }
    list_visible_rows((offset - prefix).max(0.0), height, count, SEARCH_ROW_HEIGHT)
}

/// Virtualization for the chat list: a fixed-height list where only rows
/// overlapping the viewport plus a margin of roughly one screen above and
/// below are built; `above`/`below` are the exact heights of the rows left
/// out, so the scrollbar's total length stays correct.
fn list_visible_rows(offset: f32, height: f32, count: usize, row: f32) -> Visible {
    if count == 0 || row <= 0.0 {
        return Visible {
            start: 0,
            end: 0,
            above: 0.0,
            below: 0.0,
        };
    }
    let margin = height.max(400.0);
    let low = (offset - margin).max(0.0);
    let high = offset + height + margin;
    let start = ((low / row).floor() as usize).min(count);
    let end = ((high / row).ceil() as usize).clamp(start, count);
    Visible {
        start,
        end,
        above: start as f32 * row,
        below: (count - end) as f32 * row,
    }
}

impl App {
    /// Local preferences only reorder chats actually present in the TDLib list.
    pub(super) fn displayed_chat_ids(&self, window: WinId) -> Option<Vec<i64>> {
        let pane = self.session.panes.get(&window)?;
        if let Some(found) = self.filtered_chats(window) {
            return Some(found);
        }
        if let Some(folder) = pane.list.folder.filter(|_| !pane.list.archive) {
            return Some(
                self.session
                    .folder_lists
                    .get(&folder)
                    .unwrap_or(&self.session.empty_list)
                    .iter()
                    .map(|&(_, id)| id)
                    .collect(),
            );
        }
        let (list, pins) = if pane.list.archive {
            (&self.session.archived, &self.session.local_archive)
        } else {
            (&self.session.order, &self.session.local_main)
        };
        let mut ids = Vec::with_capacity(list.len());
        let mut local = std::collections::HashSet::with_capacity(pins.len());
        for &id in pins {
            if let Some(chat) = self.session.chats.get(&id) {
                let order = if pane.list.archive {
                    chat.archive_order
                } else {
                    chat.order
                };
                if order != 0 && list.contains(&(std::cmp::Reverse(order), id)) && local.insert(id)
                {
                    ids.push(id);
                }
            }
        }
        for pinned in [true, false] {
            ids.extend(list.iter().filter_map(|&(_, id)| {
                (!local.contains(&id) && self.session.chats.get(&id)?.pinned == pinned)
                    .then_some(id)
            }));
        }
        Some(ids)
    }

    fn local_chat_in_list(&self, pane: &ChatPane, id: i64) -> bool {
        if self.session.archive.is_none()
            || self.session.local_chat_pins_failed
            || (pane.list.folder.is_some() && !pane.list.archive)
        {
            return false;
        }
        let Some(chat) = self.session.chats.get(&id) else {
            return false;
        };
        let (order, list) = if pane.list.archive {
            (chat.archive_order, &self.session.archived)
        } else {
            (chat.order, &self.session.order)
        };
        order != 0 && list.contains(&(std::cmp::Reverse(order), id))
    }

    /// Chat list; `full` adds the account header (main window only), chat
    /// windows get a compact list.
    pub(super) fn view_chat_list<'a>(
        &'a self,
        window: WinId,
        pane: &'a ChatPane,
        full: bool,
    ) -> Element<'a, Msg> {
        let archive = pane.list.archive;
        let searching = !pane.list.query.trim().is_empty();
        let ids = self.displayed_chat_ids(window).unwrap_or_default();
        // Virtualization: only chat rows near the viewport are built, like
        // the message history.
        let row_height = if full {
            LIST_ROW_FULL
        } else {
            LIST_ROW_COMPACT
        };
        let viewport_height = pane.list.view_height.unwrap_or(DEFAULT_LIST_VIEW);
        let menu_index = pane
            .list
            .menu
            .and_then(|id| ids.iter().position(|&chat| chat == id));
        let menu_height = if pane.list.folder_picker.is_some() {
            LIST_PICKER_HEIGHT
        } else if pane.list.new_folder_name.is_some() {
            LIST_NEW_FOLDER_HEIGHT
        } else if !self.session.folders.is_empty() {
            LIST_FOLDER_MENU_HEIGHT
        } else {
            LIST_MENU_HEIGHT
        };
        let menu_height = menu_height
            + if pane
                .list
                .menu
                .is_some_and(|id| self.local_chat_in_list(pane, id))
                && pane.list.folder_picker.is_none()
                && pane.list.new_folder_name.is_none()
            {
                36.0
            } else {
                0.0
            };
        let chat_offset = pane.list.scroll
            - if menu_index.is_some_and(|index| {
                pane.list.scroll >= (index + 1) as f32 * row_height + menu_height
            }) {
                menu_height
            } else {
                0.0
            };
        let visible =
            list_visible_rows(chat_offset.max(0.0), viewport_height, ids.len(), row_height);
        let primary = self.look.theme.extended_palette().primary;
        let selected_base = if primary.base.text.r < 0.5 {
            iced::Color::BLACK
        } else {
            iced::Color::WHITE
        };
        let selected_hover = if primary.strong.text.r < 0.5 {
            iced::Color::BLACK
        } else {
            iced::Color::WHITE
        };
        let items = ids[visible.start..visible.end].iter().filter_map(|&id| {
            let chat = self.session.chats.get(&id)?;
            let bot = self.is_bot_chat(id);
            let mut title = if bot {
                // Inline spans keep the label beside short names; truncate
                // long names before the badge so it stays visible in compact lists.
                let limit = if full { 18 } else { 6 };
                let name = if chat.title.chars().count() > limit {
                    let (mut short, cut) = take_clusters(&chat.title, limit);
                    if cut {
                        short.push('…');
                        std::borrow::Cow::Owned(short)
                    } else {
                        std::borrow::Cow::Borrowed(chat.title.as_str())
                    }
                } else {
                    std::borrow::Cow::Borrowed(chat.title.as_str())
                };
                row![
                    container(
                        rich_text::<(), _, _, _>([span(name), span(" Bot").size(11)])
                            .size(15)
                            .wrapping(text::Wrapping::None)
                            .width(Fill)
                    )
                    .width(Fill)
                    .clip(true)
                ]
                .spacing(4)
                .align_y(iced::Center)
                .width(Fill)
            } else {
                row![text(&chat.title).size(15).width(Fill)]
                    .spacing(4)
                    .align_y(iced::Center)
            };
            if chat.muted() {
                title = title.push(text("🔇").size(11).font(rich::EMOJI_FONT));
            }
            if let Some(receipt) =
                self.receipt(id, chat.last_id, chat.last_outgoing, chat.last_pending)
            {
                title = title.push(receipt_dot(receipt));
            }
            if chat.unread > 0 {
                // Chats without sound count in gray, like the tray badge.
                let muted = chat.muted();
                title = title.push(
                    container(text(chat.unread).size(11).color(iced::Color::WHITE))
                        .padding([0, 6])
                        .style(move |theme: &iced::Theme| container::Style {
                            background: Some(
                                if muted {
                                    iced::Color::from_rgb8(0x8A, 0x8F, 0x96)
                                } else {
                                    theme.extended_palette().primary.base.color
                                }
                                .into(),
                            ),
                            border: iced::border::rounded(8),
                            ..container::Style::default()
                        }),
                );
            }
            if chat.pinned && !searching {
                title = title.push(text("📌").size(11).font(rich::EMOJI_FONT));
            }
            if self.local_chat_in_list(pane, id)
                && (if archive {
                    &self.session.local_archive
                } else {
                    &self.session.local_main
                })
                .contains(&id)
            {
                title = title.push(text("◆ здесь").size(12));
            }
            let mut body = column![title].spacing(2);
            let draft = chat.draft.as_ref().filter(|_| !pane.shows(id));
            if let Some(typing) = self.typing_label(id) {
                body = body.push(text(typing).size(12));
            } else if let (Some(draft), true) = (draft, full) {
                // Unsent text waits here: shown instead of the last message.
                body = body.push(
                    rich_text::<(), _, _, _>([
                        span("Черновик: ").color(iced::Color::from_rgb8(0xE5, 0x5B, 0x5B)),
                        span(one_line(&draft.text, 40)),
                    ])
                    .size(12),
                );
            } else if full {
                // Cut once when the message arrives (`app.rs`), not on every
                // frame: hundreds of chats redraw at video/animation tick rate.
                body = body.push(text(chat.preview.as_str()).size(12));
            }
            let selected = pane.shows(id);
            let style = move |theme: &iced::Theme, status: button::Status| {
                if !selected {
                    return button::text(theme, status);
                }
                // Keep iced's selected/hover backgrounds, but use the strongest
                // legible foreground for each one instead of its tinted Pair text.
                let mut style = button::primary(theme, status);
                style.text_color = if status == button::Status::Hovered {
                    selected_hover
                } else {
                    selected_base
                };
                style
            };
            let size = if full { 40.0 } else { 28.0 };
            let item = row![self.avatar(Peer::Chat(id), &chat.title, size), body]
                .spacing(8)
                .align_y(iced::Center);
            let entry = mouse_area(
                button(item)
                    .width(Fill)
                    .height(row_height - 2.0)
                    .style(style)
                    .on_press(Msg::Pane(window, PaneMsg::SelectChat(id))),
            )
            .on_right_press(Msg::ChatMenu(window, Some(id)));
            if pane.list.menu == Some(id) {
                return Some(column![entry, self.view_chat_menu(window, id, chat, pane)].into());
            }
            Some(entry.into())
        });
        let mut list = column![].spacing(2).padding(4);
        // The entry stays reachable with an empty archive; placement only
        // changes which control opens it, not TDLib's list membership.
        if archive {
            list = list.push(
                button(text("← Архив").size(15))
                    .width(Fill)
                    .height(LIST_HEADER_HEIGHT)
                    .style(button::text)
                    .on_press(Msg::ShowArchive(window, false)),
            );
        } else if !searching && pane.list.folder.is_none() && (!full || !self.archive_collapsed()) {
            let mut entry = row![
                button(
                    row![
                        text("🗄").size(20).font(rich::EMOJI_FONT),
                        text(format!("Архив ({})", self.session.archived.len())).size(15),
                    ]
                    .spacing(8)
                    .align_y(iced::Center),
                )
                .width(Fill)
                .height(LIST_HEADER_HEIGHT)
                .style(button::text)
                .on_press(Msg::ShowArchive(window, true)),
            ]
            .align_y(iced::Center);
            if self.session.my_id.is_some() && full {
                entry = entry.push(tooltip(
                    button("⌄")
                        .height(LIST_HEADER_HEIGHT)
                        .style(button::text)
                        .on_press(Msg::SetArchiveCollapsed(
                            window,
                            self.session.client_id,
                            true,
                        )),
                    "Свернуть архив в меню",
                    tooltip::Position::Bottom,
                ));
            }
            entry = entry.push(tooltip(
                button("↗")
                    .height(LIST_HEADER_HEIGHT)
                    .style(button::text)
                    .on_press(Msg::OpenArchiveWindow(window, self.session.client_id)),
                "Открыть архив в отдельном окне",
                tooltip::Position::Bottom,
            ));
            list = list.push(entry);
        }
        let menu_above = menu_index.is_some_and(|index| index < visible.start);
        let menu_below = menu_index.is_some_and(|index| index >= visible.end);
        let mut list = list
            .push(space().height(visible.above + if menu_above { menu_height } else { 0.0 }))
            .extend(items)
            .push(space().height(visible.below + if menu_below { menu_height } else { 0.0 }));
        // Enter in the filter: messages from the archive or globally.
        if pane.list.messages.is_some()
            || pane.list.paging.pending.is_some()
            || pane.list.paging.error.is_some()
        {
            list = list.push(container(text("Сообщения").size(13)).height(SEARCH_HEADING_HEIGHT));
            if pane.list.messages.as_ref().is_some_and(Vec::is_empty) {
                list = list.push(text("Ничего не найдено").size(12));
            }
            if let Some(found) = &pane.list.messages {
                // Padding, the optional archive entry, chat rows, and the
                // heading all precede results in this same scrollable.
                let header = if archive
                    || (!searching && pane.list.folder.is_none() && !self.archive_collapsed())
                {
                    LIST_HEADER_HEIGHT + 2.0
                } else {
                    0.0
                };
                let prefix = 4.0
                    + header
                    + 2.0
                    + ids.len() as f32 * row_height
                    + 2.0
                    + if menu_index.is_some() {
                        menu_height
                    } else {
                        0.0
                    }
                    + SEARCH_HEADING_HEIGHT
                    + 2.0
                    + 2.0;
                let visible =
                    search_visible_rows(pane.list.scroll, viewport_height, prefix, found.len());
                list = list.push(space().height(visible.above));
                for m in &found[visible.start..visible.end] {
                    let chat = self
                        .session
                        .chats
                        .get(&m.chat_id)
                        .map_or("", |c| c.title.as_str());
                    let mut name = row![text(chat).size(12)].spacing(4).align_y(iced::Center);
                    if self.is_bot_chat(m.chat_id) {
                        name = name.push(text("Bot").size(11).style(text::primary));
                    }
                    list = list.push(
                        button(column![name, text(one_line(&m.preview(), 40)).size(12)].spacing(1))
                            .width(Fill)
                            .height(SEARCH_ROW_HEIGHT - 2.0)
                            .style(button::text)
                            .on_press(Msg::OpenFound(window, m.chat_id, m.id)),
                    );
                }
                list = list.push(space().height(visible.below));
            }
            if let Some(status) = Self::search_status(
                &pane.list.paging,
                pane.list.messages.is_some(),
                Msg::ListMore(window),
            ) {
                list = list.push(status);
            }
        }
        let placeholder = if archive {
            "Архив: загруженные чаты (Enter — сообщения)"
        } else {
            "Поиск (Enter — по сообщениям)"
        };
        let filter = text_input(placeholder, &pane.list.query)
            .on_input(move |q| Msg::ListQuery(window, q))
            .on_submit(Msg::ListSearchMessages(window))
            .size(13);
        let mut top = column![container(filter).padding([4, 6])];
        if full
            && (!self.session.folders.is_empty()
                || self.session.folder_reorder.is_some()
                || self.session.folder_reorder_error.is_some()
                || self.session.folder_creation.is_some()
                || self.session.folder_creation_warning.is_some())
        {
            top = top.push(self.view_folder_tabs(window, pane.list.folder));
        }
        if archive {
            top = top.push(
                container(
                    button("Настройки архива")
                        .style(button::secondary)
                        .on_press_maybe(
                            pane.list
                                .archive_settings
                                .is_none()
                                .then_some(Msg::ToggleArchiveSettings(window, true)),
                        ),
                )
                .padding([2, 6]),
            );
            if full {
                top = top.push(
                    container(
                        button("Открыть архив в отдельном окне")
                            .style(button::secondary)
                            .on_press(Msg::OpenArchiveWindow(window, self.session.client_id)),
                    )
                    .padding([2, 6]),
                );
            }
            if self.session.my_id.is_some() {
                let collapsed = self.archive_collapsed();
                top = top.push(
                    container(
                        button(if collapsed {
                            "Вернуть архив в список"
                        } else {
                            "Свернуть архив в меню"
                        })
                        .style(button::secondary)
                        .on_press(Msg::SetArchiveCollapsed(
                            window,
                            self.session.client_id,
                            !collapsed,
                        )),
                    )
                    .padding([2, 6]),
                );
            }
        }
        if let Some(scope) = self.current_read_list(window) {
            let name = match &scope {
                tdlib_rs::enums::ChatList::Main => "Все чаты".to_owned(),
                tdlib_rs::enums::ChatList::Archive => "Архив".to_owned(),
                tdlib_rs::enums::ChatList::Folder(folder) => self
                    .session
                    .folders
                    .iter()
                    .find(|(id, _)| *id == folder.chat_folder_id)
                    .map_or_else(|| "Папка".to_owned(), |(_, name)| name.clone()),
            };
            let busy = self
                .session
                .list_reads
                .iter()
                .any(|read| read.list == scope);
            let mut action = column![].spacing(4).width(Fill);
            if self.session.leave.is_some() {
                action = action.push(text("Смена аккаунта…").size(12));
            } else if busy {
                action = action.push(text(format!("Читаем сообщения: {name}…")).size(12));
            } else if let Some((request, pending)) = pane.list.confirm_read.as_ref()
                && *pending == scope
            {
                action = action
                    .push(
                        text(format!(
                            "Прочитать сообщения во всех чатах списка «{name}», \
                             включая не загруженные? Отметки «непрочитанное вручную» \
                             могут остаться."
                        ))
                        .size(12)
                        .width(Fill),
                    )
                    .push(
                        row![
                            button("Подтвердить").style(button::danger).on_press(
                                Msg::ConfirmReadList(
                                    window,
                                    self.session.client_id,
                                    *request,
                                    true,
                                )
                            ),
                            button("Отмена").style(button::secondary).on_press(
                                Msg::ConfirmReadList(
                                    window,
                                    self.session.client_id,
                                    *request,
                                    false,
                                )
                            ),
                        ]
                        .spacing(4),
                    );
            } else {
                action = action.push(
                    button("Прочитать сообщения")
                        .style(button::secondary)
                        .on_press(Msg::AskReadList(window, self.session.client_id)),
                );
            }
            if let Some(result) = &pane.list.read_feedback {
                action = action.push(match result {
                    Ok(()) => text("Запрос выполнен; счётчики обновятся по данным Telegram.")
                        .size(12)
                        .width(Fill),
                    Err(error) => text(format!("Не удалось прочитать сообщения: {error}"))
                        .size(12)
                        .width(Fill)
                        .style(text::danger),
                });
            }
            top = top.push(container(action).padding([2, 6]));
        }
        let list = column![
            top,
            scrollable(list)
                .id(pane.list.scroll_id.clone())
                .height(Fill)
                .on_scroll(move |v| Msg::Pane(window, PaneMsg::ListScrolled(v)))
        ];
        let sidebar = self.look.sidebar;
        let panel = move |_: &iced::Theme| container::background(sidebar);
        if !full {
            return container(list).width(220).height(Fill).style(panel).into();
        }
        container(list).width(300).height(Fill).style(panel).into()
    }

    /// Tabs of Telegram folders above the chat list, with unread counters.
    fn view_folder_tabs(&self, window: WinId, current: Option<i32>) -> Element<'_, Msg> {
        let tab = |label: &str, folder: Option<i32>, unread: (i32, i32)| {
            let mut content = row![text(label.to_owned()).size(13)]
                .spacing(4)
                .align_y(iced::Center);
            let count = if unread.1 > 0 { unread.1 } else { unread.0 };
            if count > 0 {
                let accent = unread.1 > 0 && current != folder;
                content = content.push(text(count).size(11).style(if accent {
                    text::primary
                } else {
                    text::base
                }));
            }
            button(content)
                .padding([3, 8])
                .style(if current == folder {
                    button::primary
                } else {
                    button::text
                })
                .on_press(Msg::ShowFolder(window, folder))
                .into()
        };
        let len = self.session.folders.len();
        let mut tabs: Vec<Element<'_, Msg>> = self
            .session
            .folders
            .iter()
            .enumerate()
            .map(|(index, (id, name))| {
                let mut up = button("↑").padding([3, 3]).style(button::secondary);
                let mut down = button("↓").padding([3, 3]).style(button::secondary);
                if self.session.folder_reorder.is_none() && self.session.folder_creation.is_none() {
                    if index > 0 {
                        up = up.on_press(Msg::ReorderFolder(window, *id, true));
                    }
                    if index + 1 < len {
                        down = down.on_press(Msg::ReorderFolder(window, *id, false));
                    }
                }
                row![
                    tab(
                        name,
                        Some(*id),
                        self.session
                            .folder_unread
                            .get(id)
                            .copied()
                            .unwrap_or_default(),
                    ),
                    tooltip(up, "Переместить папку выше", tooltip::Position::Bottom),
                    tooltip(down, "Переместить папку ниже", tooltip::Position::Bottom)
                ]
                .spacing(2)
                .align_y(iced::Center)
                .into()
            })
            .collect();
        tabs.insert(
            self.session.main_tab.min(tabs.len()),
            tab("Все чаты", None, self.session.unread),
        );
        let tabs = scrollable(row(tabs).spacing(2).padding([0, 4])).direction(
            scrollable::Direction::Horizontal(
                scrollable::Scrollbar::new().width(2).scroller_width(2),
            ),
        );
        let mut section = column![tabs].width(Fill);
        if let Some(error) = &self.session.folder_reorder_error {
            section = section.push(
                container(text(error).size(12).style(text::danger).width(Fill))
                    .width(Fill)
                    .padding([2, 6]),
            );
        }
        if let Some(warning) = &self.session.folder_creation_warning {
            section = section.push(
                container(text(warning).size(12).style(text::danger).width(Fill))
                    .width(Fill)
                    .padding([2, 6]),
            );
        }
        if let Some(pending) = self
            .session
            .folder_creation
            .as_ref()
            .filter(|pending| pending.id.is_some())
        {
            section = section.push(
                container(
                    column![
                        text("Продолжение не отменяет прежний запрос: позднее обновление ещё может добавить папку.")
                            .size(12)
                            .width(Fill),
                        button("Проверил список папок")
                            .style(button::secondary)
                            .on_press(Msg::AcknowledgeFolderCreation(
                                window,
                                self.session.client_id,
                                pending.request,
                            )),
                    ]
                    .spacing(4)
                    .width(Fill),
                )
                .width(Fill)
                .padding([2, 6]),
            );
        }
        if let Some(pending) = self.session.folder_reorder.as_ref().filter(|pending| {
            matches!(
                pending.state,
                FolderReorderState::AwaitingUpdate | FolderReorderState::DiscrepancyAfterReply
            )
        }) {
            section = section.push(
                container(
                    column![
                        text("Показан последний известный порядок сервера. Продолжение не отменяет прежний запрос: позднее обновление ещё может изменить порядок.")
                            .size(12)
                            .width(Fill),
                        button("Продолжить с текущим порядком")
                            .style(button::secondary)
                            .on_press(Msg::AcknowledgeFolderReorder(
                                window,
                                self.session.client_id,
                                pending.request,
                            )),
                    ]
                    .spacing(4)
                    .width(Fill),
                )
                .width(Fill)
                .padding([2, 6]),
            );
        }
        section.into()
    }

    /// Right-click menu of a chat in the list.
    fn view_chat_menu<'a>(
        &'a self,
        window: WinId,
        id: i64,
        chat: &super::ChatItem,
        pane: &'a ChatPane,
    ) -> Element<'a, Msg> {
        use super::ChatOp;
        let item = |label: &'static str, op: ChatOp| {
            button(text(label).size(14))
                .width(Fill)
                .height(34)
                .style(button::text)
                .on_press(Msg::ChatOp(window, id, op))
        };
        if let Some(name) = &pane.list.new_folder_name {
            let pending = self.session.folder_creation.as_ref();
            let busy = pending.is_some() || self.session.folder_reorder.is_some();
            let message = pane
                .list
                .new_folder_error
                .as_deref()
                .or(self.session.folder_creation_warning.as_deref())
                .unwrap_or(if busy {
                    "Ожидаем Telegram…"
                } else {
                    "От 1 до 12 символов"
                });
            let mut form = column![
                text("Новая папка для чата").size(14),
                text_input("Название папки", name)
                    .on_input(move |value| Msg::FolderName(window, id, value))
                    .on_submit(Msg::ConfirmNewFolder(window, id)),
                button("Создать")
                    .on_press_maybe((!busy).then_some(Msg::ConfirmNewFolder(window, id))),
                scrollable(text(message).size(12)).height(48),
            ]
            .spacing(5);
            if let Some(pending) = pending.filter(|pending| pending.id.is_some()) {
                form = form.push(
                    button("Проверил список папок")
                        .style(button::secondary)
                        .on_press(Msg::AcknowledgeFolderCreation(
                            window,
                            self.session.client_id,
                            pending.request,
                        )),
                );
            }
            form = form.push(item("Отмена", ChatOp::Close));
            return container(form)
                .height(LIST_NEW_FOLDER_HEIGHT)
                .padding(4)
                .style(container::bordered_box)
                .into();
        }
        if let Some(picker) = &pane.list.folder_picker {
            let mut folders = column![].spacing(4);
            for (folder, name) in &self.session.folders {
                let available =
                    picker.pending.is_none() && !self.session.folder_edits.contains_key(folder);
                let actions = row![
                    button(text("Добавить").size(12))
                        .style(button::secondary)
                        .on_press_maybe(
                            available.then_some(Msg::FolderAction(window, id, *folder, true))
                        ),
                    button(text("Исключить").size(12))
                        .style(button::secondary)
                        .on_press_maybe(
                            available.then_some(Msg::FolderAction(window, id, *folder, false))
                        ),
                ]
                .spacing(4);
                folders = folders.push(column![text(name).size(13), actions].spacing(1));
            }
            let status = if let Some((_, folder)) = picker.pending {
                text(format!(
                    "Сохраняем «{}»…",
                    self.session
                        .folders
                        .iter()
                        .find(|(id, _)| *id == folder)
                        .map_or("", |(_, name)| name.as_str())
                ))
            } else {
                text(picker.feedback.as_deref().unwrap_or(""))
            };
            let menu = column![
                text("Папка для чата").size(14).height(24),
                scrollable(folders).height(175),
                scrollable(status.size(12)).height(40),
                item("Создать папку…", ChatOp::NewFolder),
                item("Отмена", ChatOp::Close),
            ]
            .spacing(2);
            return container(menu)
                .height(LIST_PICKER_HEIGHT)
                .padding(4)
                .style(container::bordered_box)
                .into();
        }
        let archived = chat.order == 0 && chat.archive_order != 0;
        let local = if pane.list.archive {
            &self.session.local_archive
        } else {
            &self.session.local_main
        };
        let can_local = self.local_chat_in_list(pane, id);
        let mut menu = column![item(
            if chat.pinned {
                "Открепить в Telegram"
            } else {
                "Закрепить в Telegram"
            },
            ChatOp::Pin(!chat.pinned)
        )]
        .spacing(2);
        if can_local {
            let pinned = local.contains(&id);
            menu = menu.push(item(
                if pinned {
                    "Открепить здесь"
                } else {
                    "Закрепить здесь"
                },
                ChatOp::LocalPin(!pinned),
            ));
        }
        menu = menu
            .push(item(
                if archived {
                    "Вернуть из архива"
                } else {
                    "В архив"
                },
                ChatOp::Archive(!archived),
            ))
            .push(item(
                if chat.muted() {
                    "Включить звук"
                } else {
                    "Без звука"
                },
                ChatOp::Mute(!chat.muted()),
            ));
        if !self.session.folders.is_empty() {
            menu = menu.push(item("Папки…", ChatOp::Folders));
        }
        menu = menu.push(item("Создать папку…", ChatOp::NewFolder));
        menu = menu.push(item("Отмена", ChatOp::Close));
        let height = if self.session.folders.is_empty() {
            LIST_MENU_HEIGHT
        } else {
            LIST_FOLDER_MENU_HEIGHT
        } + if can_local { 36.0 } else { 0.0 };
        container(menu)
            .height(height)
            .padding(4)
            .style(container::bordered_box)
            .into()
    }

    /// Account switcher: the added accounts and "add".
    pub(super) fn view_accounts(&self) -> Element<'_, Msg> {
        let mut list = column![].spacing(2).padding([0, 6]);
        for account in self.logged_in().filter(|a| a.slot != self.session.slot) {
            let name = self.account_name(account.slot);
            list = list.push(
                button(
                    row![
                        self.account_avatar(account.slot, account.user_id.unwrap(), name, 30.0),
                        text(name).size(14)
                    ]
                    .spacing(8)
                    .align_y(iced::Center),
                )
                .width(Fill)
                .style(button::text)
                .on_press(Msg::SwitchAccount(account.slot)),
            );
        }
        if self.archive_collapsed() {
            list = list.push(
                button(text("Архив").size(14))
                    .width(Fill)
                    .style(button::text)
                    .on_press(Msg::OpenArchiveFromMenu(self.session.client_id)),
            );
            list = list.push(
                button(text("Вернуть архив в список").size(14))
                    .width(Fill)
                    .style(button::text)
                    .on_press(Msg::SetArchiveCollapsed(
                        self.main_window,
                        self.session.client_id,
                        false,
                    )),
            );
        }
        list = list.push(
            button(text("+ Добавить аккаунт").size(14))
                .width(Fill)
                .style(button::text)
                .on_press(Msg::AddAccount),
        );
        container(list).padding([0, 0]).into()
    }

    /// Scheme and accent: buttons, the current one highlighted.
    fn view_look_settings(&self) -> Element<'_, Msg> {
        use super::look::{Accent, LookSettings, Scheme};
        let current = self.settings.look;
        let schemes = Scheme::ALL.map(|scheme| {
            button(text(scheme.label()).size(14))
                .style(if current.scheme == scheme {
                    button::primary
                } else {
                    button::secondary
                })
                .on_press(Msg::SetLook(LookSettings { scheme, ..current }))
                .into()
        });
        let accents = Accent::ALL.map(|accent| {
            let color = accent.color();
            let chosen = current.accent == accent;
            button(space().width(22).height(22))
                .padding(0)
                .style(move |theme: &iced::Theme, _| button::Style {
                    background: Some(color.into()),
                    border: iced::Border {
                        color: if chosen {
                            theme.palette().text
                        } else {
                            iced::Color::TRANSPARENT
                        },
                        width: 2.0,
                        radius: 11.0.into(),
                    },
                    ..button::Style::default()
                })
                .on_press(Msg::SetLook(LookSettings { accent, ..current }))
                .into()
        });
        column![
            row(schemes).spacing(6),
            row![text("Акцент").size(14), row(accents).spacing(8)]
                .spacing(12)
                .align_y(iced::Center),
        ]
        .spacing(8)
        .into()
    }

    fn view_cache_settings(&self) -> Element<'_, Msg> {
        let limits = self.settings.cache;
        column![
            text(format!("Размер кэша: {} ГБ", limits.gib() as u32)).size(14),
            slider(1.0_f32..=100.0, limits.gib(), move |gib| {
                Msg::SetCachePolicy(crate::settings::CacheLimits::from_sliders(
                    gib,
                    limits.months(),
                ))
            })
            .step(1.0_f32)
            .on_release(Msg::ApplyCachePolicy),
            text(format!("Срок хранения: {} мес.", limits.months() as u32)).size(14),
            slider(1.0_f32..=12.0, limits.months(), move |months| {
                Msg::SetCachePolicy(crate::settings::CacheLimits::from_sliders(
                    limits.gib(),
                    months,
                ))
            })
            .step(1.0_f32)
            .on_release(Msg::ApplyCachePolicy),
            text(
                "Лимит файлов TDLib действует отдельно для каждого аккаунта; \
                 такой же отдельный лимит — для общего кэша декодированных аватаров."
            )
            .size(12),
            text(
                "Фото профилей, стикеры, миниатюры и обои TDLib может защищать от очистки. \
                 База данных, архив и плагины не входят в лимит. Очистка проходит при \
                 очередном обычном обслуживании, не сразу после изменения."
            )
            .size(12),
        ]
        .spacing(8)
        .into()
    }

    pub(super) fn view_settings(&self) -> Element<'_, Msg> {
        if self.session.plugin_help_open {
            return self.view_plugin_help();
        }
        let expanded = &self.session.settings_expanded;
        let header = |name: &'static str, section: SettingsSection| -> Element<'_, Msg> {
            button(
                text(format!(
                    "{} {name}",
                    if expanded[section as usize] {
                        "▾"
                    } else {
                        "▸"
                    }
                ))
                .size(16),
            )
            .width(Fill)
            .style(button::secondary)
            .on_press(Msg::ToggleSettingsSection(section))
            .into()
        };
        let mut page = column![
            row![
                text("Настройки").size(18).width(Fill),
                button("Закрыть")
                    .style(button::secondary)
                    .on_press(Msg::CloseSettings),
            ]
            .align_y(iced::Center),
        ]
        .spacing(16)
        .padding(10)
        .max_width(640);
        if let Some(e) = &self.error {
            page = page.push(text(e).style(text::danger));
        }
        page = page.push(header("Общие", SettingsSection::General));
        if expanded[SettingsSection::General as usize] {
            page = page.push(
                toggler(self.settings.notifications)
                    .label("Уведомления о новых сообщениях")
                    .on_toggle(Msg::SetNotifications),
            );
        }
        page = page.push(header("Сообщения", SettingsSection::Messages));
        if expanded[SettingsSection::Messages as usize] {
            page = page.push(
                column![
                    toggler(self.settings.keep_deleted)
                        .label("Сохранять удалённые сообщения")
                        .on_toggle(Msg::SetKeepDeleted),
                    text(
                        "Сообщения, которые видел клиент, остаются в чате с пометкой «удалено». \
                         Если выключить, новые сообщения не сохраняются, а уже сохранённые \
                         скрываются до повторного включения."
                    )
                    .size(12),
                ]
                .spacing(6),
            );
        }
        page = page.push(header("Оформление", SettingsSection::Look));
        if expanded[SettingsSection::Look as usize] {
            page = page.push(self.view_look_settings());
        }
        page = page.push(header("Хранилище", SettingsSection::Storage));
        if expanded[SettingsSection::Storage as usize] {
            page = page.push(self.view_cache_settings());
        }
        page = page.push(header("Безопасность", SettingsSection::Security));
        if expanded[SettingsSection::Security as usize] {
            page = page.push(self.view_password_settings());
        } else if let Some(Err(e)) = &self.session.password_form.message {
            page = page.push(text(e).style(text::danger));
        }
        page = page.push(header("Плагины", SettingsSection::Plugins));
        if expanded[SettingsSection::Plugins as usize] {
            page = page.push(self.view_plugins());
        } else {
            for info in &self.plugin_infos {
                if let Some(error) = &info.error {
                    page =
                        page.push(text(format!("Плагин {}: {error}", info.id)).style(text::danger));
                }
            }
            if let Some(log) = self.plugin_logs.get("") {
                for line in log
                    .iter()
                    .rev()
                    .take(super::plugins_view::CARD_LOG_LINES)
                    .rev()
                {
                    page = page.push(text(line).size(12).style(text::danger));
                }
            }
        }
        scrollable(page).height(Fill).into()
    }

    pub(super) fn view_archive_settings<'a>(
        &self,
        window: WinId,
        panel: &'a super::pane::ArchiveSettings,
    ) -> Element<'a, Msg> {
        use crate::td::ArchiveFlag;

        let busy = panel.loading
            || panel.saving
            || panel.capability.is_none()
            || self.session.archive_write.is_some();
        let setting = |label: &'static str,
                       description: &'static str,
                       flag: ArchiveFlag,
                       enabled: bool|
         -> Element<'a, Msg> {
            column![
                button(text(format!("{} {label}", if enabled { "✓" } else { "○" })))
                    .width(Fill)
                    .style(button::secondary)
                    .on_press_maybe(
                        (!busy).then_some(Msg::ChangeArchiveFlag(window, flag, !enabled))
                    ),
                text(description).size(12),
            ]
            .spacing(4)
            .into()
        };
        let mut page = column![row![
            text("Настройки архива").size(20).width(Fill),
            button("Закрыть")
                .style(button::text)
                .on_press(Msg::ToggleArchiveSettings(window, false)),
        ],]
        .spacing(16)
        .padding(16)
        .max_width(640);
        if panel.loading {
            page = page.push(text("Загружаем настройки архива…"));
        }
        if panel.saving {
            page = page.push(text("Сохраняем и проверяем настройки на сервере…"));
        }
        if let Some(error) = &panel.error {
            page = page.push(text(error).style(text::danger));
        }
        if !panel.saving {
            page = page.push(
                button("Обновить / повторить")
                    .style(button::secondary)
                    .on_press_maybe((!panel.loading).then_some(Msg::RetryArchiveSettings(window))),
            );
        }
        if let Some(settings) = &panel.settings {
            page = page.push(setting(
                "Оставлять чаты с включёнными уведомлениями в архиве",
                "Не возвращать такие чаты из архива при новом сообщении.",
                ArchiveFlag::KeepUnmuted,
                settings.keep_unmuted_chats_archived,
            ));
            page = page.push(setting(
                "Оставлять чаты из папок в архиве",
                "Для чатов, закреплённых или всегда включённых в папку: не возвращать при новом сообщении. Не влияет, если включена настройка выше.",
                ArchiveFlag::KeepFolderChats,
                settings.keep_chats_from_folders_archived,
            ));
            match &panel.capability {
                Some(Ok(true)) => {
                    page = page.push(setting(
                        "Архивировать и отключать уведомления новых чатов от незнакомых",
                        "Только новые чаты от пользователей не из контактов.",
                        ArchiveFlag::ArchiveUnknown,
                        settings.archive_and_mute_new_chats_from_unknown_users,
                    ));
                }
                Some(Ok(false)) => {
                    page = page.push(text("Автоархивация новых чатов от незнакомых недоступна для этого аккаунта.").size(12));
                }
                Some(Err(error)) => {
                    page = page.push(
                        text(format!(
                            "Автоархивация новых чатов от незнакомых недоступна: {error}"
                        ))
                        .size(12)
                        .style(text::danger),
                    );
                }
                None => {
                    page = page.push(text("Проверяем доступность автоархивации…").size(12));
                }
            }
        }
        container(scrollable(page).height(Fill)).width(Fill).into()
    }
}
#[cfg(test)]
mod list_virtualization_tests {
    use super::{SEARCH_ROW_HEIGHT, Visible, list_visible_rows, search_visible_rows};

    const ROW: f32 = 52.0;
    const VIEW: f32 = 700.0;

    #[test]
    fn empty_list_builds_nothing() {
        assert_eq!(
            list_visible_rows(0.0, VIEW, 0, ROW),
            Visible {
                start: 0,
                end: 0,
                above: 0.0,
                below: 0.0
            }
        );
    }

    #[test]
    fn short_list_is_built_entirely() {
        let v = list_visible_rows(0.0, VIEW, 5, ROW);
        assert_eq!(
            v,
            Visible {
                start: 0,
                end: 5,
                above: 0.0,
                below: 0.0
            }
        );
    }

    #[test]
    fn thousand_chats_build_only_a_screenful() {
        let v = list_visible_rows(0.0, VIEW, 1000, ROW);
        assert_eq!(v.start, 0);
        let built = v.end - v.start;
        // A 700 px viewport plus a ~700 px margin below fits well under a
        // hundred 52 px rows, nowhere near all 1000.
        assert!((10..80).contains(&built), "built {built}");
        assert_eq!(v.below, (1000 - v.end) as f32 * ROW);
    }

    #[test]
    fn scrolling_moves_the_built_range() {
        let top = list_visible_rows(0.0, VIEW, 1000, ROW);
        let middle = list_visible_rows(500.0 * ROW, VIEW, 1000, ROW);
        assert_eq!(top.start, 0);
        assert!(middle.start > 0 && middle.start < 500, "{middle:?}");
        assert!(middle.end > 500, "{middle:?}");
        assert_ne!(top, middle);
    }

    #[test]
    fn offset_past_the_end_clamps_to_the_last_row() {
        let v = list_visible_rows(2000.0 * ROW, VIEW, 1000, ROW);
        assert_eq!(
            v,
            Visible {
                start: 1000,
                end: 1000,
                above: 1000.0 * ROW,
                below: 0.0
            }
        );
    }

    #[test]
    fn zero_row_height_builds_nothing_without_panicking() {
        let v = list_visible_rows(0.0, VIEW, 1000, 0.0);
        assert_eq!(v.start, 0);
        assert_eq!(v.end, 0);
    }

    #[test]
    fn paged_search_builds_only_nearby_hits_and_retains_deep_navigation() {
        // Distinct ids model accumulated pages. The visible range is the
        // exact slice used to create clickable result rows in both views.
        let hits: Vec<(i64, i64)> = (0..6_001).map(|i| (i + 10_000, i + 50_000)).collect();
        let prefix = 42.0 + 300.0 * ROW + 20.0; // preceding sidebar chats/header
        let hidden = search_visible_rows(0.0, VIEW, prefix, hits.len());
        assert_eq!(hidden.end, 0);
        assert_eq!(hidden.below, hits.len() as f32 * SEARCH_ROW_HEIGHT);

        let target = 5_432;
        for prefix in [prefix, 6.0] {
            let visible = search_visible_rows(
                prefix + target as f32 * SEARCH_ROW_HEIGHT,
                VIEW,
                prefix,
                hits.len(),
            );
            let clickable = &hits[visible.start..visible.end];
            assert!(
                clickable.len() < 50,
                "built {} result buttons",
                clickable.len()
            );
            assert_eq!(
                clickable.iter().find(|&&(chat, _)| chat == target + 10_000),
                Some(&(target + 10_000, target + 50_000))
            );
            assert_eq!(
                visible.above + clickable.len() as f32 * SEARCH_ROW_HEIGHT + visible.below,
                hits.len() as f32 * SEARCH_ROW_HEIGHT
            );
            let last = search_visible_rows(
                prefix + hits.len() as f32 * SEARCH_ROW_HEIGHT - VIEW,
                VIEW,
                prefix,
                hits.len(),
            );
            assert_eq!(last.end, hits.len()); // footer follows the last result
            assert_eq!(hits[last.end - 1], (16_000, 56_000));
        }
    }
}
