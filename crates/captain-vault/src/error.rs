//! Errors raised while reading or writing credentials and state.

/// Why a vault operation failed.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The OS keychain refused or failed the operation.
    #[error("keychain {op} failed for {entry}: {message}")]
    Keychain {
        /// `get`, `set` or `delete`.
        op: &'static str,
        /// Entry name inside the profile.
        entry: String,
        /// Message from the keychain backend.
        message: String,
    },
    /// A stored value exists but cannot be decoded.
    #[error("stored {entry} is corrupt: {reason}")]
    Corrupt {
        /// Entry name inside the profile.
        entry: String,
        /// What was wrong with it.
        reason: String,
    },
    /// The profile name contains characters outside `[a-z0-9_-]` or is too long.
    #[error("profile name {0:?} must be 1-32 characters of a-z, 0-9, '-' or '_'")]
    InvalidProfile(String),
    /// No home directory to derive the state directory from.
    #[error("cannot locate the home directory for local state")]
    NoHomeDir,
    /// A local state file could not be read or written.
    #[error("state file {path}: {source}")]
    Io {
        /// File or directory involved.
        path: std::path::PathBuf,
        /// Underlying I/O error.
        source: std::io::Error,
    },
}

/// Result alias for vault operations.
pub type Result<T> = std::result::Result<T, Error>;
