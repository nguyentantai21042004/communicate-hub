//! Chats and messages in scope, plus hourly counts for chats out of scope.

use captain_core::{ChatId, ChatKind, ChatSummary, MessageId, MessageView};
use captain_inbox::{ChatEntry, EntryKind, ThreadKey};
use rusqlite::{OptionalExtension, Row, params};
use serde::Serialize;

use crate::{Result, Store};

const SECS_PER_HOUR: i64 = 3600;
const REPLY_CHAIN_MAX_HOPS: usize = 50; // guards against cycles in corrupt data
const CHANNEL_ID_BOUND: i64 = -1_000_000_000_000; // marked channel ids are below this

/// A stored message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StoredMessage {
    /// Chat.
    pub chat: ChatId,
    /// Id.
    pub id: MessageId,
    /// Forum topic.
    pub topic: Option<MessageId>,
    /// Sender.
    pub sender: Option<ChatId>,
    /// Sender display name, from the chat cache.
    pub sender_name: Option<String>,
    /// Sent by the account.
    pub outgoing: bool,
    /// Send time.
    pub date: i64,
    /// Message it replies to.
    pub reply_to: Option<MessageId>,
    /// Mentions the account.
    pub mentioned: bool,
    /// Text, until retention deletes it.
    pub text: Option<String>,
    /// Raw media type, when it has media.
    pub media_kind: Option<String>,
    /// When it was deleted on Telegram.
    pub deleted_at: Option<i64>,
}

pub(crate) fn kind_str(kind: ChatKind) -> &'static str {
    match kind {
        ChatKind::User => "user",
        ChatKind::Group => "group",
        ChatKind::Channel => "channel",
    }
}

pub(crate) fn entry_kind_str(kind: EntryKind) -> &'static str {
    match kind {
        EntryKind::Dm => "dm",
        EntryKind::Group => "group",
        EntryKind::Forum => "forum",
    }
}

pub(crate) fn parse_entry_kind(s: &str) -> Option<EntryKind> {
    match s {
        "dm" => Some(EntryKind::Dm),
        "group" => Some(EntryKind::Group),
        "forum" => Some(EntryKind::Forum),
        _ => None,
    }
}

fn opt_msg(v: Option<i32>) -> Option<MessageId> {
    v.map(MessageId)
}

impl StoredMessage {
    fn from_row(r: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            chat: ChatId(r.get("chat_id")?),
            id: MessageId(r.get("msg_id")?),
            topic: opt_msg(r.get("topic_id")?),
            sender: r.get::<_, Option<i64>>("sender_id")?.map(ChatId),
            sender_name: r.get("sender_name")?,
            outgoing: r.get("outgoing")?,
            date: r.get("date")?,
            reply_to: opt_msg(r.get("reply_to_msg_id")?),
            mentioned: r.get("mentioned")?,
            text: r.get("text")?,
            media_kind: r.get("media_kind")?,
            deleted_at: r.get("deleted_at")?,
        })
    }
}

const MESSAGE_COLUMNS: &str =
    "m.chat_id, m.msg_id, m.topic_id, m.sender_id, s.title AS sender_name,
    m.outgoing, m.date, m.reply_to_msg_id, m.mentioned, m.text, m.media_kind, m.deleted_at";

