//! Local `SQLite` store: scoped messages, tickets, metric events and daemon runs.
//!
//! Telegram stays the source of truth for messages; this is a cache plus a ledger of what the
//! pipeline did. Only the scope gate decides what reaches `messages`. Metric tables never hold
//! message text. Every call is blocking, so async callers run it on a blocking thread.

mod error;
mod messages;
mod metrics;
mod report;
mod schema;
mod tickets;

use std::path::Path;
use std::time::Duration;

use rusqlite::{Connection, OpenFlags, OptionalExtension, params};

pub use error::{Error, Result};
pub use messages::StoredMessage;
pub use metrics::{MetricEvent, MetricKind};
pub use tickets::{
    NewTicket, PendingTicket, TicketDetail, TicketEvent, TicketFilter, TicketMessage, TicketRow,
};

const PRIVATE_FILE_MODE: u32 = 0o600;
const PRIVATE_DIR_MODE: u32 = 0o700;
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);
const PAUSED_KEY: &str = "paused";

/// An open store.
#[derive(Debug)]
pub struct Store {
    conn: Connection,
}

impl Store {
    /// Opens or creates the store at `path`, owner-only, and migrates it to the current schema.
    ///
    /// # Errors
    ///
    /// [`Error::Io`] when the file cannot be created, [`Error::Sqlite`] when it cannot be opened
    /// or migrated, and [`Error::NewerSchema`] when a newer build already migrated it.
    pub fn open(path: &Path) -> Result<Self> {
        use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};

