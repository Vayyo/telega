//! Message text that can be selected with the mouse: `rich_text` (links,
//! spoilers, styles) plus hit-testing of where a drag starts and goes.
//! One widget per line of a message, so a position within its paragraph
//! is exact; `start` places it within the whole message.

use iced::advanced::layout::{self, Layout};
use iced::advanced::renderer;
use iced::advanced::text::{self as core_text, Paragraph as _, Span};
use iced::advanced::widget::{Tree, Widget, tree};
use iced::advanced::{Clipboard, Shell};
use iced::alignment;
use iced::widget::text::LineHeight;
use iced::{Element, Event, Length, Pixels, Point, Rectangle, Size, Vector, mouse};

/// What the widget reports; offsets are bytes into the whole message.
pub(crate) struct Handlers<Link, Message> {
    pub(crate) on_press: Box<dyn Fn(usize) -> Message>,
    pub(crate) on_drag: Box<dyn Fn(usize) -> Message>,
    pub(crate) on_link: Box<dyn Fn(Link) -> Message>,
}

pub(crate) struct Selectable<'a, Link, Message> {
    spans: Vec<Span<'a, Link, iced::Font>>,
    /// Byte offset of this line in the message.
    start: usize,
    /// Byte offset right after this line in the message (before the line
    /// break, if any). Hit-test offsets clamp to it, so a click past the
    /// visible text (including the single-space placeholder drawn for an
    /// empty line) never reports a position past this line's own end.
    end: usize,
    /// A selection drag is going on in this message.
    dragging: bool,
    size: Option<Pixels>,
    handlers: Handlers<Link, Message>,
}

impl<'a, Link, Message> Selectable<'a, Link, Message> {
    pub(crate) fn new(
        spans: Vec<Span<'a, Link, iced::Font>>,
        start: usize,
        end: usize,
        dragging: bool,
        handlers: Handlers<Link, Message>,
    ) -> Self {
        Self {
            spans,
            start,
            end,
            dragging,
            size: None,
            handlers,
        }
    }
}

struct State<Link, P: core_text::Paragraph> {
    spans: Vec<Span<'static, Link, P::Font>>,
    paragraph: P,
    /// Pressed here: the offset and the span (a click on a link opens it
    /// only if the mouse was not dragged).
    pressed: Option<(usize, Option<usize>)>,
    hovered_link: Option<usize>,
}

impl<Link, Message, Theme, Renderer> Widget<Message, Theme, Renderer>
    for Selectable<'_, Link, Message>
