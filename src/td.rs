//! Thin layer over TDLib: update stream, requests, text extraction.

use iced::futures::{SinkExt, Stream};
use tdlib_rs::{
    enums::{self, InputMessageContent, MessageContent, Update},
    functions,
    types::{FormattedText, InputMessageText, Message},
};

pub const HISTORY_LIMIT: usize = 50;

/// Endless stream of TDLib updates tagged with their client id. `receive`
/// blocks up to 2 s, so it runs on the blocking pool, never on the UI or async
/// worker threads. The stream outlives clients: after log out a new client is
/// created and its updates arrive here too.
pub fn updates() -> impl Stream<Item = (i32, Box<Update>)> {
    iced::stream::channel(256, async |mut tx| {
        loop {
            let received = tokio::task::spawn_blocking(tdlib_rs::receive)
                .await
                .expect("tdlib receive task panicked");
            if let Some((update, client_id)) = received
                && tx.send((client_id, Box::new(update))).await.is_err()
            {
                return;
            }
        }
    })
}

/// Logs out and wipes the local session; TDLib then closes this client.
pub async fn log_out(client_id: i32) -> TdResult<()> {
    functions::log_out(client_id).await.map_err(err)
}

/// Closes the client without logging out: the session stays on disk.
pub async fn close(client_id: i32) -> TdResult<()> {
    functions::close(client_id).await.map_err(err)
}

/// Re-encrypts the TDLib database with a new key ("" = none).
pub async fn set_db_key(client_id: i32, key: crate::lock::SecretString) -> TdResult<()> {
    tdlib_rs::send_secret_request(
        client_id,
        serde_json::json!({"@type": "setDatabaseEncryptionKey", "new_encryption_key": &*key}),
    )
    .await
    .map_err(err)
}

/// What the current user may do with a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MessageRights {
    pub delete_for_self: bool,
    pub delete_for_all: bool,
    pub edit: bool,
}

pub async fn message_rights(
    client_id: i32,
    chat_id: i64,
    message_id: i64,
) -> TdResult<MessageRights> {
    let enums::MessageProperties::MessageProperties(p) =
        functions::get_message_properties(chat_id, message_id, client_id)
            .await
            .map_err(err)?;
    Ok(MessageRights {
        delete_for_self: p.can_be_deleted_only_for_self,
        delete_for_all: p.can_be_deleted_for_all_users,
        edit: p.can_be_edited,
    })
}

/// `revoke` = delete for everyone, otherwise only for the current user.
pub async fn delete_message(
    client_id: i32,
    chat_id: i64,
    message_id: i64,
    revoke: bool,
) -> TdResult<()> {
    functions::delete_messages(chat_id, vec![message_id], revoke, client_id)
        .await
        .map_err(err)
}

/// What may be done with every one of `ids` (deleting a selection).
pub async fn common_rights(client_id: i32, chat_id: i64, ids: Vec<i64>) -> TdResult<MessageRights> {
    let mut common = MessageRights {
        delete_for_self: true,
        delete_for_all: true,
        edit: false,
    };
    for id in ids {
        let rights = message_rights(client_id, chat_id, id).await?;
        common.delete_for_self &= rights.delete_for_self;
        common.delete_for_all &= rights.delete_for_all;
    }
    Ok(common)
}

/// The ids among `ids` of messages the current user sent. Plugin requests
/// are filtered through this: they must never remove someone else's
/// messages, even in chats where the user is admin.
pub async fn own_messages(client_id: i32, chat_id: i64, ids: Vec<i64>) -> TdResult<Vec<i64>> {
    let mut own = Vec::with_capacity(ids.len());
    for id in ids {
        let enums::Message::Message(m) = functions::get_message(chat_id, id, client_id)
            .await
            .map_err(err)?;
        if m.is_outgoing {
            own.push(id);
        }
    }
    Ok(own)
}

pub async fn delete_messages(
    client_id: i32,
    chat_id: i64,
    ids: Vec<i64>,
    revoke: bool,
) -> TdResult<()> {
    functions::delete_messages(chat_id, ids, revoke, client_id)
        .await
        .map_err(err)
}

/// Text exactly as given (no markdown), leaving the user's draft alone:
/// what plugins send.
pub async fn send_plain(client_id: i32, chat_id: i64, text: String) -> TdResult<()> {
    let content = InputMessageContent::InputMessageText(InputMessageText {
        text: FormattedText {
            text,
            entities: Vec::new(),
        },
        link_preview_options: None,
        clear_draft: false,
    });
    functions::send_message(chat_id, None, None, None, content, client_id)
        .await
        .map(|_| ())
        .map_err(err)
}

/// Seconds to wait from a Telegram flood error ("… retry after N (429)").
pub fn flood_wait(error: &str) -> Option<u64> {
    let rest = error.split("retry after ").nth(1)?;
    rest.split(|c: char| !c.is_ascii_digit())
        .next()?
        .parse()
        .ok()
}

pub type TdResult<T> = Result<T, String>;

fn err(e: tdlib_rs::types::Error) -> String {
    format!("{} ({})", e.message, e.code)
}

/// Any request activates the client; TDLib emits no updates before the first one.
pub async fn start(client_id: i32) -> TdResult<()> {
    functions::set_log_verbosity_level(1, client_id)
        .await
        .map_err(err)
}

pub async fn set_parameters(
    client_id: i32,
    api_id: i32,
    api_hash: String,
    slot: u32,
    key: crate::lock::SecretString,
    limits: crate::settings::CacheLimits,
) -> TdResult<Option<String>> {
    tdlib_rs::send_secret_request(
        client_id,
        serde_json::json!({
            "@type": "setTdlibParameters",
            "use_test_dc": false,
            "database_directory": crate::paths::db_dir(slot).to_string_lossy(),
            "files_directory": crate::paths::files_dir(slot).to_string_lossy(),
            "database_encryption_key": &*key,
            "use_file_database": true,
            "use_chat_info_database": true,
            "use_message_database": true,
            "use_secret_chats": false,
            "api_id": api_id,
            "api_hash": api_hash,
            "system_language_code": "ru",
            "device_model": "Desktop",
            "system_version": "",
            "application_version": env!("CARGO_PKG_VERSION"),
        }),
    )
    .await
    .map_err(err)?;
    // Requests before accepted parameters can hang. A failed cache option is
    // reported separately and never prevents the login from continuing.
    Ok(apply_cache_limits(client_id, limits).await.err())
}

