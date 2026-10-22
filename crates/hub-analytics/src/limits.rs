//! Windows and sample thresholds for the dashboard charts.
//!
//! Day and hour lengths are Unix time, UTC, with no leap seconds. The reply sample threshold
//! is the phase 1 rule for a daily response median.

#![allow(missing_docs)]

pub(crate) const SECS_PER_DAY: i64 = 86_400; // UTC day
pub(crate) const SECS_PER_HOUR: i64 = 3_600; // UTC hour bucket
pub(crate) const SECS_PER_MINUTE: i64 = 60; // response medians are reported in minutes
pub const MIN_DAILY_REPLIES: u64 = 5; // response_p50_by_day sets low_sample below this
pub(crate) const ZALO_KIND_WINDOW_DAYS: i64 = 14; // zalo_summary content mix
pub(crate) const SPARKLINE_DAYS: i64 = 7; // conversations_page buckets, oldest first
pub(crate) const MESSAGES_24H_BUCKETS: i64 = 24; // overview message card, including the current hour
pub(crate) const HOURS_PER_DAY: usize = 24; // heatmap columns
pub(crate) const WEEKDAYS: usize = 7; // heatmap rows, Monday is 0
pub(crate) const WEEKDAY_SHIFT: i64 = 3; // Unix day 0 is Thursday, Monday is 0
