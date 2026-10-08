//! Inactive tab history is reclaimable without losing the user's place or composer.
//! These scenarios inject an explicit future `MemorySweep` timestamp; no wall-clock waits.

use super::*;
use iced::widget::scrollable::AbsoluteOffset;
use std::time::{Duration, Instant};

fn memory_history_sweep(app: &mut App, at: Instant) {
    let _ = app.update(Msg::MemorySweep(at));
}

fn memory_history_background(app: &mut App) -> Instant {
    select(app, 2, &[(20, "foreground")]);
    assert_eq!(app.session.background[&1].chat_id, Some(1));
    Instant::now()
}

fn memory_history_load(app: &mut App, chat: i64, messages: Vec<Message>) {
    let _ = app.update(Msg::Pane(
        app.main_window,
        PaneMsg::HistoryLoaded(chat, Ok(messages)),
    ));
}

fn memory_history_request(app: &App) -> u64 {
    pane(app)
        .cold
        .as_ref()
        .and_then(|cold| cold.request)
        .expect("cold tab requested its saved history")
}

fn memory_history_reply(
    app: &mut App,
    request: u64,
    result: Result<Vec<Message>, String>,
) -> iced::Task<Msg> {
    app.update(Msg::Pane(
        app.main_window,
        PaneMsg::ColdHistoryLoaded(1, request, result),
    ))
}

#[test]
fn memory_history_survives_until_five_minutes_then_releases_messages_and_capacity() {
    let mut app = app();
    let _ = app.update(Msg::Pane(app.main_window, PaneMsg::SelectChat(1)));
    memory_history_load(&mut app, 1, page(1, 1..=100));
    assert_eq!(pane(&app).messages.len(), 100);
    let start = memory_history_background(&mut app);

    memory_history_sweep(&mut app, start + Duration::from_secs(299));
    assert_eq!(app.session.background[&1].messages.len(), 100);
    memory_history_sweep(&mut app, start + Duration::from_secs(301));
    let background = &app.session.background[&1];
    assert!(
        background.messages.is_empty(),
        "confirmed history must be dropped"
    );
    assert_eq!(
        background.messages.capacity(),
        0,
        "history backing memory must be freed"
    );
    assert!(
        app.session.tabs.chats.contains(&1),
        "the tab remains available"
    );
}

#[test]
fn memory_history_keeps_main_and_every_separate_window_loaded() {
    let mut app = app();
    select(&mut app, 1, &[(10, "background")]);
    let start = memory_history_background(&mut app);
    let first = open_window(&mut app, 3, &[(30, "first")]);
    let second = open_window(&mut app, 4, &[(40, "second")]);
    memory_history_sweep(&mut app, start + Duration::from_secs(301));

    assert_eq!(shown(&app), [(20, "foreground", false)]);
    assert_eq!(shown_in(&app, first), [(30, "first", false)]);
    assert_eq!(shown_in(&app, second), [(40, "second", false)]);
    assert!(app.session.background[&1].messages.is_empty());
}

#[test]
fn memory_history_keeps_draft_reply_and_active_edit_through_cold_restore() {
    let mut app = app();
    select(&mut app, 1, &[(10, "original")]);
    pane_msg(&mut app, typed("my unsent draft"));
    pane_msg(&mut app, PaneMsg::Reply(10));
    let start = memory_history_background(&mut app);
    memory_history_sweep(&mut app, start + Duration::from_secs(301));
    let _ = app.update(Msg::SelectTab(1));
    assert_eq!(pane(&app).compose.text(), "my unsent draft");
    assert_eq!(pane(&app).reply_to, Some(10));
    let request = memory_history_request(&app);
    // The authoritative reply has newer text than the protected cached row;
    // preserving the reply target must not pin obsolete message content.
    let restored = vec![serde_json::from_value(message(1, 10, "edited on server")).unwrap()];
    drop(memory_history_reply(&mut app, request, Ok(restored)));
    assert_eq!(shown(&app), [(10, "edited on server", false)]);
    assert_eq!(pane(&app).compose.text(), "my unsent draft");
    assert_eq!(pane(&app).reply_to, Some(10));

    // Editing replaces the composer temporarily, but the original draft must
    // still be present when the user cancels the edit after a second unload.
    pane_msg(
        &mut app,
        PaneMsg::EditLoaded(1, 10, Ok("edit in progress".into())),
    );
    select(&mut app, 2, &[(20, "foreground")]);
    let again = Instant::now();
    memory_history_sweep(&mut app, again + Duration::from_secs(301));
    let _ = app.update(Msg::SelectTab(1));
    assert_eq!(pane(&app).compose.text(), "edit in progress");
    assert_eq!(pane(&app).editing.as_ref().map(|edit| edit.0), Some(10));
    pane_msg(&mut app, PaneMsg::CancelEdit);
    assert_eq!(pane(&app).compose.text(), "my unsent draft");
}

#[test]
fn memory_history_cold_tab_ignores_live_edits_and_deletions_until_latest_refetch() {
    let mut app = app();
    select(&mut app, 1, &[(10, "original")]);
    let start = memory_history_background(&mut app);
    memory_history_sweep(&mut app, start + Duration::from_secs(301));

    new_message(&mut app, 1, 11, "live");
    td(
        &mut app,
        json!({"@type": "updateMessageContent", "chat_id": 1, "message_id": 10,
               "new_content": {"@type": "messageText", "text": {"@type": "formattedText",
                   "text": "edited", "entities": []}}}),
    );
    delete(&mut app, 1, &[10], false);
    assert!(
        app.session.background[&1].messages.is_empty(),
        "updates must not refill a cold pane"
    );

    let _ = app.update(Msg::SelectTab(1));
    assert!(
        pane(&app).messages.is_empty(),
        "fresh history has not arrived yet"
    );
    let request = memory_history_request(&app);
    drop(memory_history_reply(&mut app, request, Ok(page(1, [11]))));
    assert_eq!(
        pane(&app)
            .messages
            .last()
            .map(|item| (item.id, item.text.as_str())),
        Some((11, "m"))
    );
}

#[test]
fn memory_history_reactivation_restarts_five_minute_countdown() {
    let mut app = app();
    select(&mut app, 1, &[(10, "history")]);
    let first = memory_history_background(&mut app);
    memory_history_sweep(&mut app, first + Duration::from_secs(299));
    let _ = app.update(Msg::SelectTab(1));
    let second = memory_history_background(&mut app);
    memory_history_sweep(&mut app, second + Duration::from_secs(299));
    assert_eq!(app.session.background[&1].messages.len(), 1);
    memory_history_sweep(&mut app, second + Duration::from_secs(301));
    assert!(app.session.background[&1].messages.is_empty());
}

