//! Topic rows, message links, display names, and bridge retry stamps.
//!
//! `message_map` stays unique on `(network, network_message_id)`. [`Hub::map_message`] still
//! replaces that row. [`Hub::insert_message_link`] does not: the first link wins, and the row
//! records which conversation owns it. The same network message id cannot be stored for two
//! conversations.

use rusqlite::{OptionalExtension, params};

use crate::limits::MESSAGE_MAP_MAX_ROWS;
use crate::vocab::{self, require_token};
use crate::write::conversation_row;
use crate::{ConversationId, Hub, Result};

/// One forum topic stored for a conversation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredTopic {
    /// Source conversation.
    pub conversation: ConversationId,
    /// Bot API `message_thread_id`.
    pub topic_id: i64,
    /// Display title. Empty when the row has none.
    pub title: String,
}

/// A network message linked to one Telegram message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageLink {
    /// Conversation that owns the link, when the row recorded one.
    pub conversation: Option<ConversationId>,
    /// Network message id.
    pub network_message_id: String,
    /// Telegram message id.
    pub telegram_message_id: i64,
    /// When the link was written, Unix seconds.
    pub created_at_unix_secs: i64,
}

/// One failed delivery, with the conversation restored when the row has one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredFailure {
    /// Conversation involved, when the failure names one.
    pub conversation: Option<ConversationId>,
    /// Why it failed. Already clipped by [`Hub::record_failure`](crate::Hub::record_failure).
    pub reason: String,
    /// Whether a later attempt might succeed.
    pub retryable: bool,
    /// When it failed, Unix seconds.
    pub created_at_unix_secs: i64,
}

impl Hub {
    /// Inserts the conversation when it is missing, and updates the title when `title` is set.
    ///
    /// An existing topic and mute flag are left as they are. `None` does not clear the title.
    /// Returns the row id.
    ///
    /// # Errors
    ///
    /// [`crate::Error::Empty`] when the external id is empty, and [`crate::Error::Sqlite`] on a write failure.
    pub fn ensure_conversation(
        &mut self,
        id: &ConversationId,
        title: Option<&str>,
        seen_at_unix_secs: i64,
    ) -> Result<i64> {
        require_token(&id.external_id, "conversation id")?;
        self.conn.execute(
            "INSERT INTO conversations
                (network, kind, external_id, title, topic_id, muted, first_seen, last_activity, tags)
             VALUES (?1, ?2, ?3, ?4, NULL, 0, ?5, ?5, NULL)
             ON CONFLICT (network, kind, external_id) DO UPDATE SET
                title = COALESCE(excluded.title, conversations.title),
                first_seen = min(conversations.first_seen, excluded.first_seen),
                last_activity = max(conversations.last_activity, excluded.last_activity)",
            params![
                id.network.as_str(),
                id.kind.as_str(),
                id.external_id,
                title,
                seen_at_unix_secs,
            ],
        )?;
        conversation_row(&self.conn, id)
    }

    /// Display title stored for `id`. `None` means the conversation has no row or no title.
    ///
    /// # Errors
    ///
    /// [`crate::Error::Sqlite`] on a read failure.
    pub fn conversation_title(&self, id: &ConversationId) -> Result<Option<String>> {
        let found: Option<Option<String>> = self
            .conn
            .query_row(
                "SELECT title FROM conversations
                 WHERE network = ?1 AND kind = ?2 AND external_id = ?3",
                params![id.network.as_str(), id.kind.as_str(), id.external_id],
                |row| row.get(0),
            )
            .optional()?;
        Ok(found.flatten())
    }

    /// Whether `id` is marked muted. `None` means the conversation has no row.
    ///
    /// # Errors
    ///
    /// [`crate::Error::Sqlite`] on a read failure.
    pub fn conversation_muted(&self, id: &ConversationId) -> Result<Option<bool>> {
        let muted: Option<i64> = self
            .conn
            .query_row(
                "SELECT muted FROM conversations
                 WHERE network = ?1 AND kind = ?2 AND external_id = ?3",
                params![id.network.as_str(), id.kind.as_str(), id.external_id],
                |row| row.get(0),
            )
            .optional()?;
        Ok(muted.map(|value| value != 0))
    }

    /// Every topic row, lowest conversation id first.
    ///
    /// # Errors
    ///
    /// [`crate::Error::Sqlite`] on a read failure and [`crate::Error::Corrupt`] for an unknown network or kind.
    pub fn topic_rows(&self) -> Result<Vec<StoredTopic>> {
        let mut stmt = self.conn.prepare(
            "SELECT c.network, c.kind, c.external_id, c.title, t.telegram_topic_id
             FROM topic_map t
             JOIN conversations c ON c.id = t.conversation_id
             ORDER BY c.id",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, i64>(4)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (network, kind, external_id, title, topic_id) = row?;
            out.push(StoredTopic {
                conversation: conversation_from(&network, &kind, external_id)?,
                topic_id,
                title: title.unwrap_or_default(),
            });
        }
        Ok(out)
    }