pub(crate) fn storage_limits(limits: crate::settings::CacheLimits) -> [(&'static str, i64); 3] {
    [
        (
            "storage_max_files_size",
            (limits.bytes / 1024).clamp(1, i32::MAX as u64) as i64,
        ),
        (
            "storage_max_time_from_last_access",
            limits.days.saturating_mul(86_400).clamp(1, i32::MAX as u64) as i64,
        ),
        ("storage_max_file_count", i32::MAX as i64),
    ]
}

pub async fn apply_cache_limits(
    client_id: i32,
    limits: crate::settings::CacheLimits,
) -> TdResult<()> {
    let mut first_error = None;
    for (name, value) in storage_limits(limits) {
        if let Err(e) = functions::set_option(
            name.into(),
            Some(enums::OptionValue::Integer(
                tdlib_rs::types::OptionValueInteger { value },
            )),
            client_id,
        )
        .await
        {
            first_error.get_or_insert_with(|| format!("{name}: {}", err(e)));
        }
    }
    if let Err(e) = functions::set_option(
        "use_storage_optimizer".into(),
        Some(enums::OptionValue::Boolean(
            tdlib_rs::types::OptionValueBoolean { value: true },
        )),
        client_id,
    )
    .await
    {
        first_error.get_or_insert_with(|| format!("use_storage_optimizer: {}", err(e)));
    }
    first_error.map_or(Ok(()), Err)
}

pub async fn send_phone(client_id: i32, phone: String) -> TdResult<()> {
    functions::set_authentication_phone_number(phone, None, client_id)
        .await
        .map_err(err)
}

/// Switches the login to a QR code (state `WaitOtherDeviceConfirmation`).
pub async fn request_qr(client_id: i32) -> TdResult<()> {
    functions::request_qr_code_authentication(Vec::new(), client_id)
        .await
        .map_err(err)
}

pub async fn send_code(client_id: i32, code: String) -> TdResult<()> {
    functions::check_authentication_code(code, client_id)
        .await
        .map_err(err)
}

pub async fn send_password(client_id: i32, password: crate::lock::SecretString) -> TdResult<()> {
    tdlib_rs::send_secret_request(
        client_id,
        serde_json::json!({"@type": "checkAuthenticationPassword", "password": &*password}),
    )
    .await
    .map_err(err)
}

/// Asks TDLib to push `limit` more chats of the main list as updates.
/// Error 404 means the whole list is already loaded; not a failure.
pub async fn load_chats(client_id: i32, limit: i32) -> TdResult<()> {
    load_list(client_id, enums::ChatList::Main, limit).await
}

/// Asks TDLib for (more of) a chat list; chats come as updates.
pub async fn load_list(client_id: i32, list: enums::ChatList, limit: i32) -> TdResult<()> {
    match functions::load_chats(Some(list), limit, client_id).await {
        Err(e) if e.code == 404 => Ok(()),
        r => r.map_err(err),
    }
}

/// TDLib traverses every chat in this list, including chats not loaded locally.
/// Only TDLib updates may change unread counters after this request.
pub async fn read_chat_list(client_id: i32, list: enums::ChatList) -> TdResult<()> {
    functions::read_chat_list(list, client_id)
        .await
        .map_err(err)
}

/// Opens the chat and returns its last history page, oldest first.
pub async fn open_chat(client_id: i32, chat_id: i64) -> TdResult<Vec<Message>> {
    functions::open_chat(chat_id, client_id)
        .await
        .map_err(err)?;
    let messages = history_page(client_id, chat_id, 0).await?;
    if let Some(last) = messages.last() {
        let _ = functions::view_messages(chat_id, vec![last.id], None, true, client_id).await;
    }
    Ok(messages)
}

/// Up to `HISTORY_LIMIT` messages older than `before_id` (0 = from the
/// newest), oldest first. TDLib may return fewer messages than requested
/// (local cache first), so keep asking until the limit or the chat start:
/// a page shorter than the limit means the chat start is reached.
pub async fn history_page(client_id: i32, chat_id: i64, before_id: i64) -> TdResult<Vec<Message>> {
    let mut messages: Vec<Message> = Vec::with_capacity(HISTORY_LIMIT);
    let mut from = before_id;
    while messages.len() < HISTORY_LIMIT {
        let enums::Messages::Messages(page) = functions::get_chat_history(
            chat_id,
            from,
            0,
            (HISTORY_LIMIT - messages.len()) as i32,
            false,
            client_id,
        )
        .await
        .map_err(err)?;
        let before = messages.len();
        // With offset 0 the page starts at `from` exclusive; guard anyway.
        messages.extend(
            page.messages
                .into_iter()
                .flatten()
                .filter(|m| from == 0 || m.id < from),
        );
        if messages.len() == before {
            break;
        }
        from = messages.last().map_or(0, |m| m.id);
    }
    messages.reverse();
    Ok(messages)
}

/// Marks a cached chat opened again (background tab brought to front) and
/// its newest message read, without reloading history.
pub async fn reopen_chat(client_id: i32, chat_id: i64, last_message: Option<i64>) {
    let _ = functions::open_chat(chat_id, client_id).await;
    if let Some(id) = last_message {
        let _ = functions::view_messages(chat_id, vec![id], None, true, client_id).await;
    }
}

pub async fn close_chat(client_id: i32, chat_id: i64) {
    let _ = functions::close_chat(chat_id, client_id).await;
}

pub async fn mark_read(client_id: i32, chat_id: i64, message_id: i64) {
    let _ = functions::view_messages(chat_id, vec![message_id], None, true, client_id).await;
}

/// Sends a recorded voice message (OGG/Opus) with its loudness waveform
/// (5-bit packed, as Telegram expects).
pub async fn send_voice(
    client_id: i32,
    chat_id: i64,
    path: String,
    duration: i32,
    waveform: &[u8],
) -> TdResult<()> {
    use base64::Engine;
    let content =
        InputMessageContent::InputMessageVoiceNote(tdlib_rs::types::InputMessageVoiceNote {
            voice_note: tdlib_rs::types::InputVoiceNote {
                voice_note: enums::InputFile::Local(tdlib_rs::types::InputFileLocal { path }),
                duration,
                waveform: base64::engine::general_purpose::STANDARD.encode(waveform),
            },
            caption: None,
            self_destruct_type: None,
        });
    functions::send_message(chat_id, None, None, None, content, client_id)
        .await
        .map(|_| ())
        .map_err(err)
}

/// Starts (or reprioritizes) a download; progress arrives as `updateFile`.
/// Priority 1..32, higher first.
pub async fn download_file(client_id: i32, file_id: i32, priority: i32) -> TdResult<()> {
    functions::download_file(file_id, priority, 0, 0, false, client_id)
        .await
        .map(|_| ())
        .map_err(err)
}

pub async fn cancel_download(client_id: i32, file_id: i32) {
    let _ = functions::cancel_download_file(file_id, false, client_id).await;
}

/// Sends a local file: as a compressed photo or as an untouched document.
pub async fn send_file(client_id: i32, chat_id: i64, path: String, as_photo: bool) -> TdResult<()> {
    let file = enums::InputFile::Local(tdlib_rs::types::InputFileLocal { path });
    let content = if as_photo {
        InputMessageContent::InputMessagePhoto(tdlib_rs::types::InputMessagePhoto {
            photo: tdlib_rs::types::InputPhoto {
                photo: file,
                thumbnail: None,
                video: None,
                added_sticker_file_ids: Vec::new(),
                width: 0,
                height: 0,
            },
            caption: None,
            show_caption_above_media: false,
            self_destruct_type: None,
            has_spoiler: false,
        })
    } else {
        InputMessageContent::InputMessageDocument(tdlib_rs::types::InputMessageDocument {
            document: tdlib_rs::types::InputDocument {
                document: file,
                thumbnail: None,
                disable_content_type_detection: false,
            },
            caption: None,
        })
    };
    functions::send_message(chat_id, None, None, None, content, client_id)
        .await
        .map(|_| ())
        .map_err(err)
}

/// Sends text written with Telegram-style markdown (**bold**, __italic__,
/// ~~strike~~, ||spoiler||, `code`, ```pre```, [text](url)), optionally as a
/// reply. Markup errors are ignored by TDLib and sent as plain text.
pub async fn send_text(
    client_id: i32,
    chat_id: i64,
    text: String,
    reply_to: Option<i64>,
) -> TdResult<()> {
    let plain = FormattedText {
        text,
        entities: Vec::new(),
    };
    let formatted = match functions::parse_markdown(plain.clone(), client_id).await {
        Ok(enums::FormattedText::FormattedText(f)) => f,
        Err(_) => plain,
    };
    let content = InputMessageContent::InputMessageText(InputMessageText {
        text: formatted,
        link_preview_options: None,
        clear_draft: true,
    });
    functions::send_message(
        chat_id,
        None,
        reply_input(reply_to),
        None,
        content,
        client_id,
    )
    .await
    .map(|_| ())
    .map_err(err)
}

/// Text of a message with its formatting written back as markdown, for
/// editing in the input field.
pub async fn message_markdown(client_id: i32, chat_id: i64, message_id: i64) -> TdResult<String> {
    let enums::Message::Message(m) = functions::get_message(chat_id, message_id, client_id)
        .await
        .map_err(err)?;
    let MessageContent::MessageText(t) = m.content else {
        return Err("редактировать можно только текст".into());
    };
    match functions::get_markdown_text(t.text.clone(), client_id).await {
        Ok(enums::FormattedText::FormattedText(f)) => Ok(f.text),
        Err(_) => Ok(t.text.text),
    }
}

/// Replaces the text of a sent message; markdown as in `send_text`.
pub async fn edit_text(
    client_id: i32,
    chat_id: i64,
    message_id: i64,
    text: String,
) -> TdResult<()> {
    let plain = FormattedText {
        text,
        entities: Vec::new(),
    };
    let formatted = match functions::parse_markdown(plain.clone(), client_id).await {
        Ok(enums::FormattedText::FormattedText(f)) => f,
        Err(_) => plain,
    };
    let content = InputMessageContent::InputMessageText(InputMessageText {
        text: formatted,
        link_preview_options: None,
        clear_draft: false,
    });
    functions::edit_message_text(chat_id, message_id, content, client_id)
        .await
        .map(|_| ())
        .map_err(err)
}

fn reply_input(reply_to: Option<i64>) -> Option<enums::InputMessageReplyTo> {
    reply_to.map(|message_id| {
        enums::InputMessageReplyTo::Message(tdlib_rs::types::InputMessageReplyToMessage {
            message_id,
            quote: None,
            checklist_task_id: 0,
            poll_option_id: String::new(),
        })
    })
}

/// Lets TDLib build notification groups (it applies mute settings,
/// exceptions and mentions itself) and send them as updates.
pub async fn enable_notifications(client_id: i32) {
    for (name, value) in [
        ("notification_group_count_max", 10),
        ("notification_group_size_max", 3),
    ] {
        let _ = functions::set_option(
            name.into(),
            Some(enums::OptionValue::Integer(
                tdlib_rs::types::OptionValueInteger { value },
            )),
            client_id,
        )
        .await;
    }
}

/// Messages by id (for reply previews); missing ones are skipped.
pub async fn get_messages(client_id: i32, chat_id: i64, ids: Vec<i64>) -> TdResult<Vec<Message>> {
    let enums::Messages::Messages(found) = functions::get_messages(chat_id, ids, client_id)
        .await
        .map_err(err)?;
    Ok(found.messages.into_iter().flatten().collect())
}

/// Chats matching a title among all chats TDLib knows (also archived ones).
pub async fn search_chats(client_id: i32, query: String) -> TdResult<Vec<i64>> {
    let enums::Chats::Chats(chats) = functions::search_chats(query, None, 50, client_id)
        .await
        .map_err(err)?;
    Ok(chats.chat_ids)
}

/// Messages containing `query` in one chat, newest first. `None` starts
/// at the newest message; TDLib's returned cursor, not page length, ends it.
pub async fn search_chat(
    client_id: i32,
    chat_id: i64,
    query: String,
    from: Option<i64>,
) -> TdResult<(Vec<Message>, Option<i64>)> {
    let enums::FoundChatMessages::FoundChatMessages(found) = functions::search_chat_messages(
        chat_id,
        None,
        query,
        None,
        from.unwrap_or(0),
        0,
        50,
        None,
        client_id,
    )
    .await
    .map_err(err)?;
    Ok((
        found.messages,
        (found.next_from_message_id != 0).then_some(found.next_from_message_id),
    ))
}

/// Which reaction: an emoji, a custom emoji (by id) or a paid star.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReactionKey {
    Emoji(String),
    Custom(i64),
    Paid,
}

