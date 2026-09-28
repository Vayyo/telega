mod accounts;
mod avatars;
mod card;
mod extra;
mod icons;
pub(crate) mod look;
pub(crate) mod media;
mod nav;
mod notify;
mod pane;
mod password;
mod picker;
mod playback;
mod plugin_runtime;
mod plugins_view;
mod qr;
pub(crate) mod rich;
mod selectable;
mod session;
mod tabs;
mod tray;
mod typing;
mod video;
mod view;
mod viewer;

use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use iced::keyboard::{self, Key, Modifiers};
use iced::widget::scrollable::Viewport;
use iced::{Subscription, Task, window};
use tdlib_rs::enums::{AuthorizationState, ChatList, MessageSender, OptionValue, Update};
use tdlib_rs::types::{ChatPosition, File, Message};

use crate::archive::Archive;
use crate::plugins::{self, Action, HostCmd, HostEvent, HostHandle, PluginInfo};
use crate::settings::Settings;
use crate::td;
use media::Media;
use pane::{ChatPane, Page};
use plugin_runtime::message_event;
use session::{FolderCreation, FolderReorder, FolderReorderState, ListRead, Session};

pub(crate) type WinId = window::Id;

const FOLDER_REORDER_UNCONFIRMED: &str =
    "Порядок папок не подтверждён: старый запрос ещё может примениться";
const FOLDER_CREATE_UNCONFIRMED: &str =
    "Создание папки не подтверждено списком Telegram; проверьте папки перед повтором";

#[derive(Debug, Clone)]
pub(crate) enum Msg {
    Td(i32, Box<Update>),
    Done(Result<(), String>),
    /// Completion of loading a separate archive window for one client.
    ArchiveWindowLoaded(WinId, i32, Result<(), String>),
    /// Deletion from the UI finished; on failure the ids stop counting as
    /// own deletions, same as a plugin's deletion (`PluginDeleted`).
    DeleteDone(i64, Vec<i64>, Result<(), String>),
    Input(String),
    SubmitAuth,
    /// Return from code/password entry to phone entry (wrong number).
    BackToPhone,
    /// First click on "Выйти" asks for confirmation; `ConfirmLogOut(true)` logs out.
    LogOut,
    ConfirmLogOut(bool),
    OpenSettings,
    CloseSettings,
    SetKeepDeleted(bool),
    /// Opens a separate chat window, optionally with a chat already selected.
    OpenChatWindow(Option<i64>),
    /// Opens the archive list in its own window.
    OpenArchiveWindow(WinId, i32),
    WindowClosed(WinId),
    Key(WinId, Key, Modifiers),
    /// Action inside the chat pane of a window.
    Pane(WinId, PaneMsg),
    Plugin(HostEvent),
    /// Result of an action a plugin asked for: the original action (to
    /// retry if it failed on a flood wait), a log line or error.
    PluginDone(String, Action, Result<String, String>),
    /// A plugin deletion after filtering: only these ids are the user's own.
    PluginDeleteOwn(String, i64, Vec<i64>, bool, Action),
    /// Deletion finished; on failure the ids stop counting as own deletions.
    PluginDeleted(String, i64, Vec<i64>, Action, Result<(), String>),
    PluginToggle(String, bool),
    PluginDryRun(String, bool),
    PluginSetting(String, String, serde_json::Value),
    /// Raw text typed into a numeric plugin setting; parsed and possibly
    /// committed by the update loop, so a decimal point mid-entry is not
    /// snapped back to the last committed value.
    PluginNumberSetting(String, String, String),
    StopAllPlugins,
    OpenPluginHelp(bool),
    /// Tab bar of the main window.
    SelectTab(i64),
    CloseTab(i64),
    /// The trash can in the tab bar: close every tab that is not pinned.
    CloseAllTabs,
    TogglePin(i64),
    TabsWheel(iced::mouse::ScrollDelta),
    /// Fires shortly after `save_settings_debounced` scheduled it; a stale
    /// token (superseded by a newer schedule) is ignored.
    FlushSettings(u64),
    /// Files: download, cancel, open with the system, show in folder.
    FileDownload(i32),
    FileCancel(i32),
    FileOpen(i32),
    FileShowFolder(i32),
    ImageDecoded(i32, Result<(u32, u32, Vec<u8>), String>),
    /// Round profile picture of a file, ready to show.
    AvatarDecoded(i32, Result<Vec<u8>, String>),
    /// A picture's sensor reported it on screen: fetch it if missing, or
    /// keep it from being evicted from the cache while it stays there.
    AvatarShown(avatars::Peer),
    /// A file dragged onto a window: sent to its chat.
    FileDropped(WinId, PathBuf),
    /// Result of reading an image from the clipboard (Ctrl+V).
    Pasted(WinId, Result<Option<PathBuf>, String>),
    WindowFocus(WinId, bool),
    /// A desktop notification was clicked: open its chat, if its account
    /// (the first field) is still the active one.
    NotificationClicked(u32, i64),
    SystemTheme(iced::theme::Mode),
    /// Scheme or accent chosen in the settings.
    SetLook(look::LookSettings),
    RecentStickers(Result<Vec<td::StickerRef>, String>),
    StickerSets(Result<Vec<(i64, String)>, String>),
    StickerSet(i64, Result<Vec<td::StickerRef>, String>),
    Password(password::PasswordMsg),
    RolesLoaded(i64, Result<HashMap<i64, td::Role>, String>),
    /// Pinned messages of a chat, reloaded for every pane showing it
    /// (including background tabs, unlike `PaneMsg::PinnedLoaded`).
    PinnedRefreshed(i64, Result<Vec<Message>, String>),
    /// Answer to `setTdlibParameters` (a wrong key shows up here).
    ParametersSet(Result<(), String>),
    Video(video::VideoMsg),
    Card(card::CardMsg),
    ClearNotice,
    /// Where a Telegram link leads (`None`: open it in the browser).
    LinkResolved(WinId, String, Result<Option<td::LinkTarget>, String>),
    Joined(WinId, Result<Option<i64>, String>),
    MouseReleased,
    /// Click on a photo: the full-window viewer.
    ViewPhoto(WinId, i32),
    ViewerDecoded(i32, Result<(u32, u32, Vec<u8>), String>),
    /// ‹ / › or ←/→ in the viewer.
    ViewerStep(bool),
    CloseViewer,
    /// Click on the chat title: open or close the profile panel.
    ToggleProfile(WinId),
    /// Profile of another chat (a forwarded channel) over this window.
    OpenProfile(WinId, i64),
    Profile(WinId, ProfileAction),
    ProfileLoaded(WinId, i64, Result<td::Profile, String>),
    /// A folder tab: its chats (`None` = all chats).
    ShowFolder(WinId, Option<i32>),
    /// Move a folder among folder IDs, never the main chat list (true = up).
    ReorderFolder(WinId, i32, bool),
    FolderReordered(WinId, i32, u64, Result<(), String>),
    /// User acknowledges that the accepted request has not been confirmed.
    /// The cached server order stays unchanged; no reorder is replayed.
    AcknowledgeFolderReorder(WinId, i32, u64),
    FolderName(WinId, i64, String),
    ConfirmNewFolder(WinId, i64),
    FolderCreated(WinId, i32, u64, Result<i32, String>),
    AcknowledgeFolderCreation(WinId, i32, u64),
    /// Show the archived chats instead of the main list (or back).
    ShowArchive(WinId, bool),
    /// Client-side placement of the entry, scoped to the active account.
    SetArchiveCollapsed(WinId, i32, bool),
    OpenArchiveFromMenu(i32),
    ToggleArchiveSettings(WinId, bool),
    RetryArchiveSettings(WinId),
    ArchiveSettingsLoaded(
        WinId,
        i32,
        u64,
        Result<tdlib_rs::types::ArchiveChatListSettings, String>,
    ),
    ArchiveCapabilityLoaded(WinId, i32, u64, Result<bool, String>),
    ChangeArchiveFlag(WinId, td::ArchiveFlag, bool),
    ArchiveFetched(
        WinId,
        i32,
        u64,
        td::ArchiveFlag,
        bool,
        Result<tdlib_rs::types::ArchiveChatListSettings, String>,
    ),
    ArchiveSaved(WinId, i32, u64, Result<(), String>),
    ArchiveReadback(
        WinId,
        i32,
        u64,
        Result<tdlib_rs::types::ArchiveChatListSettings, String>,
    ),
    /// Confirmation is scoped to the window, client and current list.
    AskReadList(WinId, i32),
    ConfirmReadList(WinId, i32, u64, bool),
    ListReadDone(WinId, i32, u64, Result<(), String>),
    /// Right click on a chat in the list: its menu (`None` closes it).
    ChatMenu(WinId, Option<i64>),
    ChatOp(WinId, i64, ChatOp),
    /// Actions in the per-window picker of existing Telegram folders.
    FolderAction(WinId, i64, i32, bool),
    FolderEdited(WinId, i32, i64, i32, u64, Result<bool, String>),
    /// Sign in by scanning a QR code with a logged-in phone.
    LoginQr,
    /// Next frame of the "печатает…" dots.
    TypingTick,
    ToggleAccounts,
    SwitchAccount(u32),
    AddAccount,
    CancelAddAccount,
    #[cfg(target_os = "linux")]
    Tray(tray::TrayEvent),
    SetNotifications(bool),
    /// Messages fetched for reply previews.
    RepliesLoaded(Vec<Message>),
    /// Chat list filter of a window.
    ListQuery(WinId, String),
    ListChats(WinId, String, Result<Vec<i64>, String>),
    /// Enter in the chat list filter: archive messages in Archive, global otherwise.
    ListSearchMessages(WinId),
    ListMore(WinId),
    ListMessages(
        WinId,
        String,
        u64,
        Option<String>,
        Result<(Vec<Message>, Option<String>), String>,
    ),
    /// Open a chat of a search result at the found message.
    OpenFound(WinId, i64, i64),
    /// Voice messages: play/pause, decoded audio, player progress.
    VoiceToggle(i32),
    VoiceDecoded(i32, Result<playback::PcmHandle, String>),
    PlaybackTick,
    /// Looped animations on screen / off screen / next frame / stream end.
    AnimShow(i32, media::Motion),
    AnimHide(i32),
    AnimFrame(i32, u32, u32, Vec<u8>),
    /// The frame stream ended on its own (broken/empty file): frees the
    /// `MAX_ANIMATIONS` slot it held.
    AnimEnded(i32),
    /// A video: opened in the system player once downloaded.
    PlayVideo(i32),
    /// Voice recording in a chat window.
    RecordStart(WinId),
    RecordStarted(WinId, i64, Result<playback::RecorderHandle, String>),
    /// Stop recording: send (true) or discard.
    RecordStop(bool),
    Ignore,
}

#[derive(Debug, Clone)]
pub(crate) enum PaneMsg {
    SelectChat(i64),
    HistoryLoaded(i64, Result<Vec<Message>, String>),
    /// Page of messages older than the given id.
    OlderLoaded(i64, i64, Result<Vec<Message>, String>),
    /// Chat scrolled; near the top older history is requested.
    Scrolled(Viewport),
    /// Chat list scrolled (virtualization).
    ListScrolled(Viewport),
    /// In-chat result viewport; ignore reports from an earlier query.
    SearchScrolled(iced::widget::Id, Viewport),
    /// A rendered message reported its height (virtualization).
    Measured(i64, f32),
    /// Typing in the input field.
    Compose(iced::widget::text_editor::Action),
    Send,
    PinnedLoaded(i64, Result<Vec<Message>, String>),
    /// Click on the pinned bar: jump to the shown one, then show the next
    /// older one.
    PinnedClicked,
    /// "Закрепить для себя" (true) or take it back (false).
    PinLocal(i64, bool),
    /// "Редактировать": load the message's text into the input field.
    StartEdit(i64),
    EditLoaded(i64, i64, Result<String, String>),
    CancelEdit,
    /// Right click on a message: open its context menu.
    OpenMenu(i64),
    MenuReady(i64, Result<td::MessageRights, String>),
    ReactionsReady(i64, Result<Vec<String>, String>),
    /// "Переслать": pick a chat for these messages.
    Forward(Vec<i64>),
    ForwardSelection,
    ForwardQuery(String),
    ForwardTo(i64),
    CancelForward,
    /// "Выбрать" in the menu, or a click on a message in selection mode.
    Select(i64),
    ClearSelection,
    CopySelection,
    /// "Удалить" on the selection: ask what is allowed.
    DeleteSelection,
    SelectionRights(Result<td::MessageRights, String>),
    DeleteSelected {
        revoke: bool,
    },
    DraftLoaded(i64, Result<String, String>),
    /// The emoji and sticker panel: toggle, switch tab, close.
    TogglePicker,
    PickerTab(picker::Tab),
    ClosePicker,
    InsertEmoji(String),
    SendSticker(Box<td::StickerRef>),
    /// A choice in a poll.
    Vote(i64, i32),
    /// Toggling one option of a poll that allows several answers, before
    /// «Голосовать» sends them.
    ToggleVote(i64, i32),
    /// «Голосовать»: sends the options picked with `ToggleVote`.
    SubmitVote(i64),
    /// Click on a reaction (under a message or in the menu): put or take back.
    React(i64, td::ReactionKey),
    /// Esc, a click outside the menu, or "Отмена".
    CloseMenu,
    CopyText(i64),
    Delete {
        id: i64,
        revoke: bool,
    },
    /// Drop an archived copy of a message deleted on the server.
    Forget(i64),
    /// A photo scrolled into view: download or decode it.
    MediaVisible(i32),
    /// A photo's sensor reported it left the screen: it may be evicted
    /// from the cache again without hiding a picture the user still sees.
    MediaHidden(i32),
    /// "Прикрепить": pick files to send.
    Attach,
    FilesChosen(Vec<PathBuf>),
    /// Mouse down on a message's text / moved while held (byte offset).
    TextPress(i64, usize),
    TextDrag(i64, usize),
    /// A click that no widget took: drop the text selection.
    ClickOutside,
    /// Click on a link or a hidden spoiler in a message.
    Link(Link),
    /// Confirmed / declined opening of a hidden link.
    OpenLink(String),
    CancelLink,
    /// «Вступить» on an invite link / declined.
    JoinChat(String),
    CancelJoin,
    /// Show the hidden (spoiler) parts of a message.
    Reveal(i64),
    Reply(i64),
    CancelReply,
    /// Scroll to a message, loading history around it if needed.
    JumpTo(i64),
    AroundLoaded(i64, i64, Result<Vec<Message>, String>),
    NewerLoaded(i64, i64, Result<Vec<Message>, String>),
    /// "К последним": back to the newest messages after a jump.
    ToLatest,
    SearchToggle,
    SearchQuery(String),
    SearchSubmit,
    SearchMore,
    SearchResults(
        i64,
        String,
        u64,
        Option<i64>,
        Result<(Vec<Message>, Option<i64>), String>,
    ),
}