#[test]
fn memory_history_pending_send_reconciles_after_confirmed_history_is_freed() {
    let mut app = app();
    select(&mut app, 1, &[(10, "confirmed")]);
    let mut pending = message(1, 100, "sending");
    pending["is_outgoing"] = json!(true);
    pending["sending_state"] = json!({"@type": "messageSendingStatePending", "sending_id": 1});
    td(
        &mut app,
        json!({"@type": "updateNewMessage", "message": pending}),
    );
    let start = memory_history_background(&mut app);
    memory_history_sweep(&mut app, start + Duration::from_secs(301));
    let cold = &app.session.background[&1];
    assert!(
        cold.messages.iter().all(|m| m.id != 10),
        "confirmed history was released"
    );
    assert!(cold.messages.iter().any(|m| m.id == 100 && m.pending));

    let mut delivered = message(1, 101, "sending");
    delivered["is_outgoing"] = json!(true);
    td(
        &mut app,
        json!({"@type": "updateMessageSendSucceeded", "message": delivered,
               "old_message_id": 100}),
    );
    let cold = &app.session.background[&1];
    assert!(
        cold.messages
            .iter()
            .any(|m| m.id == 101 && !m.pending && !m.failed)
    );
    assert!(cold.messages.iter().all(|m| m.id != 100));
    memory_history_sweep(&mut app, start + Duration::from_secs(316));
    assert!(
        app.session.background[&1].messages.is_empty(),
        "confirmed send must not remain cached indefinitely"
    );
}

#[test]
fn memory_history_restores_old_reading_message_and_intra_row_offset() {
    let mut app = app();
    app.session.chats.insert(
        1,
        ChatItem {
            last_id: 300,
            ..chat_item("chat", false)
        },
    );
    let _ = app.update(Msg::Pane(app.main_window, PaneMsg::SelectChat(1)));
    memory_history_load(&mut app, 1, page(1, 201..=300));
    pane_msg(&mut app, PaneMsg::JumpTo(42));
    pane_msg(&mut app, PaneMsg::AroundLoaded(1, 42, Ok(page(1, 30..=54))));
    assert!(!pane(&app).newest_loaded, "the reader is on an old page");
    for id in 30..=54 {
        pane_msg(&mut app, PaneMsg::Measured(id, 80.0));
    }
    app.session
        .panes
        .get_mut(&app.main_window)
        .unwrap()
        .view_size = Some((180.0, 800.0));
    let row_offset = 19.0;
    let anchor = pane(&app).offset_of(42).unwrap();
    let desired = anchor + row_offset;
    pane_mut_scroll(&mut app, desired);
    let start = memory_history_background(&mut app);
    memory_history_sweep(&mut app, start + Duration::from_secs(301));
    let _ = app.update(Msg::SelectTab(1));
    assert!(pane(&app).messages.is_empty());

    // Only the response to this activation may restore the saved old page.
    let request = memory_history_request(&app);
    drop(memory_history_reply(
        &mut app,
        request,
        Ok(page(1, 35..=49)),
    ));
    assert!(pane(&app).messages.iter().any(|m| m.id == 42));
    assert!(
        !pane(&app).newest_loaded,
        "do not replace old reading history with newest page"
    );
    let restored = pane(&app).scroll.expect("restored reading position").y;
    let expected = pane(&app).offset_of(42).unwrap() + row_offset;
    assert!(
        (restored - expected).abs() < 1.0,
        "message row and intra-row offset: {restored} vs {expected}"
    );
}

fn pane_mut_scroll(app: &mut App, y: f32) {
    app.session.panes.get_mut(&app.main_window).unwrap().scroll =
        Some(AbsoluteOffset { x: 0.0, y });
}

#[test]
fn memory_history_retry_and_late_latest_reply_preserve_draft_and_old_anchor() {
    let mut app = app();
    app.session.chats.insert(
        1,
        ChatItem {
            last_id: 300,
            ..chat_item("chat", false)
        },
    );
    let _ = app.update(Msg::Pane(app.main_window, PaneMsg::SelectChat(1)));
    memory_history_load(&mut app, 1, page(1, 201..=300));
    pane_msg(&mut app, PaneMsg::JumpTo(42));
    pane_msg(&mut app, PaneMsg::AroundLoaded(1, 42, Ok(page(1, 30..=54))));
    app.session
        .panes
        .get_mut(&app.main_window)
        .unwrap()
        .view_size = Some((180.0, 800.0));
    pane_msg(&mut app, typed("draft during reading"));
    let offset = pane(&app).offset_of(42).unwrap() + 13.0;
    pane_mut_scroll(&mut app, offset);
    let start = memory_history_background(&mut app);
    memory_history_sweep(&mut app, start + Duration::from_secs(301));
    let _ = app.update(Msg::SelectTab(1));
    let first_request = memory_history_request(&app);
    drop(memory_history_reply(
        &mut app,
        first_request,
        Err("offline".into()),
    ));
    assert_eq!(pane(&app).compose.text(), "draft during reading");
    pane_msg(&mut app, PaneMsg::HistoryLoaded(1, Ok(page(1, 290..=300))));
    drop(memory_history_reply(
        &mut app,
        first_request,
        Ok(page(1, 290..=300)),
    ));
    assert!(
        pane(&app).messages.iter().all(|m| m.id != 300),
        "late latest answer cannot reset an old reading anchor"
    );

    // Selecting another tab and returning retries the same saved anchor.
    select(&mut app, 2, &[(20, "foreground")]);
    let _ = app.update(Msg::SelectTab(1));
    let retry = memory_history_request(&app);
    assert_ne!(
        retry, first_request,
        "retry must ignore a late answer to the previous request"
    );
    drop(memory_history_reply(&mut app, retry, Ok(page(1, 36..=48))));
    assert_eq!(pane(&app).compose.text(), "draft during reading");
    assert!(pane(&app).messages.iter().any(|m| m.id == 42));
    let expected = pane(&app).offset_of(42).unwrap() + 13.0;
    let actual = pane(&app).scroll.expect("restored reading position").y;
    assert!(
        (actual - expected).abs() < 1.0,
        "retry preserves message and intra-row offset"
    );
}