impl ReactionKey {
    pub fn of(kind: &enums::ReactionType) -> Self {
        match kind {
            enums::ReactionType::Emoji(e) => Self::Emoji(e.emoji.clone()),
            enums::ReactionType::CustomEmoji(c) => Self::Custom(c.custom_emoji_id),
            enums::ReactionType::Paid => Self::Paid,
        }
    }

    fn to_type(&self) -> enums::ReactionType {
        match self {
            Self::Emoji(emoji) => enums::ReactionType::Emoji(tdlib_rs::types::ReactionTypeEmoji {
                emoji: emoji.clone(),
            }),
            Self::Custom(id) => {
                enums::ReactionType::CustomEmoji(tdlib_rs::types::ReactionTypeCustomEmoji {
                    custom_emoji_id: *id,
                })
            }
            Self::Paid => enums::ReactionType::Paid,
        }
    }

    /// What the bubble shows: the emoji itself; custom emoji (pictures
    /// from sticker sets) and paid stars as symbols.
    pub fn label(&self) -> &str {
        match self {
            Self::Emoji(emoji) => emoji,
            Self::Custom(_) => "✦",
            Self::Paid => "⭐",
        }
    }
}

/// Emoji reactions the user may put on a message, most used first.
pub async fn available_reactions(
    client_id: i32,
    chat_id: i64,
    message_id: i64,
) -> TdResult<Vec<String>> {
    let enums::AvailableReactions::AvailableReactions(r) =
        functions::get_message_available_reactions(chat_id, message_id, 8, client_id)
            .await
            .map_err(err)?;
    let mut emoji: Vec<String> = Vec::new();
    for reaction in r.top_reactions.iter().chain(&r.popular_reactions) {
        if let (false, enums::ReactionType::Emoji(e)) = (reaction.needs_premium, &reaction.r#type)
            && !emoji.contains(&e.emoji)
        {
            emoji.push(e.emoji.clone());
        }
    }
    emoji.truncate(8);
    Ok(emoji)
}

/// `addMessageReaction`/`removeMessageReaction` reject a paid star: it is
/// sent with `addPendingPaidMessageReaction` instead and cannot be taken
/// back, so `ReactionKey::Paid` is refused here with a message the user
/// understands instead of a raw TDLib error.
pub async fn set_reaction(
    client_id: i32,
    chat_id: i64,
    message_id: i64,
    key: ReactionKey,
    on: bool,
) -> TdResult<()> {
    if matches!(key, ReactionKey::Paid) {
        return Err("платную реакцию нельзя поставить или снять здесь".into());
    }
    if on {
        functions::add_message_reaction(chat_id, message_id, key.to_type(), false, true, client_id)
            .await
    } else {
        functions::remove_message_reaction(chat_id, message_id, key.to_type(), client_id).await
    }
    .map_err(err)
}

/// Forwards messages (with "Переслано от"), in their order.
pub async fn forward(client_id: i32, to_chat: i64, from_chat: i64, ids: Vec<i64>) -> TdResult<()> {
    functions::forward_messages(to_chat, None, from_chat, ids, None, false, false, client_id)
        .await
        .map(|_| ())
        .map_err(err)
}

pub async fn vote(
    client_id: i32,
    chat_id: i64,
    message_id: i64,
    options: Vec<i32>,
) -> TdResult<()> {
    functions::set_poll_answer(chat_id, message_id, options, client_id)
        .await
        .map_err(err)
}

pub async fn pin_chat(
    client_id: i32,
    chat_id: i64,
    list: enums::ChatList,
    pinned: bool,
) -> TdResult<()> {
    functions::toggle_chat_is_pinned(list, chat_id, pinned, client_id)
        .await
        .map_err(err)
}

/// Moves a chat to the main list or to the archive.
pub async fn move_chat(client_id: i32, chat_id: i64, list: enums::ChatList) -> TdResult<()> {
    functions::add_chat_to_list(chat_id, list, client_id)
        .await
        .map_err(err)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveFlag {
    KeepUnmuted,
    KeepFolderChats,
    ArchiveUnknown,
}

/// Edit one flag in the freshly fetched server value, never a UI snapshot.
pub fn archive_flag(
    mut settings: tdlib_rs::types::ArchiveChatListSettings,
    flag: ArchiveFlag,
    value: bool,
) -> tdlib_rs::types::ArchiveChatListSettings {
    match flag {
        ArchiveFlag::KeepUnmuted => settings.keep_unmuted_chats_archived = value,
        ArchiveFlag::KeepFolderChats => settings.keep_chats_from_folders_archived = value,
        ArchiveFlag::ArchiveUnknown => {
            settings.archive_and_mute_new_chats_from_unknown_users = value;
        }
    }
    settings
}

pub async fn archive_settings(
    client_id: i32,
) -> TdResult<tdlib_rs::types::ArchiveChatListSettings> {
    let enums::ArchiveChatListSettings::ArchiveChatListSettings(settings) =
        functions::get_archive_chat_list_settings(client_id)
            .await
            .map_err(err)?;
    Ok(settings)
}

pub async fn archive_unknown_capability(client_id: i32) -> TdResult<bool> {
    match functions::get_option(
        "can_archive_and_mute_new_chats_from_unknown_users".into(),
        client_id,
    )
    .await
    .map_err(err)?
    {
        enums::OptionValue::Boolean(option) => Ok(option.value),
        _ => Err("TDLib не сообщила доступность автоархивации".into()),
    }
}

pub async fn save_archive_settings(
    client_id: i32,
    settings: tdlib_rs::types::ArchiveChatListSettings,
) -> TdResult<()> {
    functions::set_archive_chat_list_settings(settings, client_id)
        .await
        .map_err(err)
}

/// The only explicit rule in a newly created folder is the chat whose menu
/// launched the form; TDLib owns its ID and the resulting folder list.
pub fn new_chat_folder(name: &str, chat_id: i64) -> TdResult<tdlib_rs::types::ChatFolder> {
    if name.contains('\n') || name.contains('\r') {
        return Err("Название папки: от 1 до 12 символов, без переноса строки".into());
    }
    let name = name.trim();
    if !(1..=12).contains(&name.chars().count()) {
        return Err("Название папки: от 1 до 12 символов, без переноса строки".into());
    }
    Ok(tdlib_rs::types::ChatFolder {
        name: tdlib_rs::types::ChatFolderName {
            text: FormattedText {
                text: name.into(),
                entities: Vec::new(),
            },
            animate_custom_emoji: false,
        },
        icon: None,
        color_id: -1,
        is_shareable: false,
        pinned_chat_ids: Vec::new(),
        included_chat_ids: vec![chat_id],
        excluded_chat_ids: Vec::new(),
        exclude_muted: false,
        exclude_read: false,
        exclude_archived: false,
        include_contacts: false,
        include_non_contacts: false,
        include_bots: false,
        include_groups: false,
        include_channels: false,
    })
}

pub async fn create_chat_folder(
    client_id: i32,
    folder: tdlib_rs::types::ChatFolder,
) -> TdResult<i32> {
    functions::create_chat_folder(folder, client_id)
        .await
        .map(|enums::ChatFolderInfo::ChatFolderInfo(info)| info.id)
        .map_err(err)
}

/// Reorders the existing folders without moving the main chat list.
/// The new order is displayed only after TDLib sends `updateChatFolders`.
pub async fn reorder_chat_folders(ids: Vec<i32>, main_tab: i32, client_id: i32) -> TdResult<()> {
    functions::reorder_chat_folders(ids, main_tab, client_id)
        .await
        .map_err(err)
}

/// Always include or explicitly exclude a chat in an existing folder.
/// Fetch at action time so edits never replace unrelated server-side rules
/// with stale data from the sidebar's folder names.
pub async fn set_folder_membership(
    client_id: i32,
    folder_id: i32,
    chat_id: i64,
    include: bool,
) -> TdResult<bool> {
    let enums::ChatFolder::ChatFolder(mut folder) =
        functions::get_chat_folder(folder_id, client_id)
            .await
            .map_err(err)?;
    if !change_folder_membership(&mut folder, chat_id, include) {
        return Ok(false);
    }
    functions::edit_chat_folder(folder_id, folder, client_id)
        .await
        .map_err(err)?;
    Ok(true)
}

/// Change only the three explicit membership lists; all category, exclusion,
/// sharing, name, icon and color settings remain exactly as received.
fn change_folder_membership(
    folder: &mut tdlib_rs::types::ChatFolder,
    chat_id: i64,
    include: bool,
) -> bool {
    let before = (
        folder.pinned_chat_ids.len(),
        folder.included_chat_ids.len(),
        folder.excluded_chat_ids.len(),
    );
    if include {
        folder.excluded_chat_ids.retain(|&id| id != chat_id);
        if !folder.pinned_chat_ids.contains(&chat_id)
            && !folder.included_chat_ids.contains(&chat_id)
        {
            folder.included_chat_ids.push(chat_id);
        }
    } else {
        folder.pinned_chat_ids.retain(|&id| id != chat_id);
        folder.included_chat_ids.retain(|&id| id != chat_id);
        if !folder.excluded_chat_ids.contains(&chat_id) {
            folder.excluded_chat_ids.push(chat_id);
        }
    }
    before
        != (
            folder.pinned_chat_ids.len(),
            folder.included_chat_ids.len(),
            folder.excluded_chat_ids.len(),
        )
}

pub async fn set_notifications(
    client_id: i32,
    chat_id: i64,
    settings: tdlib_rs::types::ChatNotificationSettings,
) -> TdResult<()> {
    functions::set_chat_notification_settings(chat_id, settings, client_id)
        .await
        .map_err(err)
}

/// Pinned messages of a chat, newest first.
pub async fn pinned_messages(client_id: i32, chat_id: i64) -> TdResult<Vec<Message>> {
    let enums::FoundChatMessages::FoundChatMessages(found) = functions::search_chat_messages(
        chat_id,
        None,
        String::new(),
        None,
        0,
        0,
        100,
        Some(enums::SearchMessagesFilter::Pinned),
        client_id,
    )
    .await
    .map_err(err)?;
    Ok(found.messages)
}

/// Messages containing `query` in the selected list (or globally for `None`),
/// newest first. The returned offset is valid only for the same list and query.
pub async fn search_messages(
    client_id: i32,
    chat_list: Option<enums::ChatList>,
    query: String,
    offset: Option<String>,
) -> TdResult<(Vec<Message>, Option<String>)> {
    let enums::FoundMessages::FoundMessages(found) = functions::search_messages(
        chat_list,
        query,
        offset.unwrap_or_default(),
        50,
        None,
        None,
        0,
        0,
        client_id,
    )
    .await
    .map_err(err)?;
    let next = (!found.next_offset.is_empty()).then_some(found.next_offset);
    Ok((found.messages, next))
}

/// History around a message: up to `HISTORY_LIMIT` messages centred on it,
/// oldest first (for jumping to a search result or a replied message). The
/// chat is assumed already open (`jump`/`open_found` only reach this after
/// opening it), so this does not call `openChat` itself: TDLib counts opens
/// per chat, and a second call here would never be balanced by a `closeChat`.
pub async fn history_around(
    client_id: i32,
    chat_id: i64,
    message_id: i64,
) -> TdResult<Vec<Message>> {
    let half = (HISTORY_LIMIT / 2) as i32;
    let enums::Messages::Messages(page) = functions::get_chat_history(
        chat_id,
        message_id,
        -half,
        HISTORY_LIMIT as i32,
        false,
        client_id,
    )
    .await
    .map_err(err)?;
    let mut messages: Vec<Message> = page.messages.into_iter().flatten().collect();
    messages.sort_by_key(|m| m.id);
    Ok(messages)
}

/// Up to `HISTORY_LIMIT` messages newer than `after_id`, oldest first.
/// TDLib may return fewer messages than requested per call (local cache
/// first), so keep asking, like `history_page` does, until the limit is
/// filled or a request adds nothing (the newest message is reached).
pub async fn history_newer(client_id: i32, chat_id: i64, after_id: i64) -> TdResult<Vec<Message>> {
    let mut messages: Vec<Message> = Vec::with_capacity(HISTORY_LIMIT);
    let mut from = after_id;
    while messages.len() < HISTORY_LIMIT {
        let remaining = (HISTORY_LIMIT - messages.len()) as i32;
        let enums::Messages::Messages(page) =
            functions::get_chat_history(chat_id, from, -remaining, remaining + 1, false, client_id)
                .await
                .map_err(err)?;
        let before = messages.len();
        messages.extend(page.messages.into_iter().flatten().filter(|m| m.id > from));
        if messages.len() == before {
            break;
        }
        from = messages.last().map_or(from, |m| m.id);
    }
    messages.sort_by_key(|m| m.id);
    Ok(messages)
}

/// Plain-text rendering of a message body; media become a label plus caption.
pub fn message_text(content: &MessageContent) -> String {
    let with_caption = |label: &str, caption: &FormattedText| {
        if caption.text.is_empty() {
            label.to_owned()
        } else {
            format!("{label} {}", caption.text)
        }
    };
    match content {
        MessageContent::MessageText(m) => m.text.text.clone(),
        MessageContent::MessagePhoto(m) => with_caption("[Фото]", &m.caption),
        MessageContent::MessageVideo(m) => with_caption("[Видео]", &m.caption),
        MessageContent::MessageAnimation(m) => with_caption("[GIF]", &m.caption),
        MessageContent::MessageDocument(m) => with_caption("[Файл]", &m.caption),
        MessageContent::MessageAudio(m) => with_caption("[Аудио]", &m.caption),
        MessageContent::MessageVoiceNote(m) => with_caption("[Голосовое]", &m.caption),
        MessageContent::MessageVideoNote(_) => "[Кружок]".into(),
        MessageContent::MessageSticker(m) => format!("[Стикер {}]", m.sticker.emoji),
        MessageContent::MessagePoll(m) => format!("[Опрос] {}", m.poll.question.text),
        MessageContent::MessageContact(m) => {
            format!("[Контакт] {} {}", m.contact.first_name, m.contact.last_name)
                .trim_end()
                .to_owned()
        }
        MessageContent::MessageLocation(_) => "[Геопозиция]".into(),
        MessageContent::MessageVenue(m) => format!("[Место] {}", m.venue.title),
        _ => "[Сообщение]".into(),
    }
}

/// What the profile panel shows about a chat.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Profile {
    /// Public names without "@", the main one first.
    pub usernames: Vec<String>,
    /// Status and phone of a user, «1 088 881 подписчик» of a channel.
    pub subtitle: String,
    /// Bio or description with its links and @mentions.
    pub about: Vec<crate::app::rich::Piece>,
    /// Members that could be listed (first page).
    pub members: Vec<ProfileMember>,
    /// Discussion group of a channel (or channel of a group); 0 if none.
    pub linked_chat_id: i64,
    pub is_channel: bool,
    /// A group or channel the user is in (can leave it).
    pub is_member: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProfileMember {
    pub id: i64,
    pub name: String,
    pub is_bot: bool,
}

fn usernames_of(names: Option<&tdlib_rs::types::Usernames>) -> Vec<String> {
    names
        .map(|u| u.active_usernames.clone())
        .unwrap_or_default()
}

/// «1 088 881 подписчик»: digits grouped, the noun agreeing with the number.
pub fn count_label(n: i32, forms: [&str; 3]) -> String {
    let digits = n.unsigned_abs().to_string();
    let mut grouped = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            grouped.push('\u{00A0}');
        }
        grouped.push(c);
    }
    let (m10, m100) = (n.unsigned_abs() % 10, n.unsigned_abs() % 100);
    let form = if m10 == 1 && m100 != 11 {
        forms[0]
    } else if (2..=4).contains(&m10) && !(12..=14).contains(&m100) {
        forms[1]
    } else {
        forms[2]
    };
    format!("{grouped} {form}")
}

