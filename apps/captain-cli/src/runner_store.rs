//! Dedicated `SQLite` ledger for Phase 2 AI runner runs and review-group message mappings.
//!
//! Stored in `runner.sqlite` alongside `inbox.sqlite` so Phase 1 readers (including read-only
//! dashboard attaches) stay compatible with schema version 1.

use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use captain_core::{ChatId, MessageId};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

const PRIVATE_FILE_MODE: u32 = 0o600;
const PRIVATE_DIR_MODE: u32 = 0o700;
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_FAILED_ATTEMPTS: i64 = 2; // one automatic retry after a transient failure

const SCHEMA: &str = r"
CREATE TABLE IF NOT EXISTS ticket_runs (
    run_id          INTEGER PRIMARY KEY,
    ticket_id       INTEGER NOT NULL,
    chat_id         INTEGER NOT NULL,
    topic_id        INTEGER,
    anchor_msg_id   INTEGER NOT NULL,
    chat_label      TEXT NOT NULL,
    started_at      INTEGER NOT NULL,
    finished_at     INTEGER,
    engine          TEXT NOT NULL,
    model           TEXT NOT NULL,
    status          TEXT NOT NULL,
    decision        TEXT,
    confidence      TEXT,
    auto_send_safe  INTEGER NOT NULL DEFAULT 0,
    owner           TEXT,
    handoff_to      TEXT,
    summary         TEXT,
    reply_text      TEXT,
    reason          TEXT,
    review_chat_id  INTEGER,
    review_msg_id   INTEGER,
    sent_msg_id     INTEGER,
    error           TEXT
);
CREATE INDEX IF NOT EXISTS idx_ticket_runs_ticket ON ticket_runs (ticket_id);
CREATE UNIQUE INDEX IF NOT EXISTS idx_ticket_runs_review
    ON ticket_runs (review_chat_id, review_msg_id)
    WHERE review_msg_id IS NOT NULL;
";

/// Summary of one runner execution, printed by `captain runs`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RunRow {
    pub run_id: i64,
    pub ticket_id: i64,
    pub chat_id: i64,
    pub topic_id: Option<i32>,
    pub anchor_msg_id: i32,
    pub chat_label: String,
    pub started_at: i64,
    pub finished_at: Option<i64>,
    pub engine: String,
    pub model: String,
    pub status: String,
    pub decision: Option<String>,
    pub confidence: Option<String>,
    pub auto_send_safe: bool,
    pub owner: Option<String>,
    pub handoff_to: Option<String>,
    pub summary: Option<String>,
    pub reply_text: Option<String>,
    pub reason: Option<String>,
    pub review_chat_id: Option<i64>,
    pub review_msg_id: Option<i32>,
    pub sent_msg_id: Option<i32>,
    pub error: Option<String>,
}

/// Fields recorded when starting a runner execution.
#[derive(Debug, Clone)]
pub struct NewRun<'a> {
    pub ticket_id: i64,
    pub chat: ChatId,
    pub topic: Option<MessageId>,
    pub anchor: MessageId,
    pub chat_label: &'a str,
    pub started_at: i64,
    pub engine: &'a str,
    pub model: &'a str,
}

/// Fields persisted when a run completes normally.
#[derive(Debug, Clone)]
pub struct RunOutcome {
    pub finished_at: i64,
    pub status: String,
    pub decision: String,
    pub confidence: String,
    pub auto_send_safe: bool,
    pub owner: String,
    pub handoff_to: String,
    pub summary: String,
    pub reply_text: String,
    pub reason: String,
    pub review_chat_id: Option<ChatId>,
    pub review_msg_id: Option<MessageId>,
    pub sent_msg_id: Option<MessageId>,
}

/// Handle to `runner.sqlite`.
pub struct RunnerStore {
    conn: Connection,
}