#[test]
fn security_cold_retry_selecting_failed_page_again_requests_history_without_losing_protected_rows()
{
    let mut app = app();
    select(&mut app, 1, &[(10, "reply target"), (11, "confirmed")]);
    pane_msg(&mut app, PaneMsg::Reply(10));
    let mut pending = message(1, 100, "sending");
    pending["is_outgoing"] = json!(true);
    pending["sending_state"] = json!({"@type": "messageSendingStatePending", "sending_id": 1});
    td(
        &mut app,
        json!({"@type": "updateNewMessage", "message": pending}),
    );
    let start = memory_history_background(&mut app);
    memory_history_sweep(&mut app, start + Duration::from_secs(301));
    drop(app.update(Msg::SelectTab(1)));
    let failed = memory_history_request(&app);
    drop(memory_history_reply(
        &mut app,
        failed,
        Err("offline".into()),
    ));

    drop(app.update(Msg::SelectTab(1)));
    let retry = memory_history_request(&app);
    assert_ne!(retry, failed, "retry must issue a fresh request");
    assert_eq!(pane(&app).reply_to, Some(10));
    assert_eq!(
        app.reply_lookup(1, 10).map(|item| item.text.as_str()),
        Some("reply target"),
        "the protected reply must survive the failed page"
    );
    assert!(
        pane(&app).messages.iter().any(|m| m.id == 100 && m.pending),
        "an outgoing pending message must survive the failed page"
    );
    drop(memory_history_reply(&mut app, retry, Ok(page(1, 10..=11))));
    assert!(pane(&app).cold.is_none(), "the retry restores history");
    assert!(pane(&app).messages.iter().any(|m| m.id == 11));
}

#[test]
fn security_cold_retry_selecting_failed_jump_again_requests_history_without_losing_reply() {
    let mut app = app();
    select(&mut app, 1, &[(10, "reply target"), (100, "latest")]);
    pane_msg(&mut app, PaneMsg::Reply(10));
    let start = memory_history_background(&mut app);
    memory_history_sweep(&mut app, start + Duration::from_secs(301));
    drop(app.update(Msg::SelectTab(1)));
    let interrupted = memory_history_request(&app);
    pane_msg(&mut app, PaneMsg::JumpTo(42));
    pane_msg(
        &mut app,
        PaneMsg::AroundLoaded(1, 42, Err("offline".into())),
    );

    drop(app.update(Msg::SelectTab(1)));
    let retry = memory_history_request(&app);
    assert_ne!(
        retry, interrupted,
        "the failed jump must permit a new cold request"
    );
    assert_eq!(pane(&app).reply_to, Some(10));
    assert_eq!(
        app.reply_lookup(1, 10).map(|item| item.text.as_str()),
        Some("reply target"),
        "the protected reply must survive the failed jump"
    );
    drop(memory_history_reply(&mut app, retry, Ok(page(1, 98..=100))));
    assert!(pane(&app).cold.is_none(), "the retry restores history");
    assert!(pane(&app).messages.iter().any(|m| m.id == 100));
}

#[test]
fn memory_history_active_latest_restore_keeps_live_message_and_newer_edit_and_delete() {
    let mut app = app();
    drop(app.update(Msg::SetKeepDeleted(false)));
    select(
        &mut app,
        1,
        &[(10, "unchanged"), (11, "old text"), (12, "to delete")],
    );
    let start = memory_history_background(&mut app);
    memory_history_sweep(&mut app, start + Duration::from_secs(301));
    drop(app.update(Msg::SelectTab(1)));
    let request = memory_history_request(&app);

    new_message(&mut app, 1, 13, "arrived while loading");
    assert_eq!(shown(&app), [(13, "arrived while loading", false)]);
    td(
        &mut app,
        json!({"@type": "updateMessageContent", "chat_id": 1, "message_id": 11,
               "new_content": {"@type": "messageText", "text": {"@type": "formattedText",
                   "text": "new text", "entities": []}}}),
    );
    delete(&mut app, 1, &[12], false);

    // TDLib's reply was sampled before those events. Neither its old text
    // nor its deleted row may replace the visible state on completion.
    let stale = [(10, "unchanged"), (11, "old text"), (12, "to delete")];
    let page = stale
        .iter()
        .map(|&(id, text)| serde_json::from_value(message(1, id, text)).unwrap())
        .collect();
    drop(memory_history_reply(&mut app, request, Ok(page)));
    assert_eq!(
        shown(&app),
        [
            (10, "unchanged", false),
            (11, "new text", false),
            (13, "arrived while loading", false)
        ]
    );
}

#[test]
fn memory_history_unrelated_updates_over_one_page_do_not_block_cold_restore() {
    let mut app = app();
    select(&mut app, 1, &[(101, "latest")]);
    pane_msg(
        &mut app,
        PaneMsg::OlderLoaded(1, 101, Ok(page(1, 51..=100))),
    );
    pane_msg(&mut app, PaneMsg::OlderLoaded(1, 51, Ok(page(1, 1..=50))));
    pane_msg(&mut app, typed("draft survives the wait"));
    let start = memory_history_background(&mut app);
    memory_history_sweep(&mut app, start + Duration::from_secs(301));
    drop(app.update(Msg::SelectTab(1)));
    let first = memory_history_request(&app);

    // More than a TDLib page of edits to history *outside* the newest page.
    // The client may refetch coherently, but must not require the user to
    // switch tabs again or discard their existing draft.
    for id in 1..=51 {
        td(
            &mut app,
            json!({"@type": "updateMessageContent", "chat_id": 1, "message_id": id,
                   "new_content": {"@type": "messageText", "text": {"@type": "formattedText",
                       "text": format!("older edit {id}"), "entities": []}}}),
        );
    }
    drop(memory_history_reply(&mut app, first, Ok(page(1, 52..=101))));
    if pane(&app).cold.is_some() {
        let refreshed = memory_history_request(&app);
        assert_ne!(
            first, refreshed,
            "a stale page needs a distinct bounded verification request"
        );
        // Continuing edits to the same older page while the bounded
        // by-ID check is in flight cannot strand the visible reader.
        for id in 1..=51 {
            td(
                &mut app,
                json!({"@type": "updateMessageContent", "chat_id": 1, "message_id": id,
                       "new_content": {"@type": "messageText", "text": {"@type": "formattedText",
                           "text": format!("newer older edit {id}"), "entities": []}}}),
            );
        }
        drop(app.update(Msg::Pane(
            app.main_window,
            PaneMsg::ColdHistoryVerified(1, refreshed, Ok(page(1, 52..=101))),
        )));
    }
    assert_eq!(pane(&app).messages.last().map(|m| m.id), Some(101));
    assert_eq!(pane(&app).compose.text(), "draft survives the wait");
    assert!(
        app.error.is_none(),
        "unrelated updates must not strand the reader: {:?}",
        app.error
    );
}

