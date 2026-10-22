//! Reads. Statements that select rows live here. Callers pass the window; nothing reads the clock.

use rusqlite::{Connection, Row, params};

use crate::schema::{captain_version, hub_version};
use crate::vocab::{self, ContentKind, Direction, LogLevel};
use crate::write::{bridge_supergroup, source_with_bridge};
use crate::{ConversationKind, Error, ModuleHealth, Network, Result};

const STATE_QUEUED: &str = "queued"; // captain_inbox::TicketState::Queued
const STATE_DIGEST: &str = "digest"; // captain_inbox::TicketState::Digest

/// Which log table [`Reader::event_level_counts`] reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventStream {
    /// `bridge_events`.
    Bridge,
    /// `module_events`.
    Module,
}

/// A read-only view of a [`crate::Hub`]. It cannot insert or migrate.
#[derive(Debug)]
pub struct Reader<'a> {
    conn: &'a Connection,
}

impl<'a> Reader<'a> {
    pub(crate) fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    /// Version in `hub_schema`, or 0 when that table has no row.
    ///
    /// # Errors
    ///
    /// [`Error::Sqlite`] on a read failure.
    pub fn schema_version(&self) -> Result<i64> {
        hub_version(self.conn)
    }

    /// Captain's `PRAGMA user_version`.
    ///
    /// # Errors
    ///
    /// [`Error::Sqlite`] on a read failure.
    pub fn captain_user_version(&self) -> Result<i64> {
        captain_version(self.conn)
    }

    /// The bridge supergroup chat id, or `None` when it has not been set.
    ///
    /// # Errors
    ///
    /// [`Error::Sqlite`] on a read failure and [`Error::Corrupt`] when the stored value is not
    /// an integer.
    pub fn bridge_supergroup(&self) -> Result<Option<i64>> {
        bridge_supergroup(self.conn)
    }

    /// Which network a Captain ticket on `chat_id` belongs to.
    ///
    /// See [`crate::Hub::source_of_chat`].
    ///
    /// # Errors
    ///
    /// The same cases as [`crate::Hub::source_of_chat`].
    pub fn source_of_chat(&self, chat_id: i64, topic_id: Option<i64>) -> Result<Network> {
        crate::write::source_of_chat(self.conn, chat_id, topic_id)
    }

