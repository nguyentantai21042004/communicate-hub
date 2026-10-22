//! Metric functions over a read-only [`hub_store::Reader`].
//!
//! `now_unix_secs` and the range are arguments. These functions do not read the clock.
//! Message text is not read, because the store does not keep it.
//!
//! Calendar charts use the last `days` UTC days, including the day of `now_unix_secs`.
//! Ticket windows and the active-conversation window are half-open:
//! `now_unix_secs - days * 86400` inclusive, `now_unix_secs` exclusive.

mod conversations;
mod error;
mod funnel;
mod limits;
mod overview;
mod series;
mod summaries;
mod tickets;
mod types;
mod window;

pub use conversations::conversations_page;
pub use error::{Error, Result};
pub use funnel::ticket_funnel;
pub use limits::MIN_DAILY_REPLIES;
pub use overview::overview;
pub use series::{daily_by_source, hourly_heatmap, response_p50_by_day, top_conversations};
pub use summaries::{health, telegram_summary, zalo_summary};
pub use types::{
    ConversationFilter, ConversationItem, ConversationPage, DailyResponse, DailySourceCount,
    DraftStage, Health, HeatCell, Heatmap, KindCount, LevelCount, Overview, RankedConversation,
    ResolvedLabels, Source, SourceCount, TelegramSummary, TicketFunnel, TriggerBreakdown,
    ZaloSummary,
};

#[cfg(test)]
mod tests;