where
    Link: Clone + PartialEq + 'static,
    Renderer: core_text::Renderer<Font = iced::Font>,
{
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State<Link, Renderer::Paragraph>>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State::<Link, Renderer::Paragraph> {
            spans: Vec::new(),
            paragraph: Renderer::Paragraph::default(),
            pressed: None,
            hovered_link: None,
        })
    }

    fn size(&self) -> Size<Length> {
        Size::new(Length::Shrink, Length::Shrink)
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let state = tree
            .state
            .downcast_mut::<State<Link, Renderer::Paragraph>>();
        layout::sized(limits, Length::Shrink, Length::Shrink, |limits| {
            let text = core_text::Text {
                content: self.spans.as_slice(),
                bounds: limits.max(),
                size: self.size.unwrap_or_else(|| renderer.default_size()),
                line_height: LineHeight::default(),
                font: renderer.default_font(),
                align_x: core_text::Alignment::Default,
                align_y: alignment::Vertical::Top,
                shaping: core_text::Shaping::Advanced,
                wrapping: core_text::Wrapping::WordOrGlyph,
            };
            if state.spans != self.spans {
                state.paragraph = Renderer::Paragraph::with_spans(text);
                state.spans = self.spans.iter().cloned().map(Span::to_static).collect();
            } else {
                match state.paragraph.compare(core_text::Text {
                    content: (),
                    bounds: text.bounds,
                    size: text.size,
                    line_height: text.line_height,
                    font: text.font,
                    align_x: text.align_x,
                    align_y: text.align_y,
                    shaping: text.shaping,
                    wrapping: text.wrapping,
                }) {
                    core_text::Difference::None => {}
                    core_text::Difference::Bounds => state.paragraph.resize(text.bounds),
                    core_text::Difference::Shape => {
                        state.paragraph = Renderer::Paragraph::with_spans(text);
                    }
                }
            }
            state.paragraph.min_bounds()
        })
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        _theme: &Theme,
        defaults: &renderer::Style,
        layout: Layout<'_>,
        _cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        if !layout.bounds().intersects(viewport) {
            return;
        }
        let state = tree
            .state
            .downcast_ref::<State<Link, Renderer::Paragraph>>();
        let translation = layout.position() - Point::ORIGIN;
        let size = self.size.unwrap_or_else(|| renderer.default_size());
        let line_height = LineHeight::default().to_absolute(size);
        for (index, span) in self.spans.iter().enumerate() {
            let hovered = state.hovered_link == Some(index);
            if span.highlight.is_none() && !span.underline && !span.strikethrough && !hovered {
                continue;
            }
            let regions = state.paragraph.span_bounds(index);
            if let Some(highlight) = span.highlight {
                for bounds in &regions {
                    renderer.fill_quad(
                        renderer::Quad {
                            bounds: *bounds + translation,
                            border: highlight.border,
                            ..Default::default()
                        },
                        highlight.background,
                    );
                }
            }
            let color = span.color.unwrap_or(defaults.text_color);
            let baseline = translation + Vector::new(0.0, size.0 + (line_height.0 - size.0) / 2.0);
            for bounds in &regions {
                if span.underline || hovered {
                    renderer.fill_quad(
                        renderer::Quad {
                            bounds: Rectangle::new(
                                bounds.position() + baseline - Vector::new(0.0, size.0 * 0.08),
                                Size::new(bounds.width, 1.0),
                            ),
                            ..Default::default()
                        },
                        color,
                    );
                }
                if span.strikethrough {
                    renderer.fill_quad(
                        renderer::Quad {
                            bounds: Rectangle::new(
                                bounds.position() + baseline - Vector::new(0.0, size.0 / 2.0),
                                Size::new(bounds.width, 1.0),
                            ),
                            ..Default::default()
                        },
                        color,
                    );
                }
            }
        }
        renderer.fill_paragraph(
            &state.paragraph,
            layout.position(),
            defaults.text_color,
            *viewport,
        );
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _renderer: &Renderer,
        _clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        _viewport: &Rectangle,
    ) {
        let state = tree
            .state
            .downcast_mut::<State<Link, Renderer::Paragraph>>();
        let position = cursor.position_in(layout.bounds());
        let hovered = position
            .and_then(|p| state.paragraph.hit_span(p))
            .filter(|&i| self.spans.get(i).is_some_and(|s| s.link.is_some()));
        if hovered != state.hovered_link {
            state.hovered_link = hovered;
            shell.request_redraw();
        }
        let offset = |p: Point| {
            let core_text::Hit::CharOffset(i) = state.paragraph.hit_test(p)?;
            // Clamp: on an empty line the placeholder " " span (pushed by
            // the caller so the line keeps its height) can hit-test past
            // the line's own (empty) byte range.
            Some((self.start + i).min(self.end))
        };
        match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                if let Some(at) = position.and_then(offset) {
                    state.pressed = Some((at, hovered));
                    shell.publish((self.handlers.on_press)(at));
                    shell.capture_event();
                }
            }
            Event::Mouse(mouse::Event::CursorMoved { .. }) if self.dragging => {
                // Beside the line (past its end, left of it) still counts
                // as the line: the selection runs to its edge.
                let bounds = layout.bounds();
                let beside = cursor
                    .position()
                    .filter(|p| p.y >= bounds.y && p.y < bounds.y + bounds.height)
                    .map(|p| {
                        Point::new(
                            (p.x - bounds.x).clamp(0.0, (bounds.width - 0.5).max(0.0)),
                            p.y - bounds.y,
                        )
                    });
                if let Some(at) = beside.and_then(offset) {
                    shell.publish((self.handlers.on_drag)(at));
                }
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                // A press and release on the same spot of a link is a click.
                if let Some((at, Some(span))) = state.pressed.take()
                    && position.and_then(offset) == Some(at)
                    && hovered == Some(span)
                    && let Some(link) = self.spans.get(span).and_then(|s| s.link.clone())
                {
                    shell.publish((self.handlers.on_link)(link));
                }
            }
            _ => {}
        }
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _viewport: &Rectangle,
        _renderer: &Renderer,
    ) -> mouse::Interaction {
        let state = tree
            .state
            .downcast_ref::<State<Link, Renderer::Paragraph>>();
        if state.hovered_link.is_some() {
            mouse::Interaction::Pointer
        } else if cursor.is_over(layout.bounds()) {
            mouse::Interaction::Text
        } else {
            mouse::Interaction::None
        }
    }
}