    /// Every conversation, lowest id first.
    ///
    /// # Errors
    ///
    /// [`Error::Sqlite`] on a read failure and [`Error::Corrupt`] for an unknown enum.
    pub fn conversations(&self) -> Result<Vec<Conversation>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, network, kind, external_id, title, topic_id, muted, first_seen,
                    last_activity, tags
             FROM conversations ORDER BY id",
        )?;
        let rows = stmt.query_map([], map_conversation)?;
        let raws = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        raws.into_iter().map(finish_conversation).collect()
    }

    /// Counter rows whose UTC day is in `first_day..=last_day`.
    ///
    /// # Errors
    ///
    /// [`Error::Sqlite`] on a read failure and [`Error::Corrupt`] for an unknown enum or hour.
    pub fn counters_between_days(&self, first_day: i64, last_day: i64) -> Result<Vec<CounterRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT c.day, c.hour, c.conversation_id, v.network, c.direction, c.content_kind, c.count
             FROM message_counters c
             JOIN conversations v ON v.id = c.conversation_id
             WHERE c.day >= ?1 AND c.day <= ?2",
        )?;
        let rows = stmt.query_map(params![first_day, last_day], map_counter)?;
        let raws = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        raws.iter().map(finish_counter).collect()
    }

    /// Tickets opened in the half-open window `start_unix_secs..end_unix_secs`.
    ///
    /// Each row's `source` comes from [`Reader::source_of_chat`].
    ///
    /// # Errors
    ///
    /// [`Error::Sqlite`] on a read failure, and the attribution errors of
    /// [`Reader::source_of_chat`].
    pub fn tickets_by_opened(
        &self,
        start_unix_secs: i64,
        end_unix_secs: i64,
    ) -> Result<Vec<TicketSnap>> {
        self.tickets(
            "opened_at >= ?1 AND opened_at < ?2",
            start_unix_secs,
            end_unix_secs,
        )
    }

    /// Tickets whose anchor falls in the half-open window `start_unix_secs..end_unix_secs`.
    ///
    /// # Errors
    ///
    /// [`Error::Sqlite`] on a read failure, and the attribution errors of
    /// [`Reader::source_of_chat`].
    pub fn tickets_by_anchor(
        &self,
        start_unix_secs: i64,
        end_unix_secs: i64,
    ) -> Result<Vec<TicketSnap>> {
        self.tickets(
            "anchor_date >= ?1 AND anchor_date < ?2",
            start_unix_secs,
            end_unix_secs,
        )
    }

    /// Tickets still in `queued` or `digest`, whatever day they opened.
    ///
    /// # Errors
    ///
    /// [`Error::Sqlite`] on a read failure, and the attribution errors of
    /// [`Reader::source_of_chat`].
    pub fn open_tickets(&self) -> Result<Vec<TicketSnap>> {
        let mut stmt = self.conn.prepare(&format!(
            "{TICKET_COLUMNS} WHERE state IN (?1, ?2) ORDER BY ticket_id"
        ))?;
        let rows = stmt.query_map(params![STATE_QUEUED, STATE_DIGEST], map_ticket)?;
        let raws = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        self.attach_sources(raws)
    }

    /// Latest snapshot of every module, by module name.
    ///
    /// # Errors
    ///
    /// [`Error::Sqlite`] on a read failure and [`Error::Corrupt`] for an unknown state.
    pub fn modules(&self) -> Result<Vec<ModuleHealth>> {
        let mut stmt = self.conn.prepare(
            "SELECT module, state, detail, updated_at FROM module_health ORDER BY module",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (module, state, detail, updated_at_unix_secs) = row?;
            out.push(ModuleHealth {
                module,
                state: vocab::parse_module_state(&state)?,
                detail: detail.unwrap_or_default(),
                updated_at_unix_secs,
            });
        }
        Ok(out)
    }

    /// Failed deliveries. `unresolved_only` drops rows that have `resolved_at`.
    ///
    /// # Errors
    ///
    /// [`Error::Sqlite`] on a read failure and [`Error::Corrupt`] for an unknown network.
    pub fn failures(&self, unresolved_only: bool) -> Result<Vec<FailureRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT f.id, f.conversation_id, c.network, f.reason, f.retryable, f.created_at,
                    f.resolved_at
             FROM failed_events f
             LEFT JOIN conversations c ON c.id = f.conversation_id
             WHERE (?1 = 0 OR f.resolved_at IS NULL)
             ORDER BY f.id",
        )?;
        let rows = stmt.query_map(params![unresolved_only], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, Option<i64>>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, bool>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, Option<i64>>(6)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (id, conversation_id, network, reason, retryable, created_at, resolved_at) = row?;
            out.push(FailureRow {
                id,
                conversation_id,
                network: network.as_deref().map(vocab::parse_network).transpose()?,
                reason,
                retryable,
                created_at_unix_secs: created_at,
                resolved_at_unix_secs: resolved_at,
            });
        }
        Ok(out)
    }

    /// Counts of log rows in the half-open window `start_unix_secs..end_unix_secs`, by level.
    ///
    /// Levels with no rows are omitted.
    ///
    /// # Errors
    ///
    /// [`Error::Sqlite`] on a read failure and [`Error::Corrupt`] for an unknown level.
    pub fn event_level_counts(
        &self,
        stream: EventStream,
        start_unix_secs: i64,
        end_unix_secs: i64,
    ) -> Result<Vec<LevelCount>> {
        let sql = match stream {
            EventStream::Bridge => {
                "SELECT level, count(*) FROM bridge_events
                 WHERE ts >= ?1 AND ts < ?2 GROUP BY level"
            }
            EventStream::Module => {
                "SELECT level, count(*) FROM module_events
                 WHERE ts >= ?1 AND ts < ?2 GROUP BY level"
            }
        };
        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt.query_map(params![start_unix_secs, end_unix_secs], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (level, count) = row?;
            out.push(LevelCount {
                level: vocab::parse(&level)?,
                count: nonneg(count)?,
            });
        }
        Ok(out)
    }

    /// Newest log rows in `stream`, at most `limit`.
    ///
    /// A `limit` of 0 returns no rows.
    ///
    /// # Errors
    ///
    /// [`Error::Sqlite`] on a read failure and [`Error::Corrupt`] for an unknown level.
    pub fn recent_events(&self, stream: EventStream, limit: u32) -> Result<Vec<LogRow>> {
        let sql = match stream {
            EventStream::Bridge => {
                "SELECT id, ts, module, level, kind, detail FROM bridge_events
                 ORDER BY id DESC LIMIT ?1"
            }
            EventStream::Module => {
                "SELECT id, ts, module, level, kind, detail FROM module_events
                 ORDER BY id DESC LIMIT ?1"
            }
        };
        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt.query_map([i64::from(limit)], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, Option<String>>(5)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (id, at_unix_secs, module, level, kind, detail) = row?;
            out.push(LogRow {
                id,
                at_unix_secs,
                module,
                level: vocab::parse(&level)?,
                kind,
                detail,
            });
        }
        Ok(out)
    }

    fn tickets(&self, predicate: &str, start: i64, end: i64) -> Result<Vec<TicketSnap>> {
        let sql = format!("{TICKET_COLUMNS} WHERE {predicate} ORDER BY ticket_id");
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params![start, end], map_ticket)?;
        let raws = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        self.attach_sources(raws)
    }

    fn attach_sources(&self, raws: Vec<TicketRaw>) -> Result<Vec<TicketSnap>> {
        let bridge = bridge_supergroup(self.conn)?;
        let mut out = Vec::with_capacity(raws.len());
        for raw in raws {
            let source = source_with_bridge(self.conn, bridge, raw.chat_id, raw.topic_id)?;
            out.push(raw.into_snap(source)?);
        }
        Ok(out)
    }
}

