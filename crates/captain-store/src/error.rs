//! Store errors.

/// Why a store operation failed.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// `SQLite` refused an operation.
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    /// The database file or its directory could not be created.
    #[error("cannot create {path}: {source}")]
    Io {
        /// File involved.
        path: std::path::PathBuf,
        /// Underlying I/O error.
        source: std::io::Error,
    },
    /// The database was migrated by a newer build, which this build cannot read safely.
    #[error("store schema version {found} is newer than this build's {supported}")]
    NewerSchema {
        /// Version in the file.
        found: i64,
        /// Newest version this build knows.
        supported: i64,
    },
    /// A stored value does not decode, for example an unknown ticket state.
    #[error("stored value is invalid: {0}")]
    Corrupt(String),
}

/// Result alias for store operations.
pub type Result<T> = std::result::Result<T, Error>;
