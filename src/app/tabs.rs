//! Browser-like tabs of recent chats in the main window.
//!
//! The active tab is the main window's pane; background tabs keep their pane
//! (history, draft, scroll position) in `App::background` and still receive
//! updates, but TDLib considers only the active chat opened. Chats shown in a
//! separate window are hidden from the tab bar while that window is open.

use iced::Task;
use iced::widget::scrollable::AbsoluteOffset;
use iced::widget::{Id, operation};

use super::pane::ChatPane;
use super::{App, Msg, PaneMsg};
use crate::settings::TabsState;
use crate::td;

pub(crate) const MAX_TABS: usize = 10;

/// Id of the tab bar scrollable, for wheel scrolling.
pub(crate) const TAB_BAR: &str = "tab-bar";

#[derive(Default)]
pub(crate) struct Tabs {
    /// Order as shown; pinned tabs come first.
    pub(crate) chats: Vec<i64>,
    pub(crate) pinned: Vec<i64>,
    /// Last use per chat, for evicting the least recently used tab.
    used: std::collections::HashMap<i64, u64>,
    clock: u64,
    /// Restored from settings once per session.
    pub(crate) restored: bool,
}

impl Tabs {
    fn touch(&mut self, chat_id: i64) {
        self.clock += 1;
        self.used.insert(chat_id, self.clock);
    }

    fn remove(&mut self, chat_id: i64) {
        self.chats.retain(|&c| c != chat_id);
        self.pinned.retain(|&c| c != chat_id);
        self.used.remove(&chat_id);
    }

    pub(crate) fn is_pinned(&self, chat_id: i64) -> bool {
        self.pinned.contains(&chat_id)
    }
}

impl App {
    fn main_chat(&self) -> Option<i64> {
        self.session
            .panes
            .get(&self.main_window)
            .and_then(|p| p.chat_id)
    }

    fn in_chat_window(&self, chat_id: i64) -> bool {
        self.session
            .panes
            .iter()
            .any(|(&w, p)| w != self.main_window && p.shows(chat_id))
    }

    /// Tabs shown in the bar: all except chats open in their own window.
    pub(crate) fn visible_tabs(&self) -> Vec<i64> {
        self.session
            .tabs
            .chats
            .iter()
            .copied()
            .filter(|&c| !self.in_chat_window(c))
            .collect()
    }

    /// Adds a tab right after the active one (or at the end) and evicts the
    /// least recently used unpinned tab beyond the limit.
    fn add_tab(&mut self, chat_id: i64) {
        if self.session.tabs.chats.contains(&chat_id) {
            return;
        }
        let after_active = self
            .main_chat()
            .and_then(|a| self.session.tabs.chats.iter().position(|&c| c == a))
            .map_or(self.session.tabs.chats.len(), |i| i + 1);
        // Never insert in front of pinned tabs.
        let at = after_active.max(self.session.tabs.pinned.len());
        self.session.tabs.chats.insert(at, chat_id);
        self.session.tabs.touch(chat_id);
        while self.session.tabs.chats.len() > MAX_TABS {
            let active = self.main_chat();
            let victim = self
                .session
                .tabs
                .chats
                .iter()
                .copied()
                .filter(|&c| c != chat_id && Some(c) != active && !self.session.tabs.is_pinned(c))
                .min_by_key(|c| self.session.tabs.used.get(c).copied().unwrap_or(0));
            let Some(victim) = victim else { break };
            self.session.tabs.remove(victim);
            self.session.background.remove(&victim);
        }
    }

    /// Chat list filter (archive/folder/search) of the main window: it
    /// belongs to the window, not to whichever chat its pane currently
    /// shows, and must survive switching tabs.
    fn take_main_list(&mut self) -> super::pane::ListSearch {
        self.session
            .panes
            .get_mut(&self.main_window)
            .map(|p| std::mem::take(&mut p.list))
            .unwrap_or_default()
    }

