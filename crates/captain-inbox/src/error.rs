//! Errors from reading inbox configuration.

/// Why a scope or policy file was rejected. The whole file is rejected, never part of it.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    /// The file is not valid TOML, or has unknown or missing keys.
    #[error("{file}: {reason}")]
    Parse {
        /// Which file, for example `scope.toml`.
        file: &'static str,
        /// Parser message.
        reason: String,
    },
    /// The file parsed but breaks a rule, such as a duplicate chat id.
    #[error("{file}: chat {chat}: {reason}")]
    Invalid {
        /// Which file.
        file: &'static str,
        /// Marked chat id of the offending entry, or 0 for file-level problems.
        chat: i64,
        /// What is wrong.
        reason: String,
    },
    /// A value that should be a calendar date is not `YYYY-MM-DD`.
    #[error("{0:?} is not a date in YYYY-MM-DD form")]
    Date(String),
}

/// Result alias for inbox configuration.
pub type Result<T> = std::result::Result<T, Error>;
