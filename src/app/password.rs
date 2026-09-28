//! Client password (see `crate::lock`): unlocking at start, turning it on,
//! changing and turning it off from the settings.

use iced::widget::{button, column, container, row, text, text_input};
use iced::{Element, Fill, Task};

use super::{App, Auth, Msg};
use crate::lock::{self, Key};
use crate::td;

/// Input of the password section of the settings.
#[derive(Debug, Default)]
pub(crate) struct Form {
    pub(crate) current: String,
    pub(crate) new: String,
    pub(crate) repeat: String,
    pub(crate) busy: bool,
    pub(crate) message: Option<Result<String, String>>,
}

/// Password step, from the forms to the result.
#[derive(Debug, Clone)]
pub(crate) enum PasswordMsg {
    /// Lock screen input and submit.
    Input(String),
    Unlock,
    Unlocked(Result<Key, String>),
    /// "Забыли пароль?" and its confirmation.
    Forgot(bool),
    ResetAccount,
    /// Settings: fields of the form.
    Current(String),
    New(String),
    Repeat(String),
    /// Set or change (with a new password) or remove (without).
    Apply {
        remove: bool,
    },
    /// The key was derived: (current key if checked, new key or none).
    /// Carries the slot and TDLib client it was started for, so a session
    /// swap in between (account switch, log out) drops it instead of
    /// applying someone else's key.
    Derived(u32, i32, Result<(Option<Key>, String), String>),
    /// TDLib holds the new key; `old_key` is what it had before, to put
    /// back if the archive or the settings save that must follow fails.
    Applied(u32, i32, Result<(Option<Key>, String, String), String>),
    /// The archive (the whole history of deleted messages) finished
    /// re-keying on a worker thread; carries the same key/salt/old key
    /// `Applied` had, plus the outcome.
    ArchiveRekeyed(u32, i32, Option<Key>, String, String, Result<(), String>),
    /// TDLib's key was rolled back after such a failure.
    RollbackDone(u32, String),
}

impl App {
    /// The account of the running slot has a password.
    pub(crate) fn locked_slot(&self) -> Option<(String, String)> {
        let slot = self.session.slot;
        self.settings
            .accounts
            .iter()
            .find(|a| a.slot == slot)
            .and_then(|a| Some((a.lock_salt.clone()?, a.lock_check.clone()?)))
    }

