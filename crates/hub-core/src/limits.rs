//! Telegram limits the bridge must respect before it writes into a forum topic.
//!
//! The numbers are defined once in [`captain_core::limits`] and re-exported here.
//! `TEXT_MAX_UTF16` bounds [`crate::TopicSink::send_text`]. The two caption constants bound
//! [`crate::TopicSink::send_media`]. Lengths are UTF-16 code units, which is how Telegram counts.
#![allow(missing_docs)]

pub use captain_core::limits::{CAPTION_MAX_UTF16, CAPTION_PREMIUM_MAX_UTF16, TEXT_MAX_UTF16};
