//! Memory-pressure behavior through real pane visibility and decode events.
//! Task results are never polled; no TDLib client or image decoder is started.

use super::*;
use std::time::{Duration, Instant};

const PHOTO_W: u32 = 800;
const PHOTO_H: u32 = 960;
const PHOTO_BYTES: usize = PHOTO_W as usize * PHOTO_H as usize * 4;
const PHOTO_COUNT: i32 = 22; // 21 photos fit in 64 MiB; 22 do not.
const BUDGET: usize = 64 * 1024 * 1024;
const FIRST_FILE: i32 = 1000;
const SMALL_FILE: i32 = 203;
const ALL_PHOTOS_VIEW: (f32, f32) = (20_000.0, 1000.0);

fn photo_with_file(file_id: i32) -> Value {
    let mut content = photo_content();
    content["photo"]["sizes"][2]["photo"]["id"] = json!(file_id);
    content["photo"]["sizes"][2]["width"] = json!(PHOTO_W);
    content["photo"]["sizes"][2]["height"] = json!(PHOTO_H);
    content
}

fn add_photo(app: &mut App, chat_id: i64, message_id: i64, file_id: i32) {
    new_message_value(
        app,
        media_message(chat_id, message_id, photo_with_file(file_id)),
    );
}

fn owner(app: &App, window: WinId, file_id: i32) -> media::PhotoOwner {
    let message = app.session.panes[&window]
        .messages
        .iter()
        .find(|message| matches!(&message.media, Some(Media::Photo { file_id: id, .. }) if *id == file_id))
        .expect("the visible photo belongs to this pane");
    media::PhotoOwner::Message(message.chat_id, message.id, media::PhotoKind::Photo)
}

fn show_owner(app: &mut App, window: WinId, owner: media::PhotoOwner, file_id: i32) {
    let epoch = app.photo_epoch(window);
    drop(app.update(Msg::Pane(
        window,
        PaneMsg::MediaShown(owner, file_id, epoch),
    )));
}

fn hide_owner(app: &mut App, window: WinId, owner: media::PhotoOwner, file_id: i32) {
    let epoch = app.photo_epoch(window);
    drop(app.update(Msg::Pane(window, PaneMsg::MediaGone(owner, file_id, epoch))));
}

fn show(app: &mut App, window: WinId, file_id: i32) {
    let owner = owner(app, window, file_id);
    show_owner(app, window, owner, file_id);
}

fn hide(app: &mut App, window: WinId, file_id: i32) {
    let owner = owner(app, window, file_id);
    hide_owner(app, window, owner, file_id);
}

fn decode(app: &mut App, file_id: i32, width: u32, height: u32) {
    let rgba = vec![0; width as usize * height as usize * 4];
    drop(app.update(Msg::ImageDecoded(file_id, Ok((width, height, rgba)))));
}

fn fill_visible_photos(app: &mut App, window: WinId, chat_id: i64) {
    app.session.panes.get_mut(&window).unwrap().view_size = Some(ALL_PHOTOS_VIEW);
    for offset in 0..PHOTO_COUNT {
        let file_id = FIRST_FILE + offset;
        add_photo(app, chat_id, 10 + i64::from(offset), file_id);
        show(app, window, file_id);
    }
    for offset in 0..PHOTO_COUNT {
        decode(app, FIRST_FILE + offset, PHOTO_W, PHOTO_H);
    }
    assert_eq!(
        app.session.images.total(),
        PHOTO_COUNT as usize * PHOTO_BYTES
    );
    assert!(app.session.images.total() > BUDGET);
    assert!(app.session.images.peek(FIRST_FILE).is_some());
}