    /// Shows a chat in the main window, as a tab.
    pub(crate) fn show_in_main(&mut self, chat_id: i64) -> Task<Msg> {
        if self.main_chat() == Some(chat_id) {
            return Task::none();
        }
        self.add_tab(chat_id);
        self.session.tabs.touch(chat_id);
        let restored = self.session.background.remove(&chat_id);
        // A pane cached with no history at all (its answer was lost while
        // backgrounded, see the flag reset below) must reload like a fresh
        // chat instead of the light `reopen_chat` path.
        let has_history = restored.as_ref().is_some_and(|p| p.oldest_loaded.is_some());
        let mut next = restored.unwrap_or_else(|| {
            let mut pane = ChatPane::default();
            pane.switch_to(chat_id);
            pane
        });
        next.list = self.take_main_list();
        let Some(slot) = self.session.panes.get_mut(&self.main_window) else {
            return Task::none();
        };
        let mut previous = std::mem::replace(slot, next);
        let pane = &self.session.panes[&self.main_window];
        let scroll = pane.scroll;
        // Search results replace history and use a separate scrollable id.
        // Restore its viewport after the pane has been installed, including
        // when history still needs to be fetched.
        let search_scroll = pane
            .search
            .as_ref()
            .filter(|search| search.results.is_some() && pane.forward.is_none())
            .map(|search| {
                operation::scroll_to(
                    search.scroll_id.clone(),
                    AbsoluteOffset {
                        x: 0.0,
                        y: search.scroll,
                    },
                )
            })
            .unwrap_or_else(Task::none);
        let close = match previous.chat_id {
            Some(prev) => {
                previous.menu = None;
                let draft = self.sync_draft(prev, previous.draft());
                if self.session.tabs.chats.contains(&prev) {
                    // Transient request flags do not survive being hidden:
                    // a request in flight now targets a pane that just
                    // moved, so its answer is dropped and must not leave
                    // the tab stuck "loading" forever once restored.
                    previous.loading_older = false;
                    previous.loading_newer = false;
                    previous.jump_pending = None;
                    previous.jumped = false;
                    if let Some(search) = &mut previous.search
                        && search.paging.pending.take().is_some()
                        && search.results.is_none()
                    {
                        search.paging.error = Some("Поиск прерван при переключении чата".into());
                    }
                    self.session.background.insert(prev, previous);
                }
                Task::batch([draft, self.close_chat_if_unused(prev)])
            }
            None => Task::none(),
        };
        let saved = self.save_tabs();
        let client_id = self.session.client_id;
        let window = self.main_window;
        let open = if has_history {
            // History is kept current by updates; only tell TDLib the chat is
            // opened again, mark what is visible as read and restore scroll.
            // Pinned messages are not: an `updateMessageIsPinned` while this
            // tab was backgrounded may have arrived before its very first
            // load finished and been lost with it, so it is refetched here.
            let last = self.session.panes[&window].messages.last().map(|m| m.id);
            Task::batch([
                Task::perform(td::reopen_chat(client_id, chat_id, last), |()| Msg::Ignore),
                operation::scroll_to(
                    self.session.panes[&window].scroll_id.clone(),
                    scroll.unwrap_or_default(),
                ),
                self.load_pinned(window, chat_id),
            ])
        } else {
            Task::perform(td::open_chat(client_id, chat_id), move |r| {
                Msg::Pane(window, PaneMsg::HistoryLoaded(chat_id, r))
            })
        };
        Task::batch([close.chain(open), search_scroll, saved])
    }

    /// Closes a tab; closing the active one switches to its neighbour.
    pub(crate) fn close_tab(&mut self, chat_id: i64) -> Task<Msg> {
        let visible = self.visible_tabs();
        let neighbour = visible.iter().position(|&c| c == chat_id).and_then(|i| {
            visible
                .get(i + 1)
                .or_else(|| i.checked_sub(1).and_then(|j| visible.get(j)))
                .copied()
        });
        self.session.tabs.remove(chat_id);
        // Text typed in a closed tab is kept as the chat's draft.
        let draft = match self.session.background.remove(&chat_id) {
            Some(pane) => self.sync_draft(chat_id, pane.draft()),
            None => Task::none(),
        };
        let task = if self.main_chat() == Some(chat_id) {
            self.leave_main(neighbour)
        } else {
            Task::none()
        };
        let saved = self.save_tabs();
        Task::batch([draft, task, saved])
    }

