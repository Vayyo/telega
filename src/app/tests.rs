//! Behavior tests: real TDLib-shaped JSON updates go through `App::update`,
//! with an in-memory archive and no network. Tasks returned by `update` are
//! dropped unpolled, so no TDLib request is ever sent.

use super::media::Media;
use super::pane::ChatPane;
use super::sandbox;
use super::tabs::MAX_TABS;
use super::*;
use crate::av::audio::Recorder;
use crate::settings::Account;
use serde_json::{Value, json};

#[test]
fn api_credentials_reject_missing_and_invalid_id_without_exposing_hash() {
    for id in [
        None,
        Some(""),
        Some("0"),
        Some("-1"),
        Some("abc"),
        Some("2147483648"),
    ] {
        let error = parse_api_credentials(id, Some("private-hash")).unwrap_err();
        assert!(error.contains("TG_API_ID") && error.contains(".env.example"));
        assert!(!error.contains("private-hash"));
    }
}

#[test]
fn api_credentials_reject_missing_or_blank_hash() {
    for hash in [None, Some(""), Some(" \t ")] {
        let error = parse_api_credentials(Some("12345"), hash).unwrap_err();
        assert!(error.contains("TG_API_HASH") && error.contains(".env.example"));
    }
}

#[test]
fn api_credentials_preserve_valid_keys() {
    assert_eq!(
        parse_api_credentials(Some("2147483647"), Some(" hash-value ")),
        Ok((i32::MAX, " hash-value ".into()))
    );
}

#[test]
fn runtime_api_values_override_embedded_keys_even_when_invalid() {
    use std::ffi::OsStr;

    assert_eq!(configured_value(None, Some("12345")), Some("12345"));
    for runtime in ["", "0", "not-a-number"] {
        let id = configured_value(Some(OsStr::new(runtime)), Some("12345"));
        assert!(parse_api_credentials(id, Some("embedded-hash")).is_err());
    }
    let hash = configured_value(Some(OsStr::new("")), Some("embedded-hash"));
    assert!(parse_api_credentials(Some("12345"), hash).is_err());
    let id = configured_value(Some(OsStr::new("67890")), Some("12345"));
    let hash = configured_value(Some(OsStr::new("override")), Some("embedded-hash"));
    assert_eq!(
        parse_api_credentials(id, hash),
        Ok((67890, "override".into()))
    );
}

pub(super) fn app() -> App {
    static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let settings_path =
        std::env::temp_dir().join(format!("telega-test-{}-{n}.json", std::process::id()));
    let mut app = App::new(
        0,
        0,
        String::new(),
        Settings::default(),
        settings_path,
        WinId::unique(),
    );
    app.session.auth = Auth::Ready;
    app.session.archive = Some(Archive::in_memory());
    app
}

/// Cleans up the settings file `app()` created, even if a panicking
/// assertion above would have skipped an explicit `remove_file` at the end
/// of a test. Test-only: this module only compiles under `cfg(test)`.
impl Drop for App {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.settings_path);
    }
}

pub(super) fn td(app: &mut App, update: Value) {
    let _ = app.update(Msg::Td(
        0,
        Box::new(serde_json::from_value(update).unwrap()),
    ));
}

pub(super) fn message(chat_id: i64, id: i64, text: &str) -> Value {
    json!({
        "@type": "message", "id": id, "chat_id": chat_id,
        "sender_id": {"@type": "messageSenderUser", "user_id": 7},
        "is_outgoing": false, "is_pinned": false, "is_from_offline": false,
        "can_be_saved": true, "has_timestamped_media": false, "is_channel_post": false,
        "is_paid_star_suggested_post": false, "is_paid_gram_suggested_post": false,
        "contains_unread_poll_votes": false, "sender_tag": "", "ephemeral_message_id": 0,
        "chat_instance": "0",
        "contains_unread_mention": false, "date": 0, "edit_date": 0,
        "unread_reactions": [], "self_destruct_in": 0.0, "auto_delete_in": 0.0,
        "via_bot_user_id": 0, "sender_business_bot_user_id": 0, "sender_boost_count": 0,
        "paid_message_star_count": 0, "author_signature": "", "media_album_id": "0",
        "effect_id": "0", "summary_language_code": "",
        "content": {"@type": "messageText", "text": {"@type": "formattedText", "text": text, "entities": []}}
    })
}

/// A `ChatItem` with a title and blank/zeroed everything else, the way a
/// chat looks before any position or message update touched it. Override
/// individual fields with struct update syntax, e.g.
/// `ChatItem { unread: 2, ..chat_item("x", false) }`.
pub(super) fn chat_item(title: &str, private: bool) -> ChatItem {
    ChatItem {
        title: title.into(),
        title_lower: title.to_lowercase(),
        last: String::new(),
        preview: String::new(),
        unread: 0,
        order: 0,
        last_id: 0,
        last_outgoing: false,
        last_pending: false,
        read_outbox: 0,
        read_inbox: 0,
        private,
        archive_order: 0,
        pinned: false,
        notify: Default::default(),
        folders: Vec::new(),
        kind: None,
        draft: None,
    }
}

fn new_message(app: &mut App, chat_id: i64, id: i64, text: &str) {
    td(
        app,
        json!({"@type": "updateNewMessage", "message": message(chat_id, id, text)}),
    );
}

fn delete(app: &mut App, chat_id: i64, ids: &[i64], from_cache: bool) {
    td(
        app,
        json!({"@type": "updateDeleteMessages", "chat_id": chat_id, "message_ids": ids,
               "is_permanent": true, "from_cache": from_cache}),
    );
}

/// Opens the chat the way the UI does, with `history` as TDLib's answer.
fn open(app: &mut App, chat_id: i64, history: &[(i64, &str)]) {
    app.session
        .panes
        .get_mut(&app.main_window)
        .unwrap()
        .switch_to(chat_id);
    let messages = history
        .iter()
        .map(|(id, text)| serde_json::from_value(message(chat_id, *id, text)).unwrap())
        .collect();
    let _ = app.update(Msg::Pane(
        app.main_window,
        PaneMsg::HistoryLoaded(chat_id, Ok(messages)),
    ));
}

fn pane(app: &App) -> &ChatPane {
    &app.session.panes[&app.main_window]
}

fn shown(app: &App) -> Vec<(i64, &str, bool)> {
    pane(app)
        .messages
        .iter()
        .map(|m| (m.id, m.text.as_str(), m.deleted))
        .collect()
}

fn page(chat_id: i64, ids: impl IntoIterator<Item = i64>) -> Vec<Message> {
    ids.into_iter()
        .map(|id| serde_json::from_value(message(chat_id, id, "m")).unwrap())
        .collect()
}

#[test]
fn older_pages_merge_deleted_messages_of_their_range_once() {
    let mut app = app();
    for id in [60, 120] {
        new_message(&mut app, 1, id, "gone");
    }
    delete(&mut app, 1, &[60, 120], false);

    // Full first page of 50 live messages, 100..=150 without the deleted 120.
    // Only the deleted message inside its range shows up.
    app.session
        .panes
        .get_mut(&app.main_window)
        .unwrap()
        .switch_to(1);
    let first = page(1, (100..151).filter(|&id| id != 120));
    let _ = app.update(Msg::Pane(
        app.main_window,
        PaneMsg::HistoryLoaded(1, Ok(first)),
    ));
    let deleted: Vec<i64> = pane(&app)
        .messages
        .iter()
        .filter(|m| m.deleted)
        .map(|m| m.id)
        .collect();
    assert_eq!(deleted, [120]);

    // Short older page reaches the chat start; 60 slots in, 120 is not repeated.
    let older = page(1, (50..100).filter(|&id| id != 60));
    let _ = app.update(Msg::Pane(
        app.main_window,
        PaneMsg::OlderLoaded(1, 100, Ok(older)),
    ));
    let ids: Vec<i64> = pane(&app).messages.iter().map(|m| m.id).collect();
    assert_eq!(ids, (50..151).collect::<Vec<_>>());
    let deleted: Vec<i64> = pane(&app)
        .messages
        .iter()
        .filter(|m| m.deleted)
        .map(|m| m.id)
        .collect();
    assert_eq!(deleted, [60, 120]);
    assert!(pane(&app).history_complete);

    // Nothing older remains, so scrolling to the top requests nothing.
    let _ = app.load_older(app.main_window);
    assert!(!pane(&app).loading_older);
}

#[test]
fn stale_older_page_is_ignored() {
    let mut app = app();
    app.session
        .panes
        .get_mut(&app.main_window)
        .unwrap()
        .switch_to(1);
    let _ = app.update(Msg::Pane(
        app.main_window,
        PaneMsg::HistoryLoaded(1, Ok(page(1, 100..150))),
    ));
    // Answer for another position (e.g. before the chat was reopened).
    let _ = app.update(Msg::Pane(
        app.main_window,
        PaneMsg::OlderLoaded(1, 999, Ok(page(1, 10..20))),
    ));
    assert_eq!(pane(&app).messages.first().map(|m| m.id), Some(100));
}

#[test]
fn foreign_deletion_marks_message_and_survives_reopen() {
    let mut app = app();
    open(&mut app, 1, &[(10, "a"), (11, "b")]);
    delete(&mut app, 1, &[10], false);
    assert_eq!(shown(&app), [(10, "a", true), (11, "b", false)]);

    // TDLib no longer returns the deleted message; the archive puts it back in place.
    open(&mut app, 1, &[(11, "b")]);
    assert_eq!(shown(&app), [(10, "a", true), (11, "b", false)]);
}

#[test]
fn cache_eviction_is_not_a_deletion() {
    let mut app = app();
    open(&mut app, 1, &[(10, "a")]);
    delete(&mut app, 1, &[10], true);
    assert_eq!(shown(&app), [(10, "a", false)]);
}

#[test]
fn message_of_closed_chat_is_archived_before_deletion() {
    let mut app = app();
    new_message(&mut app, 2, 20, "secret");
    delete(&mut app, 2, &[20], false);
    open(&mut app, 2, &[]);
    assert_eq!(shown(&app), [(20, "secret", true)]);
}

#[test]
fn deleted_message_shows_last_edited_text() {
    let mut app = app();
    new_message(&mut app, 1, 10, "draft");
    td(
        &mut app,
        json!({"@type": "updateMessageContent", "chat_id": 1, "message_id": 10,
               "new_content": {"@type": "messageText", "text": {"@type": "formattedText", "text": "final", "entities": []}}}),
    );
    delete(&mut app, 1, &[10], false);
    open(&mut app, 1, &[]);
    assert_eq!(shown(&app), [(10, "final", true)]);
}

#[test]
fn own_deletion_removes_message_everywhere() {
    let mut app = app();
    open(&mut app, 1, &[(10, "a"), (11, "b")]);
    let _ = app.update(Msg::Pane(
        app.main_window,
        PaneMsg::Delete {
            id: 10,
            revoke: true,
        },
    ));
    delete(&mut app, 1, &[10], false);
    assert_eq!(shown(&app), [(11, "b", false)]);

    open(&mut app, 1, &[(11, "b")]);
    assert_eq!(shown(&app), [(11, "b", false)]);
}

#[test]
fn failed_delete_rolls_back_self_deleted() {
    let mut app = app();
    open(&mut app, 1, &[(10, "a")]);
    let _ = app.update(Msg::Pane(
        app.main_window,
        PaneMsg::Delete {
            id: 10,
            revoke: true,
        },
    ));
    assert!(app.session.self_deleted.contains(&(1, 10)));

    let _ = app.update(Msg::DeleteDone(1, vec![10], Err("нет прав".into())));
    assert!(
        !app.session.self_deleted.contains(&(1, 10)),
        "a failed delete must not keep counting as the user's own"
    );
    assert_eq!(app.error.as_deref(), Some("нет прав"));

    // Deleted by someone else afterwards: now correctly foreign, so it is
    // kept archived (marked deleted) instead of purged as if it were the
    // rolled-back UI deletion.
    delete(&mut app, 1, &[10], false);
    assert_eq!(shown(&app), [(10, "a", true)]);
}

#[test]
fn forgetting_archived_copy_removes_it() {
    let mut app = app();
    open(&mut app, 1, &[(10, "a"), (11, "b")]);
    delete(&mut app, 1, &[10], false);
    let _ = app.update(Msg::Pane(app.main_window, PaneMsg::OpenMenu(10)));
    let _ = app.update(Msg::Pane(app.main_window, PaneMsg::Forget(10)));
    assert!(pane(&app).menu.is_none());

    open(&mut app, 1, &[(11, "b")]);
    assert_eq!(shown(&app), [(11, "b", false)]);
}

#[test]
fn chat_list_follows_main_list_positions() {
    let mut app = app();
    for id in [1, 2, 3] {
        app.session
            .chats
            .insert(id, chat_item(&id.to_string(), false));
    }
    let position = |app: &mut App, chat_id: i64, list: &str, order: i64| {
        td(
            app,
            json!({"@type": "updateChatPosition", "chat_id": chat_id,
                   "position": {"@type": "chatPosition", "list": {"@type": list},
                                "order": order.to_string(), "is_pinned": false}}),
        );
    };
    position(&mut app, 1, "chatListMain", 10);
    position(&mut app, 2, "chatListMain", 30);
    position(&mut app, 3, "chatListMain", 20);
    position(&mut app, 3, "chatListArchive", 99);
    position(&mut app, 1, "chatListMain", 40);
    position(&mut app, 2, "chatListMain", 0);

    let ids: Vec<i64> = app.session.order.iter().map(|&(_, id)| id).collect();
    assert_eq!(ids, [1, 3]);
}

#[test]
fn updates_of_replaced_client_are_ignored() {
    let mut app = app();
    open(&mut app, 1, &[]);
    let stale = serde_json::from_value(
        json!({"@type": "updateNewMessage", "message": message(1, 10, "old session")}),
    )
    .unwrap();
    let _ = app.update(Msg::Td(99, Box::new(stale)));
    assert!(pane(&app).messages.is_empty());
}

#[test]
fn turning_keep_deleted_off_hides_archive_and_on_restores_it() {
    let mut app = app();
    open(&mut app, 1, &[(10, "a"), (11, "b")]);
    delete(&mut app, 1, &[10], false);

    let _ = app.update(Msg::SetKeepDeleted(false));
    assert_eq!(shown(&app), [(11, "b", false)]);
    // Reopening while off does not bring it back.
    open(&mut app, 1, &[(11, "b")]);
    assert_eq!(shown(&app), [(11, "b", false)]);
    // The choice is persisted.
    assert!(!Settings::load(&app.settings_path).unwrap().keep_deleted);

    // Back on: the archived copy was kept and shows up again after reload.
    let _ = app.update(Msg::SetKeepDeleted(true));
    open(&mut app, 1, &[(11, "b")]);
    assert_eq!(shown(&app), [(10, "a", true), (11, "b", false)]);
}

#[test]
fn while_off_deletions_behave_like_a_normal_client() {
    let mut app = app();
    let _ = app.update(Msg::SetKeepDeleted(false));
    new_message(&mut app, 2, 20, "unsaved");
    open(&mut app, 1, &[(10, "a"), (11, "b")]);
    delete(&mut app, 1, &[10], false);
    delete(&mut app, 2, &[20], false);
    assert_eq!(shown(&app), [(11, "b", false)]);

    // Nothing was archived while off, so turning it on reveals nothing.
    let _ = app.update(Msg::SetKeepDeleted(true));
    open(&mut app, 1, &[(11, "b")]);
    assert_eq!(shown(&app), [(11, "b", false)]);
    open(&mut app, 2, &[]);
    assert!(pane(&app).messages.is_empty());
}

/// Opens a chat window the way the "В новом окне" button does and answers its
/// history request with `history`.
fn open_window(app: &mut App, chat_id: i64, history: &[(i64, &str)]) -> WinId {
    let _ = app.update(Msg::OpenChatWindow(Some(chat_id)));
    let window = *app
        .session
        .panes
        .keys()
        .find(|&&w| {
            w != app.main_window
                && app.session.panes[&w].messages.is_empty()
                && app.session.panes[&w].shows(chat_id)
        })
        .expect("chat window pane");
    let messages = history
        .iter()
        .map(|(id, text)| serde_json::from_value(message(chat_id, *id, text)).unwrap())
        .collect();
    let _ = app.update(Msg::Pane(
        window,
        PaneMsg::HistoryLoaded(chat_id, Ok(messages)),
    ));
    window
}

fn shown_in(app: &App, window: WinId) -> Vec<(i64, &str, bool)> {
    app.session.panes[&window]
        .messages
        .iter()
        .map(|m| (m.id, m.text.as_str(), m.deleted))
        .collect()
}

#[test]
fn updates_reach_every_window_showing_the_chat() {
    let mut app = app();
    // The main window may show a chat that also has its own window.
    let same = open_window(&mut app, 1, &[(10, "a")]);
    let other = open_window(&mut app, 2, &[(20, "x")]);
    open(&mut app, 1, &[(10, "a")]);

    new_message(&mut app, 1, 11, "b");
    delete(&mut app, 1, &[10], false);

    let expected = [(10, "a", true), (11, "b", false)];
    assert_eq!(shown(&app), expected);
    assert_eq!(shown_in(&app, same), expected);
    assert_eq!(shown_in(&app, other), [(20, "x", false)]);
}

#[test]
fn windows_keep_their_own_drafts_and_menus() {
    let mut app = app();
    let second = open_window(&mut app, 1, &[(10, "a")]);
    open(&mut app, 1, &[(10, "a")]);

    let _ = app.update(Msg::Pane(second, typed("draft")));
    let _ = app.update(Msg::Pane(second, PaneMsg::OpenMenu(10)));
    assert_eq!(app.session.panes[&second].compose.text(), "draft");
    assert!(app.session.panes[&second].menu.is_some());
    assert_eq!(pane(&app).compose.text(), "");
    assert!(pane(&app).menu.is_none());
}

#[test]
fn answers_for_a_closed_window_are_dropped() {
    let mut app = app();
    let _ = app.update(Msg::OpenChatWindow(Some(1)));
    let closed = *app
        .session
        .panes
        .keys()
        .find(|&&w| w != app.main_window)
        .unwrap();
    let _ = app.update(Msg::WindowClosed(closed));

    // Another window on the same chat stays open, so a late answer meant
    // for the closed one has somewhere to wrongly leak into.
    let kept = open_window(&mut app, 1, &[]);
    let late = page(1, [10]);
    let _ = app.update(Msg::Pane(closed, PaneMsg::HistoryLoaded(1, Ok(late))));
    new_message(&mut app, 1, 11, "b");
    assert_eq!(
        app.session.panes.len(),
        2,
        "closed window must not come back"
    );
    assert_eq!(
        shown_in(&app, kept),
        [(11, "b", false)],
        "a history answer meant for the closed window must not leak into another one"
    );
}

#[test]
fn turning_keep_deleted_off_hides_archive_in_every_window() {
    let mut app = app();
    let second = open_window(&mut app, 1, &[(10, "a"), (11, "b")]);
    open(&mut app, 1, &[(10, "a"), (11, "b")]);
    delete(&mut app, 1, &[10], false);

    let _ = app.update(Msg::SetKeepDeleted(false));
    assert_eq!(shown(&app), [(11, "b", false)]);
    assert_eq!(shown_in(&app, second), [(11, "b", false)]);
}

/// App connected to a fake plugin thread; returns what the app sends it.
fn with_plugin_host(app: &mut App) -> std::sync::mpsc::Receiver<HostCmd> {
    let (handle, rx) = HostHandle::for_test();
    let _ = app.update(Msg::Plugin(HostEvent::Ready(handle)));
    rx
}

fn sent(rx: &std::sync::mpsc::Receiver<HostCmd>) -> Vec<HostCmd> {
    rx.try_iter().collect()
}

#[test]
fn plugin_deletions_are_not_kept_in_the_deleted_archive() {
    let mut app = app();
    open(&mut app, 1, &[(10, "a"), (11, "b")]);
    // TDLib confirmed 10 as the user's own; 11 was requested but is not.
    let action = plugins::Action::Delete {
        chat_id: 1,
        ids: vec![10, 11],
        revoke: true,
    };
    let _ = app.update(Msg::PluginDeleteOwn(
        "autodelete".into(),
        1,
        vec![10],
        true,
        action,
    ));
    delete(&mut app, 1, &[10, 11], false);
    assert_eq!(
        shown(&app),
        [(11, "b", true)],
        "someone else's message stays archived"
    );
}

#[test]
fn failed_plugin_deletion_rolls_back_and_requeues_the_action() {
    let mut app = app();
    let rx = with_plugin_host(&mut app);
    sent(&rx);
    open(&mut app, 1, &[(10, "a")]);
    let action = plugins::Action::Delete {
        chat_id: 1,
        ids: vec![10],
        revoke: true,
    };
    let _ = app.update(Msg::PluginDeleteOwn(
        "autodelete".into(),
        1,
        vec![10],
        true,
        action.clone(),
    ));
    // Telegram refused the deletion (flood wait): the id must stop counting
    // as a deliberate plugin deletion, and the action must not be lost.
    let _ = app.update(Msg::PluginDeleted(
        "autodelete".into(),
        1,
        vec![10],
        action.clone(),
        Err("Too Many Requests: retry after 30 (429)".into()),
    ));
    delete(&mut app, 1, &[10], false);
    assert_eq!(
        shown(&app),
        [(10, "a", true)],
        "the rolled-back id must end up in the deleted archive, not silently ignored"
    );
    let cmds = sent(&rx);
    assert!(
        matches!(cmds.first(), Some(HostCmd::FloodWait(30))),
        "{cmds:?}"
    );
    assert!(
        matches!(cmds.get(1), Some(HostCmd::Requeue(id, a)) if id == "autodelete" && *a == action),
        "{cmds:?}"
    );
}

#[test]
fn plugins_get_the_account_only_when_logged_in() {
    let mut app = app();
    app.session.auth = Auth::Phone;
    let rx = with_plugin_host(&mut app);
    td(
        &mut app,
        json!({"@type": "updateOption", "name": "my_id",
                        "value": {"@type": "optionValueInteger", "value": "42"}}),
    );
    assert!(
        !sent(&rx)
            .iter()
            .any(|c| matches!(c, HostCmd::Account(Some(_)))),
        "not before authorization"
    );
    td(
        &mut app,
        json!({"@type": "updateAuthorizationState",
                        "authorization_state": {"@type": "authorizationStateReady"}}),
    );
    assert!(
        sent(&rx)
            .iter()
            .any(|c| matches!(c, HostCmd::Account(Some(42))))
    );

    // Log out: plugins lose the account, the thread connection survives.
    td(
        &mut app,
        json!({"@type": "updateAuthorizationState",
                        "authorization_state": {"@type": "authorizationStateClosed"}}),
    );
    assert!(
        sent(&rx)
            .iter()
            .any(|c| matches!(c, HostCmd::Account(None)))
    );
    assert!(app.plugin_host.is_some());
}

#[test]
fn message_events_reach_plugins() {
    let mut app = app();
    let rx = with_plugin_host(&mut app);
    sent(&rx);
    new_message(&mut app, 1, 10, "hi");
    td(
        &mut app,
        json!({"@type": "updateMessageContent", "chat_id": 1, "message_id": 10,
               "new_content": {"@type": "messageText", "text": {"@type": "formattedText", "text": "edited", "entities": []}}}),
    );
    delete(&mut app, 1, &[10], false);
    delete(&mut app, 1, &[11], true); // cache eviction is not an event

    let events: Vec<plugins::Event> = sent(&rx)
        .into_iter()
        .filter_map(|c| match c {
            HostCmd::Event(e) => Some(e),
            _ => None,
        })
        .collect();
    assert_eq!(
        events,
        [
            plugins::Event::New {
                chat_id: 1,
                id: 10,
                text: "hi".into(),
                outgoing: false,
                sender_id: 7
            },
            plugins::Event::Edited {
                chat_id: 1,
                id: 10,
                text: "edited".into()
            },
            plugins::Event::Deleted {
                chat_id: 1,
                ids: vec![10]
            },
        ]
    );
}

#[test]
fn plugin_settings_are_saved_and_pushed_to_plugins() {
    let mut app = app();
    let rx = with_plugin_host(&mut app);
    sent(&rx);
    let _ = app.update(Msg::PluginToggle("autodelete".into(), true));
    let _ = app.update(Msg::PluginSetting(
        "autodelete".into(),
        "minutes".into(),
        json!(10),
    ));
    let saved = Settings::load(&app.settings_path).unwrap();
    let config = &saved.plugins["autodelete"];
    assert!(
        config.enabled && config.dry_run,
        "enabled plugins start in dry run"
    );
    assert_eq!(config.values["minutes"], json!(10));
    assert!(
        matches!(sent(&rx).last(), Some(HostCmd::Configure(c)) if c["autodelete"].values["minutes"] == json!(10))
    );

    let _ = app.update(Msg::StopAllPlugins);
    assert!(!Settings::load(&app.settings_path).unwrap().plugins["autodelete"].enabled);
}

#[test]
fn expanded_plugin_permissions_revoke_actions_until_reenabled() {
    let mut app = app();
    let rx = with_plugin_host(&mut app);
    sent(&rx);
    let mut info = PluginInfo {
        id: "sender".into(),
        name: "Sender".into(),
        description: String::new(),
        permissions: vec![plugins::Permission::Send],
        settings: Vec::new(),
        builtin: false,
        error: None,
    };
    let _ = app.update(Msg::Plugin(HostEvent::Plugins(vec![info.clone()])));
    let _ = app.update(Msg::PluginToggle(info.id.clone(), true));
    let action = Action::Send {
        chat_id: 1,
        text: "hi".into(),
    };
    assert_eq!(
        app.update(Msg::Plugin(HostEvent::Action(
            info.id.clone(),
            action.clone()
        )))
        .units(),
        1
    );

    info.permissions.push(plugins::Permission::Read);
    let _ = app.update(Msg::Plugin(HostEvent::Plugins(vec![info.clone()])));
    assert!(!app.settings.plugins[&info.id].enabled);
    assert!(matches!(
        sent(&rx).last(),
        Some(HostCmd::Configure(config)) if !config[&info.id].enabled
    ));
    assert_eq!(
        app.update(Msg::Plugin(HostEvent::Action(
            info.id.clone(),
            action.clone()
        )))
        .units(),
        0,
        "a stale action cannot bypass the revoked permission"
    );

    let _ = app.update(Msg::PluginToggle(info.id.clone(), true));
    assert_eq!(
        app.update(Msg::Plugin(HostEvent::Action(info.id, action)))
            .units(),
        1
    );
}

/// Opens a chat from the main window's chat list; answers the history
/// request when a fresh (not cached) tab asks for it.
fn select(app: &mut App, chat_id: i64, history: &[(i64, &str)]) {
    let _ = app.update(Msg::Pane(app.main_window, PaneMsg::SelectChat(chat_id)));
    if pane(app).messages.is_empty() && pane(app).shows(chat_id) {
        let messages = history
            .iter()
            .map(|(id, text)| serde_json::from_value(message(chat_id, *id, text)).unwrap())
            .collect();
        let _ = app.update(Msg::Pane(
            app.main_window,
            PaneMsg::HistoryLoaded(chat_id, Ok(messages)),
        ));
    }
}

fn active(app: &App) -> Option<i64> {
    pane(app).chat_id
}

fn ctrl(app: &mut App, key: Key, shift: bool) {
    let modifiers = if shift {
        Modifiers::CTRL | Modifiers::SHIFT
    } else {
        Modifiers::CTRL
    };
    let _ = app.update(Msg::Key(app.main_window, key, modifiers));
}

#[test]
fn background_tab_keeps_draft_and_history_and_gets_updates() {
    let mut app = app();
    select(&mut app, 1, &[(10, "a")]);
    let _ = app.update(Msg::Pane(app.main_window, typed("draft")));
    select(&mut app, 2, &[(20, "x")]);

    new_message(&mut app, 1, 11, "b");
    delete(&mut app, 1, &[10], false);

    // Back to tab 1: no reload needed, everything is already there.
    let _ = app.update(Msg::SelectTab(1));
    assert_eq!(active(&app), Some(1));
    assert_eq!(shown(&app), [(10, "a", true), (11, "b", false)]);
    assert_eq!(pane(&app).compose.text(), "draft");
    assert_eq!(app.session.tabs.chats, [1, 2]);
}

#[test]
fn eleventh_tab_evicts_least_recently_used_unpinned_one() {
    let mut app = app();
    for chat in 1..=10 {
        select(&mut app, chat, &[(chat * 10, "m")]);
    }
    let _ = app.update(Msg::TogglePin(1));
    let _ = app.update(Msg::SelectTab(2));
    select(&mut app, 11, &[(110, "m")]);

    assert_eq!(app.session.tabs.chats.len(), MAX_TABS);
    // 1 is pinned, 2 was used recently: 3 is the oldest unpinned tab.
    assert!(!app.session.tabs.chats.contains(&3));
    assert!(!app.session.background.contains_key(&3));
    assert!(app.session.tabs.chats.contains(&1) && app.session.tabs.chats.contains(&2));
    assert_eq!(app.session.tabs.chats[0], 1, "pinned tabs come first");
}

#[test]
fn chat_moved_to_window_leaves_the_tab_bar_until_the_window_closes() {
    let mut app = app();
    select(&mut app, 1, &[(10, "a")]);
    select(&mut app, 2, &[(20, "b")]);

    let _ = app.update(Msg::OpenChatWindow(Some(2)));
    let window = *app
        .session
        .panes
        .keys()
        .find(|&&w| w != app.main_window)
        .unwrap();
    assert_eq!(
        active(&app),
        Some(1),
        "main window moves to the neighbour tab"
    );
    assert_eq!(app.visible_tabs(), [1]);

    let _ = app.update(Msg::WindowClosed(window));
    assert_eq!(app.visible_tabs(), [1, 2]);

    // A chat opened only in a window becomes a tab when its window closes.
    let _ = app.update(Msg::OpenChatWindow(Some(5)));
    let window = *app
        .session
        .panes
        .keys()
        .find(|&&w| w != app.main_window)
        .unwrap();
    let _ = app.update(Msg::WindowClosed(window));
    assert!(app.session.tabs.chats.contains(&5));
}