#[test]
fn memory_history_verified_full_latest_page_keeps_older_pagination_after_one_deletion() {
    let mut app = app();
    drop(app.update(Msg::SetKeepDeleted(false)));
    drop(app.update(Msg::Pane(app.main_window, PaneMsg::SelectChat(1))));
    memory_history_load(&mut app, 1, page(1, 100..=149));
    pane_msg(&mut app, PaneMsg::OlderLoaded(1, 100, Ok(page(1, 51..=99))));
    pane_msg(&mut app, PaneMsg::OlderLoaded(1, 51, Ok(page(1, 1..=50))));
    pane_msg(&mut app, typed("draft while loading"));
    let start = memory_history_background(&mut app);
    memory_history_sweep(&mut app, start + Duration::from_secs(301));
    drop(app.update(Msg::SelectTab(1)));
    let first = memory_history_request(&app);

    for id in 1..=51 {
        td(
            &mut app,
            json!({"@type": "updateMessageContent", "chat_id": 1, "message_id": id,
                   "new_content": {"@type": "messageText", "text": {"@type": "formattedText",
                       "text": format!("old page edit {id}"), "entities": []}}}),
        );
    }
    drop(memory_history_reply(
        &mut app,
        first,
        Ok(page(1, 100..=149)),
    ));
    let verification = memory_history_request(&app);
    assert_ne!(first, verification);

    // The first TDLib page had all 50 slots. A message deleted before the
    // by-ID coherence answer makes that answer shorter, not the history itself.
    delete(&mut app, 1, &[100], false);
    drop(app.update(Msg::Pane(
        app.main_window,
        PaneMsg::ColdHistoryVerified(1, verification, Ok(page(1, 101..=149))),
    )));
    assert_eq!(pane(&app).messages.first().map(|m| m.id), Some(101));
    assert_eq!(pane(&app).messages.last().map(|m| m.id), Some(149));
    assert_eq!(
        pane(&app).next_older(),
        Some(101),
        "one deleted verification result must not close older pagination"
    );
    assert_eq!(pane(&app).compose.text(), "draft while loading");
}

#[test]
fn memory_history_verified_bounded_archive_merge_excludes_deletions_before_fetched_page() {
    let mut app = app();
    new_message(&mut app, 1, 40, "older archived message");
    delete(&mut app, 1, &[40], false);
    drop(app.update(Msg::Pane(app.main_window, PaneMsg::SelectChat(1))));
    memory_history_load(&mut app, 1, page(1, 100..=149));
    pane_msg(&mut app, PaneMsg::OlderLoaded(1, 100, Ok(page(1, 51..=99))));
    pane_msg(
        &mut app,
        PaneMsg::OlderLoaded(1, 51, Ok(page(1, (1..=50).filter(|&id| id != 40)))),
    );
    pane_msg(&mut app, typed("draft while loading"));
    let start = memory_history_background(&mut app);
    memory_history_sweep(&mut app, start + Duration::from_secs(301));
    drop(app.update(Msg::SelectTab(1)));
    let first = memory_history_request(&app);

    for id in 1..=51 {
        td(
            &mut app,
            json!({"@type": "updateMessageContent", "chat_id": 1, "message_id": id,
                   "new_content": {"@type": "messageText", "text": {"@type": "formattedText",
                       "text": format!("old page edit {id}"), "entities": []}}}),
        );
    }
    drop(memory_history_reply(
        &mut app,
        first,
        Ok(page(1, 100..=149)),
    ));
    let verification = memory_history_request(&app);
    assert_ne!(first, verification);

    delete(&mut app, 1, &[100], false);
    drop(app.update(Msg::Pane(
        app.main_window,
        PaneMsg::ColdHistoryVerified(1, verification, Ok(page(1, 101..=149))),
    )));
    assert!(
        !pane(&app).messages.iter().any(|item| item.id == 40),
        "archived deletion outside the fetched page must stay outside its merge"
    );
    assert_eq!(pane(&app).oldest_loaded, Some(101));
    assert_eq!(pane(&app).next_older(), Some(101));
    assert_eq!(pane(&app).compose.text(), "draft while loading");
}

#[test]
fn memory_history_verified_empty_page_keeps_original_pagination_and_draft() {
    let mut app = app();
    drop(app.update(Msg::Pane(app.main_window, PaneMsg::SelectChat(1))));
    memory_history_load(&mut app, 1, page(1, 100..=149));
    pane_msg(&mut app, PaneMsg::OlderLoaded(1, 100, Ok(page(1, 51..=99))));
    pane_msg(&mut app, PaneMsg::OlderLoaded(1, 51, Ok(page(1, 1..=50))));
    pane_msg(&mut app, typed("draft while loading"));
    let start = memory_history_background(&mut app);
    memory_history_sweep(&mut app, start + Duration::from_secs(301));
    drop(app.update(Msg::SelectTab(1)));
    let first = memory_history_request(&app);

    for id in 1..=51 {
        td(
            &mut app,
            json!({"@type": "updateMessageContent", "chat_id": 1, "message_id": id,
                   "new_content": {"@type": "messageText", "text": {"@type": "formattedText",
                       "text": format!("old page edit {id}"), "entities": []}}}),
        );
    }
    drop(memory_history_reply(
        &mut app,
        first,
        Ok(page(1, 100..=149)),
    ));
    let verification = memory_history_request(&app);
    assert_ne!(first, verification);

    drop(app.update(Msg::Pane(
        app.main_window,
        PaneMsg::ColdHistoryVerified(1, verification, Ok(Vec::new())),
    )));
    assert!(pane(&app).messages.is_empty());
    assert_eq!(pane(&app).oldest_loaded, Some(100));
    assert_eq!(pane(&app).next_older(), Some(100));
    assert_eq!(pane(&app).compose.text(), "draft while loading");
}

