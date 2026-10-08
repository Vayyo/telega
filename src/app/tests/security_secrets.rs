//! Offline password-transition regressions; returned TDLib tasks are never run.
use super::*;
use crate::app::password::PasswordMsg;
use iced_runtime::task::into_stream;

fn password_change_in_progress() -> App {
    let mut app = app();
    app.settings.accounts = vec![
        Account {
            slot: 0,
            user_id: Some(501),
            name: "synthetic account A".into(),
            ..Default::default()
        },
        Account {
            slot: 3,
            user_id: Some(502),
            name: "synthetic account B".into(),
            ..Default::default()
        },
    ];
    app.session.my_id = Some(501);
    drop(app.update(Msg::Password(PasswordMsg::New("synthetic-secret".into()))));
    drop(app.update(Msg::Password(PasswordMsg::Repeat(
        "synthetic-secret".into(),
    ))));
    // The task is intentionally unpolled: no TDLib call or key derivation.
    drop(app.update(Msg::Password(PasswordMsg::Apply { remove: false })));
    assert!(
        app.session.password_form.busy,
        "a valid Apply starts a transition"
    );
    app
}

#[test]
fn security_fix_password_change_blocks_switch_and_preserves_current_session() {
    let mut app = password_change_in_progress();
    let client = app.session.client_id;
    drop(app.update(Msg::SwitchAccount(3)));
    assert_eq!(app.session.leave, None);
    assert_eq!(app.session.slot, 0);
    assert_eq!(app.session.client_id, client);
    assert_eq!(app.session.auth, Auth::Ready);
    assert!(app.session.password_form.busy);
    assert_eq!(app.after_close(), 0);
}

#[test]
fn security_fix_password_change_blocks_add_without_saving_a_new_account() {
    let mut app = password_change_in_progress();
    let original = app.settings.accounts.clone();
    drop(app.update(Msg::AddAccount));
    assert_eq!(app.settings.accounts, original);
    assert_eq!(app.session.leave, None);
    assert_eq!(app.session.slot, 0);
    assert_eq!(app.session.auth, Auth::Ready);
    assert!(app.session.password_form.busy);
}

#[test]
fn security_fix_password_change_blocks_confirmed_logout() {
    let mut app = password_change_in_progress();
    assert!(
        into_stream(app.update(Msg::ConfirmLogOut(true))).is_none(),
        "logout must not schedule a TDLib request while changing the key"
    );
    assert_eq!(app.session.auth, Auth::Ready);
    assert_eq!(app.session.leave, None);
    assert_eq!(app.session.slot, 0);
    assert!(app.session.password_form.busy);
}

#[cfg(target_os = "linux")]
#[test]
fn security_fix_password_change_blocks_tray_quit() {
    let mut app = password_change_in_progress();
    assert!(
        into_stream(app.update(Msg::Tray(tray::TrayEvent::Quit))).is_none(),
        "quit must not produce an exit action while a key transition is in flight"
    );
    assert_eq!(app.session.auth, Auth::Ready);
    assert_eq!(app.session.slot, 0);
    assert!(app.session.password_form.busy);
}

#[cfg(target_os = "linux")]
#[test]
fn security_fix_password_change_blocks_main_window_close() {
    let mut app = password_change_in_progress();
    let window = app.main_window;
    assert!(
        into_stream(app.update(Msg::WindowCloseRequested(window))).is_none(),
        "closing the last window must not exit while a key transition is in flight"
    );
    assert_eq!(app.main_window_state, MainWindowState::Open);
    assert_eq!(app.session.auth, Auth::Ready);
    assert_eq!(app.session.slot, 0);
    assert!(app.session.password_form.busy);
}

#[test]
fn security_fix_password_apply_rejects_known_account_without_its_archive() {
    let mut app = app();
    app.settings.accounts.push(Account {
        slot: 0,
        user_id: Some(501),
        ..Default::default()
    });
    app.session.my_id = Some(501);
    app.session.archive = None; // Simulates an existing archive that could not be opened.
    drop(app.update(Msg::Password(PasswordMsg::New("synthetic-secret".into()))));
    drop(app.update(Msg::Password(PasswordMsg::Repeat(
        "synthetic-secret".into(),
    ))));

    assert!(
        into_stream(app.update(Msg::Password(PasswordMsg::Apply { remove: false }))).is_none(),
        "an unavailable existing archive must not start TDLib key derivation"
    );
    assert!(!app.session.password_form.busy);
    assert!(matches!(
        &app.session.password_form.message,
        Some(Err(error)) if error.contains("архив")
    ));
    assert_eq!(app.session.db_key, None);
    assert_eq!(app.settings.accounts[0].lock_salt, None);
    assert_eq!(app.settings.accounts[0].lock_check, None);
    assert_eq!(app.session.auth, Auth::Ready);
}

#[test]
fn security_fix_password_apply_without_a_previous_account_can_create_its_first_archive() {
    let mut app = app();
    app.session.archive = None;
    assert_eq!(app.session.my_id, None);
    drop(app.update(Msg::Password(PasswordMsg::New("synthetic-secret".into()))));
    drop(app.update(Msg::Password(PasswordMsg::Repeat(
        "synthetic-secret".into(),
    ))));
    drop(app.update(Msg::Password(PasswordMsg::Apply { remove: false })));
    assert!(app.session.password_form.busy);
    assert_eq!(app.session.password_form.message, None);
}

