//! Client for the Zalo Node sidecar.
//!
//! This crate spawns `sidecars/zalo-node`, speaks `NDJSON` over stdin and stdout, and exposes
//! the process as a [`hub_core::NetworkAdapter`]. It does not import a Zalo protocol library.
//! Login and directory calls that are not part of that trait live on [`ZaloAdmin`].

mod adapter;
mod admin;
mod error;
mod limits;
mod normalize;
mod protocol;
mod retention;
mod sidecar;

pub use adapter::ZaloAdapter;
pub use admin::{DirectoryEntry, FriendRequestEntry, QrState, ZaloAdmin};
pub use error::{Error, Result};
pub use limits::{
    CRASH_LOOP_CAP, DEFAULT_RATE_LIMIT_RETRY_SECS, DEFAULT_REQUEST_TIMEOUT, EVENT_QUEUE_CAPACITY,
    MAX_STDOUT_LINE_BYTES, MEDIA_RETENTION, RESTART_INITIAL_BACKOFF, RESTART_MAX_BACKOFF,
    RESTART_WINDOW,
};
pub use protocol::{ERROR_CODES, EVENT_NAMES, METHOD_NAMES};
pub use retention::cleanup_expired_media;
pub use sidecar::{RunPhase, Sidecar, SidecarConfig, SidecarStatus};