const TICKET_COLUMNS: &str = "
SELECT ticket_id, chat_id, topic_id, chat_label, chat_kind, profile, trigger, route, state,
       state_reason, opened_at, closed_at, anchor_date, message_count, label_implicit,
       label_explicit, read_at, reply_at
FROM tickets";

/// One conversation row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conversation {
    /// Row id.
    pub id: i64,
    /// Network.
    pub network: Network,
    /// Shape.
    pub kind: ConversationKind,
    /// The network's own id.
    pub external_id: String,
    /// Display title.
    pub title: Option<String>,
    /// Telegram topic id, when this row is one topic.
    pub topic_id: Option<i64>,
    /// Whether triage should ignore it.
    pub muted: bool,
    /// Earliest seen time, Unix seconds.
    pub first_seen_unix_secs: i64,
    /// Latest seen time, Unix seconds.
    pub last_activity_unix_secs: i64,
    /// Owner tags, as stored.
    pub tags: Option<String>,
}

/// One hour bucket of message counts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CounterRow {
    /// UTC day number since the Unix epoch.
    pub day: i64,
    /// UTC hour, 0 to 23.
    pub hour: u8,
    /// `conversations.id`.
    pub conversation_id: i64,
    /// Network of that conversation.
    pub network: Network,
    /// In or out.
    pub direction: Direction,
    /// Payload kind.
    pub content_kind: ContentKind,
    /// Messages in the bucket.
    pub count: u64,
}

/// A Captain ticket plus the network [`crate::Hub::source_of_chat`] assigns it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TicketSnap {
    /// Ticket id.
    pub id: i64,
    /// Captain chat id.
    pub chat_id: i64,
    /// Forum topic, when the ticket has one.
    pub topic_id: Option<i64>,
    /// Scope label stored on the ticket.
    pub chat_label: String,
    /// Scope kind stored on the ticket: `dm`, `group`, `forum`, or absent.
    pub chat_kind: Option<String>,
    /// Profile name.
    pub profile: String,
    /// Trigger name, as Captain stored it.
    pub trigger: String,
    /// Route name, when the ticket has one.
    pub route: Option<String>,
    /// State name.
    pub state: String,
    /// Why it was skipped or sent to the digest.
    pub state_reason: Option<String>,
    /// When the burst opened, Unix seconds.
    pub opened_at_unix_secs: i64,
    /// When the burst closed, Unix seconds.
    pub closed_at_unix_secs: i64,
    /// Send time of the anchor, Unix seconds.
    pub anchor_date_unix_secs: i64,
    /// Messages in the burst.
    pub message_count: u64,
    /// Implicit label, once resolved.
    pub label_implicit: Option<String>,
    /// Explicit label, when the owner set one.
    pub label_explicit: Option<String>,
    /// First read covering the anchor, Unix seconds.
    pub read_at_unix_secs: Option<i64>,
    /// First reply on the key, Unix seconds.
    pub reply_at_unix_secs: Option<i64>,
    /// Network the chat belongs to.
    pub source: Network,
}

/// One failed delivery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailureRow {
    /// Row id.
    pub id: i64,
    /// Conversation involved, when there is one.
    pub conversation_id: Option<i64>,
    /// That conversation's network. `None` when the failure names no conversation.
    pub network: Option<Network>,
    /// Short reason.
    pub reason: String,
    /// Whether the bridge should try again.
    pub retryable: bool,
    /// When it failed, Unix seconds.
    pub created_at_unix_secs: i64,
    /// When it was resolved, Unix seconds.
    pub resolved_at_unix_secs: Option<i64>,
}

/// How many log rows share one level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LevelCount {
    /// Severity.
    pub level: LogLevel,
    /// Rows.
    pub count: u64,
}

/// One bridge or module log row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogRow {
    /// Row id.
    pub id: i64,
    /// When it happened, Unix seconds.
    pub at_unix_secs: i64,
    /// Module name.
    pub module: String,
    /// Severity.
    pub level: LogLevel,
    /// Machine tag.
    pub kind: String,
    /// Optional note. Not message text.
    pub detail: Option<String>,
}

