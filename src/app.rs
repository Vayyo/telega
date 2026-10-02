mod accounts;
mod avatar_cache;
mod avatars;
mod card;
mod extra;
mod icons;
pub(crate) mod look;
pub(crate) mod media;
mod nav;
mod notify;
mod pane;
mod pane_update;
mod password;
mod picker;
mod playback;
mod plugin_runtime;
mod plugins_view;
mod qr;
pub(crate) mod rich;
mod selectable;
mod session;
mod sidebar_view;
mod tabs;
mod td_updates;
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
use tdlib_rs::enums::{AuthorizationState, ChatList, MessageSender, Update};
use tdlib_rs::types::{ChatPosition, File, Message};

use crate::archive::Archive;
use crate::plugins::{self, Action, HostCmd, HostEvent, HostHandle, PluginInfo};
use crate::settings::{CacheLimits, Settings};
use crate::td;
use media::Media;
use pane::{ChatPane, Page};
use session::{
    FolderCreation, FolderReorder, FolderReorderState, ListRead, Session, SettingsSection,
};

pub(crate) type WinId = window::Id;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MainWindowState {
    Open,
    Closing { restore: bool },
    Hidden,
    Opening,
}

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
    Input(crate::lock::SecretString),
    SubmitAuth,
    /// Return from code/password entry to phone entry (wrong number).
    BackToPhone,
    /// First click on "Выйти" asks for confirmation; `ConfirmLogOut(true)` logs out.
    LogOut,
    ConfirmLogOut(bool),
    OpenSettings,
    CloseSettings,
    ToggleSettingsSection(SettingsSection),
    SetKeepDeleted(bool),
    SetCachePolicy(CacheLimits),
    ApplyCachePolicy,
    CachePolicyApplied(i32, CacheLimits, Result<(), String>),
    AvatarSweepDone,
    AvatarSweepTick,
    /// Opens a separate chat window, optionally with a chat already selected.
    OpenChatWindow(Option<i64>),
    /// Opens the archive list in its own window.
    OpenArchiveWindow(WinId, i32),
    WindowCloseRequested(WinId),
    MainWindowOpened(WinId),
    WindowClosed(WinId),
    Key(WinId, Key, Modifiers),
    /// Action inside the chat pane of a window.
    Pane(WinId, PaneMsg),
    Plugin(HostEvent),
    /// Result of an action a plugin asked for: the original action (to
    /// retry if it failed on a flood wait), a log line or error.
    PluginDone(plugins::Origin, String, Action, Result<String, String>),
    /// Deletion after filtering; origin is verified again before the effect.
    PluginDeleteOwn(plugins::Origin, String, i64, Vec<i64>, bool, Action),
    PluginDeleted(
        plugins::Origin,
        String,
        i64,
        Vec<i64>,
        Action,
        Result<(), String>,
    ),
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
    /// Answer to a `download_file` request, tagged with its file: on
    /// failure the request is taken back out of flight and the file is
    /// marked as failed, so that nothing retries it on its own.
    FileRequestDone(i32, Result<(), String>),
    ImageDecoded(i32, Result<(u32, u32, Vec<u8>), String>),
    /// Round profile picture of a file, tagged with its originating client.
    AvatarDecoded(i32, i32, Result<(Vec<u8>, [u8; 32]), String>),
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
    /// Parameters were accepted; the initial cache option error is nonfatal.
    ParametersSet(Result<Option<String>, String>),
    ParametersSetForClient(i32, CacheLimits, Result<Option<String>, String>),
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
    DismissAccountPopup,
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
    Copy(std::sync::Arc<str>),
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
    pub(crate) fn is_bot_user(&self, user_id: i64) -> bool {
        self.session.bot_users.contains(&user_id)
    }

    pub(crate) fn is_bot_chat(&self, chat_id: i64) -> bool {
        self.session
            .chats
            .get(&chat_id)
            .and_then(|chat| chat.kind.as_ref())
            .is_some_and(|kind| match kind {
                tdlib_rs::enums::ChatType::Private(private) => self.is_bot_user(private.user_id),
                _ => false,
            })
    }

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

