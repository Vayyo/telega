//! Offline sandbox: the real `App` fed with synthetic TDLib updates and
//! answers, drawn headlessly into PNG files. No network, no account, no
//! screen: `cargo test --bin telega sandbox -- --ignored` writes the scenes
//! to `target/sandbox/`.

use iced::{Font, Pixels, Size, mouse};
use iced_runtime::core::renderer::{Headless, Style};
use iced_runtime::user_interface::{Cache, UserInterface};
use serde_json::{Value, json};

use super::tests::{app, chat_item, folders_update, message, td};
use super::*;

pub(super) fn renderer() -> iced::Renderer {
    iced::futures::executor::block_on(<iced::Renderer as Headless>::new(
        Font::DEFAULT,
        Pixels(16.0),
        Some("tiny-skia"),
    ))
    .expect("headless renderer")
}

/// Sends a pointer click to the actual widget tree without running TDLib tasks.
pub(super) fn click(app: &mut App, window: WinId, position: iced::Point) -> Vec<Msg> {
    click_sized(app, window, SIZE, position)
}

/// A real pointer click at a scene's size (also used for narrow layouts).
fn click_sized(app: &mut App, window: WinId, size: Size, position: iced::Point) -> Vec<Msg> {
    let mut renderer = renderer();
    let mut messages = Vec::new();
    {
        let mut ui = UserInterface::build(app.view(window), size, Cache::default(), &mut renderer);
        for event in [
            mouse::Event::ButtonPressed(mouse::Button::Left),
            mouse::Event::ButtonReleased(mouse::Button::Left),
        ] {
            let _ = ui.update(
                &[iced::Event::Mouse(event)],
                mouse::Cursor::Available(position),
                &mut renderer,
                &mut iced_runtime::core::clipboard::Null,
                &mut messages,
            );
        }
    }
    for message in &messages {
        let _ = app.update(message.clone());
    }
    messages
}

/// Runs `frames` frames of `window` the way the event loop does: layout,
/// a redraw event, messages back into the app, draw. Returns the RGBA
/// pixels of the last frame, taken while its widget tree is alive (text
/// is drawn from it).
pub(super) fn run_frames(
    app: &mut App,
    window: WinId,
    size: Size,
    frames: usize,
    renderer: &mut iced::Renderer,
) -> Vec<u8> {
    let theme = app.theme(window).unwrap_or(iced::Theme::Dark);
    let mut cache = Cache::default();
    let mut pixels = Vec::new();
    for frame in 0..frames {
        let mut messages = Vec::new();
        {
            let mut ui = UserInterface::build(app.view(window), size, cache, renderer);
            let _ = ui.update(
                &[iced::Event::Window(iced::window::Event::RedrawRequested(
                    std::time::Instant::now(),
                ))],
                mouse::Cursor::Unavailable,
                renderer,
                &mut iced_runtime::core::clipboard::Null,
                &mut messages,
            );
            ui.draw(
                renderer,
                &theme,
                &Style {
                    text_color: theme.palette().text,
                },
                mouse::Cursor::Unavailable,
            );
            if frame + 1 == frames {
                pixels = renderer.screenshot(
                    Size::new(size.width as u32, size.height as u32),
                    1.0,
                    theme.palette().background,
                );
            }
            cache = ui.into_cache();
        }
        for message in messages {
            let _ = app.update(message);
        }
    }
    pixels
}

/// Writes a scene to `target/sandbox/<name>.png`.
pub(super) fn save(name: &str, size: Size, pixels: Vec<u8>) -> std::path::PathBuf {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/sandbox");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{name}.png"));
    image::RgbaImage::from_raw(size.width as u32, size.height as u32, pixels)
        .expect("frame size")
        .save(&path)
        .unwrap();
    path
}

// ---- Synthetic world ------------------------------------------------------

pub(super) const ME: i64 = 1000;

fn now() -> i32 {
    chrono::Utc::now().timestamp() as i32
}

/// Position of a chat in the main list, as TDLib sends it with updates.
fn positions(app: &App, chat_id: i64) -> Value {
    let Some(chat) = app.session.chats.get(&chat_id) else {
        return json!([]);
    };
    let mut list = vec![
        json!({"@type": "chatPosition", "list": {"@type": "chatListMain"},
            "order": chat.order.to_string(), "is_pinned": chat.pinned}),
    ];
    for &(folder, order) in &chat.folders {
        list.push(json!({"@type": "chatPosition",
            "list": {"@type": "chatListFolder", "chat_folder_id": folder},
            "order": order.to_string(), "is_pinned": false}));
    }
    Value::Array(list)
}

/// Newest message of a chat, as the list shows it.
pub(super) fn last_message(app: &mut App, chat_id: i64, message: &Value) {
    let positions = positions(app, chat_id);
    td(
        app,
        json!({"@type": "updateChatLastMessage", "chat_id": chat_id,
               "last_message": message, "positions": positions}),
    );
}

/// Chat as the list shows it.
pub(super) fn chat(app: &mut App, id: i64, title: &str, private: bool, order: i64) {
    app.session.chats.insert(id, chat_item(title, private));
    app.set_order(id, order);
}

pub(super) fn user(app: &mut App, id: i64, name: &str) {
    app.session.users.insert(id, name.into());
}

/// A text message as TDLib sends it.
pub(super) fn msg(chat_id: i64, id: i64, from: i64, minutes_ago: i32, text: &str) -> Value {
    let mut m = message(chat_id, id, text);
    m["sender_id"] = json!({"@type": "messageSenderUser", "user_id": from});
    m["is_outgoing"] = json!(from == ME);
    m["date"] = json!(now() - minutes_ago * 60);
    m
}

/// Adds formatting entities (UTF-16 offset, length, type) to a text message.
pub(super) fn formatted(mut m: Value, entities: &[(i32, i32, &str)]) -> Value {
    m["content"]["text"]["entities"] = entities
        .iter()
        .map(|(offset, length, kind)| {
            json!({"@type": "textEntity", "offset": offset, "length": length,
                   "type": {"@type": kind}})
        })
        .collect();
    m
}

/// Opens `chat_id` in the main window with `history` as TDLib's answer and
/// makes the chat list show its newest message.
pub(super) fn open_with(app: &mut App, chat_id: i64, history: Vec<Value>) {
    let window = app.main_window;
    if let Some(last) = history.last() {
        last_message(app, chat_id, last);
    }
    app.session
        .panes
        .get_mut(&window)
        .unwrap()
        .switch_to(chat_id);
    let messages = history
        .into_iter()
        .map(|m| serde_json::from_value(m).unwrap())
        .collect();
    let _ = app.update(Msg::Pane(
        window,
        PaneMsg::HistoryLoaded(chat_id, Ok(messages)),
    ));
}

/// A downloaded file as TDLib describes it.
pub(super) fn local_file(id: i32, path: &std::path::Path) -> tdlib_rs::types::File {
    let size = std::fs::metadata(path).map_or(0, |m| m.len() as i64);
    serde_json::from_value(json!({
        "@type": "file", "id": id, "size": size, "expected_size": size,
        "local": {"@type": "localFile", "path": path.to_string_lossy(),
                  "can_be_downloaded": true, "can_be_deleted": true,
                  "is_downloading_active": false, "is_downloading_completed": true,
                  "download_offset": 0, "downloaded_prefix_size": size,
                  "downloaded_size": size},
        "remote": {"@type": "remoteFile", "id": "", "unique_id": "",
                   "is_uploading_active": false, "is_uploading_completed": true,
                   "uploaded_size": size}
    }))
    .unwrap()
}

/// Gives a peer a generated profile photo (a two-color gradient), through
/// the app's own path: file → decode → cache.
pub(super) fn photo(app: &mut App, peer: avatars::Peer, file_id: i32, from: [u8; 3], to: [u8; 3]) {
    // Unique per call, not just per `file_id`: several `#[ignore]` sandbox
    // tests run in parallel and each builds its own `world()`, so sharing a
    // path would let one test's write race another's read of the same file.
    static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/sandbox/files");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("avatar-{file_id}-{}-{n}.png", std::process::id()));
    let img = image::RgbImage::from_fn(160, 160, |x, y| {
        let t = (x + y) as f32 / 318.0;
        image::Rgb(std::array::from_fn(|i| {
            (f32::from(from[i]) * (1.0 - t) + f32::from(to[i]) * t) as u8
        }))
    });
    img.save(&path).unwrap();
    let _ = app.set_avatar(peer, Some(&local_file(file_id, &path)));
    let decoded = avatars::decode_cached(&path.to_string_lossy());
    let client_id = app.session.client_id;
    let _ = app.update(Msg::AvatarDecoded(client_id, file_id, decoded));
}

