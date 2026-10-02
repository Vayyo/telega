//! State of one chat view. Every window has exactly one pane: the main window
//! and each separate chat window.

use std::collections::{HashMap, HashSet};

use iced::widget::Id;

use super::{Menu, MsgItem};
use crate::td;

/// Gap between messages in the history column, px.
pub(crate) const SPACING: f32 = 6.0;
/// Height kept free under the history (`view_history`) for the
/// "печатает…" line; the bottom-anchored scrollable counts it as content,
/// so scroll-to-message math below must include it too.
pub(crate) const TYPING_ROOM: f32 = 26.0;
/// Padding of the history column (`view_history`), also above and below
/// the messages themselves.
pub(crate) const HISTORY_PADDING: f32 = 4.0;
/// Assumed viewport before the scrollable has reported its size.
const DEFAULT_VIEW: (f32, f32) = (1400.0, 800.0);

pub(crate) struct ChatPane {
    pub(crate) chat_id: Option<i64>,
    pub(crate) messages: Vec<MsgItem>,
    /// Draft; every window keeps its own. Multi-line: Shift+Enter breaks
    /// the line, Enter sends.
    pub(crate) compose: iced::widget::text_editor::Content,
    /// Stable widget identities for focus operations confined to this window.
    pub(crate) root_id: Id,
    pub(crate) compose_id: Id,
    /// The input field was typed in (or got a loaded draft) since the chat
    /// opened: only then is its text saved as the chat's draft, so an
    /// untouched empty field never wipes a draft made elsewhere.
    pub(crate) compose_touched: bool,
    /// Own message whose text is in the input field for editing, and the
    /// draft it replaced (restored afterwards).
    pub(crate) editing: Option<(i64, String)>,
    /// Message the draft answers.
    pub(crate) reply_to: Option<i64>,
    /// Hidden link waiting for confirmation before it is opened.
    pub(crate) confirm_link: Option<String>,
    /// Invite link waiting for «Вступить»: (link, chat title, members).
    pub(crate) confirm_join: Option<(String, String, i32)>,
    pub(crate) menu: Option<Menu>,
    /// Oldest message id received from TDLib; older pages are requested
    /// before it. `None` while nothing is loaded.
    pub(crate) oldest_loaded: Option<i64>,
    pub(crate) history_complete: bool,
    pub(crate) loading_older: bool,
    /// False after jumping to an old message: newer history is then loaded
    /// page by page while scrolling down, and live messages wait for it.
    pub(crate) newest_loaded: bool,
    pub(crate) loading_newer: bool,
    /// Message to show highlighted (jump target).
    pub(crate) highlight: Option<i64>,
    /// Text selected in a message with the mouse.
    pub(crate) text_selection: Option<TextSelection>,
    /// Emoji and sticker panel above the input field.
    pub(crate) picker: Option<super::picker::Tab>,
    /// Profile panel: the chat and, once loaded, its details.
    pub(crate) profile: Option<(i64, Option<Result<crate::td::Profile, String>>)>,
    /// Profile panel: «Ещё» menu, QR code of the link, «Покинуть» asked.
    pub(crate) profile_more: bool,
    pub(crate) profile_qr: Option<iced::widget::image::Handle>,
    pub(crate) confirm_leave: bool,
    /// Forwarding: messages to forward and the filter of the chat picker.
    pub(crate) forward: Option<(Vec<i64>, String)>,
    /// Selection mode: the chosen messages. `None` outside the mode.
    pub(crate) selected: Option<std::collections::BTreeSet<i64>>,
    /// Deleting the selection: what TDLib allows for all of it (`None`
    /// while asked).
    pub(crate) delete_selection: Option<Option<crate::td::MessageRights>>,
    /// Pinned messages, newest first, and the one the bar shows.
    pub(crate) pinned: Vec<MsgItem>,
    /// Messages the user pinned for themselves (kept locally), most
    /// recently pinned first.
    pub(crate) local_pins: Vec<MsgItem>,
    pub(crate) pinned_shown: usize,
    /// The next jump only scrolls, without highlighting (opening at unread).
    pub(crate) jump_quiet: bool,
    /// «Непрочитанные сообщения» goes above the first message after this id;
    /// fixed when the chat opens.
    pub(crate) unread_after: Option<i64>,
    /// History around this target is loading; other history answers for
    /// this chat are ignored meanwhile, and a stale answer to an earlier,
    /// superseded jump is dropped (checked against this id).
    pub(crate) jump_pending: Option<i64>,
    /// A jump has replaced the newest-page history during this occupancy of
    /// the chat: a still-pending "latest page" answer from opening the chat
    /// must not overwrite it once it lands (`switch_to` clears this again).
    pub(crate) jumped: bool,
    /// Messages whose spoilers were clicked open.
    pub(crate) revealed: HashSet<i64>,
    /// Search inside this chat; `Some` while the search bar is open.
    pub(crate) search: Option<ChatSearch>,
    /// Chat list filter of this window and its results.
    pub(crate) list: ListSearch,
    /// Last scroll position (from the bottom), restored when a background
    /// tab becomes active again.
    pub(crate) scroll: Option<iced::widget::scrollable::AbsoluteOffset>,
    /// Id of the history scrollable, for scrolling it from code.
    pub(crate) scroll_id: Id,
    /// Size of the history viewport as last reported: (height, width).
    pub(crate) view_size: Option<(f32, f32)>,
    /// Measured heights of rendered messages, by id.
    pub(crate) heights: HashMap<i64, f32>,
    /// Options picked so far in a poll that allows several answers, by
    /// message id, before «Голосовать» sends them.
    pub(crate) poll_selection: HashMap<i64, std::collections::BTreeSet<i32>>,
}

