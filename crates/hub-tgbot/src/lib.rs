//! `Telegram` Bot API adapter for the bridge supergroup.
//!
//! [`BotClient`] implements [`hub_core::TopicSink`] and long-polls topic messages
//! and reactions. It does not read the environment or the filesystem for the bot token.
//! Tests point [`BotConfig::api_base_url`] at a local fake. Nothing in this crate
//! calls `api.telegram.org` on its own.
//!
//! `hub-bridge` defines its own `TopicIncoming` (`Message`, `Reaction`, `Edit`,
//! `Unsend`). [`TopicIncoming`] here is a different type. The app maps one to the other.

mod client;
mod config;
mod error;
mod incoming;
mod media;
mod preflight;
mod queue;

pub use client::{BotClient, TopicIconColor, TopicOptions};
pub use config::{BotConfig, BotToken};
pub use error::{Error, Result, TransferDirection};
pub use incoming::{
    IncomingAttachment, IncomingMessage, IncomingReaction, OffsetStore, TopicIncoming,
};
pub use preflight::{BotRight, PreflightReport, PreflightVerdict};

/// Bot API caps and queue settings.
pub mod limits;

#[cfg(test)]
mod fake;
#[cfg(test)]
mod tests;
