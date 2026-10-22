//! Ticket snapshots turned into the facts Captain's report already scores.
//!
//! Reply seconds use the same subtraction as `captain-store`: `reply_at - anchor_date`, floored
//! at zero. The median itself is [`captain_inbox::stats::percentile`].

use captain_core::{ChatId, MessageId};
use captain_inbox::report::TicketFact;
use captain_inbox::{
    EntryKind, ExplicitLabel, ImplicitLabel, Route, ThreadKey, TicketState, TriggerKind,
};

use hub_store::TicketSnap;

use crate::{Error, Result, Source};

const ENTRY_DM: &str = "dm"; // captain EntryKind::Dm
const ENTRY_GROUP: &str = "group"; // captain EntryKind::Group
const ENTRY_FORUM: &str = "forum"; // captain EntryKind::Forum

pub(crate) fn includes(snap: &TicketSnap, source: Source) -> bool {
    source.includes(snap.source)
}

/// Seconds from the anchor to the owner's reply, for a resolved ticket labelled `answered`.
pub(crate) fn answered_reply_secs(snap: &TicketSnap) -> Option<i64> {
    if snap.state != TicketState::Resolved.as_str() {
        return None;
    }
    if snap.label_implicit.as_deref() != Some(ImplicitLabel::Answered.as_str()) {
        return None;
    }
    let reply = snap.reply_at_unix_secs?;
    Some((reply - snap.anchor_date_unix_secs).max(0))
}

pub(crate) fn to_fact(snap: &TicketSnap) -> Result<TicketFact> {
    let topic = match snap.topic_id {
        None => None,
        Some(id) => Some(MessageId(
            i32::try_from(id).map_err(|_| Error::Corrupt(format!("topic {id}")))?,
        )),
    };
    let messages = usize::try_from(snap.message_count)
        .map_err(|_| Error::Corrupt(format!("messages {}", snap.message_count)))?;
    Ok(TicketFact {
        id: snap.id,
        key: ThreadKey {
            chat: ChatId(snap.chat_id),
            topic,
        },
        chat_label: snap.chat_label.clone(),
        chat_kind: entry_kind(snap.chat_kind.as_deref()),
        profile: snap.profile.clone(),
        trigger: parse_stored::<TriggerKind>(&snap.trigger)?,
        route: snap
            .route
            .as_deref()
            .map(parse_stored::<Route>)
            .transpose()?,
        state: parse_stored::<TicketState>(&snap.state)?,
        reason: snap.state_reason.clone(),
        opened_at: snap.opened_at_unix_secs,
        closed_at: snap.closed_at_unix_secs,
        anchor_date: snap.anchor_date_unix_secs,
        messages,
        implicit: snap
            .label_implicit
            .as_deref()
            .map(parse_stored::<ImplicitLabel>)
            .transpose()?,
        explicit: snap
            .label_explicit
            .as_deref()
            .map(parse_stored::<ExplicitLabel>)
            .transpose()?,
        read_secs: snap
            .read_at_unix_secs
            .map(|read| (read - snap.anchor_date_unix_secs).max(0)),
        reply_secs: snap
            .reply_at_unix_secs
            .map(|reply| (reply - snap.anchor_date_unix_secs).max(0)),
    })
}

fn entry_kind(value: Option<&str>) -> Option<EntryKind> {
    match value {
        Some(ENTRY_DM) => Some(EntryKind::Dm),
        Some(ENTRY_GROUP) => Some(EntryKind::Group),
        Some(ENTRY_FORUM) => Some(EntryKind::Forum),
        _ => None,
    }
}

fn parse_stored<T: std::str::FromStr<Err = String>>(value: &str) -> Result<T> {
    value.parse().map_err(Error::Corrupt)
}