/// A plain description with its links and mentions found by TDLib.
async fn linked_text(client_id: i32, text: String) -> Vec<crate::app::rich::Piece> {
    let entities = match functions::get_text_entities(text.clone(), client_id).await {
        Ok(enums::TextEntities::TextEntities(t)) => t.entities,
        Err(_) => Vec::new(),
    };
    crate::app::rich::pieces(&FormattedText { text, entities })
}

/// Leaves a group or channel.
pub async fn leave_chat(client_id: i32, chat_id: i64) -> TdResult<()> {
    functions::leave_chat(chat_id, client_id).await.map_err(err)
}

/// "в сети", "был(а) 25.09 в 14:05", "был(а) недавно".
pub fn status_label(status: &enums::UserStatus) -> String {
    use chrono::TimeZone;
    match status {
        enums::UserStatus::Online(_) => "в сети".into(),
        enums::UserStatus::Offline(o) => chrono::Local
            .timestamp_opt(i64::from(o.was_online), 0)
            .single()
            .map_or_else(
                || "не в сети".into(),
                |t| format!("был(а) {}", t.format("%d.%m в %H:%M")),
            ),
        enums::UserStatus::Recently(_) => "был(а) недавно".into(),
        enums::UserStatus::LastWeek(_) => "был(а) на этой неделе".into(),
        enums::UserStatus::LastMonth(_) => "был(а) в этом месяце".into(),
        enums::UserStatus::Empty => String::new(),
    }
}

