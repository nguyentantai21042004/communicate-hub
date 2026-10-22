//! Timeouts and caps for the Zalo sidecar supervisor.
//!
//! The restart numbers are this crate's policy. `MEDIA_RETENTION` is the age after which
//! a downloaded Zalo file may be deleted.

#![allow(missing_docs)]

use std::time::Duration;

pub const MEDIA_RETENTION: Duration = Duration::from_secs(7 * 24 * 60 * 60); // downloaded Zalo bytes
pub const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(30); // one sidecar RPC
pub const RESTART_INITIAL_BACKOFF: Duration = Duration::from_secs(1); // first unexpected exit
pub const RESTART_MAX_BACKOFF: Duration = Duration::from_secs(5 * 60); // restart delay cap
pub const CRASH_LOOP_CAP: u32 = 5; // unexpected exits in a row before giving up
pub const EVENT_QUEUE_CAPACITY: usize = 256; // sidecar events kept per subscriber
pub const MAX_STDOUT_LINE_BYTES: usize = 8 * 1024 * 1024; // matches the sidecar frame cap
pub const DEFAULT_RATE_LIMIT_RETRY_SECS: u64 = 30; // used when the sidecar omits a wait
pub const RESTART_WINDOW: Duration = Duration::from_secs(24 * 60 * 60); // restarts_24h