    /// Topic stored for `id`, if the conversation has one.
    ///
    /// # Errors
    ///
    /// [`crate::Error::Sqlite`] on a read failure and [`crate::Error::Corrupt`] for an unknown enum.
    pub fn topic_row(&self, id: &ConversationId) -> Result<Option<StoredTopic>> {
        let found = self.conn.query_row(
            "SELECT c.title, t.telegram_topic_id
             FROM topic_map t
             JOIN conversations c ON c.id = t.conversation_id
             WHERE c.network = ?1 AND c.kind = ?2 AND c.external_id = ?3",
            params![id.network.as_str(), id.kind.as_str(), id.external_id],
            |row| Ok((row.get::<_, Option<String>>(0)?, row.get::<_, i64>(1)?)),
        );
        match found {
            Ok((title, topic_id)) => Ok(Some(StoredTopic {
                conversation: id.clone(),
                topic_id,
                title: title.unwrap_or_default(),
            })),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(err) => Err(err.into()),
        }
    }

    /// Conversation that owns `topic_id`, if one topic row does.
    ///
    /// # Errors
    ///
    /// [`crate::Error::Sqlite`] on a read failure and [`crate::Error::Corrupt`] for an unknown enum.
    pub fn conversation_for_topic(&self, topic_id: i64) -> Result<Option<ConversationId>> {
        let found = self.conn.query_row(
            "SELECT c.network, c.kind, c.external_id
             FROM topic_map t
             JOIN conversations c ON c.id = t.conversation_id
             WHERE t.telegram_topic_id = ?1",
            [topic_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        );
        match found {
            Ok((network, kind, external_id)) => {
                Ok(Some(conversation_from(&network, &kind, external_id)?))
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(err) => Err(err.into()),
        }
    }

    /// Points `id` at `topic_id` and stores `title` when it is non-empty.
    ///
    /// Creates the conversation row when it is missing.
    ///
    /// # Errors
    ///
    /// [`crate::Error::Empty`] when the external id is empty, [`crate::Error::TopicInUse`] when another
    /// conversation owns `topic_id`, and [`crate::Error::Sqlite`] on a write failure.
    pub fn upsert_topic_row(
        &mut self,
        id: &ConversationId,
        topic_id: i64,
        title: &str,
        at_unix_secs: i64,
    ) -> Result<()> {
        let title = if title.is_empty() { None } else { Some(title) };
        let row = self.ensure_conversation(id, title, at_unix_secs)?;
        self.map_topic(row, topic_id, at_unix_secs)
    }

    /// Clears the topic for `id` and deletes its message links.
    ///
    /// Missing conversations are a no-op.
    ///
    /// # Errors
    ///
    /// [`crate::Error::Sqlite`] on a write failure.
    pub fn delete_topic_row(&mut self, id: &ConversationId) -> Result<()> {
        let Some(row) = lookup_row(&self.conn, id)? else {
            return Ok(());
        };
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM topic_map WHERE conversation_id = ?1", [row])?;
        tx.execute(
            "UPDATE conversations SET topic_id = NULL WHERE id = ?1",
            [row],
        )?;
        tx.execute("DELETE FROM message_map WHERE conversation_id = ?1", [row])?;
        tx.commit()?;
        Ok(())
    }

    /// Deletes message links owned by `id`. The topic row stays.
    ///
    /// Missing conversations are a no-op.
    ///
    /// # Errors
    ///
    /// [`crate::Error::Sqlite`] on a write failure.
    pub fn delete_conversation_links(&mut self, id: &ConversationId) -> Result<()> {
        let Some(row) = lookup_row(&self.conn, id)? else {
            return Ok(());
        };
        self.conn
            .execute("DELETE FROM message_map WHERE conversation_id = ?1", [row])?;
        Ok(())
    }

    /// Telegram message linked to `network_message_id` inside `id`.
    ///
    /// A link written for a different conversation is not returned.
    ///
    /// # Errors
    ///
    /// [`crate::Error::Sqlite`] on a read failure.
    pub fn message_link_for_network(
        &self,
        id: &ConversationId,
        network_message_id: &str,
    ) -> Result<Option<i64>> {
        let found: Option<i64> = self
            .conn
            .query_row(
                "SELECT m.telegram_message_id
                 FROM message_map m
                 JOIN conversations c ON c.id = m.conversation_id
                 WHERE m.network = ?1 AND m.network_message_id = ?2
                   AND c.network = ?3 AND c.kind = ?4 AND c.external_id = ?5",
                params![
                    id.network.as_str(),
                    network_message_id,
                    id.network.as_str(),
                    id.kind.as_str(),
                    id.external_id,
                ],
                |row| row.get(0),
            )
            .optional()?;
        Ok(found)
    }

    /// Network message linked from `telegram_message_id`.
    ///
    /// `None` when the row is missing or was written without a conversation.
    ///
    /// # Errors
    ///
    /// [`crate::Error::Sqlite`] on a read failure and [`crate::Error::Corrupt`] for an unknown enum.
    pub fn message_link_for_telegram(
        &self,
        telegram_message_id: i64,
    ) -> Result<Option<MessageLink>> {
        let found = self.conn.query_row(
            "SELECT m.network, m.network_message_id, m.created_at, c.network, c.kind, c.external_id
             FROM message_map m
             LEFT JOIN conversations c ON c.id = m.conversation_id
             WHERE m.telegram_message_id = ?1
             ORDER BY m.created_at DESC, m.id DESC
             LIMIT 1",
            [telegram_message_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                ))
            },
        );
        let (network, network_message_id, created_at, conv_network, kind, external_id) = match found
        {
            Ok(row) => row,
            Err(rusqlite::Error::QueryReturnedNoRows) => return Ok(None),
            Err(err) => return Err(err.into()),
        };
        let _ = network;
        let conversation = match (conv_network, kind, external_id) {
            (Some(network), Some(kind), Some(external_id)) => {
                Some(conversation_from(&network, &kind, external_id)?)
            }
            _ => None,
        };
        if conversation.is_none() {
            return Ok(None);
        }
        Ok(Some(MessageLink {
            conversation,
            network_message_id,
            telegram_message_id,
            created_at_unix_secs: created_at,
        }))
    }