impl Store {
    /// Records a chat or user the account has seen, with its scope entry when it has one.
    /// Without an entry, as for a group message's sender, the stored scope fields are kept.
    ///
    /// # Errors
    ///
    /// [`crate::Error::Sqlite`] on a write failure.
    pub fn upsert_chat(
        &self,
        chat: &ChatSummary,
        entry: Option<&ChatEntry>,
        now: i64,
    ) -> Result<()> {
        // A DM partner who also writes in a group is seen as a sender there, without an entry.
        // Overwriting would erase the DM chat's label and profile.
        self.conn.execute(
            "INSERT INTO chats (chat_id, kind, title, username, scope_label, scope_kind, profile,
                                first_seen, last_seen)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)
            ON CONFLICT (chat_id) DO UPDATE SET
                kind = excluded.kind,
                title = coalesce(excluded.title, title),
                username = coalesce(excluded.username, username),
                 scope_label = CASE WHEN ?9 THEN excluded.scope_label ELSE scope_label END,
                 scope_kind = CASE WHEN ?9 THEN excluded.scope_kind ELSE scope_kind END,
                 profile = CASE WHEN ?9 THEN excluded.profile ELSE profile END,
                last_seen = excluded.last_seen",
            params![
                chat.id.0,
                kind_str(chat.kind),
                chat.name,
                chat.username,
                entry.map(|e| e.label.as_str()),
                entry.map(|e| entry_kind_str(e.kind)),
                entry.map(|e| e.profile.as_str()),
                now,
                entry.is_some(),
            ],
        )?;
        Ok(())
    }

    /// Stores a message from a scoped chat. `live` is false for messages delivered by catch-up.
    /// Storing the same message again replaces it.
    ///
    /// # Errors
    ///
    /// [`crate::Error::Sqlite`] on a write failure.
    pub fn put_message(&self, m: &MessageView, received_at: i64, live: bool) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO messages (chat_id, msg_id, topic_id, sender_id, sender_is_bot,
                 outgoing, date, received_at, live, edit_date, reply_to_msg_id, quote_text,
                 mentioned, album_id, text, media_kind)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
            params![
                m.chat.id.0,
                m.id.0,
                m.topic.map(|t| t.0),
                m.sender.as_ref().map(|s| s.id.0),
                m.sender.as_ref().is_some_and(|s| s.is_bot),
                m.outgoing,
                m.date_unix,
                received_at,
                live,
                m.edited_at_unix,
                m.reply_to.map(|r| r.0),
                m.quote_text,
                m.mentioned,
                m.album_id,
                m.text.text,
                m.media.as_ref().map(|x| x.raw_type.as_str()),
            ],
        )?;
        Ok(())
    }

    /// Applies an edit to a stored message. Returns false when the message is not stored.
    ///
    /// # Errors
    ///
    /// [`crate::Error::Sqlite`] on a write failure.
    pub fn edit_message(&self, m: &MessageView) -> Result<bool> {
        let n = self.conn.execute(
            "UPDATE messages SET text = ?3, edit_date = ?4, mentioned = ?5, media_kind = ?6
             WHERE chat_id = ?1 AND msg_id = ?2 AND deleted_at IS NULL",
            params![
                m.chat.id.0,
                m.id.0,
                m.text.text,
                m.edited_at_unix,
                m.mentioned,
                m.media.as_ref().map(|x| x.raw_type.as_str()),
            ],
        )?;
        Ok(n > 0)
    }

    /// Marks messages deleted and clears their text. With `chat` unknown, as for private chats
    /// and basic groups, the ids are matched in non-channel chats, where ids are unique per
    /// account. Returns how many stored messages matched.
    ///
    /// # Errors
    ///
    /// [`crate::Error::Sqlite`] on a write failure.
    pub fn delete_messages(
        &self,
        chat: Option<ChatId>,
        ids: &[MessageId],
        now: i64,
    ) -> Result<usize> {
        let tx = self.conn.unchecked_transaction()?;
        let mut n = 0;
        for id in ids {
            n += match chat {
                Some(chat) => tx.execute(
                    "UPDATE messages SET deleted_at = ?3, text = NULL
                     WHERE chat_id = ?1 AND msg_id = ?2",
                    params![chat.0, id.0, now],
                )?,
                // TELEGRAM: updateDeleteMessages carries no peer; channel ids (-100...) have
                // their own counters and arrive as updateDeleteChannelMessages instead.
                None => tx.execute(
                    "UPDATE messages SET deleted_at = ?2, text = NULL
                     WHERE msg_id = ?1 AND chat_id > ?3",
                    params![id.0, now, CHANNEL_ID_BOUND],
                )?,
            };
        }
        tx.commit()?;
        Ok(n)
    }

    /// One stored message.
    ///
    /// # Errors
    ///
    /// [`crate::Error::Sqlite`] on a read failure.
    pub fn message(&self, chat: ChatId, id: MessageId) -> Result<Option<StoredMessage>> {
        Ok(self
            .conn
            .query_row(
                &format!(
                    "SELECT {MESSAGE_COLUMNS} FROM messages m
                     LEFT JOIN chats s ON s.chat_id = m.sender_id
                     WHERE m.chat_id = ?1 AND m.msg_id = ?2"
                ),
                params![chat.0, id.0],
                StoredMessage::from_row,
            )
            .optional()?)
    }

    /// Messages of a ticket, in id order.
    ///
    /// # Errors
    ///
    /// [`crate::Error::Sqlite`] on a read failure.
    pub fn ticket_message_bodies(&self, ticket: i64) -> Result<Vec<StoredMessage>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {MESSAGE_COLUMNS} FROM ticket_messages t
             JOIN messages m ON m.chat_id = t.chat_id AND m.msg_id = t.msg_id
             LEFT JOIN chats s ON s.chat_id = m.sender_id
             WHERE t.ticket_id = ?1 ORDER BY m.msg_id"
        ))?;
        let rows = stmt.query_map([ticket], StoredMessage::from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Up to `limit` non-deleted messages on `key` at or before `up_to`, oldest first.
    ///
    /// # Errors
    ///
    /// [`crate::Error::Sqlite`] on a read failure.
    pub fn recent_thread_messages(
        &self,
        key: ThreadKey,
        up_to: MessageId,
        limit: usize,
    ) -> Result<Vec<StoredMessage>> {
        let cap = i64::try_from(limit).unwrap_or(i64::MAX);
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {MESSAGE_COLUMNS} FROM messages m
             LEFT JOIN chats s ON s.chat_id = m.sender_id
             WHERE m.chat_id = ?1
               AND ((?2 IS NULL AND m.topic_id IS NULL) OR m.topic_id = ?2)
               AND m.msg_id <= ?3
               AND m.deleted_at IS NULL
             ORDER BY m.msg_id DESC
             LIMIT ?4"
        ))?;
        let topic = key.topic.map(|t| t.0);
        let rows = stmt.query_map(
            params![key.chat.0, topic, up_to.0, cap],
            StoredMessage::from_row,
        )?;
        let mut out: Vec<StoredMessage> = rows.collect::<rusqlite::Result<_>>()?;
        out.reverse();
        Ok(out)
    }

    /// Title of a chat the store has seen.
    ///
    /// # Errors
    ///
    /// [`crate::Error::Sqlite`] on a read failure.
    pub fn chat_title(&self, chat: ChatId) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT coalesce(scope_label, title) FROM chats WHERE chat_id = ?1",
                [chat.0],
                |r| r.get(0),
            )
            .optional()?
            .flatten())
    }

    /// Whether a stored message was sent by the account, or `None` if it is not stored.
    ///
    /// # Errors
    ///
    /// [`crate::Error::Sqlite`] on a read failure.
    pub fn is_outgoing(&self, chat: ChatId, id: MessageId) -> Result<Option<bool>> {
        Ok(self
            .conn
            .query_row(
                "SELECT outgoing FROM messages WHERE chat_id = ?1 AND msg_id = ?2",
                params![chat.0, id.0],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// Counts one message from an out-of-scope chat in its hour. No text is kept.
    ///
    /// # Errors
    ///
    /// [`crate::Error::Sqlite`] on a write failure.
    pub fn count_gate_drop(&self, chat: &ChatSummary, would_trigger: bool, now: i64) -> Result<()> {
        self.conn.execute(
            "INSERT INTO gate_drops (hour, chat_id, title, messages, would_trigger)
             VALUES (?1, ?2, ?3, 1, ?4)
             ON CONFLICT (hour, chat_id) DO UPDATE SET
                 messages = messages + 1,
                 would_trigger = would_trigger + excluded.would_trigger,
                 title = coalesce(excluded.title, title)",
            params![
                now.div_euclid(SECS_PER_HOUR),
                chat.id.0,
                chat.name,
                i64::from(would_trigger)
            ],
        )?;
        Ok(())
    }

    /// Walks the reply chain up from a message through stored messages. Returns the number of
    /// hops and the send time of the oldest message reached.
    ///
    /// # Errors
    ///
    /// [`crate::Error::Sqlite`] on a read failure.
    pub fn reply_chain(&self, chat: ChatId, from: MessageId) -> Result<(usize, Option<i64>)> {
        let mut hops = 0;
        let mut oldest = None;
        let mut current = self.message(chat, from)?;
        while let Some(m) = current {
            oldest = Some(oldest.map_or(m.date, |o: i64| o.min(m.date)));
            let Some(parent) = m.reply_to else { break };
            if hops == REPLY_CHAIN_MAX_HOPS {
                break;
            }
            current = self.message(chat, parent)?;
            if current.is_some() {
                hops += 1;
            }
        }
        Ok((hops, oldest))
    }

    /// Stored messages on `key` sent in `since..until`, excluding `until` itself.
    ///
    /// # Errors
    ///
    /// [`crate::Error::Sqlite`] on a read failure.
    pub fn count_on_key(&self, key: ThreadKey, since: i64, until: i64) -> Result<i64> {
        Ok(self.conn.query_row(
            "SELECT count(*) FROM messages
             WHERE chat_id = ?1 AND topic_id IS ?2 AND date >= ?3 AND date < ?4",
            params![key.chat.0, key.topic.map(|t| t.0), since, until],
            |r| r.get(0),
        )?)
    }

    /// Send time of the account's first message on `key` in `after..=until`.
    ///
    /// # Errors
    ///
    /// [`crate::Error::Sqlite`] on a read failure.
    pub fn first_outgoing_on_key(
        &self,
        key: ThreadKey,
        after: i64,
        until: i64,
    ) -> Result<Option<i64>> {
        Ok(self.conn.query_row(
            "SELECT min(date) FROM messages
             WHERE chat_id = ?1 AND topic_id IS ?2 AND outgoing = 1 AND date > ?3 AND date <= ?4",
            params![key.chat.0, key.topic.map(|t| t.0), after, until],
            |r| r.get(0),
        )?)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use captain_core::FormattedText;

    #[test]
    fn seeing_a_dm_partner_as_a_sender_keeps_the_dm_scope() {
        let store = Store::open_in_memory().unwrap();
        let partner = chat(42, ChatKind::User);
        let entry = ChatEntry {
            id: partner.id,
            label: "partner".into(),
            kind: EntryKind::Dm,
            topics: None,
            triggers: Vec::new(),
            keywords: Vec::new(),
            profile: "dm".into(),
            mute_until_unix: None,
        };
        store.upsert_chat(&partner, Some(&entry), 10).unwrap();
        store.upsert_chat(&partner, None, 20).unwrap();
        let (label, profile, last_seen): (Option<String>, Option<String>, i64) = store
            .conn
            .query_row(
                "SELECT scope_label, profile, last_seen FROM chats WHERE chat_id = 42",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(label.as_deref(), Some("partner"));
        assert_eq!(profile.as_deref(), Some("dm"));
        assert_eq!(last_seen, 20);
    }

    pub(crate) fn chat(id: i64, kind: ChatKind) -> ChatSummary {
        ChatSummary {
            id: ChatId(id),
            kind,
            name: Some(format!("chat {id}")),
            username: None,
            is_bot: false,
        }
    }

    pub(crate) fn msg(chat_id: i64, id: i32, date: i64, outgoing: bool) -> MessageView {
        MessageView {
            chat: chat(chat_id, ChatKind::Channel),
            id: MessageId(id),
            sender: Some(chat(7, ChatKind::User)),
            outgoing,
            date_unix: date,
            text: FormattedText::plain(format!("text {id}")),
            media: None,
            reply_to: None,
            album_id: None,
            mentioned: false,
            topic: None,
            quote_text: None,
            forward: None,
            edited_at_unix: None,
        }
    }

    #[test]
    fn stores_edits_and_deletes() {
        let store = Store::open_in_memory().unwrap();
        let mut m = msg(-1_000_000_000_005, 1, 10, false);
        store.put_message(&m, 11, true).unwrap();
        m.text = FormattedText::plain("edited");
        assert!(store.edit_message(&m).unwrap());
        let got = store
            .message(ChatId(-1_000_000_000_005), MessageId(1))
            .unwrap()
            .unwrap();
        assert_eq!(got.text.as_deref(), Some("edited"));

        let dm = MessageView {
            chat: chat(55, ChatKind::User),
            ..msg(55, 9, 10, true)
        };
        store.put_message(&dm, 11, true).unwrap();
        assert_eq!(
            store
                .delete_messages(None, &[MessageId(9), MessageId(1)], 20)
                .unwrap(),
            1
        );
        let gone = store.message(ChatId(55), MessageId(9)).unwrap().unwrap();
        assert_eq!((gone.deleted_at, gone.text), (Some(20), None));
        assert_eq!(
            store.is_outgoing(ChatId(55), MessageId(9)).unwrap(),
            Some(true)
        );
        assert_eq!(store.is_outgoing(ChatId(55), MessageId(8)).unwrap(), None);
    }

    #[test]
    fn walks_reply_chains_and_counts_threads() {
        let store = Store::open_in_memory().unwrap();
        let c = -1006;
        for (id, date, parent) in [(1, 100, None), (2, 200, Some(1)), (3, 300, Some(2))] {
            let mut m = msg(c, id, date, false);
            m.reply_to = parent.map(MessageId);
            store.put_message(&m, date, true).unwrap();
        }
        assert_eq!(
            store.reply_chain(ChatId(c), MessageId(3)).unwrap(),
            (2, Some(100))
        );
        let key = ThreadKey {
            chat: ChatId(c),
            topic: None,
        };
        assert_eq!(store.count_on_key(key, 0, 300).unwrap(), 2);
        store.put_message(&msg(c, 4, 400, true), 400, true).unwrap();
        assert_eq!(
            store.first_outgoing_on_key(key, 300, 1000).unwrap(),
            Some(400)
        );
        assert_eq!(store.first_outgoing_on_key(key, 400, 1000).unwrap(), None);
    }

    #[test]
    fn gate_drops_accumulate_per_hour() {
        let store = Store::open_in_memory().unwrap();
        let c = chat(-9, ChatKind::Group);
        store.count_gate_drop(&c, true, 10).unwrap();
        store.count_gate_drop(&c, false, 20).unwrap();
        let (messages, triggers): (i64, i64) = store
            .conn
            .query_row("SELECT messages, would_trigger FROM gate_drops", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!((messages, triggers), (2, 1));
    }
}
