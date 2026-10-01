//! Message content besides text and media files: link previews, polls,
//! contacts, locations and venues.

use iced::widget::{button, column, container, image, row, sensor, text};
use iced::{ContentFit, Element, Fill};
use tdlib_rs::enums::{LinkPreviewType, MessageContent, PollType};
use tdlib_rs::types::{File, Photo};

use super::{App, Link, Msg, MsgItem, PaneMsg, WinId};

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Extra {
    Link {
        url: String,
        /// Host or short form TDLib suggests showing (`display_url`):
        /// the sender controls the rest of the card, this is the one
        /// thing that says where the click actually goes.
        display: String,
        site: String,
        title: String,
        description: String,
        /// Small picture of the page (file id), if any.
        thumb: Option<i32>,
        /// The target is not shown anywhere in the message text: opening
        /// it asks for confirmation first, like a hidden `TextUrl`.
        hidden: bool,
    },
    Poll {
        question: String,
        options: Vec<PollOption>,
        total: i32,
        quiz: bool,
        /// 0-based indices of the correct options (quiz only); empty
        /// before the user answers or outside a quiz.
        correct: Vec<i32>,
        /// Several options can be picked before «Голосовать» sends them.
        allow_multiple: bool,
        closed: bool,
        anonymous: bool,
    },
    Contact {
        name: String,
        phone: String,
        /// Telegram account of the contact; 0 if none.
        user_id: i64,
    },
    Location {
        latitude: f64,
        longitude: f64,
        /// Venues: the place's name and address.
        title: String,
        address: String,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PollOption {
    pub(crate) text: String,
    pub(crate) percent: i32,
    pub(crate) votes: i32,
    pub(crate) chosen: bool,
}

impl Extra {
    /// Whether the message's own text is shown too: for link previews it
    /// is the message, for the rest the card says it all.
    pub(crate) fn keeps_text(&self) -> bool {
        matches!(self, Self::Link { .. })
    }

    /// The user voted in the poll: results are shown instead of choices.
    pub(crate) fn voted(&self) -> bool {
        matches!(self, Self::Poll { options, closed, .. } if *closed || options.iter().any(|o| o.chosen))
    }
}

/// Smallest photo size that still looks fine as a 64 px thumbnail.
fn thumb_of(photo: &Photo) -> Option<&File> {
    photo
        .sizes
        .iter()
        .filter(|s| s.width.min(s.height) >= 64)
        .min_by_key(|s| s.width * s.height)
        .or_else(|| photo.sizes.last())
        .map(|s| &s.photo)
}

/// Whether opening the preview's target should ask first: it should,
/// unless that exact URL already appears in the visible message text, so
/// the sender cannot make the card point somewhere the user never saw
/// (`site_name`/`title`/`description` come from the page and are free
/// for the sender to fake).
fn preview_hidden(url: &str, message_text: &str) -> bool {
    !message_text.contains(url)
}

/// The card of a message and the file it references (link picture).
pub(crate) fn extra_of(content: &MessageContent) -> Option<(Extra, Option<File>)> {
    Some(match content {
        MessageContent::MessageText(m) => {
            let preview = m.link_preview.as_ref()?;
            let photo = match &preview.r#type {
                LinkPreviewType::Article(a) => a.photo.as_ref(),
                LinkPreviewType::Photo(p) => Some(&p.photo),
                LinkPreviewType::Video(v) => v.cover.as_ref(),
                _ => None,
            };
            let file = photo.and_then(thumb_of).cloned();
            (
                Extra::Link {
                    url: preview.url.clone(),
                    display: preview.display_url.clone(),
                    site: preview.site_name.clone(),
                    title: preview.title.clone(),
                    description: preview.description.text.clone(),
                    thumb: file.as_ref().map(|f| f.id),
                    hidden: preview_hidden(&preview.url, &m.text.text),
                },
                file,
            )
        }
        MessageContent::MessagePoll(m) => {
            let poll = &m.poll;
            let correct = match &poll.r#type {
                PollType::Quiz(q) => q.correct_option_ids.clone(),
                PollType::Regular => Vec::new(),
            };
            (
                Extra::Poll {
                    question: poll.question.text.clone(),
                    options: poll
                        .options
                        .iter()
                        .map(|o| PollOption {
                            text: o.text.text.clone(),
                            percent: o.vote_percentage,
                            votes: o.voter_count,
                            chosen: o.is_chosen,
                        })
                        .collect(),
                    total: poll.total_voter_count,
                    quiz: matches!(poll.r#type, PollType::Quiz(_)),
                    correct,
                    allow_multiple: poll.allows_multiple_answers,
                    closed: poll.is_closed,
                    anonymous: poll.is_anonymous,
                },
                None,
            )
        }
        MessageContent::MessageContact(m) => {
            let c = &m.contact;
            (
                Extra::Contact {
                    name: format!("{} {}", c.first_name, c.last_name)
                        .trim()
                        .to_owned(),
                    phone: c.phone_number.clone(),
                    user_id: c.user_id,
                },
                None,
            )
        }
        MessageContent::MessageLocation(m) => (
            Extra::Location {
                latitude: m.location.latitude,
                longitude: m.location.longitude,
                title: String::new(),
                address: String::new(),
            },
            None,
        ),
        MessageContent::MessageVenue(m) => (
            Extra::Location {
                latitude: m.venue.location.latitude,
                longitude: m.venue.location.longitude,
                title: m.venue.title.clone(),
                address: m.venue.address.clone(),
            },
            None,
        ),
        _ => return None,
    })
}

/// OpenStreetMap page centered on a point.
pub(crate) fn map_url(latitude: f64, longitude: f64) -> String {
    format!(
        "https://www.openstreetmap.org/?mlat={latitude:.6}&mlon={longitude:.6}#map=16/{latitude:.6}/{longitude:.6}"
    )
}

/// Phone number as Telegram shows it: "+7 900 123 45 67" style grouping is
/// country-specific, so only the plus is added.
fn phone_label(phone: &str) -> String {
    if phone.is_empty() || phone.starts_with('+') {
        phone.to_owned()
    } else {
        format!("+{phone}")
    }
}

impl App {
    pub(crate) fn view_extra<'a>(
        &'a self,
        window: WinId,
        m: &'a MsgItem,
        extra: &'a Extra,
    ) -> Element<'a, Msg> {
        let on_pane = move |msg| Msg::Pane(window, msg);
        let open = |url: String, hidden: bool| on_pane(PaneMsg::Link(Link::Url { url, hidden }));
        match extra {
            Extra::Link {
                url,
                display,
                site,
                title,
                description,
                thumb,
                hidden,
            } => {
                let mut info = column![].spacing(2).width(Fill);
                if !site.is_empty() {
                    info = info.push(text(site).size(12).style(text::primary));
                }
                if !title.is_empty() {
                    info = info.push(text(title).size(14).font(iced::Font {
                        weight: iced::font::Weight::Bold,
                        ..iced::Font::DEFAULT
                    }));
                }
                if !description.is_empty() {
                    let short: String = description.chars().take(200).collect();
                    info = info.push(text(short).size(13));
                }
                if !display.is_empty() {
                    info = info.push(text(display).size(11).style(text::primary));
                }
                let mut card = row![info].spacing(8);
                if let Some(file_id) = *thumb {
                    let picture: Element<'a, Msg> = match self.session.images.peek(file_id) {
                        Some(handle) => image(handle.clone())
                            .width(64)
                            .height(64)
                            .content_fit(ContentFit::Cover)
                            .into(),
                        None => container(text("")).width(64).height(64).into(),
                    };
                    card = card.push(
                        sensor(picture).on_show(move |_| on_pane(PaneMsg::MediaVisible(file_id))),
                    );
                }
                button(card)
                    .padding([4, 8])
                    .style(card_button)
                    .on_press(open(url.clone(), *hidden))
                    .into()
            }
            Extra::Poll {
                question,
                options,
                total,
                quiz,
                correct,
                allow_multiple,
                closed,
                anonymous,
            } => {
                let kind = match (quiz, anonymous) {
                    (true, _) => "Викторина",
                    (false, true) => "Анонимный опрос",
                    (false, false) => "Опрос",
                };
                let mut card = column![
                    text(kind).size(12).style(text::primary),
                    text(question).size(15),
                ]
                .spacing(6)
                .width(Fill);
                let results = extra.voted();
                let picked = self
                    .session
                    .panes
                    .get(&window)
                    .and_then(|p| p.poll_selection.get(&m.id));
                for (i, option) in options.iter().enumerate() {
                    let i = i as i32;
                    if results {
                        let mark = if *quiz {
                            if correct.contains(&i) {
                                "✓ "
                            } else if option.chosen {
                                "✗ "
                            } else {
                                ""
                            }
                        } else if option.chosen {
                            "✓ "
                        } else {
                            ""
                        };
                        card = card.push(
                            column![
                                row![
                                    text(format!("{mark}{}", option.text)).size(14).width(Fill),
                                    text(format!("{}%", option.percent)).size(13),
                                ],
                                iced::widget::progress_bar(0.0..=100.0, option.percent as f32)
                                    .girth(4),
                            ]
                            .spacing(2),
                        );
                    } else if *allow_multiple {
                        let checked = picked.is_some_and(|s| s.contains(&i));
                        let mark = if checked { "☑ " } else { "☐ " };
                        card = card.push(
                            button(text(format!("{mark}{}", option.text)).size(14))
                                .width(Fill)
                                .style(card_button)
                                .on_press(on_pane(PaneMsg::ToggleVote(m.id, i))),
                        );
                    } else {
                        card = card.push(
                            button(text(&option.text).size(14))
                                .width(Fill)
                                .style(card_button)
                                .on_press(on_pane(PaneMsg::Vote(m.id, i))),
                        );
                    }
                }
                if !results && *allow_multiple {
                    let has_selection = picked.is_some_and(|s| !s.is_empty());
                    card = card.push(
                        button(text("Голосовать").size(13).style(text::primary))
                            .style(card_button)
                            .on_press_maybe(
                                has_selection.then(|| on_pane(PaneMsg::SubmitVote(m.id))),
                            ),
                    );
                }
                let footer = match (*closed, *total) {
                    (true, n) => format!("Завершён · голосов: {n}"),
                    (false, 0) => "Голосов пока нет".to_owned(),
                    (false, n) => format!("Голосов: {n}"),
                };
                card.push(text(footer).size(12)).into()
            }
            Extra::Contact {
                name,
                phone,
                user_id,
            } => {
                let card = row![
                    self.avatar(super::avatars::Peer::User(*user_id), name, 40.0),
                    column![text(name).size(15), text(phone_label(phone)).size(13)].spacing(2),
                ]
                .spacing(8)
                .align_y(iced::Center);
                if *user_id != 0 {
                    iced::widget::mouse_area(card)
                        .interaction(iced::mouse::Interaction::Pointer)
                        .on_press(Msg::Card(super::card::CardMsg::Open(window, *user_id)))
                        .into()
                } else {
                    card.into()
                }
            }
            Extra::Location {
                latitude,
                longitude,
                title,
                address,
            } => {
                let mut card = column![].spacing(2);
                card = card.push(
                    text(if title.is_empty() {
                        "📍 Геопозиция".to_owned()
                    } else {
                        format!("📍 {title}")
                    })
                    .size(15),
                );
                if !address.is_empty() {
                    card = card.push(text(address).size(13));
                }
                card = card.push(text(format!("{latitude:.5}, {longitude:.5}")).size(12));
                button(card.push(text("Открыть на карте").size(13).style(text::primary)))
                    .padding([4, 8])
                    .style(card_button)
                    .on_press(open(map_url(*latitude, *longitude), false))
                    .into()
            }
        }
    }
}

