//! Inserts and updates. Every statement that writes lives in this file or in [`crate::schema`].

use rusqlite::{Connection, OptionalExtension, params};

use crate::limits::{
    BRIDGE_SUPERGROUP_KEY, EVENT_DETAIL_MAX_CHARS, FAILURE_REASON_MAX_CHARS, MESSAGE_MAP_MAX_ROWS,
    SECS_PER_DAY, SECS_PER_HOUR,
};
use crate::vocab::{self, require_token};
use crate::{ContentKind, ConversationId, Direction, Error, Hub, ModuleHealth, Result};

/// A conversation to insert or refresh.
///
/// [`ConversationId`] already carries the network, kind and external id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewConversation {
    /// Network, kind and the network's own id.
    pub id: ConversationId,
    /// Display title. Not message text.
    pub title: Option<String>,
    /// Telegram topic id when this row is one topic, otherwise `None`.
    pub topic_id: Option<i64>,
    /// Whether the hub should ignore new traffic for triage.
    pub muted: bool,
    /// Optional owner tags, stored as given.
    pub tags: Option<String>,
    /// When the caller saw it, Unix seconds. The row keeps the earlier `first_seen`.
    pub seen_at_unix_secs: i64,
}

/// A bridge or module log line. `detail` is truncated before it is stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewLogEvent {
    /// When it happened, Unix seconds.
    pub at_unix_secs: i64,
    /// Module name.
    pub module: String,
    /// Severity.
    pub level: crate::LogLevel,
    /// Short machine tag, for example `send_failed`.
    pub kind: String,
    /// Optional note. Truncated to [`EVENT_DETAIL_MAX_CHARS`].
    pub detail: Option<String>,
}

/// A delivery that did not reach Telegram.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewFailure {
    /// Conversation involved, when the failure is about one.
    pub conversation_id: Option<i64>,
    /// Why it failed. Truncated to [`FAILURE_REASON_MAX_CHARS`].
    pub reason: String,
    /// Whether the bridge should try again.
    pub retryable: bool,
    /// When it failed, Unix seconds.
    pub created_at_unix_secs: i64,
}

/// One observed message reduced to the counter columns.
///
/// There is no text field. Bridge message ids live in `message_map`, not here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CountedMessage {
    /// Conversation the message belongs to. It must already have been upserted.
    pub conversation: ConversationId,
    /// Send time, Unix seconds. The counter uses the UTC day and hour of this instant.
    pub at_unix_secs: i64,
    /// In or out.
    pub direction: Direction,
    /// Payload kind. The bytes themselves are not stored.
    pub content_kind: ContentKind,
}

/// How many event rows [`Hub::prune_events`] deleted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PrunedEvents {
    /// Rows removed from `bridge_events`.
    pub bridge: u64,
    /// Rows removed from `module_events`.
    pub module: u64,
}