impl Default for ChatPane {
    fn default() -> Self {
        Self {
            chat_id: None,
            messages: Vec::new(),
            compose: iced::widget::text_editor::Content::new(),
            root_id: Id::unique(),
            compose_id: Id::unique(),
            editing: None,
            compose_touched: false,
            reply_to: None,
            confirm_link: None,
            confirm_join: None,
            menu: None,
            oldest_loaded: None,
            history_complete: false,
            loading_older: false,
            newest_loaded: true,
            loading_newer: false,
            highlight: None,
            jump_quiet: false,
            pinned: Vec::new(),
            local_pins: Vec::new(),
            selected: None,
            forward: None,
            profile: None,
            profile_more: false,
            profile_qr: None,
            confirm_leave: false,
            picker: None,
            text_selection: None,
            delete_selection: None,
            pinned_shown: 0,
            unread_after: None,
            jump_pending: None,
            jumped: false,
            revealed: HashSet::new(),
            search: None,
            list: ListSearch::default(),
            scroll: None,
            scroll_id: Id::unique(),
            view_size: None,
            heights: HashMap::new(),
            poll_selection: HashMap::new(),
        }
    }
}

pub(crate) struct ChatSearch {
    pub(crate) query: String,
    /// `None` until the query is submitted.
    pub(crate) results: Option<Vec<MsgItem>>,
    pub(crate) paging: SearchPaging<i64>,
    /// Search results have their own scroll position, independent of history.
    pub(crate) scroll: f32,
    pub(crate) view_height: Option<f32>,
    /// A fresh id resets iced's viewport when the query changes or reopens.
    pub(crate) scroll_id: Id,
}

impl Default for ChatSearch {
    fn default() -> Self {
        Self {
            query: String::new(),
            results: None,
            paging: SearchPaging::default(),
            scroll: 0.0,
            view_height: None,
            scroll_id: Id::unique(),
        }
    }
}

/// A request is identified by a fresh ID even if its query and cursor are
/// reused after reopening the search. `None` cursor denotes the first page.
pub(crate) struct SearchPaging<C> {
    pub(crate) next: Option<C>,
    pub(crate) pending: Option<(u64, Option<C>)>,
    pub(crate) error: Option<String>,
}