#[test]
fn security_weak_pin_is_rejected_before_derivation() {
    let mut app = app();
    drop(app.update(Msg::Password(PasswordMsg::New("1234".into()))));
    drop(app.update(Msg::Password(PasswordMsg::Repeat("1234".into()))));

    let task = app.update(Msg::Password(PasswordMsg::Apply { remove: false }));

    assert!(
        into_stream(task).is_none(),
        "a four-character PIN must not schedule key derivation"
    );
    assert!(!app.session.password_form.busy);
    assert!(matches!(
        &app.session.password_form.message,
        Some(Err(error)) if error.contains("12")
    ));
    assert_eq!(app.session.db_key, None);
}

#[test]
fn security_new_password_boundary_is_twelve_characters() {
    for (password, accepted) in [("12345678901", false), ("123456789012", true)] {
        let mut app = app();
        drop(app.update(Msg::Password(PasswordMsg::New(password.into()))));
        drop(app.update(Msg::Password(PasswordMsg::Repeat(password.into()))));
        let task = app.update(Msg::Password(PasswordMsg::Apply { remove: false }));
        assert_eq!(
            into_stream(task).is_some(),
            accepted,
            "new password boundary: {password}"
        );
        assert_eq!(app.session.password_form.busy, accepted);
    }
}

/// Owns only the synthetic slot and archive obstruction under the test data root.
struct ResetFailurePaths {
    slot_dir: std::path::PathBuf,
    archive: std::path::PathBuf,
}

impl ResetFailurePaths {
    fn new() -> (Self, u32, i64) {
        let accounts = crate::paths::base().join("accounts");
        std::fs::create_dir_all(&accounts).unwrap();
        let mut slot = 1_000_000;
        let slot_dir = loop {
            let path = crate::paths::account_dir(slot);
            match std::fs::create_dir(&path) {
                Ok(()) => break path,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => slot += 1,
                Err(error) => panic!("cannot reserve synthetic account slot: {error}"),
            }
        };
        let mut user_id = 1_000_000_000_000_i64;
        let archive = loop {
            let path = crate::paths::archive(user_id);
            match std::fs::create_dir(&path) {
                Ok(()) => break path,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => user_id += 1,
                Err(error) => {
                    let _ = std::fs::remove_dir(&slot_dir);
                    panic!("cannot reserve synthetic archive path: {error}");
                }
            }
        };
        (Self { slot_dir, archive }, slot, user_id)
    }
}

impl Drop for ResetFailurePaths {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir(&self.archive);
        let _ = std::fs::remove_dir(&self.slot_dir);
    }
}

#[test]
fn security_reset_retains_key_after_archive_remove_failure() {
    let (_fixture, slot, user_id) = ResetFailurePaths::new();
    let salt = "synthetic-reset-salt";
    let check =
        crate::lock::check_value(&crate::lock::derive("synthetic-reset-password", salt).unwrap());
    let mut app = app();
    app.session.slot = slot;
    app.session.archive = None;
    app.session.auth = Auth::Locked {
        error: None,
        forgot: true,
    };
    app.settings.accounts.push(Account {
        slot,
        user_id: Some(user_id),
        name: "synthetic locked account".into(),
        lock_salt: Some(salt.into()),
        lock_check: Some(check.clone()),
        ..Default::default()
    });
    let before = app.settings.clone();
    app.settings.save(&app.settings_path).unwrap();

    let task = app.update(Msg::Password(PasswordMsg::ResetAccount));

    assert_eq!(app.settings.accounts[0].lock_salt.as_deref(), Some(salt));
    assert_eq!(
        app.settings.accounts[0].lock_check.as_deref(),
        Some(check.as_str())
    );
    assert_eq!(app.settings.accounts[0].user_id, Some(user_id));
    assert_eq!(app.settings, before);
    assert_eq!(Settings::load(&app.settings_path).unwrap(), before);
    assert!(
        app.error
            .as_deref()
            .is_some_and(|error| error.contains("данные аккаунта"))
    );
    assert!(matches!(app.session.auth, Auth::Locked { .. }));
    assert!(
        !app.session.busy,
        "reset failure must not restart TDLib parameters"
    );
    assert!(
        into_stream(task).is_none(),
        "reset failure must not schedule TDLib parameters"
    );
}

