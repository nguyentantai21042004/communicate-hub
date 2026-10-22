//! Hub tables and their version. Captain's `PRAGMA user_version` is not written here.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::Connection;

use crate::Error;
use crate::error::Result;
use crate::limits::HUB_SCHEMA_VERSION;

const MIGRATION_V1: &str = "
CREATE TABLE conversations (
    id            INTEGER PRIMARY KEY,
    network       TEXT NOT NULL,
    kind          TEXT NOT NULL,
    external_id   TEXT NOT NULL,
    title         TEXT,
    topic_id      INTEGER,
    muted         INTEGER NOT NULL,
    first_seen    INTEGER NOT NULL,
    last_activity INTEGER NOT NULL,
    tags          TEXT,
    UNIQUE (network, kind, external_id)
);
CREATE UNIQUE INDEX conversations_topic
    ON conversations (topic_id) WHERE topic_id IS NOT NULL;
CREATE INDEX conversations_activity ON conversations (last_activity);

CREATE TABLE message_counters (
    day             INTEGER NOT NULL,
    hour            INTEGER NOT NULL,
    conversation_id INTEGER NOT NULL REFERENCES conversations (id),
    direction       TEXT NOT NULL,
    content_kind    TEXT NOT NULL,
    count           INTEGER NOT NULL,
    PRIMARY KEY (day, hour, conversation_id, direction, content_kind)
);
CREATE INDEX message_counters_day ON message_counters (day);

CREATE TABLE topic_map (
    conversation_id   INTEGER PRIMARY KEY REFERENCES conversations (id),
    telegram_topic_id INTEGER NOT NULL UNIQUE,
    created_at        INTEGER NOT NULL
);

CREATE TABLE message_map (
    id                  INTEGER PRIMARY KEY,
    network             TEXT NOT NULL,
    network_message_id  TEXT NOT NULL,
    telegram_message_id INTEGER NOT NULL,
    created_at          INTEGER NOT NULL,
    UNIQUE (network, network_message_id)
);
CREATE INDEX message_map_age ON message_map (created_at, id);

CREATE TABLE bridge_events (
    id     INTEGER PRIMARY KEY,
    ts     INTEGER NOT NULL,
    module TEXT NOT NULL,
    level  TEXT NOT NULL,
    kind   TEXT NOT NULL,
    detail TEXT
);
CREATE INDEX bridge_events_ts ON bridge_events (ts);

CREATE TABLE module_events (
    id     INTEGER PRIMARY KEY,
    ts     INTEGER NOT NULL,
    module TEXT NOT NULL,
    level  TEXT NOT NULL,
    kind   TEXT NOT NULL,
    detail TEXT
);
CREATE INDEX module_events_ts ON module_events (ts);

CREATE TABLE failed_events (
    id              INTEGER PRIMARY KEY,
    conversation_id INTEGER REFERENCES conversations (id),
    reason          TEXT NOT NULL,
    retryable       INTEGER NOT NULL,
    created_at      INTEGER NOT NULL,
    resolved_at     INTEGER
);
CREATE INDEX failed_events_open ON failed_events (resolved_at, conversation_id);

CREATE TABLE module_health (
    module     TEXT PRIMARY KEY,
    state      TEXT NOT NULL,
    detail     TEXT,
    updated_at INTEGER NOT NULL
);
";

const MIGRATION_V2: &str = "
CREATE TABLE IF NOT EXISTS user_names (
    scope        TEXT NOT NULL,
    name_key     TEXT NOT NULL,
    user_id      TEXT NOT NULL,
    display_name TEXT NOT NULL,
    PRIMARY KEY (scope, name_key)
);

CREATE TABLE IF NOT EXISTS bridge_retries (
    id INTEGER PRIMARY KEY,
    at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS bridge_retries_at ON bridge_retries (at);
CREATE INDEX IF NOT EXISTS message_map_telegram ON message_map (telegram_message_id);
";

const MESSAGE_MAP_CONVERSATION: &str = "conversation_id";

pub(crate) fn migrate(conn: &mut Connection) -> Result<()> {
    let tx = conn.transaction()?;
    tx.execute_batch("CREATE TABLE IF NOT EXISTS hub_schema (version INTEGER NOT NULL);")?;
    let found: Option<i64> =
        tx.query_row("SELECT max(version) FROM hub_schema", [], |row| row.get(0))?;
    let found = found.unwrap_or(0);
    if found > HUB_SCHEMA_VERSION {
        return Err(Error::NewerSchema {
            found,
            supported: HUB_SCHEMA_VERSION,
        });
    }
    // Each version is applied once. A shipped statement is never edited; a later version gets
    // its own block above this commit.
    if found < 1 {
        tx.execute_batch(MIGRATION_V1)?;
    }
    if found < 2 {
        tx.execute_batch(MIGRATION_V2)?;
        if !column_exists(&tx, MESSAGE_MAP_CONVERSATION)? {
            tx.execute(
                "ALTER TABLE message_map ADD COLUMN conversation_id INTEGER REFERENCES conversations (id)",
                [],
            )?;
        }
    }
    if found < HUB_SCHEMA_VERSION {
        tx.execute("DELETE FROM hub_schema", [])?;
        tx.execute(
            "INSERT INTO hub_schema (version) VALUES (?1)",
            [HUB_SCHEMA_VERSION],
        )?;
    }
    tx.commit()?;
    Ok(())
}

fn column_exists(conn: &Connection, column: &str) -> Result<bool> {
    let mut stmt = conn.prepare("PRAGMA table_info(message_map)")?;
    let names = stmt.query_map([], |row| row.get::<_, String>(1))?;
    for name in names {
        if name? == column {
            return Ok(true);
        }
    }
    Ok(false)
}

pub(crate) fn hub_version(conn: &Connection) -> Result<i64> {
    let found: Option<i64> =
        conn.query_row("SELECT max(version) FROM hub_schema", [], |row| row.get(0))?;
    Ok(found.unwrap_or(0))
}

pub(crate) fn captain_version(conn: &Connection) -> Result<i64> {
    Ok(conn.pragma_query_value(None, "user_version", |row| row.get(0))?)
}

/// Copies the schema `captain_store::Store::open` creates into an empty connection.
///
/// The copy includes captain's `user_version` so an in-memory hub looks like a file captain
/// just migrated. `migrate` does not write that pragma again.
pub(crate) fn apply_captain_schema(conn: &mut Connection) -> Result<()> {
    let guard = DirGuard::create()?;
    let path = guard.0.join("inbox.sqlite");
    let store = captain_store::Store::open(&path)?;
    drop(store);
    let src = Connection::open(&path)?;
    let version = captain_version(&src)?;
    let mut stmt = src.prepare(
        "SELECT sql FROM sqlite_master
         WHERE sql IS NOT NULL AND name NOT LIKE 'sqlite_%'
         ORDER BY rowid",
    )?;
    let sqls = stmt
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    drop(stmt);
    drop(src);
    for sql in sqls {
        conn.execute(&sql, [])?;
    }
    conn.pragma_update(None, "user_version", version)?;
    Ok(())
}

struct DirGuard(PathBuf);

impl DirGuard {
    fn create() -> Result<Self> {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos());
        let path =
            std::env::temp_dir().join(format!("hub-store-boot-{}-{nanos}-{n}", std::process::id()));
        std::fs::create_dir(&path).map_err(|source| Error::Io {
            path: path.clone(),
            source,
        })?;
        Ok(Self(path))
    }
}

impl Drop for DirGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