/// Logged-in app with a handful of chats.
pub(super) fn world() -> App {
    let mut app = app();
    app.session.my_id = Some(ME);
    app.settings.accounts = vec![crate::settings::Account {
        slot: 0,
        user_id: Some(ME),
        name: "Demo User".into(),
        ..Default::default()
    }];
    user(&mut app, ME, "Demo User");
    user(&mut app, 7, "Игорь Петров");
    user(&mut app, 8, "Маша");
    user(&mut app, 9, "Денис");
    chat(&mut app, 1, "Rust чат", false, 50);
    chat(&mut app, 2, "Денис", true, 40);
    chat(&mut app, 3, "Маша", true, 30);
    chat(&mut app, 4, "Новости", false, 20);
    for (chat_id, from, text) in [
        (2, 9, "А, прикольно"),
        (3, ME, "Скинешь фотки?"),
        (4, 7, "Вышел iced 0.14"),
    ] {
        last_message(&mut app, chat_id, &msg(chat_id, 1, from, 90, text));
    }
    app.session.chats.get_mut(&4).unwrap().unread = 3;
    // A muted chat with unread messages, a pinned one and two archived.
    chat(&mut app, 5, "Флудилка", false, 10);
    last_message(&mut app, 5, &msg(5, 1, 7, 200, "кто в кино?"));
    let flood = app.session.chats.get_mut(&5).unwrap();
    flood.unread = 42;
    flood.notify.mute_for = i32::MAX;
    app.session.chats.get_mut(&2).unwrap().pinned = true;
    app.session.chats.get_mut(&2).unwrap().draft = Some(tdlib_rs::types::FormattedText {
        text: "завтра в 7 подойдёт?".into(),
        entities: Vec::new(),
    });
    td(
        &mut app,
        json!({"@type": "updateChatFolders", "main_chat_list_position": 0, "are_tags_enabled": false,
               "chat_folders": [
                   {"@type": "chatFolderInfo", "id": 1, "color_id": -1, "is_shareable": false,
                    "has_my_invite_links": false, "icon": {"@type": "chatFolderIcon", "name": "Private"},
                    "name": {"@type": "chatFolderName", "animate_custom_emoji": false,
                             "text": {"@type": "formattedText", "text": "Личные", "entities": []}}},
                   {"@type": "chatFolderInfo", "id": 2, "color_id": -1, "is_shareable": false,
                    "has_my_invite_links": false, "icon": {"@type": "chatFolderIcon", "name": "Work"},
                    "name": {"@type": "chatFolderName", "animate_custom_emoji": false,
                             "text": {"@type": "formattedText", "text": "Работа", "entities": []}}}]}),
    );
    for (chat, folder) in [(2, 1), (3, 1), (1, 2), (4, 2)] {
        app.set_folder_order(chat, folder, 100 - chat);
    }
    app.session.unread = (45, 3);
    app.session.folder_unread.insert(1, (0, 0));
    app.session.folder_unread.insert(2, (3, 3));
    for (id, title) in [(6, "Старый проект"), (7, "Рассылка")] {
        chat(&mut app, id, title, false, 0);
        app.set_archive_order(id, 100 - id);
    }
    photo(
        &mut app,
        avatars::Peer::Chat(2),
        9001,
        [80, 140, 230],
        [30, 60, 120],
    );
    photo(
        &mut app,
        avatars::Peer::User(8),
        9002,
        [240, 150, 90],
        [200, 60, 100],
    );
    photo(
        &mut app,
        avatars::Peer::Chat(3),
        9002,
        [240, 150, 90],
        [200, 60, 100],
    );
    app
}

fn window_of(app: &App) -> WinId {
    app.main_window
}

const SIZE: Size = Size::new(1000.0, 700.0);

fn scene(name: &str, app: &mut App) -> std::path::PathBuf {
    scene_sized(name, app, SIZE)
}

fn scene_sized(name: &str, app: &mut App, size: Size) -> std::path::PathBuf {
    let window = app.main_window;
    let pixels = run_frames(app, window, size, 4, &mut renderer());
    save(name, size, pixels)
}

/// The group chat most scenes start from.
fn group(app: &mut App) {
    open_with(
        app,
        1,
        vec![
            msg(1, 1, 7, 50, "Кто пробовал iced 0.14?"),
            msg(1, 2, 8, 45, "Я, норм. Только keyed_column падает"),
            msg(
                1,
                3,
                ME,
                40,
                "Да, у меня тоже падал, перешёл на обычный column",
            ),
            formatted(
                msg(1, 4, 7, 30, "Жирно и курсивом, а тут спойлер"),
                &[
                    (0, 5, "textEntityTypeBold"),
                    (8, 8, "textEntityTypeItalic"),
                    (24, 7, "textEntityTypeSpoiler"),
                ],
            ),
            formatted(
                msg(1, 5, ME, 3, "Сейчас скину пример: cargo run --release"),
                &[(20, 19, "textEntityTypeCode")],
            ),
        ],
    );
    td(
        app,
        json!({"@type": "updateChatReadOutbox", "chat_id": 1, "last_read_outbox_message_id": 3}),
    );
    let window = app.main_window;
    let pinned = vec![
        serde_json::from_value(msg(1, 2, 8, 45, "Я, норм. Только keyed_column падает")).unwrap(),
        serde_json::from_value(msg(1, 1, 7, 50, "Кто пробовал iced 0.14?")).unwrap(),
    ];
    let _ = app.update(Msg::Pane(window, PaneMsg::PinnedLoaded(1, Ok(pinned))));
    let mut forwarded = msg(1, 6, 8, 2, "Смотрите, что нашла: iced теперь умеет в стеки");
    forwarded["forward_info"] = json!({"@type": "messageForwardInfo", "date": 0,
        "public_service_announcement_type": "",
        "origin": {"@type": "messageOriginChat", "sender_chat_id": 4, "author_signature": ""}});
    let _ = app.update(Msg::Td(
        0,
        Box::new(
            serde_json::from_value(json!({"@type": "updateNewMessage", "message": forwarded}))
                .unwrap(),
        ),
    ));
    // Roles and member tags next to names.
    let _ = app.update(Msg::RolesLoaded(
        1,
        Ok(std::collections::HashMap::from([(
            7,
            crate::td::Role {
                owner: true,
                admin: true,
                tag: String::new(),
            },
        )])),
    ));
    for m in &mut app.session.panes.get_mut(&window).unwrap().messages {
        if m.sender
            == tdlib_rs::enums::MessageSender::User(tdlib_rs::types::MessageSenderUser {
                user_id: 8,
            })
        {
            m.sender_tag = "дизайнер".into();
        }
    }
    // A mouse selection in a message.
    let _ = app.update(Msg::Pane(
        window,
        PaneMsg::TextPress(3, "Да, у меня ".len()),
    ));
    let _ = app.update(Msg::Pane(
        window,
        PaneMsg::TextDrag(3, "Да, у меня тоже падал, перешёл".len()),
    ));
    let _ = app.update(Msg::MouseReleased);
    // Pinned for oneself (the chat's pins stay behind it in the bar).
    let _ = app.update(Msg::Pane(window, PaneMsg::PinLocal(3, true)));
    reactions(app, 1, 2, &[("👍", 2, false), ("🔥", 1, true)]);
    reactions(app, 1, 3, &[("😂", 4, false)]);
}

