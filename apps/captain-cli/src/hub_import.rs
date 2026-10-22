//! `captain hub import-topics`.
//!
//! Reads an old chat-bridges sqlite file and, with `--apply`, copies topic rows into the hub
//! store. The old file is opened read-only. Existing hub mappings are left as they are.

use std::collections::HashSet;
use std::path::Path;

use anyhow::{Context, Result, bail};
use hub_core::{ConversationId, ConversationKind, Network};
use hub_store::SharedHub;
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde::Serialize;

const ZALO_TITLE_PREFIX: &str = "[Zalo] ";
const MESSENGER_TITLE_PREFIX: &str = "[FB] ";
const TOPIC_ID_MIN: i64 = 1;
const TOPIC_ID_MAX: i64 = i32::MAX as i64;
const USER_NAME_COLUMNS: [&str; 5] = [
    "network",
    "user_id",
    "display_name",
    "norm_name",
    "group_scope",
];
const LOCK_HINT: &str =
    "stop the hub first: launchctl bootout gui/$(id -u)/dev.tantai.comms-hub.bridge";

/// Counts from one import. Titles and message text are not included.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ImportReport {
    /// Rows in the old `topics` table.
    pub topics_found: u64,
    /// Rows whose network is `zalo` and whose kind and id can be parsed.
    pub zalo_conversations: u64,
    /// Rows skipped because they disagree with another row or with the hub.
    pub conflicts: u64,
    /// Same conversation listed twice. The first row wins.
    pub conflict_duplicate_conversation: u64,
    /// `topic_id` outside the forum topic id range.
    pub conflict_topic_id_space: u64,
    /// The hub already maps this conversation or this topic id to something else.
    pub conflict_existing_topic: u64,
    /// Network or kind this hub does not understand.
    pub conflict_unmapped: u64,
    /// Names that had a `[Zalo] ` or `[FB] ` prefix.
    pub titles_recovered: u64,
    /// `user_names` rows whose columns matched and were non-empty.
    pub user_names: u64,
    /// `true` when `--apply` wrote the rows.
    pub applied: bool,
}

struct PlannedTopic {
    conversation: ConversationId,
    topic_id: i64,
    title: String,
    at_unix_secs: i64,
}

struct PlannedName {
    scope: String,
    name_key: String,
    user_id: String,
    display_name: String,
}

/// Reads `from` and, when `apply` is set, writes through `hub`.
///
/// A second call does not replace a topic mapping that is already stored.
/// `hub-store` has no public transaction that covers every row, so each conversation is its
/// own `upsert_topic_row`. A re-run skips a mapping that the first run committed.
///
/// # Errors
///
/// Returns an error when the old file cannot be read, or when a hub write fails.
/// The error text does not include topic names.
pub fn import_topics(from: &Path, hub: Option<&SharedHub>, apply: bool) -> Result<ImportReport> {
    if apply && hub.is_none() {
        bail!("import apply needs an open hub");
    }
    let conn = open_old(from)?;
    let mut report = ImportReport {
        topics_found: 0,
        zalo_conversations: 0,
        conflicts: 0,
        conflict_duplicate_conversation: 0,
        conflict_topic_id_space: 0,
        conflict_existing_topic: 0,
        conflict_unmapped: 0,
        titles_recovered: 0,
        user_names: 0,
        applied: false,
    };
    let topics = read_topics(&conn, &mut report)?;
    let names = read_names(&conn, &mut report)?;
    let writable = match hub {
        Some(hub) => filter_existing(hub, topics, &mut report)?,
        None => topics,
    };
    if apply {
        let hub = hub.context("import apply needs an open hub")?;
        write_all(hub, &writable, &names)?;
        report.applied = true;
    }
    Ok(report)
}

/// Text for a failed `hub.lock`, including the command that stops the running agent.
pub fn lock_failure_text(err: &anyhow::Error) -> String {
    let text = err.to_string();
    if text.contains("another hub process") {
        format!("{text}\n{LOCK_HINT}")
    } else {
        text
    }
}

