//! Desktop notifications. TDLib decides what deserves a notification (mute
//! settings, exceptions, mentions in muted chats) and sends notification
//! groups; the client shows them unless the chat is on screen in a focused
//! window. Clicking a notification opens its chat.

use std::collections::HashSet;
use std::sync::LazyLock;

use parking_lot::Mutex;

use iced::Task;
use iced::futures::Stream;
use tdlib_rs::enums::NotificationType;
use tdlib_rs::types::UpdateNotificationGroup;
use tokio::sync::mpsc;

use super::{App, Msg};

type ClickChannel = (
    mpsc::UnboundedSender<(u32, i64)>,
    Mutex<Option<mpsc::UnboundedReceiver<(u32, i64)>>>,
);

/// Chats of clicked notifications, from notification threads to the UI.
static CLICKS: LazyLock<ClickChannel> = LazyLock::new(|| {
    let (tx, rx) = mpsc::unbounded_channel();
    (tx, Mutex::new(Some(rx)))
});

/// Stream of clicked notifications for an iced subscription: the account
/// slot a click's popup was shown for, and its chat.
pub(crate) fn clicks() -> impl Stream<Item = (u32, i64)> {
    iced::stream::channel(16, async |mut out| {
        use iced::futures::SinkExt;
        let Some(mut rx) = CLICKS.1.lock().take() else {
            return;
        };
        while let Some(click) = rx.recv().await {
            if out.send(click).await.is_err() {
                return;
            }
        }
    })
}

/// What a desktop notification shows.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Popup {
    /// Account it was shown for; a click is ignored once another account
    /// is active (the switch may leave this popup on screen unclosed).
    pub(crate) slot: u32,
    pub(crate) chat_id: i64,
    pub(crate) title: String,
    pub(crate) body: String,
    pub(crate) silent: bool,
}

/// Deterministic D-Bus notification id for a chat: one popup per chat, a
/// new message replaces the previous one instead of stacking.
#[cfg(all(unix, not(target_os = "macos")))]
fn notification_id(chat_id: i64) -> u32 {
    (chat_id as u64 ^ (chat_id as u64 >> 32)) as u32 | 1
}

/// Notification ids whose click is still awaited, one waiting thread per
/// id: a replacing notification (a newer message in the same chat) shares
/// the id, so the thread already watching it covers the new one too.
static WAITING_IDS: LazyLock<Mutex<HashSet<u32>>> = LazyLock::new(|| Mutex::new(HashSet::new()));

/// Whether the running notification server advertises `body-markup`
/// (org.freedesktop.Notifications `GetCapabilities`), checked once: a
/// server without it shows the body as plain text, and escaping it would
/// then put literal `&amp;`/`&lt;` on screen instead of hiding markup.
#[cfg(all(unix, not(target_os = "macos")))]
static BODY_MARKUP: LazyLock<bool> = LazyLock::new(|| {
    let Ok(conn) = zbus::blocking::Connection::session() else {
        return false;
    };
    let Ok(reply) = conn.call_method(
        Some("org.freedesktop.Notifications"),
        "/org/freedesktop/Notifications",
        Some("org.freedesktop.Notifications"),
        "GetCapabilities",
        &(),
    ) else {
        return false;
    };
    reply
        .body()
        .deserialize::<Vec<String>>()
        .is_ok_and(|caps| caps.iter().any(|c| c == "body-markup"))
});

#[cfg(all(unix, not(target_os = "macos")))]
fn body_markup_supported() -> bool {
    *BODY_MARKUP
}

/// Other notification centers (macOS, Windows) show the body as plain
/// text: no markup capability to escape against.
#[cfg(not(all(unix, not(target_os = "macos"))))]
fn body_markup_supported() -> bool {
    false
}

/// Frees the waiting slot for its id when the thread is done, even if
/// `wait_for_action` panics.
struct WaitGuard(u32);

impl Drop for WaitGuard {
    fn drop(&mut self) {
        WAITING_IDS.lock().remove(&self.0);
    }
}

/// Text for the notification body when the server advertises
/// `body-markup`: it interprets a subset of HTML, so sender-controlled
/// text is escaped to stay plain text instead of forming markup.
fn escape_markup(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

/// Shows a notification on a blocking thread; on Linux waits for a click.
fn show(popup: Popup) {
    std::thread::spawn(move || {
        let mut n = notify_rust::Notification::new();
        let body = if body_markup_supported() {
            escape_markup(&popup.body)
        } else {
            popup.body.clone()
        };
        n.appname("Telega").summary(&popup.title).body(&body);
        // One popup per chat: a new message replaces the previous one.
        #[cfg(all(unix, not(target_os = "macos")))]
        let id = notification_id(popup.chat_id);
        #[cfg(all(unix, not(target_os = "macos")))]
        n.id(id);
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            n.action("default", "Открыть");
            if popup.silent {
                n.hint(notify_rust::Hint::SuppressSound(true));
            }
        }
        let Ok(handle) = n.show() else { return };
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            // Only one thread waits per id: a replacing notification just
            // updates what the running thread already watches for a click.
            if !WAITING_IDS.lock().insert(id) {
                return;
            }
            let _guard = WaitGuard(id);
            handle.wait_for_action(|action| {
                if action == "default" {
                    let _ = CLICKS.0.send((popup.slot, popup.chat_id));
                }
            });
        }
        #[cfg(not(all(unix, not(target_os = "macos"))))]
        drop(handle);
    });
}

