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

pub(super) fn request_id() -> u64 {
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
            .find_map(|p| {
                p.messages
                    .iter()
                    .chain(&p.held_messages)
                    .find(|m| m.id == id)
            })
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
        let client_id = self.session.client_id;
        Task::perform(td::get_messages(client_id, chat_id, missing), move |r| {
            Msg::ForClient(
                client_id,
                Box::new(Msg::RepliesLoaded(r.unwrap_or_default())),
            )
        })
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
        // A deliberate jump supersedes the in-flight cold page, but keep a
        // fresh request token so updates remain tracked until the jump lands.
        // A failed jump clears it, allowing an explicit cold-page retry.
        let was_cold = pane.cold.is_some();
        if let Some(cold) = &mut pane.cold {
            cold.request = Some(request_id());
            cold.pending_page = None;
            cold.verified_ids.clear();
        }
        pane.restoring_anchor = None;
        if !pane.jump_quiet {
            pane.highlight = Some(id);
        }
        if let Some(search) = &mut pane.search {
            search.results = None;
            search.paging = Default::default();
        }
        if !was_cold && let Some(offset) = pane.offset_of(id) {
            pane.jump_quiet = false;
            pane.scroll = Some(AbsoluteOffset { x: 0.0, y: offset });
            return operation::scroll_to(
                pane.scroll_id.clone(),
                AbsoluteOffset { x: 0.0, y: offset },
            );
        }
        pane.jump_pending = Some(id);
        let client_id = self.session.client_id;
        Task::perform(td::history_around(client_id, chat_id, id), move |r| {
            Msg::ForClient(
                client_id,
                Box::new(Msg::Pane(window, PaneMsg::AroundLoaded(chat_id, id, r))),
            )
        })
    }

    pub(super) fn show_around(
        &mut self,
        window: WinId,
        chat_id: i64,
        target: i64,
        messages: &[Message],
    ) -> Task<Msg> {
        if !self
            .session
            .panes
            .get(&window)
            .is_some_and(|p| p.shows(chat_id) && p.jump_pending == Some(target))
        {
            return Task::none();
        }
        if let Some(pane) = self.session.panes.get_mut(&window)
            && pane.cold.as_ref().is_some_and(|cold| cold.overflowed)
        {
            // The bounded change log has lost edits: do not show or archive
            // this potentially stale around page. Reselecting retries cold.
            pane.cold.as_mut().unwrap().request = None;
            pane.jump_pending = None;
            self.error = Some("Обновления истории изменились; выберите вкладку снова".into());
            return Task::none();
        }
        self.clear_history_photos(window);
        let newest = self.session.chats.get(&chat_id).map_or(0, |c| c.last_id);
        let reached_newest = messages.last().is_some_and(|m| m.id >= newest);
        let lower = messages.first().map_or(i64::MAX, |m| m.id);
        let last = messages.last().map_or(i64::MIN, |m| m.id);
        let upper = if reached_newest {
            i64::MAX
        } else {
            last.saturating_add(1)
        };
        let (mut items, pending) = self.merge_range(chat_id, messages, lower, upper);
        let cold = self.session.panes[&window].cold.is_some();
        let held = cold.then(|| {
            self.reconcile_cold_page(window, chat_id, &mut items, &pending, Some((lower, last)))
        });
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
        // The jump's page replaces the cold restore only after its edits and
        // protected rows have been reconciled against the TDLib snapshot.
        if let Some(held) = held {
            pane.messages = items;
            pane.messages.extend(pending);
            pane.held_messages = held;
        } else {
            let previous = std::mem::replace(&mut pane.messages, items);
            for item in previous {
                let required = item.pending
                    || item.failed
                    || pane.reply_to == Some(item.id)
                    || pane.editing.as_ref().is_some_and(|(id, _)| *id == item.id)
                    || pane
                        .selected
                        .as_ref()
                        .is_some_and(|ids| ids.contains(&item.id))
                    || pane
                        .forward
                        .as_ref()
                        .is_some_and(|(ids, _)| ids.contains(&item.id));
                if required && !pane.messages.iter().any(|m| m.id == item.id) {
                    pane.held_messages.push(item);
                }
            }
            pane.held_messages
                .retain(|item| !pane.messages.iter().any(|m| m.id == item.id));
        }
        pane.cold = None;
        pane.jumped = true;
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

    /// Apply changes observed while a cold page or jump was in flight, then
    /// separate old protected rows from the live history page.
    fn reconcile_cold_page(
        &mut self,
        window: WinId,
        chat_id: i64,
        items: &mut Vec<MsgItem>,
        pending: &[MsgItem],
        jump_bounds: Option<(i64, i64)>,
    ) -> Vec<MsgItem> {
        let changes = std::mem::take(
            &mut self
                .session
                .panes
                .get_mut(&window)
                .unwrap()
                .cold
                .as_mut()
                .unwrap()
                .changes,
        );
        // merge_page archived the page returned by TDLib; an edit or deletion
        // observed after that snapshot must also win in the local archive.
        if let Some(archive) =
            Self::active_archive(&mut self.session.archive, self.settings.keep_deleted)
        {
            for (&id, change) in &changes {
                if let Some(content) = &change.content
                    && let Err(e) = archive.set_text(chat_id, id, &content.text)
                {
                    self.error = Some(format!("архив: {e}"));
                }
                if change.deleted == Some(true)
                    && let Err(e) = archive.mark_deleted(chat_id, &[id])
                {
                    self.error = Some(format!("архив: {e}"));
                }
                if change.purged
                    && let Err(e) = archive.purge(chat_id, &[id])
                {
                    self.error = Some(format!("архив: {e}"));
                }
            }
        }
        // Live arrivals supersede the returned page only within its range
        // on a jump. Outside it, retain operation-owned records separately
        // so newer pagination cannot skip the intervening history.
        let pane = self.session.panes.get_mut(&window).unwrap();
        let protected = std::mem::take(&mut pane.messages);
        let preexisting = std::mem::take(&mut pane.cold.as_mut().unwrap().preexisting);
        let previous_held = std::mem::take(&mut pane.held_messages);
        let mut held = Vec::new();
        for item in protected.into_iter().chain(previous_held) {
            let old = preexisting.contains(&item.id);
            let in_jump_page =
                jump_bounds.is_none_or(|(lower, upper)| (lower..=upper).contains(&item.id));
            if let Some(slot) = items.iter_mut().find(|m| m.id == item.id) {
                if !old || item.pending || item.failed {
                    *slot = item;
                }
            } else if !pending.iter().any(|m| m.id == item.id) {
                let required = item.pending
                    || item.failed
                    || pane.reply_to == Some(item.id)
                    || pane.editing.as_ref().is_some_and(|(id, _)| *id == item.id)
                    || pane
                        .selected
                        .as_ref()
                        .is_some_and(|ids| ids.contains(&item.id))
                    || pane
                        .forward
                        .as_ref()
                        .is_some_and(|(ids, _)| ids.contains(&item.id));
                if old && !item.pending && !item.failed || !in_jump_page && required {
                    held.push(item);
                } else if in_jump_page {
                    items.push(item);
                }
            }
        }
        for (id, mut change) in changes {
            if change.deleted == Some(false) {
                items.retain(|item| item.id != id);
                held.retain(|item| item.id != id);
                continue;
            }
            if let Some(item) = items
                .iter_mut()
                .chain(held.iter_mut())
                .find(|item| item.id == id)
            {
                if let Some(content) = change.content.take() {
                    item.text = content.text;
                    item.rich = content.rich;
                    item.media = content.media;
                    item.extra = content.extra;
                }
                if let Some(edited) = change.edited {
                    item.edited = edited;
                }
                if let Some(reactions) = change.reactions {
                    item.reactions = reactions;
                }
                if change.deleted == Some(true) {
                    item.deleted = true;
                }
            }
        }
        items.sort_by_key(|m| m.id);
        items.dedup_by(|right, left| {
            if right.id != left.id {
                return false;
            }
            if right.deleted && !left.deleted {
                std::mem::swap(right, left);
            }
            true
        });
        held
    }

    /// Only the active occupancy's request may restore a released tab.
    pub(super) fn on_cold_history_loaded(
        &mut self,
        window: WinId,
        chat_id: i64,
        request: u64,
        result: Result<Vec<Message>, String>,
    ) -> Task<Msg> {
        self.apply_cold_history_page(window, chat_id, request, result, None)
    }

    /// `original` describes TDLib's page before a bounded by-ID coherence
    /// read omitted missing messages. It owns archive and paging boundaries.
    fn apply_cold_history_page(
        &mut self,
        window: WinId,
        chat_id: i64,
        request: u64,
        result: Result<Vec<Message>, String>,
        original: Option<(i64, i64, usize)>,
    ) -> Task<Msg> {
        let current = self.session.panes.get(&window).is_some_and(|pane| {
            pane.shows(chat_id)
                && pane
                    .cold
                    .as_ref()
                    .is_some_and(|cold| cold.request == Some(request))
                && pane.jump_pending.is_none()
        });
        if !current {
            return Task::none();
        }
        if result.as_ref().is_ok_and(|messages| !messages.is_empty())
            && self.session.panes[&window]
                .cold
                .as_ref()
                .unwrap()
                .overflowed
        {
            let messages = result.unwrap();
            let ids: Vec<i64> = messages.iter().map(|m| m.id).collect();
            let fresh = request_id();
            let cold = self
                .session
                .panes
                .get_mut(&window)
                .unwrap()
                .cold
                .as_mut()
                .unwrap();
            cold.request = Some(fresh);
            cold.changes.clear();
            cold.overflowed = false;
            cold.verified_ids.clear();
            cold.verified_ids.extend(ids.iter().copied());
            cold.pending_page = Some(messages);
            let client_id = self.session.client_id;
            return Task::perform(td::get_messages(client_id, chat_id, ids), move |r| {
                Msg::ForClient(
                    client_id,
                    Box::new(Msg::Pane(
                        window,
                        PaneMsg::ColdHistoryVerified(chat_id, fresh, r),
                    )),
                )
            });
        }
        if self.session.panes[&window]
            .cold
            .as_ref()
            .unwrap()
            .overflowed
            && result.as_ref().is_ok_and(Vec::is_empty)
        {
            let cold = self
                .session
                .panes
                .get_mut(&window)
                .unwrap()
                .cold
                .as_mut()
                .unwrap();
            cold.changes.clear();
            cold.overflowed = false;
        }
        let messages = match result {
            Ok(messages) => messages,
            Err(error) => {
                self.session
                    .panes
                    .get_mut(&window)
                    .unwrap()
                    .cold
                    .as_mut()
                    .unwrap()
                    .request = None;
                self.error = Some(error);
                return Task::none();
            }
        };
        self.clear_history_photos(window);
        let anchor = self.session.panes[&window].cold.as_ref().unwrap().anchor;
        let newest = self.session.chats.get(&chat_id).map_or(0, |c| c.last_id);
        let last = original
            .map(|(_, last, _)| last)
            .or_else(|| messages.last().map(|m| m.id));
        let reached_newest = anchor.is_none() || last.is_some_and(|id| id >= newest);
        let (mut items, pending) =
            if anchor.is_some() && (!messages.is_empty() || original.is_some()) {
                let lower = original
                    .map(|(first, _, _)| first)
                    .or_else(|| messages.first().map(|m| m.id))
                    .unwrap_or(i64::MIN);
                let upper = if reached_newest {
                    i64::MAX
                } else {
                    last.map_or(i64::MAX, |id| id.saturating_add(1))
                };
                self.merge_range(chat_id, &messages, lower, upper)
            } else if let Some((first, _, len)) = original
                && len >= td::HISTORY_LIMIT
            {
                self.merge_range(chat_id, &messages, first, i64::MAX)
            } else {
                self.merge_page(chat_id, &messages, i64::MAX)
            };
        let held = self.reconcile_cold_page(window, chat_id, &mut items, &pending, None);
        let replies = self.fetch_replies(chat_id, &items);
        let pane = self.session.panes.get_mut(&window).unwrap();
        pane.messages = items;
        pane.held_messages = held;
        pane.messages.extend(pending);
        pane.heights = Default::default();
        pane.oldest_loaded = messages
            .first()
            .map(|m| m.id)
            .or_else(|| original.map(|(first, _, _)| first));
        pane.history_complete = anchor.is_none()
            && original.map_or(messages.len(), |(_, _, len)| len) < td::HISTORY_LIMIT;
        pane.newest_loaded = reached_newest;
        pane.loading_older = false;
        pane.loading_newer = false;
        pane.jumped = true; // Reject late unkeyed latest-page answers.
        pane.cold = None;
        let chosen = anchor.and_then(|saved| {
            pane.messages
                .iter()
                .filter(|m| !m.pending)
                .min_by_key(|m| m.id.abs_diff(saved.id))
                .map(|m| super::pane::HistoryAnchor {
                    id: m.id,
                    delta: saved.delta,
                })
        });
        pane.restoring_anchor = chosen;
        let y = chosen
            .and_then(|position| pane.offset_of(position.id).map(|y| y + position.delta))
            .unwrap_or(0.0)
            .max(0.0);
        let offset = AbsoluteOffset { x: 0.0, y };
        pane.scroll = Some(offset);
        let scroll = operation::scroll_to(pane.scroll_id.clone(), offset);
        scroll.chain(Task::batch([
            replies,
            self.load_pinned(window, chat_id),
            self.load_draft(window, chat_id),
            self.load_roles(chat_id),
        ]))
    }

    /// A single authoritative by-ID read reconciles a page when updates
    /// overwhelmed the unknown-ID phase. Unrelated churn cannot restart it.
    pub(super) fn on_cold_history_verified(
        &mut self,
        window: WinId,
        chat_id: i64,
        request: u64,
        result: Result<Vec<Message>, String>,
    ) -> Task<Msg> {
        let current = self.session.panes.get(&window).is_some_and(|pane| {
            pane.shows(chat_id)
                && pane.cold.as_ref().is_some_and(|cold| {
                    cold.request == Some(request) && cold.pending_page.is_some()
                })
                && pane.jump_pending.is_none()
        });
        if !current {
            return Task::none();
        }
        let verified = match result {
            Ok(messages) => messages,
            Err(error) => {
                let cold = self
                    .session
                    .panes
                    .get_mut(&window)
                    .unwrap()
                    .cold
                    .as_mut()
                    .unwrap();
                cold.pending_page = None;
                cold.verified_ids.clear();
                cold.changes.clear();
                cold.request = None;
                self.error = Some(error);
                return Task::none();
            }
        };
        let cold = self
            .session
            .panes
            .get_mut(&window)
            .unwrap()
            .cold
            .as_mut()
            .unwrap();
        let page = cold.pending_page.take().unwrap();
        let original = (
            page.first().unwrap().id,
            page.last().unwrap().id,
            page.len(),
        );
        cold.verified_ids.clear();
        let mut by_id: std::collections::HashMap<i64, Message> = verified
            .into_iter()
            .map(|message| (message.id, message))
            .collect();
        let fresh_page = page
            .into_iter()
            .filter_map(|old| by_id.remove(&old.id))
            .collect();
        self.apply_cold_history_page(window, chat_id, request, Ok(fresh_page), Some(original))
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
        let client_id = self.session.client_id;
        let load = Task::perform(td::history_newer(client_id, chat_id, after), move |r| {
            Msg::ForClient(
                client_id,
                Box::new(Msg::Pane(window, PaneMsg::NewerLoaded(chat_id, after, r))),
            )
        });
        if let Some(offset) = pane.cold_realign() {
            operation::scroll_to(pane.scroll_id.clone(), offset).chain(load)
        } else {
            load
        }
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
        // A restored reading anchor also accounts for the disappearing
        // loading footer; warm history retains its normal relative offset.
        let offset = if let Some(offset) = pane.cold_realign() {
            offset
        } else {
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
            offset
        };
        Task::batch([
            operation::scroll_to(pane.scroll_id.clone(), offset),
            replies,
        ])
    }

    /// Back to the newest messages after a jump.
    pub(super) fn back_to_latest(&mut self, window: WinId) -> Task<Msg> {
        let Some(chat_id) = self
            .session
            .panes
            .get(&window)
            .and_then(|pane| pane.chat_id)
        else {
            return Task::none();
        };
        self.clear_history_photos(window);
        let pane = self.session.panes.get_mut(&window).unwrap();
        let compose = std::mem::take(&mut pane.compose);
        let reply_to = pane.reply_to;
        let editing = pane.editing.take();
        let touched = pane.compose_touched;
        pane.switch_to(chat_id);
        let request = request_id();
        pane.history_request = Some(request);
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
            move |r| Msg::HistoryResult(client_id, request, window, chat_id, r),
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
        let client_id = self.session.client_id;
        Task::perform(
            td::search_chat(client_id, chat_id, query, cursor),
            move |r| {
                Msg::ForClient(
                    client_id,
                    Box::new(Msg::Pane(
                        window,
                        PaneMsg::SearchResults(chat_id, q.clone(), request, cursor, r),
                    )),
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
        let client_id = self.session.client_id;
        Task::batch([
            reset,
            Task::perform(td::search_chats(client_id, query), move |r| {
                Msg::ForClient(client_id, Box::new(Msg::ListChats(window, q.clone(), r)))
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
        pane.cold = None;
        pane.restoring_anchor = None;
        // The page from opening the chat may still be in flight; the jump's
        // own history wins regardless of which answer lands first: while
        // pending it is skipped outright, and `jumped` (set once the jump
        // lands) keeps a late arrival from overwriting it afterwards.
        pane.jump_pending = Some(id);
        pane.highlight = Some(id);
        let client_id = self.session.client_id;
        Task::batch([
            open,
            Task::perform(td::history_around(client_id, chat_id, id), move |r| {
                Msg::ForClient(
                    client_id,
                    Box::new(Msg::Pane(window, PaneMsg::AroundLoaded(chat_id, id, r))),
                )
            }),
        ])
    }
}
