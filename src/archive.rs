//! Local copy of seen messages, so messages deleted on the server stay visible.
//!
//! TDLib drops a message from its own database before reporting the deletion,
//! so the text must be saved beforehand: every message the client sees is
//! upserted here, and a permanent deletion only flips the `deleted` flag.
//! Messages that arrive and get deleted while the client is not running are
//! never seen and cannot be recovered.

use std::collections::{HashMap, VecDeque};

use rusqlite::{Connection, params};
use tdlib_rs::enums::MessageSender;
use tdlib_rs::types::{MessageSenderChat, MessageSenderUser};

use crate::app::MsgItem;

pub struct Archive {
    conn: Connection,
    /// With a client password, texts are stored encrypted with this key.
    key: Option<crate::lock::Key>,
    /// Plaintext most recently written for a message, so an identical
    /// re-save skips the SQLite write instead of opening a transaction and
    /// re-encrypting for nothing: TDLib reports the same message through
    /// several updates in a row (`NewChat`'s `last_message`, then
    /// `ChatLastMessage`, then `NewMessage`), and a history reload re-saves
    /// what is already archived unchanged. Bounded so a long session does
    /// not grow it forever.
    dedup: HashMap<(i64, i64), String>,
    dedup_order: VecDeque<(i64, i64)>,
}

/// Messages remembered by `Archive::dedup` at most.
const DEDUP_CAP: usize = 4096;

impl Archive {
    /// One file per account: chat ids of private chats are user ids, so
    /// archives of different accounts must never mix.
    pub fn open_for_account(account_id: i64) -> rusqlite::Result<Self> {
        Self::open(Connection::open(crate::paths::archive(account_id))?)
    }

    #[cfg(test)]
    pub fn in_memory() -> Self {
        Self::open(Connection::open_in_memory().unwrap()).unwrap()
    }

    #[cfg(test)]
    pub(crate) fn insert_malformed_chat_pin(&self, archived: bool) {
        self.conn
            .execute(
                "INSERT INTO local_chat_pins (list, chat_id, pinned_at) VALUES (?1, 'invalid', 1)",
                params![i32::from(archived)],
            )
            .unwrap();
    }