#[test]
fn memory_media_hiding_a_photo_reclaims_over_budget_invisible_pixels_without_another_decode() {
    let mut app = app();
    let window = app.main_window;
    select(&mut app, 1, &[]);
    fill_visible_photos(&mut app, window, 1);

    hide(&mut app, window, FIRST_FILE);
    assert!(
        app.session.images.peek(FIRST_FILE).is_none(),
        "a hidden over-budget image must be reclaimed without waiting for another insertion"
    );
    assert!(
        app.session.images.peek(FIRST_FILE + 1).is_some(),
        "visible photos remain displayed"
    );
    assert_eq!(
        app.session.images.total(),
        (PHOTO_COUNT as usize - 1) * PHOTO_BYTES
    );
    assert!(app.session.images.total() <= BUDGET);
}

#[test]
fn memory_media_switching_main_chat_releases_old_photo_for_eviction() {
    let mut app = app();
    let window = app.main_window;
    select(&mut app, 1, &[]);
    fill_visible_photos(&mut app, window, 1);

    select(&mut app, 2, &[]);
    add_photo(&mut app, 2, 100, SMALL_FILE);
    show(&mut app, window, SMALL_FILE);
    decode(&mut app, SMALL_FILE, 1, 1);

    assert!(
        app.session.images.peek(FIRST_FILE).is_none(),
        "a previous chat's first picture is not visible in the newly selected pane"
    );
    assert!(app.session.images.peek(SMALL_FILE).is_some());
    assert!(app.session.images.total() <= BUDGET);
}

#[test]
fn memory_media_same_file_widgets_and_windows_keep_independent_visibility_lifetimes() {
    let mut app = app();
    let main = app.main_window;
    select(&mut app, 1, &[]);
    let second = open_window(&mut app, 1, &[]);
    select(&mut app, 1, &[]); // Opening its own window moved this chat out of the main pane.
    app.session.panes.get_mut(&second).unwrap().view_size = Some(ALL_PHOTOS_VIEW);
    fill_visible_photos(&mut app, main, 1);
    // Two distinct bubbles in the main pane reference the same file. The
    // second window displays it as well, with its own window-scoped owner.
    add_photo(&mut app, 1, 100, FIRST_FILE);
    let original = media::PhotoOwner::Message(1, 10, media::PhotoKind::Photo);
    let duplicate = media::PhotoOwner::Message(1, 100, media::PhotoKind::Photo);
    show_owner(&mut app, main, duplicate, FIRST_FILE);
    show_owner(&mut app, second, original, FIRST_FILE);

    hide_owner(&mut app, main, original, FIRST_FILE);
    assert!(
        app.session.images.peek(FIRST_FILE).is_some(),
        "the other bubble and window still display this file"
    );
    assert!(app.session.images.total() > BUDGET);
    drop(app.update(Msg::WindowClosed(second)));
    assert!(
        app.session.images.peek(FIRST_FILE).is_some(),
        "closing another window cannot evict the main pane's second bubble"
    );
    hide_owner(&mut app, main, duplicate, FIRST_FILE);
    assert!(
        app.session.images.peek(FIRST_FILE).is_none(),
        "hiding the final same-file widget releases the over-budget photo"
    );
    assert!(app.session.images.peek(FIRST_FILE + 1).is_some());
    assert!(app.session.images.total() <= BUDGET);
}

#[test]
fn memory_media_switching_one_window_preserves_shared_photo_until_other_window_hides_it() {
    let mut app = app();
    let main = app.main_window;
    select(&mut app, 1, &[]);
    let second = open_window(&mut app, 1, &[]);
    select(&mut app, 1, &[]);
    app.session.panes.get_mut(&second).unwrap().view_size = Some(ALL_PHOTOS_VIEW);
    fill_visible_photos(&mut app, main, 1);
    for file_id in FIRST_FILE..FIRST_FILE + PHOTO_COUNT {
        show(&mut app, second, file_id);
    }

    select(&mut app, 2, &[]);
    add_photo(&mut app, 2, 100, SMALL_FILE);
    show(&mut app, main, SMALL_FILE);
    decode(&mut app, SMALL_FILE, 1, 1);
    assert!(
        app.session.images.peek(FIRST_FILE).is_some(),
        "switching the main pane must not evict another window's visible photo"
    );
    assert!(app.session.images.peek(SMALL_FILE).is_some());
    assert!(app.session.images.total() > BUDGET);

    hide(&mut app, second, FIRST_FILE);
    assert!(
        app.session.images.peek(FIRST_FILE).is_none(),
        "the last owner leaving releases the old photo without another decode"
    );
    assert!(app.session.images.peek(FIRST_FILE + 1).is_some());
    assert!(app.session.images.peek(SMALL_FILE).is_some());
    assert!(app.session.images.total() <= BUDGET);
}