/// Clickable parts of formatted text.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Link {
    /// `hidden`: the visible text is not the address; opening asks first.
    Url {
        url: String,
        hidden: bool,
    },
    Spoiler(i64),
    /// Monospace text: a click copies it.
    Copy(String),
}

#[derive(Debug, PartialEq)]
enum Auth {
    Starting,
    Phone,
    Code,
    Password {
        hint: String,
    },
    /// Waiting for a scan of the QR code by a logged-in phone.
    Qr {
        link: String,
        image: Option<iced::widget::image::Handle>,
    },
    Ready,
    LoggingOut,
    /// The account's data is encrypted: waiting for the client password.
    Locked {
        error: Option<String>,
        forgot: bool,
    },
    Unsupported(String),
}

struct ChatItem {
    title: String,
    /// Lowercase `title`, kept alongside it so the chat list filter does not
    /// redo `to_lowercase()` for every chat on every redraw.
    title_lower: String,
    last: String,
    /// First line of `last`, already cut for the chat list; built once when
    /// `last` changes instead of on every frame.
    preview: String,
    unread: i32,
    /// Position in the main list; 0 = not in the list.
    order: i64,
    /// Id of the newest message, to know when history is complete.
    last_id: i64,
    /// The newest message is the user's own; its receipt shows in the list.
    last_outgoing: bool,
    last_pending: bool,
    /// Own messages up to this id were read by the other side.
    read_outbox: i64,
    /// Incoming messages up to this id were read by the user.
    read_inbox: i64,
    /// One-to-one chat: typing shows without a name.
    private: bool,
    /// Position in the archive; 0 = not archived.
    archive_order: i64,
    /// Pinned at the top of its list (main or archive).
    pinned: bool,
    notify: tdlib_rs::types::ChatNotificationSettings,
    /// Positions in user folders: (folder id, order).
    folders: Vec<(i32, i64)>,
    /// Private chat, group or channel (for the profile panel).
    kind: Option<tdlib_rs::enums::ChatType>,
    /// Unsent text saved in Telegram (from any device).
    draft: Option<tdlib_rs::types::FormattedText>,
}

impl ChatItem {
    /// Notifications are off for this chat.
    fn muted(&self) -> bool {
        !self.notify.use_default_mute_for && self.notify.mute_for > 0
    }
}

/// Buttons of the chat profile panel.
#[derive(Debug, Clone)]
pub(crate) enum ProfileAction {
    ToggleMute,
    Discuss,
    ToggleMore,
    ToggleQr,
    NewWindow,
    CopyLink,
    /// «Перейти в канал»: open the chat shown in the panel.
    OpenChat,
    Leave,
    Left(i64, Result<(), String>),
}

/// Chat list menu actions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChatOp {
    Pin(bool),
    LocalPin(bool),
    Archive(bool),
    Folders,
    NewFolder,
    Mute(bool),
    Close,
}

/// State of an own message, shown as a colored dot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Receipt {
    Sending,
    Sent,
    Read,
    /// The server rejected the send; shown with "!" instead of a dot.
    Failed,
}

#[derive(Clone)]
pub(crate) struct MsgItem {
    pub(crate) chat_id: i64,
    pub(crate) id: i64,
    pub(crate) sender: MessageSender,
    pub(crate) text: String,
    pub(crate) outgoing: bool,
    /// Deleted on the server; shown from the local archive.
    pub(crate) deleted: bool,
    /// Photo or file carried by the message.
    pub(crate) media: Option<Media>,
    /// Outgoing, not yet confirmed by the server (upload in progress).
    pub(crate) pending: bool,
    /// Outgoing, the server rejected the send (no rights, flood wait, file
    /// too large); shown with a red "!" instead of a receipt dot.
    pub(crate) failed: bool,
    /// Unix time the message was sent; 0 when unknown (old archive rows).
    pub(crate) date: i32,
    /// Changed after sending: "изменено" next to the time.
    pub(crate) edited: bool,
    pub(crate) reactions: Vec<Reaction>,
    /// Author of the original of a forwarded message.
    pub(crate) forwarded: Option<Origin>,
    /// Link preview, poll, contact or location card.
    pub(crate) extra: Option<Box<extra::Extra>>,
    /// The sender's member tag in the group when the message was sent.
    pub(crate) sender_tag: String,
    /// Formatted text (or media caption).
    pub(crate) rich: Vec<rich::Piece>,
    /// Message of the same chat this one answers.
    pub(crate) reply_to: Option<i64>,
}

/// Context menu of one message in a chat pane.
struct Menu {
    message_id: i64,
    /// `None` while TDLib is asked what is allowed.
    rights: Option<td::MessageRights>,
    /// Emoji the user may react with; empty while loading or if none.
    reactions: Vec<String>,
}

/// Where a forwarded message comes from.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Origin {
    User(i64),
    /// A user who hides their account: only the name is known.
    Name(String),
    Chat(i64),
}

impl Origin {
    fn of(info: &tdlib_rs::types::MessageForwardInfo) -> Self {
        use tdlib_rs::enums::MessageOrigin;
        match &info.origin {
            MessageOrigin::User(u) => Self::User(u.sender_user_id),
            MessageOrigin::HiddenUser(h) => Self::Name(h.sender_name.clone()),
            MessageOrigin::Chat(c) => Self::Chat(c.sender_chat_id),
            MessageOrigin::Channel(c) => Self::Chat(c.chat_id),
        }
    }
}

/// One reaction under a message.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Reaction {
    pub(crate) key: td::ReactionKey,
    pub(crate) count: i32,
    /// Put by the user.
    pub(crate) chosen: bool,
}

/// Text pieces of a message: its formatted text or caption, else the label
/// ("[Фото]"…); cards (poll, contact, place) say it themselves.
pub(crate) fn rich_of(
    content: &tdlib_rs::enums::MessageContent,
    extra: Option<&extra::Extra>,
) -> Vec<rich::Piece> {
    if extra.is_some_and(|e| !e.keeps_text()) {
        return Vec::new();
    }
    rich::formatted_of(content)
        .map_or_else(|| rich::plain(&td::message_text(content)), rich::pieces)
}

pub(crate) fn reactions_of(
    info: Option<&tdlib_rs::types::MessageInteractionInfo>,
) -> Vec<Reaction> {
    info.and_then(|i| i.reactions.as_ref())
        .map(|r| {
            r.reactions
                .iter()
                .map(|r| Reaction {
                    key: td::ReactionKey::of(&r.r#type),
                    count: r.total_count,
                    chosen: r.is_chosen,
                })
                .collect()
        })
        .unwrap_or_default()
}

impl MsgItem {
    /// Builds the item and collects the TDLib files it references, in one
    /// pass over the message content: `media_of`/`extra_of` decode base64
    /// thumbnails into fresh GPU-backed handles, so parsing the same new
    /// message three times (as `Update::NewMessage` used to, once each for
    /// `note_files`, `archive_save` and the item pushed into the pane) redid
    /// that decoding on every incoming message.
    fn parse(m: &Message) -> (Self, Vec<File>) {
        let (media, mut files) = media::media_of(&m.content)
            .map_or((None, Vec::new()), |(media, files)| (Some(media), files));
        let (extra, extra_file) =
            extra::extra_of(&m.content).map_or((None, None), |(extra, file)| (Some(extra), file));
        files.extend(extra_file);
        let rich = rich_of(&m.content, extra.as_ref());
        let item = Self {
            chat_id: m.chat_id,
            id: m.id,
            sender: m.sender_id.clone(),
            text: td::message_text(&m.content),
            outgoing: m.is_outgoing,
            deleted: false,
            media,
            pending: matches!(
                m.sending_state,
                Some(tdlib_rs::enums::MessageSendingState::Pending(_))
            ),
            failed: matches!(
                m.sending_state,
                Some(tdlib_rs::enums::MessageSendingState::Failed(_))
            ),
            date: m.date,
            edited: m.edit_date > 0,
            reactions: reactions_of(m.interaction_info.as_ref()),
            forwarded: m.forward_info.as_ref().map(Origin::of),
            extra: extra.map(Box::new),
            sender_tag: m.sender_tag.clone(),
            rich,
            reply_to: match &m.reply_to {
                Some(tdlib_rs::enums::MessageReplyTo::Message(r))
                    if r.chat_id == 0 || r.chat_id == m.chat_id =>
                {
                    Some(r.message_id)
                }
                _ => None,
            },
        };
        (item, files)
    }
}

impl From<&Message> for MsgItem {
    fn from(m: &Message) -> Self {
        Self::parse(m).0
    }
}

impl App {
    fn badge(&self) -> Option<tray::Badge> {
        tray::badge(self.session.unread.0, self.session.unread.1)
    }
}

impl App {
    fn load_roles(&self, chat_id: i64) -> Task<Msg> {
        let Some(kind) = self
            .session
            .chats
            .get(&chat_id)
            .and_then(|c| c.kind.clone())
        else {
            return Task::none();
        };
        if !matches!(
            kind,
            tdlib_rs::enums::ChatType::BasicGroup(_) | tdlib_rs::enums::ChatType::Supergroup(_)
        ) {
            return Task::none();
        }
        Task::perform(
            td::chat_roles(self.session.client_id, chat_id, kind),
            move |r| Msg::RolesLoaded(chat_id, r),
        )
    }

    /// What goes next to a sender's name: the role (owner, admin, or an
    /// admin's title) marked as a role, else the member tag.
    pub(crate) fn sender_badge(&self, m: &MsgItem) -> Option<(String, bool)> {
        let MessageSender::User(user) = &m.sender else {
            return None;
        };
        let role = self
            .session
            .roles
            .get(&m.chat_id)
            .and_then(|r| r.get(&user.user_id));
        match role {
            Some(role) if role.admin => {
                let label = if !role.tag.is_empty() {
                    role.tag.clone()
                } else if role.owner {
                    "владелец".to_owned()
                } else {
                    "админ".to_owned()
                };
                Some((label, true))
            }
            Some(role) if !role.tag.is_empty() => Some((role.tag.clone(), false)),
            _ => (!m.sender_tag.is_empty()).then(|| (m.sender_tag.clone(), false)),
        }
    }

    fn load_pinned(&self, window: WinId, chat_id: i64) -> Task<Msg> {
        Task::perform(
            td::pinned_messages(self.session.client_id, chat_id),
            move |r| Msg::Pane(window, PaneMsg::PinnedLoaded(chat_id, r)),
        )
    }

    /// Receipt of an own message; `None` for others' messages.
    pub(crate) fn receipt(
        &self,
        chat_id: i64,
        id: i64,
        outgoing: bool,
        pending: bool,
    ) -> Option<Receipt> {
        if !outgoing {
            return None;
        }
        if pending {
            return Some(Receipt::Sending);
        }
        let read = self
            .session
            .chats
            .get(&chat_id)
            .is_some_and(|c| id <= c.read_outbox);
        Some(if read { Receipt::Read } else { Receipt::Sent })
    }
}

impl App {
    /// "печатает…" for a chat, with the current dots.
    pub(crate) fn typing_label(&self, chat_id: i64) -> Option<String> {
        let names: Vec<&str> = self
            .session
            .typing
            .senders(chat_id)
            .map(|s| {
                let name = self.sender_display(s);
                name.split_whitespace().next().unwrap_or(name)
            })
            .collect();
        let private = self.session.chats.get(&chat_id).is_some_and(|c| c.private);
        typing::label(private, &names).map(|l| format!("{l}{}", self.session.typing.dots()))
    }
}

impl MsgItem {
    /// The user has put this reaction.
    pub(crate) fn reacted(&self, key: &td::ReactionKey) -> bool {
        self.reactions.iter().any(|r| &r.key == key && r.chosen)
    }

    /// Text with spoilers covered, for quotes, search results and previews.
    pub(crate) fn preview(&self) -> String {
        if !self.rich.iter().any(|p| p.style.spoiler) {
            return self.text.clone();
        }
        let raw: String = self.rich.iter().map(|p| p.text.as_str()).collect();
        let hidden = rich::masked(&self.rich);
        match self.text.strip_suffix(raw.as_str()) {
            Some(prefix) => format!("{prefix}{hidden}"),
            None => hidden,
        }
    }
}

fn window_settings(width: f32) -> window::Settings {
    window::Settings {
        size: iced::Size::new(width, 700.0),
        // One Wayland app id for all windows, so compositor rules can match them.
        #[cfg(target_os = "linux")]
        platform_specific: window::settings::PlatformSpecific {
            application_id: "telega".into(),
            ..Default::default()
        },
        icon: tray::window_icon(),
        ..Default::default()
    }
}

pub(crate) struct App {
    api_id: i32,
    api_hash: String,

    /// Full client window; closing it quits the program.
    main_window: WinId,
    settings: Settings,
    settings_path: PathBuf,
    /// Plugin thread; `None` until it has started.
    plugin_host: Option<HostHandle>,
    plugin_infos: Vec<PluginInfo>,
    /// Last lines of each plugin's journal; "" = plugin system itself.
    plugin_logs: HashMap<String, std::collections::VecDeque<String>>,
    /// Window having keyboard focus; its visible chat gets no notifications.
    focused: Option<WinId>,
    /// Light or dark as the system says; `None` until it has said.
    /// System light/dark (for the "as in the system" scheme).
    system_mode: iced::theme::Mode,
    look: look::Look,
    #[cfg(target_os = "linux")]
    tray: Option<tray::TrayHandle>,
    error: Option<String>,
    /// The settings file on disk failed to parse at start: `self.settings`
    /// is defaults, and saving over the broken file is refused (see
    /// `try_save_settings`) so whatever is still in it, including a
    /// password salt, is not lost for good by the next autosave.
    settings_load_failed: bool,

    /// Everything tied to the running TDLib client / logged-in account:
    /// replaced wholesale by a blank one on account switch or log-out
    /// (see `start_session`) instead of a hand-maintained carry-over list.
    session: Session,
}

/// A runtime value overrides an embedded one even when it is invalid.
/// Non-Unicode values are invalid as well, not grounds for falling back.
fn configured_value<'a>(
    runtime: Option<&'a std::ffi::OsStr>,
    embedded: Option<&'a str>,
) -> Option<&'a str> {
    runtime
        .map(|value| value.to_str().unwrap_or(""))
        .or(embedded)
}

