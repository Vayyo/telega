//! TDLib update handling and propagation to chat panes.

use super::{
    App, ChatItem, Msg, MsgItem, avatars, extra, media, pane::ColdContent,
    plugin_runtime::message_event, reactions_of, rich, rich_of, session::FolderReorderState, view,
};
use crate::{archive::Archive, plugins, td};
use iced::Task;
use tdlib_rs::enums::{ChatList, MessageSender, OptionValue, Update};

impl App {
    pub(super) fn on_update(&mut self, update: Update) -> Task<Msg> {
        match update {
            Update::AuthorizationState(u) => return self.on_auth_state(u.authorization_state),
            Update::Option(u) => {
                if u.name == "my_id"
                    && let OptionValue::Integer(v) = u.value
                {
                    self.session.my_id = Some(v.value);
                    if self.pending_transition().is_some() {
                        return Task::none();
                    }
                    if self.session.archive.is_none() && self.session.rekeying_archive.is_none() {
                        match Archive::open_for_account(v.value) {
                            Ok(archive) => self.attach_archive(archive),
                            Err(e) => self.error = Some(format!("архив: {e}")),
                        }
                    }
                    self.announce_account();
                    let known = self.account_known(v.value);
                    return Task::batch([known, self.restore_tabs()]);
                }
            }
            Update::NewChat(u) => {
                let chat = u.chat;
                if let Some(m) = &chat.last_message {
                    self.archive_save(&MsgItem::from(m));
                }
                let last = chat
                    .last_message
                    .as_ref()
                    .map(|m| rich::preview(&m.content))
                    .unwrap_or_default();
                let preview = view::preview_text(&last);
                let title_lower = chat.title.to_lowercase();
                self.session.chats.insert(
                    chat.id,
                    ChatItem {
                        title: chat.title,
                        title_lower,
                        last,
                        preview,
                        unread: chat.unread_count,
                        order: 0,
                        last_id: chat.last_message.as_ref().map_or(0, |m| m.id),
                        last_outgoing: chat.last_message.as_ref().is_some_and(|m| m.is_outgoing),
                        last_pending: chat
                            .last_message
                            .as_ref()
                            .is_some_and(|m| m.sending_state.is_some()),
                        read_outbox: chat.last_read_outbox_message_id,
                        read_inbox: chat.last_read_inbox_message_id,
                        archive_order: 0,
                        pinned: false,
                        notify: chat.notification_settings.clone(),
                        folders: Vec::new(),
                        kind: Some(chat.r#type.clone()),
                        draft: td::draft_text(chat.draft_message.as_ref()),
                        private: matches!(
                            chat.r#type,
                            tdlib_rs::enums::ChatType::Private(_)
                                | tdlib_rs::enums::ChatType::Secret(_)
                        ),
                    },
                );
                self.set_positions(chat.id, &chat.positions);
                let peer = avatars::Peer::Chat(chat.id);
                self.set_profile_avatar(peer, chat.photo.as_ref().map(|p| &p.big));
                return self.set_avatar(peer, chat.photo.as_ref().map(|p| &p.small));
            }
            Update::ChatPhoto(u) => {
                let peer = avatars::Peer::Chat(u.chat_id);
                self.set_profile_avatar(peer, u.photo.as_ref().map(|p| &p.big));
                return self.set_avatar(peer, u.photo.as_ref().map(|p| &p.small));
            }
            Update::ChatTitle(u) => {
                if let Some(chat) = self.session.chats.get_mut(&u.chat_id) {
                    chat.title_lower = u.title.to_lowercase();
                    chat.title = u.title;
                }
            }
            Update::UnreadMessageCount(u) if matches!(u.chat_list, ChatList::Folder(_)) => {
                if let ChatList::Folder(f) = u.chat_list {
                    self.session
                        .folder_unread
                        .insert(f.chat_folder_id, (u.unread_count, u.unread_unmuted_count));
                }
            }
            Update::UnreadMessageCount(u) => {
                if matches!(u.chat_list, ChatList::Main) {
                    self.session.unread = (u.unread_count, u.unread_unmuted_count);
                    #[cfg(target_os = "linux")]
                    if let Some(tray) = &self.tray {
                        return tray.show(self.badge());
                    }
                }
            }
            Update::ChatLastMessage(u) => {
                if let Some(m) = &u.last_message {
                    self.archive_save(&MsgItem::from(m));
                }
                if let Some(chat) = self.session.chats.get_mut(&u.chat_id) {
                    chat.last = u
                        .last_message
                        .as_ref()
                        .map(|m| rich::preview(&m.content))
                        .unwrap_or_default();
                    chat.preview = view::preview_text(&chat.last);
                    if let Some(m) = &u.last_message {
                        chat.last_id = m.id;
                    }
                    chat.last_outgoing = u.last_message.as_ref().is_some_and(|m| m.is_outgoing);
                    chat.last_pending = u
                        .last_message
                        .as_ref()
                        .is_some_and(|m| m.sending_state.is_some());
                }
                self.set_positions(u.chat_id, &u.positions);
            }
            Update::ChatPosition(u) => match u.position.list {
                ChatList::Main => {
                    self.set_order(u.chat_id, u.position.order);
                    if let Some(chat) = self.session.chats.get_mut(&u.chat_id)
                        && (u.position.order != 0 || chat.archive_order == 0)
                    {
                        chat.pinned = u.position.is_pinned;
                    }
                }
                ChatList::Archive => {
                    self.set_archive_order(u.chat_id, u.position.order);
                    if let Some(chat) = self.session.chats.get_mut(&u.chat_id)
                        && (u.position.order != 0 || chat.order == 0)
                    {
                        chat.pinned = u.position.is_pinned;
                    }
                }
                ChatList::Folder(f) => {
                    self.set_folder_order(u.chat_id, f.chat_folder_id, u.position.order);
                }
            },
            Update::ChatFolders(u) => {
                self.session.folders = u
                    .chat_folders
                    .iter()
                    .map(|f| (f.id, f.name.text.text.clone()))
                    .collect();
                self.session.main_tab =
                    (u.main_chat_list_position.max(0) as usize).min(self.session.folders.len());
                let known: Vec<i32> = self.session.folders.iter().map(|&(id, _)| id).collect();
                if let Some(pending) = self.session.folder_creation.as_mut() {
                    pending.observed = self
                        .session
                        .folders
                        .iter()
                        .filter(|(id, _)| !pending.initial.contains(id))
                        .map(|(id, _)| *id)
                        .collect();
                    if pending.id.is_some_and(|id| pending.observed.contains(&id)) {
                        let window = pending.window;
                        let chat_id = pending.chat_id;
                        let request = pending.request;
                        self.session.folder_creation = None;
                        self.session.folder_creation_warning = None;
                        if let Some(pane) = self.session.panes.get_mut(&window)
                            && pane.list.menu == Some(chat_id)
                            && pane.list.new_folder_request == Some(request)
                        {
                            pane.list.new_folder_name = None;
                            pane.list.new_folder_request = None;
                        }
                    }
                }
                if let Some(pending) = self.session.folder_reorder.as_mut() {
                    let matches_request = known == pending.expected
                        && self.session.main_tab == pending.main_tab as usize;
                    let unchanged_order = known == pending.initial
                        && self.session.main_tab == pending.initial_main_tab;
                    if matches_request {
                        if matches!(
                            pending.state,
                            FolderReorderState::AwaitingUpdate
                                | FolderReorderState::DiscrepancyAfterReply
                        ) {
                            self.session.folder_reorder = None;
                        } else {
                            pending.state = FolderReorderState::MatchedBeforeReply;
                        }
                        self.session.folder_reorder_error = None;
                    } else if unchanged_order {
                        if pending.state == FolderReorderState::MatchedBeforeReply {
                            pending.state = FolderReorderState::AwaitingReply;
                        }
                        // Metadata-only updates cannot confirm a reorder, and
                        // cannot resolve an existing order discrepancy either.
                    } else {
                        pending.state = match pending.state {
                            FolderReorderState::AwaitingUpdate
                            | FolderReorderState::DiscrepancyAfterReply => {
                                FolderReorderState::DiscrepancyAfterReply
                            }
                            _ => FolderReorderState::DiscrepancyBeforeReply,
                        };
                        self.session.folder_reorder_error = Some(
                            "Порядок папок расходится с запросом; результат операции неизвестен"
                                .into(),
                        );
                    }
                }
                self.session.folder_lists.retain(|id, _| known.contains(id));
                // A pane showing a folder that is gone falls back to all chats.
                for pane in self.all_panes_mut() {
                    if pane.list.folder.is_some_and(|f| !known.contains(&f)) {
                        pane.list.folder = None;
                        pane.list.confirm_read = None;
                        pane.list.read_feedback = None;
                    }
                    if let Some(picker) = &mut pane.list.folder_picker
                        && picker
                            .pending
                            .is_some_and(|(_, folder)| !known.contains(&folder))
                    {
                        picker.pending = None;
                        picker.feedback = Some("Папка больше не существует".into());
                    }
                }
            }
            Update::ChatDraftMessage(u) => {
                if let Some(chat) = self.session.chats.get_mut(&u.chat_id) {
                    chat.draft = td::draft_text(u.draft_message.as_ref());
                }
                self.set_positions(u.chat_id, &u.positions);
            }
            Update::ChatNotificationSettings(u) => {
                if let Some(chat) = self.session.chats.get_mut(&u.chat_id) {
                    chat.notify = u.notification_settings;
                }
            }
            Update::ChatAction(u) => {
                // Own actions from other devices, and topic threads, are not shown.
                let own = matches!(&u.sender_id, MessageSender::User(s) if Some(s.user_id) == self.session.my_id);
                if !own && u.topic_id.is_none() {
                    let typing = matches!(u.action, tdlib_rs::enums::ChatAction::Typing);
                    self.session.typing.set(
                        u.chat_id,
                        u.sender_id,
                        typing,
                        std::time::Instant::now(),
                    );
                }
            }
            Update::MessageIsPinned(u) => {
                let client_id = self.session.client_id;
                return Task::perform(td::pinned_messages(client_id, u.chat_id), move |r| {
                    Msg::ForClient(client_id, Box::new(Msg::PinnedRefreshed(u.chat_id, r)))
                });
            }
            Update::ChatReadOutbox(u) => {
                if let Some(chat) = self.session.chats.get_mut(&u.chat_id) {
                    chat.read_outbox = u.last_read_outbox_message_id;
                }
            }
            Update::ChatReadInbox(u) => {
                if let Some(chat) = self.session.chats.get_mut(&u.chat_id) {
                    chat.unread = u.unread_count;
                    chat.read_inbox = u.last_read_inbox_message_id;
                }
            }
            Update::User(u) => {
                let user = u.user;
                let is_bot = matches!(&user.r#type, tdlib_rs::enums::UserType::Bot(_));
                if is_bot {
                    self.session.bot_users.insert(user.id);
                } else {
                    self.session.bot_users.remove(&user.id);
                }
                for pane in self.session.panes.values_mut() {
                    if let Some((_, Some(Ok(profile)))) = &mut pane.profile
                        && let Some(member) = profile.members.iter_mut().find(|m| m.id == user.id)
                    {
                        member.is_bot = is_bot;
                    }
                }
                let name = format!("{} {}", user.first_name, user.last_name);
                self.session.users.insert(user.id, name.trim().to_owned());
                self.user_renamed(user.id);
                let peer = avatars::Peer::User(user.id);
                self.set_profile_avatar(peer, user.profile_photo.as_ref().map(|p| &p.big));
                return self.set_avatar(peer, user.profile_photo.as_ref().map(|p| &p.small));
            }
            Update::NewMessage(u) => {
                let m = u.message;
                let (item, files) = MsgItem::parse(&m);
                self.note_files_of(&files);
                self.archive_save(&item);
                // Pending outgoing messages are reported once confirmed.
                if m.sending_state.is_none() {
                    self.plugin_event(message_event(&m));
                }
                // Inactive cold tabs keep only pending sends. An active
                // latest-page restore receives live messages like any other
                // visible chat; they are merged over a possibly older page.
                let mut shown = false;
                for pane in self.panes_of(m.chat_id) {
                    let restoring_latest = pane
                        .cold
                        .as_ref()
                        .is_some_and(|cold| cold.request.is_some() && cold.anchor.is_none());
                    shown |= pane.cold.is_none() || restoring_latest;
                    if (item.pending
                        || pane.cold.is_none() && pane.newest_loaded
                        || restoring_latest)
                        && !pane.messages.iter().any(|x| x.id == m.id)
                    {
                        pane.messages.push(item.clone());
                    }
                }
                let replies = if shown {
                    self.fetch_replies(m.chat_id, std::slice::from_ref(&item))
                } else {
                    Task::none()
                };
                // Read only when on screen in a window, not in a background tab.
                let on_screen = self.session.panes.values().any(|p| p.shows(m.chat_id));
                if on_screen && !m.is_outgoing {
                    return Task::batch([
                        Task::perform(
                            td::mark_read(self.session.client_id, m.chat_id, m.id),
                            |()| Msg::Ignore,
                        ),
                        replies,
                    ]);
                }
                return replies;
            }
            // Outgoing messages get a temporary id until the server confirms them.
            Update::MessageSendSucceeded(u) => {
                let (item, files) = MsgItem::parse(&u.message);
                self.note_files_of(&files);
                self.archive_save(&item);
                self.plugin_event(message_event(&u.message));
                for pane in self.panes_of(u.message.chat_id) {
                    if let Some(slot) = pane.find_mut(u.old_message_id) {
                        *slot = item.clone();
                    }
                }
            }
            // The server rejected the send: show why and mark the message
            // failed instead of leaving it "sending" forever.
            Update::MessageSendFailed(u) => {
                self.error = Some(format!("не отправлено: {}", u.error.message));
                for pane in self.panes_of(u.message.chat_id) {
                    if let Some(item) = pane.find_mut(u.old_message_id) {
                        *item = MsgItem::from(&u.message);
                    }
                }
            }
            Update::MessageContent(u) => {
                let text = td::message_text(&u.new_content);
                self.note_files(&u.new_content);
                self.plugin_event(plugins::Event::Edited {
                    chat_id: u.chat_id,
                    id: u.message_id,
                    text: text.clone(),
                });
                if self.key_transition_active() {
                    self.defer_archive_edit(u.chat_id, u.message_id, text.clone());
                } else if let Some(archive) =
                    Self::active_archive(&mut self.session.archive, self.settings.keep_deleted)
                    && let Err(e) = archive.set_text(u.chat_id, u.message_id, &text)
                {
                    self.error = Some(format!("архив: {e}"));
                }
                let new_media = media::media_of(&u.new_content).map(|(m, _)| m);
                if self.session.video.as_ref().is_some_and(|video| {
                    video.chat_id == u.chat_id
                        && video.message_id == u.message_id
                        && !matches!(
                            new_media.as_ref(),
                            Some(media::Media::Video { file_id, round, .. })
                                if *file_id == video.file_id && *round == video.round
                        )
                }) {
                    self.persist_video_volume();
                    self.session.video = None;
                }
                let new_extra = extra::extra_of(&u.new_content).map(|(e, _)| Box::new(e));
                let new_rich = rich_of(&u.new_content, new_extra.as_deref());
                let picture = |media: &Option<media::Media>, extra: Option<&extra::Extra>| {
                    let source = media.as_ref().map(|m| {
                        let (thumb, spoiler) = match m {
                            media::Media::Photo { spoiler, .. } => (None, *spoiler),
                            media::Media::Video { thumb, spoiler, .. }
                            | media::Media::Animation { thumb, spoiler, .. } => (*thumb, *spoiler),
                            _ => (None, false),
                        };
                        (std::mem::discriminant(m), m.file_id(), thumb, spoiler)
                    });
                    let link = match extra {
                        Some(extra::Extra::Link { thumb, .. }) => *thumb,
                        _ => None,
                    };
                    (source, link)
                };
                let new_picture = picture(&new_media, new_extra.as_deref());
                let mut photo_changed = false;
                for pane in self.panes_of(u.chat_id) {
                    if let Some(change) = pane.cold_change(u.message_id) {
                        change.content = Some(ColdContent {
                            text: text.clone(),
                            rich: new_rich.clone(),
                            media: new_media.clone(),
                            extra: new_extra.clone(),
                        });
                    }
                    if let Some(item) = pane.find_mut(u.message_id) {
                        photo_changed |= picture(&item.media, item.extra.as_deref()) != new_picture;
                        item.media = new_media.clone();
                        item.text = text.clone();
                        item.rich = new_rich.clone();
                        item.extra = new_extra.clone();
                    }
                }
                if photo_changed {
                    self.clear_message_photos(u.chat_id, u.message_id);
                }
                // A cached preview of this message (shown as another
                // message's reply-to) would otherwise keep the pre-edit text.
                if let Some(item) = self
                    .session
                    .reply_previews
                    .get_mut(&(u.chat_id, u.message_id))
                {
                    item.media = new_media;
                    item.text = text;
                    item.rich = new_rich;
                    item.extra = new_extra;
                }
            }
            Update::MessageInteractionInfo(u) => {
                let reactions = reactions_of(u.interaction_info.as_ref());
                for pane in self.panes_of(u.chat_id) {
                    if let Some(change) = pane.cold_change(u.message_id) {
                        change.reactions = Some(reactions.clone());
                    }
                    if let Some(item) = pane.find_mut(u.message_id) {
                        item.reactions = reactions.clone();
                    }
                }
            }
            Update::MessageEdited(u) => {
                for pane in self.panes_of(u.chat_id) {
                    if let Some(change) = pane.cold_change(u.message_id) {
                        change.edited = Some(u.edit_date > 0);
                    }
                    if let Some(item) = pane.find_mut(u.message_id) {
                        item.edited = u.edit_date > 0;
                    }
                }
            }
            // `from_cache` = only evicted from TDLib's cache, not really deleted.
            Update::File(u) => return self.on_file(&u.file),
            Update::NotificationGroup(u) => return self.on_notification_group(&u),
            Update::DeleteMessages(u) if u.is_permanent && !u.from_cache => {
                let keep_deleted = self.settings.keep_deleted && self.session.archive.is_some();
                let own = &self.session.self_deleted;
                for pane in self
                    .session
                    .panes
                    .values_mut()
                    .chain(self.session.background.values_mut())
                    .filter(|pane| pane.shows(u.chat_id))
                {
                    for &id in &u.message_ids {
                        if let Some(change) = pane.cold_change(id) {
                            change.purged = own.contains(&(u.chat_id, id));
                            change.deleted = Some(keep_deleted && !change.purged);
                        }
                    }
                }
                self.plugin_event(plugins::Event::Deleted {
                    chat_id: u.chat_id,
                    ids: u.message_ids.clone(),
                });
                self.on_deleted(u.chat_id, &u.message_ids)
            }
            _ => {}
        }
        Task::none()
    }
}