#[test]
fn closing_the_active_tab_switches_to_a_neighbour() {
    let mut app = app();
    for chat in [1, 2, 3] {
        select(&mut app, chat, &[(chat * 10, "m")]);
    }
    let _ = app.update(Msg::SelectTab(2));
    let _ = app.update(Msg::CloseTab(2));
    assert_eq!(active(&app), Some(3));
    let _ = app.update(Msg::CloseTab(3));
    assert_eq!(active(&app), Some(1));
    let _ = app.update(Msg::CloseTab(1));
    assert_eq!(active(&app), None);
    assert!(app.session.tabs.chats.is_empty() && app.session.background.is_empty());
}

#[test]
fn backgrounded_tab_resets_stuck_flags_and_reloads_without_history() {
    let mut app = app();
    // Chat 1 opens but its answer never arrives before the tab is backgrounded.
    let _ = app.update(Msg::Pane(app.main_window, PaneMsg::SelectChat(1)));
    assert!(pane(&app).shows(1));
    {
        // Requests that were in flight when the tab went to the background:
        // their answers, addressed to this window, will target chat 2 once
        // it arrives and get dropped, so these flags would otherwise stick.
        let p = app.session.panes.get_mut(&app.main_window).unwrap();
        p.loading_older = true;
        p.loading_newer = true;
        p.jump_pending = Some(42);
    }
    select(&mut app, 2, &[(20, "b")]);

    // Back to chat 1: no history was ever cached for it, so it must reload
    // from scratch instead of staying "loading" forever.
    let _ = app.update(Msg::SelectTab(1));
    assert!(pane(&app).shows(1));
    assert!(!pane(&app).loading_older);
    assert!(!pane(&app).loading_newer);
    assert_eq!(pane(&app).jump_pending, None);
    assert!(
        pane(&app).messages.is_empty(),
        "the reload has not answered yet"
    );
    let _ = app.update(Msg::Pane(
        app.main_window,
        PaneMsg::HistoryLoaded(
            1,
            Ok(vec![serde_json::from_value(message(1, 10, "a")).unwrap()]),
        ),
    ));
    assert_eq!(shown(&app), [(10, "a", false)]);
}

#[test]
fn a_stale_jump_answer_does_not_override_a_newer_one() {
    let mut app = app();
    open(&mut app, 1, &[(200, "cur")]);
    pane_msg(&mut app, PaneMsg::JumpTo(10));
    assert_eq!(pane(&app).jump_pending, Some(10));
    // A second jump starts before the first one's answer arrives.
    pane_msg(&mut app, PaneMsg::JumpTo(100));
    assert_eq!(pane(&app).jump_pending, Some(100));

    // The first jump's answer arrives late: it must be dropped.
    pane_msg(&mut app, PaneMsg::AroundLoaded(1, 10, Ok(page(1, 5..15))));
    assert_eq!(pane(&app).jump_pending, Some(100), "stale answer ignored");
    assert_eq!(pane(&app).messages.first().map(|m| m.id), Some(200));

    // The current jump's answer lands normally.
    pane_msg(
        &mut app,
        PaneMsg::AroundLoaded(1, 100, Ok(page(1, 95..105))),
    );
    assert_eq!(pane(&app).jump_pending, None);
    assert_eq!(pane(&app).messages.first().map(|m| m.id), Some(95));
}

#[test]
fn tab_shortcuts_work_like_in_a_browser() {
    let mut app = app();
    for chat in [1, 2, 3] {
        select(&mut app, chat, &[(chat * 10, "m")]);
    }
    ctrl(&mut app, Key::Named(keyboard::key::Named::Tab), false);
    assert_eq!(active(&app), Some(1), "wraps around");
    ctrl(&mut app, Key::Named(keyboard::key::Named::Tab), true);
    assert_eq!(active(&app), Some(3));
    ctrl(&mut app, Key::Character("2".into()), false);
    assert_eq!(active(&app), Some(2));
    ctrl(&mut app, Key::Character("9".into()), false);
    assert_eq!(active(&app), Some(3), "Ctrl+9 is the last tab");
    ctrl(&mut app, Key::Character("w".into()), false);
    assert_eq!(app.session.tabs.chats, [1, 2]);
}

#[test]
fn tabs_are_restored_for_the_same_account() {
    let mut app = app();
    app.session.my_id = Some(42);
    select(&mut app, 1, &[(10, "a")]);
    select(&mut app, 2, &[(20, "b")]);
    let _ = app.update(Msg::TogglePin(2));
    // The save is debounced: simulate the scheduled flush firing, as it
    // would shortly after in a real run.
    let _ = app.update(Msg::FlushSettings(app.session.settings_save_token));
    let path = app.settings_path.clone();

    let settings = Settings::load(&path).unwrap();
    let mut restarted = App::new(0, 0, String::new(), settings, path.clone(), WinId::unique());
    restarted.session.auth = Auth::Ready;
    restarted.session.my_id = Some(42);
    let _ = restarted.restore_tabs();
    assert_eq!(restarted.session.tabs.chats, [2, 1]);
    assert_eq!(restarted.session.tabs.pinned, [2]);
    assert_eq!(active(&restarted), Some(2));
}

#[test]
fn switching_tabs_debounces_the_settings_save() {
    let mut app = app();
    app.session.my_id = Some(7);
    select(&mut app, 1, &[(10, "a")]);

    // Switching to another tab schedules a flush, but must not write to
    // disk on the spot: a burst of clicks must not hit the disk once per
    // click.
    let scheduled = app.session.settings_save_token;
    select(&mut app, 2, &[(20, "b")]);
    assert!(
        app.session.settings_save_token > scheduled,
        "a flush was scheduled"
    );
    assert!(
        Settings::load(&app.settings_path).unwrap().tabs.is_empty(),
        "not written yet: still debounced"
    );

    // A second switch before the first flush fires schedules a newer one:
    // the stale token from the first switch must not write anything.
    let stale = app.session.settings_save_token;
    select(&mut app, 3, &[(30, "c")]);
    assert!(app.session.settings_save_token > stale);
    let _ = app.update(Msg::FlushSettings(stale));
    assert!(
        Settings::load(&app.settings_path).unwrap().tabs.is_empty(),
        "a superseded flush must not write"
    );

    // The flush matching the latest change writes the up-to-date state
    // once.
    let _ = app.update(Msg::FlushSettings(app.session.settings_save_token));
    let saved = Settings::load(&app.settings_path).unwrap();
    assert_eq!(saved.tabs["7"].active, Some(3));
}

fn file(id: i32, size: i64, downloaded: i64, done: bool, path: &str) -> Value {
    json!({
        "@type": "file", "id": id, "size": size, "expected_size": size,
        "local": {"@type": "localFile", "path": path, "can_be_downloaded": true,
                  "can_be_deleted": false, "is_downloading_active": !done && downloaded > 0,
                  "is_downloading_completed": done, "download_offset": 0,
                  "downloaded_prefix_size": downloaded, "downloaded_size": downloaded},
        "remote": {"@type": "remoteFile", "id": "r", "unique_id": "u",
                   "is_uploading_active": false, "is_uploading_completed": true,
                   "uploaded_size": size}
    })
}

fn media_message(chat_id: i64, id: i64, content: Value) -> Value {
    let mut m = message(chat_id, id, "");
    m["content"] = content;
    m
}

fn photo_content() -> Value {
    let size = |t: &str, id: i32, w: i32| {
        json!({"@type": "photoSize", "type": t, "photo": file(id, 1000, 0, false, ""),
               "width": w, "height": w * 3 / 4, "progressive_sizes": []})
    };
    json!({
        "@type": "messagePhoto",
        "photo": {"@type": "photo", "has_stickers": false,
                  // 1x1 PNG, base64: stands in for the tiny inline JPEG.
                  "minithumbnail": {"@type": "minithumbnail", "width": 1, "height": 1,
                                    "data": "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg=="},
                  "sizes": [size("m", 101, 320), size("x", 102, 800), size("y", 103, 1280), size("w", 104, 2560)]},
        "caption": {"@type": "formattedText", "text": "море", "entities": []},
        "show_caption_above_media": false, "has_spoiler": false, "is_secret": false
    })
}

#[test]
fn photo_uses_largest_size_up_to_1280_and_loads_when_shown() {
    let mut app = app();
    open(&mut app, 1, &[]);
    new_message_value(&mut app, media_message(1, 10, photo_content()));

    let item = &pane(&app).messages[0];
    let Some(Media::Photo { file_id, mini, .. }) = &item.media else {
        panic!("no photo media");
    };
    assert_eq!(*file_id, 103, "1280 px size, not 2560");
    assert!(mini.is_some(), "inline preview decoded from base64");
    assert_eq!(item.text, "[Фото] море");

    // Seen on screen before the download finished: decoded right when it does.
    let _ = app.update(Msg::Pane(app.main_window, PaneMsg::MediaVisible(103)));
    assert!(app.session.images.decoding.is_empty());
    td(
        &mut app,
        json!({"@type": "updateFile", "file": file(103, 1000, 1000, true, "/tmp/p.jpg")}),
    );
    assert!(app.session.images.decoding.contains(&103));

    let _ = app.update(Msg::ImageDecoded(103, Ok((2, 1, vec![0; 8]))));
    assert!(app.session.images.peek(103).is_some());
    assert!(app.session.images.decoding.is_empty());
}

#[test]
fn photos_never_seen_are_not_decoded() {
    let mut app = app();
    open(&mut app, 1, &[]);
    new_message_value(&mut app, media_message(1, 10, photo_content()));
    td(
        &mut app,
        json!({"@type": "updateFile", "file": file(103, 1000, 1000, true, "/tmp/p.jpg")}),
    );
    assert!(app.session.images.decoding.is_empty());
}

#[test]
fn file_progress_is_tracked_and_executables_are_not_opened() {
    let mut app = app();
    open(&mut app, 1, &[]);
    let document = |name: &str| {
        json!({"@type": "messageDocument",
               "document": {"@type": "document", "file_name": name, "mime_type": "application/octet-stream",
                            "document": file(200, 4000, 0, false, "")},
               "caption": {"@type": "formattedText", "text": "", "entities": []}})
    };
    new_message_value(&mut app, media_message(1, 10, document("setup.exe")));
    assert!(
        matches!(&pane(&app).messages[0].media, Some(Media::Document { name, size: 4000, .. }) if name == "setup.exe")
    );

    td(
        &mut app,
        json!({"@type": "updateFile", "file": file(200, 4000, 1000, false, "")}),
    );
    assert_eq!(app.session.files[&200].percent(), 25);
    td(
        &mut app,
        json!({"@type": "updateFile", "file": file(200, 4000, 4000, true, "/tmp/setup.exe")}),
    );
    assert!(app.session.files[&200].done);

    let _ = app.update(Msg::FileOpen(200));
    assert!(
        app.error
            .as_deref()
            .is_some_and(|e| e.contains("только через папку"))
    );
}

fn new_message_value(app: &mut App, message: Value) {
    td(
        app,
        json!({"@type": "updateNewMessage", "message": message}),
    );
}

fn reply_message(chat_id: i64, id: i64, text: &str, to: i64) -> Value {
    let mut m = message(chat_id, id, text);
    m["reply_to"] = json!({"@type": "messageReplyToMessage", "poll_option_id": "", "chat_id": chat_id, "message_id": to,
                           "checklist_task_id": 0, "origin_send_date": 0});
    m
}

/// Text typed into the input field, as a paste into the editor.
fn typed(text: &str) -> PaneMsg {
    use iced::widget::text_editor::{Action, Edit};
    PaneMsg::Compose(Action::Edit(Edit::Paste(std::sync::Arc::new(
        text.to_owned(),
    ))))
}

fn pane_msg(app: &mut App, msg: PaneMsg) {
    let _ = app.update(Msg::Pane(app.main_window, msg));
}

#[test]
fn replies_show_the_loaded_original_or_fetch_it() {
    let mut app = app();
    open(&mut app, 1, &[(10, "вопрос")]);
    new_message_value(&mut app, reply_message(1, 11, "ответ", 10));
    let reply = pane(&app).messages.last().unwrap().reply_to;
    assert_eq!(reply, Some(10));
    assert_eq!(
        app.reply_lookup(1, 10).map(|m| m.text.as_str()),
        Some("вопрос")
    );

    // Original not loaded: its preview arrives separately.
    new_message_value(&mut app, reply_message(1, 12, "ещё", 5));
    assert!(app.reply_lookup(1, 5).is_none());
    let original = serde_json::from_value(message(1, 5, "давнее")).unwrap();
    let _ = app.update(Msg::RepliesLoaded(vec![original]));
    assert_eq!(
        app.reply_lookup(1, 5).map(|m| m.text.as_str()),
        Some("давнее")
    );
}

#[test]
fn sending_a_reply_clears_it_with_the_draft() {
    let mut app = app();
    open(&mut app, 1, &[(10, "вопрос")]);
    pane_msg(&mut app, PaneMsg::Reply(10));
    assert_eq!(pane(&app).reply_to, Some(10));
    pane_msg(&mut app, typed("ответ"));
    pane_msg(&mut app, PaneMsg::Send);
    assert_eq!(pane(&app).reply_to, None);
    assert!(pane(&app).compose.is_empty());

    // Esc drops a reply that was not sent.
    pane_msg(&mut app, PaneMsg::Reply(10));
    ctrl_free(&mut app, Key::Named(keyboard::key::Named::Escape));
    assert_eq!(pane(&app).reply_to, None);
}

fn ctrl_free(app: &mut App, key: Key) {
    let _ = app.update(Msg::Key(app.main_window, key, Modifiers::empty()));
}

#[test]
fn jump_loads_history_around_and_pages_newer_until_the_newest() {
    let mut app = app();
    app.session.chats.insert(
        1,
        ChatItem {
            order: 1,
            last_id: 400,
            ..chat_item("чат", false)
        },
    );
    open(&mut app, 1, &[(390, "a"), (400, "b")]);
    pane_msg(&mut app, PaneMsg::JumpTo(100));
    assert_eq!(pane(&app).jump_pending, Some(100));
    // A late "latest page" answer from opening the chat must not replace the
    // jump target history while the jump is still pending.
    let latest = page(1, [399, 400]);
    pane_msg(&mut app, PaneMsg::HistoryLoaded(1, Ok(latest)));
    assert_eq!(
        pane(&app).messages.iter().map(|m| m.id).collect::<Vec<_>>(),
        vec![390, 400]
    );

    pane_msg(
        &mut app,
        PaneMsg::AroundLoaded(1, 100, Ok(page(1, 90..110))),
    );
    assert_eq!(pane(&app).jump_pending, None);
    assert_eq!(pane(&app).highlight, Some(100));
    assert!(!pane(&app).newest_loaded);
    assert_eq!(pane(&app).messages.first().map(|m| m.id), Some(90));

    // A stale "latest page" answer landing after the jump already replaced
    // the history must not overwrite it either, whichever answer is late.
    pane_msg(&mut app, PaneMsg::HistoryLoaded(1, Ok(page(1, [500, 501]))));
    assert_eq!(pane(&app).messages.first().map(|m| m.id), Some(90));

    // Live messages wait for the newer pages instead of leaving a gap.
    new_message(&mut app, 1, 401, "новое");
    assert_eq!(pane(&app).messages.last().map(|m| m.id), Some(109));

    // A page of 49 messages (the most `history_newer` used to ever return)
    // must not be mistaken for reaching the chat's last known message.
    pane_msg(
        &mut app,
        PaneMsg::NewerLoaded(1, 109, Ok(page(1, 110..159))),
    );
    assert!(
        !pane(&app).newest_loaded,
        "still short of the chat's last_id"
    );
    pane_msg(
        &mut app,
        PaneMsg::NewerLoaded(1, 158, Ok(page(1, [390, 400, 401]))),
    );
    assert!(pane(&app).newest_loaded);
    assert_eq!(pane(&app).messages.last().map(|m| m.id), Some(401));
}

#[test]
fn append_newer_covers_deleted_messages_right_after_the_last_loaded_one() {
    let mut app = app();
    // Archived and deleted exactly where the newer page starts: TDLib
    // never returns a deleted message, so `history_newer`'s answer jumps
    // straight from 100 to 102, skipping 101.
    new_message(&mut app, 1, 101, "gone");
    delete(&mut app, 1, &[101], false);

    app.session.chats.insert(
        1,
        ChatItem {
            order: 1,
            last_id: 200,
            ..chat_item("чат", false)
        },
    );
    open(&mut app, 1, &[(100, "a")]);
    // As after a jump: history loaded up to 100 only, newer not reached yet.
    let window = app.main_window;
    let loaded = app.session.panes.get_mut(&window).unwrap();
    loaded.messages.retain(|m| m.id <= 100);
    loaded.newest_loaded = false;
    pane_msg(
        &mut app,
        PaneMsg::NewerLoaded(1, 100, Ok(page(1, [102, 103]))),
    );
    let ids: Vec<i64> = pane(&app).messages.iter().map(|m| m.id).collect();
    assert_eq!(
        ids,
        [100, 101, 102, 103],
        "the deleted message right after the last loaded one must not fall \
         into the gap between it and the first message of the new page"
    );
    assert!(
        pane(&app)
            .messages
            .iter()
            .find(|m| m.id == 101)
            .unwrap()
            .deleted
    );
}

fn chat_search_request(app: &App) -> (u64, Option<i64>) {
    pane(app).search.as_ref().unwrap().paging.pending.unwrap()
}

fn list_search_request(app: &App) -> (u64, Option<String>) {
    pane(app).list.paging.pending.clone().unwrap()
}

#[test]
fn chat_search_appends_short_and_overlapping_pages_until_server_cursor_ends() {
    let mut app = app();
    open(&mut app, 1, &[(10, "a")]);
    pane_msg(&mut app, PaneMsg::SearchToggle);
    pane_msg(&mut app, PaneMsg::SearchQuery("  кот ".into()));
    pane_msg(&mut app, PaneMsg::SearchSubmit);
    let (first, cursor) = chat_search_request(&app);
    assert_eq!(cursor, None);
    pane_msg(
        &mut app,
        PaneMsg::SearchResults(
            1,
            "кот".into(),
            first,
            cursor,
            Ok((page(1, (51..=100).rev()), Some(51))),
        ),
    );
    assert_eq!(
        pane(&app)
            .search
            .as_ref()
            .unwrap()
            .results
            .as_ref()
            .unwrap()
            .len(),
        50
    );
    pane_msg(&mut app, PaneMsg::SearchMore);
    let (second, cursor) = chat_search_request(&app);
    assert_eq!(cursor, Some(51));
    pane_msg(&mut app, PaneMsg::SearchMore);
    assert_eq!(
        chat_search_request(&app),
        (second, cursor),
        "one request per page"
    );
    pane_msg(
        &mut app,
        PaneMsg::SearchResults(
            1,
            "кот".into(),
            second,
            cursor,
            Ok((page(1, [51, 50, 49]), Some(49))),
        ),
    );
    assert_eq!(
        pane(&app)
            .search
            .as_ref()
            .unwrap()
            .results
            .as_ref()
            .unwrap()
            .len(),
        52
    );
    assert_eq!(
        pane(&app).search.as_ref().unwrap().paging.next,
        Some(49),
        "short pages are not terminal"
    );
    pane_msg(&mut app, PaneMsg::SearchMore);
    let (third, cursor) = chat_search_request(&app);
    pane_msg(
        &mut app,
        PaneMsg::SearchResults(
            1,
            "кот".into(),
            third,
            cursor,
            Ok((page(1, [49, 48]), None)),
        ),
    );
    let found: Vec<_> = pane(&app)
        .search
        .as_ref()
        .unwrap()
        .results
        .as_ref()
        .unwrap()
        .iter()
        .map(|m| m.id)
        .collect();
    assert_eq!(found.len(), 53);
    assert_eq!(&found[..3], &[100, 99, 98]);
    assert_eq!(&found[50..], &[50, 49, 48]);
    assert!(pane(&app).search.as_ref().unwrap().paging.next.is_none());
    pane_msg(&mut app, PaneMsg::SearchMore);
    assert!(pane(&app).search.as_ref().unwrap().paging.pending.is_none());
    pane_msg(&mut app, PaneMsg::JumpTo(48));
    assert!(pane(&app).search.as_ref().unwrap().results.is_none());
    assert_eq!(pane(&app).highlight, Some(48));
}

#[test]
fn chat_search_rejects_old_query_reopen_chat_and_error_then_retries_page() {
    let mut app = app();
    select(&mut app, 1, &[(10, "a")]);
    pane_msg(&mut app, PaneMsg::SearchToggle);
    pane_msg(&mut app, PaneMsg::SearchQuery("кот".into()));
    pane_msg(&mut app, PaneMsg::SearchSubmit);
    let (old, _) = chat_search_request(&app);
    pane_msg(&mut app, PaneMsg::SearchQuery("друг".into()));
    pane_msg(&mut app, PaneMsg::SearchSubmit);
    let (changed, _) = chat_search_request(&app);
    pane_msg(
        &mut app,
        PaneMsg::SearchResults(1, "кот".into(), old, None, Err("old".into())),
    );
    assert!(pane(&app).search.as_ref().unwrap().paging.error.is_none());
    pane_msg(
        &mut app,
        PaneMsg::SearchResults(1, "друг".into(), changed, None, Ok((page(1, [5]), Some(5)))),
    );
    pane_msg(&mut app, PaneMsg::SearchMore);
    let (failed, cursor) = chat_search_request(&app);
    pane_msg(
        &mut app,
        PaneMsg::SearchResults(1, "друг".into(), failed, cursor, Err("offline".into())),
    );
    assert_eq!(
        pane(&app)
            .search
            .as_ref()
            .unwrap()
            .results
            .as_ref()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        pane(&app).search.as_ref().unwrap().paging.error.as_deref(),
        Some("offline")
    );
    pane_msg(&mut app, PaneMsg::SearchMore);
    let (retry, cursor) = chat_search_request(&app);
    assert_ne!(retry, failed);
    pane_msg(
        &mut app,
        PaneMsg::SearchResults(1, "друг".into(), failed, cursor, Ok((page(1, [4]), None))),
    );
    assert_eq!(
        pane(&app)
            .search
            .as_ref()
            .unwrap()
            .results
            .as_ref()
            .unwrap()
            .len(),
        1
    );
    pane_msg(
        &mut app,
        PaneMsg::SearchResults(1, "друг".into(), retry, cursor, Ok((page(1, [4]), None))),
    );
    assert_eq!(
        pane(&app)
            .search
            .as_ref()
            .unwrap()
            .results
            .as_ref()
            .unwrap()
            .len(),
        2
    );
    pane_msg(&mut app, PaneMsg::SearchToggle);
    pane_msg(&mut app, PaneMsg::SearchToggle);
    pane_msg(&mut app, PaneMsg::SearchQuery("друг".into()));
    pane_msg(&mut app, PaneMsg::SearchSubmit);
    pane_msg(
        &mut app,
        PaneMsg::SearchResults(1, "друг".into(), changed, None, Err("old error".into())),
    );
    assert!(pane(&app).search.as_ref().unwrap().paging.error.is_none());
    let (reopened, _) = chat_search_request(&app);
    select(&mut app, 2, &[(20, "b")]);
    pane_msg(&mut app, PaneMsg::SearchToggle);
    pane_msg(&mut app, PaneMsg::SearchQuery("друг".into()));
    pane_msg(&mut app, PaneMsg::SearchSubmit);
    pane_msg(
        &mut app,
        PaneMsg::SearchResults(1, "друг".into(), reopened, None, Ok((page(1, [3]), None))),
    );
    assert!(pane(&app).search.as_ref().unwrap().results.is_none());
    select(&mut app, 1, &[(10, "a")]);
    assert!(pane(&app).search.as_ref().unwrap().paging.pending.is_none());
    pane_msg(&mut app, PaneMsg::SearchMore);
    let (restored, cursor) = chat_search_request(&app);
    assert_ne!(restored, reopened);
    assert_eq!(cursor, None);
}

#[test]
fn global_search_preserves_pages_across_errors_retries_and_result_navigation() {
    let mut app = app();
    let window = app.main_window;
    let _ = app.update(Msg::ListQuery(window, "  кот  ".into()));
    let _ = app.update(Msg::ListSearchMessages(window));
    let (first, cursor) = list_search_request(&app);
    let _ = app.update(Msg::ListMessages(
        window,
        "кот".into(),
        first,
        cursor,
        Ok((page(1, (51..=100).rev()), Some("offset-1".into()))),
    ));
    let _ = app.update(Msg::ListMore(window));
    let (second, cursor) = list_search_request(&app);
    let mut overlap = page(1, [51, 50]);
    overlap.extend(page(2, [50]));
    let _ = app.update(Msg::ListMessages(
        window,
        "кот".into(),
        second,
        cursor,
        Ok((overlap, Some("offset-2".into()))),
    ));
    assert_eq!(pane(&app).list.messages.as_ref().unwrap().len(), 52);
    let _ = app.update(Msg::ListMore(window));
    let (third, cursor) = list_search_request(&app);
    let _ = app.update(Msg::ListMore(window));
    assert_eq!(list_search_request(&app).0, third);
    let _ = app.update(Msg::ListMessages(
        window,
        "кот".into(),
        third,
        cursor.clone(),
        Err("offline".into()),
    ));
    assert_eq!(pane(&app).list.messages.as_ref().unwrap().len(), 52);
    assert_eq!(pane(&app).list.paging.error.as_deref(), Some("offline"));
    let _ = app.update(Msg::ListMore(window));
    let (retry, _) = list_search_request(&app);
    assert_ne!(retry, third);
    let _ = app.update(Msg::ListMessages(
        window,
        "кот".into(),
        third,
        cursor.clone(),
        Err("stale".into()),
    ));
    assert!(pane(&app).list.paging.error.is_none());
    let _ = app.update(Msg::ListMessages(
        window,
        "кот".into(),
        retry,
        cursor,
        Ok((page(1, [49]), None)),
    ));
    assert_eq!(pane(&app).list.messages.as_ref().unwrap().len(), 53);
    assert!(pane(&app).list.paging.next.is_none());
    let _ = app.update(Msg::OpenFound(window, 2, 50));
    assert!(pane(&app).shows(2));
    assert_eq!(pane(&app).highlight, Some(50));
}

#[test]
fn new_global_search_resets_deep_sidebar_and_shows_clickable_short_result() {
    let mut app = app();
    let window = app.main_window;
    let sidebar_id = pane(&app).list.scroll_id.clone();
    let _ = app.update(Msg::ListQuery(window, "старый".into()));
    let _ = app.update(Msg::ListSearchMessages(window));
    let (first, _) = list_search_request(&app);
    let _ = app.update(Msg::ListMessages(
        window,
        "старый".into(),
        first,
        None,
        Ok((page(1, 1..=200), Some("next".into()))),
    ));
    app.session.panes.get_mut(&window).unwrap().list.scroll = 6_000.0;
    let _ = app.update(Msg::ListMore(window));
    let (stale, cursor) = list_search_request(&app);
    let _ = app.update(Msg::ListQuery(window, "новый".into()));
    assert_eq!(pane(&app).list.scroll, 0.0);
    assert_eq!(pane(&app).list.scroll_id, sidebar_id);
    let _ = app.update(Msg::ListMessages(
        window,
        "старый".into(),
        stale,
        cursor,
        Ok((page(1, [201]), None)),
    ));
    assert!(pane(&app).list.messages.is_none());
    let _ = app.update(Msg::ListSearchMessages(window));
    let (short, _) = list_search_request(&app);
    let _ = app.update(Msg::ListMessages(
        window,
        "новый".into(),
        short,
        None,
        Ok((page(2, [9]), None)),
    ));
    let hits = pane(&app).list.messages.as_ref().unwrap();
    let visible = super::view::search_visible_rows(pane(&app).list.scroll, 100.0, 32.0, hits.len());
    assert_eq!((visible.start, visible.end), (0, 1));
    let hit = &hits[visible.start];
    let _ = app.update(Msg::OpenFound(window, hit.chat_id, hit.id));
    assert!(pane(&app).shows(2));
    assert_eq!(pane(&app).highlight, Some(9));

    app.session.panes.get_mut(&window).unwrap().list.scroll = 6_000.0;
    let _ = app.update(Msg::ListSearchMessages(window));
    assert_eq!(pane(&app).list.scroll, 0.0);
    assert_eq!(pane(&app).list.scroll_id, sidebar_id);
}

#[test]
fn global_search_discards_same_query_old_reply_and_shows_empty_terminal_page() {
    let mut app = app();
    let window = app.main_window;
    let _ = app.update(Msg::ListQuery(window, "кот".into()));
    let _ = app.update(Msg::ListSearchMessages(window));
    let (old, _) = list_search_request(&app);
    let _ = app.update(Msg::ListSearchMessages(window));
    let (current, _) = list_search_request(&app);
    assert_ne!(old, current);
    let _ = app.update(Msg::ListMessages(
        window,
        "кот".into(),
        old,
        None,
        Ok((page(1, [10]), None)),
    ));
    let _ = app.update(Msg::ListMessages(
        window,
        "кот".into(),
        old,
        None,
        Err("stale".into()),
    ));
    assert!(pane(&app).list.messages.is_none());
    assert!(pane(&app).list.paging.error.is_none());
    let _ = app.update(Msg::ListMessages(
        window,
        "кот".into(),
        current,
        None,
        Ok((Vec::new(), None)),
    ));
    assert!(pane(&app).list.messages.as_ref().unwrap().is_empty());
    assert!(pane(&app).list.paging.next.is_none());
    let _ = app.update(Msg::ListQuery(window, "друг".into()));
    assert!(pane(&app).list.messages.is_none());
}