/// Reactions on a message as TDLib reports them: (emoji, count, own).
pub(super) fn reactions(app: &mut App, chat_id: i64, id: i64, list: &[(&str, i32, bool)]) {
    let list: Vec<Value> = list
        .iter()
        .map(|(emoji, count, own)| {
            json!({"@type": "messageReaction", "type": {"@type": "reactionTypeEmoji", "emoji": emoji},
                   "total_count": count, "is_chosen": own, "recent_sender_ids": []})
        })
        .collect();
    td(
        app,
        json!({"@type": "updateMessageInteractionInfo", "chat_id": chat_id, "message_id": id,
               "interaction_info": {"@type": "messageInteractionInfo", "view_count": 0,
                   "forward_count": 0, "reactions": {"@type": "messageReactions",
                   "are_tags": false, "paid_reactors": [], "can_get_added_reactions": false,
                   "reactions": list}}}),
    );
}

/// History over several days with unread messages at the end.
fn days_and_unread(app: &mut App) {
    let chat = app.session.chats.get_mut(&3).unwrap();
    chat.unread = 2;
    chat.read_inbox = 4;
    open_with(
        app,
        3,
        vec![
            msg(3, 1, 8, 60 * 24 * 40, "Привет! Давно не виделись"),
            msg(3, 2, ME, 60 * 24 * 40 - 5, "Привет) Да, надо встретиться"),
            msg(3, 3, 8, 60 * 26, "Ты завтра свободен?"),
            msg(3, 4, ME, 60 * 25, "Вечером да"),
            msg(3, 5, 8, 20, "Скинешь фотки с поездки?"),
            msg(3, 6, 8, 19, "Особенно ту, с горами"),
        ],
    );
}

/// A channel with a link preview, a poll, a contact and a place.
fn cards(app: &mut App) {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/sandbox/files");
    std::fs::create_dir_all(&dir).unwrap();
    let thumb = dir.join("link-thumb.png");
    image::RgbImage::from_fn(160, 160, |x, y| {
        image::Rgb([(x * 3 / 2) as u8, 90, (y * 3 / 2) as u8])
    })
    .save(&thumb)
    .unwrap();
    let file = serde_json::to_value(local_file(9100, &thumb)).unwrap();
    let mut link = msg(
        4,
        1,
        7,
        60,
        "Вышел iced 0.14: https://github.com/iced-rs/iced",
    );
    link["content"]["link_preview"] = json!({"@type": "linkPreview",
        "url": "https://github.com/iced-rs/iced", "display_url": "github.com/iced-rs/iced",
        "site_name": "GitHub", "title": "iced-rs/iced",
        "description": {"@type": "formattedText", "text": "A cross-platform GUI library for Rust, inspired by Elm", "entities": []},
        "author": "", "type": {"@type": "linkPreviewTypeArticle", "photo": {"@type": "photo",
            "has_stickers": false, "sizes": [{"@type": "photoSize", "type": "m", "photo": file,
            "width": 160, "height": 160, "progressive_sizes": []}]}},
        "has_large_media": false, "show_large_media": false, "show_media_above_description": false,
        "skip_confirmation": false, "show_above_text": false, "instant_view_version": 0});
    let poll = |id: i64, chosen: bool| {
        let mut m = msg(4, id, 7, 50 - id as i32, "");
        m["content"] = json!({"@type": "messagePoll", "description": {"@type": "formattedText", "text": "", "entities": []}, "can_add_option": false, "poll": {"@type": "poll", "can_get_voters": false, "can_see_results": true, "allows_multiple_answers": false, "allows_revoting": true, "members_only": false, "country_codes": [], "option_order": [], "id": id.to_string(),
            "question": {"@type": "formattedText", "text": "На чём пишете GUI?", "entities": []},
            "options": [
                {"@type": "pollOption", "id": "1", "recent_voter_ids": [], "addition_date": 0, "text": {"@type": "formattedText", "text": "iced", "entities": []},
                 "voter_count": 12, "vote_percentage": 60, "is_chosen": chosen, "is_being_chosen": false},
                {"@type": "pollOption", "id": "2", "recent_voter_ids": [], "addition_date": 0, "text": {"@type": "formattedText", "text": "egui", "entities": []},
                 "voter_count": 6, "vote_percentage": 30, "is_chosen": false, "is_being_chosen": false},
                {"@type": "pollOption", "id": "3", "recent_voter_ids": [], "addition_date": 0, "text": {"@type": "formattedText", "text": "slint", "entities": []},
                 "voter_count": 2, "vote_percentage": 10, "is_chosen": false, "is_being_chosen": false}],
            "total_voter_count": 20, "recent_voter_ids": [], "is_anonymous": true,
            "type": {"@type": "pollTypeRegular"},
            "open_period": 0, "close_date": 0, "is_closed": false}});
        m
    };
    let mut contact = msg(4, 4, 8, 30, "");
    contact["content"] = json!({"@type": "messageContact", "contact": {"@type": "contact",
        "phone_number": "5550100", "first_name": "Маша", "last_name": "", "vcard": "", "user_id": 8}});
    let mut venue = msg(4, 5, 8, 20, "");
    venue["content"] = json!({"@type": "messageVenue", "venue": {"@type": "venue",
        "location": {"@type": "location", "latitude": 55.751244, "longitude": 37.618423, "horizontal_accuracy": 0.0},
        "title": "Кофейня на Никольской", "address": "Никольская ул., 10", "provider": "", "id": "", "type": ""}});
    open_with(
        app,
        4,
        vec![link, poll(2, false), poll(3, true), contact, venue],
    );
    // The link picture as the app gets it: shown, decoded, cached.
    let _ = app.update(Msg::Pane(window_of(app), PaneMsg::MediaVisible(9100)));
    let decoded = media::decode(&thumb.to_string_lossy());
    let _ = app.update(Msg::ImageDecoded(9100, decoded));
}

#[test]
#[ignore = "writes sandbox screenshots; run explicitly"]
fn sandbox_replies() {
    let mut app = world();
    let reply = |m: Value, to: i64| {
        let mut m = m;
        m["reply_to"] = json!({"@type": "messageReplyToMessage", "poll_option_id": "", "chat_id": 1, "message_id": to,
            "origin_send_date": 0, "checklist_task_id": 0});
        m
    };
    open_with(
        &mut app,
        1,
        vec![
            msg(1, 1, 7, 50, "Кто пробовал iced 0.14?"),
            reply(msg(1, 2, ME, 45, "Я"), 1),
            reply(msg(1, 3, 8, 40, "И как?"), 2),
            reply(
                msg(
                    1,
                    4,
                    ME,
                    30,
                    "Норм, только keyed_column падает, перешёл на column",
                ),
                3,
            ),
        ],
    );
    let window = app.main_window;
    let _ = app.update(Msg::Pane(window, PaneMsg::Reply(3)));
    println!("{}", scene("replies", &mut app).display());
}

#[test]
#[ignore = "writes sandbox screenshots; run explicitly"]
fn sandbox_looks() {
    use super::look::{Accent, LookSettings, Scheme};
    for (scheme, accent, name) in [
        (Scheme::Telegram, Accent::Blue, "look-telegram"),
        (Scheme::Graphite, Accent::Violet, "look-graphite"),
        (Scheme::System, Accent::Green, "look-system"),
    ] {
        let mut app = world();
        let window = app.main_window;
        for chat in [2, 3, 1] {
            let _ = app.update(Msg::Pane(window, PaneMsg::SelectChat(chat)));
        }
        let _ = app.update(Msg::SetLook(LookSettings { scheme, accent }));
        group(&mut app);
        let mut reply = msg(1, 7, ME, 1, "Кидай, посмотрю");
        reply["reply_to"] = json!({"@type": "messageReplyToMessage", "poll_option_id": "", "chat_id": 1, "message_id": 4,
            "origin_send_date": 0, "checklist_task_id": 0});
        let _ = app.update(Msg::Td(
            0,
            Box::new(
                serde_json::from_value(json!({"@type": "updateNewMessage", "message": reply}))
                    .unwrap(),
            ),
        ));
        println!("{}", scene(name, &mut app).display());
    }
}