impl<C> Default for SearchPaging<C> {
    fn default() -> Self {
        Self {
            next: None,
            pending: None,
            error: None,
        }
    }
}

impl<C: PartialEq> SearchPaging<C> {
    pub(crate) fn accepts(&self, request: u64, cursor: &Option<C>) -> bool {
        self.pending
            .as_ref()
            .is_some_and(|(id, expected)| *id == request && expected == cursor)
    }

    pub(crate) fn finish(
        &mut self,
        results: &mut Option<Vec<MsgItem>>,
        page: Vec<tdlib_rs::types::Message>,
        next: Option<C>,
        requested: &Option<C>,
    ) {
        let items = results.get_or_insert_with(Vec::new);
        let mut seen: HashSet<(i64, i64)> = items.iter().map(|m| (m.chat_id, m.id)).collect();
        let before = items.len();
        items.extend(
            page.iter()
                .filter(|m| seen.insert((m.chat_id, m.id)))
                .map(MsgItem::from),
        );
        // TDLib chooses page size freely. Only its cursor ends pagination;
        // a repeated cursor or a page adding nothing cannot make progress.
        self.next = if next.as_ref() == requested.as_ref() || items.len() == before {
            None
        } else {
            next
        };
        self.pending = None;
        self.error = None;
    }
}

/// Filter of the chat list: Archive matches loaded archive titles and searches
/// archive messages on Enter; other lists retain global TDLib search.
pub(crate) struct ListSearch {
    /// The archived chats are shown instead of the main list.
    pub(crate) archive: bool,
    /// Telegram folder shown instead of all chats.
    pub(crate) folder: Option<i32>,
    /// Archive settings replace this window's chat pane, not its chat list.
    pub(crate) archive_settings: Option<ArchiveSettings>,
    /// One confirmation for this window and the list selected when it opened.
    pub(crate) confirm_read: Option<(u64, tdlib_rs::enums::ChatList)>,
    /// Request feedback for this window; counters remain server-owned.
    pub(crate) read_feedback: Option<Result<(), String>>,
    /// Chat whose context menu is open in the list.
    pub(crate) menu: Option<i64>,
    /// Expanded folder actions for this menu, owned by this window only.
    pub(crate) folder_picker: Option<FolderPicker>,
    /// New-folder form is part of the existing menu, never an optimistic tab.
    pub(crate) new_folder_name: Option<String>,
    pub(crate) new_folder_error: Option<String>,
    /// Which dispatched create still owns this invocation of the form.
    pub(crate) new_folder_request: Option<u64>,
    pub(crate) query: String,
    pub(crate) chats: Vec<i64>,
    pub(crate) messages: Option<Vec<MsgItem>>,
    pub(crate) paging: SearchPaging<String>,
    /// Scroll offset of the chat list scrollable, for virtualization.
    pub(crate) scroll: f32,
    /// Stable id of this window's sidebar scrollable, for programmatic resets.
    pub(crate) scroll_id: Id,
    /// Height of the chat list viewport as last reported, for virtualization.
    pub(crate) view_height: Option<f32>,
}

impl Default for ListSearch {
    fn default() -> Self {
        Self {
            archive: false,
            archive_settings: None,
            folder: None,
            confirm_read: None,
            read_feedback: None,
            menu: None,
            folder_picker: None,
            new_folder_name: None,
            new_folder_error: None,
            new_folder_request: None,
            query: String::new(),
            chats: Vec::new(),
            messages: None,
            paging: SearchPaging::default(),
            scroll: 0.0,
            scroll_id: Id::unique(),
            view_height: None,
        }
    }
}

