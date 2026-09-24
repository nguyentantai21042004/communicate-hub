//! Metric events: what the other tables cannot tell the report. Never message text.

use captain_core::{ChatId, MessageId};
use rusqlite::params;

use crate::{Result, Store};

/// Kinds of metric event. Their fields are described in `docs/design/metrics-p1.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetricKind {
    /// The account read a scoped chat: `a` = max read id, `b` = unread left.
    ReadAdvanced,
    /// Catch-up after start: `a` = messages delivered, `b` = seconds offline.
    CatchUp,
    /// `FLOOD_WAIT_X`: `a` = X, `tag` = method.
    FloodWait,
    /// Other RPC error: `a` = code, `tag` = method and error name.
    RpcError,
    /// A reply target was fetched from Telegram: `a` = its age, `tag` = `hit` or `miss`.
    ReplyTargetFetch,
    /// A ticket closed: `a` = reply chain hops, `b` = age of the oldest chain message.
    ContextShape,
    /// A ticket closed: `a` = messages on the key in the prior hour, `b` = prior day.
    ContextDensity,
    /// Daily: `a` = database bytes, `b` = message rows.
    StoreSize,
    /// The account wrote in a private chat, scoped or not. Chat id and time only.
    OutgoingDm,
}

impl MetricKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::ReadAdvanced => "read_advanced",
            Self::CatchUp => "catch_up",
            Self::FloodWait => "flood_wait",
            Self::RpcError => "rpc_error",
            Self::ReplyTargetFetch => "reply_target_fetch",
            Self::ContextShape => "context_shape",
            Self::ContextDensity => "context_density",
            Self::StoreSize => "store_size",
            Self::OutgoingDm => "outgoing_dm",
        }
    }
}

/// One metric event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetricEvent {
    /// When it happened.
    pub at: i64,
    /// What happened.
    pub kind: MetricKind,
    /// Chat involved.
    pub chat: Option<ChatId>,
    /// Topic involved.
    pub topic: Option<MessageId>,
    /// Ticket involved.
    pub ticket: Option<i64>,
    /// First value; meaning depends on `kind`.
    pub a: Option<i64>,
    /// Second value.
    pub b: Option<i64>,
    /// Short tag; never message text.
    pub tag: Option<String>,
}

impl MetricEvent {
    /// An event with only a kind and time; fill the rest with struct update syntax.
    pub fn new(kind: MetricKind, at: i64) -> Self {
        Self {
            at,
            kind,
            chat: None,
            topic: None,
            ticket: None,
            a: None,
            b: None,
            tag: None,
        }
    }
}

impl Store {
    /// Appends a metric event.
    ///
    /// # Errors
    ///
    /// [`crate::Error::Sqlite`] on a write failure.
    pub fn record(&self, e: &MetricEvent) -> Result<()> {
        self.conn.execute(
            "INSERT INTO metric_events (at, kind, chat_id, topic_id, ticket_id, value_a, value_b, tag)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                e.at,
                e.kind.as_str(),
                e.chat.map(|c| c.0),
                e.topic.map(|t| t.0),
                e.ticket,
                e.a,
                e.b,
                e.tag
            ],
        )?;
        Ok(())
    }

    /// Time of the first read event on `chat` in `after..=until` that covers `message`. A read
    /// event without a topic covers every topic.
    ///
    /// # Errors
    ///
    /// [`crate::Error::Sqlite`] on a read failure.
    pub fn first_read_covering(
        &self,
        chat: ChatId,
        topic: Option<MessageId>,
        message: MessageId,
        after: i64,
        until: i64,
    ) -> Result<Option<i64>> {
        Ok(self.conn.query_row(
            "SELECT min(at) FROM metric_events
             WHERE kind = 'read_advanced' AND chat_id = ?1 AND value_a >= ?2
               AND (topic_id IS NULL OR topic_id IS ?3) AND at >= ?4 AND at <= ?5",
            params![chat.0, message.0, topic.map(|t| t.0), after, until],
            |r| r.get(0),
        )?)
    }

    /// Whether the account wrote to any of `users` in private in `after..=until`.
    ///
    /// # Errors
    ///
    /// [`crate::Error::Sqlite`] on a read failure.
    pub fn wrote_to_any(&self, users: &[ChatId], after: i64, until: i64) -> Result<bool> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT 1 FROM metric_events
             WHERE kind = 'outgoing_dm' AND chat_id = ?1 AND at > ?2 AND at <= ?3 LIMIT 1",
        )?;
        for user in users {
            if stmt.exists(params![user.0, after, until])? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Number of stored messages, for the daily size sample.
    ///
    /// # Errors
    ///
    /// [`crate::Error::Sqlite`] on a read failure.
    pub fn message_count(&self) -> Result<i64> {
        Ok(self
            .conn
            .query_row("SELECT count(*) FROM messages", [], |r| r.get(0))?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_events_cover_by_id_and_topic() {
        let store = Store::open_in_memory().unwrap();
        let chat = ChatId(-1001);
        store
            .record(&MetricEvent {
                chat: Some(chat),
                topic: Some(MessageId(4)),
                a: Some(50),
                ..MetricEvent::new(MetricKind::ReadAdvanced, 100)
            })
            .unwrap();
        let read = |topic, id| {
            store
                .first_read_covering(chat, topic, MessageId(id), 0, 1000)
                .unwrap()
        };
        assert_eq!(read(Some(MessageId(4)), 50), Some(100));
        assert_eq!(read(Some(MessageId(4)), 51), None);
        assert_eq!(read(Some(MessageId(5)), 10), None);
    }

    #[test]
    fn private_writes_are_found_by_user() {
        let store = Store::open_in_memory().unwrap();
        store
            .record(&MetricEvent {
                chat: Some(ChatId(7)),
                ..MetricEvent::new(MetricKind::OutgoingDm, 50)
            })
            .unwrap();
        assert!(store.wrote_to_any(&[ChatId(8), ChatId(7)], 0, 100).unwrap());
        assert!(!store.wrote_to_any(&[ChatId(7)], 50, 100).unwrap());
    }
}
