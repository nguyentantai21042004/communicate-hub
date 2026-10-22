//! Caps and wire constants for the dashboard routes.
//!
//! Days and page sizes match the monitoring UI: a report window is at most half a year, and a
//! history search stops at the same page cap the daemon already uses.

#![allow(missing_docs)]

pub const DEFAULT_DAYS: u32 = 14; // omitted `days` on overview, analytics, telegram, and zalo
pub const MIN_DAYS: u32 = 1; // a zero-length window is rejected, not clamped
pub const MAX_DAYS: u32 = 180; // same ceiling as `GET /api/report`
pub const DEFAULT_PAGE_LIMIT: u32 = 50; // conversation page when `limit` is omitted
pub const MAX_PAGE_LIMIT: u32 = 200; // conversation page is clamped here
pub const MAX_OFFSET: u32 = 10_000; // conversation offset is clamped here
pub const DEFAULT_EVENT_LIMIT: u32 = 50; // module events when `limit` is omitted
pub const MAX_EVENT_LIMIT: u32 = 200; // module events are clamped here
pub const DEFAULT_HISTORY_SCAN: u32 = 3_000; // omitted `scan_limit`, same as the CLI default
pub const MAX_HISTORY_SCAN: usize = 20_000; // same cap as the daemon history search
pub const MAX_HISTORY_BODY: usize = 65_536; // bytes read from `POST /api/history`
pub const ATTENTION_LIMIT: usize = 6; // needs-attention rows on the overview
pub const SPARKLINE_DAYS: usize = 7; // conversation `messages_7d` buckets
pub const HEATMAP_WEEKDAYS: u8 = 7; // Monday is 0
pub const HOURS_PER_DAY: u8 = 24; // UTC hour of a heatmap cell
pub const TOP_CONVERSATIONS: usize = 8; // analytics ranking
pub const CANDIDATE_LIMIT: usize = 10; // out-of-scope chats, same as M19
pub const LOW_SAMPLE_MIN: u64 = 5; // below this, `low_sample` is true
pub const MESSAGE_KIND_DAYS: u32 = 14; // Zalo kind bars are this wide, not the query window
pub const SECS_PER_DAY: i64 = 86_400; // UTC day, no leap seconds
pub const SECS_PER_MINUTE: i64 = 60; // response times are stored in seconds
pub const TICKET_LIST_CAP: usize = 10_000; // open tickets read in one call
pub const MODULE_CAPTAIN_DAEMON: &str = "captain-daemon"; // module strip name
pub const MODULE_ZALO_BRIDGE: &str = "zalo-bridge"; // module strip name
pub const MODULE_BRIDGE: &str = "bridge"; // module strip name
pub const STORAGE_KEYCHAIN: &str = "keychain"; // Telegram session storage kind, never the secret
pub const STORAGE_NONE: &str = "none"; // no Zalo session is loaded
pub const BRIDGE_DOWN: &str = "down"; // `bridge.state` when the sidecar is not running
pub const DRAFT_UNAVAILABLE: &str = "not available in P1"; // funnel draft slot until the runner exists
pub const CONNECTION_OFFLINE: &str = "offline"; // telegram session when the daemon is stopped
