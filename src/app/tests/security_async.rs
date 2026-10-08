//! Late async answers must not cross clients or supersede newer history requests.
//! TDLib-shaped messages go through `App::update`; returned tasks stay unpolled.
use super::*;

#[test]
fn security_async_origin_foreign_history_cannot_replace_or_archive_current_chat() {
    let mut app = app();
    app.session.client_id = 42;
    let window = app.main_window;
    drop(app.update(Msg::Pane(window, PaneMsg::SelectChat(1))));
    let request = pane(&app)
        .history_request
        .expect("chat opening requests history");
    drop(app.update(Msg::Pane(
        window,
        PaneMsg::HistoryLoaded(1, Ok(page(1, [10]))),
    )));
    assert_eq!(shown(&app), [(10, "m", false)]);

    // The window, chat, and request all match; only the former TDLib client differs.
    drop(app.update(Msg::HistoryResult(
        41,
        request,
        window,
        1,
        Ok(page(1, [20])),
    )));

    // A foreign row must not become available later as deleted archive history.
    let displayed: Vec<_> = shown(&app)
        .into_iter()
        .map(|(id, text, deleted)| (id, text.to_owned(), deleted))
        .collect();
    let archive = app.session.archive.as_mut().unwrap();
    archive.mark_deleted(1, &[20]).unwrap();
    let foreign_archived = archive
        .deleted(1, i64::MIN, i64::MAX)
        .unwrap()
        .iter()
        .any(|m| m.id == 20);
    assert_eq!(
        (displayed, foreign_archived),
        (vec![(10, "m".to_owned(), false)], false),
        "foreign page must neither be displayed nor archived",
    );
}

#[test]
fn security_async_origin_older_history_request_cannot_replace_newer_page() {
    let mut app = app();
    app.session.client_id = 42;
    let window = app.main_window;
    drop(app.update(Msg::Pane(window, PaneMsg::SelectChat(1))));
    let stale_request = pane(&app)
        .history_request
        .expect("chat opening requests history");
    drop(app.update(Msg::Pane(
        window,
        PaneMsg::HistoryLoaded(1, Ok(page(1, [30]))),
    )));
    let current_request = super::nav::request_id();
    assert_ne!(stale_request, current_request);
    app.session.panes.get_mut(&window).unwrap().history_request = Some(current_request);
    assert_eq!(shown(&app), [(30, "m", false)]);

    drop(app.update(Msg::HistoryResult(
        42,
        stale_request,
        window,
        1,
        Ok(page(1, [10])),
    )));
    assert_eq!(
        shown(&app),
        [(30, "m", false)],
        "older request must not replace the newer page"
    );
}

#[test]
fn security_async_origin_foreign_replies_cannot_populate_current_previews() {
    let mut app = app();
    app.session.client_id = 42;
    open(&mut app, 1, &[]);
    assert!(!app.session.reply_previews.contains_key(&(1, 10)));

    drop(app.update(Msg::ForClient(
        41,
        Box::new(Msg::RepliesLoaded(vec![
            serde_json::from_value(message(1, 10, "other account reply")).unwrap(),
        ])),
    )));
    assert!(
        !app.session.reply_previews.contains_key(&(1, 10)),
        "foreign reply must not populate this account's previews",
    );
}
