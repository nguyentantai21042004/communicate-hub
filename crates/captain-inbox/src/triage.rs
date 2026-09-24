//! Triage: fixed rules that give a closed burst its route. No model runs here.

use crate::coalesce::Burst;
use crate::config::Policy;
use crate::ticket::{Route, TicketState};
use crate::trigger::TriggerKind;

/// Facts about a burst that the daemon looks up before triage.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TriageFacts {
    /// The pipeline is paused.
    pub paused: bool,
    /// The chat is muted.
    pub muted: bool,
    /// Seconds between the anchor's send time and the burst closing. Large after catch-up.
    pub anchor_age_secs: i64,
}

/// The outcome of triage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Triage {
    /// State the ticket starts in.
    pub state: TicketState,
    /// Route, when the ticket goes anywhere.
    pub route: Option<Route>,
    /// Why, for skipped tickets and digest routes.
    pub reason: Option<&'static str>,
    /// The anchor was older than the stale threshold.
    pub stale: bool,
}

/// Gives `burst` its route under `policy`.
pub fn triage(burst: &Burst, facts: TriageFacts, policy: &Policy) -> Triage {
    let skip = |reason| Triage {
        state: TicketState::Skipped,
        route: None,
        reason: Some(reason),
        stale: false,
    };
    if facts.paused {
        return skip("paused");
    }
    if facts.muted {
        return skip("muted");
    }
    let stale = facts.anchor_age_secs > policy.stale_after_secs;
    let digest_reason = if stale {
        Some("stale")
    } else if burst.trigger().kind == TriggerKind::Keyword {
        Some("keyword")
    } else {
        None
    };
    match digest_reason {
        Some(reason) => Triage {
            state: TicketState::Digest,
            route: Some(Route::Digest),
            reason: Some(reason),
            stale,
        },
        None => Triage {
            state: TicketState::Queued,
            route: Some(Route::Draft),
            reason: None,
            stale,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coalesce::{Arrival, Coalescer};
    use crate::ticket::ThreadKey;
    use crate::trigger::Trigger;
    use captain_core::{ChatId, MessageId};

    fn burst(kind: TriggerKind) -> Burst {
        let mut c = Coalescer::default();
        let policy = Policy::default();
        let arrival = Arrival {
            key: ThreadKey {
                chat: ChatId(1),
                topic: None,
            },
            id: MessageId(1),
            album: None,
            sender: None,
            trigger: Some(Trigger {
                kind,
                reason: String::new(),
            }),
        };
        c.observe(arrival, "dm", policy.profile("dm"), 0);
        c.due(i64::MAX).remove(0)
    }

    #[test]
    fn routes_by_trigger_and_age() {
        let policy = Policy::default();
        let fresh = TriageFacts::default();
        assert_eq!(
            triage(&burst(TriggerKind::Mention), fresh, &policy).route,
            Some(Route::Draft)
        );
        assert_eq!(
            triage(&burst(TriggerKind::Keyword), fresh, &policy).reason,
            Some("keyword")
        );
        let old = TriageFacts {
            anchor_age_secs: policy.stale_after_secs + 1,
            ..fresh
        };
        let t = triage(&burst(TriggerKind::DmAll), old, &policy);
        assert_eq!((t.route, t.stale), (Some(Route::Digest), true));
    }

    #[test]
    fn pause_and_mute_skip() {
        let policy = Policy::default();
        let paused = TriageFacts {
            paused: true,
            ..TriageFacts::default()
        };
        assert_eq!(
            triage(&burst(TriggerKind::Mention), paused, &policy).state,
            TicketState::Skipped
        );
    }
}
