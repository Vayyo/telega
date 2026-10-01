//! Renders real views headlessly (tiny-skia) through consecutive frames, the
//! way iced diffs widget trees between them. Catches widget-tree mismatches
//! that only show up at runtime.

use iced::advanced::renderer::Headless;
use iced::{Size, Theme, mouse};
use iced_runtime::core::renderer::Style;
use iced_runtime::user_interface::{Cache, UserInterface};

use super::media::{Media, Motion};
use super::sandbox::renderer;
use super::tests::app;
use super::*;

fn frame(app: &App, cache: Cache, renderer: &mut iced::Renderer) -> Cache {
    let mut ui = UserInterface::build(
        app.view(app.main_window),
        Size::new(1000.0, 700.0),
        cache,
        renderer,
    );
    // A redraw event, as the window sends every frame: widgets that react to
    // it (sensors, scrollables) read their state here.
    let mut messages = Vec::new();
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
        &Theme::Dark,
        &Style::default(),
        mouse::Cursor::Unavailable,
    );
    ui.into_cache()
}

/// Mixed history: text, photos (with and without preview) and files.
fn fill(app: &mut App, n: i64) {
    let window = app.main_window;
    let pane = app.session.panes.get_mut(&window).unwrap();
    pane.switch_to(1);
    pane.view_size = Some((560.0, 680.0));
    for id in 1..=n {
        let media = match id % 9 {
            1 => Some(Media::Photo {
                mini: None,
                file_id: id as i32,
                width: 800,
                height: 600,
                spoiler: id % 8 == 1,
            }),
            2 => Some(Media::Document {
                file_id: id as i32,
                name: "report.pdf".into(),
                size: 12_345,
            }),
            3 => Some(Media::Voice {
                file_id: id as i32,
                duration: 12,
                waveform: (0..100).map(|i| (i % 32) as u8).collect(),
            }),
            4 => Some(Media::Sticker {
                file_id: id as i32,
                kind: [Motion::Still, Motion::Lottie, Motion::Video][(id % 3) as usize],
                width: 512,
                height: 512,
                emoji: "😀".into(),
            }),
            5 => Some(Media::Video {
                file_id: id as i32,
                mini: None,
                thumb: Some(id as i32 + 10_000),
                width: 1280,
                height: 720,
                duration: 75,
                spoiler: id % 2 == 0,
                round: false,
            }),
            6 => Some(Media::Animation {
                file_id: id as i32,
                mini: None,
                thumb: None,
                width: 480,
                height: 270,
                duration: 3,
                spoiler: false,
            }),
            _ => None,
        };
        pane.messages.push(MsgItem {
            chat_id: 1,
            id,
            sender: MessageSender::User(tdlib_rs::types::MessageSenderUser { user_id: 7 }),
            text: format!("сообщение {id}"),
            outgoing: id % 3 == 0,
            deleted: id % 7 == 0,
            media,
            pending: false,
            failed: false,
            rich: crate::app::rich::plain(&format!("сообщение {id}")),
            reply_to: (id % 5 == 0).then(|| id - 1),
            date: 0,
            edited: false,
            reactions: Vec::new(),
            forwarded: None,
            extra: None,
            sender_tag: String::new(),
        });
    }
}

fn scroll_to(app: &mut App, offset: f32) {
    let window = app.main_window;
    app.session.panes.get_mut(&window).unwrap().scroll =
        Some(iced::widget::scrollable::AbsoluteOffset { x: 0.0, y: offset });
}

#[test]
fn virtualized_history_survives_jumps_and_changing_rows() {
    let mut app = app();
    fill(&mut app, 600);
    let mut renderer = renderer();
    let mut cache = frame(&app, Cache::default(), &mut renderer);
    // Small steps, big jumps (scrollbar drag), back to the bottom, and the
    // "loading" row appearing at the top.
    for (i, offset) in [120.0, 240.0, 5_000.0, 20_000.0, 60.0, 0.0, 40_000.0, 1e9]
        .into_iter()
        .enumerate()
    {
        scroll_to(&mut app, offset);
        let window = app.main_window;
        app.session.panes.get_mut(&window).unwrap().loading_older = i % 2 == 0;
        cache = frame(&app, cache, &mut renderer);
    }
    // Menu opened on a message, then chat switched to another tab.
    let _ = app.update(Msg::Pane(app.main_window, PaneMsg::OpenMenu(600)));
    cache = frame(&app, cache, &mut renderer);
    let window = app.main_window;
    app.session
        .panes
        .get_mut(&window)
        .unwrap()
        .messages
        .truncate(3);
    let _ = frame(&app, cache, &mut renderer);
}

#[test]
fn history_appearing_in_an_empty_chat() {
    // Chat opened: first frames have no messages, then history arrives.
    let mut app = app();
    let window = app.main_window;
    app.session.panes.get_mut(&window).unwrap().switch_to(1);
    let mut renderer = renderer();
    let cache = frame(&app, Cache::default(), &mut renderer);
    fill(&mut app, 40);
    let cache = frame(&app, cache, &mut renderer);
    // And a chat emptied again (e.g. all messages removed).
    app.session.panes.get_mut(&window).unwrap().messages.clear();
    let _ = frame(&app, cache, &mut renderer);
}