#[test]
fn searches_stop_on_repeated_cursor_or_duplicate_only_page_and_retry_first_error() {
    let mut app = app();
    let window = app.main_window;
    open(&mut app, 1, &[]);
    pane_msg(&mut app, PaneMsg::SearchToggle);
    pane_msg(&mut app, PaneMsg::SearchQuery("q".into()));
    pane_msg(&mut app, PaneMsg::SearchSubmit);
    let (first, cursor) = chat_search_request(&app);
    pane_msg(
        &mut app,
        PaneMsg::SearchResults(1, "q".into(), first, cursor, Err("offline".into())),
    );
    pane_msg(&mut app, PaneMsg::SearchMore);
    let (retry, cursor) = chat_search_request(&app);
    assert_ne!(first, retry);
    pane_msg(
        &mut app,
        PaneMsg::SearchResults(1, "q".into(), retry, cursor, Ok((page(1, [7]), Some(7)))),
    );
    pane_msg(&mut app, PaneMsg::SearchMore);
    let (second, cursor) = chat_search_request(&app);
    pane_msg(
        &mut app,
        PaneMsg::SearchResults(1, "q".into(), second, cursor, Ok((page(1, [6]), Some(7)))),
    );
    assert!(pane(&app).search.as_ref().unwrap().paging.next.is_none());
    let _ = app.update(Msg::ListQuery(window, "q".into()));
    let _ = app.update(Msg::ListSearchMessages(window));
    let (first, cursor) = list_search_request(&app);
    let _ = app.update(Msg::ListMessages(
        window,
        "q".into(),
        first,
        cursor,
        Ok((page(1, [7]), Some("next".into()))),
    ));
    let _ = app.update(Msg::ListMore(window));
    let (second, cursor) = list_search_request(&app);
    let _ = app.update(Msg::ListMessages(
        window,
        "q".into(),
        second,
        cursor,
        Ok((page(1, [7]), Some("later".into()))),
    ));
    assert!(pane(&app).list.paging.next.is_none());
}

#[test]
fn chat_list_filter_matches_titles_and_tdlib_results() {
    let mut app = app();
    for (id, title) in [(1, "Работа"), (2, "Мама"), (3, "Рабочий чат")] {
        app.session.chats.insert(
            id,
            ChatItem {
                order: id,
                ..chat_item(title, false)
            },
        );
        app.session.order.insert((std::cmp::Reverse(id), id));
    }
    let main = app.main_window;
    let _ = app.update(Msg::ListQuery(main, "раб".into()));
    assert_eq!(app.filtered_chats(main), Some(vec![3, 1]));
    // TDLib also found chat 2 (e.g. by username); unknown ids are skipped.
    let _ = app.update(Msg::ListChats(main, "раб".into(), Ok(vec![2, 99])));
    assert_eq!(app.filtered_chats(main), Some(vec![3, 1, 2]));
    let _ = app.update(Msg::ListQuery(main, String::new()));
    assert_eq!(app.filtered_chats(main), None);
}

#[test]
fn archive_title_filter_uses_only_tdlib_archive_members() {
    let mut app = app();
    let window = app.main_window;
    for (id, title) in [(1, "Работа"), (2, "Рабочий архив"), (3, "Не по запросу")]
    {
        app.session.chats.insert(id, chat_item(title, false));
    }
    app.set_order(1, 30);
    app.set_archive_order(2, 20);
    app.set_archive_order(3, 10);
    let _ = app.update(Msg::ListQuery(window, "раб".into()));
    let _ = app.update(Msg::ListChats(window, "раб".into(), Ok(vec![2, 3])));
    assert_eq!(app.filtered_chats(window), Some(vec![1, 2, 3]));
    let _ = app.update(Msg::ShowArchive(window, true));
    assert_eq!(app.filtered_chats(window), Some(vec![2]));
    // A late global title-search answer must not leak into Archive.
    let _ = app.update(Msg::ListChats(window, "раб".into(), Ok(vec![1, 3])));
    assert!(pane(&app).list.chats.is_empty());
    assert_eq!(app.filtered_chats(window), Some(vec![2]));
    let _ = app.update(Msg::ListQuery(window, "раб".into()));
    assert_eq!(app.filtered_chats(window), Some(vec![2]));
    let _ = app.update(Msg::ShowArchive(window, false));
    assert_eq!(app.filtered_chats(window), Some(vec![1]));
    let _ = app.update(Msg::ListChats(window, "раб".into(), Ok(vec![2])));
    assert_eq!(app.filtered_chats(window), Some(vec![1, 2]));
}

#[test]
fn archive_message_search_keeps_scope_and_cursor_and_rejects_scope_switch_replies() {
    use tdlib_rs::enums::ChatList;

    let mut app = app();
    let window = app.main_window;
    let _ = app.update(Msg::ListQuery(window, "кот".into()));
    assert!(super::nav::list_search_scope(pane(&app).list.archive).is_none());
    let _ = app.update(Msg::ListSearchMessages(window));
    let (old_global, _) = list_search_request(&app);
    let _ = app.update(Msg::ShowArchive(window, true));
    assert!(matches!(
        super::nav::list_search_scope(pane(&app).list.archive),
        Some(ChatList::Archive)
    ));
    assert!(pane(&app).list.paging.pending.is_none());
    let _ = app.update(Msg::ListSearchMessages(window));
    let (first, cursor) = list_search_request(&app);
    assert_eq!(cursor, None);
    let _ = app.update(Msg::ListMessages(
        window,
        "кот".into(),
        old_global,
        None,
        Ok((page(1, [11]), Some("global-next".into()))),
    ));
    assert!(pane(&app).list.messages.is_none());
    let _ = app.update(Msg::ListMessages(
        window,
        "кот".into(),
        first,
        None,
        Ok((page(2, [20]), Some("archive-next".into()))),
    ));
    let _ = app.update(Msg::ListMore(window));
    let (second, cursor) = list_search_request(&app);
    assert_eq!(cursor.as_deref(), Some("archive-next"));
    assert!(matches!(
        super::nav::list_search_scope(pane(&app).list.archive),
        Some(ChatList::Archive)
    ));
    let _ = app.update(Msg::ShowFolder(window, Some(9)));
    assert!(super::nav::list_search_scope(pane(&app).list.archive).is_none());
    assert!(pane(&app).list.messages.is_none());
    assert!(pane(&app).list.paging.next.is_none());
    let _ = app.update(Msg::ListMessages(
        window,
        "кот".into(),
        second,
        cursor,
        Err("late archive error".into()),
    ));
    assert!(pane(&app).list.paging.error.is_none());
    let _ = app.update(Msg::ListSearchMessages(window));
    let (folder_request, folder_cursor) = list_search_request(&app);
    assert_eq!(folder_cursor, None);
    let _ = app.update(Msg::ShowArchive(window, true));
    let _ = app.update(Msg::ListSearchMessages(window));
    let (archive_request, _) = list_search_request(&app);
    let _ = app.update(Msg::ListMessages(
        window,
        "кот".into(),
        folder_request,
        folder_cursor,
        Ok((page(1, [12]), Some("folder-next".into()))),
    ));
    assert!(pane(&app).list.messages.is_none());
    let _ = app.update(Msg::ListMessages(
        window,
        "кот".into(),
        archive_request,
        None,
        Ok((page(2, [19]), None)),
    ));
    assert_eq!(pane(&app).list.messages.as_ref().unwrap()[0].chat_id, 2);
    assert!(pane(&app).list.paging.next.is_none());
    let _ = app.update(Msg::ShowArchive(window, false));
    assert!(super::nav::list_search_scope(pane(&app).list.archive).is_none());
    assert!(pane(&app).list.messages.is_none());
    let _ = app.update(Msg::ListMessages(
        window,
        "кот".into(),
        archive_request,
        None,
        Err("late archive error".into()),
    ));
    assert!(pane(&app).list.paging.error.is_none());
}

fn notification_group(
    chat_id: i64,
    text: &str,
    show_preview: bool,
    silent: bool,
) -> tdlib_rs::types::UpdateNotificationGroup {
    let mut m = message(chat_id, 50, text);
    m["sender_id"] = json!({"@type": "messageSenderUser", "user_id": 7});
    serde_json::from_value(json!({
        "@type": "updateNotificationGroup", "notification_group_id": 1,
        "type": {"@type": "notificationGroupTypeMessages"}, "chat_id": chat_id,
        "notification_settings_chat_id": chat_id, "notification_sound_id": "0", "total_count": 1,
        "added_notifications": [{"@type": "notification", "id": 1, "date": 0, "is_silent": silent,
            "type": {"@type": "notificationTypeNewMessage", "message": m, "show_preview": show_preview}}],
        "removed_notification_ids": []
    }))
    .unwrap()
}

#[test]
fn notifications_respect_preview_focus_and_setting() {
    let mut app = app();
    app.session.chats.insert(-100, chat_item("Группа", false));
    app.session.users.insert(7, "Аня".into());
    let popup = app
        .popup(&notification_group(-100, "привет", true, false))
        .unwrap();
    assert_eq!(
        (popup.title.as_str(), popup.body.as_str()),
        ("Группа", "Аня: привет")
    );

    let hidden = app
        .popup(&notification_group(-100, "секрет", false, true))
        .unwrap();
    assert_eq!(hidden.body, "Новое сообщение");
    assert!(hidden.silent);

    // The chat is on screen in the focused window: no popup.
    open(&mut app, -100, &[]);
    let _ = app.update(Msg::WindowFocus(app.main_window, true));
    assert!(
        app.popup(&notification_group(-100, "x", true, false))
            .is_none()
    );
    let _ = app.update(Msg::WindowFocus(app.main_window, false));
    assert!(
        app.popup(&notification_group(-100, "x", true, false))
            .is_some()
    );

    app.settings.notifications = false;
    assert!(
        app.popup(&notification_group(-100, "x", true, false))
            .is_none()
    );
}

#[test]
fn notification_group_shows_only_its_newest_message() {
    let mut app = app();
    app.session.users.insert(7, "Аня".into());
    let mut first = message(-100, 50, "первое");
    first["sender_id"] = json!({"@type": "messageSenderUser", "user_id": 7});
    let mut second = message(-100, 51, "второе");
    second["sender_id"] = json!({"@type": "messageSenderUser", "user_id": 7});
    let group: tdlib_rs::types::UpdateNotificationGroup = serde_json::from_value(json!({
        "@type": "updateNotificationGroup", "notification_group_id": 1,
        "type": {"@type": "notificationGroupTypeMessages"}, "chat_id": -100,
        "notification_settings_chat_id": -100, "notification_sound_id": "0", "total_count": 2,
        "added_notifications": [
            {"@type": "notification", "id": 1, "date": 0, "is_silent": false,
                "type": {"@type": "notificationTypeNewMessage", "message": first, "show_preview": true}},
            {"@type": "notification", "id": 2, "date": 0, "is_silent": false,
                "type": {"@type": "notificationTypeNewMessage", "message": second, "show_preview": true}}
        ],
        "removed_notification_ids": []
    }))
    .unwrap();
    // Both notifications replace the same on-screen popup: only the
    // newest is worth a call to `show`.
    assert_eq!(app.popup(&group).unwrap().body, "Аня: второе");
}

#[test]
fn notification_click_for_another_account_is_ignored() {
    let mut app = app();
    app.session.chats.insert(5, chat_item("Чат", false));
    // The popup was shown for a session that is no longer the active
    // account (e.g. it switched, or logged out and back in): stale.
    let other_slot = app.session.slot + 1;
    let _ = app.update(Msg::NotificationClicked(other_slot, 5));
    assert_eq!(active(&app), None, "another account's click opens nothing");
    // The same chat, clicked for the account that is actually running.
    let slot = app.session.slot;
    let _ = app.update(Msg::NotificationClicked(slot, 5));
    assert_eq!(active(&app), Some(5));
}

#[test]
fn spoiler_photo_loads_only_after_reveal() {
    let mut app = app();
    open(&mut app, 1, &[]);
    let mut content = photo_content();
    content["has_spoiler"] = json!(true);
    new_message_value(&mut app, media_message(1, 10, content));
    assert!(matches!(
        pane(&app).messages[0].media,
        Some(Media::Photo { spoiler: true, .. })
    ));
    assert!(app.session.wanted_photos.is_empty());
    pane_msg(&mut app, PaneMsg::Reveal(10));
    assert!(pane(&app).revealed.contains(&10));
    assert!(app.session.wanted_photos.contains(&103));
}

#[test]
fn evicted_avatar_is_refetched_when_shown_again() {
    let mut app = app();
    let peer = avatars::Peer::User(42);
    sandbox::photo(&mut app, peer, 9500, [10, 20, 30], [200, 210, 220]);
    assert!(
        app.session.avatars.handle(peer).is_some(),
        "decoded and cached"
    );

    // More avatars than the private MAX_CACHED bound (600): 9500 evicts.
    for id in 9501..10300 {
        app.avatar_decoded(id, Ok(vec![0u8; 4]));
    }
    assert!(
        app.session.avatars.handle(peer).is_none(),
        "evicted by newer avatars"
    );

    // Shown again: the file is already downloaded, so it is decoded right
    // away instead of leaving initials up until the app restarts.
    let _ = app.update(Msg::AvatarShown(peer));
    assert!(app.session.avatars.decoding.contains(&9500));
}

fn thumbnail(id: i32, format: &str) -> Value {
    json!({"@type": "thumbnail", "format": {"@type": format}, "width": 90, "height": 60,
           "file": file(id, 100, 0, false, "")})
}

#[test]
fn voice_stickers_videos_and_gifs_are_recognized() {
    let packed = crate::av::audio::pack_waveform(&[0, 31, 16, 8]);
    let voice = json!({"@type": "messageVoiceNote", "is_listened": false,
        "caption": {"@type": "formattedText", "text": "", "entities": []},
        "voice_note": {"@type": "voiceNote", "duration": 7, "mime_type": "audio/ogg",
            "waveform": base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &packed),
            "voice": file(300, 5000, 0, false, "")}});
    let sticker = json!({"@type": "messageSticker", "is_premium": false,
        "sticker": {"@type": "sticker", "id": "1", "set_id": "2", "width": 512, "height": 256, "emoji": "😀",
            "format": {"@type": "stickerFormatTgs"},
            "full_type": {"@type": "stickerFullTypeRegular"},
            "sticker": file(301, 5000, 0, false, "")}});
    let video = json!({"@type": "messageVideo", "alternative_videos": [], "storyboards": [],
        "start_timestamp": 0, "show_caption_above_media": false, "has_spoiler": true, "is_secret": false,
        "caption": {"@type": "formattedText", "text": "", "entities": []},
        "video": {"@type": "video", "duration": 75, "width": 1280, "height": 720, "file_name": "v.mp4",
            "mime_type": "video/mp4", "has_stickers": false, "supports_streaming": true,
            "thumbnail": thumbnail(303, "thumbnailFormatJpeg"),
            "video": file(302, 9000, 0, false, "")}});
    let gif = json!({"@type": "messageAnimation", "show_caption_above_media": false,
        "has_spoiler": false, "is_secret": false,
        "caption": {"@type": "formattedText", "text": "", "entities": []},
        "animation": {"@type": "animation", "duration": 3, "width": 480, "height": 270,
            "file_name": "g.mp4", "mime_type": "video/mp4", "has_stickers": false,
            // An animated preview cannot be shown as a still image.
            "thumbnail": thumbnail(305, "thumbnailFormatMpeg4"),
            "animation": file(304, 9000, 0, false, "")}});
    let round = json!({"@type": "messageVideoNote", "is_viewed": false, "is_secret": false,
        "video_note": {"@type": "videoNote", "duration": 9, "waveform": "", "length": 384,
            "video": file(306, 9000, 0, false, "")}});

    let mut app = app();
    open(&mut app, 1, &[]);
    for (id, content) in [(1, voice), (2, sticker), (3, video), (4, gif), (5, round)] {
        new_message_value(&mut app, media_message(1, id, content));
    }
    let media: Vec<&Media> = pane(&app)
        .messages
        .iter()
        .filter_map(|m| m.media.as_ref())
        .collect();
    assert!(
        matches!(media[0], Media::Voice { file_id: 300, duration: 7, waveform } if waveform[..4] == [0, 31, 16, 8])
    );
    assert!(matches!(
        media[1],
        Media::Sticker {
            file_id: 301,
            kind: media::Motion::Lottie,
            ..
        }
    ));
    assert_eq!(media[1].display_box(), Some((180.0, 90.0)));
    assert!(matches!(
        media[2],
        Media::Video {
            file_id: 302,
            thumb: Some(303),
            duration: 75,
            spoiler: true,
            ..
        }
    ));
    assert!(matches!(
        media[3],
        Media::Animation {
            file_id: 304,
            thumb: None,
            round: false,
            ..
        }
    ));
    assert!(matches!(
        media[4],
        Media::Animation {
            file_id: 306,
            round: true,
            ..
        }
    ));
    assert_eq!(
        media[4].display_box(),
        Some((media::ROUND_SIZE, media::ROUND_SIZE))
    );
    // Files of all of them are tracked (video thumbnail included).
    for id in [300, 301, 302, 303, 304, 306] {
        assert!(app.session.files.contains_key(&id), "file {id}");
    }
}

#[test]
fn animations_start_when_downloaded_and_stop_off_screen() {
    let mut app = app();
    // On screen before its file is there: waits for the download.
    let _ = app.update(Msg::AnimShow(304, media::Motion::Video));
    assert!(app.session.playback.waiting(304));
    assert!(!app.session.playback.playing(304));
    td(
        &mut app,
        json!({"@type": "updateFile", "file": file(304, 9000, 9000, true, "/tmp/g.mp4")}),
    );
    assert!(app.session.playback.playing(304));

    let _ = app.update(Msg::AnimHide(304));
    assert!(!app.session.playback.playing(304));

    // At most three at once.
    for id in 400..405 {
        td(
            &mut app,
            json!({"@type": "updateFile", "file": file(id, 10, 10, true, "/tmp/x.mp4")}),
        );
        let _ = app.update(Msg::AnimShow(id, media::Motion::Video));
    }
    let playing = (400..405)
        .filter(|&id| app.session.playback.playing(id))
        .count();
    assert_eq!(playing, 3);
}

#[test]
fn logged_in_account_is_remembered_and_a_duplicate_is_refused() {
    let mut app = app();
    app.session.users.insert(5, "Alice".into());
    let _ = app.account_known(5);
    assert_eq!(
        app.settings.accounts,
        [Account {
            slot: 0,
            user_id: Some(5),
            name: "Alice".into(),
            ..Default::default()
        }]
    );
    // A new slot logs into the same user: back to the first slot, the new
    // one is dropped.
    let _ = app.add_account();
    let added = app.settings.accounts[1].slot;
    assert!(added > 0);
    assert_eq!(
        app.session.leave,
        Some(accounts::Leave {
            to: added,
            forget: false
        })
    );
    app.session.leave = None;
    app.session.slot = added;
    let _ = app.account_known(5);
    assert_eq!(
        app.session.leave,
        Some(accounts::Leave {
            to: 0,
            forget: true
        })
    );
    assert_eq!(app.after_close(), 0);
    assert_eq!(app.settings.accounts.len(), 1);
    assert_eq!(app.settings.active_account, 0);
}

#[test]
fn switching_keeps_accounts_and_log_out_moves_to_another() {
    let mut app = app();
    app.settings.accounts = vec![
        Account {
            slot: 0,
            user_id: Some(5),
            name: "Alice".into(),
            ..Default::default()
        },
        Account {
            slot: 3,
            user_id: Some(6),
            name: "Demo User".into(),
            ..Default::default()
        },
    ];
    let _ = app.switch_account(3);
    assert_eq!(app.after_close(), 3, "switch starts the other slot");
    assert_eq!(app.settings.accounts.len(), 2, "switching forgets nothing");

    // Now on slot 3: an unexpected close reopens the same account.
    app.session.slot = 3;
    assert_eq!(app.after_close(), 3);
    assert_eq!(app.settings.accounts.len(), 2);

    // Log out (here or from another device): the account goes, the other opens.
    let data = crate::paths::db_dir(3);
    std::fs::create_dir_all(&data).unwrap();
    // The archive of deleted messages and the plugin database, keyed by the
    // user id rather than the slot, with their WAL/SHM side files.
    let archive = crate::paths::archive(6);
    let plugins_db = crate::paths::plugins_db(6);
    let side = |path: &std::path::Path, suffix: &str| {
        let mut s = path.as_os_str().to_owned();
        s.push(suffix);
        std::path::PathBuf::from(s)
    };
    for path in [&archive, &plugins_db] {
        std::fs::write(path, b"data").unwrap();
        std::fs::write(side(path, "-wal"), b"wal").unwrap();
        std::fs::write(side(path, "-shm"), b"shm").unwrap();
    }
    app.session.logging_out = true;
    assert_eq!(app.after_close(), 0);
    assert_eq!(
        app.settings
            .accounts
            .iter()
            .map(|a| a.slot)
            .collect::<Vec<_>>(),
        [0]
    );
    assert!(
        !data.exists(),
        "session data of the logged out account removed"
    );
    for path in [&archive, &plugins_db] {
        assert!(!path.exists(), "{path:?} removed");
        assert!(!side(path, "-wal").exists());
        assert!(!side(path, "-shm").exists());
    }
    assert_eq!(
        Settings::load(&app.settings_path).unwrap().active_account,
        0
    );
}

#[test]
fn unfinished_added_account_can_be_cancelled() {
    let mut app = app();
    app.settings.accounts = vec![Account {
        slot: 0,
        user_id: Some(5),
        name: "Alice".into(),
        ..Default::default()
    }];
    let _ = app.add_account();
    let added = app.settings.accounts[1].slot;
    app.session.leave = None;
    app.session.slot = added;
    app.session.auth = Auth::Phone;
    let _ = app.update(Msg::CancelAddAccount);
    assert_eq!(app.after_close(), 0);
    assert_eq!(app.settings.accounts.len(), 1);
}

#[test]
fn own_messages_turn_read_when_the_other_side_reads_them() {
    let mut app = app();
    app.session.chats.insert(1, chat_item("Alice", false));
    // The chat list follows the newest message: own and sent.
    let mut own = message(1, 11, "привет");
    own["is_outgoing"] = json!(true);
    td(
        &mut app,
        json!({"@type": "updateChatLastMessage", "chat_id": 1,
               "last_message": own, "positions": []}),
    );
    let chat = &app.session.chats[&1];
    assert_eq!(
        app.receipt(1, chat.last_id, chat.last_outgoing, chat.last_pending),
        Some(Receipt::Sent)
    );
    assert_eq!(app.receipt(1, 12, true, true), Some(Receipt::Sending));
    assert_eq!(
        app.receipt(1, 10, false, false),
        None,
        "others' messages have no dot"
    );

    td(
        &mut app,
        json!({"@type": "updateChatReadOutbox", "chat_id": 1, "last_read_outbox_message_id": 11}),
    );
    assert_eq!(app.receipt(1, 10, true, false), Some(Receipt::Read));
    assert_eq!(app.receipt(1, 11, true, false), Some(Receipt::Read));
    assert_eq!(
        app.receipt(1, 13, true, false),
        Some(Receipt::Sent),
        "newer ones stay unread"
    );
}

#[test]
fn message_send_failed_marks_the_message_and_shows_the_error() {
    let mut app = app();
    // A message old enough to make the chat "at the newest": needed for
    // updateNewMessage to append the pending message below.
    open(&mut app, 1, &[(5, "старое")]);
    let mut pending = message(1, 100, "привет");
    pending["is_outgoing"] = json!(true);
    pending["sending_state"] = json!({"@type": "messageSendingStatePending", "sending_id": 1});
    td(
        &mut app,
        json!({"@type": "updateNewMessage", "message": pending}),
    );
    assert!(
        pane(&app)
            .messages
            .iter()
            .any(|m| m.id == 100 && m.pending && !m.failed)
    );

    let mut failed = message(1, 100, "привет");
    failed["is_outgoing"] = json!(true);
    failed["sending_state"] = json!({"@type": "messageSendingStateFailed",
        "error": {"@type": "error", "code": 400, "message": "CHAT_WRITE_FORBIDDEN"},
        "can_retry": false, "need_another_sender": false, "need_another_reply_quote": false,
        "need_drop_reply": false, "required_paid_message_star_count": 0, "retry_after": 0.0});
    td(
        &mut app,
        json!({"@type": "updateMessageSendFailed", "message": failed, "old_message_id": 100,
               "error": {"@type": "error", "code": 400, "message": "CHAT_WRITE_FORBIDDEN"}}),
    );
    let item = pane(&app).messages.iter().find(|m| m.id == 100).unwrap();
    assert!(item.failed, "no longer stuck as \"sending\" forever");
    assert!(!item.pending);
    assert_eq!(
        app.error,
        Some("не отправлено: CHAT_WRITE_FORBIDDEN".to_owned())
    );
}

#[test]
fn successful_done_keeps_an_existing_error() {
    let mut app = app();
    app.error = Some("предыдущая ошибка".to_owned());
    let _ = app.update(Msg::Done(Ok(())));
    assert_eq!(
        app.error,
        Some("предыдущая ошибка".to_owned()),
        "an unrelated success must not wipe an error shown elsewhere"
    );
    let _ = app.update(Msg::Done(Err("новая ошибка".to_owned())));
    assert_eq!(app.error, Some("новая ошибка".to_owned()));
}

#[test]
fn typing_shows_names_in_groups_and_ignores_own_devices() {
    let mut app = app();
    app.session.my_id = Some(1);
    for (id, private) in [(10, true), (20, false)] {
        app.session
            .chats
            .insert(id, chat_item(&id.to_string(), private));
    }
    app.session.users.insert(7, "Игорь Петров".into());
    app.session.users.insert(8, "Маша".into());
    let action = |app: &mut App, chat: i64, user: i64, action: &str| {
        td(
            app,
            json!({"@type": "updateChatAction", "chat_id": chat,
                   "sender_id": {"@type": "messageSenderUser", "user_id": user},
                   "action": {"@type": action}}),
        );
    };
    action(&mut app, 10, 7, "chatActionTyping");
    assert_eq!(app.typing_label(10).as_deref(), Some("печатает."));
    action(&mut app, 20, 7, "chatActionTyping");
    action(&mut app, 20, 8, "chatActionTyping");
    action(&mut app, 20, 1, "chatActionTyping");
    let _ = app.update(Msg::TypingTick);
    assert_eq!(
        app.typing_label(20).as_deref(),
        Some("Игорь и Маша печатают..")
    );
    action(&mut app, 20, 7, "chatActionCancel");
    assert_eq!(app.typing_label(20).as_deref(), Some("Маша печатает.."));
}

#[test]
fn editing_puts_the_text_in_the_input_and_brings_the_draft_back() {
    let mut app = app();
    open(&mut app, 1, &[(10, "привет")]);
    pane_msg(&mut app, typed("черновик"));
    pane_msg(&mut app, PaneMsg::Reply(10));
    pane_msg(
        &mut app,
        PaneMsg::EditLoaded(1, 10, Ok("**привет**".into())),
    );
    assert_eq!(pane(&app).compose.text(), "**привет**");
    assert_eq!(pane(&app).reply_to, None, "editing replaces a reply");

    // Multi-line edit saved: the draft returns.
    pane_msg(&mut app, typed("\nвторая строка"));
    assert_eq!(pane(&app).compose.text(), "**привет**\nвторая строка");
    pane_msg(&mut app, PaneMsg::Send);
    assert_eq!(pane(&app).editing, None);
    assert_eq!(pane(&app).compose.text(), "черновик");

    // Esc cancels an edit the same way.
    pane_msg(&mut app, PaneMsg::EditLoaded(1, 10, Ok("привет".into())));
    let _ = app.update(Msg::Key(
        app.main_window,
        keyboard::Key::Named(keyboard::key::Named::Escape),
        keyboard::Modifiers::empty(),
    ));
    assert_eq!(pane(&app).editing, None);
    assert_eq!(pane(&app).compose.text(), "черновик");
}

#[test]
fn edited_message_shows_new_text_and_mark() {
    let mut app = app();
    open(&mut app, 1, &[(10, "было")]);
    td(
        &mut app,
        json!({"@type": "updateMessageContent", "chat_id": 1, "message_id": 10,
               "new_content": {"@type": "messageText", "text": {"@type": "formattedText",
                   "text": "стало", "entities": [{"@type": "textEntity", "offset": 0,
                   "length": 5, "type": {"@type": "textEntityTypeBold"}}]}}}),
    );
    td(
        &mut app,
        json!({"@type": "updateMessageEdited", "chat_id": 1, "message_id": 10,
               "edit_date": 1_700_000_000}),
    );
    let m = &pane(&app).messages[0];
    assert!(m.edited);
    let shown: String = m.rich.iter().map(|p| p.text.as_str()).collect();
    assert_eq!(shown, "стало", "the bubble shows the new text, not the old");
    assert!(m.rich[0].style.bold);
}

#[test]
fn chat_with_unread_opens_at_the_first_unread_without_highlight() {
    let mut app = app();
    let chat = |unread, read_inbox| ChatItem {
        unread,
        read_inbox,
        ..chat_item("Маша", true)
    };
    // Unread inside the first page: divider after 11, scrolled there.
    app.session.chats.insert(1, chat(2, 11));
    open(&mut app, 1, &[(10, "a"), (11, "b"), (12, "c"), (13, "d")]);
    assert_eq!(pane(&app).unread_after, Some(11));
    assert!(pane(&app).scroll.is_some());
    assert_eq!(pane(&app).jump_pending, None);

    // More unread than a page: the history around the last read one loads,
    // quietly.
    app.session.chats.insert(2, chat(80, 5));
    open(&mut app, 2, &[(90, "x"), (91, "y")]);
    assert_eq!(pane(&app).unread_after, Some(5));
    assert_eq!(pane(&app).jump_pending, Some(5));
    let around = page(2, 1..=10);
    let _ = app.update(Msg::Pane(
        app.main_window,
        PaneMsg::AroundLoaded(2, 5, Ok(around)),
    ));
    assert_eq!(
        pane(&app).highlight,
        None,
        "opening at unread highlights nothing"
    );

    // Nothing unread: no divider.
    app.session.chats.insert(3, chat(0, 20));
    open(&mut app, 3, &[(20, "z")]);
    assert_eq!(pane(&app).unread_after, None);
}

