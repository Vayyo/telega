//! Chat-pane message routing and pane-local actions.

use super::{
    App, Link, Menu, Msg, MsgItem, PaneMsg, WinId, card, media::Media, pane, picker, rich,
};
use crate::td;
use iced::Task;
use tdlib_rs::enums::MessageSender;

impl App {
    pub(super) fn on_pane(&mut self, window: WinId, msg: PaneMsg) -> Task<Msg> {
        if matches!(&msg, PaneMsg::Compose(_) | PaneMsg::Send)
            && !self
                .session
                .panes
                .get(&window)
                .is_some_and(|p| self.composer_available(window, p))
        {
            return Task::none();
        }
        if matches!(&msg, PaneMsg::PinLocal(..) | PaneMsg::Forget(_))
            && self.session.panes.contains_key(&window)
            && self.key_transition_active()
        {
            self.error = Some("сначала завершите смену ключа".into());
            return Task::none();
        }
        // Answers for a window that was closed meanwhile are dropped here.
        let Some(pane) = self.session.panes.get_mut(&window) else {
            return Task::none();
        };
        match msg {
            PaneMsg::SelectChat(chat_id) => {
                if window == self.main_window {
                    self.session.settings_open = false;
                }
                return self.select_chat(window, chat_id);
            }
            PaneMsg::HistoryLoaded(chat_id, result) => {
                if pane.shows(chat_id) {
                    match result {
                        Ok(messages) => {
                            if let Some(archive) = &self.session.archive {
                                pane.local_pins = archive.local_pins(chat_id).unwrap_or_default();
                            }
                            // A jump in flight or already landed keeps its
                            // own history; pins, draft and roles still load
                            // for the chat that just opened either way.
                            let skip_history = pane.jump_pending.is_some() || pane.jumped;
                            let pinned = self.load_pinned(window, chat_id);
                            let draft = self.load_draft(window, chat_id);
                            let roles = self.load_roles(chat_id);
                            let history = if skip_history {
                                Task::none()
                            } else {
                                self.show_history(window, chat_id, &messages)
                            };
                            return Task::batch([history, pinned, draft, roles]);
                        }
                        Err(e) => self.error = Some(e),
                    }
                }
            }
            PaneMsg::PinnedLoaded(chat_id, result) => {
                if pane.shows(chat_id)
                    && let Ok(messages) = result
                {
                    let current = pane.pins().get(pane.pinned_shown).map(|(m, _)| m.id);
                    pane.pinned = messages.iter().map(MsgItem::from).collect();
                    pane.pinned_shown = current
                        .and_then(|id| pane.pins().iter().position(|(m, _)| m.id == id))
                        .unwrap_or(0);
                }
            }
            PaneMsg::PinnedClicked => {
                let pins = pane.pins();
                let Some(id) = pins.get(pane.pinned_shown).map(|(m, _)| m.id) else {
                    return Task::none();
                };
                // Like Telegram: each click goes one pinned message further back.
                pane.pinned_shown = (pane.pinned_shown + 1) % pins.len();
                return self.jump(window, id);
            }
            PaneMsg::PinLocal(id, on) => {
                pane.menu = None;
                let Some(chat_id) = pane.chat_id else {
                    return Task::none();
                };
                let message = pane.messages.iter().find(|m| m.id == id).cloned();
                let Some(archive) = &self.session.archive else {
                    self.error = Some("закрепы для себя появятся после входа в аккаунт".into());
                    return Task::none();
                };
                let result = match (on, message) {
                    (true, Some(m)) => archive.pin_local(&m, chrono::Utc::now().timestamp()),
                    (true, None) => return Task::none(),
                    (false, _) => archive.unpin_local(chat_id, id),
                };
                if let Err(e) = result {
                    self.error = Some(format!("закреп: {e}"));
                }
                self.refresh_local_pins(chat_id, on.then_some(id));
            }
            PaneMsg::OlderLoaded(chat_id, before, result) => {
                // Ignore answers for a chat or position that is no longer shown.
                if pane.shows(chat_id) && pane.oldest_loaded == Some(before) {
                    match result {
                        Ok(messages) => {
                            return self.prepend_history(window, chat_id, before, &messages);
                        }
                        Err(e) => {
                            pane.loading_older = false;
                            self.error = Some(e);
                        }
                    }
                }
            }
            PaneMsg::Scrolled(viewport) => {
                pane.scroll = Some(viewport.absolute_offset());
                let bounds = viewport.bounds();
                pane.view_size = Some((bounds.height, bounds.width));
                // The list is anchored to the bottom; the reversed offset is
                // the distance from the top.
                if viewport.absolute_offset_reversed().y < 200.0 {
                    return self.load_older(window);
                }
                if viewport.absolute_offset().y < 200.0 {
                    return self.load_newer(window);
                }
            }
            PaneMsg::ListScrolled(viewport) => {
                pane.list.scroll = viewport.absolute_offset().y;
                pane.list.view_height = Some(viewport.bounds().height);
            }
            PaneMsg::SearchScrolled(id, viewport) => {
                if let Some(search) = &mut pane.search
                    && search.scroll_id == id
                {
                    search.scroll = viewport.absolute_offset().y;
                    search.view_height = Some(viewport.bounds().height);
                }
            }
            PaneMsg::Measured(id, height) => {
                pane.heights.insert(id, height);
            }
            PaneMsg::Compose(action) => {
                pane.compose_touched |= action.is_edit();
                pane.compose.perform(action);
            }
            PaneMsg::StartEdit(id) => {
                pane.menu = None;
                let Some(chat_id) = pane.chat_id else {
                    return Task::none();
                };
                return Task::perform(
                    td::message_markdown(self.session.client_id, chat_id, id),
                    move |r| Msg::Pane(window, PaneMsg::EditLoaded(chat_id, id, r)),
                );
            }
            PaneMsg::EditLoaded(chat_id, id, result) => {
                // The window switched chats while this was loading: applying
                // it now would put another chat's message text, under this
                // chat's own message id, into the wrong chat's input field.
                if !pane.shows(chat_id) {
                    return Task::none();
                }
                match result {
                    Ok(text) => {
                        // The draft steps aside and comes back after the edit.
                        let draft = match pane.editing.take() {
                            Some((_, draft)) => draft,
                            None => pane.compose.text(),
                        };
                        pane.editing = Some((id, draft));
                        pane.reply_to = None;
                        pane.compose = iced::widget::text_editor::Content::with_text(&text);
                        pane.compose
                            .perform(iced::widget::text_editor::Action::Move(
                                iced::widget::text_editor::Motion::DocumentEnd,
                            ));
                    }
                    Err(e) => self.error = Some(e),
                }
            }
            PaneMsg::CancelEdit => pane.finish_edit(),
            PaneMsg::Send => {
                let text = pane.compose.text().trim().to_owned();
                if let Some(chat_id) = pane.chat_id
                    && let Some((id, _)) = pane.editing
                {
                    if text.is_empty() {
                        return Task::none();
                    }
                    pane.finish_edit();
                    return Task::perform(
                        td::edit_text(self.session.client_id, chat_id, id, text),
                        Msg::Done,
                    );
                }
                if let Some(chat_id) = pane.chat_id
                    && !text.is_empty()
                {
                    pane.compose = iced::widget::text_editor::Content::new();
                    // Sending clears the draft in Telegram too.
                    self.session.synced_drafts.insert(chat_id, String::new());
                    let reply_to = pane.reply_to.take();
                    return Task::perform(
                        td::send_text(self.session.client_id, chat_id, text, reply_to),
                        Msg::Done,
                    );
                }
            }
            PaneMsg::Link(Link::Url { url, hidden: true }) => pane.confirm_link = Some(url),
            PaneMsg::Link(Link::Url { url, hidden: false }) | PaneMsg::OpenLink(url) => {
                pane.confirm_link = None;
                // A user mentioned without a username (no link TDLib can
                // resolve exists for them): open the profile directly.
                if let Some(user_id) = url
                    .strip_prefix("tg://user?id=")
                    .and_then(|id| id.parse::<i64>().ok())
                {
                    return self.on_card(card::CardMsg::Open(window, user_id));
                }
                // Telegram links open inside the client.
                if td::is_telegram_link(&url) {
                    return Task::perform(
                        td::resolve_link(self.session.client_id, url.clone()),
                        move |r| Msg::LinkResolved(window, url.clone(), r),
                    );
                }
                if let Some(url) = rich::safe_link(&url)
                    && let Err(e) = open::that_detached(&url)
                {
                    self.error = Some(format!("не удалось открыть ссылку: {e}"));
                }
            }
            PaneMsg::CancelLink => pane.confirm_link = None,
            PaneMsg::JoinChat(link) => {
                pane.confirm_join = None;
                return Task::perform(td::join_by_link(self.session.client_id, link), move |r| {
                    Msg::Joined(window, r)
                });
            }
            PaneMsg::CancelJoin => pane.confirm_join = None,
            PaneMsg::Link(Link::Copy(text)) => return self.copy_notice(text.to_string()),
            PaneMsg::Link(Link::Spoiler(id)) | PaneMsg::Reveal(id) => {
                pane.revealed.insert(id);
                // A spoiler photo is loaded only once revealed.
                let photo = pane
                    .messages
                    .iter()
                    .find(|m| m.id == id)
                    .and_then(|m| m.media.as_ref())
                    .and_then(|media| match media {
                        Media::Photo { file_id, .. } => Some(*file_id),
                        _ => None,
                    });
                if let Some(file_id) = photo {
                    return self.show_photo(file_id);
                }
            }
            PaneMsg::Reply(id) => {
                pane.menu = None;
                pane.finish_edit();
                pane.reply_to = Some(id);
            }
            PaneMsg::CancelReply => pane.reply_to = None,
            PaneMsg::JumpTo(id) => return self.jump(window, id),
            PaneMsg::AroundLoaded(chat_id, target, result) => {
                // A later jump already changed what is awaited: drop this
                // stale answer instead of acting on an abandoned jump.
                if pane.shows(chat_id) && pane.jump_pending == Some(target) {
                    match result {
                        Ok(messages) => {
                            return self.show_around(window, chat_id, target, &messages);
                        }
                        Err(e) => {
                            pane.jump_pending = None;
                            self.error = Some(e);
                        }
                    }
                }
            }
            PaneMsg::NewerLoaded(chat_id, after, result) => {
                let current =
                    pane.shows(chat_id) && pane.messages.last().map(|m| m.id) == Some(after);
                match result {
                    Ok(messages) if current => {
                        return self.append_newer(window, chat_id, after, &messages);
                    }
                    Ok(_) => pane.loading_newer = false,
                    Err(e) => {
                        pane.loading_newer = false;
                        self.error = Some(e);
                    }
                }
            }
            PaneMsg::ToLatest => return self.back_to_latest(window),
            PaneMsg::SearchToggle => {
                pane.search = match pane.search.take() {
                    Some(_) => None,
                    None => Some(pane::ChatSearch::default()),
                };
            }
            PaneMsg::SearchQuery(q) => {
                if let Some(search) = &mut pane.search {
                    search.query = q;
                    search.results = None;
                    search.scroll = 0.0;
                    search.view_height = None;
                    search.scroll_id = iced::widget::Id::unique();
                    search.paging = Default::default();
                }
            }
            PaneMsg::SearchSubmit => {
                let id = pane.search.as_mut().map(|search| {
                    search.scroll = 0.0;
                    search.view_height = None;
                    search.scroll_id = iced::widget::Id::unique();
                    search.scroll_id.clone()
                });
                let task = self.search_submit(window);
                return if let Some(id) = id {
                    Task::batch([
                        task,
                        iced::widget::operation::scroll_to(
                            id,
                            iced::widget::scrollable::AbsoluteOffset { x: 0.0, y: 0.0 },
                        ),
                    ])
                } else {
                    task
                };
            }
            PaneMsg::SearchMore => return self.search_more(window),
            PaneMsg::SearchResults(chat_id, query, request, cursor, result) => {
                self.search_results(window, chat_id, &query, request, cursor, result);
            }
            PaneMsg::OpenMenu(id) => {
                let Some(chat_id) = pane.chat_id else {
                    return Task::none();
                };
                let deleted = pane.messages.iter().any(|m| m.id == id && m.deleted);
                // A message deleted on the server has nothing to ask TDLib about.
                let rights = deleted.then_some(td::MessageRights {
                    delete_for_self: false,
                    delete_for_all: false,
                    edit: false,
                });
                pane.menu = Some(Menu {
                    message_id: id,
                    rights,
                    reactions: Vec::new(),
                });
                if !deleted {
                    return Task::batch([
                        Task::perform(
                            td::message_rights(self.session.client_id, chat_id, id),
                            move |r| Msg::Pane(window, PaneMsg::MenuReady(id, r)),
                        ),
                        Task::perform(
                            td::available_reactions(self.session.client_id, chat_id, id),
                            move |r| Msg::Pane(window, PaneMsg::ReactionsReady(id, r)),
                        ),
                    ]);
                }
            }
            PaneMsg::MenuReady(id, result) => {
                if let Some(menu) = &mut pane.menu
                    && menu.message_id == id
                {
                    match result {
                        Ok(rights) => menu.rights = Some(rights),
                        Err(e) => {
                            pane.menu = None;
                            self.error = Some(e);
                        }
                    }
                }
            }
            PaneMsg::ReactionsReady(id, result) => {
                if let Some(menu) = &mut pane.menu
                    && menu.message_id == id
                {
                    menu.reactions = result.unwrap_or_default();
                }
            }
            PaneMsg::DraftLoaded(chat_id, result) => {
                if let Ok(text) = result
                    && pane.shows(chat_id)
                    && pane.compose.text().is_empty()
                {
                    pane.compose = iced::widget::text_editor::Content::with_text(&text);
                    pane.compose_touched = true;
                    self.session.synced_drafts.insert(chat_id, text);
                }
            }
            PaneMsg::TogglePicker => {
                if pane.picker.take().is_none() {
                    return self.picker_tab(window, picker::Tab::Emoji);
                }
            }
            PaneMsg::PickerTab(tab) => return self.picker_tab(window, tab),
            PaneMsg::ClosePicker => pane.picker = None,
            PaneMsg::InsertEmoji(emoji) => {
                pane.compose_touched = true;
                picker::insert(&mut pane.compose, &emoji);
            }
            PaneMsg::SendSticker(sticker) => {
                if let Some(chat_id) = pane.chat_id {
                    pane.picker = None;
                    // Telegram just moved it to the front of "recent": load
                    // that list again next time the tab opens instead of
                    // showing a stale one.
                    self.session.stickers.recent = None;
                    return Task::perform(
                        td::send_sticker(self.session.client_id, chat_id, *sticker),
                        Msg::Done,
                    );
                }
            }
            PaneMsg::Vote(id, option) => {
                if let Some(chat_id) = pane.chat_id {
                    return Task::perform(
                        td::vote(self.session.client_id, chat_id, id, vec![option]),
                        Msg::Done,
                    );
                }
            }
            PaneMsg::ToggleVote(id, option) => {
                let picked = pane.poll_selection.entry(id).or_default();
                if !picked.remove(&option) {
                    picked.insert(option);
                }
            }
            PaneMsg::SubmitVote(id) => {
                if let Some(chat_id) = pane.chat_id {
                    let options: Vec<i32> = pane
                        .poll_selection
                        .remove(&id)
                        .unwrap_or_default()
                        .into_iter()
                        .collect();
                    if !options.is_empty() {
                        return Task::perform(
                            td::vote(self.session.client_id, chat_id, id, options),
                            Msg::Done,
                        );
                    }
                }
            }
            PaneMsg::React(id, key) => {
                pane.menu = None;
                let Some(chat_id) = pane.chat_id else {
                    return Task::none();
                };
                // A reaction already put by the user is taken back.
                let on = !pane
                    .messages
                    .iter()
                    .find(|m| m.id == id)
                    .is_some_and(|m| m.reacted(&key));
                return Task::perform(
                    td::set_reaction(self.session.client_id, chat_id, id, key, on),
                    Msg::Done,
                );
            }
            PaneMsg::Forward(ids) => {
                pane.menu = None;
                pane.forward = Some((ids, String::new()));
            }
            PaneMsg::ForwardSelection => {
                if let Some(selected) = &pane.selected {
                    pane.forward = Some((selected.iter().copied().collect(), String::new()));
                }
            }
            PaneMsg::ForwardQuery(query) => {
                if let Some((_, q)) = &mut pane.forward {
                    *q = query;
                }
            }
            PaneMsg::CancelForward => pane.forward = None,
            PaneMsg::ForwardTo(to) => {
                let (Some(from), Some((ids, _))) = (pane.chat_id, pane.forward.take()) else {
                    return Task::none();
                };
                pane.selected = None;
                pane.delete_selection = None;
                // Like Telegram: the chat the messages went to opens.
                let forward = Task::perform(
                    td::forward(self.session.client_id, to, from, ids),
                    Msg::Done,
                );
                return Task::batch([forward, self.select_chat(window, to)]);
            }
            PaneMsg::Select(id) => {
                pane.menu = None;
                let selected = pane.selected.get_or_insert_with(Default::default);
                if !selected.remove(&id) {
                    selected.insert(id);
                }
                if selected.is_empty() {
                    pane.selected = None;
                }
                pane.delete_selection = None;
            }
            PaneMsg::ClearSelection => {
                pane.selected = None;
                pane.delete_selection = None;
            }
            PaneMsg::CopySelection => {
                let Some(selected) = pane.selected.take() else {
                    return Task::none();
                };
                pane.delete_selection = None;
                let picked: Vec<(bool, MessageSender, String)> = pane
                    .messages
                    .iter()
                    .filter(|m| selected.contains(&m.id))
                    .map(|m| (m.outgoing, m.sender.clone(), m.text.clone()))
                    .collect();
                let me = self
                    .session
                    .users
                    .get(&self.session.my_id.unwrap_or_default())
                    .map_or("Я", String::as_str);
                let lines: Vec<String> = picked
                    .iter()
                    .map(|(outgoing, sender, text)| {
                        let name = if *outgoing {
                            me
                        } else {
                            self.sender_display(sender)
                        };
                        format!("{name}: {text}")
                    })
                    .collect();
                return iced::clipboard::write(lines.join("\n"));
            }
            PaneMsg::DeleteSelection => {
                let (Some(chat_id), Some(selected)) = (pane.chat_id, &pane.selected) else {
                    return Task::none();
                };
                pane.delete_selection = Some(None);
                let ids: Vec<i64> = selected.iter().copied().collect();
                return Task::perform(
                    td::common_rights(self.session.client_id, chat_id, ids),
                    move |r| Msg::Pane(window, PaneMsg::SelectionRights(r)),
                );
            }
            PaneMsg::SelectionRights(result) => match result {
                Ok(rights) if pane.delete_selection.is_some() => {
                    pane.delete_selection = Some(Some(rights));
                }
                Ok(_) => {}
                Err(e) => {
                    pane.delete_selection = None;
                    self.error = Some(e);
                }
            },
            PaneMsg::DeleteSelected { revoke } => {
                let (Some(chat_id), Some(selected)) = (pane.chat_id, pane.selected.take()) else {
                    return Task::none();
                };
                pane.delete_selection = None;
                let ids: Vec<i64> = selected.into_iter().collect();
                self.session
                    .self_deleted
                    .extend(ids.iter().map(|&id| (chat_id, id)));
                let done_ids = ids.clone();
                return Task::perform(
                    td::delete_messages(self.session.client_id, chat_id, ids, revoke),
                    move |r| Msg::DeleteDone(chat_id, done_ids.clone(), r),
                );
            }
            PaneMsg::TextPress(message, at) => {
                pane.text_selection = Some(pane::TextSelection {
                    message,
                    anchor: at,
                    focus: at,
                    dragging: true,
                });
            }
            PaneMsg::TextDrag(message, at) => {
                if let Some(sel) = &mut pane.text_selection
                    && sel.message == message
                    && sel.dragging
                {
                    sel.focus = at;
                }
            }
            PaneMsg::ClickOutside => {
                pane.menu = None;
                pane.text_selection = None;
            }
            PaneMsg::CloseMenu => pane.menu = None,
            PaneMsg::CopyText(id) => {
                pane.menu = None;
                // A selection in this message is what gets copied.
                if pane.text_selection.is_some_and(|s| s.message == id)
                    && let Some(text) = pane.selected_text()
                {
                    return iced::clipboard::write(text);
                }
                if let Some(m) = pane.messages.iter().find(|m| m.id == id) {
                    return iced::clipboard::write(m.text.clone());
                }
            }
            PaneMsg::Delete { id, revoke } => {
                pane.menu = None;
                if let Some(chat_id) = pane.chat_id {
                    self.session.self_deleted.insert((chat_id, id));
                    return Task::perform(
                        td::delete_message(self.session.client_id, chat_id, id, revoke),
                        move |r| Msg::DeleteDone(chat_id, vec![id], r),
                    );
                }
            }
            PaneMsg::Forget(id) => {
                pane.menu = None;
                if let Some(chat_id) = pane.chat_id {
                    self.forget(chat_id, &[id]);
                }
            }
            PaneMsg::MediaVisible(file_id) => return self.show_photo(file_id),
            PaneMsg::MediaHidden(file_id) => {
                self.session.wanted_photos.remove(&file_id);
            }
            PaneMsg::Attach => {
                return Task::perform(
                    async {
                        rfd::AsyncFileDialog::new()
                            .set_title("Отправить файлы")
                            .pick_files()
                            .await
                            .map(|files| files.iter().map(|f| f.path().to_path_buf()).collect())
                            .unwrap_or_default()
                    },
                    move |paths| Msg::Pane(window, PaneMsg::FilesChosen(paths)),
                );
            }
            PaneMsg::FilesChosen(paths) => return self.send_files(window, paths),
        }
        Task::none()
    }
}
