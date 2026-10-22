//! Daily totals, the weekday heatmap, top conversations, and the daily reply median.

use hub_store::Reader;

use crate::limits::{HOURS_PER_DAY, MIN_DAILY_REPLIES, WEEKDAYS};
use crate::tickets::{answered_reply_secs, includes};
use crate::types::{
    DailyResponse, DailySourceCount, HeatCell, Heatmap, NETWORKS, RankedConversation,
};
use crate::window::{self, add};
use crate::{Error, Result, Source};

/// Message totals for each UTC day in the window, for Telegram, Zalo and Messenger.
///
/// The window is the last `days` UTC days including the day of `now_unix_secs`. Days with no
/// messages are still emitted, and the source filter is not applied: every network is present.
/// Order is day ascending, then Telegram, Zalo, Messenger. Both directions count.
///
/// # Errors
///
/// [`Error::InvalidRange`] when `days` is not positive, [`Error::Store`] on a read failure,
/// and [`Error::Corrupt`] when a count overflows.
pub fn daily_by_source(
    reader: &Reader<'_>,
    now_unix_secs: i64,
    days: i64,
) -> Result<Vec<DailySourceCount>> {
    let (first, last) = window::calendar_days(now_unix_secs, days)?;
    let rows = reader.counters_between_days(first, last)?;
    let mut out = Vec::new();
    for day in first..=last {
        for network in NETWORKS {
            let mut count = 0u64;
            for row in rows
                .iter()
                .filter(|row| row.day == day && row.network == network)
            {
                add(&mut count, row.count)?;
            }
            out.push(DailySourceCount {
                day,
                source: network,
                count,
            });
        }
    }
    Ok(out)
}

/// Message counts by local weekday and hour.
///
/// Each counter row is one UTC hour. `offset_minutes` is added to that hour's start before the
/// weekday and hour are taken. Weekday 0 is Monday. `cells` has [`WEEKDAYS`] times [`HOURS_PER_DAY`]
/// entries, weekday-major. The window is the last `days` UTC days including `now_unix_secs`.
///
/// # Errors
///
/// [`Error::InvalidRange`] when `days` is not positive or `offset_minutes` overflows,
/// [`Error::Store`] on a read failure, and [`Error::Corrupt`] when a count overflows.
pub fn hourly_heatmap(
    reader: &Reader<'_>,
    now_unix_secs: i64,
    days: i64,
    source: Source,
    offset_minutes: i32,
) -> Result<Heatmap> {
    let (first, last) = window::calendar_days(now_unix_secs, days)?;
    let rows = reader.counters_between_days(first, last)?;
    let width = HOURS_PER_DAY;
    let mut totals = vec![0u64; WEEKDAYS * width];
    for row in rows {
        if !source.includes(row.network) {
            continue;
        }
        let start = window::bucket_start(row.day, row.hour)?;
        let (weekday, hour) = window::local_parts(start, offset_minutes)?;
        let index = usize::from(weekday) * width + usize::from(hour);
        let cell = totals
            .get_mut(index)
            .ok_or_else(|| Error::Corrupt(format!("heatmap {index}")))?;
        add(cell, row.count)?;
    }
    let mut cells = Vec::with_capacity(totals.len());
    for (index, count) in totals.into_iter().enumerate() {
        let weekday = u8::try_from(index / width).map_err(|_| Error::Corrupt("weekday".into()))?;
        let hour = u8::try_from(index % width).map_err(|_| Error::Corrupt("hour".into()))?;
        cells.push(HeatCell {
            weekday,
            hour,
            count,
        });
    }
    Ok(Heatmap {
        offset_minutes,
        cells,
    })
}