/// A card inside a bubble: a faint panel, lighter on hover.
fn card_button(theme: &iced::Theme, status: button::Status) -> button::Style {
    let palette = theme.extended_palette();
    let base = palette.background.base.color;
    let alpha = match status {
        button::Status::Hovered | button::Status::Pressed => 0.35,
        _ => 0.2,
    };
    button::Style {
        background: Some(iced::Color { a: alpha, ..base }.into()),
        text_color: palette.background.base.text,
        border: iced::border::rounded(6),
        ..button::Style::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    #[test]
    fn map_link_points_at_the_place() {
        assert_eq!(
            map_url(55.751244, 37.618423),
            "https://www.openstreetmap.org/?mlat=55.751244&mlon=37.618423#map=16/55.751244/37.618423"
        );
    }

    #[test]
    fn contact_phone_gets_a_plus() {
        assert_eq!(phone_label("5550100"), "+5550100");
        assert_eq!(phone_label("+1 555"), "+1 555");
        assert_eq!(phone_label(""), "");
    }

    #[test]
    fn preview_hidden_checks_the_visible_text_literally() {
        assert!(!preview_hidden(
            "https://bank.example",
            "смотри https://bank.example"
        ));
        assert!(preview_hidden("https://evil.example", "просто текст"));
    }

    fn link_preview_message(url: &str, text: &str) -> Value {
        json!({"@type": "messageText",
            "text": {"@type": "formattedText", "text": text, "entities": []},
            "link_preview": {"@type": "linkPreview", "url": url, "display_url": "bank.example",
                "site_name": "", "title": "Обновление пароля", "author": "",
                "description": {"@type": "formattedText", "text": "", "entities": []},
                "type": {"@type": "linkPreviewTypeArticle"},
                "has_large_media": false, "show_large_media": false,
                "show_media_above_description": false, "skip_confirmation": false,
                "show_above_text": false, "instant_view_version": 0}})
    }

    #[test]
    fn link_card_hides_a_url_not_shown_in_the_message() {
        let visible: MessageContent = serde_json::from_value(link_preview_message(
            "https://bank.example",
            "смотри https://bank.example",
        ))
        .unwrap();
        let (extra, _) = extra_of(&visible).unwrap();
        assert!(
            matches!(extra, Extra::Link { hidden: false, .. }),
            "the address is right there in the text"
        );

        let swapped: MessageContent =
            serde_json::from_value(link_preview_message("https://evil.example", "смотри сюда"))
                .unwrap();
        let (extra, _) = extra_of(&swapped).unwrap();
        assert!(
            matches!(extra, Extra::Link { hidden: true, .. }),
            "the card's target is not what the text shows"
        );
    }

    fn quiz_poll_content(correct: Vec<i32>, chosen: [bool; 2]) -> Value {
        json!({"@type": "messagePoll",
            "description": {"@type": "formattedText", "text": "", "entities": []},
            "can_add_option": false,
            "poll": {"@type": "poll", "can_get_voters": false, "can_see_results": true,
                "allows_multiple_answers": false, "allows_revoting": false, "members_only": false,
                "country_codes": [], "option_order": [], "id": "1",
                "question": {"@type": "formattedText", "text": "2+2?", "entities": []},
                "options": [
                    {"@type": "pollOption", "id": "1", "recent_voter_ids": [], "addition_date": 0,
                     "text": {"@type": "formattedText", "text": "3", "entities": []},
                     "voter_count": 1, "vote_percentage": 50, "is_chosen": chosen[0], "is_being_chosen": false},
                    {"@type": "pollOption", "id": "2", "recent_voter_ids": [], "addition_date": 0,
                     "text": {"@type": "formattedText", "text": "4", "entities": []},
                     "voter_count": 1, "vote_percentage": 50, "is_chosen": chosen[1], "is_being_chosen": false}],
                "total_voter_count": 2, "recent_voter_ids": [], "is_anonymous": true,
                "type": {"@type": "pollTypeQuiz", "correct_option_ids": correct,
                    "explanation": {"@type": "formattedText", "text": "", "entities": []}},
                "open_period": 0, "close_date": 0, "is_closed": false}})
    }

    #[test]
    fn quiz_carries_the_correct_option_regardless_of_the_users_choice() {
        let content: MessageContent =
            serde_json::from_value(quiz_poll_content(vec![1], [true, false])).unwrap();
        let (extra, _) = extra_of(&content).unwrap();
        let Extra::Poll {
            quiz,
            correct,
            options,
            ..
        } = extra
        else {
            panic!("expected a poll");
        };
        assert!(quiz);
        assert_eq!(correct, vec![1]);
        // The user picked the wrong option (index 0); the right one (index
        // 1) is not marked as their choice.
        assert!(options[0].chosen && !options[1].chosen);
    }

    fn multi_poll_content() -> Value {
        json!({"@type": "messagePoll",
            "description": {"@type": "formattedText", "text": "", "entities": []},
            "can_add_option": false,
            "poll": {"@type": "poll", "can_get_voters": false, "can_see_results": true,
                "allows_multiple_answers": true, "allows_revoting": false, "members_only": false,
                "country_codes": [], "option_order": [], "id": "1",
                "question": {"@type": "formattedText", "text": "Что берём?", "entities": []},
                "options": [
                    {"@type": "pollOption", "id": "1", "recent_voter_ids": [], "addition_date": 0,
                     "text": {"@type": "formattedText", "text": "хлеб", "entities": []},
                     "voter_count": 0, "vote_percentage": 0, "is_chosen": false, "is_being_chosen": false},
                    {"@type": "pollOption", "id": "2", "recent_voter_ids": [], "addition_date": 0,
                     "text": {"@type": "formattedText", "text": "молоко", "entities": []},
                     "voter_count": 0, "vote_percentage": 0, "is_chosen": false, "is_being_chosen": false}],
                "total_voter_count": 0, "recent_voter_ids": [], "is_anonymous": true,
                "type": {"@type": "pollTypeRegular"},
                "open_period": 0, "close_date": 0, "is_closed": false}})
    }

    #[test]
    fn poll_allowing_several_answers_is_marked_as_such() {
        let content: MessageContent = serde_json::from_value(multi_poll_content()).unwrap();
        let (extra, _) = extra_of(&content).unwrap();
        assert!(matches!(
            extra,
            Extra::Poll {
                allow_multiple: true,
                ..
            }
        ));
    }
}
