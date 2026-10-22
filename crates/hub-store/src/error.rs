//! Failures from opening the database or writing a hub table.

use std::path::PathBuf;

/// Why a hub-store operation failed.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// `SQLite` refused a statement.
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    /// A directory for the bootstrap copy could not be created.
    #[error("cannot create {path}: {source}")]
    Io {
        /// Directory involved.
        path: PathBuf,
        /// Underlying I/O error.
        source: std::io::Error,
    },
    /// Captain's own open or migration failed. Its `user_version` is unchanged by this crate.
    #[error(transparent)]
    Captain(#[from] captain_store::Error),
    /// `hub_schema.version` was written by a newer build.
    #[error("hub schema version {found} is newer than this build's {supported}")]
    NewerSchema {
        /// Version found in `hub_schema`.
        found: i64,
        /// Newest version this build applies.
        supported: i64,
    },
    /// A stored enum or number does not match what this build writes.
    #[error("stored value is invalid: {0}")]
    Corrupt(String),
    /// A required token such as an external id was empty.
    #[error("{0} is empty")]
    Empty(String),
    /// `record_message` was asked to count a conversation that was never upserted.
    #[error("no {network} conversation {external_id}")]
    UnknownConversation {
        /// Network name.
        network: String,
        /// Caller-supplied id.
        external_id: String,
    },
    /// A conversation row id does not exist.
    #[error("no conversation {id}")]
    UnknownConversationId {
        /// `conversations.id`.
        id: i64,
    },
    /// The chat is the bridge supergroup and the message has no topic.
    #[error("bridge chat has no topic")]
    BridgeTopicMissing,
    /// The chat is the bridge supergroup and no conversation owns that topic.
    #[error("bridge topic {topic_id} has no conversation")]
    UnmappedTopic {
        /// Telegram topic id.
        topic_id: i64,
    },
    /// More than one conversation row has this topic id.
    #[error("topic {topic_id} maps to more than one conversation")]
    AmbiguousTopic {
        /// Telegram topic id.
        topic_id: i64,
    },
    /// Another conversation already owns this Telegram topic.
    #[error("topic {topic_id} is already mapped")]
    TopicInUse {
        /// Telegram topic id.
        topic_id: i64,
    },
}

/// Result alias for hub-store operations.
pub type Result<T> = std::result::Result<T, Error>;
