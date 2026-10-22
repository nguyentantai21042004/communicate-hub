//! Schema migrations, tracked with `PRAGMA user_version`.

use rusqlite::Connection;

use crate::{Error, Result};

/// Each entry moves the schema up one version. Never edit an entry that has shipped; add one.
const MIGRATIONS: &[&str] = &[r"
CREATE TABLE settings (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE TABLE chats (
    chat_id     INTEGER PRIMARY KEY,
    kind        TEXT NOT NULL,
    title       TEXT,
    username    TEXT,
    scope_label TEXT,
    scope_kind  TEXT,
    profile     TEXT,
    first_seen  INTEGER NOT NULL,
    last_seen   INTEGER NOT NULL
);

CREATE TABLE messages (
    chat_id         INTEGER NOT NULL,
    msg_id          INTEGER NOT NULL,
    topic_id        INTEGER,
    sender_id       INTEGER,
    sender_is_bot   INTEGER NOT NULL,
    outgoing        INTEGER NOT NULL,
    date            INTEGER NOT NULL,
    received_at     INTEGER NOT NULL,
    live            INTEGER NOT NULL,
    edit_date       INTEGER,
    reply_to_msg_id INTEGER,
    quote_text      TEXT,
    mentioned       INTEGER NOT NULL,
    album_id        INTEGER,
    text            TEXT,
    media_kind      TEXT,
    deleted_at      INTEGER,
    PRIMARY KEY (chat_id, msg_id)
);
CREATE INDEX messages_thread ON messages (chat_id, topic_id, date);
CREATE INDEX messages_id ON messages (msg_id);
CREATE INDEX messages_received ON messages (received_at);

CREATE TABLE daemon_runs (
    run_id      INTEGER PRIMARY KEY,
    started_at  INTEGER NOT NULL,
    stopped_at  INTEGER NOT NULL,
    stop_reason TEXT
);

CREATE TABLE tickets (
    ticket_id      INTEGER PRIMARY KEY,
    chat_id        INTEGER NOT NULL,
    topic_id       INTEGER,
    chat_label     TEXT NOT NULL,
    chat_kind      TEXT,
    profile        TEXT NOT NULL,
    trigger        TEXT NOT NULL,
    trigger_reason TEXT NOT NULL,
    route          TEXT,
    state          TEXT NOT NULL,
    state_reason   TEXT,
    stale          INTEGER NOT NULL,
    note           TEXT,
    opened_at      INTEGER NOT NULL,
    closed_at      INTEGER NOT NULL,
    anchor_msg_id  INTEGER NOT NULL,
    anchor_date    INTEGER NOT NULL,
    message_count  INTEGER NOT NULL,
    resolved_at    INTEGER,
    label_implicit TEXT,
    label_explicit TEXT,
    labelled_at    INTEGER,
    read_at        INTEGER,
    reply_at       INTEGER
);
CREATE INDEX tickets_opened ON tickets (opened_at);
CREATE INDEX tickets_pending ON tickets (state, anchor_date);

CREATE TABLE ticket_messages (
    ticket_id INTEGER NOT NULL REFERENCES tickets ON DELETE CASCADE,
    chat_id   INTEGER NOT NULL,
    msg_id    INTEGER NOT NULL,
    role      TEXT NOT NULL,
    PRIMARY KEY (ticket_id, chat_id, msg_id)
);
CREATE INDEX ticket_messages_msg ON ticket_messages (chat_id, msg_id);

CREATE TABLE ticket_senders (
    ticket_id INTEGER NOT NULL REFERENCES tickets ON DELETE CASCADE,
    sender_id INTEGER NOT NULL,
    PRIMARY KEY (ticket_id, sender_id)
);

CREATE TABLE ticket_events (
    ticket_id  INTEGER NOT NULL REFERENCES tickets ON DELETE CASCADE,
    seq        INTEGER NOT NULL,
    at         INTEGER NOT NULL,
    from_state TEXT,
    to_state   TEXT NOT NULL,
    reason     TEXT,
    PRIMARY KEY (ticket_id, seq)
);

CREATE TABLE metric_events (
    seq       INTEGER PRIMARY KEY,
    at        INTEGER NOT NULL,
    kind      TEXT NOT NULL,
    chat_id   INTEGER,
    topic_id  INTEGER,
    ticket_id INTEGER,
    value_a   INTEGER,
    value_b   INTEGER,
    tag       TEXT
);
CREATE INDEX metric_events_kind ON metric_events (kind, at);

CREATE TABLE gate_drops (
    hour          INTEGER NOT NULL,
    chat_id       INTEGER NOT NULL,
    title         TEXT,
    messages      INTEGER NOT NULL,
    would_trigger INTEGER NOT NULL,
    PRIMARY KEY (hour, chat_id)
);
"];

/// Refuses a file migrated by a newer build. Does not apply migrations.
pub(crate) fn refuse_newer(conn: &Connection) -> Result<()> {
    let supported = i64::try_from(MIGRATIONS.len()).unwrap_or(i64::MAX);
    let found: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
    if found > supported {
        return Err(Error::NewerSchema { found, supported });
    }
    Ok(())
}

pub(crate) fn migrate(conn: &Connection) -> Result<()> {
    refuse_newer(conn)?;
    let found: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
    for (version, sql) in (1..)
        .zip(MIGRATIONS)
        .skip(usize::try_from(found).unwrap_or(0))
    {
        let tx = conn.unchecked_transaction()?;
        tx.execute_batch(sql)?;
        tx.pragma_update(None, "user_version", version)?;
        tx.commit()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrating_twice_is_a_no_op_and_newer_files_are_refused() {
        let conn = Connection::open_in_memory().unwrap();
        migrate(&conn).unwrap();
        migrate(&conn).unwrap();
        conn.pragma_update(None, "user_version", 99).unwrap();
        assert!(matches!(
            migrate(&conn),
            Err(Error::NewerSchema { found: 99, .. })
        ));
    }
}
