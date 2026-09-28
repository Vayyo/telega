//! Moving around history: replies, jumps to a message, newer pages after a
//! jump, search in a chat and across chats.

use std::sync::atomic::{AtomicU64, Ordering};

use super::{App, Msg, MsgItem, PaneMsg, WinId};
use crate::td;
use iced::Task;
use iced::widget::operation;
use iced::widget::scrollable::AbsoluteOffset;
use tdlib_rs::{enums::ChatList, types::Message};

static SEARCH_REQUEST: AtomicU64 = AtomicU64::new(1);

fn request_id() -> u64 {
    SEARCH_REQUEST.fetch_add(1, Ordering::Relaxed)
}

/// TDLib accepts an archive list for message search; `None` keeps the
/// existing global search for the main list and folders.
pub(super) fn list_search_scope(archive: bool) -> Option<ChatList> {
    archive.then_some(ChatList::Archive)
}

impl App {
    /// A replied message: from any loaded history, else from the preview
    /// cache (filled by `fetch_replies`).
    pub(super) fn reply_lookup(&self, chat_id: i64, id: i64) -> Option<&MsgItem> {
        self.session
            .panes
            .values()
            .chain(self.session.background.values())
            .filter(|p| p.shows(chat_id))
            .find_map(|p| p.messages.iter().find(|m| m.id == id))
            .or_else(|| self.session.reply_previews.get(&(chat_id, id)))
    }

    /// Loads previews of replied messages that are not at hand.
    pub(super) fn fetch_replies(&self, chat_id: i64, items: &[MsgItem]) -> Task<Msg> {
        let mut missing: Vec<i64> = items
            .iter()
            .filter_map(|m| m.reply_to)
            .filter(|&id| self.reply_lookup(chat_id, id).is_none())
            .collect();
        missing.sort_unstable();
        missing.dedup();
        if missing.is_empty() {
            return Task::none();
        }
        Task::perform(
            td::get_messages(self.session.client_id, chat_id, missing),
            |r| Msg::RepliesLoaded(r.unwrap_or_default()),
        )
    }

    pub(super) fn store_replies(&mut self, messages: Vec<Message>) {
        for m in messages {
            self.note_files(&m.content);
            self.session
                .reply_previews
                .insert((m.chat_id, m.id), MsgItem::from(&m));
        }
    }

    /// Scrolls to a message: directly if loaded, else after loading the
    /// history around it.
    pub(super) fn jump(&mut self, window: WinId, id: i64) -> Task<Msg> {
        let Some(pane) = self.session.panes.get_mut(&window) else {
            return Task::none();
        };
        let Some(chat_id) = pane.chat_id else {
            return Task::none();
        };
        if !pane.jump_quiet {
            pane.highlight = Some(id);
        }
        if let Some(search) = &mut pane.search {
            search.results = None;
            search.paging = Default::default();
        }
        if let Some(offset) = pane.offset_of(id) {
            pane.jump_quiet = false;
            pane.scroll = Some(AbsoluteOffset { x: 0.0, y: offset });
            return operation::scroll_to(
                pane.scroll_id.clone(),
                AbsoluteOffset { x: 0.0, y: offset },
            );
        }
        pane.jump_pending = Some(id);
        Task::perform(
            td::history_around(self.session.client_id, chat_id, id),
            move |r| Msg::Pane(window, PaneMsg::AroundLoaded(chat_id, id, r)),
        )
    }

    pub(super) fn show_around(
        &mut self,
        window: WinId,
        chat_id: i64,
        target: i64,
        messages: &[Message],
    ) -> Task<Msg> {
        let newest = self.session.chats.get(&chat_id).map_or(0, |c| c.last_id);
        let reached_newest = messages.last().is_some_and(|m| m.id >= newest);
        let lower = messages.first().map_or(i64::MIN, |m| m.id);
        let upper = if reached_newest {
            i64::MAX
        } else {
            messages.last().map_or(i64::MAX, |m| m.id + 1)
        };
        let (items, _) = self.merge_range(chat_id, messages, lower, upper);
        let replies = self.fetch_replies(chat_id, &items);
        // Drop a stale answer: a later jump already changed what is awaited.
        let Some(pane) = self
            .session
            .panes
            .get_mut(&window)
            .filter(|p| p.shows(chat_id) && p.jump_pending == Some(target))
        else {
            return Task::none();
        };
        pane.jump_pending = None;
        pane.jumped = true;
        pane.messages = items;
        pane.heights.clear();
        pane.oldest_loaded = messages.first().map(|m| m.id);
        pane.history_complete = false;
        pane.loading_older = false;
        pane.newest_loaded = reached_newest;
        pane.loading_newer = false;
        if !std::mem::take(&mut pane.jump_quiet) {
            pane.highlight = Some(target);
        }
        let offset = pane.offset_of(target).unwrap_or(0.0);
        pane.scroll = Some(AbsoluteOffset { x: 0.0, y: offset });
        Task::batch([
            operation::scroll_to(pane.scroll_id.clone(), AbsoluteOffset { x: 0.0, y: offset }),
            replies,
        ])
    }