#[test]
fn memory_history_jump_to_protected_reply_selection_or_edit_loads_full_around_page() {
    let mut app = app();
    select(
        &mut app,
        1,
        &[(10, "selected"), (11, "reply target"), (12, "ordinary")],
    );
    pane_msg(&mut app, PaneMsg::Select(10));
    pane_msg(&mut app, PaneMsg::Reply(11));
    let start = memory_history_background(&mut app);
    memory_history_sweep(&mut app, start + Duration::from_secs(301));
    drop(app.update(Msg::SelectTab(1)));
    let stale = memory_history_request(&app);

    // Row 11 was retained solely because the composer replies to it.
    // Clicking it is still a navigation request for its surrounding history.
    pane_msg(&mut app, PaneMsg::JumpTo(11));
    pane_msg(&mut app, PaneMsg::AroundLoaded(1, 11, Ok(page(1, 8..=12))));
    drop(memory_history_reply(&mut app, stale, Ok(page(1, 90..=92))));
    assert_eq!(
        pane(&app).messages.iter().map(|m| m.id).collect::<Vec<_>>(),
        (8..=12).collect::<Vec<_>>()
    );
    assert_eq!(pane(&app).reply_to, Some(11));
    assert_eq!(
        pane(&app).selected.as_ref().map(|ids| ids.contains(&10)),
        Some(true)
    );

    pane_msg(
        &mut app,
        PaneMsg::EditLoaded(1, 11, Ok("editing reply target".into())),
    );
    select(&mut app, 2, &[(20, "foreground")]);
    let second = Instant::now();
    memory_history_sweep(&mut app, second + Duration::from_secs(301));
    drop(app.update(Msg::SelectTab(1)));
    let stale = memory_history_request(&app);
    pane_msg(&mut app, PaneMsg::JumpTo(11));
    pane_msg(&mut app, PaneMsg::AroundLoaded(1, 11, Ok(page(1, 7..=13))));
    drop(memory_history_reply(&mut app, stale, Ok(page(1, 90..=92))));
    assert_eq!(
        pane(&app).messages.iter().map(|m| m.id).collect::<Vec<_>>(),
        (7..=13).collect::<Vec<_>>()
    );
    assert_eq!(pane(&app).editing.as_ref().map(|edit| edit.0), Some(11));
    assert_eq!(pane(&app).compose.text(), "editing reply target");
}

#[test]
fn memory_history_open_found_jump_wins_over_cold_reply_after_around_answer() {
    let mut app = app();
    select(&mut app, 1, &[(100, "latest")]);
    let start = memory_history_background(&mut app);
    memory_history_sweep(&mut app, start + Duration::from_secs(301));
    drop(app.update(Msg::SelectTab(1)));
    let stale = memory_history_request(&app);

    // A global search hit uses OpenFound (not the in-chat JumpTo action).
    // Its AroundLoaded result wins even after jump_pending has cleared.
    drop(app.update(Msg::OpenFound(app.main_window, 1, 42)));
    pane_msg(&mut app, PaneMsg::AroundLoaded(1, 42, Ok(page(1, 40..=44))));
    drop(memory_history_reply(&mut app, stale, Ok(page(1, 98..=100))));
    assert_eq!(
        pane(&app).messages.iter().map(|m| m.id).collect::<Vec<_>>(),
        (40..=44).collect::<Vec<_>>()
    );
    assert_eq!(pane(&app).highlight, Some(42));
}

#[test]
fn memory_history_second_cold_refetch_replaces_held_reply_with_authoritative_text() {
    let mut app = app();
    select(
        &mut app,
        1,
        &[(10, "cached reply"), (99, "newer"), (100, "latest")],
    );
    pane_msg(&mut app, PaneMsg::Reply(10));
    let start = memory_history_background(&mut app);
    memory_history_sweep(&mut app, start + Duration::from_secs(301));
    drop(app.update(Msg::SelectTab(1)));
    let first = memory_history_request(&app);
    drop(memory_history_reply(&mut app, first, Ok(page(1, 98..=100))));
    assert_eq!(
        app.reply_lookup(1, 10).map(|item| item.text.as_str()),
        Some("cached reply")
    );
    assert!(
        pane(&app).messages.iter().all(|item| item.id != 10),
        "the held reply is not a history row"
    );

    select(&mut app, 2, &[(20, "foreground")]);
    let second = Instant::now();
    memory_history_sweep(&mut app, second + Duration::from_secs(301));
    drop(app.update(Msg::SelectTab(1)));
    let request = memory_history_request(&app);
    let fresh = [
        message(1, 10, "server edit"),
        message(1, 99, "newer"),
        message(1, 100, "latest"),
    ]
    .into_iter()
    .map(|m| serde_json::from_value(m).unwrap())
    .collect();
    drop(memory_history_reply(&mut app, request, Ok(fresh)));
    assert_eq!(pane(&app).reply_to, Some(10));
    assert_eq!(
        app.reply_lookup(1, 10).map(|item| item.text.as_str()),
        Some("server edit")
    );
    assert_eq!(
        pane(&app)
            .messages
            .iter()
            .filter(|item| item.id == 10)
            .count(),
        1
    );
}

#[test]
fn memory_history_copy_selection_orders_held_and_refetched_rows_by_message_id() {
    use iced::futures::{FutureExt, StreamExt};
    use iced_runtime::{Action, clipboard, task::into_stream};

    let mut app = app();
    app.session.users.insert(7, "Alice".into());
    select(
        &mut app,
        1,
        &[(10, "held ten"), (20, "middle"), (30, "cached thirty")],
    );
    pane_msg(&mut app, PaneMsg::Select(10));
    pane_msg(&mut app, PaneMsg::Select(30));
    let start = memory_history_background(&mut app);
    memory_history_sweep(&mut app, start + Duration::from_secs(301));
    drop(app.update(Msg::SelectTab(1)));
    let request = memory_history_request(&app);
    let fresh = [message(1, 20, "middle"), message(1, 30, "fresh thirty")]
        .into_iter()
        .map(|m| serde_json::from_value(m).unwrap())
        .collect();
    drop(memory_history_reply(&mut app, request, Ok(fresh)));

    let task = app.update(Msg::Pane(app.main_window, PaneMsg::CopySelection));
    let mut stream = into_stream(task).expect("copy produces clipboard action");
    let action = stream
        .next()
        .now_or_never()
        .expect("immediate clipboard effect")
        .expect("clipboard action");
    let Action::Clipboard(clipboard::Action::Write { contents, .. }) = action else {
        panic!("copy must write actual selected messages to the clipboard");
    };
    assert_eq!(contents, "Alice: held ten\nAlice: fresh thirty");
}

