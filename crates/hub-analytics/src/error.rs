//! Failures from a metric function. The store's own errors pass through unchanged.

/// Why a metric function failed.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The store could not read a row or attribute a chat.
    #[error(transparent)]
    Store(#[from] hub_store::Error),
    /// A ticket or counter row could not be decoded.
    #[error("stored value is invalid: {0}")]
    Corrupt(String),
    /// `days` was zero, negative, or the window overflowed `i64`.
    #[error("range is not valid")]
    InvalidRange,
}

/// Result alias for metric functions.
pub type Result<T> = std::result::Result<T, Error>;