/// Never include the hash or invalid input in diagnostics.
fn parse_api_credentials(id: Option<&str>, hash: Option<&str>) -> Result<(i32, String), String> {
    let api_id = id
        .and_then(|value| value.parse::<i32>().ok())
        .filter(|&value| value > 0)
        .ok_or("TG_API_ID: укажите положительное число в .env (см. .env.example) или переменной окружения TG_API_ID.")?;
    let api_hash = hash
        .filter(|value| !value.trim().is_empty())
        .ok_or("TG_API_HASH: укажите непустое значение в .env (см. .env.example) или переменной окружения TG_API_HASH.")?;
    Ok((api_id, api_hash.to_owned()))
}

impl App {
    pub(crate) fn boot() -> (Self, Task<Msg>) {
        // Keys are embedded at build time (from .env, see build.rs); the
        // environment can override them. No .env is read at run time: one in
        // the working directory could redirect data or the programs started.
        let env_id = std::env::var_os("TG_API_ID");
        let env_hash = std::env::var_os("TG_API_HASH");
        let credentials = parse_api_credentials(
            configured_value(env_id.as_deref(), option_env!("TG_API_ID")),
            configured_value(env_hash.as_deref(), option_env!("TG_API_HASH")),
        );
        let data_error = crate::paths::init().err();
        media::clean_paste_dir();
        let path = crate::paths::settings();
        let (settings, error) = match Settings::load(&path) {
            Ok(s) => (s, None),
            Err(e) => (Settings::default(), Some(e)),
        };
        let load_failed = error.is_some();
        let (main_window, open) = window::open(window_settings(1000.0));
        let (mut app, start) = match credentials {
            Ok((api_id, api_hash)) => {
                let client_id = tdlib_rs::create_client();
                let app = Self::new(client_id, api_id, api_hash, settings, path, main_window);
                (app, Task::perform(td::start(client_id), Msg::Done))
            }
            Err(message) => {
                // No TDLib client is created or started until configuration is fixed
                // and the application is restarted.
                let mut app = Self::new(0, 0, String::new(), settings, path, main_window);
                app.session.auth = Auth::Unsupported(message);
                (app, Task::none())
            }
        };
        app.settings_load_failed = load_failed;
        app.error = data_error.or(error);
        // The system theme as iced read it at start, before any second
        // window could overwrite it.
        let theme = iced::system::theme().map(Msg::SystemTheme);
        (app, Task::batch([open.discard(), start, theme]))
    }

    /// Fresh TDLib client with empty state on the active account slot,
    /// replacing the session of the account that is gone. A closed client
    /// cannot be reused, so log out and account switches start a new one
    /// in the same main window.
    fn start_session(&mut self) -> Task<Msg> {
        let client_id = tdlib_rs::create_client();
        self.session = Session::new(client_id, self.main_window, self.settings.active_account);
        Task::perform(td::start(client_id), Msg::Done)
    }

    fn new(
        client_id: i32,
        api_id: i32,
        api_hash: String,
        settings: Settings,
        settings_path: PathBuf,
        main_window: WinId,
    ) -> Self {
        let look_settings = settings.look;
        let slot = settings.active_account;
        Self {
            api_id,
            api_hash,
            main_window,
            settings,
            settings_path,
            plugin_host: None,
            plugin_infos: Vec::new(),
            plugin_logs: HashMap::new(),
            focused: None,
            system_mode: iced::theme::Mode::Dark,
            look: look::Look::new(look_settings, iced::theme::Mode::Dark),
            #[cfg(target_os = "linux")]
            tray: None,
            error: None,
            settings_load_failed: false,
            session: Session::new(client_id, main_window, slot),
        }
    }

    fn archive_collapsed(&self) -> bool {
        self.session
            .my_id
            .is_some_and(|id| self.settings.collapsed_archives.get(&id) == Some(&true))
    }

    /// Writes settings to disk, unless the file on disk failed to parse at
    /// start: overwriting it then would silently replace whatever is still
    /// recoverable in it (including `lock_salt`/`lock_check`) with fresh
    /// defaults, the moment anything else gets saved (a tab switch, a toggle).
    pub(crate) fn try_save_settings(&mut self) -> Result<(), String> {
        if self.settings_load_failed {
            return Err("настройки повреждены на диске, автосохранение отключено, \
                 чтобы не потерять пароль: почините или удалите settings.json"
                .to_owned());
        }
        self.settings.save(&self.settings_path)
    }

    /// Same as `try_save_settings`, reporting a failure through `self.error`
    /// instead of returning it: the common case, where the caller has
    /// nothing more specific to do about it.
    pub(crate) fn save_settings(&mut self) {
        if let Err(e) = self.try_save_settings() {
            self.error = Some(e);
        }
    }

    /// Schedules a save shortly from now instead of writing immediately: a
    /// burst of changes that each touch settings (clicking through several
    /// tabs, say) writes `settings.json` once, after the burst settles,
    /// rather than once per change. `Msg::FlushSettings` carries back the
    /// token handed out here; only the one matching the latest call still
    /// does the write, so an earlier scheduled flush superseded by a newer
    /// change becomes a no-op instead of a redundant write.
    pub(crate) fn save_settings_debounced(&mut self) -> Task<Msg> {
        self.session.settings_save_token = self.session.settings_save_token.wrapping_add(1);
        let token = self.session.settings_save_token;
        Task::perform(
            async move {
                tokio::time::sleep(std::time::Duration::from_millis(800)).await;
                token
            },
            Msg::FlushSettings,
        )
    }

    /// The archive, if saving deleted messages is enabled. Own deletions and
    /// "Убрать из архива" purge through `self.session.archive` regardless.
    fn active_archive(archive: &mut Option<Archive>, keep_deleted: bool) -> Option<&mut Archive> {
        archive.as_mut().filter(|_| keep_deleted)
    }

    pub(crate) fn subscription(&self) -> Subscription<Msg> {
        Subscription::batch([
            Subscription::run(td::updates).map(|(client_id, update)| Msg::Td(client_id, update)),
            iced::event::listen_with(|event, status, window| match event {
                // Ctrl+C a widget took (the input field copying its own
                // selection) is not also a copy of a message selection.
                iced::Event::Keyboard(keyboard::Event::KeyPressed { key, modifiers, .. })
                    if !(status == iced::event::Status::Captured
                        && modifiers.command()
                        && matches!(key.as_ref(), Key::Character("c" | "C" | "с" | "С"))) =>
                {
                    Some(Msg::Key(window, key, modifiers))
                }
                // The drag of a text selection ends wherever the button is let go.
                iced::Event::Mouse(iced::mouse::Event::ButtonReleased(
                    iced::mouse::Button::Left,
                )) => Some(Msg::MouseReleased),
                iced::Event::Window(window::Event::FileDropped(path)) => {
                    Some(Msg::FileDropped(window, path))
                }
                iced::Event::Window(window::Event::Focused) => Some(Msg::WindowFocus(window, true)),
                iced::Event::Window(window::Event::Unfocused) => {
                    Some(Msg::WindowFocus(window, false))
                }
                _ => None,
            }),
            window::close_events().map(Msg::WindowClosed),
            Subscription::run(plugins::run).map(Msg::Plugin),
            Subscription::run(notify::clicks)
                .map(|(slot, chat_id)| Msg::NotificationClicked(slot, chat_id)),
            iced::system::theme_changes().map(Msg::SystemTheme),
            #[cfg(target_os = "linux")]
            Subscription::run(tray::run).map(Msg::Tray),
            if self.session.video.is_some() {
                // The picture redraws itself (`VideoProgram`); this only
                // does low-rate upkeep: the position label and the
                // fallback cleanup in `on_video`'s `VideoMsg::Tick`.
                iced::time::every(std::time::Duration::from_millis(250))
                    .map(|_| Msg::Video(video::VideoMsg::Tick))
            } else {
                Subscription::none()
            },
            if self.session.typing.active() {
                iced::time::every(typing::FRAME).map(|_| Msg::TypingTick)
            } else {
                Subscription::none()
            },
            if self.session.playback.busy() {
                iced::time::every(std::time::Duration::from_millis(150)).map(|_| Msg::PlaybackTick)
            } else {
                Subscription::none()
            },
        ])
    }

    /// The same theme for every window: the system's, fixed at start and
    /// followed on changes.
    pub(crate) fn theme(&self, _window: WinId) -> Option<iced::Theme> {
        Some(self.look.theme.clone())
    }

    pub(crate) fn title(&self, window: WinId) -> String {
        let chat = self.session.panes.get(&window).and_then(|p| p.chat_id);
        match chat.and_then(|id| self.session.chats.get(&id)) {
            Some(chat) if window != self.main_window => format!("Telega — {}", chat.title),
            _ => "Telega".into(),
        }
    }