#[test]
#[ignore = "writes sandbox screenshots; run explicitly"]
fn sandbox_user_card() {
    use super::card::CardMsg;
    let mut app = world();
    group(&mut app);
    let window = app.main_window;
    let _ = app.update(Msg::Card(CardMsg::Open(window, 8)));
    let _ = app.update(Msg::Card(CardMsg::Loaded(
        8,
        Ok(crate::td::UserCard {
            name: "Маша".into(),
            username: "masha_design".into(),
            phone: String::new(),
            status: "был(а) недавно".into(),
            bio: "coito ergo sum".into(),
            is_contact: false,
            blocked: false,
            chat_id: 3,
            is_self: false,
        }),
    )));
    println!(
        "{}",
        scene_sized("user-card", &mut app, Size::new(1000.0, 760.0)).display()
    );
    let _ = app.update(Msg::Card(CardMsg::ToggleQr));
    let _ = app.update(Msg::Card(CardMsg::ToggleMore));
    println!(
        "{}",
        scene_sized("user-card-qr", &mut app, Size::new(1000.0, 900.0)).display()
    );
}

#[test]
#[ignore = "writes sandbox screenshots; run explicitly"]
fn sandbox_tabs() {
    let mut app = world();
    let window = app.main_window;
    for chat in [2, 3, 4, 1] {
        let _ = app.update(Msg::Pane(window, PaneMsg::SelectChat(chat)));
    }
    let _ = app.update(Msg::TogglePin(2));
    group(&mut app);
    println!("{}", scene("tabs", &mut app).display());
}

#[test]
#[ignore = "writes sandbox screenshots; run explicitly"]
fn sandbox_main_header() {
    let mut app = world();
    let window = app.main_window;
    app.settings.accounts.push(crate::settings::Account {
        slot: 1,
        user_id: Some(1001),
        name: "Другой аккаунт".into(),
        ..Default::default()
    });
    photo(
        &mut app,
        avatars::Peer::User(ME),
        9010,
        [60, 160, 200],
        [35, 80, 130],
    );
    for chat in [2, 3, 4, 1] {
        let _ = app.update(Msg::Pane(window, PaneMsg::SelectChat(chat)));
    }
    let _ = app.update(Msg::TogglePin(2));
    group(&mut app);
    println!("{}", scene("main-header", &mut app).display());

    let opened = click(&mut app, window, iced::Point::new(21.0, 19.0));
    assert!(opened.iter().any(|m| matches!(m, Msg::ToggleAccounts)));
    assert!(app.session.accounts_open);
    println!("{}", scene("main-header-accounts", &mut app).display());

    let logout = click(&mut app, window, iced::Point::new(92.0, 88.0));
    assert!(logout.iter().any(|m| matches!(m, Msg::LogOut)));
    assert!(!app.session.accounts_open && app.session.confirm_logout);
    println!("{}", scene("main-header-confirm", &mut app).display());

    let cancel = click(&mut app, window, iced::Point::new(94.0, 105.0));
    assert!(
        cancel
            .iter()
            .any(|m| matches!(m, Msg::ConfirmLogOut(false)))
    );
    assert_eq!(app.session.auth, Auth::Ready);
    assert!(!app.session.confirm_logout);
    let _ = click(&mut app, window, iced::Point::new(21.0, 19.0));
    let outside = click(&mut app, window, iced::Point::new(720.0, 400.0));
    assert!(
        outside
            .iter()
            .any(|m| matches!(m, Msg::DismissAccountPopup))
    );
    assert!(!app.session.accounts_open);
    let _ = click(&mut app, window, iced::Point::new(21.0, 19.0));
    let _ = click(&mut app, window, iced::Point::new(92.0, 88.0));
    let outside = click(&mut app, window, iced::Point::new(720.0, 400.0));
    assert!(
        outside
            .iter()
            .any(|m| matches!(m, Msg::DismissAccountPopup))
    );
    assert_eq!(app.session.auth, Auth::Ready);
    assert!(!app.session.confirm_logout);
    let _ = click(&mut app, window, iced::Point::new(21.0, 19.0));
    let _ = app.update(Msg::Key(
        window,
        keyboard::Key::Named(keyboard::key::Named::Escape),
        keyboard::Modifiers::empty(),
    ));
    assert!(!app.session.accounts_open);

    let narrow = Size::new(360.0, 700.0);
    println!(
        "{}",
        scene_sized("main-header-narrow", &mut app, narrow).display()
    );
    let opened = click_sized(&mut app, window, narrow, iced::Point::new(21.0, 19.0));
    assert!(opened.iter().any(|m| matches!(m, Msg::ToggleAccounts)));
    println!(
        "{}",
        scene_sized("main-header-narrow-accounts", &mut app, narrow).display()
    );
    let tab = click_sized(&mut app, window, narrow, iced::Point::new(155.0, 19.0));
    assert!(tab.iter().any(|m| matches!(m, Msg::SelectTab(2))));
    assert!(!app.session.accounts_open && !app.session.confirm_logout);
    assert_eq!(app.session.panes[&window].chat_id, Some(2));

    let _ = app.update(Msg::ShowArchive(window, true));
    let _ = app.update(Msg::ToggleArchiveSettings(window, true));
    assert!(app.session.panes[&window].list.archive_settings.is_some());
    println!(
        "{}",
        scene_sized("main-header-narrow-archive-settings", &mut app, narrow).display()
    );
    let tab = click_sized(&mut app, window, narrow, iced::Point::new(155.0, 19.0));
    assert!(tab.iter().any(|m| matches!(m, Msg::SelectTab(2))));
    assert!(app.session.panes[&window].list.archive_settings.is_none());
    assert!(app.session.panes[&window].list.archive);
    assert_eq!(app.session.panes[&window].chat_id, Some(2));
    println!(
        "{}",
        scene("main-header-archive-chat-again", &mut app).display()
    );
    let settings = click_sized(&mut app, window, narrow, iced::Point::new(60.0, 19.0));
    assert!(settings.iter().any(|m| matches!(m, Msg::OpenSettings)));
    assert!(app.session.settings_open && !app.session.accounts_open);
    let tab = click_sized(&mut app, window, narrow, iced::Point::new(155.0, 19.0));
    assert!(tab.iter().any(|m| matches!(m, Msg::SelectTab(2))));
    assert!(!app.session.settings_open);
    assert_eq!(app.session.panes[&window].chat_id, Some(2));
    let trash = click(&mut app, window, iced::Point::new(99.0, 19.0));
    assert!(trash.iter().any(|m| matches!(m, Msg::CloseAllTabs)));
    assert_eq!(app.session.tabs.chats, [2]);
    let _ = click(&mut app, window, iced::Point::new(21.0, 19.0));
    let _ = click(&mut app, window, iced::Point::new(92.0, 88.0));
    let yes = click(&mut app, window, iced::Point::new(35.0, 103.0));
    assert!(yes.iter().any(|m| matches!(m, Msg::ConfirmLogOut(true))));
    assert_eq!(app.session.auth, Auth::LoggingOut);
}

#[test]
#[ignore = "writes sandbox screenshots; run explicitly"]
fn sandbox_inactive_account_portraits() {
    let _shared = crate::paths::AVATARS_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut app = world();
    app.settings.accounts.push(crate::settings::Account {
        slot: 1,
        user_id: Some(1001),
        name: "Second Account".into(),
        ..Default::default()
    });
    photo(
        &mut app,
        avatars::Peer::User(ME),
        9912,
        [250, 20, 180],
        [210, 50, 155],
    );

    // One TDLib client runs at a time. Recreate its session without polling
    // the close task; the first owner's portrait must outlive that session.
    let _ = app.switch_account(1);
    assert_eq!(app.after_close(), 1);
    app.session = Session::new(0, app.main_window, 1);
    app.session.auth = Auth::Ready;
    app.session.my_id = Some(1001);
    photo(
        &mut app,
        avatars::Peer::User(1001),
        9913,
        [35, 180, 215],
        [10, 85, 160],
    );
    let _ = app.update(Msg::ToggleAccounts);
    println!(
        "{}",
        scene("inactive-account-portraits", &mut app).display()
    );
}

