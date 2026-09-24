//! Extracts the facts `captain_inbox::report` needs. Reads ids, times and counts, never text.

use std::collections::HashSet;

use captain_core::{ChatId, MessageId};
use captain_inbox::report::{
    Answered, ApiEvent, ApiTrouble, Candidate, ContextShape, InboundCount, ReportInput, RunSpan,
    TicketFact,
};
use captain_inbox::unix_day;
use rusqlite::params;

use crate::{Result, Store, TicketFilter};

const BURST_GAP_MAX_SECS: i64 = 300; // M7 counts gaps up to five minutes
const PROXIMITY_REPLY_SECS: i64 = 1800; // M6: an unthreaded reply answers the last message within 30 min
const SECS_PER_HOUR: i64 = 3600;
const ALL_TICKETS: usize = 1 << 40; // no practical limit on tickets in a report window

impl Store {
    /// Collects the report input for `since..until`.
    ///
    /// # Errors
    ///
    /// [`crate::Error::Sqlite`] on a read failure, [`crate::Error::Corrupt`] for undecodable
    /// tickets.
    pub fn report_input(&self, since: i64, until: i64) -> Result<ReportInput> {
        Ok(ReportInput {
            since,
            until,
            tickets: self.ticket_facts(since, until)?,
            inbound: self.inbound_counts(since, until)?,
            burst_gaps: self.burst_gaps(since, until)?,
            answered: self.answered(since, until)?,
            shapes: self.shapes(since, until)?,
            runs: self.runs(since, until)?,
            first_run_at: self.conn.query_row(
                "SELECT min(started_at) FROM daemon_runs",
                [],
                |r| r.get(0),
            )?,
            update_lags_secs: self.update_lags(since, until)?,
            api: self.api_events(since, until)?,
            store_sizes: self.store_sizes(since, until)?,
            candidates: self.candidates(since, until)?,
        })
    }

    fn ticket_facts(&self, since: i64, until: i64) -> Result<Vec<TicketFact>> {
        let rows = self.tickets(&TicketFilter {
            since: Some(since),
            limit: Some(ALL_TICKETS),
            ..TicketFilter::default()
        })?;
        Ok(rows
            .into_iter()
            .filter(|t| t.opened_at < until)
            .map(|t| TicketFact {
                id: t.id,
                key: t.key(),
                chat_label: t.chat_label,
                chat_kind: t.chat_kind,
                profile: t.profile,
                trigger: t.trigger,
                route: t.route,
                state: t.state,
                reason: t.state_reason,
                opened_at: t.opened_at,
                closed_at: t.closed_at,
                anchor_date: t.anchor_date,
                messages: t.message_count,
                implicit: t.label_implicit,
                explicit: t.label_explicit,
                read_secs: t.read_at.map(|r| (r - t.anchor_date).max(0)),
                reply_secs: t.reply_at.map(|r| (r - t.anchor_date).max(0)),
            })
            .collect())
    }

    fn inbound_counts(&self, since: i64, until: i64) -> Result<Vec<InboundCount>> {
        let mut stmt = self.conn.prepare(
            "SELECT m.chat_id, coalesce(c.scope_label, c.title, CAST(m.chat_id AS TEXT)), m.date
             FROM messages m LEFT JOIN chats c ON c.chat_id = m.chat_id
             WHERE m.outgoing = 0 AND m.date >= ?1 AND m.date < ?2",
        )?;
        let mut counts: Vec<InboundCount> = Vec::new();
        let rows = stmt.query_map(params![since, until], |r| {
            Ok((
                ChatId(r.get(0)?),
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
            ))
        })?;
        for row in rows {
            let (chat, label, date) = row?;
            let day = unix_day(date);
            match counts.iter_mut().find(|c| c.chat == chat && c.day == day) {
                Some(c) => c.count += 1,
                None => counts.push(InboundCount {
                    chat,
                    label,
                    day,
                    count: 1,
                }),
            }
        }
        Ok(counts)
    }