    pub(crate) fn update(&mut self, msg: Msg) -> Task<Msg> {
        match msg {
            Msg::Td(client_id, update) => {
                // Late updates of an already replaced client.
                if client_id != self.session.client_id {
                    return Task::none();
                }
                return self.on_update(*update);
            }
            Msg::Done(result) => {
                self.session.busy = false;
                if let Err(e) = result {
                    self.error = Some(e);
                }
            }
            Msg::ArchiveWindowLoaded(window, client, result) => {
                if client != self.session.client_id
                    || self.session.leave.is_some()
                    || self.session.logging_out
                    || self.session.auth != Auth::Ready
                    || !self.session.panes.contains_key(&window)
                {
                    return Task::none();
                }
                self.session.busy = false;
                if let Err(e) = result {
                    self.error = Some(e);
                }
            }
            Msg::DeleteDone(chat_id, ids, result) => {
                if let Err(e) = result {
                    // The deletion never happened: it must not count as the
                    // user's own if the message is deleted some other way
                    // later (else `on_deleted` would `forget` it instead of
                    // marking it deleted, losing the archived copy).
                    for id in ids {
                        self.session.self_deleted.remove(&(chat_id, id));
                    }
                    self.error = Some(e);
                }
            }
            Msg::Input(s) => self.session.input = s,
            Msg::SubmitAuth => return self.submit_auth(),
            Msg::BackToPhone => {
                if matches!(
                    self.session.auth,
                    Auth::Code | Auth::Password { .. } | Auth::Qr { .. }
                ) && !self.session.busy
                {
                    self.session.auth = Auth::Phone;
                    self.session.input.clear();
                    self.error = None;
                }
            }
            Msg::LogOut => self.session.confirm_logout = true,
            Msg::ConfirmLogOut(confirmed) => {
                self.session.confirm_logout = false;
                if confirmed {
                    self.session.auth = Auth::LoggingOut;
                    return Task::perform(td::log_out(self.session.client_id), Msg::Done);
                }
            }
            Msg::OpenSettings => {
                self.session.settings_open = true;
                if let Some(pane) = self.session.panes.get_mut(&self.main_window) {
                    pane.menu = None;
                    pane.list.archive_settings = None;
                }
            }
            Msg::CloseSettings => self.session.settings_open = false,
            Msg::SetKeepDeleted(keep) => return self.set_keep_deleted(keep),
            Msg::OpenChatWindow(chat_id) => {
                if chat_id.is_some() {
                    self.dismiss_chat_list_menu(self.main_window);
                }
                return self.open_chat_window(chat_id);
            }
            Msg::OpenArchiveWindow(source, client) => {
                if client != self.session.client_id
                    || !self.session.panes.contains_key(&source)
                    || !self.auth_ready()
                    || self.session.leave.is_some()
                {
                    return Task::none();
                }
                return self.open_archive_window();
            }
            Msg::WindowClosed(window) => return self.on_window_closed(window),
            Msg::Key(window, key, modifiers) => return self.on_key(window, key, modifiers),
            Msg::Pane(window, msg) => return self.on_pane(window, msg),
            Msg::Plugin(event) => return self.on_plugin(event),
            Msg::PluginDone(id, action, result) => {
                let line = match result {
                    Ok(line) => line,
                    Err(e) => {
                        if let (Some(secs), Some(host)) = (td::flood_wait(&e), &self.plugin_host) {
                            // The action itself would otherwise be lost: put
                            // it back at the head of the queue instead of
                            // only pausing future ones.
                            host.send(HostCmd::FloodWait(secs));
                            host.send(HostCmd::Requeue(id.clone(), action));
                        }
                        format!("ошибка: {e}")
                    }
                };
                self.plugin_log(id, line);
            }
            Msg::PluginDeleteOwn(plugin, chat_id, own, revoke, action) => {
                if own.is_empty() {
                    self.plugin_log(plugin, "нечего удалять: сообщения не ваши".into());
                    return Task::none();
                }
                // Deliberate deletions are not kept in the deleted archive.
                self.session
                    .self_deleted
                    .extend(own.iter().map(|&message| (chat_id, message)));
                let ids = own.clone();
                return Task::perform(
                    td::delete_messages(self.session.client_id, chat_id, own, revoke),
                    move |r| {
                        Msg::PluginDeleted(plugin.clone(), chat_id, ids.clone(), action.clone(), r)
                    },
                );
            }
            Msg::PluginDeleted(plugin, chat_id, ids, action, result) => {
                let line = match result {
                    Ok(()) => Ok(format!("удалено {} сообщ. в чате {chat_id}", ids.len())),
                    Err(e) => {
                        for id in &ids {
                            self.session.self_deleted.remove(&(chat_id, *id));
                        }
                        Err(e)
                    }
                };
                return self.update(Msg::PluginDone(plugin, action, line));
            }
            Msg::PluginToggle(id, on) => {
                // Enabling records which permissions the user agreed to.
                let granted: Vec<String> = self
                    .plugin_infos
                    .iter()
                    .find(|i| i.id == id)
                    .map(|i| i.permissions.iter().map(|p| p.name().to_owned()).collect())
                    .unwrap_or_default();
                self.plugin_config(&id, |c| {
                    c.enabled = on;
                    if on {
                        c.granted = granted;
                    }
                });
            }
            Msg::PluginDryRun(id, on) => self.plugin_config(&id, |c| c.dry_run = on),
            Msg::PluginSetting(id, key, value) => self.plugin_config(&id, |c| {
                c.values.insert(key, value);
            }),
            Msg::PluginNumberSetting(id, key, raw) => {
                let draft_key = (id.clone(), key.clone());
                if raw.is_empty() {
                    // Empty means "use the plugin's default", not zero: a
                    // zero delay/threshold can trigger destructive plugin
                    // actions immediately while the field is being edited.
                    self.session.plugin_number_drafts.remove(&draft_key);
                    self.plugin_config(&id, |c| {
                        c.values.remove(&key);
                    });
                } else {
                    self.session
                        .plugin_number_drafts
                        .insert(draft_key, raw.clone());
                    if let Ok(n) = raw.parse::<f64>()
                        && n.is_finite()
                        && n >= 0.0
                    {
                        self.plugin_config(&id, |c| {
                            c.values.insert(key, serde_json::json!(n));
                        });
                    }
                }
            }
            Msg::StopAllPlugins => {
                for config in self.settings.plugins.values_mut() {
                    config.enabled = false;
                }
                self.save_plugin_settings();
                self.plugin_log(String::new(), "все плагины остановлены".into());
            }
            Msg::OpenPluginHelp(open) => self.session.plugin_help_open = open,
            Msg::SelectTab(chat_id) => {
                self.session.settings_open = false;
                self.dismiss_chat_list_menu(self.main_window);
                return self.show_in_main(chat_id);
            }
            Msg::CloseTab(chat_id) => {
                self.dismiss_chat_list_menu(self.main_window);
                return self.close_tab(chat_id);
            }
            Msg::CloseAllTabs => {
                self.dismiss_chat_list_menu(self.main_window);
                return self.close_all_tabs();
            }
            Msg::TogglePin(chat_id) => return self.toggle_pin(chat_id),
            Msg::TabsWheel(delta) => return self.scroll_tab_bar(delta),
            Msg::FlushSettings(token) => {
                if token == self.session.settings_save_token {
                    self.save_settings();
                }
            }
            Msg::FileDownload(file_id) => {
                return Task::perform(
                    td::download_file(self.session.client_id, file_id, 24),
                    Msg::Done,
                );
            }
            Msg::FileCancel(file_id) => {
                return Task::perform(td::cancel_download(self.session.client_id, file_id), |()| {
                    Msg::Ignore
                });
            }
            Msg::FileOpen(file_id) => self.open_file(file_id, false),
            Msg::FileShowFolder(file_id) => self.open_file(file_id, true),
            Msg::AvatarDecoded(file_id, result) => self.avatar_decoded(file_id, result),
            Msg::AvatarShown(peer) => return self.avatar_shown(peer),
            Msg::ImageDecoded(file_id, result) => match result {
                Ok((w, h, rgba)) => {
                    self.session
                        .images
                        .insert(file_id, w, h, rgba, &self.session.wanted_photos)
                }
                Err(e) => {
                    self.session.images.decoding.remove(&file_id);
                    self.error = Some(e);
                }
            },
            Msg::FileDropped(window, path) => return self.send_files(window, vec![path]),
            Msg::Pasted(window, result) => match result {
                Ok(Some(path)) => return self.send_files(window, vec![path]),
                Ok(None) => {}
                Err(e) => self.error = Some(e),
            },
            Msg::WindowFocus(window, focused) => {
                if focused {
                    self.focused = Some(window);
                } else if self.focused == Some(window) {
                    self.focused = None;
                }
            }
            #[cfg(target_os = "linux")]
            Msg::Tray(event) => match event {
                tray::TrayEvent::Ready(handle) => {
                    let task = handle.show(self.badge());
                    self.tray = Some(handle);
                    return task;
                }
                tray::TrayEvent::Activate => {
                    return Task::batch([
                        tray::raise_on_hyprland(),
                        window::gain_focus(self.main_window),
                    ]);
                }
                tray::TrayEvent::Quit => return iced::exit(),
            },
            Msg::Password(msg) => return self.on_password(msg),
            // A failure (no rights to list admins, etc.) just leaves the
            // tags carried by messages.
            Msg::RolesLoaded(chat_id, result) => {
                if let Ok(roles) = result {
                    self.session.roles.insert(chat_id, roles);
                }
            }
            Msg::PinnedRefreshed(chat_id, result) => {
                if let Ok(messages) = result {
                    let pinned: Vec<MsgItem> = messages.iter().map(MsgItem::from).collect();
                    for pane in self.panes_of(chat_id) {
                        let current = pane.pins().get(pane.pinned_shown).map(|(m, _)| m.id);
                        pane.pinned = pinned.clone();
                        pane.pinned_shown = current
                            .and_then(|id| pane.pins().iter().position(|(m, _)| m.id == id))
                            .unwrap_or(0);
                    }
                }
            }
            Msg::ParametersSet(result) => {
                self.session.busy = false;
                if let Err(e) = result {
                    if self.session.db_key.is_some() && e.to_lowercase().contains("encryption") {
                        // The check value passed but TDLib disagrees: the data
                        // was written with another key.
                        self.session.db_key = None;
                        self.session.auth = Auth::Locked {
                            error: Some("база зашифрована другим паролем".into()),
                            forgot: false,
                        };
                    } else {
                        self.error = Some(e);
                    }
                }
            }
            Msg::Video(msg) => return self.on_video(msg),
            Msg::Card(msg) => return self.on_card(msg),
            Msg::ClearNotice => self.session.notice = None,
            Msg::LinkResolved(window, url, result) => {
                return self.link_resolved(window, url, result);
            }
            Msg::Joined(window, result) => match result {
                Ok(Some(chat_id)) => return self.select_chat(window, chat_id),
                Ok(None) => self.error = Some("заявка на вступление отправлена".into()),
                Err(e) => self.error = Some(e),
            },
            Msg::MouseReleased => {
                for pane in self.session.panes.values_mut() {
                    if let Some(sel) = &mut pane.text_selection {
                        sel.dragging = false;
                    }
                }
            }
            Msg::ViewPhoto(window, file_id) => return self.view_photo(window, file_id),
            Msg::RecentStickers(result) => match result {
                Ok(stickers) => {
                    self.note_stickers(&stickers);
                    self.session.stickers.recent = Some(stickers);
                }
                Err(e) => {
                    self.error = Some(e);
                    return self.retry_recent_stickers();
                }
            },
            Msg::StickerSets(result) => match result {
                Ok(sets) => self.session.stickers.sets = Some(sets),
                Err(e) => {
                    self.error = Some(e);
                    return self.retry_sticker_sets();
                }
            },
            Msg::StickerSet(id, result) => match result {
                Ok(stickers) => {
                    self.note_stickers(&stickers);
                    self.session.stickers.set_stickers.insert(id, stickers);
                }
                Err(e) => {
                    self.error = Some(e);
                    return self.retry_sticker_set(id);
                }
            },
            Msg::ViewerDecoded(file_id, result) => self.viewer_decoded(file_id, result),
            Msg::ViewerStep(forward) => return self.viewer_step(forward),
            Msg::CloseViewer => self.session.photo_view = None,
            Msg::ToggleProfile(window) => {
                let Some(pane) = self.session.panes.get_mut(&window) else {
                    return Task::none();
                };
                if pane.profile.take().is_some() {
                    return Task::none();
                }
                let Some(chat_id) = pane.chat_id else {
                    return Task::none();
                };
                return self.open_profile(window, chat_id);
            }
            Msg::OpenProfile(window, chat_id) => return self.open_profile(window, chat_id),
            Msg::Profile(window, action) => return self.profile_action(window, action),
            Msg::ProfileLoaded(window, chat_id, result) => {
                if let Some(pane) = self.session.panes.get_mut(&window)
                    && let Some((shown, data)) = &mut pane.profile
                    && *shown == chat_id
                {
                    *data = Some(result);
                }
            }
            Msg::ShowFolder(window, folder) => {
                if let Some(pane) = self.session.panes.get_mut(&window) {
                    pane.list.folder = folder;
                    pane.list.archive = false;
                    pane.list.menu = None;
                    pane.list.folder_picker = None;
                    pane.list.new_folder_name = None;
                    pane.list.new_folder_error = None;
                    pane.list.new_folder_request = None;
                    pane.list.confirm_read = None;
                    pane.list.archive_settings = None;
                    pane.list.read_feedback = None;
                } else {
                    return Task::none();
                }
                let query = self.session.panes[&window].list.query.clone();
                let reset = self.list_query(window, query);
                if let Some(id) = folder {
                    let list =
                        ChatList::Folder(tdlib_rs::types::ChatListFolder { chat_folder_id: id });
                    return Task::batch([
                        reset,
                        Task::perform(td::load_list(self.session.client_id, list, 100), Msg::Done),
                    ]);
                }
                return reset;
            }
            Msg::ReorderFolder(window, folder, up) => {
                return self.reorder_folder(window, folder, up);
            }
            Msg::FolderReordered(window, client, request, result) => {
                let Some(pending) = self.session.folder_reorder.as_mut() else {
                    return Task::none();
                };
                if client != self.session.client_id
                    || pending.window != window
                    || pending.request != request
                {
                    return Task::none();
                }
                match result {
                    Ok(()) if pending.state == FolderReorderState::MatchedBeforeReply => {
                        self.session.folder_reorder = None;
                        self.session.folder_reorder_error = None;
                    }
                    Ok(()) => {
                        pending.state = match pending.state {
                            FolderReorderState::DiscrepancyBeforeReply => {
                                FolderReorderState::DiscrepancyAfterReply
                            }
                            FolderReorderState::AwaitingReply => FolderReorderState::AwaitingUpdate,
                            state => state,
                        };
                        if pending.state == FolderReorderState::AwaitingUpdate {
                            self.session.folder_reorder_error =
                                Some(FOLDER_REORDER_UNCONFIRMED.into());
                        }
                    }
                    Err(e) => {
                        self.session.folder_reorder = None;
                        if self.session.panes.contains_key(&window) {
                            self.session.folder_reorder_error = Some(format!("Порядок папок: {e}"));
                        }
                    }
                }
            }
            Msg::AcknowledgeFolderReorder(window, client, request) => {
                let Some(pending) = self.session.folder_reorder.as_ref() else {
                    return Task::none();
                };
                if client != self.session.client_id
                    || pending.request != request
                    || !self.session.panes.contains_key(&window)
                    || !matches!(
                        pending.state,
                        FolderReorderState::AwaitingUpdate
                            | FolderReorderState::DiscrepancyAfterReply
                    )
                {
                    return Task::none();
                }
                self.session.folder_reorder = None;
                self.session.folder_reorder_error = Some(FOLDER_REORDER_UNCONFIRMED.into());
            }
            Msg::FolderName(window, chat_id, name) => {
                if let Some(pane) = self.session.panes.get_mut(&window)
                    && pane.list.menu == Some(chat_id)
                    && pane.list.new_folder_name.is_some()
                {
                    pane.list.new_folder_name = Some(name);
                    pane.list.new_folder_error = None;
                }
            }
            Msg::ConfirmNewFolder(window, chat_id) => {
                return self.confirm_new_folder(window, chat_id);
            }
            Msg::FolderCreated(window, client, request, result) => {
                let Some(pending) = self.session.folder_creation.as_mut() else {
                    return Task::none();
                };
                if client != self.session.client_id
                    || pending.window != window
                    || pending.request != request
                {
                    return Task::none();
                }
                match result {
                    Ok(id) => {
                        if pending.observed.contains(&id) {
                            let chat_id = pending.chat_id;
                            self.session.folder_creation = None;
                            self.session.folder_creation_warning = None;
                            if let Some(pane) = self.session.panes.get_mut(&window)
                                && pane.list.menu == Some(chat_id)
                                && pane.list.new_folder_request == Some(request)
                            {
                                pane.list.new_folder_name = None;
                                pane.list.new_folder_request = None;
                            }
                        } else {
                            pending.id = Some(id);
                            self.session.folder_creation_warning =
                                Some(FOLDER_CREATE_UNCONFIRMED.into());
                        }
                    }
                    Err(error) => {
                        let chat_id = pending.chat_id;
                        self.session.folder_creation = None;
                        if let Some(pane) = self.session.panes.get_mut(&window)
                            && pane.list.menu == Some(chat_id)
                            && pane.list.new_folder_request == Some(request)
                        {
                            pane.list.new_folder_error = Some(error);
                        }
                    }
                }
            }
            Msg::AcknowledgeFolderCreation(window, client, request) => {
                if client == self.session.client_id
                    && self.session.panes.contains_key(&window)
                    && self
                        .session
                        .folder_creation
                        .as_ref()
                        .is_some_and(|pending| pending.request == request && pending.id.is_some())
                {
                    self.session.folder_creation = None;
                    self.session.folder_creation_warning = Some(FOLDER_CREATE_UNCONFIRMED.into());
                }
            }
            Msg::SetArchiveCollapsed(window, client, collapsed) => {
                if client != self.session.client_id
                    || !self.auth_ready()
                    || self.session.leave.is_some()
                    || self.session.logging_out
                {
                    return Task::none();
                }
                let Some(pane) = self.session.panes.get(&window) else {
                    return Task::none();
                };
                if !pane.list.archive
                    && (window != self.main_window
                        || if collapsed {
                            pane.list.folder.is_some() || !pane.list.query.trim().is_empty()
                        } else {
                            !self.session.accounts_open
                        })
                {
                    return Task::none();
                }
                let Some(id) = self.session.my_id else {
                    return Task::none();
                };
                if self.archive_collapsed() == collapsed {
                    return Task::none();
                }
                if collapsed {
                    self.settings.collapsed_archives.insert(id, true);
                } else {
                    self.settings.collapsed_archives.remove(&id);
                }
                self.save_settings();
                if !collapsed && window == self.main_window {
                    self.session.accounts_open = false;
                }
            }
            Msg::OpenArchiveFromMenu(client) => {
                if client != self.session.client_id
                    || !self.auth_ready()
                    || self.session.leave.is_some()
                    || !self.session.accounts_open
                    || !self.archive_collapsed()
                {
                    return Task::none();
                }
                return self.update(Msg::ShowArchive(self.main_window, true));
            }
            Msg::ShowArchive(window, on) => {
                if let Some(pane) = self.session.panes.get_mut(&window) {
                    if on && window == self.main_window {
                        self.session.accounts_open = false;
                    }
                    pane.list.archive = on;
                    pane.list.menu = None;
                    pane.list.folder_picker = None;
                    pane.list.confirm_read = None;
                    pane.list.read_feedback = None;
                    pane.list.new_folder_name = None;
                    pane.list.new_folder_error = None;
                    pane.list.new_folder_request = None;
                    if !on {
                        pane.list.archive_settings = None;
                    }
                } else {
                    return Task::none();
                }
                let query = self.session.panes[&window].list.query.clone();
                let reset = self.list_query(window, query);
                if on {
                    return Task::batch([
                        reset,
                        Task::perform(
                            td::load_list(self.session.client_id, ChatList::Archive, 100),
                            Msg::Done,
                        ),
                    ]);
                }
                return reset;
            }
            Msg::ToggleArchiveSettings(window, true) => return self.open_archive_settings(window),
            Msg::ToggleArchiveSettings(window, false) => {
                if let Some(pane) = self.session.panes.get_mut(&window) {
                    pane.list.archive_settings = None;
                }
            }
            Msg::RetryArchiveSettings(window) => return self.read_archive_settings(window),
            Msg::ArchiveSettingsLoaded(window, client, request, result) => {
                // A read started before another panel's write may finish after
                // the save but before its authoritative readback.
                if self.session.archive_write.is_some() {
                    return Task::none();
                }
                if let Some(panel) = self.archive_panel_mut(window, client, request) {
                    panel.loading = false;
                    match result {
                        Ok(settings) => {
                            panel.settings = Some(settings);
                            panel.error = None;
                        }
                        Err(error) => {
                            panel.error = Some(format!("Не удалось загрузить настройки: {error}"))
                        }
                    }
                }
            }
            Msg::ArchiveCapabilityLoaded(window, client, request, result) => {
                if let Some(panel) = self.archive_panel_mut(window, client, request) {
                    panel.capability = Some(result);
                }
            }
            Msg::ChangeArchiveFlag(window, flag, value) => {
                return self.change_archive_flag(window, flag, value);
            }
            Msg::ArchiveFetched(window, client, request, flag, value, result) => {
                return self.archive_fetched(window, client, request, flag, value, result);
            }
            Msg::ArchiveSaved(window, client, request, result) => {
                return self.archive_saved(window, client, request, result);
            }
            Msg::ArchiveReadback(window, client, request, result) => {
                return self.archive_readback(window, client, request, result);
            }
            Msg::AskReadList(window, client) => {
                if client != self.session.client_id
                    || !self.auth_ready()
                    || self.session.leave.is_some()
                {
                    return Task::none();
                }
                let Some(list) = self.current_read_list(window) else {
                    return Task::none();
                };
                if self.session.list_reads.iter().any(|read| read.list == list) {
                    return Task::none();
                }
                self.session.next_list_read = self.session.next_list_read.wrapping_add(1);
                let request = self.session.next_list_read;
                let pane = &mut self.session.panes.get_mut(&window).unwrap().list;
                pane.confirm_read = Some((request, list));
                pane.read_feedback = None;
            }
            Msg::ConfirmReadList(window, client, request, confirmed) => {
                if client != self.session.client_id
                    || !self.auth_ready()
                    || self.session.leave.is_some()
                {
                    return Task::none();
                }
                let selected = self.current_read_list(window);
                let Some(pane) = self.session.panes.get_mut(&window) else {
                    return Task::none();
                };
                let Some((token, list)) = pane.list.confirm_read.as_ref() else {
                    return Task::none();
                };
                if *token != request {
                    return Task::none();
                }
                let list = list.clone();
                pane.list.confirm_read = None;
                if !confirmed
                    || selected.as_ref() != Some(&list)
                    || self.session.list_reads.iter().any(|read| read.list == list)
                {
                    return Task::none();
                }
                // Any confirmation opened in another window before this send
                // is now stale, even if the request finishes before it is clicked.
                for pane in self.session.panes.values_mut() {
                    if pane
                        .list
                        .confirm_read
                        .as_ref()
                        .is_some_and(|(_, pending)| *pending == list)
                    {
                        pane.list.confirm_read = None;
                    }
                }
                self.session.list_reads.push(ListRead {
                    list: list.clone(),
                    request,
                    window,
                });
                return Task::perform(td::read_chat_list(client, list), move |result| {
                    Msg::ListReadDone(window, client, request, result)
                });
            }
            Msg::ListReadDone(window, client, request, result) => {
                if client != self.session.client_id {
                    return Task::none();
                }
                let Some(index) = self
                    .session
                    .list_reads
                    .iter()
                    .position(|read| read.request == request && read.window == window)
                else {
                    return Task::none();
                };
                let list = self.session.list_reads.remove(index).list;
                if self.current_read_list(window).as_ref() == Some(&list)
                    && let Some(pane) = self.session.panes.get_mut(&window)
                {
                    pane.list.read_feedback = Some(result);
                }
            }
            Msg::ChatMenu(window, chat) => {
                if let Some(pane) = self.session.panes.get_mut(&window) {
                    pane.list.menu = chat.filter(|id| self.session.chats.contains_key(id));
                    pane.list.folder_picker = None;
                    pane.list.new_folder_name = None;
                    pane.list.new_folder_error = None;
                    pane.list.new_folder_request = None;
                }
            }
            Msg::ChatOp(window, chat_id, op) => {
                let Some(pane) = self.session.panes.get_mut(&window) else {
                    return Task::none();
                };
                if pane.list.menu != Some(chat_id) {
                    return Task::none();
                }
                if op == ChatOp::Folders {
                    if pane.list.folder_picker.is_some()
                        || self.session.folders.is_empty()
                        || !self.session.chats.contains_key(&chat_id)
                    {
                        return Task::none();
                    }
                    pane.list.folder_picker = Some(pane::FolderPicker {
                        chat_id,
                        pending: None,
                        feedback: None,
                    });
                    return Task::none();
                }
                if op == ChatOp::NewFolder {
                    if !self.session.chats.contains_key(&chat_id) {
                        return Task::none();
                    }
                    pane.list.folder_picker = None;
                    pane.list.new_folder_name = Some(String::new());
                    pane.list.new_folder_error = None;
                    pane.list.new_folder_request = None;
                    return Task::none();
                }
                pane.list.menu = None;
                pane.list.folder_picker = None;
                pane.list.new_folder_name = None;
                pane.list.new_folder_error = None;
                pane.list.new_folder_request = None;
                if let ChatOp::LocalPin(on) = op {
                    return self.local_chat_pin(window, chat_id, on);
                }
                return self.chat_op(chat_id, op);
            }
            Msg::FolderAction(window, chat_id, folder, include) => {
                return self.folder_action(window, chat_id, folder, include);
            }
            Msg::FolderEdited(window, client, chat_id, folder, request, result) => {
                if client != self.session.client_id
                    || self.session.folder_edits.get(&folder) != Some(&request)
                {
                    return Task::none();
                }
                self.session.folder_edits.remove(&folder);
                if !self.session.folders.iter().any(|(id, _)| *id == folder)
                    || !self.session.chats.contains_key(&chat_id)
                {
                    return Task::none();
                }
                if let Some(pane) = self.session.panes.get_mut(&window)
                    && pane.list.menu == Some(chat_id)
                    && let Some(picker) = &mut pane.list.folder_picker
                    && picker.chat_id == chat_id
                    && picker.pending == Some((request, folder))
                {
                    picker.pending = None;
                    picker.feedback = Some(match result {
                        Ok(true) => "Сохранено. Список обновит Telegram.".into(),
                        Ok(false) => "Состав папки уже соответствует выбранному действию.".into(),
                        Err(error) => error,
                    });
                }
            }
            Msg::LoginQr => {
                if self.session.auth == Auth::Phone && !self.session.busy {
                    self.session.busy = true;
                    self.error = None;
                    return Task::perform(td::request_qr(self.session.client_id), Msg::Done);
                }
            }
            Msg::TypingTick => self.session.typing.tick(std::time::Instant::now()),
            Msg::SetLook(choice) => {
                self.settings.look = choice;
                self.look = look::Look::new(choice, self.system_mode);
                self.save_settings();
            }
            Msg::ToggleAccounts => self.session.accounts_open = !self.session.accounts_open,
            Msg::SwitchAccount(slot) => return self.switch_account(slot),
            Msg::AddAccount => return self.add_account(),
            Msg::CancelAddAccount => return self.cancel_add_account(),
            Msg::SystemTheme(mode) => {
                // iced reports "no preference" whenever a new window opens
                // (Wayland windows have no theme of their own); that is not
                // a change of the system setting, so it is ignored.
                if mode != iced::theme::Mode::None {
                    self.system_mode = mode;
                    self.look = look::Look::new(self.settings.look, mode);
                }
            }
            Msg::NotificationClicked(slot, chat_id) => {
                // A click on a stale popup left over from a previous
                // account's session: that account is no longer running.
                if slot != self.session.slot {
                    return Task::none();
                }
                self.dismiss_chat_list_menu(self.main_window);
                let main = self.main_window;
                #[cfg(target_os = "linux")]
                let raise = tray::raise_on_hyprland();
                #[cfg(not(target_os = "linux"))]
                let raise = Task::none();
                return Task::batch([self.show_in_main(chat_id), window::gain_focus(main), raise]);
            }
            Msg::SetNotifications(on) => {
                self.settings.notifications = on;
                self.save_settings();
            }
            Msg::RepliesLoaded(messages) => {
                self.store_replies(messages);
                self.bound_reply_previews();
            }
            Msg::ListQuery(window, query) => return self.list_query(window, query),
            Msg::ListChats(window, query, result) => {
                if let (Ok(ids), Some(pane)) = (result, self.session.panes.get_mut(&window))
                    && pane.list.query == query
                    && !pane.list.archive
                {
                    pane.list.chats = ids;
                }
            }
            Msg::ListSearchMessages(window) => return self.list_search_messages(window),
            Msg::ListMore(window) => return self.list_more(window),
            Msg::ListMessages(window, query, request, cursor, result) => {
                self.list_results(window, &query, request, cursor, result);
            }
            Msg::OpenFound(window, chat_id, id) => return self.open_found(window, chat_id, id),
            Msg::VoiceToggle(file_id) => return self.voice_toggle(file_id),
            Msg::VoiceDecoded(file_id, result) => match result {
                Ok(pcm) => return self.voice_decoded(file_id, pcm.0),
                Err(e) => self.error = Some(e),
            },
            Msg::PlaybackTick => return self.playback_tick(),
            Msg::AnimShow(file_id, motion) => return self.animation_shown(file_id, motion),
            Msg::AnimHide(file_id) => self.animation_hidden(file_id),
            Msg::AnimFrame(file_id, w, h, rgba) => self.animation_frame(file_id, w, h, rgba),
            Msg::AnimEnded(file_id) => self.animation_ended(file_id),
            Msg::PlayVideo(file_id) => return self.play_video(file_id),
            Msg::RecordStart(window) => return self.record_start(window),
            Msg::RecordStarted(window, chat_id, result) => match result {
                Ok(recorder) => self.record_started(window, chat_id, recorder.0),
                Err(e) => self.error = Some(e),
            },
            Msg::RecordStop(send) => return self.record_finish(send),
            Msg::Ignore => {}
        }
        Task::none()
    }