#[test]
#[ignore = "writes sandbox screenshots; run explicitly"]
fn sandbox_folder_picker() {
    let mut app = world();
    let window = app.main_window;
    let _ = app.update(Msg::ChatMenu(window, Some(1)));
    let _ = app.update(Msg::ChatOp(window, 1, ChatOp::Folders));
    println!("{}", scene("folder-picker", &mut app).display());
    let _ = app.update(Msg::FolderAction(window, 1, 2, true));
    let request = app.session.folder_edits[&2];
    let client = app.session.client_id;
    let _ = app.update(Msg::FolderEdited(
        window,
        client,
        1,
        2,
        request,
        Err("PREMIUM_REQUIRED (400)".into()),
    ));
    println!("{}", scene("folder-picker-error", &mut app).display());
}

#[test]
#[ignore = "writes sandbox screenshots; run explicitly"]
fn sandbox_create_folder() {
    let mut app = world();
    let window = app.main_window;
    td(&mut app, folders_update(&[]));
    let _ = app.update(Msg::ChatMenu(window, Some(1)));
    let _ = app.update(Msg::ChatOp(window, 1, ChatOp::NewFolder));
    let _ = app.update(Msg::FolderName(window, 1, "Друзья".into()));
    println!("{}", scene("folder-create-form", &mut app).display());
    let _ = app.update(Msg::ConfirmNewFolder(window, 1));
    let request = app.session.folder_creation.as_ref().unwrap().request;
    let _ = app.update(Msg::FolderCreated(
        window,
        app.session.client_id,
        request,
        Err("FOLDER_COUNT_LIMIT (400)".into()),
    ));
    println!("{}", scene("folder-create-error", &mut app).display());
    let _ = app.update(Msg::ConfirmNewFolder(window, 1));
    let request = app.session.folder_creation.as_ref().unwrap().request;
    let client = app.session.client_id;
    let _ = app.update(Msg::FolderCreated(window, client, request, Ok(14)));
    td(&mut app, folders_update(&[(14, "Друзья")]));
    println!("{}", scene("folder-create-updated", &mut app).display());
}

#[test]
#[ignore = "writes sandbox screenshots; run explicitly"]
fn sandbox_folder_reorder() {
    let mut app = world();
    let window = app.main_window;
    let mut initial = folders_update(&[(1, "Личные"), (2, "Работа"), (3, "Новости")]);
    initial["main_chat_list_position"] = json!(1);
    td(&mut app, initial);
    println!("{}", scene("folder-reorder", &mut app).display());
    let _ = app.update(Msg::ReorderFolder(window, 1, false));
    let request = app.session.folder_reorder.as_ref().unwrap().request;
    let client = app.session.client_id;
    let _ = app.update(Msg::FolderReordered(
        window,
        client,
        request,
        Err("REORDER_FAILED (400)".into()),
    ));
    println!("{}", scene("folder-reorder-error", &mut app).display());
    let _ = app.update(Msg::ReorderFolder(window, 1, false));
    let request = app.session.folder_reorder.as_ref().unwrap().request;
    let mut server = folders_update(&[(2, "Работа"), (1, "Личные"), (3, "Новости")]);
    server["main_chat_list_position"] = json!(1);
    td(&mut app, server);
    let _ = app.update(Msg::FolderReordered(window, client, request, Ok(())));
    println!("{}", scene("folder-reorder-applied", &mut app).display());
    // Ok without the corresponding update leaves the last server order visible.
    let _ = app.update(Msg::ReorderFolder(window, 1, false));
    let request = app.session.folder_reorder.as_ref().unwrap().request;
    println!(
        "{}",
        scene("folder-reorder-before-reply", &mut app).display()
    );
    let _ = app.update(Msg::FolderReordered(window, client, request, Ok(())));
    println!("{}", scene("folder-reorder-unresolved", &mut app).display());
    let _ = app.update(Msg::AcknowledgeFolderReorder(window, client, request));
    println!("{}", scene("folder-reorder-recovered", &mut app).display());
}

#[test]
#[ignore = "writes sandbox screenshots; run explicitly"]
fn sandbox_bulk_read() {
    let mut app = world();
    let window = app.main_window;
    let client = app.session.client_id;
    let _ = app.update(Msg::ShowFolder(window, Some(2)));
    let _ = app.update(Msg::AskReadList(window, client));
    println!("{}", scene("bulk-read-confirm", &mut app).display());
    let token = app.session.panes[&window]
        .list
        .confirm_read
        .as_ref()
        .unwrap()
        .0;
    let _task = app.update(Msg::ConfirmReadList(window, client, token, true));
    let _ = app.update(Msg::ListReadDone(
        window,
        client,
        token,
        Err("NETWORK (500)".into()),
    ));
    println!("{}", scene("bulk-read-error", &mut app).display());
}

#[test]
#[ignore = "writes sandbox screenshots; run explicitly"]
fn sandbox_archive_window() {
    let mut app = world();
    let main = app.main_window;
    let _ = app.update(Msg::OpenArchiveWindow(main, app.session.client_id));
    let archive = *app.session.panes.keys().find(|&&w| w != main).unwrap();
    let size = Size::new(760.0, 700.0);
    let pixels = run_frames(&mut app, archive, size, 4, &mut renderer());
    println!("{}", save("archive-window", size, pixels).display());
    let client = app.session.client_id;
    let _ = app.update(Msg::ArchiveWindowLoaded(
        archive,
        client,
        Err("NETWORK (500)".into()),
    ));
    let pixels = run_frames(&mut app, archive, size, 4, &mut renderer());
    println!("{}", save("archive-window-error", size, pixels).display());
}

#[test]
#[ignore = "writes sandbox screenshots; run explicitly"]
fn sandbox_local_chat_pins() {
    let mut app = world();
    let window = app.main_window;
    // More client-only pins than the ordinary server quota, without a
    // Telegram account or a request to change TDLib's pin state.
    for id in 8..=16 {
        chat(&mut app, id, &format!("Локальный чат {id}"), false, 30 + id);
        let _ = app.update(Msg::ChatMenu(window, Some(id)));
        let _ = app.update(Msg::ChatOp(window, id, ChatOp::LocalPin(true)));
    }
    assert_eq!(app.session.local_main.len(), 9);
    let size = Size::new(1000.0, 1000.0);
    println!(
        "{}",
        scene_sized("local-chat-pins", &mut app, size).display()
    );
    let _ = app.update(Msg::ChatMenu(window, Some(16)));
    println!(
        "{}",
        scene_sized("local-chat-pin-menu", &mut app, size).display()
    );
}

#[test]
#[ignore = "writes sandbox screenshots; run explicitly"]
fn sandbox_archive_placement() {
    let mut app = world();
    let window = app.main_window;
    let client = app.session.client_id;
    let snapshot = |name, app: &mut App| {
        let mut renderer = renderer();
        let _ = run_frames(app, window, SIZE, 4, &mut renderer);
        save(name, SIZE, run_frames(app, window, SIZE, 4, &mut renderer))
    };
    println!(
        "{}",
        snapshot("archive-sidebar-default", &mut app).display()
    );
    let _ = app.update(Msg::ShowArchive(window, true));
    let _ = app.update(Msg::SetArchiveCollapsed(window, client, true));
    assert!(app.archive_collapsed());
    let _ = app.update(Msg::ShowArchive(window, false));
    let _ = app.update(Msg::ToggleAccounts);
    assert!(app.session.accounts_open);
    println!("{}", snapshot("archive-collapsed-menu", &mut app).display());
    let _ = app.update(Msg::SetArchiveCollapsed(window, client, false));
    println!(
        "{}",
        snapshot("archive-sidebar-restored", &mut app).display()
    );
}