#[test]
fn pinned_bar_cycles_back_through_pinned_messages() {
    let mut app = app();
    open(
        &mut app,
        1,
        &[(10, "правила"), (11, "a"), (12, "расписание")],
    );
    let pinned = page(1, [12, 10]);
    pane_msg(&mut app, PaneMsg::PinnedLoaded(1, Ok(pinned)));
    assert_eq!(pane(&app).pinned.len(), 2);
    assert_eq!(pane(&app).pinned_shown, 0, "the newest pinned shows first");

    pane_msg(&mut app, PaneMsg::PinnedClicked);
    assert_eq!(pane(&app).highlight, Some(12));
    assert_eq!(pane(&app).pinned_shown, 1);
    pane_msg(&mut app, PaneMsg::PinnedClicked);
    assert_eq!(pane(&app).highlight, Some(10));
    assert_eq!(pane(&app).pinned_shown, 0, "wraps around to the newest");

    // A reload keeps showing the same message.
    pane_msg(&mut app, PaneMsg::PinnedClicked);
    pane_msg(
        &mut app,
        PaneMsg::PinnedLoaded(1, Ok(page(1, [12, 11, 10]))),
    );
    assert_eq!(pane(&app).pinned[pane(&app).pinned_shown].id, 10);
    // Answers for another chat are ignored.
    pane_msg(&mut app, PaneMsg::PinnedLoaded(2, Ok(Vec::new())));
    assert_eq!(pane(&app).pinned.len(), 3);
}

#[test]
fn reactions_follow_tdlib_and_own_one_is_known() {
    let mut app = app();
    open(&mut app, 1, &[(10, "фото с гор")]);
    td(
        &mut app,
        json!({"@type": "updateMessageInteractionInfo", "chat_id": 1, "message_id": 10,
        "interaction_info": {"@type": "messageInteractionInfo", "view_count": 0,
            "forward_count": 0, "reactions": {"@type": "messageReactions",
            "are_tags": false, "paid_reactors": [], "can_get_added_reactions": false,
            "reactions": [
                {"@type": "messageReaction", "type": {"@type": "reactionTypeEmoji", "emoji": "🔥"},
                 "total_count": 3, "is_chosen": true, "recent_sender_ids": []},
                {"@type": "messageReaction", "type": {"@type": "reactionTypeCustomEmoji", "custom_emoji_id": "5"},
                 "total_count": 1, "is_chosen": false, "recent_sender_ids": []}
            ]}}}),
    );
    let m = &pane(&app).messages[0];
    assert_eq!(m.reactions.len(), 2);
    let fire = td::ReactionKey::Emoji("🔥".into());
    assert!(m.reacted(&fire), "a click on 🔥 takes it back");
    assert!(!m.reacted(&td::ReactionKey::Custom(5)));
    assert_eq!(m.reactions[1].key.label(), "✦");

    td(
        &mut app,
        json!({"@type": "updateMessageInteractionInfo", "chat_id": 1, "message_id": 10}),
    );
    assert!(
        pane(&app).messages[0].reactions.is_empty(),
        "all reactions removed"
    );
}

#[test]
fn selection_picks_messages_and_deletes_them_together() {
    let mut app = app();
    open(&mut app, 1, &[(10, "a"), (11, "b"), (12, "c")]);
    pane_msg(&mut app, PaneMsg::Select(10));
    pane_msg(&mut app, PaneMsg::Select(12));
    pane_msg(&mut app, PaneMsg::Select(11));
    pane_msg(&mut app, PaneMsg::Select(11));
    assert_eq!(
        pane(&app)
            .selected
            .as_ref()
            .map(|s| s.iter().copied().collect::<Vec<_>>()),
        Some(vec![10, 12])
    );

    pane_msg(&mut app, PaneMsg::DeleteSelection);
    assert_eq!(pane(&app).delete_selection, Some(None), "asking TDLib");
    let rights = td::MessageRights {
        delete_for_self: true,
        delete_for_all: true,
        edit: false,
    };
    pane_msg(&mut app, PaneMsg::SelectionRights(Ok(rights)));
    pane_msg(&mut app, PaneMsg::DeleteSelected { revoke: true });
    assert_eq!(pane(&app).selected, None);
    // Own deletions are not archived as "deleted by someone".
    delete(&mut app, 1, &[10, 12], false);
    assert_eq!(shown(&app), [(11, "b", false)]);

    // Unselecting the last one, or Esc, leaves the mode; a late answer is ignored.
    pane_msg(&mut app, PaneMsg::Select(11));
    pane_msg(&mut app, PaneMsg::Select(11));
    assert_eq!(pane(&app).selected, None);
    pane_msg(&mut app, PaneMsg::Select(11));
    let _ = app.update(Msg::Key(
        app.main_window,
        keyboard::Key::Named(keyboard::key::Named::Escape),
        keyboard::Modifiers::empty(),
    ));
    assert_eq!(pane(&app).selected, None);
    pane_msg(&mut app, PaneMsg::SelectionRights(Ok(rights)));
    assert_eq!(pane(&app).delete_selection, None);
}

#[test]
fn forwarding_picks_a_chat_and_opens_it() {
    let mut app = app();
    for (id, title) in [(1, "Откуда"), (2, "Rust чат"), (3, "Маша")] {
        app.session.chats.insert(id, chat_item(title, false));
    }
    open(&mut app, 1, &[(10, "a"), (11, "b")]);
    pane_msg(&mut app, PaneMsg::Select(10));
    pane_msg(&mut app, PaneMsg::Select(11));
    pane_msg(&mut app, PaneMsg::ForwardSelection);
    assert_eq!(pane(&app).forward, Some((vec![10, 11], String::new())));
    pane_msg(&mut app, PaneMsg::ForwardQuery("rust".into()));
    pane_msg(&mut app, PaneMsg::ForwardTo(2));
    assert_eq!(pane(&app).forward, None);
    assert_eq!(pane(&app).selected, None);
    assert_eq!(pane(&app).chat_id, Some(2), "the target chat opens");

    // Esc closes the picker without forwarding.
    open(&mut app, 1, &[(10, "a")]);
    pane_msg(&mut app, PaneMsg::Forward(vec![10]));
    let _ = app.update(Msg::Key(
        app.main_window,
        keyboard::Key::Named(keyboard::key::Named::Escape),
        keyboard::Modifiers::empty(),
    ));
    assert_eq!(pane(&app).forward, None);
    assert_eq!(pane(&app).chat_id, Some(1));
}

#[test]
fn forwarded_message_knows_its_origin() {
    let mut m = message(1, 10, "новость");
    m["forward_info"] = json!({"@type": "messageForwardInfo", "date": 0,
        "public_service_announcement_type": "",
        "origin": {"@type": "messageOriginHiddenUser", "sender_name": "Аноним"}});
    let m: Message = serde_json::from_value(m).unwrap();
    assert_eq!(
        MsgItem::from(&m).forwarded,
        Some(Origin::Name("Аноним".into()))
    );
    let plain: Message = serde_json::from_value(message(1, 11, "x")).unwrap();
    assert_eq!(MsgItem::from(&plain).forwarded, None);
}

fn poll_content(chosen: bool) -> Value {
    json!({"@type": "messagePoll", "description": {"@type": "formattedText", "text": "", "entities": []}, "can_add_option": false, "poll": {"@type": "poll", "can_get_voters": false, "can_see_results": true, "allows_multiple_answers": false, "allows_revoting": true, "members_only": false, "country_codes": [], "option_order": [], "id": "1",
        "question": {"@type": "formattedText", "text": "Куда идём?", "entities": []},
        "options": [
            {"@type": "pollOption", "id": "1", "recent_voter_ids": [], "addition_date": 0, "text": {"@type": "formattedText", "text": "Кино", "entities": []},
             "voter_count": 2, "vote_percentage": 67, "is_chosen": chosen, "is_being_chosen": false},
            {"@type": "pollOption", "id": "2", "recent_voter_ids": [], "addition_date": 0, "text": {"@type": "formattedText", "text": "Бар", "entities": []},
             "voter_count": 1, "vote_percentage": 33, "is_chosen": false, "is_being_chosen": false}],
        "total_voter_count": 3, "recent_voter_ids": [], "is_anonymous": true,
        "type": {"@type": "pollTypeRegular"},
        "open_period": 0, "close_date": 0, "is_closed": false}})
}

#[test]
fn poll_card_replaces_text_and_shows_results_after_voting() {
    let mut app = app();
    let mut m = message(1, 10, "");
    m["content"] = poll_content(false);
    app.session
        .panes
        .get_mut(&app.main_window)
        .unwrap()
        .switch_to(1);
    let _ = app.update(Msg::Pane(
        app.main_window,
        PaneMsg::HistoryLoaded(1, Ok(vec![serde_json::from_value(m).unwrap()])),
    ));
    let item = &pane(&app).messages[0];
    assert!(
        item.rich.is_empty(),
        "the card shows the question, not a text label"
    );
    let extra = item.extra.as_deref().unwrap();
    assert!(!extra.voted(), "choices are offered before voting");
    assert!(matches!(extra, extra::Extra::Poll { options, total: 3, .. } if options.len() == 2));

    td(
        &mut app,
        json!({"@type": "updateMessageContent", "chat_id": 1, "message_id": 10,
               "new_content": poll_content(true)}),
    );
    assert!(pane(&app).messages[0].extra.as_deref().unwrap().voted());
    assert_eq!(
        td::message_text(&serde_json::from_value(poll_content(false)).unwrap()),
        "[Опрос] Куда идём?"
    );
}

fn multi_poll_content() -> Value {
    json!({"@type": "messagePoll", "description": {"@type": "formattedText", "text": "", "entities": []}, "can_add_option": false, "poll": {"@type": "poll", "can_get_voters": false, "can_see_results": true, "allows_multiple_answers": true, "allows_revoting": true, "members_only": false, "country_codes": [], "option_order": [], "id": "1",
        "question": {"@type": "formattedText", "text": "Что берём?", "entities": []},
        "options": [
            {"@type": "pollOption", "id": "1", "recent_voter_ids": [], "addition_date": 0, "text": {"@type": "formattedText", "text": "хлеб", "entities": []},
             "voter_count": 0, "vote_percentage": 0, "is_chosen": false, "is_being_chosen": false},
            {"@type": "pollOption", "id": "2", "recent_voter_ids": [], "addition_date": 0, "text": {"@type": "formattedText", "text": "молоко", "entities": []},
             "voter_count": 0, "vote_percentage": 0, "is_chosen": false, "is_being_chosen": false},
            {"@type": "pollOption", "id": "3", "recent_voter_ids": [], "addition_date": 0, "text": {"@type": "formattedText", "text": "яйца", "entities": []},
             "voter_count": 0, "vote_percentage": 0, "is_chosen": false, "is_being_chosen": false}],
        "total_voter_count": 0, "recent_voter_ids": [], "is_anonymous": true,
        "type": {"@type": "pollTypeRegular"},
        "open_period": 0, "close_date": 0, "is_closed": false}})
}

#[test]
fn multi_answer_poll_accumulates_picks_before_sending_them_all() {
    let mut app = app();
    let mut m = message(1, 10, "");
    m["content"] = multi_poll_content();
    app.session
        .panes
        .get_mut(&app.main_window)
        .unwrap()
        .switch_to(1);
    let _ = app.update(Msg::Pane(
        app.main_window,
        PaneMsg::HistoryLoaded(1, Ok(vec![serde_json::from_value(m).unwrap()])),
    ));

    // Two picks accumulate instead of voting on the first click.
    pane_msg(&mut app, PaneMsg::ToggleVote(10, 0));
    pane_msg(&mut app, PaneMsg::ToggleVote(10, 2));
    assert_eq!(
        pane(&app).poll_selection.get(&10).cloned(),
        Some(std::collections::BTreeSet::from([0, 2])),
        "both picks are kept, not sent one by one"
    );
    assert!(
        !pane(&app).messages[0].extra.as_deref().unwrap().voted(),
        "no vote is sent until «Голосовать»"
    );

    // Unpicking one and picking another still just updates the set.
    pane_msg(&mut app, PaneMsg::ToggleVote(10, 0));
    pane_msg(&mut app, PaneMsg::ToggleVote(10, 1));
    assert_eq!(
        pane(&app).poll_selection.get(&10).cloned(),
        Some(std::collections::BTreeSet::from([1, 2]))
    );

    // «Голосовать» sends the whole picked set and clears it.
    let task = app.update(Msg::Pane(app.main_window, PaneMsg::SubmitVote(10)));
    assert_eq!(task.units(), 1, "one vote request, with both options");
    assert!(!pane(&app).poll_selection.contains_key(&10));
}

#[test]
fn archive_pins_and_mute_follow_tdlib() {
    let mut app = app();
    for id in [1, 2] {
        app.session
            .chats
            .insert(id, chat_item(&id.to_string(), false));
    }
    let position = |app: &mut App, chat_id: i64, list: &str, order: i64, pinned: bool| {
        td(
            app,
            json!({"@type": "updateChatPosition", "chat_id": chat_id,
                   "position": {"@type": "chatPosition", "list": {"@type": list},
                                "order": order.to_string(), "is_pinned": pinned}}),
        );
    };
    position(&mut app, 1, "chatListMain", 10, true);
    position(&mut app, 2, "chatListArchive", 5, false);
    assert!(app.session.chats[&1].pinned);
    assert_eq!(
        app.session
            .order
            .iter()
            .map(|&(_, id)| id)
            .collect::<Vec<_>>(),
        [1]
    );
    assert_eq!(
        app.session
            .archived
            .iter()
            .map(|&(_, id)| id)
            .collect::<Vec<_>>(),
        [2]
    );

    // Moving out of the archive: TDLib drops the archive position.
    position(&mut app, 2, "chatListArchive", 0, false);
    position(&mut app, 2, "chatListMain", 20, false);
    assert!(app.session.archived.is_empty());
    assert_eq!(
        app.session
            .order
            .iter()
            .map(|&(_, id)| id)
            .collect::<Vec<_>>(),
        [2, 1]
    );

    assert!(!app.session.chats[&1].muted());
    td(
        &mut app,
        json!({"@type": "updateChatNotificationSettings", "chat_id": 1,
               "notification_settings": {"@type": "chatNotificationSettings",
                   "use_default_mute_for": false, "mute_for": 2147483647,
                   "use_default_sound": true, "sound_id": "0", "use_default_show_preview": true,
                   "show_preview": true, "use_default_mute_stories": true, "mute_stories": false,
                   "use_default_story_sound": true, "story_sound_id": "0",
                   "use_default_show_story_poster": true, "show_story_poster": true,
                   "use_default_disable_pinned_message_notifications": true,
                   "disable_pinned_message_notifications": false,
                   "use_default_disable_mention_notifications": true,
                   "disable_mention_notifications": false}}),
    );
    assert!(app.session.chats[&1].muted());

    // The list view switches to the archive and back.
    let window = app.main_window;
    let _ = app.update(Msg::ShowArchive(window, true));
    assert!(pane(&app).list.archive);
    let _ = app.update(Msg::ChatMenu(window, Some(1)));
    let _ = app.update(Msg::ChatOp(window, 1, ChatOp::Close));
    assert_eq!(pane(&app).list.menu, None);
}

#[test]
fn archived_chat_found_in_main_search_offers_return_from_archive() {
    let mut app = sandbox::world();
    let window = app.main_window;
    let _ = app.update(Msg::ListQuery(window, "Старый".into()));
    let _ = app.update(Msg::ListChats(window, "Старый".into(), Ok(vec![6])));
    assert_eq!(app.displayed_chat_ids(window).unwrap(), [6]);
    let _ = app.update(Msg::ChatMenu(window, Some(6)));
    let messages = sandbox::click(&mut app, window, iced::Point::new(100.0, 246.0));
    assert!(
        messages
            .iter()
            .any(|msg| matches!(msg, Msg::ChatOp(id, 6, ChatOp::Archive(false)) if *id == window))
    );
}

#[test]
fn failed_chat_pin_load_keeps_message_archive_and_disables_both_pin_lists() {
    for bad_list in [false, true] {
        let mut app = app();
        let main = app.main_window;
        let other = WinId::unique();
        app.session.panes.insert(other, ChatPane::default());
        app.session.panes.get_mut(&other).unwrap().list.archive = true;
        for (id, order) in [(1, 10), (3, 30)] {
            sandbox::chat(&mut app, id, &format!("Чат {id}"), false, order);
        }
        for (id, order) in [(2, 20), (4, 40)] {
            sandbox::chat(&mut app, id, &format!("Чат {id}"), false, 0);
            app.set_archive_order(id, order);
        }

        let archive = Archive::in_memory();
        archive.set_local_chat_pin(false, 1, true).unwrap();
        archive.set_local_chat_pin(true, 2, true).unwrap();
        let message: tdlib_rs::types::Message =
            serde_json::from_value(message(1, 41, "закреп сообщения")).unwrap();
        archive.pin_local(&MsgItem::from(&message), 1).unwrap();
        archive.insert_malformed_chat_pin(bad_list);
        assert!(archive.local_chat_pins(bad_list).is_err());
        assert_eq!(
            archive.local_chat_pins(!bad_list).unwrap(),
            [if bad_list { 1 } else { 2 }]
        );
        app.attach_archive(archive);

        assert!(app.session.archive.is_some());
        assert!(app.session.local_chat_pins_failed);
        assert!(app.error.as_deref().unwrap().contains("закрепы:"));
        assert!(app.session.local_main.is_empty() && app.session.local_archive.is_empty());
        assert_eq!(app.displayed_chat_ids(main).unwrap(), [3, 1]);
        assert_eq!(app.displayed_chat_ids(other).unwrap(), [4, 2]);

        new_message(&mut app, 1, 50, "сохранено");
        delete(&mut app, 1, &[50], false);
        let archive = app.session.archive.as_ref().unwrap();
        assert_eq!(archive.deleted(1, 0, 100).unwrap()[0].text, "сохранено");
        assert_eq!(archive.local_pins(1).unwrap()[0].text, "закреп сообщения");
        for (window, chat_id, archived) in [(main, 3, false), (other, 4, true)] {
            let _ = app.update(Msg::ChatMenu(window, Some(chat_id)));
            let _ = app.update(Msg::ChatOp(window, chat_id, ChatOp::LocalPin(true)));
            assert!(app.error.as_deref().unwrap().contains("недоступны"));
            assert!(
                !app.session
                    .archive
                    .as_ref()
                    .unwrap()
                    .has_local_chat_pin(archived, chat_id)
            );
        }

        app.session = Session::new(0, main, 1);
        assert!(!app.session.local_chat_pins_failed);
        let fresh = Archive::in_memory();
        fresh.set_local_chat_pin(false, 1, true).unwrap();
        app.attach_archive(fresh);
        assert_eq!(app.session.local_main, [1]);
        sandbox::chat(&mut app, 3, "Чат 3", false, 30);
        let _ = app.update(Msg::ChatMenu(main, Some(3)));
        let _ = app.update(Msg::ChatOp(main, 3, ChatOp::LocalPin(true)));
        assert_eq!(app.session.local_main, [3, 1]);
        assert!(
            app.session
                .archive
                .as_ref()
                .unwrap()
                .has_local_chat_pin(false, 3)
        );
    }
}

#[test]
fn successful_chat_pin_load_restores_both_lists_order() {
    let mut app = app();
    let main = app.main_window;
    let other = WinId::unique();
    app.session.panes.insert(other, ChatPane::default());
    app.session.panes.get_mut(&other).unwrap().list.archive = true;
    for (id, order) in [(1, 10), (3, 30)] {
        sandbox::chat(&mut app, id, &format!("Чат {id}"), false, order);
    }
    for (id, order) in [(2, 20), (4, 40)] {
        sandbox::chat(&mut app, id, &format!("Чат {id}"), false, 0);
        app.set_archive_order(id, order);
    }
    let archive = Archive::in_memory();
    archive.set_local_chat_pin(false, 1, true).unwrap();
    archive.set_local_chat_pin(true, 2, true).unwrap();
    app.attach_archive(archive);
    assert!(!app.session.local_chat_pins_failed);
    assert_eq!(app.displayed_chat_ids(main).unwrap(), [1, 3]);
    assert_eq!(app.displayed_chat_ids(other).unwrap(), [2, 4]);
}

#[test]
fn local_chat_pin_rows_prioritize_local_then_telegram_without_ghosts_or_duplicates() {
    let mut app = app();
    for id in 1..=5 {
        sandbox::chat(&mut app, id, &format!("Чат {id}"), false, 100 - id);
    }
    let main = app.main_window;
    let other = WinId::unique();
    app.session.panes.insert(other, ChatPane::default());
    app.session.panes.get_mut(&other).unwrap().list.archive = true;
    assert_eq!(app.displayed_chat_ids(main).unwrap(), [1, 2, 3, 4, 5]);
    let _ = app.update(Msg::ChatMenu(main, Some(4)));
    let _ = app.update(Msg::ChatOp(main, 4, ChatOp::LocalPin(true)));
    let _ = app.update(Msg::ChatMenu(main, Some(2)));
    let _ = app.update(Msg::ChatOp(main, 2, ChatOp::LocalPin(true)));
    assert_eq!(app.displayed_chat_ids(main).unwrap(), [2, 4, 1, 3, 5]);
    assert_eq!(
        app.session
            .archive
            .as_ref()
            .unwrap()
            .local_chat_pins(false)
            .unwrap(),
        [2, 4]
    );

    // Telegram's authoritative position moves a locally pinned row into
    // the server-pin group exactly once, then back after a rollback.
    for pinned in [true, false] {
        td(
            &mut app,
            json!({"@type": "updateChatPosition", "chat_id": 4,
            "position": {"@type": "chatPosition", "list": {"@type": "chatListMain"},
                         "order": "96", "is_pinned": pinned}}),
        );
        assert_eq!(app.displayed_chat_ids(main).unwrap(), [2, 4, 1, 3, 5]);
    }
    td(
        &mut app,
        json!({"@type": "updateChatPosition", "chat_id": 3,
        "position": {"@type": "chatPosition", "list": {"@type": "chatListMain"},
                     "order": "97", "is_pinned": true}}),
    );
    assert_eq!(app.displayed_chat_ids(main).unwrap(), [2, 4, 3, 1, 5]);
    // A move hides the local main preference but does not erase it.
    app.set_order(4, 0);
    app.set_archive_order(4, 50);
    assert_eq!(app.displayed_chat_ids(main).unwrap(), [2, 3, 1, 5]);
    assert_eq!(app.displayed_chat_ids(other).unwrap(), [4]);
    let _ = app.update(Msg::ChatMenu(other, Some(4)));
    let _ = app.update(Msg::ChatOp(other, 4, ChatOp::LocalPin(true)));
    assert_eq!(
        app.session
            .archive
            .as_ref()
            .unwrap()
            .local_chat_pins(true)
            .unwrap(),
        [4]
    );
    app.set_archive_order(4, 0);
    app.set_order(4, 96);
    assert_eq!(app.displayed_chat_ids(main).unwrap(), [2, 4, 3, 1, 5]);
    assert!(app.displayed_chat_ids(other).unwrap().is_empty());

    app.session.panes.get_mut(&main).unwrap().list.query = "Чат 5".into();
    assert_eq!(app.displayed_chat_ids(main).unwrap(), [5]);
    app.session.panes.get_mut(&main).unwrap().list.query.clear();
    app.session = Session::new(0, main, 1);
    assert!(app.session.local_main.is_empty() && app.session.local_archive.is_empty());
}

#[test]
fn archive_local_pins_stay_ahead_of_server_updates_and_rollbacks() {
    let mut app = app();
    let window = app.main_window;
    app.session.panes.get_mut(&window).unwrap().list.archive = true;
    for (id, order) in [(1, 10), (2, 20), (3, 30)] {
        sandbox::chat(&mut app, id, &format!("Чат {id}"), false, 0);
        app.set_archive_order(id, order);
    }
    let _ = app.update(Msg::ChatMenu(window, Some(1)));
    let _ = app.update(Msg::ChatOp(window, 1, ChatOp::LocalPin(true)));
    assert_eq!(app.displayed_chat_ids(window).unwrap(), [1, 3, 2]);
    td(
        &mut app,
        json!({"@type": "updateChatPosition", "chat_id": 2,
        "position": {"@type": "chatPosition", "list": {"@type": "chatListArchive"},
                     "order": "20", "is_pinned": true}}),
    );
    assert_eq!(app.displayed_chat_ids(window).unwrap(), [1, 2, 3]);
    td(
        &mut app,
        json!({"@type": "updateChatPosition", "chat_id": 2,
        "position": {"@type": "chatPosition", "list": {"@type": "chatListArchive"},
                     "order": "20", "is_pinned": false}}),
    );
    assert_eq!(app.displayed_chat_ids(window).unwrap(), [1, 3, 2]);
    app.set_archive_order(1, 0);
    app.set_order(1, 40);
    assert_eq!(app.displayed_chat_ids(window).unwrap(), [3, 2]);
    app.set_order(1, 0);
    app.set_archive_order(1, 10);
    assert_eq!(app.displayed_chat_ids(window).unwrap(), [1, 3, 2]);
}

#[test]
fn local_chat_menu_rejects_outside_lists_and_failed_writes_without_fallback() {
    let mut app = app();
    sandbox::chat(&mut app, 1, "В списке", false, 20);
    sandbox::chat(&mut app, 2, "Только поиск", false, 0);
    let window = app.main_window;
    app.session.panes.get_mut(&window).unwrap().list.query = "Только".into();
    app.session
        .panes
        .get_mut(&window)
        .unwrap()
        .list
        .chats
        .push(2);
    assert_eq!(app.displayed_chat_ids(window).unwrap(), [2]);
    let _ = app.update(Msg::ChatMenu(window, Some(2)));
    let _ = app.update(Msg::ChatOp(window, 2, ChatOp::LocalPin(true)));
    assert!(app.session.local_main.is_empty());
    app.session
        .panes
        .get_mut(&window)
        .unwrap()
        .list
        .query
        .clear();
    app.session.panes.get_mut(&window).unwrap().list.folder = Some(7);
    let _ = app.update(Msg::ChatMenu(window, Some(1)));
    let _ = app.update(Msg::ChatOp(window, 1, ChatOp::LocalPin(true)));
    assert!(app.session.local_main.is_empty());
    app.session.panes.get_mut(&window).unwrap().list.folder = None;

    let _ = app.update(Msg::ChatMenu(window, Some(1)));
    let server = app.update(Msg::ChatOp(window, 1, ChatOp::Pin(true)));
    assert_eq!(
        server.units(),
        1,
        "Telegram request is not replaced with a local pin"
    );
    let _ = app.update(Msg::Done(Err("PREMIUM_REQUIRED".into())));
    assert!(app.error.as_deref().unwrap().contains("PREMIUM_REQUIRED"));
    assert_eq!(app.displayed_chat_ids(window).unwrap(), [1]);
    assert!(app.session.local_main.is_empty());

    let _ = app.update(Msg::ChatMenu(window, Some(1)));
    let _ = app.update(Msg::ChatOp(window, 1, ChatOp::LocalPin(true)));
    assert_eq!(app.session.local_main, [1]);
    app.session.archive.as_ref().unwrap().make_read_only();
    let _ = app.update(Msg::ChatMenu(window, Some(1)));
    let _ = app.update(Msg::ChatOp(window, 1, ChatOp::LocalPin(false)));
    assert!(app.error.as_deref().unwrap().contains("локальный закреп"));
    assert_eq!(app.session.local_main, [1]);
    assert_eq!(
        app.session
            .archive
            .as_ref()
            .unwrap()
            .local_chat_pins(false)
            .unwrap(),
        [1]
    );
    sandbox::chat(&mut app, 3, "Новое", false, 10);
    let _ = app.update(Msg::ChatMenu(window, Some(3)));
    let _ = app.update(Msg::ChatOp(window, 3, ChatOp::LocalPin(true)));
    assert!(app.error.as_deref().unwrap().contains("локальный закреп"));
    assert_eq!(app.displayed_chat_ids(window).unwrap(), [1, 3]);
    assert_eq!(app.session.local_main, [1]);

    app.session.archive = None; // The same state as an in-progress rekey.
    let _ = app.update(Msg::ChatMenu(window, Some(3)));
    let _ = app.update(Msg::ChatOp(window, 3, ChatOp::LocalPin(true)));
    assert!(app.error.as_deref().unwrap().contains("недоступны"));
    assert_eq!(app.displayed_chat_ids(window).unwrap(), [1, 3]);
    assert_eq!(app.session.local_main, [1]);
}

fn archive_values(
    auto: bool,
    unmuted: bool,
    folders: bool,
) -> tdlib_rs::types::ArchiveChatListSettings {
    tdlib_rs::types::ArchiveChatListSettings {
        archive_and_mute_new_chats_from_unknown_users: auto,
        keep_unmuted_chats_archived: unmuted,
        keep_chats_from_folders_archived: folders,
    }
}

fn archive_panel(
    app: &mut App,
    window: WinId,
    settings: tdlib_rs::types::ArchiveChatListSettings,
    capability: Result<bool, String>,
) -> u64 {
    let _ = app.update(Msg::ShowArchive(window, true));
    let _ = app.update(Msg::ToggleArchiveSettings(window, true));
    let request = app.session.panes[&window]
        .list
        .archive_settings
        .as_ref()
        .unwrap()
        .request;
    let client = app.session.client_id;
    let _ = app.update(Msg::ArchiveSettingsLoaded(
        window,
        client,
        request,
        Ok(settings),
    ));
    let _ = app.update(Msg::ArchiveCapabilityLoaded(
        window, client, request, capability,
    ));
    request
}