    fn on_key(&mut self, window: WinId, key: Key, modifiers: Modifiers) -> Task<Msg> {
        if self
            .session
            .user_card
            .as_ref()
            .is_some_and(|c| c.window == window)
            && key == Key::Named(keyboard::key::Named::Escape)
        {
            self.session.user_card = None;
            return Task::none();
        }
        // The expanded video takes the keys while open.
        if let Some(video) = self
            .session
            .video
            .as_ref()
            .filter(|v| v.window == window && v.expanded)
        {
            let step = std::time::Duration::from_secs(5);
            let position = video.player.position();
            match key.as_ref() {
                Key::Named(keyboard::key::Named::Escape) => {
                    return self.on_video(video::VideoMsg::ToggleExpanded);
                }
                Key::Named(keyboard::key::Named::Space) => {
                    return self.on_video(video::VideoMsg::TogglePause);
                }
                Key::Named(keyboard::key::Named::ArrowLeft) => {
                    video.player.seek(position.saturating_sub(step));
                }
                Key::Named(keyboard::key::Named::ArrowRight) => video.player.seek(position + step),
                Key::Character("m" | "M" | "ь" | "Ь") => {
                    return self.on_video(video::VideoMsg::ToggleMute);
                }
                _ => {}
            }
            return Task::none();
        }
        // The photo viewer takes the keys while open.
        if self
            .session
            .photo_view
            .as_ref()
            .is_some_and(|v| v.window == window)
        {
            match key.as_ref() {
                Key::Named(keyboard::key::Named::Escape) => self.session.photo_view = None,
                Key::Named(keyboard::key::Named::ArrowLeft) => return self.viewer_step(false),
                Key::Named(keyboard::key::Named::ArrowRight) => return self.viewer_step(true),
                _ => {}
            }
            return Task::none();
        }
        match key.as_ref() {
            Key::Named(keyboard::key::Named::Escape) => {
                // Esc closes the innermost thing: menu, reply, edit, then search.
                if let Some(pane) = self.session.panes.get_mut(&window)
                    && pane.list.new_folder_name.is_some()
                {
                    pane.list.new_folder_request = None;
                }
                if let Some(pane) = self.session.panes.get_mut(&window)
                    && pane.menu.take().is_none()
                    && pane.list.folder_picker.take().is_none()
                    && pane.list.new_folder_name.take().is_none()
                    && pane.forward.take().is_none()
                    && pane.reply_to.take().is_none()
                {
                    if pane.selected.is_some() {
                        pane.selected = None;
                        pane.delete_selection = None;
                    } else if pane.editing.is_some() {
                        pane.finish_edit();
                    } else {
                        pane.search = None;
                    }
                }
                if window == self.main_window {
                    self.session.confirm_logout = false;
                }
            }
            // Ctrl+V: an image in the clipboard is sent as a photo; text
            // pasting into the input field is untouched.
            Key::Character("c" | "C" | "с" | "С") if modifiers.command() => {
                if let Some(text) = self
                    .session
                    .panes
                    .get(&window)
                    .and_then(ChatPane::selected_text)
                {
                    return iced::clipboard::write(text);
                }
            }
            Key::Character("v" | "V" | "м" | "М") if modifiers.command() && self.auth_ready() => {
                return Task::perform(
                    async {
                        tokio::task::spawn_blocking(media::paste_image)
                            .await
                            .map_err(|e| e.to_string())
                            .and_then(|r| r)
                    },
                    move |r| Msg::Pasted(window, r),
                );
            }
            // Ctrl+N (Cmd+N on macOS); "т" is the same key on the Russian layout.
            Key::Character("n" | "т" | "N" | "Т")
                if modifiers.command() && self.session.auth == Auth::Ready =>
            {
                let chat = self.session.panes.get(&window).and_then(|p| p.chat_id);
                return self.open_chat_window(chat);
            }
            _ if window == self.main_window && modifiers.command() && self.auth_ready() => {
                return self.on_tab_key(key.as_ref(), modifiers);
            }
            _ => {}
        }
        Task::none()
    }

    /// Browser tab shortcuts in the main window.
    fn on_tab_key(&mut self, key: Key<&str>, modifiers: Modifiers) -> Task<Msg> {
        self.dismiss_chat_list_menu(self.main_window);
        match key {
            Key::Named(keyboard::key::Named::Tab) => self.cycle_tab(!modifiers.shift()),
            // "ц" is the same key on the Russian layout.
            Key::Character("w" | "W" | "ц" | "Ц") => {
                match self.session.panes[&self.main_window].chat_id {
                    Some(chat_id) => self.close_tab(chat_id),
                    None => Task::none(),
                }
            }
            Key::Character(c) => match c.parse::<usize>() {
                Ok(n @ 1..=9) => self.tab_by_number(n),
                _ => Task::none(),
            },
            _ => Task::none(),
        }
    }

