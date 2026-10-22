//! Ticket funnel. Drafts are reported as unavailable rather than as zero.

use captain_inbox::{ImplicitLabel, TicketState};

use hub_store::Reader;

use crate::tickets::includes;
use crate::types::{DraftStage, ResolvedLabels, TicketFunnel};
use crate::window::{self, add};
use crate::{Result, Source};

/// Tickets opened in `[now - days * 86400, now)`, split by state and implicit label.
///
/// `queued + resolved` counts the tickets that were queued or later resolved. `digest` is its
/// own count. Skipped tickets are omitted. A resolved ticket whose label is missing or unknown
/// increments `unlabelled`, so the five label counts add up to `resolved`. `draft` is
/// [`DraftStage::NotAvailableInP1`] and is never a number.
///
/// # Errors
///
/// [`Error`](crate::Error) when the range is invalid, a count overflows, or the store cannot
/// attribute a chat.
pub fn ticket_funnel(
    reader: &Reader<'_>,
    now_unix_secs: i64,
    days: i64,
    source: Source,
) -> Result<TicketFunnel> {
    let start = window::rolling_start(now_unix_secs, days)?;
    let mut queued = 0u64;
    let mut digest = 0u64;
    let mut resolved = 0u64;
    let mut labels = ResolvedLabels {
        answered: 0,
        answered_elsewhere: 0,
        read_only: 0,
        unseen: 0,
        unlabelled: 0,
    };
    for snap in reader.tickets_by_opened(start, now_unix_secs)? {
        if !includes(&snap, source) {
            continue;
        }
        if snap.state == TicketState::Queued.as_str() {
            add(&mut queued, 1)?;
        } else if snap.state == TicketState::Digest.as_str() {
            add(&mut digest, 1)?;
        } else if snap.state == TicketState::Resolved.as_str() {
            add(&mut resolved, 1)?;
            match snap.label_implicit.as_deref() {
                Some(label) if label == ImplicitLabel::Answered.as_str() => {
                    add(&mut labels.answered, 1)?;
                }
                Some(label) if label == ImplicitLabel::AnsweredElsewhere.as_str() => {
                    add(&mut labels.answered_elsewhere, 1)?;
                }
                Some(label) if label == ImplicitLabel::ReadOnly.as_str() => {
                    add(&mut labels.read_only, 1)?;
                }
                Some(label) if label == ImplicitLabel::Unseen.as_str() => {
                    add(&mut labels.unseen, 1)?;
                }
                _ => add(&mut labels.unlabelled, 1)?,
            }
        }
    }
    Ok(TicketFunnel {
        draft: DraftStage::NotAvailableInP1,
        queued,
        digest,
        resolved,
        resolved_labels: labels,
    })
}