#[test]
fn empty_archive_moves_between_sidebar_and_menu_without_changing_chat_lists() {
    let mut app = app();
    let main = app.main_window;
    let client = app.session.client_id;
    app.session.my_id = Some(42);
    app.settings.accounts.push(Account {
        slot: app.session.slot,
        user_id: Some(42),
        name: "Тест".into(),
        ..Default::default()
    });
    assert!(app.session.archived.is_empty());
    let entry = sandbox::click(&mut app, main, iced::Point::new(120.0, 125.0));
    assert!(
        entry
            .iter()
            .any(|m| matches!(m, Msg::ShowArchive(w, true) if *w == main))
    );
    let _ = app.update(Msg::SetArchiveCollapsed(main, client, true));
    assert!(app.archive_collapsed());
    assert!(app.session.archived.is_empty() && app.session.order.is_empty());
    let _ = app.update(Msg::ShowArchive(main, false));
    let _ = app.update(Msg::ToggleAccounts);
    let open = sandbox::click(&mut app, main, iced::Point::new(70.0, 85.0));
    assert!(
        open.iter()
            .any(|m| matches!(m, Msg::OpenArchiveFromMenu(c) if *c == client))
    );
    assert!(pane(&app).list.archive);
    assert!(!app.session.accounts_open);
    let _ = app.update(Msg::SetArchiveCollapsed(main, client, false));
    assert!(!app.archive_collapsed());
    let _ = app.update(Msg::ShowArchive(main, false));
    let collapse = sandbox::click(&mut app, main, iced::Point::new(252.0, 125.0));
    assert!(
        collapse.iter().any(
            |m| matches!(m, Msg::SetArchiveCollapsed(w, c, true) if *w == main && *c == client)
        )
    );
    let _ = app.update(Msg::ToggleAccounts);
    let restore = sandbox::click(&mut app, main, iced::Point::new(70.0, 115.0));
    assert!(
        restore.iter().any(
            |m| matches!(m, Msg::SetArchiveCollapsed(w, c, false) if *w == main && *c == client)
        )
    );
    assert!(!app.archive_collapsed());
    assert!(app.session.archived.is_empty() && app.session.order.is_empty());
    let entry = sandbox::click(&mut app, main, iced::Point::new(120.0, 125.0));
    assert!(
        entry
            .iter()
            .any(|m| matches!(m, Msg::ShowArchive(w, true) if *w == main))
    );
}

#[test]
fn archive_placement_is_account_scoped_persisted_and_rejects_stale_actions() {
    let mut app = app();
    let main = app.main_window;
    let client = app.session.client_id;
    app.session.my_id = Some(42);
    app.session.chats.insert(9, chat_item("В архиве", false));
    app.set_archive_order(9, 50);
    let archived = app.session.archived.clone();
    let _ = app.update(Msg::SetArchiveCollapsed(main, client, true));
    assert!(app.archive_collapsed());
    assert_eq!(app.session.archived, archived);
    let _ = app.update(Msg::SetArchiveCollapsed(WinId::unique(), client, false));
    assert!(
        app.archive_collapsed(),
        "a closed window cannot change placement"
    );
    assert!(
        Settings::load(&app.settings_path)
            .unwrap()
            .collapsed_archives[&42]
    );

    app.session.my_id = Some(43);
    assert!(!app.archive_collapsed());
    let _ = app.update(Msg::SetArchiveCollapsed(main, client + 1, true));
    assert!(!app.archive_collapsed());
    let _ = app.update(Msg::OpenArchiveFromMenu(client));
    assert!(!pane(&app).list.archive);
    let _ = app.update(Msg::SetArchiveCollapsed(main, client, true));
    assert!(app.archive_collapsed());
    let _ = app.update(Msg::ToggleAccounts);
    let _ = app.update(Msg::SetArchiveCollapsed(main, client, false));
    assert!(!app.archive_collapsed());
    assert!(app.settings.collapsed_archives.contains_key(&42));
    assert!(!app.settings.collapsed_archives.contains_key(&43));
    app.session.my_id = Some(42);
    assert!(app.archive_collapsed());
    let _ = app.update(Msg::OpenArchiveFromMenu(client));
    assert!(
        !pane(&app).list.archive,
        "a closed menu cannot open the archive"
    );
    let _ = app.update(Msg::ShowArchive(main, true));
    assert!(!app.session.accounts_open);
    assert_eq!(app.session.archived, archived);
    assert_eq!(
        Settings::load(&app.settings_path)
            .unwrap()
            .collapsed_archives,
        app.settings.collapsed_archives
    );
}

#[test]
fn empty_archive_has_a_clickable_entry_and_settings_even_with_search() {
    let mut app = app();
    let window = app.main_window;
    assert!(app.session.archived.is_empty());
    let messages = sandbox::click(&mut app, window, iced::Point::new(120.0, 125.0));
    assert!(
        messages
            .iter()
            .any(|msg| matches!(msg, Msg::ShowArchive(id, true) if *id == window))
    );
    assert!(pane(&app).list.archive);
    let messages = sandbox::click(&mut app, window, iced::Point::new(120.0, 88.0));
    assert!(
        messages
            .iter()
            .any(|msg| matches!(msg, Msg::ToggleArchiveSettings(id, true) if *id == window))
    );
    assert!(pane(&app).list.archive_settings.is_some());
    let _ = app.update(Msg::ToggleArchiveSettings(window, false));
    let _ = app.update(Msg::ListQuery(window, "not found".into()));
    assert_eq!(app.filtered_chats(window), Some(Vec::new()));
    let messages = sandbox::click(&mut app, window, iced::Point::new(120.0, 88.0));
    assert!(
        messages
            .iter()
            .any(|msg| matches!(msg, Msg::ToggleArchiveSettings(id, true) if *id == window))
    );
    assert!(pane(&app).list.archive_settings.is_some());
}

#[test]
fn empty_archive_opens_distinct_window_from_entry_and_archive_page() {
    let mut app = app();
    let main = app.main_window;
    let client = app.session.client_id;
    assert!(app.session.archived.is_empty());
    let messages = sandbox::click(&mut app, main, iced::Point::new(280.0, 125.0));
    assert_eq!(
        messages
            .iter()
            .filter(|m| matches!(m, Msg::OpenArchiveWindow(id, origin) if *id == main && *origin == client))
            .count(),
        1
    );
    let first = *app.session.panes.keys().find(|&&w| w != main).unwrap();
    assert!(app.session.panes[&first].list.archive);
    assert_eq!(app.session.panes[&first].chat_id, None);
    assert!(!app.session.panes[&main].list.archive);
    assert_eq!(app.session.client_id, client);

    let _ = app.update(Msg::ShowArchive(main, true));
    let messages = sandbox::click(&mut app, main, iced::Point::new(120.0, 125.0));
    assert_eq!(
        messages
            .iter()
            .filter(|m| matches!(m, Msg::OpenArchiveWindow(id, origin) if *id == main && *origin == client))
            .count(),
        1
    );
    let second = *app
        .session
        .panes
        .keys()
        .find(|&&w| w != main && w != first)
        .unwrap();
    assert!(app.session.panes[&second].list.archive);
    assert!(app.session.panes[&main].list.archive);
    assert_eq!(app.session.panes.len(), 3);
    assert_eq!(app.session.client_id, client);

    let _ = app.update(Msg::ShowArchive(first, false));
    assert!(!app.session.panes[&first].list.archive);
    assert!(app.session.panes[&second].list.archive);
    let _ = app.update(Msg::WindowClosed(first));
    assert!(!app.session.panes.contains_key(&first));
    let _ = app.update(Msg::ShowArchive(first, true));
    assert!(app.session.panes[&main].list.archive);
    assert!(app.session.panes[&second].list.archive);
}

#[test]
fn collapsed_archive_remains_reachable_after_back_in_secondary_window() {
    let mut app = app();
    let main = app.main_window;
    let client = app.session.client_id;
    app.session.my_id = Some(42);
    let _ = app.update(Msg::OpenArchiveWindow(main, client));
    let secondary = *app
        .session
        .panes
        .keys()
        .find(|&&window| window != main)
        .unwrap();
    let _ = app.update(Msg::SetArchiveCollapsed(secondary, client, true));
    let _ = app.update(Msg::ShowArchive(secondary, false));
    let entry = sandbox::click(&mut app, secondary, iced::Point::new(80.0, 90.0));
    assert!(
        entry
            .iter()
            .any(|msg| matches!(msg, Msg::ShowArchive(window, true) if *window == secondary))
    );
    assert!(app.session.panes[&secondary].list.archive);
    assert!(!app.session.panes[&main].list.archive);
}

#[test]
fn populated_archive_windows_keep_selection_search_settings_and_read_scope_separate() {
    let mut app = app();
    let main = app.main_window;
    for (id, title) in [(1, "Первый"), (2, "Второй")] {
        app.session.chats.insert(id, chat_item(title, false));
        app.set_archive_order(id, id);
    }
    let _ = app.update(Msg::ListQuery(main, "main".into()));
    let _ = app.update(Msg::OpenArchiveWindow(main, app.session.client_id));
    let first = *app.session.panes.keys().find(|&&w| w != main).unwrap();
    let _ = app.update(Msg::OpenArchiveWindow(main, app.session.client_id));
    let second = *app
        .session
        .panes
        .keys()
        .find(|&&w| w != main && w != first)
        .unwrap();
    assert_eq!(app.session.archived.len(), 2);
    assert!(app.session.panes[&first].list.archive);
    assert!(app.session.panes[&second].list.archive);
    assert_eq!(app.session.panes[&main].list.query, "main");
    assert!(!app.session.panes[&main].list.archive);

    let _ = app.update(Msg::ListQuery(first, "Первый".into()));
    let _ = app.update(Msg::ListChats(first, "Первый".into(), Ok(vec![1])));
    assert_eq!(app.filtered_chats(first), Some(vec![1]));
    assert_eq!(app.session.panes[&second].list.query, "");
    let _ = app.update(Msg::Pane(first, PaneMsg::SelectChat(1)));
    assert_eq!(app.session.panes[&first].chat_id, Some(1));
    assert_eq!(app.session.panes[&second].chat_id, None);
    assert_eq!(app.session.panes[&main].chat_id, None);
    let _ = app.update(Msg::ToggleArchiveSettings(second, true));
    assert!(app.session.panes[&second].list.archive_settings.is_some());
    assert!(app.session.panes[&first].list.archive_settings.is_none());
    let client = app.session.client_id;
    let _ = app.update(Msg::AskReadList(first, client));
    assert_eq!(
        app.session.panes[&first]
            .list
            .confirm_read
            .as_ref()
            .unwrap()
            .1,
        ChatList::Archive
    );
    assert!(app.session.panes[&second].list.confirm_read.is_none());
    assert!(app.session.panes[&main].list.confirm_read.is_none());
    let _ = app.update(Msg::WindowClosed(first));
    assert_eq!(app.session.panes[&main].list.query, "main");
    assert!(app.session.panes[&second].list.archive_settings.is_some());
}

#[test]
fn archive_window_closes_safely_during_account_switch_and_logout() {
    let mut app = app();
    let main = app.main_window;
    let client = app.session.client_id;
    let _ = app.update(Msg::OpenArchiveWindow(main, client));
    let archive = *app.session.panes.keys().find(|&&w| w != main).unwrap();
    let _ = app.switch_account(3);
    assert_eq!(app.session.leave.as_ref().unwrap().to, 3);
    assert_eq!(app.session.client_id, client);
    app.session.busy = true;
    let _ = app.update(Msg::ArchiveWindowLoaded(
        archive,
        client,
        Err("old load".into()),
    ));
    assert!(app.session.busy);
    assert!(app.error.is_none());
    let _ = app.update(Msg::OpenArchiveWindow(main, client));
    assert_eq!(app.session.panes.len(), 2);
    let _ = app.update(Msg::WindowClosed(archive));
    let _ = app.update(Msg::ListQuery(archive, "late".into()));
    let _ = app.update(Msg::ShowArchive(archive, true));
    let _ = app.update(Msg::WindowClosed(archive));
    assert_eq!(app.session.panes.len(), 1);
    assert!(!app.session.panes[&main].list.archive);
    assert_eq!(app.session.panes[&main].list.query, "");
    app.session.leave = None;
    let _ = app.update(Msg::OpenArchiveWindow(main, client));
    let archive = *app.session.panes.keys().find(|&&w| w != main).unwrap();
    let _ = app.update(Msg::ConfirmLogOut(true));
    app.session.busy = true;
    let _ = app.update(Msg::ArchiveWindowLoaded(
        archive,
        client,
        Err("old logout load".into()),
    ));
    assert!(app.session.busy);
    assert!(app.error.is_none());
    let _ = app.update(Msg::OpenArchiveWindow(main, client));
    assert_eq!(app.session.panes.len(), 2);
    let _ = app.update(Msg::WindowClosed(archive));
    assert_eq!(app.session.panes.len(), 1);
    assert_eq!(app.session.client_id, client);
}

#[test]
fn queued_archive_click_cannot_open_window_after_client_replacement_or_source_close() {
    let mut app = app();
    let main = app.main_window;
    let old_client = app.session.client_id;
    let messages = sandbox::click(&mut app, main, iced::Point::new(280.0, 125.0));
    let queued = messages
        .into_iter()
        .find(|msg| {
            matches!(msg, Msg::OpenArchiveWindow(id, client) if *id == main && *client == old_client)
        })
        .unwrap();
    app.session = Session::new(old_client + 1, main, 1);
    app.session.auth = Auth::Ready;
    let _ = app.update(queued);
    assert_eq!(app.session.panes.len(), 1);
    assert!(app.session.archived.is_empty());

    let client = app.session.client_id;
    let _ = app.update(Msg::OpenArchiveWindow(main, client));
    let archive = *app.session.panes.keys().find(|&&w| w != main).unwrap();
    let _ = app.update(Msg::WindowClosed(archive));
    let _ = app.update(Msg::OpenArchiveWindow(archive, client));
    assert_eq!(app.session.panes.len(), 1);
}

#[test]
fn archive_load_reply_from_replaced_client_does_not_touch_new_session() {
    let mut app = app();
    let main = app.main_window;
    let old_client = app.session.client_id;
    let _ = app.update(Msg::OpenArchiveWindow(main, old_client));
    let archive = *app.session.panes.keys().find(|&&w| w != main).unwrap();
    app.session = Session::new(old_client + 1, main, 1);
    app.session.auth = Auth::Ready;
    app.session.busy = true;
    app.error = Some("ошибка текущего аккаунта".into());
    let _ = app.update(Msg::ArchiveWindowLoaded(
        archive,
        old_client,
        Err("старая ошибка".into()),
    ));
    let _ = app.update(Msg::ArchiveWindowLoaded(archive, old_client, Ok(())));
    // Even when a current pane exists, the old client cannot finish its load.
    let _ = app.update(Msg::ArchiveWindowLoaded(
        main,
        old_client,
        Err("старая ошибка".into()),
    ));
    assert!(app.session.busy);
    assert_eq!(app.error.as_deref(), Some("ошибка текущего аккаунта"));
    assert_eq!(app.session.panes.len(), 1);
}

#[test]
fn archive_load_reply_reports_active_errors_but_ignores_closed_window() {
    let mut app = app();
    let main = app.main_window;
    let client = app.session.client_id;
    let _ = app.update(Msg::OpenArchiveWindow(main, client));
    let archive = *app.session.panes.keys().find(|&&w| w != main).unwrap();
    app.session.busy = true;
    let _ = app.update(Msg::ArchiveWindowLoaded(
        archive,
        client,
        Err("ошибка загрузки".into()),
    ));
    assert!(!app.session.busy);
    assert_eq!(app.error.as_deref(), Some("ошибка загрузки"));
    assert!(app.session.archived.is_empty());
    app.session.busy = true;
    let _ = app.update(Msg::ArchiveWindowLoaded(archive, client, Ok(())));
    assert!(!app.session.busy);
    assert_eq!(app.error.as_deref(), Some("ошибка загрузки"));
    app.error = None;
    app.session.busy = true;
    let _ = app.update(Msg::WindowClosed(archive));
    let _ = app.update(Msg::ArchiveWindowLoaded(
        archive,
        client,
        Err("поздняя ошибка".into()),
    ));
    assert!(app.session.busy);
    assert_eq!(app.error, None);
}

#[test]
fn main_settings_take_precedence_and_close_archive_panel_on_entry() {
    let mut app = app();
    let window = app.main_window;
    archive_panel(
        &mut app,
        window,
        archive_values(false, true, false),
        Ok(true),
    );
    app.session.settings_open = true;
    let messages = sandbox::click(&mut app, window, iced::Point::new(945.0, 25.0));
    assert!(messages.iter().any(|msg| matches!(msg, Msg::CloseSettings)));
    assert!(!app.session.settings_open);
    assert!(pane(&app).list.archive_settings.is_some());

    let messages = sandbox::click(&mut app, window, iced::Point::new(190.0, 20.0));
    assert!(messages.iter().any(|msg| matches!(msg, Msg::OpenSettings)));
    assert!(app.session.settings_open);
    assert!(pane(&app).list.archive_settings.is_none());
    let _ = app.update(Msg::CloseSettings);
    assert!(pane(&app).list.archive);
    let _ = app.update(Msg::ToggleArchiveSettings(window, true));
    assert!(pane(&app).list.archive_settings.is_some());
    // The archive action remains in the sidebar while main Settings is open.
    app.session
        .panes
        .get_mut(&window)
        .unwrap()
        .list
        .archive_settings = None;
    app.session.settings_open = true;
    let messages = sandbox::click(&mut app, window, iced::Point::new(120.0, 88.0));
    assert!(
        messages
            .iter()
            .any(|msg| matches!(msg, Msg::ToggleArchiveSettings(id, true) if *id == window))
    );
    assert!(pane(&app).list.archive_settings.is_some());
    assert!(!app.session.settings_open);
}

#[test]
fn readback_invalidates_older_reads_in_every_open_window() {
    for old_reply_before_readback in [false, true] {
        let mut app = app();
        let first = app.main_window;
        let second = WinId::unique();
        app.session.panes.insert(second, ChatPane::default());
        let old = archive_values(false, false, false);
        archive_panel(&mut app, first, old.clone(), Ok(true));
        let _ = app.update(Msg::ShowArchive(second, true));
        let _ = app.update(Msg::ToggleArchiveSettings(second, true));
        let initial_request = app.session.panes[&second]
            .list
            .archive_settings
            .as_ref()
            .unwrap()
            .request;
        let _ = app.update(Msg::RetryArchiveSettings(second));
        let client = app.session.client_id;
        let old_request = app.session.panes[&second]
            .list
            .archive_settings
            .as_ref()
            .unwrap()
            .request;
        assert_ne!(old_request, initial_request);
        let _ = app.update(Msg::ArchiveCapabilityLoaded(
            second,
            client,
            old_request,
            Ok(true),
        ));
        let _ = app.update(Msg::ChangeArchiveFlag(
            first,
            td::ArchiveFlag::KeepUnmuted,
            true,
        ));
        let token = app.session.archive_write.unwrap().1;
        let _ = app.update(Msg::ArchiveFetched(
            first,
            client,
            token,
            td::ArchiveFlag::KeepUnmuted,
            true,
            Ok(old.clone()),
        ));
        if !old_reply_before_readback {
            let _ = app.update(Msg::ArchiveSettingsLoaded(
                second,
                client,
                old_request,
                Ok(old.clone()),
            ));
        }
        let panel = app.session.panes[&second]
            .list
            .archive_settings
            .as_ref()
            .unwrap();
        assert!(panel.loading);
        assert!(
            panel.settings.is_none(),
            "pre-write read must not publish stale values"
        );
        let _ = app.update(Msg::ArchiveSaved(first, client, token, Ok(())));
        if old_reply_before_readback {
            let _ = app.update(Msg::ArchiveSettingsLoaded(
                second,
                client,
                old_request,
                Ok(old.clone()),
            ));
        }
        let panel = app.session.panes[&second]
            .list
            .archive_settings
            .as_ref()
            .unwrap();
        assert!(panel.loading);
        assert!(
            panel.settings.is_none(),
            "saved write still awaits authoritative readback"
        );
        let confirmed = archive_values(false, true, false);
        let _ = app.update(Msg::ArchiveReadback(
            first,
            client,
            token,
            Ok(confirmed.clone()),
        ));
        let panel = app.session.panes[&second]
            .list
            .archive_settings
            .as_ref()
            .unwrap();
        assert_ne!(panel.request, old_request);
        assert_eq!(panel.settings.as_ref(), Some(&confirmed));
        let _ = app.update(Msg::ArchiveSettingsLoaded(
            second,
            client,
            old_request,
            Ok(old),
        ));
        assert_eq!(
            app.session.panes[&second]
                .list
                .archive_settings
                .as_ref()
                .unwrap()
                .settings
                .as_ref(),
            Some(&confirmed)
        );
    }
}

#[test]
fn failed_archive_write_releases_suppressed_refresh_for_retry() {
    let mut app = app();
    let first = app.main_window;
    let second = WinId::unique();
    app.session.panes.insert(second, ChatPane::default());
    let old = archive_values(false, false, false);
    archive_panel(&mut app, first, old.clone(), Ok(true));
    let _ = app.update(Msg::ShowArchive(second, true));
    let _ = app.update(Msg::ToggleArchiveSettings(second, true));
    let _ = app.update(Msg::RetryArchiveSettings(second));
    let client = app.session.client_id;
    let stale_request = app.session.panes[&second]
        .list
        .archive_settings
        .as_ref()
        .unwrap()
        .request;
    let _ = app.update(Msg::ChangeArchiveFlag(
        first,
        td::ArchiveFlag::KeepUnmuted,
        true,
    ));
    let token = app.session.archive_write.unwrap().1;
    let _ = app.update(Msg::ArchiveFetched(
        first,
        client,
        token,
        td::ArchiveFlag::KeepUnmuted,
        true,
        Ok(old.clone()),
    ));
    let _ = app.update(Msg::ArchiveSettingsLoaded(
        second,
        client,
        stale_request,
        Ok(old.clone()),
    ));
    let _ = app.update(Msg::ArchiveSaved(
        first,
        client,
        token,
        Err("WRITE_FAILED".into()),
    ));
    let panel = app.session.panes[&second]
        .list
        .archive_settings
        .as_ref()
        .unwrap();
    assert!(
        !panel.loading,
        "suppressed refresh must not leave retry disabled"
    );
    assert!(panel.settings.is_none());
    assert_ne!(panel.request, stale_request);
    assert!(app.session.archive_write.is_none());

    let _ = app.update(Msg::ArchiveSettingsLoaded(
        second,
        client,
        stale_request,
        Ok(old),
    ));
    assert!(
        app.session.panes[&second]
            .list
            .archive_settings
            .as_ref()
            .unwrap()
            .settings
            .is_none()
    );
    let _ = app.update(Msg::RetryArchiveSettings(second));
    let retry = app.session.panes[&second]
        .list
        .archive_settings
        .as_ref()
        .unwrap()
        .request;
    let fresh = archive_values(true, false, true);
    let _ = app.update(Msg::ArchiveSettingsLoaded(
        second,
        client,
        retry,
        Ok(fresh.clone()),
    ));
    let _ = app.update(Msg::ArchiveCapabilityLoaded(
        second,
        client,
        retry,
        Ok(true),
    ));
    let panel = app.session.panes[&second]
        .list
        .archive_settings
        .as_ref()
        .unwrap();
    assert!(!panel.loading);
    assert_eq!(panel.settings.as_ref(), Some(&fresh));
}

#[test]
fn archive_settings_retry_and_capability_do_not_invent_server_values() {
    let mut app = app();
    let window = app.main_window;
    let _ = app.update(Msg::ShowArchive(window, true));
    let _ = app.update(Msg::ToggleArchiveSettings(window, true));
    let client = app.session.client_id;
    let first = pane(&app).list.archive_settings.as_ref().unwrap().request;
    assert!(
        pane(&app)
            .list
            .archive_settings
            .as_ref()
            .unwrap()
            .settings
            .is_none()
    );
    let _ = app.update(Msg::ArchiveSettingsLoaded(
        window,
        client,
        first,
        Err("NETWORK".into()),
    ));
    let _ = app.update(Msg::ArchiveCapabilityLoaded(
        window,
        client,
        first,
        Err("OPTION_UNKNOWN".into()),
    ));
    let panel = pane(&app).list.archive_settings.as_ref().unwrap();
    assert!(panel.settings.is_none());
    assert!(panel.error.as_ref().unwrap().contains("NETWORK"));
    let _ = app.update(Msg::ChangeArchiveFlag(
        window,
        td::ArchiveFlag::KeepUnmuted,
        true,
    ));
    assert!(app.session.archive_write.is_none());

    let _ = app.update(Msg::RetryArchiveSettings(window));
    let retry = pane(&app).list.archive_settings.as_ref().unwrap().request;
    assert_ne!(retry, first);
    let _ = app.update(Msg::ArchiveSettingsLoaded(
        window,
        client,
        first,
        Ok(archive_values(true, true, true)),
    ));
    assert!(
        pane(&app)
            .list
            .archive_settings
            .as_ref()
            .unwrap()
            .settings
            .is_none()
    );
    let actual = archive_values(false, false, true);
    let _ = app.update(Msg::ArchiveSettingsLoaded(
        window,
        client,
        retry,
        Ok(actual.clone()),
    ));
    let _ = app.update(Msg::ArchiveCapabilityLoaded(
        window,
        client,
        retry,
        Ok(false),
    ));
    let _ = app.update(Msg::ChangeArchiveFlag(
        window,
        td::ArchiveFlag::ArchiveUnknown,
        true,
    ));
    assert!(
        app.session.archive_write.is_none(),
        "unavailable flag cannot be written"
    );
    assert_eq!(
        pane(&app)
            .list
            .archive_settings
            .as_ref()
            .unwrap()
            .settings
            .as_ref(),
        Some(&actual)
    );
    let _ = app.update(Msg::ChangeArchiveFlag(
        window,
        td::ArchiveFlag::KeepUnmuted,
        true,
    ));
    let action = app.session.archive_write.unwrap().1;
    let _ = app.update(Msg::ArchiveFetched(
        window,
        client,
        action,
        td::ArchiveFlag::KeepUnmuted,
        true,
        Err("READ_FAILED".into()),
    ));
    let panel = pane(&app).list.archive_settings.as_ref().unwrap();
    assert_eq!(panel.settings.as_ref(), Some(&actual));
    assert!(panel.error.as_ref().unwrap().contains("READ_FAILED"));
    assert!(app.session.archive_write.is_none());
}

#[test]
fn archive_writes_are_serialized_and_read_back_without_optimism() {
    let mut app = app();
    let first = app.main_window;
    let second = WinId::unique();
    app.session.panes.insert(second, ChatPane::default());
    let shown = archive_values(false, false, true);
    archive_panel(&mut app, first, shown.clone(), Ok(true));
    archive_panel(&mut app, second, shown.clone(), Ok(true));
    let client = app.session.client_id;
    let _task = app.update(Msg::ChangeArchiveFlag(
        first,
        td::ArchiveFlag::KeepUnmuted,
        true,
    ));
    let token = app.session.archive_write.unwrap().1;
    let _ = app.update(Msg::ChangeArchiveFlag(
        second,
        td::ArchiveFlag::KeepFolderChats,
        false,
    ));
    assert_eq!(app.session.archive_write, Some((first, token)));

    // A concurrent server change must survive: only the selected field is changed.
    let latest = archive_values(true, false, false);
    assert_eq!(
        td::archive_flag(latest.clone(), td::ArchiveFlag::KeepUnmuted, true),
        archive_values(true, true, false)
    );
    let _write = app.update(Msg::ArchiveFetched(
        first,
        client,
        token,
        td::ArchiveFlag::KeepUnmuted,
        true,
        Ok(latest),
    ));
    assert_eq!(
        pane(&app)
            .list
            .archive_settings
            .as_ref()
            .unwrap()
            .settings
            .as_ref(),
        Some(&shown)
    );
    let _readback = app.update(Msg::ArchiveSaved(first, client, token, Ok(())));
    assert_eq!(
        app.session.archive_write,
        Some((first, token)),
        "held through readback"
    );
    assert_eq!(
        pane(&app)
            .list
            .archive_settings
            .as_ref()
            .unwrap()
            .settings
            .as_ref(),
        Some(&shown)
    );
    let confirmed = archive_values(true, true, false);
    let _ = app.update(Msg::ArchiveReadback(
        first,
        client,
        token,
        Ok(confirmed.clone()),
    ));
    assert_eq!(
        pane(&app)
            .list
            .archive_settings
            .as_ref()
            .unwrap()
            .settings
            .as_ref(),
        Some(&confirmed)
    );
    assert!(app.session.archive_write.is_none());
    assert_eq!(
        app.session.panes[&second]
            .list
            .archive_settings
            .as_ref()
            .unwrap()
            .settings
            .as_ref(),
        Some(&confirmed)
    );

    let _ = app.update(Msg::ChangeArchiveFlag(
        second,
        td::ArchiveFlag::KeepFolderChats,
        false,
    ));
    let token = app.session.archive_write.unwrap().1;
    let _ = app.update(Msg::ArchiveFetched(
        second,
        client,
        token,
        td::ArchiveFlag::KeepFolderChats,
        false,
        Ok(confirmed.clone()),
    ));
    let _ = app.update(Msg::ArchiveSaved(
        second,
        client,
        token,
        Err("WRITE_FAILED".into()),
    ));
    assert!(app.session.archive_write.is_none());
    let panel = app.session.panes[&second]
        .list
        .archive_settings
        .as_ref()
        .unwrap();
    assert_eq!(panel.settings.as_ref(), Some(&confirmed));
    assert!(panel.error.as_ref().unwrap().contains("WRITE_FAILED"));

    let _ = app.update(Msg::ChangeArchiveFlag(
        second,
        td::ArchiveFlag::KeepUnmuted,
        true,
    ));
    let token = app.session.archive_write.unwrap().1;
    let _ = app.update(Msg::ArchiveFetched(
        second,
        client,
        token,
        td::ArchiveFlag::KeepUnmuted,
        true,
        Ok(archive_values(true, false, false)),
    ));
    let _ = app.update(Msg::ArchiveSaved(second, client, token, Ok(())));
    let _ = app.update(Msg::ArchiveReadback(
        second,
        client,
        token,
        Err("VERIFY_FAILED".into()),
    ));
    let panel = app.session.panes[&second]
        .list
        .archive_settings
        .as_ref()
        .unwrap();
    assert!(panel.settings.is_none());
    assert!(panel.error.as_ref().unwrap().contains("VERIFY_FAILED"));
    assert!(!panel.saving);
}