    fn auth_ready(&self) -> bool {
        self.session.auth == Auth::Ready
    }

    /// The selected TDLib list, not the locally loaded chat rows.
    fn current_read_list(&self, window: WinId) -> Option<ChatList> {
        let list = &self.session.panes.get(&window)?.list;
        if list.archive {
            Some(ChatList::Archive)
        } else if let Some(id) = list.folder {
            self.session
                .folders
                .iter()
                .any(|(folder, _)| *folder == id)
                .then_some(ChatList::Folder(tdlib_rs::types::ChatListFolder {
                    chat_folder_id: id,
                }))
        } else {
            Some(ChatList::Main)
        }
    }

    fn open_chat_window(&mut self, chat_id: Option<i64>) -> Task<Msg> {
        let (window, open) = window::open(window_settings(760.0));
        self.session.panes.insert(window, ChatPane::default());
        let select = match chat_id {
            Some(chat_id) => Task::batch([
                // Moves the pane out of the main window and balances its
                // open reference first: `select_chat` below opens `chat_id`
                // again for the new window regardless of it being open
                // elsewhere, so by the time it runs no pane must still be
                // counted as showing the chat, or the balancing close here
                // would see it as still in use and never fire, leaking one
                // `openChat` for the rest of the session.
                self.main_chat_moved_to_window(chat_id),
                self.select_chat(window, chat_id),
            ]),
            None => Task::none(),
        };
        Task::batch([open.discard(), select])
    }

    fn open_archive_window(&mut self) -> Task<Msg> {
        let (window, open) = window::open(window_settings(760.0));
        let mut pane = ChatPane::default();
        pane.list.archive = true;
        self.session.panes.insert(window, pane);
        let client = self.session.client_id;
        Task::batch([
            open.discard(),
            Task::perform(
                td::load_list(client, ChatList::Archive, 100),
                move |result| Msg::ArchiveWindowLoaded(window, client, result),
            ),
        ])
    }

    fn on_window_closed(&mut self, window: WinId) -> Task<Msg> {
        if window == self.main_window {
            return iced::exit();
        }
        self.close_video_in(window);
        let cancel = self.cancel_recording_in(window);
        let Some(pane) = self.session.panes.remove(&window) else {
            return cancel;
        };
        match pane.chat_id {
            Some(chat_id) => {
                let closed = self.chat_window_closed(chat_id);
                cancel.chain(Task::batch([
                    closed,
                    self.sync_draft(chat_id, pane.draft()),
                    self.close_chat_if_unused(chat_id),
                ]))
            }
            None => cancel,
        }
    }

    /// TDLib keeps a per-chat open counter, not a flag: `openChat` increments
    /// it and `closeChat` decrements it. `history_around` and
    /// `back_to_latest` no longer call `openChat` for a chat a pane already
    /// opened, so exactly one `openChat` call needs balancing here, when no
    /// pane shows the chat anymore.
    fn close_chat_if_unused(&self, chat_id: i64) -> Task<Msg> {
        if self.session.panes.values().any(|p| p.shows(chat_id)) {
            return Task::none();
        }
        Task::perform(td::close_chat(self.session.client_id, chat_id), |()| {
            Msg::Ignore
        })
    }