#[test]
fn security_unlock_origin_stale_client_result_cannot_unlock_the_new_session() {
    let mut app = app();
    let old_key = crate::lock::derive("synthetic old password", "synthetic-old-salt").unwrap();
    let new_key = crate::lock::derive("synthetic new password", "synthetic-new-salt").unwrap();
    app.settings.accounts = vec![
        Account {
            slot: 0,
            user_id: Some(501),
            lock_salt: Some("synthetic-old-salt".into()),
            lock_check: Some(crate::lock::check_value(&old_key)),
            ..Default::default()
        },
        Account {
            slot: 3,
            user_id: Some(502),
            lock_salt: Some("synthetic-new-salt".into()),
            lock_check: Some(crate::lock::check_value(&new_key)),
            ..Default::default()
        },
    ];
    app.session.auth = Auth::Locked {
        error: None,
        forgot: false,
    };
    app.session.archive = None;
    drop(app.update(Msg::Password(PasswordMsg::Input(
        "synthetic old password".into(),
    ))));
    drop(app.update(Msg::Password(PasswordMsg::Unlock)));
    assert!(app.session.busy, "old unlock must be in flight");
    let old_client = app.session.client_id;

    app.session = Session::new(old_client + 1, app.main_window, 3);
    app.session.auth = Auth::Locked {
        error: None,
        forgot: false,
    };
    drop(app.update(Msg::Password(PasswordMsg::Input(
        "synthetic new password".into(),
    ))));
    drop(app.update(Msg::Password(PasswordMsg::Unlock)));
    assert!(app.session.busy, "new unlock must be in flight");
    assert_eq!(app.session.db_key, None);

    drop(app.update(Msg::ForClient(
        old_client,
        Box::new(Msg::Password(PasswordMsg::Unlocked(Ok((
            Some(old_key),
            None,
        ))))),
    )));

    assert_eq!(app.session.slot, 3);
    assert_eq!(app.session.client_id, old_client + 1);
    assert_eq!(
        app.session.db_key, None,
        "old client's key must not be adopted"
    );
    assert!(
        app.session.busy,
        "old completion must not finish the new unlock"
    );
    assert_eq!(
        app.session.auth,
        Auth::Locked {
            error: None,
            forgot: false
        }
    );
}

#[test]
fn security_unlock_origin_reset_while_unlock_busy_preserves_account_and_schedules_no_tdlib() {
    let (fixture, slot, user_id) = ResetFailurePaths::new();
    // This fixture owns both paths. Leave the archive absent so an unguarded
    // reset would succeed and expose the metadata loss instead of failing on I/O.
    std::fs::remove_dir(&fixture.archive).unwrap();
    let key = crate::lock::derive("synthetic reset password", "synthetic-reset-salt").unwrap();
    let mut app = app();
    app.session.slot = slot;
    app.session.archive = None;
    app.session.auth = Auth::Locked {
        error: None,
        forgot: true,
    };
    app.settings.accounts.push(Account {
        slot,
        user_id: Some(user_id),
        name: "synthetic locked account".into(),
        lock_salt: Some("synthetic-reset-salt".into()),
        lock_check: Some(crate::lock::check_value(&key)),
        ..Default::default()
    });
    let before = app.settings.clone();
    app.settings.save(&app.settings_path).unwrap();
    drop(app.update(Msg::Password(PasswordMsg::Input(
        "synthetic reset password".into(),
    ))));
    drop(app.update(Msg::Password(PasswordMsg::Unlock)));
    assert!(app.session.busy, "Argon2 unlock must be in flight");

    let task = app.update(Msg::Password(PasswordMsg::ResetAccount));

    assert_eq!(app.settings, before, "reset must not clear lock metadata");
    assert_eq!(Settings::load(&app.settings_path).unwrap(), before);
    assert!(
        fixture.slot_dir.is_dir(),
        "reset must not remove TDLib data"
    );
    assert!(
        app.error.as_deref().is_some_and(|error| !error.is_empty()),
        "reset while unlock is in progress must report failure"
    );
    assert!(
        app.session.busy,
        "reset must not interrupt the in-flight unlock"
    );
    assert!(matches!(app.session.auth, Auth::Locked { .. }));
    assert!(
        into_stream(task).is_none(),
        "reset while unlock is in progress must not schedule a TDLib request"
    );
}

#[test]
fn security_fix_stale_password_replies_cannot_change_the_current_account() {
    let mut app = password_change_in_progress();
    let stale_client = app.session.client_id + 1;
    let new_key = crate::lock::derive("synthetic-new-key", "synthetic-new-salt").unwrap();
    let slot = app.session.slot;
    drop(app.update(Msg::Password(PasswordMsg::Derived(
        slot,
        stale_client,
        Ok((Some(new_key.clone()), "stale-salt".into())),
    ))));
    drop(app.update(Msg::Password(PasswordMsg::Applied(
        slot,
        stale_client,
        Ok((Some(new_key.clone()), "stale-salt".into())),
    ))));
    drop(app.update(Msg::Password(PasswordMsg::ArchiveRekeyed(
        slot,
        stale_client,
        Some(new_key),
        "stale-salt".into(),
        Ok(()),
    ))));
    assert_eq!(app.session.slot, slot);
    assert_eq!(app.session.auth, Auth::Ready);
    assert_eq!(app.session.db_key, None);
    assert!(app.session.archive.is_some());
    assert!(app.settings.accounts[0].lock_salt.is_none());
    assert!(app.settings.accounts[0].lock_check.is_none());
    assert!(app.session.password_form.busy);
}

struct RecoverySettings {
    path: std::path::PathBuf,
    dir: std::path::PathBuf,
}

impl RecoverySettings {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "telega-security-fix-recovery-{}-{n}",
            std::process::id()
        ));
        Self {
            path: dir.join("settings.json"),
            dir,
        }
    }
}