impl<'a, Link, Message, Theme, Renderer> From<Selectable<'a, Link, Message>>
    for Element<'a, Message, Theme, Renderer>
where
    Link: Clone + PartialEq + 'static,
    Message: 'a,
    Theme: 'a,
    Renderer: core_text::Renderer<Font = iced::Font> + 'a,
{
    fn from(widget: Selectable<'a, Link, Message>) -> Self {
        Element::new(widget)
    }
}

/// Whether `c` is a Unicode Bidi_Class=B paragraph separator: `\n`, `\r`,
/// NEL (U+0085), the information separators (U+001C–1E) or U+2029. This is
/// exactly the set `cosmic-text` (`set_rich_text` → `BidiParagraphs`) splits
/// a run of spans into separate internal lines on, so `lines()` has to
/// split on the same characters for a `Selectable` widget's text (one per
/// line here) to always be a single cosmic-text line: otherwise the
/// widget's own `start`/`end` no longer line up with what `hit_test`
/// reports (see `Selectable::update`).
pub(crate) fn is_line_separator(c: char) -> bool {
    matches!(
        c,
        '\n' | '\r' | '\u{0085}' | '\u{001C}'..='\u{001E}' | '\u{2029}'
    )
}

/// Byte ranges of the lines of `text` (without the line breaks).
pub(crate) fn lines(text: &str) -> Vec<std::ops::Range<usize>> {
    let mut start = 0;
    let mut out = Vec::new();
    for (i, c) in text.char_indices() {
        if is_line_separator(c) {
            out.push(start..i);
            start = i + c.len_utf8();
        }
    }
    out.push(start..text.len());
    out
}