fn open_old(path: &Path) -> Result<Connection> {
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let conn = Connection::open_with_flags(path, flags)
        .with_context(|| format!("cannot open {}", path.display()))?;
    conn.pragma_update(None, "query_only", true)
        .context("cannot mark the old store read-only")?;
    let found: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'topics'",
            [],
            |row| row.get(0),
        )
        .optional()
        .context("cannot read the old store")?;
    if found.is_none() {
        bail!("topics table is missing");
    }
    Ok(conn)
}

fn read_topics(conn: &Connection, report: &mut ImportReport) -> Result<Vec<PlannedTopic>> {
    let mut stmt = conn
        .prepare(
            "SELECT topic_id, network, kind, external_id, name, created_at
             FROM topics ORDER BY topic_id",
        )
        .context("cannot read topics")?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, i64>(5)?,
            ))
        })
        .context("cannot read topics")?;
    let mut seen = HashSet::new();
    let mut planned = Vec::new();
    for row in rows {
        let (topic_id, network, kind, external_id, name, created_at) =
            row.context("cannot read a topic row")?;
        report.topics_found += 1;
        let Some(conversation) = conversation_of(&network, &kind, &external_id) else {
            report.conflict_unmapped += 1;
            report.conflicts += 1;
            continue;
        };
        if conversation.network == Network::Zalo {
            report.zalo_conversations += 1;
        }
        if !(TOPIC_ID_MIN..=TOPIC_ID_MAX).contains(&topic_id) {
            report.conflict_topic_id_space += 1;
            report.conflicts += 1;
            continue;
        }
        let key = conversation.to_string();
        if !seen.insert(key) {
            report.conflict_duplicate_conversation += 1;
            report.conflicts += 1;
            continue;
        }
        let (title, recovered) = recover_title(&name);
        if recovered {
            report.titles_recovered += 1;
        }
        planned.push(PlannedTopic {
            conversation,
            topic_id,
            title,
            at_unix_secs: created_at,
        });
    }
    Ok(planned)
}

fn conversation_of(network: &str, kind: &str, external_id: &str) -> Option<ConversationId> {
    if external_id.is_empty() {
        return None;
    }
    let network = network.parse::<Network>().ok()?;
    let kind = kind.parse::<ConversationKind>().ok()?;
    Some(ConversationId {
        network,
        kind,
        external_id: external_id.to_owned(),
    })
}

fn recover_title(name: &str) -> (String, bool) {
    let trimmed = name.trim();
    let (body, recovered) = if let Some(rest) = trimmed.strip_prefix(ZALO_TITLE_PREFIX) {
        (rest.trim(), true)
    } else if let Some(rest) = trimmed.strip_prefix(MESSENGER_TITLE_PREFIX) {
        (rest.trim(), true)
    } else {
        (trimmed, false)
    };
    if body.is_empty() {
        (String::new(), false)
    } else {
        (body.to_owned(), recovered)
    }
}

fn read_names(conn: &Connection, report: &mut ImportReport) -> Result<Vec<PlannedName>> {
    if !names_compatible(conn)? {
        return Ok(Vec::new());
    }
    let mut stmt = conn
        .prepare("SELECT network, user_id, display_name, norm_name, group_scope FROM user_names")
        .context("cannot read user names")?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
            ))
        })
        .context("cannot read user names")?;
    let mut planned = Vec::new();
    for row in rows {
        let (network, user_id, display_name, norm_name, group_scope) =
            row.context("cannot read a user name row")?;
        if network.is_empty() || user_id.is_empty() || norm_name.is_empty() {
            continue;
        }
        report.user_names += 1;
        planned.push(PlannedName {
            scope: name_scope(&network, &group_scope),
            name_key: norm_name,
            user_id,
            display_name,
        });
    }
    Ok(planned)
}

