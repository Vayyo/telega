//! "печатает…" indicators: who is typing in which chat, from TDLib chat
//! actions, with the animated dots.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use tdlib_rs::enums::MessageSender;

/// TDLib cancels actions itself; this only guards against a lost cancel.
const EXPIRY: Duration = Duration::from_secs(7);
/// Dot animation step.
pub(crate) const FRAME: Duration = Duration::from_millis(400);

#[derive(Default)]
pub(crate) struct Typing {
    /// Per chat, in the order they started typing.
    chats: HashMap<i64, Vec<(MessageSender, Instant)>>,
    frame: usize,
}

impl Typing {
    pub(crate) fn set(&mut self, chat_id: i64, sender: MessageSender, typing: bool, now: Instant) {
        let senders = self.chats.entry(chat_id).or_default();
        match senders.iter_mut().find(|(s, _)| *s == sender) {
            Some(entry) if typing => entry.1 = now,
            Some(_) => senders.retain(|(s, _)| *s != sender),
            None if typing => senders.push((sender, now)),
            None => {}
        }
        if senders.is_empty() {
            self.chats.remove(&chat_id);
        }
    }

    /// Next animation frame; forgets actions whose cancel never came.
    pub(crate) fn tick(&mut self, now: Instant) {
        self.frame = self.frame.wrapping_add(1);
        self.chats.retain(|_, senders| {
            senders.retain(|(_, at)| now.duration_since(*at) < EXPIRY);
            !senders.is_empty()
        });
    }

    pub(crate) fn active(&self) -> bool {
        !self.chats.is_empty()
    }

    pub(crate) fn senders(&self, chat_id: i64) -> impl Iterator<Item = &MessageSender> {
        self.chats
            .get(&chat_id)
            .into_iter()
            .flatten()
            .map(|(s, _)| s)
    }

    /// ".", "..", "..." in turn.
    pub(crate) fn dots(&self) -> &'static str {
        [".", "..", "..."][self.frame % 3]
    }
}

/// "печатает" in a private chat; in groups "Игорь печатает",
/// "Игорь и Маша печатают", "Игорь и ещё 3 печатают".
pub(crate) fn label(private: bool, names: &[&str]) -> Option<String> {
    Some(match names {
        [] => return None,
        _ if private => "печатает".to_owned(),
        [one] => format!("{one} печатает"),
        [a, b] => format!("{a} и {b} печатают"),
        [a, rest @ ..] => format!("{a} и ещё {} печатают", rest.len()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tdlib_rs::types::MessageSenderUser;

    fn user(id: i64) -> MessageSender {
        MessageSender::User(MessageSenderUser { user_id: id })
    }

    #[test]
    fn labels_for_private_chats_and_groups() {
        assert_eq!(label(true, &["Денис"]).as_deref(), Some("печатает"));
        assert_eq!(label(false, &["Игорь"]).as_deref(), Some("Игорь печатает"));
        assert_eq!(
            label(false, &["Игорь", "Маша"]).as_deref(),
            Some("Игорь и Маша печатают")
        );
        assert_eq!(
            label(false, &["Игорь", "Маша", "Петя"]).as_deref(),
            Some("Игорь и ещё 2 печатают")
        );
        assert_eq!(label(false, &[]), None);
    }

    #[test]
    fn typing_starts_stops_and_expires_without_cancel() {
        let start = Instant::now();
        let mut typing = Typing::default();
        typing.set(1, user(7), true, start);
        typing.set(1, user(8), true, start);
        typing.set(1, user(7), true, start + Duration::from_secs(5));
        assert_eq!(typing.senders(1).count(), 2);
        typing.set(1, user(8), false, start);
        assert_eq!(typing.senders(1).collect::<Vec<_>>(), [&user(7)]);
        // Renewed at 5 s: alive at 10 s, gone at 13 s.
        typing.tick(start + Duration::from_secs(10));
        assert!(typing.active());
        typing.tick(start + Duration::from_secs(13));
        assert!(!typing.active());
    }

    #[test]
    fn dots_cycle() {
        let mut typing = Typing::default();
        let frames: Vec<_> = (0..4)
            .map(|_| {
                let d = typing.dots();
                typing.tick(Instant::now());
                d
            })
            .collect();
        assert_eq!(frames, [".", "..", "...", "."]);
    }
}