    /// Near the bottom of a history that does not reach the newest message.
    pub(super) fn load_newer(&mut self, window: WinId) -> Task<Msg> {
        let Some(pane) = self.session.panes.get_mut(&window) else {
            return Task::none();
        };
        if pane.newest_loaded || pane.loading_newer || pane.jump_pending.is_some() {
            return Task::none();
        }
        let (Some(chat_id), Some(after)) = (pane.chat_id, pane.messages.last().map(|m| m.id))
        else {
            return Task::none();
        };
        pane.loading_newer = true;
        Task::perform(
            td::history_newer(self.session.client_id, chat_id, after),
            move |r| Msg::Pane(window, PaneMsg::NewerLoaded(chat_id, after, r)),
        )
    }

    pub(super) fn append_newer(
        &mut self,
        window: WinId,
        chat_id: i64,
        after: i64,
        messages: &[Message],
    ) -> Task<Msg> {
        let newest = self.session.chats.get(&chat_id).map_or(0, |c| c.last_id);
        // A page shorter than the limit does not mean the newest message was
        // reached: `history_newer` may simply need another round trip.
        let reached = messages.is_empty() || messages.last().is_some_and(|m| m.id >= newest);
        // Archived deleted messages strictly between the pane's last loaded
        // message and this page's first one are otherwise never covered:
        // TDLib does not return them, and `messages.first()` would start the
        // range too late, right at the first *new* message.
        let lower = after + 1;
        let upper = if reached {
            i64::MAX
        } else {
            messages.last().map_or(i64::MAX, |m| m.id + 1)
        };
        let (items, _) = self.merge_range(chat_id, messages, lower, upper);
        let replies = self.fetch_replies(chat_id, &items);
        let Some(pane) = self
            .session
            .panes
            .get_mut(&window)
            .filter(|p| p.shows(chat_id))
        else {
            return Task::none();
        };
        let before = pane.messages.len();
        pane.append_page(items, reached);
        // The list is anchored to the bottom: keep the same messages on
        // screen instead of jumping to the newly appended ones.
        let added: f32 = pane.messages[before..]
            .iter()
            .map(|m| pane.height_estimate(m))
            .sum();
        let current = pane.scroll.map_or(0.0, |s| s.y);
        let offset = AbsoluteOffset {
            x: 0.0,
            y: current + added,
        };
        pane.scroll = Some(offset);
        Task::batch([
            operation::scroll_to(pane.scroll_id.clone(), offset),
            replies,
        ])
    }

    /// Back to the newest messages after a jump.
    pub(super) fn back_to_latest(&mut self, window: WinId) -> Task<Msg> {
        let Some(pane) = self.session.panes.get_mut(&window) else {
            return Task::none();
        };
        let Some(chat_id) = pane.chat_id else {
            return Task::none();
        };
        let compose = std::mem::take(&mut pane.compose);
        let reply_to = pane.reply_to;
        let editing = pane.editing.take();
        let touched = pane.compose_touched;
        pane.switch_to(chat_id);
        pane.compose = compose;
        pane.reply_to = reply_to;
        pane.editing = editing;
        pane.compose_touched = touched;
        let client_id = self.session.client_id;
        // The chat is already open (this only runs after a jump): reload its
        // latest page without another `openChat`, which TDLib would never
        // see balanced by a matching `closeChat`.
        Task::perform(
            async move {
                let messages = td::history_page(client_id, chat_id, 0).await?;
                if let Some(last) = messages.last() {
                    td::mark_read(client_id, chat_id, last.id).await;
                }
                Ok::<_, String>(messages)
            },
            move |r| Msg::Pane(window, PaneMsg::HistoryLoaded(chat_id, r)),
        )
    }

