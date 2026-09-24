//! Tickets, their messages, senders and state transitions.

use captain_core::{ChatId, MessageId};
use captain_inbox::{
    EntryKind, ExplicitLabel, ImplicitLabel, Route, ThreadKey, TicketState, TriggerKind,
};
use rusqlite::{OptionalExtension, Row, params};
use serde::Serialize;

use crate::messages::{entry_kind_str, parse_entry_kind};
use crate::{Error, Result, Store, StoredMessage};

const ANCHOR_ROLE: &str = "anchor";
const BURST_ROLE: &str = "burst";
const DEFAULT_LIST_LIMIT: usize = 100;

/// A ticket to create.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewTicket {
    /// Thread.
    pub key: ThreadKey,
    /// Scope label, or the chat title for manual tickets.
    pub chat_label: String,
    /// Kind of the scope entry.
    pub chat_kind: Option<EntryKind>,
    /// Profile of the burst.
    pub profile: String,
    /// First trigger.
    pub trigger: TriggerKind,
    /// Why it fired, without message text.
    pub trigger_reason: String,
    /// Route, if any.
    pub route: Option<Route>,
    /// Starting state.
    pub state: TicketState,
    /// Why it was skipped or sent to the digest.
    pub state_reason: Option<String>,
    /// The anchor was older than the stale threshold.
    pub stale: bool,
    /// Owner's note, for manual tickets.
    pub note: Option<String>,
    /// When the burst opened.
    pub opened_at: i64,
    /// When the burst closed.
    pub closed_at: i64,
    /// The message a reply would answer.
    pub anchor: MessageId,
    /// Send time of the anchor.
    pub anchor_date: i64,
    /// Every message of the burst.
    pub messages: Vec<MessageId>,
    /// Distinct senders.
    pub senders: Vec<ChatId>,
}

/// A ticket as listed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TicketRow {
    /// Id.
    pub id: i64,
    /// Chat.
    pub chat: ChatId,
    /// Topic.
    pub topic: Option<MessageId>,
    /// Scope label.
    pub chat_label: String,
    /// Kind of the scope entry.
    pub chat_kind: Option<EntryKind>,
    /// Profile.
    pub profile: String,
    /// First trigger.
    pub trigger: TriggerKind,
    /// Why it fired.
    pub trigger_reason: String,
    /// Route.
    pub route: Option<Route>,
    /// State.
    pub state: TicketState,
    /// Why it was skipped or sent to the digest.
    pub state_reason: Option<String>,
    /// Anchor was stale.
    pub stale: bool,
    /// Owner's note.
    pub note: Option<String>,
    /// Burst open time.
    pub opened_at: i64,
    /// Burst close time.
    pub closed_at: i64,
    /// Anchor id.
    pub anchor: MessageId,
    /// Anchor send time.
    pub anchor_date: i64,
    /// Messages in the burst.
    pub message_count: usize,
    /// When the resolver labelled it.
    pub resolved_at: Option<i64>,
    /// Implicit label.
    pub label_implicit: Option<ImplicitLabel>,
    /// Explicit label.
    pub label_explicit: Option<ExplicitLabel>,
    /// First read covering the anchor.
    pub read_at: Option<i64>,
    /// First reply on the key after the anchor.
    pub reply_at: Option<i64>,
}

impl TicketRow {
    /// The thread the ticket is on.
    pub fn key(&self) -> ThreadKey {
        ThreadKey {
            chat: self.chat,
            topic: self.topic,
        }
    }
}

/// One state transition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TicketEvent {
    /// When.
    pub at: i64,
    /// Previous state; `None` at creation.
    pub from: Option<TicketState>,
    /// New state.
    pub to: TicketState,
    /// Why.
    pub reason: Option<String>,
}

/// A message id in a ticket, with its role.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TicketMessage {
    /// Chat.
    pub chat: ChatId,
    /// Id.
    pub id: MessageId,
    /// `anchor` or `burst`.
    pub role: String,
}

/// A ticket with its history.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TicketDetail {
    /// The ticket.
    pub ticket: TicketRow,
    /// Transitions, oldest first.
    pub events: Vec<TicketEvent>,
    /// Message ids.
    pub messages: Vec<TicketMessage>,
    /// Stored copies of those messages. Text is included; callers decide whether to show it.
    pub bodies: Vec<StoredMessage>,
}

/// Filter for [`Store::tickets`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TicketFilter {
    /// Only this state.
    pub state: Option<TicketState>,
    /// Only this trigger.
    pub trigger: Option<TriggerKind>,
    /// Only tickets opened at or after this time.
    pub since: Option<i64>,
    /// Only tickets without an explicit label.
    pub unlabelled: bool,
    /// At most this many, newest first. Defaults to 100.
    pub limit: Option<usize>,
}

