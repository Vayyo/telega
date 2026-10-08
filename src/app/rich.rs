//! Formatted message text: TDLib entities (UTF-16 offsets) turned into
//! styled pieces for iced's rich text, including spoilers.

use tdlib_rs::enums::{MessageContent, TextEntityType};
use tdlib_rs::types::FormattedText;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Style {
    pub(crate) bold: bool,
    pub(crate) italic: bool,
    pub(crate) underline: bool,
    pub(crate) strike: bool,
    pub(crate) code: bool,
    pub(crate) spoiler: bool,
    /// Target of a clickable link.
    pub(crate) link: Option<String>,
    /// The visible text is not the target (hidden link): opening asks first.
    pub(crate) link_hidden: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Piece {
    pub(crate) text: String,
    pub(crate) style: Style,
}

/// Text or caption of a message, with its formatting.
pub(crate) fn formatted_of(content: &MessageContent) -> Option<&FormattedText> {
    Some(match content {
        MessageContent::MessageText(m) => &m.text,
        MessageContent::MessagePhoto(m) => &m.caption,
        MessageContent::MessageVideo(m) => &m.caption,
        MessageContent::MessageAnimation(m) => &m.caption,
        MessageContent::MessageDocument(m) => &m.caption,
        MessageContent::MessageAudio(m) => &m.caption,
        MessageContent::MessageVoiceNote(m) => &m.caption,
        _ => return None,
    })
}

pub(crate) fn plain(text: &str) -> Vec<Piece> {
    vec![Piece {
        text: text.to_owned(),
        style: Style::default(),
    }]
}

/// Splits text at entity boundaries; each piece carries the union of the
/// styles of all entities covering it. Entity offsets are UTF-16 units.
pub(crate) fn pieces(text: &FormattedText) -> Vec<Piece> {
    if text.entities.is_empty() {
        return vec![Piece {
            text: text.text.clone(),
            style: Style::default(),
        }];
    }
    // UTF-16 offset → byte offset, for every char boundary.
    let mut utf16_to_byte = Vec::with_capacity(text.text.len() + 1);
    for (byte, ch) in text.text.char_indices() {
        for _ in 0..ch.len_utf16() {
            utf16_to_byte.push(byte);
        }
    }
    utf16_to_byte.push(text.text.len());
    let byte_at = |utf16: i32| {
        utf16_to_byte
            .get(utf16.max(0) as usize)
            .copied()
            .unwrap_or(text.text.len())
    };

    let spans: Vec<(usize, usize, &TextEntityType)> = text
        .entities
        .iter()
        .filter_map(|e| {
            let end = e.offset.checked_add(e.length)?;
            Some((byte_at(e.offset), byte_at(end), &e.r#type))
        })
        .filter(|(s, e, _)| s < e)
        .collect();
    let mut cuts: Vec<usize> = spans.iter().flat_map(|&(s, e, _)| [s, e]).collect();
    cuts.extend([0, text.text.len()]);
    cuts.sort_unstable();
    cuts.dedup();

    let mut out: Vec<Piece> = Vec::new();
    for pair in cuts.windows(2) {
        let (start, end) = (pair[0], pair[1]);
        let segment = &text.text[start..end];
        let mut style = Style::default();
        for &(s, e, kind) in &spans {
            if s <= start && end <= e {
                apply(&mut style, kind, &text.text[s..e]);
            }
        }
        match out.last_mut() {
            Some(last) if last.style == style => last.text.push_str(segment),
            _ => out.push(Piece {
                text: segment.to_owned(),
                style,
            }),
        }
    }
    out
}

fn apply(style: &mut Style, kind: &TextEntityType, whole: &str) {
    match kind {
        TextEntityType::Bold => style.bold = true,
        TextEntityType::Italic => style.italic = true,
        TextEntityType::Underline => style.underline = true,
        TextEntityType::Strikethrough => style.strike = true,
        TextEntityType::Code | TextEntityType::Pre | TextEntityType::PreCode(_) => {
            style.code = true;
        }
        TextEntityType::Spoiler => style.spoiler = true,
        TextEntityType::Url => style.link = Some(with_scheme(whole)),
        TextEntityType::TextUrl(u) => {
            style.link = Some(u.url.clone());
            style.link_hidden = whole.trim() != u.url.trim();
        }
        TextEntityType::EmailAddress => style.link = Some(format!("mailto:{whole}")),
        TextEntityType::Mention => {
            style.link = Some(format!("https://t.me/{}", whole.trim_start_matches('@')));
        }
        // No username exists for this user (that's why TDLib gives an id
        // instead): a synthetic link the client recognizes and turns into
        // opening the profile directly, without asking TDLib to resolve it.
        TextEntityType::MentionName(m) => {
            style.link = Some(format!("tg://user?id={}", m.user_id));
        }
        _ => {}
    }
}

fn with_scheme(url: &str) -> String {
    // A real scheme is `<letters/digits/+-.>://`, checked at the very
    // start: a `Url` entity without a protocol can still contain "://"
    // further in (e.g. in a query string, `example.com/go?to=https://x`),
    // which must not be mistaken for one.
    let has_scheme = url.split_once("://").is_some_and(|(scheme, _)| {
        !scheme.is_empty()
            && scheme
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c))
    });
    if has_scheme {
        url.to_owned()
    } else {
        format!("https://{url}")
    }
}

/// What the client hands to the browser or mail program: web links as
/// they are, e-mail links reduced to the address (parameters like `attach=`
/// could make a mail program attach local files). Anything else (file://,
/// javascript:, custom schemes) is refused.
pub(crate) fn safe_link(url: &str) -> Option<String> {
    let lower = url.to_ascii_lowercase();
    if lower.starts_with("https://") || lower.starts_with("http://") {
        return Some(url.to_owned());
    }
    if lower.starts_with("mailto:") {
        let address = url["mailto:".len()..]
            .split(['?', '#'])
            .next()
            .unwrap_or("");
        return (!address.is_empty()).then(|| format!("mailto:{address}"));
    }
    None
}

/// Color emoji font of the system. Plain sans fonts (DejaVu) carry
/// black-and-white outlines for many emoji and would be picked first.
pub(crate) const EMOJI_FONT: iced::Font = iced::Font::with_name(if cfg!(windows) {
    "Segoe UI Emoji"
} else if cfg!(target_os = "macos") {
    "Apple Color Emoji"
} else {
    "Noto Color Emoji"
});

fn is_emoji(c: char) -> bool {
    matches!(u32::from(c),
        0x1F000..=0x1FAFF      // pictographs, emoticons, flags, symbols
        | 0x2600..=0x27BF      // misc symbols, dingbats
        | 0x2B00..=0x2BFF      // arrows, stars
        | 0x2300..=0x23FF      // technical (⌚ ⏰)
        | 0x20E3               // keycap
        | 0xE0020..=0xE007F) // tag sequences
}

/// Splits text into runs of emoji and other text: `("hi ", false),
/// ("🔥", true)`. Joiners and variation selectors stay with their emoji.
pub(crate) fn emoji_runs(text: &str) -> Vec<(&str, bool)> {
    let mut runs = Vec::new();
    let mut start = 0;
    let mut current: Option<bool> = None;
    let mut chars = text.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        // ZWJ and the emoji variation selector only count as emoji right
        // after an emoji: on their own they join ordinary letters (Indic
        // conjuncts, Arabic/Persian joining forms) and must not cut the
        // run apart. A plain symbol is still turned into emoji by a
        // following selector or keycap (▶️, ©️, 1️⃣).
        let emoji = if matches!(c, '\u{200D}' | '\u{FE0F}') {
            current == Some(true)
        } else {
            is_emoji(c)
                || chars
                    .peek()
                    .is_some_and(|&(_, n)| matches!(n, '\u{FE0F}' | '\u{20E3}'))
        };
        match current {
            Some(kind) if kind != emoji => {
                runs.push((&text[start..i], kind));
                start = i;
                current = Some(emoji);
            }
            None => current = Some(emoji),
            _ => {}
        }
    }
    if let Some(kind) = current {
        runs.push((&text[start..], kind));
    }
    runs
}