/// The current sidebar picker. A request token rejects answers to an older
/// invocation, even if the same chat and folder are reopened immediately.
pub(crate) struct FolderPicker {
    pub(crate) chat_id: i64,
    pub(crate) pending: Option<(u64, i32)>,
    pub(crate) feedback: Option<String>,
}

/// Server-owned values; errors and pending requests never change the last
/// displayed settings. `request` invalidates replies from a closed panel.
pub(crate) struct ArchiveSettings {
    pub(crate) request: u64,
    pub(crate) loading: bool,
    pub(crate) saving: bool,
    pub(crate) settings: Option<tdlib_rs::types::ArchiveChatListSettings>,
    pub(crate) capability: Option<Result<bool, String>>,
    pub(crate) error: Option<String>,
}
/// Which messages to build: `messages[start..end]`, with empty space of the
/// given heights standing in for the rest.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Visible {
    pub(crate) start: usize,
    pub(crate) end: usize,
    pub(crate) above: f32,
    pub(crate) below: f32,
}

/// Height guess for a message never shown yet: padding + sender line +
/// wrapped text lines at the bubble width.
fn estimate(m: &MsgItem, width: f32) -> f32 {
    let bubble = width.min(520.0) - 16.0;
    let per_line = (bubble / 8.5).max(20.0);
    let lines: f32 = m
        .text
        .split('\n')
        .map(|l| (l.chars().count() as f32 / per_line).ceil().max(1.0))
        .sum();
    16.0 + 16.0
        + lines * 21.0
        + if m.deleted { 15.0 } else { 0.0 }
        + m.media.as_ref().map_or(0.0, |media| media.height())
}

/// A mouse selection within one message's text (byte offsets).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TextSelection {
    pub(crate) message: i64,
    pub(crate) anchor: usize,
    pub(crate) focus: usize,
    /// The button is still down.
    pub(crate) dragging: bool,
}

impl ChatPane {
    /// The selected text, if something is selected.
    pub(crate) fn selected_text(&self) -> Option<String> {
        let sel = self.text_selection?;
        if sel.anchor == sel.focus {
            return None;
        }
        let m = self.messages.iter().find(|m| m.id == sel.message)?;
        let text: String = m.rich.iter().map(|p| p.text.as_str()).collect();
        Some(super::selectable::selected(&text, sel.anchor, sel.focus).to_owned())
    }

    /// What the pinned bar cycles through: own pins first, then the chat's
    /// (a message pinned both ways counts once, as own). `true` = own.
    pub(crate) fn pins(&self) -> Vec<(&MsgItem, bool)> {
        let own = self.local_pins.iter().map(|m| (m, true));
        let chat = self
            .pinned
            .iter()
            .filter(|m| !self.local_pins.iter().any(|l| l.id == m.id))
            .map(|m| (m, false));
        own.chain(chat).collect()
    }

    pub(crate) fn pinned_locally(&self, id: i64) -> bool {
        self.local_pins.iter().any(|m| m.id == id)
    }

    /// What to save as the chat's draft, if the field was touched: the
    /// draft set aside while an own message is being edited, else the field.
    pub(crate) fn draft(&self) -> Option<String> {
        if !self.compose_touched {
            return None;
        }
        Some(match &self.editing {
            Some((_, draft)) => draft.clone(),
            None => self.compose.text(),
        })
    }

    /// Leaves edit mode: the draft from before comes back.
    pub(crate) fn finish_edit(&mut self) {
        if let Some((_, draft)) = self.editing.take() {
            self.compose = iced::widget::text_editor::Content::with_text(&draft);
            self.compose
                .perform(iced::widget::text_editor::Action::Move(
                    iced::widget::text_editor::Motion::DocumentEnd,
                ));
        }
    }

    pub(crate) fn shows(&self, chat_id: i64) -> bool {
        self.chat_id == Some(chat_id)
    }