    /// Links `network_message_id` to `telegram_message_id` unless that network id is already linked.
    ///
    /// The first write wins. A second write for the same network message id does not replace
    /// the Telegram id. Returns `Ok(false)` when another conversation already owns the id.
    ///
    /// # Errors
    ///
    /// [`crate::Error::Empty`] when the network message id is empty, and [`crate::Error::Sqlite`] on a write
    /// failure.
    pub fn insert_message_link(
        &mut self,
        id: &ConversationId,
        network_message_id: &str,
        telegram_message_id: i64,
        at_unix_secs: i64,
    ) -> Result<bool> {
        require_token(network_message_id, "network message id")?;
        if self
            .message_link_for_network(id, network_message_id)?
            .is_some()
        {
            return Ok(true);
        }
        let row = self.ensure_conversation(id, None, at_unix_secs)?;
        let tx = self.conn.transaction()?;
        let inserted = tx.execute(
            "INSERT INTO message_map
                (network, network_message_id, telegram_message_id, created_at, conversation_id)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT (network, network_message_id) DO NOTHING",
            params![
                id.network.as_str(),
                network_message_id,
                telegram_message_id,
                at_unix_secs,
                row,
            ],
        )?;
        prune_message_map_tx(&tx)?;
        tx.commit()?;
        if inserted == 0 {
            return Ok(self
                .message_link_for_network(id, network_message_id)?
                .is_some());
        }
        Ok(true)
    }

    /// Deletes the link for `telegram_message_id`, if one exists.
    ///
    /// # Errors
    ///
    /// [`crate::Error::Sqlite`] on a write failure.
    pub fn delete_message_link(&mut self, telegram_message_id: i64) -> Result<()> {
        self.conn.execute(
            "DELETE FROM message_map WHERE telegram_message_id = ?1",
            [telegram_message_id],
        )?;
        Ok(())
    }

    /// Remembers `display_name` for `user_id` under `scope` and `name_key`.
    ///
    /// Empty `scope`, `name_key`, or `user_id` is ignored. The caller folds `name_key`.
    ///
    /// # Errors
    ///
    /// [`crate::Error::Sqlite`] on a write failure.
    pub fn remember_user_name(
        &mut self,
        scope: &str,
        name_key: &str,
        user_id: &str,
        display_name: &str,
    ) -> Result<()> {
        if scope.is_empty() || name_key.is_empty() || user_id.is_empty() {
            return Ok(());
        }
        self.conn.execute(
            "INSERT INTO user_names (scope, name_key, user_id, display_name)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (scope, name_key) DO UPDATE SET
                user_id = excluded.user_id,
                display_name = excluded.display_name",
            params![scope, name_key, user_id, display_name],
        )?;
        Ok(())
    }