async fn member_names(
    client_id: i32,
    members: &[tdlib_rs::types::ChatMember],
) -> Vec<ProfileMember> {
    let mut names = Vec::new();
    for m in members.iter().take(50) {
        if let enums::MessageSender::User(u) = &m.member_id
            && let Ok(enums::User::User(user)) = functions::get_user(u.user_id, client_id).await
        {
            let name = format!("{} {}", user.first_name, user.last_name);
            names.push(ProfileMember {
                id: user.id,
                name: name.trim().to_owned(),
                is_bot: matches!(user.r#type, enums::UserType::Bot(_)),
            });
        }
    }
    names
}

pub async fn profile(client_id: i32, kind: enums::ChatType) -> TdResult<Profile> {
    match kind {
        enums::ChatType::Private(p) => user_profile(client_id, p.user_id).await,
        enums::ChatType::Secret(s) => user_profile(client_id, s.user_id).await,
        enums::ChatType::BasicGroup(g) => {
            let enums::BasicGroup::BasicGroup(group) =
                functions::get_basic_group(g.basic_group_id, client_id)
                    .await
                    .map_err(err)?;
            let enums::BasicGroupFullInfo::BasicGroupFullInfo(info) =
                functions::get_basic_group_full_info(g.basic_group_id, client_id)
                    .await
                    .map_err(err)?;
            Ok(Profile {
                subtitle: count_label(
                    info.members.len() as i32,
                    ["участник", "участника", "участников"],
                ),
                about: linked_text(client_id, info.description.clone()).await,
                members: member_names(client_id, &info.members).await,
                is_member: !matches!(
                    group.status,
                    enums::ChatMemberStatus::Left | enums::ChatMemberStatus::Banned(_)
                ),
                ..Profile::default()
            })
        }
        enums::ChatType::Supergroup(s) => {
            let enums::Supergroup::Supergroup(group) =
                functions::get_supergroup(s.supergroup_id, client_id)
                    .await
                    .map_err(err)?;
            let enums::SupergroupFullInfo::SupergroupFullInfo(info) =
                functions::get_supergroup_full_info(s.supergroup_id, client_id)
                    .await
                    .map_err(err)?;
            let count = info.member_count.max(group.member_count);
            let forms = if s.is_channel {
                ["подписчик", "подписчика", "подписчиков"]
            } else {
                ["участник", "участника", "участников"]
            };
            // Channel subscribers are visible to admins only; a refusal
            // just leaves the list out.
            let members = if s.is_channel {
                Vec::new()
            } else {
                match functions::get_supergroup_members(s.supergroup_id, None, 0, 50, client_id)
                    .await
                {
                    Ok(enums::ChatMembers::ChatMembers(m)) => {
                        member_names(client_id, &m.members).await
                    }
                    Err(_) => Vec::new(),
                }
            };
            Ok(Profile {
                usernames: usernames_of(group.usernames.as_ref()),
                subtitle: count_label(count, forms),
                about: linked_text(client_id, info.description).await,
                members,
                linked_chat_id: info.linked_chat_id,
                is_channel: s.is_channel,
                is_member: !matches!(
                    group.status,
                    enums::ChatMemberStatus::Left | enums::ChatMemberStatus::Banned(_)
                ),
            })
        }
    }
}

async fn user_profile(client_id: i32, user_id: i64) -> TdResult<Profile> {
    let enums::User::User(user) = functions::get_user(user_id, client_id).await.map_err(err)?;
    let enums::UserFullInfo::UserFullInfo(full) = functions::get_user_full_info(user_id, client_id)
        .await
        .map_err(err)?;
    let phone = if user.phone_number.is_empty() {
        String::new()
    } else {
        format!("+{}", user.phone_number)
    };
    let status = status_label(&user.status);
    let about = match full.bio {
        Some(bio) => crate::app::rich::pieces(&bio),
        None => Vec::new(),
    };
    Ok(Profile {
        usernames: usernames_of(user.usernames.as_ref()),
        subtitle: [phone, status]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" · "),
        about,
        ..Profile::default()
    })
}

/// A sticker as the picker needs it.
#[derive(Debug, Clone, PartialEq)]
pub struct StickerRef {
    /// The sticker file itself (what is sent).
    pub file: tdlib_rs::types::File,
    /// A still picture to show in the picker: a thumbnail, or the sticker
    /// itself when it is a static WebP. `None`: the emoji stands in.
    pub picture: Option<tdlib_rs::types::File>,
    pub emoji: String,
    pub width: i32,
    pub height: i32,
}

impl StickerRef {
    fn of(s: &tdlib_rs::types::Sticker) -> Self {
        use enums::{StickerFormat, ThumbnailFormat};
        let still_thumb = s.thumbnail.as_ref().filter(|t| {
            matches!(
                t.format,
                ThumbnailFormat::Webp | ThumbnailFormat::Jpeg | ThumbnailFormat::Png
            )
        });
        let picture = match (still_thumb, &s.format) {
            (Some(t), _) => Some(t.file.clone()),
            (None, StickerFormat::Webp) => Some(s.sticker.clone()),
            _ => None,
        };
        Self {
            file: s.sticker.clone(),
            picture,
            emoji: s.emoji.clone(),
            width: s.width,
            height: s.height,
        }
    }
}

pub async fn recent_stickers(client_id: i32) -> TdResult<Vec<StickerRef>> {
    let enums::Stickers::Stickers(s) = functions::get_recent_stickers(false, client_id)
        .await
        .map_err(err)?;
    Ok(s.stickers.iter().take(40).map(StickerRef::of).collect())
}

/// Installed sticker sets: (id, title).
pub async fn sticker_sets(client_id: i32) -> TdResult<Vec<(i64, String)>> {
    let enums::StickerSets::StickerSets(s) =
        functions::get_installed_sticker_sets(enums::StickerType::Regular, client_id)
            .await
            .map_err(err)?;
    Ok(s.sets.into_iter().map(|s| (s.id, s.title)).collect())
}

pub async fn sticker_set(client_id: i32, set_id: i64) -> TdResult<Vec<StickerRef>> {
    let enums::StickerSet::StickerSet(s) = functions::get_sticker_set(set_id, client_id)
        .await
        .map_err(err)?;
    Ok(s.stickers.iter().map(StickerRef::of).collect())
}

pub async fn send_sticker(client_id: i32, chat_id: i64, sticker: StickerRef) -> TdResult<()> {
    let content = InputMessageContent::InputMessageSticker(tdlib_rs::types::InputMessageSticker {
        sticker: tdlib_rs::types::InputSticker {
            sticker: enums::InputFile::Id(tdlib_rs::types::InputFileId {
                id: sticker.file.id,
            }),
            thumbnail: None,
            width: sticker.width,
            height: sticker.height,
        },
        emoji: sticker.emoji,
    });
    functions::send_message(chat_id, None, None, None, content, client_id)
        .await
        .map(|_| ())
        .map_err(err)
}

/// The text of a draft, if it is a text draft.
pub fn draft_text(draft: Option<&tdlib_rs::types::DraftMessage>) -> Option<FormattedText> {
    match &draft?.content {
        enums::DraftMessageContent::Text(t) if !t.text.text.is_empty() => Some(t.text.clone()),
        _ => None,
    }
}

/// Saves the input field of a chat as its draft (synced to other devices);
/// empty text removes the draft. Markdown is applied like when sending.
pub async fn save_draft(client_id: i32, chat_id: i64, text: String) -> TdResult<()> {
    let draft = if text.trim().is_empty() {
        None
    } else {
        let plain = FormattedText {
            text,
            entities: Vec::new(),
        };
        let formatted = match functions::parse_markdown(plain.clone(), client_id).await {
            Ok(enums::FormattedText::FormattedText(f)) => f,
            Err(_) => plain,
        };
        Some(tdlib_rs::types::DraftMessage {
            reply_to: None,
            date: 0,
            content: enums::DraftMessageContent::Text(tdlib_rs::types::DraftMessageContentText {
                text: formatted,
                link_preview_options: None,
            }),
            effect_id: 0,
            suggested_post_info: None,
        })
    };
    functions::set_chat_draft_message(chat_id, None, draft, client_id)
        .await
        .map_err(err)
}

/// A draft's text with formatting as markdown, for the input field.
pub async fn draft_markdown(client_id: i32, text: FormattedText) -> TdResult<String> {
    match functions::get_markdown_text(text.clone(), client_id).await {
        Ok(enums::FormattedText::FormattedText(f)) => Ok(f.text),
        Err(_) => Ok(text.text),
    }
}

/// A member's role and tag in a group, shown next to their name.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Role {
    pub owner: bool,
    pub admin: bool,
    /// Custom title of an admin, or the member tag; may be empty.
    pub tag: String,
}

/// Roles and tags of a group's members: administrators of a supergroup
/// (whose tags messages do not carry), or all members of a basic group.
pub async fn chat_roles(
    client_id: i32,
    chat_id: i64,
    kind: enums::ChatType,
) -> TdResult<std::collections::HashMap<i64, Role>> {
    let mut roles = std::collections::HashMap::new();
    match kind {
        enums::ChatType::Supergroup(s) if !s.is_channel => {
            let enums::ChatAdministrators::ChatAdministrators(admins) =
                functions::get_chat_administrators(chat_id, client_id)
                    .await
                    .map_err(err)?;
            for a in admins.administrators {
                roles.insert(
                    a.user_id,
                    Role {
                        owner: a.is_owner,
                        admin: true,
                        tag: a.custom_title,
                    },
                );
            }
        }
        enums::ChatType::BasicGroup(g) => {
            let enums::BasicGroupFullInfo::BasicGroupFullInfo(info) =
                functions::get_basic_group_full_info(g.basic_group_id, client_id)
                    .await
                    .map_err(err)?;
            for m in info.members {
                let enums::MessageSender::User(user) = m.member_id else {
                    continue;
                };
                let (owner, admin) = match m.status {
                    enums::ChatMemberStatus::Creator(_) => (true, true),
                    enums::ChatMemberStatus::Administrator(_) => (false, true),
                    _ => (false, false),
                };
                if admin || !m.tag.is_empty() {
                    roles.insert(
                        user.user_id,
                        Role {
                            owner,
                            admin,
                            tag: m.tag,
                        },
                    );
                }
            }
        }
        _ => {}
    }
    Ok(roles)
}