    /// Switches to another chat, dropping everything of the previous one
    /// except the window's chat list filter.
    pub(crate) fn switch_to(&mut self, chat_id: i64) {
        let list = std::mem::take(&mut self.list);
        let root_id = self.root_id.clone();
        let compose_id = self.compose_id.clone();
        *self = Self {
            chat_id: Some(chat_id),
            list,
            root_id,
            compose_id,
            ..Self::default()
        };
    }

    /// Scroll offset (from the bottom, as the history is anchored) that
    /// brings message `id` to the middle of the viewport.
    pub(crate) fn offset_of(&self, id: i64) -> Option<f32> {
        let (height, width) = self.view_size.unwrap_or(DEFAULT_VIEW);
        // The bottom-anchored column reserves this much space under the
        // newest message (typing room, then the column's own padding), so a
        // message is never really flush with the scrollable's bottom edge.
        let mut from_bottom = TYPING_ROOM + HISTORY_PADDING;
        for m in self.messages.iter().rev() {
            let h = self.height_of(m, width) + SPACING;
            if m.id == id {
                return Some((from_bottom + h / 2.0 - height / 2.0).max(0.0));
            }
            from_bottom += h;
        }
        None
    }

    /// Appends a page of newer history (after a jump), skipping duplicates.
    pub(crate) fn append_page(&mut self, items: Vec<MsgItem>, reached_newest: bool) {
        let last = self.messages.last().map_or(i64::MIN, |m| m.id);
        self.messages
            .extend(items.into_iter().filter(|m| m.id > last));
        self.newest_loaded = reached_newest;
        self.loading_newer = false;
    }

    /// Height of a message at the current viewport width (measured or
    /// estimated), including the gap after it.
    pub(crate) fn height_estimate(&self, m: &MsgItem) -> f32 {
        let (_, width) = self.view_size.unwrap_or(DEFAULT_VIEW);
        self.height_of(m, width) + SPACING
    }

    fn height_of(&self, m: &MsgItem, width: f32) -> f32 {
        self.heights
            .get(&m.id)
            .copied()
            .unwrap_or_else(|| estimate(m, width))
    }

    /// Virtualization: only messages within the viewport plus a margin of
    /// roughly one screen on each side are built; the history is anchored to
    /// the bottom, so positions are counted from the newest message up.
    pub(crate) fn visible(&self) -> Visible {
        let (height, width) = self.view_size.unwrap_or(DEFAULT_VIEW);
        let offset = self.scroll.map_or(0.0, |s| s.y);
        let margin = height.max(400.0);
        let (low, high) = (offset - margin, offset + height + margin);

        let n = self.messages.len();
        let (mut start, mut end) = (n, n);
        let (mut above, mut below) = (0.0, 0.0);
        // Distance from the bottom of the list (including the reserved
        // typing room and padding below the newest message) to the bottom
        // of message `i`.
        let mut from_bottom = TYPING_ROOM + HISTORY_PADDING;
        for (i, m) in self.messages.iter().enumerate().rev() {
            let h = self.height_of(m, width) + SPACING;
            if from_bottom + h < low {
                below += h;
                end = i;
            } else if from_bottom > high {
                above += h;
            } else {
                start = i;
            }
            from_bottom += h;
        }
        if start > end {
            // Nothing in range (empty chat): build nothing.
            start = end;
        }
        Visible {
            start,
            end,
            above,
            below,
        }
    }

    /// Replaces the history with the first page; `pending` outgoing messages
    /// stay at the end, as TDLib shows them.
    pub(crate) fn show_page(&mut self, mut items: Vec<MsgItem>, pending: Vec<MsgItem>, page: Page) {
        items.extend(pending);
        self.messages = items;
        self.track(page);
    }

    pub(crate) fn prepend_page(&mut self, mut items: Vec<MsgItem>, page: Page) {
        items.append(&mut self.messages);
        self.messages = items;
        self.loading_older = false;
        self.track(page);
    }

    fn track(&mut self, page: Page) {
        if let Some(first) = page.first_id {
            self.oldest_loaded = Some(first);
        }
        self.history_complete = page.len < td::HISTORY_LIMIT;
    }