    fn burst_gaps(&self, since: i64, until: i64) -> Result<Vec<(String, i64)>> {
        let mut stmt = self.conn.prepare(
            "SELECT coalesce(c.profile, 'group'), gap FROM (
                 SELECT chat_id, date - lag(date) OVER (
                     PARTITION BY chat_id, topic_id ORDER BY date, msg_id) AS gap
                 FROM messages WHERE outgoing = 0 AND date >= ?1 AND date < ?2
             ) g LEFT JOIN chats c ON c.chat_id = g.chat_id
             WHERE gap IS NOT NULL AND gap <= ?3",
        )?;
        let rows = stmt.query_map(params![since, until, BURST_GAP_MAX_SECS], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Messages from others that the account answered: the target of each reply it sent, or
    /// for an unthreaded message, the last incoming one on the thread within 30 minutes.
    fn answered(&self, since: i64, until: i64) -> Result<Vec<Answered>> {
        let mut stmt = self.conn.prepare(
            "SELECT o.chat_id,
                    CASE WHEN r.msg_id IS NOT NULL THEN r.msg_id ELSE (
                        SELECT i.msg_id FROM messages i
                        WHERE i.chat_id = o.chat_id AND i.topic_id IS o.topic_id
                          AND i.outgoing = 0 AND i.sender_is_bot = 0
                          AND i.date <= o.date AND i.date >= o.date - ?3
                        ORDER BY i.date DESC, i.msg_id DESC LIMIT 1)
                    END AS answered_id
             FROM messages o
             LEFT JOIN messages r ON r.chat_id = o.chat_id AND r.msg_id = o.reply_to_msg_id
                                 AND r.outgoing = 0 AND r.sender_is_bot = 0
             WHERE o.outgoing = 1 AND o.date >= ?1 AND o.date < ?2",
        )?;
        let rows = stmt.query_map(params![since, until, PROXIMITY_REPLY_SECS], |r| {
            Ok((ChatId(r.get(0)?), r.get::<_, Option<i32>>(1)?))
        })?;
        let mut seen = HashSet::new();
        let mut covered = self.conn.prepare_cached(
            "SELECT 1 FROM ticket_messages WHERE chat_id = ?1 AND msg_id = ?2 LIMIT 1",
        )?;
        let mut out = Vec::new();
        for row in rows {
            let (chat, id) = row?;
            let Some(id) = id.map(MessageId) else {
                continue;
            };
            if seen.insert((chat, id)) {
                out.push(Answered {
                    covered: covered.exists(params![chat.0, id.0])?,
                });
            }
        }
        Ok(out)
    }

    fn shapes(&self, since: i64, until: i64) -> Result<Vec<ContextShape>> {
        let mut stmt = self.conn.prepare(
            "SELECT s.value_a, s.value_b, d.value_a, d.value_b
             FROM metric_events s JOIN metric_events d
               ON d.ticket_id = s.ticket_id AND d.kind = 'context_density'
             WHERE s.kind = 'context_shape' AND s.at >= ?1 AND s.at < ?2",
        )?;
        let rows = stmt.query_map(params![since, until], |r| {
            Ok(ContextShape {
                depth: r.get::<_, Option<i64>>(0)?.unwrap_or_default(),
                oldest_age_secs: r.get::<_, Option<i64>>(1)?.unwrap_or_default(),
                prior_hour: r.get::<_, Option<i64>>(2)?.unwrap_or_default(),
                prior_day: r.get::<_, Option<i64>>(3)?.unwrap_or_default(),
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    fn runs(&self, since: i64, until: i64) -> Result<Vec<RunSpan>> {
        let mut stmt = self.conn.prepare(
            "SELECT started_at, stopped_at FROM daemon_runs
             WHERE stopped_at >= ?1 AND started_at < ?2 ORDER BY started_at",
        )?;
        let rows = stmt.query_map(params![since, until], |r| {
            Ok(RunSpan {
                started_at: r.get(0)?,
                stopped_at: r.get(1)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    fn update_lags(&self, since: i64, until: i64) -> Result<Vec<i64>> {
        let mut stmt = self.conn.prepare(
            "SELECT max(received_at - date, 0) FROM messages
             WHERE live = 1 AND outgoing = 0 AND received_at >= ?1 AND received_at < ?2",
        )?;
        let rows = stmt.query_map(params![since, until], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    fn api_events(&self, since: i64, until: i64) -> Result<Vec<ApiEvent>> {
        let mut stmt = self.conn.prepare(
            "SELECT kind, coalesce(tag, '?'), coalesce(value_a, 0) FROM metric_events
             WHERE kind IN ('flood_wait', 'rpc_error') AND at >= ?1 AND at < ?2",
        )?;
        let rows = stmt.query_map(params![since, until], |r| {
            let kind: String = r.get(0)?;
            Ok(ApiEvent {
                kind: if kind == "flood_wait" {
                    ApiTrouble::FloodWait
                } else {
                    ApiTrouble::RpcError
                },
                method: r.get(1)?,
                value: r.get(2)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    fn store_sizes(&self, since: i64, until: i64) -> Result<Vec<(i64, i64)>> {
        let mut stmt = self.conn.prepare(
            "SELECT at, value_a FROM metric_events
             WHERE kind = 'store_size' AND at >= ?1 AND at < ?2 ORDER BY at",
        )?;
        let rows = stmt.query_map(params![since, until], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    fn candidates(&self, since: i64, until: i64) -> Result<Vec<Candidate>> {
        let mut stmt = self.conn.prepare(
            "SELECT chat_id, max(title), sum(would_trigger), sum(messages) FROM gate_drops
             WHERE hour >= ?1 AND hour < ?2 GROUP BY chat_id HAVING sum(would_trigger) > 0",
        )?;
        let rows = stmt.query_map(
            params![
                since.div_euclid(SECS_PER_HOUR),
                until.div_euclid(SECS_PER_HOUR) + 1
            ],
            |r| {
                Ok(Candidate {
                    chat: ChatId(r.get(0)?),
                    title: r.get(1)?,
                    would_trigger: crate::count(r.get(2)?),
                    messages: crate::count(r.get(3)?),
                })
            },
        )?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::messages::tests::{chat, msg};
    use crate::tickets::tests::new_ticket;
    use crate::{MetricEvent, MetricKind};
    use captain_core::ChatKind;
    use captain_inbox::{Policy, report};

    #[test]
    fn report_input_reflects_stored_facts() {
        let store = Store::open_in_memory().unwrap();
        let c = -1007;
        let run = store.start_run(0).unwrap();
        store.heartbeat(run, 5000).unwrap();
        for (id, date, out, reply) in [
            (1, 100, false, None),
            (2, 110, false, None),
            (3, 200, true, Some(2)),
            (4, 1000, false, None),
            (5, 1100, true, None),
        ] {
            let mut m = msg(c, id, date, out);
            m.reply_to = reply.map(MessageId);
            store.put_message(&m, date + 1, true).unwrap();
        }
        store.create_ticket(&new_ticket(c, 2, 110)).unwrap();
        store
            .count_gate_drop(&chat(-9, ChatKind::Group), true, 500)
            .unwrap();
        store
            .record(&MetricEvent {
                tag: Some("messages.getMessages".into()),
                a: Some(30),
                ..MetricEvent::new(MetricKind::FloodWait, 300)
            })
            .unwrap();

        let input = store.report_input(0, 5000).unwrap();
        assert_eq!(input.tickets.len(), 1);
        assert_eq!(input.inbound.iter().map(|c| c.count).sum::<usize>(), 3);
        assert_eq!(input.burst_gaps, [("group".to_owned(), 10)]);
        // Message 2 was answered by reply and has a ticket; message 4 by proximity, without.
        let covered: Vec<bool> = input.answered.iter().map(|a| a.covered).collect();
        assert_eq!(covered.len(), 2);
        assert_eq!(covered.iter().filter(|c| **c).count(), 1);
        assert_eq!(input.update_lags_secs, [1, 1, 1]);
        assert_eq!(input.candidates.len(), 1);
        assert_eq!(input.api.len(), 1);
        let built = report::build(&input, &Policy::default());
        assert_eq!(built.sections.len(), 7);
    }
}