/// A TDLib file read while it downloads: reads wait for the bytes they
/// need, asking TDLib to download from that place (a seek to the end of an
/// MP4 for its index, then back, both work). For the video player thread;
/// TDLib answers through the app's update loop.
pub struct FileStream<B = NativeFileBackend> {
    client_id: i32,
    file_id: i32,
    size: u64,
    pos: u64,
    file: Option<std::fs::File>,
    /// Bytes known to be downloaded from `available.0` on.
    available: (u64, u64),
    /// Where TDLib was last successfully asked to download from.
    requested: Option<u64>,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    backend: B,
    request_timeout: std::time::Duration,
    progress_timeout: std::time::Duration,
}

pub struct NativeFileBackend;

pub(crate) trait FileStreamBackend {
    fn prefix(
        &self,
        client_id: i32,
        file_id: i32,
        offset: u64,
    ) -> impl Future<Output = std::io::Result<u64>>;
    fn get_file(
        &self,
        client_id: i32,
        file_id: i32,
    ) -> impl Future<Output = std::io::Result<std::fs::File>>;
    fn download(
        &self,
        client_id: i32,
        file_id: i32,
        offset: u64,
    ) -> impl Future<Output = std::io::Result<()>>;
}

impl FileStreamBackend for NativeFileBackend {
    async fn prefix(&self, client_id: i32, file_id: i32, offset: u64) -> std::io::Result<u64> {
        let offset = i64::try_from(offset).map_err(|_| std::io::ErrorKind::InvalidInput)?;
        let enums::FileDownloadedPrefixSize::FileDownloadedPrefixSize(prefix) =
            functions::get_file_downloaded_prefix_size(file_id, offset, client_id)
                .await
                .map_err(|e| std::io::Error::other(e.message))?;
        Ok(prefix.size.max(0) as u64)
    }

    async fn get_file(&self, client_id: i32, file_id: i32) -> std::io::Result<std::fs::File> {
        let enums::File::File(file) = functions::get_file(file_id, client_id)
            .await
            .map_err(|e| std::io::Error::other(e.message))?;
        std::fs::File::open(&file.local.path)
    }

    async fn download(&self, client_id: i32, file_id: i32, offset: u64) -> std::io::Result<()> {
        let offset = i64::try_from(offset).map_err(|_| std::io::ErrorKind::InvalidInput)?;
        functions::download_file(file_id, 32, offset, 0, false, client_id)
            .await
            .map_err(|e| std::io::Error::other(e.message))?;
        Ok(())
    }
}

impl FileStream {
    pub fn new(
        client_id: i32,
        file_id: i32,
        size: u64,
        stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) -> Self {
        Self::with_backend(
            client_id,
            file_id,
            size,
            stop,
            NativeFileBackend,
            std::time::Duration::from_secs(30),
            std::time::Duration::from_secs(30),
        )
    }
}

impl<B: FileStreamBackend> FileStream<B> {
    pub(crate) fn with_backend(
        client_id: i32,
        file_id: i32,
        size: u64,
        stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
        backend: B,
        request_timeout: std::time::Duration,
        progress_timeout: std::time::Duration,
    ) -> Self {
        Self {
            client_id,
            file_id,
            size,
            pos: 0,
            file: None,
            available: (0, 0),
            requested: None,
            stop,
            backend,
            request_timeout,
            progress_timeout,
        }
    }

    fn wait_request<T>(
        &self,
        last_progress: std::time::Instant,
        future: impl Future<Output = std::io::Result<T>>,
    ) -> std::io::Result<T> {
        use std::task::{Context, Poll, Waker};
        let deadline = std::time::Instant::now() + self.request_timeout;
        let progress_deadline = last_progress + self.progress_timeout;
        let mut future = std::pin::pin!(future);
        let mut context = Context::from_waker(Waker::noop());
        loop {
            if self.stop.load(std::sync::atomic::Ordering::Relaxed) {
                return Err(std::io::ErrorKind::Interrupted.into());
            }
            let now = std::time::Instant::now();
            if now >= deadline || now >= progress_deadline {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "TDLib file request timed out",
                ));
            }
            if let Poll::Ready(result) = future.as_mut().poll(&mut context) {
                return result;
            }
            std::thread::sleep(
                std::time::Duration::from_millis(40)
                    .min(deadline.saturating_duration_since(now))
                    .min(progress_deadline.saturating_duration_since(now)),
            );
        }
    }
}

impl<B: FileStreamBackend> std::io::Read for FileStream<B> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        use std::io::Seek;
        if buf.is_empty() {
            return Ok(0);
        }
        let last_progress = std::time::Instant::now();
        loop {
            if self.stop.load(std::sync::atomic::Ordering::Relaxed) {
                return Err(std::io::ErrorKind::Interrupted.into());
            }
            if self.size > 0 && self.pos >= self.size {
                return Ok(0);
            }
            let (start, len) = self.available;
            let mut ready = if self.pos < start {
                0
            } else {
                start.saturating_add(len).saturating_sub(self.pos)
            };
            if ready == 0 {
                ready = self.wait_request(
                    last_progress,
                    self.backend.prefix(self.client_id, self.file_id, self.pos),
                )?;
                self.available = (self.pos, ready);
            }
            if ready > 0 {
                let pos = self.pos;
                if self.file.is_none() {
                    self.file = Some(self.wait_request(
                        last_progress,
                        self.backend.get_file(self.client_id, self.file_id),
                    )?);
                }
                let file = self.file.as_mut().expect("file just opened");
                file.seek(std::io::SeekFrom::Start(pos))?;
                let want = buf.len().min(usize::try_from(ready).unwrap_or(usize::MAX));
                let want = if self.size > 0 {
                    want.min(usize::try_from(self.size - self.pos).unwrap_or(usize::MAX))
                } else {
                    want
                };
                let n = file.read(&mut buf[..want])?;
                if n > 0 {
                    self.pos += n as u64;
                    return Ok(n);
                }
            }
            if self.requested != Some(self.pos) {
                self.wait_request(
                    last_progress,
                    self.backend
                        .download(self.client_id, self.file_id, self.pos),
                )?;
                self.requested = Some(self.pos);
            }
            let remaining = self
                .progress_timeout
                .saturating_sub(last_progress.elapsed());
            if remaining.is_zero() {
                return Err(std::io::ErrorKind::TimedOut.into());
            }
            std::thread::sleep(std::time::Duration::from_millis(40).min(remaining));
            self.available = (self.pos, 0);
        }
    }
}

impl<B> std::io::Seek for FileStream<B> {
    fn seek(&mut self, to: std::io::SeekFrom) -> std::io::Result<u64> {
        let target = match to {
            std::io::SeekFrom::Start(p) => p as i128,
            std::io::SeekFrom::Current(d) => self.pos as i128 + d as i128,
            std::io::SeekFrom::End(d) => self.size as i128 + d as i128,
        };
        self.pos = u64::try_from(target).map_err(|_| std::io::ErrorKind::InvalidInput)?;
        Ok(self.pos)
    }
}

/// What a user's mini profile shows.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct UserCard {
    pub name: String,
    /// Without "@"; empty if none.
    pub username: String,
    /// With "+"; empty if hidden.
    pub phone: String,
    pub status: String,
    pub bio: String,
    pub is_contact: bool,
    pub blocked: bool,
    /// The private chat with the user (created locally, no network).
    pub chat_id: i64,
    pub is_self: bool,
}

pub async fn user_card(client_id: i32, user_id: i64) -> TdResult<UserCard> {
    let enums::User::User(user) = functions::get_user(user_id, client_id).await.map_err(err)?;
    let enums::UserFullInfo::UserFullInfo(full) = functions::get_user_full_info(user_id, client_id)
        .await
        .map_err(err)?;
    let enums::Chat::Chat(chat) = functions::create_private_chat(user_id, true, client_id)
        .await
        .map_err(err)?;
    let enums::OptionValue::Integer(me) = functions::get_option("my_id".into(), client_id)
        .await
        .unwrap_or(enums::OptionValue::Empty)
    else {
        return Err("нет входа в аккаунт".into());
    };
    Ok(UserCard {
        name: format!("{} {}", user.first_name, user.last_name)
            .trim()
            .to_owned(),
        username: user
            .usernames
            .as_ref()
            .and_then(|u| u.active_usernames.first().cloned())
            .unwrap_or_default(),
        phone: if user.phone_number.is_empty() {
            String::new()
        } else {
            format!("+{}", user.phone_number)
        },
        status: status_label(&user.status),
        bio: full.bio.map(|b| b.text).unwrap_or_default(),
        is_contact: user.is_contact,
        blocked: matches!(full.block_list, Some(enums::BlockList::Main)),
        chat_id: chat.id,
        is_self: me.value == user_id,
    })
}

