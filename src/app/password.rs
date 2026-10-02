//! Client password (see `crate::lock`): unlocking at start, turning it on,
//! changing and turning it off from the settings.

use iced::widget::{button, column, container, row, text, text_input};
use iced::{Element, Fill, Task};

use super::{App, Auth, Msg};
use crate::lock::{self, Key, SecretString};
use crate::td;

/// Input of the password section of the settings.
#[derive(Debug, Default)]
pub(crate) struct Form {
    pub(crate) current: SecretString,
    pub(crate) new: SecretString,
    pub(crate) repeat: SecretString,
    pub(crate) busy: bool,
    pub(crate) message: Option<Result<String, String>>,
}

/// Password step, from the forms to the result.
#[derive(Debug, Clone)]
pub(crate) enum PasswordMsg {
    /// Lock screen input and submit.
    Input(SecretString),
    Unlock,
    Unlocked(Result<(Option<Key>, Option<Key>), String>),
    /// "Забыли пароль?" and its confirmation.
    Forgot(bool),
    ResetAccount,
    /// Settings: fields of the form.
    Current(SecretString),
    New(SecretString),
    Repeat(SecretString),
    /// Set or change (with a new password) or remove (without).
    Apply {
        remove: bool,
    },
    /// The key was derived: (current key if checked, new key or none).
    /// Carries the slot and TDLib client it was started for, so a session
    /// swap in between (account switch, log out) drops it instead of
    /// applying someone else's key.
    Derived(u32, i32, Result<(Option<Key>, String), String>),
    /// TDLib has accepted the target; the journal remains until all stores commit.
    Applied(u32, i32, Result<(Option<Key>, String), String>),
    ArchiveRekeyed(u32, i32, Option<Key>, String, Result<(), String>),
}

impl App {
    /// The account of the running slot has a password.
    pub(crate) fn locked_slot(&self) -> Option<(String, String)> {
        let account = self
            .settings
            .accounts
            .iter()
            .find(|a| a.slot == self.session.slot)?;
        if let Some(pending) = &account.pending_key_change {
            return pending
                .old_salt
                .as_ref()
                .zip(pending.old_check.as_ref())
                .or_else(|| pending.new_salt.as_ref().zip(pending.new_check.as_ref()))
                .map(|(salt, check)| (salt.clone(), check.clone()));
        }
        account
            .lock_salt
            .as_ref()
            .zip(account.lock_check.as_ref())
            .map(|(salt, check)| (salt.clone(), check.clone()))
    }

    pub(crate) fn key_transition_active(&self) -> bool {
        self.session.password_form.busy
            || self.session.rekeying_archive.is_some()
            || self
                .settings
                .accounts
                .iter()
                .any(|a| a.slot == self.session.slot && a.pending_key_change.is_some())
    }