fn main_window_settings() -> window::Settings {
    let settings = window_settings(1000.0);
    #[cfg(target_os = "linux")]
    {
        window::Settings {
            exit_on_close_request: false,
            ..settings
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        settings
    }
}

pub(crate) struct App {
    api_id: i32,
    api_hash: String,

    /// Full client window; its native surface can close while the session stays alive.
    main_window: WinId,
    main_window_state: MainWindowState,
    settings: Settings,
    settings_path: PathBuf,
    /// Portraits of logged-in accounts survive replacement of the TDLib session.
    account_portraits: HashMap<u32, iced::widget::image::Handle>,
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
    cache_unsaved: bool,
    last_avatar_sweep: Option<std::time::Instant>,
    avatar_sweep_active: bool,
    avatar_sweep_pending: Option<CacheLimits>,

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
        let (main_window, open) = window::open(main_window_settings());
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
        let sweep = if load_failed {
            Task::none()
        } else {
            app.schedule_avatar_sweep()
        };
        (app, Task::batch([open.discard(), start, theme, sweep]))
    }

    /// Fresh TDLib client with empty state on the active account slot,
    /// replacing the session of the account that is gone. A closed client
    /// cannot be reused, so log out and account switches start a new one
    /// in the same main window.
    fn start_session(&mut self) -> Task<Msg> {
        if self.cache_unsaved {
            self.save_settings();
        }
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
        let account_portraits = settings
            .accounts
            .iter()
            .filter(|account| account.user_id.is_some())
            .filter_map(|account| {
                let rgba = avatar_cache::load(&account.avatar_digest?)?;
                Some((
                    account.slot,
                    iced::widget::image::Handle::from_rgba(avatars::SIDE, avatars::SIDE, rgba),
                ))
            })
            .collect();
        Self {
            api_id,
            api_hash,
            main_window,
            main_window_state: MainWindowState::Open,
            settings,
            settings_path,
            account_portraits,
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
            cache_unsaved: false,
            last_avatar_sweep: None,
            avatar_sweep_active: false,
            avatar_sweep_pending: None,
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
        self.settings.save(&self.settings_path)?;
        self.cache_unsaved = false;
        Ok(())
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

    /// Maintenance uses the blocking pool and coalesces concurrent requests.
    fn schedule_avatar_sweep(&mut self) -> Task<Msg> {
        if self.settings_load_failed {
            return Task::none();
        }
        let limits = self.settings.cache;
        if self.avatar_sweep_active {
            self.avatar_sweep_pending = Some(limits);
            return Task::none();
        }
        self.avatar_sweep_active = true;
        self.last_avatar_sweep = Some(std::time::Instant::now());
        Task::perform(
            async move {
                let _ = tokio::task::spawn_blocking(move || avatar_cache::sweep(limits)).await;
            },
            |()| Msg::AvatarSweepDone,
        )
    }

    /// One task writes all four TDLib options in order; a newer policy waits.
    fn apply_cache_policy(&mut self) -> Task<Msg> {
        let limits = self.settings.cache;
        if self.session.cache_initializing || !self.session.cache_params_accepted {
            self.session.cache_pending = Some(limits);
            self.session.cache_commit_pending = true;
            return Task::none();
        }
        if self.session.cache_applying.is_some() {
            self.session.cache_pending = Some(limits);
            self.session.cache_commit_pending = true;
            return Task::none();
        }
        self.session.cache_pending = None;
        self.session.cache_commit_pending = false;
        let client = self.session.client_id;
        self.session.cache_applying = Some(limits);
        Task::perform(td::apply_cache_limits(client, limits), move |result| {
            Msg::CachePolicyApplied(client, limits, result)
        })
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
            window::close_requests().map(Msg::WindowCloseRequested),
            window::close_events().map(Msg::WindowClosed),
            Subscription::run(plugins::run).map(Msg::Plugin),
            Subscription::run(notify::clicks)
                .map(|(slot, chat_id)| Msg::NotificationClicked(slot, chat_id)),
            iced::system::theme_changes().map(Msg::SystemTheme),
            iced::time::every(std::time::Duration::from_secs(86_400)).map(|_| Msg::AvatarSweepTick),
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

    /// The chat editor is visible but must not receive keys through another surface.
    fn composer_available(&self, window: WinId, pane: &ChatPane) -> bool {
        self.auth_ready()
            && pane.chat_id.is_some()
            && self.session.leave.is_none()
            && pane.menu.is_none()
            && pane.search.is_none()
            && pane.forward.is_none()
            && pane.delete_selection.is_none()
            && !(pane.profile.is_some()
                && (pane.profile_more || pane.profile_qr.is_some() || pane.confirm_leave))
            && pane.confirm_join.is_none()
            && pane.confirm_link.is_none()
            && pane.list.menu.is_none()
            && pane.list.folder_picker.is_none()
            && pane.list.new_folder_name.is_none()
            && pane.list.confirm_read.is_none()
            && pane.list.archive_settings.is_none()
            && (window != self.main_window
                || (!self.session.settings_open
                    && !self.session.accounts_open
                    && !self.session.confirm_logout))
            && !self
                .session
                .photo_view
                .as_ref()
                .is_some_and(|v| v.window == window)
            && !self
                .session
                .user_card
                .as_ref()
                .is_some_and(|c| c.window == window)
            && !self
                .session
                .video
                .as_ref()
                .is_some_and(|v| v.window == window && v.expanded)
            && !self
                .session
                .playback
                .recording
                .as_ref()
                .is_some_and(|r| r.window == window)
    }

    /// Focus operations are scoped: iced's plain `focus` unfocuses every other
    /// window's input when traversing all window trees.
    fn composer_focus_task(&self, window: WinId, focus: bool) -> Task<Msg> {
        use iced::advanced::widget::operation::{self, focusable};
        let Some(pane) = self.session.panes.get(&window) else {
            return Task::none();
        };
        let root = pane.root_id.clone();
        let operation: Box<dyn iced::advanced::widget::Operation> = if focus {
            Box::new(operation::scope(
                root,
                focusable::focus(pane.compose_id.clone()),
            ))
        } else {
            Box::new(operation::scope(root, focusable::unfocus()))
        };
        iced_runtime::task::effect(iced_runtime::Action::widget(operation))
    }

    pub(crate) fn update(&mut self, msg: Msg) -> Task<Msg> {
        // Only explicit user transitions can take focus. No per-update scan of
        // the panes: background TDLib replies must not move the caret.
        let window = match &msg {
            Msg::Pane(window, _)
            | Msg::WindowFocus(window, _)
            | Msg::Key(window, _, _)
            | Msg::ToggleProfile(window)
            | Msg::OpenProfile(window, _)
            | Msg::Profile(window, _)
            | Msg::ShowArchive(window, _)
            | Msg::ShowFolder(window, _)
            | Msg::ToggleArchiveSettings(window, _)
            | Msg::ViewPhoto(window, _)
            | Msg::ChatMenu(window, _)
            | Msg::ChatOp(window, _, _)
            | Msg::AskReadList(window, _)
            | Msg::ConfirmReadList(window, _, _, _)
            | Msg::OpenFound(window, _, _)
            | Msg::RecordStarted(window, _, _)
            | Msg::MainWindowOpened(window) => Some(*window),
            Msg::SelectTab(_)
            | Msg::CloseTab(_)
            | Msg::CloseAllTabs
            | Msg::OpenChatWindow(_)
            | Msg::CloseSettings
            | Msg::OpenSettings
            | Msg::ToggleAccounts
            | Msg::DismissAccountPopup
            | Msg::LogOut
            | Msg::ConfirmLogOut(_)
            | Msg::NotificationClicked(_, _) => Some(self.main_window),
            Msg::CloseViewer => self.session.photo_view.as_ref().map(|v| v.window),
            Msg::Card(card::CardMsg::Open(window, _)) => Some(*window),
            Msg::Card(_) => self.session.user_card.as_ref().map(|c| c.window),
            Msg::Video(video::VideoMsg::Play(window, ..)) => Some(*window),
            Msg::Video(_) => self.session.video.as_ref().map(|v| v.window),
            Msg::RecordStop(_) => self.session.playback.recording.as_ref().map(|r| r.window),
            _ => None,
        };
        let chat_before = window.and_then(|w| self.session.panes.get(&w).and_then(|p| p.chat_id));
        let was_available = window.is_some_and(|w| {
            self.session
                .panes
                .get(&w)
                .is_some_and(|p| self.composer_available(w, p))
        });
        let explicit_focus = matches!(
            &msg,
            Msg::Pane(
                _,
                PaneMsg::SelectChat(_) | PaneMsg::ClickOutside | PaneMsg::ForwardTo(_)
            ) | Msg::SelectTab(_)
                | Msg::WindowFocus(_, true)
                | Msg::MainWindowOpened(_)
        );
        let task = self.update_inner(msg);
        let Some(window) = window else { return task };
        let available = self
            .session
            .panes
            .get(&window)
            .is_some_and(|p| self.composer_available(window, p));
        if available
            && (explicit_focus
                || !was_available
                || chat_before != self.session.panes[&window].chat_id)
        {
            Task::batch([self.composer_focus_task(window, true), task])
        } else if !available && was_available {
            Task::batch([self.composer_focus_task(window, false), task])
        } else {
            task
        }
    }

    fn update_inner(&mut self, msg: Msg) -> Task<Msg> {
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
            Msg::FileRequestDone(file_id, result) => {
                // TDLib did not take the request: the file is not coming,
                // so it must not read as "downloading" forever, and the
                // paths that ask on their own must leave it alone — only an
                // explicit action (`request_download`) asks again.
                if result.is_err()
                    && let Some(state) = self.session.files.get_mut(&file_id)
                {
                    state.downloading = false;
                    state.failed = true;
                }
                // A viewer waiting for exactly this file must say so
                // instead of spinning on "Загрузка…" forever. The global
                // `self.error` is deliberately left alone: pictures are
                // also fetched automatically (avatars, bubbles), and those
                // must not throw errors at the user for a request they
                // never made.
                if result.is_err() {
                    return self.viewer_file_failed(file_id);
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
            Msg::LogOut => {
                self.session.accounts_open = false;
                self.session.confirm_logout = true;
            }
            Msg::ConfirmLogOut(confirmed) => {
                self.session.confirm_logout = false;
                if confirmed {
                    if self.key_transition_active() {
                        self.error = Some("сначала завершите смену ключа".into());
                        return Task::none();
                    }
                    self.session.auth = Auth::LoggingOut;
                    if let Some(host) = &self.plugin_host {
                        host.send(HostCmd::Account(None));
                    }
                    return Task::perform(td::log_out(self.session.client_id), Msg::Done);
                }
            }
            Msg::OpenSettings => {
                self.session.accounts_open = false;
                self.session.confirm_logout = false;
                self.session.settings_open = true;
                if let Some(pane) = self.session.panes.get_mut(&self.main_window) {
                    pane.menu = None;
                    pane.list.archive_settings = None;
                }
            }
            Msg::ToggleSettingsSection(section) => {
                let expanded = &mut self.session.settings_expanded[section as usize];
                *expanded = !*expanded;
            }
            Msg::CloseSettings => {
                self.session.settings_open = false;
                self.session.password_form.current.clear();
                self.session.password_form.new.clear();
                self.session.password_form.repeat.clear();
                if self.cache_unsaved {
                    self.save_settings();
                }
                if self.session.cache_pending.is_some()
                    || self.session.cache_applied != Some(self.settings.cache)
                {
                    return self.update(Msg::ApplyCachePolicy);
                }
            }
            Msg::SetCachePolicy(limits) => {
                if self.settings.cache == limits {
                    return Task::none();
                }
                self.settings.cache = limits;
                self.cache_unsaved = true;
                self.session.cache_commit_pending = false;
                if self.session.cache_applying.is_some() || self.session.cache_initializing {
                    self.session.cache_pending = Some(limits);
                }
                return self.save_settings_debounced();
            }
            Msg::ApplyCachePolicy => {
                return Task::batch([self.apply_cache_policy(), self.schedule_avatar_sweep()]);
            }
            Msg::CachePolicyApplied(client, limits, result) => {
                if client != self.session.client_id || self.session.cache_applying != Some(limits) {
                    return Task::none();
                }
                self.session.cache_applying = None;
                match result {
                    Ok(()) => {
                        self.session.cache_applied = Some(limits);
                        if self.error.as_deref().is_some_and(|error| {
                            error.starts_with("Не удалось применить настройки хранилища:")
                        }) {
                            self.error = None;
                        }
                    }
                    Err(error) => {
                        self.error =
                            Some(format!("Не удалось применить настройки хранилища: {error}"));
                    }
                }
                if self.session.cache_commit_pending {
                    self.session.cache_commit_pending = false;
                    if let Some(pending) = self.session.cache_pending.take()
                        && (pending != limits || self.session.cache_applied != Some(limits))
                    {
                        return self.apply_cache_policy();
                    }
                }
            }
            Msg::AvatarSweepDone => {
                self.avatar_sweep_active = false;
                if self.avatar_sweep_pending.take().is_some() {
                    return self.schedule_avatar_sweep();
                }
            }
            Msg::AvatarSweepTick => {
                if self
                    .last_avatar_sweep
                    .is_none_or(|last| last.elapsed() >= std::time::Duration::from_secs(86_400))
                {
                    return self.schedule_avatar_sweep();
                }
            }
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
            Msg::WindowCloseRequested(window) => return self.on_window_close_requested(window),
            Msg::MainWindowOpened(window) => {
                if window == self.main_window && self.main_window_state == MainWindowState::Opening
                {
                    self.main_window_state = MainWindowState::Open;
                    return self.focus_main_window();
                }
            }
            Msg::WindowClosed(window) => return self.on_window_closed(window),
            Msg::Key(window, key, modifiers) => return self.on_key(window, key, modifiers),
            Msg::Pane(window, msg) => return self.on_pane(window, msg),
            Msg::Plugin(event) => return self.on_plugin(event),
            Msg::PluginDone(origin, id, action, result) => {
                return self.plugin_done(origin, id, action, result);
            }
            Msg::PluginDeleteOwn(origin, plugin, chat_id, own, revoke, action) => {
                return self.plugin_delete_own(origin, plugin, chat_id, own, revoke, action);
            }
            Msg::PluginDeleted(origin, plugin, chat_id, ids, action, result) => {
                return self.plugin_deleted(origin, plugin, chat_id, ids, action, result);
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
                self.session.accounts_open = false;
                self.session.confirm_logout = false;
                if let Some(pane) = self.session.panes.get_mut(&self.main_window) {
                    pane.list.archive_settings = None;
                }
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
                // Explicit, and sent even while a download is already
                // running: TDLib then raises its priority (the bubble's 16 →
                // 24) instead of fetching anything twice. The button shows
                // "Отмена" while the file is on its way, so only a double
                // click can reach this.
                return self.request_download(file_id, 24);
            }
            Msg::FileCancel(file_id) => {
                // The request is no longer in flight, and the file is not
                // being fetched by anybody else: without this it would keep
                // reading as "downloading" (and never be asked for again)
                // for the rest of the session.
                if let Some(state) = self.session.files.get_mut(&file_id) {
                    state.downloading = false;
                    state.failed = false;
                }
                return Task::perform(td::cancel_download(self.session.client_id, file_id), |()| {
                    Msg::Ignore
                });
            }
            Msg::FileOpen(file_id) => self.open_file(file_id, false),
            Msg::FileShowFolder(file_id) => self.open_file(file_id, true),
            Msg::AvatarDecoded(client_id, file_id, result) => {
                if client_id == self.session.client_id {
                    self.avatar_decoded(file_id, result);
                }
            }
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
                Ok(Some(path))
                    if self
                        .session
                        .panes
                        .get(&window)
                        .is_some_and(|p| self.composer_available(window, p)) =>
                {
                    return self.send_files(window, vec![path]);
                }
                Ok(Some(_) | None) => {}
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
                tray::TrayEvent::Activate => return self.restore_main_window(),
                tray::TrayEvent::Quit => {
                    if self.key_transition_active() {
                        self.error = Some("сначала завершите смену ключа".into());
                        return Task::none();
                    }
                    if self.cache_unsaved {
                        self.save_settings();
                    }
                    return iced::exit();
                }
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
            Msg::ParametersSetForClient(client, initial, result) => {
                if client != self.session.client_id {
                    return Task::none();
                }
                if matches!(result, Ok(None)) {
                    self.session.cache_applied = Some(initial);
                }
                return self.update(Msg::ParametersSet(result));
            }
            Msg::ParametersSet(result) => {
                if matches!(self.session.auth, Auth::Starting | Auth::Locked { .. }) {
                    self.session.busy = false;
                }
                self.session.cache_initializing = false;
                match result {
                    Ok(option_error) => {
                        self.session.cache_params_accepted = true;
                        if let Some(error) = option_error {
                            self.error =
                                Some(format!("Не удалось применить настройки хранилища: {error}"));
                        }
                        if self.pending_transition().is_some() {
                            return self.resume_key_recovery();
                        }
                        if self.session.auth == Auth::Ready || self.session.cache_commit_pending {
                            self.session.cache_pending = None;
                            self.session.cache_commit_pending = false;
                            return self.apply_cache_policy();
                        }
                    }
                    Err(e) => {
                        if self.pending_transition().is_some() {
                            if e.to_lowercase().contains("encryption")
                                && !self.session.recovery_tried_other
                                && let Some((_, target)) = &self.session.recovery_keys
                            {
                                self.session.recovery_tried_other = true;
                                self.session.db_key = target.clone();
                                return self.set_parameters();
                            }
                            self.recovery_failed(format!("база TDLib: {e}"));
                        } else if self.session.db_key.is_some()
                            && e.to_lowercase().contains("encryption")
                        {
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
            Msg::ProfileLoaded(window, chat_id, mut result) => {
                if let Some(pane) = self.session.panes.get_mut(&window)
                    && let Some((shown, data)) = &mut pane.profile
                    && *shown == chat_id
                {
                    if let Ok(profile) = &mut result {
                        for member in &mut profile.members {
                            if self.session.users.contains_key(&member.id) {
                                member.is_bot = self.session.bot_users.contains(&member.id);
                            }
                        }
                    }
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
                let mut dismissed_windows = Vec::new();
                for (other_window, pane) in &mut self.session.panes {
                    if pane
                        .list
                        .confirm_read
                        .as_ref()
                        .is_some_and(|(_, pending)| *pending == list)
                    {
                        pane.list.confirm_read = None;
                        if *other_window != window {
                            dismissed_windows.push(*other_window);
                        }
                    }
                }
                self.session.list_reads.push(ListRead {
                    list: list.clone(),
                    request,
                    window,
                });
                let read = Task::perform(td::read_chat_list(client, list), move |result| {
                    Msg::ListReadDone(window, client, request, result)
                });
                return Task::batch(
                    dismissed_windows
                        .into_iter()
                        .filter(|other| self.composer_available(*other, &self.session.panes[other]))
                        .map(|other| self.composer_focus_task(other, true))
                        .chain(std::iter::once(read)),
                );
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
            Msg::ToggleAccounts => {
                self.session.confirm_logout = false;
                self.session.accounts_open = !self.session.accounts_open;
            }
            Msg::DismissAccountPopup => {
                self.session.accounts_open = false;
                self.session.confirm_logout = false;
            }
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
                return Task::batch([self.show_in_main(chat_id), self.restore_main_window()]);
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
            Msg::RecordStart(window) => {
                if window != self.main_window || self.main_window_state == MainWindowState::Open {
                    return self.record_start(window);
                }
            }
            Msg::RecordStarted(window, chat_id, result) => match result {
                Ok(recorder)
                    if window == self.main_window
                        && self.main_window_state != MainWindowState::Open =>
                {
                    recorder.0.cancel();
                }
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
                if window == self.main_window
                    && (self.session.accounts_open || self.session.confirm_logout)
                {
                    self.session.accounts_open = false;
                    self.session.confirm_logout = false;
                    return Task::none();
                }
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
            Key::Character("v" | "V" | "м" | "М")
                if modifiers.command()
                    && self
                        .session
                        .panes
                        .get(&window)
                        .is_some_and(|p| self.composer_available(window, p)) =>
            {
                use iced::advanced::widget::operation::{self, focusable};
                let pane = &self.session.panes[&window];
                let focused = iced::advanced::widget::operate(operation::scope(
                    pane.root_id.clone(),
                    focusable::is_focused(pane.compose_id.clone()),
                ));
                // The sidebar search also receives Ctrl+V. Only an editor
                // actually holding focus may turn clipboard media into a send.
                return focused.then(move |focused| {
                    if !focused {
                        return Task::none();
                    }
                    Task::perform(
                        async {
                            tokio::task::spawn_blocking(media::paste_image)
                                .await
                                .map_err(|e| e.to_string())
                                .and_then(|r| r)
                        },
                        move |r| Msg::Pasted(window, r),
                    )
                });
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
        let focus = if chat_id.is_some() {
            self.composer_focus_task(window, true)
        } else {
            Task::none()
        };
        Task::batch([open.discard(), focus, select])
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

    fn focus_main_window(&self) -> Task<Msg> {
        #[cfg(target_os = "linux")]
        {
            Task::batch([
                tray::raise_on_hyprland(),
                window::gain_focus(self.main_window),
            ])
        }
        #[cfg(not(target_os = "linux"))]
        {
            window::gain_focus(self.main_window)
        }
    }

    fn restore_main_window(&mut self) -> Task<Msg> {
        match self.main_window_state {
            MainWindowState::Open => self.focus_main_window(),
            MainWindowState::Closing { .. } => {
                self.main_window_state = MainWindowState::Closing { restore: true };
                Task::none()
            }
            MainWindowState::Hidden => {
                self.main_window_state = MainWindowState::Opening;
                iced_runtime::task::oneshot(|channel| {
                    iced_runtime::Action::Window(iced_runtime::window::Action::Open(
                        self.main_window,
                        main_window_settings(),
                        channel,
                    ))
                })
                .map(Msg::MainWindowOpened)
            }
            MainWindowState::Opening => Task::none(),
        }
    }

    fn on_window_close_requested(&mut self, window: WinId) -> Task<Msg> {
        #[cfg(target_os = "linux")]
        {
            if window != self.main_window || self.main_window_state != MainWindowState::Open {
                return Task::none();
            }
            if self.key_transition_active() {
                self.error = Some("сначала завершите смену ключа".into());
                return Task::none();
            }
            if self.cache_unsaved {
                self.save_settings();
            }
            if self.tray.is_none() {
                return iced::exit();
            }
            self.main_window_state = MainWindowState::Closing { restore: false };
            if self.focused == Some(window) {
                self.focused = None;
            }
            Task::batch([self.cancel_recording_in(window), window::close(window)])
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = window;
            Task::none()
        }
    }

    fn on_window_closed(&mut self, window: WinId) -> Task<Msg> {
        if window == self.main_window {
            if self.key_transition_active() {
                // A compositor may destroy the window without sending a
                // cancellable close request; reopen a recovery surface.
                self.error = Some("сначала завершите смену ключа".into());
                self.main_window_state = MainWindowState::Hidden;
                return self.restore_main_window();
            }
            if self.cache_unsaved {
                self.save_settings();
            }
            self.close_video_in(window);
            #[cfg(target_os = "linux")]
            {
                let restore = matches!(
                    self.main_window_state,
                    MainWindowState::Closing { restore: true }
                );
                self.main_window_state = MainWindowState::Hidden;
                if self.focused == Some(window) {
                    self.focused = None;
                }
                let cancel = self.cancel_recording_in(window);
                return if restore {
                    Task::batch([cancel, self.restore_main_window()])
                } else {
                    cancel
                };
            }
            #[cfg(not(target_os = "linux"))]
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
        if self.key_transition_active() {
            self.error = Some("архив ожидает восстановления ключа".into());
            return;
        }
        if let Err(e) = archive.verify_key(self.session.db_key.as_ref()) {
            self.error = Some(format!("ключ архива: {e}"));
            return;
        }
        archive.set_key(self.session.db_key.clone());
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

    fn on_auth_state(&mut self, state: AuthorizationState) -> Task<Msg> {
        self.session.busy = false;
        self.session.input.clear();
        if matches!(state, AuthorizationState::Ready) && self.pending_transition().is_some() {
            self.session.recovery_ready_pending = true;
            if self.session.recovery_keys.is_none() {
                self.session.auth = Auth::Locked {
                    error: Some("смена ключа прервана; введите пароль для восстановления".into()),
                    forgot: false,
                };
            }
            return Task::none();
        }
        self.session.auth = match state {
            AuthorizationState::WaitTdlibParameters => {
                if self.pending_transition().is_some() && self.session.recovery_keys.is_none() {
                    return {
                        self.session.auth = Auth::Locked {
                            error: Some(
                                "смена ключа прервана; введите старый или новый пароль".into(),
                            ),
                            forgot: false,
                        };
                        Task::none()
                    };
                }
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
                    self.apply_cache_policy(),
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
        self.session.cache_initializing = true;
        let client = self.session.client_id;
        let initial = self.settings.cache;
        Task::perform(
            td::set_parameters(
                client,
                self.api_id,
                self.api_hash.clone(),
                self.session.slot,
                crate::lock::tdlib_key(self.session.db_key.as_ref()),
                initial,
            ),
            move |result| Msg::ParametersSetForClient(client, initial, result),
        )
    }

    fn submit_auth(&mut self) -> Task<Msg> {
        if self.session.input.trim().is_empty() || self.session.busy {
            return Task::none();
        }
        let id = self.session.client_id;
        let request = match self.session.auth {
            Auth::Phone => Task::perform(
                td::send_phone(id, self.session.input.trim().to_owned()),
                Msg::Done,
            ),
            Auth::Code => Task::perform(
                td::send_code(id, self.session.input.trim().to_owned()),
                Msg::Done,
            ),
            // Password is not trimmed: spaces may be part of it.
            Auth::Password { .. } => Task::perform(
                td::send_password(id, std::mem::take(&mut self.session.input)),
                Msg::Done,
            ),
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
        // Opening the panel is the user asking for this chat: the explicit
        // action that gives a failed picture a second chance (the sensor
        // alone never retries, see `retry_avatar`).
        self.retry_avatar(avatars::Peer::Chat(chat_id));
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
        if self.key_transition_active() {
            self.error = Some("локальные закрепы недоступны во время смены ключа".into());
            return Task::none();
        }
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

    fn defer_archive_save(&mut self, item: &MsgItem) {
        if !self.settings.keep_deleted || item.pending || item.failed {
            return;
        }
        let row = self
            .session
            .deferred_archive
            .entry((item.chat_id, item.id))
            .or_default();
        if row.purge {
            return;
        }
        row.save = Some(item.clone());
        row.edit = None;
    }

    fn defer_archive_edit(&mut self, chat_id: i64, id: i64, text: String) {
        if self.settings.keep_deleted {
            let row = self
                .session
                .deferred_archive
                .entry((chat_id, id))
                .or_default();
            if !row.purge {
                row.edit = Some(text);
            }
        }
    }

    /// Replay server changes only through a handle verified under the target
    /// key. Keep the entire coalesced queue on any failed write for retry.
    fn flush_deferred_archive(&mut self, archive: &mut Archive) -> Result<(), String> {
        let mut deferred = std::mem::take(&mut self.session.deferred_archive);
        if let Some(pending) = self.pending_transition() {
            for (chat, ids) in pending.deleted_ids {
                for id in ids {
                    let row = deferred.entry((chat, id)).or_default();
                    if !row.purge {
                        row.deleted = true;
                    }
                }
            }
            for (chat, ids) in pending.purged_ids {
                for id in ids {
                    let row = deferred.entry((chat, id)).or_default();
                    row.purge = true;
                    row.deleted = false;
                    row.save = None;
                    row.edit = None;
                }
            }
        }
        let result = (|| {
            let mut purges: HashMap<i64, Vec<i64>> = HashMap::new();
            let mut deletes: HashMap<i64, Vec<i64>> = HashMap::new();
            for (&(chat, id), row) in &deferred {
                if row.purge {
                    purges.entry(chat).or_default().push(id);
                } else if row.deleted {
                    deletes.entry(chat).or_default().push(id);
                }
            }
            for (chat, ids) in purges {
                archive.purge(chat, &ids).map_err(|e| e.to_string())?;
            }
            archive
                .save(
                    deferred
                        .values()
                        .filter(|row| !row.purge)
                        .filter_map(|row| row.save.as_ref()),
                )
                .map_err(|e| e.to_string())?;
            for (&(chat, id), row) in &deferred {
                if !row.purge
                    && let Some(text) = &row.edit
                {
                    archive
                        .set_text(chat, id, text)
                        .map_err(|e| e.to_string())?;
                }
            }
            for (chat, ids) in deletes {
                archive
                    .mark_deleted(chat, &ids)
                    .map_err(|e| e.to_string())?;
            }
            Ok(())
        })();
        if let Err(e) = result {
            self.session.deferred_archive = deferred;
            return Err(e);
        }
        for ((chat, id), row) in deferred {
            if row.purge {
                for pane in self.panes_of(chat) {
                    pane.remove(&[id]);
                }
                self.session.reply_previews.remove(&(chat, id));
            } else if row.deleted {
                for pane in self.panes_of(chat) {
                    pane.mark_deleted(&[id]);
                }
                if let Some(item) = self.session.reply_previews.get_mut(&(chat, id)) {
                    item.deleted = true;
                }
            }
        }
        Ok(())
    }

    /// Archives a confirmed message; pending outgoing ones have temporary ids
    /// and are archived on `MessageSendSucceeded` instead.
    fn archive_save(&mut self, item: &MsgItem) {
        if item.pending || item.failed {
            return;
        }
        if self.key_transition_active() {
            self.defer_archive_save(item);
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
        if self.key_transition_active() {
            let (own, foreign): (Vec<i64>, Vec<i64>) = ids
                .iter()
                .partition(|&&id| self.session.self_deleted.remove(&(chat_id, id)));
            // Deleting a row or setting its flag does not require its text key.
            // While we still own the verified source archive, commit it there
            // before any background rekey can take the connection.
            let direct = self.session.archive.as_mut().map(|archive| {
                if !own.is_empty() {
                    archive.purge(chat_id, &own)?;
                }
                if self.settings.keep_deleted && !foreign.is_empty() {
                    archive.mark_deleted(chat_id, &foreign)?;
                }
                Ok::<(), rusqlite::Error>(())
            });
            if let Some(Err(e)) = &direct {
                self.error = Some(format!("архив: {e}"));
            }
            if !matches!(direct, Some(Ok(())))
                && (!own.is_empty() || self.settings.keep_deleted && !foreign.is_empty())
            {
                let slot = self.session.slot;
                let Some(pending) = self
                    .settings
                    .accounts
                    .iter_mut()
                    .find(|account| account.slot == slot)
                    .and_then(|account| account.pending_key_change.as_mut())
                else {
                    self.error = Some("архив недоступен для удаления".into());
                    self.session
                        .self_deleted
                        .extend(own.into_iter().map(|id| (chat_id, id)));
                    return;
                };
                for &id in &own {
                    pending.purged_ids.entry(chat_id).or_default().insert(id);
                    if let Some(deleted) = pending.deleted_ids.get_mut(&chat_id) {
                        deleted.remove(&id);
                    }
                }
                if self.settings.keep_deleted {
                    for &id in &foreign {
                        if !pending
                            .purged_ids
                            .get(&chat_id)
                            .is_some_and(|ids| ids.contains(&id))
                        {
                            pending.deleted_ids.entry(chat_id).or_default().insert(id);
                        }
                    }
                }
                if let Err(e) = self.try_save_settings() {
                    // Keep the in-memory intent for an explicit retry; the
                    // on-disk outcome after a failed sync may be ambiguous.
                    self.recovery_failed(format!("журнал удаления архива: {e}"));
                    return;
                }
            }
            for id in own {
                let row = self
                    .session
                    .deferred_archive
                    .entry((chat_id, id))
                    .or_default();
                row.purge = true;
                row.save = None;
                row.edit = None;
                row.deleted = false;
            }
            for id in foreign {
                if self.settings.keep_deleted {
                    let row = self
                        .session
                        .deferred_archive
                        .entry((chat_id, id))
                        .or_default();
                    if !row.purge {
                        row.deleted = true;
                    }
                } else {
                    for pane in self.panes_of(chat_id) {
                        pane.remove(&[id]);
                    }
                    self.session.reply_previews.remove(&(chat_id, id));
                }
            }
            return;
        }
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
        if self.key_transition_active() {
            self.error = Some("сначала завершите смену ключа".into());
            return;
        }
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
        if self.key_transition_active() {
            for item in &items {
                self.defer_archive_save(item);
            }
        } else if let Some(archive) =
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