impl Hub {
    /// Inserts a conversation or refreshes its title, kind, topic, mute flag and tags.
    ///
    /// `first_seen` stays at the earliest `seen_at_unix_secs`. `last_activity` stays at the
    /// latest. `topic_id` replaces the stored topic, and `None` clears it. Returns the row id.
    ///
    /// The identity is `(network, kind, external_id)`, matching [`ConversationId`].
    ///
    /// # Errors
    ///
    /// [`Error::Empty`] when the external id is empty, [`Error::TopicInUse`] when `topic_id`
    /// is already owned by another conversation, and [`Error::Sqlite`] on a write failure.
    pub fn upsert_conversation(&mut self, conversation: &NewConversation) -> Result<i64> {
        require_token(&conversation.id.external_id, "conversation id")?;
        let tx = self.conn.transaction()?;
        let topic_id = conversation.topic_id;
        match tx.execute(
            "INSERT INTO conversations
                (network, kind, external_id, title, topic_id, muted, first_seen, last_activity, tags)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7, ?8)
             ON CONFLICT (network, kind, external_id) DO UPDATE SET
                title = excluded.title,
                topic_id = excluded.topic_id,
                muted = excluded.muted,
                tags = excluded.tags,
                first_seen = min(conversations.first_seen, excluded.first_seen),
                last_activity = max(conversations.last_activity, excluded.last_activity)",
            params![
                conversation.id.network.as_str(),
                conversation.id.kind.as_str(),
                conversation.id.external_id,
                conversation.title,
                topic_id,
                conversation.muted,
                conversation.seen_at_unix_secs,
                conversation.tags,
            ],
        ) {
            Ok(_) => {}
            Err(err) if is_constraint(&err) => {
                if let Some(topic_id) = topic_id {
                    return Err(Error::TopicInUse { topic_id });
                }
                return Err(err.into());
            }
            Err(err) => return Err(err.into()),
        }
        let id = conversation_row(&tx, &conversation.id)?;
        tx.commit()?;
        Ok(id)
    }

    /// Adds one to the UTC hour bucket for `message` and touches the conversation's activity.
    ///
    /// `message` has no text, and this function writes none. The conversation must already exist.
    ///
    /// # Errors
    ///
    /// [`Error::Empty`] when the external id is empty, [`Error::UnknownConversation`] when the
    /// conversation was not upserted, and [`Error::Sqlite`] on a write failure.
    pub fn record_message(&mut self, message: &CountedMessage) -> Result<()> {
        require_token(&message.conversation.external_id, "conversation id")?;
        let day = message.at_unix_secs.div_euclid(SECS_PER_DAY);
        let hour = message
            .at_unix_secs
            .div_euclid(SECS_PER_HOUR)
            .rem_euclid(24);
        let tx = self.conn.transaction()?;
        let id = conversation_row(&tx, &message.conversation)?;
        tx.execute(
            "INSERT INTO message_counters (day, hour, conversation_id, direction, content_kind, count)
             VALUES (?1, ?2, ?3, ?4, ?5, 1)
             ON CONFLICT (day, hour, conversation_id, direction, content_kind)
             DO UPDATE SET count = count + 1",
            params![
                day,
                hour,
                id,
                message.direction.as_str(),
                message.content_kind.as_str(),
            ],
        )?;
        tx.execute(
            "UPDATE conversations
             SET first_seen = min(first_seen, ?2), last_activity = max(last_activity, ?2)
             WHERE id = ?1",
            params![id, message.at_unix_secs],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Points `conversation_id` at a Telegram forum topic and records that in `topic_map`.
    ///
    /// Also sets `conversations.topic_id`, which is what [`Hub::source_of_chat`] reads.
    ///
    /// # Errors
    ///
    /// [`Error::UnknownConversationId`] when the row does not exist, [`Error::TopicInUse`] when
    /// the topic belongs to another conversation, and [`Error::Sqlite`] on a write failure.
    pub fn map_topic(
        &mut self,
        conversation_id: i64,
        telegram_topic_id: i64,
        at_unix_secs: i64,
    ) -> Result<()> {
        let tx = self.conn.transaction()?;
        let updated = match tx.execute(
            "UPDATE conversations SET topic_id = ?2 WHERE id = ?1",
            params![conversation_id, telegram_topic_id],
        ) {
            Ok(n) => n,
            Err(err) if is_constraint(&err) => {
                return Err(Error::TopicInUse {
                    topic_id: telegram_topic_id,
                });
            }
            Err(err) => return Err(err.into()),
        };
        if updated == 0 {
            return Err(Error::UnknownConversationId {
                id: conversation_id,
            });
        }
        match tx.execute(
            "INSERT INTO topic_map (conversation_id, telegram_topic_id, created_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT (conversation_id) DO UPDATE SET
                telegram_topic_id = excluded.telegram_topic_id,
                created_at = excluded.created_at",
            params![conversation_id, telegram_topic_id, at_unix_secs],
        ) {
            Ok(_) => {}
            Err(err) if is_constraint(&err) => {
                return Err(Error::TopicInUse {
                    topic_id: telegram_topic_id,
                });
            }
            Err(err) => return Err(err.into()),
        }
        tx.commit()?;
        Ok(())
    }

    /// Remembers which Telegram message a network message became.
    ///
    /// Writing the same network id again updates the Telegram id and counts as newer. The table
    /// is then pruned to [`MESSAGE_MAP_MAX_ROWS`].
    ///
    /// # Errors
    ///
    /// [`Error::Empty`] when the network message id is empty, and [`Error::Sqlite`] on a write
    /// failure.
    pub fn map_message(
        &mut self,
        network: crate::Network,
        network_message_id: &str,
        telegram_message_id: i64,
        at_unix_secs: i64,
    ) -> Result<()> {
        require_token(network_message_id, "network message id")?;
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO message_map (network, network_message_id, telegram_message_id, created_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (network, network_message_id) DO UPDATE SET
                telegram_message_id = excluded.telegram_message_id,
                created_at = excluded.created_at",
            params![
                network.as_str(),
                network_message_id,
                telegram_message_id,
                at_unix_secs,
            ],
        )?;
        prune_message_map_tx(&tx, MESSAGE_MAP_MAX_ROWS)?;
        tx.commit()?;
        Ok(())
    }

    /// Deletes the oldest `message_map` rows past [`MESSAGE_MAP_MAX_ROWS`].
    ///
    /// Returns how many rows were deleted. [`Hub::map_message`] already calls this.
    ///
    /// # Errors
    ///
    /// [`Error::Sqlite`] on a write failure.
    pub fn prune_message_map(&mut self) -> Result<u64> {
        prune_message_map(&mut self.conn, MESSAGE_MAP_MAX_ROWS)
    }

    /// Appends a bridge log line.
    ///
    /// # Errors
    ///
    /// [`Error::Empty`] when `module` or `kind` is empty, and [`Error::Sqlite`] on a write
    /// failure.
    pub fn record_bridge_event(&mut self, event: &NewLogEvent) -> Result<i64> {
        insert_event(&mut self.conn, "bridge_events", event)
    }

    /// Appends a module log line.
    ///
    /// # Errors
    ///
    /// [`Error::Empty`] when `module` or `kind` is empty, and [`Error::Sqlite`] on a write
    /// failure.
    pub fn record_module_event(&mut self, event: &NewLogEvent) -> Result<i64> {
        insert_event(&mut self.conn, "module_events", event)
    }

    /// Deletes bridge and module events strictly older than `retention_days` before `now_unix_secs`.
    ///
    /// A row at exactly the cutoff stays.
    ///
    /// # Errors
    ///
    /// [`Error::Corrupt`] when `retention_days` is negative or the cutoff overflows, and
    /// [`Error::Sqlite`] on a write failure.
    pub fn prune_events(
        &mut self,
        now_unix_secs: i64,
        retention_days: i64,
    ) -> Result<PrunedEvents> {
        if retention_days < 0 {
            return Err(Error::Corrupt(format!("retention_days {retention_days}")));
        }
        let Some(window) = retention_days.checked_mul(SECS_PER_DAY) else {
            return Err(Error::Corrupt(format!("retention_days {retention_days}")));
        };
        let Some(cutoff) = now_unix_secs.checked_sub(window) else {
            return Err(Error::Corrupt(format!("prune time {now_unix_secs}")));
        };
        let tx = self.conn.transaction()?;
        let bridge = delete_before(&tx, "bridge_events", cutoff)?;
        let module = delete_before(&tx, "module_events", cutoff)?;
        tx.commit()?;
        Ok(PrunedEvents { bridge, module })
    }

    /// Records a failed delivery. Returns the new row id.
    ///
    /// # Errors
    ///
    /// [`Error::UnknownConversationId`] when `conversation_id` is set and missing, and
    /// [`Error::Sqlite`] on a write failure.
    pub fn record_failure(&mut self, failure: &NewFailure) -> Result<i64> {
        if let Some(id) = failure.conversation_id {
            let exists: bool = self
                .conn
                .query_row("SELECT 1 FROM conversations WHERE id = ?1", [id], |_| {
                    Ok(true)
                })
                .optional()?
                .unwrap_or(false);
            if !exists {
                return Err(Error::UnknownConversationId { id });
            }
        }
        self.conn.execute(
            "INSERT INTO failed_events (conversation_id, reason, retryable, created_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                failure.conversation_id,
                shorten(&failure.reason, FAILURE_REASON_MAX_CHARS),
                failure.retryable,
                failure.created_at_unix_secs,
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Marks a failure resolved at `at_unix_secs`. Returns false when the id does not exist.
    ///
    /// # Errors
    ///
    /// [`Error::Sqlite`] on a write failure.
    pub fn resolve_failure(&mut self, id: i64, at_unix_secs: i64) -> Result<bool> {
        let updated = self.conn.execute(
            "UPDATE failed_events SET resolved_at = ?2 WHERE id = ?1",
            params![id, at_unix_secs],
        )?;
        Ok(updated > 0)
    }

    /// Replaces the latest snapshot for `health.module`.
    ///
    /// # Errors
    ///
    /// [`Error::Empty`] when the module name is empty, and [`Error::Sqlite`] on a write failure.
    pub fn record_health(&mut self, health: &ModuleHealth) -> Result<()> {
        require_token(&health.module, "module")?;
        let detail = shorten(&health.detail, EVENT_DETAIL_MAX_CHARS);
        self.conn.execute(
            "INSERT INTO module_health (module, state, detail, updated_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (module) DO UPDATE SET
                state = excluded.state,
                detail = excluded.detail,
                updated_at = excluded.updated_at",
            params![
                health.module,
                vocab::module_state_name(health.state),
                detail,
                health.updated_at_unix_secs,
            ],
        )?;
        Ok(())
    }

    /// Stores the bridge supergroup chat id in Captain's `settings` table.
    ///
    /// [`Hub::source_of_chat`] reads this key. It is not a compiled-in constant.
    ///
    /// # Errors
    ///
    /// [`Error::Sqlite`] on a write failure.
    pub fn set_bridge_supergroup(&mut self, chat_id: i64) -> Result<()> {
        self.conn.execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2)
             ON CONFLICT (key) DO UPDATE SET value = excluded.value",
            params![BRIDGE_SUPERGROUP_KEY, chat_id.to_string()],
        )?;
        Ok(())
    }

    /// The bridge supergroup chat id, or `None` when it has not been set.
    ///
    /// # Errors
    ///
    /// [`Error::Sqlite`] on a read failure and [`Error::Corrupt`] when the stored value is not
    /// an integer.
    pub fn bridge_supergroup(&self) -> Result<Option<i64>> {
        bridge_supergroup(&self.conn)
    }

    /// Which network a Captain ticket on `chat_id` belongs to.
    ///
    /// Any chat other than the bridge supergroup is Telegram. On the bridge supergroup the
    /// source is the conversation whose `topic_id` equals `topic_id`.
    ///
    /// # Errors
    ///
    /// [`Error::BridgeTopicMissing`] when the bridge chat has no topic,
    /// [`Error::UnmappedTopic`] when no conversation owns it, [`Error::AmbiguousTopic`] when
    /// several do, and [`Error::Sqlite`] or [`Error::Corrupt`] on a bad settings value.
    pub fn source_of_chat(&self, chat_id: i64, topic_id: Option<i64>) -> Result<crate::Network> {
        source_of_chat(&self.conn, chat_id, topic_id)
    }
}

pub(crate) fn bridge_supergroup(conn: &Connection) -> Result<Option<i64>> {
    let value: Option<String> = conn
        .query_row(
            "SELECT value FROM settings WHERE key = ?1",
            [BRIDGE_SUPERGROUP_KEY],
            |row| row.get(0),
        )
        .optional()?;
    match value {
        None => Ok(None),
        Some(text) => text
            .parse()
            .map(Some)
            .map_err(|_| Error::Corrupt(format!("bridge supergroup id {text}"))),
    }
}

pub(crate) fn source_of_chat(
    conn: &Connection,
    chat_id: i64,
    topic_id: Option<i64>,
) -> Result<crate::Network> {
    let bridge = bridge_supergroup(conn)?;
    source_with_bridge(conn, bridge, chat_id, topic_id)
}

pub(crate) fn source_with_bridge(
    conn: &Connection,
    bridge: Option<i64>,
    chat_id: i64,
    topic_id: Option<i64>,
) -> Result<crate::Network> {
    if bridge != Some(chat_id) {
        return Ok(crate::Network::Telegram);
    }
    let Some(topic_id) = topic_id else {
        return Err(Error::BridgeTopicMissing);
    };
    let mut stmt = conn.prepare("SELECT network FROM conversations WHERE topic_id = ?1")?;
    let networks = stmt
        .query_map([topic_id], |row| row.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    match networks.as_slice() {
        [one] => vocab::parse_network(one),
        [] => Err(Error::UnmappedTopic { topic_id }),
        _ => Err(Error::AmbiguousTopic { topic_id }),
    }
}

pub(crate) fn conversation_row(conn: &Connection, id: &ConversationId) -> Result<i64> {
    conn.query_row(
        "SELECT id FROM conversations
         WHERE network = ?1 AND kind = ?2 AND external_id = ?3",
        params![id.network.as_str(), id.kind.as_str(), id.external_id],
        |row| row.get(0),
    )
    .optional()?
    .ok_or_else(|| Error::UnknownConversation {
        network: id.network.as_str().to_owned(),
        external_id: id.external_id.clone(),
    })
}

fn insert_event(conn: &mut Connection, table: &str, event: &NewLogEvent) -> Result<i64> {
    require_token(&event.module, "module")?;
    require_token(&event.kind, "event kind")?;
    let detail = event
        .detail
        .as_deref()
        .map(|text| shorten(text, EVENT_DETAIL_MAX_CHARS));
    // Table names are fixed by the two callers. They are not taken from input.
    let sql = if table == "module_events" {
        "INSERT INTO module_events (ts, module, level, kind, detail) VALUES (?1, ?2, ?3, ?4, ?5)"
    } else {
        "INSERT INTO bridge_events (ts, module, level, kind, detail) VALUES (?1, ?2, ?3, ?4, ?5)"
    };
    conn.execute(
        sql,
        params![
            event.at_unix_secs,
            event.module,
            event.level.as_str(),
            event.kind,
            detail,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

fn delete_before(tx: &rusqlite::Transaction<'_>, table: &str, cutoff: i64) -> Result<u64> {
    let sql = if table == "module_events" {
        "DELETE FROM module_events WHERE ts < ?1"
    } else {
        "DELETE FROM bridge_events WHERE ts < ?1"
    };
    let deleted = tx.execute(sql, [cutoff])?;
    u64::try_from(deleted).map_err(|_| Error::Corrupt(format!("deleted {deleted}")))
}

const PRUNE_MESSAGE_MAP: &str = "
DELETE FROM message_map WHERE id IN (
    SELECT id FROM message_map ORDER BY created_at ASC, id ASC LIMIT ?1
)";

pub(crate) fn prune_message_map(conn: &mut Connection, max_rows: i64) -> Result<u64> {
    let count: i64 = conn.query_row("SELECT count(*) FROM message_map", [], |row| row.get(0))?;
    delete_oldest(count, max_rows, |extra| {
        conn.execute(PRUNE_MESSAGE_MAP, [extra])
    })
}

fn prune_message_map_tx(tx: &rusqlite::Transaction<'_>, max_rows: i64) -> Result<u64> {
    let count: i64 = tx.query_row("SELECT count(*) FROM message_map", [], |row| row.get(0))?;
    delete_oldest(count, max_rows, |extra| {
        tx.execute(PRUNE_MESSAGE_MAP, [extra])
    })
}

fn delete_oldest(
    count: i64,
    max_rows: i64,
    delete: impl FnOnce(i64) -> rusqlite::Result<usize>,
) -> Result<u64> {
    let extra = count - max_rows;
    if extra <= 0 {
        return Ok(0);
    }
    let deleted = delete(extra)?;
    u64::try_from(deleted).map_err(|_| Error::Corrupt(format!("deleted {deleted}")))
}

fn shorten(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_owned();
    }
    value.chars().take(max_chars).collect()
}

fn is_constraint(err: &rusqlite::Error) -> bool {
    matches!(
        err,
        rusqlite::Error::SqliteFailure(code, _)
            if code.code == rusqlite::ErrorCode::ConstraintViolation
    )
}
