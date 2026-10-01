//! State of one TDLib client / logged-in account. Everything here is reset
//! to a blank `Session` on every fresh log-in, account switch, or log-out
//! (see `App::start_session`), instead of a hand-maintained list of fields
//! to carry over from the old one. State that outlives the session (open
//! windows, look, settings, the plugin host…) stays directly on `App`.

use std::cmp::Reverse;
use std::collections::{BTreeSet, HashMap, HashSet};

use crate::archive::Archive;
use crate::td;

use super::accounts::Leave;
use super::avatars::Avatars;
use super::card::UserCard;
use super::media::{FileState, ImageCache};
use super::pane::ChatPane;
use super::password;
use super::picker::Catalog;
use super::playback::Playback;
use super::tabs::Tabs;
use super::typing::Typing;
use super::video::VideoPlayback;
use super::viewer::PhotoView;
use super::{Auth, ChatItem, MsgItem, WinId};

/// One reorder owns the controls until the requested order is observed after
/// TDLib accepts it, or the user acknowledges the unresolved result.
pub(super) struct FolderReorder {
    pub(super) window: WinId,
    pub(super) request: u64,
    pub(super) expected: Vec<i32>,
    pub(super) main_tab: i32,
    /// The order at dispatch distinguishes a metadata-only update from an
    /// unexpected order change; neither is proof that the request completed.
    pub(super) initial: Vec<i32>,
    pub(super) initial_main_tab: usize,
    pub(super) state: FolderReorderState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FolderReorderState {
    AwaitingReply,
    MatchedBeforeReply,
    AwaitingUpdate,
    DiscrepancyBeforeReply,
    DiscrepancyAfterReply,
}

/// A create request remains locked across windows until its returned ID
/// appears in the authoritative folder update (which may precede the reply).
pub(super) struct FolderCreation {
    pub(super) window: WinId,
    pub(super) chat_id: i64,
    pub(super) request: u64,
    pub(super) id: Option<i32>,
    pub(super) observed: HashSet<i32>,
    pub(super) initial: HashSet<i32>,
}

/// One TDLib readChatList task, shared across all windows of this session.
pub(super) struct ListRead {
    pub(super) list: tdlib_rs::enums::ChatList,
    pub(super) request: u64,
    pub(super) window: WinId,
}

pub(super) struct Session {
    pub(super) client_id: i32,
    pub(super) auth: Auth,
    pub(super) input: String,
    pub(super) busy: bool,
    /// Option writes start only after parameters have been accepted.
    pub(super) cache_params_accepted: bool,
    pub(super) cache_initializing: bool,
    /// One policy write at a time for this client, with the latest queued value.
    pub(super) cache_applying: Option<crate::settings::CacheLimits>,
    pub(super) cache_pending: Option<crate::settings::CacheLimits>,
    /// Drag changes wait for release before launching their queued option group.
    pub(super) cache_commit_pending: bool,
    pub(super) cache_applied: Option<crate::settings::CacheLimits>,
    pub(super) chats: HashMap<i64, ChatItem>,
    /// Main chat list sorted like Telegram: by descending TDLib `order`.
    pub(super) order: BTreeSet<(Reverse<i64>, i64)>,
    /// The archived chats, the same way.
    pub(super) archived: BTreeSet<(Reverse<i64>, i64)>,
    /// Client-only chat pins, newest first, loaded once from the account DB.
    pub(super) local_main: Vec<i64>,
    pub(super) local_archive: Vec<i64>,
    /// Both local pin lists are disabled until a fresh successful load.
    pub(super) local_chat_pins_failed: bool,
    /// Telegram folders in the user's order: (id, name); the "all chats"
    /// tab goes at `main_tab`.
    pub(super) folders: Vec<(i32, String)>,
    pub(super) main_tab: usize,
    /// One reorder request across every window; replies never change the
    /// displayed order, which always comes from `Update::ChatFolders`.
    pub(super) folder_reorder: Option<FolderReorder>,
    pub(super) next_folder_reorder: u64,
    pub(super) folder_reorder_error: Option<String>,
    pub(super) folder_creation: Option<FolderCreation>,
    pub(super) next_folder_creation: u64,
    pub(super) folder_creation_warning: Option<String>,
    pub(super) next_list_read: u64,
    pub(super) list_reads: Vec<ListRead>,
    /// A single action-time read → write → readback owns archive settings
    /// across all windows of this account until it finishes or is cancelled.
    pub(super) archive_write: Option<(WinId, u64)>,
    pub(super) next_archive_request: u64,
    /// Chats of each folder, sorted like `order`.
    pub(super) folder_lists: HashMap<i32, BTreeSet<(Reverse<i64>, i64)>>,
    /// Unread messages per folder: (all, in chats that notify).
    pub(super) folder_unread: HashMap<i32, (i32, i32)>,
    /// One edit per folder across all windows; kept until its TDLib answer.
    pub(super) folder_edits: HashMap<i32, u64>,
    pub(super) next_folder_edit: u64,
    /// Stand-in for a folder whose chats are not loaded yet.
    pub(super) empty_list: BTreeSet<(Reverse<i64>, i64)>,
    pub(super) users: HashMap<i64, String>,
    /// TDLib users currently reported as bots; resolved by user id, not chat id.
    pub(super) bot_users: HashSet<i64>,
    /// One pane per open window, the main one included.
    pub(super) panes: HashMap<WinId, ChatPane>,
    pub(super) confirm_logout: bool,
    /// Opened once the account id is known (`my_id` option).
    pub(super) archive: Option<Archive>,
    /// Messages the user deleted from this client: on deletion they are
    /// dropped instead of archived, since the user wanted them gone.
    pub(super) self_deleted: HashSet<(i64, i64)>,
    /// Settings page replaces the chat pane of the main window.
    pub(super) settings_open: bool,
    /// Raw text typed into numeric plugin settings, keyed by (plugin id,
    /// setting key); shown instead of the reformatted committed value.
    pub(super) plugin_number_drafts: HashMap<(String, String), String>,
    pub(super) plugin_help_open: bool,
    /// Logged-in account id (`my_id` option).
    pub(super) my_id: Option<i64>,
    pub(super) tabs: Tabs,
    /// Panes of background tabs, by chat.
    pub(super) background: HashMap<i64, ChatPane>,
    /// TDLib files referenced by shown messages.
    pub(super) files: HashMap<i32, FileState>,
    pub(super) images: ImageCache,
    /// Photos that were on screen: decoded as soon as their download ends.
    pub(super) wanted_photos: HashSet<i32>,
    /// Replied-to messages not loaded in any pane: (chat, id) → preview.
    pub(super) reply_previews: HashMap<(i64, i64), MsgItem>,
    pub(super) playback: Playback,
    /// Unread messages in the main chat list: all, and in chats that notify.
    pub(super) unread: (i32, i32),
    /// Data slot of the running account.
    pub(super) slot: u32,
    /// Set when the client is being closed on purpose (switch, cancel).
    pub(super) leave: Option<Leave>,
    /// TDLib reported a log out; the closed account is forgotten.
    pub(super) logging_out: bool,
    /// Account list under the chat list header.
    pub(super) accounts_open: bool,
    pub(super) typing: Typing,
    pub(super) avatars: Avatars,
    /// Full-window photo viewer, if open.
    pub(super) photo_view: Option<PhotoView>,
    /// The video playing (in its bubble or over the window).
    pub(super) video: Option<VideoPlayback>,
    /// Mini profile of a user, if open.
    pub(super) user_card: Option<UserCard>,
    /// Short confirmation shown under the chat («Скопировано»).
    pub(super) notice: Option<String>,
    pub(super) stickers: Catalog,
    /// Input field text last saved as (or loaded from) each chat's draft;
    /// unchanged text is not saved again.
    pub(super) synced_drafts: HashMap<i64, String>,
    /// Roles and tags of group members, by chat, loaded when a group opens.
    pub(super) roles: HashMap<i64, HashMap<i64, td::Role>>,
    /// Key of the client password of the running account, once unlocked.
    pub(super) db_key: Option<crate::lock::Key>,
    pub(super) password_form: password::Form,
    /// The archive, taken out while a password change re-keys it on a
    /// worker thread; the connection comes back through this channel
    /// rather than through a `Msg` (which must stay `Clone`).
    pub(super) rekeying_archive: Option<tokio::sync::oneshot::Receiver<Archive>>,
    /// Bumped by `save_settings_debounced`; a scheduled `FlushSettings`
    /// whose token no longer matches lost the race to a newer one and is
    /// dropped, so a burst of changes (switching tabs repeatedly) writes
    /// `settings.json` once, shortly after it settles, not on every change.
    pub(super) settings_save_token: u64,
}

impl Session {
    /// Fresh state for a new TDLib client on `slot`, with `main_window`'s
    /// pane ready. A closed client cannot be reused, so log out and account
    /// switches start a new one in the same main window.
    pub(super) fn new(client_id: i32, main_window: WinId, slot: u32) -> Self {
        Self {
            client_id,
            auth: Auth::Starting,
            input: String::new(),
            busy: false,
            cache_params_accepted: false,
            cache_initializing: false,
            cache_applying: None,
            cache_pending: None,
            cache_commit_pending: false,
            cache_applied: None,
            chats: HashMap::new(),
            order: BTreeSet::new(),
            archived: BTreeSet::new(),
            local_main: Vec::new(),
            local_archive: Vec::new(),
            local_chat_pins_failed: false,
            folders: Vec::new(),
            main_tab: 0,
            folder_reorder: None,
            next_folder_reorder: 0,
            folder_reorder_error: None,
            folder_creation: None,
            next_folder_creation: 0,
            folder_creation_warning: None,
            next_list_read: 0,
            list_reads: Vec::new(),
            archive_write: None,
            next_archive_request: 0,
            folder_lists: HashMap::new(),
            folder_unread: HashMap::new(),
            folder_edits: HashMap::new(),
            next_folder_edit: 0,
            empty_list: BTreeSet::new(),
            users: HashMap::new(),
            bot_users: HashSet::new(),
            panes: HashMap::from([(main_window, ChatPane::default())]),
            confirm_logout: false,
            archive: None,
            self_deleted: HashSet::new(),
            settings_open: false,
            plugin_number_drafts: HashMap::new(),
            plugin_help_open: false,
            my_id: None,
            tabs: Tabs::default(),
            background: HashMap::new(),
            files: HashMap::new(),
            images: ImageCache::default(),
            wanted_photos: HashSet::new(),
            reply_previews: HashMap::new(),
            playback: Playback::default(),
            unread: (0, 0),
            slot,
            leave: None,
            logging_out: false,
            accounts_open: false,
            typing: Typing::default(),
            avatars: Avatars::default(),
            photo_view: None,
            video: None,
            user_card: None,
            notice: None,
            stickers: Catalog::default(),
            synced_drafts: HashMap::new(),
            roles: HashMap::new(),
            db_key: None,
            password_form: password::Form::default(),
            rekeying_archive: None,
            settings_save_token: 0,
        }
    }

    /// Swap two folder IDs; the main-list slot and every other ID are kept.
    pub(super) fn reordered_folders(&self, folder: i32, up: bool) -> Option<(Vec<i32>, i32)> {
        let index = self.folders.iter().position(|(id, _)| *id == folder)?;
        let other = if up {
            index.checked_sub(1)?
        } else {
            index.checked_add(1).filter(|&i| i < self.folders.len())?
        };
        let main_tab = i32::try_from(self.main_tab).ok()?;
        let mut ids: Vec<i32> = self.folders.iter().map(|(id, _)| *id).collect();
        ids.swap(index, other);
        Some((ids, main_tab))
    }
}
