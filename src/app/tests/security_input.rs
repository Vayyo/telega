//! Explicit, bounded local diagnostics using real TDLib update parsing and iced rendering.
//! No tasks from `App::update` are polled; there are no network or opener calls.

use super::*;
use std::time::{Duration, Instant};

const VIEW: iced::Size = iced::Size::new(640.0, 480.0);

fn entity(offset: i32, length: i32, kind: &str) -> Value {
    json!({"@type": "textEntity", "offset": offset, "length": length,
           "type": {"@type": kind}})
}

#[test]
#[ignore = "explicit local security audit probe"]
fn security_probe_text_entities_and_controls_reach_rendered_history() {
    // Compare one 4095-character line against 4095 scalars split by 2047
    // line separators; code and ordinary line-break variants exercise
    // different span construction without asserting a timing threshold.
    let single_line = "a".repeat(4095);
    let many_lines = "a\n".repeat(2047) + "a";
    let mixed = "😀\u{202e}ab\u{200b}cd\u{202c}\nZ".to_owned();
    let scenarios = [
        ("ordinary_single_line", single_line, Vec::new(), 1, false),
        ("ordinary_lines", many_lines.clone(), Vec::new(), 1, false),
        (
            "code_lines",
            many_lines,
            vec![entity(0, 4095, "textEntityTypeCode")],
            1,
            true,
        ),
        (
            "rtl_zero_width",
            mixed.clone(),
            vec![entity(2, 7, "textEntityTypeSpoiler")],
            10,
            false,
        ),
        (
            "unpaired_utf16_and_large_offsets",
            mixed,
            vec![
                entity(1, 1, "textEntityTypeSpoiler"),
                entity(i32::MAX - 64, 64, "textEntityTypeCode"),
                entity(-1, 1, "textEntityTypeCode"),
            ],
            1,
            false,
        ),
    ];
    let mut renderer = sandbox::renderer();
    for (label, text, entities, renders, expect_code) in scenarios {
        let mut app = app();
        open(&mut app, 1, &[]);
        let window = app.main_window;
        let blank = sandbox::run_frames(&mut app, window, VIEW, 1, &mut renderer);
        let mut msg = message(1, 10, &text);
        msg["content"]["text"]["entities"] = json!(entities);
        let started = Instant::now();
        new_message_value(&mut app, msg);
        let item = pane(&app)
            .messages
            .last()
            .expect("update retained the message");
        assert_eq!(item.id, 10);
        assert_eq!(item.text, text, "parsed message text changed");
        assert_eq!(
            item.rich
                .iter()
                .map(|piece| piece.text.as_str())
                .collect::<String>(),
            text,
            "UTF-16 entity boundaries changed displayed text"
        );
        assert_eq!(item.rich.iter().any(|piece| piece.style.code), expect_code);
        let masked = crate::app::rich::masked(&item.rich);
        let masked_changed = masked != item.text;
        let pieces = item.rich.len();
        let mut max_render = Duration::ZERO;
        let mut pixels_changed = 0;
        for _ in 0..renders {
            let frame_start = Instant::now();
            let pixels = sandbox::run_frames(&mut app, window, VIEW, 1, &mut renderer);
            max_render = max_render.max(frame_start.elapsed());
            assert_eq!(pixels.len(), 640 * 480 * 4, "render size changed");
            pixels_changed += pixels.iter().zip(&blank).filter(|(a, b)| a != b).count();
        }
        assert!(
            pixels_changed > 0,
            "the real widget tree did not draw the message"
        );
        println!(
            "security_probe_input case={label} chars={} separators={} pieces={pieces} masked_changed={masked_changed} renders={renders} pixel_differences={pixels_changed} elapsed_ms={} max_render_ms={}",
            text.chars().count(),
            text.chars().filter(|&c| c == '\n').count(),
            started.elapsed().as_millis(),
            max_render.as_millis(),
        );
    }
}

#[test]
#[ignore = "explicit local security audit probe"]
fn security_probe_ten_thousand_updates_measure_retained_and_visible_history() {
    const CHATS: i64 = 10;
    const EACH: i64 = 1000;
    let mut app = app();
    app.settings.keep_deleted = true; // In-memory SQLite archive follows the real update path.
    open(&mut app, 1, &[]);
    let start = Instant::now();
    for id in 1..=EACH {
        for chat_id in 1..=CHATS {
            // One foreground chat retains 1000 messages; nine are unopened.
            new_message_value(&mut app, message(chat_id, id, "synthetic"));
        }
    }
    let elapsed = start.elapsed();
    let history = pane(&app);
    let retained = history.messages.len();
    let visible = history.visible();
    assert_eq!(
        retained, EACH as usize,
        "only foreground chat belongs in history"
    );
    assert_eq!(history.messages.first().map(|m| m.id), Some(1));
    assert_eq!(history.messages.last().map(|m| m.id), Some(EACH));
    assert!(visible.start < visible.end && visible.end <= retained);
    assert!(app.error.is_none(), "an in-memory archive update failed");
    println!(
        "security_probe_updates total={} chats={CHATS} retained={retained} visible={} elapsed_ms={}",
        CHATS * EACH,
        visible.end - visible.start,
        elapsed.as_millis(),
    );
}