#[test]
fn memory_media_hidden_photo_stays_cached_before_five_minutes_and_expires_at_five_minutes() {
    let mut app = app();
    let window = app.main_window;
    select(&mut app, 1, &[]);
    add_photo(&mut app, 1, 10, FIRST_FILE);
    show(&mut app, window, FIRST_FILE);
    decode(&mut app, FIRST_FILE, PHOTO_W, PHOTO_H);
    hide(&mut app, window, FIRST_FILE);
    let hidden_at = app
        .session
        .images
        .inactive_since(FIRST_FILE)
        .expect("photo became invisible");

    drop(app.update(Msg::MemorySweep(hidden_at + Duration::from_secs(299))));
    assert!(
        app.session.images.peek(FIRST_FILE).is_some(),
        "a hidden below-budget photo remains cached before five minutes"
    );

    drop(app.update(Msg::MemorySweep(hidden_at + Duration::from_secs(300))));
    assert!(
        app.session.images.peek(FIRST_FILE).is_none(),
        "an invisible photo expires at the five-minute boundary"
    );
    assert_eq!(app.session.images.total(), 0);
}

#[test]
fn memory_media_visible_in_another_window_survives_sweep_then_expires_after_final_hide() {
    let mut app = app();
    let main = app.main_window;
    select(&mut app, 1, &[]);
    let second = open_window(&mut app, 1, &[]);
    select(&mut app, 1, &[]);
    add_photo(&mut app, 1, 10, FIRST_FILE);
    show(&mut app, main, FIRST_FILE);
    show(&mut app, second, FIRST_FILE);
    decode(&mut app, FIRST_FILE, PHOTO_W, PHOTO_H);

    hide(&mut app, main, FIRST_FILE);
    assert!(
        app.session.images.inactive_since(FIRST_FILE).is_none(),
        "the other window still owns the image"
    );
    drop(app.update(Msg::MemorySweep(Instant::now() + Duration::from_secs(301))));
    assert!(
        app.session.images.peek(FIRST_FILE).is_some(),
        "no time limit applies to an image still visible in another window"
    );

    hide(&mut app, second, FIRST_FILE);
    let hidden_at = app
        .session
        .images
        .inactive_since(FIRST_FILE)
        .expect("last window hid the photo");
    drop(app.update(Msg::MemorySweep(hidden_at + Duration::from_secs(300))));
    assert!(
        app.session.images.peek(FIRST_FILE).is_none(),
        "five minutes start only after the last owner leaves"
    );
}

#[test]
fn memory_media_decode_completing_while_invisible_gets_its_own_expiry_deadline() {
    let mut app = app();
    select(&mut app, 1, &[]);
    add_photo(&mut app, 1, 10, FIRST_FILE);
    td(
        &mut app,
        json!({"@type": "updateFile",
               "file": file(FIRST_FILE, 1000, 1000, true, "/tmp/telega-memory-photo.png")}),
    );
    let window = app.main_window;
    show(&mut app, window, FIRST_FILE);
    // The decode task is deliberately left unpolled: its completion can
    // arrive after the sensor leaves, without reading the fixture path.
    hide(&mut app, window, FIRST_FILE);
    decode(&mut app, FIRST_FILE, PHOTO_W, PHOTO_H);
    let decoded_at = app
        .session
        .images
        .inactive_since(FIRST_FILE)
        .expect("decoded while invisible");

    drop(app.update(Msg::MemorySweep(decoded_at + Duration::from_secs(299))));
    assert!(
        app.session.images.peek(FIRST_FILE).is_some(),
        "a newly decoded invisible photo remains cached before expiry"
    );
    drop(app.update(Msg::MemorySweep(decoded_at + Duration::from_secs(300))));
    assert!(
        app.session.images.peek(FIRST_FILE).is_none(),
        "a decode completed after its sensor left expires five minutes after completion"
    );
}