#[test]
#[ignore = "writes sandbox screenshots; run explicitly"]
fn sandbox_archive_settings() {
    let mut app = world();
    let window = app.main_window;
    let client = app.session.client_id;
    let _ = app.update(Msg::ShowArchive(window, true));
    let _ = app.update(Msg::ToggleArchiveSettings(window, true));
    let request = app.session.panes[&window]
        .list
        .archive_settings
        .as_ref()
        .unwrap()
        .request;
    let _ = app.update(Msg::ArchiveSettingsLoaded(
        window,
        client,
        request,
        Ok(tdlib_rs::types::ArchiveChatListSettings {
            archive_and_mute_new_chats_from_unknown_users: false,
            keep_unmuted_chats_archived: true,
            keep_chats_from_folders_archived: false,
        }),
    ));
    let _ = app.update(Msg::ArchiveCapabilityLoaded(
        window,
        client,
        request,
        Ok(false),
    ));
    println!("{}", scene("archive-settings", &mut app).display());
    let _ = app.update(Msg::ChangeArchiveFlag(
        window,
        td::ArchiveFlag::KeepFolderChats,
        true,
    ));
    let token = app.session.archive_write.unwrap().1;
    let _ = app.update(Msg::ArchiveFetched(
        window,
        client,
        token,
        td::ArchiveFlag::KeepFolderChats,
        true,
        Ok(tdlib_rs::types::ArchiveChatListSettings {
            archive_and_mute_new_chats_from_unknown_users: false,
            keep_unmuted_chats_archived: true,
            keep_chats_from_folders_archived: false,
        }),
    ));
    let _ = app.update(Msg::ArchiveSaved(
        window,
        client,
        token,
        Err("SETTING_REJECTED (400)".into()),
    ));
    println!("{}", scene("archive-settings-rejected", &mut app).display());
}

#[test]
#[ignore = "writes sandbox screenshots; run explicitly"]
fn sandbox_password() {
    let mut app = world();
    app.session.auth = Auth::Locked {
        error: Some("неверный пароль".into()),
        forgot: true,
    };
    println!("{}", scene("lock", &mut app).display());
    app.session.auth = Auth::Ready;
    let _ = app.update(Msg::OpenSettings);
    println!(
        "{}",
        scene_sized("settings", &mut app, Size::new(1000.0, 900.0)).display()
    );
}

/// Finds each real slider's hit area, drags its pointer, and renders the saved result.
#[test]
#[ignore = "writes offline storage screenshots; run explicitly"]
fn sandbox_storage_settings() {
    const STORAGE_SIZE: Size = Size::new(1000.0, 900.0);
    let mut app = app();
    let window = app.main_window;
    let _ = app.update(Msg::OpenSettings);
    println!(
        "{}",
        scene_sized("storage-defaults", &mut app, STORAGE_SIZE).display()
    );
    assert_eq!(app.settings.cache.gib(), 10.0);
    assert_eq!(app.settings.cache.months(), 3.0);

    for axis in 0..2 {
        let mut renderer = renderer();
        let y = (120..700)
            .step_by(3)
            .find(|&y| {
                let mut messages = Vec::new();
                let mut ui = UserInterface::build(
                    app.view(window),
                    STORAGE_SIZE,
                    Cache::default(),
                    &mut renderer,
                );
                let _ = ui.update(
                    &[iced::Event::Mouse(mouse::Event::ButtonPressed(
                        mouse::Button::Left,
                    ))],
                    mouse::Cursor::Available(iced::Point::new(550.0, y as f32)),
                    &mut renderer,
                    &mut iced_runtime::core::clipboard::Null,
                    &mut messages,
                );
                messages.iter().any(|msg| match msg {
                    Msg::SetCachePolicy(limits) if axis == 0 => {
                        limits.bytes != app.settings.cache.bytes
                            && limits.days == app.settings.cache.days
                    }
                    Msg::SetCachePolicy(limits) => {
                        limits.days != app.settings.cache.days
                            && limits.bytes == app.settings.cache.bytes
                    }
                    _ => false,
                })
            })
            .expect("slider pointer hit area");
        let mut messages = Vec::new();
        {
            let mut ui = UserInterface::build(
                app.view(window),
                STORAGE_SIZE,
                Cache::default(),
                &mut renderer,
            );
            for (event, x) in [
                (mouse::Event::ButtonPressed(mouse::Button::Left), 550.0),
                (
                    mouse::Event::CursorMoved {
                        position: iced::Point::new(750.0, y as f32),
                    },
                    750.0,
                ),
                (mouse::Event::ButtonReleased(mouse::Button::Left), 750.0),
            ] {
                let _ = ui.update(
                    &[iced::Event::Mouse(event)],
                    mouse::Cursor::Available(iced::Point::new(x, y as f32)),
                    &mut renderer,
                    &mut iced_runtime::core::clipboard::Null,
                    &mut messages,
                );
            }
        }
        assert!(
            messages
                .iter()
                .any(|msg| matches!(msg, Msg::ApplyCachePolicy)),
            "slider release commits"
        );
        for message in messages {
            let _ = app.update(message);
        }
    }
    assert!(
        app.settings.cache.gib() > 10.0,
        "size changed by pointer drag"
    );
    assert!(
        app.settings.cache.months() > 3.0,
        "age changed by pointer drag"
    );
    println!(
        "{}",
        scene_sized("storage-changed", &mut app, STORAGE_SIZE).display()
    );
    let chosen = app.settings.cache;
    let _ = app.update(Msg::CloseSettings);
    assert_eq!(
        Settings::load(&app.settings_path).unwrap().cache,
        chosen,
        "choices saved only in this sandbox's temporary settings file"
    );
    app.settings = Settings::load(&app.settings_path).unwrap();
    let _ = app.update(Msg::OpenSettings);
    println!(
        "{}",
        scene_sized("storage-reopened", &mut app, STORAGE_SIZE).display()
    );
    println!(
        "storage saved: {} GiB, {} months",
        chosen.gib(),
        chosen.months()
    );
}