    pub(super) fn search_submit(&mut self, window: WinId) -> Task<Msg> {
        let Some(pane) = self.session.panes.get_mut(&window) else {
            return Task::none();
        };
        let (Some(chat_id), Some(search)) = (pane.chat_id, &mut pane.search) else {
            return Task::none();
        };
        let query = search.query.trim().to_owned();
        if query.is_empty() {
            return Task::none();
        }
        search.results = None;
        search.paging = Default::default();
        self.search_page(window, chat_id, query, None)
    }

    pub(super) fn search_more(&mut self, window: WinId) -> Task<Msg> {
        let Some(pane) = self.session.panes.get(&window) else {
            return Task::none();
        };
        let (Some(chat_id), Some(search)) = (pane.chat_id, &pane.search) else {
            return Task::none();
        };
        if search.paging.pending.is_some() {
            return Task::none();
        }
        let cursor = if search.results.is_none() && search.paging.error.is_some() {
            None
        } else if search.results.is_some() {
            search.paging.next
        } else {
            return Task::none();
        };
        if cursor.is_none() && search.results.is_some() {
            return Task::none();
        }
        self.search_page(window, chat_id, search.query.trim().to_owned(), cursor)
    }

    fn search_page(
        &mut self,
        window: WinId,
        chat_id: i64,
        query: String,
        cursor: Option<i64>,
    ) -> Task<Msg> {
        let request = request_id();
        let search = self
            .session
            .panes
            .get_mut(&window)
            .unwrap()
            .search
            .as_mut()
            .unwrap();
        search.paging.pending = Some((request, cursor));
        search.paging.error = None;
        let q = query.clone();
        Task::perform(
            td::search_chat(self.session.client_id, chat_id, query, cursor),
            move |r| {
                Msg::Pane(
                    window,
                    PaneMsg::SearchResults(chat_id, q.clone(), request, cursor, r),
                )
            },
        )
    }

    /// Only an answer for this exact request and cursor may change the page.
    pub(super) fn search_results(
        &mut self,
        window: WinId,
        chat_id: i64,
        query: &str,
        request: u64,
        cursor: Option<i64>,
        result: Result<(Vec<Message>, Option<i64>), String>,
    ) {
        let current = self
            .session
            .panes
            .get(&window)
            .filter(|p| p.shows(chat_id))
            .and_then(|p| p.search.as_ref())
            .is_some_and(|s| s.query.trim() == query && s.paging.accepts(request, &cursor));
        if !current {
            return;
        }
        if let Ok((messages, _)) = &result {
            for m in messages {
                self.note_files(&m.content);
            }
        }
        let search = self
            .session
            .panes
            .get_mut(&window)
            .unwrap()
            .search
            .as_mut()
            .unwrap();
        match result {
            Ok((messages, next)) => {
                search
                    .paging
                    .finish(&mut search.results, messages, next, &cursor)
            }
            Err(e) => {
                search.paging.pending = None;
                search.paging.error = Some(e);
            }
        }
    }

    /// Archive titles are limited to locally loaded archive members; TDLib's
    /// unscoped title search is used only outside the archive.
    pub(super) fn list_query(&mut self, window: WinId, query: String) -> Task<Msg> {
        let Some(pane) = self.session.panes.get_mut(&window) else {
            return Task::none();
        };
        pane.list.query = query.clone();
        pane.list.chats.clear();
        pane.list.messages = None;
        pane.list.paging = Default::default();
        pane.list.scroll = 0.0;
        let reset = operation::scroll_to(
            pane.list.scroll_id.clone(),
            AbsoluteOffset { x: 0.0, y: 0.0 },
        );
        if query.trim().is_empty() || pane.list.archive {
            return reset;
        }
        let q = query.clone();
        Task::batch([
            reset,
            Task::perform(td::search_chats(self.session.client_id, query), move |r| {
                Msg::ListChats(window, q.clone(), r)
            }),
        ])
    }