#[test]
fn memory_media_replacing_visible_photo_message_releases_stale_owner_without_sensor_hide() {
    let mut app = app();
    let main = app.main_window;
    select(&mut app, 1, &[]);
    fill_visible_photos(&mut app, main, 1);

    td(
        &mut app,
        json!({"@type": "updateMessageContent", "chat_id": 1, "message_id": 10,
               "new_content": {"@type": "messageText", "text": {
                   "@type": "formattedText", "text": "photo replaced", "entities": []}}}),
    );
    drop(app.update(Msg::MemorySweep(Instant::now())));
    assert!(
        app.session.images.peek(FIRST_FILE).is_none(),
        "the replaced photo is no longer displayed and must release memory under pressure"
    );
    assert!(
        app.session.images.peek(FIRST_FILE + 1).is_some(),
        "the remaining visible message still has its decoded image"
    );
    assert!(app.session.images.total() <= BUDGET);
}

#[test]
fn memory_media_stale_show_after_message_replacement_cannot_pin_old_photo() {
    let mut app = app();
    let main = app.main_window;
    select(&mut app, 1, &[]);
    fill_visible_photos(&mut app, main, 1);
    let delayed_show = Msg::Pane(
        main,
        PaneMsg::MediaShown(
            media::PhotoOwner::Message(1, 10, media::PhotoKind::Photo),
            FIRST_FILE,
            app.photo_epoch(main),
        ),
    );

    td(
        &mut app,
        json!({"@type": "updateMessageContent", "chat_id": 1, "message_id": 10,
               "new_content": {"@type": "messageText", "text": {
                   "@type": "formattedText", "text": "photo replaced", "entities": []}}}),
    );
    drop(app.update(Msg::MemorySweep(Instant::now())));
    assert!(
        app.session.images.peek(FIRST_FILE).is_none(),
        "the old image was released when its bubble disappeared"
    );

    drop(app.update(delayed_show));
    // Its old asynchronous decode completes after the new text has replaced
    // the bubble. A stale show must not protect this invisible image.
    decode(&mut app, FIRST_FILE, PHOTO_W, PHOTO_H);
    assert!(
        app.session.images.peek(FIRST_FILE).is_none(),
        "a queued show from the replaced photo cannot pin late decoded pixels"
    );
    assert!(app.session.images.peek(FIRST_FILE + 1).is_some());
    assert!(app.session.images.total() <= BUDGET);
}

#[test]
fn memory_media_old_hide_after_reopening_same_photo_cannot_remove_new_visibility() {
    let mut app = app();
    let main = app.main_window;
    select(&mut app, 1, &[]);
    fill_visible_photos(&mut app, main, 1);
    let delayed_hide = Msg::Pane(
        main,
        PaneMsg::MediaGone(
            media::PhotoOwner::Message(1, 10, media::PhotoKind::Photo),
            FIRST_FILE,
            app.photo_epoch(main),
        ),
    );

    select(&mut app, 2, &[]);
    select(&mut app, 1, &[]);
    fill_visible_photos(&mut app, main, 1);
    assert!(
        app.session.images.peek(FIRST_FILE).is_some(),
        "reopened chat displays its photo"
    );

    drop(app.update(delayed_hide));
    assert!(
        app.session.images.peek(FIRST_FILE).is_some(),
        "a hide queued for the previous pane incarnation cannot evict the newly visible photo"
    );
    assert!(
        app.session.images.total() > BUDGET,
        "all visible photos remain protected"
    );
}