    /// Id before which the next older page is requested, if any is left.
    pub(crate) fn next_older(&self) -> Option<i64> {
        if self.loading_older || self.history_complete {
            return None;
        }
        self.oldest_loaded
    }

    pub(crate) fn mark_deleted(&mut self, ids: &[i64]) {
        for item in &mut self.messages {
            if ids.contains(&item.id) {
                item.deleted = true;
            }
        }
    }

    pub(crate) fn remove(&mut self, ids: &[i64]) {
        self.messages.retain(|m| !ids.contains(&m.id));
    }

    pub(crate) fn find_mut(&mut self, id: i64) -> Option<&mut MsgItem> {
        self.messages.iter_mut().find(|m| m.id == id)
    }
}

/// What a history page from TDLib looked like, for paging bookkeeping.
#[derive(Clone, Copy)]
pub(crate) struct Page {
    pub(crate) len: usize,
    pub(crate) first_id: Option<i64>,
}

#[cfg(test)]
mod tests {
    use tdlib_rs::enums::MessageSender;
    use tdlib_rs::types::MessageSenderUser;

    use super::*;

    /// Pane with `n` messages of measured height 50 and a 500 px viewport
    /// scrolled `offset` px up from the bottom.
    fn pane(n: i64, offset: f32) -> ChatPane {
        let mut pane = ChatPane::default();
        pane.switch_to(1);
        for id in 1..=n {
            pane.messages.push(MsgItem {
                chat_id: 1,
                id,
                sender: MessageSender::User(MessageSenderUser { user_id: 7 }),
                text: "m".into(),
                outgoing: false,
                deleted: false,
                media: None,
                pending: false,
                failed: false,
                rich: Vec::new(),
                reply_to: None,
                date: 0,
                edited: false,
                reactions: Vec::new(),
                forwarded: None,
                extra: None,
                sender_tag: String::new(),
            });
            pane.heights.insert(id, 50.0);
        }
        pane.view_size = Some((500.0, 800.0));
        pane.scroll = Some(iced::widget::scrollable::AbsoluteOffset { x: 0.0, y: offset });
        pane
    }

    const ROW: f32 = 50.0 + SPACING;

    #[test]
    fn only_messages_near_the_viewport_are_built() {
        let p = pane(2000, 0.0);
        let v = p.visible();
        // At the bottom: the viewport plus one screen above it (500 px each),
        // nothing below.
        assert_eq!(v.end, 2000);
        assert_eq!(v.below, 0.0);
        let built = v.end - v.start;
        assert!((15..=25).contains(&built), "built {built}");
        // Space stands in for everything else, so the total height is exact.
        assert_eq!(v.above + built as f32 * ROW, 2000.0 * ROW);
    }

    #[test]
    fn scrolled_up_range_follows_the_viewport() {
        let offset = 1000.0 * ROW;
        let p = pane(2000, offset);
        let v = p.visible();
        assert!(v.start < 1000 && v.end > 1000, "{v:?}");
        assert!(v.end - v.start <= 30, "{v:?}");
        let built = (v.end - v.start) as f32 * ROW;
        assert!((v.above + built + v.below - 2000.0 * ROW).abs() < 0.01);
        // Everything below the built range is covered by the bottom space.
        assert_eq!(v.below, (2000 - v.end) as f32 * ROW);
    }

    #[test]
    fn unmeasured_messages_use_an_estimate_growing_with_text() {
        let mut p = pane(3, 0.0);
        p.heights.clear();
        p.messages[0].text = "слово ".repeat(200);
        let short = p.height_of(&p.messages[1], 800.0);
        let long = p.height_of(&p.messages[0], 800.0);
        assert!(long > short * 5.0, "{short} vs {long}");
    }

    #[test]
    fn short_chat_is_built_entirely() {
        let v = pane(5, 0.0).visible();
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
}