    #[cfg(test)]
    pub(crate) fn has_local_chat_pin(&self, archived: bool, chat_id: i64) -> bool {
        self.conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM local_chat_pins WHERE list = ?1 AND chat_id = ?2)",
                params![i32::from(archived), chat_id],
                |row| row.get(0),
            )
            .unwrap()
    }

    #[cfg(test)]
    pub(crate) fn make_read_only(&self) {
        self.conn.execute_batch("PRAGMA query_only = ON;").unwrap();
    }

    fn open(conn: Connection) -> rusqlite::Result<Self> {
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA secure_delete = ON;
             CREATE TABLE IF NOT EXISTS messages (
                 chat_id     INTEGER NOT NULL,
                 id          INTEGER NOT NULL,
                 sender_user INTEGER,
                 sender_chat INTEGER,
                 outgoing    INTEGER NOT NULL,
                 text        TEXT    NOT NULL,
                 deleted     INTEGER NOT NULL DEFAULT 0,
                 PRIMARY KEY (chat_id, id)
             ) WITHOUT ROWID;",
        )?;
        // Messages the user pinned for themselves, in any chat or channel.
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS local_pins (
                 chat_id     INTEGER NOT NULL,
                 id          INTEGER NOT NULL,
                 sender_user INTEGER,
                 sender_chat INTEGER,
                 text        TEXT    NOT NULL,
                 date        INTEGER NOT NULL,
                 pinned_at   INTEGER NOT NULL,
                 PRIMARY KEY (chat_id, id)
             ) WITHOUT ROWID;",
        )?;
        // Chat-list preferences are independent of message pins and TDLib
        // positions. 0 = main, 1 = archive; folders never have local pins.
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS local_chat_pins (
                 list      INTEGER NOT NULL,
                 chat_id   INTEGER NOT NULL,
                 pinned_at INTEGER NOT NULL,
                 PRIMARY KEY (list, chat_id)
             ) WITHOUT ROWID;",
        )?;
        // Archives made before send times were kept get the column.
        let has_date = conn
            .prepare("SELECT 1 FROM pragma_table_info('messages') WHERE name = 'date'")?
            .exists([])?;
        if !has_date {
            conn.execute_batch("ALTER TABLE messages ADD COLUMN date INTEGER NOT NULL DEFAULT 0")?;
        }
        Ok(Self {
            conn,
            key: None,
            dedup: HashMap::new(),
            dedup_order: VecDeque::new(),
        })
    }

    /// Texts from now on are encrypted with `key` (or stored plain).
    pub fn set_key(&mut self, key: Option<crate::lock::Key>) {
        self.key = key;
    }

    fn stored(&self, text: &str) -> rusqlite::Result<String> {
        crate::lock::store(self.key.as_ref(), text)
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(e.into()))
    }

    /// Re-stores every text under `new` (a password set, changed or
    /// removed), in one transaction: all or nothing.
    pub fn rekey(&mut self, new: Option<crate::lock::Key>) -> rusqlite::Result<()> {
        let old = self.key;
        let tx = self.conn.transaction()?;
        {
            let rows: Vec<(i64, i64, String)> = tx
                .prepare("SELECT chat_id, id, text FROM messages")?
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                .collect::<rusqlite::Result<_>>()?;
            let mut update =
                tx.prepare("UPDATE messages SET text = ?3 WHERE chat_id = ?1 AND id = ?2")?;
            for (chat_id, id, text) in rows {
                let plain = crate::lock::open(old.as_ref(), &text).map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        2,
                        rusqlite::types::Type::Text,
                        e.into(),
                    )
                })?;
                let restored = match &new {
                    Some(key) => crate::lock::seal(key, plain.as_bytes())
                        .map_err(|e| rusqlite::Error::ToSqlConversionFailure(e.into()))?,
                    None => crate::lock::mark_plain(&plain),
                };
                update.execute(params![chat_id, id, restored])?;
            }
        }
        {
            let pins: Vec<(i64, i64, String)> = tx
                .prepare("SELECT chat_id, id, text FROM local_pins")?
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                .collect::<rusqlite::Result<_>>()?;
            let mut update =
                tx.prepare("UPDATE local_pins SET text = ?3 WHERE chat_id = ?1 AND id = ?2")?;
            for (chat_id, id, text) in pins {
                let plain = crate::lock::open(old.as_ref(), &text).map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        2,
                        rusqlite::types::Type::Text,
                        e.into(),
                    )
                })?;
                let restored = match &new {
                    Some(key) => crate::lock::seal(key, plain.as_bytes())
                        .map_err(|e| rusqlite::Error::ToSqlConversionFailure(e.into()))?,
                    None => crate::lock::mark_plain(&plain),
                };
                update.execute(params![chat_id, id, restored])?;
            }
        }
        tx.commit()?;
        self.key = new;
        // Old plaintext or ciphertext left behind by the UPDATEs above must
        // not linger in freed pages or WAL frames now that the archive is
        // meant to be encrypted (or, on removal, no longer needs to be).
        self.conn
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE); VACUUM;")?;
        Ok(())
    }

    /// Pins a message for the user only; the text is kept (sealed with the
    /// password key), so the pin shows even when the message is not loaded.
    pub fn pin_local(&self, m: &MsgItem, now: i64) -> rusqlite::Result<()> {
        let (user, chat) = match &m.sender {
            MessageSender::User(u) => (Some(u.user_id), None),
            MessageSender::Chat(c) => (None, Some(c.chat_id)),
        };
        let text = self.stored(&m.text)?;
        // Strictly after every earlier pin, even within the same second:
        // the order of pinning is the order of the bar.
        let last: Option<i64> =
            self.conn
                .query_row("SELECT MAX(pinned_at) FROM local_pins", [], |r| r.get(0))?;
        let now = last.map_or(now, |last| now.max(last + 1));
        self.conn
            .prepare_cached(
                "INSERT OR REPLACE INTO local_pins
                 (chat_id, id, sender_user, sender_chat, text, date, pinned_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            )?
            .execute(params![m.chat_id, m.id, user, chat, text, m.date, now])
            .map(drop)
    }

    pub fn unpin_local(&self, chat_id: i64, id: i64) -> rusqlite::Result<()> {
        self.conn
            .prepare_cached("DELETE FROM local_pins WHERE chat_id = ?1 AND id = ?2")?
            .execute(params![chat_id, id])
            .map(drop)
    }

    /// Local chat pins of one TDLib list, newest first. Load once per account.
    pub fn local_chat_pins(&self, archive: bool) -> rusqlite::Result<Vec<i64>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT chat_id FROM local_chat_pins WHERE list = ?1
             ORDER BY pinned_at DESC, chat_id DESC",
        )?;
        stmt.query_map(params![i32::from(archive)], |row| row.get(0))?
            .collect()
    }

    /// SQL commits before the caller changes its in-memory list.
    pub fn set_local_chat_pin(
        &self,
        archive: bool,
        chat_id: i64,
        on: bool,
    ) -> rusqlite::Result<()> {
        let list = i32::from(archive);
        if on {
            self.conn.execute(
                "INSERT INTO local_chat_pins (list, chat_id, pinned_at)
                 VALUES (?1, ?2, (SELECT COALESCE(MAX(pinned_at), 0) + 1 FROM local_chat_pins))
                 ON CONFLICT (list, chat_id) DO UPDATE SET pinned_at = excluded.pinned_at",
                params![list, chat_id],
            )?;
        } else {
            self.conn.execute(
                "DELETE FROM local_chat_pins WHERE list = ?1 AND chat_id = ?2",
                params![list, chat_id],
            )?;
        }
        Ok(())
    }

    /// The chat's own pins, most recently pinned first.
    pub fn local_pins(&self, chat_id: i64) -> rusqlite::Result<Vec<MsgItem>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT id, sender_user, sender_chat, text, date FROM local_pins
             WHERE chat_id = ?1 ORDER BY pinned_at DESC, id DESC",
        )?;
        stmt.query_map(params![chat_id], |row| {
            let user: Option<i64> = row.get(1)?;
            let chat: Option<i64> = row.get(2)?;
            let sender = match (user, chat) {
                (Some(user_id), _) => MessageSender::User(MessageSenderUser { user_id }),
                (None, chat) => MessageSender::Chat(MessageSenderChat {
                    chat_id: chat.unwrap_or_default(),
                }),
            };
            let stored: String = row.get(3)?;
            let text = crate::lock::open(self.key.as_ref(), &stored)
                .unwrap_or_else(|_| "[зашифровано]".to_owned());
            Ok(MsgItem {
                chat_id,
                id: row.get(0)?,
                sender,
                outgoing: false,
                rich: crate::app::rich::plain(&text),
                text,
                deleted: false,
                media: None,
                pending: false,
                failed: false,
                reply_to: None,
                date: row.get(4)?,
                edited: false,
                reactions: Vec::new(),
                forwarded: None,
                extra: None,
                sender_tag: String::new(),
            })
        })?
        .collect()
    }

    /// Upserts messages; an already deleted message keeps its flag. A
    /// message whose text is byte-for-byte the same as what was last
    /// written here is skipped: TDLib reports one message through several
    /// updates in a row (`NewChat`'s `last_message`, then
    /// `ChatLastMessage`, then `NewMessage`), and a history reload re-saves
    /// what is already archived unchanged. Without this, each of those
    /// opens its own transaction and re-encrypts the same text with a
    /// fresh nonce for nothing.
    pub fn save<'a>(
        &mut self,
        messages: impl IntoIterator<Item = &'a MsgItem>,
    ) -> rusqlite::Result<()> {
        let to_write: Vec<&MsgItem> = messages
            .into_iter()
            .filter(|m| self.dedup.get(&(m.chat_id, m.id)) != Some(&m.text))
            .collect();
        if to_write.is_empty() {
            return Ok(());
        }
        let tx = self.conn.transaction()?;
        {
            let mut stmt = tx.prepare_cached(
                "INSERT INTO messages (chat_id, id, sender_user, sender_chat, outgoing, text, date)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT (chat_id, id) DO UPDATE SET text = excluded.text,
                     date = CASE WHEN date = 0 THEN excluded.date ELSE date END",
            )?;
            for m in &to_write {
                let (user, chat) = match &m.sender {
                    MessageSender::User(u) => (Some(u.user_id), None),
                    MessageSender::Chat(c) => (None, Some(c.chat_id)),
                };
                let text = crate::lock::store(self.key.as_ref(), &m.text)
                    .map_err(|e| rusqlite::Error::ToSqlConversionFailure(e.into()))?;
                stmt.execute(params![
                    m.chat_id, m.id, user, chat, m.outgoing, text, m.date
                ])?;
            }
        }
        tx.commit()?;
        for m in to_write {
            self.remember(m.chat_id, m.id, m.text.clone());
        }
        Ok(())
    }

    /// Remembers the plaintext just written for a message, so the next
    /// identical `save` of it is skipped. Evicts the oldest entry beyond
    /// `DEDUP_CAP`, so a long session does not grow this forever.
    fn remember(&mut self, chat_id: i64, id: i64, text: String) {
        let key = (chat_id, id);
        if self.dedup.insert(key, text).is_none() {
            self.dedup_order.push_back(key);
            if self.dedup_order.len() > DEDUP_CAP
                && let Some(old) = self.dedup_order.pop_front()
            {
                self.dedup.remove(&old);
            }
        }
    }

    /// Drops a message from the dedup cache: after it, `save` must not
    /// skip the row as unchanged, since it no longer holds that text (or
    /// does not exist at all any more).
    fn forget_dedup(&mut self, chat_id: i64, id: i64) {
        let key = (chat_id, id);
        self.dedup.remove(&key);
        self.dedup_order.retain(|&k| k != key);
    }

    /// Keeps the archived text in sync with edits.
    pub fn set_text(&mut self, chat_id: i64, id: i64, text: &str) -> rusqlite::Result<()> {
        let stored = self.stored(text)?;
        self.conn
            .prepare_cached("UPDATE messages SET text = ?3 WHERE chat_id = ?1 AND id = ?2")?
            .execute(params![chat_id, id, stored])?;
        self.remember(chat_id, id, text.to_owned());
        Ok(())
    }

    pub fn mark_deleted(&mut self, chat_id: i64, ids: &[i64]) -> rusqlite::Result<()> {
        let tx = self.conn.transaction()?;
        {
            let mut stmt = tx
                .prepare_cached("UPDATE messages SET deleted = 1 WHERE chat_id = ?1 AND id = ?2")?;
            for id in ids {
                stmt.execute(params![chat_id, id])?;
            }
        }
        tx.commit()
    }

    /// Forgets messages entirely: the user deleted them on purpose from this
    /// client, or removed an archived copy.
    pub fn purge(&mut self, chat_id: i64, ids: &[i64]) -> rusqlite::Result<()> {
        let tx = self.conn.transaction()?;
        {
            let mut stmt =
                tx.prepare_cached("DELETE FROM messages WHERE chat_id = ?1 AND id = ?2")?;
            for id in ids {
                stmt.execute(params![chat_id, id])?;
            }
        }
        tx.commit()?;
        // A purged row no longer exists: `save` must not skip it as an
        // unchanged resave were it to reappear (e.g. a history reload
        // racing the purge).
        for &id in ids {
            self.forget_dedup(chat_id, id);
        }
        // The deleted text must not survive in freed pages or WAL frames:
        // the point of purging is that it is gone for good.
        self.conn
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE); VACUUM;")
    }

    /// Deleted messages of the chat with `min_id <= id < max_id`.
    pub fn deleted(
        &self,
        chat_id: i64,
        min_id: i64,
        max_id: i64,
    ) -> rusqlite::Result<Vec<MsgItem>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT id, sender_user, sender_chat, outgoing, text, date FROM messages
             WHERE chat_id = ?1 AND id >= ?2 AND id < ?3 AND deleted = 1",
        )?;
        stmt.query_map(params![chat_id, min_id, max_id], |row| {
            let user: Option<i64> = row.get(1)?;
            let chat: Option<i64> = row.get(2)?;
            let sender = match (user, chat) {
                (Some(user_id), _) => MessageSender::User(MessageSenderUser { user_id }),
                (None, chat) => MessageSender::Chat(MessageSenderChat {
                    chat_id: chat.unwrap_or_default(),
                }),
            };
            let stored: String = row.get(4)?;
            // An unreadable row (wrong or missing key) shows as a marker
            // instead of failing the whole page.
            let text = crate::lock::open(self.key.as_ref(), &stored)
                .unwrap_or_else(|_| "[зашифровано]".to_owned());
            Ok(MsgItem {
                chat_id,
                id: row.get(0)?,
                sender,
                outgoing: row.get(3)?,
                rich: crate::app::rich::plain(&text),
                text,
                deleted: true,
                media: None,
                pending: false,
                failed: false,
                reply_to: None,
                date: row.get(5)?,
                edited: false,
                reactions: Vec::new(),
                forwarded: None,
                extra: None,
                sender_tag: String::new(),
            })
        })?
        .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(id: i64, text: &str) -> MsgItem {
        MsgItem {
            chat_id: 1,
            id,
            sender: MessageSender::User(MessageSenderUser { user_id: 7 }),
            text: text.into(),
            outgoing: false,
            deleted: false,
            media: None,
            pending: false,
            failed: false,
            rich: Vec::new(),
            reply_to: None,
            date: 0,
            edited: false,
            reactions: Vec::new(),
            forwarded: None,
            extra: None,
            sender_tag: String::new(),
        }
    }

    fn archive() -> Archive {
        Archive::open(Connection::open_in_memory().unwrap()).unwrap()
    }

    #[test]
    fn deleted_message_keeps_last_text_and_flag_after_resave() {
        let mut a = archive();
        a.save(&[msg(10, "old"), msg(11, "kept")]).unwrap();
        a.set_text(1, 10, "edited").unwrap();
        a.mark_deleted(1, &[10]).unwrap();
        // History reload re-saves surviving messages; must not undelete.
        a.save(&[msg(10, "edited"), msg(11, "kept")]).unwrap();

        let deleted = a.deleted(1, i64::MIN, i64::MAX).unwrap();
        assert_eq!(deleted.len(), 1);
        assert_eq!((deleted[0].id, deleted[0].text.as_str()), (10, "edited"));
        assert!(deleted[0].deleted);
        assert!(matches!(&deleted[0].sender, MessageSender::User(u) if u.user_id == 7));
    }

    #[test]
    fn deleted_respects_range_and_chat() {
        let mut a = archive();
        a.save(&[msg(5, "a"), msg(20, "b"), msg(30, "c")]).unwrap();
        a.mark_deleted(1, &[5, 20, 30]).unwrap();
        a.mark_deleted(2, &[20]).unwrap();

        let ids: Vec<i64> = a.deleted(1, 10, 30).unwrap().iter().map(|m| m.id).collect();
        assert_eq!(ids, [20]);
        assert!(a.deleted(2, i64::MIN, i64::MAX).unwrap().is_empty());
    }

    #[test]
    fn texts_are_encrypted_with_the_password_key_and_rekeyed() {
        let mut archive = archive();
        let mut m = msg(5, "секрет");
        m.deleted = true;
        archive.save([&msg(4, "открытый")]).unwrap();
        let key = crate::lock::derive("пароль", "соль-соль").unwrap();
        archive.rekey(Some(key)).unwrap();
        archive.save([&m]).unwrap();
        archive.mark_deleted(1, &[4, 5]).unwrap();
        let raw: Vec<String> = archive
            .conn
            .prepare("SELECT text FROM messages")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert!(raw.iter().all(|t| t.starts_with("enc:")), "{raw:?}");
        let texts = |a: &Archive| -> Vec<String> {
            a.deleted(1, 0, 10)
                .unwrap()
                .into_iter()
                .map(|m| m.text)
                .collect()
        };
        assert_eq!(texts(&archive), ["открытый", "секрет"]);

        // Without the key the rows are unreadable, not garbage.
        archive.set_key(None);
        assert_eq!(texts(&archive), ["[зашифровано]", "[зашифровано]"]);
        // Password removed: back to plain text.
        archive.set_key(Some(key));
        archive.rekey(None).unwrap();
        archive.set_key(None);
        assert_eq!(texts(&archive), ["открытый", "секрет"]);
    }

    #[test]
    fn plaintext_starting_with_the_cipher_marker_survives_enabling_a_password() {
        let mut archive = archive();
        archive
            .save(&[msg(4, "enc:это выглядит как шифротекст, но нет")])
            .unwrap();
        archive.mark_deleted(1, &[4]).unwrap();
        // Before the fix this failed with "архив зашифрован, нужен пароль",
        // leaving TDLib re-keyed while the archive (and so the password)
        // never got enabled.
        let key = crate::lock::derive("пароль", "соль-соль").unwrap();
        archive.rekey(Some(key)).unwrap();
        let text = archive.deleted(1, 0, 10).unwrap()[0].text.clone();
        assert_eq!(text, "enc:это выглядит как шифротекст, но нет");
    }

    #[test]
    fn own_pins_are_kept_per_chat_newest_first_and_follow_the_key() {
        let mut archive = archive();
        let mut a = msg(4, "первое");
        let mut b = msg(5, "второе");
        a.date = 10;
        b.date = 20;
        archive.pin_local(&a, 100).unwrap();
        archive.pin_local(&b, 200).unwrap();
        let ids =
            |ar: &Archive| -> Vec<i64> { ar.local_pins(1).unwrap().iter().map(|m| m.id).collect() };
        assert_eq!(ids(&archive), [5, 4], "the last pinned comes first");
        assert!(archive.local_pins(2).unwrap().is_empty());

        let key = crate::lock::derive("пароль", "соль-соль").unwrap();
        archive.rekey(Some(key)).unwrap();
        assert_eq!(archive.local_pins(1).unwrap()[1].text, "первое");
        archive.set_key(None);
        assert_eq!(archive.local_pins(1).unwrap()[1].text, "[зашифровано]");
        archive.set_key(Some(key));

        archive.unpin_local(1, 5).unwrap();
        assert_eq!(ids(&archive), [4]);
    }

    #[test]
    fn chat_pins_survive_reopen_without_losing_legacy_message_rows_or_other_accounts() {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let number = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let base =
            std::env::temp_dir().join(format!("telega-chat-pins-{}-{number}", std::process::id()));
        let first = base.with_extension("first.sqlite");
        let second = base.with_extension("second.sqlite");
        let old = Connection::open(&first).unwrap();
        old.execute_batch(
            "CREATE TABLE messages (chat_id INTEGER NOT NULL, id INTEGER NOT NULL,
             sender_user INTEGER, sender_chat INTEGER, outgoing INTEGER NOT NULL,
             text TEXT NOT NULL, deleted INTEGER NOT NULL DEFAULT 0,
             PRIMARY KEY (chat_id, id)) WITHOUT ROWID;
             INSERT INTO messages VALUES (1, 5, 7, NULL, 0, 'старое', 1);
             CREATE TABLE local_pins (chat_id INTEGER NOT NULL, id INTEGER NOT NULL,
             sender_user INTEGER, sender_chat INTEGER, text TEXT NOT NULL,
             date INTEGER NOT NULL, pinned_at INTEGER NOT NULL,
             PRIMARY KEY (chat_id, id)) WITHOUT ROWID;
             INSERT INTO local_pins VALUES (1, 6, 7, NULL, 'закреп', 10, 100);",
        )
        .unwrap();
        let a = Archive::open(old).unwrap();
        a.set_local_chat_pin(false, 1, true).unwrap();
        a.set_local_chat_pin(false, 2, true).unwrap();
        a.set_local_chat_pin(true, 1, true).unwrap();
        a.set_local_chat_pin(false, 1, true).unwrap();
        drop(a);
        let a = Archive::open(Connection::open(&first).unwrap()).unwrap();
        assert_eq!(a.local_chat_pins(false).unwrap(), [1, 2]);
        assert_eq!(a.local_chat_pins(true).unwrap(), [1]);
        assert_eq!(a.deleted(1, 0, 10).unwrap()[0].text, "старое");
        assert_eq!(a.local_pins(1).unwrap()[0].text, "закреп");
        a.set_local_chat_pin(false, 1, false).unwrap();
        assert_eq!(a.local_chat_pins(false).unwrap(), [2]);
        let b = Archive::open(Connection::open(&second).unwrap()).unwrap();
        assert!(b.local_chat_pins(false).unwrap().is_empty());
        b.set_local_chat_pin(false, 3, true).unwrap();
        assert_eq!(a.local_chat_pins(false).unwrap(), [2]);
        drop((a, b));
        std::fs::remove_file(first).unwrap();
        std::fs::remove_file(second).unwrap();
    }

    #[test]
    fn failed_chat_pin_write_preserves_committed_order() {
        let a = archive();
        a.set_local_chat_pin(false, 1, true).unwrap();
        a.conn.execute_batch("PRAGMA query_only = ON;").unwrap();
        assert!(a.set_local_chat_pin(false, 2, true).is_err());
        assert!(a.set_local_chat_pin(false, 1, false).is_err());
        assert_eq!(a.local_chat_pins(false).unwrap(), [1]);
    }

    #[test]
    fn old_archive_gains_send_times() {
        // Schema of archives written before send times were kept.
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE messages (chat_id INTEGER NOT NULL, id INTEGER NOT NULL,
                 sender_user INTEGER, sender_chat INTEGER, outgoing INTEGER NOT NULL,
                 text TEXT NOT NULL, deleted INTEGER NOT NULL DEFAULT 0,
                 PRIMARY KEY (chat_id, id)) WITHOUT ROWID;
             INSERT INTO messages VALUES (1, 5, 7, NULL, 0, 'старое', 1);",
        )
        .unwrap();
        let mut archive = Archive::open(conn).unwrap();
        assert_eq!(archive.deleted(1, 0, 10).unwrap()[0].date, 0);
        // Seen again: the missing time is filled in, a known one is kept.
        let mut seen = msg(5, "старое");
        seen.date = 1_700_000_000;
        archive.save([&seen]).unwrap();
        seen.date = 1_800_000_000;
        archive.save([&seen]).unwrap();
        assert_eq!(archive.deleted(1, 0, 10).unwrap()[0].date, 1_700_000_000);
    }

    #[test]
    fn identical_resave_is_skipped_but_a_change_or_purge_still_writes() {
        let mut a = archive();
        let key = crate::lock::derive("пароль", "соль-соль").unwrap();
        a.set_key(Some(key));
        a.save(&[msg(1, "hello")]).unwrap();
        let raw = |a: &Archive| -> String {
            a.conn
                .query_row(
                    "SELECT text FROM messages WHERE chat_id = 1 AND id = 1",
                    [],
                    |r| r.get(0),
                )
                .unwrap()
        };
        let first = raw(&a);

        // TDLib reports the very same message through several updates in a
        // row (`NewChat`'s `last_message`, then `ChatLastMessage`, then
        // `NewMessage`); re-saving it unchanged must not re-encrypt and
        // rewrite the row (a fresh nonce would change the stored bytes
        // even though nothing did).
        a.save(&[msg(1, "hello")]).unwrap();
        assert_eq!(raw(&a), first, "identical resave must be skipped");

        // An actual edit is still written.
        a.save(&[msg(1, "world")]).unwrap();
        assert_ne!(raw(&a), first, "changed text must be written");

        // Purging drops the dedup entry: a message reappearing afterward
        // with the same text it had before purging must still be written
        // back, not skipped as an unchanged resave of a row that is gone.
        a.purge(1, &[1]).unwrap();
        let count = |a: &Archive| -> i64 {
            a.conn
                .query_row(
                    "SELECT COUNT(*) FROM messages WHERE chat_id = 1 AND id = 1",
                    [],
                    |r| r.get(0),
                )
                .unwrap()
        };
        assert_eq!(count(&a), 0, "purged row is gone");
        a.save(&[msg(1, "world")]).unwrap();
        assert_eq!(count(&a), 1, "resave after purge must reinsert the row");
    }
}