    fn on_pane(&mut self, window: WinId, msg: PaneMsg) -> Task<Msg> {
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
            PaneMsg::Link(Link::Copy(text)) => return self.copy_notice(text),
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

    /// Panes showing the chat.
    /// Own pins changed: every pane of the chat shows the new list; `show`
    /// makes the bar show that message.
    fn refresh_local_pins(&mut self, chat_id: i64, show: Option<i64>) {
        let pins = self
            .session
            .archive
            .as_ref()
            .and_then(|a| a.local_pins(chat_id).ok())
            .unwrap_or_default();
        for pane in self.panes_of(chat_id) {
            pane.local_pins = pins.clone();
            pane.pinned_shown = show
                .and_then(|id| pane.pins().iter().position(|(m, _)| m.id == id))
                .unwrap_or(0);
        }
    }

    /// Every pane in every window, main or separate, plus every background
    /// tab: the one helper for updates that must reach hidden tabs too.
    fn all_panes_mut(&mut self) -> impl Iterator<Item = &mut ChatPane> {
        self.session
            .panes
            .values_mut()
            .chain(self.session.background.values_mut())
    }

    fn panes_of(&mut self, chat_id: i64) -> impl Iterator<Item = &mut ChatPane> {
        self.all_panes_mut().filter(move |p| p.shows(chat_id))
    }

    /// Keeps the reply-preview cache from growing forever: a preview is
    /// only useful while its chat is shown somewhere, so entries for chats
    /// that closed since (or were never opened, before the fetch answered)
    /// are dropped.
    fn bound_reply_previews(&mut self) {
        let shown: HashSet<i64> = self
            .session
            .panes
            .values()
            .chain(self.session.background.values())
            .filter_map(|p| p.chat_id)
            .collect();
        self.session
            .reply_previews
            .retain(|(chat_id, _), _| shown.contains(chat_id));
    }

    /// A pin-list read must not prevent the already-open message archive
    /// from saving messages. Neither list is usable if either read fails.
    fn attach_archive(&mut self, mut archive: Archive) {
        archive.set_key(self.session.db_key);
        let pins = (
            archive.local_chat_pins(false),
            archive.local_chat_pins(true),
        );
        self.session.archive = Some(archive);
        match pins {
            (Ok(main), Ok(archived)) => {
                self.session.local_main = main;
                self.session.local_archive = archived;
                self.session.local_chat_pins_failed = false;
            }
            (Err(e), _) | (_, Err(e)) => {
                self.session.local_main.clear();
                self.session.local_archive.clear();
                self.session.local_chat_pins_failed = true;
                self.error = Some(format!("закрепы: {e}"));
            }
        }
    }

    fn on_update(&mut self, update: Update) -> Task<Msg> {
        match update {
            Update::AuthorizationState(u) => return self.on_auth_state(u.authorization_state),
            Update::Option(u) => {
                if u.name == "my_id"
                    && let OptionValue::Integer(v) = u.value
                {
                    self.session.my_id = Some(v.value);
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
                return self.set_avatar(
                    avatars::Peer::Chat(chat.id),
                    chat.photo.as_ref().map(|p| &p.small),
                );
            }
            Update::ChatPhoto(u) => {
                return self.set_avatar(
                    avatars::Peer::Chat(u.chat_id),
                    u.photo.as_ref().map(|p| &p.small),
                );
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
                return Task::perform(
                    td::pinned_messages(self.session.client_id, u.chat_id),
                    move |r| Msg::PinnedRefreshed(u.chat_id, r),
                );
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
                let name = format!("{} {}", user.first_name, user.last_name);
                self.session.users.insert(user.id, name.trim().to_owned());
                self.user_renamed(user.id);
                return self.set_avatar(
                    avatars::Peer::User(user.id),
                    user.profile_photo.as_ref().map(|p| &p.small),
                );
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
                // A chat shown nowhere never displays this message, so its
                // reply (if any) does not need a preview either.
                let mut shown = false;
                for pane in self.panes_of(m.chat_id) {
                    shown = true;
                    // After a jump the newest history is not loaded yet; the
                    // message arrives with the newer pages instead.
                    if pane.newest_loaded && !pane.messages.iter().any(|x| x.id == m.id) {
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
                if let Some(archive) =
                    Self::active_archive(&mut self.session.archive, self.settings.keep_deleted)
                    && let Err(e) = archive.set_text(u.chat_id, u.message_id, &text)
                {
                    self.error = Some(format!("архив: {e}"));
                }
                let new_media = media::media_of(&u.new_content).map(|(m, _)| m);
                let new_extra = extra::extra_of(&u.new_content).map(|(e, _)| Box::new(e));
                let new_rich = rich_of(&u.new_content, new_extra.as_deref());
                for pane in self.panes_of(u.chat_id) {
                    if let Some(item) = pane.find_mut(u.message_id) {
                        item.media = new_media.clone();
                        item.text = text.clone();
                        item.rich = new_rich.clone();
                        item.extra = new_extra.clone();
                    }
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
                    if let Some(item) = pane.find_mut(u.message_id) {
                        item.reactions = reactions.clone();
                    }
                }
            }
            Update::MessageEdited(u) => {
                for pane in self.panes_of(u.chat_id) {
                    if let Some(item) = pane.find_mut(u.message_id) {
                        item.edited = u.edit_date > 0;
                    }
                }
            }
            // `from_cache` = only evicted from TDLib's cache, not really deleted.
            Update::File(u) => return self.on_file(&u.file),
            Update::NotificationGroup(u) => return self.on_notification_group(&u),
            Update::DeleteMessages(u) if u.is_permanent && !u.from_cache => {
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

    fn on_auth_state(&mut self, state: AuthorizationState) -> Task<Msg> {
        self.session.busy = false;
        self.session.input.clear();
        self.session.auth = match state {
            AuthorizationState::WaitTdlibParameters => {
                if self.locked_slot().is_some() && self.session.db_key.is_none() {
                    Auth::Locked {
                        error: None,
                        forgot: false,
                    }
                } else {
                    return self.set_parameters();
                }
            }
            AuthorizationState::WaitPhoneNumber => Auth::Phone,
            AuthorizationState::WaitCode(_) => Auth::Code,
            // TDLib renews the link every half minute; each renewal comes here.
            AuthorizationState::WaitOtherDeviceConfirmation(s) => Auth::Qr {
                image: qr::image(&s.link),
                link: s.link,
            },
            AuthorizationState::WaitPassword(p) => Auth::Password {
                hint: p.password_hint,
            },
            AuthorizationState::Ready => {
                self.session.auth = Auth::Ready;

                self.announce_account();
                return Task::batch([
                    Task::perform(td::load_chats(self.session.client_id, 200), Msg::Done),
                    Task::perform(td::enable_notifications(self.session.client_id), |()| {
                        Msg::Ignore
                    }),
                    self.restore_tabs(),
                ]);
            }
            AuthorizationState::LoggingOut => {
                self.session.logging_out = true;
                Auth::LoggingOut
            }
            AuthorizationState::Closing => Auth::LoggingOut,
            AuthorizationState::Closed => {
                self.after_close();
                // Chat windows belong to the account that is gone.
                let close = Task::batch(
                    self.session
                        .panes
                        .keys()
                        .filter(|&&w| w != self.main_window)
                        .map(|&w| window::close(w)),
                );
                // The plugin thread outlives sessions; it just loses the account.
                if let Some(host) = &self.plugin_host {
                    host.send(HostCmd::Account(None));
                }
                let start = self.start_session();
                #[cfg(target_os = "linux")]
                if let Some(tray) = self.tray.as_ref().map(|t| t.show(None)) {
                    return Task::batch([close, start, tray]);
                }
                return Task::batch([close, start]);
            }
            other => Auth::Unsupported(format!("{other:?}")),
        };
        Task::none()
    }

    /// Opens the account's database (with the password key, if any).
    pub(crate) fn set_parameters(&mut self) -> Task<Msg> {
        self.session.busy = true;
        Task::perform(
            td::set_parameters(
                self.session.client_id,
                self.api_id,
                self.api_hash.clone(),
                self.session.slot,
                crate::lock::tdlib_key(self.session.db_key.as_ref()),
            ),
            Msg::ParametersSet,
        )
    }

    fn submit_auth(&mut self) -> Task<Msg> {
        let value = self.session.input.trim().to_owned();
        if value.is_empty() || self.session.busy {
            return Task::none();
        }
        let id = self.session.client_id;
        let request = match self.session.auth {
            Auth::Phone => Task::perform(td::send_phone(id, value), Msg::Done),
            Auth::Code => Task::perform(td::send_code(id, value), Msg::Done),
            // Password is not trimmed: spaces may be part of it.
            Auth::Password { .. } => {
                Task::perform(td::send_password(id, self.session.input.clone()), Msg::Done)
            }
            _ => return Task::none(),
        };
        self.session.busy = true;
        self.error = None;
        request
    }

    fn dismiss_chat_list_menu(&mut self, window: WinId) {
        if let Some(pane) = self.session.panes.get_mut(&window) {
            pane.list.menu = None;
            pane.list.folder_picker = None;
            pane.list.new_folder_name = None;
            pane.list.new_folder_error = None;
            pane.list.new_folder_request = None;
        }
    }

    fn select_chat(&mut self, window: WinId, chat_id: i64) -> Task<Msg> {
        self.dismiss_chat_list_menu(window);
        self.close_video_leaving(window, chat_id);
        let cancel = self.cancel_recording_leaving(window, chat_id);
        if window == self.main_window {
            return cancel.chain(self.show_in_main(chat_id));
        }
        let Some(pane) = self.session.panes.get_mut(&window) else {
            return cancel;
        };
        if pane.shows(chat_id) {
            return cancel;
        }
        let previous = pane.chat_id;
        let left_draft = pane.draft();
        pane.switch_to(chat_id);
        let close = match previous {
            Some(prev) => Task::batch([
                self.sync_draft(prev, left_draft),
                self.close_chat_if_unused(prev),
            ]),
            None => Task::none(),
        };
        cancel.chain(close).chain(Task::perform(
            td::open_chat(self.session.client_id, chat_id),
            move |r| Msg::Pane(window, PaneMsg::HistoryLoaded(chat_id, r)),
        ))
    }

    /// A chat's input field was left: its text becomes the chat's draft in
    /// Telegram, unless it is what Telegram already has.
    pub(crate) fn sync_draft(&mut self, chat_id: i64, text: Option<String>) -> Task<Msg> {
        let Some(text) = text else {
            return Task::none();
        };
        let known = self.session.synced_drafts.get(&chat_id).map_or_else(
            || {
                self.session
                    .chats
                    .get(&chat_id)
                    .and_then(|c| c.draft.as_ref())
                    .map_or("", |d| d.text.as_str())
            },
            String::as_str,
        );
        if known == text {
            return Task::none();
        }
        self.session.synced_drafts.insert(chat_id, text.clone());
        Task::perform(
            td::save_draft(self.session.client_id, chat_id, text),
            Msg::Done,
        )
    }

    /// An opened chat with an empty input field gets its saved draft.
    pub(crate) fn load_draft(&self, window: WinId, chat_id: i64) -> Task<Msg> {
        let empty = self
            .session
            .panes
            .get(&window)
            .is_some_and(|p| p.compose.text().is_empty());
        match self
            .session
            .chats
            .get(&chat_id)
            .and_then(|c| c.draft.clone())
        {
            Some(draft) if empty => Task::perform(
                td::draft_markdown(self.session.client_id, draft),
                move |r| Msg::Pane(window, PaneMsg::DraftLoaded(chat_id, r)),
            ),
            _ => Task::none(),
        }
    }

    fn set_positions(&mut self, chat_id: i64, positions: &[ChatPosition]) {
        let main = positions.iter().find(|p| matches!(p.list, ChatList::Main));
        let archive = positions
            .iter()
            .find(|p| matches!(p.list, ChatList::Archive));
        self.set_order(chat_id, main.map_or(0, |p| p.order));
        self.set_archive_order(chat_id, archive.map_or(0, |p| p.order));
        // The positions are complete: folders not listed no longer hold it.
        let folders: Vec<(i32, i64)> = positions
            .iter()
            .filter_map(|p| match &p.list {
                ChatList::Folder(f) => Some((f.chat_folder_id, p.order)),
                _ => None,
            })
            .collect();
        let old: Vec<i32> = self
            .session
            .chats
            .get(&chat_id)
            .map(|c| c.folders.iter().map(|&(f, _)| f).collect())
            .unwrap_or_default();
        for folder in old {
            if !folders.iter().any(|&(f, _)| f == folder) {
                self.set_folder_order(chat_id, folder, 0);
            }
        }
        for (folder, order) in folders {
            self.set_folder_order(chat_id, folder, order);
        }
        if let Some(chat) = self.session.chats.get_mut(&chat_id) {
            chat.pinned = main
                .filter(|p| p.order != 0)
                .or_else(|| archive.filter(|p| p.order != 0))
                .is_some_and(|p| p.is_pinned);
        }
    }

    fn archive_panel_mut(
        &mut self,
        window: WinId,
        client: i32,
        request: u64,
    ) -> Option<&mut pane::ArchiveSettings> {
        if client != self.session.client_id || !self.auth_ready() || self.session.leave.is_some() {
            return None;
        }
        let pane = self.session.panes.get_mut(&window)?;
        if !pane.list.archive {
            return None;
        }
        pane.list
            .archive_settings
            .as_mut()
            .filter(|panel| panel.request == request)
    }

    fn open_archive_settings(&mut self, window: WinId) -> Task<Msg> {
        if !self.auth_ready() || self.session.leave.is_some() {
            return Task::none();
        }
        let Some(pane) = self.session.panes.get_mut(&window) else {
            return Task::none();
        };
        if !pane.list.archive || pane.list.archive_settings.is_some() {
            return Task::none();
        }
        pane.list.archive_settings = Some(pane::ArchiveSettings {
            request: 0,
            loading: false,
            saving: false,
            settings: None,
            capability: None,
            error: None,
        });
        if window == self.main_window {
            self.session.settings_open = false;
        }
        self.read_archive_settings(window)
    }

    fn read_archive_settings(&mut self, window: WinId) -> Task<Msg> {
        if !self.auth_ready() || self.session.leave.is_some() {
            return Task::none();
        }
        let Some(pane) = self.session.panes.get_mut(&window) else {
            return Task::none();
        };
        if !pane.list.archive {
            return Task::none();
        }
        let Some(panel) = pane.list.archive_settings.as_mut() else {
            return Task::none();
        };
        if panel.saving {
            return Task::none();
        }
        self.session.next_archive_request = self.session.next_archive_request.wrapping_add(1);
        let request = self.session.next_archive_request;
        panel.request = request;
        panel.loading = true;
        panel.capability = None;
        panel.error = None;
        let client = self.session.client_id;
        Task::batch([
            Task::perform(td::archive_settings(client), move |result| {
                Msg::ArchiveSettingsLoaded(window, client, request, result)
            }),
            Task::perform(td::archive_unknown_capability(client), move |result| {
                Msg::ArchiveCapabilityLoaded(window, client, request, result)
            }),
        ])
    }

    fn release_suppressed_archive_reads(&mut self) {
        for pane in self.session.panes.values_mut() {
            let Some(panel) = pane.list.archive_settings.as_mut() else {
                continue;
            };
            if panel.loading {
                // The suppressed reply can still arrive after the failed write.
                self.session.next_archive_request =
                    self.session.next_archive_request.wrapping_add(1);
                panel.request = self.session.next_archive_request;
                panel.loading = false;
            }
        }
    }

    fn change_archive_flag(
        &mut self,
        window: WinId,
        flag: td::ArchiveFlag,
        value: bool,
    ) -> Task<Msg> {
        if self.session.archive_write.is_some()
            || !self.auth_ready()
            || self.session.leave.is_some()
        {
            return Task::none();
        }
        let Some(pane) = self.session.panes.get_mut(&window) else {
            return Task::none();
        };
        if !pane.list.archive {
            return Task::none();
        }
        let Some(panel) = pane.list.archive_settings.as_mut() else {
            return Task::none();
        };
        if panel.loading
            || panel.saving
            || panel.settings.is_none()
            || panel.capability.is_none()
            || (flag == td::ArchiveFlag::ArchiveUnknown
                && !panel
                    .capability
                    .as_ref()
                    .is_some_and(|capability| matches!(capability, Ok(true))))
        {
            return Task::none();
        }
        self.session.next_archive_request = self.session.next_archive_request.wrapping_add(1);
        let request = self.session.next_archive_request;
        panel.request = request;
        panel.saving = true;
        panel.error = None;
        self.session.archive_write = Some((window, request));
        let client = self.session.client_id;
        Task::perform(td::archive_settings(client), move |result| {
            Msg::ArchiveFetched(window, client, request, flag, value, result)
        })
    }

    fn archive_fetched(
        &mut self,
        window: WinId,
        client: i32,
        request: u64,
        flag: td::ArchiveFlag,
        value: bool,
        result: td::TdResult<tdlib_rs::types::ArchiveChatListSettings>,
    ) -> Task<Msg> {
        if client != self.session.client_id || self.session.archive_write != Some((window, request))
        {
            return Task::none();
        }
        let Some(panel) = self.archive_panel_mut(window, client, request) else {
            // Cancellation before the server write is safe: no write was sent.
            self.session.archive_write = None;
            self.release_suppressed_archive_reads();
            return Task::none();
        };
        match result {
            Ok(latest) => {
                let settings = td::archive_flag(latest, flag, value);
                Task::perform(td::save_archive_settings(client, settings), move |result| {
                    Msg::ArchiveSaved(window, client, request, result)
                })
            }
            Err(error) => {
                panel.saving = false;
                panel.error = Some(format!("Не удалось получить актуальные настройки: {error}"));
                self.session.archive_write = None;
                self.release_suppressed_archive_reads();
                Task::none()
            }
        }
    }

    fn archive_saved(
        &mut self,
        window: WinId,
        client: i32,
        request: u64,
        result: td::TdResult<()>,
    ) -> Task<Msg> {
        if client != self.session.client_id || self.session.archive_write != Some((window, request))
        {
            return Task::none();
        }
        match result {
            Ok(()) => Task::perform(td::archive_settings(client), move |result| {
                Msg::ArchiveReadback(window, client, request, result)
            }),
            Err(error) => {
                if let Some(panel) = self.archive_panel_mut(window, client, request) {
                    panel.saving = false;
                    panel.error = Some(format!("Не удалось сохранить настройки: {error}"));
                }
                self.session.archive_write = None;
                self.release_suppressed_archive_reads();
                Task::none()
            }
        }
    }

    fn archive_readback(
        &mut self,
        window: WinId,
        client: i32,
        request: u64,
        result: td::TdResult<tdlib_rs::types::ArchiveChatListSettings>,
    ) -> Task<Msg> {
        if client != self.session.client_id || self.session.archive_write != Some((window, request))
        {
            return Task::none();
        }
        self.session.archive_write = None;
        let confirmed = result.as_ref().ok().cloned();
        let mut refresh = Vec::new();
        for (&id, pane) in &mut self.session.panes {
            if !pane.list.archive {
                continue;
            }
            let Some(panel) = pane.list.archive_settings.as_mut() else {
                continue;
            };
            if id == window && panel.request == request {
                panel.saving = false;
                match &result {
                    Ok(settings) => {
                        panel.settings = Some(settings.clone());
                        panel.error = None;
                    }
                    Err(error) => {
                        panel.settings = None;
                        panel.error = Some(format!(
                            "Не удалось проверить настройки после сохранения: {error}"
                        ));
                    }
                }
            } else {
                // An earlier read may complete after this readback. Give every
                // other open panel a new request identity before it can arrive.
                self.session.next_archive_request =
                    self.session.next_archive_request.wrapping_add(1);
                panel.request = self.session.next_archive_request;
                panel.settings = confirmed.clone();
                panel.loading = confirmed.is_none() || panel.capability.is_none();
                panel.error = None;
                if panel.loading {
                    refresh.push(id);
                }
            }
        }
        Task::batch(refresh.into_iter().map(|id| self.read_archive_settings(id)))
    }

    fn open_profile(&mut self, window: WinId, chat_id: i64) -> Task<Msg> {
        let Some(pane) = self.session.panes.get_mut(&window) else {
            return Task::none();
        };
        pane.profile = Some((chat_id, None));
        pane.profile_more = false;
        pane.profile_qr = None;
        pane.confirm_leave = false;
        let Some(kind) = self
            .session
            .chats
            .get(&chat_id)
            .and_then(|c| c.kind.clone())
        else {
            return Task::none();
        };
        Task::perform(td::profile(self.session.client_id, kind), move |r| {
            Msg::ProfileLoaded(window, chat_id, r)
        })
    }

    fn profile_action(&mut self, window: WinId, action: ProfileAction) -> Task<Msg> {
        let Some(pane) = self.session.panes.get_mut(&window) else {
            return Task::none();
        };
        let Some((chat_id, data)) = &pane.profile else {
            return Task::none();
        };
        let chat_id = *chat_id;
        let (linked, username) = match data {
            Some(Ok(p)) => (p.linked_chat_id, p.usernames.first().cloned()),
            _ => (0, None),
        };
        match action {
            ProfileAction::ToggleMute => {
                let muted = self.session.chats.get(&chat_id).is_some_and(|c| c.muted());
                return self.chat_op(chat_id, ChatOp::Mute(!muted));
            }
            ProfileAction::Discuss if linked != 0 => {
                pane.profile = None;
                return self.select_chat(window, linked);
            }
            ProfileAction::Discuss => {}
            ProfileAction::ToggleMore => pane.profile_more = !pane.profile_more,
            ProfileAction::ToggleQr => {
                pane.profile_qr = if pane.profile_qr.is_some() {
                    None
                } else {
                    username
                        .as_deref()
                        .and_then(|name| qr::image(&format!("https://t.me/{name}")))
                };
            }
            ProfileAction::NewWindow => {
                pane.profile = None;
                return self.open_chat_window(Some(chat_id));
            }
            ProfileAction::CopyLink => {
                pane.profile_more = false;
                if let Some(name) = username {
                    return self.copy_notice(format!("https://t.me/{name}"));
                }
            }
            ProfileAction::OpenChat => {
                pane.profile = None;
                return self.select_chat(window, chat_id);
            }
            ProfileAction::Leave => {
                // The first click asks, the second leaves.
                if !pane.confirm_leave {
                    pane.confirm_leave = true;
                    return Task::none();
                }
                pane.confirm_leave = false;
                return Task::perform(td::leave_chat(self.session.client_id, chat_id), move |r| {
                    Msg::Profile(window, ProfileAction::Left(chat_id, r))
                });
            }
            ProfileAction::Left(left, result) => match result {
                Ok(()) => {
                    if pane.profile.as_ref().is_some_and(|(id, _)| *id == left) {
                        pane.profile = None;
                    }
                }
                Err(e) => self.error = Some(e),
            },
        }
        Task::none()
    }

    fn link_resolved(
        &mut self,
        window: WinId,
        url: String,
        result: Result<Option<td::LinkTarget>, String>,
    ) -> Task<Msg> {
        match result {
            Ok(Some(td::LinkTarget::Chat(chat_id))) => self.select_chat(window, chat_id),
            Ok(Some(td::LinkTarget::Message(chat_id, id))) => {
                self.update(Msg::OpenFound(window, chat_id, id))
            }
            Ok(Some(td::LinkTarget::Invite {
                link,
                title,
                members,
            })) => {
                if let Some(pane) = self.session.panes.get_mut(&window) {
                    pane.confirm_join = Some((link, title, members));
                }
                Task::none()
            }
            // Not a place inside Telegram (a web page on t.me, a login link…).
            Ok(None) => {
                if let Some(url) = rich::safe_link(&url)
                    && let Err(e) = open::that_detached(&url)
                {
                    self.error = Some(format!("не удалось открыть ссылку: {e}"));
                }
                Task::none()
            }
            Err(e) => {
                let reason =
                    if e.contains("USERNAME_NOT_OCCUPIED") || e.contains("USERNAME_INVALID") {
                        "нет такого пользователя или канала".to_owned()
                    } else if e.contains("INVITE_HASH") {
                        "приглашение недействительно или устарело".to_owned()
                    } else {
                        e
                    };
                self.error = Some(format!("ссылка: {reason}"));
                Task::none()
            }
        }
    }

    /// Copies to the clipboard with a short «Скопировано».
    pub(crate) fn copy_notice(&mut self, text: String) -> Task<Msg> {
        self.session.notice = Some("Скопировано".into());
        Task::batch([
            iced::clipboard::write(text),
            Task::perform(
                async { tokio::time::sleep(std::time::Duration::from_secs(2)).await },
                |()| Msg::ClearNotice,
            ),
        ])
    }

    fn confirm_new_folder(&mut self, window: WinId, chat_id: i64) -> Task<Msg> {
        if !self.session.chats.contains_key(&chat_id) {
            return Task::none();
        }
        let Some(pane) = self.session.panes.get_mut(&window) else {
            return Task::none();
        };
        if pane.list.menu != Some(chat_id) {
            return Task::none();
        }
        let Some(name) = pane.list.new_folder_name.as_deref() else {
            return Task::none();
        };
        if self.session.folder_creation.is_some() || self.session.folder_reorder.is_some() {
            pane.list.new_folder_error = Some("Дождитесь завершения операции с папками".into());
            return Task::none();
        }
        let folder = match td::new_chat_folder(name, chat_id) {
            Ok(folder) => folder,
            Err(error) => {
                pane.list.new_folder_error = Some(error);
                return Task::none();
            }
        };
        self.session.next_folder_creation += 1;
        let request = self.session.next_folder_creation;
        self.session.folder_creation = Some(FolderCreation {
            window,
            chat_id,
            request,
            id: None,
            observed: HashSet::new(),
            initial: self.session.folders.iter().map(|(id, _)| *id).collect(),
        });
        self.session.folder_creation_warning = None;
        pane.list.new_folder_error = None;
        pane.list.new_folder_request = Some(request);
        let client = self.session.client_id;
        Task::perform(td::create_chat_folder(client, folder), move |result| {
            Msg::FolderCreated(window, client, request, result)
        })
    }

    fn reorder_folder(&mut self, window: WinId, folder: i32, up: bool) -> Task<Msg> {
        if !self.session.panes.contains_key(&window)
            || self.session.folder_reorder.is_some()
            || self.session.folder_creation.is_some()
        {
            return Task::none();
        }
        let Some((ids, main_tab)) = self.session.reordered_folders(folder, up) else {
            return Task::none();
        };
        let initial = self.session.folders.iter().map(|&(id, _)| id).collect();
        self.session.next_folder_reorder += 1;
        let request = self.session.next_folder_reorder;
        self.session.folder_reorder = Some(FolderReorder {
            window,
            request,
            expected: ids.clone(),
            main_tab,
            initial,
            initial_main_tab: self.session.main_tab,
            state: FolderReorderState::AwaitingReply,
        });
        self.session.folder_reorder_error = None;
        let client = self.session.client_id;
        Task::perform(
            td::reorder_chat_folders(ids, main_tab, client),
            move |result| Msg::FolderReordered(window, client, request, result),
        )
    }

    fn folder_action(
        &mut self,
        window: WinId,
        chat_id: i64,
        folder: i32,
        include: bool,
    ) -> Task<Msg> {
        if !self.session.chats.contains_key(&chat_id)
            || !self.session.folders.iter().any(|(id, _)| *id == folder)
        {
            return Task::none();
        }
        let Some(pane) = self.session.panes.get_mut(&window) else {
            return Task::none();
        };
        let Some(picker) = pane.list.folder_picker.as_mut() else {
            return Task::none();
        };
        if pane.list.menu != Some(chat_id) || picker.chat_id != chat_id || picker.pending.is_some()
        {
            return Task::none();
        }
        if self.session.folder_edits.contains_key(&folder) {
            picker.feedback = Some("Папка уже изменяется в другом окне. Повторите позже.".into());
            return Task::none();
        }
        self.session.next_folder_edit += 1;
        let request = self.session.next_folder_edit;
        self.session.folder_edits.insert(folder, request);
        picker.pending = Some((request, folder));
        picker.feedback = None;
        let client = self.session.client_id;
        Task::perform(
            td::set_folder_membership(client, folder, chat_id, include),
            move |result| Msg::FolderEdited(window, client, chat_id, folder, request, result),
        )
    }

    /// The window's actual TDLib list owns the preference, never a folder or
    /// an unscoped search result. Persist first; failures leave memory intact.
    /// An unreadable list cannot be edited from an incomplete in-memory view.
    fn local_chat_pin(&mut self, window: WinId, chat_id: i64, on: bool) -> Task<Msg> {
        if self.session.local_chat_pins_failed {
            self.error = Some("локальные закрепы недоступны: ошибка загрузки".into());
            return Task::none();
        }
        let Some(pane) = self.session.panes.get(&window) else {
            return Task::none();
        };
        if pane.list.folder.is_some() && !pane.list.archive {
            return Task::none();
        }
        let archived = pane.list.archive;
        let Some(chat) = self.session.chats.get(&chat_id) else {
            return Task::none();
        };
        let (order, list) = if archived {
            (chat.archive_order, &self.session.archived)
        } else {
            (chat.order, &self.session.order)
        };
        if order == 0 || !list.contains(&(Reverse(order), chat_id)) {
            return Task::none();
        }
        let pins = if archived {
            &mut self.session.local_archive
        } else {
            &mut self.session.local_main
        };
        if pins.contains(&chat_id) == on {
            return Task::none();
        }
        let Some(archive) = &self.session.archive else {
            self.error = Some("локальные закрепы недоступны во время смены пароля".into());
            return Task::none();
        };
        if let Err(e) = archive.set_local_chat_pin(archived, chat_id, on) {
            self.error = Some(format!("локальный закреп: {e}"));
            return Task::none();
        }
        pins.retain(|&id| id != chat_id);
        if on {
            pins.insert(0, chat_id);
        }
        Task::none()
    }

    fn chat_op(&mut self, chat_id: i64, op: ChatOp) -> Task<Msg> {
        let Some(chat) = self.session.chats.get(&chat_id) else {
            return Task::none();
        };
        let client = self.session.client_id;
        let list = if chat.order == 0 && chat.archive_order != 0 {
            ChatList::Archive
        } else {
            ChatList::Main
        };
        match op {
            ChatOp::Pin(on) => Task::perform(td::pin_chat(client, chat_id, list, on), Msg::Done),
            ChatOp::Archive(on) => {
                let to = if on {
                    ChatList::Archive
                } else {
                    ChatList::Main
                };
                Task::perform(td::move_chat(client, chat_id, to), Msg::Done)
            }
            ChatOp::Mute(on) => {
                let mut settings = chat.notify.clone();
                settings.use_default_mute_for = false;
                // "Forever" is the largest period TDLib accepts.
                settings.mute_for = if on { i32::MAX } else { 0 };
                Task::perform(td::set_notifications(client, chat_id, settings), Msg::Done)
            }
            ChatOp::Close => Task::none(),
            ChatOp::LocalPin(_) | ChatOp::Folders | ChatOp::NewFolder => Task::none(),
        }
    }

    fn set_folder_order(&mut self, chat_id: i64, folder: i32, order: i64) {
        let Some(chat) = self.session.chats.get_mut(&chat_id) else {
            return;
        };
        let list = self.session.folder_lists.entry(folder).or_default();
        if let Some(i) = chat.folders.iter().position(|&(f, _)| f == folder) {
            let (_, old) = chat.folders.remove(i);
            list.remove(&(Reverse(old), chat_id));
        }
        if order != 0 {
            chat.folders.push((folder, order));
            list.insert((Reverse(order), chat_id));
        }
    }

    fn set_archive_order(&mut self, chat_id: i64, order: i64) {
        let Some(chat) = self.session.chats.get_mut(&chat_id) else {
            return;
        };
        self.session
            .archived
            .remove(&(Reverse(chat.archive_order), chat_id));
        chat.archive_order = order;
        if order != 0 {
            self.session.archived.insert((Reverse(order), chat_id));
        }
    }

    /// Archives a confirmed message; pending outgoing ones have temporary ids
    /// and are archived on `MessageSendSucceeded` instead.
    fn archive_save(&mut self, item: &MsgItem) {
        if item.pending || item.failed {
            return;
        }
        if let Some(archive) =
            Self::active_archive(&mut self.session.archive, self.settings.keep_deleted)
            && let Err(e) = archive.save([item])
        {
            self.error = Some(format!("архив: {e}"));
        }
    }

    /// Permanent deletion on the server. Messages deleted by the user from
    /// this client disappear; any other deletion keeps the archived copy.
    fn on_deleted(&mut self, chat_id: i64, ids: &[i64]) {
        let (own, foreign): (Vec<i64>, Vec<i64>) = ids
            .iter()
            .partition(|&&id| self.session.self_deleted.remove(&(chat_id, id)));
        if !own.is_empty() {
            self.forget(chat_id, &own);
        }
        if foreign.is_empty() {
            return;
        }
        let Some(archive) =
            Self::active_archive(&mut self.session.archive, self.settings.keep_deleted)
        else {
            // Not keeping deleted messages: behave like a normal client.
            for pane in self.panes_of(chat_id) {
                pane.remove(&foreign);
            }
            for &id in &foreign {
                self.session.reply_previews.remove(&(chat_id, id));
            }
            return;
        };
        if let Err(e) = archive.mark_deleted(chat_id, &foreign) {
            self.error = Some(format!("архив: {e}"));
        }
        for pane in self.panes_of(chat_id) {
            pane.mark_deleted(&foreign);
        }
        for &id in &foreign {
            if let Some(item) = self.session.reply_previews.get_mut(&(chat_id, id)) {
                item.deleted = true;
            }
        }
    }

    /// Removes messages from the archive and from every window.
    fn forget(&mut self, chat_id: i64, ids: &[i64]) {
        if let Some(archive) = &mut self.session.archive
            && let Err(e) = archive.purge(chat_id, ids)
        {
            self.error = Some(format!("архив: {e}"));
        }
        for pane in self.panes_of(chat_id) {
            pane.remove(ids);
        }
        self.session
            .reply_previews
            .retain(|&(c, i), _| !(c == chat_id && ids.contains(&i)));
    }

    /// First page of a chat opened in a window.
    /// First page of a chat. With unread messages the chat opens at the
    /// first of them, under a «Непрочитанные сообщения» line.
    fn show_history(&mut self, window: WinId, chat_id: i64, messages: &[Message]) -> Task<Msg> {
        let (items, pending) = self.merge_page(chat_id, messages, i64::MAX);
        let replies = self.fetch_replies(chat_id, &items);
        let read = self
            .session
            .chats
            .get(&chat_id)
            .filter(|c| c.unread > 0)
            .map(|c| c.read_inbox);
        let Some(pane) = self.session.panes.get_mut(&window) else {
            return replies;
        };
        pane.show_page(items, pending, page_of(messages));
        let Some(read) = read else {
            return replies;
        };
        pane.unread_after = Some(read);
        let first_unread = pane.messages.iter().find(|m| m.id > read).map(|m| m.id);
        let page_reaches = pane.messages.first().is_some_and(|m| m.id <= read);
        match first_unread {
            Some(id) if page_reaches => {
                let offset = pane.offset_of(id).unwrap_or(0.0);
                let offset = iced::widget::scrollable::AbsoluteOffset { x: 0.0, y: offset };
                pane.scroll = Some(offset);
                Task::batch([
                    replies,
                    iced::widget::operation::scroll_to(pane.scroll_id.clone(), offset),
                ])
            }
            // More unread than one page: history around the last read one.
            _ => {
                pane.jump_quiet = true;
                Task::batch([replies, self.jump(window, read)])
            }
        }
    }

    fn prepend_history(
        &mut self,
        window: WinId,
        chat_id: i64,
        before: i64,
        messages: &[Message],
    ) -> Task<Msg> {
        let (items, _) = self.merge_page(chat_id, messages, before);
        let replies = self.fetch_replies(chat_id, &items);
        if let Some(pane) = self.session.panes.get_mut(&window) {
            pane.prepend_page(items, page_of(messages));
        }
        replies
    }

    fn load_older(&mut self, window: WinId) -> Task<Msg> {
        let Some(pane) = self.session.panes.get_mut(&window) else {
            return Task::none();
        };
        let (Some(chat_id), Some(before)) = (pane.chat_id, pane.next_older()) else {
            return Task::none();
        };
        pane.loading_older = true;
        Task::perform(
            td::history_page(self.session.client_id, chat_id, before),
            move |r| Msg::Pane(window, PaneMsg::OlderLoaded(chat_id, before, r)),
        )
    }

    /// Converts a history page (ids below `upper`) into confirmed items sorted
    /// by id plus pending outgoing ones, and archives it. Archived deleted
    /// messages of the same id range are merged in. A short page means the
    /// chat start is reached, so every older deleted message belongs to it.
    fn merge_page(
        &mut self,
        chat_id: i64,
        messages: &[Message],
        upper: i64,
    ) -> (Vec<MsgItem>, Vec<MsgItem>) {
        let lower = match messages.first() {
            Some(first) if messages.len() >= td::HISTORY_LIMIT => first.id,
            _ => i64::MIN,
        };
        self.merge_range(chat_id, messages, lower, upper)
    }

    /// Like `merge_page` with an explicit id range `[lower, upper)` for the
    /// archived deleted messages.
    fn merge_range(
        &mut self,
        chat_id: i64,
        messages: &[Message],
        lower: i64,
        upper: i64,
    ) -> (Vec<MsgItem>, Vec<MsgItem>) {
        for m in messages {
            self.note_files(&m.content);
        }
        let (confirmed, pending): (Vec<&Message>, Vec<&Message>) =
            messages.iter().partition(|m| m.sending_state.is_none());
        let mut items: Vec<MsgItem> = confirmed.into_iter().map(MsgItem::from).collect();
        if let Some(archive) =
            Self::active_archive(&mut self.session.archive, self.settings.keep_deleted)
        {
            let deleted = archive
                .save(&items)
                .and_then(|()| archive.deleted(chat_id, lower, upper));
            match deleted {
                Ok(deleted) => items.extend(deleted),
                Err(e) => self.error = Some(format!("архив: {e}")),
            }
        }
        items.sort_by_key(|m| m.id);
        (items, pending.into_iter().map(MsgItem::from).collect())
    }

    /// Turning the setting off hides archived deleted messages (the archive
    /// is kept); turning it on merges them back into every pane's currently
    /// loaded range, foreground or background, without touching compose,
    /// reply, edit or selection state (no TDLib round trip is needed: the
    /// archive already has them).
    fn set_keep_deleted(&mut self, keep: bool) -> Task<Msg> {
        if self.settings.keep_deleted == keep {
            return Task::none();
        }
        self.settings.keep_deleted = keep;
        self.save_settings();
        let archive = self.session.archive.as_ref();
        for pane in self
            .session
            .panes
            .values_mut()
            .chain(self.session.background.values_mut())
        {
            let Some(chat_id) = pane.chat_id else {
                continue;
            };
            if !keep {
                pane.messages.retain(|m| !m.deleted);
                continue;
            }
            let Some(archive) = archive else { continue };
            let lower = pane.oldest_loaded.unwrap_or(i64::MIN);
            let upper = if pane.newest_loaded {
                i64::MAX
            } else {
                pane.messages.last().map_or(i64::MAX, |m| m.id + 1)
            };
            if let Ok(deleted) = archive.deleted(chat_id, lower, upper) {
                let known: HashSet<i64> = pane.messages.iter().map(|m| m.id).collect();
                pane.messages
                    .extend(deleted.into_iter().filter(|m| !known.contains(&m.id)));
                pane.messages.sort_by_key(|m| m.id);
            }
        }
        Task::none()
    }

    fn set_order(&mut self, chat_id: i64, order: i64) {
        let Some(chat) = self.session.chats.get_mut(&chat_id) else {
            return;
        };
        self.session.order.remove(&(Reverse(chat.order), chat_id));
        chat.order = order;
        if order != 0 {
            self.session.order.insert((Reverse(order), chat_id));
        }
    }
}

fn page_of(messages: &[Message]) -> Page {
    Page {
        len: messages.len(),
        first_id: messages.first().map(|m| m.id),
    }
}

#[cfg(test)]
mod render_tests;
#[cfg(test)]
mod sandbox;
#[cfg(test)]
mod tests;