        let io = |source| Error::Io {
            path: path.to_owned(),
            source,
        };
        if let Some(dir) = path.parent() {
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(PRIVATE_DIR_MODE)
                .create(dir)
                .map_err(io)?;
        }
        // SECURITY: create the file owner-only before SQLite does; its -wal and -shm files copy
        // these permissions.
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .mode(PRIVATE_FILE_MODE)
            .open(path)
            .map_err(io)?;
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        Self::init(conn)
    }

    /// Opens an existing store for reads. The file is not created and is not migrated.
    ///
    /// The connection is `SQLite` read-only with `query_only`, so this process cannot write
    /// tickets, metrics, or settings. A busy timeout waits while another process holds a write
    /// lock.
    ///
    /// # Errors
    ///
    /// [`Error::NotFound`] when `path` does not exist, [`Error::NewerSchema`] when a newer
    /// build already migrated it, and [`Error::Sqlite`] when it cannot be opened.
    pub fn open_read_only(path: &Path) -> Result<Self> {
        if !path.is_file() {
            return Err(Error::NotFound(path.to_owned()));
        }
        let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        conn.busy_timeout(BUSY_TIMEOUT)?;
        conn.pragma_update(None, "query_only", true)?;
        conn.pragma_update(None, "foreign_keys", true)?;
        schema::refuse_newer(&conn)?;
        Ok(Self { conn })
    }

    /// An empty store in memory, for tests.
    ///
    /// # Errors
    ///
    /// [`Error::Sqlite`] if `SQLite` cannot allocate it.
    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.busy_timeout(BUSY_TIMEOUT)?;
        conn.pragma_update(None, "foreign_keys", true)?;
        schema::migrate(&conn)?;
        Ok(Self { conn })
    }

    /// Whether the pipeline is paused. Survives restarts.
    ///
    /// # Errors
    ///
    /// [`Error::Sqlite`] on a read failure.
    pub fn paused(&self) -> Result<bool> {
        let value: Option<String> = self
            .conn
            .query_row(
                "SELECT value FROM settings WHERE key = ?1",
                [PAUSED_KEY],
                |r| r.get(0),
            )
            .optional()?;
        Ok(value.as_deref() == Some("1"))
    }

    /// Pauses or resumes the pipeline.
    ///
    /// # Errors
    ///
    /// [`Error::Sqlite`] on a write failure.
    pub fn set_paused(&self, paused: bool) -> Result<()> {
        self.conn.execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2)
             ON CONFLICT (key) DO UPDATE SET value = excluded.value",
            params![PAUSED_KEY, if paused { "1" } else { "0" }],
        )?;
        Ok(())
    }

    /// Records the start of a daemon run and returns its id.
    ///
    /// # Errors
    ///
    /// [`Error::Sqlite`] on a write failure.
    pub fn start_run(&self, now: i64) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO daemon_runs (started_at, stopped_at) VALUES (?1, ?1)",
            [now],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Extends a run to `now`. Called periodically, so a crash still leaves a close end time.
    ///
    /// # Errors
    ///
    /// [`Error::Sqlite`] on a write failure.
    pub fn heartbeat(&self, run: i64, now: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE daemon_runs SET stopped_at = ?2 WHERE run_id = ?1",
            params![run, now],
        )?;
        Ok(())
    }

    /// Ends a run with a reason such as `signal` or `unauthorized`.
    ///
    /// # Errors
    ///
    /// [`Error::Sqlite`] on a write failure.
    pub fn stop_run(&self, run: i64, now: i64, reason: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE daemon_runs SET stopped_at = ?2, stop_reason = ?3 WHERE run_id = ?1",
            params![run, now, reason],
        )?;
        Ok(())
    }

    /// End time of the most recent run before `run`, to measure how long the daemon was away.
    ///
    /// # Errors
    ///
    /// [`Error::Sqlite`] on a read failure.
    pub fn previous_run_end(&self, run: i64) -> Result<Option<i64>> {
        Ok(self
            .conn
            .query_row(
                "SELECT stopped_at FROM daemon_runs WHERE run_id < ?1
                 ORDER BY run_id DESC LIMIT 1",
                [run],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// Database size in bytes.
    ///
    /// # Errors
    ///
    /// [`Error::Sqlite`] on a read failure.
    pub fn size_bytes(&self) -> Result<i64> {
        Ok(self.conn.query_row(
            "SELECT page_count * page_size FROM pragma_page_count(), pragma_page_size()",
            [],
            |r| r.get(0),
        )?)
    }

    /// Deletes messages older than `message_days`, and tickets, metric events and runs older
    /// than `metric_days`. Returns the number of messages deleted.
    ///
    /// # Errors
    ///
    /// [`Error::Sqlite`] on a write failure.
    pub fn apply_retention(&self, now: i64, message_days: i64, metric_days: i64) -> Result<usize> {
        const SECS_PER_DAY: i64 = 86_400;
        const SECS_PER_HOUR: i64 = 3600;
        let message_cutoff = now - message_days * SECS_PER_DAY;
        let metric_cutoff = now - metric_days * SECS_PER_DAY;
        let tx = self.conn.unchecked_transaction()?;
        let deleted = tx.execute(
            "DELETE FROM messages WHERE received_at < ?1",
            [message_cutoff],
        )?;
        tx.execute(
            "DELETE FROM gate_drops WHERE hour < ?1",
            [message_cutoff.div_euclid(SECS_PER_HOUR)],
        )?;
        tx.execute("DELETE FROM tickets WHERE opened_at < ?1", [metric_cutoff])?;
        tx.execute("DELETE FROM metric_events WHERE at < ?1", [metric_cutoff])?;
        tx.execute(
            "DELETE FROM daemon_runs WHERE stopped_at < ?1",
            [metric_cutoff],
        )?;
        tx.commit()?;
        Ok(deleted)
    }
}

/// Converts a `SQLite` count to `usize`; counts are never negative.
pub(crate) fn count(n: i64) -> usize {
    usize::try_from(n).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pause_flag_and_runs_persist() {
        let store = Store::open_in_memory().unwrap();
        assert!(!store.paused().unwrap());
        store.set_paused(true).unwrap();
        assert!(store.paused().unwrap());

        let first = store.start_run(100).unwrap();
        store.stop_run(first, 150, "signal").unwrap();
        let second = store.start_run(200).unwrap();
        assert_eq!(store.previous_run_end(second).unwrap(), Some(150));
        assert!(store.size_bytes().unwrap() > 0);
    }

    #[test]
    fn opens_a_private_file_and_reopens_it() {
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir().join(format!("captain-store-{}", std::process::id()));
        let path = dir.join("inbox.sqlite");
        Store::open(&path).unwrap().set_paused(true).unwrap();
        assert!(Store::open(&path).unwrap().paused().unwrap());
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, PRIVATE_FILE_MODE);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn read_only_open_does_not_create_or_migrate_and_rejects_writes() {
        let dir = std::env::temp_dir().join(format!(
            "captain-store-ro-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.as_nanos())
        ));
        let missing = dir.join("missing").join("inbox.sqlite");
        assert!(matches!(
            Store::open_read_only(&missing),
            Err(Error::NotFound(_))
        ));
        assert!(!missing.exists());
        assert!(!dir.join("missing").exists());

        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("inbox.sqlite");
        let writer = Store::open(&path).unwrap();
        writer.set_paused(true).unwrap();
        let before = std::fs::read(&path).unwrap();
        let reader = Store::open_read_only(&path).unwrap();
        assert!(reader.paused().unwrap());
        assert!(reader.set_paused(false).is_err());
        assert!(writer.paused().unwrap());
        drop(reader);
        assert_eq!(std::fs::read(&path).unwrap(), before);

        let conn = Connection::open(&path).unwrap();
        conn.pragma_update(None, "user_version", 99).unwrap();
        drop(conn);
        assert!(matches!(
            Store::open_read_only(&path),
            Err(Error::NewerSchema { found: 99, .. })
        ));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
