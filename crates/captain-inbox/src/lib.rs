//! Inbox logic: which messages matter, how they group into tickets, and what the numbers say.
//!
//! Everything here is pure. It does no I/O, reads no clock, and knows no Telegram or `SQLite`
//! library: the daemon looks facts up and passes them in, with the current time as Unix
//! seconds. That keeps every rule testable without a network or a database.

pub mod config;
pub mod manual;
pub mod report;
pub mod stats;
pub mod ticket;
pub mod trigger;

mod coalesce;
mod error;
mod time;
mod triage;

pub use coalesce::{Arrival, Burst, Coalescer};
pub use config::{ChatEntry, EntryKind, Policy, Profile, Scope};
pub use error::{Error, Result};
pub use ticket::{ExplicitLabel, ImplicitLabel, ResolveFacts, Route, ThreadKey, TicketState};
pub use time::{parse_date, unix_day};
pub use triage::{Triage, TriageFacts, triage};
pub use trigger::{Trigger, TriggerKind};