    /// Chats matching the window's filter: loaded titles from the selected
    /// list; outside the archive, append unscoped TDLib chat search hits.
    pub(super) fn filtered_chats(&self, window: WinId) -> Option<Vec<i64>> {
        let pane = self.session.panes.get(&window)?;
        let query = pane.list.query.trim().to_lowercase();
        if query.is_empty() {
            return None;
        }
        let source = if pane.list.archive {
            &self.session.archived
        } else {
            &self.session.order
        };
        let mut ids: Vec<i64> = source
            .iter()
            .map(|&(_, id)| id)
            .filter(|id| {
                self.session
                    .chats
                    .get(id)
                    .is_some_and(|c| c.title_lower.contains(&query))
            })
            .collect();
        if pane.list.archive {
            return Some(ids);
        }
        // TDLib's own results come after and may repeat the ones just
        // found; a set makes the check O(1) instead of scanning `ids`
        // again for every candidate.
        let mut seen: std::collections::HashSet<i64> = ids.iter().copied().collect();
        for &id in &pane.list.chats {
            if seen.insert(id) && self.session.chats.contains_key(&id) {
                ids.push(id);
            }
        }
        Some(ids)
    }

    pub(super) fn list_search_messages(&mut self, window: WinId) -> Task<Msg> {
        let Some(pane) = self.session.panes.get_mut(&window) else {
            return Task::none();
        };
        let query = pane.list.query.trim().to_owned();
        if query.is_empty() {
            return Task::none();
        }
        pane.list.messages = None;
        pane.list.paging = Default::default();
        pane.list.scroll = 0.0;
        let reset = operation::scroll_to(
            pane.list.scroll_id.clone(),
            AbsoluteOffset { x: 0.0, y: 0.0 },
        );
        Task::batch([reset, self.list_page(window, query, None)])
    }

    pub(super) fn list_more(&mut self, window: WinId) -> Task<Msg> {
        let Some(pane) = self.session.panes.get(&window) else {
            return Task::none();
        };
        let list = &pane.list;
        if list.paging.pending.is_some() {
            return Task::none();
        }
        let cursor = if list.messages.is_none() && list.paging.error.is_some() {
            None
        } else if list.messages.is_some() {
            list.paging.next.clone()
        } else {
            return Task::none();
        };
        if cursor.is_none() && list.messages.is_some() {
            return Task::none();
        }
        self.list_page(window, list.query.trim().to_owned(), cursor)
    }

    fn list_page(&mut self, window: WinId, query: String, cursor: Option<String>) -> Task<Msg> {
        let request = request_id();
        let pane = self.session.panes.get_mut(&window).unwrap();
        let scope = list_search_scope(pane.list.archive);
        pane.list.paging.pending = Some((request, cursor.clone()));
        pane.list.paging.error = None;
        let q = query.clone();
        Task::perform(
            td::search_messages(self.session.client_id, scope, query, cursor.clone()),
            move |r| Msg::ListMessages(window, q.clone(), request, cursor.clone(), r),
        )
    }

    pub(super) fn list_results(
        &mut self,
        window: WinId,
        query: &str,
        request: u64,
        cursor: Option<String>,
        result: Result<(Vec<Message>, Option<String>), String>,
    ) {
        let current = self.session.panes.get(&window).is_some_and(|p| {
            p.list.query.trim() == query && p.list.paging.accepts(request, &cursor)
        });
        if !current {
            return;
        }
        if let Ok((messages, _)) = &result {
            for m in messages {
                self.note_files(&m.content);
            }
        }
        let list = &mut self.session.panes.get_mut(&window).unwrap().list;
        match result {
            Ok((messages, next)) => list
                .paging
                .finish(&mut list.messages, messages, next, &cursor),
            Err(e) => {
                list.paging.pending = None;
                list.paging.error = Some(e);
            }
        }
    }

    /// Opens the chat of a search result and scrolls to the message.
    pub(super) fn open_found(&mut self, window: WinId, chat_id: i64, id: i64) -> Task<Msg> {
        let open = self.select_chat(window, chat_id);
        let Some(pane) = self
            .session
            .panes
            .get_mut(&window)
            .filter(|p| p.shows(chat_id))
        else {
            return open;
        };
        // The page from opening the chat may still be in flight; the jump's
        // own history wins regardless of which answer lands first: while
        // pending it is skipped outright, and `jumped` (set once the jump
        // lands) keeps a late arrival from overwriting it afterwards.
        pane.jump_pending = Some(id);
        pane.highlight = Some(id);
        Task::batch([
            open,
            Task::perform(
                td::history_around(self.session.client_id, chat_id, id),
                move |r| Msg::Pane(window, PaneMsg::AroundLoaded(chat_id, id, r)),
            ),
        ])
    }
}