    pub(crate) fn on_password(&mut self, msg: PasswordMsg) -> Task<Msg> {
        match msg {
            PasswordMsg::Input(s) => self.session.input = s,
            PasswordMsg::Unlock => {
                let Some((salt, check)) = self.locked_slot() else {
                    return Task::none();
                };
                if self.session.busy {
                    return Task::none();
                }
                self.session.busy = true;
                let password = std::mem::take(&mut self.session.input);
                return Task::perform(
                    async move {
                        tokio::task::spawn_blocking(move || {
                            let key = lock::derive(&password, &salt)?;
                            if lock::check_value(&key) == check {
                                Ok(key)
                            } else {
                                Err("неверный пароль".to_owned())
                            }
                        })
                        .await
                        .map_err(|e| e.to_string())?
                    },
                    |r| Msg::Password(PasswordMsg::Unlocked(r)),
                );
            }
            PasswordMsg::Unlocked(result) => {
                self.session.busy = false;
                match result {
                    Ok(key) => {
                        self.session.db_key = Some(key);
                        self.error = None;
                        return self.set_parameters();
                    }
                    Err(e) => {
                        self.session.auth = Auth::Locked {
                            error: Some(e),
                            forgot: false,
                        }
                    }
                }
            }
            PasswordMsg::Forgot(on) => {
                if let Auth::Locked { forgot, .. } = &mut self.session.auth {
                    *forgot = on;
                }
            }
            PasswordMsg::ResetAccount => {
                // Nothing of this slot is open yet (TDLib waits for the key),
                // so its data can go now; the account logs in afresh.
                let slot = self.session.slot;
                let user = self
                    .settings
                    .accounts
                    .iter()
                    .find(|a| a.slot == slot)
                    .and_then(|a| a.user_id);
                if let Err(e) = crate::paths::remove_account(slot) {
                    self.error = Some(format!("данные аккаунта: {e}"));
                    return Task::none();
                }
                if let Some(user) = user
                    && let Err(e) = crate::paths::remove_account_data(user)
                {
                    self.error = Some(format!("данные аккаунта: {e}"));
                }
                if let Some(account) = self.settings.accounts.iter_mut().find(|a| a.slot == slot) {
                    account.lock_salt = None;
                    account.lock_check = None;
                    account.user_id = None;
                }
                self.save_settings();
                return self.set_parameters();
            }
            PasswordMsg::Current(s) => self.session.password_form.current = s,
            PasswordMsg::New(s) => self.session.password_form.new = s,
            PasswordMsg::Repeat(s) => self.session.password_form.repeat = s,
            PasswordMsg::Apply { remove } => return self.apply_password(remove),
            PasswordMsg::Derived(slot, client_id, result) => {
                if slot != self.session.slot || client_id != self.session.client_id {
                    // The account changed while Argon2 ran (switch, log
                    // out); the derived key belongs to a session that is no
                    // longer running.
                    return Task::none();
                }
                match result {
                    // TDLib re-encrypts its database; then the archive
                    // follows, and only then are the new salt and check
                    // value written, so nothing is ever saved that points
                    // at a database TDLib does not actually hold.
                    Ok((new_key, salt)) => {
                        let key = lock::tdlib_key(new_key.as_ref());
                        let old_key = lock::tdlib_key(self.session.db_key.as_ref());
                        return Task::perform(
                            async move {
                                td::set_db_key(client_id, key)
                                    .await
                                    .map(|()| (new_key, salt, old_key))
                            },
                            move |r| Msg::Password(PasswordMsg::Applied(slot, client_id, r)),
                        );
                    }
                    Err(e) => self.password_done(Err(e)),
                }
            }
            PasswordMsg::Applied(slot, client_id, result) => {
                if slot != self.session.slot || client_id != self.session.client_id {
                    return Task::none();
                }
                match result {
                    Ok((new_key, salt, old_key)) => {
                        // TDLib already carries the new key from here on:
                        // every failure below must put the old one back, or
                        // the account ends up re-keyed with no salt/check
                        // anywhere that matches it. The archive holds every
                        // deleted message ever seen, so it is re-keyed on a
                        // worker thread instead of freezing the window; the
                        // connection travels there and back through a
                        // one-shot channel (it cannot ride in a `Msg`, which
                        // must stay `Clone`).
                        let Some(mut archive) = self.session.archive.take() else {
                            return self.finish_password_change(
                                slot, client_id, None, new_key, salt, old_key,
                            );
                        };
                        let (tx, rx) = tokio::sync::oneshot::channel();
                        self.session.rekeying_archive = Some(rx);
                        return Task::perform(
                            async move {
                                match tokio::task::spawn_blocking(move || {
                                    let result = archive.rekey(new_key).map_err(|e| e.to_string());
                                    (archive, result)
                                })
                                .await
                                {
                                    Ok((archive, result)) => {
                                        let _ = tx.send(archive);
                                        result
                                    }
                                    Err(e) => Err(e.to_string()),
                                }
                            },
                            move |result| {
                                Msg::Password(PasswordMsg::ArchiveRekeyed(
                                    slot, client_id, new_key, salt, old_key, result,
                                ))
                            },
                        );
                    }
                    Err(e) => self.password_done(Err(e)),
                }
            }
            PasswordMsg::ArchiveRekeyed(slot, client_id, new_key, salt, old_key, result) => {
                let archive = self
                    .session
                    .rekeying_archive
                    .take()
                    .and_then(|mut rx| rx.try_recv().ok());
                if slot != self.session.slot || client_id != self.session.client_id {
                    // The account changed while the archive was being
                    // re-encrypted; its data is already durably on disk
                    // either way (the re-key transaction already committed),
                    // so the stale handle is simply dropped instead of
                    // being attached to a different session.
                    return Task::none();
                }
                match result {
                    Ok(()) => {
                        return self.finish_password_change(
                            slot, client_id, archive, new_key, salt, old_key,
                        );
                    }
                    Err(e) => {
                        self.session.archive = archive;
                        return self.rollback_tdlib_key(
                            client_id,
                            slot,
                            old_key,
                            format!("архив: {e}"),
                        );
                    }
                }
            }
            PasswordMsg::RollbackDone(slot, e) => {
                if slot == self.session.slot {
                    self.password_done(Err(e));
                }
            }
        }
        Task::none()
    }