#[test]
fn replies_spoilers_search_and_filters_render_across_frames() {
    use crate::app::rich::{Piece, Style};
    let mut app = app();
    fill(&mut app, 60);
    let window = app.main_window;
    {
        let pane = app.session.panes.get_mut(&window).unwrap();
        for m in pane.messages.iter_mut().filter(|m| m.id % 3 == 0) {
            m.rich = vec![
                Piece {
                    text: "жирно ".into(),
                    style: Style {
                        bold: true,
                        ..Style::default()
                    },
                },
                Piece {
                    text: "тайна".into(),
                    style: Style {
                        spoiler: true,
                        ..Style::default()
                    },
                },
                Piece {
                    text: " ссылка".into(),
                    style: Style {
                        link: Some("https://example.org".into()),
                        ..Style::default()
                    },
                },
            ];
        }
        pane.reply_to = Some(59);
        pane.highlight = Some(58);
        pane.newest_loaded = false;
        pane.loading_newer = true;
        pane.list.query = "сообщ".into();
    }
    let mut renderer = renderer();
    let mut cache = frame(&app, Cache::default(), &mut renderer);
    // Spoilers opened, search results shown, messages found across chats.
    let _ = app.update(Msg::Pane(window, PaneMsg::Reveal(60)));
    let _ = app.update(Msg::Pane(window, PaneMsg::Reveal(57)));
    cache = frame(&app, cache, &mut renderer);
    let found: Vec<MsgItem> = app.session.panes[&window].messages[..5].to_vec();
    {
        let pane = app.session.panes.get_mut(&window).unwrap();
        pane.search = Some(super::pane::ChatSearch {
            query: "сообщ".into(),
            results: Some(found.clone()),
            paging: Default::default(),
            ..Default::default()
        });
        pane.list.messages = Some(found);
    }
    cache = frame(&app, cache, &mut renderer);
    // Back from results to the history.
    let _ = app.update(Msg::Pane(window, PaneMsg::JumpTo(10)));
    let _ = frame(&app, cache, &mut renderer);
}

#[test]
fn own_messages_and_replies_are_on_the_right_in_the_accent_color() {
    let mut app = app();
    let window = app.main_window;
    let pane = app.session.panes.get_mut(&window).unwrap();
    pane.switch_to(1);
    pane.view_size = Some((560.0, 680.0));
    for (id, outgoing) in [(1, false), (2, true)] {
        pane.messages.push(MsgItem {
            chat_id: 1,
            id,
            sender: MessageSender::User(tdlib_rs::types::MessageSenderUser { user_id: 7 }),
            text: "привет".into(),
            outgoing,
            deleted: false,
            media: None,
            pending: false,
            failed: false,
            rich: crate::app::rich::plain("привет"),
            // A quote must not stretch the own bubble off the right edge.
            reply_to: outgoing.then_some(1),
            date: 0,
            edited: false,
            reactions: Vec::new(),
            forwarded: None,
            extra: None,
            sender_tag: String::new(),
        });
    }
    let mut renderer = renderer();
    let cache = frame(&app, Cache::default(), &mut renderer);
    let _ = frame(&app, cache, &mut renderer);
    let (w, h) = (1000, 700);
    let pixels = renderer.screenshot(Size::new(w, h), 1.0, iced::Color::BLACK);
    let accent = Theme::Dark
        .extended_palette()
        .primary
        .weak
        .color
        .into_rgba8();
    let neutral = Theme::Dark
        .extended_palette()
        .background
        .weak
        .color
        .into_rgba8();
    // Columns of the history area (right of the chat list) holding a color.
    let columns = |rgba: [u8; 4]| -> Vec<u32> {
        (310..w)
            .filter(|&x| {
                (60..h - 60).any(|y| {
                    let i = ((y * w + x) * 4) as usize;
                    pixels[i..i + 3] == rgba[..3]
                })
            })
            .collect()
    };
    let (own, other) = (columns(accent), columns(neutral));
    assert!(!own.is_empty() && !other.is_empty(), "both bubbles drawn");
    assert!(
        own[0] > 700,
        "own bubble hugs the right edge: starts at {}",
        own[0]
    );
    assert!(*other.last().unwrap() < 700, "other bubble stays left");
}

#[test]
fn typing_overlay_comes_and_goes_across_frames() {
    let mut app = app();
    fill(&mut app, 40);
    let mut renderer = renderer();
    let mut cache = frame(&app, Cache::default(), &mut renderer);
    let user = MessageSender::User(tdlib_rs::types::MessageSenderUser { user_id: 7 });
    for typing in [true, true, false, true, false] {
        app.session
            .typing
            .set(1, user.clone(), typing, std::time::Instant::now());
        app.session.typing.tick(std::time::Instant::now());
        cache = frame(&app, cache, &mut renderer);
    }
}

#[test]
fn selecting_text_across_lines_renders_across_frames() {
    let mut app = app();
    fill(&mut app, 12);
    let window = app.main_window;
    let pane = app.session.panes.get_mut(&window).unwrap();
    let id = pane.messages[2].id;
    pane.messages[2].rich = crate::app::rich::plain("раз\nдва 🔥 три\n\nчетыре");
    let mut renderer = renderer();
    let mut cache = frame(&app, Cache::default(), &mut renderer);
    for (anchor, focus, dragging) in [(0, 0, true), (2, 9, true), (2, 20, false), (20, 2, false)] {
        app.session.panes.get_mut(&window).unwrap().text_selection = Some(pane::TextSelection {
            message: id,
            anchor,
            focus,
            dragging,
        });
        cache = frame(&app, cache, &mut renderer);
    }
}