#[test]
fn archive_cancel_before_save_and_stale_account_window_replies_are_ignored() {
    let mut app = app();
    let first = app.main_window;
    let second = WinId::unique();
    app.session.panes.insert(second, ChatPane::default());
    let shown = archive_values(false, false, false);
    archive_panel(&mut app, first, shown.clone(), Ok(true));
    archive_panel(&mut app, second, shown.clone(), Ok(true));
    let client = app.session.client_id;
    let _read = app.update(Msg::ChangeArchiveFlag(
        second,
        td::ArchiveFlag::ArchiveUnknown,
        true,
    ));
    let token = app.session.archive_write.unwrap().1;
    let _ = app.update(Msg::WindowClosed(second));
    let _ = app.update(Msg::ArchiveFetched(
        second,
        client,
        token,
        td::ArchiveFlag::ArchiveUnknown,
        true,
        Ok(shown.clone()),
    ));
    assert!(
        app.session.archive_write.is_none(),
        "closed before save: no write"
    );
    assert!(!app.session.panes.contains_key(&second));
    let _ = app.update(Msg::ChangeArchiveFlag(
        first,
        td::ArchiveFlag::KeepFolderChats,
        true,
    ));
    let token = app.session.archive_write.unwrap().1;
    let _ = app.update(Msg::ArchiveFetched(
        first,
        client,
        token,
        td::ArchiveFlag::KeepFolderChats,
        true,
        Ok(shown.clone()),
    ));
    let _ = app.update(Msg::ToggleArchiveSettings(first, false));
    let _ = app.update(Msg::ChangeArchiveFlag(
        first,
        td::ArchiveFlag::KeepUnmuted,
        true,
    ));
    assert_eq!(
        app.session.archive_write,
        Some((first, token)),
        "in-flight write still owns lock"
    );
    let _ = app.update(Msg::ArchiveSaved(first, client, token, Ok(())));
    assert_eq!(app.session.archive_write, Some((first, token)));
    let _ = app.update(Msg::ToggleArchiveSettings(first, true));
    let reopened = pane(&app).list.archive_settings.as_ref().unwrap().request;
    let _ = app.update(Msg::ArchiveSettingsLoaded(
        first,
        client,
        reopened,
        Ok(shown.clone()),
    ));
    let _ = app.update(Msg::ArchiveCapabilityLoaded(
        first,
        client,
        reopened,
        Ok(true),
    ));
    let _ = app.update(Msg::ArchiveReadback(
        first,
        client,
        token,
        Ok(archive_values(false, false, true)),
    ));
    assert!(app.session.archive_write.is_none());
    assert_eq!(
        pane(&app)
            .list
            .archive_settings
            .as_ref()
            .unwrap()
            .settings
            .as_ref(),
        Some(&archive_values(false, false, true))
    );
    let refreshed = pane(&app).list.archive_settings.as_ref().unwrap().request;
    assert_ne!(
        reopened, refreshed,
        "reopened panel must reject its pre-save read"
    );
    let _ = app.update(Msg::ArchiveSettingsLoaded(
        first,
        client,
        reopened,
        Ok(shown.clone()),
    ));
    assert_eq!(
        pane(&app)
            .list
            .archive_settings
            .as_ref()
            .unwrap()
            .settings
            .as_ref(),
        Some(&archive_values(false, false, true))
    );
    let _ = app.update(Msg::ChangeArchiveFlag(
        first,
        td::ArchiveFlag::KeepUnmuted,
        true,
    ));
    let newer = app.session.archive_write.unwrap().1;
    app.session = Session::new(1, first, 1);
    let _ = app.update(Msg::ArchiveFetched(
        first,
        client,
        newer,
        td::ArchiveFlag::KeepUnmuted,
        true,
        Ok(shown),
    ));
    assert!(app.session.archive_write.is_none());
    assert!(pane(&app).list.archive_settings.is_none());
}

#[test]
fn archive_panel_belongs_to_the_window_when_tabs_switch() {
    let mut app = app();
    let window = app.main_window;
    select(&mut app, 1, &[(10, "one")]);
    let values = archive_values(false, true, false);
    let request = archive_panel(&mut app, window, values.clone(), Ok(false));
    select(&mut app, 2, &[(20, "two")]);
    assert_eq!(
        pane(&app).list.archive_settings.as_ref().unwrap().request,
        request
    );
    assert_eq!(
        pane(&app)
            .list
            .archive_settings
            .as_ref()
            .unwrap()
            .settings
            .as_ref(),
        Some(&values)
    );
    let _ = app.update(Msg::SelectTab(1));
    assert_eq!(
        pane(&app)
            .list
            .archive_settings
            .as_ref()
            .unwrap()
            .settings
            .as_ref(),
        Some(&values)
    );
}

#[test]
fn window_list_filter_survives_switching_tabs() {
    let mut app = app();
    let window = app.main_window;
    select(&mut app, 1, &[(10, "a")]);
    let _ = app.update(Msg::ShowArchive(window, true));
    assert!(pane(&app).list.archive);
    app.session.panes.get_mut(&window).unwrap().list.scroll = 300.0;
    let sidebar_id = pane(&app).list.scroll_id.clone();

    // The archive filter belongs to the main window, not to chat 1's tab:
    // opening another chat (a fresh tab) must not reset it.
    select(&mut app, 2, &[(20, "b")]);
    assert!(pane(&app).list.archive, "filter reset by a fresh tab");
    assert_eq!(pane(&app).list.scroll, 300.0);
    assert_eq!(pane(&app).list.scroll_id, sidebar_id);

    // Nor does restoring chat 1 from the background bring back a stale one.
    let _ = app.update(Msg::SelectTab(1));
    assert!(pane(&app).list.archive, "filter reset by a cached tab");
    assert_eq!(pane(&app).list.scroll, 300.0);
    assert_eq!(pane(&app).list.scroll_id, sidebar_id);
}

#[test]
fn folders_list_their_chats_and_drop_ones_moved_out() {
    let mut app = app();
    for id in [1, 2] {
        app.session
            .chats
            .insert(id, chat_item(&id.to_string(), false));
    }
    td(
        &mut app,
        json!({"@type": "updateChatFolders", "main_chat_list_position": 1, "are_tags_enabled": false,
               "chat_folders": [{"@type": "chatFolderInfo", "id": 3, "color_id": -1,
                   "is_shareable": false, "has_my_invite_links": false,
                   "icon": {"@type": "chatFolderIcon", "name": "Work"},
                   "name": {"@type": "chatFolderName", "animate_custom_emoji": false,
                            "text": {"@type": "formattedText", "text": "Работа", "entities": []}}}]}),
    );
    assert_eq!(app.session.folders, [(3, "Работа".to_owned())]);
    assert_eq!(
        app.session.main_tab, 1,
        "«Все чаты» after the folder, as the user ordered"
    );

    let last = |app: &mut App, chat_id: i64, positions: Value| {
        let mut m = message(chat_id, 1, "x");
        m["chat_id"] = json!(chat_id);
        td(
            app,
            json!({"@type": "updateChatLastMessage", "chat_id": chat_id, "last_message": m,
                   "positions": positions}),
        );
    };
    let pos = |list: Value, order: i64| json!({"@type": "chatPosition", "list": list, "order": order.to_string(), "is_pinned": false});
    let folder = json!({"@type": "chatListFolder", "chat_folder_id": 3});
    let main = json!({"@type": "chatListMain"});
    last(
        &mut app,
        1,
        json!([pos(main.clone(), 10), pos(folder.clone(), 10)]),
    );
    last(&mut app, 2, json!([pos(main.clone(), 20), pos(folder, 20)]));
    let in_folder = |app: &App| -> Vec<i64> {
        app.session.folder_lists[&3]
            .iter()
            .map(|&(_, id)| id)
            .collect()
    };
    assert_eq!(in_folder(&app), [2, 1]);
    // Chat 2 taken out of the folder: its positions no longer mention it.
    last(&mut app, 2, json!([pos(main, 30)]));
    assert_eq!(in_folder(&app), [1]);
    assert!(app.session.chats[&2].folders.is_empty());

    let window = app.main_window;
    let _ = app.update(Msg::ShowFolder(window, Some(3)));
    assert_eq!(pane(&app).list.folder, Some(3));
    // The folder is deleted: back to all chats.
    td(
        &mut app,
        json!({"@type": "updateChatFolders", "main_chat_list_position": 0,
               "are_tags_enabled": false, "chat_folders": []}),
    );
    assert_eq!(pane(&app).list.folder, None);
}

pub(super) fn folders_update(ids: &[(i32, &str)]) -> Value {
    json!({"@type": "updateChatFolders", "main_chat_list_position": 0,
           "are_tags_enabled": false,
           "chat_folders": ids.iter().map(|(id, name)| json!({
               "@type": "chatFolderInfo", "id": id, "color_id": -1,
               "is_shareable": false, "has_my_invite_links": false,
               "icon": {"@type": "chatFolderIcon", "name": "Work"},
               "name": {"@type": "chatFolderName", "animate_custom_emoji": false,
                        "text": {"@type": "formattedText", "text": name, "entities": []}}
           })).collect::<Vec<_>>()})
}

#[test]
fn bulk_read_confirms_exact_main_folder_and_archive_without_local_unread_changes() {
    let mut app = app();
    let window = app.main_window;
    let client = app.session.client_id;
    app.session.chats.insert(
        1,
        ChatItem {
            unread: 5,
            ..chat_item("Первый", false)
        },
    );
    app.session.unread = (5, 5);
    td(&mut app, folders_update(&[(3, "Работа")]));
    for (selection, expected) in [
        (0, ChatList::Main),
        (
            1,
            ChatList::Folder(tdlib_rs::types::ChatListFolder { chat_folder_id: 3 }),
        ),
        (2, ChatList::Archive),
    ] {
        match selection {
            1 => {
                let _ = app.update(Msg::ShowFolder(window, Some(3)));
            }
            2 => {
                let _ = app.update(Msg::ShowArchive(window, true));
            }
            _ => {}
        }
        let _ = app.update(Msg::AskReadList(window, client));
        let (token, scope) = pane(&app).list.confirm_read.clone().unwrap();
        assert_eq!(scope, expected);
        let _ = app.update(Msg::ConfirmReadList(window, client, token, false));
        assert!(app.session.list_reads.is_empty(), "cancel never dispatches");
        let _ = app.update(Msg::ConfirmReadList(window, client, token, true));
        assert!(
            app.session.list_reads.is_empty(),
            "stale confirm never dispatches"
        );
        let _ = app.update(Msg::AskReadList(window, client));
        let (token, _) = pane(&app).list.confirm_read.clone().unwrap();
        let _task = app.update(Msg::ConfirmReadList(window, client, token, true));
        assert_eq!(app.session.list_reads.len(), 1);
        assert_eq!(app.session.list_reads[0].list, expected);
        assert_eq!(app.session.unread, (5, 5));
        assert_eq!(app.session.chats[&1].unread, 5);
        let _ = app.update(Msg::ListReadDone(window, client, token, Ok(())));
        assert!(app.session.list_reads.is_empty());
        assert_eq!(app.session.unread, (5, 5));
        assert_eq!(app.session.chats[&1].unread, 5);
        assert!(pane(&app).list.read_feedback.as_ref().unwrap().is_ok());
    }
}

#[test]
fn bulk_read_rejects_scope_changes_deleted_folders_and_foreign_windows() {
    let mut app = app();
    let main = app.main_window;
    let other = WinId::unique();
    app.session.panes.insert(other, ChatPane::default());
    let client = app.session.client_id;
    td(&mut app, folders_update(&[(3, "Работа")]));
    let _ = app.update(Msg::AskReadList(main, client));
    let token = pane(&app).list.confirm_read.as_ref().unwrap().0;
    let _ = app.update(Msg::ShowArchive(main, true));
    let _ = app.update(Msg::ConfirmReadList(main, client, token, true));
    assert!(app.session.list_reads.is_empty());
    let _ = app.update(Msg::ShowFolder(main, Some(3)));
    let _ = app.update(Msg::AskReadList(main, client));
    let token = pane(&app).list.confirm_read.as_ref().unwrap().0;
    td(&mut app, folders_update(&[]));
    td(&mut app, folders_update(&[(3, "Работа")]));
    let _ = app.update(Msg::ShowFolder(main, Some(3)));
    let _ = app.update(Msg::ConfirmReadList(main, client, token, true));
    assert!(
        app.session.list_reads.is_empty(),
        "deleted then recreated folder invalidates confirmation"
    );
    let _ = app.update(Msg::AskReadList(other, client));
    let token = app.session.panes[&other]
        .list
        .confirm_read
        .as_ref()
        .unwrap()
        .0;
    let _ = app.update(Msg::ConfirmReadList(main, client, token, true));
    assert!(
        app.session.list_reads.is_empty(),
        "another window cannot confirm"
    );
    let _ = app.update(Msg::WindowClosed(other));
    let _ = app.update(Msg::ConfirmReadList(other, client, token, true));
    assert!(
        app.session.list_reads.is_empty(),
        "closed window cannot confirm"
    );
    let _ = app.update(Msg::AskReadList(main, client));
    let token = pane(&app).list.confirm_read.as_ref().unwrap().0;
    let _ = app.update(Msg::SwitchAccount(1));
    let _ = app.update(Msg::ConfirmReadList(main, client, token, true));
    assert!(
        app.session.list_reads.is_empty(),
        "switching account before confirmation cannot dispatch"
    );
}

#[test]
fn bulk_read_serializes_windows_and_only_server_updates_change_counts() {
    let mut app = app();
    let main = app.main_window;
    let other = WinId::unique();
    app.session.panes.insert(other, ChatPane::default());
    let client = app.session.client_id;
    app.session.chats.insert(
        1,
        ChatItem {
            unread: 9,
            ..chat_item("Первый", false)
        },
    );
    td(
        &mut app,
        json!({"@type": "updateUnreadMessageCount", "chat_list": {"@type": "chatListMain"},
            "unread_count": 9, "unread_unmuted_count": 9}),
    );
    let _ = app.update(Msg::AskReadList(main, client));
    let first = pane(&app).list.confirm_read.as_ref().unwrap().0;
    let _ = app.update(Msg::AskReadList(other, client));
    let second = app.session.panes[&other]
        .list
        .confirm_read
        .as_ref()
        .unwrap()
        .0;
    let _task = app.update(Msg::ConfirmReadList(main, client, first, true));
    let _ = app.update(Msg::ConfirmReadList(other, client, second, true));
    let _ = app.update(Msg::AskReadList(other, client));
    assert_eq!(app.session.list_reads.len(), 1);
    assert!(app.session.panes[&other].list.confirm_read.is_none());
    let _ = app.update(Msg::ListReadDone(
        other,
        client,
        first,
        Err("foreign".into()),
    ));
    assert_eq!(app.session.list_reads.len(), 1);
    let _ = app.update(Msg::ListReadDone(
        main,
        client,
        first,
        Err("NETWORK (500)".into()),
    ));
    assert!(app.session.list_reads.is_empty());
    let _ = app.update(Msg::ConfirmReadList(other, client, second, true));
    assert!(
        app.session.list_reads.is_empty(),
        "confirmation predating another window's request is stale"
    );
    assert_eq!(app.session.unread, (9, 9));
    assert_eq!(app.session.chats[&1].unread, 9);
    assert!(
        pane(&app)
            .list
            .read_feedback
            .as_ref()
            .unwrap()
            .as_ref()
            .unwrap_err()
            .contains("NETWORK")
    );
    let _ = app.update(Msg::AskReadList(other, client));
    let retry = app.session.panes[&other]
        .list
        .confirm_read
        .as_ref()
        .unwrap()
        .0;
    let _task = app.update(Msg::ConfirmReadList(other, client, retry, true));
    assert_eq!(app.session.list_reads.len(), 1);
    let _ = app.update(Msg::ListReadDone(main, client, first, Ok(())));
    assert_eq!(app.session.list_reads[0].request, retry);
    let _ = app.update(Msg::ListReadDone(other, client, retry, Ok(())));
    assert_eq!(app.session.unread, (9, 9));
    td(
        &mut app,
        json!({"@type": "updateUnreadMessageCount", "chat_list": {"@type": "chatListMain"},
            "unread_count": 0, "unread_unmuted_count": 0}),
    );
    td(
        &mut app,
        json!({"@type": "updateChatReadInbox", "chat_id": 1,
            "last_read_inbox_message_id": 10, "unread_count": 0}),
    );
    assert_eq!(app.session.unread, (0, 0));
    assert_eq!(app.session.chats[&1].unread, 0);
    assert_eq!(app.session.panes[&other].list.read_feedback, Some(Ok(())));
    let _ = app.update(Msg::AskReadList(other, client));
    let stale = app.session.panes[&other]
        .list
        .confirm_read
        .as_ref()
        .unwrap()
        .0;
    app.session = Session::new(client + 1, main, 1);
    app.session.auth = Auth::Ready;
    let _ = app.update(Msg::ConfirmReadList(other, client, stale, true));
    let _ = app.update(Msg::ListReadDone(
        other,
        client,
        retry,
        Err("old account".into()),
    ));
    assert!(app.session.list_reads.is_empty());
    assert!(pane(&app).list.read_feedback.is_none());
}

#[test]
fn bulk_read_keeps_closed_origins_serialized_until_their_reply() {
    let mut app = app();
    let main = app.main_window;
    let other = WinId::unique();
    app.session.panes.insert(other, ChatPane::default());
    let client = app.session.client_id;
    let _ = app.update(Msg::AskReadList(other, client));
    let token = app.session.panes[&other]
        .list
        .confirm_read
        .as_ref()
        .unwrap()
        .0;
    let _task = app.update(Msg::ConfirmReadList(other, client, token, true));
    let _ = app.update(Msg::WindowClosed(other));
    let _ = app.update(Msg::AskReadList(main, client));
    assert!(
        pane(&app).list.confirm_read.is_none(),
        "in-flight task survives origin closure"
    );
    let _ = app.update(Msg::ListReadDone(
        other,
        client,
        token,
        Err("closed".into()),
    ));
    assert!(app.session.list_reads.is_empty());
    assert!(pane(&app).list.read_feedback.is_none());
    let _ = app.update(Msg::AskReadList(main, client));
    assert!(pane(&app).list.confirm_read.is_some());
}

#[test]
fn bulk_read_reply_after_scope_switch_has_no_feedback_or_local_effect() {
    let mut app = app();
    let window = app.main_window;
    let client = app.session.client_id;
    app.session.unread = (7, 7);
    let _ = app.update(Msg::AskReadList(window, client));
    let token = pane(&app).list.confirm_read.as_ref().unwrap().0;
    let _task = app.update(Msg::ConfirmReadList(window, client, token, true));
    let _ = app.update(Msg::ShowArchive(window, true));
    let _ = app.update(Msg::ListReadDone(window, client, token, Err("late".into())));
    assert!(app.session.list_reads.is_empty());
    assert!(pane(&app).list.read_feedback.is_none());
    assert_eq!(app.session.unread, (7, 7));
}

fn pending_reorder(app: &App) -> u64 {
    app.session.folder_reorder.as_ref().unwrap().request
}

#[test]
fn folder_reorder_waits_for_update_and_error_keeps_server_order() {
    let mut app = app();
    let window = app.main_window;
    let mut initial = folders_update(&[(10, "Личные"), (20, "Работа")]);
    initial["main_chat_list_position"] = json!(1);
    td(&mut app, initial);
    let other = WinId::unique();
    app.session.panes.insert(other, ChatPane::default());
    let _ = app.update(Msg::ShowFolder(window, Some(10)));
    let _ = app.update(Msg::ShowFolder(other, Some(20)));
    app.session.folder_unread.insert(10, (2, 1));
    app.session.folder_unread.insert(20, (7, 5));
    assert_eq!(
        app.session.reordered_folders(10, false),
        Some((vec![20, 10], 1))
    );
    assert_eq!(app.session.reordered_folders(10, true), None);
    assert_eq!(app.session.reordered_folders(20, false), None);
    assert_eq!(app.session.reordered_folders(999, false), None);

    let _ = app.update(Msg::ReorderFolder(window, 10, true));
    assert!(
        app.session.folder_reorder.is_none(),
        "boundary sends nothing"
    );
    let _ = app.update(Msg::ReorderFolder(window, 10, false));
    let pending = pending_reorder(&app);
    let client = app.session.client_id;
    assert_eq!(app.session.folder_reorder.as_ref().unwrap().window, window);
    assert_eq!(app.session.folders[0].0, 10, "no optimistic change");
    let _ = app.update(Msg::ReorderFolder(window, 20, true));
    assert_eq!(pending_reorder(&app), pending, "one in flight");
    let _ = app.update(Msg::FolderReordered(window, client, pending, Ok(())));
    assert_eq!(app.session.folders[0].0, 10, "answer is not an update");
    assert_eq!(
        pending_reorder(&app),
        pending,
        "reply alone keeps controls locked"
    );
    let _ = app.update(Msg::ReorderFolder(window, 10, false));
    assert_eq!(
        pending_reorder(&app),
        pending,
        "a second click cannot use stale order"
    );
    let mut server = folders_update(&[(20, "Работа"), (10, "Личные")]);
    server["main_chat_list_position"] = json!(1);
    td(&mut app, server);
    assert!(
        app.session.folder_reorder.is_none(),
        "authoritative update unlocks"
    );
    assert_eq!(app.session.folders[0].0, 20);
    assert_eq!(
        app.session.main_tab, 1,
        "main slot stays where TDLib put it"
    );
    assert_eq!(app.session.folder_unread[&20], (7, 5));
    assert_eq!(app.session.folder_unread[&10], (2, 1));
    assert_eq!(app.session.panes[&window].list.folder, Some(10));
    assert_eq!(app.session.panes[&other].list.folder, Some(20));

    let _ = app.update(Msg::ReorderFolder(window, 10, true));
    let failed = pending_reorder(&app);
    let _ = app.update(Msg::FolderReordered(
        window,
        client,
        failed,
        Err("REORDER_FAILED (400)".into()),
    ));
    assert_eq!(app.session.folders[0].0, 20);
    assert!(app.session.folder_reorder.is_none(), "failure allows retry");
    assert!(
        app.session
            .folder_reorder_error
            .as_deref()
            .unwrap()
            .contains("REORDER_FAILED")
    );
    let _ = app.update(Msg::ReorderFolder(window, 10, true));
    let retry = pending_reorder(&app);
    assert_ne!(failed, retry);
    let mut server = folders_update(&[(10, "Личные"), (20, "Работа")]);
    server["main_chat_list_position"] = json!(1);
    td(&mut app, server);
    assert_eq!(
        pending_reorder(&app),
        retry,
        "early update awaits the reply"
    );
    let _ = app.update(Msg::FolderReordered(window, client, retry, Ok(())));
    assert!(
        app.session.folder_reorder.is_none(),
        "early update unlocks after Ok"
    );
    assert_eq!(app.session.folders[0].0, 10);
    assert_eq!(app.session.main_tab, 1);
    assert!(app.session.folder_reorder_error.is_none());
}

#[test]
fn folder_reorder_serializes_windows_and_ignores_closed_replaced_and_old_accounts() {
    let mut app = app();
    let main = app.main_window;
    let other = WinId::unique();
    app.session.panes.insert(other, ChatPane::default());
    let mut initial = folders_update(&[(10, "A"), (20, "B"), (30, "C")]);
    initial["main_chat_list_position"] = json!(2);
    td(&mut app, initial);
    assert_eq!(
        app.session.reordered_folders(20, false),
        Some((vec![10, 30, 20], 2)),
        "request includes even folders outside the viewport"
    );
    let _ = app.update(Msg::ReorderFolder(other, 20, false));
    let old = pending_reorder(&app);
    let client = app.session.client_id;
    let _ = app.update(Msg::ReorderFolder(main, 10, false));
    assert_eq!(pending_reorder(&app), old);
    let _ = app.update(Msg::WindowClosed(other));
    let _ = app.update(Msg::ReorderFolder(main, 10, false));
    assert_eq!(
        pending_reorder(&app),
        old,
        "closed window's request is still in flight"
    );
    let _ = app.update(Msg::FolderReordered(other, client, old, Err("late".into())));
    assert!(app.session.folder_reorder.is_none());
    assert!(
        app.session.folder_reorder_error.is_none(),
        "closed window's error is private"
    );
    let _ = app.update(Msg::ReorderFolder(other, 20, false));
    assert!(
        app.session.folder_reorder.is_none(),
        "closed window cannot send"
    );

    let _ = app.update(Msg::ReorderFolder(main, 10, false));
    let current = pending_reorder(&app);
    let mut server = folders_update(&[(30, "C"), (10, "A"), (20, "B")]);
    server["main_chat_list_position"] = json!(1);
    td(&mut app, server);
    let _ = app.update(Msg::FolderReordered(other, client, old, Ok(())));
    assert_eq!(pending_reorder(&app), current);
    let _ = app.update(Msg::FolderReordered(
        main,
        client,
        current,
        Err("rejected".into()),
    ));
    assert_eq!(
        app.session
            .folders
            .iter()
            .map(|(id, _)| *id)
            .collect::<Vec<_>>(),
        [30, 10, 20]
    );
    assert_eq!(app.session.main_tab, 1);

    let _ = app.update(Msg::ReorderFolder(main, 10, true));
    let stale = pending_reorder(&app);
    app.session = Session::new(client + 1, main, 1);
    assert!(app.session.folder_reorder.is_none());
    assert!(app.session.folder_reorder_error.is_none());
    let _ = app.update(Msg::Td(
        client + 1,
        Box::new(serde_json::from_value(folders_update(&[(10, "New"), (20, "Other")])).unwrap()),
    ));
    let _ = app.update(Msg::ReorderFolder(main, 10, false));
    let fresh = pending_reorder(&app);
    let _ = app.update(Msg::FolderReordered(
        main,
        client,
        stale,
        Err("old account".into()),
    ));
    assert_eq!(pending_reorder(&app), fresh);
    assert!(app.session.folder_reorder_error.is_none());
    assert_eq!(app.session.folders[0].1, "New");
}

#[test]
fn folder_reorder_two_consecutive_moves_follow_server_order_in_both_event_orders() {
    let mut app = app();
    let window = app.main_window;
    let client = app.session.client_id;
    let mut initial = folders_update(&[(10, "A"), (20, "B"), (30, "C")]);
    initial["main_chat_list_position"] = json!(2);
    td(&mut app, initial);

    let _ = app.update(Msg::ReorderFolder(window, 10, false));
    let first = pending_reorder(&app);
    assert_eq!(
        app.session.folder_reorder.as_ref().unwrap().expected,
        [20, 10, 30]
    );
    let _ = app.update(Msg::FolderReordered(window, client, first, Ok(())));
    let _ = app.update(Msg::ReorderFolder(window, 10, false));
    assert_eq!(
        pending_reorder(&app),
        first,
        "Ok cannot permit a stale second move"
    );
    assert_eq!(
        app.session.reordered_folders(10, false),
        Some((vec![20, 10, 30], 2))
    );
    let mut server = folders_update(&[(20, "B"), (10, "A"), (30, "C")]);
    server["main_chat_list_position"] = json!(2);
    td(&mut app, server);
    assert!(app.session.folder_reorder.is_none());

    let _ = app.update(Msg::ReorderFolder(window, 10, false));
    let second = pending_reorder(&app);
    assert_ne!(first, second);
    assert_eq!(
        app.session.folder_reorder.as_ref().unwrap().expected,
        [20, 30, 10]
    );
    let _ = app.update(Msg::FolderReordered(
        window,
        client,
        first,
        Err("stale".into()),
    ));
    assert_eq!(pending_reorder(&app), second);
    let mut server = folders_update(&[(20, "B"), (30, "C"), (10, "A")]);
    server["main_chat_list_position"] = json!(2);
    td(&mut app, server);
    assert_eq!(
        pending_reorder(&app),
        second,
        "update before reply retains lock"
    );
    let _ = app.update(Msg::FolderReordered(window, client, second, Ok(())));
    assert!(app.session.folder_reorder.is_none());
    assert_eq!(
        app.session
            .folders
            .iter()
            .map(|(id, _)| *id)
            .collect::<Vec<_>>(),
        [20, 30, 10]
    );
    assert_eq!(app.session.main_tab, 2);
    assert!(app.session.folder_reorder_error.is_none());
}