#[test]
fn security_cold_jump_updates_edits_before_and_during_jump_outlive_stale_around_page() {
    let mut app = app();
    select(
        &mut app,
        1,
        &[(40, "before jump"), (41, "during jump"), (100, "latest")],
    );
    let start = memory_history_background(&mut app);
    memory_history_sweep(&mut app, start + Duration::from_secs(301));
    drop(app.update(Msg::SelectTab(1)));
    let interrupted = memory_history_request(&app);

    td(
        &mut app,
        json!({"@type": "updateMessageContent", "chat_id": 1, "message_id": 40,
               "new_content": {"@type": "messageText", "text": {"@type": "formattedText",
                   "text": "edited before jump", "entities": []}}}),
    );
    pane_msg(&mut app, PaneMsg::JumpTo(43));
    assert_eq!(pane(&app).jump_pending, Some(43));
    td(
        &mut app,
        json!({"@type": "updateMessageContent", "chat_id": 1, "message_id": 41,
               "new_content": {"@type": "messageText", "text": {"@type": "formattedText",
                   "text": "edited during jump", "entities": []}}}),
    );

    let stale = [
        (40, "before jump"),
        (41, "during jump"),
        (42, "neighbor"),
        (43, "target"),
        (44, "neighbor after"),
    ]
    .into_iter()
    .map(|(id, text)| serde_json::from_value(message(1, id, text)).unwrap())
    .collect();
    pane_msg(&mut app, PaneMsg::AroundLoaded(1, 43, Ok(stale)));
    drop(memory_history_reply(
        &mut app,
        interrupted,
        Ok(page(1, 98..=100)),
    ));
    assert_eq!(
        shown(&app),
        [
            (40, "edited before jump", false),
            (41, "edited during jump", false),
            (42, "neighbor", false),
            (43, "target", false),
            (44, "neighbor after", false)
        ],
        "a stale around snapshot must not overwrite TDLib edits on either side of JumpTo"
    );

    // Tombstones expose the archived text, including stale text re-saved by
    // the around page even if the visible rows were corrected separately.
    delete(&mut app, 1, &[40, 41], false);
    let archived = app
        .session
        .archive
        .as_ref()
        .unwrap()
        .deleted(1, 40, 42)
        .unwrap();
    assert_eq!(
        archived
            .iter()
            .map(|m| (m.id, m.text.as_str()))
            .collect::<Vec<_>>(),
        [(40, "edited before jump"), (41, "edited during jump")],
        "the enabled archive must not retain the stale around-page text"
    );
}

#[test]
fn security_cold_jump_updates_deletion_during_jump_obeys_archive_visibility_policy() {
    for keep_deleted in [true, false] {
        let mut app = app();
        drop(app.update(Msg::SetKeepDeleted(keep_deleted)));
        select(&mut app, 1, &[(42, "to delete"), (100, "latest")]);
        let start = memory_history_background(&mut app);
        memory_history_sweep(&mut app, start + Duration::from_secs(301));
        drop(app.update(Msg::SelectTab(1)));
        let interrupted = memory_history_request(&app);
        pane_msg(&mut app, PaneMsg::JumpTo(43));
        assert_eq!(pane(&app).jump_pending, Some(43));
        delete(&mut app, 1, &[42], false);

        let stale = [(41, "neighbor"), (42, "to delete"), (43, "target")]
            .into_iter()
            .map(|(id, text)| serde_json::from_value(message(1, id, text)).unwrap())
            .collect();
        pane_msg(&mut app, PaneMsg::AroundLoaded(1, 43, Ok(stale)));
        drop(memory_history_reply(
            &mut app,
            interrupted,
            Ok(page(1, 98..=100)),
        ));
        let expected = if keep_deleted {
            vec![
                (41, "neighbor", false),
                (42, "to delete", true),
                (43, "target", false),
            ]
        } else {
            vec![(41, "neighbor", false), (43, "target", false)]
        };
        assert_eq!(
            shown(&app),
            expected,
            "a deletion during JumpTo must not be resurrected by the stale around page (keep_deleted={keep_deleted})"
        );
        if keep_deleted {
            let archived = app
                .session
                .archive
                .as_ref()
                .unwrap()
                .deleted(1, 42, 43)
                .unwrap();
            assert_eq!(
                archived
                    .iter()
                    .map(|m| (m.id, m.text.as_str()))
                    .collect::<Vec<_>>(),
                [(42, "to delete")],
                "the archived tombstone must survive a stale around page"
            );
        }
    }
}

#[test]
fn security_cold_jump_cursor_pages_through_gap_before_recent_arrival() {
    let mut app = app();
    app.session.chats.insert(
        1,
        ChatItem {
            order: 1,
            last_id: 100,
            ..chat_item("chat", false)
        },
    );
    select(&mut app, 1, &[(100, "latest")]);
    let start = memory_history_background(&mut app);
    memory_history_sweep(&mut app, start + Duration::from_secs(301));
    drop(app.update(Msg::SelectTab(1)));
    let interrupted = memory_history_request(&app);

    new_message(&mut app, 1, 101, "recent arrival");
    assert!(
        pane(&app).messages.iter().any(|item| item.id == 101),
        "the live arrival is present while cold history is restoring"
    );
    pane_msg(&mut app, PaneMsg::JumpTo(42));
    pane_msg(&mut app, PaneMsg::AroundLoaded(1, 42, Ok(page(1, 40..=44))));
    drop(memory_history_reply(
        &mut app,
        interrupted,
        Ok(page(1, 98..=100)),
    ));
    assert_eq!(
        pane(&app)
            .messages
            .iter()
            .map(|item| item.id)
            .collect::<Vec<_>>(),
        (40..=44).collect::<Vec<_>>(),
        "a recent arrival must not enter the older around page or move its newer cursor"
    );
    assert!(!pane(&app).newest_loaded);

    drop(app.load_newer(app.main_window));
    assert!(
        pane(&app).loading_newer,
        "the older page requests newer history"
    );
    pane_msg(&mut app, PaneMsg::NewerLoaded(1, 44, Ok(page(1, 45..=94))));
    assert_eq!(
        pane(&app)
            .messages
            .iter()
            .map(|item| item.id)
            .collect::<Vec<_>>(),
        (40..=94).collect::<Vec<_>>(),
        "the first newer result must fill the gap after the around page"
    );
    drop(app.load_newer(app.main_window));
    assert!(
        pane(&app).loading_newer,
        "the next newer page is still needed"
    );
    pane_msg(&mut app, PaneMsg::NewerLoaded(1, 94, Ok(page(1, 95..=101))));
    assert_eq!(
        pane(&app)
            .messages
            .iter()
            .map(|item| item.id)
            .collect::<Vec<_>>(),
        (40..=101).collect::<Vec<_>>(),
        "ordinary newer pagination must reach the live arrival without skipping 45–100"
    );
    assert!(pane(&app).newest_loaded);
}