/// Conversations with the most messages in the window, highest count first.
///
/// The window is the last `days` UTC days including `now_unix_secs`. Conversations with no
/// messages in the window are omitted. Ties break by ascending id. `limit` of 0 returns an
/// empty vec. Both directions count.
///
/// # Errors
///
/// [`Error::InvalidRange`] when `days` is not positive, [`Error::Store`] on a read failure,
/// and [`Error::Corrupt`] when a count overflows.
pub fn top_conversations(
    reader: &Reader<'_>,
    now_unix_secs: i64,
    days: i64,
    limit: u64,
    source: Source,
) -> Result<Vec<RankedConversation>> {
    let (first, last) = window::calendar_days(now_unix_secs, days)?;
    let rows = reader.counters_between_days(first, last)?;
    let mut ranked = Vec::new();
    for conv in reader.conversations()? {
        if !source.includes(conv.network) {
            continue;
        }
        let mut messages = 0u64;
        for row in rows.iter().filter(|row| row.conversation_id == conv.id) {
            add(&mut messages, row.count)?;
        }
        if messages == 0 {
            continue;
        }
        ranked.push(RankedConversation {
            id: conv.id,
            network: conv.network,
            kind: conv.kind,
            external_id: conv.external_id,
            title: conv.title,
            messages,
        });
    }
    ranked.sort_by(|left, right| {
        right
            .messages
            .cmp(&left.messages)
            .then(left.id.cmp(&right.id))
    });
    let keep = usize::try_from(limit).unwrap_or(usize::MAX);
    ranked.truncate(keep);
    Ok(ranked)
}

/// Reply median for each UTC day of anchor times.
///
/// A sample is a resolved `answered` ticket with `reply_at`, bucketed by the UTC day of
/// `anchor_date`. Days run from `days` ago through the day of `now_unix_secs`. Anchors at or
/// after `now_unix_secs` are excluded. `low_sample` is set when the day has fewer than
/// [`MIN_DAILY_REPLIES`] replies. A day with some replies still returns a median. A day with
/// none has `p50_minutes: None` and `low_sample: true`.
///
/// # Errors
///
/// [`Error::InvalidRange`] when `days` is not positive or the window overflows,
/// [`Error::Store`] on a read or attribution failure, and [`Error::Corrupt`] when a count overflows.
pub fn response_p50_by_day(
    reader: &Reader<'_>,
    now_unix_secs: i64,
    days: i64,
    source: Source,
) -> Result<Vec<DailyResponse>> {
    let (first, last) = window::calendar_days(now_unix_secs, days)?;
    let start = first
        .checked_mul(crate::limits::SECS_PER_DAY)
        .ok_or(Error::InvalidRange)?;
    let snaps = reader.tickets_by_anchor(start, now_unix_secs)?;
    let mut by_day: Vec<Vec<i64>> = vec![Vec::new(); usize_days(first, last)?];
    for snap in snaps {
        if !includes(&snap, source) {
            continue;
        }
        let Some(secs) = answered_reply_secs(&snap) else {
            continue;
        };
        let day = snap
            .anchor_date_unix_secs
            .div_euclid(crate::limits::SECS_PER_DAY);
        if day < first || day > last {
            continue;
        }
        let index = usize::try_from(day - first).map_err(|_| Error::Corrupt("day index".into()))?;
        by_day
            .get_mut(index)
            .ok_or_else(|| Error::Corrupt("day index".into()))?
            .push(secs);
    }
    let mut out = Vec::with_capacity(by_day.len());
    for (offset, samples) in by_day.into_iter().enumerate() {
        let day = first + i64::try_from(offset).map_err(|_| Error::Corrupt("day".into()))?;
        let count = u64::try_from(samples.len()).map_err(|_| Error::Corrupt("samples".into()))?;
        out.push(DailyResponse {
            day,
            p50_minutes: captain_inbox::stats::percentile(&samples, 50).map(window::minutes),
            samples: count,
            low_sample: count < MIN_DAILY_REPLIES,
        });
    }
    Ok(out)
}

fn usize_days(first: i64, last: i64) -> Result<usize> {
    let span = last.checked_sub(first).ok_or(Error::InvalidRange)?;
    let count = span.checked_add(1).ok_or(Error::InvalidRange)?;
    usize::try_from(count).map_err(|_| Error::InvalidRange)
}