fn names_compatible(conn: &Connection) -> Result<bool> {
    let mut stmt = conn
        .prepare("PRAGMA table_info(user_names)")
        .context("cannot read user name columns")?;
    let found = stmt
        .query_map([], |row| row.get::<_, String>(1))
        .context("cannot read user name columns")?;
    let mut columns = HashSet::new();
    for name in found {
        columns.insert(name.context("cannot read a user name column")?);
    }
    Ok(USER_NAME_COLUMNS
        .iter()
        .all(|column| columns.contains(*column)))
}

fn name_scope(network: &str, group_scope: &str) -> String {
    if group_scope.is_empty() {
        format!("network:{network}")
    } else {
        format!("{network}:group:{group_scope}")
    }
}

fn filter_existing(
    hub: &SharedHub,
    topics: Vec<PlannedTopic>,
    report: &mut ImportReport,
) -> Result<Vec<PlannedTopic>> {
    let guard = hub.lock();
    let mut writable = Vec::new();
    for topic in topics {
        match guard.topic_row(&topic.conversation) {
            Ok(Some(existing)) if existing.topic_id == topic.topic_id => continue,
            Ok(Some(_)) => {
                report.conflict_existing_topic += 1;
                report.conflicts += 1;
                continue;
            }
            Ok(None) => {}
            Err(err) => return Err(err).context("cannot read a hub topic"),
        }
        match guard.conversation_for_topic(topic.topic_id) {
            Ok(Some(owner)) if owner != topic.conversation => {
                report.conflict_existing_topic += 1;
                report.conflicts += 1;
                continue;
            }
            Ok(_) => {}
            Err(err) => return Err(err).context("cannot read a hub topic id"),
        }
        writable.push(topic);
    }
    Ok(writable)
}