struct TicketRaw {
    id: i64,
    chat_id: i64,
    topic_id: Option<i64>,
    chat_label: String,
    chat_kind: Option<String>,
    profile: String,
    trigger: String,
    route: Option<String>,
    state: String,
    state_reason: Option<String>,
    opened_at_unix_secs: i64,
    closed_at_unix_secs: i64,
    anchor_date_unix_secs: i64,
    message_count: i64,
    label_implicit: Option<String>,
    label_explicit: Option<String>,
    read_at_unix_secs: Option<i64>,
    reply_at_unix_secs: Option<i64>,
}

impl TicketRaw {
    fn into_snap(self, source: Network) -> Result<TicketSnap> {
        Ok(TicketSnap {
            id: self.id,
            chat_id: self.chat_id,
            topic_id: self.topic_id,
            chat_label: self.chat_label,
            chat_kind: self.chat_kind,
            profile: self.profile,
            trigger: self.trigger,
            route: self.route,
            state: self.state,
            state_reason: self.state_reason,
            opened_at_unix_secs: self.opened_at_unix_secs,
            closed_at_unix_secs: self.closed_at_unix_secs,
            anchor_date_unix_secs: self.anchor_date_unix_secs,
            message_count: nonneg(self.message_count)?,
            label_implicit: self.label_implicit,
            label_explicit: self.label_explicit,
            read_at_unix_secs: self.read_at_unix_secs,
            reply_at_unix_secs: self.reply_at_unix_secs,
            source,
        })
    }
}

struct ConversationRaw {
    id: i64,
    network: String,
    kind: String,
    external_id: String,
    title: Option<String>,
    topic_id: Option<i64>,
    muted: bool,
    first_seen_unix_secs: i64,
    last_activity_unix_secs: i64,
    tags: Option<String>,
}

struct CounterRaw {
    day: i64,
    hour: i64,
    conversation_id: i64,
    network: String,
    direction: String,
    content_kind: String,
    count: i64,
}

fn finish_conversation(raw: ConversationRaw) -> Result<Conversation> {
    Ok(Conversation {
        id: raw.id,
        network: vocab::parse_network(&raw.network)?,
        kind: vocab::parse_kind(&raw.kind)?,
        external_id: raw.external_id,
        title: raw.title,
        topic_id: raw.topic_id,
        muted: raw.muted,
        first_seen_unix_secs: raw.first_seen_unix_secs,
        last_activity_unix_secs: raw.last_activity_unix_secs,
        tags: raw.tags,
    })
}

fn finish_counter(raw: &CounterRaw) -> Result<CounterRow> {
    let hour = u8::try_from(raw.hour)
        .ok()
        .filter(|hour| *hour < 24)
        .ok_or_else(|| Error::Corrupt(format!("hour {}", raw.hour)))?;
    Ok(CounterRow {
        day: raw.day,
        hour,
        conversation_id: raw.conversation_id,
        network: vocab::parse_network(&raw.network)?,
        direction: vocab::parse(&raw.direction)?,
        content_kind: vocab::parse(&raw.content_kind)?,
        count: nonneg(raw.count)?,
    })
}

fn map_conversation(row: &Row<'_>) -> rusqlite::Result<ConversationRaw> {
    Ok(ConversationRaw {
        id: row.get(0)?,
        network: row.get(1)?,
        kind: row.get(2)?,
        external_id: row.get(3)?,
        title: row.get(4)?,
        topic_id: row.get(5)?,
        muted: row.get(6)?,
        first_seen_unix_secs: row.get(7)?,
        last_activity_unix_secs: row.get(8)?,
        tags: row.get(9)?,
    })
}

fn map_counter(row: &Row<'_>) -> rusqlite::Result<CounterRaw> {
    Ok(CounterRaw {
        day: row.get(0)?,
        hour: row.get(1)?,
        conversation_id: row.get(2)?,
        network: row.get(3)?,
        direction: row.get(4)?,
        content_kind: row.get(5)?,
        count: row.get(6)?,
    })
}

fn map_ticket(row: &Row<'_>) -> rusqlite::Result<TicketRaw> {
    Ok(TicketRaw {
        id: row.get(0)?,
        chat_id: row.get(1)?,
        topic_id: row.get(2)?,
        chat_label: row.get(3)?,
        chat_kind: row.get(4)?,
        profile: row.get(5)?,
        trigger: row.get(6)?,
        route: row.get(7)?,
        state: row.get(8)?,
        state_reason: row.get(9)?,
        opened_at_unix_secs: row.get(10)?,
        closed_at_unix_secs: row.get(11)?,
        anchor_date_unix_secs: row.get(12)?,
        message_count: row.get(13)?,
        label_implicit: row.get(14)?,
        label_explicit: row.get(15)?,
        read_at_unix_secs: row.get(16)?,
        reply_at_unix_secs: row.get(17)?,
    })
}

fn nonneg(value: i64) -> Result<u64> {
    u64::try_from(value).map_err(|_| Error::Corrupt(format!("negative count {value}")))
}
