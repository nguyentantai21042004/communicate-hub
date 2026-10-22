//! Caps and timings the bridge enforces.
//!
//! Telegram's text and caption limits live in [`hub_core::limits`]. The numbers here are this
//! crate's own policy: how many topics run at once, how many times a sink call is retried, and
//! how a topic title is clipped.

#![allow(missing_docs)]

pub const TOPIC_CONCURRENCY: usize = 8; // topics with a running worker at once
pub const SINK_MAX_ATTEMPTS: u32 = 3; // tries per sink call, including the first
pub const TOPIC_RECREATE_LIMIT: u32 = 1; // replacements after TopicGone, then the send is retried
pub const RETRY_WINDOW_SECS: i64 = 86_400; // health counts retries inside this window
pub const TOPIC_TITLE_MAX_CHARS: usize = 128; // TELEGRAM: Bot API createForumTopic name, UTF-16 units
pub const SENDER_NAME_MAX_CHARS: usize = 64; // sender label before the body, UTF-16 units
pub const FAILURE_REASON_MAX_CHARS: usize = 240; // matches hub-store's failed_events.reason cap
pub const MENTION_MAX_WORDS: usize = 5; // words after `@` that can form one display name
pub const ADAPTER_BACKOFF_SECS: i64 = 1; // pause after `next_event` fails before reading again
pub const DEFAULT_SENDER_NAME: &str = "someone"; // inbound message with a blank sender
pub const FALLBACK_TOPIC_NAME: &str = "chat"; // title body when the conversation has no name
pub const MODULE_NAME: &str = "bridge"; // ModuleHealth.module for this crate