/// Removes the desktop popup for a chat whose notification was read or
/// deleted elsewhere, by the deterministic id `show` gave it.
fn close(chat_id: i64) {
    #[cfg(target_os = "linux")]
    std::thread::spawn(move || {
        let id = notification_id(chat_id);
        if let Ok(conn) = zbus::blocking::Connection::session() {
            let _ = conn.call_method(
                Some("org.freedesktop.Notifications"),
                "/org/freedesktop/Notifications",
                Some("org.freedesktop.Notifications"),
                "CloseNotification",
                &(id,),
            );
        }
    });
    #[cfg(not(target_os = "linux"))]
    let _ = chat_id;
}

/// The group has nothing left worth a popup: its notifications were all
/// read or deleted elsewhere before the user acted on them.
fn group_emptied(group: &UpdateNotificationGroup) -> bool {
    group.total_count == 0 || !group.removed_notification_ids.is_empty()
}

impl App {
    /// New notifications from TDLib become desktop popups; a group that
    /// has nothing left to show (read or deleted elsewhere) closes its
    /// popup instead.
    pub(super) fn on_notification_group(&mut self, group: &UpdateNotificationGroup) -> Task<Msg> {
        match self.popup(group) {
            Some(popup) => show(popup),
            None if group_emptied(group) => close(group.chat_id),
            None => {}
        }
        Task::none()
    }

    /// Popup for the newest notification added to a group; `None` when
    /// disabled, the chat is visible in the focused window, or this update
    /// only removed notifications without adding a new one. Notifications
    /// in a group all replace the same on-screen popup, so only the
    /// newest (last, they are sorted by id) is worth showing.
    pub(super) fn popup(&self, group: &UpdateNotificationGroup) -> Option<Popup> {
        if !self.settings.notifications || self.chat_in_focus(group.chat_id) {
            return None;
        }
        let title = self
            .session
            .chats
            .get(&group.chat_id)
            .map_or_else(|| "Telega".to_owned(), |c| c.title.clone());
        group
            .added_notifications
            .iter()
            .rev()
            .find_map(|n| match &n.r#type {
                NotificationType::NewMessage(m) => {
                    let body = if m.show_preview {
                        let text = super::rich::preview(&m.message.content);
                        let sender = self.sender_label(&m.message);
                        match sender {
                            Some(name) => format!("{name}: {text}"),
                            None => text,
                        }
                    } else {
                        "Новое сообщение".to_owned()
                    };
                    Some(Popup {
                        slot: self.session.slot,
                        chat_id: group.chat_id,
                        title: title.clone(),
                        body,
                        silent: n.is_silent,
                    })
                }
                _ => None,
            })
    }

    /// Author shown in group chats; private chats need no prefix.
    fn sender_label(&self, m: &tdlib_rs::types::Message) -> Option<String> {
        match &m.sender_id {
            tdlib_rs::enums::MessageSender::User(u) if u.user_id != m.chat_id => {
                self.session.users.get(&u.user_id).cloned()
            }
            _ => None,
        }
    }

    fn chat_in_focus(&self, chat_id: i64) -> bool {
        self.focused
            .and_then(|w| self.session.panes.get(&w))
            .is_some_and(|p| p.shows(chat_id))
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn notification_text_cannot_carry_markup() {
        assert_eq!(
            super::escape_markup("<a href=\"x\">Банк</a> & <b>"),
            "&lt;a href=&quot;x&quot;&gt;Банк&lt;/a&gt; &amp; &lt;b&gt;"
        );
    }

    fn group(total_count: i32, removed: Vec<i32>) -> tdlib_rs::types::UpdateNotificationGroup {
        tdlib_rs::types::UpdateNotificationGroup {
            notification_group_id: 1,
            r#type: tdlib_rs::enums::NotificationGroupType::Messages,
            chat_id: 1,
            notification_settings_chat_id: 1,
            notification_sound_id: 0,
            total_count,
            added_notifications: Vec::new(),
            removed_notification_ids: removed,
        }
    }

    #[test]
    fn group_emptied_by_total_count_or_a_removal_with_nothing_new() {
        assert!(super::group_emptied(&group(0, Vec::new())));
        assert!(super::group_emptied(&group(1, vec![7])));
        assert!(!super::group_emptied(&group(1, Vec::new())));
    }
}
