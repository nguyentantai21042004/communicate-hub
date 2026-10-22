//! Telegram trigger mix, Zalo content mix, and the health page.
//!
//! Telegram ratios come from [`captain_inbox::report::build`]. This module does not recompute
//! them. Zalo does not have that report; its numbers are counters and failed events.

use captain_inbox::Policy;
use captain_inbox::report::{self, Report, ReportInput, Stat};

use hub_store::{ContentKind, EventStream, LogLevel, Network, Reader};

use crate::limits::ZALO_KIND_WINDOW_DAYS;
use crate::tickets::to_fact;
use crate::types::{Health, KindCount, LevelCount, TelegramSummary, TriggerBreakdown, ZaloSummary};
use crate::window::{self, add};
use crate::{Error, Result};

const METRIC_TICKETS: &str = "M2"; // captain report: tickets per trigger
const METRIC_PRECISION: &str = "M4"; // captain report: answered / labelled
const STAT_ALL: &str = "all"; // M4 summary label
const STAT_ANSWERED: &str = "answered"; // M4 breakdown label

/// Trigger breakdown and answered ratio for Telegram tickets opened in the half-open range.
///
/// Zalo tickets, including ones on the bridge supergroup, are dropped before
/// [`report::build`] runs, so they cannot change the ratio. `resolved_ratio` is M4's `all`
/// stat: answered labels divided by labelled tickets. It is `None` when nothing was labelled.
/// `tickets` is M2's sample size, which excludes self-handled tickets.
///
/// # Errors
///
/// [`Error::InvalidRange`] when `days` is not positive, [`Error::Store`] on a read or
/// attribution failure, and [`Error::Corrupt`] when a ticket or the report cannot be decoded.
pub fn telegram_summary(
    reader: &Reader<'_>,
    now_unix_secs: i64,
    days: i64,
) -> Result<TelegramSummary> {
    let since = window::rolling_start(now_unix_secs, days)?;
    let mut facts = Vec::new();
    for snap in reader.tickets_by_opened(since, now_unix_secs)? {
        if snap.source != Network::Telegram {
            continue;
        }
        facts.push(to_fact(&snap)?);
    }
    let input = ReportInput {
        since,
        until: now_unix_secs,
        tickets: facts,
        ..ReportInput::default()
    };
    let report = report::build(&input, &Policy::default());
    let tickets = metric(&report, METRIC_TICKETS)?;
    let precision = metric(&report, METRIC_PRECISION)?;
    let mut triggers = Vec::with_capacity(tickets.breakdown.len());
    for row in &tickets.breakdown {
        let answered_ratio = precision
            .breakdown
            .iter()
            .find(|item| item.key == row.key)
            .and_then(|item| stat_value(&item.stats, STAT_ANSWERED));
        triggers.push(TriggerBreakdown {
            trigger: row.key.clone(),
            tickets: as_count(row.n)?,
            answered_ratio,
        });
    }
    Ok(TelegramSummary {
        tickets: as_count(tickets.n)?,
        labelled: as_count(precision.n)?,
        resolved_ratio: stat_value(&precision.summary, STAT_ALL),
        triggers,
    })
}

/// Zalo content mix for the last 14 UTC days, plus unresolved Zalo failures.
///
/// Every [`ContentKind`] is present, including zeros. Both directions count. An unresolved
/// failure counts when its conversation is Zalo or it names no conversation. Telegram failures
/// and resolved failures do not count. `topicless_groups` is copied into the result.
///
/// # Errors
///
/// [`Error::InvalidRange`] when the 14-day window overflows, [`Error::Store`] on a read
/// failure, and [`Error::Corrupt`] when a count overflows.
pub fn zalo_summary(
    reader: &Reader<'_>,
    now_unix_secs: i64,
    topicless_groups: u64,
) -> Result<ZaloSummary> {
    let (first, last) = window::calendar_days(now_unix_secs, ZALO_KIND_WINDOW_DAYS)?;
    let rows = reader.counters_between_days(first, last)?;
    let mut kinds = Vec::with_capacity(ContentKind::ALL.len());
    for kind in ContentKind::ALL {
        let mut count = 0u64;
        for row in rows
            .iter()
            .filter(|row| row.network == Network::Zalo && row.content_kind == *kind)
        {
            add(&mut count, row.count)?;
        }
        kinds.push(KindCount { kind: *kind, count });
    }
    let mut failed_events = 0u64;
    for failure in reader.failures(true)? {
        if failure.network.is_none() || failure.network == Some(Network::Zalo) {
            add(&mut failed_events, 1)?;
        }
    }
    Ok(ZaloSummary {
        window_days: ZALO_KIND_WINDOW_DAYS,
        kinds,
        failed_events,
        topicless_groups,
    })
}

/// Module snapshots, unresolved failures, and log rows in `[now - 86400, now)`.
///
/// Both event lists contain every [`LogLevel`], with zero where the window had none.
///
/// # Errors
///
/// [`Error::InvalidRange`] when `now_unix_secs - 86400` overflows, [`Error::Store`] on a read
/// failure, and [`Error::Corrupt`] when a count overflows.
pub fn health(reader: &Reader<'_>, now_unix_secs: i64) -> Result<Health> {
    let start = now_unix_secs
        .checked_sub(crate::limits::SECS_PER_DAY)
        .ok_or(Error::InvalidRange)?;
    let mut unresolved_failures = 0u64;
    let mut retryable_failures = 0u64;
    for failure in reader.failures(true)? {
        add(&mut unresolved_failures, 1)?;
        if failure.retryable {
            add(&mut retryable_failures, 1)?;
        }
    }
    Ok(Health {
        modules: reader.modules()?,
        unresolved_failures,
        retryable_failures,
        bridge_events_24h: fill_levels(&reader.event_level_counts(
            EventStream::Bridge,
            start,
            now_unix_secs,
        )?)?,
        module_events_24h: fill_levels(&reader.event_level_counts(
            EventStream::Module,
            start,
            now_unix_secs,
        )?)?,
    })
}

fn fill_levels(counts: &[hub_store::LevelCount]) -> Result<Vec<LevelCount>> {
    let mut out = Vec::with_capacity(LogLevel::ALL.len());
    for level in LogLevel::ALL {
        let count = counts
            .iter()
            .find(|row| row.level == *level)
            .map(|row| row.count)
            .unwrap_or(0);
        out.push(LevelCount {
            level: *level,
            count,
        });
    }
    Ok(out)
}

fn metric<'a>(report: &'a Report, id: &str) -> Result<&'a report::Metric> {
    report
        .sections
        .iter()
        .flat_map(|section| &section.metrics)
        .find(|item| item.id == id)
        .ok_or_else(|| Error::Corrupt(format!("metric {id}")))
}

fn stat_value(stats: &[Stat], label: &str) -> Option<f64> {
    stats
        .iter()
        .find(|stat| stat.label == label)
        .and_then(|stat| stat.value)
}

fn as_count(value: usize) -> Result<u64> {
    u64::try_from(value).map_err(|_| Error::Corrupt("count".into()))
}