/// Adds the user to contacts under their own name, not sharing our phone.
pub async fn add_contact(client_id: i32, user_id: i64, name: String) -> TdResult<()> {
    let (first, last) = name.split_once(' ').unwrap_or((name.as_str(), ""));
    functions::add_contact(
        user_id,
        tdlib_rs::types::ImportedContact {
            phone_number: String::new(),
            first_name: first.to_owned(),
            last_name: last.to_owned(),
            note: FormattedText::default(),
        },
        false,
        client_id,
    )
    .await
    .map_err(err)
}

pub async fn set_blocked(client_id: i32, user_id: i64, blocked: bool) -> TdResult<()> {
    functions::set_message_sender_block_list(
        enums::MessageSender::User(tdlib_rs::types::MessageSenderUser { user_id }),
        blocked.then_some(enums::BlockList::Main),
        client_id,
    )
    .await
    .map_err(err)
}

/// Where a Telegram link (t.me/…, tg://…) leads inside the client.
#[derive(Debug, Clone, PartialEq)]
pub enum LinkTarget {
    Chat(i64),
    /// A message: its chat and id.
    Message(i64, i64),
    /// An invite to a chat the user is not in yet.
    Invite {
        link: String,
        title: String,
        members: i32,
    },
}

/// Links worth asking TDLib about; others go straight to the browser.
pub fn is_telegram_link(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    if lower.starts_with("tg:") {
        return true;
    }
    let rest = lower
        .strip_prefix("https://")
        .or_else(|| lower.strip_prefix("http://"))
        .unwrap_or(&lower);
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = host.strip_prefix("www.").unwrap_or(host);
    matches!(host, "t.me" | "telegram.me" | "telegram.dog") || host.ends_with(".t.me")
}

/// Resolves a Telegram link; `Ok(None)`: not something the client opens
/// itself (the browser should).
pub async fn resolve_link(client_id: i32, url: String) -> TdResult<Option<LinkTarget>> {
    use enums::InternalLinkType as L;
    let Ok(kind) = functions::get_internal_link_type(url.clone(), client_id).await else {
        return Ok(None);
    };
    let public_chat = |username: String| async move {
        let enums::Chat::Chat(chat) = functions::search_public_chat(username, client_id)
            .await
            .map_err(err)?;
        Ok::<_, String>(Some(LinkTarget::Chat(chat.id)))
    };
    match kind {
        L::PublicChat(p) => public_chat(p.chat_username).await,
        L::BotStart(b) => public_chat(b.bot_username).await,
        L::UserPhoneNumber(p) => {
            let enums::User::User(user) =
                functions::search_user_by_phone_number(p.phone_number, false, client_id)
                    .await
                    .map_err(err)?;
            let enums::Chat::Chat(chat) = functions::create_private_chat(user.id, true, client_id)
                .await
                .map_err(err)?;
            Ok(Some(LinkTarget::Chat(chat.id)))
        }
        L::Message(m) => {
            let enums::MessageLinkInfo::MessageLinkInfo(info) =
                functions::get_message_link_info(m.url, client_id)
                    .await
                    .map_err(err)?;
            if info.chat_id == 0 {
                return Err("сообщение недоступно".into());
            }
            Ok(Some(match info.message {
                Some(message) => LinkTarget::Message(info.chat_id, message.id),
                None => LinkTarget::Chat(info.chat_id),
            }))
        }
        L::ChatInvite(i) => {
            let enums::ChatInviteLinkInfo::ChatInviteLinkInfo(info) =
                functions::check_chat_invite_link(i.invite_link.clone(), client_id)
                    .await
                    .map_err(err)?;
            // Already a member (or a preview is open): straight to the chat.
            if info.chat_id != 0 {
                return Ok(Some(LinkTarget::Chat(info.chat_id)));
            }
            Ok(Some(LinkTarget::Invite {
                link: i.invite_link,
                title: info.title,
                members: info.member_count,
            }))
        }
        _ => Ok(None),
    }
}