#[test]
fn folder_reorder_ignores_unrelated_pre_reply_updates_and_reports_server_conflict() {
    let mut app = app();
    let window = app.main_window;
    let client = app.session.client_id;
    let mut initial = folders_update(&[(10, "A"), (20, "B"), (30, "C")]);
    initial["main_chat_list_position"] = json!(1);
    td(&mut app, initial);
    let _ = app.update(Msg::ReorderFolder(window, 10, false));
    let request = pending_reorder(&app);
    td(&mut app, folders_update(&[(30, "C"), (10, "A"), (20, "B")]));
    assert_eq!(
        app.session.folder_reorder.as_ref().unwrap().state,
        FolderReorderState::DiscrepancyBeforeReply
    );
    assert!(app.session.folder_reorder_error.is_some());
    let _ = app.update(Msg::AcknowledgeFolderReorder(window, client, request));
    assert_eq!(pending_reorder(&app), request, "a reply is still required");
    let _ = app.update(Msg::FolderReordered(window, client, request, Ok(())));
    assert_eq!(
        app.session.folder_reorder.as_ref().unwrap().state,
        FolderReorderState::DiscrepancyAfterReply
    );
    let _ = app.update(Msg::ReorderFolder(window, 10, false));
    assert_eq!(pending_reorder(&app), request);
    td(&mut app, folders_update(&[(30, "C"), (20, "B"), (10, "A")]));
    assert_eq!(
        pending_reorder(&app),
        request,
        "a different order is not confirmation"
    );
    assert!(app.session.folder_reorder_error.is_some());
    assert_eq!(
        app.session
            .folders
            .iter()
            .map(|(id, _)| *id)
            .collect::<Vec<_>>(),
        [30, 20, 10]
    );
    let _ = app.update(Msg::ReorderFolder(window, 30, false));
    assert_eq!(pending_reorder(&app), request, "no stale second move");
    let _ = app.update(Msg::AcknowledgeFolderReorder(window, client + 1, request));
    let _ = app.update(Msg::AcknowledgeFolderReorder(window, client, request + 1));
    assert_eq!(pending_reorder(&app), request, "stale recovery is ignored");
    let _ = app.update(Msg::AcknowledgeFolderReorder(window, client, request));
    assert!(app.session.folder_reorder.is_none());
    assert!(
        app.session
            .folder_reorder_error
            .as_deref()
            .unwrap()
            .contains("ещё может")
    );
    assert_eq!(
        app.session.next_folder_reorder, request,
        "recovery does not replay"
    );
    let _ = app.update(Msg::FolderReordered(window, client, request, Ok(())));
    assert!(
        app.session.folder_reorder.is_none(),
        "late reply cannot start a move"
    );
    let _ = app.update(Msg::ReorderFolder(window, 30, false));
    assert_eq!(
        app.session.folder_reorder.as_ref().unwrap().expected,
        [20, 30, 10],
        "only an explicit new move uses the latest cached server order"
    );
}

#[test]
fn folder_reorder_early_confirmation_is_invalidated_by_newer_update() {
    let mut app = app();
    let window = app.main_window;
    let client = app.session.client_id;
    td(&mut app, folders_update(&[(10, "A"), (20, "B"), (30, "C")]));
    let _ = app.update(Msg::ReorderFolder(window, 10, false));
    let request = pending_reorder(&app);
    td(&mut app, folders_update(&[(20, "B"), (10, "A"), (30, "C")]));
    td(&mut app, folders_update(&[(30, "C"), (10, "A"), (20, "B")]));
    let _ = app.update(Msg::FolderReordered(window, client, request, Ok(())));
    assert_eq!(pending_reorder(&app), request);
    td(&mut app, folders_update(&[(20, "B"), (10, "A"), (30, "C")]));
    assert!(app.session.folder_reorder.is_none());
    assert!(app.session.folder_reorder_error.is_none());
}

#[test]
fn folder_reorder_metadata_update_after_ok_does_not_unlock() {
    let mut app = app();
    let window = app.main_window;
    let client = app.session.client_id;
    td(&mut app, folders_update(&[(10, "A"), (20, "B"), (30, "C")]));
    let _ = app.update(Msg::ReorderFolder(window, 10, false));
    let request = pending_reorder(&app);
    let _ = app.update(Msg::FolderReordered(window, client, request, Ok(())));
    td(
        &mut app,
        folders_update(&[(10, "renamed"), (20, "B"), (30, "C")]),
    );
    assert_eq!(app.session.folders[0].1, "renamed");
    assert_eq!(pending_reorder(&app), request);
    assert_eq!(
        app.session.folder_reorder.as_ref().unwrap().state,
        FolderReorderState::AwaitingUpdate
    );
    assert!(
        app.session
            .folder_reorder_error
            .as_deref()
            .unwrap()
            .contains("ещё может")
    );
    let _ = app.update(Msg::ReorderFolder(window, 10, false));
    assert_eq!(
        pending_reorder(&app),
        request,
        "rename cannot dispatch a stale move"
    );
    let _ = app.update(Msg::AcknowledgeFolderReorder(window, client, request));
    assert!(app.session.folder_reorder.is_none());
    assert!(
        app.session
            .folder_reorder_error
            .as_deref()
            .unwrap()
            .contains("ещё может")
    );
}

#[test]
fn folder_reorder_conflicting_update_after_ok_waits_for_exact_order() {
    let mut app = app();
    let window = app.main_window;
    let client = app.session.client_id;
    td(&mut app, folders_update(&[(10, "A"), (20, "B"), (30, "C")]));
    let _ = app.update(Msg::ReorderFolder(window, 10, false));
    let request = pending_reorder(&app);
    let _ = app.update(Msg::FolderReordered(window, client, request, Ok(())));
    td(&mut app, folders_update(&[(30, "C"), (10, "A"), (20, "B")]));
    assert_eq!(
        app.session.folder_reorder.as_ref().unwrap().state,
        FolderReorderState::DiscrepancyAfterReply
    );
    assert!(app.session.folder_reorder_error.is_some());
    td(
        &mut app,
        folders_update(&[(10, "renamed"), (20, "B"), (30, "C")]),
    );
    assert_eq!(
        pending_reorder(&app),
        request,
        "return to old order is not proof"
    );
    assert!(app.session.folder_reorder_error.is_some());
    let _ = app.update(Msg::ReorderFolder(window, 10, false));
    assert_eq!(pending_reorder(&app), request);
    td(
        &mut app,
        folders_update(&[(20, "B"), (10, "renamed"), (30, "C")]),
    );
    assert!(app.session.folder_reorder.is_none());
    assert!(app.session.folder_reorder_error.is_none());
}

#[test]
fn folder_reorder_early_match_invalidated_by_metadata_update() {
    let mut app = app();
    let window = app.main_window;
    let client = app.session.client_id;
    td(&mut app, folders_update(&[(10, "A"), (20, "B"), (30, "C")]));
    let _ = app.update(Msg::ReorderFolder(window, 10, false));
    let request = pending_reorder(&app);
    td(&mut app, folders_update(&[(20, "B"), (10, "A"), (30, "C")]));
    assert_eq!(
        app.session.folder_reorder.as_ref().unwrap().state,
        FolderReorderState::MatchedBeforeReply
    );
    td(
        &mut app,
        folders_update(&[(10, "renamed"), (20, "B"), (30, "C")]),
    );
    let _ = app.update(Msg::FolderReordered(window, client, request, Ok(())));
    assert_eq!(
        app.session.folder_reorder.as_ref().unwrap().state,
        FolderReorderState::AwaitingUpdate
    );
    td(
        &mut app,
        folders_update(&[(20, "B"), (10, "renamed"), (30, "C")]),
    );
    assert!(app.session.folder_reorder.is_none());
}

#[test]
fn folder_picker_waits_for_server_positions_and_allows_failure_retry() {
    let mut app = app();
    app.session.chats.insert(1, chat_item("Чат", false));
    let window = app.main_window;
    let _ = app.update(Msg::ChatMenu(window, Some(1)));
    let _ = app.update(Msg::ChatOp(window, 1, ChatOp::Folders));
    assert!(
        pane(&app).list.folder_picker.is_none(),
        "no existing folders"
    );
    let _ = app.update(Msg::FolderAction(window, 1, 3, true));
    assert!(app.session.folder_edits.is_empty());

    td(&mut app, folders_update(&[(3, "Работа"), (4, "Личное")]));
    let _ = app.update(Msg::ChatOp(window, 1, ChatOp::Folders));
    assert_eq!(pane(&app).list.folder_picker.as_ref().unwrap().chat_id, 1);
    // Cancel without selecting a folder does not start any request.
    let _ = app.update(Msg::ChatOp(window, 1, ChatOp::Close));
    assert!(app.session.folder_edits.is_empty());
    assert!(pane(&app).list.folder_picker.is_none());

    let _ = app.update(Msg::ChatMenu(window, Some(1)));
    let _ = app.update(Msg::ChatOp(window, 1, ChatOp::Folders));
    let _ = app.update(Msg::FolderAction(window, 1, 3, true));
    let first = app.session.folder_edits[&3];
    assert_eq!(
        pane(&app).list.folder_picker.as_ref().unwrap().pending,
        Some((first, 3))
    );
    let _ = app.update(Msg::FolderAction(window, 1, 3, true));
    assert_eq!(app.session.folder_edits[&3], first, "no duplicate request");
    assert!(app.session.chats[&1].folders.is_empty());
    assert!(!app.session.folder_lists.contains_key(&3));

    let _ = app.update(Msg::FolderEdited(
        window,
        0,
        1,
        3,
        first,
        Err("PREMIUM_REQUIRED (400)".into()),
    ));
    assert!(
        pane(&app)
            .list
            .folder_picker
            .as_ref()
            .unwrap()
            .feedback
            .as_deref()
            .unwrap()
            .contains("PREMIUM_REQUIRED")
    );
    assert!(
        app.session.chats[&1].folders.is_empty(),
        "failure cannot change list"
    );
    let _ = app.update(Msg::FolderAction(window, 1, 3, true));
    let retry = app.session.folder_edits[&3];
    assert_ne!(retry, first);
    let _ = app.update(Msg::FolderEdited(window, 0, 1, 3, first, Ok(true)));
    assert_eq!(
        app.session.folder_edits[&3], retry,
        "stale reply cannot finish retry"
    );
    let _ = app.update(Msg::FolderEdited(window, 0, 1, 3, retry, Ok(true)));
    assert!(
        app.session.chats[&1].folders.is_empty(),
        "edit answer is not a list update"
    );
    td(
        &mut app,
        json!({"@type": "updateChatPosition", "chat_id": 1,
        "position": {"@type": "chatPosition", "list": {"@type": "chatListFolder", "chat_folder_id": 3},
                     "order": "75", "is_pinned": false}}),
    );
    assert_eq!(
        app.session.folder_lists[&3]
            .iter()
            .map(|&(_, id)| id)
            .collect::<Vec<_>>(),
        [1]
    );
    assert!(!app.session.folder_lists.contains_key(&4));

    let _ = app.update(Msg::FolderAction(window, 1, 3, false));
    let exclude = app.session.folder_edits[&3];
    assert_eq!(app.session.chats[&1].folders, [(3, 75)]);
    let _ = app.update(Msg::FolderEdited(window, 0, 1, 3, exclude, Ok(true)));
    assert_eq!(app.session.chats[&1].folders, [(3, 75)]);
    td(
        &mut app,
        json!({"@type": "updateChatPosition", "chat_id": 1,
        "position": {"@type": "chatPosition", "list": {"@type": "chatListFolder", "chat_folder_id": 3},
                     "order": "0", "is_pinned": false}}),
    );
    assert!(app.session.folder_lists[&3].is_empty());
    assert!(app.session.chats[&1].folders.is_empty());
}

#[test]
fn folder_picker_drops_stale_window_chat_folder_and_account_answers() {
    let mut app = app();
    app.session.chats.insert(1, chat_item("Первый", false));
    app.session.chats.insert(2, chat_item("Второй", false));
    td(&mut app, folders_update(&[(3, "Работа"), (4, "Личное")]));
    let main = app.main_window;
    let other = WinId::unique();
    app.session.panes.insert(other, ChatPane::default());
    let _ = app.update(Msg::ChatMenu(other, Some(1)));
    let _ = app.update(Msg::ChatOp(other, 1, ChatOp::Folders));
    let _ = app.update(Msg::FolderAction(other, 1, 3, true));
    let request = app.session.folder_edits[&3];
    let _ = app.update(Msg::ChatMenu(main, Some(2)));
    let _ = app.update(Msg::ChatOp(main, 2, ChatOp::Folders));
    let _ = app.update(Msg::FolderAction(main, 2, 3, false));
    assert_eq!(
        app.session.folder_edits[&3], request,
        "one folder edit across windows"
    );
    assert!(
        pane(&app)
            .list
            .folder_picker
            .as_ref()
            .unwrap()
            .feedback
            .is_some()
    );

    let _ = app.update(Msg::ChatOp(other, 1, ChatOp::Close));
    let _ = app.update(Msg::FolderEdited(
        other,
        0,
        1,
        3,
        request,
        Err("late error".into()),
    ));
    assert!(app.session.folder_edits.is_empty());
    assert!(app.session.chats[&1].folders.is_empty());
    assert!(app.session.panes[&other].list.folder_picker.is_none());
    assert!(
        pane(&app)
            .list
            .folder_picker
            .as_ref()
            .unwrap()
            .feedback
            .is_some()
    );

    let _ = app.update(Msg::FolderAction(main, 2, 4, true));
    let request = app.session.folder_edits[&4];
    let _ = app.update(Msg::Pane(main, PaneMsg::SelectChat(1)));
    assert!(pane(&app).list.folder_picker.is_none());
    let _ = app.update(Msg::FolderEdited(main, 0, 2, 4, request, Ok(true)));
    assert!(pane(&app).list.folder_picker.is_none(), "switched chat");

    let _ = app.update(Msg::ChatMenu(other, Some(1)));
    let _ = app.update(Msg::ChatOp(other, 1, ChatOp::Folders));
    let _ = app.update(Msg::FolderAction(other, 1, 3, true));
    let request = app.session.folder_edits[&3];
    td(&mut app, folders_update(&[(4, "Личное")]));
    assert!(
        app.session.panes[&other]
            .list
            .folder_picker
            .as_ref()
            .unwrap()
            .pending
            .is_none()
    );
    let _ = app.update(Msg::FolderEdited(
        other,
        0,
        1,
        3,
        request,
        Err("deleted".into()),
    ));
    assert!(
        app.session.panes[&other]
            .list
            .folder_picker
            .as_ref()
            .unwrap()
            .feedback
            .is_some()
    );

    let _ = app.update(Msg::FolderAction(other, 1, 4, true));
    let request = app.session.folder_edits[&4];
    let _ = app.update(Msg::WindowClosed(other));
    let _ = app.update(Msg::FolderEdited(
        other,
        0,
        1,
        4,
        request,
        Err("closed window".into()),
    ));
    assert!(!app.session.panes.contains_key(&other));
    assert!(app.session.folder_edits.is_empty());
    assert!(app.error.is_none(), "old window's error must stay private");

    let _ = app.update(Msg::ChatMenu(main, Some(1)));
    let _ = app.update(Msg::ChatOp(main, 1, ChatOp::Folders));
    let _ = app.update(Msg::FolderAction(main, 1, 4, true));
    let request = app.session.folder_edits[&4];
    app.session = Session::new(1, main, 1);
    let _ = app.update(Msg::FolderEdited(main, 0, 1, 4, request, Ok(true)));
    assert!(app.session.folder_edits.is_empty());
    assert!(app.session.panes[&main].list.folder_picker.is_none());
}

#[test]
fn first_folder_waits_for_server_update_and_serializes_windows() {
    let mut app = app();
    let main = app.main_window;
    let other = WinId::unique();
    app.session.panes.insert(other, ChatPane::default());
    app.session.chats.insert(1, chat_item("Чат", false));
    app.session.chats.insert(2, chat_item("Другой", false));
    let _ = app.update(Msg::ChatMenu(main, Some(1)));
    let _ = app.update(Msg::ChatOp(main, 1, ChatOp::NewFolder));
    assert_eq!(pane(&app).list.new_folder_name.as_deref(), Some(""));
    let _ = app.update(Msg::FolderName(main, 1, "  Друзья  ".into()));
    let _ = app.update(Msg::ConfirmNewFolder(main, 1));
    let pending = app.session.folder_creation.as_ref().unwrap();
    let request = pending.request;
    assert!(app.session.folders.is_empty());
    let _ = app.update(Msg::ChatMenu(other, Some(2)));
    let _ = app.update(Msg::ChatOp(other, 2, ChatOp::NewFolder));
    let _ = app.update(Msg::FolderName(other, 2, "Друзья".into()));
    let _ = app.update(Msg::ConfirmNewFolder(other, 2));
    let _ = app.update(Msg::ConfirmNewFolder(main, 1));
    assert_eq!(
        app.session.folder_creation.as_ref().unwrap().request,
        request
    );
    assert!(app.session.panes[&other].list.new_folder_error.is_some());
    td(&mut app, folders_update(&[]));
    assert!(app.session.folder_creation.is_some());
    let _ = app.update(Msg::FolderCreated(main, 0, request, Ok(73)));
    assert!(app.session.folders.is_empty(), "reply cannot invent tab");
    assert!(
        app.session.folder_creation.is_some(),
        "reply alone cannot unlock"
    );
    let _ = app.update(Msg::ReorderFolder(other, 73, true));
    td(&mut app, folders_update(&[(73, "Друзья")]));
    assert!(app.session.folder_creation.is_none());
    assert_eq!(app.session.folders, [(73, "Друзья".into())]);
    assert!(app.session.panes[&other].list.folder.is_none());
}

#[test]
fn folder_creation_uses_returned_id_after_early_update_and_rename() {
    let mut app = app();
    let window = app.main_window;
    app.session.chats.insert(1, chat_item("Чат", false));
    td(&mut app, folders_update(&[(10, "Новая"), (20, "Другая")]));
    let _ = app.update(Msg::ChatMenu(window, Some(1)));
    let _ = app.update(Msg::ChatOp(window, 1, ChatOp::NewFolder));
    let _ = app.update(Msg::FolderName(window, 1, "Новая".into()));
    let _ = app.update(Msg::ConfirmNewFolder(window, 1));
    let request = app.session.folder_creation.as_ref().unwrap().request;
    td(
        &mut app,
        folders_update(&[(10, "Новая"), (20, "Другая"), (30, "Новая")]),
    );
    td(
        &mut app,
        folders_update(&[(10, "Новая"), (20, "Другая"), (30, "Переименована")]),
    );
    let _ = app.update(Msg::FolderCreated(window, 0, request + 1, Ok(30)));
    let _ = app.update(Msg::FolderCreated(window, 1, request, Ok(30)));
    let _ = app.update(Msg::FolderCreated(WinId::unique(), 0, request, Ok(30)));
    assert!(
        app.session.folder_creation.is_some(),
        "stale replies cannot unlock"
    );
    let _ = app.update(Msg::FolderCreated(window, 0, request, Ok(10)));
    assert!(
        app.session.folder_creation.is_some(),
        "initial folder id cannot confirm"
    );
    let _ = app.update(Msg::FolderCreated(window, 0, request, Ok(30)));
    assert!(
        app.session.folder_creation.is_none(),
        "returned new id confirms after rename"
    );
    assert_eq!(app.session.folders[2], (30, "Переименована".into()));
}

#[test]
fn folder_creation_ignores_unrelated_names_until_returned_id_arrives() {
    let mut app = app();
    let window = app.main_window;
    app.session.chats.insert(1, chat_item("Чат", false));
    td(&mut app, folders_update(&[(10, "Старая"), (20, "Другая")]));
    let _ = app.update(Msg::ChatMenu(window, Some(1)));
    let _ = app.update(Msg::ChatOp(window, 1, ChatOp::NewFolder));
    let _ = app.update(Msg::FolderName(window, 1, "Новая".into()));
    let _ = app.update(Msg::ConfirmNewFolder(window, 1));
    let request = app.session.folder_creation.as_ref().unwrap().request;
    let _ = app.update(Msg::FolderCreated(window, 0, request, Ok(40)));
    td(
        &mut app,
        folders_update(&[(10, "Новая"), (20, "Другая"), (41, "Новая")]),
    );
    assert!(
        app.session.folder_creation.is_some(),
        "name matches cannot confirm another id"
    );
    let _ = app.update(Msg::ConfirmNewFolder(window, 1));
    assert_eq!(
        app.session.folder_creation.as_ref().unwrap().request,
        request
    );
    let _ = app.update(Msg::ReorderFolder(window, 10, false));
    assert!(app.session.folder_reorder.is_none());
    td(
        &mut app,
        folders_update(&[
            (10, "Новая"),
            (20, "Другая"),
            (41, "Новая"),
            (40, "Переименована"),
        ]),
    );
    assert!(
        app.session.folder_creation.is_none(),
        "reply-before-update confirms by id"
    );
    assert_eq!(app.session.folders[3], (40, "Переименована".into()));
}

#[test]
fn folder_creation_handles_early_update_failure_and_stale_replies() {
    let mut app = app();
    let window = app.main_window;
    app.session.chats.insert(1, chat_item("Чат", false));
    app.session.chats.insert(2, chat_item("Другой", false));
    let _ = app.update(Msg::ChatMenu(window, Some(1)));
    let _ = app.update(Msg::ChatOp(window, 1, ChatOp::NewFolder));
    for name in [" ", "Слишком длинное имя", "С\nстрокой"] {
        let _ = app.update(Msg::FolderName(window, 1, name.into()));
        let _ = app.update(Msg::ConfirmNewFolder(window, 1));
        assert!(app.session.folder_creation.is_none());
        assert!(pane(&app).list.new_folder_error.is_some());
    }
    let _ = app.update(Msg::ChatOp(window, 1, ChatOp::Close));
    assert!(app.session.folder_creation.is_none());
    let _ = app.update(Msg::ChatMenu(window, Some(1)));
    let _ = app.update(Msg::ChatOp(window, 1, ChatOp::NewFolder));
    let _ = app.update(Msg::FolderName(window, 1, "Новая".into()));
    let _ = app.update(Msg::ConfirmNewFolder(window, 1));
    let request = app.session.folder_creation.as_ref().unwrap().request;
    td(&mut app, folders_update(&[(91, "Новая")]));
    assert!(
        app.session.folder_creation.is_some(),
        "early update awaits returned id"
    );
    let _ = app.update(Msg::FolderCreated(window, 0, request, Ok(91)));
    assert!(app.session.folder_creation.is_none());
    assert_eq!(app.session.folders, [(91, "Новая".into())]);

    let _ = app.update(Msg::ChatMenu(window, Some(2)));
    let _ = app.update(Msg::ChatOp(window, 2, ChatOp::NewFolder));
    let _ = app.update(Msg::FolderName(window, 2, "Ошибка".into()));
    let _ = app.update(Msg::ConfirmNewFolder(window, 2));
    let failed = app.session.folder_creation.as_ref().unwrap().request;
    let _ = app.update(Msg::FolderCreated(
        window,
        0,
        failed,
        Err("LIMIT (400)".into()),
    ));
    assert!(app.session.folder_creation.is_none());
    assert_eq!(pane(&app).list.new_folder_name.as_deref(), Some("Ошибка"));
    assert!(
        pane(&app)
            .list
            .new_folder_error
            .as_deref()
            .unwrap()
            .contains("LIMIT")
    );
    assert_eq!(app.session.folders.len(), 1);
    let _ = app.update(Msg::FolderCreated(window, 0, failed, Ok(92)));
    assert_eq!(app.session.folders.len(), 1);
    let _ = app.update(Msg::ConfirmNewFolder(window, 2));
    let retry = app.session.folder_creation.as_ref().unwrap().request;
    let _ = app.update(Msg::ChatMenu(window, Some(1)));
    let _ = app.update(Msg::FolderCreated(window, 1, retry, Ok(92)));
    assert!(app.session.folder_creation.is_some(), "old account ignored");
    let _ = app.update(Msg::FolderCreated(WinId::unique(), 0, retry, Ok(92)));
    assert!(
        app.session.folder_creation.is_some(),
        "foreign window ignored"
    );
    let _ = app.update(Msg::FolderCreated(window, 0, retry, Err("late".into())));
    assert!(pane(&app).list.new_folder_error.is_none(), "switched chat");
    app.session = Session::new(1, window, 1);
    let _ = app.update(Msg::FolderCreated(window, 0, retry, Ok(92)));
    assert!(app.session.folder_creation.is_none());
}

#[test]
fn switching_selected_chat_or_reopening_form_cannot_take_old_reply() {
    let mut app = app();
    let window = app.main_window;
    app.session.chats.insert(1, chat_item("Первый", false));
    app.session.chats.insert(2, chat_item("Второй", false));
    let _ = app.update(Msg::ChatMenu(window, Some(1)));
    let _ = app.update(Msg::ChatOp(window, 1, ChatOp::NewFolder));
    let _ = app.update(Msg::FolderName(window, 1, "Новая".into()));
    let _ = app.update(Msg::ConfirmNewFolder(window, 1));
    let request = app.session.folder_creation.as_ref().unwrap().request;
    let _ = app.update(Msg::Pane(window, PaneMsg::SelectChat(2)));
    assert!(pane(&app).list.new_folder_name.is_none());
    let _ = app.update(Msg::ChatMenu(window, Some(1)));
    let _ = app.update(Msg::ChatOp(window, 1, ChatOp::NewFolder));
    let _ = app.update(Msg::FolderName(window, 1, "Черновик".into()));
    td(&mut app, folders_update(&[(74, "Новая")]));
    let _ = app.update(Msg::FolderCreated(window, 0, request, Ok(74)));
    assert_eq!(pane(&app).list.new_folder_name.as_deref(), Some("Черновик"));
    assert!(app.session.folder_creation.is_none());
    assert_eq!(app.session.folders, [(74, "Новая".into())]);
}

#[test]
fn unconfirmed_creation_requires_explicit_recovery_and_blocks_reordering() {
    let mut app = app();
    let window = app.main_window;
    app.session.chats.insert(1, chat_item("Чат", false));
    td(&mut app, folders_update(&[(10, "Старая"), (20, "Ещё")]));
    let _ = app.update(Msg::ChatMenu(window, Some(1)));
    let _ = app.update(Msg::ChatOp(window, 1, ChatOp::NewFolder));
    let _ = app.update(Msg::FolderName(window, 1, "Новая".into()));
    let _ = app.update(Msg::ConfirmNewFolder(window, 1));
    let request = app.session.folder_creation.as_ref().unwrap().request;
    let _ = app.update(Msg::ReorderFolder(window, 10, false));
    assert!(app.session.folder_reorder.is_none());
    let _ = app.update(Msg::FolderCreated(window, 0, request, Ok(30)));
    td(&mut app, folders_update(&[(10, "Старая"), (20, "Ещё")]));
    assert!(app.session.folder_creation.is_some());
    assert!(app.session.folder_creation_warning.is_some());
    let _ = app.update(Msg::AcknowledgeFolderCreation(window, 0, request));
    assert!(app.session.folder_creation.is_none());
    assert_eq!(app.session.folders.len(), 2);
    assert!(app.session.folder_creation_warning.is_some());
    let _ = app.update(Msg::ReorderFolder(window, 10, false));
    assert!(app.session.folder_reorder.is_some());
    let _ = app.update(Msg::ConfirmNewFolder(window, 1));
    assert!(
        app.session.folder_creation.is_none(),
        "reorder blocks stale create"
    );
}

#[test]
fn last_chat_gone_can_acknowledge_unconfirmed_creation_from_sidebar() {
    let mut app = app();
    let window = app.main_window;
    app.session.chats.insert(1, chat_item("Последний", false));
    let _ = app.update(Msg::ChatMenu(window, Some(1)));
    let _ = app.update(Msg::ChatOp(window, 1, ChatOp::NewFolder));
    let _ = app.update(Msg::FolderName(window, 1, "Новая".into()));
    let _ = app.update(Msg::ConfirmNewFolder(window, 1));
    let request = app.session.folder_creation.as_ref().unwrap().request;
    let _ = app.update(Msg::AcknowledgeFolderCreation(window, 0, request));
    assert!(
        app.session.folder_creation.is_some(),
        "unanswered request stays locked"
    );
    let _ = app.update(Msg::FolderCreated(window, 0, request, Ok(30)));
    let _ = app.update(Msg::ChatMenu(window, None));
    app.session.chats.remove(&1);
    assert!(pane(&app).list.menu.is_none());
    assert!(app.session.chats.is_empty());
    assert!(app.session.folders.is_empty());
    let _ = app.update(Msg::AcknowledgeFolderCreation(WinId::unique(), 0, request));
    let _ = app.update(Msg::AcknowledgeFolderCreation(window, 1, request));
    let _ = app.update(Msg::AcknowledgeFolderCreation(window, 0, request + 1));
    assert!(
        app.session.folder_creation.is_some(),
        "stale recovery ignored"
    );
    let clicked = (150..280).step_by(5).any(|y| {
        sandbox::click(&mut app, window, iced::Point::new(85.0, y as f32))
            .iter()
            .any(|msg| {
                matches!(msg, Msg::AcknowledgeFolderCreation(id, 0, r)
                    if *id == window && *r == request)
            })
    });
    assert!(
        clicked,
        "sidebar recovery button must be reachable without chats or folders"
    );
    assert!(app.session.folder_creation.is_none());
    assert!(
        app.session.folders.is_empty(),
        "acknowledgment cannot invent a folder"
    );
    let _ = app.update(Msg::FolderCreated(window, 0, request, Ok(30)));
    assert!(
        app.session.folders.is_empty(),
        "late reply cannot invent a folder"
    );
}

#[test]
fn closed_origin_keeps_creation_serialized_until_authoritative_update() {
    let mut app = app();
    let other = WinId::unique();
    app.session.panes.insert(other, ChatPane::default());
    app.session.chats.insert(1, chat_item("Чат", false));
    let _ = app.update(Msg::ChatMenu(other, Some(1)));
    let _ = app.update(Msg::ChatOp(other, 1, ChatOp::NewFolder));
    let _ = app.update(Msg::FolderName(other, 1, "Папка".into()));
    let _ = app.update(Msg::ConfirmNewFolder(other, 1));
    let request = app.session.folder_creation.as_ref().unwrap().request;
    let _ = app.update(Msg::WindowClosed(other));
    let main = app.main_window;
    let _ = app.update(Msg::ChatMenu(main, Some(1)));
    let _ = app.update(Msg::ChatOp(main, 1, ChatOp::NewFolder));
    let _ = app.update(Msg::FolderName(main, 1, "Папка".into()));
    let _ = app.update(Msg::ConfirmNewFolder(main, 1));
    assert_eq!(
        app.session.folder_creation.as_ref().unwrap().request,
        request
    );
    let _ = app.update(Msg::FolderCreated(other, 0, request, Ok(77)));
    assert!(app.session.folder_creation.is_some());
    assert!(app.session.folders.is_empty());
    td(&mut app, folders_update(&[(77, "Папка")]));
    assert!(app.session.folder_creation.is_none());
    assert_eq!(app.session.folders, [(77, "Папка".into())]);
}