/// Text with spoilers covered, for places that cannot hide them otherwise
/// (notifications, chat list, quotes, search results).
pub(crate) fn masked(pieces: &[Piece]) -> String {
    pieces
        .iter()
        .map(|p| {
            if p.style.spoiler {
                "▒▒▒"
            } else {
                p.text.as_str()
            }
        })
        .collect()
}

/// One-line description of a message with spoilers covered.
pub(crate) fn preview(content: &MessageContent) -> String {
    let label = crate::td::message_text(content);
    match formatted_of(content) {
        Some(f)
            if f.entities
                .iter()
                .any(|e| matches!(e.r#type, TextEntityType::Spoiler)) =>
        {
            let hidden = masked(&pieces(f));
            match label.strip_suffix(f.text.as_str()) {
                Some(prefix) => format!("{prefix}{hidden}"),
                None => hidden,
            }
        }
        _ => label,
    }
}

#[cfg(test)]
mod tests {
    use tdlib_rs::types::{TextEntity, TextEntityTypeTextUrl};

    use super::*;

    fn text(s: &str, entities: Vec<(i32, i32, TextEntityType)>) -> FormattedText {
        FormattedText {
            text: s.into(),
            entities: entities
                .into_iter()
                .map(|(offset, length, r#type)| TextEntity {
                    offset,
                    length,
                    r#type,
                })
                .collect(),
        }
    }

    #[test]
    fn overlapping_entities_combine_and_utf16_offsets_are_respected() {
        // "😀" is two UTF-16 units; bold covers "жир ", italic covers "ир кур".
        let t = text(
            "😀 жир курсив",
            vec![(3, 4, TextEntityType::Bold), (4, 6, TextEntityType::Italic)],
        );
        let got: Vec<(String, bool, bool)> = pieces(&t)
            .into_iter()
            .map(|p| (p.text, p.style.bold, p.style.italic))
            .collect();
        assert_eq!(
            got,
            [
                ("😀 ".into(), false, false),
                ("ж".into(), true, false),
                ("ир ".into(), true, true),
                ("кур".into(), false, true),
                ("сив".into(), false, false),
            ]
        );
    }

    #[test]
    fn overflowing_entity_does_not_crash() {
        let t = text(
            "hello",
            vec![
                (i32::MAX, 1, TextEntityType::Bold),
                (0, 5, TextEntityType::Italic),
            ],
        );
        assert_eq!(
            pieces(&t),
            vec![Piece {
                text: "hello".into(),
                style: Style {
                    italic: true,
                    ..Style::default()
                },
            }]
        );
    }

    #[test]
    fn spoilers_and_links_are_marked() {
        let t = text(
            "тайна и сайт",
            vec![
                (0, 5, TextEntityType::Spoiler),
                (
                    8,
                    4,
                    TextEntityType::TextUrl(TextEntityTypeTextUrl {
                        url: "https://example.org".into(),
                    }),
                ),
            ],
        );
        let p = pieces(&t);
        assert!(p[0].style.spoiler && p[0].text == "тайна");
        assert_eq!(p[2].style.link.as_deref(), Some("https://example.org"));
    }

    #[test]
    fn emoji_are_split_from_text_with_their_joiners() {
        assert_eq!(
            emoji_runs("ну 😂👍🏻 да ❤️ 👨‍👩‍👧"),
            [
                ("ну ", false),
                ("😂👍🏻", true),
                (" да ", false),
                ("❤️", true),
                (" ", false),
                ("👨‍👩‍👧", true)
            ]
        );
        assert_eq!(
            emoji_runs("▶️ пуск 1️⃣"),
            [("▶️", true), (" пуск ", false), ("1️⃣", true)]
        );
        assert_eq!(emoji_runs("просто текст"), [("просто текст", false)]);
        assert!(emoji_runs("").is_empty());
    }

    #[test]
    fn zwj_inside_devanagari_does_not_split_the_conjunct() {
        // "क्‍ष" (ka + virama + ZWJ + sha): the ZWJ asks the shaper to keep
        // the conjunct form and must stay in the same, non-emoji run.
        assert_eq!(
            emoji_runs("नमस्ते क्\u{200D}ष विश्व"),
            [("नमस्ते क्\u{200D}ष विश्व", false)]
        );
    }

    #[test]
    fn with_scheme_only_recognizes_a_leading_scheme() {
        assert_eq!(with_scheme("example.com"), "https://example.com");
        // "://" appears, but not right after a scheme: must still be prefixed.
        assert_eq!(
            with_scheme("example.com/go?to=https://x"),
            "https://example.com/go?to=https://x"
        );
        assert_eq!(
            with_scheme("t.me/share/url?url=http://evil.example"),
            "https://t.me/share/url?url=http://evil.example"
        );
        // A real scheme is left alone.
        assert_eq!(with_scheme("http://example.com"), "http://example.com");
        assert_eq!(with_scheme("ftp://example.com"), "ftp://example.com");
    }

    #[test]
    fn mention_name_links_to_the_user_without_a_username() {
        let t = text(
            "привет, Аня",
            vec![(
                8,
                3,
                TextEntityType::MentionName(tdlib_rs::types::TextEntityTypeMentionName {
                    user_id: 42,
                }),
            )],
        );
        let p = pieces(&t);
        assert_eq!(p[1].style.link.as_deref(), Some("tg://user?id=42"));
    }

    #[test]
    fn only_web_and_plain_mail_links_are_opened() {
        assert_eq!(
            safe_link("https://t.me/x").as_deref(),
            Some("https://t.me/x")
        );
        assert_eq!(
            safe_link("mailto:a@b.c?attach=/home/u/.ssh/id_rsa&body=x").as_deref(),
            Some("mailto:a@b.c")
        );
        assert_eq!(safe_link("mailto:?attach=/etc/passwd"), None);
        assert_eq!(safe_link("file:///etc/passwd"), None);
        assert_eq!(safe_link("javascript:alert(1)"), None);
    }

    #[test]
    fn hidden_links_and_spoilers_are_detected() {
        let t = text(
            "https://bank.example и тайна",
            vec![
                (
                    0,
                    20,
                    TextEntityType::TextUrl(TextEntityTypeTextUrl {
                        url: "https://evil.example".into(),
                    }),
                ),
                (23, 5, TextEntityType::Spoiler),
            ],
        );
        let p = pieces(&t);
        assert!(
            p[0].style.link_hidden,
            "text shows another address than it opens"
        );
        assert_eq!(masked(&p), "https://bank.example и ▒▒▒");
    }
}