impl RunnerStore {
    /// Opens or creates `runner.sqlite` with mode `0600`.
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(PRIVATE_DIR_MODE)
                .create(dir)
                .with_context(|| format!("cannot create {}", dir.display()))?;
        }
        // SECURITY: create owner-only before SQLite opens it so WAL/SHM inherit 0600.
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .mode(PRIVATE_FILE_MODE)
            .open(path)
            .with_context(|| format!("cannot open {}", path.display()))?;
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        Self::init(conn)
    }

    #[cfg(test)]
    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.busy_timeout(BUSY_TIMEOUT)?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self { conn })
    }

    /// True if `ticket_id` already succeeded, is awaiting review, is running, or exhausted retries.
    pub fn should_skip_ticket(&self, ticket_id: i64) -> Result<bool> {
        let active_or_done: i64 = self.conn.query_row(
            "SELECT count(*) FROM ticket_runs
             WHERE ticket_id = ?1
               AND status IN ('running', 'review_pending', 'auto_sent', 'sent_as_is', 'sent_edited', 'declined', 'skipped')",
            [ticket_id],
            |r| r.get(0),
        )?;
        if active_or_done > 0 {
            return Ok(true);
        }
        let failed: i64 = self.conn.query_row(
            "SELECT count(*) FROM ticket_runs WHERE ticket_id = ?1 AND status = 'failed'",
            [ticket_id],
            |r| r.get(0),
        )?;
        Ok(failed >= MAX_FAILED_ATTEMPTS)
    }

    /// Returns ticket ids explicitly marked `retry` by the owner in the review group.
    pub fn retry_ticket_ids(&self) -> Result<Vec<i64>> {
        let mut stmt = self.conn.prepare(
            "SELECT DISTINCT ticket_id FROM ticket_runs WHERE status = 'retry' ORDER BY run_id ASC",
        )?;
        let rows = stmt.query_map([], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Clears pending `retry` markers for `ticket_id` once a new run starts.
    pub fn clear_retry_markers(&self, ticket_id: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE ticket_runs SET status = 'retried' WHERE ticket_id = ?1 AND status = 'retry'",
            [ticket_id],
        )?;
        Ok(())
    }

    /// Number of runs started since `since_unix`.
    pub fn runs_since(&self, since_unix: i64) -> Result<usize> {
        let count: i64 = self.conn.query_row(
            "SELECT count(*) FROM ticket_runs WHERE started_at >= ?1",
            [since_unix],
            |r| r.get(0),
        )?;
        Ok(usize::try_from(count).unwrap_or(0))
    }

    /// Inserts a `running` record and returns its `run_id`.
    pub fn begin_run(&self, run: &NewRun<'_>) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO ticket_runs (
                ticket_id, chat_id, topic_id, anchor_msg_id, chat_label,
                started_at, engine, model, status
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'running')",
            params![
                run.ticket_id,
                run.chat.0,
                run.topic.map(|t| t.0),
                run.anchor.0,
                run.chat_label,
                run.started_at,
                run.engine,
                run.model,
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Saves the structured outcome and optional review/sent message ids.
    pub fn finish_run(&self, run_id: i64, outcome: &RunOutcome) -> Result<()> {
        self.conn.execute(
            "UPDATE ticket_runs
             SET finished_at = ?2, status = ?3, decision = ?4, confidence = ?5,
                 auto_send_safe = ?6, owner = ?7, handoff_to = ?8, summary = ?9,
                 reply_text = ?10, reason = ?11, review_chat_id = ?12,
                 review_msg_id = ?13, sent_msg_id = ?14, error = NULL
             WHERE run_id = ?1",
            params![
                run_id,
                outcome.finished_at,
                outcome.status,
                outcome.decision,
                outcome.confidence,
                i64::from(outcome.auto_send_safe),
                outcome.owner,
                outcome.handoff_to,
                outcome.summary,
                outcome.reply_text,
                outcome.reason,
                outcome.review_chat_id.map(|c| c.0),
                outcome.review_msg_id.map(|m| m.0),
                outcome.sent_msg_id.map(|m| m.0),
            ],
        )?;
        Ok(())
    }

    /// Marks a run as `failed` with its error message.
    pub fn fail_run(&self, run_id: i64, finished_at: i64, error: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE ticket_runs SET finished_at = ?2, status = 'failed', error = ?3 WHERE run_id = ?1",
            params![run_id, finished_at, error],
        )?;
        Ok(())
    }

    /// Looks up the run linked to a posted review card in the review group.
    pub fn lookup_by_review_msg(
        &self,
        review_chat: ChatId,
        review_msg: MessageId,
    ) -> Result<Option<RunRow>> {
        Ok(self
            .conn
            .query_row(
                "SELECT run_id, ticket_id, chat_id, topic_id, anchor_msg_id, chat_label,
                        started_at, finished_at, engine, model, status, decision, confidence,
                        auto_send_safe, owner, handoff_to, summary, reply_text, reason,
                        review_chat_id, review_msg_id, sent_msg_id, error
                 FROM ticket_runs
                 WHERE review_chat_id = ?1 AND review_msg_id = ?2",
                params![review_chat.0, review_msg.0],
                row_from_sql,
            )
            .optional()?)
    }

    /// Updates a run after the owner replies `send`, `send: ...`, `skip`, or `retry`.
    pub fn update_status(
        &self,
        run_id: i64,
        status: &str,
        sent_msg_id: Option<MessageId>,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE ticket_runs
             SET status = ?2, sent_msg_id = coalesce(?3, sent_msg_id)
             WHERE run_id = ?1",
            params![run_id, status, sent_msg_id.map(|m| m.0)],
        )?;
        Ok(())
    }

    /// Lists the newest `limit` runs.
    pub fn recent_runs(&self, limit: usize) -> Result<Vec<RunRow>> {
        let cap = i64::try_from(limit).unwrap_or(i64::MAX);
        let mut stmt = self.conn.prepare(
            "SELECT run_id, ticket_id, chat_id, topic_id, anchor_msg_id, chat_label,
                    started_at, finished_at, engine, model, status, decision, confidence,
                    auto_send_safe, owner, handoff_to, summary, reply_text, reason,
                    review_chat_id, review_msg_id, sent_msg_id, error
             FROM ticket_runs
             ORDER BY run_id DESC
             LIMIT ?1",
        )?;
        let rows = stmt.query_map([cap], row_from_sql)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
}

