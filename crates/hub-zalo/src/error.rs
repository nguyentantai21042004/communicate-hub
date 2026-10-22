//! Failures from the sidecar process. Adapters map these into [`hub_core::Error`].

use std::io;

/// Failure while supervising the Zalo sidecar or reading one of its frames.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The process could not be spawned or a pipe was missing.
    #[error("sidecar spawn failed")]
    Spawn,
    /// A read or write on the sidecar pipes failed.
    #[error("sidecar io failed")]
    Io(#[source] io::Error),
    /// The sidecar rejected the call with one of the contract error codes.
    #[error("sidecar {code}")]
    Rpc {
        /// Contract error code, for example `rate_limited`.
        code: String,
        /// Present only for `rate_limited`.
        retry_after_secs: Option<u64>,
    },
    /// The response did not arrive before `request_timeout`.
    #[error("sidecar request timed out")]
    Timeout,
    /// Too many unexpected exits in a row.
    #[error("sidecar crash loop")]
    CrashLoop,
    /// The process exited while this call was in flight.
    #[error("sidecar exited")]
    Exited,
    /// The supervisor was asked to stop, or it has already stopped.
    #[error("sidecar stopped")]
    Stopped,
    /// A frame was `JSON` but not the object the contract describes.
    #[error("sidecar protocol mismatch")]
    Protocol,
    /// A supervisor mutex was poisoned.
    #[error("sidecar state is poisoned")]
    Poisoned,
}

/// Result of a fallible sidecar operation.
pub type Result<T> = std::result::Result<T, Error>;