    pub(crate) fn on_password(&mut self, msg: PasswordMsg) -> Task<Msg> {
        match msg {
            PasswordMsg::Input(s) => self.session.input = s,
            PasswordMsg::Unlock => {
                let pending = self
                    .settings
                    .accounts
                    .iter()
                    .find(|a| a.slot == self.session.slot)
                    .and_then(|a| a.pending_key_change.clone());
                let normal = self.locked_slot();
                if pending.is_none() && normal.is_none() {
                    return Task::none();
                }
                if self.session.busy {
                    return Task::none();
                }
                self.session.busy = true;
                let password = std::mem::take(&mut self.session.input);
                return Task::perform(
                    async move {
                        tokio::task::spawn_blocking(move || {
                            if let Some(pending) = pending {
                                pending.keys(&password)
                            } else if let Some((salt, check)) = normal {
                                let key = lock::derive(&password, &salt)?;
                                if lock::check_value(&key) == check {
                                    Ok((Some(key), None))
                                } else {
                                    Err("неверный пароль".to_owned())
                                }
                            } else {
                                Err("ключ аккаунта не найден".into())
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
                    Ok((old, new)) => {
                        if self.pending_transition().is_some() {
                            let td_target_confirmed = self.session.cache_params_accepted
                                && self.session.recovery_keys.is_some()
                                && self.session.db_key.as_ref() == new.as_ref();
                            self.session.db_key = if td_target_confirmed {
                                new.clone()
                            } else {
                                old.clone()
                            };
                            self.session.recovery_keys = Some((old, new));
                            self.session.recovery_tried_other = false;
                            self.error = None;
                            if self.session.cache_params_accepted {
                                return self.resume_key_recovery();
                            }
                        } else {
                            self.session.db_key = old;
                            self.error = None;
                        }
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
                if !on {
                    self.session.input.clear();
                }
            }
            PasswordMsg::ResetAccount => {
                if self.key_transition_active() {
                    self.error = Some("сначала завершите восстановление ключа".into());
                    return Task::none();
                }
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
                    account.pending_key_change = None;
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
                    Ok((new_key, salt)) => {
                        if self.pending_transition().is_some() {
                            self.password_done(Err("уже идёт восстановление ключа".into()));
                            return Task::none();
                        }
                        let slot = self.session.slot;
                        let old = self.settings.accounts.iter().find(|a| a.slot == slot);
                        let (old_salt, old_check) = old
                            .map(|a| (a.lock_salt.clone(), a.lock_check.clone()))
                            .unwrap_or_default();
                        let new_salt = new_key.as_ref().map(|_| salt);
                        let new_check = new_key.as_ref().map(lock::check_value);
                        let journal = match crate::settings::PendingKeyChange::new(
                            old_salt,
                            old_check,
                            new_salt,
                            new_check,
                            self.session.db_key.as_ref(),
                            new_key.as_ref(),
                        ) {
                            Ok(journal) => journal,
                            Err(e) => {
                                self.password_done(Err(e));
                                return Task::none();
                            }
                        };
                        if !self.settings.accounts.iter().any(|a| a.slot == slot) {
                            self.settings.accounts.push(crate::settings::Account {
                                slot,
                                ..Default::default()
                            });
                        }
                        self.settings
                            .accounts
                            .iter_mut()
                            .find(|a| a.slot == slot)
                            .expect("account created")
                            .pending_key_change = Some(journal);
                        self.session.recovery_keys = Some((self.session.db_key.clone(), new_key));
                        return self.resume_key_recovery();
                    }
                    Err(e) => self.password_done(Err(e)),
                }
            }
            PasswordMsg::Applied(slot, client_id, result) => {
                if slot != self.session.slot || client_id != self.session.client_id {
                    return Task::none();
                }
                let Some(pending) = self.pending_transition() else {
                    self.error = Some("журнал смены ключа отсутствует".into());
                    return Task::none();
                };
                let target_check = self
                    .session
                    .recovery_keys
                    .as_ref()
                    .and_then(|(_, target)| target.as_ref().map(lock::check_value));
                if self.session.recovery_keys.is_none() || target_check != pending.new_check {
                    self.recovery_failed("ключ перехода не совпадает с журналом".into());
                    return Task::none();
                }
                match result {
                    Ok((new_key, salt)) => {
                        if new_key.as_ref().map(lock::check_value) != pending.new_check
                            || new_key.as_ref().map(|_| salt.as_str())
                                != pending.new_salt.as_deref()
                        {
                            self.recovery_failed("ответ ключа не совпадает с журналом".into());
                            return Task::none();
                        }
                        // A successful native reply establishes this candidate.
                        // Keep it through archive/settings failures for safe retry.
                        self.session.db_key = new_key.clone();
                        let old_key = self
                            .session
                            .recovery_keys
                            .as_ref()
                            .and_then(|(old, _)| old.clone());
                        let mut archive = self
                            .session
                            .archive
                            .take()
                            .or_else(|| self.session.detached_archive.take());
                        if archive.is_none() {
                            let user = self
                                .settings
                                .accounts
                                .iter()
                                .find(|a| a.slot == slot)
                                .and_then(|a| a.user_id)
                                .or(self.session.my_id);
                            if let Some(user) = user {
                                match crate::archive::Archive::open_for_account(user) {
                                    Ok(opened) => archive = Some(opened),
                                    Err(e) => {
                                        self.recovery_failed(format!("архив: {e}"));
                                        return Task::none();
                                    }
                                }
                            }
                        }
                        let Some(mut archive) = archive else {
                            let durable_deletions =
                                pending.deleted_ids.values().any(|ids| !ids.is_empty())
                                    || pending.purged_ids.values().any(|ids| !ids.is_empty());
                            if !self.session.deferred_archive.is_empty() || durable_deletions {
                                self.recovery_failed(
                                    "архив недоступен для отложенных сообщений".into(),
                                );
                                return Task::none();
                            }
                            return self.finish_password_change(slot, new_key);
                        };
                        let (tx, rx) = tokio::sync::oneshot::channel();
                        self.session.rekeying_archive = Some(rx);
                        let target = new_key.clone();
                        return Task::perform(
                            async move {
                                match tokio::task::spawn_blocking(move || {
                                    let result = archive
                                        .recover_rekey(old_key, target)
                                        .map_err(|e| e.to_string());
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
                                    slot, client_id, new_key, salt, result,
                                ))
                            },
                        );
                    }
                    Err(e) => self.recovery_failed(format!("ключ TDLib: {e}")),
                }
            }
            PasswordMsg::ArchiveRekeyed(slot, client_id, new_key, _salt, result) => {
                let archive = self
                    .session
                    .rekeying_archive
                    .take()
                    .and_then(|mut rx| rx.try_recv().ok());
                if slot != self.session.slot || client_id != self.session.client_id {
                    return Task::none();
                }
                let Some(mut archive) = archive else {
                    self.recovery_failed("архив не вернул соединение".into());
                    return Task::none();
                };
                if matches!(self.session.auth, Auth::Locked { .. })
                    && !self.session.password_form.busy
                {
                    // A permanent deletion could not be synced into the
                    // journal. Do not clear it on an in-flight worker reply.
                    self.session.detached_archive = Some(archive);
                    return Task::none();
                }
                if let Err(e) = result {
                    self.session.detached_archive = Some(archive);
                    self.recovery_failed(format!("архив: {e}"));
                    return Task::none();
                }
                if let Err(e) = archive.verify_key(new_key.as_ref()) {
                    self.session.detached_archive = Some(archive);
                    self.recovery_failed(format!("неверный ключ архива: {e}"));
                    return Task::none();
                }
                if let Err(e) = self.flush_deferred_archive(&mut archive) {
                    self.session.detached_archive = Some(archive);
                    self.recovery_failed(format!("отложенные изменения архива: {e}"));
                    return Task::none();
                }
                if let Some(warning) = archive.maintenance_warning() {
                    self.error = Some(format!("очистка архива: {warning}"));
                }
                self.session.archive = Some(archive);
                return self.finish_password_change(slot, new_key);
            }
        }
        Task::none()
    }

    pub(crate) fn pending_transition(&self) -> Option<crate::settings::PendingKeyChange> {
        self.settings
            .accounts
            .iter()
            .find(|a| a.slot == self.session.slot)?
            .pending_key_change
            .clone()
    }

    /// Saving the journal succeeds durably before every possible TDLib mutation.
    pub(crate) fn resume_key_recovery(&mut self) -> Task<Msg> {
        let Some(pending) = self.pending_transition() else {
            return Task::none();
        };
        if let Err(e) = self.try_save_settings() {
            self.recovery_failed(format!("журнал ключа: {e}"));
            return Task::none();
        }
        let Some((_, target)) = &self.session.recovery_keys else {
            self.recovery_failed("ключи восстановления недоступны".into());
            return Task::none();
        };
        self.session.password_form.busy = true;
        let client = self.session.client_id;
        let slot = self.session.slot;
        let target = target.clone();
        let salt = pending.new_salt.unwrap_or_default();
        if self.session.cache_params_accepted && self.session.db_key.as_ref() == target.as_ref() {
            // Successful set_parameters authenticated the target key; there
            // is no TDLib mutation to repeat after a crash at this point.
            return self.on_password(PasswordMsg::Applied(slot, client, Ok((target, salt))));
        }
        let key = lock::tdlib_key(target.as_ref());
        Task::perform(
            async move { td::set_db_key(client, key).await.map(|()| (target, salt)) },
            move |result| Msg::Password(PasswordMsg::Applied(slot, client, result)),
        )
    }

    fn password_done(&mut self, result: Result<String, String>) {
        self.session.password_form.current.clear();
        self.session.password_form.new.clear();
        self.session.password_form.repeat.clear();
        self.session.password_form.busy = false;
        self.session.password_form.message = Some(result);
    }

    pub(crate) fn recovery_failed(&mut self, error: String) {
        self.password_done(Err(format!("{error}; ключ не завершён, повторите пароль")));
        self.session.busy = false;
        self.session.auth = Auth::Locked {
            error: Some(format!("{error}; повторите пароль для восстановления")),
            forgot: false,
        };
    }

    /// Metadata is first durably saved while the journal still holds both
    /// candidates. Only a second durable save may remove that journal.
    fn finish_password_change(&mut self, slot: u32, new_key: Option<Key>) -> Task<Msg> {
        let Some(account) = self.settings.accounts.iter_mut().find(|a| a.slot == slot) else {
            self.recovery_failed("аккаунт не найден".into());
            return Task::none();
        };
        let Some(journal) = account.pending_key_change.clone() else {
            self.recovery_failed("журнал ключа не найден".into());
            return Task::none();
        };
        account.lock_salt = journal.new_salt.clone();
        account.lock_check = journal.new_check.clone();
        if let Err(e) = self.try_save_settings() {
            self.recovery_failed(format!("метаданные ключа: {e}"));
            return Task::none();
        }
        self.settings
            .accounts
            .iter_mut()
            .find(|a| a.slot == slot)
            .expect("account present")
            .pending_key_change = None;
        if let Err(e) = self.try_save_settings() {
            self.settings
                .accounts
                .iter_mut()
                .find(|a| a.slot == slot)
                .expect("account present")
                .pending_key_change = Some(journal);
            self.recovery_failed(format!("очистка журнала: {e}"));
            return Task::none();
        }
        self.session.db_key = new_key.clone();
        self.session.recovery_keys = None;
        let done = if new_key.is_some() {
            "пароль установлен"
        } else {
            "пароль снят, данные больше не зашифрованы"
        };
        self.session.password_form = Form::default();
        self.password_done(Ok(done.to_owned()));
        let known = self
            .session
            .my_id
            .map(|user| self.account_known(user))
            .unwrap_or_else(Task::none);
        if self.session.recovery_ready_pending {
            self.session.recovery_ready_pending = false;
            return Task::batch([
                known,
                self.on_auth_state(tdlib_rs::enums::AuthorizationState::Ready),
            ]);
        }
        if matches!(self.session.auth, Auth::Locked { .. }) {
            self.session.auth = Auth::Starting;
        }
        known
    }

    /// Checks the form, then derives keys off the UI thread.
    fn apply_password(&mut self, remove: bool) -> Task<Msg> {
        if self.session.password_form.busy || self.session.rekeying_archive.is_some() {
            return Task::none();
        }
        let known = self.session.my_id.is_some()
            || self
                .settings
                .accounts
                .iter()
                .any(|a| a.slot == self.session.slot && a.user_id.is_some());
        if known && self.session.archive.is_none() {
            self.password_done(Err("архив недоступен; пароль не изменён".into()));
            return Task::none();
        }
        if self.pending_transition().is_some() {
            self.password_done(Err("сначала восстановите прежнюю смену ключа".into()));
            return Task::none();
        }
        let lock = self.locked_slot();
        if remove && lock.is_none() {
            self.password_done(Err("пароль не установлен".into()));
            return Task::none();
        }
        let form = &mut self.session.password_form;
        if !remove {
            if form.new.chars().count() < 4 {
                form.message = Some(Err("пароль — не короче 4 символов".into()));
                form.current.clear();
                form.new.clear();
                form.repeat.clear();
                return Task::none();
            }
            if form.new != form.repeat {
                form.message = Some(Err("пароли не совпадают".into()));
                form.current.clear();
                form.new.clear();
                form.repeat.clear();
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
                .on_input(|s| Msg::Password(PasswordMsg::Input(s.into())))
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
                                .on_press_maybe(
                                    (!self.key_transition_active())
                                        .then_some(Msg::Password(PasswordMsg::ResetAccount))
                                ),
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
        let field =
            |placeholder: &'static str, value: &str, msg: fn(SecretString) -> PasswordMsg| {
                text_input(placeholder, value)
                    .secure(true)
                    .on_input(move |s| Msg::Password(msg(s.into())))
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
