//! Routes remote events into Telegram forum topics and topic replies back out.
//!
//! One conversation maps to one topic. Each topic has its own queue, so its messages stay in
//! order, and different topics run at the same time up to a fixed cap. This crate does not open
//! a socket. Callers pass a [`hub_core::TopicSink`] and [`hub_core::NetworkAdapter`]s.

mod adapter;
mod bridge;
mod clock;
mod config;
mod error;
mod format;
mod incoming;
mod limits;
mod mention;
mod queue;
mod reaction;
mod store;
mod store_sqlite;

pub use adapter::AdapterHandle;
pub use bridge::Bridge;
pub use clock::{Clock, ManualClock, SystemClock};
pub use config::BridgeConfig;
pub use error::{Error, Result};
pub use hub_core::{RemoteEvent, TgMessageId, TopicId};
pub use incoming::TopicIncoming;
/// How many placeholder topics one directory pass renames, and the seconds between them.
pub use limits::{BACKFILL_RENAME_CAP, BACKFILL_RENAME_GAP_SECS};
pub use reaction::{
    TELEGRAM_REACTIONS, is_telegram_reaction, to_network_reaction, to_telegram_reaction,
};
pub use store::{BridgeStore, FailureRecord, MemoryStore, TopicMapping};
pub use store_sqlite::SqliteStore;

#[cfg(test)]
mod tests;