    /// Closes every tab except the pinned ones; the main window moves to
    /// the first pinned tab, or shows no chat.
    pub(crate) fn close_all_tabs(&mut self) -> Task<Msg> {
        let closing: Vec<i64> = self
            .session
            .tabs
            .chats
            .iter()
            .copied()
            .filter(|&chat| !self.session.tabs.is_pinned(chat))
            .collect();
        let main = self.main_chat();
        let mut tasks = Vec::new();
        for chat in &closing {
            self.session.tabs.remove(*chat);
            if let Some(pane) = self.session.background.remove(chat) {
                tasks.push(self.sync_draft(*chat, pane.draft()));
                tasks.push(self.close_chat_if_unused(*chat));
            }
        }
        if main.is_some_and(|m| closing.contains(&m)) {
            let next = self.visible_tabs().first().copied();
            tasks.push(self.leave_main(next));
        }
        tasks.push(self.save_tabs());
        Task::batch(tasks)
    }

    /// The main window stops showing its chat (closed tab or moved to a
    /// window) and shows `next` or nothing.
    fn leave_main(&mut self, next: Option<i64>) -> Task<Msg> {
        match next {
            Some(next) => self.show_in_main(next),
            None => {
                let list = self.take_main_list();
                let fresh = ChatPane {
                    list,
                    ..ChatPane::default()
                };
                let previous = self.session.panes.insert(self.main_window, fresh);
                match previous
                    .as_ref()
                    .and_then(|p| p.chat_id.map(|c| (c, p.draft())))
                {
                    Some((prev, draft)) => Task::batch([
                        self.sync_draft(prev, draft),
                        self.close_chat_if_unused(prev),
                        self.save_tabs(),
                    ]),
                    None => Task::none(),
                }
            }
        }
    }

    /// The main window's chat got its own window: the tab hides and the main
    /// window moves on to a neighbouring tab.
    pub(crate) fn main_chat_moved_to_window(&mut self, chat_id: i64) -> Task<Msg> {
        if self.main_chat() != Some(chat_id) {
            return Task::none();
        }
        let tabs = self.session.tabs.chats.clone();
        let i = tabs.iter().position(|&c| c == chat_id);
        let visible = |c: &i64| *c != chat_id && !self.in_chat_window(*c);
        let next = i
            .and_then(|i| {
                tabs[i + 1..]
                    .iter()
                    .find(|c| visible(c))
                    .or_else(|| tabs[..i].iter().rev().find(|c| visible(c)))
            })
            .copied();
        // Keep its cached pane for when the window closes; the main window's
        // own list filter stays with the main window, not with this tab.
        let list = self.take_main_list();
        let fresh = ChatPane {
            list,
            ..ChatPane::default()
        };
        let mut pane = self
            .session
            .panes
            .insert(self.main_window, fresh)
            .unwrap_or_default();
        // See `show_in_main`: an answer in flight would target a pane that
        // just moved, so its stuck flags must be cleared before it goes idle.
        pane.loading_older = false;
        pane.loading_newer = false;
        pane.jump_pending = None;
        pane.jumped = false;
        let draft = self.sync_draft(chat_id, pane.draft());
        // The window loads and syncs its own draft separately; this cached
        // pane must not carry the compose text along too, or leaving the
        // tab later would resync this stale copy over the window's own.
        pane.compose = iced::widget::text_editor::Content::new();
        pane.compose_touched = false;
        self.session.background.insert(chat_id, pane);
        // Balances this pane's own `openChat`: at this point no pane counts
        // as showing `chat_id` yet (the new window's pane is still empty),
        // so this reliably closes it exactly once, regardless of the caller
        // opening it again for the new window right after (`select_chat`,
        // called after this by `open_chat_window`).
        let close = self.close_chat_if_unused(chat_id);
        match next {
            Some(next) => Task::batch([draft, close.chain(self.show_in_main(next))]),
            None => Task::batch([draft, self.save_tabs(), close]),
        }
    }

    /// A chat window closed: its chat becomes a tab again.
    pub(crate) fn chat_window_closed(&mut self, chat_id: i64) -> Task<Msg> {
        if !self.session.tabs.chats.contains(&chat_id) && self.auth_ready() {
            self.add_tab(chat_id);
            self.save_tabs()
        } else {
            Task::none()
        }
    }

