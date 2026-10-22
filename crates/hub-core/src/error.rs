//! The one error type for hub-core.
//!
//! Adapters map transport failures into [`Error::Adapter`] at the crate boundary.
//! Parse failures and the two Telegram outcomes the bridge matches
//! ([`Error::RateLimited`], [`Error::TopicGone`]) are their own variants.

/// Failure from parsing a hub id, or from a [`crate::NetworkAdapter`] or [`crate::TopicSink`] call.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    /// The network wire name is not `telegram`, `zalo`, or `messenger`.
    #[error("network {value:?} is unknown")]
    UnknownNetwork {
        /// The string that was rejected.
        value: String,
    },
    /// The kind wire name is not `dm` or `group`.
    #[error("conversation kind {value:?} is unknown")]
    UnknownKind {
        /// The string that was rejected.
        value: String,
    },
    /// `network:kind:external_id` is missing a part or has an empty part.
    #[error("conversation id {value:?} is malformed")]
    MalformedConversationId {
        /// The string that was rejected.
        value: String,
    },
    /// Telegram asked the caller to wait before the next write.
    #[error("rate limited, retry after {retry_after_secs}s")]
    RateLimited {
        /// Seconds to wait before retrying the same call.
        retry_after_secs: u64,
    },
    /// The forum topic no longer exists.
    ///
    /// The caller still has the topic id it passed in. Drop that mapping before opening
    /// a replacement topic.
    #[error("topic is gone")]
    TopicGone,
    /// The adapter failed for a reason the bridge cannot recover by itself.
    ///
    /// SECURITY: `detail` is logged. Put an id or a status in it, never message text or a token.
    #[error("adapter failed: {detail}")]
    Adapter {
        /// Short, log-safe description of the failure.
        detail: String,
    },
}

/// Result of a fallible hub-core operation.
pub type Result<T> = std::result::Result<T, Error>;