    /// User id stored for `name_key` in `scope`.
    ///
    /// # Errors
    ///
    /// [`crate::Error::Sqlite`] on a read failure.
    pub fn lookup_user_name(&self, scope: &str, name_key: &str) -> Result<Option<String>> {
        let found: Option<String> = self
            .conn
            .query_row(
                "SELECT user_id FROM user_names WHERE scope = ?1 AND name_key = ?2",
                params![scope, name_key],
                |row| row.get(0),
            )
            .optional()?;
        Ok(found)
    }

    /// Failed deliveries, oldest first.
    ///
    /// # Errors
    ///
    /// [`crate::Error::Sqlite`] on a read failure and [`crate::Error::Corrupt`] for an unknown enum.
    pub fn stored_failures(&self) -> Result<Vec<StoredFailure>> {
        let mut stmt = self.conn.prepare(
            "SELECT f.reason, f.retryable, f.created_at, c.network, c.kind, c.external_id
             FROM failed_events f
             LEFT JOIN conversations c ON c.id = f.conversation_id
             ORDER BY f.id",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, Option<String>>(5)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (reason, retryable, created_at, network, kind, external_id) = row?;
            let conversation = match (network, kind, external_id) {
                (Some(network), Some(kind), Some(external_id)) => {
                    Some(conversation_from(&network, &kind, external_id)?)
                }
                _ => None,
            };
            out.push(StoredFailure {
                conversation,
                reason,
                retryable: retryable != 0,
                created_at_unix_secs: created_at,
            });
        }
        Ok(out)
    }

    /// Records that a sink call was retried at `at_unix_secs`.
    ///
    /// # Errors
    ///
    /// [`crate::Error::Sqlite`] on a write failure.
    pub fn record_retry_at(&mut self, at_unix_secs: i64) -> Result<()> {
        self.conn.execute(
            "INSERT INTO bridge_retries (at) VALUES (?1)",
            [at_unix_secs],
        )?;
        Ok(())
    }

    /// Retry timestamps at or after `since_unix_secs`, oldest first.
    ///
    /// # Errors
    ///
    /// [`crate::Error::Sqlite`] on a read failure.
    pub fn retries_since(&self, since_unix_secs: i64) -> Result<Vec<i64>> {
        let mut stmt = self
            .conn
            .prepare("SELECT at FROM bridge_retries WHERE at >= ?1 ORDER BY id")?;
        let rows = stmt.query_map([since_unix_secs], |row| row.get(0))?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    /// Value of Captain's `settings` row `key`, or `None` when it is absent.
    ///
    /// # Errors
    ///
    /// [`crate::Error::Empty`] when `key` is empty, and [`crate::Error::Sqlite`] on a read failure.
    pub fn setting(&self, key: &str) -> Result<Option<String>> {
        require_token(key, "setting")?;
        let found: Option<String> = self
            .conn
            .query_row("SELECT value FROM settings WHERE key = ?1", [key], |row| {
                row.get(0)
            })
            .optional()?;
        Ok(found)
    }

    /// Creates or replaces Captain's `settings` row `key`.
    ///
    /// # Errors
    ///
    /// [`crate::Error::Empty`] when `key` is empty, and [`crate::Error::Sqlite`] on a write failure.
    pub fn set_setting(&mut self, key: &str, value: &str) -> Result<()> {
        require_token(key, "setting")?;
        self.conn.execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2)
             ON CONFLICT (key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }
}

fn lookup_row(conn: &rusqlite::Connection, id: &ConversationId) -> Result<Option<i64>> {
    let found: Option<i64> = conn
        .query_row(
            "SELECT id FROM conversations
             WHERE network = ?1 AND kind = ?2 AND external_id = ?3",
            params![id.network.as_str(), id.kind.as_str(), id.external_id],
            |row| row.get(0),
        )
        .optional()?;
    Ok(found)
}

fn conversation_from(network: &str, kind: &str, external_id: String) -> Result<ConversationId> {
    Ok(ConversationId {
        network: vocab::parse_network(network)?,
        kind: vocab::parse_kind(kind)?,
        external_id,
    })
}

fn prune_message_map_tx(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    let count: i64 = tx.query_row("SELECT count(*) FROM message_map", [], |row| row.get(0))?;
    let extra = count - MESSAGE_MAP_MAX_ROWS;
    if extra <= 0 {
        return Ok(());
    }
    tx.execute(
        "DELETE FROM message_map WHERE id IN (
            SELECT id FROM message_map ORDER BY created_at ASC, id ASC LIMIT ?1
        )",
        [extra],
    )?;
    Ok(())
}