fn write_all(hub: &SharedHub, topics: &[PlannedTopic], names: &[PlannedName]) -> Result<()> {
    let mut guard = hub.lock();
    for topic in topics {
        guard
            .upsert_topic_row(
                &topic.conversation,
                topic.topic_id,
                &topic.title,
                topic.at_unix_secs,
            )
            .context("cannot store an imported topic")?;
    }
    for name in names {
        guard
            .remember_user_name(
                &name.scope,
                &name.name_key,
                &name.user_id,
                &name.display_name,
            )
            .context("cannot store an imported user name")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;
    use crate::hub_run::lock_hub;

    fn scratch() -> std::path::PathBuf {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos());
        let path = std::env::temp_dir().join(format!("hub-import-{nanos}-{n}"));
        fs::create_dir(&path).unwrap();
        path
    }

    struct Tmp(std::path::PathBuf);

    impl Tmp {
        fn new() -> Self {
            Self(scratch())
        }
    }

    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn old_db(path: &Path) {
        let conn = Connection::open(path).unwrap();
        conn.execute_batch(
            "CREATE TABLE topics (
                topic_id INTEGER PRIMARY KEY,
                network TEXT NOT NULL,
                kind TEXT NOT NULL,
                external_id TEXT NOT NULL,
                name TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
            );
            CREATE TABLE user_names (
                network TEXT NOT NULL,
                user_id TEXT NOT NULL,
                group_scope TEXT NOT NULL DEFAULT '',
                display_name TEXT NOT NULL,
                norm_name TEXT NOT NULL,
                updated_at INTEGER NOT NULL
            );",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO topics VALUES (3693, 'zalo', 'group', 'g1', '[Zalo] Gia Đình', 10, 10)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO topics VALUES (3694, 'zalo', 'group', 'g1', '[Zalo] Other', 11, 11)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO topics VALUES (0, 'zalo', 'dm', 'u1', '[Zalo] An', 12, 12)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO topics VALUES (8, 'sms', 'group', 'x', 'Nope', 13, 13)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO user_names VALUES ('zalo', 'u1', '', 'An', 'an', 10)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO user_names VALUES ('zalo', 'u2', 'g1', 'Binh', 'binh', 10)",
            [],
        )
        .unwrap();
    }

    #[test]
    fn dry_run_counts_conflicts_and_apply_is_idempotent() {
        let tmp = Tmp::new();
        let old = tmp.0.join("old.sqlite");
        old_db(&old);
        let dry = import_topics(&old, None, false).unwrap();
        assert_eq!(dry.topics_found, 4);
        assert_eq!(dry.zalo_conversations, 3);
        assert_eq!(dry.conflict_duplicate_conversation, 1);
        assert_eq!(dry.conflict_topic_id_space, 1);
        assert_eq!(dry.conflict_unmapped, 1);
        assert_eq!(dry.conflicts, 3);
        assert_eq!(dry.titles_recovered, 1);
        assert_eq!(dry.user_names, 2);
        assert!(!dry.applied);

        let store = tmp.0.join("inbox.sqlite");
        let hub = SharedHub::open(&store).unwrap();
        let applied = import_topics(&old, Some(&hub), true).unwrap();
        assert!(applied.applied);
        assert_eq!(applied.titles_recovered, 1);
        let conversation = ConversationId {
            network: Network::Zalo,
            kind: ConversationKind::Group,
            external_id: "g1".into(),
        };
        let row = hub.lock().topic_row(&conversation).unwrap().unwrap();
        assert_eq!(row.topic_id, 3693);
        assert_eq!(row.title, "Gia Đình");
        let again = import_topics(&old, Some(&hub), true).unwrap();
        assert_eq!(again.conflict_existing_topic, 0);
        assert_eq!(
            hub.lock()
                .topic_row(&conversation)
                .unwrap()
                .unwrap()
                .topic_id,
            3693
        );
        let scope = hub.lock().lookup_user_name("network:zalo", "an").unwrap();
        assert_eq!(scope.as_deref(), Some("u1"));
        let grouped = hub
            .lock()
            .lookup_user_name("zalo:group:g1", "binh")
            .unwrap();
        assert_eq!(grouped.as_deref(), Some("u2"));

        let other = ConversationId {
            network: Network::Zalo,
            kind: ConversationKind::Group,
            external_id: "g2".into(),
        };
        hub.lock()
            .upsert_topic_row(&other, 3693, "taken", 1)
            .unwrap_err();
        {
            let blocked = Connection::open(&old).unwrap();
            blocked.execute("DELETE FROM topics", []).unwrap();
            blocked
                .execute(
                    "INSERT INTO topics VALUES (3693, 'zalo', 'group', 'g9', '[Zalo] Nine', 14, 14)",
                    [],
                )
                .unwrap();
        }
        let conflict = import_topics(&old, Some(&hub), true).unwrap();
        assert_eq!(conflict.conflict_existing_topic, 1);
        assert!(hub.lock().topic_row(&conversation_g9()).unwrap().is_none());
    }

    fn conversation_g9() -> ConversationId {
        ConversationId {
            network: Network::Zalo,
            kind: ConversationKind::Group,
            external_id: "g9".into(),
        }
    }

    #[test]
    fn a_held_lock_names_the_bootout_command() {
        let tmp = Tmp::new();
        let held = lock_hub(&tmp.0).unwrap();
        let err = lock_hub(&tmp.0).unwrap_err();
        let text = lock_failure_text(&err);
        assert!(text.contains("another hub process"), "{text}");
        assert!(
            text.contains("launchctl bootout gui/$(id -u)/dev.tantai.comms-hub.bridge"),
            "{text}"
        );
        drop(held);
    }

    #[test]
    fn real_store_dry_run_when_configured() {
        let Ok(path) = std::env::var("CHAT_BRIDGES_SQLITE") else {
            return;
        };
        let report = import_topics(Path::new(&path), None, false).unwrap();
        assert_eq!(report.topics_found, 3);
        assert_eq!(report.zalo_conversations, 3);
        assert_eq!(report.conflict_duplicate_conversation, 0);
        assert_eq!(report.conflict_topic_id_space, 0);
        assert_eq!(report.titles_recovered, 3);
        assert_eq!(report.user_names, 3);
        assert!(!report.applied);
    }
}