    pub(crate) fn toggle_pin(&mut self, chat_id: i64) -> Task<Msg> {
        if !self.session.tabs.chats.contains(&chat_id) {
            return Task::none();
        }
        self.session.tabs.chats.retain(|&c| c != chat_id);
        if self.session.tabs.is_pinned(chat_id) {
            self.session.tabs.pinned.retain(|&c| c != chat_id);
            // First unpinned position.
            let at = self.session.tabs.pinned.len();
            self.session.tabs.chats.insert(at, chat_id);
        } else {
            let at = self.session.tabs.pinned.len();
            self.session.tabs.pinned.push(chat_id);
            self.session.tabs.chats.insert(at, chat_id);
        }
        self.save_tabs()
    }

    /// Ctrl+Tab / Ctrl+Shift+Tab.
    pub(crate) fn cycle_tab(&mut self, forward: bool) -> Task<Msg> {
        let visible = self.visible_tabs();
        if visible.is_empty() {
            return Task::none();
        }
        let current = self
            .main_chat()
            .and_then(|a| visible.iter().position(|&c| c == a));
        let n = visible.len();
        let next = match (current, forward) {
            (Some(i), true) => (i + 1) % n,
            (Some(i), false) => (i + n - 1) % n,
            (None, _) => 0,
        };
        self.show_in_main(visible[next])
    }

    /// Ctrl+1…8 by position, Ctrl+9 the last tab, as in browsers.
    pub(crate) fn tab_by_number(&mut self, number: usize) -> Task<Msg> {
        let visible = self.visible_tabs();
        let chat = if number == 9 {
            visible.last()
        } else {
            visible.get(number - 1)
        };
        match chat {
            Some(&chat) => self.show_in_main(chat),
            None => Task::none(),
        }
    }

    pub(crate) fn scroll_tab_bar(&self, delta: iced::mouse::ScrollDelta) -> Task<Msg> {
        let dx = match delta {
            iced::mouse::ScrollDelta::Lines { x, y } => -(if x != 0.0 { x } else { y }) * 60.0,
            iced::mouse::ScrollDelta::Pixels { x, y } => -(if x != 0.0 { x } else { y }),
        };
        operation::scroll_by(Id::new(TAB_BAR), AbsoluteOffset { x: dx, y: 0.0 })
    }

    /// Persists the tab bar if it changed, debounced (see
    /// `save_settings_debounced`): every tab switch touches `active`, so
    /// without this every click would write `settings.json` on the spot.
    fn save_tabs(&mut self) -> Task<Msg> {
        let Some(account) = self.session.my_id else {
            return Task::none();
        };
        let state = TabsState {
            chats: self.session.tabs.chats.clone(),
            pinned: self.session.tabs.pinned.clone(),
            active: self.main_chat(),
        };
        if self.settings.tabs.get(&account.to_string()) == Some(&state) {
            return Task::none();
        }
        self.settings.tabs.insert(account.to_string(), state);
        self.save_settings_debounced()
    }

    /// Once logged in: brings back the account's tabs and active chat.
    pub(crate) fn restore_tabs(&mut self) -> Task<Msg> {
        let (Some(account), true) = (self.session.my_id, self.auth_ready()) else {
            return Task::none();
        };
        if self.session.tabs.restored {
            return Task::none();
        }
        self.session.tabs.restored = true;
        let Some(state) = self.settings.tabs.get(&account.to_string()).cloned() else {
            return Task::none();
        };
        self.session.tabs.pinned = state
            .pinned
            .iter()
            .copied()
            .filter(|c| state.chats.contains(c))
            .collect();
        self.session.tabs.chats = state.chats;
        self.session
            .tabs
            .chats
            .truncate(MAX_TABS.max(self.session.tabs.pinned.len()));
        for &chat in &self.session.tabs.chats.clone() {
            self.session.tabs.touch(chat);
        }
        match state.active.filter(|a| self.session.tabs.chats.contains(a)) {
            Some(active) => self.show_in_main(active),
            None => Task::none(),
        }
    }
}