    fn password_done(&mut self, result: Result<String, String>) {
        self.session.password_form.busy = false;
        self.session.password_form.message = Some(result);
    }

    /// TDLib was re-keyed but the archive re-key or the settings save that
    /// must follow it failed; puts the previous key back on TDLib before
    /// reporting `error`, so the account stays openable with the password it
    /// had before this attempt.
    fn rollback_tdlib_key(
        &mut self,
        client_id: i32,
        slot: u32,
        old_key: String,
        error: String,
    ) -> Task<Msg> {
        Task::perform(
            async move { td::set_db_key(client_id, old_key).await },
            move |r| {
                let error = match r {
                    Ok(()) => error,
                    Err(e) => format!("{error}; откат ключа TDLib не удался: {e}"),
                };
                Msg::Password(PasswordMsg::RollbackDone(slot, error))
            },
        )
    }

    /// Common tail once the archive holds the new key (or there was none to
    /// re-key): saves the salt and check value, rolling TDLib's key back if
    /// that fails, so the account is never left re-keyed with nothing on
    /// disk that matches it.
    fn finish_password_change(
        &mut self,
        slot: u32,
        client_id: i32,
        archive: Option<crate::archive::Archive>,
        new_key: Option<Key>,
        salt: String,
        old_key: String,
    ) -> Task<Msg> {
        if archive.is_some() {
            self.session.archive = archive;
        }
        let Some(account) = self.settings.accounts.iter_mut().find(|a| a.slot == slot) else {
            return self.rollback_tdlib_key(client_id, slot, old_key, "аккаунт не найден".into());
        };
        let prev_salt = account.lock_salt.clone();
        let prev_check = account.lock_check.clone();
        account.lock_salt = new_key.map(|_| salt);
        account.lock_check = new_key.as_ref().map(lock::check_value);
        if let Err(e) = self.try_save_settings() {
            // The write never reached disk (atomic save), so only the
            // in-memory fields need undoing, along with the archive and
            // TDLib. This rollback runs on the UI thread: it is the rare
            // failure path (a full disk), not the common case the worker
            // thread above was for.
            if let Some(account) = self.settings.accounts.iter_mut().find(|a| a.slot == slot) {
                account.lock_salt = prev_salt;
                account.lock_check = prev_check;
            }
            if let Some(archive) = &mut self.session.archive {
                let _ = archive.rekey(self.session.db_key);
            }
            return self.rollback_tdlib_key(client_id, slot, old_key, e);
        }
        self.session.db_key = new_key;
        let done = if new_key.is_some() {
            "пароль установлен"
        } else {
            "пароль снят, данные больше не зашифрованы"
        };
        self.session.password_form = Form::default();
        self.password_done(Ok(done.to_owned()));
        Task::none()
    }

    /// Checks the form, then derives keys off the UI thread.
    fn apply_password(&mut self, remove: bool) -> Task<Msg> {
        let lock = self.locked_slot();
        let form = &mut self.session.password_form;
        if form.busy {
            return Task::none();
        }
        if !remove {
            if form.new.chars().count() < 4 {
                form.message = Some(Err("пароль — не короче 4 символов".into()));
                return Task::none();
            }
            if form.new != form.repeat {
                form.message = Some(Err("пароли не совпадают".into()));
                return Task::none();
            }
        }
        form.busy = true;
        form.message = None;
        let current = std::mem::take(&mut form.current);
        let new = (!remove).then(|| std::mem::take(&mut form.new));
        form.repeat.clear();
        let slot = self.session.slot;
        let client_id = self.session.client_id;
        Task::perform(
            async move {
                tokio::task::spawn_blocking(move || {
                    // Changing or removing asks for the password in use.
                    if let Some((salt, check)) = &lock
                        && lock::check_value(&lock::derive(&current, salt)?) != *check
                    {
                        return Err("текущий пароль неверен".to_owned());
                    }
                    let salt = lock::new_salt();
                    let key = new.map(|p| lock::derive(&p, &salt)).transpose()?;
                    Ok((key, salt))
                })
                .await
                .map_err(|e| e.to_string())?
            },
            move |r| Msg::Password(PasswordMsg::Derived(slot, client_id, r)),
        )
    }

