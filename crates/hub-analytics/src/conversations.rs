//! The conversations page: filter, sparkline, and open-ticket counts.

use hub_store::{Conversation, Network, Reader, TicketSnap};

use crate::limits::SPARKLINE_DAYS;
use crate::types::{ConversationFilter, ConversationItem, ConversationPage};
use crate::window::{self, add};
use crate::{Error, Result};

/// One page of conversations, most recently active first.
///
/// `total` counts matches before `limit` and `offset`. A `limit` of 0 returns no items and
/// still sets `total`. The title query is a Unicode case-insensitive substring; blank queries
/// match every title. `filter.days` keeps conversations whose last activity is in
/// `[now - days * 86400, now)`.
///
/// `sparkline` is always the last 7 UTC days ending on the day of `now_unix_secs`, oldest
/// first, both directions. It does not follow `filter.days`.
///
/// An open ticket on the bridge supergroup belongs to the conversation with the same topic id
/// and network. Any other ticket belongs to the Telegram conversation whose external id is the
/// chat id in decimal, with the same topic id (two absent topic ids match).
///
/// # Errors
///
/// [`Error::InvalidRange`] when `filter.days` is set and not positive, [`Error::Store`] on a
/// read or attribution failure, and [`Error::Corrupt`] when a count overflows.
pub fn conversations_page(
    reader: &Reader<'_>,
    now_unix_secs: i64,
    filter: &ConversationFilter,
    limit: u64,
    offset: u64,
) -> Result<ConversationPage> {
    let activity_start = match filter.days {
        Some(days) => Some(window::rolling_start(now_unix_secs, days)?),
        None => None,
    };
    let needle = filter
        .query
        .as_deref()
        .map(str::trim)
        .filter(|query| !query.is_empty())
        .map(str::to_lowercase);
    let mut matched: Vec<Conversation> = reader
        .conversations()?
        .into_iter()
        .filter(|conv| filter.source.includes(conv.network))
        .filter(|conv| filter.kind.is_none_or(|kind| conv.kind == kind))
        .filter(|conv| match activity_start {
            Some(start) => {
                conv.last_activity_unix_secs >= start
                    && conv.last_activity_unix_secs < now_unix_secs
            }
            None => true,
        })
        .filter(|conv| title_matches(conv.title.as_deref(), needle.as_deref()))
        .collect();
    matched.sort_by(|left, right| {
        right
            .last_activity_unix_secs
            .cmp(&left.last_activity_unix_secs)
            .then(right.id.cmp(&left.id))
    });
    let total = u64::try_from(matched.len()).map_err(|_| Error::Corrupt("total".into()))?;
    let skip = usize::try_from(offset).unwrap_or(usize::MAX);
    let take = usize::try_from(limit).unwrap_or(usize::MAX);
    let page: Vec<Conversation> = matched.into_iter().skip(skip).take(take).collect();
    let (first, last) = window::calendar_days(now_unix_secs, SPARKLINE_DAYS)?;
    let counters = reader.counters_between_days(first, last)?;
    let open = reader.open_tickets()?;
    let bridge = reader.bridge_supergroup()?;
    let mut items = Vec::with_capacity(page.len());
    for conv in page {
        items.push(ConversationItem {
            sparkline: sparkline(&counters, conv.id, first, last)?,
            open_tickets: open_tickets(&conv, &open, bridge)?,
            id: conv.id,
            network: conv.network,
            kind: conv.kind,
            external_id: conv.external_id,
            title: conv.title,
            topic_id: conv.topic_id,
            muted: conv.muted,
            first_seen_unix_secs: conv.first_seen_unix_secs,
            last_activity_unix_secs: conv.last_activity_unix_secs,
            tags: conv.tags,
        });
    }
    Ok(ConversationPage {
        total,
        limit,
        offset,
        items,
    })
}

fn title_matches(title: Option<&str>, needle: Option<&str>) -> bool {
    let Some(needle) = needle else {
        return true;
    };
    title.is_some_and(|title| title.to_lowercase().contains(needle))
}

fn sparkline(
    counters: &[hub_store::CounterRow],
    conversation_id: i64,
    first: i64,
    last: i64,
) -> Result<Vec<u64>> {
    let mut out = Vec::new();
    let mut day = first;
    loop {
        let mut total = 0u64;
        for row in counters
            .iter()
            .filter(|row| row.conversation_id == conversation_id && row.day == day)
        {
            add(&mut total, row.count)?;
        }
        out.push(total);
        if day == last {
            break;
        }
        day = day.checked_add(1).ok_or(Error::InvalidRange)?;
    }
    Ok(out)
}

fn open_tickets(conv: &Conversation, tickets: &[TicketSnap], bridge: Option<i64>) -> Result<u64> {
    let mut count = 0u64;
    for ticket in tickets {
        if owns(conv, ticket, bridge) {
            add(&mut count, 1)?;
        }
    }
    Ok(count)
}

fn owns(conv: &Conversation, ticket: &TicketSnap, bridge: Option<i64>) -> bool {
    if bridge == Some(ticket.chat_id) {
        return conv.topic_id.is_some()
            && conv.topic_id == ticket.topic_id
            && conv.network == ticket.source;
    }
    conv.network == Network::Telegram
        && conv.external_id == ticket.chat_id.to_string()
        && conv.topic_id == ticket.topic_id
}