/// A ticket waiting for its implicit label.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingTicket {
    /// Id.
    pub id: i64,
    /// Thread.
    pub key: ThreadKey,
    /// Anchor.
    pub anchor: MessageId,
    /// Anchor send time.
    pub anchor_date: i64,
    /// Senders, for the answered-elsewhere check.
    pub senders: Vec<ChatId>,
}

fn decode<T: std::str::FromStr<Err = String>>(v: Option<String>) -> rusqlite::Result<Option<T>> {
    v.map(|s| s.parse::<T>()).transpose().map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, e.into())
    })
}

fn required<T: std::str::FromStr<Err = String>>(v: &str) -> rusqlite::Result<T> {
    v.parse::<T>().map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, e.into())
    })
}

impl TicketRow {
    fn from_row(r: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: r.get("ticket_id")?,
            chat: ChatId(r.get("chat_id")?),
            topic: r.get::<_, Option<i32>>("topic_id")?.map(MessageId),
            chat_label: r.get("chat_label")?,
            chat_kind: r
                .get::<_, Option<String>>("chat_kind")?
                .as_deref()
                .and_then(parse_entry_kind),
            profile: r.get("profile")?,
            trigger: required(&r.get::<_, String>("trigger")?)?,
            trigger_reason: r.get("trigger_reason")?,
            route: decode(r.get("route")?)?,
            state: required(&r.get::<_, String>("state")?)?,
            state_reason: r.get("state_reason")?,
            stale: r.get("stale")?,
            note: r.get("note")?,
            opened_at: r.get("opened_at")?,
            closed_at: r.get("closed_at")?,
            anchor: MessageId(r.get("anchor_msg_id")?),
            anchor_date: r.get("anchor_date")?,
            message_count: crate::count(r.get("message_count")?),
            resolved_at: r.get("resolved_at")?,
            label_implicit: decode(r.get("label_implicit")?)?,
            label_explicit: decode(r.get("label_explicit")?)?,
            read_at: r.get("read_at")?,
            reply_at: r.get("reply_at")?,
        })
    }
}

