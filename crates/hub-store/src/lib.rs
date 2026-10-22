//! `SQLite` tables for every hub source, in the same file Captain already uses.
//!
//! Captain-store owns `PRAGMA user_version` and the inbox tables. This crate adds its own
//! tables and records their version in `hub_schema`. Counters, maps and health rows store ids
//! and counts. Message text is never written here.
//!
//! [`Network`], [`ConversationId`], [`ModuleHealth`] and [`RemoteEvent`] are the `hub-core`
//! types. [`RemoteEvent`] can carry a message body; nothing in this crate reads that field.
//! Counters are recorded through [`CountedMessage`], which has no text field.

mod error;
pub mod limits;
mod maps;
mod read;
mod schema;
mod vocab;
mod write;

pub use error::{Error, Result};
pub use hub_core::{
    ConversationId, ConversationKind, ModuleHealth, ModuleState, Network, RemoteEvent,
};
pub use maps::{MessageLink, StoredFailure, StoredTopic};
pub use read::{
    Conversation, CounterRow, EventStream, FailureRow, LevelCount, LogRow, Reader, TicketSnap,
};
pub use vocab::{ContentKind, Direction, LogLevel};
pub use write::{CountedMessage, NewConversation, NewFailure, NewLogEvent, PrunedEvents};

use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use rusqlite::{Connection, OpenFlags};

use crate::limits::BUSY_TIMEOUT_SECS;

/// An open hub database. The same file also holds Captain's tables.
#[derive(Debug)]
pub struct Hub {
    conn: Connection,
}

impl Hub {
    /// Opens `path`, applies any pending Captain migrations, then applies hub migrations.
    ///
    /// Captain's `user_version` moves only when `captain_store::Store::open` itself migrates.
    /// This function never writes that pragma. An existing database keeps its rows.
    ///
    /// # Errors
    ///
    /// [`Error::Captain`] when Captain cannot open or migrate the file, [`Error::Sqlite`] when
    /// the hub tables cannot be created, and [`Error::NewerSchema`] when `hub_schema` is newer
    /// than this build.
    pub fn open(path: &Path) -> Result<Self> {
        captain_store::Store::open(path)?;
        let conn = Connection::open(path)?;
        init(conn, true)
    }

    /// Opens an existing hub database for reads. The file is not created and is not migrated.
    ///
    /// Captain's `user_version` and `hub_schema` are checked. The connection is `SQLite`
    /// read-only with `query_only`, so this process cannot write hub tables.
    ///
    /// # Errors
    ///
    /// [`Error::Captain`] when the file is missing or Captain's schema is newer than this
    /// build, [`Error::NewerSchema`] when `hub_schema` is newer, and [`Error::Sqlite`] when
    /// the file cannot be opened.
    pub fn open_read_only(path: &Path) -> Result<Self> {
        let _checked = captain_store::Store::open_read_only(path)?;
        drop(_checked);
        let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        conn.busy_timeout(Duration::from_secs(BUSY_TIMEOUT_SECS))?;
        conn.pragma_update(None, "query_only", true)?;
        conn.pragma_update(None, "foreign_keys", true)?;
        schema::refuse_newer(&conn)?;
        Ok(Hub { conn })
    }

    /// An empty database in memory, with Captain's schema and the hub tables, for tests.
    ///
    /// # Errors
    ///
    /// [`Error::Sqlite`] when `SQLite` cannot allocate it, [`Error::Captain`] or [`Error::Io`]
    /// when the Captain schema cannot be copied in.
    pub fn open_in_memory() -> Result<Self> {
        let mut conn = Connection::open_in_memory()?;
        schema::apply_captain_schema(&mut conn)?;
        init(conn, false)
    }

    /// A read-only view of this connection.
    pub fn reader(&self) -> Reader<'_> {
        Reader::new(&self.conn)
    }

    /// Version in `hub_schema`, or 0 when that table has no row.
    ///
    /// # Errors
    ///
    /// [`Error::Sqlite`] on a read failure.
    pub fn schema_version(&self) -> Result<i64> {
        schema::hub_version(&self.conn)
    }

    /// Captain's `PRAGMA user_version`. Hub migrations do not change it.
    ///
    /// # Errors
    ///
    /// [`Error::Sqlite`] on a read failure.
    pub fn captain_user_version(&self) -> Result<i64> {
        schema::captain_version(&self.conn)
    }
}

/// A [`Hub`] that can be shared across tasks.
///
/// `rusqlite::Connection` is `Send` and not `Sync`. The mutex is the share handle: one
/// connection, one writer at a time. Callers that sit on the async runtime should run the
/// locked call inside `spawn_blocking`, because `SQLite` blocks. A worker thread would
/// serialize the same calls and add a channel. The mutex matches the daemon store.
///
/// A poisoned lock is recovered. The rows are still there after a caller panicked.
#[derive(Clone, Debug)]
pub struct SharedHub {
    inner: Arc<Mutex<Hub>>,
}

impl SharedHub {
    /// Wraps an open [`Hub`].
    pub fn new(hub: Hub) -> Self {
        Self {
            inner: Arc::new(Mutex::new(hub)),
        }
    }

    /// Opens `path` and wraps the connection.
    ///
    /// # Errors
    ///
    /// Same as [`Hub::open`].
    pub fn open(path: &Path) -> Result<Self> {
        Ok(Self::new(Hub::open(path)?))
    }

    /// An in-memory hub, for tests.
    ///
    /// # Errors
    ///
    /// Same as [`Hub::open_in_memory`].
    pub fn open_in_memory() -> Result<Self> {
        Ok(Self::new(Hub::open_in_memory()?))
    }

    /// Locks the connection. A poisoned lock is recovered.
    pub fn lock(&self) -> MutexGuard<'_, Hub> {
        self.inner.lock().unwrap_or_else(|err| err.into_inner())
    }
}

fn init(mut conn: Connection, file_backed: bool) -> Result<Hub> {
    conn.busy_timeout(Duration::from_secs(BUSY_TIMEOUT_SECS))?;
    conn.pragma_update(None, "foreign_keys", true)?;
    if file_backed {
        conn.pragma_update(None, "journal_mode", "WAL")?;
    }
    schema::migrate(&mut conn)?;
    Ok(Hub { conn })
}

#[cfg(test)]
mod tests;
