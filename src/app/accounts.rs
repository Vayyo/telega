//! Several accounts, one running at a time. Each account has its own TDLib
//! data slot; switching closes the current client (the session stays on
//! disk) and starts one on the other slot, without logging out.

use iced::Task;

use super::{App, Auth, Msg};
use crate::plugins::HostCmd;
use crate::settings::Account;
use crate::td;

/// What happens once the current client has closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Leave {
    /// Slot to start next.
    pub(crate) to: u32,
    /// The current slot is dropped from the list and its data removed.
    pub(crate) forget: bool,
}

impl App {
    /// Accounts that finished logging in.
    pub(crate) fn logged_in(&self) -> impl Iterator<Item = &Account> {
        self.settings
            .accounts
            .iter()
            .filter(|a| a.user_id.is_some())
    }

    pub(crate) fn account_name(&self, slot: u32) -> &str {
        self.settings
            .accounts
            .iter()
            .find(|a| a.slot == slot)
            .map(|a| a.name.as_str())
            .filter(|n| !n.is_empty())
            .unwrap_or("Аккаунт")
    }

    /// Another logged-in account to go back to, if any.
    pub(crate) fn other_account(&self) -> Option<u32> {
        self.logged_in()
            .map(|a| a.slot)
            .find(|&slot| slot != self.session.slot)
    }

    pub(crate) fn switch_account(&mut self, slot: u32) -> Task<Msg> {
        self.session.accounts_open = false;
        if slot == self.session.slot || self.session.leave.is_some() || self.key_transition_active()
        {
            return Task::none();
        }
        self.session.leave = Some(Leave {
            to: slot,
            forget: false,
        });
        self.session.auth = Auth::LoggingOut;
        if let Some(host) = &self.plugin_host {
            host.send(HostCmd::Account(None));
        }
        Task::perform(td::close(self.session.client_id), Msg::Done)
    }

    /// A fresh slot with a login screen; the current account stays.
    pub(crate) fn add_account(&mut self) -> Task<Msg> {
        if self.session.leave.is_some() || self.key_transition_active() {
            return Task::none();
        }
        let used = self.settings.accounts.iter().map(|a| a.slot);
        let mut slot = used.chain([self.session.slot]).max().unwrap_or(0) + 1;
        // Leftovers of an interrupted add are not reused.
        while crate::paths::account_dir(slot).exists() {
            slot += 1;
        }
        self.settings.accounts.push(Account {
            slot,
            ..Account::default()
        });
        self.save_settings();
        self.switch_account(slot)
    }

    /// Login screen of an added account: back to a logged-in one, dropping
    /// the unfinished slot.
    pub(crate) fn cancel_add_account(&mut self) -> Task<Msg> {
        let Some(to) = self.other_account() else {
            return Task::none();
        };
        if self.session.leave.is_some() || self.key_transition_active() {
            return Task::none();
        }
        self.session.leave = Some(Leave { to, forget: true });
        self.session.auth = Auth::LoggingOut;
        if let Some(host) = &self.plugin_host {
            host.send(HostCmd::Account(None));
        }
        Task::perform(td::close(self.session.client_id), Msg::Done)
    }

    /// The logged-in user is known: remember the account in its slot. The
    /// same user added twice keeps the older slot; the new session is ended.
    pub(crate) fn account_known(&mut self, user_id: i64) -> Task<Msg> {
        let slot = self.session.slot;
        let existing = self
            .logged_in()
            .find(|a| a.user_id == Some(user_id) && a.slot != slot)
            .map(|a| a.slot);
        if let Some(existing) = existing {
            if self.key_transition_active() {
                self.error = Some("сначала завершите смену ключа".into());
                return Task::none();
            }
            self.session.leave = Some(Leave {
                to: existing,
                forget: true,
            });
            self.session.auth = Auth::LoggingOut;
            if let Some(host) = &self.plugin_host {
                host.send(HostCmd::Account(None));
            }
            return Task::perform(td::log_out(self.session.client_id), Msg::Done);
        }
        let name = self
            .session
            .users
            .get(&user_id)
            .cloned()
            .unwrap_or_default();
        let mut changed = false;
        match self.settings.accounts.iter_mut().find(|a| a.slot == slot) {
            Some(account) if account.user_id == Some(user_id) => {}
            Some(account) => {
                self.account_portraits.remove(&slot);
                account.avatar_digest = None;
                account.user_id = Some(user_id);
                if !name.is_empty() {
                    account.name = name;
                }
                changed = true;
            }
            None => {
                self.settings.accounts.push(Account {
                    slot,
                    user_id: Some(user_id),
                    name,
                    ..Account::default()
                });
                changed = true;
            }
        }
        // The picture may have finished decoding before TDLib reported my_id.
        if let Some((handle, digest)) = self
            .session
            .avatars
            .portrait(super::avatars::Peer::User(user_id))
        {
            let account = self
                .settings
                .accounts
                .iter_mut()
                .find(|a| a.slot == slot)
                .unwrap();
            if account.avatar_digest != Some(digest) || !self.account_portraits.contains_key(&slot)
            {
                self.account_portraits.insert(slot, handle.clone());
            }
            if account.avatar_digest != Some(digest) {
                account.avatar_digest = Some(digest);
                changed = true;
            }
        }
        if changed {
            self.save_settings();
        }
        Task::none()
    }

    /// Keeps the account's name in the switcher up to date.
    pub(crate) fn user_renamed(&mut self, user_id: i64) {
        if self.session.my_id != Some(user_id) {
            return;
        }
        let Some(name) = self
            .session
            .users
            .get(&user_id)
            .filter(|n| !n.is_empty())
            .cloned()
        else {
            return;
        };
        let slot = self.session.slot;
        if let Some(account) = self.settings.accounts.iter_mut().find(|a| a.slot == slot)
            && account.name != name
        {
            account.name = name;
            self.save_settings();
        }
    }

    /// The client has closed: decides the next slot. Without an explicit
    /// request, after a log out (here or from another device) the account is
    /// forgotten and another one, if any, opens.
    pub(crate) fn after_close(&mut self) -> u32 {
        let active = self.key_transition_active();
        let logged_out = std::mem::take(&mut self.session.logging_out) && !active;
        let leave = if active {
            Leave {
                to: self.session.slot,
                forget: false,
            }
        } else {
            self.session.leave.take().unwrap_or_else(|| {
                if logged_out {
                    Leave {
                        to: self.other_account().unwrap_or(self.session.slot),
                        forget: true,
                    }
                } else {
                    // Closed for another reason: restart the same account.
                    Leave {
                        to: self.session.slot,
                        forget: false,
                    }
                }
            })
        };
        if !leave.forget {
            self.persist_video_volume();
        }
        if leave.forget {
            let slot = self.session.slot;
            let user_id = self
                .settings
                .accounts
                .iter()
                .find(|a| a.slot == slot)
                .and_then(|a| a.user_id);
            self.account_portraits.remove(&slot);
            self.settings.accounts.retain(|a| a.slot != slot);
            if let Err(e) = crate::paths::remove_account(slot) {
                self.error = Some(format!("данные аккаунта: {e}"));
            }
            // The archive and plugin database live outside the slot, keyed
            // by TDLib user id: a later login with the same id must not
            // reopen the forgotten account's leftovers.
            if let Some(user_id) = user_id
                && let Err(e) = crate::paths::remove_account_data(user_id)
            {
                self.error = Some(format!("данные аккаунта: {e}"));
            }
        }
        self.settings.active_account = leave.to;
        self.save_settings();
        leave.to
    }
}
