//! Failures a caller of [`crate::Bridge`] can act on.
//!
//! A sink or adapter error during delivery is stored in `failed_events` and does not surface
//! here. These variants mean the event was not queued, or the store itself failed.

use hub_core::Error as CoreError;

/// Failure from the bridge control plane.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    /// The store rejected a read or write.
    #[error("store failed: {detail}")]
    Store {
        /// Log-safe description. No message text.
        detail: String,
    },
    /// [`crate::Bridge::run`] is already in progress.
    #[error("bridge is already running")]
    AlreadyRunning,
    /// The bridge has finished [`crate::Bridge::run`] and no longer accepts events.
    #[error("bridge is shut down")]
    ShutDown,
    /// A hub-core call failed before delivery could record it.
    #[error(transparent)]
    Core(#[from] CoreError),
}

/// Result of a fallible bridge operation.
pub type Result<T> = std::result::Result<T, Error>;
