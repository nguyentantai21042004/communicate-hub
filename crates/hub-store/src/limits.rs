//! Caps and storage keys this crate enforces.
//!
//! The numbers are local policy. They keep bridge tables bounded and keep free-text columns too
//! short to hold a message body. They are not Telegram protocol limits.

#![allow(missing_docs)]

pub const HUB_SCHEMA_VERSION: i64 = 2; // newest version `migrate` applies
pub const MESSAGE_MAP_MAX_ROWS: i64 = 20_000; // oldest bridge id mappings are dropped past this
pub const EVENT_DETAIL_MAX_CHARS: usize = 240; // bridge and module event detail, in Unicode scalars
pub const FAILURE_REASON_MAX_CHARS: usize = 240; // failed-event reason, in Unicode scalars
pub const EVENT_RETENTION_DAYS: i64 = 30; // default horizon for `prune_events`
pub const SECS_PER_DAY: i64 = 86_400; // Unix day, UTC, no leap seconds
pub const SECS_PER_HOUR: i64 = 3_600; // hour bucket width
pub const BUSY_TIMEOUT_SECS: u64 = 5; // wait when the captain daemon holds the file
pub const BRIDGE_SUPERGROUP_KEY: &str = "bridge_supergroup_id"; // `settings.key` for the bridge forum