#[test]
#[ignore = "writes sandbox screenshots; run explicitly"]
fn sandbox_message_menu() {
    let mut app = world();
    open_with(
        &mut app,
        2,
        vec![
            msg(2, 1, 9, 4, "Первое сообщение в видимой истории"),
            msg(2, 2, ME, 3, "Ответ на него, справа"),
            msg(
                2,
                3,
                9,
                2,
                "Это сообщение останется на месте под открытым меню",
            ),
            msg(2, 4, ME, 1, "Последнее сообщение у нижней границы"),
        ],
    );
    println!("{}", scene("message-menu-closed", &mut app).display());
    let window = window_of(&app);
    for (id, name, size) in [
        (1, "message-menu-top", SIZE),
        (4, "message-menu-bottom", SIZE),
        (4, "message-menu-narrow", Size::new(540.0, 700.0)),
    ] {
        let _ = app.update(Msg::Pane(window, PaneMsg::OpenMenu(id)));
        let _ = app.update(Msg::Pane(
            window,
            PaneMsg::MenuReady(
                id,
                Ok(crate::td::MessageRights {
                    delete_for_self: true,
                    delete_for_all: true,
                    edit: id == 4,
                }),
            ),
        ));
        let _ = app.update(Msg::Pane(
            window,
            PaneMsg::ReactionsReady(id, Ok(vec!["👍".into(), "🔥".into()])),
        ));
        println!("{}", scene_sized(name, &mut app, size).display());
        if name == "message-menu-top" {
            click(&mut app, window, iced::Point::new(610.0, 400.0));
            println!("{}", scene("message-menu-outside", &mut app).display());
        } else if name == "message-menu-bottom" {
            click(&mut app, window, iced::Point::new(775.0, 634.0));
            println!("{}", scene("message-menu-cancel", &mut app).display());
        }
        let _ = app.update(Msg::Pane(window, PaneMsg::CloseMenu));
    }

    // A viewport that grows until every row fits must not reuse a positive
    // bottom offset cached before the resize.
    let large = Size::new(1000.0, 1100.0);
    let _ = app.update(Msg::Pane(window, PaneMsg::OpenMenu(4)));
    let normal = run_frames(&mut app, window, large, 4, &mut renderer());
    let stale = app.session.panes.get_mut(&window).unwrap();
    stale.scroll = Some(iced::widget::scrollable::AbsoluteOffset { x: 0.0, y: 300.0 });
    let resized = run_frames(&mut app, window, large, 1, &mut renderer());
    assert_eq!(
        normal, resized,
        "fully fitting history ignores stale scroll"
    );
    println!("{}", save("message-menu-resized", large, resized).display());

    let tiny = Size::new(700.0, 300.0);
    let mut compact = world();
    open_with(
        &mut compact,
        2,
        vec![msg(2, 1, 9, 1, "Сообщение в низком окне")],
    );
    let compact_window = window_of(&compact);
    let _ = compact.update(Msg::Pane(compact_window, PaneMsg::OpenMenu(1)));
    let _ = compact.update(Msg::Pane(
        compact_window,
        PaneMsg::ReactionsReady(1, Ok(vec!["👍".into(), "🔥".into()])),
    ));
    let compact_open = run_frames(&mut compact, compact_window, tiny, 4, &mut renderer());
    let _ = compact.update(Msg::Pane(compact_window, PaneMsg::CloseMenu));
    let compact_closed = run_frames(&mut compact, compact_window, tiny, 4, &mut renderer());
    assert_ne!(
        compact_open, compact_closed,
        "tiny viewport retains a popup"
    );
    println!(
        "{}",
        save("message-menu-tiny", tiny, compact_open).display()
    );

    // Keep the target in the prefetched slice, but outside the real viewport.
    let mut long = world();
    open_with(
        &mut long,
        2,
        (1..=45)
            .map(|id| msg(2, id, 9, 46 - id as i32, "Сообщение длинной истории"))
            .collect(),
    );
    let long_window = window_of(&long);
    let closed = run_frames(&mut long, long_window, SIZE, 4, &mut renderer());
    let pane = &long.session.panes[&long_window];
    let visible = pane.visible();
    let target = visible.start;
    assert!(target < visible.end, "prefetched bubble exists");
    let below: f32 = pane.messages[target + 1..]
        .iter()
        .map(|message| pane.height_estimate(message))
        .sum();
    assert!(
        below > pane.view_size.unwrap().0,
        "prefetched bubble is fully above the viewport"
    );
    let target_id = pane.messages[target].id;
    let _ = long.update(Msg::Pane(long_window, PaneMsg::OpenMenu(target_id)));
    let offscreen = run_frames(&mut long, long_window, SIZE, 4, &mut renderer());
    assert_eq!(closed, offscreen, "offscreen bubble has no visible popup");
    println!(
        "{}",
        save("message-menu-offscreen", SIZE, offscreen).display()
    );

    let last_id = long.session.panes[&long_window].messages.last().unwrap().id;
    let _ = long.update(Msg::Pane(long_window, PaneMsg::OpenMenu(last_id)));
    long.session
        .panes
        .get_mut(&long_window)
        .unwrap()
        .loading_newer = true;
    let loading_closed = {
        let menu = long
            .session
            .panes
            .get_mut(&long_window)
            .unwrap()
            .menu
            .take();
        let pixels = run_frames(&mut long, long_window, SIZE, 4, &mut renderer());
        long.session.panes.get_mut(&long_window).unwrap().menu = menu;
        pixels
    };
    let loading_open = run_frames(&mut long, long_window, SIZE, 4, &mut renderer());
    assert_ne!(
        loading_closed, loading_open,
        "menu remains visible next to the last bubble during newer loading"
    );
    println!(
        "{}",
        save("message-menu-loading-newer", SIZE, loading_open).display()
    );
}

#[test]
#[ignore = "writes sandbox screenshots; run explicitly"]
fn sandbox_scenes() {
    let mut app = world();
    cards(&mut app);
    println!(
        "{}",
        scene_sized("cards", &mut app, Size::new(1000.0, 1150.0)).display()
    );

    let mut app = world();
    days_and_unread(&mut app);
    println!("{}", scene("days-unread", &mut app).display());

    let mut app = world();
    group(&mut app);
    println!("{}", scene("group", &mut app).display());

    // Two people typing.
    let action = |app: &mut App, user: i64| {
        td(
            app,
            json!({"@type": "updateChatAction", "chat_id": 1,
                   "sender_id": {"@type": "messageSenderUser", "user_id": user},
                   "action": {"@type": "chatActionTyping"}}),
        );
    };
    action(&mut app, 7);
    action(&mut app, 8);
    println!("{}", scene("typing", &mut app).display());

    // Context menu with quick reactions.
    let _ = app.update(Msg::Pane(window_of(&app), PaneMsg::OpenMenu(4)));
    let _ = app.update(Msg::Pane(
        window_of(&app),
        PaneMsg::ReactionsReady(
            4,
            Ok(["👍", "❤", "🔥", "😂", "😮", "😢", "🙏"]
                .map(String::from)
                .to_vec()),
        ),
    ));
    let _ = app.update(Msg::Pane(
        window_of(&app),
        PaneMsg::MenuReady(
            4,
            Ok(crate::td::MessageRights {
                delete_for_self: true,
                delete_for_all: false,
                edit: false,
            }),
        ),
    ));
    println!("{}", scene("menu", &mut app).display());
    let _ = app.update(Msg::Pane(window_of(&app), PaneMsg::CloseMenu));

    // Selection of two messages, deleting.
    for id in [2, 3] {
        let _ = app.update(Msg::Pane(window_of(&app), PaneMsg::Select(id)));
    }
    let _ = app.update(Msg::Pane(
        window_of(&app),
        PaneMsg::SelectionRights(Ok(crate::td::MessageRights {
            delete_for_self: true,
            delete_for_all: false,
            edit: false,
        })),
    ));
    println!("{}", scene("selection", &mut app).display());
    let _ = app.update(Msg::Pane(window_of(&app), PaneMsg::ClearSelection));

    // Forward picker.
    let _ = app.update(Msg::Pane(window_of(&app), PaneMsg::Forward(vec![2])));
    println!("{}", scene("forward", &mut app).display());
    let _ = app.update(Msg::Pane(window_of(&app), PaneMsg::CancelForward));

    let _ = app.update(Msg::ShowFolder(window_of(&app), Some(2)));
    println!("{}", scene("folder", &mut app).display());
    let _ = app.update(Msg::ShowFolder(window_of(&app), None));

    // Right-click menu of a chat in the list, then the archive.
    let _ = app.update(Msg::ChatMenu(window_of(&app), Some(5)));
    println!("{}", scene("chat-menu", &mut app).display());
    let _ = app.update(Msg::ChatMenu(window_of(&app), None));
    let _ = app.update(Msg::ShowArchive(window_of(&app), true));
    println!("{}", scene("archive", &mut app).display());
    let _ = app.update(Msg::ShowArchive(window_of(&app), false));

    // Profile panel of the group.
    let _ = app.update(Msg::ToggleProfile(window_of(&app)));
    let _ = app.update(Msg::ProfileLoaded(
        window_of(&app),
        1,
        Ok(crate::td::Profile {
            usernames: vec!["rust_chat".into(), "rustru".into()],
            subtitle: "3 участника".into(),
            about: crate::app::rich::pieces(&tdlib_rs::types::FormattedText {
                text: "Обсуждаем Rust и iced. Реклама: @ads_rust\nПравила: t.me/rust_chat/1".into(),
                entities: serde_json::from_value(json!([
                    {"@type": "textEntity", "offset": 32, "length": 9, "type": {"@type": "textEntityTypeMention"}},
                    {"@type": "textEntity", "offset": 51, "length": 16, "type": {"@type": "textEntityTypeUrl"}}
                ]))
                .unwrap(),
            }),
            linked_chat_id: 2,
            is_channel: false,
            is_member: true,
            members: vec![
                (7, "Игорь Петров".into()),
                (8, "Маша".into()),
                (ME, "Demo User".into()),
            ],
        }),
    ));
    let _ = app.update(Msg::Profile(window_of(&app), ProfileAction::ToggleQr));
    println!(
        "{}",
        scene_sized("profile", &mut app, Size::new(1000.0, 1000.0)).display()
    );
    let _ = app.update(Msg::ToggleProfile(window_of(&app)));

    // Photo viewer over the chat.
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/sandbox/files");
    let picture = dir.join("mountains.png");
    image::RgbImage::from_fn(1200, 800, |x, y| {
        let ridge = 500.0 - 180.0 * ((x as f32) / 170.0).sin().abs() - 0.1 * x as f32;
        if (y as f32) < ridge {
            image::Rgb([90, (140 + y / 8) as u8, 220])
        } else {
            image::Rgb([60, (90 + y.saturating_sub(400) / 10).min(255) as u8, 70])
        }
    })
    .save(&picture)
    .unwrap();
    let file = local_file(9200, &picture);
    let _ = app.update(Msg::Td(
        0,
        Box::new(tdlib_rs::enums::Update::File(tdlib_rs::types::UpdateFile {
            file,
        })),
    ));
    let window = window_of(&app);
    let _ = app.update(Msg::ViewPhoto(window, 9200));
    let decoded = viewer::decode_full_for_tests(&picture.to_string_lossy());
    let _ = app.update(Msg::ViewerDecoded(9200, decoded));
    println!("{}", scene("viewer", &mut app).display());
    let _ = app.update(Msg::CloseViewer);

    // Emoji panel, then a sticker set with pictures.
    let _ = app.update(Msg::Pane(window_of(&app), PaneMsg::TogglePicker));
    println!("{}", scene("emoji", &mut app).display());
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/sandbox/files");
    let stickers: Vec<crate::td::StickerRef> = (0..8)
        .map(|i| {
            let path = dir.join(format!("sticker-{i}.png"));
            image::RgbaImage::from_fn(128, 128, |x, y| {
                let d = ((x as f32 - 64.0).powi(2) + (y as f32 - 64.0).powi(2)).sqrt();
                if d < 56.0 {
                    image::Rgba([(40 + i * 25) as u8, (200 - i * 20) as u8, 120, 255])
                } else {
                    image::Rgba([0, 0, 0, 0])
                }
            })
            .save(&path)
            .unwrap();
            let file = local_file(9300 + i, &path);
            crate::td::StickerRef {
                file: file.clone(),
                picture: Some(file),
                emoji: "😺".into(),
                width: 512,
                height: 512,
            }
        })
        .collect();
    let _ = app.update(Msg::StickerSets(Ok(vec![
        (77, "Кружочки".into()),
        (78, "Котики".into()),
    ])));
    let _ = app.update(Msg::StickerSet(77, Ok(stickers.clone())));
    let _ = app.update(Msg::Pane(
        window_of(&app),
        PaneMsg::PickerTab(picker::Tab::Set(77)),
    ));
    for s in &stickers {
        let id = s.file.id;
        let _ = app.update(Msg::Pane(window_of(&app), PaneMsg::MediaVisible(id)));
        let decoded = media::decode(&s.file.local.path);
        let _ = app.update(Msg::ImageDecoded(id, decoded));
    }
    println!("{}", scene("stickers", &mut app).display());
    let _ = app.update(Msg::Pane(window_of(&app), PaneMsg::ClosePicker));

    // Editing an own message.
    let window = app.main_window;
    let _ = app.update(Msg::Pane(
        window,
        PaneMsg::EditLoaded(1, 5, Ok("Сейчас скину **пример**".into())),
    ));
    println!("{}", scene("editing", &mut app).display());
}