    /// Lock screen shown before TDLib opens the account's database.
    pub(crate) fn view_lock(&self, error: Option<&str>, forgot: bool) -> Element<'_, Msg> {
        let mut form = column![
            text("Клиент защищён паролем").size(20),
            text_input("пароль", &self.session.input)
                .secure(true)
                .on_input(|s| Msg::Password(PasswordMsg::Input(s)))
                .on_submit(Msg::Password(PasswordMsg::Unlock)),
            row![
                button(if self.session.busy {
                    "…"
                } else {
                    "Разблокировать"
                })
                .on_press_maybe((!self.session.busy).then_some(Msg::Password(PasswordMsg::Unlock))),
                button("Забыли пароль?")
                    .style(button::text)
                    .on_press(Msg::Password(PasswordMsg::Forgot(true))),
            ]
            .spacing(8),
        ]
        .spacing(12)
        .width(360);
        if let Some(e) = error {
            form = form.push(text(e.to_owned()).style(text::danger));
        }
        if forgot {
            form = form.push(
                container(
                    column![
                        text(
                            "Без пароля данные не открыть. Можно удалить данные этого аккаунта \
                             на этом компьютере (сессию, кэш, архив удалённых сообщений) и \
                             войти заново: переписка останется в Telegram."
                        )
                        .size(13),
                        row![
                            button("Удалить и войти заново")
                                .style(button::danger)
                                .on_press(Msg::Password(PasswordMsg::ResetAccount)),
                            button("Отмена")
                                .style(button::secondary)
                                .on_press(Msg::Password(PasswordMsg::Forgot(false))),
                        ]
                        .spacing(8),
                    ]
                    .spacing(8),
                )
                .padding(10)
                .style(container::bordered_box),
            );
        }
        container(form).center(Fill).into()
    }

    /// Settings section: set, change or remove the password.
    pub(crate) fn view_password_settings(&self) -> Element<'_, Msg> {
        let form = &self.session.password_form;
        let has = self.locked_slot().is_some();
        let field = |placeholder: &'static str, value: &str, msg: fn(String) -> PasswordMsg| {
            text_input(placeholder, value)
                .secure(true)
                .on_input(move |s| Msg::Password(msg(s)))
                .size(14)
        };
        let mut section = column![
            text("Пароль на клиент").size(16),
            text(
                "Шифрует базу Telegram этого аккаунта и архив удалённых сообщений; \
                 спрашивается при запуске. Скачанные файлы и база плагинов \
                 не шифруются. Забытый пароль не восстановить — только \
                 удалить данные и войти заново."
            )
            .size(12),
        ]
        .spacing(8);
        if has {
            section = section.push(field("текущий пароль", &form.current, PasswordMsg::Current));
        }
        section = section
            .push(field(
                if has {
                    "новый пароль"
                } else {
                    "пароль"
                },
                &form.new,
                PasswordMsg::New,
            ))
            .push(field("ещё раз", &form.repeat, PasswordMsg::Repeat));
        let mut buttons = row![
            button(if has {
                "Сменить пароль"
            } else {
                "Включить"
            })
            .on_press_maybe(
                (!form.busy).then_some(Msg::Password(PasswordMsg::Apply { remove: false }))
            ),
        ]
        .spacing(8);
        if has {
            buttons = buttons.push(button("Снять пароль").style(button::danger).on_press_maybe(
                (!form.busy).then_some(Msg::Password(PasswordMsg::Apply { remove: true })),
            ));
        }
        section = section.push(buttons);
        match &form.message {
            _ if form.busy => section = section.push(text("…").size(13)),
            Some(Ok(m)) => section = section.push(text(m.clone()).size(13).style(text::success)),
            Some(Err(e)) => section = section.push(text(e.clone()).size(13).style(text::danger)),
            None => {}
        }
        section.into()
    }
}