fn row_from_sql(r: &rusqlite::Row<'_>) -> rusqlite::Result<RunRow> {
    let safe_int: i64 = r.get("auto_send_safe")?;
    Ok(RunRow {
        run_id: r.get("run_id")?,
        ticket_id: r.get("ticket_id")?,
        chat_id: r.get("chat_id")?,
        topic_id: r.get("topic_id")?,
        anchor_msg_id: r.get("anchor_msg_id")?,
        chat_label: r.get("chat_label")?,
        started_at: r.get("started_at")?,
        finished_at: r.get("finished_at")?,
        engine: r.get("engine")?,
        model: r.get("model")?,
        status: r.get("status")?,
        decision: r.get("decision")?,
        confidence: r.get("confidence")?,
        auto_send_safe: safe_int != 0,
        owner: r.get("owner")?,
        handoff_to: r.get("handoff_to")?,
        summary: r.get("summary")?,
        reply_text: r.get("reply_text")?,
        reason: r.get("reason")?,
        review_chat_id: r.get("review_chat_id")?,
        review_msg_id: r.get("review_msg_id")?,
        sent_msg_id: r.get("sent_msg_id")?,
        error: r.get("error")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tracks_run_lifecycle_and_review_lookup() {
        let store = RunnerStore::open_in_memory().unwrap();
        assert!(!store.should_skip_ticket(10).unwrap());

        let run_id = store
            .begin_run(&NewRun {
                ticket_id: 10,
                chat: ChatId(-100),
                topic: None,
                anchor: MessageId(55),
                chat_label: "Fleet",
                started_at: 1000,
                engine: "codex",
                model: "gpt-6.2-sol",
            })
            .unwrap();
        assert!(store.should_skip_ticket(10).unwrap());
        assert_eq!(store.runs_since(900).unwrap(), 1);

        store
            .finish_run(
                run_id,
                &RunOutcome {
                    finished_at: 1020,
                    status: "review_pending".into(),
                    decision: "draft".into(),
                    confidence: "high".into(),
                    auto_send_safe: true,
                    owner: "self".into(),
                    handoff_to: String::new(),
                    summary: "Checked order".into(),
                    reply_text: "Dạ ok anh ạ".into(),
                    reason: "Verified".into(),
                    review_chat_id: Some(ChatId(-200)),
                    review_msg_id: Some(MessageId(99)),
                    sent_msg_id: None,
                },
            )
            .unwrap();

        let found = store
            .lookup_by_review_msg(ChatId(-200), MessageId(99))
            .unwrap()
            .unwrap();
        assert_eq!(found.ticket_id, 10);
        assert_eq!(found.reply_text.as_deref(), Some("Dạ ok anh ạ"));

        store
            .update_status(run_id, "sent_as_is", Some(MessageId(77)))
            .unwrap();
        let rows = store.recent_runs(5).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].status, "sent_as_is");
        assert_eq!(rows[0].sent_msg_id, Some(77));
    }
}