#[test]
#[ignore = "writes sandbox screenshots; run explicitly"]
fn sandbox_search_paging() {
    let mut app = world();
    group(&mut app);
    let window = app.main_window;
    let _ = app.update(Msg::Pane(window, PaneMsg::SearchToggle));
    let _ = app.update(Msg::Pane(window, PaneMsg::SearchQuery("iced".into())));
    let _ = app.update(Msg::Pane(window, PaneMsg::SearchSubmit));
    println!("{}", scene("search-loading", &mut app).display());
    let (request, cursor) = app.session.panes[&window]
        .search
        .as_ref()
        .unwrap()
        .paging
        .pending
        .unwrap();
    let page = [5, 4, 3]
        .into_iter()
        .map(|id| serde_json::from_value(msg(1, id, 7, 5, "iced: найденное сообщение")).unwrap())
        .collect();
    let _ = app.update(Msg::Pane(
        window,
        PaneMsg::SearchResults(1, "iced".into(), request, cursor, Ok((page, Some(3)))),
    ));
    println!("{}", scene("search-page", &mut app).display());
    let _ = app.update(Msg::Pane(window, PaneMsg::SearchMore));
    let (request, cursor) = app.session.panes[&window]
        .search
        .as_ref()
        .unwrap()
        .paging
        .pending
        .unwrap();
    let _ = app.update(Msg::Pane(
        window,
        PaneMsg::SearchResults(
            1,
            "iced".into(),
            request,
            cursor,
            Err("Нет соединения".into()),
        ),
    ));
    println!("{}", scene("search-error", &mut app).display());
    let _ = app.update(Msg::Pane(window, PaneMsg::SearchMore));
    let (request, cursor) = app.session.panes[&window]
        .search
        .as_ref()
        .unwrap()
        .paging
        .pending
        .unwrap();
    let last = vec![serde_json::from_value(msg(1, 2, 7, 8, "iced: последнее сообщение")).unwrap()];
    let _ = app.update(Msg::Pane(
        window,
        PaneMsg::SearchResults(1, "iced".into(), request, cursor, Ok((last, None))),
    ));
    println!("{}", scene("search-end", &mut app).display());
}

#[test]
#[ignore = "writes sandbox screenshots; run explicitly"]
fn sandbox_archive_search() {
    let mut app = world();
    chat(&mut app, 8, "Старый проект в основном списке", false, 15);
    let window = app.main_window;
    let screenshot = |name: &str, app: &mut App| {
        let _ = run_frames(app, window, SIZE, 4, &mut renderer());
        scene(name, app)
    };
    let _ = app.update(Msg::ShowArchive(window, true));
    let _ = app.update(Msg::ListQuery(window, "проект".into()));
    let _ = app.update(Msg::ListSearchMessages(window));
    let (request, cursor) = app.session.panes[&window]
        .list
        .paging
        .pending
        .clone()
        .unwrap();
    let page =
        vec![serde_json::from_value(msg(6, 10, 7, 5, "Проект: найденное сообщение")).unwrap()];
    let _ = app.update(Msg::ListMessages(
        window,
        "проект".into(),
        request,
        cursor,
        Ok((page, None)),
    ));
    println!(
        "{}",
        screenshot("archive-search-results", &mut app).display()
    );
}

/// Headless iced draws the real controls and bubbles; decoded YUV frames need
/// a live TDLib media stream/GPU and therefore are not part of these PNGs.
#[test]
#[ignore = "writes video-player sandbox screenshots; run explicitly"]
fn sandbox_video_note_player() {
    let mut app = world();
    let window = app.main_window;
    let file_path = std::env::current_exe().expect("sandbox media file path");
    let round_file = local_file(306, &file_path);
    let normal_file = local_file(307, &file_path);
    let mut round = msg(1, 101, 7, 3, "");
    round["content"] = json!({
        "@type": "messageVideoNote", "is_viewed": false, "is_secret": false,
        "video_note": {"@type": "videoNote", "duration": 9, "waveform": "",
            "length": 384, "video": round_file}
    });
    let mut normal = msg(1, 102, 7, 2, "");
    normal["content"] = json!({
        "@type": "messageVideo", "alternative_videos": [], "storyboards": [],
        "start_timestamp": 0, "show_caption_above_media": false,
        "has_spoiler": false, "is_secret": false,
        "caption": {"@type": "formattedText", "text": "", "entities": []},
        "video": {"@type": "video", "duration": 75, "width": 480, "height": 270,
            "file_name": "normal.mp4", "mime_type": "video/mp4", "has_stickers": false,
            "supports_streaming": true, "video": normal_file}
    });
    open_with(&mut app, 1, vec![round, normal]);

    let _ = app.update(Msg::Video(video::VideoMsg::Play(window, 1, 101, 306, true)));
    assert!(
        app.session
            .video
            .as_ref()
            .is_some_and(|v| v.round && v.file_id == 306)
    );
    println!("{}", scene("video-note-player-round", &mut app).display());

    let _ = app.update(Msg::Video(video::VideoMsg::Play(
        window, 1, 102, 307, false,
    )));
    assert!(
        app.session
            .video
            .as_ref()
            .is_some_and(|v| !v.round && v.file_id == 307)
    );
    println!("{}", scene("video-note-player-normal", &mut app).display());

    let _ = app.update(Msg::Video(video::VideoMsg::ToggleExpanded));
    assert!(app.view_video_overlay(window).is_some());
    println!(
        "{}",
        scene("video-note-player-expanded", &mut app).display()
    );
}