impl Drop for RecoverySettings {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn security_fix_password_change_journal_survives_restart_with_either_password_without_plain_keys() {
    let file = RecoverySettings::new();
    let old = crate::lock::derive("synthetic old password", "synthetic-old-salt").unwrap();
    let new = crate::lock::derive("synthetic new password", "synthetic-new-salt").unwrap();
    let pending = crate::settings::PendingKeyChange::new(
        Some("synthetic-old-salt".into()),
        Some(crate::lock::check_value(&old)),
        Some("synthetic-new-salt".into()),
        Some(crate::lock::check_value(&new)),
        Some(&old),
        Some(&new),
    )
    .unwrap();
    let mut settings = Settings::default();
    settings.accounts.push(Account {
        slot: 0,
        user_id: Some(501),
        pending_key_change: Some(pending),
        ..Default::default()
    });
    settings.save(&file.path).unwrap();
    let json = std::fs::read_to_string(&file.path).unwrap();
    assert!(!json.contains(&*crate::lock::tdlib_key(Some(&old))));
    assert!(!json.contains(&*crate::lock::tdlib_key(Some(&new))));

    let restarted = Settings::load(&file.path).unwrap();
    let pending = restarted.accounts[0].pending_key_change.as_ref().unwrap();
    assert_eq!(
        pending.keys("synthetic old password").unwrap(),
        (Some(old.clone()), Some(new.clone()))
    );
    assert_eq!(
        pending.keys("synthetic new password").unwrap(),
        (Some(old), Some(new))
    );
    assert!(pending.keys("wrong password").is_err());
}

#[test]
fn security_fix_password_enable_and_remove_journals_recover_the_only_available_password() {
    for (old_password, new_password) in [
        (None, Some("synthetic new password")),
        (Some("synthetic old password"), None),
    ] {
        let file = RecoverySettings::new();
        let old = old_password
            .map(|password| crate::lock::derive(password, "synthetic-old-salt").unwrap());
        let new = new_password
            .map(|password| crate::lock::derive(password, "synthetic-new-salt").unwrap());
        let pending = crate::settings::PendingKeyChange::new(
            old.as_ref().map(|_| "synthetic-old-salt".into()),
            old.as_ref().map(crate::lock::check_value),
            new.as_ref().map(|_| "synthetic-new-salt".into()),
            new.as_ref().map(crate::lock::check_value),
            old.as_ref(),
            new.as_ref(),
        )
        .unwrap();
        let mut settings = Settings::default();
        settings.accounts.push(Account {
            slot: 0,
            user_id: Some(501),
            pending_key_change: Some(pending),
            ..Default::default()
        });
        settings.save(&file.path).unwrap();
        let restarted = Settings::load(&file.path).unwrap();
        let recovered = restarted.accounts[0].pending_key_change.as_ref().unwrap();
        assert_eq!(
            recovered
                .keys(old_password.or(new_password).unwrap())
                .unwrap(),
            (old, new)
        );
        assert!(recovered.keys("wrong password").is_err());
    }
}

#[test]
fn security_fix_pending_recovery_blocks_transitions_even_after_busy_clears() {
    let mut app = app();
    let key = crate::lock::derive("synthetic new password", "synthetic-new-salt").unwrap();
    app.settings.accounts = vec![
        Account {
            slot: 0,
            user_id: Some(501),
            pending_key_change: Some(
                crate::settings::PendingKeyChange::new(
                    None,
                    None,
                    Some("synthetic-new-salt".into()),
                    Some(crate::lock::check_value(&key)),
                    None,
                    Some(&key),
                )
                .unwrap(),
            ),
            ..Default::default()
        },
        Account {
            slot: 3,
            user_id: Some(502),
            ..Default::default()
        },
    ];
    assert!(!app.session.password_form.busy);
    let before = app.settings.accounts.clone();
    drop(app.update(Msg::SwitchAccount(3)));
    drop(app.update(Msg::AddAccount));
    assert!(into_stream(app.update(Msg::ConfirmLogOut(true))).is_none());
    #[cfg(target_os = "linux")]
    {
        assert!(into_stream(app.update(Msg::Tray(tray::TrayEvent::Quit))).is_none());
        assert!(into_stream(app.update(Msg::WindowCloseRequested(app.main_window))).is_none());
        assert_eq!(app.main_window_state, MainWindowState::Open);
    }
    assert_eq!(app.settings.accounts, before);
    assert_eq!(app.session.leave, None);
    assert_eq!(app.session.slot, 0);
    assert_eq!(app.session.auth, Auth::Ready);
}

#[test]
fn security_fix_failed_final_settings_save_keeps_recovery_journal_and_reports_failure() {
    let mut app = app();
    let key = crate::lock::derive("synthetic new password", "synthetic-new-salt").unwrap();
    let pending = crate::settings::PendingKeyChange::new(
        None,
        None,
        Some("synthetic-new-salt".into()),
        Some(crate::lock::check_value(&key)),
        None,
        Some(&key),
    )
    .unwrap();
    app.settings.accounts.push(Account {
        slot: 0,
        user_id: Some(501),
        pending_key_change: Some(pending.clone()),
        ..Default::default()
    });
    app.settings.save(&app.settings_path).unwrap();
    let mut archive = app.session.archive.take().unwrap();
    archive.recover_rekey(None, Some(key.clone())).unwrap();
    let (tx, rx) = tokio::sync::oneshot::channel();
    tx.send(archive).ok().unwrap();
    app.session.rekeying_archive = Some(rx);
    app.settings_load_failed = true; // Only the final metadata write fails.
    let slot = app.session.slot;
    let client = app.session.client_id;
    drop(app.update(Msg::Password(PasswordMsg::ArchiveRekeyed(
        slot,
        client,
        Some(key.clone()),
        "synthetic-new-salt".into(),
        Ok(()),
    ))));

    assert!(matches!(&app.session.password_form.message, Some(Err(_))));
    assert!(matches!(app.session.auth, Auth::Locked { .. }));
    assert!(app.settings.accounts[0].pending_key_change.is_some());
    let saved = Settings::load(&app.settings_path).unwrap();
    assert_eq!(saved.accounts[0].pending_key_change, Some(pending));
    assert_eq!(
        saved.accounts[0]
            .pending_key_change
            .as_ref()
            .unwrap()
            .keys("synthetic new password")
            .unwrap(),
        (None, Some(key.clone()))
    );
    assert!(
        app.session
            .archive
            .as_ref()
            .unwrap()
            .verify_key(Some(&key))
            .is_ok()
    );
    assert_ne!(app.session.db_key, Some(key));
}

#[test]
fn security_fix_deletion_and_edits_during_rekey_reach_verified_archive_in_order() {
    let mut app = app();
    let old = crate::lock::derive("synthetic-old-key", "synthetic-old-salt").unwrap();
    let new = crate::lock::derive("synthetic-new-key", "synthetic-new-salt").unwrap();
    app.session
        .archive
        .as_mut()
        .unwrap()
        .set_key(Some(old.clone()));
    open(&mut app, 1, &[(10, "original")]);
    let mut archive = app.session.archive.take().unwrap();
    assert!(archive.verify_key(Some(&old)).is_ok());
    app.settings.accounts.push(Account {
        slot: 0,
        user_id: Some(501),
        pending_key_change: Some(
            crate::settings::PendingKeyChange::new(
                Some("synthetic-old-salt".into()),
                Some(crate::lock::check_value(&old)),
                Some("synthetic-new-salt".into()),
                Some(crate::lock::check_value(&new)),
                Some(&old),
                Some(&new),
            )
            .unwrap(),
        ),
        ..Default::default()
    });
    app.session.db_key = Some(old.clone());
    app.session.recovery_keys = Some((Some(old.clone()), Some(new.clone())));
    app.session.password_form.busy = true;
    new_message(&mut app, 1, 11, "new while changing keys");
    td(
        &mut app,
        json!({"@type": "updateMessageContent", "chat_id": 1, "message_id": 11,
               "new_content": {"@type": "messageText", "text": {
                   "@type": "formattedText", "text": "new message edited in flight",
                   "entities": []}}}),
    );
    td(
        &mut app,
        json!({"@type": "updateMessageContent", "chat_id": 1, "message_id": 10,
               "new_content": {"@type": "messageText", "text": {
                   "@type": "formattedText", "text": "edited while changing keys",
                   "entities": []}}}),
    );
    delete(&mut app, 1, &[10, 11], false);
    archive
        .recover_rekey(Some(old.clone()), Some(new.clone()))
        .unwrap();
    let (tx, rx) = tokio::sync::oneshot::channel();
    tx.send(archive).ok().unwrap();
    app.session.rekeying_archive = Some(rx);
    let slot = app.session.slot;
    let client = app.session.client_id;
    drop(app.update(Msg::Password(PasswordMsg::ArchiveRekeyed(
        slot,
        client,
        Some(new.clone()),
        "synthetic-new-salt".into(),
        Ok(()),
    ))));

    let archive = app.session.archive.as_ref().unwrap();
    assert!(archive.verify_key(Some(&old)).is_err());
    assert!(archive.verify_key(Some(&new)).is_ok());
    let mut deleted: Vec<_> = archive
        .deleted(1, 0, 12)
        .unwrap()
        .into_iter()
        .map(|m| (m.id, m.text))
        .collect();
    deleted.sort_by_key(|(id, _)| *id);
    assert_eq!(
        deleted,
        [
            (10, "edited while changing keys".to_owned()),
            (11, "new message edited in flight".to_owned())
        ]
    );
    assert!(app.settings.accounts[0].pending_key_change.is_none());
    open(&mut app, 1, &[]);
    let mut visible: Vec<_> = shown(&app)
        .into_iter()
        .map(|(id, text, deleted)| (id, text.to_owned(), deleted))
        .collect();
    visible.sort_by_key(|(id, _, _)| *id);
    assert_eq!(
        visible,
        [
            (10, "edited while changing keys".to_owned(), true),
            (11, "new message edited in flight".to_owned(), true)
        ]
    );
}

#[test]
fn security_fix_failed_archive_recovery_keeps_unverified_archive_detached_during_edits() {
    let mut app = app();
    let old = crate::lock::derive("synthetic-old-key", "synthetic-old-salt").unwrap();
    let new = crate::lock::derive("synthetic-new-key", "synthetic-new-salt").unwrap();
    let corrupted = crate::lock::derive("different-key", "synthetic-wrong-salt").unwrap();
    app.session
        .archive
        .as_mut()
        .unwrap()
        .set_key(Some(corrupted));
    new_message(&mut app, 1, 10, "encrypted archive text");
    let mut archive = app.session.archive.take().unwrap();
    archive.set_key(None); // The handle reopened on restart has not been verified.
    let error = archive
        .recover_rekey(Some(old.clone()), Some(new.clone()))
        .unwrap_err()
        .to_string();
    app.settings.accounts.push(Account {
        slot: 0,
        user_id: Some(501),
        pending_key_change: Some(
            crate::settings::PendingKeyChange::new(
                Some("synthetic-old-salt".into()),
                Some(crate::lock::check_value(&old)),
                Some("synthetic-new-salt".into()),
                Some(crate::lock::check_value(&new)),
                Some(&old),
                Some(&new),
            )
            .unwrap(),
        ),
        ..Default::default()
    });
    app.session.recovery_keys = Some((Some(old), Some(new.clone())));
    app.session.password_form.busy = true;
    let (tx, rx) = tokio::sync::oneshot::channel();
    tx.send(archive).ok().unwrap();
    app.session.rekeying_archive = Some(rx);
    let slot = app.session.slot;
    let client = app.session.client_id;
    drop(app.update(Msg::Password(PasswordMsg::ArchiveRekeyed(
        slot,
        client,
        Some(new),
        "synthetic-new-salt".into(),
        Err(error),
    ))));
    assert!(
        app.session.archive.is_none(),
        "failed verification cannot attach an archive"
    );
    td(
        &mut app,
        json!({"@type": "updateMessageContent", "chat_id": 1, "message_id": 10,
               "new_content": {"@type": "messageText", "text": {
                   "@type": "formattedText", "text": "later edit", "entities": []}}}),
    );
    assert!(
        app.session.archive.is_none(),
        "edits must not write plaintext through an unverified handle"
    );
    assert!(app.settings.accounts[0].pending_key_change.is_some());
    assert!(matches!(&app.session.password_form.message, Some(Err(_))));
}

#[test]
fn security_fix_forget_during_key_transition_keeps_archived_copy_visible_and_durable() {
    let mut app = app();
    open(&mut app, 1, &[(10, "archived"), (11, "survives")]);
    delete(&mut app, 1, &[10], false);
    assert_eq!(
        shown(&app),
        [(10, "archived", true), (11, "survives", false)]
    );
    app.session.password_form.busy = true;
    drop(app.update(Msg::Pane(app.main_window, PaneMsg::Forget(10))));
    assert_eq!(
        shown(&app),
        [(10, "archived", true), (11, "survives", false)],
        "a rejected purge cannot remove the item from the pane"
    );
    assert_eq!(
        app.session
            .archive
            .as_ref()
            .unwrap()
            .deleted(1, 0, 11)
            .unwrap()[0]
            .text,
        "archived"
    );
    app.session.password_form.busy = false;
    open(&mut app, 1, &[(11, "survives")]);
    assert_eq!(
        shown(&app),
        [(10, "archived", true), (11, "survives", false)]
    );
}

#[test]
fn security_fix_local_pin_changes_wait_for_verified_archive_then_apply_normally() {
    let mut app = app();
    open(&mut app, 1, &[(10, "existing pin"), (11, "later pin")]);
    let window = app.main_window;
    drop(app.update(Msg::Pane(window, PaneMsg::PinLocal(10, true))));
    assert_eq!(
        app.session.archive.as_ref().unwrap().local_pins(1).unwrap()[0].text,
        "existing pin"
    );
    app.session.password_form.busy = true;
    drop(app.update(Msg::Pane(window, PaneMsg::PinLocal(11, true))));
    drop(app.update(Msg::Pane(window, PaneMsg::PinLocal(10, false))));
    let stored = app.session.archive.as_ref().unwrap().local_pins(1).unwrap();
    assert_eq!(
        stored
            .iter()
            .map(|m| (m.id, m.text.as_str()))
            .collect::<Vec<_>>(),
        [(10, "existing pin")],
        "a transition must not modify archived pin text with an unverified key"
    );
    assert_eq!(
        pane(&app)
            .local_pins
            .iter()
            .map(|m| m.id)
            .collect::<Vec<_>>(),
        [10],
        "rejected pin/unpin cannot change the visible pinned bar"
    );
    app.session.password_form.busy = false;
    drop(app.update(Msg::Pane(window, PaneMsg::PinLocal(11, true))));
    assert_eq!(
        app.session
            .archive
            .as_ref()
            .unwrap()
            .local_pins(1)
            .unwrap()
            .iter()
            .map(|m| m.id)
            .collect::<Vec<_>>(),
        [11, 10]
    );
    assert_eq!(
        pane(&app)
            .local_pins
            .iter()
            .map(|m| m.id)
            .collect::<Vec<_>>(),
        [11, 10]
    );
}

#[test]
fn security_fix_permanent_deletions_survive_crash_before_archive_worker_returns() {
    let mut app = app();
    let old = crate::lock::derive("synthetic-old-key", "synthetic-old-salt").unwrap();
    let new = crate::lock::derive("synthetic-new-key", "synthetic-new-salt").unwrap();
    app.session
        .archive
        .as_mut()
        .unwrap()
        .set_key(Some(old.clone()));
    open(&mut app, 1, &[(10, "own copy"), (11, "foreign copy")]);
    let mut worker_archive = app.session.archive.take().unwrap();
    app.settings.accounts.push(Account {
        slot: 0,
        user_id: Some(501),
        pending_key_change: Some(
            crate::settings::PendingKeyChange::new(
                Some("synthetic-old-salt".into()),
                Some(crate::lock::check_value(&old)),
                Some("synthetic-new-salt".into()),
                Some(crate::lock::check_value(&new)),
                Some(&old),
                Some(&new),
            )
            .unwrap(),
        ),
        ..Default::default()
    });
    app.settings.save(&app.settings_path).unwrap();
    app.session.password_form.busy = true;
    let (_worker_sender, rx) = tokio::sync::oneshot::channel();
    app.session.rekeying_archive = Some(rx);
    app.session.self_deleted.insert((1, 10));
    delete(&mut app, 1, &[10, 11], false);

    // This is a new App and a fresh Session: the in-flight worker's RAM
    // queue and its channel are deliberately not carried across restart.
    let persisted = Settings::load(&app.settings_path).unwrap();
    assert!(persisted.accounts[0].pending_key_change.is_some());
    let mut restarted = super::app();
    restarted.settings = persisted;
    restarted.settings_path = app.settings_path.clone();
    restarted.session.archive = None;
    restarted.session.db_key = Some(old.clone());
    restarted.session.recovery_keys = Some((Some(old.clone()), Some(new.clone())));
    restarted.session.password_form.busy = true;
    worker_archive
        .recover_rekey(Some(old.clone()), Some(new.clone()))
        .unwrap();
    let (tx, rx) = tokio::sync::oneshot::channel();
    tx.send(worker_archive).ok().unwrap();
    restarted.session.rekeying_archive = Some(rx);
    let slot = restarted.session.slot;
    let client = restarted.session.client_id;
    drop(restarted.update(Msg::Password(PasswordMsg::ArchiveRekeyed(
        slot,
        client,
        Some(new.clone()),
        "synthetic-new-salt".into(),
        Ok(()),
    ))));

    let archive = restarted.session.archive.as_mut().unwrap();
    assert!(archive.verify_key(Some(&new)).is_ok());
    // Marking a nonexistent id is harmless; a resurrected own-deleted row
    // would become visible here, even if it was saved as a live row.
    archive.mark_deleted(1, &[10]).unwrap();
    let deleted = archive.deleted(1, 0, 12).unwrap();
    assert_eq!(
        deleted
            .iter()
            .map(|m| (m.id, m.text.as_str()))
            .collect::<Vec<_>>(),
        [(11, "foreign copy")]
    );
    assert!(
        Settings::load(&app.settings_path).unwrap().accounts[0]
            .pending_key_change
            .is_none()
    );
    open(&mut restarted, 1, &[]);
    assert_eq!(shown(&restarted), [(11, "foreign copy", true)]);
}

#[test]
fn security_fix_own_deletion_remains_purged_after_late_history_and_edit_replay() {
    let mut app = app();
    let old = crate::lock::derive("synthetic-old-key", "synthetic-old-salt").unwrap();
    let new = crate::lock::derive("synthetic-new-key", "synthetic-new-salt").unwrap();
    app.session
        .archive
        .as_mut()
        .unwrap()
        .set_key(Some(old.clone()));
    open(&mut app, 1, &[(10, "own archived copy")]);
    let mut worker_archive = app.session.archive.take().unwrap();
    app.settings.accounts.push(Account {
        slot: 0,
        user_id: Some(501),
        pending_key_change: Some(
            crate::settings::PendingKeyChange::new(
                Some("synthetic-old-salt".into()),
                Some(crate::lock::check_value(&old)),
                Some("synthetic-new-salt".into()),
                Some(crate::lock::check_value(&new)),
                Some(&old),
                Some(&new),
            )
            .unwrap(),
        ),
        ..Default::default()
    });
    app.settings.save(&app.settings_path).unwrap();
    app.session.db_key = Some(old.clone());
    app.session.recovery_keys = Some((Some(old.clone()), Some(new.clone())));
    app.session.password_form.busy = true;
    app.session.self_deleted.insert((1, 10));
    delete(&mut app, 1, &[10], false);
    open(&mut app, 1, &[(10, "late history copy")]);
    td(
        &mut app,
        json!({"@type": "updateMessageContent", "chat_id": 1, "message_id": 10,
               "new_content": {"@type": "messageText", "text": {
                   "@type": "formattedText", "text": "late edit", "entities": []}}}),
    );
    worker_archive
        .recover_rekey(Some(old), Some(new.clone()))
        .unwrap();
    let (tx, rx) = tokio::sync::oneshot::channel();
    tx.send(worker_archive).ok().unwrap();
    app.session.rekeying_archive = Some(rx);
    let slot = app.session.slot;
    let client = app.session.client_id;
    drop(app.update(Msg::Password(PasswordMsg::ArchiveRekeyed(
        slot,
        client,
        Some(new.clone()),
        "synthetic-new-salt".into(),
        Ok(()),
    ))));

    let archive = app.session.archive.as_mut().unwrap();
    assert!(archive.verify_key(Some(&new)).is_ok());
    archive.mark_deleted(1, &[10]).unwrap();
    assert!(archive.deleted(1, 0, 11).unwrap().is_empty());
    open(&mut app, 1, &[]);
    assert!(
        shown(&app).is_empty(),
        "a late stale update must not resurrect a purged message"
    );
}

#[test]
fn security_fix_applied_without_archive_keeps_persisted_deletions_until_verified_replay() {
    let mut app = app();
    let new = crate::lock::derive("synthetic-new-key", "synthetic-new-salt").unwrap();
    let mut pending = crate::settings::PendingKeyChange::new(
        None,
        None,
        Some("synthetic-new-salt".into()),
        Some(crate::lock::check_value(&new)),
        None,
        Some(&new),
    )
    .unwrap();
    pending
        .deleted_ids
        .insert(1, std::collections::BTreeSet::from([10]));
    pending
        .purged_ids
        .insert(1, std::collections::BTreeSet::from([11]));
    app.settings.accounts.push(Account {
        slot: 0,
        user_id: None,
        pending_key_change: Some(pending.clone()),
        ..Default::default()
    });
    app.settings.save(&app.settings_path).unwrap();
    app.session.archive = None;
    assert!(app.session.my_id.is_none());
    assert!(app.session.deferred_archive.is_empty());
    app.session.recovery_keys = Some((None, Some(new.clone())));
    app.session.password_form.busy = true;
    let slot = app.session.slot;
    let client = app.session.client_id;
    drop(app.update(Msg::Password(PasswordMsg::Applied(
        slot,
        client,
        Ok((Some(new.clone()), "synthetic-new-salt".into())),
    ))));

    assert_eq!(
        app.settings.accounts[0].pending_key_change,
        Some(pending.clone())
    );
    assert_eq!(
        Settings::load(&app.settings_path).unwrap().accounts[0].pending_key_change,
        Some(pending)
    );
    assert!(app.session.recovery_keys.is_some());
    assert!(!matches!(&app.session.password_form.message, Some(Ok(_))));

    // Once a real archive becomes available, verified target-key replay
    // completes the same durable operation instead of losing its deletions.
    let first: Message = serde_json::from_value(message(1, 10, "foreign")).unwrap();
    let second: Message = serde_json::from_value(message(1, 11, "own")).unwrap();
    let mut archive = Archive::in_memory();
    archive
        .save([&MsgItem::from(&first), &MsgItem::from(&second)])
        .unwrap();
    archive.recover_rekey(None, Some(new.clone())).unwrap();
    app.session.password_form.busy = true;
    let (tx, rx) = tokio::sync::oneshot::channel();
    tx.send(archive).ok().unwrap();
    app.session.rekeying_archive = Some(rx);
    drop(app.update(Msg::Password(PasswordMsg::ArchiveRekeyed(
        slot,
        client,
        Some(new.clone()),
        "synthetic-new-salt".into(),
        Ok(()),
    ))));
    let archive = app.session.archive.as_mut().unwrap();
    assert!(archive.verify_key(Some(&new)).is_ok());
    archive.mark_deleted(1, &[11]).unwrap();
    let deleted = archive.deleted(1, 0, 12).unwrap();
    assert_eq!(
        deleted
            .iter()
            .map(|m| (m.id, m.text.as_str()))
            .collect::<Vec<_>>(),
        [(10, "foreign")]
    );
    assert!(
        Settings::load(&app.settings_path).unwrap().accounts[0]
            .pending_key_change
            .is_none()
    );
}

#[test]
fn security_fix_committed_deferred_purge_with_vacuum_denied_finishes_recovery() {
    let mut app = app();
    let old = crate::lock::derive("synthetic-old-key", "synthetic-old-salt").unwrap();
    let new = crate::lock::derive("synthetic-new-key", "synthetic-new-salt").unwrap();
    app.session
        .archive
        .as_mut()
        .unwrap()
        .set_key(Some(old.clone()));
    open(&mut app, 1, &[(10, "own removed text")]);
    let mut worker_archive = app.session.archive.take().unwrap();
    app.settings.accounts.push(Account {
        slot: 0,
        user_id: Some(501),
        pending_key_change: Some(
            crate::settings::PendingKeyChange::new(
                Some("synthetic-old-salt".into()),
                Some(crate::lock::check_value(&old)),
                Some("synthetic-new-salt".into()),
                Some(crate::lock::check_value(&new)),
                Some(&old),
                Some(&new),
            )
            .unwrap(),
        ),
        ..Default::default()
    });
    app.settings.save(&app.settings_path).unwrap();
    app.session.db_key = Some(old.clone());
    app.session.recovery_keys = Some((Some(old.clone()), Some(new.clone())));
    app.session.password_form.busy = true;
    app.session.self_deleted.insert((1, 10));
    delete(&mut app, 1, &[10], false);
    worker_archive
        .recover_rekey(Some(old), Some(new.clone()))
        .unwrap();
    let blocker = worker_archive.block_vacuum_for_test();
    let (tx, rx) = tokio::sync::oneshot::channel();
    tx.send(worker_archive).ok().unwrap();
    app.session.rekeying_archive = Some(rx);
    let slot = app.session.slot;
    let client = app.session.client_id;
    drop(app.update(Msg::Password(PasswordMsg::ArchiveRekeyed(
        slot,
        client,
        Some(new.clone()),
        "synthetic-new-salt".into(),
        Ok(()),
    ))));
    drop(blocker);

    assert!(matches!(&app.session.password_form.message, Some(Ok(_))));
    assert!(
        Settings::load(&app.settings_path).unwrap().accounts[0]
            .pending_key_change
            .is_none()
    );
    assert!(
        app.error
            .as_deref()
            .is_some_and(|warning| warning.contains("очистка архива"))
    );
    let archive = app.session.archive.as_mut().unwrap();
    assert!(archive.verify_key(Some(&new)).is_ok());
    archive.mark_deleted(1, &[10]).unwrap();
    assert!(archive.deleted(1, 0, 11).unwrap().is_empty());
    open(&mut app, 1, &[]);
    assert!(shown(&app).is_empty());
}
