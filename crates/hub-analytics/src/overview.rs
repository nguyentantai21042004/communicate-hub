//! Overview cards. Open tickets are the current queue. The other ranged numbers use `range_days`.

use captain_inbox::TicketState;

use hub_store::Reader;

use crate::limits::MESSAGES_24H_BUCKETS;
use crate::tickets::{answered_reply_secs, includes};
use crate::types::{NETWORKS, Overview, SourceCount};
use crate::window::{self, add};
use crate::{Error, Result, Source};

/// Messages in the last 24 hours, open tickets, reply median, active chats and module health.
///
/// `messages_24h` is the 24 UTC hour buckets ending at the hour of `now_unix_secs`, both
/// directions. [`Source::All`] emits Telegram, Zalo and Messenger, including zeros. A single
/// source emits one row even when the count is zero.
///
/// Active conversations are unmuted and have `last_activity` in `[now - range_days * 86400, now)`.
/// Watched conversations are unmuted and match `source`, with no time limit. Muted chats are
/// neither. Open tickets are every current `queued` and `digest` row, not only those opened in
/// the range. The reply median covers resolved `answered` tickets opened in that half-open range.
///
/// # Errors
///
/// [`Error::InvalidRange`] when `range_days` is not positive or a timestamp overflows,
/// [`Error::Store`] on a read or attribution failure, and [`Error::Corrupt`] when a count overflows.
pub fn overview(
    reader: &Reader<'_>,
    now_unix_secs: i64,
    range_days: i64,
    source: Source,
) -> Result<Overview> {
    let start = window::rolling_start(now_unix_secs, range_days)?;
    let messages_24h = messages_24h(reader, now_unix_secs, source)?;
    let (queued_tickets, digest_tickets) = open_counts(reader, source)?;
    let (response_p50_minutes, response_samples) = response(reader, start, now_unix_secs, source)?;
    let (active_conversations, watched_conversations) =
        activity(reader, start, now_unix_secs, source)?;
    Ok(Overview {
        messages_24h,
        queued_tickets,
        digest_tickets,
        response_p50_minutes,
        response_samples,
        active_conversations,
        watched_conversations,
        modules: reader.modules()?,
    })
}

fn messages_24h(
    reader: &Reader<'_>,
    now_unix_secs: i64,
    source: Source,
) -> Result<Vec<SourceCount>> {
    let end_hour = window::hour_index(now_unix_secs);
    let start_hour = end_hour
        .checked_sub(MESSAGES_24H_BUCKETS - 1)
        .ok_or(Error::InvalidRange)?;
    let hours = hour_span()?;
    let first_day = start_hour.div_euclid(hours);
    let last_day = end_hour.div_euclid(hours);
    let rows = reader.counters_between_days(first_day, last_day)?;
    let mut counts = [0u64; NETWORKS.len()];
    for row in rows {
        let index = row
            .day
            .checked_mul(hours)
            .and_then(|day| day.checked_add(i64::from(row.hour)))
            .ok_or(Error::InvalidRange)?;
        if index < start_hour || index > end_hour {
            continue;
        }
        let Some(slot) = NETWORKS.iter().position(|network| *network == row.network) else {
            continue;
        };
        add(&mut counts[slot], row.count)?;
    }
    Ok(NETWORKS
        .into_iter()
        .zip(counts)
        .filter(|(network, _)| source.includes(*network))
        .map(|(network, count)| SourceCount {
            source: network,
            count,
        })
        .collect())
}

fn open_counts(reader: &Reader<'_>, source: Source) -> Result<(u64, u64)> {
    let mut queued = 0u64;
    let mut digest = 0u64;
    for snap in reader.open_tickets()? {
        if !includes(&snap, source) {
            continue;
        }
        if snap.state == TicketState::Queued.as_str() {
            add(&mut queued, 1)?;
        } else if snap.state == TicketState::Digest.as_str() {
            add(&mut digest, 1)?;
        }
    }
    Ok((queued, digest))
}

fn response(
    reader: &Reader<'_>,
    start_unix_secs: i64,
    now_unix_secs: i64,
    source: Source,
) -> Result<(Option<f64>, u64)> {
    let mut samples = Vec::new();
    for snap in reader.tickets_by_opened(start_unix_secs, now_unix_secs)? {
        if !includes(&snap, source) {
            continue;
        }
        if let Some(secs) = answered_reply_secs(&snap) {
            samples.push(secs);
        }
    }
    let p50 = captain_inbox::stats::percentile(&samples, 50).map(window::minutes);
    Ok((
        p50,
        u64::try_from(samples.len()).map_err(|_| Error::Corrupt("samples".into()))?,
    ))
}

fn activity(
    reader: &Reader<'_>,
    start_unix_secs: i64,
    now_unix_secs: i64,
    source: Source,
) -> Result<(u64, u64)> {
    let mut active = 0u64;
    let mut watched = 0u64;
    for conv in reader.conversations()? {
        if conv.muted || !source.includes(conv.network) {
            continue;
        }
        add(&mut watched, 1)?;
        if conv.last_activity_unix_secs >= start_unix_secs
            && conv.last_activity_unix_secs < now_unix_secs
        {
            add(&mut active, 1)?;
        }
    }
    Ok((active, watched))
}

fn hour_span() -> Result<i64> {
    i64::try_from(crate::limits::HOURS_PER_DAY).map_err(|_| Error::Corrupt("hours".into()))
}