#[test]
fn profile_panel_opens_for_the_shown_chat_and_ignores_stale_answers() {
    let mut app = app();
    open(&mut app, 1, &[(10, "a")]);
    let window = app.main_window;
    let _ = app.update(Msg::ToggleProfile(window));
    assert_eq!(pane(&app).profile, Some((1, None)));
    let profile = td::Profile {
        usernames: vec!["rust".into()],
        subtitle: "3 участника".into(),
        about: crate::app::rich::plain("Чат о Rust"),
        members: vec![(7, "Игорь".into())],
        is_member: true,
        ..Default::default()
    };
    let _ = app.update(Msg::ProfileLoaded(window, 2, Ok(td::Profile::default())));
    assert_eq!(
        pane(&app).profile,
        Some((1, None)),
        "answer for another chat"
    );
    let _ = app.update(Msg::ProfileLoaded(window, 1, Ok(profile.clone())));
    assert_eq!(pane(&app).profile, Some((1, Some(Ok(profile)))));
    let _ = app.update(Msg::ToggleProfile(window));
    assert_eq!(pane(&app).profile, None);
}

#[test]
fn counts_agree_with_their_numbers() {
    let forms = ["подписчик", "подписчика", "подписчиков"];
    assert_eq!(
        td::count_label(1_088_881, forms),
        "1\u{a0}088\u{a0}881 подписчик"
    );
    assert_eq!(td::count_label(3, forms), "3 подписчика");
    assert_eq!(td::count_label(12, forms), "12 подписчиков");
    assert_eq!(td::count_label(111, forms), "111 подписчиков");
    assert_eq!(td::count_label(1000, forms), "1\u{a0}000 подписчиков");
}

#[test]
fn profile_actions_ask_before_leaving_and_open_linked_chats() {
    let mut app = app();
    open(&mut app, 1, &[(10, "a")]);
    let window = app.main_window;
    // A forwarded channel's profile opens over the current chat.
    let _ = app.update(Msg::OpenProfile(window, 5));
    let data = td::Profile {
        usernames: vec!["pekagame".into(), "pekarnya".into()],
        linked_chat_id: 6,
        is_channel: true,
        is_member: true,
        ..Default::default()
    };
    let _ = app.update(Msg::ProfileLoaded(window, 5, Ok(data)));
    assert_eq!(active(&app), Some(1));
    assert_eq!(
        app.update(Msg::Profile(window, ProfileAction::Leave))
            .units(),
        0,
        "asks first"
    );
    assert!(pane(&app).confirm_leave);
    assert_eq!(
        app.update(Msg::Profile(window, ProfileAction::Leave))
            .units(),
        1
    );
    let _ = app.update(Msg::Profile(window, ProfileAction::Left(5, Ok(()))));
    assert_eq!(pane(&app).profile, None, "the panel of a left chat closes");

    let _ = app.update(Msg::OpenProfile(window, 5));
    let _ = app.update(Msg::ProfileLoaded(
        window,
        5,
        Ok(td::Profile {
            linked_chat_id: 6,
            ..Default::default()
        }),
    ));
    let _ = app.update(Msg::Profile(window, ProfileAction::Discuss));
    assert_eq!(active(&app), Some(6), "«Обсуждение» opens the linked chat");
}

#[test]
fn user_status_reads_naturally() {
    use tdlib_rs::enums::UserStatus;
    let online: UserStatus =
        serde_json::from_value(json!({"@type": "userStatusOnline", "expires": 0})).unwrap();
    assert_eq!(td::status_label(&online), "в сети");
    let recently: UserStatus = serde_json::from_value(
        json!({"@type": "userStatusRecently", "by_my_privacy_settings": false}),
    )
    .unwrap();
    assert_eq!(td::status_label(&recently), "был(а) недавно");
}

#[test]
fn photo_viewer_steps_through_chat_photos_and_closes_on_esc() {
    let mut app = app();
    let photo = |id: i64, file: i32| {
        let mut m = message(1, id, "");
        m["content"] = json!({"@type": "messagePhoto", "has_spoiler": false, "show_caption_above_media": false,
            "is_secret": false,
            "caption": {"@type": "formattedText", "text": "", "entities": []},
            "photo": {"@type": "photo", "has_stickers": false, "sizes": [{"@type": "photoSize",
                "type": "x", "width": 800, "height": 600, "progressive_sizes": [],
                "photo": {"@type": "file", "id": file, "size": 1000, "expected_size": 1000,
                    "local": {"@type": "localFile", "path": "", "can_be_downloaded": true,
                        "can_be_deleted": false, "is_downloading_active": false,
                        "is_downloading_completed": false, "download_offset": 0,
                        "downloaded_prefix_size": 0, "downloaded_size": 0},
                    "remote": {"@type": "remoteFile", "id": "", "unique_id": "",
                        "is_uploading_active": false, "is_uploading_completed": true,
                        "uploaded_size": 1000}}}]}});
        serde_json::from_value::<Message>(m).unwrap()
    };
    let window = app.main_window;
    app.session.panes.get_mut(&window).unwrap().switch_to(1);
    let history = vec![photo(10, 100), photo(11, 101), photo(12, 102)];
    let _ = app.update(Msg::Pane(window, PaneMsg::HistoryLoaded(1, Ok(history))));

    let _ = app.update(Msg::ViewPhoto(window, 101));
    let key = |app: &mut App, named| {
        let _ = app.update(Msg::Key(
            window,
            keyboard::Key::Named(named),
            keyboard::Modifiers::empty(),
        ));
    };
    key(&mut app, keyboard::key::Named::ArrowRight);
    assert_eq!(
        app.session.photo_view.as_ref().map(|v| v.file_id),
        Some(102)
    );
    key(&mut app, keyboard::key::Named::ArrowRight);
    assert_eq!(
        app.session.photo_view.as_ref().map(|v| v.file_id),
        Some(102),
        "stops at the last"
    );
    key(&mut app, keyboard::key::Named::ArrowLeft);
    key(&mut app, keyboard::key::Named::ArrowLeft);
    assert_eq!(
        app.session.photo_view.as_ref().map(|v| v.file_id),
        Some(100)
    );

    // A late decode of a photo already left behind is dropped.
    let _ = app.update(Msg::ViewerDecoded(101, Ok((1, 1, vec![0; 4]))));
    assert!(app.session.photo_view.as_ref().unwrap().image.is_none());

    // Esc must close only the viewer, not an unrelated open search bar.
    let _ = app.update(Msg::Pane(window, PaneMsg::SearchToggle));
    assert!(pane(&app).search.is_some());
    key(&mut app, keyboard::key::Named::Escape);
    assert!(app.session.photo_view.is_none());
    assert!(pane(&app).search.is_some(), "Esc went to the viewer only");
}

#[test]
fn picker_inserts_emoji_at_the_cursor_and_keeps_loaded_sets() {
    let mut app = app();
    open(&mut app, 1, &[(10, "a")]);
    pane_msg(&mut app, typed("привет "));
    pane_msg(&mut app, PaneMsg::TogglePicker);
    assert_eq!(pane(&app).picker, Some(picker::Tab::Emoji));
    pane_msg(&mut app, PaneMsg::InsertEmoji("🔥".into()));
    assert_eq!(pane(&app).compose.text(), "привет 🔥");

    let _ = app.update(Msg::StickerSets(Ok(vec![(5, "Котики".into())])));
    let _ = app.update(Msg::StickerSet(5, Ok(Vec::new())));
    pane_msg(&mut app, PaneMsg::PickerTab(picker::Tab::Set(5)));
    assert_eq!(pane(&app).picker, Some(picker::Tab::Set(5)));
    assert!(
        app.session.stickers.set_stickers.contains_key(&5),
        "loaded once, kept for later"
    );
    pane_msg(&mut app, PaneMsg::TogglePicker);
    assert_eq!(pane(&app).picker, None);
}

#[test]
fn drafts_are_saved_when_leaving_and_loaded_into_an_empty_field() {
    let mut app = app();
    for id in [1, 2] {
        app.session
            .chats
            .insert(id, chat_item(&id.to_string(), false));
    }
    select(&mut app, 1, &[(10, "a")]);
    pane_msg(&mut app, typed("недописанное"));
    select(&mut app, 2, &[(20, "b")]);
    assert_eq!(
        app.session.synced_drafts.get(&1).map(String::as_str),
        Some("недописанное")
    );

    // A draft from the phone lands in an empty field, not over typed text.
    pane_msg(&mut app, PaneMsg::DraftLoaded(2, Ok("с телефона".into())));
    assert_eq!(pane(&app).compose.text(), "с телефона");
    pane_msg(&mut app, PaneMsg::DraftLoaded(2, Ok("другое".into())));
    assert_eq!(pane(&app).compose.text(), "с телефона");

    // Sent: the draft is gone in Telegram as well, nothing to save on leaving.
    pane_msg(&mut app, PaneMsg::Send);
    assert_eq!(
        app.session.synced_drafts.get(&2).map(String::as_str),
        Some("")
    );
    assert!(app.sync_draft(2, Some(String::new())).units() == 0);

    // An untouched empty field (the phone's draft not loaded yet) saves
    // nothing: the draft made elsewhere survives.
    app.session.chats.get_mut(&1).unwrap().draft = Some(tdlib_rs::types::FormattedText {
        text: "с телефона".into(),
        entities: Vec::new(),
    });
    app.session.synced_drafts.remove(&1);
    // A fresh open (as after a restart), not the kept background tab.
    app.session.background.remove(&1);
    select(&mut app, 1, &[(10, "a")]);
    select(&mut app, 2, &[(20, "b")]);
    assert_eq!(
        app.session.synced_drafts.get(&1),
        None,
        "nothing was saved for chat 1"
    );

    // While editing an own message, the draft set aside is what is saved.
    app.session.background.remove(&1);
    select(&mut app, 1, &[(10, "a")]);
    pane_msg(&mut app, typed("черновик"));
    pane_msg(&mut app, PaneMsg::EditLoaded(1, 10, Ok("правка".into())));
    select(&mut app, 2, &[(20, "b")]);
    assert_eq!(
        app.session.synced_drafts.get(&1).map(String::as_str),
        Some("черновик")
    );

    // TDLib reports drafts for the list.
    td(
        &mut app,
        json!({"@type": "updateChatDraftMessage", "chat_id": 1, "positions": [],
               "draft_message": {"@type": "draftMessage", "date": 0, "effect_id": "0",
                   "content": {"@type": "draftMessageContentText",
                       "text": {"@type": "formattedText", "text": "позже", "entities": []}}}}),
    );
    assert_eq!(
        app.session.chats[&1]
            .draft
            .as_ref()
            .map(|d| d.text.as_str()),
        Some("позже")
    );
}

#[test]
fn locked_account_waits_for_the_password_and_keys_follow_changes() {
    use crate::app::password::PasswordMsg;
    let mut app = app();
    app.session.auth = Auth::Starting;
    let salt = crate::lock::new_salt();
    let key = crate::lock::derive("секрет", &salt).unwrap();
    app.settings.accounts = vec![Account {
        slot: 0,
        user_id: Some(5),
        name: "Alice".into(),
        lock_salt: Some(salt.clone()),
        lock_check: Some(crate::lock::check_value(&key)),
    }];
    td(
        &mut app,
        json!({"@type": "updateAuthorizationState",
               "authorization_state": {"@type": "authorizationStateWaitTdlibParameters"}}),
    );
    assert_eq!(
        app.session.auth,
        Auth::Locked {
            error: None,
            forgot: false
        }
    );

    let _ = app.update(Msg::Password(PasswordMsg::Unlocked(Err(
        "неверный пароль".into()
    ))));
    assert_eq!(
        app.session.auth,
        Auth::Locked {
            error: Some("неверный пароль".into()),
            forgot: false
        }
    );
    let _ = app.update(Msg::Password(PasswordMsg::Unlocked(Ok(key))));
    assert_eq!(app.session.db_key, Some(key));
    // TDLib still refuses: the data has another key.
    let _ = app.update(Msg::ParametersSet(Err(
        "Wrong database encryption key (401)".into(),
    )));
    assert!(matches!(
        app.session.auth,
        Auth::Locked { error: Some(_), .. }
    ));
    assert_eq!(app.session.db_key, None);

    // Settings: the form checks its fields before any work.
    app.session.auth = Auth::Ready;
    let _ = app.update(Msg::Password(PasswordMsg::New("abc".into())));
    let _ = app.update(Msg::Password(PasswordMsg::Apply { remove: false }));
    assert!(matches!(&app.session.password_form.message, Some(Err(e)) if e.contains("4")));
    let _ = app.update(Msg::Password(PasswordMsg::New("abcd".into())));
    let _ = app.update(Msg::Password(PasswordMsg::Repeat("abce".into())));
    let _ = app.update(Msg::Password(PasswordMsg::Apply { remove: false }));
    assert!(
        matches!(&app.session.password_form.message, Some(Err(e)) if e.contains("не совпадают"))
    );

    // TDLib re-keyed: the archive follows (on a worker thread, so `update`
    // only starts it here; `ArchiveRekeyed` is what it reports back with),
    // settings keep salt and check.
    app.session.db_key = Some(key);
    app.session.archive.as_mut().unwrap().set_key(Some(key));
    let new_salt = crate::lock::new_salt();
    let new_key = crate::lock::derive("новый", &new_salt).unwrap();
    let _ = app.update(Msg::Password(PasswordMsg::Applied(
        app.session.slot,
        app.session.client_id,
        Ok((Some(new_key), new_salt.clone(), String::new())),
    )));
    let _ = app.update(Msg::Password(PasswordMsg::ArchiveRekeyed(
        app.session.slot,
        app.session.client_id,
        Some(new_key),
        new_salt.clone(),
        String::new(),
        Ok(()),
    )));
    assert_eq!(app.session.db_key, Some(new_key));
    assert_eq!(
        app.settings.accounts[0].lock_salt.as_deref(),
        Some(new_salt.as_str())
    );
    // Removed: nothing left to ask at the next start. The archive is
    // already `None` here (taken, not given back, by the unpolled task
    // above), so this round finishes synchronously with no archive to wait
    // for.
    let _ = app.update(Msg::Password(PasswordMsg::Applied(
        app.session.slot,
        app.session.client_id,
        Ok((None, String::new(), String::new())),
    )));
    assert_eq!(app.session.db_key, None);
    assert_eq!(app.locked_slot(), None);
}

#[test]
fn trash_closes_all_unpinned_tabs_and_keeps_their_drafts() {
    let mut app = app();
    select(&mut app, 1, &[(10, "a")]);
    let _ = app.update(Msg::TogglePin(1));
    select(&mut app, 2, &[(20, "b")]);
    pane_msg(&mut app, typed("не забыть"));
    select(&mut app, 3, &[(30, "c")]);
    assert_eq!(app.session.tabs.chats.len(), 3);

    let _ = app.update(Msg::CloseAllTabs);
    assert_eq!(app.session.tabs.chats, [1], "the pinned tab stays");
    assert_eq!(active(&app), Some(1), "the main window moves to it");
    assert_eq!(
        app.session.synced_drafts.get(&2).map(String::as_str),
        Some("не забыть"),
        "text typed in a closed tab becomes its draft"
    );

    // With nothing pinned the main window ends up empty.
    let _ = app.update(Msg::TogglePin(1));
    let _ = app.update(Msg::CloseAllTabs);
    assert!(app.session.tabs.chats.is_empty());
    assert_eq!(active(&app), None);
}

#[test]
fn own_pins_come_first_in_the_bar_and_survive_reopening() {
    let mut app = app();
    open(&mut app, 1, &[(10, "правила"), (11, "важное"), (12, "c")]);
    pane_msg(&mut app, PaneMsg::PinnedLoaded(1, Ok(page(1, [10]))));
    pane_msg(&mut app, PaneMsg::PinLocal(11, true));
    let bar: Vec<(i64, bool)> = pane(&app)
        .pins()
        .iter()
        .map(|(m, own)| (m.id, *own))
        .collect();
    assert_eq!(
        bar,
        [(11, true), (10, false)],
        "own pin first, then the chat's"
    );
    assert_eq!(pane(&app).pinned_shown, 0, "the new pin is shown");

    // Pinning a message the chat has pinned too: listed once, as own.
    pane_msg(&mut app, PaneMsg::PinLocal(10, true));
    assert_eq!(pane(&app).pins().len(), 2);

    // Kept locally: a reopened chat has them without asking Telegram.
    open(&mut app, 1, &[(10, "правила"), (11, "важное"), (12, "c")]);
    let own: Vec<i64> = pane(&app).local_pins.iter().map(|m| m.id).collect();
    assert_eq!(own, [10, 11]);
    assert_eq!(pane(&app).local_pins[1].text, "важное");

    pane_msg(&mut app, PaneMsg::PinLocal(10, false));
    pane_msg(&mut app, PaneMsg::PinLocal(11, false));
    assert!(pane(&app).local_pins.is_empty());
}

#[test]
fn roles_are_marked_and_member_tags_come_from_messages() {
    let mut app = app();
    let tagged = |id: i64, user: i64, tag: &str| {
        let mut m = message(1, id, "x");
        m["sender_id"] = json!({"@type": "messageSenderUser", "user_id": user});
        m["sender_tag"] = json!(tag);
        MsgItem::from(&serde_json::from_value::<Message>(m).unwrap())
    };
    let _ = app.update(Msg::RolesLoaded(
        1,
        Ok(HashMap::from([
            (
                7,
                td::Role {
                    owner: true,
                    admin: true,
                    tag: String::new(),
                },
            ),
            (
                8,
                td::Role {
                    owner: false,
                    admin: true,
                    tag: "модератор".into(),
                },
            ),
        ])),
    ));
    assert_eq!(
        app.sender_badge(&tagged(1, 7, "")),
        Some(("владелец".into(), true))
    );
    assert_eq!(
        app.sender_badge(&tagged(2, 8, "старый")),
        Some(("модератор".into(), true))
    );
    // A member without a role: the tag the message carries, not a role.
    assert_eq!(
        app.sender_badge(&tagged(3, 9, "дизайнер")),
        Some(("дизайнер".into(), false))
    );
    assert_eq!(app.sender_badge(&tagged(4, 10, "")), None);
}

#[test]
fn selected_text_is_copied_instead_of_the_whole_message() {
    let mut app = app();
    open(&mut app, 1, &[(10, "первая строка\nвторая строка")]);
    // Drag from "строка" of the first line into the second one.
    pane_msg(&mut app, PaneMsg::TextPress(10, "первая ".len()));
    pane_msg(
        &mut app,
        PaneMsg::TextDrag(10, "первая строка\nвторая".len()),
    );
    let _ = app.update(Msg::MouseReleased);
    assert_eq!(
        pane(&app).selected_text().as_deref(),
        Some("строка\nвторая")
    );
    // After release further moves do not change it.
    pane_msg(&mut app, PaneMsg::TextDrag(10, 3));
    assert_eq!(
        pane(&app).selected_text().as_deref(),
        Some("строка\nвторая")
    );

    // A plain click (no drag) selects nothing; a click elsewhere clears.
    pane_msg(&mut app, PaneMsg::TextPress(10, 4));
    assert_eq!(pane(&app).selected_text(), None);
    pane_msg(&mut app, PaneMsg::TextPress(10, 0));
    pane_msg(&mut app, PaneMsg::TextDrag(10, "первая".len()));
    pane_msg(&mut app, PaneMsg::ClickOutside);
    assert_eq!(pane(&app).text_selection, None);
}

#[test]
fn user_card_opens_loads_and_asks_before_blocking() {
    use crate::app::card::CardMsg;
    let mut app = app();
    let window = app.main_window;
    let _ = app.update(Msg::Card(CardMsg::Open(window, 7)));
    let data = td::UserCard {
        name: "Дмитрий".into(),
        username: "lynulx".into(),
        status: "был(а) недавно".into(),
        bio: "coito ergo sum".into(),
        chat_id: 7,
        ..Default::default()
    };
    // An answer for someone else is ignored.
    let _ = app.update(Msg::Card(CardMsg::Loaded(8, Ok(td::UserCard::default()))));
    assert!(app.session.user_card.as_ref().unwrap().data.is_none());
    let _ = app.update(Msg::Card(CardMsg::Loaded(7, Ok(data))));
    assert!(app.session.user_card.as_ref().unwrap().data.is_some());

    // The first «Заблокировать» only asks; the second one sends.
    assert_eq!(app.update(Msg::Card(CardMsg::Block)).units(), 0);
    assert_eq!(app.update(Msg::Card(CardMsg::Block)).units(), 1);

    // «Написать» opens the chat and closes the card; Esc closes too.
    let _ = app.update(Msg::Card(CardMsg::Done(Ok(()))));
    let _ = app.update(Msg::Card(CardMsg::Loaded(
        7,
        Ok(td::UserCard {
            chat_id: 7,
            ..Default::default()
        }),
    )));
    let _ = app.update(Msg::Card(CardMsg::Message));
    assert!(app.session.user_card.is_none());
    assert_eq!(active(&app), Some(7));
    let _ = app.update(Msg::Card(CardMsg::Open(window, 7)));
    let _ = app.update(Msg::Key(
        window,
        keyboard::Key::Named(keyboard::key::Named::Escape),
        keyboard::Modifiers::empty(),
    ));
    assert!(app.session.user_card.is_none());
}

#[test]
fn telegram_links_open_inside_and_others_do_not() {
    for url in [
        "https://t.me/awn_v001_bot",
        "t.me/durov/1",
        "tg://resolve?domain=x",
        "https://telegram.me/x",
        "https://www.t.me/+abc",
        "https://name.t.me",
    ] {
        assert!(td::is_telegram_link(url), "{url}");
    }
    for url in [
        "https://example.com/t.me/x",
        "https://nott.me/x",
        "mailto:a@t.me",
    ] {
        assert!(!td::is_telegram_link(url), "{url}");
    }

    let mut app = app();
    open(&mut app, 1, &[(10, "a")]);
    let window = app.main_window;
    let url = "https://t.me/x".to_owned();
    let _ = app.update(Msg::LinkResolved(
        window,
        url.clone(),
        Ok(Some(td::LinkTarget::Chat(2))),
    ));
    assert_eq!(active(&app), Some(2), "a chat link opens the chat");

    // An invite asks first; «Вступить» and a success open the chat.
    let invite = td::LinkTarget::Invite {
        link: "https://t.me/+abc".into(),
        title: "Клуб".into(),
        members: 12,
    };
    let _ = app.update(Msg::LinkResolved(window, url, Ok(Some(invite))));
    assert_eq!(
        pane(&app).confirm_join,
        Some(("https://t.me/+abc".into(), "Клуб".into(), 12))
    );
    pane_msg(&mut app, PaneMsg::JoinChat("https://t.me/+abc".into()));
    assert_eq!(pane(&app).confirm_join, None);
    let _ = app.update(Msg::Joined(window, Ok(Some(3))));
    assert_eq!(active(&app), Some(3));
}

#[test]
fn monospace_text_is_copied_by_a_click() {
    let mut app = app();
    open(&mut app, 1, &[(10, "код")]);
    let tasks = app.update(Msg::Pane(
        app.main_window,
        PaneMsg::Link(Link::Copy("git push".into())),
    ));
    assert!(tasks.units() >= 1, "clipboard write and notice timer");
    assert_eq!(app.session.notice.as_deref(), Some("Скопировано"));
    let _ = app.update(Msg::ClearNotice);
    assert_eq!(app.session.notice, None);
}

#[test]
fn closing_the_recording_window_cancels_it() {
    let mut app = app();
    let window = open_window(&mut app, 1, &[]);
    let _ = app.update(Msg::RecordStarted(
        window,
        1,
        Ok(playback::RecorderHandle(Recorder::stub())),
    ));
    assert!(app.session.playback.recording.is_some());

    let _ = app.update(Msg::WindowClosed(window));
    assert!(app.session.playback.recording.is_none());
}

#[test]
fn switching_the_recording_windows_chat_cancels_it() {
    let mut app = app();
    let window = open_window(&mut app, 1, &[]);
    let _ = app.update(Msg::RecordStarted(
        window,
        1,
        Ok(playback::RecorderHandle(Recorder::stub())),
    ));
    assert!(app.session.playback.recording.is_some());

    let _ = app.update(Msg::Pane(window, PaneMsg::SelectChat(2)));
    assert!(app.session.playback.recording.is_none());
}

#[test]
fn record_started_for_a_window_closed_meanwhile_is_cancelled() {
    let mut app = app();
    let window = open_window(&mut app, 1, &[]);
    let _ = app.update(Msg::WindowClosed(window));

    // The recorder that opened before the window closed reports back after.
    let _ = app.update(Msg::RecordStarted(
        window,
        1,
        Ok(playback::RecorderHandle(Recorder::stub())),
    ));
    assert!(app.session.playback.recording.is_none());
}

#[test]
fn two_shows_and_one_hide_keep_an_animation_wanted() {
    let mut app = app();
    let _ = app.update(Msg::AnimShow(305, media::Motion::Video));
    // A second copy of the same file on screen (forwarded, or a chat open
    // in two windows): hiding one must not stop the other.
    let _ = app.update(Msg::AnimShow(305, media::Motion::Video));
    assert!(app.session.playback.waiting(305));

    let _ = app.update(Msg::AnimHide(305));
    assert!(
        app.session.playback.waiting(305),
        "the other copy is still shown"
    );

    let _ = app.update(Msg::AnimHide(305));
    assert!(!app.session.playback.waiting(305));
}

#[test]
fn channel_posts_show_no_sender_name_or_avatar() {
    let mut app = app();
    // A channel: `ChatType::Supergroup` with `is_channel`. Its posts arrive
    // as incoming messages sent by the channel itself.
    app.session.chats.insert(
        1,
        ChatItem {
            kind: Some(tdlib_rs::enums::ChatType::Supergroup(
                tdlib_rs::types::ChatTypeSupergroup {
                    supergroup_id: 55,
                    is_channel: true,
                },
            )),
            ..chat_item("Канал", false)
        },
    );
    let mut post = message(1, 10, "пост");
    post["sender_id"] = json!({"@type": "messageSenderChat", "chat_id": 1});
    let post: tdlib_rs::types::Message = serde_json::from_value(post).unwrap();
    let item = MsgItem::from(&post);
    assert!(
        !app.shows_sender(&item),
        "a channel post shows no name or avatar, unlike a group message"
    );

    // The same message in an ordinary (non-channel) group does get one.
    app.session.chats.get_mut(&1).unwrap().kind = Some(tdlib_rs::enums::ChatType::Supergroup(
        tdlib_rs::types::ChatTypeSupergroup {
            supergroup_id: 55,
            is_channel: false,
        },
    ));
    assert!(app.shows_sender(&item));
}

#[test]
fn truncation_keeps_flags_and_zwj_sequences_whole() {
    // Two regional-indicator code points make one flag; cutting between
    // them used to leave "🇺" (a boxed letter) instead of "🇺🇸".
    let flag = "🇺🇸";
    let (cut, shortened) = view::take_clusters(flag, 1);
    assert_eq!(cut, flag, "a flag is one cluster, not split in half");
    assert!(!shortened);

    // MAN-ZWJ-WOMAN-ZWJ-GIRL: a family emoji joined by zero-width joiners.
    let family = "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}";
    let (cut, shortened) = view::take_clusters(family, 1);
    assert_eq!(cut, family, "the whole ZWJ chain is one cluster");
    assert!(!shortened);
}

#[test]
fn reply_preview_is_not_fetched_for_a_chat_shown_nowhere() {
    let mut app = app();
    // Chat 9 is not open in any window pane or background tab.
    let post = reply_message(9, 2, "ответ", 1);
    let task = app.update(Msg::Td(
        0,
        Box::new(
            serde_json::from_value(json!({"@type": "updateNewMessage", "message": post})).unwrap(),
        ),
    ));
    assert_eq!(
        task.units(),
        0,
        "chat 9 is shown nowhere, so no getMessages is scheduled for its reply"
    );
}

#[test]
fn reply_previews_are_dropped_once_their_chat_closes() {
    let mut app = app();
    open(&mut app, 1, &[]);
    let _ = app.update(Msg::RepliesLoaded(vec![
        serde_json::from_value(message(1, 1, "исходное")).unwrap(),
    ]));
    assert!(app.session.reply_previews.contains_key(&(1, 1)));

    // Switching the only pane away closes chat 1 everywhere; the cache is
    // bounded to chats currently shown, so its preview goes with it on the
    // next answer that triggers the bound (any `RepliesLoaded`, even empty).
    app.session
        .panes
        .get_mut(&app.main_window)
        .unwrap()
        .switch_to(2);
    let _ = app.update(Msg::RepliesLoaded(vec![]));
    assert!(!app.session.reply_previews.contains_key(&(1, 1)));
}

#[test]
fn reply_previews_follow_edits_and_deletions() {
    let mut app = app();
    open(&mut app, 1, &[]);
    let _ = app.update(Msg::RepliesLoaded(vec![
        serde_json::from_value(message(1, 1, "исходное")).unwrap(),
    ]));
    assert_eq!(app.session.reply_previews[&(1, 1)].text, "исходное");

    td(
        &mut app,
        json!({"@type": "updateMessageContent", "chat_id": 1, "message_id": 1,
               "new_content": {"@type": "messageText", "text": {"@type": "formattedText",
                   "text": "исправлено", "entities": []}}}),
    );
    assert_eq!(
        app.session.reply_previews[&(1, 1)].text,
        "исправлено",
        "a cached preview must not keep showing the pre-edit text"
    );

    delete(&mut app, 1, &[1], false);
    assert!(
        app.session.reply_previews[&(1, 1)].deleted,
        "a foreign deletion marks the cached preview deleted, like the pane"
    );
}