/// A selection as a byte range of `text`, on character boundaries.
pub(crate) fn selected(text: &str, anchor: usize, focus: usize) -> &str {
    let (mut a, mut b) = (
        anchor.min(focus).min(text.len()),
        anchor.max(focus).min(text.len()),
    );
    while !text.is_char_boundary(a) {
        a -= 1;
    }
    while !text.is_char_boundary(b) {
        b += 1;
    }
    &text[a..b]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_cover_the_text_without_breaks() {
        let text = "раз\nдва\n\nтри";
        let parts: Vec<&str> = lines(text).into_iter().map(|r| &text[r]).collect();
        assert_eq!(parts, ["раз", "два", "", "три"]);
    }

    #[test]
    fn lines_split_on_bidi_class_b_separators_like_cosmic_text_does() {
        // cosmic-text's own `set_rich_text` (via `BidiParagraphs`) breaks a
        // paragraph on every Bidi_Class=B character, not just '\n'; `lines()`
        // has to agree so each `Selectable` widget's text is exactly one
        // cosmic-text line and its `start`/`end` line up with `hit_test`.
        let text = "раз\rдва\u{0085}три\u{001C}четыре\u{001D}пять\u{001E}шесть\u{2029}семь";
        let parts: Vec<&str> = lines(text).into_iter().map(|r| &text[r]).collect();
        assert_eq!(
            parts,
            ["раз", "два", "три", "четыре", "пять", "шесть", "семь"]
        );
    }

    #[test]
    fn selection_is_ordered_and_whole_characters() {
        let text = "привет мир";
        assert_eq!(selected(text, 12, 0), "привет");
        // Offsets inside a character widen to it rather than panic.
        assert_eq!(selected(text, 1, 3), "пр");
        assert_eq!(selected(text, 5, 500), &text[4..]);
    }

    #[derive(Debug, Clone, PartialEq)]
    enum Ev {
        Press(usize),
        Drag(usize),
        Link(u8),
    }

    /// Real mouse events through a laid-out widget: a press at the left
    /// edge is offset 0 of the line, a drag to the far right its end, and a
    /// click without moving on a link opens it.
    #[test]
    fn mouse_turns_into_offsets_and_link_clicks() {
        use iced::advanced::renderer::Headless;
        use iced::widget::span;
        use iced_runtime::user_interface::{Cache, UserInterface};

        let mut renderer = iced::futures::executor::block_on(<iced::Renderer as Headless>::new(
            iced::Font::DEFAULT,
            Pixels(16.0),
            Some("tiny-skia"),
        ))
        .unwrap();
        let line = "привет мир";
        let widget = |dragging: bool| -> Element<'static, Ev> {
            Selectable::new(
                vec![span("привет "), span("мир").link(7u8)],
                100,
                100 + line.len(),
                dragging,
                Handlers {
                    on_press: Box::new(Ev::Press),
                    on_drag: Box::new(Ev::Drag),
                    on_link: Box::new(Ev::Link),
                },
            )
            .into()
        };
        let events = |dragging: bool, events: &[Event], renderer: &mut iced::Renderer| {
            let mut ui = UserInterface::build(
                widget(dragging),
                Size::new(400.0, 100.0),
                Cache::default(),
                renderer,
            );
            let mut out = Vec::new();
            for event in events {
                let cursor = match event {
                    Event::Mouse(mouse::Event::CursorMoved { position }) => {
                        mouse::Cursor::Available(*position)
                    }
                    _ => mouse::Cursor::Available(Point::new(1.0, 8.0)),
                };
                let _ = ui.update(
                    std::slice::from_ref(event),
                    cursor,
                    renderer,
                    &mut iced::advanced::clipboard::Null,
                    &mut out,
                );
            }
            out
        };
        let press = Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left));
        assert_eq!(
            events(false, std::slice::from_ref(&press), &mut renderer),
            [Ev::Press(100)]
        );

        let far = Event::Mouse(mouse::Event::CursorMoved {
            position: Point::new(390.0, 8.0),
        });
        let drag = events(true, &[far], &mut renderer);
        assert_eq!(drag, [Ev::Drag(100 + line.len())]);

        // Press and release at the same spot on "мир": the link opens.
        let on_link = Point::new(80.0, 8.0);
        let mut ui_events = Vec::new();
        {
            let mut ui = UserInterface::build(
                widget(false),
                Size::new(400.0, 100.0),
                Cache::default(),
                &mut renderer,
            );
            for event in [
                Event::Mouse(mouse::Event::CursorMoved { position: on_link }),
                Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
                Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
            ] {
                let _ = ui.update(
                    &[event],
                    mouse::Cursor::Available(on_link),
                    &mut renderer,
                    &mut iced::advanced::clipboard::Null,
                    &mut ui_events,
                );
            }
        }
        assert!(ui_events.contains(&Ev::Link(7)), "{ui_events:?}");
    }

    /// An empty line is rendered as a single-space placeholder span so it
    /// keeps its height (`view_rich` in view.rs); hitting its right half
    /// must not report an offset past this (empty) line's own end.
    #[test]
    fn hit_test_clamps_to_line_end_on_empty_placeholder_line() {
        use iced::advanced::renderer::Headless;
        use iced::widget::span;

        let renderer = iced::futures::executor::block_on(<iced::Renderer as Headless>::new(
            iced::Font::DEFAULT,
            Pixels(16.0),
            Some("tiny-skia"),
        ))
        .unwrap();
        // An empty line: start == end, only the placeholder " " is drawn.
        let mut widget: Element<'static, Ev> = Selectable::new(
            vec![span(" ")],
            100,
            100,
            false,
            Handlers {
                on_press: Box::new(Ev::Press),
                on_drag: Box::new(Ev::Drag),
                on_link: Box::new(Ev::Link),
            },
        )
        .into();
        // Drive `layout`/`update` directly (rather than through
        // `UserInterface`) so the click lands exactly on the placeholder's
        // own (small) width, however wide the font renders it.
        let mut tree = Tree::new(widget.as_widget());
        let limits = layout::Limits::new(Size::ZERO, Size::new(400.0, 100.0));
        let node = widget.as_widget_mut().layout(&mut tree, &renderer, &limits);
        let width = node.size().width;
        assert!(width > 0.0, "placeholder space should have non-zero width");
        let mut messages = Vec::new();
        let mut shell = Shell::new(&mut messages);
        // Right at the placeholder's trailing edge: without clamping this
        // hits `CharOffset(1)`, one byte past the (empty) line.
        let cursor = mouse::Cursor::Available(Point::new(width - 0.01, 8.0));
        widget.as_widget_mut().update(
            &mut tree,
            &Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
            Layout::new(&node),
            cursor,
            &renderer,
            &mut iced::advanced::clipboard::Null,
            &mut shell,
            &Rectangle::new(Point::ORIGIN, Size::new(400.0, 100.0)),
        );
        assert_eq!(messages, [Ev::Press(100)]);
    }

    /// Word wrap splits a paragraph into several visual lines inside the
    /// same cosmic-text buffer line; a later wrapped line's offsets must
    /// keep counting from where the previous one left off, not reset to 0.
    #[test]
    fn hit_test_offsets_keep_counting_across_wrapped_visual_lines() {
        use iced::advanced::renderer::Headless;
        use iced::widget::span;
        use iced::widget::text::LineHeight;
        use iced_runtime::user_interface::{Cache, UserInterface};

        let mut renderer = iced::futures::executor::block_on(<iced::Renderer as Headless>::new(
            iced::Font::DEFAULT,
            Pixels(16.0),
            Some("tiny-skia"),
        ))
        .unwrap();
        // No spaces to break on: a narrow width forces a glyph-by-glyph
        // wrap of this single "word" onto several visual lines.
        let text = "aaaaaaaaaa";
        let widget: Element<'static, Ev> = Selectable::new(
            vec![span(text)],
            100,
            100 + text.len(),
            false,
            Handlers {
                on_press: Box::new(Ev::Press),
                on_drag: Box::new(Ev::Drag),
                on_link: Box::new(Ev::Link),
            },
        )
        .into();
        let mut ui = UserInterface::build(
            widget,
            Size::new(30.0, 400.0),
            Cache::default(),
            &mut renderer,
        );
        let line_height = LineHeight::default().to_absolute(Pixels(16.0)).0;
        let mut click = |point: Point| {
            let mut out = Vec::new();
            let _ = ui.update(
                &[Event::Mouse(mouse::Event::ButtonPressed(
                    mouse::Button::Left,
                ))],
                mouse::Cursor::Available(point),
                &mut renderer,
                &mut iced::advanced::clipboard::Null,
                &mut out,
            );
            match out.as_slice() {
                [Ev::Press(at)] => *at,
                other => panic!("expected exactly one Press, got {other:?}"),
            }
        };
        // Left edge of the first visual line: the very first character.
        let first_line = click(Point::new(0.0, line_height / 2.0));
        assert_eq!(first_line, 100);
        // Left edge of the second visual line: further into the text, not
        // reset back to its own start.
        let second_line = click(Point::new(0.0, line_height + line_height / 2.0));
        assert!(
            second_line > first_line && second_line <= 100 + text.len(),
            "second wrapped line's offset {second_line} did not advance past the first line's {first_line}"
        );
    }
}