#[test]
fn security_cold_jump_tail_tombstone_is_visible_without_newer_pagination() {
    let mut app = app();
    drop(app.update(Msg::SetKeepDeleted(true)));
    app.session.chats.insert(
        1,
        ChatItem {
            order: 1,
            last_id: 44,
            ..chat_item("chat", false)
        },
    );
    select(&mut app, 1, &[(44, "latest live")]);
    let deleted: Message = serde_json::from_value(message(1, 45, "archived tail")).unwrap();
    let archive = app.session.archive.as_mut().unwrap();
    archive.save([&MsgItem::from(&deleted)]).unwrap();
    archive.mark_deleted(1, &[45]).unwrap();
    assert_eq!(
        archive
            .deleted(1, 45, 46)
            .unwrap()
            .iter()
            .map(|m| (m.id, m.text.as_str()))
            .collect::<Vec<_>>(),
        [(45, "archived tail")],
        "the archive fixture contains the deleted row above the last live message"
    );

    let start = memory_history_background(&mut app);
    memory_history_sweep(&mut app, start + Duration::from_secs(301));
    drop(app.update(Msg::SelectTab(1)));
    let interrupted = memory_history_request(&app);
    pane_msg(&mut app, PaneMsg::JumpTo(42));
    pane_msg(&mut app, PaneMsg::AroundLoaded(1, 42, Ok(page(1, 40..=44))));
    drop(memory_history_reply(
        &mut app,
        interrupted,
        Ok(page(1, [44])),
    ));

    assert!(
        pane(&app).newest_loaded,
        "the around page reaches the last live message"
    );
    drop(app.load_newer(app.main_window));
    assert!(
        !pane(&app).loading_newer,
        "no newer TDLib request is available"
    );
    assert_eq!(
        shown(&app),
        [
            (40, "m", false),
            (41, "m", false),
            (42, "m", false),
            (43, "m", false),
            (44, "m", false),
            (45, "archived tail", true),
        ],
        "the archived tail must appear even though the live around page is already newest"
    );
}

/// Retains the iced widget tree so scroll operations act on the real
/// scrollable rather than changing `ChatPane::scroll` metadata alone.
struct MemoryHistorySurface {
    renderer: iced::Renderer,
    cache: iced_runtime::user_interface::Cache,
    size: iced::Size,
}

impl MemoryHistorySurface {
    fn new() -> Self {
        Self {
            renderer: sandbox::renderer(),
            cache: Default::default(),
            size: iced::Size::new(1000.0, 700.0),
        }
    }

    fn draw(&mut self, app: &mut App) -> Vec<u8> {
        use iced_runtime::core::renderer::{Headless, Style};
        use iced_runtime::user_interface::UserInterface;

        let theme = app.theme(app.main_window).unwrap_or(iced::Theme::Dark);
        let mut messages = Vec::new();
        let mut ui = UserInterface::build(
            app.view(app.main_window),
            self.size,
            std::mem::take(&mut self.cache),
            &mut self.renderer,
        );
        let _ = ui.update(
            &[iced::Event::Window(iced::window::Event::RedrawRequested(
                Instant::now(),
            ))],
            iced::mouse::Cursor::Unavailable,
            &mut self.renderer,
            &mut iced_runtime::core::clipboard::Null,
            &mut messages,
        );
        ui.draw(
            &mut self.renderer,
            &theme,
            &Style {
                text_color: theme.palette().text,
            },
            iced::mouse::Cursor::Unavailable,
        );
        let pixels = self.renderer.screenshot(
            iced::Size::new(self.size.width as u32, self.size.height as u32),
            1.0,
            theme.palette().background,
        );
        self.cache = ui.into_cache();
        for message in messages {
            // Apply the first real widget correction only; any chained
            // TDLib request must remain unpolled.
            let measured = matches!(&message, Msg::Pane(_, PaneMsg::Measured(..)));
            let resized = matches!(&message, Msg::Pane(window, PaneMsg::Scrolled(viewport))
                if *window == app.main_window
                    && pane(app).restoring_anchor.is_some()
                    && pane(app).view_size != Some((viewport.bounds().height, viewport.bounds().width)));
            let scrolled = matches!(&message, Msg::Pane(_, PaneMsg::Scrolled(..)));
            let newer = pane(app).loading_newer;
            let task = app.update(message);
            let footer = scrolled
                && pane(app).restoring_anchor.is_some()
                && newer != pane(app).loading_newer;
            if measured || resized || footer {
                self.apply(app, task);
            }
        }
        pixels
    }

    fn operate(&mut self, app: &App, operation: &mut dyn iced_runtime::core::widget::Operation) {
        use iced_runtime::user_interface::UserInterface;

        let mut ui = UserInterface::build(
            app.view(app.main_window),
            self.size,
            std::mem::take(&mut self.cache),
            &mut self.renderer,
        );
        ui.operate(&self.renderer, operation);
        self.cache = ui.into_cache();
    }

    fn apply(&mut self, app: &App, task: iced::Task<Msg>) {
        use iced::futures::{FutureExt, StreamExt};
        use iced_runtime::core::widget::operation::Outcome;
        use iced_runtime::{Action, task::into_stream};

        if let Some(mut stream) = into_stream(task)
            && let Some(Some(Action::Widget(mut operation))) = stream.next().now_or_never()
        {
            loop {
                self.operate(app, &mut *operation);
                match operation.finish() {
                    Outcome::Chain(next) => operation = next,
                    Outcome::None | Outcome::Some(()) => break,
                }
            }
        }
    }

    fn scroll_to(&mut self, app: &App, offset: f32) {
        let mut operation = iced_runtime::core::widget::operation::scrollable::scroll_to(
            pane(app).scroll_id.clone(),
            AbsoluteOffset {
                x: Some(0.0),
                y: Some(offset),
            },
        );
        self.operate(app, &mut operation);
    }