/// Joins by an invite link; the chat id, or `None` if a request was sent
/// to the admins instead.
pub async fn join_by_link(client_id: i32, link: String) -> TdResult<Option<i64>> {
    match functions::join_chat_by_invite_link(link, client_id)
        .await
        .map_err(err)?
    {
        enums::ChatJoinResult::Success(s) => Ok(Some(s.chat_id)),
        enums::ChatJoinResult::RequestSent => Ok(None),
        _ => Err("вступить не удалось".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flood_wait_parses_seconds_from_the_retry_after_message() {
        assert_eq!(
            flood_wait("Too Many Requests: retry after 30 (429)"),
            Some(30)
        );
        assert_eq!(flood_wait("retry after 5"), Some(5));
        assert_eq!(flood_wait("Bad Request: CHAT_ID_INVALID (400)"), None);
        assert_eq!(flood_wait(""), None);
    }
    #[test]
    fn new_folder_has_only_selected_chat_and_valid_unicode_name() {
        let folder = new_chat_folder("  Папка 🚀  ", 42).unwrap();
        assert_eq!(folder.name.text.text, "Папка 🚀");
        assert!(folder.name.text.entities.is_empty());
        assert_eq!(folder.included_chat_ids, [42]);
        assert!(folder.pinned_chat_ids.is_empty());
        assert!(folder.excluded_chat_ids.is_empty());
        assert!(folder.icon.is_none());
        assert_eq!(folder.color_id, -1);
        assert!(!folder.is_shareable);
        assert!(!folder.exclude_muted && !folder.exclude_read && !folder.exclude_archived);
        assert!(!folder.include_contacts && !folder.include_non_contacts);
        assert!(!folder.include_bots && !folder.include_groups && !folder.include_channels);
        for invalid in [
            "",
            " \t ",
            "длинное название",
            "одна\nстрока",
            "одна\rстрока",
            "a\n",
        ] {
            assert!(new_chat_folder(invalid, 42).is_err(), "{invalid:?}");
        }
        assert!(new_chat_folder("ёжикиёжики12", 42).is_ok());
    }

    #[test]
    fn membership_edits_only_explicit_chat_ids_and_are_idempotent() {
        let original = tdlib_rs::types::ChatFolder {
            name: tdlib_rs::types::ChatFolderName {
                text: FormattedText {
                    text: "🚀Проекты".into(),
                    entities: vec![tdlib_rs::types::TextEntity {
                        offset: 0,
                        length: 2,
                        r#type: enums::TextEntityType::CustomEmoji(
                            tdlib_rs::types::TextEntityTypeCustomEmoji {
                                custom_emoji_id: 123,
                            },
                        ),
                    }],
                },
                animate_custom_emoji: true,
            },
            icon: Some(tdlib_rs::types::ChatFolderIcon {
                name: "Work".into(),
            }),
            color_id: 4,
            is_shareable: true,
            pinned_chat_ids: vec![7, 11],
            included_chat_ids: vec![8, 12],
            excluded_chat_ids: vec![9, 42],
            exclude_muted: true,
            exclude_read: true,
            exclude_archived: true,
            include_contacts: true,
            include_non_contacts: false,
            include_bots: true,
            include_groups: false,
            include_channels: true,
        };
        let mut included = original.clone();
        assert!(change_folder_membership(&mut included, 42, true));
        assert_eq!(included.excluded_chat_ids, [9]);
        assert_eq!(included.included_chat_ids, [8, 12, 42]);
        assert_eq!(included.pinned_chat_ids, [7, 11]);
        let snapshot = included.clone();
        assert!(!change_folder_membership(&mut included, 42, true));
        assert_eq!(included, snapshot);

        // An existing pin counts as included; only contradictory exclusion is removed.
        let mut pinned = original.clone();
        pinned.excluded_chat_ids.push(7);
        assert!(change_folder_membership(&mut pinned, 7, true));
        assert!(!pinned.included_chat_ids.contains(&7));
        assert_eq!(pinned.pinned_chat_ids, [7, 11]);

        let mut excluded = original.clone();
        assert!(change_folder_membership(&mut excluded, 7, false));
        assert_eq!(excluded.pinned_chat_ids, [11]);
        assert_eq!(excluded.included_chat_ids, [8, 12]);
        assert_eq!(excluded.excluded_chat_ids, [9, 42, 7]);
        assert!(!change_folder_membership(&mut excluded, 7, false));
        assert!(change_folder_membership(&mut excluded, 8, false));
        assert_eq!(excluded.included_chat_ids, [12]);
        assert_eq!(excluded.excluded_chat_ids, [9, 42, 7, 8]);

        for edited in [&included, &pinned, &excluded] {
            let mut untouched = edited.clone();
            untouched.pinned_chat_ids = original.pinned_chat_ids.clone();
            untouched.included_chat_ids = original.included_chat_ids.clone();
            untouched.excluded_chat_ids = original.excluded_chat_ids.clone();
            assert_eq!(untouched, original, "non-membership folder rules changed");
        }
    }

    #[test]
    fn default_cache_limits_use_kib_seconds_and_unlimited_file_count() {
        assert_eq!(
            storage_limits(crate::settings::CacheLimits::default()),
            [
                ("storage_max_files_size", 10_485_760),
                ("storage_max_time_from_last_access", 7_776_000),
                ("storage_max_file_count", i32::MAX as i64),
            ]
        );
    }

    #[test]
    fn cache_storage_limits_saturate_large_sizes_and_ages() {
        assert_eq!(
            storage_limits(crate::settings::CacheLimits {
                bytes: u64::MAX,
                days: u64::MAX,
            }),
            [
                ("storage_max_files_size", i32::MAX as i64),
                ("storage_max_time_from_last_access", i32::MAX as i64),
                ("storage_max_file_count", i32::MAX as i64),
            ]
        );
    }

    #[test]
    fn zero_cache_limits_still_allow_a_kib_and_a_second() {
        assert_eq!(
            storage_limits(crate::settings::CacheLimits { bytes: 0, days: 0 }),
            [
                ("storage_max_files_size", 1),
                ("storage_max_time_from_last_access", 1),
                ("storage_max_file_count", i32::MAX as i64),
            ]
        );
    }

    static NEXT_CACHED_FILE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    struct OfflineCachedFile(std::path::PathBuf);

    impl Drop for OfflineCachedFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    fn offline_cached_file(contents: &[u8]) -> OfflineCachedFile {
        use std::io::Write;

        let path = std::env::temp_dir().join(format!(
            "telega-stream-{}-{}",
            std::process::id(),
            NEXT_CACHED_FILE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .expect("create test-owned cached file");
        file.write_all(contents).expect("cache test bytes");
        OfflineCachedFile(path)
    }

    #[derive(Default)]
    struct OfflineFileBackend {
        available: u64,
        downloaded_after_request: Option<std::sync::atomic::AtomicBool>,
        path: Option<std::path::PathBuf>,
        prefix_error: bool,
        file_error: bool,
        download_error: bool,
        pending_download: bool,
        entered_download: Option<std::sync::mpsc::Sender<()>>,
    }

    impl FileStreamBackend for OfflineFileBackend {
        async fn prefix(&self, _: i32, _: i32, offset: u64) -> std::io::Result<u64> {
            if self.prefix_error {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "offline prefix failure",
                ));
            }
            if self
                .downloaded_after_request
                .as_ref()
                .is_some_and(|flag| !flag.load(std::sync::atomic::Ordering::Relaxed))
            {
                return Ok(0);
            }
            Ok(self.available.saturating_sub(offset))
        }

        async fn get_file(&self, _: i32, _: i32) -> std::io::Result<std::fs::File> {
            if self.file_error {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "offline local-file failure",
                ));
            }
            std::fs::File::open(self.path.as_ref().expect("cached test file"))
        }

        async fn download(&self, _: i32, _: i32, _: u64) -> std::io::Result<()> {
            if self.download_error {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "offline download failure",
                ));
            }
            if self.pending_download {
                if let Some(sender) = &self.entered_download {
                    sender.send(()).expect("stop test receiver");
                }
                return std::future::pending().await;
            }
            if let Some(flag) = &self.downloaded_after_request {
                flag.store(true, std::sync::atomic::Ordering::Relaxed);
            }
            Ok(())
        }
    }

    fn offline_stream(
        backend: OfflineFileBackend,
        stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
        request_timeout: std::time::Duration,
        progress_timeout: std::time::Duration,
    ) -> FileStream<OfflineFileBackend> {
        FileStream::with_backend(7, 11, 5, stop, backend, request_timeout, progress_timeout)
    }

    #[test]
    fn security_fix_file_stream_reports_prefix_download_and_local_file_failures() {
        use std::io::{ErrorKind, Read};
        use std::sync::{Arc, atomic::AtomicBool};
        use std::time::Duration;

        for (backend, kind, marker) in [
            (
                OfflineFileBackend {
                    prefix_error: true,
                    ..Default::default()
                },
                ErrorKind::PermissionDenied,
                "offline prefix failure",
            ),
            (
                OfflineFileBackend {
                    download_error: true,
                    ..Default::default()
                },
                ErrorKind::BrokenPipe,
                "offline download failure",
            ),
            (
                OfflineFileBackend {
                    available: 5,
                    file_error: true,
                    ..Default::default()
                },
                ErrorKind::NotFound,
                "offline local-file failure",
            ),
        ] {
            let mut stream = offline_stream(
                backend,
                Arc::new(AtomicBool::new(false)),
                Duration::from_millis(300),
                Duration::from_millis(300),
            );
            let error = stream.read(&mut [0u8; 1]).expect_err("backend failure");
            assert_eq!(error.kind(), kind);
            assert!(
                error.to_string().contains(marker),
                "backend error detail lost: {error}"
            );
        }
    }

    #[test]
    fn security_fix_file_stream_ends_no_progress_and_unanswered_downloads() {
        use std::io::{ErrorKind, Read};
        use std::sync::{Arc, atomic::AtomicBool};
        use std::time::Duration;

        for (backend, request_timeout, progress_timeout) in [
            (
                OfflineFileBackend::default(),
                Duration::from_secs(1),
                Duration::from_millis(90),
            ),
            (
                OfflineFileBackend {
                    pending_download: true,
                    ..Default::default()
                },
                Duration::from_millis(90),
                Duration::from_secs(1),
            ),
        ] {
            let mut stream = offline_stream(
                backend,
                Arc::new(AtomicBool::new(false)),
                request_timeout,
                progress_timeout,
            );
            assert_eq!(
                stream.read(&mut [0u8; 1]).expect_err("deadline").kind(),
                ErrorKind::TimedOut
            );
        }
    }

    #[test]
    fn security_fix_file_stream_stop_interrupts_a_pending_download_request() {
        use std::io::{ErrorKind, Read};
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
            mpsc,
        };
        use std::time::Duration;

        let (entered_tx, entered_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let mut stream = offline_stream(
            OfflineFileBackend {
                pending_download: true,
                entered_download: Some(entered_tx),
                ..Default::default()
            },
            Arc::clone(&stop),
            Duration::from_secs(2),
            Duration::from_secs(2),
        );
        let worker = std::thread::spawn(move || {
            done_tx
                .send(stream.read(&mut [0u8; 1]).map_err(|error| error.kind()))
                .expect("stop result receiver");
        });
        entered_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("download future started");
        stop.store(true, Ordering::Relaxed);
        assert_eq!(
            done_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("stop must interrupt pending request"),
            Err(ErrorKind::Interrupted)
        );
        worker.join().expect("stream worker");
    }

    #[test]
    fn security_fix_file_stream_reads_cached_bytes_after_seek_and_handles_empty_reads() {
        use std::io::{Read, Seek};
        use std::sync::{Arc, atomic::AtomicBool};
        use std::time::Duration;

        let fixture = offline_cached_file(b"ABCDE");

        let mut stream = offline_stream(
            OfflineFileBackend {
                available: 5,
                path: Some(fixture.0.clone()),
                ..Default::default()
            },
            Arc::new(AtomicBool::new(false)),
            Duration::from_millis(300),
            Duration::from_millis(300),
        );
        assert_eq!(stream.read(&mut []).expect("empty read"), 0);
        let mut bytes = [0u8; 2];
        assert_eq!(stream.read(&mut bytes).expect("first cached range"), 2);
        assert_eq!(&bytes, b"AB");
        stream
            .seek(std::io::SeekFrom::Start(3))
            .expect("seek forward");
        assert_eq!(stream.read(&mut bytes).expect("later cached range"), 2);
        assert_eq!(&bytes, b"DE");
        assert_eq!(stream.read(&mut bytes).expect("end of file"), 0);
        stream
            .seek(std::io::SeekFrom::Start(1))
            .expect("seek backward");
        assert_eq!(stream.read(&mut bytes).expect("earlier cached range"), 2);
        assert_eq!(&bytes, b"BC");
    }

    #[test]
    fn security_fix_unknown_size_stream_downloads_reads_and_seeks_without_false_eof() {
        use std::io::{ErrorKind, Read, Seek};
        use std::sync::{Arc, atomic::AtomicBool};
        use std::time::Duration;

        let fixture = offline_cached_file(b"ABCDE");
        let mut stream = FileStream::with_backend(
            7,
            11,
            0,
            Arc::new(AtomicBool::new(false)),
            OfflineFileBackend {
                available: 5,
                path: Some(fixture.0.clone()),
                downloaded_after_request: Some(AtomicBool::new(false)),
                ..Default::default()
            },
            Duration::from_millis(300),
            Duration::from_millis(90),
        );
        let mut bytes = [0u8; 2];
        assert_eq!(
            stream
                .read(&mut bytes)
                .expect("downloaded unknown-size bytes"),
            2
        );
        assert_eq!(&bytes, b"AB");
        stream
            .seek(std::io::SeekFrom::Start(3))
            .expect("seek in unknown-size file");
        assert_eq!(
            stream.read(&mut bytes).expect("later unknown-size bytes"),
            2
        );
        assert_eq!(&bytes, b"DE");
        assert_eq!(
            stream
                .read(&mut bytes)
                .expect_err("no final size or EOF signal")
                .kind(),
            ErrorKind::TimedOut,
            "unknown size must not be mistaken for a completed file"
        );
    }
}