impl Store {
    /// Creates a ticket with its messages, senders and first event. Returns its id.
    ///
    /// # Errors
    ///
    /// [`crate::Error::Sqlite`] on a write failure; nothing is written then.
    pub fn create_ticket(&self, t: &NewTicket) -> Result<i64> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO tickets (chat_id, topic_id, chat_label, chat_kind, profile, trigger,
                 trigger_reason, route, state, state_reason, stale, note, opened_at, closed_at,
                 anchor_msg_id, anchor_date, message_count)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
            params![
                t.key.chat.0,
                t.key.topic.map(|x| x.0),
                t.chat_label,
                t.chat_kind.map(entry_kind_str),
                t.profile,
                t.trigger.as_str(),
                t.trigger_reason,
                t.route.map(Route::as_str),
                t.state.as_str(),
                t.state_reason,
                t.stale,
                t.note,
                t.opened_at,
                t.closed_at,
                t.anchor.0,
                t.anchor_date,
                i64::try_from(t.messages.len()).unwrap_or(i64::MAX),
            ],
        )?;
        let id = tx.last_insert_rowid();
        for m in &t.messages {
            let role = if *m == t.anchor {
                ANCHOR_ROLE
            } else {
                BURST_ROLE
            };
            tx.execute(
                "INSERT OR IGNORE INTO ticket_messages (ticket_id, chat_id, msg_id, role)
                 VALUES (?1, ?2, ?3, ?4)",
                params![id, t.key.chat.0, m.0, role],
            )?;
        }
        for s in &t.senders {
            tx.execute(
                "INSERT OR IGNORE INTO ticket_senders (ticket_id, sender_id) VALUES (?1, ?2)",
                params![id, s.0],
            )?;
        }
        tx.execute(
            "INSERT INTO ticket_events (ticket_id, seq, at, from_state, to_state, reason)
             VALUES (?1, 1, ?2, NULL, ?3, ?4)",
            params![id, t.closed_at, t.state.as_str(), t.state_reason],
        )?;
        tx.commit()?;
        Ok(id)
    }

    /// Tickets matching `filter`, newest first.
    ///
    /// # Errors
    ///
    /// [`crate::Error::Sqlite`] on a read failure, [`Error::Corrupt`] for undecodable rows.
    pub fn tickets(&self, filter: &TicketFilter) -> Result<Vec<TicketRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT * FROM tickets
             WHERE (?1 IS NULL OR state = ?1) AND (?2 IS NULL OR trigger = ?2)
               AND (?3 IS NULL OR opened_at >= ?3)
               AND (?4 = 0 OR label_explicit IS NULL)
             ORDER BY opened_at DESC, ticket_id DESC LIMIT ?5",
        )?;
        let rows = stmt.query_map(
            params![
                filter.state.map(TicketState::as_str),
                filter.trigger.map(TriggerKind::as_str),
                filter.since,
                filter.unlabelled,
                i64::try_from(filter.limit.unwrap_or(DEFAULT_LIST_LIMIT)).unwrap_or(i64::MAX),
            ],
            TicketRow::from_row,
        )?;
        rows.collect::<rusqlite::Result<_>>().map_err(corrupt)
    }

    /// One ticket with its history, or `None` if it does not exist.
    ///
    /// # Errors
    ///
    /// [`crate::Error::Sqlite`] on a read failure, [`Error::Corrupt`] for undecodable rows.
    pub fn ticket(&self, id: i64) -> Result<Option<TicketDetail>> {
        let Some(ticket) = self
            .conn
            .query_row(
                "SELECT * FROM tickets WHERE ticket_id = ?1",
                [id],
                TicketRow::from_row,
            )
            .optional()
            .map_err(corrupt)?
        else {
            return Ok(None);
        };
        let mut stmt = self.conn.prepare(
            "SELECT at, from_state, to_state, reason FROM ticket_events
             WHERE ticket_id = ?1 ORDER BY seq",
        )?;
        let events = stmt
            .query_map([id], |r| {
                Ok(TicketEvent {
                    at: r.get(0)?,
                    from: decode(r.get(1)?)?,
                    to: required(&r.get::<_, String>(2)?)?,
                    reason: r.get(3)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()
            .map_err(corrupt)?;
        let mut stmt = self.conn.prepare(
            "SELECT chat_id, msg_id, role FROM ticket_messages WHERE ticket_id = ?1 ORDER BY msg_id",
        )?;
        let messages = stmt
            .query_map([id], |r| {
                Ok(TicketMessage {
                    chat: ChatId(r.get(0)?),
                    id: MessageId(r.get(1)?),
                    role: r.get(2)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        let bodies = self.ticket_message_bodies(id)?;
        Ok(Some(TicketDetail {
            ticket,
            events,
            messages,
            bodies,
        }))
    }

    fn transition(&self, id: i64, to: TicketState, reason: Option<&str>, now: i64) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        let from: String = tx.query_row(
            "SELECT state FROM tickets WHERE ticket_id = ?1",
            [id],
            |r| r.get(0),
        )?;
        tx.execute(
            "UPDATE tickets SET state = ?2 WHERE ticket_id = ?1",
            params![id, to.as_str()],
        )?;
        tx.execute(
            "INSERT INTO ticket_events (ticket_id, seq, at, from_state, to_state, reason)
             VALUES (?1, (SELECT coalesce(max(seq), 0) + 1 FROM ticket_events WHERE ticket_id = ?1),
                     ?2, ?3, ?4, ?5)",
            params![id, now, from, to.as_str(), reason],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Queued tickets whose anchor was sent before `cutoff`, ready for the resolver.
    ///
    /// # Errors
    ///
    /// [`crate::Error::Sqlite`] on a read failure.
    pub fn pending_resolution(&self, cutoff: i64) -> Result<Vec<PendingTicket>> {
        let mut stmt = self.conn.prepare(
            "SELECT ticket_id, chat_id, topic_id, anchor_msg_id, anchor_date FROM tickets
             WHERE state IN ('queued', 'digest') AND anchor_date < ?1 ORDER BY ticket_id",
        )?;
        let mut pending: Vec<PendingTicket> = stmt
            .query_map([cutoff], |r| {
                Ok(PendingTicket {
                    id: r.get(0)?,
                    key: ThreadKey {
                        chat: ChatId(r.get(1)?),
                        topic: r.get::<_, Option<i32>>(2)?.map(MessageId),
                    },
                    anchor: MessageId(r.get(3)?),
                    anchor_date: r.get(4)?,
                    senders: Vec::new(),
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        let mut senders = self
            .conn
            .prepare("SELECT sender_id FROM ticket_senders WHERE ticket_id = ?1")?;
        for p in &mut pending {
            p.senders = senders
                .query_map([p.id], |r| Ok(ChatId(r.get(0)?)))?
                .collect::<rusqlite::Result<_>>()?;
        }
        Ok(pending)
    }

    /// Labels a ticket from the owner's behaviour and moves it to `resolved`.
    ///
    /// # Errors
    ///
    /// [`crate::Error::Sqlite`] on a write failure.
    pub fn resolve_ticket(
        &self,
        id: i64,
        label: ImplicitLabel,
        read_at: Option<i64>,
        reply_at: Option<i64>,
        now: i64,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE tickets SET label_implicit = ?2, read_at = ?3, reply_at = ?4, resolved_at = ?5
             WHERE ticket_id = ?1",
            params![id, label.as_str(), read_at, reply_at, now],
        )?;
        self.transition(id, TicketState::Resolved, Some(label.as_str()), now)
    }

    /// Sets or clears the owner's label. Returns false when the ticket does not exist.
    ///
    /// # Errors
    ///
    /// [`crate::Error::Sqlite`] on a write failure.
    pub fn label_ticket(&self, id: i64, label: Option<ExplicitLabel>, now: i64) -> Result<bool> {
        let n = self.conn.execute(
            "UPDATE tickets SET label_explicit = ?2, labelled_at = ?3 WHERE ticket_id = ?1",
            params![id, label.map(ExplicitLabel::as_str), now],
        )?;
        Ok(n > 0)
    }

    /// Ticket counts by state, for the status bar.
    ///
    /// # Errors
    ///
    /// [`crate::Error::Sqlite`] on a read failure, [`Error::Corrupt`] for an unknown state.
    pub fn ticket_counts(&self, since: i64) -> Result<Vec<(TicketState, usize)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT state, count(*) FROM tickets WHERE opened_at >= ?1 GROUP BY state")?;
        let rows = stmt.query_map([since], |r| {
            Ok((required(&r.get::<_, String>(0)?)?, crate::count(r.get(1)?)))
        })?;
        rows.collect::<rusqlite::Result<_>>().map_err(corrupt)
    }
}

fn corrupt(e: rusqlite::Error) -> Error {
    match e {
        rusqlite::Error::FromSqlConversionFailure(_, _, inner) => Error::Corrupt(inner.to_string()),
        other => Error::Sqlite(other),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn new_ticket(chat: i64, anchor: i32, at: i64) -> NewTicket {
        NewTicket {
            key: ThreadKey {
                chat: ChatId(chat),
                topic: None,
            },
            chat_label: "ops".into(),
            chat_kind: Some(EntryKind::Group),
            profile: "group".into(),
            trigger: TriggerKind::Mention,
            trigger_reason: format!("mentioned in message {anchor}"),
            route: Some(Route::Draft),
            state: TicketState::Queued,
            state_reason: None,
            stale: false,
            note: None,
            opened_at: at,
            closed_at: at + 10,
            anchor: MessageId(anchor),
            anchor_date: at,
            messages: vec![MessageId(anchor - 1), MessageId(anchor)],
            senders: vec![ChatId(7)],
        }
    }

    #[test]
    fn tickets_round_trip_with_history() {
        let store = Store::open_in_memory().unwrap();
        let id = store.create_ticket(&new_ticket(-1001, 5, 100)).unwrap();
        let listed = store.tickets(&TicketFilter::default()).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].trigger, TriggerKind::Mention);

        let pending = store.pending_resolution(1000).unwrap();
        assert_eq!(pending[0].senders, [ChatId(7)]);
        store
            .resolve_ticket(id, ImplicitLabel::Answered, Some(150), Some(160), 2000)
            .unwrap();
        assert!(store.pending_resolution(5000).unwrap().is_empty());

        assert!(
            store
                .label_ticket(id, Some(ExplicitLabel::Want), 2100)
                .unwrap()
        );
        assert!(!store.label_ticket(999, None, 2100).unwrap());
        let detail = store.ticket(id).unwrap().unwrap();
        assert_eq!(detail.events.len(), 2);
        assert_eq!(detail.events[1].from, Some(TicketState::Queued));
        assert_eq!(
            detail
                .messages
                .iter()
                .filter(|m| m.role == ANCHOR_ROLE)
                .count(),
            1
        );
        assert_eq!(detail.ticket.label_explicit, Some(ExplicitLabel::Want));
    }

    #[test]
    fn filters_apply() {
        let store = Store::open_in_memory().unwrap();
        store.create_ticket(&new_ticket(-1001, 5, 100)).unwrap();
        let mut skipped = new_ticket(-1001, 9, 200);
        skipped.state = TicketState::Skipped;
        store.create_ticket(&skipped).unwrap();
        let only = |filter: TicketFilter| store.tickets(&filter).unwrap().len();
        assert_eq!(
            only(TicketFilter {
                state: Some(TicketState::Skipped),
                ..TicketFilter::default()
            }),
            1
        );
        assert_eq!(
            only(TicketFilter {
                since: Some(150),
                ..TicketFilter::default()
            }),
            1
        );
        assert_eq!(store.ticket_counts(0).unwrap().len(), 2);
    }
}