    fn viewport(&mut self, app: &App) -> iced::Rectangle {
        #[derive(Default)]
        struct FindViewport {
            target: Option<iced::widget::Id>,
            bounds: Option<iced::Rectangle>,
        }
        impl iced_runtime::core::widget::Operation for FindViewport {
            fn traverse(
                &mut self,
                operate: &mut dyn FnMut(&mut dyn iced_runtime::core::widget::Operation),
            ) {
                operate(self);
            }
            fn scrollable(
                &mut self,
                id: Option<&iced::widget::Id>,
                bounds: iced::Rectangle,
                _content: iced::Rectangle,
                _translation: iced::Vector,
                _state: &mut dyn iced_runtime::core::widget::operation::Scrollable,
            ) {
                if id == self.target.as_ref() {
                    self.bounds = Some(bounds);
                }
            }
        }
        let mut operation = FindViewport {
            target: Some(pane(app).scroll_id.clone()),
            ..Default::default()
        };
        self.operate(app, &mut operation);
        operation
            .bounds
            .expect("actual history scrollable in widget tree")
    }
}

fn memory_history_visual_page(first: i64, last: i64) -> Vec<Message> {
    (first..=last)
        .map(|id| {
            let value = if id == 40 {
                media_message(1, id, photo_content())
            } else {
                message(1, id, &format!("visual message {id}"))
            };
            serde_json::from_value(value).unwrap()
        })
        .collect()
}

/// Compare the *drawn* highlight border with the otherwise identical frame.
/// This measures the real anchor row position, not an estimated pane offset.
fn memory_history_drawn_anchor(ui: &mut MemoryHistorySurface, app: &mut App) -> (Vec<u8>, f32) {
    app.session
        .panes
        .get_mut(&app.main_window)
        .unwrap()
        .highlight = None;
    for _ in 0..3 {
        let _ = ui.draw(app);
    }
    let plain = ui.draw(app);
    app.session
        .panes
        .get_mut(&app.main_window)
        .unwrap()
        .highlight = Some(42);
    for _ in 0..3 {
        let _ = ui.draw(app);
    }
    let highlighted = ui.draw(app);
    let bounds = ui.viewport(app);
    let width = ui.size.width as usize;
    let left = bounds.x.max(0.0) as usize;
    let right = (bounds.x + bounds.width).min(ui.size.width) as usize;
    let top = bounds.y.max(0.0) as usize;
    let bottom = (bounds.y + bounds.height).min(ui.size.height) as usize;
    let changed_rows: Vec<usize> = (top..bottom)
        .filter(|&y| {
            (left..right).any(|x| {
                let pixel = 4 * (y * width + x);
                plain[pixel..pixel + 4] != highlighted[pixel..pixel + 4]
            })
        })
        .collect();
    assert!(
        !changed_rows.is_empty(),
        "highlighted reading row must appear in actual viewport"
    );
    let first = changed_rows[0];
    let last = *changed_rows.last().unwrap();
    assert!(
        last - first < 120,
        "only the targeted bubble should change: rows {first}..{last}"
    );
    (highlighted, (first + last) as f32 / 2.0)
}

#[test]
#[ignore = "offline PNG surface: run explicitly after history eviction lands"]
fn sandbox_memory_reclamation() {
    let mut app = app();
    let window = app.main_window;
    app.session.chats.insert(
        1,
        ChatItem {
            last_id: 300,
            ..chat_item("Reading chat", false)
        },
    );
    app.session.chats.insert(2, chat_item("Other chat", false));
    let mut ui = MemoryHistorySurface::new();
    let _ = app.update(Msg::Pane(window, PaneMsg::SelectChat(1)));
    memory_history_load(&mut app, 1, page(1, 201..=300));
    pane_msg(&mut app, PaneMsg::JumpTo(42));
    drop(app.update(Msg::Pane(
        window,
        PaneMsg::AroundLoaded(1, 42, Ok(memory_history_visual_page(30, 54))),
    )));
    ui.scroll_to(&app, pane(&app).scroll.expect("initial jump scroll").y);
    for _ in 0..4 {
        let _ = ui.draw(&mut app);
    }
    let target = pane(&app).offset_of(42).expect("old reading message") + 13.0;
    pane_mut_scroll(&mut app, target);
    ui.scroll_to(&app, target);
    for _ in 0..4 {
        let _ = ui.draw(&mut app);
    }
    // An actually decoded image on the old history page must also become
    // reclaimable after the tab and its picture leave the screen.
    let rgba = [60_u8, 120, 180, 255].repeat(64 * 64);
    let _ = app.update(Msg::ImageDecoded(103, Ok((64, 64, rgba))));
    let (before_png, before_y) = memory_history_drawn_anchor(&mut ui, &mut app);
    let before_viewport = ui.viewport(&app);
    let before_capacity = pane(&app).messages.capacity();
    let before_bytes = app.session.images.total();
    let _ = sandbox::save("memory-history-before", ui.size, before_png);
    assert!(before_capacity >= 25 && before_bytes > 0);

    let start = memory_history_background(&mut app);
    for _ in 0..3 {
        let _ = ui.draw(&mut app);
    }
    memory_history_sweep(&mut app, start + Duration::from_secs(301));
    let freed_capacity = app.session.background[&1].messages.capacity();
    let freed_bytes = app.session.images.total();
    println!(
        "decoded cache bytes: {before_bytes} -> {freed_bytes}; background history capacity: {before_capacity} -> {freed_capacity}"
    );
    assert_eq!(freed_capacity, 0);
    assert_eq!(
        freed_bytes, 0,
        "invisible decoded picture must be freed after five minutes"
    );

    drop(app.update(Msg::SelectTab(1)));
    for _ in 0..3 {
        let _ = ui.draw(&mut app);
    }
    let request = memory_history_request(&app);
    let task = memory_history_reply(&mut app, request, Ok(memory_history_visual_page(35, 49)));
    ui.apply(&app, task);
    // Change only viewport height while the newly fetched rows still need
    // post-layout measurement. A layout-induced scroll callback must not
    // cancel the saved message-relative reading position.
    ui.size.height = 520.0;
    let (after_png, after_y) = memory_history_drawn_anchor(&mut ui, &mut app);
    let after_viewport = ui.viewport(&app);
    let _ = sandbox::save("memory-history-after", ui.size, after_png);
    assert!(pane(&app).messages.iter().any(|m| m.id == 42));
    let before_from_center = before_y - before_viewport.center_y();
    let after_from_center = after_y - after_viewport.center_y();
    assert!(
        (before_from_center - after_from_center).abs() < 8.0,
        "resizing the real scrollable shifted the reading row: {before_from_center} -> {after_from_center}"
    );
}
