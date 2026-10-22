//! Bot API limits, forum icon colors, and the request queue settings.
//!
//! Download and upload caps are the Bot API file limits at
//! <https://core.telegram.org/bots/api#sending-files>.
//! `createForumTopic` accepts only the six icon colors listed here.
//! The queue numbers match the bridge this crate replaces: five calls in flight,
//! five retries after a 429, and one extra second on top of `retry_after`.
#![allow(missing_docs)]

pub const DOWNLOAD_MAX_BYTES: u64 = 20 * 1024 * 1024; // getFile cap, bytes
pub const UPLOAD_MAX_BYTES: u64 = 50 * 1024 * 1024; // multipart upload cap, bytes
pub const ALBUM_MAX_ITEMS: usize = 10; // sendMediaGroup hard cap
pub const REQUEST_CONCURRENCY: usize = 5; // in-flight Bot API calls
pub const MAX_RATE_LIMIT_RETRIES: u32 = 5; // retries after the first 429
pub const RETRY_BACKOFF_EXTRA_SECS: u64 = 1; // added to retry_after before the next try
pub const DEFAULT_RETRY_AFTER_SECS: u64 = 30; // used when a 429 omits retry_after
pub const LONG_POLL_TIMEOUT_SECS: u32 = 25; // getUpdates timeout, seconds
pub const GENERAL_TOPIC_ID: i32 = 1; // General forum topic when message_thread_id is absent

pub const ICON_COLOR_BLUE: u32 = 0x6F_B9_F0; // createForumTopic icon color
pub const ICON_COLOR_YELLOW: u32 = 0xFF_D6_7E; // createForumTopic icon color
pub const ICON_COLOR_PURPLE: u32 = 0xCB_86_DB; // createForumTopic icon color
pub const ICON_COLOR_GREEN: u32 = 0x8E_EE_98; // createForumTopic icon color
pub const ICON_COLOR_PINK: u32 = 0xFF_93_B2; // createForumTopic icon color
pub const ICON_COLOR_RED: u32 = 0xFB_6F_5F; // createForumTopic icon color

pub(crate) const HTTP_FORBIDDEN: u64 = 403;
pub(crate) const HTTP_REQUEST_TIMEOUT: u64 = 408;
pub(crate) const HTTP_TOO_MANY_REQUESTS: u64 = 429;
pub(crate) const HTTP_SERVER_ERROR_MIN: u64 = 500;
pub(crate) const HTTP_SERVER_ERROR_MAX: u64 = 599;
